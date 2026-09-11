# Smart Review — Implementation Plan

**Companion to:** `REQUIREMENTS.md` (the source of truth for *what*; this file is *how and in what order*).
**Status:** Draft v1.

---

## 0. How to use this plan

- **A milestone is a shippable increment, not a phase.** Every milestone ends with a binary the owner can run and judge. If a milestone cannot be demonstrated in the terminal in under five minutes, it is scoped wrong.
- **Exit criteria are objective.** A milestone is done when: (a) its FR acceptance criteria in `REQUIREMENTS.md` all pass, (b) `scripts/validate/m<N>.sh` passes, (c) CI is green on `fmt` + `clippy -D warnings` + `test`, and (d) the manual demo in §2 is rehearsed.
- **Order is a hard dependency chain** (DEV-3). Do not start `M(n+1)` while `M(n)` is open. Within a milestone, work items can be parallelised.
- **Every work item names the FRs it satisfies**, so coverage gaps are visible by grepping for an FR id across the plan.
- **Crates are gated.** No dependency is added without explicit owner approval (DEP-1). The ledger in §5 lists everything each milestone would need, with justification, so approvals can be granted in one batch per milestone.
- **`REQUIREMENTS.md` wins.** If implementation reveals a requirement is wrong, stop and change the requirement first (DEV-5), then the code.
- **Nothing is committed or pushed unless asked** (DEV-6).

### 0.1 Definition of done (applies to every milestone)
- [ ] `cargo fmt --check` clean; `cargo clippy --all-targets --all-features -- -D warnings` clean, including `clippy::pedantic` and the workspace restriction lints (DEV-8). No blanket `#[allow]`: every suppression is item-scoped and carries a comment.
- [ ] `cargo test --all-features` green; new behaviour has tests at the lowest sufficient level (DEV-4).
- [ ] No `unwrap`/`expect`/`panic!`/`unreachable!` outside tests (ARCH-7) — enforced by lints, not by review.
- [ ] Terminal is restored on exit, error and panic (NFR-4.2 / FR-9.1). **Known gap:** a `SIGTERM`/`kill -INT` restore needs `signal-hook`, which is not yet approved — tracked in the M0 summary and added to the ledger before M1.
- [ ] Works with `SMART_REVIEW_HOME=$(mktemp -d)` and zero config.
- [ ] `REQUIREMENTS.md` updated for anything learned or changed (DEV-5).
- [ ] `git tag m<N>` on the milestone commit; `cargo build --release` produces `target/release/smart-review`.

---

## 1. Milestone overview

| ID | Theme | Runnable artifact (what you can *do*) | Validation | Primary FRs | New crates (approval needed) |
|---|---|---|---|---|---|
| **M0** | Skeleton, rules, CI | `smart-review` opens a themed TUI shell: status line, placeholder panes, modes, `?` help, `<leader>` menu, `:theme`, `:q`. `--version`, `--check`. | `scripts/validate/m0.sh` + manual demo | FR-7.1–7.4, 7.6–7.8, 8.1–8.3, 9.1, 9.2 | ratatui, crossterm, clap, serde, toml, thiserror, anyhow |
| **M1** | PR browsing & diff reading | Inside a clone: list PRs, filter/search, open one, read metadata + full diff with vim navigation, mouse support, cached. | `scripts/validate/m1.sh` + manual demo | FR-1.1–1.3, 2.1–2.4, 3.2–3.4, 7.5, 7.7, 9.3 | serde_json, `time` (or chrono), unicode-width |
| **M2** | Workspace + LLM analysis + ordered review | Pick provider/model/thinking and paste a key **in the TUI**, then get a streamed analysis and a re-ordered diff; workspace created in the background. | `scripts/validate/m2.sh` + manual demo | FR-3.1, 3.5, 4.1–4.8 | llm, tokio, reqwest (or reuse), maybe `secrecy` |
| **M3** | Chat | A persistent, streaming chat per PR grounded in the context bundle, with `:context` inspection. | `scripts/validate/m3.sh` + manual demo | FR-5.1–5.4 | none new |
| **M4** | Review publishing | Stage inline comments, review the publish modal, submit one batched review to GitHub; `--dry-run` prints commands only. | `scripts/validate/m4.sh` + manual demo against a sandbox PR | FR-6.1–6.5, 3.3 (existing discussion) | none new |
| **M5** | Polish & release | Visual ranges, thread replies, docs, `NO_COLOR`, release binary + `--version`; backlog items from §11. | `scripts/validate/m5.sh` + manual demo | backlog + NFR polish | decided then |

