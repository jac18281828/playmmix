//! Machine pane: general registers, special registers, and loaded memory.
//!
//! Computation is plain functions over `&MMix` and the assembler's label
//! table (`AGENTS.md`'s rule that logic not needing browser APIs stays
//! host-testable); [`MachinePane`] only renders their *owned* output --
//! `Properties` must be `'static`, so a borrowed `&MMix` can't cross that
//! boundary. [`ViewState`] -- the continuity trackers, the pause-boundary
//! snapshot, and the `diff_*` functions over it -- is the same kind of
//! plain, testable state: cross-render view state `App` owns, not machine
//! state.

#[cfg(test)]
mod fixtures;
mod memory;
mod pane;
mod registers;
mod view_state;

pub use pane::MachinePane;
pub use view_state::ViewState;
