use std::collections::BTreeSet;

use gloo_timers::callback::Timeout;
use log::info;
use yew::{Component, Context, Html, Renderer, html};

mod control;
mod editor;
mod highlight;
mod machine;

use control::{Control, ControlBar, StepOutcome, yield_to_event_loop};
use editor::Editor;
use machine::{MachinePane, OutputPane, RegisterContinuity, SpecialContinuity};

/// `examples/hello_world.mms` from checksmix, embedded verbatim. Not
/// `include_str!`: a dependency's on-disk location, wherever cargo puts it
/// (crates.io registry cache or git checkout alike), is not a stable,
/// crate-relative path.
const HELLO_WORLD_MMS: &str = "\tLOC\tData_Segment\n\tGREG\t@\nText\tBYTE\t\"Hello world!\",'\\n',0\n\n\tLOC\t#100\n\nMain\tdebug \"Version 0.1: Hello World Example\"\t\n\tLDA\t\t$255,Text\n\tTRAP\t0,Fputs,StdOut\n\tTRAP\t0,Halt,0\n";

/// A minimal, fixed program that always assembles -- the fallback if the
/// embedded `HELLO_WORLD_MMS` somehow doesn't (e.g. a checksmix upgrade
/// changes accepted syntax), so `App::create` has no fallible path of its
/// own without threading an `Option<Control>` through every view and update
/// path just for that one unlikely case.
const FALLBACK_MMS: &str = "\tLOC\t#100\nMain\tTRAP\t0,Halt,0\n";

/// The filename `Control` assembles the editor's buffer under. Fixed:
/// playmmix edits a single in-memory buffer, not a multi-file project, and
/// breakpoint/PC line lookups need the same name on every assemble.
const SOURCE_FILENAME: &str = "source.mms";

pub enum Msg {
    SourceChanged(String),
    ToggleBreakpoint(usize),
    Run,
    Step,
    StepOver,
    Stop,
    Reset,
    /// One chunk boundary: reschedule if the run isn't finished, or if a
    /// `Stop` landed while this tick was scheduled, do nothing.
    ChunkTick,
}

pub struct App {
    source: String,
    control: Control,
    error: Option<String>,
    /// The pending chunk-tick timeout, if a chunked Run or Step Over is in
    /// flight. Held rather than `.forget()`-ten so Stop, or a `reload` mid-
    /// run, can cancel it by dropping this (runs `clearTimeout` and frees
    /// the closure) instead of leaking one allocation per chunk boundary.
    chunk_timeout: Option<Timeout>,
    /// Cross-render register/special visibility, view state owned here (not
    /// on `Control`, not derived from `&MMix` alone) per
    /// `docs/layout-spec.md`'s Registers section. Replaced wholesale on a
    /// successful Reset/reload.
    register_continuity: RegisterContinuity,
    special_continuity: SpecialContinuity,
    /// The registers/specials/memory as of the previous pause boundary --
    /// the comparison snapshot `diff_registers`/`diff_specials`/
    /// `diff_memory` diff the current render against. Seeded on load,
    /// updated only at the pause boundaries `record_pause_boundary`
    /// documents.
    prev_registers: Vec<machine::RegisterRow>,
    prev_specials: Vec<machine::SpecialRegisterRow>,
    prev_memory: Vec<machine::MemoryRow>,
    /// The most recently computed changed-since-last-pause sets, empty
    /// while a Run or Step Over is advancing.
    changed_registers: BTreeSet<u8>,
    changed_specials: BTreeSet<String>,
    changed_memory: BTreeSet<u64>,
}

impl App {
    /// Yield to the event loop, then deliver `Msg::ChunkTick` -- the one
    /// place a chunked Run or Step Over reschedules itself. Replaces
    /// `chunk_timeout`, dropping (and so cancelling) any tick already
    /// pending.
    fn schedule_chunk_tick(&mut self, ctx: &Context<Self>) {
        let link = ctx.link().clone();
        self.chunk_timeout = Some(yield_to_event_loop(move || {
            link.send_message(Msg::ChunkTick)
        }));
    }

    /// Advance one chunk of whichever operation -- Run or Step Over -- is
    /// in flight, rescheduling if it isn't finished. The scheduling policy
    /// for `Msg::ChunkTick`.
    fn advance_chunk(&mut self, ctx: &Context<Self>) {
        // A Stop is checked only between chunks, never inside one; this is
        // that check. Cancelling `chunk_timeout` on Stop already prevents
        // this from firing in the normal case -- this guard is a
        // defensive backstop, not the primary safety mechanism.
        if !self.control.is_running() {
            return;
        }
        let outcome = self.control.continue_chunk(control::CHUNK_BUDGET);
        self.observe_continuity();
        match outcome {
            StepOutcome::BudgetExhausted => self.schedule_chunk_tick(ctx),
            StepOutcome::Halted | StepOutcome::Breakpoint(_) | StepOutcome::Advanced => {
                self.record_pause_boundary();
            }
        }
    }