Each milestone is a strictly larger subset of the same binary — never a rewrite. M0's TUI shell, action registry, job framework and config loader are load-bearing for M1–M5, which is why they get disproportionate care up front.

---

## 2. Milestone detail

### M0 — Skeleton, hard rules, CI

**Goal:** a real binary that starts, respects the architecture, restores the terminal no matter what, and is already governed by CI and the repo's hard rules. No network, no git, no LLM.

**Runnable artifact**
```
$ cargo run                      # TUI shell: header, status line, placeholder panes
$ cargo run -- --version         # smart-review 0.1.0
$ cargo run -- --check           # env checklist, exit code 0/1/2 (stub values OK in M0)
$ cargo run -- --help
```
Inside the TUI: switch modes (`:` command line), open `?` help, press `<leader>` for the action menu, `:theme light|dark`, `:q` / `Ctrl-C` to exit with the terminal intact.

**Work items**

*Repo & tooling*
- [ ] `Cargo.toml`: `edition = "2024"`, `rust-version = "1.88"` (ratatui 0.30.2 is the binding constraint — DEV-1), description/repository, and the lint policy in `[workspace.lints]` with `[lints] workspace = true`: `clippy::pedantic`, `missing_errors_doc`, `missing_panics_doc`, `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`, `dbg_macro`, `unsafe_code = "forbid"` (DEV-8).
- [ ] `rustfmt.toml` (stable-only options), `.gitignore` reviewed (`/target`, `*.log`, `.env*`).
- [ ] `scripts/validate/m0.sh` — formatting, lints, tests, release build, CLI surface, first-run layout and permissions, graceful degradation on an unusable config value, clean failure on unparseable TOML, and proof that nothing is written inside the repository.
- [ ] `README.md`: what it is, prerequisites (`git` ≥ 2.30, `gh` ≥ 2.40), quick start, where state lives.
- [ ] `AGENTS.md` — see the required content below. This is the first file an agent reads; write it before writing code.
- [ ] `.github/workflows/ci.yml` — jobs `fmt`, `clippy`, `test` on `ubuntu-latest` + `macos-latest`, triggered on pushes to `main` and on all PRs, with `concurrency` cancellation and a Rust build cache. `clippy` runs `-D warnings`; `test` runs `--all-features`.

*`AGENTS.md` required content (the hard rules)*
- [ ] Project one-liner + pointers: read `REQUIREMENTS.md` first, then this plan; requirement IDs are the vocabulary for commit messages and PRs.
- [ ] **Never add a dependency without asking.** Use `cargo add` (never hand-edit versions) and state crate, version, purpose, weight and why std/existing deps are not enough (DEP-1, DEV-2).
- [ ] Architecture dependency rule verbatim (ARCH-1): `domain` depends on nothing; `application` depends on `ports` only; `adapters` implement ports; `tui` is a pure function of state + events.
- [ ] `unwrap`/`expect`/`panic!` forbidden outside tests; errors are typed (`thiserror`) and mapped to user-facing messages (ARCH-7, FR-9.1).
- [ ] Every IO/long operation is a cancellable job on the background runtime; the event loop never blocks > 50 ms and never performs IO (ARCH-5, NFR-1.2).
- [ ] Never write inside the user's repository working tree; all state lives under `$SMART_REVIEW_HOME` (FR-8.1).
- [ ] Never mutate remote state (GitHub) without an explicit user confirmation; honor `--dry-run` in every adapter (FR-6.5, NFR-3.4).
- [ ] Never log or display secrets; keys live only in `credentials.toml` (NFR-3.1).
- [ ] Requirement ID in test names/doc comments; update `REQUIREMENTS.md` when behaviour or decisions change (DEV-4, DEV-5).
- [ ] Work milestone order; do not start the next milestone early (DEV-3).
- [ ] Never commit or push unless asked (DEV-6).

