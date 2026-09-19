//! The resize splitters: layout math and the drag-handling wiring.

use std::cell::RefCell;
use std::rc::Rc;

use web_sys::{Element, PointerEvent};
use yew::{Callback, NodeRef};

/// Resize splitters (§1.3): both handles' fixed grid-track size, pixels --
/// `style.css`'s `main` grid, the `6px` column/row track each occupies.
const SPLITTER_SIZE_PX: f64 = 6.0;

/// `main`'s grid `gap`, pixels, at the stylesheet's 16px root font size --
/// `style.css`: `gap: 0.75rem`. Inserting a dedicated splitter track means
/// two of these separate the left column from the machine column (and,
/// vertically, the editor row from the output row), not one.
const GRID_GAP_PX: f64 = 12.0;

/// Column splitter's floor for the left (editor+output) column, pixels --
/// `20rem`'s worth at the 16px root font size.
const LEFT_COLUMN_FLOOR_PX: f64 = 320.0;

/// The machine column's own floor, pixels -- `style.css`'s
/// `minmax(38rem, 42rem)`. The column splitter's ceiling must never let the
/// left column push the machine column narrower than this: CSS grid does
/// not shrink an over-large explicit track to protect a sibling's floor, it
/// overflows the container instead.
const MACHINE_COLUMN_FLOOR_PX: f64 = 608.0;

/// Row splitter's floor for the output pane, pixels -- roughly its header
/// plus a couple of lines of monospace output.
const OUTPUT_FLOOR_PX: f64 = 96.0;

/// The editor pane's own minimum share the row splitter's ceiling
/// preserves, pixels.
const EDITOR_MIN_SHARE_PX: f64 = 128.0;

/// Minimum net pointer displacement, pixels, a drag must cross before its
/// `pointerup` commits a resize. A bare click -- zero or near-zero
/// movement, e.g. a stray click that happens to land on a splitter -- must
/// leave the dragged dimension exactly as it found it (the stylesheet's
/// fluid default, if never dragged before) rather than pinning it to
/// today's rendered pixel size.
const DRAG_COMMIT_THRESHOLD_PX: f64 = 3.0;

/// The column splitter's ceiling, expressed as the `calc()` `style.css`'s
/// own rules would compute (the `6px` splitter track, the two `0.75rem`
/// grid gaps either side of it, the machine column's `38rem` floor).
/// Embedded directly into the `--left-col` value a commit writes (see
/// `main_style`) rather than pre-computed in Rust: the browser re-evaluates
/// this `calc()` on every reflow, so a window narrowed after a drag -- with
/// no further drag -- still can't push the grid past this ceiling (finding
/// 3), with no resize listener needed.
const LEFT_COLUMN_CEILING_CALC: &str = "calc(100% - 6px - 1.5rem - 38rem)";

/// The row splitter's vertical twin of `LEFT_COLUMN_CEILING_CALC`: the same
/// `min(px, calc(...))` backstop, embedded in `--output-h` (see
/// `main_style`), so a window shortened after a drag -- with no further
/// drag -- can't push the output pane tall enough to starve the editor,
/// mirroring finding 3's horizontal fix for the vertical axis. Only
/// `grid-template-rows` reads this value directly; `.output-pane`'s own
/// `max-height` deliberately does not (see that rule's comment in
/// `style.css` for why feeding it the same percentage-bearing value once
/// capped the pane 158px short of its own track).
///
/// Three `0.75rem` gaps separate this axis's four row tracks (header,
/// editor, splitter, output) -- not two, the column axis's count between
/// its three tracks. Unlike the horizontal ceiling, this also can't
/// subtract the header row's own height: `style.css`'s `main` grid sizes it
/// `auto` (`.app-header` can wrap onto a second line), which a static
/// `calc()` has no way to read. The `100%` here is `main`'s full height,
/// header included, so this ceiling is conservative rather than exact -- it
/// can still let `--output-h` claim the header's height more than it
/// strictly leaves free. `style.css`'s `minmax(8rem, 1fr)` on the editor row
/// (`EDITOR_MIN_SHARE_PX`, matched below) is the actual floor guarantee:
/// together, the editor's rendered height never drops below its share, and
/// any residual overflow this ceiling misses is bounded to the header's own
/// height rather than growing without limit as the viewport shrinks (the
/// unfixed behavior).
const OUTPUT_HEIGHT_CEILING_CALC: &str = "calc(100% - 6px - 2.25rem - 8rem)";

