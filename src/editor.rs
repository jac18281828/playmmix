//! Editable source pane: a transparent `<textarea>` layered over a
//! highlight-colored `<pre>` overlay, with a line-number gutter to the left.
//! `white-space: pre` and no wrap everywhere (`style.css`) keep line N of
//! the source mapped to row N of both the overlay and the gutter.

use std::collections::BTreeSet;

use web_sys::{HtmlElement, HtmlTextAreaElement};
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
    overlay_ref: NodeRef,
    gutter_ref: NodeRef,
}

pub enum EditorMsg {
    /// The textarea's value changed (typing, paste, cut, ...).
    Input,
    /// Tab was pressed: a literal tab was already spliced into the
    /// textarea's DOM value; report the new value upward.
    TabInserted,
    /// The textarea scrolled; mirror its position onto the overlay and
    /// gutter.
    Scroll,
}

impl Component for Editor {
    type Message = EditorMsg;
    type Properties = EditorProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            textarea_ref: NodeRef::default(),
            overlay_ref: NodeRef::default(),
            gutter_ref: NodeRef::default(),
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
                if let Some(overlay) = self.overlay_ref.cast::<HtmlElement>() {
                    overlay.set_scroll_top(scroll_top);
                    overlay.set_scroll_left(scroll_left);
                }
                if let Some(gutter) = self.gutter_ref.cast::<HtmlElement>() {
                    gutter.set_scroll_top(scroll_top);
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

        let overlay_rows: Html = lines.iter().map(|line| render_line(line)).collect();

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
                <div class="gutter" ref={self.gutter_ref.clone()}>
                    { gutter_rows }
                </div>
                <div class="editor-surface">
                    <pre class="overlay" ref={self.overlay_ref.clone()}>
                        { overlay_rows }
                    </pre>
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

/// Render one source line as the overlay's colored spans. Untagged gaps
/// between spans render in the overlay's default text color.
fn render_line(line: &str) -> Html {
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

    html! { <span class="overlay-line">{ for children }</span> }
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
}
