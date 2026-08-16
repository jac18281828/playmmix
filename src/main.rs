use checksmix::{Command, Debugger, MMixAssembler};
use log::info;
use yew::{Component, Context, Html, Renderer, html};

mod editor;
mod highlight;

use editor::Editor;

/// `examples/hello_world.mms` from checksmix, embedded verbatim. Not
/// `include_str!`: a dependency's on-disk location, wherever cargo puts it
/// (crates.io registry cache or git checkout alike), is not a stable,
/// crate-relative path.
const HELLO_WORLD_MMS: &str = "\tLOC\tData_Segment\n\tGREG\t@\nText\tBYTE\t\"Hello world!\",'\\n',0\n\n\tLOC\t#100\n\nMain\tdebug \"Version 0.1: Hello World Example\"\t\n\tLDA\t\t$255,Text\n\tTRAP\t0,Fputs,StdOut\n\tTRAP\t0,Halt,0\n";

/// Assemble, load, and run `source` to halt, returning the rendered
/// `Command::Run` output followed by `Command::State`'s. A parse error
/// surfaces as `Err` rather than a panic. Free of Yew and browser APIs so it
/// is unit-testable on the host target; the Yew component below is the only
/// wasm-specific caller.
fn run_source(source: &str) -> Result<Vec<String>, String> {
    let mut assembler = MMixAssembler::new(source, "hello.mms");
    assembler.parse()?;
    let mut debugger = Debugger::load(assembler);
    let mut output = debugger.execute(Command::Run);
    output.extend(debugger.execute(Command::State));
    Ok(output)
}

/// Assemble and run `source`, returning the fields `App` stores: the source
/// itself (handed back so the caller can move it in unconditionally), the
/// rendered output, and a parse error if assembly failed. Free of Yew and
/// browser APIs, so the editor's `oninput` wiring is host-testable without a
/// browser — `Component::update` is the only caller.
fn apply_source(source: String) -> (String, Vec<String>, Option<String>) {
    match run_source(&source) {
        Ok(output) => (source, output, None),
        Err(error) => (source, Vec::new(), Some(error)),
    }
}

pub enum Msg {
    SourceChanged(String),
}

pub struct App {
    source: String,
    output: Vec<String>,
    error: Option<String>,
}

impl Component for App {
    type Message = Msg;
    type Properties = ();

    fn create(_ctx: &Context<Self>) -> Self {
        let (source, output, error) = apply_source(HELLO_WORLD_MMS.to_string());
        Self {
            source,
            output,
            error,
        }
    }

    fn update(&mut self, _ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            Msg::SourceChanged(source) => {
                let (source, output, error) = apply_source(source);
                self.source = source;
                self.output = output;
                self.error = error;
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let body = match &self.error {
            Some(error) => format!("Assembly error: {error}"),
            None => self.output.join("\n"),
        };
        let on_change = ctx.link().callback(Msg::SourceChanged);

        html! {
            <main>
                <h1>{ "playmmix" }</h1>
                <Editor source={self.source.clone()} on_change={on_change} />
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_source_reports_halted_state() {
        let output = run_source(HELLO_WORLD_MMS).expect("hello world assembles and runs");
        assert!(!output.is_empty());
        assert!(
            output.iter().any(|line| line.contains("PC = 0x")),
            "expected Command::State output to include a PC line, got: {output:?}"
        );
    }

    #[test]
    fn run_source_surfaces_parse_errors() {
        let result = run_source("this is not valid mmix assembly $$$ ???");
        assert!(result.is_err());
    }

    #[test]
    fn apply_source_reflects_the_current_program() {
        let (_, hello_world_output, hello_world_error) = apply_source(HELLO_WORLD_MMS.to_string());
        assert!(hello_world_error.is_none());

        let halt_only = "\tLOC\t#100\n\tTRAP\t0,Halt,0\n".to_string();
        let (_, halt_only_output, halt_only_error) = apply_source(halt_only);
        assert!(halt_only_error.is_none());

        assert_ne!(
            hello_world_output, halt_only_output,
            "a different valid program must change the rendered output"
        );
    }

    #[test]
    fn apply_source_surfaces_a_new_parse_error() {
        let (_, _, valid_error) = apply_source(HELLO_WORLD_MMS.to_string());
        assert!(valid_error.is_none());

        let (_, _, broken_error) =
            apply_source("this is not valid mmix assembly $$$ ???".to_string());
        assert_ne!(valid_error, broken_error);
        assert!(broken_error.is_some());
    }
}