/// Clamp the column splitter's requested left-column width to the range
/// `style.css`'s grid can actually render without overflowing: never
/// narrower than `LEFT_COLUMN_FLOOR_PX`, and never wide enough to push the
/// machine column below its own `minmax(38rem, 42rem)` floor.
/// `container_width_px` is `main`'s own rendered width.
fn clamp_left_column_width(requested_px: f64, container_width_px: f64) -> f64 {
    let ceiling =
        (container_width_px - SPLITTER_SIZE_PX - 2.0 * GRID_GAP_PX - MACHINE_COLUMN_FLOOR_PX)
            .max(LEFT_COLUMN_FLOOR_PX);
    requested_px.clamp(LEFT_COLUMN_FLOOR_PX, ceiling)
}

/// Clamp the row splitter's requested output-pane height: never shorter
/// than `OUTPUT_FLOOR_PX` (its header plus a couple of lines), and never
/// tall enough to leave the editor pane less than `EDITOR_MIN_SHARE_PX`.
/// `column_height_px` is the combined height available to the editor, row
/// splitter, and output pane together (excludes the header row).
fn clamp_output_height(requested_px: f64, column_height_px: f64) -> f64 {
    let ceiling = (column_height_px - SPLITTER_SIZE_PX - 2.0 * GRID_GAP_PX - EDITOR_MIN_SHARE_PX)
        .max(OUTPUT_FLOOR_PX);
    requested_px.clamp(OUTPUT_FLOOR_PX, ceiling)
}

/// Which boundary a resize drag is adjusting -- shared by both splitter
/// handles so one `DragState`/`Rc<RefCell<..>>` and matching pointer-event
/// wiring can serve both (only one drag is ever in flight at a time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Splitter {
    Column,
    Row,
}

/// Live drag state, captured at `pointerdown` and read by every
/// `pointermove` until `pointerup` commits it. Kept off `App` and out of
/// the `Msg`/`update` cycle entirely during the drag itself: a message per
/// pointer move would re-render the whole `App`, including the machine
/// pane's register/memory diffs, for every pixel dragged.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DragState {
    which: Splitter,
    pointer_start_client_px: f64,
    size_start_px: f64,
    /// The other clamp parameter for `which` -- `main`'s width for
    /// `Column`, the editor/splitter/output column's combined height for
    /// `Row` -- read once at `pointerdown` rather than on every move.
    ceiling_param_px: f64,
}

/// The inline `style` attribute for `<main>`: the resize custom properties,
/// set only for a dimension a drag has actually touched (`None` means "use
/// the stylesheet's default", so nothing is written for it), plus
/// `user-select: none` for an active drag's duration -- the direct fix for
/// a drag starting a text selection instead of resizing. Both dimensions'
/// properties are always built together from the same two values, whether
/// called from `view()` or from a live drag's imperative write, so
/// committing one drag never clobbers the other's already-set properties.
///
/// `--left-col` and `--output-h` are wrapped in `min(..., ...CEILING_CALC)`
/// (finding 3 and its vertical twin): the plain px value alone would go
/// stale the moment the window resizes again without a new drag, since
/// nothing re-clamps it but another drag. `--output-pane-cap` rides
/// alongside `--output-h`, written only when it is (i.e. only once a drag
/// has committed): a fixed `"100%"`, never a length, and never read by
/// `grid-template-rows` -- `.output-pane`'s own `max-height` reads it
/// instead, exactly when its containing block (the row track) is a
/// definite size rather than the undragged default's `auto` (see that
/// rule's comment in `style.css`).
pub(crate) fn main_style(
    left_column_width: Option<f64>,
    output_height: Option<f64>,
    dragging: bool,
) -> String {
    let mut style = String::new();
    if let Some(px) = left_column_width {
        style.push_str(&format!(
            "--left-col:min({px}px,{LEFT_COLUMN_CEILING_CALC});"
        ));
    }
    if let Some(px) = output_height {
        style.push_str(&format!(
            "--output-h:min({px}px,{OUTPUT_HEIGHT_CEILING_CALC});--output-pane-cap:100%;"
        ));
    }
    if dragging {
        style.push_str("user-select:none;");
    }
    style
}

