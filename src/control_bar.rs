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
const INTERRUPT_TITLE: &str = "Interrupt (i): pause a Run, Continue, or Next in flight.";
/// Reset's title, exact per the owner's settled decision.
const RESET_TITLE: &str = "Reload the program and return the machine to its start state: \
registers, memory and output as loaded, PC at Main. Breakpoints are kept.";

/// Identifies a button, so `disabled_and_callback` can key
/// `ControlBarProps`' runtime-only disabled flag and callback to
/// `BUTTON_TABLE`'s row rather than to its position -- reordering the table
/// changes bar order, never which title or click handler a button gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Run,
    Continue,
    Step,
    Next,
    Interrupt,
    Reset,
}

/// The six controls, in bar order: identity, label, keyboard-shortcut key
/// (absent for Reset, mouse-only), and title. `ControlBar` renders every
/// button's face from this table, and the §8 face test iterates it too, so
/// the face and the keymap cannot drift apart.
const BUTTON_TABLE: [(Control, &str, Option<&str>, &str); 6] = [
    (Control::Run, "Run", Some("r"), RUN_TITLE),
    (Control::Continue, "Continue", Some("c"), CONTINUE_TITLE),
    (Control::Step, "Step", Some("s"), STEP_TITLE),
    (Control::Next, "Next", Some("n"), NEXT_TITLE),
    (Control::Interrupt, "Interrupt", Some("i"), INTERRUPT_TITLE),
    (Control::Reset, "Reset", None, RESET_TITLE),
];

/// Each control's disabled flag and click callback, keyed off its identity
/// -- see `Control`'s own doc comment for why this is a match, not a
/// position.
fn disabled_and_callback(
    control: Control,
    enablement: ControlEnablement,
    props: &ControlBarProps,
) -> (bool, Callback<()>) {
    match control {
        Control::Run => (enablement.run_disabled, props.on_run.clone()),
        Control::Continue => (enablement.continue_disabled, props.on_continue.clone()),
        Control::Step => (enablement.step_disabled, props.on_step.clone()),
        Control::Next => (enablement.next_disabled, props.on_next.clone()),
        Control::Interrupt => (enablement.interrupt_disabled, props.on_interrupt.clone()),
        Control::Reset => (enablement.reset_disabled, props.on_reset.clone()),
    }
}

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
            { for BUTTON_TABLE.into_iter().map(|(control, label, key, title)| {
                let (disabled, on_click) = disabled_and_callback(control, enablement, props);
                control_button(label, key, title, disabled, on_click)
            }) }
            <span class="run-state">
                { run_state_label(running, halted, props.session) }
            </span>
            <span class="status-message">{ &props.status }</span>
        </div>
    }
}

/// The button's face: its keyboard cue -- the label's first letter -- paired
/// with the rest of the label, or `None` when the table entry carries no key
/// (Reset, mouse-only). A plain function per `AGENTS.md`'s host-testable-
/// logic rule; `control_button` only renders its result.
fn control_face(
    label: &'static str,
    key: Option<&'static str>,
) -> Option<(&'static str, &'static str)> {
    key?;
    Some(label.split_at(1))
}

