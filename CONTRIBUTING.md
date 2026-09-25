# Contributing to `playmmix`

Contributions are welcome: a bug report, a fix, a program that runs wrong or
a whole feature. Keyboard input is open: MMIX programs in `playmmix` can
print but cannot read `StdIn`, which first needs an input primitive in
checksmix's `Host`.

## Pull requests

1. Fork the repo and branch from `main`.
2. Add tests for anything that changes behavior.
3. Update the docs when you change what they describe.
4. Make the checks below pass.
5. Open the pull request.

`main` only ever fast-forwards. Rebase rather than merge.

## Before you push

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
trunk build --release
```

The CDK stack that deploys the site has its own checks:

```sh
bun install
bun run build
bun run test
bun run cdk:synth
```

Green on all of them, and green in CI, which runs the same set.

## Tests

Add tests for behavior changes, and prove each one fails when its target
breaks: break the code on purpose, watch the test go red, put it back. A
vacuous test covers nothing. Tests are hermetic: no network, no files outside
the checked-in tree, no clock.

Keep logic that needs no browser API (Yew, `wasm-bindgen`, `web-sys`) in
plain functions, so `cargo test` covers it on the host. Layout and CSS have
no host test; check them in a browser at desktop and phone widths against
`docs/layout-spec.md`.

## Commits

[Conventional Commits](https://www.conventionalcommits.org), signed:
`feat(header): …`, `fix(control): …`, `docs(readme): …`.

## Style

The tree is `rustfmt` clean and `clippy` clean with warnings as errors.
Otherwise match the file you are in: semantic names with no type or
namespace affixes, small single-purpose functions, `Result` and `Option`
rather than `unwrap` outside tests, and modules under about 1,500 lines.

Ask in an issue before adding a dependency. Anything new must build for
`wasm32-unknown-unknown`.

## Bug reports

Open an issue with a summary, the steps to reproduce, what you expected and
what you got.

For a program that assembles or runs wrong, **the share link is the
reproduction**. Click **Share**, paste the link into the issue and say what
you expected the output, a register or the exit code to be.

## Working with an AI agent

`AGENTS.md` is the brief for AI agents working in this repo: the conventions
at length and the completion gates. Point your agent at it.

## License

`playmmix` is distributed under the BSD 3-Clause License (`LICENSE.md`). By
contributing you agree that your contributions are licensed under the same
terms.

---

Adapted from the open-source contribution guidelines for
[Facebook's Draft](https://github.com/facebook/draft-js).
