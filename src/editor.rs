//! Editable source pane: a transparent `<textarea>` layered over a
//! highlight-colored `<pre>` overlay, with a line-number gutter to the left.
//! `white-space: pre` and no wrap everywhere (`style.css`) keep line N of
//! the source mapped to row N of both the overlay and the gutter.
//!
//! The overlay and gutter never scroll themselves: `.overlay` and `.gutter`
//! are fixed-size `overflow: hidden` viewports, and `.overlay-content`/
//! `.gutter-content` -- sized to their full, unclipped content -- carry a
//! `transform: translate(...)` that `EditorMsg::Scroll` updates on every
//! native `scroll` event on the textarea. Translating the content instead
//! of scrolling the viewport (or translating the viewport itself, which
//! would drag its own clip box along with it) is what keeps the two layers
//! from visibly desyncing under fast or inertial scrolling.

use std::collections::BTreeSet;

use web_sys::{Element, HtmlElement, HtmlTextAreaElement};
use yew::prelude::*;

use crate::highlight;

#[derive(Properties, PartialEq)]
pub struct EditorProps {
    pub source: String,
    pub on_change: Callback<String>,
    /// Line numbers with a breakpoint set, for the gutter markers.
    pub breakpoints: BTreeSet<usize>,
    /// The line the paused machine's PC maps to, if any. `None` while
    /// running, since nothing should visibly track a moving PC mid-chunk.
    pub current_line: Option<usize>,
    pub on_toggle_breakpoint: Callback<usize>,
}

pub struct Editor {
    textarea_ref: NodeRef,
    overlay_content_ref: NodeRef,
    gutter_content_ref: NodeRef,
}

pub enum EditorMsg {
    /// The textarea's value changed (typing, paste, cut, ...).
    Input,
    /// Tab was pressed: a literal tab was already spliced into the
    /// textarea's DOM value; report the new value upward.
    TabInserted,
    /// The textarea scrolled; transform the overlay and gutter content to
    /// match (see the module doc comment).
    Scroll,
}

impl Component for Editor {
    type Message = EditorMsg;
    type Properties = EditorProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            textarea_ref: NodeRef::default(),
            overlay_content_ref: NodeRef::default(),
            gutter_content_ref: NodeRef::default(),
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            EditorMsg::Input | EditorMsg::TabInserted => {
                if let Some(textarea) = self.textarea_ref.cast::<HtmlTextAreaElement>() {
                    ctx.props().on_change.emit(textarea.value());
                }
                false
            }
            EditorMsg::Scroll => {
                let Some(textarea) = self.textarea_ref.cast::<HtmlElement>() else {
                    return false;
                };
                let scroll_top = textarea.scroll_top();
                let scroll_left = textarea.scroll_left();
                if let Some(overlay_content) = self.overlay_content_ref.cast::<Element>() {
                    let style = format!(
                        "transform: translate({}px, {}px)",
                        -scroll_left, -scroll_top
                    );
                    let _ = overlay_content.set_attribute("style", &style);
                }
                if let Some(gutter_content) = self.gutter_content_ref.cast::<Element>() {
                    let style = format!("transform: translateY({}px)", -scroll_top);
                    let _ = gutter_content.set_attribute("style", &style);
                }
                false
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let source = ctx.props().source.clone();
        // split, not lines(): a trailing '\n' means the cursor sits on a
        // new, empty final line, which still needs a gutter row.
        let lines: Vec<&str> = source.split('\n').collect();

        let breakpoints = ctx.props().breakpoints.clone();
        let current_line = ctx.props().current_line;
        let on_toggle_breakpoint = ctx.props().on_toggle_breakpoint.clone();

        let gutter_rows: Html = lines
            .iter()
            .enumerate()
            .map(|(i, _)| {
                render_gutter_row(i + 1, &breakpoints, current_line, &on_toggle_breakpoint)
            })
            .collect();

        let overlay_rows: Html = lines
            .iter()
            .enumerate()
            .map(|(i, line)| render_line(line, overlay_row_is_current(i, current_line)))
            .collect();

        let oninput = ctx.link().callback(|_: InputEvent| EditorMsg::Input);
        let onscroll = ctx.link().callback(|_: Event| EditorMsg::Scroll);
        let onkeydown = {
            let textarea_ref = self.textarea_ref.clone();
            ctx.link().batch_callback(move |event: KeyboardEvent| {
                if event.key() != "Tab" {
                    return None;
                }
                event.prevent_default();
                let textarea = textarea_ref.cast::<HtmlTextAreaElement>()?;
                splice_tab(&textarea);
                Some(EditorMsg::TabInserted)
            })
        };

        html! {
            <div class="editor">
                <div class="gutter">
                    <div class="gutter-content" ref={self.gutter_content_ref.clone()}>
                        { gutter_rows }
                    </div>
                </div>
                <div class="editor-surface">
                    <div class="overlay">
                        <pre class="overlay-content" ref={self.overlay_content_ref.clone()}>
                            { overlay_rows }
                        </pre>
                    </div>
                    <textarea
                        class="source-input"
                        ref={self.textarea_ref.clone()}
                        value={source}
                        spellcheck="false"
                        autocapitalize="off"
                        autocomplete="off"
                        {oninput}
                        {onscroll}
                        {onkeydown}
                    />
                </div>
            </div>
        }
    }
}