/// Write `main_style`'s result directly onto `main_ref`'s DOM node --
/// `pointerdown`/`pointermove`/`pointerup`'s shared imperative-write path,
/// bypassing `Component::update` so a live drag never re-renders `App`.
/// `dragging_value_px` is the dimension `which` is adjusting -- `Some` for
/// a live drag or a commit, `None` to revert it to "use the stylesheet's
/// default" (a bare click, or a cancelled drag, undoing whatever
/// `pointerdown` wrote speculatively).
fn write_drag_style(
    main_ref: &NodeRef,
    which: Splitter,
    dragging_value_px: Option<f64>,
    other_dimension_px: Option<f64>,
    dragging: bool,
) {
    let (left_column_width, output_height) = match which {
        Splitter::Column => (dragging_value_px, other_dimension_px),
        Splitter::Row => (other_dimension_px, dragging_value_px),
    };
    let style = main_style(left_column_width, output_height, dragging);
    if let Some(element) = main_ref.cast::<Element>() {
        let _ = element.set_attribute("style", &style);
    }
}

/// Map a drag's start size and net pointer displacement to the resized
/// extent -- the one place the column and row splitters' otherwise-
/// identical math must differ. The column splitter's pane sits left of its
/// handle, so dragging right grows it; the row splitter's output pane sits
/// below its handle, so dragging down (which grows the space above the
/// handle) shrinks it. Plain and host-testable so a future sign flip is
/// caught by `cargo test`, not just by a human dragging it in a browser.
fn resized_extent(size_start_px: f64, delta_px: f64, splitter: Splitter) -> f64 {
    match splitter {
        Splitter::Column => size_start_px + delta_px,
        Splitter::Row => size_start_px - delta_px,
    }
}

/// Capture the pointer on `handle_ref`'s element so a drag keeps tracking
/// once the pointer leaves the thin splitter handle.
fn capture_pointer(handle_ref: &NodeRef, pointer_id: i32) {
    if let Some(element) = handle_ref.cast::<Element>() {
        let _ = element.set_pointer_capture(pointer_id);
    }
}

/// Both dimensions' current, `App`-committed values -- `None` means "use
/// the stylesheet's default". Passed as one unit to each splitter's handler
/// builder: a drag's `pointerup`/`pointercancel` needs both its own
/// dimension, to revert a bare click or a cancelled drag to exactly what
/// `<main>` had before `pointerdown` touched it, and the other, so a live
/// style write never clobbers it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CommittedSizes {
    pub(crate) left_column_width: Option<f64>,
    pub(crate) output_height: Option<f64>,
}

