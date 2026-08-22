# playmmix

A browser playground for [checksmix](https://github.com/jac18281828/checksmix),
an MMIX interpreter written in Rust. checksmix's library half compiles for
`wasm32-unknown-unknown`; playmmix is a Yew app that runs it in a browser tab.

playmmix splits the screen into two columns: the source editor and program
output on the left, machine status/registers/specials/memory on the right,
collapsing to one column below about 1100px wide. Both the column boundary
and the boundary between the editor and the output pane are draggable
splitters, clamped so neither pane can be resized away entirely; sizes reset
to the default layout on reload. The source pane is editable: it loads with
`examples/hello_world.mms` from checksmix, and every
edit re-assembles and loads a fresh machine at the program's entry point —
nothing runs until asked to. The pane pairs a transparent `<textarea>` with a
syntax-highlight overlay and a line-number gutter; the current line (the last
instruction that ran) is marked in both the gutter and a full-width band in
the overlay.

advances one source-level step — a multi-word pseudo-op such as `SETI`,
`SET`, or `LDA` executes as a single Step, not several; Step Over runs a
whole call without descending into it; Reset reloads the current source
from the top, clearing output and highlights. The run state reads as one of
`stopped` (loaded, nothing executed yet), `running`, `paused` (stopped
after at least one instruction — a breakpoint, a Step, or an explicit
Stop), or `halted`; Run while halted resets and runs again in one click.
Clicking a line number in the gutter toggles a breakpoint there — a
standalone label line resolves to its next instruction's address; it's
rejected as a no-op on a line with no address at all, such as a blank line,
a comment, or a trailing label past the last instruction. A brief status
readout next to the Control Bar echoes the last action taken — `Running`,
`Stepped`, `Breakpoint set`, and so on.

The output pane, under the editor, shows the program's own stdout, stderr,
and diagnostic output (such as the HALT notice checksmix emits), interleaved
in arrival order and pinned to the bottom as it grows; its header shows the
exit code once halted.

The machine pane, on the right, reflects whatever the machine last did — a
step, a run, or a fresh load. Registers and specials render in a single
column, one row per register at a fixed width so a value updates in place
without reflow. `$0`-`$31` always render; a register or special that has
ever needed to render individually keeps rendering for the life of the
load, even if its value later returns to what would otherwise hide it, so a
row never pops in and out of view between renders. An unallocated global
range still collapses into one summary line when no `GREG` directive has
run. Memory shows the program's loaded extent, aligned to 16-byte rows
regardless of where a run starts, with hex and ASCII columns and the source
labels naming each address; the row and 4-byte instruction span containing
the last executed instruction are highlighted, as is any register, special,
or byte whose value changed since the last pause. Registers/specials and
memory each scroll independently.

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

## Deploying

playmmix is served from CloudFront over an S3 origin at
[playmmix.2ad.com](https://playmmix.2ad.com). The stack that provisions the
certificate, bucket, distribution, and DNS records lives in `cdk/`. Pushing a
tag runs `.github/workflows/deploy-static-site.yml`, which builds the bundle
with Trunk and syncs it to the bucket.

Two steps are human-only and are not run by this repository's automation:

1. Deploy the stack with AWS credentials: `bun run cdk:deploy StackPlaymmix2adCom`.
2. Take the `DistributionId` output from that deploy and set it as the
   `CLOUDFRONT_DISTRIBUTION_ID` secret in this repository. Until that secret
   is set, the deploy workflow's cache-invalidation step is skipped.

### Lifecycle

The stack owns every resource it needs, including its S3 origin bucket, so it
can be destroyed and redeployed freely with `bun run cdk:destroy` and
`bun run cdk:deploy StackPlaymmix2adCom`. Destroying it **deletes the bucket
and its contents** — safe, because the bundle is nothing but a Trunk build
artifact: `trunk build` regenerates it from source and
`deploy-static-site.yml` re-syncs it on the next tag push. The one thing
`cdk destroy` does not touch is the hosted zone, `Z09862671HYH6ZFKNPGNL`,
which this stack imports read-only and no stack owns.

## Relationship to checksmix

playmmix depends on checksmix as a published crates.io version (see
`Cargo.toml`). It calls only checksmix's public API — `MMixAssembler` to
assemble source, and `MMix` directly (not `Debugger`) to load, step, and run
the assembled program and render its state. Driving `MMix` directly gives
playmmix a caller-chosen instruction budget per chunk and a structured
[`Stop`](https://docs.rs/checksmix) reason, which the chunked run loop and
breakpoint handling in `src/control.rs` depend on.