*Architecture scaffolding (empty but real)*
- [ ] Module tree per ARCH-6: `main.rs`, `domain/`, `application/`, `ports/`, `adapters/{gh,git,llm,fs,clock}/`, `tui/{app,event,action,update,layout}.rs`, `tui/components/`, `tui/keymap/`, `tui/theme/`.
- [ ] `ARCHITECTURE.md` split out of `REQUIREMENTS.md` §4 as the living design doc (REQUIREMENTS §0.1).
- [ ] Error types per layer with `thiserror`; `anyhow` only at the `main.rs` boundary.

*Terminal lifecycle (the thing that bites later — do it now)*
- [ ] Enter/leave alternate screen + raw mode + mouse capture via a single RAII guard; `Drop` restores.
- [ ] Panic hook that restores the terminal, prints the panic to the log, then re-raises.
- [ ] `SIGINT`/`SIGTERM` handler that restores and exits cleanly (NFR-4.2).
- [ ] `SIGWINCH` → reflow without losing cursor/scroll state (FR-7.8).
- [ ] Below 80×24: single centered "terminal too small" panel (FR-7.8).

*Config, keymap, theme foundations*
- [ ] `ConfigStore` port + `TomlConfigStore` adapter: load `$SMART_REVIEW_HOME/config.toml`, all defaults compiled in, unknown keys tolerated and preserved, precise type errors with file+line, fall back per-key so the app still starts (FR-8.2, FR-8.6).
- [ ] `keybinds.toml` loader with **multi-key sequences**, `<leader>` expansion, `timeoutlen`, per-mode tables, `"x" = "none"` unbinding (FR-8.3).
- [ ] **Action registry**: one static table of `action id → description → default binding(s) → handler`. Help popup, leader menu and command palette are all generated from it (FR-7.3). This registry is the spine of the UI — design it now, not in M1.
- [ ] Path resolution: `$SMART_REVIEW_HOME` else `~/.smart-review`; create dirs `0700`; write a `README.md` explaining the layout (FR-8.1, NFR-3.1).
- [ ] Theme engine: `dark` + `light` built-in themes, every element resolved through the theme (no hard-coded colors), user themes from `themes/*.toml` with `base` inheritance, `:theme` + `<leader>t` live switching (FR-7.7, FR-8.4).
- [ ] `state.toml` read/write for layout + last theme with atomic writes (FR-8.5, minimal subset).

*Modes & commands*
- [ ] Mode state machine `normal|insert|command|search|popup` with the active mode in the status line; `Esc` returns to `normal`, second `Esc` cancels (FR-7.1).
- [ ] Command line with fuzzy completion over the command table and a candidate palette rendered above the prompt; `:q`/`:qa`/`:help`/`:theme [name|reload]`/`:set`/`:keymap [action]`/`:doctor`/`:messages` implemented, unknown commands error inline **and** on the status line (FR-7.3, FR-7.4).
- [ ] Status line with the FR-7.6 segments — mode, focus, repository, pull request, model, draft count — rendering an em dash where the data arrives in a later milestone, plus notifications with levels, deduplication and persistent errors (FR-7.6).
- [ ] Structured logging to `$SMART_REVIEW_HOME/logs/` with rotation and a level from config/`RUST_LOG`; the path and level are shown by `:doctor` (FR-9.2).

*The one job M0 needs*
- [ ] The reducer returns an `Effect` instead of performing IO, and `tui::run` is the only place that acts on it, so `App::render` stays pure and nothing blocks the loop (ARCH-1, NFR-1.2).
- [ ] `:doctor` collects its report on a background thread and delivers it over a channel, with the popup showing a "collecting…" state meanwhile (FR-9.3).

*Tests*
- [ ] First unit tests: config parse + defaults + unknown-key preservation; keymap sequence resolution and ambiguity; theme inheritance.
- [ ] First snapshot tests with ratatui `TestBackend`: shell layout, status line variants, help popup, too-small terminal.
- [ ] A test asserting the terminal guard restores on a simulated panic.

