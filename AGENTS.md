# AGENTS.md — hard repository rules

These rules bind every contributor and agent. If they conflict with a request, stop and
ask instead of guessing.

## Read and scope

Read, in order:

1. [`docs/product.md`](docs/product.md) — current behavior and guarantees.
2. [`ARCHITECTURE.md`](ARCHITECTURE.md) — ownership and dependency boundaries.
3. [`docs/testing.md`](docs/testing.md) — which evidence to run.
4. This file — non-negotiable working rules.

Use [`docs/decisions.md`](docs/decisions.md) for accepted/open choices and
[`docs/legacy-ids.md`](docs/legacy-ids.md) when interpreting old FR/NFR/DEC references.
The improvement plan is historical; completed milestones do not order new work.

## Dependencies and architecture

- Never add any dependency, including dev/build dependencies, without owner approval.
  State crate, version, purpose, approximate transitive weight, and why std/existing
  dependencies are insufficient. After approval use `cargo add`, never hand-edit a
  version into `Cargo.toml`.
- Ask explicitly before any dependency needing unsafe code or an FFI build step.
- Preserve `tui → application → ports ← adapters`, with pure `domain`. Application and
  domain do not import Ratatui/Crossterm or spawn processes.
- Reducers and rendering perform no IO. Long work is a cancellable background job;
  results carry identity and stale/superseded results are dropped.
- One thread owns `App`; do not introduce `Arc<Mutex<App>>`.
- Add a port only for a real external boundary with a test fake or imminent second
  implementation. Do not build empty abstractions.

## Errors, terminal, and concurrency

- `unwrap()`, `expect()`, `panic!()`, and `unreachable!()` are forbidden outside
  `#[cfg(test)]`. Return a typed per-layer `thiserror`; `anyhow` is only for `main.rs`.
- Every user-facing failure states the next action.
- Anything taking over the terminal uses `tui::terminal::TerminalGuard`; restore it on
  normal exit, error, panic, and supported signals.
- The event loop performs no IO and must not block more than 50 ms. Bound progress,
  workers, memory, and child output; preserve prompt cancellation.
- Never claim cancellation proves a dispatched remote mutation did not happen.

## Files, privacy, and remote state

- Never write in the user's repository or `.git`. App-owned files belong under
  `$SMART_REVIEW_HOME` (default `~/.smart-review`).
- Never log, print, snapshot, or display API key values or source/prompt/response/review
  contents in ordinary diagnostics. Credentials exist only in `credentials.toml` mode
  `0600`; doctor reports presence/source, never values.
- Never interpolate input into a shell string. Pass executable plus argv array.
- Never mutate GitHub, push, comment, resolve, or publish without explicit confirmation;
  always honor `--dry-run`. Tests use fakes unless the owner explicitly authorizes a
  sandbox target.
- Update the relevant living doc in the same change when behavior, architecture,
  testing policy, configuration, or a decision changes. Never infer an open decision.

## Lints and tests

The workspace lint policy lives in `Cargo.toml`: pedantic Clippy, missing error/panic
docs, forbidden unsafe and production panic/unwrap/expect/todo/unimplemented/
unreachable/dbg. Fix code rather than suppressing a lint. Any necessary `#[allow]` is
item-scoped and comments why the lint is wrong. Use checked integer conversions.

Add the lowest-level test that catches the regression. Routine tests use no network,
real repository, credential, account, or installed `gh`. Race tests control completion
order; TUI changes normally need reviewed `TestBackend` snapshots. Reference a legacy
FR/NFR/IR ID in the test name/doc when it materially improves traceability.

Before claiming completion, run:

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
scripts/validate/all.sh
```

For intentional snapshots run `UPDATE_SNAPSHOTS=1 cargo test --test shell_snapshots`,
then inspect every changed line. Paths shown in snapshots use `paths::shorten_for_display`.

## Git and review evidence

- Never commit or push unless asked. Do not rewrite shared history or discard work you
  did not create.
- Keep a PR focused on one coherent invariant. Its description ends with an exact
  isolated manual recipe: setup, terminal size, numbered inputs, expected/failure
  states, disk/request checks, human-only judgments, and cleanup.
- A green test or snapshot is not proof that a stream arrives, click lands, preview is
  readable, or prompt prevents spending. Name what automation cannot judge.
