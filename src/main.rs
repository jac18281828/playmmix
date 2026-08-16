use gloo_timers::callback::Timeout;
use log::info;
use yew::{Component, Context, Html, Renderer, html};

mod control;
mod editor;
mod highlight;
mod machine;

use control::{Control, ControlBar, StepOutcome, yield_to_event_loop};
use editor::Editor;
use machine::MachinePane;

/// `examples/hello_world.mms` from checksmix, embedded verbatim. Not
/// `include_str!`: a dependency's on-disk location, wherever cargo puts it
/// (crates.io registry cache or git checkout alike), is not a stable,
/// crate-relative path.
const HELLO_WORLD_MMS: &str = "\tLOC\tData_Segment\n\tGREG\t@\nText\tBYTE\t\"Hello world!\",'\\n',0\n\n\tLOC\t#100\n\nMain\tdebug \"Version 0.1: Hello World Example\"\t\n\tLDA\t\t$255,Text\n\tTRAP\t0,Fputs,StdOut\n\tTRAP\t0,Halt,0\n";

/// A minimal, fixed program that always assembles -- the fallback if the
/// embedded `HELLO_WORLD_MMS` somehow doesn't (e.g. a checksmix upgrade
/// changes accepted syntax), so `App::create` has no fallible path of its
/// own without threading an `Option<Control>` through every view and update
/// path just for that one unlikely case.
const FALLBACK_MMS: &str = "\tLOC\t#100\nMain\tTRAP\t0,Halt,0\n";

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
    /// The pending chunk-tick timeout, if a chunked Run or Step Over is in
    /// flight. Held rather than `.forget()`-ten so Stop, or a `reload` mid-
    /// run, can cancel it by dropping this (runs `clearTimeout` and frees
    /// the closure) instead of leaking one allocation per chunk boundary.
    chunk_timeout: Option<Timeout>,
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
        match self.control.continue_chunk(control::CHUNK_BUDGET) {
            StepOutcome::BudgetExhausted => self.schedule_chunk_tick(ctx),
            StepOutcome::Halted | StepOutcome::Breakpoint(_) | StepOutcome::Advanced => {}
        }
    }
}

impl Component for App {
    type Message = Msg;
    type Properties = ();

    fn create(_ctx: &Context<Self>) -> Self {
        match Control::new(HELLO_WORLD_MMS, SOURCE_FILENAME) {
            Ok(control) => Self {
                source: HELLO_WORLD_MMS.to_string(),
                control,
                error: None,
                chunk_timeout: None,
            },
            Err(error) => Self {
                source: HELLO_WORLD_MMS.to_string(),
                control: Control::new(FALLBACK_MMS, SOURCE_FILENAME)
                    .expect("FALLBACK_MMS is a fixed, minimal, always-valid program"),
                error: Some(error),
                chunk_timeout: None,
            },
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            Msg::SourceChanged(source) => {
                self.error = self.control.reload(&source).err();
                self.source = source;
                self.chunk_timeout = None;
                true
            }
            Msg::ToggleBreakpoint(line) => {
                self.control.toggle_breakpoint(line);
                true
            }
            Msg::Run => {
                self.control.start_run();
                if self.control.is_running() {
                    self.schedule_chunk_tick(ctx);
                }
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
                    match self.control.step_over_chunk(control::CHUNK_BUDGET) {
                        StepOutcome::BudgetExhausted => self.schedule_chunk_tick(ctx),
                        StepOutcome::Advanced
                        | StepOutcome::Halted
                        | StepOutcome::Breakpoint(_) => {}
                    }
                }
                true
            }
            Msg::Stop => {
                self.control.stop();
                self.chunk_timeout = None;
                true
            }
            Msg::ChunkTick => {
                self.advance_chunk(ctx);
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let machine_view = match &self.error {
            Some(error) => html! { <pre>{ format!("Assembly error: {error}") }</pre> },
            None => {
                let mmix = self.control.machine();
                let registers = machine::visible_registers(mmix);
                let specials = machine::visible_specials(mmix);
                let runs = machine::memory_runs(mmix, self.control.labels());
                let memory = machine::memory_rows(&runs);
                let exit_code = self.control.is_halted().then(|| mmix.get_exit_code());
                html! {
                    <MachinePane
                        {registers}
                        {specials}
                        {memory}
                        pc={self.control.get_pc()}
                        {exit_code}
                        call_depth={self.control.call_depth()}
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

        // The PC indicator only means something while nothing is actively
        // moving it; showing it mid-run would flicker with every chunk.
        let current_line = (!self.control.is_running())
            .then(|| self.control.current_line())
            .flatten();

        html! {
            <main>
                <h1>{ "playmmix" }</h1>
                <ControlBar
                    running={self.control.is_running()}
                    halted={self.control.is_halted()}
                    {on_run}
                    {on_step}
                    {on_step_over}
                    {on_stop}
                />
                <Editor
                    source={self.source.clone()}
                    {on_change}
                    breakpoints={self.control.breakpoint_lines().clone()}
                    {current_line}
                    {on_toggle_breakpoint}
                />
                { machine_view }
            </main>
        }
    }
}

fn main() {
    wasm_logger::init(wasm_logger::Config::default());
    info!("Starting playmmix");
    Renderer::<App>::new().render();
}