*Validation*
- [ ] `scripts/validate/m0.sh`: `fmt --check`, `clippy -D warnings`, `test`, `build --release`, `--version`, `--help`, `--check` exit code, and a `SMART_REVIEW_HOME=$(mktemp -d)` run asserting the directory layout and permissions.

**FR coverage:** FR-1.2 (CLI surface), FR-7.1–7.4, 7.6–7.8, FR-8.1–8.6 (config/keybinds/themes/state), FR-9.1, FR-9.2.
**Crates to approve:** `ratatui`, `crossterm`, `clap`, `serde`, `toml`, `thiserror`, `anyhow` (see §5).
**Risks:** the action registry and the keymap engine are easy to under-design and painful to retrofit — over-invest here. Deliberately **no** `tokio` in M0: keep the first milestone pure-sync so the terminal lifecycle is provably correct.

---

### M1 — PR browsing and diff reading

**Goal:** the app is useful with no LLM at all: find a PR, understand it, read it with vim motions.

**Runnable artifact**
```
$ cd ~/code/some-repo && smart-review
```
List open PRs (newest first, paginated), type `/` to filter, press `Enter` to open, walk files and hunks with `j/k`, `]c`, `}`, `Tab` between tree and diff, click with the mouse, `R` to refresh, `:doctor` to see environment health. Still no LLM.

**Work items**
- [ ] `ForgePort` + `GhCliForge`: `list_pull_requests`, `get_pull_request`, `list_reviews`, `list_review_comments`, `list_checks` (FR-2.1, FR-2.4).
- [ ] Process runner: argv arrays only, no shell, stdout/stderr separated and size-capped, timeout, exit code + stderr tail in errors, `--repo` always passed (ARCH-3, NFR-3.3).
- [ ] Environment detection + four distinct actionable failures (not a git repo / no GitHub remote / no `gh` / `gh` unauthenticated) (FR-1.1).
- [ ] Remote resolution (`origin` → first GitHub remote → `--remote`/config) and repository identity key (FR-1.1, FR-1.3).
- [ ] PR list UI: rows, draft marker, author, relative time, ±stats, check summary, review decision; explicit pagination with `:load-more` and an honest "showing 50 of ≥137" (FR-2.1).
- [ ] Filter chips → `gh --search` query builder + local fuzzy incremental search (FR-2.2).
- [ ] Disk cache with TTLs, cache-first first paint, revalidate-in-place preserving cursor/scroll, offline indicator (FR-2.3, DEC-14 default).
- [ ] Diff acquisition: remote mode via `gh pr diff --patch` (local mode arrives in M2) (FR-3.2).
- [ ] **Unified diff parser** as a pure, exhaustively unit-tested function: renames, binary, mode-only, submodule, CRLF, `\ No newline at end of file`, missing trailing newline, malformed input (FR-3.2).
- [ ] Diff rendering: file tree with per-file stats and folder grouping, hunk headers, dual line numbers, add/del/context styles, cursor line, **virtualized** (only visible lines laid out) (FR-3.3).
- [ ] Side-by-side toggle at width ≥ 140, unavailable below with an explanation (DEC-4, FR-3.3).
- [ ] Navigation set + hunk/file folding + `:copy-path` via OSC 52 (FR-3.4).
- [ ] Mouse: wheel scroll, click-to-focus, click-to-position in tree/diff (FR-7.5).
- [ ] `:doctor` full checklist incl. gh version/scopes, config parse status, paths, terminal info (FR-9.3).
- [ ] Tests: parser units; snapshot tests for list, diff (unified/split/empty/huge), too-small; application tests with a **fake `ForgePort`**; a fake `gh` executable on `PATH` asserting exact argv.

**FR coverage:** FR-1.1, 1.3, 2.1–2.4, 3.2–3.4, 7.5, 7.7 (on real content), 9.3.
**Crates to approve:** `serde_json`, one date/time crate (`time` preferred over `chrono` — justify at approval), `unicode-width`.
**Risks:** diff virtualization and the parser's edge cases are where time disappears; the fake-`gh` harness must land early so no test needs the network (NFR-5.2).

