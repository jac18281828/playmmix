//! Captures a running program's output and renders it as the output pane.

use std::cell::RefCell;
use std::rc::Rc;

use checksmix::Host;
use web_sys::Element;
use yew::prelude::*;

/// Which stream a captured [`OutputSpan`] came from. `Diagnostic` is
/// checksmix's own operator-facing notices (an unhandled trap, a truncated
/// string, the HALT notice `handle_halt` always emits) -- a third class,
/// distinct from the program's own stdout/stderr, but appended to the same
/// buffer in arrival order so the output pane reads as one timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
    Diagnostic,
}

/// One captured write, in the order it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSpan {
    pub stream: OutputStream,
    pub text: String,
}

/// The committed spans, plus stdout's and stderr's own undecoded tail --
/// bytes a `write` call has seen but not yet resolved into a whole
/// character. The two tails are kept apart so a byte from one stream can
/// never complete a character the other began; `debug`/diagnostic text
/// bypasses both, since checksmix hands it over as a complete, already-
/// valid `&str`.
///
/// `CaptureHost::write` extends whichever tail its own fd names, then
/// commits everything that decodes as a complete `char` -- an invalid
/// sequence included, exactly as `String::from_utf8_lossy` renders it --
/// leaving only a still-incomplete trailing sequence pending. `visible_spans`
/// renders both tails the same lossy way without draining them, so a read
/// mid-character shows U+FFFD without losing bytes a later write might yet
/// complete.
#[derive(Default)]
pub(crate) struct CapturedOutput {
    spans: Vec<OutputSpan>,
    stdout_pending: Vec<u8>,
    stderr_pending: Vec<u8>,
}

impl CapturedOutput {
    /// `spans`, plus a trailing, un-drained view of each stream's own
    /// pending tail -- computed fresh on every call, never cached, since
    /// nothing here commits to `spans` until `CaptureHost::write` itself
    /// decides a character is whole or definitely never will be.
    pub(crate) fn visible_spans(&self) -> Vec<OutputSpan> {
        let mut spans = self.spans.clone();
        for (stream, pending) in [
            (OutputStream::Stdout, &self.stdout_pending),
            (OutputStream::Stderr, &self.stderr_pending),
        ] {
            if !pending.is_empty() {
                spans.push(OutputSpan {
                    stream,
                    text: String::from_utf8_lossy(pending).into_owned(),
                });
            }
        }
        spans
    }
}

/// Shared handle to a program's captured output. `MMix::with_host` consumes
/// the host, so this `Rc` is the only way back to what it wrote -- held by
/// `Control`, cloned into the `Host` impl passed to `with_host`.
pub(crate) type OutputBuffer = Rc<RefCell<CapturedOutput>>;

/// Routes a loaded program's stdout (fd 1), stderr (fd 2), and diagnostics
/// into the shared [`OutputBuffer`] -- the seam that replaces `StdHost`
/// (whose `stdout()`/`stderr()` are a silent sink under
/// `wasm32-unknown-unknown`) with something the output pane can render.
pub(crate) struct CaptureHost {
    pub(crate) buffer: OutputBuffer,
}

/// Decodes as much of `pending`'s buffered bytes as forms complete `char`s,
/// draining what it decodes and leaving any trailing bytes that might still
/// complete a character arriving in the stream's next write. An invalid
/// sequence decodes immediately, as `String::from_utf8_lossy` would, rather
/// than waiting on bytes that will never complete it -- `Utf8Error::
/// error_len` is exactly this distinction: `None` for a sequence merely cut
/// short at the end of `pending`, `Some(len)` for one that is definitely
/// wrong regardless of what follows.
fn decode_complete_prefix(pending: &mut Vec<u8>) -> String {
    let mut text = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(valid) => {
                text.push_str(valid);
                pending.clear();
                return text;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                text.push_str(
                    std::str::from_utf8(&pending[..valid_up_to])
                        .expect("bytes before an error's own start are valid UTF-8"),
                );
                match error.error_len() {
                    Some(len) => {
                        text.push('\u{FFFD}');
                        pending.drain(..valid_up_to + len);
                    }
                    None => {
                        pending.drain(..valid_up_to);
                        return text;
                    }
                }
            }
        }
    }
}

impl Host for CaptureHost {
    fn write(&mut self, fd: u8, bytes: &[u8]) -> std::io::Result<()> {
        // checksmix confirms only fd 1 or 2 ever reach a `Host`; an
        // unrecognized fd (defensive only, never expected) is treated as
        // stdout rather than dropped, so no write silently vanishes.
        let stream = if fd == 2 {
            OutputStream::Stderr
        } else {
            OutputStream::Stdout
        };
        let mut captured = self.buffer.borrow_mut();
        let pending = match stream {
            OutputStream::Stderr => &mut captured.stderr_pending,
            _ => &mut captured.stdout_pending,
        };
        pending.extend_from_slice(bytes);
        let text = decode_complete_prefix(pending);
        if !text.is_empty() {
            captured.spans.push(OutputSpan { stream, text });
        }
        Ok(())
    }

    fn now_micros(&mut self) -> u64 {
        // Unused by any example this prompt covers; matches checksmix's own
        // `Host` doctest.
        0
    }

    fn diagnostic(&mut self, msg: &str) {
        self.buffer.borrow_mut().spans.push(OutputSpan {
            stream: OutputStream::Diagnostic,
            text: format!("{msg}\n"),
        });
    }
}