/// One control-pane button: a plain `<button>` so every control is
/// keyboard-reachable without extra wiring. `key`, when present, cues the
/// label's first letter via `control_face`; `title` names the action, its
/// key, and what it does in one clause.
fn control_button(
    label: &'static str,
    key: Option<&'static str>,
    title: &'static str,
    disabled: bool,
    on_click: Callback<()>,
) -> Html {
    let onclick = Callback::from(move |_| on_click.emit(()));
    let face = match control_face(label, key) {
        Some((cue, rest)) => html! {
            <span class="control-label">
                <span class="control-cue">{ cue }</span>
                { rest }
            </span>
        },
        None => html! { <span class="control-label">{ label }</span> },
    };
    html! {
        <button {disabled} {onclick} {title}>
            { face }
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
    use crate::keys::{KeyboardShortcut, keyboard_shortcut_for};

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

    #[test]
    fn face_carries_the_cue_and_matches_the_keymap() {
        let ready = control_enablement(false, false, false, false);
        let paused = control_enablement(false, false, true, false);
        let running = control_enablement(true, false, true, false);

        for (control, label, key, _title) in BUTTON_TABLE {
            // A state where this control is enabled, so its key can be
            // proven to actually fire its shortcut; `None` for Reset, whose
            // table row carries no key and whose face is checked below.
            let keyed = match control {
                Control::Run => Some((ready, KeyboardShortcut::Run)),
                Control::Continue => Some((paused, KeyboardShortcut::Continue)),
                Control::Step => Some((ready, KeyboardShortcut::Step)),
                Control::Next => Some((ready, KeyboardShortcut::Next)),
                Control::Interrupt => Some((running, KeyboardShortcut::Interrupt)),
                Control::Reset => None,
            };
            let Some((enablement, shortcut)) = keyed else {
                assert_eq!(key, None, "Reset must carry no key");
                assert_eq!(control_face(label, key), None, "Reset must carry no cue");
                continue;
            };

            let key = key.unwrap_or_else(|| panic!("{label} must carry a key"));
            assert_eq!(
                key,
                label[..1].to_lowercase(),
                "{label}'s key must be its label's first letter lowercased"
            );
            let (cue, rest) = control_face(label, Some(key))
                .unwrap_or_else(|| panic!("{label} must carry a cue"));
            assert_eq!(cue, &label[..1], "{label}'s cue must be its first letter");
            assert_eq!(
                format!("{cue}{rest}"),
                label,
                "{label}'s cue plus the rest must reconstruct the label"
            );

            assert_eq!(
                keyboard_shortcut_for(key, false, false, false, enablement),
                Some(shortcut),
                "{key} must fire {label} when its control is enabled"
            );
        }
    }

    #[test]
    fn button_table_is_in_bar_order() {
        let labels: Vec<&str> = BUTTON_TABLE.iter().map(|(_, label, ..)| *label).collect();
        assert_eq!(
            labels,
            ["Run", "Continue", "Step", "Next", "Interrupt", "Reset"],
            "BUTTON_TABLE's row order is the rendered bar order"
        );
    }

    #[test]
    fn titles_carry_every_key() {
        assert!(STEP_TITLE.contains("F11"), "Step's title must name F11");
        assert!(NEXT_TITLE.contains("F10"), "Next's title must name F10");
        assert!(
            INTERRUPT_TITLE.starts_with("Interrupt (i)"),
            "Interrupt's title must start with its key"
        );
    }

    #[test]
    fn keyed_titles_start_with_their_label_and_key() {
        // Reset carries no key; its title is pinned by its own exact-text
        // test above.
        for (_, label, key, title) in BUTTON_TABLE {
            let Some(key) = key else { continue };
            assert!(
                title.starts_with(&format!("{label} ({key}")),
                "{label}'s title must start with \"{label} ({key}\": {title}"
            );
        }
    }

    #[test]
    fn disabled_and_callback_ties_each_control_to_its_own_flag_and_callback() {
        let props = ControlBarProps {
            running: false,
            halted: false,
            session: false,
            has_error: false,
            on_run: Callback::from(|_| {}),
            on_continue: Callback::from(|_| {}),
            on_step: Callback::from(|_| {}),
            on_next: Callback::from(|_| {}),
            on_interrupt: Callback::from(|_| {}),
            on_reset: Callback::from(|_| {}),
            status: String::new(),
        };
        let callback_for = |control: Control| match control {
            Control::Run => props.on_run.clone(),
            Control::Continue => props.on_continue.clone(),
            Control::Step => props.on_step.clone(),
            Control::Next => props.on_next.clone(),
            Control::Interrupt => props.on_interrupt.clone(),
            Control::Reset => props.on_reset.clone(),
        };
        let every_control = [
            Control::Run,
            Control::Continue,
            Control::Step,
            Control::Next,
            Control::Interrupt,
            Control::Reset,
        ];

        for lone in every_control {
            // One-hot: only `lone`'s flag disabled. Every real lifecycle
            // state has Step == Next and Run == Reset (see
            // `control_enablement_matches_the_run_lifecycle_table`), so
            // only a synthetic, one-hot enablement like this -- never a
            // reachable `control_enablement` result -- can catch a swap
            // between them.
            let enablement = ControlEnablement {
                run_disabled: lone == Control::Run,
                continue_disabled: lone == Control::Continue,
                step_disabled: lone == Control::Step,
                next_disabled: lone == Control::Next,
                interrupt_disabled: lone == Control::Interrupt,
                reset_disabled: lone == Control::Reset,
            };
            for control in every_control {
                let (disabled, callback) = disabled_and_callback(control, enablement, &props);
                assert_eq!(
                    disabled,
                    control == lone,
                    "{control:?} disabled under a one-hot {lone:?} enablement"
                );
                assert_eq!(
                    callback,
                    callback_for(control),
                    "{control:?} must return its own callback regardless of enablement"
                );
            }
        }
    }
}