---

### M2 — Workspace, model configuration, LLM analysis, ordered review

**Goal:** the product's differentiator. Configure an LLM entirely in the TUI, then get a streamed analysis and an architecture-ordered review.

**Runnable artifact**
```
$ cd ~/code/some-repo && smart-review
```
Press `<leader>m` → pick provider → pick model (searchable, with reasoning/cost/context badges) → set thinking → paste the API key into a masked prompt (saved to `credentials.toml`). Open a PR: a worktree is created in the background. Press `<leader>a`: the analysis streams in, the analysis panel fills, and the file tree reorders to domain → application → infra. Press `o` to toggle back to path order. Restart and re-open: cache hit, no network.

**Work items**
- [ ] `WorkspacePort` + `GitCliWorkspace`: `git fetch origin <base> refs/pull/<N>/head`, `git worktree add --detach`, merge-base resolution, `remove`, `prune` (FR-3.1, DEC-1).
- [ ] Local diff mode: `--unified=<n>` runtime-adjustable, `-w` whitespace toggle, `--find-renames`, three-dot revision (FR-3.2).
- [ ] File access at the PR revision via `git show <head>:<path>` (independent of worktree state) + `git ls-files` for the tree (FR-4.6, Appendix A).
- [ ] Workspace lifecycle: reuse for the same head SHA, transparent recreation, `:workspace clean`, stale detection (FR-3.1, DEC-15 default = ask).
- [ ] `ModelCatalogPort` + models.dev adapter: fetch, TTL cache at `cache/models.json`, offline/manual-entry fallback, hidden-if-unmappable providers (FR-4.7, §7.5).
- [ ] Provider mapping (DEC-17): curated native backends + OpenAI-compatible passthrough via catalog `api` base URL.
- [ ] `CredentialsStore`: masked in-TUI entry, atomic `0600` writes, env override + source reporting, `:key clear`, mode verification on load (FR-4.5, §7.4, NFR-3.1).
- [ ] Model picker UI: three steps, searchable, `Esc` backs out, no restart required, status-line indicator, optional presets (FR-4.5).
- [ ] Thinking controls constrained by `reasoning_options` (`toggle` / `effort` / `budget_tokens`), explicit refusal for unmappable options, reasoning token usage displayed, **no trace promised** (FR-4.8, DEC-18).
- [ ] `LlmPort` + `llm`-crate adapter: streaming via tokio, bounded concurrency, cancellation by job id, superseded results dropped (FR-4.4, ARCH-5).
- [ ] Analysis request/response: strict JSON schema + normalize (unknown paths dropped with warning, missing files appended as `unclassified`), one repair retry, raw text viewable on failure (FR-4.1, §7.1).
- [ ] Analysis cache keyed by `(repo, pr, head_sha, provider, model, thinking, prompt_version)`, atomic writes, stale marking on head change (FR-4.3).
- [ ] Context bundle builder: metadata + commits + diff + changed files at head + `AGENTS.md`/`CLAUDE.md`/`README.md`, redaction of `.env*`/ignored/oversize/binary, truncation order, token estimate, `:context` inspector + opt-in notice (FR-4.6).
- [ ] Review plan UI: groups with rationale, recommended vs path order toggle, manual overrides persisted per PR (FR-3.5, FR-4.2, DEC-10 default).
- [ ] Tests: analysis normalization/repair, cache key sensitivity to thinking, context truncation/redaction, cancellation and superseded-job discard, catalog parsing from a committed fixture, picker state machine.

**FR coverage:** FR-3.1, 3.2 (local), 3.5, 4.1–4.8.
**Crates to approve:** `llm` (features `openrouter`, `deepseek`, plus one TLS feature), `tokio`, an HTTP client for the catalog (`reqwest` already in the tree via `llm` — reuse before adding; `ureq` preferred over a second async stack), possibly `secrecy` for key handling.
**Risks:** provider/model heterogeneity is the biggest unknown (DEC-17) — build the passthrough path and one native path first, then add native backends only as needed. `llm` crate gaps (effort levels, no reasoning text) are already accounted for in FR-4.8. Token budgeting without a real tokenizer will be approximate; say so in the UI.

