# playmmix

A browser playground for [checksmix](https://github.com/jac18281828/checksmix),
an MMIX interpreter written in Rust. checksmix's library half compiles for
`wasm32-unknown-unknown`; playmmix is a Yew app that runs it in a browser tab.

The source pane is editable: it loads with `examples/hello_world.mms` from
checksmix, and every edit re-assembles and loads a fresh machine at the
program's entry point — nothing runs until asked to. The pane pairs a
transparent `<textarea>` with a syntax-highlight overlay and a line-number
gutter.

Run, Step, Step Over, and Stop control execution explicitly. Run executes in
chunks, yielding to the browser between them, so Stop takes effect within
about a quarter second rather than freezing the tab. Step advances one
instruction; Step Over runs a whole call without descending into it.
Clicking a line number in the gutter toggles a breakpoint there (rejected as
a no-op on a line with no address, such as a blank line or a comment); the
gutter also marks the paused machine's current line.

The machine pane on the right reflects whatever the machine last did — a
step, a run, or a fresh load. General registers show under the full ISA
visibility rule (nonzero, or a local register in use, or a global register),
so a run doesn't drown in 224 zero rows; an unallocated global range
collapses into one summary line instead. Special registers pin `rA rG rL rO
rS rJ`, the ones that teach the register stack, and show any other nonzero
one alongside them. Memory shows the program's loaded extent — grouped by
MMIX segment, with hex and ASCII columns, tagged with the source labels that
name each address.

It shows machine state only — the program's own output (`Fputs`) writes to
`stdout()`, which is a silent sink on wasm until checksmix lands a `Host`
trait. There is no examples picker or output pane yet; those arrive in later
phases.

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
