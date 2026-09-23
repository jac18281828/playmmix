use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gloo_timers::callback::Timeout;
use js_sys::Promise;
use log::info;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{BeforeUnloadEvent, Event, MouseEvent, ShareData};
use yew::html::Scope;
use yew::{Callback, Component, Context, Html, NodeRef, Renderer, html};

mod autosave;
mod control;
mod control_bar;
mod diagnostics;
mod editor;
mod examples;
mod highlight;
mod keys;
mod layout;
mod machine;
mod output;
mod share;

use control::{Control, StepOutcome, yield_to_event_loop};
use control_bar::{ControlBar, ControlEnablement, control_enablement};
use diagnostics::{describe_source_error, parse_error_location};
use editor::Editor;
use examples::DEFAULT_MMS;
use keys::{KeydownHandler, install_keyboard_shortcuts};
use layout::{
    CommittedSizes, DragState, column_splitter_handlers, main_style, row_splitter_handlers,
};
use machine::{MachinePane, ViewState};
use output::OutputPane;

/// The filename `Control` assembles the editor's buffer under. Fixed:
/// playmmix edits a single in-memory buffer, not a multi-file project, and
/// breakpoint/PC line lookups need the same name on every assemble.
const SOURCE_FILENAME: &str = "source.mms";

/// How long a keystroke's `SourceChanged` waits, with no further keystroke,
/// before `Msg::ReassembleSource` actually re-assembles and (on a parse
/// error) shows one -- so typing a line the assembler can't parse yet (e.g.
/// `ADDI $1, ` mid-operand) doesn't flash "Assembly error" on every
/// character. Long enough to cover ordinary inter-keystroke gaps, short
/// enough that a genuine pause still reads as immediate.
const SOURCE_DEBOUNCE_MS: u32 = 400;

/// New's confirmation prompt, shown only when `autosave::confirm_needed`
/// says the editor holds a program distinct from the minimal skeleton.
const NEW_CONFIRM_MESSAGE: &str = "Replace your program with the minimal skeleton?";

/// A shared link's replace-confirmation prompt, shown only when
/// `autosave::confirm_needed` says loading the shared program would
/// replace different work.
const SHARE_LOAD_CONFIRM_MESSAGE: &str = "Replace your program with the shared one?";

/// A shared program's status once loaded.
const SHARE_LOADED_STATUS: &str = "Loaded shared program";

/// An unreadable share link's status: the editor and saved work stay as
/// they were.
const SHARE_UNREADABLE_STATUS: &str = "The shared link could not be read";

/// The status readout's text for a chunked Run, Continue, or Next's
/// outcome -- shared because all three drive the same chunk-yield loop
/// (`Control::resume_chunk`) and report through the same four `StepOutcome`
/// variants. Neither `run_chunk` nor `continue_chunk` ever returns
/// `Advanced` (a chunk that neither halts nor hits a breakpoint always
/// exhausts its budget), so "Stepped over" only ever surfaces from a
/// chunked Next.
fn status_for(outcome: StepOutcome) -> &'static str {
    match outcome {
        StepOutcome::BudgetExhausted => "Running",
        StepOutcome::Advanced => "Stepped over",
        StepOutcome::Halted => "Halted",
        StepOutcome::Breakpoint(_) => "Hit breakpoint",
    }
}

/// The status text for a chunk-tick's terminal outcome (`Halted`,
/// `Breakpoint`, or `Advanced` -- never `BudgetExhausted`, an intermediate
/// tick this never composes with `restart_signal`), composed with
/// `Restarted · ` when `restart_signal` marks this outcome as the one a
/// restarting Run set it for -- see `App::restart_signal`.
fn restarted_status(outcome: StepOutcome, restart_signal: bool) -> String {
    let base = status_for(outcome);
    if restart_signal {
        format!("Restarted · {base}")
    } else {
        base.to_string()
    }
}

/// `App::advance_chunk`'s core: run one chunk of whichever operation --
/// Run, Continue, or Next -- is in flight, record the pause boundary once
/// the outcome is terminal, and return the status text to show plus
/// whether another tick must be scheduled. Factored out for the same
/// reason `reload_and_record` is: testable without a live `Context` --
/// the plain seam the restart signal's visibility (§1's "visible to the
/// end") is proven through.
///
/// `restart_signal` composes onto the status (`restarted_status`) only
/// once the outcome is terminal, never a `BudgetExhausted` tick, and is
/// cleared there -- so it reaches exactly the Run that set it, and no
/// later command's own outcome. Increments `execution_stops` on that same
/// terminal outcome, never on `BudgetExhausted`.
fn advance_chunk_once(
    control: &mut Control,
    view_state: &mut ViewState,
    restart_signal: &mut bool,
    execution_stops: &mut u64,
) -> (String, bool) {
    let outcome = control.resume_chunk(control::CHUNK_BUDGET);
    view_state.observe(control);
    match outcome {
        StepOutcome::BudgetExhausted => (status_for(outcome).to_string(), true),
        StepOutcome::Halted | StepOutcome::Breakpoint(_) | StepOutcome::Advanced => {
            let status = restarted_status(outcome, *restart_signal);
            *restart_signal = false;
            view_state.record_pause_boundary(control);
            *execution_stops += 1;
            (status, false)
        }
    }
}

/// Continue's and Next's shared "first chunk landed" tail: observe the
/// resulting state, and on every terminal outcome (not `BudgetExhausted`)
/// record the pause boundary and increment `execution_stops` too. Takes the
/// outcome already computed, not the call that produced it -- Continue's
/// unconditional first instruction and Next's own statement-then-target-check
/// are the one part that isn't shared. Returns the status text plus whether
/// the caller must schedule another chunk tick.
fn first_chunk_outcome(
    control: &mut Control,
    view_state: &mut ViewState,
    outcome: StepOutcome,
    execution_stops: &mut u64,
) -> (String, bool) {
    view_state.observe(control);
    match outcome {
        StepOutcome::BudgetExhausted => (status_for(outcome).to_string(), true),
        StepOutcome::Advanced | StepOutcome::Halted | StepOutcome::Breakpoint(_) => {
            view_state.record_pause_boundary(control);
            *execution_stops += 1;
            (status_for(outcome).to_string(), false)
        }
    }
}

/// `Msg::Continue`'s core, factored out for the same reason
/// `interrupt_if_running` is: testable without a live `Context`. `None` --
/// nothing run, `restart_signal` and `status_message` both left untouched by
/// the caller -- when nothing is running but no session has started either:
/// gdb answers "The program is not being run" there. Reachable even though
/// `ControlEnablement` already gates Continue on a started session, because
/// `App::update`'s own `flush_pending_reassemble` runs first and can itself
/// reload, ending the very session this call would otherwise have resumed --
/// the owner's settled decision is that the flush, not this call, is what
/// already changed state, so this is a true no-op, not a fallback path.
/// Clears the changed-value highlights before executing, same as Run and
/// Next (`docs/layout-spec.md`'s Highlights §3).
fn continue_pressed(
    control: &mut Control,
    view_state: &mut ViewState,
    execution_stops: &mut u64,
) -> Option<(String, bool)> {
    if control.is_running() || !control.session() {
        return None;
    }
    view_state.clear_changed();
    let outcome = control.continue_chunk(control::CHUNK_BUDGET);
    Some(first_chunk_outcome(
        control,
        view_state,
        outcome,
        execution_stops,
    ))
}

/// `Msg::Step`'s core, factored out for the same reason `continue_pressed`
/// is: testable without a live `Context`. `None` -- `execution_stops`
/// untouched -- while the machine is already halted; the caller's own
/// `flush_pending_reassemble` already guards the other refusal, an assembly
/// error, before this runs. Otherwise steps once, increments
/// `execution_stops`, and returns the outcome for the caller's own status
/// text.
fn step_pressed(
    control: &mut Control,
    view_state: &mut ViewState,
    restart_signal: &mut bool,
    execution_stops: &mut u64,
) -> Option<StepOutcome> {
    if control.is_running() {
        return None;
    }
    let was_halted = control.is_halted();
    let outcome = control.step();
    if was_halted {
        return None;
    }
    *restart_signal = false;
    view_state.observe(control);
    view_state.record_pause_boundary(control);
    *execution_stops += 1;
    Some(outcome)
}

/// Whether `window.onbeforeunload` should arm the native leave-this-page
/// confirmation: the most recent save failed, and the editor holds
/// something other than `DEFAULT_MMS` to lose. A successful save means
/// leaving costs nothing; the untouched skeleton has nothing to lose even
/// after a failed save.
fn leave_warning_needed(last_save_succeeded: bool, source: &str) -> bool {
    !last_save_succeeded && source != DEFAULT_MMS
}

/// The JS closure backing `window.onbeforeunload` -- must stay alive for as
/// long as the handler should stay registered; dropping it frees the JS
/// function `onbeforeunload` points at.
type BeforeUnloadHandler = Closure<dyn FnMut(Event)>;

/// Registers `window.onbeforeunload`: when `armed` (shared with `App`, see
/// `leave_warning_needed`) is set at unload time, calls
/// `Event::prevent_default` and sets `BeforeUnloadEvent`'s `returnValue` --
/// the modern and legacy triggers, respectively, for a browser's native
/// "leave this page? changes may not be saved" prompt, since browsers vary
/// in which one they honor. Returns the shared flag alongside the
/// `Closure` backing the handler; the caller must keep it alive (see
/// [`BeforeUnloadHandler`]).
fn install_beforeunload_handler() -> (Rc<RefCell<bool>>, BeforeUnloadHandler) {
    let armed = Rc::new(RefCell::new(false));
    let armed_for_handler = armed.clone();
    let handler = Closure::wrap(Box::new(move |event: Event| {
        if !*armed_for_handler.borrow() {
            return;
        }
        event.prevent_default();
        if let Ok(event) = event.dyn_into::<BeforeUnloadEvent>() {
            event.set_return_value("The last save failed; changes may not be saved.");
        }
    }) as Box<dyn FnMut(Event)>);

    if let Some(window) = web_sys::window() {
        window.set_onbeforeunload(Some(handler.as_ref().unchecked_ref()));
    }

    (armed, handler)
}

/// The JS closure backing `window.onhashchange` -- must stay alive for as
/// long as the handler should stay registered, same as
/// [`BeforeUnloadHandler`].
type HashchangeHandler = Closure<dyn FnMut(Event)>;

/// Registers `window.onhashchange`, sending `Msg::CheckSharedLink` on every
/// fire: pasting a share link's fragment into an open tab changes only the
/// fragment and reloads nothing, so the page's own hash-change event is the
/// only signal such a paste gives. Returns the `Closure` backing the
/// handler; the caller must keep it alive (see [`HashchangeHandler`]).
fn install_hashchange_handler(link: Scope<App>) -> HashchangeHandler {
    let handler = Closure::wrap(Box::new(move |_event: Event| {
        link.send_message(Msg::CheckSharedLink);
    }) as Box<dyn FnMut(Event)>);

    if let Some(window) = web_sys::window() {
        window.set_onhashchange(Some(handler.as_ref().unchecked_ref()));
    }

    handler
}