/// Build the column splitter's `pointerdown`/`pointermove`/`pointerup`/
/// `pointercancel` callbacks. `left_col_ref` is any element whose rendered
/// width equals the left column's current width -- `App::row_splitter_ref`
/// works, since the row splitter's handle lives in that same column.
/// `committed` is `App`'s own current values for both dimensions -- see
/// `CommittedSizes`'s doc comment.
pub(crate) fn column_splitter_handlers(
    drag_state: Rc<RefCell<Option<DragState>>>,
    main_ref: NodeRef,
    handle_ref: NodeRef,
    left_col_ref: NodeRef,
    committed: CommittedSizes,
    on_commit: Callback<f64>,
) -> (
    Callback<PointerEvent>,
    Callback<PointerEvent>,
    Callback<PointerEvent>,
    Callback<PointerEvent>,
) {
    let CommittedSizes {
        left_column_width,
        output_height,
    } = committed;

    let onpointerdown = {
        let drag_state = drag_state.clone();
        let main_ref = main_ref.clone();
        let handle_ref = handle_ref.clone();
        let left_col_ref = left_col_ref.clone();
        Callback::from(move |event: PointerEvent| {
            let size_start_px = left_col_ref
                .cast::<Element>()
                .map(|element| element.client_width() as f64)
                .unwrap_or(0.0);
            let ceiling_param_px = main_ref
                .cast::<Element>()
                .map(|element| element.client_width() as f64)
                .unwrap_or(0.0);
            *drag_state.borrow_mut() = Some(DragState {
                which: Splitter::Column,
                pointer_start_client_px: event.client_x() as f64,
                size_start_px,
                ceiling_param_px,
            });
            capture_pointer(&handle_ref, event.pointer_id());
            write_drag_style(
                &main_ref,
                Splitter::Column,
                Some(size_start_px),
                output_height,
                true,
            );
        })
    };

    let onpointermove = {
        let drag_state = drag_state.clone();
        let main_ref = main_ref.clone();
        Callback::from(move |event: PointerEvent| {
            let Some(state) = *drag_state.borrow() else {
                return;
            };
            if state.which != Splitter::Column {
                return;
            }
            let delta = event.client_x() as f64 - state.pointer_start_client_px;
            let requested = resized_extent(state.size_start_px, delta, Splitter::Column);
            let clamped = clamp_left_column_width(requested, state.ceiling_param_px);
            write_drag_style(
                &main_ref,
                Splitter::Column,
                Some(clamped),
                output_height,
                true,
            );
        })
    };

    let onpointerup = {
        let drag_state = drag_state.clone();
        let main_ref = main_ref.clone();
        Callback::from(move |event: PointerEvent| {
            let Some(state) = drag_state.borrow_mut().take() else {
                return;
            };
            if state.which != Splitter::Column {
                return;
            }
            let delta = event.client_x() as f64 - state.pointer_start_client_px;
            if delta.abs() <= DRAG_COMMIT_THRESHOLD_PX {
                write_drag_style(
                    &main_ref,
                    Splitter::Column,
                    left_column_width,
                    output_height,
                    false,
                );
                return;
            }
            let requested = resized_extent(state.size_start_px, delta, Splitter::Column);
            let clamped = clamp_left_column_width(requested, state.ceiling_param_px);
            write_drag_style(
                &main_ref,
                Splitter::Column,
                Some(clamped),
                output_height,
                false,
            );
            on_commit.emit(clamped);
        })
    };

    let onpointercancel = {
        Callback::from(move |_event: PointerEvent| {
            let Some(state) = drag_state.borrow_mut().take() else {
                return;
            };
            if state.which != Splitter::Column {
                return;
            }
            write_drag_style(
                &main_ref,
                Splitter::Column,
                left_column_width,
                output_height,
                false,
            );
        })
    };

    (onpointerdown, onpointermove, onpointerup, onpointercancel)
}

