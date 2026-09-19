//! The control bar: Run, Continue, Step, Next, Interrupt and Reset.

use yew::prelude::*;

/// Run/Continue/Step/Next/Interrupt/Reset's disabled state for a given
/// `(running, halted, session, has_error)` quadruple --
/// `docs/layout-spec.md`'s Run lifecycle table, factored into one plain
/// function so `ControlBar`'s body states it once and a table-driven test
/// can pin the whole table at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlEnablement {
    pub run_disabled: bool,
    pub continue_disabled: bool,
    pub step_disabled: bool,
    pub next_disabled: bool,
    pub interrupt_disabled: bool,
    pub reset_disabled: bool,
}

/// `has_error` forces every field `true`: an assembly error means there is
/// no valid program loaded to Run, Continue, Step, Next, Interrupt, or
/// Reset, so every control disables regardless of `running`/`halted`/
/// `session`.
pub fn control_enablement(
    running: bool,
    halted: bool,
    session: bool,
    has_error: bool,
) -> ControlEnablement {
    if has_error {
        return ControlEnablement {
            run_disabled: true,
            continue_disabled: true,
            step_disabled: true,
            next_disabled: true,
            interrupt_disabled: true,
            reset_disabled: true,
        };
    }
    // A started session that is neither running nor halted -- the one state
    // Continue is live in, gdb's "The program is not being run" everywhere
    // else.
    let paused = session && !running && !halted;
    ControlEnablement {
        run_disabled: running,
        continue_disabled: !paused,
        step_disabled: running || halted,
        next_disabled: running || halted,
        // Interrupt is live only while running: a live button that does
        // nothing in `ready`/`paused` is the defect the owner found in
        // Stop. Reset's own gate is the exact opposite -- live everywhere
        // but `running` -- so the two are never live together; Interrupt
        // is the sole live control in `running`. Run shares Reset's gate
        // too, so both are live in `halted`, not Reset alone.
        interrupt_disabled: !running,
        reset_disabled: running,
    }
}

/// Run's title: its keys, and what separates it from Continue and Reset.
const RUN_TITLE: &str = "Run (r): restart from the start state, run to a breakpoint or halt.";
const CONTINUE_TITLE: &str =
    "Continue (c): execute the instruction at the PC, then run to a breakpoint or halt.";
const STEP_TITLE: &str = "Step (s, F11): one source line, into calls.";
const NEXT_TITLE: &str = "Next (n, F10): one source line, over calls.";
const INTERRUPT_TITLE: &str = "Interrupt (x): pause a Run, Continue, or Next in flight.";
/// Reset's title, exact per the owner's settled decision.
const RESET_TITLE: &str = "Reload the program and return the machine to its start state: \
registers, memory and output as loaded, PC at Main. Breakpoints are kept.";

#[derive(Properties, PartialEq)]
pub struct ControlBarProps {
    pub running: bool,
    pub halted: bool,
    /// Whether a session has started -- distinguishes `paused` from `ready`
    /// in the run-state label, and gates Continue.
    pub session: bool,
    /// Whether the currently displayed source has an assembly error --
    /// forces every control disabled (see `control_enablement`), since a
    /// broken program isn't the one that would actually run.
    pub has_error: bool,
    pub on_run: Callback<()>,
    pub on_continue: Callback<()>,
    pub on_step: Callback<()>,
    pub on_next: Callback<()>,
    pub on_interrupt: Callback<()>,
    pub on_reset: Callback<()>,
    /// A short (4-5 word) echo of the last action taken, rendered to the
    /// right of the run-state label -- `App::status_message` in `main.rs`.
    pub status: String,
}

/// Run / Continue / Step / Next / Interrupt / Reset, enabled per
/// `control_enablement`.
#[function_component(ControlBar)]
pub fn control_bar(props: &ControlBarProps) -> Html {
    let running = props.running;
    let halted = props.halted;
    let enablement = control_enablement(running, halted, props.session, props.has_error);

    html! {
        <div class="controls">
            { control_button("Run", Some("r"), RUN_TITLE, enablement.run_disabled, props.on_run.clone()) }
            { control_button("Continue", Some("c"), CONTINUE_TITLE, enablement.continue_disabled, props.on_continue.clone()) }
            { control_button("Step", Some("s F11"), STEP_TITLE, enablement.step_disabled, props.on_step.clone()) }
            { control_button("Next", Some("n F10"), NEXT_TITLE, enablement.next_disabled, props.on_next.clone()) }
            { control_button("Interrupt", Some("x"), INTERRUPT_TITLE, enablement.interrupt_disabled, props.on_interrupt.clone()) }
            { control_button("Reset", None, RESET_TITLE, enablement.reset_disabled, props.on_reset.clone()) }
            <span class="run-state">
                { run_state_label(running, halted, props.session) }
            </span>
            <span class="status-message">{ &props.status }</span>
        </div>
    }
}