/// `Msg::Interrupt`'s core logic, factored out of `App::update` so it is
/// testable without a live `Context`: ends a chunked Run, Continue, or Next
/// in flight, records the resulting pause boundary, and increments
/// `execution_stops`. A true no-op otherwise -- `ready`/`paused` have
/// nothing left to interrupt, and recording a boundary with nothing having
/// moved since the last one would diff the current state against itself and
/// silently clear the changed-value highlights that boundary already set.
/// Returns whether anything actually happened.
fn interrupt_if_running(
    control: &mut Control,
    view_state: &mut ViewState,
    execution_stops: &mut u64,
) -> bool {
    if !control.is_running() {
        return false;
    }
    control.end_in_flight();
    view_state.observe(control);
    view_state.record_pause_boundary(control);
    *execution_stops += 1;
    true
}

/// Cancel any pending chunk tick and reload `source` into `control`,
/// recording the resulting success or parse error -- the sequence
/// `App::reload_source` always runs, factored out so it can also run
/// unconditionally as part of a caller-checked flush.
fn reload_and_record(
    chunk_timeout: &mut Option<Timeout>,
    control: &mut Control,
    source: &str,
    error: &mut Option<String>,
    error_line: &mut Option<usize>,
    view_state: &mut ViewState,
) {
    *chunk_timeout = None;
    match control.reload(source) {
        Ok(()) => {
            *error = None;
            *error_line = None;
            view_state.reset(control);
        }
        Err(err) => {
            *error_line = parse_error_location(&err).map(|(line, _)| line);
            *error = Some(describe_source_error(source, &err));
        }
    }
}

/// Loads `DEFAULT_MMS`: the machine `App::create` and New both start from.
/// Infallible -- pinned by `examples::tests::default_mms_assembles`.
fn default_control() -> Control {
    Control::new(DEFAULT_MMS, SOURCE_FILENAME)
        .expect("DEFAULT_MMS assembles; pinned by examples::tests::default_mms_assembles")
}

/// `App::create`'s restore step, the plain seam a host test can drive
/// without a live `Context`: given what `autosave::load` returned, decide
/// the editor's starting text. `control` must already hold `DEFAULT_MMS`
/// (`Control::new`'s own infallible load); a saved program loads through
/// `reload_and_record`, the same path `Msg::SourceChanged`'s debounced
/// reassemble takes, so one that fails to assemble shows its error exactly
/// as an edit would -- and leaves `control` on `DEFAULT_MMS`'s machine,
/// `reload`'s own guarantee on a parse error. Nothing saved, or an empty
/// entry, starts from `DEFAULT_MMS` with no reload needed.
fn restore_source(
    saved: Option<String>,
    chunk_timeout: &mut Option<Timeout>,
    control: &mut Control,
    error: &mut Option<String>,
    error_line: &mut Option<usize>,
    view_state: &mut ViewState,
) -> String {
    match saved {
        Some(source) => {
            reload_and_record(
                chunk_timeout,
                control,
                &source,
                error,
                error_line,
                view_state,
            );
            source
        }
        None => DEFAULT_MMS.to_string(),
    }
}

/// New's core: delegates to `start_fresh` with `DEFAULT_MMS` -- a fresh
/// `Control` holds no breakpoints, unlike a reload, which keeps every
/// breakpoint line that still resolves. Returns `DEFAULT_MMS`, for the
/// caller to assign as the editor's text and save.
fn start_over(
    chunk_timeout: &mut Option<Timeout>,
    debounce_timeout: &mut Option<Timeout>,
    control: &mut Control,
    error: &mut Option<String>,
    error_line: &mut Option<usize>,
    view_state: &mut ViewState,
) -> String {
    start_fresh(
        chunk_timeout,
        debounce_timeout,
        control,
        error,
        error_line,
        view_state,
        DEFAULT_MMS,
    );
    DEFAULT_MMS.to_string()
}

/// The browser's native confirm dialog, `true` when the user accepts. A
/// confirm error counts as No: there is no way to ask again, so the safer
/// reading of "couldn't confirm" is "didn't confirm".
fn confirm(message: &str) -> bool {
    web_sys::window()
        .and_then(|window| window.confirm_with_message(message).ok())
        .unwrap_or(false)
}

/// Whether New may proceed: `true` outright when `current` matches the
/// skeleton or `replacement` (`autosave::confirm_needed`), otherwise
/// `confirm` decides.
fn confirm_new(current: &str, replacement: &str) -> bool {
    !autosave::confirm_needed(current, replacement) || confirm(NEW_CONFIRM_MESSAGE)
}

/// What a `hashchange`, or the check scheduled after the first paint, does
/// with a share link -- computed by `decide_shared_link`, a plain function
/// so a host test drives every branch directly. `Msg::CheckSharedLink`
/// turns each variant into `window.confirm`, the load itself, and the
/// fragment strip.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SharedLinkDecision {
    /// No `share::FRAGMENT_PREFIX` fragment at all: do nothing.
    Ignore,
    /// A `share::FRAGMENT_PREFIX` fragment whose payload didn't decode:
    /// show the failure and strip it.
    Unreadable,
    /// A `share::FRAGMENT_PREFIX` fragment that decoded to `source`: load
    /// it, asking first when `ask` is true.
    Load { source: String, ask: bool },
}

/// The whole hash-triage: given `hash` (`Location::hash()`'s value) and the
/// editor's current text, what to do next. `share::program_from_hash`
/// alone can't distinguish a fragment to ignore from an unreadable link --
/// both read `None` -- so the prefix is checked here first.
fn decide_shared_link(hash: &str, current: &str) -> SharedLinkDecision {
    if !hash.starts_with(share::FRAGMENT_PREFIX) {
        return SharedLinkDecision::Ignore;
    }
    match share::program_from_hash(hash) {
        None => SharedLinkDecision::Unreadable,
        Some(source) => {
            let ask = autosave::confirm_needed(current, &source);
            SharedLinkDecision::Load { source, ask }
        }
    }
}

/// Starts fresh from `shared`, for both New (`start_over`, with
/// `DEFAULT_MMS`) and a shared link's load (`Msg::CheckSharedLink`):
/// testable without a live `Context`. Builds a fresh `Control` from
/// `shared` -- so no breakpoint from the replaced program carries over --
/// and, on a parse error, falls back to `DEFAULT_MMS`'s own machine,
/// showing `shared`'s error exactly as a restored program shows one
/// (`restore_source`). The caller assigns `shared` as the editor's new
/// text and saves it; `shared` isn't returned here since it's already the
/// caller's own, owned string.
fn start_fresh(
    chunk_timeout: &mut Option<Timeout>,
    debounce_timeout: &mut Option<Timeout>,
    control: &mut Control,
    error: &mut Option<String>,
    error_line: &mut Option<usize>,
    view_state: &mut ViewState,
    shared: &str,
) {
    *chunk_timeout = None;
    *debounce_timeout = None;
    match Control::new(shared, SOURCE_FILENAME) {
        Ok(fresh) => {
            *control = fresh;
            *error = None;
            *error_line = None;
        }
        Err(err) => {
            *control = default_control();
            *error_line = parse_error_location(&err).map(|(line, _)| line);
            *error = Some(describe_source_error(shared, &err));
        }
    }
    view_state.reset(control);
}

/// The page's own URL with no fragment: `origin + pathname + search`. The
/// one page identity a stripped share link and a link Share builds must
/// agree on. `None` on any failure to read `Location`'s parts -- both
/// callers treat that as nothing to do.
fn page_url() -> Option<String> {
    let window = web_sys::window()?;
    let location = window.location();
    let origin = location.origin().ok()?;
    let pathname = location.pathname().ok()?;
    let search = location.search().ok()?;
    Some(format!("{origin}{pathname}{search}"))
}

/// Replaces the current URL with the same URL minus its fragment: once a
/// share link is handled -- loaded, or found unreadable -- autosave
/// already owns the program, and a reload must not load the same link
/// again. Best-effort: any failure leaves the fragment in the address bar,
/// no worse off than before this ran.
fn strip_fragment() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(history) = window.history() else {
        return;
    };
    let Some(url) = page_url() else {
        return;
    };
    let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url));
}

/// Whether `object` has a property named `name` -- checked before calling a
/// method that might not exist (`Navigator.share`, `Navigator.clipboard`),
/// since calling an absent method throws across the wasm boundary.
fn has_property(object: &impl AsRef<JsValue>, name: &str) -> bool {
    js_sys::Reflect::has(object.as_ref(), &JsValue::from_str(name)).unwrap_or(false)
}

/// Attaches `promise`'s settlement to `Msg::ShareSettled`: `on_resolve` on
/// success. On rejection, reads the error's `name` (`js_sys::Reflect::get`)
/// and asks `on_reject` for the status to show, or `None` to leave the
/// status unchanged (an `AbortError` -- the user dismissed the share
/// sheet). Each closure is one-shot, converted straight into the `JsValue`
/// `.then` takes (`Closure::once_into_js`): the one that fires is freed on
/// invocation; the other leaks, captures included, since a settled promise
/// never calls it. `.then` is fetched through `js_sys::Reflect` and called
/// through `Function::call2`, since `Promise::then2` only takes the typed
/// closures `ScopedClosure` wraps, not a bare `JsValue`.
fn watch_share_settlement(
    link: Scope<App>,
    promise: Promise,
    on_resolve: &'static str,
    on_reject: impl Fn(&str) -> Option<&'static str> + 'static,
) {
    let resolve_link = link.clone();
    let resolve = Closure::once_into_js(move |_value: JsValue| {
        resolve_link.send_message(Msg::ShareSettled(on_resolve.to_string()));
    });
    let reject = Closure::once_into_js(move |value: JsValue| {
        let name = js_sys::Reflect::get(&value, &JsValue::from_str("name"))
            .ok()
            .and_then(|name| name.as_string())
            .unwrap_or_default();
        if let Some(status) = on_reject(&name) {
            link.send_message(Msg::ShareSettled(status.to_string()));
        }
    });
    let then = js_sys::Reflect::get(promise.as_ref(), &JsValue::from_str("then"))
        .ok()
        .and_then(|value| value.dyn_into::<js_sys::Function>().ok());
    if let Some(then) = then {
        let _ = then.call2(promise.as_ref(), &resolve, &reject);
    }
}

/// The Share button's onclick core: builds the link from `source` and the
/// current page, then shares or copies it. Runs directly
/// in the click handler, not through a dispatched `Msg` first -- Safari
/// grants `share`/`clipboard` only inside the user gesture, which a
/// message round-tripped through Yew's update queue would already have
/// left. Reports the settled status back through `link`.
fn share_program(link: Scope<App>, source: &str) {
    const COULDNT_SHARE: &str = "Couldn't share the link";
    const COULDNT_COPY: &str = "Couldn't copy the link";

    let (Some(page), Some(navigator)) = (page_url(), web_sys::window().map(|w| w.navigator()))
    else {
        link.send_message(Msg::ShareSettled(COULDNT_SHARE.to_string()));
        return;
    };
    let url = share::share_url(&page, source);

    if has_property(&navigator, "share") {
        let data = ShareData::new();
        data.set_url(&url);
        data.set_title("playmmix program");
        let promise = navigator.share_with_data(&data);
        watch_share_settlement(link, promise, "Shared", |name| {
            (name != "AbortError").then_some(COULDNT_SHARE)
        });
        return;
    }

    if has_property(&navigator, "clipboard") {
        let promise = navigator.clipboard().write_text(&url);
        watch_share_settlement(link, promise, "Link copied", |_| Some(COULDNT_COPY));
        return;
    }

    link.send_message(Msg::ShareSettled(COULDNT_COPY.to_string()));
}