/// Build the row splitter's `pointerdown`/`pointermove`/`pointerup`/
/// `pointercancel` callbacks. `column_height_ref` is any element spanning
/// exactly the editor, row-splitter, and output rows --
/// `App::col_splitter_ref` works, since the column splitter's handle spans
/// that same range. `output_pane_ref` is the output pane's own element,
/// read at `pointerdown` for the drag-start height -- unlike the column
/// splitter's fluid `minmax(0, 1fr)` default, the output pane's rendered
/// height when never dragged is content-driven (`.output-pane`'s `max-
/// height` caps it, it doesn't fix it), so, exactly as the column splitter
/// reads `left_col_ref`, this must be a live DOM read, not a constant
/// (finding 2). `committed` is `App`'s own current values for both
/// dimensions -- see `CommittedSizes`'s doc comment.
pub(crate) fn row_splitter_handlers(
    drag_state: Rc<RefCell<Option<DragState>>>,
    main_ref: NodeRef,
    handle_ref: NodeRef,
    column_height_ref: NodeRef,
    output_pane_ref: NodeRef,
    committed: CommittedSizes,
    on_commit: Callback<f64>,
) -> (
    Callback<PointerEvent>,
    Callback<PointerEvent>,
    Callback<PointerEvent>,
    Callback<PointerEvent>,
) {
    let CommittedSizes {
        left_column_width,
        output_height,
    } = committed;

    let onpointerdown = {
        let drag_state = drag_state.clone();
        let main_ref = main_ref.clone();
        let handle_ref = handle_ref.clone();
        let column_height_ref = column_height_ref.clone();
        let output_pane_ref = output_pane_ref.clone();
        Callback::from(move |event: PointerEvent| {
            let size_start_px = output_pane_ref
                .cast::<Element>()
                .map(|element| element.client_height() as f64)
                .unwrap_or(0.0);
            let ceiling_param_px = column_height_ref
                .cast::<Element>()
                .map(|element| element.client_height() as f64)
                .unwrap_or(0.0);
            *drag_state.borrow_mut() = Some(DragState {
                which: Splitter::Row,
                pointer_start_client_px: event.client_y() as f64,
                size_start_px,
                ceiling_param_px,
            });
            capture_pointer(&handle_ref, event.pointer_id());
            write_drag_style(
                &main_ref,
                Splitter::Row,
                Some(size_start_px),
                left_column_width,
                true,
            );
        })
    };

    let onpointermove = {
        let drag_state = drag_state.clone();
        let main_ref = main_ref.clone();
        Callback::from(move |event: PointerEvent| {
            let Some(state) = *drag_state.borrow() else {
                return;
            };
            if state.which != Splitter::Row {
                return;
            }
            let delta = event.client_y() as f64 - state.pointer_start_client_px;
            let requested = resized_extent(state.size_start_px, delta, Splitter::Row);
            let clamped = clamp_output_height(requested, state.ceiling_param_px);
            write_drag_style(
                &main_ref,
                Splitter::Row,
                Some(clamped),
                left_column_width,
                true,
            );
        })
    };

    let onpointerup = {
        let drag_state = drag_state.clone();
        let main_ref = main_ref.clone();
        Callback::from(move |event: PointerEvent| {
            let Some(state) = drag_state.borrow_mut().take() else {
                return;
            };
            if state.which != Splitter::Row {
                return;
            }
            let delta = event.client_y() as f64 - state.pointer_start_client_px;
            if delta.abs() <= DRAG_COMMIT_THRESHOLD_PX {
                write_drag_style(
                    &main_ref,
                    Splitter::Row,
                    output_height,
                    left_column_width,
                    false,
                );
                return;
            }
            let requested = resized_extent(state.size_start_px, delta, Splitter::Row);
            let clamped = clamp_output_height(requested, state.ceiling_param_px);
            write_drag_style(
                &main_ref,
                Splitter::Row,
                Some(clamped),
                left_column_width,
                false,
            );
            on_commit.emit(clamped);
        })
    };

    let onpointercancel = {
        Callback::from(move |_event: PointerEvent| {
            let Some(state) = drag_state.borrow_mut().take() else {
                return;
            };
            if state.which != Splitter::Row {
                return;
            }
            write_drag_style(
                &main_ref,
                Splitter::Row,
                output_height,
                left_column_width,
                false,
            );
        })
    };

    (onpointerdown, onpointermove, onpointerup, onpointercancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative desktop container width/height, pixels -- large
    /// enough that both clamps' floor and ceiling are simultaneously
    /// reachable (i.e. ceiling > floor), so every case below tests a real
    /// clamp rather than the `.max(floor)` degenerate fallback.
    const CONTAINER_WIDTH_PX: f64 = 1200.0;

    const COLUMN_HEIGHT_PX: f64 = 800.0;

    #[test]
    fn clamp_left_column_width_holds_at_the_floor() {
        assert_eq!(
            clamp_left_column_width(LEFT_COLUMN_FLOOR_PX, CONTAINER_WIDTH_PX),
            LEFT_COLUMN_FLOOR_PX
        );
    }

    #[test]
    fn clamp_left_column_width_clamps_up_from_just_below_the_floor() {
        assert_eq!(
            clamp_left_column_width(LEFT_COLUMN_FLOOR_PX - 1.0, CONTAINER_WIDTH_PX),
            LEFT_COLUMN_FLOOR_PX
        );
    }

    #[test]
    fn clamp_left_column_width_holds_at_the_ceiling() {
        let ceiling =
            CONTAINER_WIDTH_PX - SPLITTER_SIZE_PX - 2.0 * GRID_GAP_PX - MACHINE_COLUMN_FLOOR_PX;
        assert_eq!(
            clamp_left_column_width(ceiling, CONTAINER_WIDTH_PX),
            ceiling
        );
    }

    #[test]
    fn clamp_left_column_width_clamps_down_from_just_above_the_ceiling() {
        // A wider container raises the computed ceiling correspondingly --
        // the opposite of assuming no ceiling is needed at all, which would
        // let the left column overflow the grid instead of yielding to the
        // machine column's floor.
        let ceiling =
            CONTAINER_WIDTH_PX - SPLITTER_SIZE_PX - 2.0 * GRID_GAP_PX - MACHINE_COLUMN_FLOOR_PX;
        assert_eq!(
            clamp_left_column_width(ceiling + 1.0, CONTAINER_WIDTH_PX),
            ceiling
        );
    }

    #[test]
    fn clamp_left_column_width_never_lets_the_ceiling_fall_below_the_floor() {
        // A container too small to satisfy both floors at once must still
        // return a value at least at the floor, not panic or invert the
        // clamp range.
        let tiny_container = LEFT_COLUMN_FLOOR_PX;
        assert_eq!(
            clamp_left_column_width(LEFT_COLUMN_FLOOR_PX, tiny_container),
            LEFT_COLUMN_FLOOR_PX
        );
    }

    #[test]
    fn clamp_output_height_holds_at_the_floor() {
        assert_eq!(
            clamp_output_height(OUTPUT_FLOOR_PX, COLUMN_HEIGHT_PX),
            OUTPUT_FLOOR_PX
        );
    }

    #[test]
    fn clamp_output_height_clamps_up_from_just_below_the_floor() {
        assert_eq!(
            clamp_output_height(OUTPUT_FLOOR_PX - 1.0, COLUMN_HEIGHT_PX),
            OUTPUT_FLOOR_PX
        );
    }

    #[test]
    fn clamp_output_height_holds_at_the_ceiling() {
        let ceiling = COLUMN_HEIGHT_PX - SPLITTER_SIZE_PX - 2.0 * GRID_GAP_PX - EDITOR_MIN_SHARE_PX;
        assert_eq!(clamp_output_height(ceiling, COLUMN_HEIGHT_PX), ceiling);
    }

    #[test]
    fn clamp_output_height_clamps_down_from_just_above_the_ceiling() {
        // A taller column raises the computed ceiling correspondingly.
        let ceiling = COLUMN_HEIGHT_PX - SPLITTER_SIZE_PX - 2.0 * GRID_GAP_PX - EDITOR_MIN_SHARE_PX;
        assert_eq!(
            clamp_output_height(ceiling + 1.0, COLUMN_HEIGHT_PX),
            ceiling
        );
    }

    #[test]
    fn clamp_output_height_never_lets_the_ceiling_fall_below_the_floor() {
        let tiny_column = OUTPUT_FLOOR_PX;
        assert_eq!(
            clamp_output_height(OUTPUT_FLOOR_PX, tiny_column),
            OUTPUT_FLOOR_PX
        );
    }

    #[test]
    fn main_style_is_empty_when_nothing_is_set() {
        assert_eq!(main_style(None, None, false), "");
    }

    #[test]
    fn main_style_sets_only_the_dimension_that_is_some() {
        assert_eq!(
            main_style(Some(300.0), None, false),
            format!("--left-col:min(300px,{LEFT_COLUMN_CEILING_CALC});")
        );
        assert_eq!(
            main_style(None, Some(200.0), false),
            format!("--output-h:min(200px,{OUTPUT_HEIGHT_CEILING_CALC});--output-pane-cap:100%;")
        );
    }

    #[test]
    fn main_style_writes_both_dimensions_together_without_clobbering_either() {
        assert_eq!(
            main_style(Some(300.0), Some(200.0), false),
            format!(
                "--left-col:min(300px,{LEFT_COLUMN_CEILING_CALC});--output-h:min(200px,{OUTPUT_HEIGHT_CEILING_CALC});--output-pane-cap:100%;"
            )
        );
    }

    #[test]
    fn main_style_writes_output_pane_cap_only_when_output_height_is_committed() {
        // `.output-pane`'s max-height reads --output-pane-cap, never
        // --output-h directly (see style.css): a percentage max-height
        // against the row's `auto` (undragged) containing block computes to
        // "none", not 0, so this property must be absent -- not merely
        // unused -- whenever the row itself is still auto-sized.
        assert!(!main_style(Some(300.0), None, false).contains("--output-pane-cap"));
        assert!(main_style(None, Some(200.0), false).contains("--output-pane-cap:100%;"));
    }

    #[test]
    fn main_style_adds_user_select_none_only_while_dragging() {
        assert_eq!(main_style(None, None, true), "user-select:none;");
        assert_eq!(
            main_style(Some(300.0), None, true),
            format!("--left-col:min(300px,{LEFT_COLUMN_CEILING_CALC});user-select:none;")
        );
    }

    #[test]
    fn main_style_left_col_ceiling_never_lets_the_grid_overflow_a_narrower_container() {
        // finding 3: a stale, too-wide `--left-col` (set by a drag at a
        // wider viewport) must not out-run a `min()` ceiling recomputed at
        // the container's current, narrower width -- the exact CSS-only
        // backstop `grid-template-columns` relies on to never overflow
        // regardless of when the window was last dragged.
        let style = main_style(Some(930.0), None, false);
        assert!(style.contains("min(930px,"));
        assert!(style.contains(LEFT_COLUMN_CEILING_CALC));
    }

    #[test]
    fn main_style_output_h_ceiling_never_lets_the_grid_overflow_a_shorter_container() {
        // The vertical twin of the test above: a stale, too-tall
        // `--output-h` (set by a drag at a taller viewport) must not out-run
        // a `min()` ceiling recomputed at the container's current, shorter
        // height.
        let style = main_style(None, Some(445.0), false);
        assert!(style.contains("min(445px,"));
        assert!(style.contains(OUTPUT_HEIGHT_CEILING_CALC));
    }

    #[test]
    fn left_column_ceiling_calc_matches_the_floor_constants_it_names() {
        // `LEFT_COLUMN_CEILING_CALC` is a literal string, not derived from
        // `SPLITTER_SIZE_PX`/`GRID_GAP_PX`/`MACHINE_COLUMN_FLOOR_PX` at
        // compile time -- nothing stops it drifting out of sync with them.
        // Asserting containment of the constant against itself (as the test
        // above does) can't catch that: it passes even if the constant's
        // numbers are wrong, since it only checks the constant was copied
        // verbatim into the style string. This test instead rebuilds the
        // expected calc() from the same floor constants `clamp_left_column_
        // width` uses, so the two can't silently disagree.
        let expected = format!(
            "calc(100% - {}px - {}rem - {}rem)",
            SPLITTER_SIZE_PX as u32,
            (2.0 * GRID_GAP_PX) / 16.0,
            MACHINE_COLUMN_FLOOR_PX / 16.0
        );
        assert_eq!(LEFT_COLUMN_CEILING_CALC, expected);
    }

    #[test]
    fn output_height_ceiling_calc_matches_the_floor_constants_it_names() {
        // 3 gaps, not 2: the row axis has 4 tracks (header, editor,
        // splitter, output) to the column axis's 3, so one more 0.75rem
        // gap separates them.
        let expected = format!(
            "calc(100% - {}px - {}rem - {}rem)",
            SPLITTER_SIZE_PX as u32,
            (3.0 * GRID_GAP_PX) / 16.0,
            EDITOR_MIN_SHARE_PX / 16.0
        );
        assert_eq!(OUTPUT_HEIGHT_CEILING_CALC, expected);
    }

    #[test]
    fn resized_extent_grows_the_column_splitter_pane_as_the_pointer_moves_right() {
        // The column splitter's pane sits left of its handle: a positive
        // delta (pointer moving right) must grow it.
        assert_eq!(resized_extent(400.0, 50.0, Splitter::Column), 450.0);
        assert_eq!(resized_extent(400.0, -50.0, Splitter::Column), 350.0);
    }

    #[test]
    fn resized_extent_shrinks_the_row_splitter_pane_as_the_pointer_moves_down() {
        // The row splitter's output pane sits below its handle: a positive
        // delta (pointer moving down, growing the space above the handle)
        // must shrink it -- the opposite sign from the column splitter
        // (finding 1: this is exactly the sign the earlier implementation
        // got backwards by copying the column case).
        assert_eq!(resized_extent(200.0, 50.0, Splitter::Row), 150.0);
        assert_eq!(resized_extent(200.0, -50.0, Splitter::Row), 250.0);
    }
}
