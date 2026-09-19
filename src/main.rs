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
    keydown_decision, save_shortcut, yield_to_event_loop,
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
const SOURCE_DEBOUNCE_MS: u32 = 400;

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
/// later command's own outcome.
fn advance_chunk_once(
    control: &mut Control,
    view_state: &mut ViewState,
    restart_signal: &mut bool,
) -> (String, bool) {
    let outcome = control.resume_chunk(control::CHUNK_BUDGET);
    view_state.observe(control);
    match outcome {
        StepOutcome::BudgetExhausted => (status_for(outcome).to_string(), true),
        StepOutcome::Halted | StepOutcome::Breakpoint(_) | StepOutcome::Advanced => {
            let status = restarted_status(outcome, *restart_signal);
            *restart_signal = false;
            view_state.record_pause_boundary(control);
            (status, false)
        }
    }
}

/// Continue's and Next's shared "first chunk landed" tail: observe the
/// resulting state, and on every terminal outcome (not `BudgetExhausted`)
/// record the pause boundary too. Takes the outcome already computed, not
/// the call that produced it -- Continue's unconditional first instruction
/// and Next's own statement-then-target-check are the one part that isn't
/// shared. Returns the status text plus whether the caller must schedule
/// another chunk tick.
fn first_chunk_outcome(
    control: &mut Control,
    view_state: &mut ViewState,
    outcome: StepOutcome,
) -> (String, bool) {
    view_state.observe(control);
    match outcome {
        StepOutcome::BudgetExhausted => (status_for(outcome).to_string(), true),
        StepOutcome::Advanced | StepOutcome::Halted | StepOutcome::Breakpoint(_) => {
            view_state.record_pause_boundary(control);
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
fn continue_pressed(control: &mut Control, view_state: &mut ViewState) -> Option<(String, bool)> {
    if control.is_running() || !control.session() {
        return None;
    }
    view_state.clear_changed();
    let outcome = control.continue_chunk(control::CHUNK_BUDGET);
    Some(first_chunk_outcome(control, view_state, outcome))
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
fn install_keyboard_shortcuts(
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

/// `Msg::Interrupt`'s core logic, factored out of `App::update` so it is
/// testable without a live `Context`: ends a chunked Run, Continue, or Next
/// in flight and records the resulting pause boundary. A true no-op
/// otherwise -- `ready`/`paused` have nothing left to interrupt, and
/// recording a boundary with nothing having moved since the last one would
/// diff the current state against itself and silently clear the
/// changed-value highlights that boundary already set. Returns whether
/// anything actually happened.
fn interrupt_if_running(control: &mut Control, view_state: &mut ViewState) -> bool {
    if !control.is_running() {
        return false;
    }
    control.end_in_flight();
    view_state.observe(control);
    view_state.record_pause_boundary(control);
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
    /// kept current by the end of `update`, since the handler itself runs
    /// outside any render and so can't call `control_enablement` against
    /// fresh state directly. Refreshed from `update`, not `rendered`: Yew
    /// 0.23's scheduler (`run_scheduler`'s `can_yield` ignores the rendered
    /// queue while `fill_queue` runs updates first) can starve `rendered` for
    /// the whole span of a chunked Run/Continue/Next, since `ChunkTick`
    /// messages keep the update queue non-empty -- `x` stopped mapping to
    /// Interrupt mid-run before this moved.
    shortcut_enablement: Rc<Cell<ControlEnablement>>,
    /// Kept alive for as long as `App` is, same reason as
    /// `_beforeunload_handler`. Never read directly.
    _keydown_handler: KeydownHandler,
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
        );
        self.status_message = status;
        if needs_tick {
            self.schedule_chunk_tick(ctx);
        }
    }

    /// Whether `window.onbeforeunload` should arm the native leave-this-
    /// page confirmation: there is something the user typed that a silent
    /// reload would lose. `false` for the untouched default program, so a
    /// visitor who loads the page and changes nothing is never nagged.
    fn should_confirm_before_leaving(&self) -> bool {
        self.source != DEFAULT_MMS
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
            status_message: "Loaded".to_string(),
            restart_signal: false,
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
                *self.source_dirty.borrow_mut() = self.should_confirm_before_leaving();
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
                    if let Some((status, needs_tick)) =
                        continue_pressed(&mut self.control, &mut self.view_state)
                    {
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
                    if !self.control.is_running() {
                        let was_halted = self.control.is_halted();
                        let outcome = self.control.step();
                        if !was_halted {
                            self.restart_signal = false;
                            self.view_state.observe(&self.control);
                            self.view_state.record_pause_boundary(&self.control);
                            self.status_message = if outcome == StepOutcome::Halted {
                                "Halted"
                            } else {
                                "Stepped"
                            }
                            .to_string();
                        }
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
                        let (status, needs_tick) =
                            first_chunk_outcome(&mut self.control, &mut self.view_state, outcome);
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
                if interrupt_if_running(&mut self.control, &mut self.view_state) {
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
                    self.status_message = "Reset".to_string();
                }
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
        assert!(interrupt_if_running(&mut control, &mut view_state));

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
            loop {
                let (status, needs_tick) =
                    advance_chunk_once(control, view_state, &mut restart_signal);
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
        // The data half of the fix for "`x` never interrupts a chunked
        // Run/Continue/Next": while a chunk is between ticks
        // (`BudgetExhausted`, still `is_running()`), the enablement the
        // keydown handler reads must already show Interrupt live and every
        // other control disabled -- not whatever `App` had before the run
        // started. The previous bug was `App::rendered` alone refreshing
        // this cell, which Yew 0.23's scheduler can starve for a chunked
        // run's entire span (`run_scheduler`'s `can_yield` ignores the
        // rendered queue while `ChunkTick` messages keep the update queue
        // non-empty) -- `App::rendered` no longer exists at all, and
        // `App::update` refreshes this cell itself, at the end, on every
        // message. That structural half can't be driven host-side without a
        // live `yew::Context` (`AGENTS.md`'s Component-lifecycle exemption);
        // this test pins the state the choke point must compute from.
        let mut control = Control::new(INFINITE_LOOP_MMS, "loop.mms").expect("assembles");
        let mut view_state = ViewState::new();
        view_state.reset(&control);
        let mut restart_signal = false;
        control.start_run();

        let (_, needs_tick) =
            advance_chunk_once(&mut control, &mut view_state, &mut restart_signal);
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
        // `SETL $1,7` also grows `rL` (from `$1 < rG`'s default of 32), so
        // this fixture exercises the specials side of the diff too.
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
        assert!(!interrupt_if_running(&mut control, &mut view_state));
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

        assert!(continue_pressed(&mut control, &mut view_state).is_none());
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

        let (_, needs_tick) =
            continue_pressed(&mut control, &mut view_state).expect("a paused session proceeds");
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
        // `SETL $1,7` also grows `rL` (from `$1 < rG`'s default of 32), so
        // this fixture exercises the specials side of the diff too.
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