/// `Msg::Run`'s restart-and-run core, factored out for the same reason
/// `reload_and_record` is: testable without a live `Context`. Always
/// restarts through `reload_and_record` -- Reset's own path -- so Run and
/// Reset can never land in different states: a program mid-run, or paused
/// at a breakpoint, restarts exactly as one freshly loaded does.
///
/// Returns whether a session existed before this call, the restart signal
/// `App::advance_chunk` composes onto this Run's own terminal outcome
/// (`Restarted · <outcome>`, via `restarted_status`) -- never set when
/// there was no session to restart from. `false` on a parse error too:
/// nothing will run for this signal to reach.
///
/// Thin wrapper over [`restart_and_run_with`], reading whether a debounce is
/// actually pending off `debounce_timeout` -- the one thing a host test can't
/// drive directly, since a real `Timeout` can't be constructed off the wasm
/// target.
fn restart_and_run(
    chunk_timeout: &mut Option<Timeout>,
    debounce_timeout: &mut Option<Timeout>,
    control: &mut Control,
    source: &str,
    error: &mut Option<String>,
    error_line: &mut Option<usize>,
    view_state: &mut ViewState,
) -> bool {
    let debounce_pending = debounce_timeout.is_some();
    *debounce_timeout = None;
    restart_and_run_with(
        debounce_pending,
        chunk_timeout,
        control,
        source,
        error,
        error_line,
        view_state,
    )
}

/// `Msg::Run`'s core, factored out for the same reason `continue_pressed`
/// is: testable without a live `Context`, and the guard against a Run
/// already in flight lives here, not inline in `update`, so a test can
/// drive it directly. `None` -- nothing touched, `restart_and_run` never
/// called -- while a Run is already in flight: nothing here should
/// re-restart a running program out from under itself, and a no-op must
/// leave a restarting Run's own `Restarted ·` untouched. `Some(had_session)`
/// otherwise, via [`restart_and_run`].
fn run_pressed(
    chunk_timeout: &mut Option<Timeout>,
    debounce_timeout: &mut Option<Timeout>,
    control: &mut Control,
    source: &str,
    error: &mut Option<String>,
    error_line: &mut Option<usize>,
    view_state: &mut ViewState,
) -> Option<bool> {
    if control.is_running() {
        return None;
    }
    Some(restart_and_run(
        chunk_timeout,
        debounce_timeout,
        control,
        source,
        error,
        error_line,
        view_state,
    ))
}

/// [`restart_and_run`]'s testable core, parameterized on `debounce_pending`
/// rather than reading it off a real `Timeout`.
///
/// A pending debounce means the shown source has already outrun the loaded
/// one -- `had_session` must read the same whether the debounce had already
/// fired, Ctrl-S had already flushed it, or neither has happened yet and this
/// Run's own reload below is what settles it: all three end with the same
/// "was there a session before *this edit's own* reload" answer, `false`.
/// Reading `control.session()` only when nothing is pending is what makes
/// the three converge, without a second reload just to force the read: one
/// reload, always -- this Run's own, the only one that ever runs.
fn restart_and_run_with(
    debounce_pending: bool,
    chunk_timeout: &mut Option<Timeout>,
    control: &mut Control,
    source: &str,
    error: &mut Option<String>,
    error_line: &mut Option<usize>,
    view_state: &mut ViewState,
) -> bool {
    let had_session = !debounce_pending && control.session();
    reload_and_record(
        chunk_timeout,
        control,
        source,
        error,
        error_line,
        view_state,
    );
    if error.is_some() {
        return false;
    }
    view_state.clear_changed();
    control.start_run();
    view_state.observe(control);
    had_session
}

/// `Msg::FlushSource`'s core logic, factored out for the same reason
/// `interrupt_if_running` is: testable without a live `Context`. Ctrl-S's whole
/// reason to exist is the window before `SOURCE_DEBOUNCE_MS` catches up on
/// its own, so this only acts when a debounce is actually pending --
/// dropping it (which cancels the pending `setTimeout`, `Timeout`'s `Drop`)
/// before reloading, so `Msg::ReassembleSource` cannot also fire afterward
/// and reset `view_state` a second time. Returns whether anything was
/// pending; a `false` return is a genuine no-op, not a failure.
///
/// `debounce_timeout` and `chunk_timeout` are deliberately not adjacent
/// parameters -- both are `&mut Option<Timeout>`, and a swap between two
/// same-typed neighbors compiles silently. A real `Timeout` can't be
/// constructed off the wasm target to test this directly (`Timeout::new`
/// aborts the process on the host), so this ordering is the mitigation.
fn flush_pending_source(
    debounce_timeout: &mut Option<Timeout>,
    control: &mut Control,
    chunk_timeout: &mut Option<Timeout>,
    source: &str,
    error: &mut Option<String>,
    error_line: &mut Option<usize>,
    view_state: &mut ViewState,
) -> bool {
    if debounce_timeout.is_none() {
        return false;
    }
    *debounce_timeout = None;
    reload_and_record(
        chunk_timeout,
        control,
        source,
        error,
        error_line,
        view_state,
    );
    true
}

pub enum Msg {
    SourceChanged(String),
    /// Fires once `SOURCE_DEBOUNCE_MS` has passed with no further
    /// `SourceChanged` -- re-assembles `self.source` and updates
    /// `self.error` accordingly. Carries no payload: `self.source` is
    /// already current by the time this arrives.
    ReassembleSource,
    /// Ctrl-S / Cmd-S: flush a pending debounced re-assemble immediately.
    /// A no-op when nothing is pending -- see `flush_pending_source`.
    FlushSource,
    ToggleBreakpoint(usize),
    Run,
    Continue,
    Step,
    Next,
    Interrupt,
    Reset,
    /// The header's New button: start over from `DEFAULT_MMS`, confirming
    /// first when the editor holds a program that differs from the
    /// skeleton (`autosave::confirm_needed`).
    New,
    /// Checks the URL fragment for a shared program: sent once after the
    /// first paint and on every `hashchange`, since pasting a link into an
    /// open tab changes only the fragment and reloads nothing.
    CheckSharedLink,
    /// The Share button's `navigator.share`/clipboard call has settled;
    /// carries the status text to show.
    ShareSettled(String),
    /// One chunk boundary: reschedule if the run isn't finished, or if an
    /// `Interrupt` landed while this tick was scheduled, do nothing.
    ChunkTick,
    /// The column splitter's drag committed, carrying the already-clamped
    /// left-column width.
    ColumnResized(f64),
    /// The row splitter's drag committed, carrying the already-clamped
    /// output-pane height.
    RowResized(f64),
}

pub struct App {
    source: String,
    control: Control,
    error: Option<String>,
    /// The 1-based source line a parsed `self.error` location names, if the
    /// raw error text carried one (see `parse_error_location`). Updated at
    /// the same points `self.error` itself is; stale during the same
    /// debounce window `self.error` is already accepted to be stale in
    /// (`Msg::SourceChanged` clears neither).
    error_line: Option<usize>,
    /// The pending chunk-tick timeout, if a chunked Run, Continue, or Next
    /// is in flight. Held rather than `.forget()`-ten so Interrupt, or a
    /// `reload` mid-run, can cancel it by dropping this (runs `clearTimeout`
    /// and frees the closure) instead of leaking one allocation per chunk
    /// boundary.
    chunk_timeout: Option<Timeout>,
    /// The pending `Msg::ReassembleSource` timeout, if a keystroke's
    /// re-assemble is still waiting out `SOURCE_DEBOUNCE_MS`. Held for the
    /// same reason as `chunk_timeout`: dropping it (a further keystroke, or
    /// unmounting) cancels the pending `setTimeout` instead of leaking it.
    debounce_timeout: Option<Timeout>,
    /// Cross-render register/special visibility and the pause-boundary diff
    /// snapshot -- view state owned here (not on `Control`, not derived
    /// from `&MMix` alone) per `docs/layout-spec.md`'s Registers section.
    view_state: ViewState,
    /// A short echo of the last action taken, rendered next to the Control
    /// Bar (§1.4). Left unchanged by a parse error -- the error itself is
    /// already shown prominently elsewhere. Owned (not `&'static str`):
    /// `restarted_status` composes a `Restarted · ` prefix onto it.
    status_message: String,
    /// Whether the Run in flight (or about to be) restarted an existing
    /// session -- set from `restart_and_run`'s own return in `Msg::Run`,
    /// composed onto the next terminal chunk outcome by `advance_chunk`
    /// (`Restarted · <outcome>`, via `restarted_status`) and cleared there,
    /// so it reaches exactly that one outcome and no later one. Interrupt,
    /// Continue, Step, Next, and any reload also clear it, so a later
    /// command's own outcome never reads `Restarted`.
    restart_signal: bool,
    /// The drag-set left-column width and output-pane height, pixels.
    /// `None` means "use the stylesheet's default sizing" -- a page that's
    /// never been dragged renders identically to before (§1.3). No
    /// persistence: both reset to the stylesheet defaults on reload.
    left_column_width: Option<f64>,
    output_height: Option<f64>,
    main_ref: NodeRef,
    /// Spans the column-splitter's own track (editor/row-splitter/output
    /// rows, column 2) -- its `client_height()` is the row splitter's
    /// ceiling parameter.
    col_splitter_ref: NodeRef,
    /// Lives in the left column (its own row, column 1) -- its
    /// `client_width()` is the column splitter's drag-start left-column
    /// width.
    row_splitter_ref: NodeRef,
    /// The output pane's own root element -- its `client_height()` is the
    /// row splitter's drag-start output height (finding 2: this must be a
    /// live DOM read, not a stylesheet-default constant, since the output
    /// pane's undragged height is content-driven, capped by `max-height`
    /// rather than fixed to it).
    output_pane_ref: NodeRef,
    /// Live drag state, shared with both splitters' pointer-event closures.
    /// A persistent field, not a value `view()` creates fresh each render:
    /// an unrelated re-render mid-drag (a `Msg::ChunkTick` from a Run in
    /// the background, say) rebuilds every closure in `view()` with a new
    /// clone of whatever this holds, so a fresh, empty cell here would
    /// silently drop an in-flight drag the moment that happened.
    drag_state: Rc<RefCell<Option<DragState>>>,
    /// Whether `window.onbeforeunload`'s handler (`_beforeunload_handler`)
    /// currently arms the native confirmation dialog -- `leave_warning_needed`,
    /// re-evaluated at each save (`Msg::SourceChanged`, `Msg::New`, and a
    /// shared program loading in `Msg::CheckSharedLink`, the only three save
    /// points). Shared with that handler rather than read from `self`
    /// directly: the handler is a `'static` JS closure, registered once at
    /// `create` and outliving any single `view()`/`update()` call.
    leave_warning_armed: Rc<RefCell<bool>>,
    /// Kept alive for as long as `App` is -- dropping a `Closure` frees the
    /// JS function it backs, which would leave `window.onbeforeunload`
    /// pointing at freed memory. Never read directly; `leave_warning_armed`
    /// is the live channel to it.
    _beforeunload_handler: BeforeUnloadHandler,
    /// Live enablement `window.onkeydown`'s handler reads on every keydown --
    /// kept current by the end of `update`, since the handler itself runs
    /// outside any render and so can't call `control_enablement` against
    /// fresh state directly. Refreshed from `update`, not `rendered`: Yew
    /// 0.23's scheduler (`run_scheduler`'s `can_yield` ignores the rendered
    /// queue while `fill_queue` runs updates first) can starve `rendered` for
    /// the whole span of a chunked Run/Continue/Next, since `ChunkTick`
    /// messages keep the update queue non-empty -- the Interrupt shortcut
    /// stopped firing mid-run before this moved.
    shortcut_enablement: Rc<Cell<ControlEnablement>>,
    /// Kept alive for as long as `App` is, same reason as
    /// `_beforeunload_handler`. Never read directly.
    _keydown_handler: KeydownHandler,
    /// Kept alive for as long as `App` is, same reason as
    /// `_beforeunload_handler`: dropping it would unregister
    /// `window.onhashchange`, the handler pasting a share link into an
    /// open tab relies on. Never read directly.
    _hashchange_handler: HashchangeHandler,
    /// Counts execution stops: increments once whenever Step, Next, Run, or
    /// Continue leaves the machine stopped, an Interrupt actually
    /// interrupts, or a Reset's reload succeeds. An edit, a re-assemble,
    /// New, and a loaded program never increment it -- nor does loading a
    /// shared link (`Msg::CheckSharedLink`), the same "a loaded program
    /// never increments" rule `restore_source` already follows. Passed to
    /// `Editor`, which compares it against the count it last saw to decide
    /// whether to scroll the current line into view (`docs/layout-spec.md`'s
    /// Highlights).
    execution_stops: u64,
}