/// Render one gutter row: the line number, styled for a breakpoint and/or
/// the paused machine's current line, and clickable to toggle a breakpoint.
fn render_gutter_row(
    line: usize,
    breakpoints: &BTreeSet<usize>,
    current_line: Option<usize>,
    on_toggle_breakpoint: &Callback<usize>,
) -> Html {
    let mut class = classes!("gutter-line");
    if breakpoints.contains(&line) {
        class.push("gutter-breakpoint");
    }
    if current_line == Some(line) {
        class.push("gutter-current");
    }
    let on_toggle_breakpoint = on_toggle_breakpoint.clone();
    let onclick = Callback::from(move |_: MouseEvent| on_toggle_breakpoint.emit(line));
    html! { <span {class} {onclick}>{ line }</span> }
}

/// Splice a literal tab into `textarea` at the caret, replacing any current
/// selection, and place the caret immediately after it. Setting `.value()`
/// this way fires no `input` event, so the caller must re-run source-change
/// handling itself.
fn splice_tab(textarea: &HtmlTextAreaElement) {
    let value = textarea.value();
    let start = textarea.selection_start().ok().flatten().unwrap_or(0) as usize;
    let end = textarea.selection_end().ok().flatten().unwrap_or(0) as usize;

    let spliced = splice_at_utf16_offsets(&value, start, end, "\t");
    textarea.set_value(&spliced);

    let caret = (start + 1) as u32;
    let _ = textarea.set_selection_range(caret, caret);
}

/// Convert a UTF-16 code-unit offset — what `selectionStart`/`selectionEnd`
/// report — into a UTF-8 byte offset into `value`. An offset at or past the
/// end of `value` clamps to `value.len()`.
fn utf16_offset_to_byte(value: &str, utf16_offset: usize) -> usize {
    let mut utf16_count = 0;
    for (byte_offset, ch) in value.char_indices() {
        if utf16_count >= utf16_offset {
            return byte_offset;
        }
        utf16_count += ch.len_utf16();
    }
    value.len()
}

/// Splice `insert` into `value` in place of the `[start_utf16, end_utf16)`
/// range, given as UTF-16 code-unit offsets (a textarea's selection
/// endpoints). Plain function, no browser types: `value.len()` (bytes) and
/// selection offsets (UTF-16 code units) disagree whenever `value` holds a
/// character outside the ASCII range, so indexing the raw offsets straight
/// into a UTF-8 `String` can land off a char boundary and panic; converting
/// through `utf16_offset_to_byte` first keeps this host-testable.
fn splice_at_utf16_offsets(
    value: &str,
    start_utf16: usize,
    end_utf16: usize,
    insert: &str,
) -> String {
    let start = utf16_offset_to_byte(value, start_utf16);
    let end = utf16_offset_to_byte(value, end_utf16);

    let mut spliced = String::with_capacity(value.len() + insert.len());
    spliced.push_str(&value[..start]);
    spliced.push_str(insert);
    spliced.push_str(&value[end..]);
    spliced
}

/// One piece of a tokenized line: either literal text between (or after)
/// `highlight::classify` spans, or the text of a span itself.
enum LinePiece<'a> {
    Plain(&'a str),
    Styled(&'a str, highlight::TokenKind),
}

impl<'a> LinePiece<'a> {
    fn text(&self) -> &'a str {
        match *self {
            LinePiece::Plain(text) | LinePiece::Styled(text, _) => text,
        }
    }
}

/// Split `line` into `LinePiece`s covering it end to end with no gaps and no
/// overlaps: `highlight::classify`'s spans as `Styled`, and everything
/// between them as `Plain`. Plain function, no `Html`, so the
/// reconstruction invariant this depends on — concatenating every piece's
/// text reproduces `line` exactly — is host-testable; `classify` emitting an
/// out-of-order or overlapping span is a bug in `classify`, not here.
fn line_pieces(line: &str) -> Vec<LinePiece<'_>> {
    let spans = highlight::classify(line);
    let mut pieces = Vec::new();
    let mut cursor = 0;

    for span in &spans {
        if span.start > cursor {
            pieces.push(LinePiece::Plain(&line[cursor..span.start]));
        }
        pieces.push(LinePiece::Styled(&line[span.start..span.end], span.kind));
        cursor = span.end;
    }
    if cursor < line.len() {
        pieces.push(LinePiece::Plain(&line[cursor..]));
    }

    pieces
}

