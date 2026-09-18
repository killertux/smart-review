# AGENTS.md — hard rules for this repository

This file is binding for any agent (or human) working on smart-review. If a rule
here conflicts with a request, stop and ask rather than guessing.

## 1. Read this first, in order

1. `REQUIREMENTS.md` — *what* to build. Requirement IDs (`FR-3.2`, `NFR-1.2`,
   `DEC-7`) are the vocabulary for commits, PRs, tests and questions.
2. `PLAN.md` — *in what order*, and what "done" means per milestone.
3. `ARCHITECTURE.md` — *how* the code is organised.
4. This file — the rules that are not negotiable.

Work in milestone order. Do not start `M(n+1)` while `M(n)` is open.

## 2. Dependencies

- **Never add a dependency without asking the owner first.** This includes
  `[dev-dependencies]` and `[build-dependencies]`.
- When asking, state: crate, version, what it is for, roughly how much it pulls
  in, and why the standard library or an existing dependency is not enough.
- **Always add dependencies with `cargo add`**, never by hand-editing a version
  into `Cargo.toml`. That keeps versions current and the lock file consistent.
- Prefer one well-maintained crate over two narrow ones. Ask explicitly before
  anything that needs `unsafe` or an FFI build step.

## 3. Architecture

- Obey the dependency rule (`ARCHITECTURE.md` §2): `tui` → `application` →
  `ports`, with `adapters` implementing `ports`, and `domain` depending on
  nothing. `application` and `domain` must not import `ratatui`, `crossterm`, or
  spawn processes.
- `tui` renders state and turns input into actions. It must never perform IO.
- Put behaviour behind a port only when there is, or will imminently be, a second
  implementation or a test fake. Do not build empty abstractions.

## 4. Errors and panics

- `unwrap()`, `expect()`, `panic!()` and `unreachable!()` are forbidden outside
  `#[cfg(test)]` code. Return a typed error instead.
- Errors are typed per layer with `thiserror`; `anyhow` appears only at the
  `main.rs` boundary.
- Every user-facing message must state the next action ("run `gh auth login`"),
  not just the failure.
- The terminal must be restored on every exit path: normal, error, panic and
  signal. Anything that takes over the terminal goes through
  `tui::terminal::TerminalGuard`.

## 5. Concurrency and the event loop

- The event loop must never block for more than 50 ms and never performs IO
  (`NFR-1.2`). Long work is a job on the background runtime, reporting progress
  over a channel and cancellable from the UI.
- Results carry their job id and are dropped if superseded or cancelled.
- State is owned by one thread. Do not introduce `Arc<Mutex<App>>`.

## 6. Files, secrets and the user's machine

- Never write inside the user's repository working tree, including `.git`.
  Everything the app owns lives under `$SMART_REVIEW_HOME` (default
  `~/.smart-review`).
- Never log, print or display an API key, token, or file contents. Keys live only
  in `credentials.toml` (mode `0600`); `doctor` reports presence, never values.
- Never mutate remote state (post a review, push, comment) without an explicit
  user confirmation, and always honour `--dry-run`.
- Never interpolate user input into a shell string. Pass an argv array.

## 7. Lints and formatting

The workspace sets the lint policy once, in `Cargo.toml` under
`[workspace.lints]`; crates opt in with `[lints] workspace = true`. That includes
`clippy::pedantic`, `missing_errors_doc`, `missing_panics_doc`, `unwrap_used`,
`expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`, `dbg_macro` and
`unsafe_code = "forbid"`.

CI runs clippy with `-D warnings`, so every warning is an error there. Before
claiming any work is done, run:

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
scripts/validate/all.sh
```

Rules of engagement with the lints:

- **Fix the code, do not silence the lint.** An `#[allow(...)]` needs a comment
  saying why the lint is wrong here, and it must be as narrow as possible (on the
  item, not the module or crate).
- Public fallible functions need a `# Errors` doc section; public functions that
  can panic need `# Panics`. `#[must_use]` goes on accessors and pure
  constructors.
- Prefer `u16::try_from(x).unwrap_or(u16::MAX)` over `x as u16` when truncation is
  possible.

## 8. Tests

- Add tests at the lowest level that can catch the regression: unit tests for
  pure functions, `TestBackend` snapshot tests for rendering, fakes for ports in
  application tests.
- **No test may touch the network** or require `gh`, an API key, or a real
  repository. Adapter contract tests stay behind an explicit opt-in.
- Reference the requirement ID in the test name or its doc comment where it is
  not obvious (for example `parses_the_leader_token` covers `FR-7.2`).
- TUI changes usually require regenerating snapshots: run
  `UPDATE_SNAPSHOTS=1 cargo test --test shell_snapshots`, then **read the diff**
  and confirm every changed line is intentional before committing.
- Snapshot text must not contain machine-specific absolute paths; paths rendered
  in the UI go through `paths::shorten_for_display`.

## 9. Documentation

- Update `REQUIREMENTS.md` in the same change as the code when behaviour or a
  decision changes. Flip `DEC-n` rows from proposed to decided and add a line to
  the decision log — never leave a `[PROPOSED]` marker on something implemented.
- Keep `PLAN.md` honest: if a milestone's scope changes, change the plan.

## 10. Git

- **Never commit or push unless asked.**
- Never rewrite shared history, force-push, or amend someone else's commit.
- One milestone per commit or PR, titled with the milestone (`M0: ...`), unless
  the owner asks otherwise.
- **Every milestone PR description ends with a manual test recipe.** A reviewer who
  does not want to read the diff must be able to convince themselves the milestone
  works by running the app. Write it as a script: the exact commands to set up
  (`SMART_REVIEW_HOME`, a fixture repository, a fake or real provider), then numbered
  keystrokes with what to look for after each, then what to check on disk. Say what a
  *failure* looks like, not only what success looks like, and name the one or two
  things the automated gate cannot check (a stream arriving, a click landing, a
  prompt asking before it spends money) — that is what the recipe is for.