impl App {
    /// Yield to the event loop, then deliver `Msg::ChunkTick` -- the one
    /// place a chunked Run, Continue, or Next reschedules itself. Replaces
    /// `chunk_timeout`, dropping (and so cancelling) any tick already
    /// pending.
    fn schedule_chunk_tick(&mut self, ctx: &Context<Self>) {
        let link = ctx.link().clone();
        self.chunk_timeout = Some(yield_to_event_loop(move || {
            link.send_message(Msg::ChunkTick)
        }));
    }

    /// Advance one chunk of whichever operation -- Run, Continue, or Next --
    /// is in flight, rescheduling if `advance_chunk_once` says it isn't
    /// finished. The scheduling policy for `Msg::ChunkTick`; the state
    /// update itself lives in `advance_chunk_once`, the plain seam a test
    /// drives directly.
    fn advance_chunk(&mut self, ctx: &Context<Self>) {
        // An Interrupt is checked only between chunks, never inside one;
        // this is that check. Cancelling `chunk_timeout` on Interrupt
        // already prevents this from firing in the normal case -- this
        // guard is a defensive backstop, not the primary safety mechanism.
        if !self.control.is_running() {
            return;
        }
        let (status, needs_tick) = advance_chunk_once(
            &mut self.control,
            &mut self.view_state,
            &mut self.restart_signal,
            &mut self.execution_stops,
        );
        self.status_message = status;
        if needs_tick {
            self.schedule_chunk_tick(ctx);
        }
    }

    /// Saves `self.source` and re-arms the leave-page warning from the
    /// result (`leave_warning_needed`) -- the one path every save takes,
    /// whether from an edit (`Msg::SourceChanged`), from New, or from
    /// loading a shared program.
    fn save_and_arm_warning(&mut self) {
        let saved = autosave::save(&self.source);
        *self.leave_warning_armed.borrow_mut() = leave_warning_needed(saved, &self.source);
    }

    /// Reload the current source -- Reset's own step, and the start state
    /// every restarting Run reuses (see `restart_and_run`): cancel any
    /// pending chunk tick and any pending debounced re-assemble (this
    /// reload supersedes both), re-run `Control::reload`, and on success
    /// reseed the continuity/snapshot state. On a parse error, `reload`
    /// already leaves the previous machine and everything else untouched,
    /// so only `self.error` moves. Clears `restart_signal`: this is a
    /// reload, and every reload clears it.
    fn reload_source(&mut self) {
        self.debounce_timeout = None;
        self.restart_signal = false;
        reload_and_record(
            &mut self.chunk_timeout,
            &mut self.control,
            &self.source,
            &mut self.error,
            &mut self.error_line,
            &mut self.view_state,
        );
    }

    /// Flush a debounced re-assemble that hasn't fired yet, so Continue,
    /// Step, and Next can never execute a program older than `self.source`
    /// -- `SOURCE_DEBOUNCE_MS` otherwise leaves a window where every
    /// control still reads enabled against the stale prior program. Run
    /// never calls this: its own restart always reloads `self.source`
    /// directly. Returns whether the caller may proceed: `false` once the
    /// flush surfaces a parse error, since the edit that is pending is not a
    /// program that can run.
    fn flush_pending_reassemble(&mut self) -> bool {
        if self.debounce_timeout.is_some() {
            self.reload_source();
        }
        self.error.is_none()
    }
}

impl Component for App {
    type Message = Msg;
    type Properties = ();

