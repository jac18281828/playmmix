# AI Contributing Requirements

These rules apply to all AI-assisted changes in this repository.

## Workflow
1. Read every file you plan to change and directly related modules.
2. Summarize current behavior and invariants before changing it.
3. **Ask each time** — `Cargo.toml` deps, cross-module or public-API refactors, file deletions,
   CI or release changes.
4. **Always ask** — merging to `main`, opening a PR, tags, force ops, anything that touches
   shared state or `main`.
5. Affirm all `Completion Gates` are met.

## Code Design
- Prioritize correctness, then clarity, convention, and reviewability over cleverness.
- Keep diffs focused; avoid idiosyncratic churn.
- Decompose into small, single-purpose functions and modules.
- Extract helpers as behavior grows rather than accreting onto existing ones.
- Write comments that explain enduring intent or constraints, no editorial comments.

## Naming
- Naming must be semantic.
- Do not encode type or structural primitives in names (int, object, string, etc).
- Avoid namespace prefixes or suffixes. If everything starts with or ends with
  `_fix_`, nothing should.
- Use names like `State`, `Context`, or `Manager` only if a clear abstraction
  requires it at a systemic level.

## Abstraction
- Abstract to remove duplication or enforce invariants.
- Prefer concrete types over generic wrappers.
- Avoid `unwrap`/`expect` outside of tests; truly-infallible uses with a
  justifying comment are acceptable. Use effective error handling patterns
  including `Result` and `Option`.

## Dependencies and Imports
- Prefer the standard library and checksmix's public API.
- Add external crates only with user approval.
- Declare imports at the top of each module; keep them explicit and organized
  so dependencies are clear.
- Respect the WASM target: any new dependency must build on
  `wasm32-unknown-unknown` and not pull in native-only syscalls.

## Tests
- Test project behavior and contracts, not language or dependency internals.
- Avoid vacuous tests: removing or breaking target code must cause a test to fail.
- Unit tests are required to be hermetic: no network, no external assets, no
  wall-clock or entropy dependencies.
- Add or update tests for every behavior change.
- Keep logic that does not need browser APIs (Yew, `wasm-bindgen`, `web-sys`)
  in plain functions so it can be unit-tested on the host target — see
  `run_source` in `src/main.rs`.

## Completion Gates

Before marking work complete, run and report:

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test`
4. `cargo build`
5. `trunk build --release` — subsumes a wasm-target check; the wasm bundle
   is the artifact that matters, so there is no separate
   `cargo check --target wasm32-unknown-unknown` step.
6. `npm run build` — typecheck the CDK stack in `cdk/`.
7. `npm test` — jest specs for the CDK stack.
8. `npx cdk synth` — the stack must synthesize to a template.

Do not mark work complete until all gates pass.
