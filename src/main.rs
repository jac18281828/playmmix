use checksmix::{Command, Debugger, MMixAssembler};
use log::info;
use yew::{Component, Context, Html, Renderer, html};

/// `examples/hello_world.mms` from checksmix, embedded verbatim. Not
/// `include_str!`: the git dependency's checkout path under
/// `~/.cargo/git/checkouts/` is not a stable, crate-relative path.
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

pub struct App {
    output: Vec<String>,
    error: Option<String>,
}

impl Component for App {
    type Message = ();
    type Properties = ();

    fn create(_ctx: &Context<Self>) -> Self {
        match run_source(HELLO_WORLD_MMS) {
            Ok(output) => Self {
                output,
                error: None,
            },
            Err(error) => Self {
                output: Vec::new(),
                error: Some(error),
            },
        }
    }

    fn update(&mut self, _ctx: &Context<Self>, _msg: Self::Message) -> bool {
        false
    }

    fn view(&self, _ctx: &Context<Self>) -> Html {
        let body = match &self.error {
            Some(error) => format!("Assembly error: {error}"),
            None => self.output.join("\n"),
        };

        html! {
            <main>
                <h1>{ "playmmix" }</h1>
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
}