    /// Union the currently-visible registers/specials into both continuity
    /// trackers' sticky sets. Called at every point that mutates
    /// `self.control` -- see `docs/layout-spec.md`'s Settled decisions for
    /// the exact call sites.
    fn observe_continuity(&mut self) {
        self.register_continuity.observe(self.control.machine());
        self.special_continuity.observe(self.control.machine());
    }

    /// Recompute the changed-since-last-pause sets against the previous
    /// pause boundary's snapshot, then advance the snapshot to the current
    /// state -- called only at an actual pause boundary (a Step that
    /// executed, a Step Over's or Run's terminal outcome, or an explicit
    /// Stop), never on an intermediate chunk repaint.
    fn record_pause_boundary(&mut self) {
        let mmix = self.control.machine();
        let registers = machine::visible_registers(mmix, &self.register_continuity);
        let specials = machine::visible_specials(mmix, &self.special_continuity);
        let runs = machine::memory_runs(mmix, self.control.labels());
        let memory = machine::memory_rows(&runs);

        self.changed_registers = machine::diff_registers(&self.prev_registers, &registers);
        self.changed_specials = machine::diff_specials(&self.prev_specials, &specials);
        self.changed_memory = machine::diff_memory(&self.prev_memory, &memory);

        self.prev_registers = registers;
        self.prev_specials = specials;
        self.prev_memory = memory;
    }

    /// Establish a fresh baseline after a successful load/reload: reset
    /// both continuity trackers, seed them from the freshly loaded machine
    /// (so a register visible only at load isn't lost on the very first
    /// step -- see `docs/layout-spec.md`'s Settled decisions), and capture
    /// the fresh state as the "previous" snapshot so the first real pause
    /// boundary diffs against actual fresh-load values, not an empty
    /// snapshot that would flag every already-nonzero register as changed.
    /// Not a pause boundary itself -- `changed_*` stays empty.
    fn reset_view_state(&mut self) {
        self.register_continuity = RegisterContinuity::new();
        self.special_continuity = SpecialContinuity::new();
        self.observe_continuity();

        let mmix = self.control.machine();
        self.prev_registers = machine::visible_registers(mmix, &self.register_continuity);
        self.prev_specials = machine::visible_specials(mmix, &self.special_continuity);
        let runs = machine::memory_runs(mmix, self.control.labels());
        self.prev_memory = machine::memory_rows(&runs);

        self.clear_changed();
    }

    /// Clear the changed-since-last-pause sets -- the moment a Run or Step
    /// Over resumes advancing, per `docs/layout-spec.md`'s Highlights §3.
    fn clear_changed(&mut self) {
        self.changed_registers.clear();
        self.changed_specials.clear();
        self.changed_memory.clear();
    }

    /// Reload the current source (Reset's and halted-Run's shared "play
    /// again" step): cancel any pending chunk tick, re-run
    /// `Control::reload`, and on success reseed the continuity/snapshot
    /// state. On a parse error, `reload` already leaves the previous
    /// machine and everything else untouched, so only `self.error` moves.
    fn reload_source(&mut self) {
        self.chunk_timeout = None;
        match self.control.reload(&self.source) {
            Ok(()) => {
                self.error = None;
                self.reset_view_state();
            }
            Err(error) => {
                self.error = Some(error);
            }
        }
    }
}

impl Component for App {
    type Message = Msg;
    type Properties = ();