/// Whether the overlay's zero-based row `i` (source line `i + 1`) is the
/// paused machine's current line. Factored out of `view()` so the row-index-
/// to-line-number mapping is testable on its own, separately from
/// `overlay_line_class`'s CSS-class logic.
fn overlay_row_is_current(i: usize, current_line: Option<usize>) -> bool {
    current_line == Some(i + 1)
}

/// `.overlay-line`, plus `.overlay-current` when this line carries the
/// paused machine's current line -- a full-width background band, additive
/// alongside the gutter's own `gutter-current` marker (defect 4 is
/// specifically that the gutter-only marker is too easy to miss).
fn overlay_line_class(is_current: bool) -> Classes {
    let mut class = classes!("overlay-line");
    if is_current {
        class.push("overlay-current");
    }
    class
}

/// Render one source line as the overlay's colored spans. Untagged gaps
/// between spans render in the overlay's default text color.
fn render_line(line: &str, is_current: bool) -> Html {
    let children: Vec<Html> = line_pieces(line)
        .into_iter()
        .map(|piece| {
            let class = match piece {
                LinePiece::Plain(_) => None,
                LinePiece::Styled(_, kind) => Some(kind.css_class()),
            };
            html! { <span {class}>{ piece.text() }</span> }
        })
        .collect();

    html! { <span class={overlay_line_class(is_current)}>{ for children }</span> }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_at_utf16_offsets_handles_multibyte_prefix() {
        // "café": 'é' is 2 UTF-8 bytes but 1 UTF-16 code unit, so a caret
        // reported at UTF-16 offset 4 (just after 'é') lands on UTF-8 byte
        // offset 4, which is mid-character. Indexing that byte offset
        // straight into the `String` (the original bug) panics; converting
        // through `utf16_offset_to_byte` first must not.
        let value = "café";
        let spliced = splice_at_utf16_offsets(value, 4, 4, "\t");
        assert_eq!(spliced, "café\t");
    }

    #[test]
    fn splice_at_utf16_offsets_replaces_a_selection_after_multibyte_text() {
        let value = "café bar";
        // "bar" sits at UTF-16 offsets 5..8, same as its byte offsets here,
        // but reached only by walking past the multibyte 'é' first.
        let spliced = splice_at_utf16_offsets(value, 5, 8, "\t");
        assert_eq!(spliced, "café \t");
    }

    #[test]
    fn splice_at_utf16_offsets_inserts_at_start_of_multibyte_line() {
        let value = "café";
        let spliced = splice_at_utf16_offsets(value, 0, 0, "\t");
        assert_eq!(spliced, "\tcafé");
    }

    fn reconstruct(line: &str) -> String {
        line_pieces(line)
            .into_iter()
            .map(|piece| piece.text())
            .collect()
    }

    #[test]
    fn line_pieces_reconstruct_every_test_corpus_line() {
        // Reconstruction invariant: for any line, concatenating every piece
        // `render_line` would draw must reproduce the line exactly. A
        // `classify` span that overlaps or skips out of order breaks this
        // and duplicates or drops glyphs in the overlay — see the
        // unterminated char-literal case below, which broke it once.
        let lines = [
            r#"Text	BYTE	"Hello world!",'\n',0"#,
            "X\tBYTE\t\"100%\"\t% real comment",
            "Main\tdebug \"hi\"",
            "\tLDA\t\t$255,Text",
            r"'\n'",
            "\t.BYTE\t1,2,3",
            "Main' IS 3",
            "café ; a comment with non-ASCII",
            "",
        ];
        for line in lines {
            assert_eq!(reconstruct(line), line, "line: {line:?}");
        }
    }

    #[test]
    fn overlay_current_line_gets_the_class_only_on_that_line() {
        let current_line = Some(2);
        let line_numbers = [1, 2, 3];
        for line in line_numbers {
            let class = overlay_line_class(current_line == Some(line));
            assert_eq!(class.contains("overlay-current"), line == 2, "line {line}");
        }
    }

    #[test]
    fn overlay_row_is_current_maps_zero_based_row_to_one_based_line() {
        // Source line 2 is overlay row index 1 -- an off-by-one here would
        // flag row 2 (line 3) instead.
        let current_line = Some(2);
        for i in 0..4 {
            assert_eq!(overlay_row_is_current(i, current_line), i == 1, "row {i}");
        }
    }
}
