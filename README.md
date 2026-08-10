# playmmix

A browser playground for [checksmix](https://github.com/jac18281828/checksmix),
an MMIX interpreter written in Rust. checksmix's library half compiles for
`wasm32-unknown-unknown`; playmmix is a Yew app that runs it in a browser tab.

This is the **walking skeleton**: one hardcoded MMIX program
(`examples/hello_world.mms` from checksmix), assembled and run to halt on
page load, with the resulting machine state rendered as preformatted text.
It shows machine state only — the program's own output (`Fputs`) writes to
`stdout()`, which is a silent sink on wasm until checksmix lands a `Host`
trait. There is no editor, no examples picker, and no output pane yet; those
arrive in later phases.

## Run locally

Install [Trunk](https://trunkrs.dev/) and the wasm target:

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
```

Then, from this directory:

```sh
trunk serve
```

Open `http://127.0.0.1:8080`.

## Relationship to checksmix

playmmix depends on checksmix as a git dependency pinned to a commit SHA
(see `Cargo.toml`). It calls only checksmix's public API — `MMixAssembler`
to assemble source, and `Debugger` to load and run the assembled program and
render its state.
