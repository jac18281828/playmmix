//! Editable source pane: a transparent `<textarea>` layered over a
//! highlight-colored `<pre>` overlay, with a line-number gutter to the left.
//! `white-space: pre` and no wrap everywhere (`style.css`) keep line N of
//! the source mapped to row N of both the overlay and the gutter.

use web_sys::{HtmlElement, HtmlTextAreaElement};
use yew::prelude::*;

use crate::highlight;

#[derive(Properties, PartialEq)]
pub struct EditorProps {
    pub source: String,
    pub on_change: Callback<String>,
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

        let gutter_rows: Html = lines
            .iter()
            .enumerate()
            .map(|(i, _)| html! { <span class="gutter-line">{ i + 1 }</span> })
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

/// Splice a literal tab into `textarea` at the caret, replacing any current
/// selection, and place the caret immediately after it. Setting `.value()`
/// this way fires no `input` event, so the caller must re-run source-change
/// handling itself.
fn splice_tab(textarea: &HtmlTextAreaElement) {
    let value = textarea.value();
    let start = textarea.selection_start().ok().flatten().unwrap_or(0) as usize;
    let end = textarea.selection_end().ok().flatten().unwrap_or(0) as usize;
    let start = start.min(value.len());
    let end = end.min(value.len());

    let mut spliced = String::with_capacity(value.len() + 1);
    spliced.push_str(&value[..start]);
    spliced.push('\t');
    spliced.push_str(&value[end..]);
    textarea.set_value(&spliced);

    let caret = (start + 1) as u32;
    let _ = textarea.set_selection_range(caret, caret);
}

/// Render one source line as the overlay's colored spans, tagged by
/// `highlight::classify`. Untagged gaps between spans render in the
/// overlay's default text color.
fn render_line(line: &str) -> Html {
    let spans = highlight::classify(line);
    let mut children = Vec::new();
    let mut cursor = 0;

    for span in &spans {
        if span.start > cursor {
            children.push(html! { <span>{ &line[cursor..span.start] }</span> });
        }
        children.push(html! {
            <span class={span.kind.css_class()}>{ &line[span.start..span.end] }</span>
        });
        cursor = span.end;
    }
    if cursor < line.len() {
        children.push(html! { <span>{ &line[cursor..] }</span> });
    }

    html! { <span class="overlay-line">{ for children }</span> }
}
