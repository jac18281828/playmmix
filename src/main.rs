use log::info;
use yew::{Component, Context, Html, Renderer, html};

mod control;
mod editor;
mod highlight;

use control::{Control, Controls, StepOutcome, yield_to_event_loop};
use editor::Editor;

/// `examples/hello_world.mms` from checksmix, embedded verbatim. Not
/// `include_str!`: a dependency's on-disk location, wherever cargo puts it
/// (crates.io registry cache or git checkout alike), is not a stable,
/// crate-relative path.
const HELLO_WORLD_MMS: &str = "\tLOC\tData_Segment\n\tGREG\t@\nText\tBYTE\t\"Hello world!\",'\\n',0\n\n\tLOC\t#100\n\nMain\tdebug \"Version 0.1: Hello World Example\"\t\n\tLDA\t\t$255,Text\n\tTRAP\t0,Fputs,StdOut\n\tTRAP\t0,Halt,0\n";

/// The filename `Control` assembles the editor's buffer under. Fixed:
/// playmmix edits a single in-memory buffer, not a multi-file project, and
/// breakpoint/PC line lookups need the same name on every assemble.
const SOURCE_FILENAME: &str = "source.mms";

pub enum Msg {
    SourceChanged(String),
    ToggleBreakpoint(usize),
    Run,
    Step,
    StepOver,
    Stop,
    /// One chunk boundary: reschedule if the run isn't finished, or if a
    /// `Stop` landed while this tick was scheduled, do nothing.
    ChunkTick,
}

pub struct App {
    source: String,
    control: Control,
    error: Option<String>,
}

impl App {
    /// Yield to the event loop, then deliver `Msg::ChunkTick` -- the one
    /// place a chunked run reschedules itself.
    fn schedule_chunk_tick(ctx: &Context<Self>) {
        let link = ctx.link().clone();
        yield_to_event_loop(move || link.send_message(Msg::ChunkTick));
    }
}

impl Component for App {
    type Message = Msg;
    type Properties = ();

    fn create(_ctx: &Context<Self>) -> Self {
        let control = Control::new(HELLO_WORLD_MMS, SOURCE_FILENAME)
            .expect("the embedded hello-world example assembles");
        Self {
            source: HELLO_WORLD_MMS.to_string(),
            control,
            error: None,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            Msg::SourceChanged(source) => {
                self.error = self.control.reload(&source).err();
                self.source = source;
                true
            }
            Msg::ToggleBreakpoint(line) => {
                self.control.toggle_breakpoint(line);
                true
            }
            Msg::Run => {
                self.control.start_run();
                Self::schedule_chunk_tick(ctx);
                true
            }
            Msg::Step => {
                if !self.control.is_running() {
                    self.control.step();
                }
                true
            }
            Msg::StepOver => {
                if !self.control.is_running() {
                    self.control.step_over();
                }
                true
            }
            Msg::Stop => {
                self.control.stop();
                true
            }
            Msg::ChunkTick => {
                // A Stop is checked only between chunks, never inside one;
                // this is that check. If Stop landed while this tick was
                // already scheduled, drop the tick instead of running.
                if !self.control.is_running() {
                    return false;
                }
                match self.control.run_chunk(control::CHUNK_BUDGET) {
                    StepOutcome::BudgetExhausted => Self::schedule_chunk_tick(ctx),
                    StepOutcome::Halted | StepOutcome::Breakpoint(_) => {}
                    StepOutcome::Advanced => {
                        unreachable!(
                            "run_chunk always halts, hits a breakpoint, or exhausts its budget"
                        )
                    }
                }
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let body = match &self.error {
            Some(error) => format!("Assembly error: {error}"),
            None => format!("{}", self.control.machine()),
        };

        let on_change = ctx.link().callback(Msg::SourceChanged);
        let on_toggle_breakpoint = ctx.link().callback(Msg::ToggleBreakpoint);
        let on_run = ctx.link().callback(|()| Msg::Run);
        let on_step = ctx.link().callback(|()| Msg::Step);
        let on_step_over = ctx.link().callback(|()| Msg::StepOver);
        let on_stop = ctx.link().callback(|()| Msg::Stop);

        // The PC indicator only means something while nothing is actively
        // moving it; showing it mid-run would flicker with every chunk.
        let current_line = (!self.control.is_running())
            .then(|| self.control.current_line())
            .flatten();

        html! {
            <main>
                <h1>{ "playmmix" }</h1>
                <Controls running={self.control.is_running()} {on_run} {on_step} {on_step_over} {on_stop} />
                <Editor
                    source={self.source.clone()}
                    {on_change}
                    breakpoints={self.control.breakpoint_lines().clone()}
                    {current_line}
                    {on_toggle_breakpoint}
                />
                <pre>{ body }</pre>
            </main>
        }
    }
}

fn main() {
    wasm_logger::init(wasm_logger::Config::default());
    info!("Starting playmmix");
    Renderer::<App>::new().render();
}