---

### M3 — Chat

**Goal:** ask questions about the PR and get grounded, streamed, persistent answers.

**Runnable artifact:** `Tab` to the Chat pane, ask "does this break the webhook retry contract?", watch the answer stream with file references, press `Esc` mid-answer to cancel, restart the app and find the conversation.

**Work items**
- [ ] Sessions per `(repo, PR)` with `:chat new|list|open|export`, append-only history, persistence under `cache/` (FR-5.1).
- [ ] Streaming UI with cancellation, retry/regenerate, and `Shift-Enter` newline fallbacks (FR-5.2).
- [ ] Grounding: shared context bundle + history trimming, `:context add <path>` so the user can widen context instead of the model guessing, and no tool loop (FR-5.3, DEC-2).
- [ ] Answer rendering: markdown-ish, path references jump to the diff/file, citation distinction between provided context and general knowledge (FR-5.1, FR-5.3).
- [ ] Token/cost accounting per session, optional per-message cost estimate from catalog `cost` (FR-5.4).
- [ ] Retention cap per DEC-9 (default: 50 sessions × 2 MB per PR) with announced pruning (FR-8.5).
- [ ] Tests: history persistence round-trip, trimming, cancellation, prompt assembly snapshots with a fake `LlmPort`.

**FR coverage:** FR-5.1–5.4.
**Crates to approve:** none expected.

---

### M4 — Review publishing

**Goal:** finish the job — leave a real, correctly-formed review on GitHub without ever posting something by accident.

**Runnable artifact:** `c` on a diff line opens the composer; three comments land in the draft panel; `<leader>rr` opens the publish modal showing decision + body + every comment verbatim; confirm; the PR gets **one** review on GitHub; `:draft` is cleared. `--dry-run` prints the exact `gh`/GraphQL calls instead.

**Work items**
- [ ] Draft model + persistence per PR: decision, body, inline comments with side/line/range (FR-6.1, §7.2).
- [ ] Comment composer: line and range (same file, same side, start ≤ end), empty-body rejection, multi-line input, `:edit` via `$EDITOR` with recoverable terminal restore (FR-6.2).
- [ ] Draft markers in the diff gutter; draft panel with remove/clear + confirmation (FR-6.1).
- [ ] `submit_review` on `ForgePort` with two paths behind one method: batched GraphQL pending-review + submit (with inline comments) and `gh pr review` (no inline comments) (FR-6.3, DEC-3).
- [ ] Publish modal, in-flight guard/idempotency, own-PR and "no commits between" error translation, draft preserved on failure, refresh after success (FR-6.3).
- [ ] Existing reviews/comments/threads displayed read-only, reachable from the affected diff line (FR-6.4).
- [ ] `--dry-run` honored by every mutating adapter, printing argv/GraphQL body (FR-6.5).
- [ ] Tests: comment validation, publish-payload construction snapshots, failure-preserves-draft, double-submit prevention, fake-`gh` argv assertions, GraphQL body golden files.

**FR coverage:** FR-6.1–6.5, FR-3.3 (existing discussion overlay).
**Crates to approve:** none expected.
**Risks:** the GraphQL pending-review → submit flow is the least-documented surface here; validate against a sandbox PR behind `SMART_REVIEW_CONTRACT_REPO` and keep the fallback path for bodies without inline comments.

---

### M5 — Polish, docs, release

**Goal:** something you would hand to a colleague.

**Work items**
- [ ] Visual-mode line/range selection for comments (FR-7.1).
- [ ] Thread replies/resolution if DEC-16/backlog is approved; manual review-order override UX finalized.
- [ ] Docs: `README.md`, generated `docs/keymaps.md` (from the action registry) with a test that it stays in sync, `docs/themes.md`, `docs/configuration.md`.
- [ ] `NO_COLOR` support (MAY in FR-7.7) if it does not break the UI.
- [ ] Release: `--version` from Cargo metadata, release profile tuning (`lto`, `strip`), a tag-driven GitHub Actions release job producing Linux + macOS binaries (owner approval for the workflow).
- [ ] Backlog decisions from `REQUIREMENTS.md` §11 as approved: syntax highlighting (DEC-4 revisit), agentic file reading (DEC-2), Windows tier.