    fn create(_ctx: &Context<Self>) -> Self {
        let mut control = default_control();
        let (leave_warning_armed, beforeunload_handler) = install_beforeunload_handler();
        let (shortcut_enablement, keydown_handler) =
            install_keyboard_shortcuts(_ctx.link().clone());
        let hashchange_handler = install_hashchange_handler(_ctx.link().clone());

        // Restore a saved program, if any: the machine above already holds
        // `DEFAULT_MMS`, so a saved program that fails to assemble leaves
        // it there, showing its error exactly as an edit would.
        let mut chunk_timeout = None;
        let mut error = None;
        let mut error_line = None;
        let mut view_state = ViewState::new();
        let source = restore_source(
            autosave::load(),
            &mut chunk_timeout,
            &mut control,
            &mut error,
            &mut error_line,
            &mut view_state,
        );

        let mut app = Self {
            source,
            control,
            error,
            error_line,
            chunk_timeout,
            debounce_timeout: None,
            view_state,
            status_message: "Loaded".to_string(),
            restart_signal: false,
            left_column_width: None,
            output_height: None,
            main_ref: NodeRef::default(),
            col_splitter_ref: NodeRef::default(),
            row_splitter_ref: NodeRef::default(),
            output_pane_ref: NodeRef::default(),
            drag_state: Rc::new(RefCell::new(None)),
            leave_warning_armed,
            _beforeunload_handler: beforeunload_handler,
            shortcut_enablement,
            _keydown_handler: keydown_handler,
            _hashchange_handler: hashchange_handler,
            execution_stops: 0,
        };
        // Seed continuity and the diff baseline off the freshly loaded
        // machine -- not about the first render (`visible_registers`
        // already computes the correct set live), but about stickiness: a
        // register visible only at load must already be in the sticky set
        // before the first `step()` runs.
        app.view_state.reset(&app.control);
        app
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        // No arm below returns early: the render decision is always this
        // match's own tail value, never a `return`, so `shortcut_enablement`
        // below is refreshed on every message, unconditionally -- the fix
        // for the field's own starvation bug (see its doc comment).
        let should_render = match msg {
            Msg::SourceChanged(source) => {
                // Re-assembling (and showing a resulting parse error) is
                // debounced to `Msg::ReassembleSource` -- see
                // `SOURCE_DEBOUNCE_MS` -- so typing an in-progress line
                // doesn't flash "Assembly error" on every keystroke. A run,
                // Continue, or chunked Next in flight still ends
                // immediately: the source shown alongside it is already no
                // longer the one that produced it. Ends any pending restart
                // signal too, the same as an actual reload: an interrupted
                // Run's chunk sequence never reaches its own terminal
                // outcome.
                self.source = source;
                self.save_and_arm_warning();
                self.control.end_in_flight();
                self.chunk_timeout = None;
                self.restart_signal = false;
                let link = ctx.link().clone();
                self.debounce_timeout = Some(Timeout::new(SOURCE_DEBOUNCE_MS, move || {
                    link.send_message(Msg::ReassembleSource)
                }));
                true
            }
            Msg::ReassembleSource => {
                self.debounce_timeout = None;
                self.restart_signal = false;
                match self.control.reload(&self.source) {
                    Ok(()) => {
                        self.error = None;
                        self.error_line = None;
                        self.view_state.reset(&self.control);
                        self.status_message = "Loaded".to_string();
                    }
                    Err(error) => {
                        self.error_line = parse_error_location(&error).map(|(line, _)| line);
                        self.error = Some(describe_source_error(&self.source, &error));
                    }
                }
                true
            }
            Msg::ToggleBreakpoint(line) => {
                // `toggle_breakpoint`'s bool return only says
                // succeeded-or-not, not which direction -- check
                // membership first to know which of the three status texts
                // applies.
                let was_set = self.control.breakpoint_lines().contains(&line);
                let toggled = self.control.toggle_breakpoint(line);
                self.status_message = if !toggled {
                    "No code on that line"
                } else if was_set {
                    "Breakpoint cleared"
                } else {
                    "Breakpoint set"
                }
                .to_string();
                true
            }
            Msg::Run => {
                // Run always restarts, through Reset's own path -- a
                // pending debounce is superseded by that restart, not
                // flushed separately. `run_pressed` holds the guard against
                // a Run already in flight.
                match run_pressed(
                    &mut self.chunk_timeout,
                    &mut self.debounce_timeout,
                    &mut self.control,
                    &self.source,
                    &mut self.error,
                    &mut self.error_line,
                    &mut self.view_state,
                ) {
                    None => false,
                    Some(restarted) => {
                        if self.error.is_some() {
                            true
                        } else {
                            self.restart_signal = restarted;
                            if self.control.is_running() {
                                self.schedule_chunk_tick(ctx);
                            }
                            self.status_message = "Running".to_string();
                            true
                        }
                    }
                }
            }
            Msg::Continue => {
                if !self.flush_pending_reassemble() {
                    true
                } else {
                    // `continue_pressed` is `None` when nothing is running
                    // but no session has started either -- the flush above
                    // may itself have reloaded, ending the session Continue
                    // depended on, per the owner's settled decision. A no-op
                    // then: `restart_signal` and `status_message` are only
                    // touched once Continue actually proceeds, so a no-op
                    // Continue never wipes a restarting Run's own
                    // `Restarted ·`.
                    if let Some((status, needs_tick)) = continue_pressed(
                        &mut self.control,
                        &mut self.view_state,
                        &mut self.execution_stops,
                    ) {
                        self.restart_signal = false;
                        self.status_message = status;
                        if needs_tick {
                            self.schedule_chunk_tick(ctx);
                        }
                    }
                    true
                }
            }
            Msg::Step => {
                if !self.flush_pending_reassemble() {
                    true
                } else {
                    if let Some(outcome) = step_pressed(
                        &mut self.control,
                        &mut self.view_state,
                        &mut self.restart_signal,
                        &mut self.execution_stops,
                    ) {
                        self.status_message = if outcome == StepOutcome::Halted {
                            "Halted"
                        } else {
                            "Stepped"
                        }
                        .to_string();
                    }
                    true
                }
            }
            Msg::Next => {
                if !self.flush_pending_reassemble() {
                    true
                } else {
                    if !self.control.is_running() {
                        self.restart_signal = false;
                        self.view_state.clear_changed();
                        let outcome = self.control.next_chunk(control::CHUNK_BUDGET);
                        let (status, needs_tick) = first_chunk_outcome(
                            &mut self.control,
                            &mut self.view_state,
                            outcome,
                            &mut self.execution_stops,
                        );
                        self.status_message = status;
                        if needs_tick {
                            self.schedule_chunk_tick(ctx);
                        }
                    }
                    true
                }
            }
            Msg::Interrupt => {
                self.restart_signal = false;
                if interrupt_if_running(
                    &mut self.control,
                    &mut self.view_state,
                    &mut self.execution_stops,
                ) {
                    self.chunk_timeout = None;
                    self.status_message = "Interrupted".to_string();
                    true
                } else {
                    false
                }
            }
            Msg::Reset => {
                self.reload_source();
                if self.error.is_none() {
                    self.execution_stops += 1;
                    self.status_message = "Reset".to_string();
                }
                true
            }
            Msg::New => {
                if !confirm_new(&self.source, DEFAULT_MMS) {
                    false
                } else {
                    self.source = start_over(
                        &mut self.chunk_timeout,
                        &mut self.debounce_timeout,
                        &mut self.control,
                        &mut self.error,
                        &mut self.error_line,
                        &mut self.view_state,
                    );
                    self.restart_signal = false;
                    self.status_message = "Loaded".to_string();
                    self.save_and_arm_warning();
                    true
                }
            }
            Msg::CheckSharedLink => {
                let hash = web_sys::window()
                    .and_then(|window| window.location().hash().ok())
                    .unwrap_or_default();
                match decide_shared_link(&hash, &self.source) {
                    SharedLinkDecision::Ignore => false,
                    SharedLinkDecision::Unreadable => {
                        self.status_message = SHARE_UNREADABLE_STATUS.to_string();
                        strip_fragment();
                        true
                    }
                    SharedLinkDecision::Load { source, ask } => {
                        let proceeds = !ask || confirm(SHARE_LOAD_CONFIRM_MESSAGE);
                        if !proceeds {
                            false
                        } else {
                            start_fresh(
                                &mut self.chunk_timeout,
                                &mut self.debounce_timeout,
                                &mut self.control,
                                &mut self.error,
                                &mut self.error_line,
                                &mut self.view_state,
                                &source,
                            );
                            self.source = source;
                            self.restart_signal = false;
                            self.status_message = SHARE_LOADED_STATUS.to_string();
                            self.save_and_arm_warning();
                            strip_fragment();
                            true
                        }
                    }
                }
            }
            Msg::ShareSettled(status) => {
                self.status_message = status;
                true
            }
            Msg::FlushSource => {
                let flushed = flush_pending_source(
                    &mut self.debounce_timeout,
                    &mut self.control,
                    &mut self.chunk_timeout,
                    &self.source,
                    &mut self.error,
                    &mut self.error_line,
                    &mut self.view_state,
                );
                if flushed {
                    self.restart_signal = false;
                    if self.error.is_none() {
                        self.status_message = "Loaded".to_string();
                    }
                    true
                } else {
                    false
                }
            }
            Msg::ChunkTick => {
                self.advance_chunk(ctx);
                true
            }
            Msg::ColumnResized(width) => {
                self.left_column_width = Some(width);
                true
            }
            Msg::RowResized(height) => {
                self.output_height = Some(height);
                true
            }
        };
        // `window.onkeydown`'s choke point -- see `shortcut_enablement`'s own
        // doc comment for why this lives here, at the end of every `update`,
        // rather than in `rendered`.
        self.shortcut_enablement.set(control_enablement(
            self.control.is_running(),
            self.control.is_halted(),
            self.control.session(),
            self.error.is_some(),
        ));
        should_render
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let exit_code = self
            .control
            .is_halted()
            .then(|| self.control.machine().get_exit_code());

        let machine_view = match &self.error {
            Some(error) => html! { <pre>{ error.clone() }</pre> },
            None => {
                let (registers, specials, memory) = self.view_state.machine_rows(&self.control);
                html! {
                    <MachinePane
                        {registers}
                        {specials}
                        {memory}
                        pc={self.control.get_pc()}
                        marker_pc={self.control.marker_pc()}
                        {exit_code}
                        call_depth={self.control.call_depth()}
                        changed_registers={self.view_state.changed_registers().clone()}
                        changed_specials={self.view_state.changed_specials().clone()}
                        changed_memory={self.view_state.changed_memory().clone()}
                    />
                }
            }
        };

        let on_change = ctx.link().callback(Msg::SourceChanged);
        let on_toggle_breakpoint = ctx.link().callback(Msg::ToggleBreakpoint);
        let on_run = ctx.link().callback(|()| Msg::Run);
        let on_continue = ctx.link().callback(|()| Msg::Continue);
        let on_step = ctx.link().callback(|()| Msg::Step);
        let on_next = ctx.link().callback(|()| Msg::Next);
        let on_interrupt = ctx.link().callback(|()| Msg::Interrupt);
        let on_reset = ctx.link().callback(|()| Msg::Reset);
        let on_new = ctx.link().callback(|_| Msg::New);
        // Runs `share_program` directly, not through a dispatched `Msg`:
        // Safari grants `share`/`clipboard` only inside the click's own
        // gesture, which a round trip through Yew's update queue would
        // already have left.
        let on_share = {
            let link = ctx.link().clone();
            let source = self.source.clone();
            Callback::from(move |_: MouseEvent| share_program(link.clone(), &source))
        };

        // The PC indicator only means something while nothing is actively
        // moving it; showing it mid-run would flicker with every chunk.
        let current_line = (!self.control.is_running())
            .then(|| self.control.current_line())
            .flatten();

        let committed_sizes = CommittedSizes {
            left_column_width: self.left_column_width,
            output_height: self.output_height,
        };
        let (col_onpointerdown, col_onpointermove, col_onpointerup, col_onpointercancel) =
            column_splitter_handlers(
                self.drag_state.clone(),
                self.main_ref.clone(),
                self.col_splitter_ref.clone(),
                self.row_splitter_ref.clone(),
                committed_sizes,
                ctx.link().callback(Msg::ColumnResized),
            );
        let (row_onpointerdown, row_onpointermove, row_onpointerup, row_onpointercancel) =
            row_splitter_handlers(
                self.drag_state.clone(),
                self.main_ref.clone(),
                self.row_splitter_ref.clone(),
                self.col_splitter_ref.clone(),
                self.output_pane_ref.clone(),
                committed_sizes,
                ctx.link().callback(Msg::RowResized),
            );

        let main_style_attr = {
            let style = main_style(self.left_column_width, self.output_height, false);
            (!style.is_empty()).then_some(style)
        };

        html! {
            <main ref={self.main_ref.clone()} style={main_style_attr}>
                <div class="app-header">
                    <h1>{ "playmmix" }</h1>
                    <button
                        class="header-button"
                        onclick={on_new}
                        title="New: start over from the minimal skeleton"
                    >
                        { "New" }
                    </button>
                    <button
                        class="header-button"
                        onclick={on_share}
                        title="Share a link that opens this program"
                    >
                        { "Share" }
                    </button>
                    <ControlBar
                        running={self.control.is_running()}
                        halted={self.control.is_halted()}
                        session={self.control.session()}
                        has_error={self.error.is_some()}
                        {on_run}
                        {on_continue}
                        {on_step}
                        {on_next}
                        {on_interrupt}
                        {on_reset}
                        status={self.status_message.clone()}
                    />
                </div>
                <Editor
                    source={self.source.clone()}
                    {on_change}
                    breakpoints={self.control.breakpoint_lines().clone()}
                    {current_line}
                    execution_stops={self.execution_stops}
                    error_line={self.error_line}
                    {on_toggle_breakpoint}
                />
                <div
                    class="row-splitter"
                    ref={self.row_splitter_ref.clone()}
                    onpointerdown={row_onpointerdown}
                    onpointermove={row_onpointermove}
                    onpointerup={row_onpointerup}
                    onpointercancel={row_onpointercancel}
                />
                <div
                    class="col-splitter"
                    ref={self.col_splitter_ref.clone()}
                    onpointerdown={col_onpointerdown}
                    onpointermove={col_onpointermove}
                    onpointerup={col_onpointerup}
                    onpointercancel={col_onpointercancel}
                />
                <OutputPane
                    spans={self.control.output()}
                    {exit_code}
                    pane_ref={self.output_pane_ref.clone()}
                />
                <div class="machine-slot">{ machine_view }</div>
            </main>
        }
    }

    /// Schedules the shared-link check once, after the first render's own
    /// task: `rendered` runs synchronously within that task, so a
    /// zero-delay `Timeout` runs in a later task than it. `hashchange`
    /// (`install_hashchange_handler`) takes the same path for every
    /// fragment change after this first one. Leaked (`forget`): nothing
    /// needs to cancel this one-shot timer.
    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        if first_render {
            let link = ctx.link().clone();
            Timeout::new(0, move || link.send_message(Msg::CheckSharedLink)).forget();
        }
    }
}