#[derive(Properties, PartialEq)]
pub struct OutputPaneProps {
    pub spans: Vec<OutputSpan>,
    /// Mirrored from the status line once halted, per `docs/layout-spec.md`'s
    /// Output pane section, so the result of a run reads in one place.
    pub exit_code: Option<u64>,
    /// Set on the `.output-pane` root element. `App`'s row splitter reads
    /// its `client_height()` as a drag's start size -- the pane's rendered
    /// height is content-driven (capped, not fixed, by `max-height`), so no
    /// constant can stand in for a live DOM read.
    #[prop_or_default]
    pub pane_ref: NodeRef,
}

/// The output pane: the program's captured stdout/stderr/diagnostic output,
/// pinned to the bottom while new output arrives. A user scroll-up unpins
/// it until they scroll back to the bottom themselves -- tracked with a
/// scroll listener rather than re-pinning on every render, which would
/// fight a deliberate scroll-up mid-run.
pub struct OutputPane {
    container_ref: NodeRef,
    pinned: bool,
}

pub enum OutputPaneMsg {
    Scroll,
}

impl Component for OutputPane {
    type Message = OutputPaneMsg;
    type Properties = OutputPaneProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            container_ref: NodeRef::default(),
            pinned: true,
        }
    }

    fn update(&mut self, _ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            OutputPaneMsg::Scroll => {
                if let Some(el) = self.container_ref.cast::<Element>() {
                    // A couple of pixels of slack: some browsers report a
                    // scroll position that never quite reaches the exact
                    // bottom due to subpixel rounding.
                    let at_bottom = el.scroll_top() + el.client_height() >= el.scroll_height() - 2;
                    self.pinned = at_bottom;
                }
                false
            }
        }
    }

    fn rendered(&mut self, _ctx: &Context<Self>, _first_render: bool) {
        if self.pinned
            && let Some(el) = self.container_ref.cast::<Element>()
        {
            el.set_scroll_top(el.scroll_height());
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let onscroll = ctx.link().callback(|_: Event| OutputPaneMsg::Scroll);
        let header = match ctx.props().exit_code {
            Some(code) => format!("OUTPUT  exit {code}"),
            None => "OUTPUT".to_string(),
        };
        html! {
            <div class="output-pane" ref={ctx.props().pane_ref.clone()}>
                <div class="output-header">{ header }</div>
                <div class="output-body" ref={self.container_ref.clone()} {onscroll}>
                    { for ctx.props().spans.iter().map(render_output_span) }
                </div>
            </div>
        }
    }
}

fn render_output_span(span: &OutputSpan) -> Html {
    let class = match span.stream {
        OutputStream::Stdout => "output-stdout",
        OutputStream::Stderr => "output-stderr",
        OutputStream::Diagnostic => "output-diagnostic",
    };
    html! { <span {class}>{ &span.text }</span> }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_host() -> (CaptureHost, OutputBuffer) {
        let buffer: OutputBuffer = Rc::new(RefCell::new(CapturedOutput::default()));
        let host = CaptureHost {
            buffer: buffer.clone(),
        };
        (host, buffer)
    }

    #[test]
    fn a_character_split_across_two_writes_reads_whole() {
        let (mut host, buffer) = fresh_host();
        host.write(1, &[0xC3]).expect("write never fails here");
        // The lead byte alone is a valid but incomplete start of a
        // two-byte character: it commits nothing to `spans` yet -- see
        // `reading_mid_character_...` for what a read shows in this window.
        assert!(buffer.borrow().spans.is_empty());

        host.write(1, &[0xA9]).expect("write never fails here");
        let spans = buffer.borrow().visible_spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].stream, OutputStream::Stdout);
        assert_eq!(spans[0].text, "é");
    }

    #[test]
    fn an_interleaved_write_to_the_other_stream_never_completes_this_streams_character() {
        let (mut host, buffer) = fresh_host();
        host.write(1, &[0xC3]).expect("write never fails here"); // stdout: half of 'é'
        host.write(2, b"x").expect("write never fails here"); // stderr: unrelated, complete
        host.write(1, &[0xA9]).expect("write never fails here"); // stdout: the other half

        let spans = buffer.borrow().visible_spans();
        let stdout_text: String = spans
            .iter()
            .filter(|span| span.stream == OutputStream::Stdout)
            .map(|span| span.text.as_str())
            .collect();
        let stderr_text: String = spans
            .iter()
            .filter(|span| span.stream == OutputStream::Stderr)
            .map(|span| span.text.as_str())
            .collect();
        assert_eq!(stdout_text, "é");
        assert_eq!(stderr_text, "x");
    }

    #[test]
    fn a_lone_continuation_byte_reads_as_the_replacement_character() {
        let (mut host, buffer) = fresh_host();
        // 0xA9 alone is a continuation byte with no lead byte -- genuinely
        // invalid, not merely incomplete, so it decodes immediately.
        host.write(1, &[0xA9]).expect("write never fails here");
        let spans = buffer.borrow().visible_spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "\u{FFFD}");
    }

    #[test]
    fn reading_mid_character_shows_the_replacement_character_without_losing_the_pending_byte() {
        let (mut host, buffer) = fresh_host();
        host.write(1, &[0xC3]).expect("write never fails here");
        // A read here, before the second byte ever arrives, must show
        // U+FFFD without discarding the byte already buffered.
        let spans = buffer.borrow().visible_spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "\u{FFFD}");

        host.write(1, &[0xA9]).expect("write never fails here");
        let spans = buffer.borrow().visible_spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].text, "é",
            "the pending byte must survive an intervening read"
        );
    }

    #[test]
    fn diagnostic_text_bypasses_the_pending_byte_path() {
        let (mut host, buffer) = fresh_host();
        host.diagnostic("halted");
        let spans = buffer.borrow().visible_spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].stream, OutputStream::Diagnostic);
        assert_eq!(spans[0].text, "halted\n");
    }
}