---

## 3. Cross-cutting enablers (build once, in M0, use everywhere)

| Enabler | Why it must be early | Where specified |
|---|---|---|
| **Action registry** | Help, leader menu, command palette, keymap validation and generated docs all derive from it. Retrofitting means touching every widget. | FR-7.2, FR-7.3, FR-7.4 |
| **Job framework** (id, progress channel, cancellation, superseded-drop) | Every milestone adds a new long operation; without it the event loop gets blocked once and stays blocked. | ARCH-5, NFR-1.2, NFR-1.4 |
| **Terminal guard** | A single missed restore path corrupts the user's terminal and erodes trust immediately. | FR-9.1, NFR-4.2 |
| **Port fakes** | Makes every application-level requirement testable offline, which is the only way CI stays fast and hermetic. | ARCH-2, NFR-5.2 |
| **Theme token set** | Hard-coded colors creep in within days; a lint/test that renders with an all-default theme keeps it honest. | FR-7.7 |
| **Config loader with unknown-key preservation** | Rewriting a user's config file is data loss; the picker writes config back in M2, so this must be right first. | FR-8.2, FR-8.6 |

---

## 4. Test & fixture strategy

- **No test touches the network.** Adapters are exercised through fakes; the live `gh`/LLM paths are opt-in behind `--features contract-tests` and env vars (`SMART_REVIEW_CONTRACT_REPO`).
- **Fake `gh`**: a small executable script placed first on `PATH` that records argv and replays canned JSON. This gives real coverage of command construction without any auth.
- **Temp git repo fixture**: created by tests with the real `git` binary — a few commits, a branch, a rename, a binary file, a `.gitignore`, an `AGENTS.md`. Used for workspace, diff and context-bundle tests.
- **Fake `LlmPort`**: deterministic streaming (chunks with delays) so cancellation, superseded-drop and partial-render behaviour are testable.
- **Committed fixtures**: a trimmed `models.dev` sample (2 providers × 3 models incl. a reasoning model), sample `gh pr view/list` JSON, sample unified diffs (renames, binary, CRLF, no-newline).
- **Snapshot tests** (`TestBackend`) are the primary UI regression net; snapshots are reviewed like code.
- **Clock is injected** so TTL/cache/staleness tests are deterministic.

---

## 5. Dependency approval ledger

Per DEP-1, nothing below is added until approved. Versions come from `cargo add` at implementation time (DEV-2), never hand-pinned.

| Milestone | Crate | Purpose | Why not std/existing |
|---|---|---|---|
| M0 | `ratatui` | TUI rendering, widgets, `TestBackend` snapshots | Foundational; required by the product definition |
| M0 | `crossterm` | Terminal backend, events, mouse, raw mode | ratatui's default backend |
| M0 | `clap` | CLI parsing, `--help`/`--version` | Hand-rolled parsing for 8 flags is not worth the maintenance |
| M0 | `serde` + `toml` | `config.toml`, `keybinds.toml`, themes, state | No std TOML support |
| M0 | `thiserror` | Typed layered errors (ARCH-7) | Avoids hand-written `Display`/`Error` boilerplate |
| M0 | `anyhow` | Error context at the `main`/CLI boundary only | Small, boundary-only |
| M1 | `serde_json` | `gh --json` parsing, cache payloads | No std JSON support |
| M1 | *date/time crate* (`time` preferred) | Relative timestamps, TTLs, ISO-8601 parsing from `gh` | std has no date arithmetic; `time` is smaller than `chrono` |
| M1 | `unicode-width` | Correct width for CJK/emoji in lists and diffs | `str::len` is bytes, not columns |
| M2 | `llm` (features `openrouter`, `deepseek`, TLS) | Provider abstraction, streaming, reasoning params | Explicitly mandated by the requirements |
| M2 | `tokio` | Async runtime required by `llm`; job/process supervision | `llm` is async-only |
| M2 | HTTP client for the catalog | Fetch `models.dev/api.json` | Reuse `reqwest` (already in the tree via `llm`) if a client is exposed; otherwise `ureq`. **Do not add both.** |
| M2 | `secrecy` *(optional)* | Zeroizing key material, redaction in logs | Reduces the chance of a key leaking through a `Debug` impl |
| M5 | `syntect` / `tree-sitter-*` | Syntax highlighting | **Deferred by DEC-4**; needs a new decision + approval |