fn main() {
    wasm_logger::init(wasm_logger::Config::default());
    info!("Starting playmmix");
    Renderer::<App>::new().render();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_for_covers_every_step_outcome() {
        assert_eq!(status_for(StepOutcome::BudgetExhausted), "Running");
        assert_eq!(status_for(StepOutcome::Advanced), "Stepped over");
        assert_eq!(status_for(StepOutcome::Halted), "Halted");
        assert_eq!(status_for(StepOutcome::Breakpoint(0x100)), "Hit breakpoint");
    }

    /// A straight-line program -- no loop, so a Run from the current PC can
    /// never reach a breakpoint past the entry again once the PC has moved
    /// beyond it.
    const RESTART_STRAIGHT_LINE_MMS: &str =
        "\tLOC\t#100\nMain\tSETL\t$1,1\n\tSETL\t$2,2\n\tSETL\t$3,3\n\tTRAP\t0,Halt,0\n";

    /// Drives `restart_and_run` on `control`, then `resume_chunk` to a
    /// terminal outcome -- the plain seam `Msg::Run` and `App::advance_chunk`
    /// together drive, without a live `Context`.
    fn restart_then_drive_to_terminal(control: &mut Control) -> (bool, StepOutcome) {
        let mut chunk_timeout = None;
        let mut debounce_timeout = None;
        let mut error = None;
        let mut error_line = None;
        let mut view_state = ViewState::new();
        view_state.reset(control);
        let had_session = restart_and_run(
            &mut chunk_timeout,
            &mut debounce_timeout,
            control,
            RESTART_STRAIGHT_LINE_MMS,
            &mut error,
            &mut error_line,
            &mut view_state,
        );
        assert!(error.is_none(), "fixture must still assemble");
        let mut outcome = control.resume_chunk(control::CHUNK_BUDGET);
        while outcome == StepOutcome::BudgetExhausted {
            outcome = control.resume_chunk(control::CHUNK_BUDGET);
        }
        (had_session, outcome)
    }

    #[test]
    fn restart_and_run_always_restarts_through_the_shared_start_state() {
        // From paused: Step past the breakpointed line (line 3) -- a Run
        // from the current PC could never reach it again in this
        // straight-line program.
        let mut from_paused =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "restart-paused.mms").expect("assembles");
        assert!(from_paused.toggle_breakpoint(3), "line 3 has an address");
        assert_eq!(from_paused.step(), StepOutcome::Advanced); // line 2
        assert_eq!(from_paused.step(), StepOutcome::Advanced); // line 3, past it
        assert!(from_paused.session());

        let (had_session, outcome) = restart_then_drive_to_terminal(&mut from_paused);
        assert!(had_session, "a session existed before this Run");
        assert!(
            matches!(outcome, StepOutcome::Breakpoint(_)),
            "Run must have restarted to hit the breakpoint again: {outcome:?}"
        );

        // From halted: run the whole program to completion (breakpoint not
        // yet set), then arm it before restarting.
        let mut from_halted =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "restart-halted.mms").expect("assembles");
        assert_eq!(
            from_halted.run_chunk(control::CHUNK_BUDGET),
            StepOutcome::Halted,
            "fixture must reach a halt with no breakpoint set"
        );
        assert!(from_halted.toggle_breakpoint(3), "line 3 has an address");
        assert!(from_halted.is_halted());

        let (had_session_halted, outcome_halted) = restart_then_drive_to_terminal(&mut from_halted);
        assert!(
            had_session_halted,
            "a session existed before this Run -- halted counts too"
        );
        assert!(
            matches!(outcome_halted, StepOutcome::Breakpoint(_)),
            "Run must have restarted from halted to hit the breakpoint again: {outcome_halted:?}"
        );
    }

    #[test]
    fn run_pressed_is_a_true_no_op_while_a_run_is_already_in_flight() {
        // A Run already in flight must be a no-op: nothing here should
        // re-restart a running program out from under itself, or clobber a
        // restarting Run's own `Restarted ·`.
        let mut control =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "run-in-flight.mms").expect("assembles");
        control.start_run();
        assert!(control.is_running(), "fixture assumption");
        let pc_before = control.get_pc();

        let mut chunk_timeout = None;
        let mut debounce_timeout = None;
        let mut error = None;
        let mut error_line = None;
        let mut view_state = ViewState::new();
        view_state.reset(&control);

        assert!(
            run_pressed(
                &mut chunk_timeout,
                &mut debounce_timeout,
                &mut control,
                RESTART_STRAIGHT_LINE_MMS,
                &mut error,
                &mut error_line,
                &mut view_state,
            )
            .is_none(),
            "a Run already in flight must be a no-op"
        );
        // A restart would reload and reset the PC to the entry point; the
        // no-op must leave the still-running program's own machine intact.
        assert_eq!(control.get_pc(), pc_before);
        assert!(control.is_running());
    }

    #[test]
    fn run_then_immediate_interrupt_before_the_first_tick_still_reports_a_session() {
        // `Msg::Run` calls `restart_and_run` (which calls `Control::
        // start_run`), then `App::update` refreshes `shortcut_enablement`
        // before the first `ChunkTick` -- scheduled async -- ever reaches
        // `run_chunk`. An Interrupt landing in that window (a stray keydown,
        // or a very fast double-tap) must already see a started session:
        // `paused`, Continue live, not `ready`. `run_chunk` alone starting
        // the session is too late for this window, since it never runs.
        let mut control =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "run-then-interrupt.mms").expect("assembles");
        let mut chunk_timeout = None;
        let mut debounce_timeout = None;
        let mut error = None;
        let mut error_line = None;
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        restart_and_run(
            &mut chunk_timeout,
            &mut debounce_timeout,
            &mut control,
            RESTART_STRAIGHT_LINE_MMS,
            &mut error,
            &mut error_line,
            &mut view_state,
        );
        assert!(error.is_none(), "fixture must still assemble");
        assert!(
            control.is_running(),
            "fixture assumption: nothing has ticked yet"
        );

        // Interrupt before any `resume_chunk`/`run_chunk` call at all.
        let mut execution_stops = 0;
        assert!(interrupt_if_running(
            &mut control,
            &mut view_state,
            &mut execution_stops
        ));

        assert!(
            control.session(),
            "a Run interrupted before its first tick must still report a session"
        );
        let enablement = control_enablement(
            control.is_running(),
            control.is_halted(),
            control.session(),
            false,
        );
        assert!(
            !enablement.continue_disabled,
            "Continue must read live, not `ready`'s disabled"
        );
    }

    #[test]
    fn restart_and_run_with_reports_the_same_had_session_whichever_order_the_debounce_lands_in() {
        // A Step starts a session; an edit thereafter (`Msg::SourceChanged`)
        // never touches `session` itself -- only a reload does, whether
        // that reload is the debounce firing, Ctrl-S flushing it, or this
        // Run's own restart. All three must report the same `had_session`
        // for the restart that follows an edit: `false`, since the edit is
        // what ended the prior session, not this Run.
        fn session_after_a_step(filename: &str) -> Control {
            let mut control = Control::new(RESTART_STRAIGHT_LINE_MMS, filename).expect("assembles");
            assert_eq!(control.step(), StepOutcome::Advanced);
            assert!(control.session(), "fixture assumption");
            control
        }

        // The debounce already fired (or Ctrl-S flushed it): by the time
        // Run runs, `session` is already false.
        let mut already_flushed = session_after_a_step("already-flushed.mms");
        already_flushed
            .reload(RESTART_STRAIGHT_LINE_MMS)
            .expect("still assembles");
        assert!(!already_flushed.session());
        let mut chunk_timeout = None;
        let mut error = None;
        let mut error_line = None;
        let mut view_state = ViewState::new();
        view_state.reset(&already_flushed);
        let had_session_after_flush = restart_and_run_with(
            false,
            &mut chunk_timeout,
            &mut already_flushed,
            RESTART_STRAIGHT_LINE_MMS,
            &mut error,
            &mut error_line,
            &mut view_state,
        );
        assert!(error.is_none());
        assert!(
            !had_session_after_flush,
            "a Run after the debounce already fired reports its outcome alone"
        );

        // The debounce is still pending: without the ordering fix, `session`
        // would still read true here, since nothing has reloaded yet.
        let mut still_pending = session_after_a_step("still-pending.mms");
        assert!(still_pending.session());
        let mut chunk_timeout2 = None;
        let mut error2 = None;
        let mut error_line2 = None;
        let mut view_state2 = ViewState::new();
        view_state2.reset(&still_pending);
        let had_session_within_debounce = restart_and_run_with(
            true,
            &mut chunk_timeout2,
            &mut still_pending,
            RESTART_STRAIGHT_LINE_MMS,
            &mut error2,
            &mut error_line2,
            &mut view_state2,
        );
        assert!(error2.is_none());
        assert_eq!(
            had_session_within_debounce, had_session_after_flush,
            "a Run pressed within the debounce window must report the same \
             `had_session` as one pressed after the debounce fires"
        );
        assert!(!had_session_within_debounce);
    }

    #[test]
    fn a_restart_composes_onto_the_final_outcome_not_an_intermediate_tick() {
        fn restart_then_advance_to_terminal(
            control: &mut Control,
            view_state: &mut ViewState,
        ) -> String {
            let mut chunk_timeout = None;
            let mut debounce_timeout = None;
            let mut error = None;
            let mut error_line = None;
            let mut restart_signal = restart_and_run(
                &mut chunk_timeout,
                &mut debounce_timeout,
                control,
                RESTART_STRAIGHT_LINE_MMS,
                &mut error,
                &mut error_line,
                view_state,
            );
            assert!(error.is_none(), "fixture must still assemble");
            let mut execution_stops = 0;
            loop {
                let (status, needs_tick) = advance_chunk_once(
                    control,
                    view_state,
                    &mut restart_signal,
                    &mut execution_stops,
                );
                if !needs_tick {
                    return status;
                }
            }
        }

        // A Run after something executed.
        let mut ran_before =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "restart-visible-1.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&ran_before);
        assert_eq!(ran_before.step(), StepOutcome::Advanced);
        assert_eq!(
            restart_then_advance_to_terminal(&mut ran_before, &mut view_state),
            "Restarted · Halted"
        );

        // A Run after a Run stopped at an entry breakpoint, where nothing
        // executed.
        let mut entry_breakpoint =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "restart-visible-2.mms").expect("assembles");
        let mut view_state2 = ViewState::new();
        view_state2.reset(&entry_breakpoint);
        assert!(
            entry_breakpoint.toggle_breakpoint(2),
            "line 2 has an address"
        );
        assert!(matches!(
            entry_breakpoint.run_chunk(control::CHUNK_BUDGET),
            StepOutcome::Breakpoint(_)
        ));
        assert!(entry_breakpoint.session());
        assert_eq!(
            restart_then_advance_to_terminal(&mut entry_breakpoint, &mut view_state2),
            "Restarted · Hit breakpoint"
        );

        // A Run from a fresh load ends on its outcome alone.
        let mut fresh =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "restart-visible-3.mms").expect("assembles");
        let mut view_state3 = ViewState::new();
        view_state3.reset(&fresh);
        assert_eq!(
            restart_then_advance_to_terminal(&mut fresh, &mut view_state3),
            "Halted"
        );
    }

    /// A non-halting counter loop, long enough to outlast one
    /// `control::CHUNK_BUDGET`-sized chunk -- `BudgetExhausted`, not a
    /// terminal outcome, is the state this file's own §8-style proofs need
    /// for a chunk still mid-run.
    const INFINITE_LOOP_MMS: &str =
        "\tLOC\t#100\nMain\tSETL\t$1,0\nLoop\tADDU\t$1,$1,1\n\tJMP\tLoop\n";

    #[test]
    fn shortcut_enablement_reads_interrupt_live_mid_chunk_not_just_at_rest() {
        // The data half of the fix for "the Interrupt shortcut never
        // interrupts a chunked Run/Continue/Next": while a chunk is between
        // ticks (`BudgetExhausted`, still `is_running()`), the enablement the
        // keydown handler reads must already show Interrupt live and every
        // other control disabled -- not whatever `App` had before the run
        // started. The previous bug was `App::rendered` alone refreshing
        // this cell, which Yew 0.23's scheduler can starve for a chunked
        // run's entire span (`run_scheduler`'s `can_yield` ignores the
        // rendered queue while `ChunkTick` messages keep the update queue
        // non-empty) -- `App::update` refreshes this cell itself, at the
        // end, on every message, regardless of what `App::rendered` does.
        // That structural half can't be driven host-side without a live
        // `yew::Context` (`AGENTS.md`'s Component-lifecycle exemption);
        // this test pins the state the choke point must compute from.
        let mut control = Control::new(INFINITE_LOOP_MMS, "loop.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        let mut restart_signal = false;
        let mut execution_stops = 0;
        control.start_run();

        let (_, needs_tick) = advance_chunk_once(
            &mut control,
            &mut view_state,
            &mut restart_signal,
            &mut execution_stops,
        );
        assert!(needs_tick, "fixture must outlast one chunk budget");
        assert!(control.is_running(), "a BudgetExhausted tick stays running");

        let enablement = control_enablement(
            control.is_running(),
            control.is_halted(),
            control.session(),
            false,
        );
        assert!(
            !enablement.interrupt_disabled,
            "Interrupt must read live mid-chunk"
        );
        assert!(enablement.run_disabled);
        assert!(enablement.continue_disabled);
        assert!(enablement.step_disabled);
        assert!(enablement.next_disabled);
    }

    #[test]
    fn interrupt_if_running_is_a_true_no_op_while_paused() {
        const WRITES_REGISTER_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,7\n\tTRAP\t0,Halt,0\n";
        let mut control = Control::new(WRITES_REGISTER_MMS, "interrupt.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);

        // An explicit Step, not a chunked Run/Continue/Next: `is_running()`
        // stays false throughout, exactly the `paused` state a stray
        // `Msg::Interrupt` can still arrive in.
        assert_eq!(control.step(), StepOutcome::Advanced);
        assert!(!control.is_running(), "a plain Step never sets running");
        assert!(control.session(), "a Step must start a session -- paused");
        assert!(
            control_enablement(
                control.is_running(),
                control.is_halted(),
                control.session(),
                false,
            )
            .interrupt_disabled,
            "Interrupt must be disabled while paused"
        );
        view_state.observe(&control);
        view_state.record_pause_boundary(&control);
        let changed_after_step = view_state.changed_registers().clone();
        assert!(
            changed_after_step.contains(&1),
            "the step must flag $1 as changed: {changed_after_step:?}"
        );
        // `SETL $1,7` also grows `rL` (from `$1 < rG`'s default of 255),
        // so this fixture exercises the specials side of the diff too.
        let changed_specials_after_step = view_state.changed_specials().clone();
        assert!(
            changed_specials_after_step.contains("rL"),
            "the step must flag rL as changed: {changed_specials_after_step:?}"
        );

        // Interrupt, while paused with nothing having moved since that
        // boundary, must leave the changed set exactly as it was --
        // reverting the `is_running()` guard would re-diff the current
        // state against itself (the snapshot `record_pause_boundary` just
        // advanced to) and silently clear it to empty.
        let mut execution_stops = 0;
        assert!(!interrupt_if_running(
            &mut control,
            &mut view_state,
            &mut execution_stops
        ));
        assert_eq!(
            view_state.changed_registers(),
            &changed_after_step,
            "an inert Interrupt must not touch the changed-registers set"
        );
        assert_eq!(
            view_state.changed_specials(),
            &changed_specials_after_step,
            "an inert Interrupt must not touch the changed-specials set"
        );
    }

    #[test]
    fn continue_pressed_is_a_true_no_op_once_the_session_has_ended() {
        // `Msg::Continue`'s own `flush_pending_reassemble` can itself
        // reload, ending the session Continue depended on -- a genuine
        // no-op then, per the owner's settled decision, previously
        // untested at this seam.
        let mut control =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "continue-no-session.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        assert!(!control.session(), "fixture assumption: fresh load");

        let mut execution_stops = 0;
        assert!(continue_pressed(&mut control, &mut view_state, &mut execution_stops).is_none());
    }

    #[test]
    fn continue_pressed_clears_the_changed_highlights_before_executing() {
        // A Step first flags `$1` changed; `continue_pressed` must clear
        // that before running, same as Run and Next
        // (`docs/layout-spec.md`'s Highlights §3) -- not leave it to
        // `record_pause_boundary`, which a `BudgetExhausted` outcome (this
        // fixture never halts) never reaches, only `observe`, which never
        // touches the changed sets.
        let mut control =
            Control::new(INFINITE_LOOP_MMS, "continue-clears.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        assert_eq!(control.step(), StepOutcome::Advanced); // SETL $1,0 -- starts the session
        assert_eq!(control.step(), StepOutcome::Advanced); // ADDU $1,$1,1 -- $1 becomes 1
        view_state.observe(&control);
        view_state.record_pause_boundary(&control);
        assert!(
            view_state.changed_registers().contains(&1),
            "fixture assumption: the second step must flag $1 changed"
        );

        let mut execution_stops = 0;
        let (_, needs_tick) = continue_pressed(&mut control, &mut view_state, &mut execution_stops)
            .expect("a paused session proceeds");
        assert!(needs_tick, "fixture must outlast one chunk budget");
        assert!(
            !view_state.changed_registers().contains(&1),
            "Continue must clear the stale changed mark before running: {:?}",
            view_state.changed_registers()
        );
    }

    #[test]
    fn flush_pending_source_is_a_true_no_op_with_nothing_pending() {
        const WRITES_REGISTER_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,7\n\tTRAP\t0,Halt,0\n";
        let mut control = Control::new(WRITES_REGISTER_MMS, "flush.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);

        // Advance the machine and record a pause boundary first, so the
        // baseline below is provably non-empty -- mirrors
        // `interrupt_if_running_is_a_true_no_op_while_paused`, and for the
        // same reason: a freshly-reset empty baseline can't distinguish "nothing
        // happened" from "view_state got reset a second time", which is
        // exactly the bug this function exists to prevent (a swallowed
        // second `view_state.reset()` if the pending debounce timer weren't
        // dropped).
        assert_eq!(control.step(), StepOutcome::Advanced);
        assert!(!control.is_running(), "a plain Step never sets running");
        view_state.observe(&control);
        view_state.record_pause_boundary(&control);
        let changed_registers_before = view_state.changed_registers().clone();
        assert!(
            changed_registers_before.contains(&1),
            "the step must flag $1 as changed: {changed_registers_before:?}"
        );
        // `SETL $1,7` also grows `rL` (from `$1 < rG`'s default of 255),
        // so this fixture exercises the specials side of the diff too.
        let changed_specials_before = view_state.changed_specials().clone();
        assert!(
            changed_specials_before.contains("rL"),
            "the step must flag rL as changed: {changed_specials_before:?}"
        );

        // Nothing pending, the case `save_shortcut`'s unit test alone
        // cannot cover since it never sees `debounce_timeout`: this must
        // leave `error` and `view_state` exactly as they were, the same way
        // `interrupt_if_running` leaves the changed set alone while paused.
        let mut debounce_timeout: Option<Timeout> = None;
        let mut chunk_timeout: Option<Timeout> = None;
        let mut error: Option<String> = None;
        let mut error_line: Option<usize> = None;
        assert!(!flush_pending_source(
            &mut debounce_timeout,
            &mut control,
            &mut chunk_timeout,
            WRITES_REGISTER_MMS,
            &mut error,
            &mut error_line,
            &mut view_state,
        ));
        assert!(error.is_none(), "a no-op flush must not set an error");
        assert!(
            error_line.is_none(),
            "a no-op flush must not set error_line"
        );
        assert_eq!(
            view_state.changed_registers(),
            &changed_registers_before,
            "a no-op flush must not touch the changed-registers set"
        );
        assert_eq!(
            view_state.changed_specials(),
            &changed_specials_before,
            "a no-op flush must not touch the changed-specials set"
        );
    }

    #[test]
    fn restore_source_with_nothing_saved_starts_from_default() {
        let mut control = Control::new(DEFAULT_MMS, "restore-none.mms").expect("assembles");
        let mut chunk_timeout = None;
        let mut error = None;
        let mut error_line = None;
        let mut view_state = ViewState::new();
        view_state.reset(&control);

        let source = restore_source(
            None,
            &mut chunk_timeout,
            &mut control,
            &mut error,
            &mut error_line,
            &mut view_state,
        );
        assert_eq!(source, DEFAULT_MMS);
        assert!(error.is_none());
    }

    #[test]
    fn restore_source_loads_a_saved_program_that_assembles() {
        let mut control = Control::new(DEFAULT_MMS, "restore-ok.mms").expect("assembles");
        let mut chunk_timeout = None;
        let mut error = None;
        let mut error_line = None;
        let mut view_state = ViewState::new();
        view_state.reset(&control);

        let source = restore_source(
            Some(RESTART_STRAIGHT_LINE_MMS.to_string()),
            &mut chunk_timeout,
            &mut control,
            &mut error,
            &mut error_line,
            &mut view_state,
        );
        assert_eq!(source, RESTART_STRAIGHT_LINE_MMS);
        assert!(error.is_none());
        // Both fixtures start at `#100`, so a PC comparison alone would
        // pass even without the reload: `DEFAULT_MMS` halts on its own
        // first instruction, so only the reloaded program's own `SETL
        // $1,1` can advance the PC and set `$1`.
        assert_eq!(control.step(), StepOutcome::Advanced);
        assert_eq!(control.machine().get_register(1), 1);
    }

    #[test]
    fn restore_source_shows_a_saved_programs_own_error_but_keeps_default_mms_running() {
        // A saved program that fails to assemble becomes the editor text
        // regardless, but the machine stays on `DEFAULT_MMS`'s own load --
        // `Control::reload` leaves the previous machine untouched on a
        // parse error.
        const BOGUS_MMS: &str = "\tLOC\t#100\nMain\tBOGUS\t$1,1\n";
        let mut control = Control::new(DEFAULT_MMS, "restore-bad.mms").expect("assembles");
        let default_pc = control.get_pc();
        let mut chunk_timeout = None;
        let mut error = None;
        let mut error_line = None;
        let mut view_state = ViewState::new();
        view_state.reset(&control);

        let source = restore_source(
            Some(BOGUS_MMS.to_string()),
            &mut chunk_timeout,
            &mut control,
            &mut error,
            &mut error_line,
            &mut view_state,
        );
        assert_eq!(source, BOGUS_MMS, "the editor must show the saved text");
        assert!(
            error.is_some(),
            "the saved program's own error must surface"
        );
        assert_eq!(
            control.get_pc(),
            default_pc,
            "the machine must stay on DEFAULT_MMS's own load"
        );
    }

    #[test]
    fn leave_warning_needed_arms_only_on_a_failed_save_with_work_to_lose() {
        assert!(
            leave_warning_needed(false, RESTART_STRAIGHT_LINE_MMS),
            "a failed save with work to lose must arm the warning"
        );
        assert!(
            !leave_warning_needed(false, DEFAULT_MMS),
            "a failed save with nothing but the skeleton must not arm the warning"
        );
        assert!(
            !leave_warning_needed(true, RESTART_STRAIGHT_LINE_MMS),
            "a successful save must not arm the warning"
        );
    }

    #[test]
    fn start_over_drops_breakpoints_resets_view_state_and_returns_to_the_default_skeleton() {
        // A fixture that does not already hold `DEFAULT_MMS`, so the
        // assertions below prove New actually replaces the machine and
        // resets the view state rather than merely finding them already in
        // that shape.
        let mut control =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "new-fixture.mms").expect("assembles");
        assert!(
            control.toggle_breakpoint(5),
            "line 5 (the TRAP) must resolve in this fixture"
        );
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        assert_eq!(control.step(), StepOutcome::Advanced); // SETL $1,1
        view_state.observe(&control);
        view_state.record_pause_boundary(&control);
        assert!(
            view_state.changed_registers().contains(&1),
            "fixture assumption: the step must flag $1 changed"
        );

        let mut chunk_timeout = None;
        let mut debounce_timeout = None;
        let mut error = Some("stale error".to_string());
        let mut error_line = Some(5);

        let source = start_over(
            &mut chunk_timeout,
            &mut debounce_timeout,
            &mut control,
            &mut error,
            &mut error_line,
            &mut view_state,
        );

        assert_eq!(source, DEFAULT_MMS);
        assert!(
            control.breakpoint_lines().is_empty(),
            "New must start with no breakpoints"
        );
        assert!(error.is_none());
        assert!(error_line.is_none());
        assert!(
            view_state.changed_registers().is_empty(),
            "New must reset the view state, dropping the prior program's changed marks"
        );
        assert_eq!(
            control.machine().get_register(1),
            0,
            "the machine must be DEFAULT_MMS's own fresh load, not the prior program's $1==1"
        );
        let fresh = Control::new(DEFAULT_MMS, "new-compare.mms").expect("assembles");
        assert_eq!(control.get_pc(), fresh.get_pc());
        assert_eq!(control.session(), fresh.session());
    }

    /// Work distinct from both `DEFAULT_MMS` and `RESTART_STRAIGHT_LINE_MMS`
    /// -- the row `decide_shared_link` must ask before replacing.
    const OTHER_WORK_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$9,9\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn decide_shared_link_covers_every_row() {
        // No fragment at all: ignored.
        assert_eq!(
            decide_shared_link("", RESTART_STRAIGHT_LINE_MMS),
            SharedLinkDecision::Ignore
        );
        // A fragment present but not a share link: ignored, never decoded.
        assert_eq!(
            decide_shared_link("#x=abc", RESTART_STRAIGHT_LINE_MMS),
            SharedLinkDecision::Ignore
        );
        // A `#p=` fragment whose payload doesn't decode: unreadable.
        assert_eq!(
            decide_shared_link("#p=", RESTART_STRAIGHT_LINE_MMS),
            SharedLinkDecision::Unreadable
        );
        // The shared program equals the editor's current text: load, no
        // question -- nothing would change.
        let hash_same = format!("#p={}", share::encode(RESTART_STRAIGHT_LINE_MMS));
        assert_eq!(
            decide_shared_link(&hash_same, RESTART_STRAIGHT_LINE_MMS),
            SharedLinkDecision::Load {
                source: RESTART_STRAIGHT_LINE_MMS.to_string(),
                ask: false,
            }
        );
        // The editor holds only the skeleton: load, no question -- nothing
        // to lose.
        let hash_other = format!("#p={}", share::encode(RESTART_STRAIGHT_LINE_MMS));
        assert_eq!(
            decide_shared_link(&hash_other, DEFAULT_MMS),
            SharedLinkDecision::Load {
                source: RESTART_STRAIGHT_LINE_MMS.to_string(),
                ask: false,
            }
        );
        // The editor holds different work: ask first.
        assert_eq!(
            decide_shared_link(&hash_other, OTHER_WORK_MMS),
            SharedLinkDecision::Load {
                source: RESTART_STRAIGHT_LINE_MMS.to_string(),
                ask: true,
            }
        );
    }

    #[test]
    fn start_fresh_loads_the_shared_programs_own_machine() {
        // The fixture starts from `DEFAULT_MMS`, which differs from the
        // shared program below, so stepping the loaded machine tells the
        // two apart.
        let mut control = Control::new(DEFAULT_MMS, "share-load-fixture.mms").expect("assembles");
        assert!(
            control.toggle_breakpoint(5),
            "line 5 (the TRAP) must resolve in DEFAULT_MMS"
        );
        let mut chunk_timeout = None;
        let mut debounce_timeout = None;
        let mut error = Some("stale error".to_string());
        let mut error_line = Some(5);
        let mut view_state = ViewState::new();
        view_state.reset(&control);

        start_fresh(
            &mut chunk_timeout,
            &mut debounce_timeout,
            &mut control,
            &mut error,
            &mut error_line,
            &mut view_state,
            RESTART_STRAIGHT_LINE_MMS,
        );

        assert!(
            control.breakpoint_lines().is_empty(),
            "a shared program must start with no breakpoints from the program it replaced"
        );
        assert!(error.is_none());
        assert!(error_line.is_none());
        // Prove the loaded machine is the shared program's own, not
        // `DEFAULT_MMS`'s: stepping runs `SETL $1,1`, which only this
        // fixture's own entry point contains.
        assert_eq!(control.step(), StepOutcome::Advanced);
        assert_eq!(
            control.machine().get_register(1),
            1,
            "the machine must be the shared program's own fresh load"
        );
    }

    #[test]
    fn start_fresh_that_fails_to_assemble_falls_back_to_default_mms_with_its_error_set() {
        // A fixture that does not already hold `DEFAULT_MMS`, with a
        // breakpoint set and one step taken, so a register holds a nonzero
        // value and shows as changed -- proving the fallback below actually
        // replaces the machine and resets the view state, rather than
        // merely finding them already in that shape.
        const BOGUS_MMS: &str = "\tLOC\t#100\nMain\tBOGUS\t$1,1\n";
        let mut control =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "share-bad-fixture.mms").expect("assembles");
        assert!(
            control.toggle_breakpoint(3),
            "line 3 has an address in this fixture"
        );
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        assert_eq!(control.step(), StepOutcome::Advanced); // SETL $1,1
        view_state.observe(&control);
        view_state.record_pause_boundary(&control);
        assert!(
            view_state.changed_registers().contains(&1),
            "fixture assumption: the step must flag $1 changed"
        );

        let mut chunk_timeout = None;
        let mut debounce_timeout = None;
        let mut error = None;
        let mut error_line = None;

        start_fresh(
            &mut chunk_timeout,
            &mut debounce_timeout,
            &mut control,
            &mut error,
            &mut error_line,
            &mut view_state,
            BOGUS_MMS,
        );

        assert!(
            error.is_some(),
            "the shared program's own error must surface"
        );
        assert_eq!(
            error_line,
            Some(2),
            "error_line must name BOGUS_MMS's own bad line"
        );
        assert!(
            control.breakpoint_lines().is_empty(),
            "a failed shared load must still start with no breakpoints from the program it replaced"
        );
        assert_eq!(
            control.machine().get_register(1),
            0,
            "the machine must fall back to DEFAULT_MMS's own fresh load, not the prior program's $1==1"
        );
        assert!(
            view_state.changed_registers().is_empty(),
            "the fallback must reset the view state, dropping the prior program's changed marks"
        );
    }

    #[test]
    fn advance_chunk_once_leaves_execution_stops_unchanged_on_budget_exhausted() {
        // A chunk that reschedules itself hasn't stopped anything yet.
        let mut control = Control::new(INFINITE_LOOP_MMS, "stops-budget.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        control.start_run();
        let mut restart_signal = false;
        let mut execution_stops = 0;

        let (_, needs_tick) = advance_chunk_once(
            &mut control,
            &mut view_state,
            &mut restart_signal,
            &mut execution_stops,
        );
        assert!(needs_tick, "fixture must outlast one chunk budget");
        assert_eq!(
            execution_stops, 0,
            "a BudgetExhausted chunk must not count as a stop"
        );
    }

    #[test]
    fn advance_chunk_once_increments_execution_stops_once_on_a_terminal_outcome() {
        // Halted: the straight-line fixture halts within its first chunk.
        let mut halts =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "stops-halt.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&halts);
        halts.start_run();
        let mut restart_signal = false;
        let mut execution_stops = 0;
        let (_, needs_tick) = advance_chunk_once(
            &mut halts,
            &mut view_state,
            &mut restart_signal,
            &mut execution_stops,
        );
        assert!(!needs_tick, "fixture must halt within one chunk");
        assert_eq!(execution_stops, 1, "a Halted outcome must count as a stop");

        // Breakpoint: same fixture, a breakpoint set before the halt.
        let mut hits_breakpoint =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "stops-breakpoint.mms").expect("assembles");
        assert!(
            hits_breakpoint.toggle_breakpoint(3),
            "line 3 has an address"
        );
        let mut view_state2 = ViewState::new();
        view_state2.reset(&hits_breakpoint);
        hits_breakpoint.start_run();
        let mut restart_signal2 = false;
        let mut execution_stops2 = 0;
        let (_, needs_tick2) = advance_chunk_once(
            &mut hits_breakpoint,
            &mut view_state2,
            &mut restart_signal2,
            &mut execution_stops2,
        );
        assert!(
            !needs_tick2,
            "fixture must hit the breakpoint within one chunk"
        );
        assert_eq!(
            execution_stops2, 1,
            "a Breakpoint outcome must count as a stop"
        );
    }

    /// A fresh `Control` loaded from `source`, paired with a `ViewState`
    /// seeded from it -- the repeated setup the `first_chunk_outcome` tests
    /// below start from.
    fn fresh_control_and_view_state(source: &str, filename: &str) -> (Control, ViewState) {
        let control = Control::new(source, filename).expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        (control, view_state)
    }

    #[test]
    fn first_chunk_outcome_increments_execution_stops_once_on_halted() {
        // `first_chunk_outcome` is Continue's and Next's own first chunk;
        // its increment needs direct coverage, since neither command's own
        // caller-side test reaches a terminal outcome on the first chunk.
        let (mut control, mut view_state) =
            fresh_control_and_view_state(DEFAULT_MMS, "first-chunk-halted.mms");
        let mut execution_stops = 0;

        let (_, needs_tick) = first_chunk_outcome(
            &mut control,
            &mut view_state,
            StepOutcome::Halted,
            &mut execution_stops,
        );
        assert!(!needs_tick, "a Halted outcome is terminal");
        assert_eq!(execution_stops, 1, "a Halted outcome must count as a stop");
    }

    #[test]
    fn first_chunk_outcome_leaves_execution_stops_unchanged_on_budget_exhausted() {
        let (mut control, mut view_state) =
            fresh_control_and_view_state(DEFAULT_MMS, "first-chunk-budget.mms");
        let mut execution_stops = 0;

        let (_, needs_tick) = first_chunk_outcome(
            &mut control,
            &mut view_state,
            StepOutcome::BudgetExhausted,
            &mut execution_stops,
        );
        assert!(needs_tick, "a BudgetExhausted outcome needs another tick");
        assert_eq!(
            execution_stops, 0,
            "a BudgetExhausted chunk must not count as a stop"
        );
    }

    #[test]
    fn interrupt_if_running_increments_execution_stops_once_when_it_interrupts() {
        let mut control =
            Control::new(INFINITE_LOOP_MMS, "stops-interrupt.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        control.start_run();
        let mut execution_stops = 0;

        assert!(interrupt_if_running(
            &mut control,
            &mut view_state,
            &mut execution_stops
        ));
        assert_eq!(
            execution_stops, 1,
            "an Interrupt that actually interrupted must count as a stop"
        );
    }

    #[test]
    fn step_pressed_increments_execution_stops_once_when_it_steps() {
        let mut control =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "stops-step.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        let mut restart_signal = false;
        let mut execution_stops = 0;

        let outcome = step_pressed(
            &mut control,
            &mut view_state,
            &mut restart_signal,
            &mut execution_stops,
        );
        assert!(outcome.is_some(), "a Step that ran must return its outcome");
        assert_eq!(execution_stops, 1, "a Step that ran must count as a stop");
    }

    #[test]
    fn step_pressed_leaves_execution_stops_unchanged_when_already_halted() {
        // Decision: a Step refused by a halt, same as one refused by an
        // assembly error (guarded a level up, in `flush_pending_reassemble`,
        // before `step_pressed` ever runs), must not count as a stop.
        let mut control =
            Control::new(RESTART_STRAIGHT_LINE_MMS, "stops-halted.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        while !control.is_halted() {
            control.step();
        }
        let mut restart_signal = false;
        let mut execution_stops = 0;

        let outcome = step_pressed(
            &mut control,
            &mut view_state,
            &mut restart_signal,
            &mut execution_stops,
        );
        assert!(
            outcome.is_none(),
            "a Step refused by a halt must be a no-op"
        );
        assert_eq!(
            execution_stops, 0,
            "a refused Step must not count as a stop"
        );
    }
}
