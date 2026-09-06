use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gloo_timers::callback::Timeout;
use log::info;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BeforeUnloadEvent, Element, Event, KeyboardEvent, PointerEvent};
use yew::{Callback, Component, Context, Html, NodeRef, Renderer, html};

mod control;
mod editor;
mod examples;
mod highlight;
mod machine;

use control::{
    Control, ControlBar, ControlEnablement, KeyboardShortcut, StepOutcome, control_enablement,
    keyboard_shortcut_for, yield_to_event_loop,
};
use editor::Editor;
use examples::DEFAULT_MMS;
use machine::{MachinePane, OutputPane, ViewState};

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
const SOURCE_DEBOUNCE_MS: u32 = 500;

/// MIX-only opcodes: mnemonics classic MIX has but MMIXAL doesn't, so their
/// presence as a whole token is a strong signal the pasted source is MIX,
/// not MMIXAL. The register-sign/zero/overflow jump family (`JAN`, `JAZ`,
/// `JAP`, `JANN`, `JANZ`, `JANP`, `JAO`) repeats for registers 1-6 and X;
/// `JNOV` has no per-register form.
const MIX_ONLY_OPCODES: &[&str] = &[
    "ENTA", "ENTX", "ENNA", "ENNX", "CMPA", "CMP1", "CMP2", "CMP3", "CMP4", "CMP5", "CMP6", "INCA",
    "INCX", "DECA", "DECX", "SLAX", "SRAX", "SLC", "SRC", "HLT", "IOC", "JAN", "JAZ", "JAP",
    "JANN", "JANZ", "JANP", "JAO", "J1N", "J1Z", "J1P", "J1NN", "J1NZ", "J1NP", "J1O", "J2N",
    "J2Z", "J2P", "J2NN", "J2NZ", "J2NP", "J2O", "J3N", "J3Z", "J3P", "J3NN", "J3NZ", "J3NP",
    "J3O", "J4N", "J4Z", "J4P", "J4NN", "J4NZ", "J4NP", "J4O", "J5N", "J5Z", "J5P", "J5NN", "J5NZ",
    "J5NP", "J5O", "J6N", "J6Z", "J6P", "J6NN", "J6NZ", "J6NP", "J6O", "JXN", "JXZ", "JXP", "JXNN",
    "JXNZ", "JXNP", "JXO", "JNOV",
];

/// Whether `token` contains a MIX field-spec operand, `(<digits>:<digits>)`
/// -- MMIXAL never uses a parenthesized range. Scoped to one token (not a
/// raw substring search over the whole line) so a match can't span into an
/// unrelated token, e.g. a comment or string literal.
fn token_has_mix_field_spec(token: &str) -> bool {
    let mut rest = token;
    while let Some(open) = rest.find('(') {
        let after_open = &rest[open + 1..];
        match after_open.find(')') {
            Some(close) => {
                let inner = &after_open[..close];
                if let Some((left, right)) = inner.split_once(':') {
                    let is_digits =
                        |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
                    if is_digits(left) && is_digits(right) {
                        return true;
                    }
                }
                rest = &after_open[close + 1..];
            }
            None => return false,
        }
    }
    false
}

/// A heuristic classifier for classic MIX source (Knuth's original
/// architecture, distinct from MMIX): checked only after MMIXAL parsing has
/// already failed, so it never changes what parses -- it only improves the
/// message when parsing was always going to fail. Matches whitespace-
/// delimited tokens, never a raw substring search, so an MMIXAL comment or
/// string literal containing e.g. "ORIG" or "CMPA" can't false-positive.
fn looks_like_mix(source: &str) -> bool {
    source.lines().any(|line| {
        line.split_whitespace().any(|token| {
            token == "ORIG" || MIX_ONLY_OPCODES.contains(&token) || token_has_mix_field_spec(token)
        })
    })
}