---

## 6. Risk register

| Risk | Impact | Mitigation | Milestone |
|---|---|---|---|
| Provider/model heterogeneity (DEC-17) | Analysis silently fails or returns provider-specific errors | Build one native backend + the OpenAI-compatible passthrough first; surface provider errors verbatim; catalog metadata is advisory only | M2 |
| `llm` crate gaps (3 effort levels, no reasoning text) | Users expect a visible thinking trace | FR-4.8 forbids implying a trace; show settings + reasoning token counts; DEC-18 tracks upstream | M2 |
| Diff virtualization done late | Large PRs feel broken, then need a rewrite of the renderer | Treat as a first-class M1 requirement with a 10k-line fixture and a performance assertion | M1 |
| Event loop blocked by a git/gh call | UI freezes; the fix is architectural | Job framework from M0; lint/review rule "no IO in the main loop" in `AGENTS.md` | M0 |
| Worktree sprawl and disk growth | User annoyance, stale code | Reuse by head SHA, `:workspace clean`, `auto_clean_days`, doctor reporting | M2 |
| Config rewrite losing user comments | Data loss, trust | Preserve unknown keys, never rewrite the file except for the specific key being set, back up before write | M0/M2 |
| Publish mistakes | Visible, embarrassing, hard to undo | Confirmation modal, verbatim preview, `--dry-run`, draft preserved on failure, sandbox-repo contract tests | M4 |
| Token cost surprises | Unwanted spend | Pre-send estimate + one-time opt-in per repo, catalog-driven context limits, per-session accounting, ask before re-analyzing a moved head (DEC-15) | M2/M3 |

---

## 7. Suggested work sequencing

- **M0** is deliberately dependency-light: do *not* pull `tokio` yet; prove the terminal lifecycle and the action registry in a synchronous app.
- **M1 before any LLM work.** A reviewer must be able to browse and read diffs with no model configured; that is also the fallback when the LLM is unavailable.
- **M2 splits naturally into three parallelisable tracks** once the ports exist: (a) workspace/git, (b) catalog + credentials + picker, (c) `llm` adapter + context bundle + analysis panel. Track (c) depends on (b) only for the active selection.
- **M3 and M4 are independent** of each other once M2 is done; M4 is the higher-risk one, so consider doing it first if the owner wants a complete review loop sooner.
- **M5** only after M0–M4 exit; it is where deferred decisions are revisited.

---

## 8. Traceability (requirement → milestone)

| Milestone | Requirements covered |
|---|---|
| M0 | FR-1.2, FR-7.1, FR-7.2, FR-7.3, FR-7.4, FR-7.6, FR-7.7, FR-7.8, FR-8.1, FR-8.2, FR-8.3, FR-8.4, FR-8.5 (subset), FR-8.6, FR-9.1, FR-9.2 |
| M1 | FR-1.1, FR-1.3, FR-2.1, FR-2.2, FR-2.3, FR-2.4, FR-3.2, FR-3.3, FR-3.4, FR-7.5, FR-9.3 |
| M2 | FR-3.1, FR-3.5, FR-4.1, FR-4.2, FR-4.3, FR-4.4, FR-4.5, FR-4.6, FR-4.7, FR-4.8 |
| M3 | FR-5.1, FR-5.2, FR-5.3, FR-5.4 |
| M4 | FR-6.1, FR-6.2, FR-6.3, FR-6.4, FR-6.5 |
| M5 | Backlog + NFR polish; nothing in §3 of `REQUIREMENTS.md` may be left unmapped before M5 starts |
