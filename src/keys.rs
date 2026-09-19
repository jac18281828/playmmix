//! Keyboard shortcuts: mapping a keydown to a control-bar action.

use std::cell::Cell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Element, KeyboardEvent};

use crate::control_bar::{ControlEnablement, control_enablement};
use crate::{App, Msg};

/// A control-bar action triggered from the keyboard rather than a click.
/// Reset has no shortcut: it stays mouse-only, deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardShortcut {
    Run,
    Continue,
    Step,
    Next,
    Interrupt,
}

/// Whether a bare F10/F11 keydown must be prevented regardless of whether it
/// fires a control -- Firefox's menu bar (F10) and the browser's fullscreen
/// toggle (F11) must never trigger on this page. A chord -- Shift, Ctrl,
/// Cmd, or Alt held -- is never swallowed: Shift+F10 is the standard
/// context-menu key and must keep working, the editor included.
pub fn should_swallow_bare_fkey(key: &str, modifier_held: bool, shift_held: bool) -> bool {
    matches!(key, "F10" | "F11") && !modifier_held && !shift_held
}

/// Maps a bare keydown to the [`KeyboardShortcut`] it fires, gated by the
/// same [`ControlEnablement`] the control-bar buttons use -- a disabled
/// action stays a no-op from the keyboard too. `modifier_held` (Ctrl, Cmd,
/// or Alt) forces `None` regardless of `key`, keeping every OS chord
/// untouched. `key` is matched against the layout-produced string
/// (`KeyboardEvent::key()`), not the physical code, so a Shift-held letter
/// (which yields the uppercase form) never matches.
///
/// F10 (Next) and F11 (Step) fire even while a text-entry element has
/// focus -- they type nothing there -- but only with `shift_held` false
/// too: Shift+F11 is VS Code's step-out, which playmmix lacks. Every other
/// key -- `r` Run, `c` Continue, `s` Step, `n` Next, `i` Interrupt -- fires
/// only outside a text-entry element, `focused_element_is_text_input`.
/// `x` maps to nothing.
pub fn keyboard_shortcut_for(
    key: &str,
    modifier_held: bool,
    shift_held: bool,
    focused_element_is_text_input: bool,
    enablement: ControlEnablement,
) -> Option<KeyboardShortcut> {
    if modifier_held {
        return None;
    }
    match key {
        "F10" if !shift_held && !enablement.next_disabled => Some(KeyboardShortcut::Next),
        "F11" if !shift_held && !enablement.step_disabled => Some(KeyboardShortcut::Step),
        _ if focused_element_is_text_input => None,
        "r" if !enablement.run_disabled => Some(KeyboardShortcut::Run),
        "c" if !enablement.continue_disabled => Some(KeyboardShortcut::Continue),
        "s" if !enablement.step_disabled => Some(KeyboardShortcut::Step),
        "n" if !enablement.next_disabled => Some(KeyboardShortcut::Next),
        "i" if !enablement.interrupt_disabled => Some(KeyboardShortcut::Interrupt),
        _ => None,
    }
}

/// The whole `window.onkeydown` decision for a bare (non-Ctrl-S) keydown:
/// which shortcut it fires, if any, and whether the browser's own default
/// action must be prevented regardless. One function, not two split across
/// [`should_swallow_bare_fkey`] and the wasm-only closure that calls it: a
/// mutant that only prevents the default when a shortcut actually fired --
/// exactly the bug bare F10/F11 swallowing exists to rule out, since Firefox's
/// menu bar and the browser's fullscreen toggle must never trigger here even
/// while both controls are disabled -- must be visible to one function's own
/// test, not hidden behind two call sites the closure alone recombines.
pub fn keydown_decision(
    key: &str,
    modifier_held: bool,
    shift_held: bool,
    focused_element_is_text_input: bool,
    enablement: ControlEnablement,
) -> (Option<KeyboardShortcut>, bool) {
    let must_swallow = should_swallow_bare_fkey(key, modifier_held, shift_held);
    let shortcut = keyboard_shortcut_for(
        key,
        modifier_held,
        shift_held,
        focused_element_is_text_input,
        enablement,
    );
    (shortcut, must_swallow || shortcut.is_some())
}

/// Whether a keydown is the platform Save chord (Ctrl-S / Cmd-S), unlike
/// [`keyboard_shortcut_for`]'s bare-key shortcuts. No `ControlEnablement`
/// gate: flushing a pending re-assemble is always safe to attempt, the same
/// way `flush_pending_reassemble` unconditionally checks whether anything is
/// pending.
pub fn save_shortcut(key: &str, ctrl_or_meta_held: bool) -> bool {
    key == "s" && ctrl_or_meta_held
}