    fn create(_ctx: &Context<Self>) -> Self {
        let (control, error) = match Control::new(HELLO_WORLD_MMS, SOURCE_FILENAME) {
            Ok(control) => (control, None),
            Err(error) => (
                Control::new(FALLBACK_MMS, SOURCE_FILENAME)
                    .expect("FALLBACK_MMS is a fixed, minimal, always-valid program"),
                Some(error),
            ),
        };
        let mut app = Self {
            source: HELLO_WORLD_MMS.to_string(),
            control,
            error,
            chunk_timeout: None,
            register_continuity: RegisterContinuity::new(),
            special_continuity: SpecialContinuity::new(),
            prev_registers: Vec::new(),
            prev_specials: Vec::new(),
            prev_memory: Vec::new(),
            changed_registers: BTreeSet::new(),
            changed_specials: BTreeSet::new(),
            changed_memory: BTreeSet::new(),
        };
        // Seed continuity and the diff baseline off the freshly loaded
        // machine -- not about the first render (`visible_registers`
        // already computes the correct set live), but about stickiness: a
        // register visible only at load must already be in the sticky set
        // before the first `step()` runs.
        app.reset_view_state();
        app
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            Msg::SourceChanged(source) => {
                match self.control.reload(&source) {
                    Ok(()) => {
                        self.error = None;
                        self.reset_view_state();
                    }
                    Err(error) => {
                        self.error = Some(error);
                    }
                }
                self.source = source;
                self.chunk_timeout = None;
                true
            }
            Msg::ToggleBreakpoint(line) => {
                self.control.toggle_breakpoint(line);
                true
            }
            Msg::Run => {
                // Run while halted is Reset then Run -- the "play again"
                // affordance, one click to replay from the top.
                if self.control.is_halted() {
                    self.reload_source();
                    if self.error.is_some() {
                        return true;
                    }
                }
                self.clear_changed();
                self.control.start_run();
                self.observe_continuity();
                if self.control.is_running() {
                    self.schedule_chunk_tick(ctx);
                }
                true
            }
            Msg::Step => {
                if !self.control.is_running() {
                    let was_halted = self.control.is_halted();
                    self.control.step();
                    if !was_halted {
                        self.observe_continuity();
                        self.record_pause_boundary();
                    }
                }
                true
            }
            Msg::StepOver => {
                if !self.control.is_running() {
                    self.clear_changed();
                    match self.control.step_over_chunk(control::CHUNK_BUDGET) {
                        StepOutcome::BudgetExhausted => {
                            self.observe_continuity();
                            self.schedule_chunk_tick(ctx);
                        }
                        StepOutcome::Advanced
                        | StepOutcome::Halted
                        | StepOutcome::Breakpoint(_) => {
                            self.observe_continuity();
                            self.record_pause_boundary();
                        }
                    }
                }
                true
            }
            Msg::Stop => {
                self.control.stop();
                self.chunk_timeout = None;
                self.observe_continuity();
                self.record_pause_boundary();
                true
            }
            Msg::Reset => {
                self.reload_source();
                true
            }
            Msg::ChunkTick => {
                self.advance_chunk(ctx);
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let exit_code = self
            .control
            .is_halted()
            .then(|| self.control.machine().get_exit_code());

        let machine_view = match &self.error {
            Some(error) => html! { <pre>{ format!("Assembly error: {error}") }</pre> },
            None => {
                let mmix = self.control.machine();
                let registers = machine::visible_registers(mmix, &self.register_continuity);
                let specials = machine::visible_specials(mmix, &self.special_continuity);
                let runs = machine::memory_runs(mmix, self.control.labels());
                let memory = machine::memory_rows(&runs);
                html! {
                    <MachinePane
                        {registers}
                        {specials}
                        {memory}
                        pc={self.control.get_pc()}
                        marker_pc={self.control.marker_pc()}
                        {exit_code}
                        call_depth={self.control.call_depth()}
                        changed_registers={self.changed_registers.clone()}
                        changed_specials={self.changed_specials.clone()}
                        changed_memory={self.changed_memory.clone()}
                    />
                }
            }
        };

        let on_change = ctx.link().callback(Msg::SourceChanged);
        let on_toggle_breakpoint = ctx.link().callback(Msg::ToggleBreakpoint);
        let on_run = ctx.link().callback(|()| Msg::Run);
        let on_step = ctx.link().callback(|()| Msg::Step);
        let on_step_over = ctx.link().callback(|()| Msg::StepOver);
        let on_stop = ctx.link().callback(|()| Msg::Stop);
        let on_reset = ctx.link().callback(|()| Msg::Reset);

        // The PC indicator only means something while nothing is actively
        // moving it; showing it mid-run would flicker with every chunk.
        let current_line = (!self.control.is_running())
            .then(|| self.control.current_line())
            .flatten();

        html! {
            <main>
                <div class="app-header">
                    <h1>{ "playmmix" }</h1>
                    <ControlBar
                        running={self.control.is_running()}
                        halted={self.control.is_halted()}
                        has_advanced={self.control.has_advanced()}
                        {on_run}
                        {on_step}
                        {on_step_over}
                        {on_stop}
                        {on_reset}
                    />
                </div>
                <Editor
                    source={self.source.clone()}
                    {on_change}
                    breakpoints={self.control.breakpoint_lines().clone()}
                    {current_line}
                    {on_toggle_breakpoint}
                />
                <OutputPane spans={self.control.output()} {exit_code} />
                <div class="machine-slot">{ machine_view }</div>
            </main>
        }
    }
}

fn main() {
    wasm_logger::init(wasm_logger::Config::default());
    info!("Starting playmmix");
    Renderer::<App>::new().render();
}
