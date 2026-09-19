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

/// Shared handle to a program's captured output. `MMix::with_host` consumes
/// the host, so this `Rc` is the only way back to what it wrote -- held by
/// `Control`, cloned into the `Host` impl passed to `with_host`.
pub(crate) type OutputBuffer = Rc<RefCell<Vec<OutputSpan>>>;

/// Routes a loaded program's stdout (fd 1), stderr (fd 2), and diagnostics
/// into the shared [`OutputBuffer`] -- the seam that replaces `StdHost`
/// (whose `stdout()`/`stderr()` are a silent sink under
/// `wasm32-unknown-unknown`) with something the output pane can render.
pub(crate) struct CaptureHost {
    pub(crate) buffer: OutputBuffer,
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
        let text = String::from_utf8_lossy(bytes).into_owned();
        self.buffer.borrow_mut().push(OutputSpan { stream, text });
        Ok(())
    }

    fn now_micros(&mut self) -> u64 {
        // Unused by any example this prompt covers; matches checksmix's own
        // `Host` doctest.
        0
    }

    fn diagnostic(&mut self, msg: &str) {
        self.buffer.borrow_mut().push(OutputSpan {
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