/// One control-pane button: a plain `<button>` so every control is
/// keyboard-reachable without extra wiring. `key_hint`, when present, renders
/// beside `label` as a hint on the button's own face, not only on hover;
/// `title` names the action, its keys, and what it does in one clause.
fn control_button(
    label: &'static str,
    key_hint: Option<&'static str>,
    title: &'static str,
    disabled: bool,
    on_click: Callback<()>,
) -> Html {
    let onclick = Callback::from(move |_| on_click.emit(()));
    html! {
        <button {disabled} {onclick} {title}>
            <span class="control-label">{ label }</span>
            { for key_hint.map(|hint| html! { <span class="control-key">{ hint }</span> }) }
        </button>
    }
}

/// The run-state label: `running`/`halted` take precedence over whether a
/// session has started; otherwise `paused` (a started session that has since stopped)
/// or `stopped` (no session since the last load) distinguish a fresh load
/// from a mid-program pause -- including a Run stopped at a breakpoint on
/// the entry line, which starts a session even though nothing has executed
/// yet. `running && halted` cannot occur.
fn run_state_label(running: bool, halted: bool, session: bool) -> &'static str {
    if running {
        "running"
    } else if halted {
        "halted"
    } else if session {
        "paused"
    } else {
        "stopped"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_enablement_matches_the_run_lifecycle_table() {
        // `docs/layout-spec.md`'s Run lifecycle table, restated as
        // (running, halted, session, has_error) -> disabled state for every
        // control.
        let cases = [
            // (running, halted, session, has_error,
            //  run, continue, step, next, interrupt, reset)
            (
                false, false, false, false, false, true, false, false, true, false,
            ), // ready
            (
                true, false, true, false, true, true, true, true, false, true,
            ), // running
            // paused: a started session, neither running nor halted -- the
            // one state Continue is live in.
            (
                false, false, true, false, false, false, false, false, true, false,
            ), // paused
            (
                false, true, true, false, false, true, true, true, true, false,
            ), // halted
            // has_error forces every control disabled, regardless of what
            // running/halted/session would otherwise allow.
            (
                false, false, false, true, true, true, true, true, true, true,
            ), // ready + error
            (true, false, true, true, true, true, true, true, true, true), // running + error
            (false, false, true, true, true, true, true, true, true, true), // paused + error
            (false, true, true, true, true, true, true, true, true, true), // halted + error
        ];
        for (running, halted, session, has_error, run, continue_, step, next, interrupt, reset) in
            cases
        {
            let enablement = control_enablement(running, halted, session, has_error);
            let ctx = format!(
                "running={running} halted={halted} session={session} has_error={has_error}"
            );
            assert_eq!(enablement.run_disabled, run, "Run disabled at {ctx}");
            assert_eq!(
                enablement.continue_disabled, continue_,
                "Continue disabled at {ctx}"
            );
            assert_eq!(enablement.step_disabled, step, "Step disabled at {ctx}");
            assert_eq!(enablement.next_disabled, next, "Next disabled at {ctx}");
            assert_eq!(
                enablement.interrupt_disabled, interrupt,
                "Interrupt disabled at {ctx}"
            );
            assert_eq!(enablement.reset_disabled, reset, "Reset disabled at {ctx}");
        }
    }

    #[test]
    fn run_state_label_matches_every_reachable_state() {
        // running && halted cannot occur.
        let cases = [
            (false, false, false, "stopped"),
            (false, false, true, "paused"),
            (true, false, false, "running"),
            (true, false, true, "running"),
            (false, true, false, "halted"),
            (false, true, true, "halted"),
        ];
        for (running, halted, session, expected) in cases {
            assert_eq!(
                run_state_label(running, halted, session),
                expected,
                "running={running} halted={halted} session={session}"
            );
        }
    }

    #[test]
    fn reset_title_matches_the_owners_exact_text() {
        assert_eq!(
            RESET_TITLE,
            "Reload the program and return the machine to its start state: \
             registers, memory and output as loaded, PC at Main. Breakpoints are kept."
        );
    }
}