/// The complete message to display for a load/reload error from
/// user-supplied source: the bare MIX sentence if `source` looks like MIX
/// (see `looks_like_mix`), or the normal "Assembly error: ..." otherwise --
/// decided once, here, so `view()` can render `self.error` verbatim with no
/// formatting of its own. Either way, every literal `"{SOURCE_FILENAME}:"`
/// is stripped from `error` first (see `strip_source_filename`) -- the
/// phantom filename is checksmix's own error text, not something the user
/// ever named.
fn describe_source_error(source: &str, error: &str) -> String {
    if looks_like_mix(source) {
        "This looks like MIX, not MMIX.".to_string()
    } else {
        format!("Assembly error: {}", strip_source_filename(error))
    }
}

/// Every literal occurrence of `"{SOURCE_FILENAME}:"` removed from `error` --
/// not just a leading prefix, since the symbol-redefinition shape embeds a
/// *second* `{SOURCE_FILENAME}:{line}` reference inside the message body
/// (`"... redefined (first defined at source.mms:2)"`), which a
/// prefix-only strip would leave untouched.
fn strip_source_filename(error: &str) -> String {
    error.replace(&format!("{SOURCE_FILENAME}:"), "")
}

/// Parses checksmix's raw error text for a `(line, column)` source
/// location, defensively -- checksmix's error shapes are not uniform:
///
/// - The common `pest`-parser syntax error:
///   `"{SOURCE_FILENAME}:{line}:{col}: {message}"` -- both a line and a
///   column.
/// - A symbol-redefinition error: `"{SOURCE_FILENAME}:{line}: symbol
///   '{name}' redefined (first defined at {SOURCE_FILENAME}:{line})"` --
///   a line only, no column, and a second, embedded
///   `{SOURCE_FILENAME}:{line}` reference later in the message that must
///   not be mistaken for this one.
/// - Everything else (e.g. `"Invalid opcode: {value}"`) -- no location at
///   all.
///
/// Tries the line-and-column prefix first, then the line-only prefix,
/// returning `None` if neither matches -- there is genuinely nothing to
/// point at. Only ever looks at a leading `"{SOURCE_FILENAME}:"` prefix, so
/// the redefinition shape's second, embedded reference is never mistaken
/// for the primary location.
fn parse_error_location(error: &str) -> Option<(usize, Option<usize>)> {
    let rest = error.strip_prefix(&format!("{SOURCE_FILENAME}:"))?;
    let (line_str, after_line) = rest.split_once(':')?;

    if let Some((col_str, after_col)) = after_line.split_once(':')
        && after_col.starts_with(' ')
        && let (Ok(line), Ok(col)) = (line_str.parse(), col_str.parse())
    {
        return Some((line, Some(col)));
    }
    if after_line.starts_with(' ')
        && let Ok(line) = line_str.parse()
    {
        return Some((line, None));
    }
    None
}

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
struct DragState {
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
fn main_style(
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
struct CommittedSizes {
    left_column_width: Option<f64>,
    output_height: Option<f64>,
}

/// Build the column splitter's `pointerdown`/`pointermove`/`pointerup`/
/// `pointercancel` callbacks. `left_col_ref` is any element whose rendered
/// width equals the left column's current width -- `App::row_splitter_ref`
/// works, since the row splitter's handle lives in that same column.
/// `committed` is `App`'s own current values for both dimensions -- see
/// `CommittedSizes`'s doc comment.
fn column_splitter_handlers(
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
fn row_splitter_handlers(
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

/// The status readout's text for a chunked Run or Step Over's outcome --
/// shared because both drive the same chunk-yield loop
/// (`Control::continue_chunk`) and report through the same four
/// `StepOutcome` variants. `run_chunk` never returns `Advanced` (a chunk
/// that neither halts nor hits a breakpoint always exhausts its budget), so
/// "Stepped over call" only ever surfaces from a chunked Step Over.
fn status_for(outcome: StepOutcome) -> &'static str {
    match outcome {
        StepOutcome::BudgetExhausted => "Running",
        StepOutcome::Advanced => "Stepped over call",
        StepOutcome::Halted => "Halted",
        StepOutcome::Breakpoint(_) => "Hit breakpoint",
    }
}

/// The JS closure backing `window.onbeforeunload` -- must stay alive for as
/// long as the handler should stay registered; dropping it frees the JS
/// function `onbeforeunload` points at.
type BeforeUnloadHandler = Closure<dyn FnMut(Event)>;

/// Registers `window.onbeforeunload`: when `dirty` (shared with `App`) is
/// set at unload time, calls `Event::prevent_default` and sets
/// `BeforeUnloadEvent`'s `returnValue` -- the modern and legacy triggers,
/// respectively, for a browser's native "leave this page? changes may not
/// be saved" prompt, since browsers vary in which one they still honor.
/// Returns the shared flag alongside the `Closure` backing the handler; the
/// caller must keep it alive (see [`BeforeUnloadHandler`]).
fn install_beforeunload_handler() -> (Rc<RefCell<bool>>, BeforeUnloadHandler) {
    let dirty = Rc::new(RefCell::new(false));
    let dirty_for_handler = dirty.clone();
    let handler = Closure::wrap(Box::new(move |event: Event| {
        if !*dirty_for_handler.borrow() {
            return;
        }
        event.prevent_default();
        if let Ok(event) = event.dyn_into::<BeforeUnloadEvent>() {
            event.set_return_value("Changes you made may not be saved.");
        }
    }) as Box<dyn FnMut(Event)>);

    if let Some(window) = web_sys::window() {
        window.set_onbeforeunload(Some(handler.as_ref().unchecked_ref()));
    }

    (dirty, handler)
}

/// The JS closure backing `window.onkeydown` -- must stay alive for as long
/// as the handler should stay registered, same as [`BeforeUnloadHandler`].
type KeydownHandler = Closure<dyn FnMut(KeyboardEvent)>;

/// Registers `window.onkeydown`: on a bare keypress (no Ctrl/Cmd/Alt) outside
/// a text-entry element, dispatches the `Msg` `keyboard_shortcut_for` maps
/// the key to, gated by the returned cell's current `ControlEnablement` --
/// kept live by `App::rendered`, not recomputed here. Seeded with the ready
/// state (`control_enablement(false, false, false)`), matching a
/// freshly-constructed `Control`. Returns the shared cell alongside the
/// `Closure` backing the handler; the caller must keep the latter alive (see
/// [`KeydownHandler`]).
fn install_keyboard_shortcuts(
    link: yew::html::Scope<App>,
) -> (Rc<Cell<ControlEnablement>>, KeydownHandler) {
    let enablement = Rc::new(Cell::new(control_enablement(false, false, false)));
    let enablement_for_handler = enablement.clone();
    let handler = Closure::wrap(Box::new(move |event: KeyboardEvent| {
        let modifier_held = event.ctrl_key() || event.meta_key() || event.alt_key();
        let focused_element_is_text_input = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
            .map(|element| matches!(element.tag_name().as_str(), "TEXTAREA" | "INPUT"))
            .unwrap_or(false);
        let shortcut = keyboard_shortcut_for(
            &event.key(),
            modifier_held,
            focused_element_is_text_input,
            enablement_for_handler.get(),
        );
        if let Some(shortcut) = shortcut {
            event.prevent_default();
            let msg = match shortcut {
                KeyboardShortcut::StepOver => Msg::StepOver,
                KeyboardShortcut::Step => Msg::Step,
                KeyboardShortcut::Run => Msg::Run,
                KeyboardShortcut::Stop => Msg::Stop,
            };
            link.send_message(msg);
        }
    }) as Box<dyn FnMut(KeyboardEvent)>);

    if let Some(window) = web_sys::window() {
        window.set_onkeydown(Some(handler.as_ref().unchecked_ref()));
    }

    (enablement, handler)
}

/// `Msg::Stop`'s core logic, factored out of `App::update` so it is
/// testable without a live `Context`: interrupts a chunked Run or Step
/// Over in flight and records the resulting pause boundary. A true no-op
/// otherwise -- `ready`/`paused` have nothing left to interrupt, and
/// recording a boundary with nothing having moved since the last one would
/// diff the current state against itself and silently clear the
/// changed-value highlights that boundary already set. Returns whether
/// anything actually happened.
fn stop_if_running(control: &mut Control, view_state: &mut ViewState) -> bool {
    if !control.is_running() {
        return false;
    }
    control.stop();
    view_state.observe(control);
    view_state.record_pause_boundary(control);
    true
}

pub enum Msg {
    SourceChanged(String),
    /// Fires once `SOURCE_DEBOUNCE_MS` has passed with no further
    /// `SourceChanged` -- re-assembles `self.source` and updates
    /// `self.error` accordingly. Carries no payload: `self.source` is
    /// already current by the time this arrives.
    ReassembleSource,
    ToggleBreakpoint(usize),
    Run,
    Step,
    StepOver,
    Stop,
    Reset,
    /// One chunk boundary: reschedule if the run isn't finished, or if a
    /// `Stop` landed while this tick was scheduled, do nothing.
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
    /// The pending chunk-tick timeout, if a chunked Run or Step Over is in
    /// flight. Held rather than `.forget()`-ten so Stop, or a `reload` mid-
    /// run, can cancel it by dropping this (runs `clearTimeout` and frees
    /// the closure) instead of leaking one allocation per chunk boundary.
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
    /// already shown prominently elsewhere.
    status_message: &'static str,
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
    /// currently arms the native confirmation dialog -- `self.source !=
    /// DEFAULT_MMS`, re-evaluated at the same point `self.source` itself
    /// changes. Shared with that handler rather than read from `self`
    /// directly: the handler is a `'static` JS closure, registered once at
    /// `create` and outliving any single `view()`/`update()` call.
    source_dirty: Rc<RefCell<bool>>,
    /// Kept alive for as long as `App` is -- dropping a `Closure` frees the
    /// JS function it backs, which would leave `window.onbeforeunload`
    /// pointing at freed memory. Never read directly; `source_dirty` is the
    /// live channel to it.
    _beforeunload_handler: BeforeUnloadHandler,
    /// Live enablement `window.onkeydown`'s handler reads on every keydown --
    /// kept current by `rendered`, since the handler itself runs outside any
    /// render and so can't call `control_enablement` against fresh state
    /// directly.
    shortcut_enablement: Rc<Cell<ControlEnablement>>,
    /// Kept alive for as long as `App` is, same reason as
    /// `_beforeunload_handler`. Never read directly.
    _keydown_handler: KeydownHandler,
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
        self.view_state.observe(&self.control);
        self.status_message = status_for(outcome);
        match outcome {
            StepOutcome::BudgetExhausted => self.schedule_chunk_tick(ctx),
            StepOutcome::Halted | StepOutcome::Breakpoint(_) | StepOutcome::Advanced => {
                self.view_state.record_pause_boundary(&self.control);
            }
        }
    }

    /// Whether `window.onbeforeunload` should arm the native leave-this-
    /// page confirmation: there is something the user typed that a silent
    /// reload would lose. `false` for the untouched default program, so a
    /// visitor who loads the page and changes nothing is never nagged.
    fn should_confirm_before_leaving(&self) -> bool {
        self.source != DEFAULT_MMS
    }

    /// Reload the current source (Reset's and halted-Run's shared "play
    /// again" step): cancel any pending chunk tick and any pending
    /// debounced re-assemble (this reload supersedes both), re-run
    /// `Control::reload`, and on success reseed the continuity/snapshot
    /// state. On a parse error, `reload` already leaves the previous
    /// machine and everything else untouched, so only `self.error` moves.
    fn reload_source(&mut self) {
        self.chunk_timeout = None;
        self.debounce_timeout = None;
        match self.control.reload(&self.source) {
            Ok(()) => {
                self.error = None;
                self.error_line = None;
                self.view_state.reset(&self.control);
            }
            Err(error) => {
                self.error_line = parse_error_location(&error).map(|(line, _)| line);
                self.error = Some(describe_source_error(&self.source, &error));
            }
        }
    }

    /// Flush a debounced re-assemble that hasn't fired yet, so Run, Step,
    /// and Step Over can never execute a program older than `self.source`
    /// -- `SOURCE_DEBOUNCE_MS` otherwise leaves up to half a second where
    /// every control still reads enabled against the stale prior program.
    /// Returns whether the caller may proceed: `false` once the flush
    /// surfaces a parse error, since the edit that is pending is not a
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
        let control = Control::new(DEFAULT_MMS, SOURCE_FILENAME)
            .expect("DEFAULT_MMS assembles; pinned by examples::tests::default_mms_assembles");
        let (source_dirty, beforeunload_handler) = install_beforeunload_handler();
        let (shortcut_enablement, keydown_handler) =
            install_keyboard_shortcuts(_ctx.link().clone());
        let mut app = Self {
            source: DEFAULT_MMS.to_string(),
            control,
            error: None,
            error_line: None,
            chunk_timeout: None,
            debounce_timeout: None,
            view_state: ViewState::new(),
            status_message: "Loaded",
            left_column_width: None,
            output_height: None,
            main_ref: NodeRef::default(),
            col_splitter_ref: NodeRef::default(),
            row_splitter_ref: NodeRef::default(),
            output_pane_ref: NodeRef::default(),
            drag_state: Rc::new(RefCell::new(None)),
            source_dirty,
            _beforeunload_handler: beforeunload_handler,
            shortcut_enablement,
            _keydown_handler: keydown_handler,
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
        match msg {
            Msg::SourceChanged(source) => {
                // Re-assembling (and showing a resulting parse error) is
                // debounced to `Msg::ReassembleSource` -- see
                // `SOURCE_DEBOUNCE_MS` -- so typing an in-progress line
                // doesn't flash "Assembly error" on every keystroke. A run
                // or chunked Step Over in flight still stops immediately,
                // same as before: the source shown alongside it is already
                // no longer the one that produced it.
                self.source = source;
                *self.source_dirty.borrow_mut() = self.should_confirm_before_leaving();
                self.control.stop();
                self.chunk_timeout = None;
                let link = ctx.link().clone();
                self.debounce_timeout = Some(Timeout::new(SOURCE_DEBOUNCE_MS, move || {
                    link.send_message(Msg::ReassembleSource)
                }));
                true
            }
            Msg::ReassembleSource => {
                self.debounce_timeout = None;
                match self.control.reload(&self.source) {
                    Ok(()) => {
                        self.error = None;
                        self.error_line = None;
                        self.view_state.reset(&self.control);
                        self.status_message = "Loaded";
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
                };
                true
            }
            Msg::Run => {
                // A pending debounce means `self.control` still holds a
                // program older than `self.source`; flush it rather than
                // run the stale one.
                if !self.flush_pending_reassemble() {
                    return true;
                }
                if !self.control.is_running() {
                    // Run while halted is Reset then Run -- the "play again"
                    // affordance, one click to replay from the top.
                    if self.control.is_halted() {
                        self.reload_source();
                        if self.error.is_some() {
                            return true;
                        }
                    }
                    self.view_state.clear_changed();
                    self.control.start_run();
                    self.view_state.observe(&self.control);
                    if self.control.is_running() {
                        self.schedule_chunk_tick(ctx);
                    }
                    // Fresh or replay-from-halt both read the same: no
                    // "Restarted" status distinct from plain Running -- a
                    // Msg::Run while halted reloads and then schedules a
                    // chunk tick exactly like a fresh run, and that first
                    // tick's BudgetExhausted branch would overwrite anything
                    // more specific one event-loop turn later anyway.
                    self.status_message = "Running";
                }
                true
            }
            Msg::Step => {
                if !self.flush_pending_reassemble() {
                    return true;
                }
                if !self.control.is_running() {
                    let was_halted = self.control.is_halted();
                    let outcome = self.control.step();
                    if !was_halted {
                        self.view_state.observe(&self.control);
                        self.view_state.record_pause_boundary(&self.control);
                        self.status_message = if outcome == StepOutcome::Halted {
                            "Halted"
                        } else {
                            "Stepped"
                        };
                    }
                }
                true
            }
            Msg::StepOver => {
                if !self.flush_pending_reassemble() {
                    return true;
                }
                if !self.control.is_running() {
                    self.view_state.clear_changed();
                    let outcome = self.control.step_over_chunk(control::CHUNK_BUDGET);
                    self.status_message = status_for(outcome);
                    match outcome {
                        StepOutcome::BudgetExhausted => {
                            self.view_state.observe(&self.control);
                            self.schedule_chunk_tick(ctx);
                        }
                        StepOutcome::Advanced
                        | StepOutcome::Halted
                        | StepOutcome::Breakpoint(_) => {
                            self.view_state.observe(&self.control);
                            self.view_state.record_pause_boundary(&self.control);
                        }
                    }
                }
                true
            }
            Msg::Stop => {
                if stop_if_running(&mut self.control, &mut self.view_state) {
                    self.chunk_timeout = None;
                    self.status_message = "Stopped";
                    true
                } else {
                    false
                }
            }
            Msg::Reset => {
                self.reload_source();
                if self.error.is_none() {
                    self.status_message = "Reset";
                }
                true
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
        }
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
        let on_step = ctx.link().callback(|()| Msg::Step);
        let on_step_over = ctx.link().callback(|()| Msg::StepOver);
        let on_stop = ctx.link().callback(|()| Msg::Stop);
        let on_reset = ctx.link().callback(|()| Msg::Reset);

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
                    <ControlBar
                        running={self.control.is_running()}
                        halted={self.control.is_halted()}
                        has_advanced={self.control.has_advanced()}
                        has_error={self.error.is_some()}
                        {on_run}
                        {on_step}
                        {on_step_over}
                        {on_stop}
                        {on_reset}
                        status={self.status_message.to_string()}
                    />
                </div>
                <Editor
                    source={self.source.clone()}
                    {on_change}
                    breakpoints={self.control.breakpoint_lines().clone()}
                    {current_line}
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

    /// Keeps `shortcut_enablement` current for `window.onkeydown`'s handler
    /// -- one choke point, guaranteed to run after every render, rather than
    /// a write in each of `update`'s several state-changing arms. Recomputes
    /// unconditionally, `first_render` included: there is no cheaper correct
    /// state to seed with than the real one.
    fn rendered(&mut self, _ctx: &Context<Self>, _first_render: bool) {
        self.shortcut_enablement.set(control_enablement(
            self.control.is_running(),
            self.control.is_halted(),
            self.error.is_some(),
        ));
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
    fn looks_like_mix_detects_orig_on_its_own() {
        // No MIX-only opcode here -- isolates the ORIG signal so a mutant
        // that deletes it (leaving only the opcode-table check) goes red,
        // instead of surviving behind the opcode signal in a combined test.
        let mix_source = "\tORIG\t3000\nSTART\tLDA\t0\n";
        assert!(looks_like_mix(mix_source));
    }

    #[test]
    fn looks_like_mix_detects_a_mix_only_opcode_on_its_own() {
        // No ORIG here -- isolates the opcode-table signal so a mutant that
        // deletes MIX_ONLY_OPCODES entirely goes red on its own.
        let mix_source = "START\tENTA\t0\n\tCMPA\t2000,1\n";
        assert!(looks_like_mix(mix_source));
    }

    #[test]
    fn looks_like_mix_detects_a_field_spec_operand() {
        let mix_source = "\tLDA\t6,X(1:3)\n";
        assert!(looks_like_mix(mix_source));
    }

    #[test]
    fn looks_like_mix_is_false_for_ordinary_mmixal() {
        assert!(!looks_like_mix(DEFAULT_MMS));
    }

    #[test]
    fn parse_error_location_recovers_line_and_column_from_the_common_shape() {
        let error = "source.mms:3:13: syntax error: expected one of: ...";
        assert_eq!(parse_error_location(error), Some((3, Some(13))));
    }

    #[test]
    fn parse_error_location_recovers_line_only_from_the_redefinition_shape() {
        // No column in this shape, and a *second* `filename:line` reference
        // embedded later in the message -- must not be mistaken for the
        // primary location.
        let error = "source.mms:5: symbol 'Foo' redefined (first defined at source.mms:2)";
        assert_eq!(parse_error_location(error), Some((5, None)));
    }

    #[test]
    fn parse_error_location_is_none_for_a_location_less_message() {
        let error = "Invalid opcode: 0x1a";
        assert_eq!(parse_error_location(error), None);
    }

    #[test]
    fn parse_error_location_recovers_a_location_for_real_mix_looking_source() {
        // checksmix's *raw* error for MIX-shaped input, not the friendly
        // "This looks like MIX" display string the parser never sees --
        // `looks_like_mix` only changes what text the user reads, not what
        // checksmix actually returned, so a location genuinely exists here
        // too.
        let mix_source = "\tORIG\t3000\nSTART\tLDA\t0\n";
        assert!(looks_like_mix(mix_source), "fixture must look like MIX");
        let error = match Control::new(mix_source, SOURCE_FILENAME) {
            Ok(_) => panic!("MIX-shaped source must fail MMIXAL assembly"),
            Err(error) => error,
        };
        assert!(
            parse_error_location(&error).is_some(),
            "a real checksmix parse failure must still yield a location: {error:?}"
        );
    }

    #[test]
    fn strip_source_filename_removes_every_occurrence() {
        let error = "source.mms:5: symbol 'Foo' redefined (first defined at source.mms:2)";
        let stripped = strip_source_filename(error);
        assert!(!stripped.contains(SOURCE_FILENAME));
    }

    #[test]
    fn describe_source_error_strips_the_phantom_filename() {
        let error = "source.mms:3:13: syntax error: ...";
        let message = describe_source_error("\tADDD\t$1,$2,$3\n", error);
        assert!(!message.contains(SOURCE_FILENAME), "{message}");
    }

    #[test]
    fn looks_like_mix_never_substring_matches_across_tokens() {
        // "ORIG" and "CMPA" as substrings of unrelated, larger tokens (not
        // whitespace-delimited on their own) must not false-positive.
        let source = "; a comment mentioning PREORIGAMI and DECMPACT\n";
        assert!(!looks_like_mix(source));
    }

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
    fn status_for_covers_every_step_outcome() {
        assert_eq!(status_for(StepOutcome::BudgetExhausted), "Running");
        assert_eq!(status_for(StepOutcome::Advanced), "Stepped over call");
        assert_eq!(status_for(StepOutcome::Halted), "Halted");
        assert_eq!(status_for(StepOutcome::Breakpoint(0x100)), "Hit breakpoint");
    }

    #[test]
    fn stop_if_running_is_a_true_no_op_while_paused() {
        const WRITES_REGISTER_MMS: &str = "\tLOC\t#100\nMain\tSETL\t$1,7\n\tTRAP\t0,Halt,0\n";
        let mut control = Control::new(WRITES_REGISTER_MMS, "stop.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);

        // An explicit Step, not a chunked Run/Step Over: `is_running()`
        // stays false throughout, exactly the `paused` state Stop is newly
        // enabled in.
        assert_eq!(control.step(), StepOutcome::Advanced);
        assert!(!control.is_running(), "a plain Step never sets running");
        view_state.observe(&control);
        view_state.record_pause_boundary(&control);
        let changed_after_step = view_state.changed_registers().clone();
        assert!(
            changed_after_step.contains(&1),
            "the step must flag $1 as changed: {changed_after_step:?}"
        );
        // `SETL $1,7` also grows `rL` (from `$1 < rG`'s default of 32), so
        // this fixture exercises the specials side of the diff too.
        let changed_specials_after_step = view_state.changed_specials().clone();
        assert!(
            changed_specials_after_step.contains("rL"),
            "the step must flag rL as changed: {changed_specials_after_step:?}"
        );

        // Stop, while paused with nothing having moved since that boundary,
        // must leave the changed set exactly as it was -- reverting the
        // `is_running()` guard would re-diff the current state against
        // itself (the snapshot `record_pause_boundary` just advanced to)
        // and silently clear it to empty.
        assert!(!stop_if_running(&mut control, &mut view_state));
        assert_eq!(
            view_state.changed_registers(),
            &changed_after_step,
            "an inert Stop must not touch the changed-registers set"
        );
        assert_eq!(
            view_state.changed_specials(),
            &changed_specials_after_step,
            "an inert Stop must not touch the changed-specials set"
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
