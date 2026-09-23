//! Keeps the editor's program in the browser's `localStorage`, so a reload
//! restores it (§1 of `docs/layout-spec.md`'s companion prompt). The
//! share-link task builds on this module, so `STORAGE_KEY`, `load`, `save`,
//! and `confirm_needed` are a contract: renaming or reshaping any of them
//! strands that follow-up. Two tabs share one key; the last write wins,
//! with no cross-tab coordination.

use crate::examples::DEFAULT_MMS;

/// The key the saved program lives under. A stable name: renaming it stops
/// every prior save from being found, since a browser's `localStorage` is
/// keyed by exact string.
pub const STORAGE_KEY: &str = "playmmix.source";

/// The saved program, or `None` when there is nothing worth restoring --
/// absent, empty, or unreadable (a private window, a blocked origin, a
/// `getItem` error). The caller falls back to `DEFAULT_MMS` in every
/// `None` case alike.
pub fn load() -> Option<String> {
    restored_program(read_entry())
}

/// `load`'s plain core, split out so a host test can drive the restore
/// decision without a browser: an absent or empty entry restores nothing.
fn restored_program(entry: Option<String>) -> Option<String> {
    entry.filter(|source| !source.is_empty())
}

/// Saves `source` as typed, assembled or not -- unassembled work is still
/// work. `true` when the write reached storage; `false` on any failure,
/// logged rather than shown, since a lost save must not interrupt editing.
pub fn save(source: &str) -> bool {
    write_entry(source)
}

/// Whether New's replace-confirmation must show: `current` differs from
/// both the minimal skeleton (nothing to lose) and `replacement` (nothing
/// would change). Pure -- no browser call -- so New's own confirmation
/// text and dialog stay in `main.rs`, the one place that already owns
/// `window.confirm`.
pub fn confirm_needed(current: &str, replacement: &str) -> bool {
    current != DEFAULT_MMS && current != replacement
}

/// `Window::local_storage`'s `getItem`, `None` on any failure: no storage
/// object at all (`local_storage()` itself failing or returning `None`) or
/// a `getItem` error.
fn read_entry() -> Option<String> {
    let storage = web_sys::window()?.local_storage().ok().flatten()?;
    match storage.get_item(STORAGE_KEY) {
        Ok(value) => value,
        Err(_) => {
            log::warn!("autosave: could not read {STORAGE_KEY} from local storage");
            None
        }
    }
}

/// `Window::local_storage`'s `setItem`, `true` on success. `false` covers
/// every failure a user's browser can hand back: no storage object, a
/// blocked origin, or a full quota.
fn write_entry(source: &str) -> bool {
    let Some(storage) = web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    else {
        log::warn!("autosave: local storage is unavailable");
        return false;
    };
    match storage.set_item(STORAGE_KEY, source) {
        Ok(()) => true,
        Err(_) => {
            log::warn!("autosave: could not write {STORAGE_KEY} to local storage");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OTHER_PROGRAM_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,1\n\tTRAP\t0,Halt,0\n";

    #[test]
    fn confirm_needed_is_false_against_the_skeleton() {
        for candidate in ["", OTHER_PROGRAM_MMS, DEFAULT_MMS] {
            assert!(
                !confirm_needed(DEFAULT_MMS, candidate),
                "the skeleton itself never needs confirming, against {candidate:?}"
            );
        }
    }

    #[test]
    fn confirm_needed_is_false_when_current_matches_the_replacement() {
        assert!(!confirm_needed(OTHER_PROGRAM_MMS, OTHER_PROGRAM_MMS));
    }

    #[test]
    fn confirm_needed_is_true_for_distinct_work() {
        assert!(confirm_needed(OTHER_PROGRAM_MMS, DEFAULT_MMS));
    }

    #[test]
    fn restored_program_starts_from_default_with_nothing_saved() {
        assert_eq!(restored_program(None), None);
        assert_eq!(restored_program(Some(String::new())), None);
    }

    #[test]
    fn restored_program_returns_a_saved_program() {
        assert_eq!(
            restored_program(Some(OTHER_PROGRAM_MMS.to_string())),
            Some(OTHER_PROGRAM_MMS.to_string())
        );
    }
}