/// The JS closure backing `window.onkeydown` -- must stay alive for as long
/// as the handler should stay registered, same as
/// [`crate::BeforeUnloadHandler`].
pub(crate) type KeydownHandler = Closure<dyn FnMut(KeyboardEvent)>;

/// Registers `window.onkeydown`. Ctrl-S / Cmd-S (`save_shortcut`) dispatches
/// `Msg::FlushSource` and suppresses the browser's Save dialog regardless of
/// focus, checked first since it is the one shortcut that must fire while a
/// text-entry element is focused. Every other keydown's whole decision --
/// which shortcut fires, if any, and whether the browser's own default
/// action must be prevented regardless -- comes from one call to
/// `keydown_decision`, gated by the returned cell's current
/// `ControlEnablement` -- kept live by `App::update`, not recomputed here.
/// Seeded with the ready state (`control_enablement(false, false, false,
/// false)`), matching a freshly-constructed `Control`. Returns the shared
/// cell alongside the `Closure` backing the handler; the caller must keep the
/// latter alive (see [`KeydownHandler`]).
pub(crate) fn install_keyboard_shortcuts(
    link: yew::html::Scope<App>,
) -> (Rc<Cell<ControlEnablement>>, KeydownHandler) {
    let enablement = Rc::new(Cell::new(control_enablement(false, false, false, false)));
    let enablement_for_handler = enablement.clone();
    let handler = Closure::wrap(Box::new(move |event: KeyboardEvent| {
        let key = event.key();
        // Checked before the text-input bail-out below, unlike the other
        // shortcuts: Ctrl-S's only realistic use is while typing in the
        // source editor, so it must fire regardless of focus.
        if save_shortcut(&key, event.ctrl_key() || event.meta_key()) {
            event.prevent_default();
            link.send_message(Msg::FlushSource);
            return;
        }
        let modifier_held = event.ctrl_key() || event.meta_key() || event.alt_key();
        let shift_held = event.shift_key();
        let focused_element_is_text_input = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
            .map(|element| matches!(element.tag_name().as_str(), "TEXTAREA" | "INPUT"))
            .unwrap_or(false);
        let (shortcut, prevent_default) = keydown_decision(
            &key,
            modifier_held,
            shift_held,
            focused_element_is_text_input,
            enablement_for_handler.get(),
        );
        if prevent_default {
            event.prevent_default();
        }
        if let Some(shortcut) = shortcut {
            let msg = match shortcut {
                KeyboardShortcut::Run => Msg::Run,
                KeyboardShortcut::Continue => Msg::Continue,
                KeyboardShortcut::Step => Msg::Step,
                KeyboardShortcut::Next => Msg::Next,
                KeyboardShortcut::Interrupt => Msg::Interrupt,
            };
            link.send_message(msg);
        }
    }) as Box<dyn FnMut(KeyboardEvent)>);

    if let Some(window) = web_sys::window() {
        window.set_onkeydown(Some(handler.as_ref().unchecked_ref()));
    }

    (enablement, handler)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_shortcut_for_maps_each_key_when_enabled() {
        let ready = control_enablement(false, false, false, false);
        let paused = control_enablement(false, false, true, false);
        let running = control_enablement(true, false, true, false);
        let cases = [
            ("r", ready, KeyboardShortcut::Run),
            ("c", paused, KeyboardShortcut::Continue),
            ("s", ready, KeyboardShortcut::Step),
            ("n", ready, KeyboardShortcut::Next),
            ("i", running, KeyboardShortcut::Interrupt),
            ("F11", ready, KeyboardShortcut::Step),
            ("F10", ready, KeyboardShortcut::Next),
        ];
        for (key, enablement, expected) in cases {
            assert_eq!(
                keyboard_shortcut_for(key, false, false, false, enablement),
                Some(expected),
                "key {key:?} must map to {expected:?} when its control is enabled"
            );
        }
    }

    #[test]
    fn keyboard_shortcut_for_maps_f10_and_f11_even_inside_a_text_entry_element() {
        let ready = control_enablement(false, false, false, false);
        assert_eq!(
            keyboard_shortcut_for("F10", false, false, true, ready),
            Some(KeyboardShortcut::Next),
            "F10 must fire Next even while a text input is focused"
        );
        assert_eq!(
            keyboard_shortcut_for("F11", false, false, true, ready),
            Some(KeyboardShortcut::Step),
            "F11 must fire Step even while a text input is focused"
        );
    }

    #[test]
    fn keyboard_shortcut_for_is_none_when_its_control_is_disabled() {
        // `running`: Step and Next disable (Interrupt is the live control).
        let running = control_enablement(true, false, true, false);
        assert_eq!(
            keyboard_shortcut_for("s", false, false, false, running),
            None
        );
        assert_eq!(
            keyboard_shortcut_for("n", false, false, false, running),
            None
        );
        assert_eq!(
            keyboard_shortcut_for("F11", false, false, false, running),
            None
        );
        assert_eq!(
            keyboard_shortcut_for("F10", false, false, false, running),
            None
        );

        // `halted`: Interrupt disables (nothing left to interrupt).
        let halted = control_enablement(false, true, true, false);
        assert_eq!(
            keyboard_shortcut_for("i", false, false, false, halted),
            None
        );

        // `ready`: Continue disables (no session yet).
        let ready = control_enablement(false, false, false, false);
        assert_eq!(keyboard_shortcut_for("c", false, false, false, ready), None);

        // Run has no reachable state above that disables it alone; hand-build
        // one to cover run_disabled directly.
        let run_disabled = ControlEnablement {
            run_disabled: true,
            continue_disabled: false,
            step_disabled: false,
            next_disabled: false,
            interrupt_disabled: false,
            reset_disabled: false,
        };
        assert_eq!(
            keyboard_shortcut_for("r", false, false, false, run_disabled),
            None
        );
    }

    #[test]
    fn keyboard_shortcut_for_ignores_every_key_while_a_modifier_is_held() {
        let ready = control_enablement(false, false, false, false);
        let paused = control_enablement(false, false, true, false);
        let running = control_enablement(true, false, true, false);
        let cases = [
            ("r", ready),
            ("c", paused),
            ("s", ready),
            ("n", ready),
            ("i", running),
            ("F10", ready),
            ("F11", ready),
        ];
        for (key, enablement) in cases {
            assert_eq!(
                keyboard_shortcut_for(key, true, false, false, enablement),
                None,
                "key {key:?} must not fire while a modifier is held, even when \
                 its control is otherwise enabled"
            );
        }
    }

    #[test]
    fn keyboard_shortcut_for_ignores_letters_while_a_text_input_has_focus() {
        let ready = control_enablement(false, false, false, false);
        let paused = control_enablement(false, false, true, false);
        let running = control_enablement(true, false, true, false);
        let cases = [
            ("r", ready),
            ("c", paused),
            ("s", ready),
            ("n", ready),
            ("i", running),
        ];
        for (key, enablement) in cases {
            assert_eq!(
                keyboard_shortcut_for(key, false, false, true, enablement),
                None,
                "letter {key:?} must not fire while a text input is focused"
            );
        }
    }

    #[test]
    fn keyboard_shortcut_for_blocks_f10_and_f11_while_shift_is_held() {
        let ready = control_enablement(false, false, false, false);
        assert_eq!(
            keyboard_shortcut_for("F10", false, true, false, ready),
            None
        );
        assert_eq!(
            keyboard_shortcut_for("F11", false, true, false, ready),
            None
        );
    }

    #[test]
    fn keyboard_shortcut_for_ignores_an_unmapped_key() {
        let paused = control_enablement(false, false, true, false);
        assert_eq!(
            keyboard_shortcut_for("q", false, false, false, paused),
            None
        );
        // `running`, not `paused`: Interrupt is disabled while paused, so an
        // `x`-as-Interrupt arm would return `None` there regardless, making
        // the case vacuous. Running is where Interrupt is live.
        let running = control_enablement(true, false, true, false);
        assert_eq!(
            keyboard_shortcut_for("x", false, false, false, running),
            None,
            "x must map to nothing"
        );
    }

    #[test]
    fn i_is_interrupt_only_while_running_and_x_never_fires() {
        let ready = control_enablement(false, false, false, false);
        let running = control_enablement(true, false, true, false);
        let paused = control_enablement(false, false, true, false);
        let halted = control_enablement(false, true, true, false);

        assert_eq!(
            keyboard_shortcut_for("i", false, false, false, running),
            Some(KeyboardShortcut::Interrupt),
            "i must fire Interrupt while running"
        );
        for (state, enablement) in [("ready", ready), ("paused", paused), ("halted", halted)] {
            assert_eq!(
                keyboard_shortcut_for("i", false, false, false, enablement),
                None,
                "i must fire nothing while {state}"
            );
        }
        assert_eq!(
            keyboard_shortcut_for("x", false, false, false, running),
            None,
            "x must fire nothing while running"
        );
    }

    #[test]
    fn keyboard_shortcut_for_does_not_lowercase_shift_held_keys() {
        // `key()` reports Shift-held letters in uppercase; matching only the
        // lowercase form is what keeps Shift+S from firing Step.
        let enablement = control_enablement(false, false, false, false);
        assert_eq!(
            keyboard_shortcut_for("S", false, false, false, enablement),
            None
        );
    }

    #[test]
    fn bare_f10_is_swallowed_even_when_its_control_is_disabled() {
        let running = control_enablement(true, false, true, false); // Next disabled
        assert_eq!(
            keyboard_shortcut_for("F10", false, false, false, running),
            None,
            "fixture assumption: Next is disabled while running"
        );
        assert!(
            should_swallow_bare_fkey("F10", false, false),
            "a bare F10 must still be swallowed even when it fires nothing"
        );
    }

    #[test]
    fn should_swallow_bare_fkey_swallows_f10_and_f11_with_no_modifier_or_shift() {
        assert!(should_swallow_bare_fkey("F10", false, false));
        assert!(should_swallow_bare_fkey("F11", false, false));
    }

    #[test]
    fn should_swallow_bare_fkey_never_swallows_shift_f10() {
        assert!(!should_swallow_bare_fkey("F10", false, true));
    }

    #[test]
    fn should_swallow_bare_fkey_never_swallows_a_modifier_held_fkey() {
        assert!(!should_swallow_bare_fkey("F10", true, false));
        assert!(!should_swallow_bare_fkey("F11", true, false));
    }

    #[test]
    fn should_swallow_bare_fkey_ignores_every_other_key() {
        assert!(!should_swallow_bare_fkey("F9", false, false));
        assert!(!should_swallow_bare_fkey("r", false, false));
    }

    #[test]
    fn keydown_decision_swallows_a_bare_f10_even_when_its_control_is_disabled() {
        // Next disabled (halted): no shortcut fires, but F10 must still be
        // prevented -- Firefox's menu bar doesn't care whether playmmix had
        // anything to do with the key. A mutant that only sets `must_swallow`
        // when a shortcut also fired (splitting the two decisions back apart,
        // the bug this function exists to rule out) would report `false`
        // here instead.
        let disabled = control_enablement(false, true, false, false);
        let (shortcut, prevent_default) = keydown_decision("F10", false, false, false, disabled);
        assert_eq!(shortcut, None);
        assert!(prevent_default);
    }

    #[test]
    fn keydown_decision_fires_and_swallows_a_bare_f10_when_enabled() {
        let ready = control_enablement(false, false, false, false);
        let (shortcut, prevent_default) = keydown_decision("F10", false, false, false, ready);
        assert_eq!(shortcut, Some(KeyboardShortcut::Next));
        assert!(prevent_default);
    }

    #[test]
    fn keydown_decision_never_swallows_shift_f10() {
        let ready = control_enablement(false, false, false, false);
        let (shortcut, prevent_default) = keydown_decision("F10", false, true, false, ready);
        assert_eq!(shortcut, None);
        assert!(!prevent_default);
    }

    #[test]
    fn keydown_decision_only_prevents_default_for_a_fired_ordinary_key() {
        let ready = control_enablement(false, false, false, false);
        let (fired, prevent_fired) = keydown_decision("r", false, false, false, ready);
        assert_eq!(fired, Some(KeyboardShortcut::Run));
        assert!(prevent_fired);

        // Run disables while already running -- unlike halted, where Run
        // stays live (Reset's own gate).
        let running = control_enablement(true, false, false, false);
        let (not_fired, prevent_not_fired) = keydown_decision("r", false, false, false, running);
        assert_eq!(not_fired, None);
        assert!(!prevent_not_fired);
    }

    #[test]
    fn save_shortcut_matches_s_with_a_modifier_held() {
        assert!(save_shortcut("s", true));
    }

    #[test]
    fn save_shortcut_ignores_s_without_a_modifier_held() {
        assert!(!save_shortcut("s", false));
    }

    #[test]
    fn save_shortcut_ignores_every_other_key_even_with_a_modifier_held() {
        assert!(!save_shortcut("r", true));
    }
}
