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
- **Every milestone PR ends with a manual test recipe** (owner's request, M3 onwards): environment, numbered steps with what to look for, what to check on disk, and the one or two behaviours no automated check covers. The PR is where the reviewer decides whether to believe the milestone; a diff is not evidence that a stream arrives or a click lands.

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
| **M2a** | Workspace + model configuration | Read a PR from a managed worktree (local diff, context and whitespace toggles), and pick provider/model/thinking + paste a key **in the TUI**, with the choice verified against the provider. | `scripts/validate/m2a.sh` + manual demo | FR-3.1, 3.2 (local), 4.5, 4.7, 4.8 | llm, tokio, reqwest, toml_edit |
| **M2b** | LLM analysis + ordered review | Press `<leader>a` twice: the analysis streams into a panel and the tree reorders to the plan it returned; `o` toggles path order. Restart: cache hit, no network. | `scripts/validate/m2b.sh` (35 checks) + manual demo | FR-3.5, 4.1–4.4, 4.6 | none new |
| **M3** | Chat | A persistent, streaming chat per PR grounded in the context bundle, with `:context` inspection. | `scripts/validate/m3.sh` + manual demo | FR-5.1–5.4 | none new |
| **M4** | Review publishing | Stage inline comments, review the publish modal, submit one batched review to GitHub; `--dry-run` writes the commands it would run to `logs/dry-run.log`. | `scripts/validate/m4.sh` (23 checks) + manual demo against a sandbox PR | FR-6.1–6.5, 3.3 (existing discussion) | none new |
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
- [ ] `.github/workflows/ci.yml` — a single `tests` job on `ubuntu-latest`, triggered on pushes to `main` and on all PRs, with `concurrency` cancellation and a Rust build cache. `clippy` runs `-D warnings`; `test` runs `--all-features`.

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
- [x] `ForgePort` + `GhCliForge`: `list_pull_requests`, `get_pull_request`, `list_reviews`, `list_review_comments`, `list_checks` (FR-2.1, FR-2.4).
- [x] Process runner: argv arrays only, no shell, stdout/stderr separated and size-capped, timeout, exit code + stderr tail in errors, `--repo` always passed (ARCH-3, NFR-3.3).
- [x] Environment detection + four distinct actionable failures (not a git repo / no GitHub remote / no `gh` / `gh` unauthenticated) (FR-1.1).
- [x] Remote resolution (`origin` → first GitHub remote → `--remote`/config) and repository identity key (FR-1.1, FR-1.3).
- [x] PR list UI: rows, draft marker, author, relative time, ±stats, check summary, review decision; explicit pagination with `:load-more` and an honest "showing 50 of ≥137" (FR-2.1).
- [x] Filter chips → `gh --search` query builder + local fuzzy incremental search (FR-2.2).
- [x] Disk cache with TTLs, cache-first first paint, revalidate-in-place preserving cursor/scroll, offline indicator (FR-2.3, DEC-14 default).
- [x] Diff acquisition: remote mode via `gh pr diff --patch` (local mode arrives in M2a) (FR-3.2).
- [x] **Unified diff parser** as a pure, exhaustively unit-tested function: renames, binary, mode-only, submodule, CRLF, `\ No newline at end of file`, missing trailing newline, malformed input (FR-3.2).
- [x] Diff rendering: file tree with per-file stats and folder grouping, hunk headers, dual line numbers, add/del/context styles, cursor line, **virtualized** (only visible lines laid out) (FR-3.3).
- [x] Side-by-side toggle at width ≥ 140, unavailable below with an explanation (DEC-4, FR-3.3).
- [x] Navigation set + hunk/file folding + `:copy-path` via OSC 52 (FR-3.4).
- [x] Mouse: wheel scroll, click-to-focus, click-to-position in tree/diff (FR-7.5).
- [x] `:doctor` full checklist incl. gh version/scopes, config parse status, paths, terminal info (FR-9.3).
- [ ] Tests: parser units; snapshot tests for list, diff (unified/split/empty/huge), too-small; application tests with a **fake `ForgePort`**; a fake `gh` executable on `PATH` asserting exact argv.

**FR coverage:** FR-1.1, 1.3, 2.1–2.4, 3.2–3.4, 7.5, 7.7 (on real content), 9.3.
**Crates approved:** `serde_json`, `chrono` (with `serde`) in place of `time` — the
user's choice, and its `DateTime<Utc>` is the type the fixtures and the cache use —
`unicode-width`, and `signal-hook` for the terminal restore on a signal.
**Risks:** diff virtualization and the parser's edge cases are where time disappears;
the fake-`gh` harness must land early so no test needs the network (NFR-5.2).

**Delivered.** `scripts/validate/m1.sh` drives the whole chain through a fake `gh`
and asserts what the *screen* shows, with a small terminal emulator
(`scripts/validate/screen.py`) reconstructing the final frame from the pty capture,
because the raw stream contains only the cells that changed.

Two things M1 does not do, both stated in the interface rather than hidden:
`<leader>dc`/`<leader>dw` (context lines and whitespace ignoring) need the local
workspace that M2a creates, and they say so when pressed; and the inline comments are
fetched and cached but not yet drawn on the diff, which is M4's publishing flow.

---

### M2a — Workspace and model configuration

**Goal:** everything an analysis needs to exist and be trustworthy, before any money is
spent on one: the PR checked out locally, and a model chosen and verified in the TUI.

**Runnable artifact**
```
$ cd ~/code/some-repo && smart-review
```
Open a PR: a worktree appears in the background and the diff is now read from it, so
`<leader>dc` adds context lines and `<leader>dw` hides whitespace. Press `<leader>m` →
pick provider → pick model (searchable, with reasoning/cost/context badges) → set
thinking → paste the API key into a masked prompt (saved to `credentials.toml`, mode
0600). The choice is checked against the provider before it is kept, and the status line
names the active model.

**Work items**
- [x] `WorkspacePort` + `GitCliWorkspace`: `git fetch origin <base> refs/pull/<N>/head`, `git worktree add --detach`, merge-base resolution, `remove`, `prune` (FR-3.1, DEC-1).
- [x] Local diff mode: `--unified=<n>` runtime-adjustable, `-w` whitespace toggle, `--find-renames`, three-dot revision (FR-3.2).
- [x] `--pr N` actually opens that pull request: M1 accepted the flag, showed it and never acted on it.
- [x] File access at the PR revision via `git show <head>:<path>` (independent of worktree state) + `git ls-files` for the tree (FR-4.6, Appendix A).
- [x] Workspace lifecycle: reuse for the same head SHA, transparent recreation, `:workspace clean`, stale detection (FR-3.1, DEC-15 default = ask).
- [x] `ModelCatalogPort` + models.dev adapter: fetch, TTL cache at `cache/models.json`, offline/manual-entry fallback, hidden-if-unmappable providers (FR-4.7, §7.5).
- [x] Provider mapping (DEC-17): curated native backends + OpenAI-compatible passthrough via catalog `api` base URL.
- [x] `CredentialsStore`: masked in-TUI entry, atomic `0600` writes, env override + source reporting, `:key clear`, mode verification on load (FR-4.5, §7.4, NFR-3.1).
- [x] Model picker UI: three steps, searchable, `Esc` backs out, no restart required, status-line indicator, optional presets (FR-4.5).
- [x] Thinking controls constrained by `reasoning_options` (`toggle` / `effort` / `budget_tokens`), explicit refusal for unmappable options, reasoning token usage displayed, **no trace promised** (FR-4.8, DEC-18).
- [x] `[llm.active]` written back with `toml_edit`, preserving the user's comments and formatting (FR-8.6, DEC-19).
- [x] `LlmPort` + `llm`-crate adapter: chat with streaming and usage, bounded concurrency, cancellation by job id, superseded results dropped (FR-4.4, ARCH-5), plus the connection check the picker runs.
- [x] Tests: worktree lifecycle against a real fixture repo, diff flags, catalog parsing from a committed fixture, credential file mode and env precedence, picker state machine, thinking-option mapping. `scripts/validate/m2a.sh` covers the same ground end to end, with a local catalog server and a real git repository.

**FR coverage:** FR-3.1, 3.2 (local), 4.5, 4.7, 4.8.
**Outstanding from this milestone:** the optional `[llm.presets]` convenience (`:model save` / `:model use`) and the manual-entry fallback for a catalog that cannot be fetched at all (FR-4.7's MAY). Both are recorded in §6.
**Crates (approved):** `llm` (features `openrouter`, `deepseek`, TLS), `tokio`, `reqwest`, `toml_edit`.
**Risks:** provider/model heterogeneity is the biggest unknown (DEC-17) — the passthrough path covers most of the catalog, and a provider that cannot be mapped is hidden rather than offered. `llm` crate gaps (effort levels, no reasoning text) are accounted for in FR-4.8. Worktree creation is the first thing here that writes outside `SMART_REVIEW_HOME`'s cache, so its failure modes get their own tests.

---

### M2b — LLM analysis and ordered review

**Goal:** the product's differentiator.

**Runnable artifact:** on the PR from M2a, press `<leader>a`: the first press shows what would be sent and waits, the second streams the answer into a panel and reorders the file tree to the plan the analysis returned (domain → tests → unclassified in the validator's fixture). Press `o` to read the same files in path order. Restart and re-open: cache hit, no network.

**Work items**
- [x] Analysis request/response: strict JSON schema + normalize (unknown paths dropped with warning, missing files appended as `unclassified`), one repair retry, raw text viewable on failure (FR-4.1, §7.1).
- [x] Analysis cache keyed by `(repo, pr, head_sha, provider, model, thinking, prompt_version)`, atomic writes, stale marking on head change (FR-4.3, DEC-15).
- [x] Context bundle builder: metadata + commits + diff + changed files at head + `AGENTS.md`/`CLAUDE.md`/`README.md`, redaction of `.env*`/ignored/oversize/binary, truncation order, token estimate, `:context` inspector + opt-in notice (FR-4.6).
- [x] Review plan UI: groups with rationale, recommended vs path order toggle, manual overrides persisted per PR (FR-3.5, FR-4.2, DEC-10 default).
- [x] Tests: analysis normalization/repair, cache key sensitivity to thinking, context truncation/redaction, cancellation and superseded-job discard.
- [x] `scripts/validate/m2b.sh`: 35 checks, driven end to end against a scripted OpenAI-compatible provider (`scripts/validate/fake_llm.py`), including what the app actually sent.

**Notes on what M2b decided, where it is not obvious from the requirements**
- **`.gitignore` is enforced by construction.** Only paths that exist in the head revision are considered for the bundle (`git ls-tree`), so an ignored file is not a candidate in the first place; a `.env` that somebody committed by accident is caught by the secret denylist on top of that. The validator asserts on the wire that such a file never reaches the provider.
- **The truncation order is implemented for the first two steps.** Per-file elision and diff context reduction are done and tested; "oldest chat turns dropped" waits for chat (M3) because there are no turns yet.
- **The corrections and the repair flag are stored with the document.** They describe the answer rather than the run, so a cache hit reports them exactly as the run that wrote them did (FR-4.1).
- **The passthrough route was wrong, and is fixed here.** `LLMBackend::OpenAI` speaks the Responses API (`/responses`), which most OpenAI-compatible providers do not implement; the passthrough now builds the crate's generic compatible provider, whose endpoint is `/chat/completions`. Appendix B records it. This was found by the validator, not by review.
- **The analysis panel does not scroll yet.** It is capped at the terminal height and shows its key hints and its notices first, with `:plan` and `:context` as the escape hatches for what does not fit. Chat (M3) is where the panel gets real scrolling, and the panel was ordered so that the parts with another home come last.

**FR coverage:** FR-3.5, 4.1–4.4, 4.6.
**Crates to approve:** none new.
**Risks:** token budgeting without a real tokenizer is approximate (the UI says `~`, and Appendix B.3 fixes the approximation); a model that answers with prose instead of JSON is a normal outcome, not an exception, so the repair path and the raw-text view are features rather than fallbacks.

---

### M3 — Chat

**Goal:** ask questions about the PR and get grounded, streamed, persistent answers.

**Runnable artifact:** `Tab` to the Chat pane (or `<leader>c`), ask "does this break the webhook retry contract?", watch the answer stream with its file references, press `Esc` mid-answer to stop it, restart the app and find the conversation.

**Work items**
- [x] Sessions per `(repo, PR)` with `:chat new|list|open|export`, append-only history, persistence under `cache/` (FR-5.1).
- [x] Streaming UI with cancellation, retry/regenerate, and `Shift-Enter` newline fallbacks (FR-5.2).
- [x] Grounding: shared context bundle + history trimming, `:context add <path>` so the user can widen context instead of the model guessing, and no tool loop (FR-5.3, DEC-2).
- [x] Answer rendering: markdown-ish, path references jump to the diff/file, citation distinction between provided context and general knowledge (FR-5.1, FR-5.3).
- [x] Token/cost accounting per session, optional per-message cost estimate from catalog `cost` (FR-5.4).
- [x] Retention cap per DEC-9 (default: 50 sessions × 2 MB per PR) with announced pruning (FR-8.5).
- [x] Tests: history persistence round-trip, trimming, cancellation, prompt assembly snapshots with a fake `LlmPort`.
- [x] `scripts/validate/m3.sh`: 59 checks, driving the chat against a scripted provider and reading the wire, including a provider the pinned crate cannot stream for and one that never answers.

**Notes on what M3 decided, where it is not obvious from the requirements**
- **The context goes in the system prompt, not in a message.** That is what makes FR-4.6's third truncation step safe: the oldest turns can be dropped without ever being able to drop the subject of the conversation, and no message has to be flagged as the important one. The cost is that the bundle is re-sent with every turn, which is what "the model sees exactly what `:context` reports" costs.
- **The conversation starts when the question does**, not when the answer arrives: the question is visible while the model is thinking and is on disk before its answer exists, so a crash between the two loses nothing the user typed.
- **A question whose answer was stopped keeps its question** when the history is replayed, and the half-answer is not replayed. The natural follow-up ("finish the thought") is otherwise a question about nothing.
- **`Tab` is a pane move in insert mode too** — the compose box would otherwise be a room with no door. `Esc` clears a pending confirmation first, then leaves the pane.
- **`:context add <path>` refuses** a path the pull request does not have, and refuses anything that looks like a secret outright: a bundle that silently leaves out what the user asked for by name is the one thing FR-4.6 exists to prevent. The added files live in `state.toml` per pull request — a preference about the pull request, not about one conversation.
- **Transcripts are written to `exports/`**, not to `cache/`: a transcript the user asked for is not disposable (FR-8.5).
- **A provider the crate cannot stream for still answers.** Only three of the crate's backends implement structured streaming and only three more implement the text-only one; DeepSeek, Groq, Mistral and OpenRouter implement neither, and asking them for it produced an error the interface then dropped. The adapter walks down four ways of asking (structured stream → string stream → structured stream through the passthrough → one un-streamed request) and stops the moment text has arrived, because a partial answer has already been paid for. Appendix B has the measurements.
- **A failed job is never invisible.** Every job slot a pane can hold belongs in `is_current_job`; leaving one out does not make its failure harmless, it makes it look like a pane that is still thinking. `scripts/validate/m3.sh` now checks the failure *on screen*, for the chat and for the analysis, because that is the shape the bug took: a real provider, a real error, and a pane that said "asking" forever.
- **The crate does not ask for streaming usage on the passthrough route** (FR-5.4's tokens and per-session cost would be permanently empty): only its native `OpenAI` backend sets `stream_options.include_usage`. The passthrough provider's config now sets `SUPPORT_STREAM_OPTIONS`, and the validator asserts the field is on the wire.

**FR coverage:** FR-5.1–5.4.
**Crates to approve:** none new.
**Risks:** a long conversation is re-sent every turn, so cost grows with turns rather than with the question (the history is capped at a quarter of the context budget, and the per-answer cost is on screen). Cancellation depends on the provider stopping when the connection closes; a provider that keeps generating is billed for what it generated before the app dropped the stream.

---

### M4 — Review publishing

**Goal:** finish the job — leave a real, correctly-formed review on GitHub without ever posting something by accident.

**Runnable artifact:** `c` on a diff line opens the composer; two comments land in the draft panel (`<leader>rd`); `<leader>rr` opens the publish modal showing the decision, the body and every comment verbatim; `Enter` twice posts **one** review to GitHub; the draft is cleared and the status line says what was sent. `--dry-run` records the exact `gh` calls in `logs/dry-run.log` instead.

**Work items**
- [x] Draft model + persistence per PR: decision, body, inline comments with side/line/range (FR-6.1, §7.2). Kept at `<home>/drafts/…`, not under `cache/`.
- [x] Comment composer: line and range (same file, same side, start ≤ end), empty-body rejection, multi-line input (FR-6.2).
- [x] Draft markers in the diff gutter; draft panel with remove, `:draft clear` and a confirmation (FR-6.1).
- [x] `submit_review` on `ForgePort` with two paths behind one method: one batched request with the comments, and `gh pr review` when there are none (FR-6.3, DEC-3).
- [x] Publish modal, in-flight guard/idempotency, own-PR and bad-anchor translation, draft preserved on failure, refresh after success (FR-6.3).
- [x] Existing reviews/comments/threads displayed read-only, under the affected diff line (FR-6.4).
- [x] `--dry-run` honoured by every mutating adapter: the gate is a property of the process runner, so `:workspace clean` is covered by the same promise (FR-6.5).
- [x] Tests: comment validation, payload construction, failure-preserves-draft, double-submit prevention, fake-`gh` argv and payload assertions.
- [x] `scripts/validate/m4.sh`: 23 checks over the whole flow, including that the batched request is the *only* mutating call, that a dry run reaches nothing, and that an existing thread is drawn.

**Notes on what M4 decided, where it is not obvious from the requirements**
- **One REST call, not two GraphQL ones.** DEC-3 chose a batched review and recorded `gh api graphql` as the route; the route changed at the owner's suggestion once the shape was clear. `POST /repos/{owner}/{repo}/pulls/{N}/reviews` takes the decision, the body and every comment in **one** request, so "one review" cannot half-happen — and the payload is built by `serde_json` and passed as a file, so nothing ever escapes user prose by hand. A list of GraphQL input objects cannot be passed as a variable over argv, which is what the two-call route would have required. DEC-3's *decision* (one batched review, inline comments in v1) is unchanged; only its recorded route is, and REQUIREMENTS says so.
- **`gh pr review` remains for the one case it can do**: a verdict with nothing anchored to a line.
- **A dry run is a property of the runner, not of a feature.** A call says whether it mutates; the runner records mutating calls instead of running them. A forgotten mark fails *visibly* (the call runs during a dry run) rather than invisibly (a real call that silently does not happen). Reads still run: a dry run that could not read would have nothing to describe. `git fetch` is deliberately not marked: it writes only into this application's own ref namespace, and holding it back would mean the app could not show a diff at all.
- **The draft is the UI's document and the service keeps none of it.** `Drafts` is handed a document to write or send; the reducer owns the draft and sets a `dirty` flag, and the loop writes. This is what makes the key that opens the composer instant whatever the disk is doing, and it is why `:draft export`, `:draft list` and even the `:workspace` listing became effects.
- **Enter stages a comment where it sends a question in the chat pane.** The two compose boxes look alike; the difference is what the key costs. Staging is local and reversible, sending a question is paid for. A modifier adds a line in both, so the muscle memory transfers.
- **Publishing takes two Enters.** The first arms, the second sends: a modal that both shows and does it on one keypress makes reading it optional, and this is the only surface that can put words on the internet.
- **A failed publish keeps the draft and says so in the modal**, not only in a notice that expires. That is the moment the draft matters most, and the moment a modal that closed itself would leave the user unsure whether anything had been posted.
- **A comment whose line is no longer in the diff is not drawn.** Attaching an outdated comment to the nearest line would look like a comment about *that* line — a worse lie than an omission. The tab's count still includes it.
- **The failure message names the most informative error.** The same rule M3 arrived at for providers applies to the forge: GitHub's own sentence, translated (`"Can not approve your own pull request"` → "this pull request is yours, so GitHub will not let you approve it").
- **A job slot needs three edits, and only two of them fail loudly.** `record_job` had no arm for publishing, so the job id was never stored and *every* publish completion — success and failure — was dropped by the id check. The same shape as M3's invisible provider failure, in the same place. Worth stating plainly: this class of bug is found by driving the real flow, not by reading the code.

**FR coverage:** FR-6.1–6.5, FR-3.3 (existing discussion, drawn inline).
**Crates to approve:** none new.
**Risks:** the anchor is a line number, so a force-push between writing and publishing can move it; the draft records the commit it was written against and the panel warns when it differs, but re-anchoring is manual. Replying to a thread is M5 (DEC-16).

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
| **Config loader with unknown-key preservation** | Rewriting a user's config file is data loss; the picker writes config back in M2a, so this must be right first. | FR-8.2, FR-8.6 |

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
| M1 | `chrono` (features `serde`) | Relative timestamps, TTLs, ISO-8601 parsing from `gh` | std has no date arithmetic; **user chose `chrono` over `time`** |
| M1 | `unicode-width` | Correct width for CJK/emoji in lists and diffs | `str::len` is bytes, not columns |
| M2a | `llm` (features `openrouter`, `deepseek`, TLS) | Provider abstraction, streaming, reasoning params | Explicitly mandated by the requirements |
| M2a | `tokio` | Async runtime required by `llm`; job/process supervision | `llm` is async-only |
| M2a | `reqwest` | Catalog fetch, and the HTTP stack `llm` already pulls in | **User chose `reqwest` directly over `ureq`**, so one HTTP implementation is compiled and there is no second stack to keep current |
| M2a | `toml_edit` | Write `[llm.active]` back without destroying comments (DEC-19) | `toml` 1.x cannot preserve comments (verified at M0) |
| — | `secrecy` | Zeroizing key material | **Not added.** Rejected in favour of keeping the only copy of a key in a `String` that is never `Debug`-printed, never logged, and cleared on drop of the picker state (see `credentials.rs`); revisit if that proves hard to hold to. |
| M5 | `syntect` / `tree-sitter-*` | Syntax highlighting | **Deferred by DEC-4**; needs a new decision + approval |

---

## 6. Risk register

| Risk | Impact | Mitigation | Milestone |
|---|---|---|---|
| Provider/model heterogeneity (DEC-17) | Analysis silently fails or returns provider-specific errors | Build one native backend + the OpenAI-compatible passthrough first; surface provider errors verbatim; catalog metadata is advisory only | M2a |
| `llm` crate gaps (3 effort levels, no reasoning text) | Users expect a visible thinking trace | FR-4.8 forbids implying a trace; show settings + reasoning token counts; DEC-18 tracks upstream | M2a |
| Diff virtualization done late | Large PRs feel broken, then need a rewrite of the renderer | Treat as a first-class M1 requirement with a 10k-line fixture and a performance assertion | M1 |
| Event loop blocked by a git/gh call | UI freezes; the fix is architectural | Job framework from M0; lint/review rule "no IO in the main loop" in `AGENTS.md` | M0 |
| Worktree sprawl and disk growth | User annoyance, stale code | Reuse by head SHA, `:workspace clean`, `auto_clean_days`, doctor reporting | M2a |
| Config rewrite losing user comments | Data loss, trust | Preserve unknown keys, never rewrite the file except for the specific key being set, back up before write | M0/M2a |
| Publish mistakes | Visible, embarrassing, hard to undo | Confirmation modal, verbatim preview, `--dry-run`, draft preserved on failure, sandbox-repo contract tests | M4 |
| Token cost surprises | Unwanted spend | Pre-send estimate + one-time opt-in per repo, catalog-driven context limits, per-session accounting, ask before re-analyzing a moved head (DEC-15) | M2b/M3 |

---

## 7. Suggested work sequencing

- **M0** is deliberately dependency-light: do *not* pull `tokio` yet; prove the terminal lifecycle and the action registry in a synchronous app.
- **M1 before any LLM work.** A reviewer must be able to browse and read diffs with no model configured; that is also the fallback when the LLM is unavailable.
- **M2a carries two parallelisable tracks** once the ports exist: (a) workspace/git + local diff, (b) catalog + credentials + picker + `llm` adapter. Track (b) depends on (a) only for the diff source. M2b then builds the context bundle and the analysis panel on both.
- **M3 and M4 are independent** of each other once M2b is done; M4 is the higher-risk one, so consider doing it first if the owner wants a complete review loop sooner.
- **M5** only after M0–M4 exit; it is where deferred decisions are revisited.

---

## 8. Traceability (requirement → milestone)

| Milestone | Requirements covered |
|---|---|
| M0 | FR-1.2, FR-7.1, FR-7.2, FR-7.3, FR-7.4, FR-7.6, FR-7.7, FR-7.8, FR-8.1, FR-8.2, FR-8.3, FR-8.4, FR-8.5 (subset), FR-8.6, FR-9.1, FR-9.2 |
| M1 | FR-1.1, FR-1.3, FR-2.1, FR-2.2, FR-2.3, FR-2.4, FR-3.2, FR-3.3, FR-3.4, FR-7.5, FR-9.3 |
| M2a | FR-3.1, FR-3.2 (local), FR-4.5, FR-4.7, FR-4.8 |
| M2b | FR-3.5, FR-4.1, FR-4.2, FR-4.3, FR-4.4, FR-4.6 |
| M3 | FR-5.1, FR-5.2, FR-5.3, FR-5.4 |
| M4 | FR-6.1, FR-6.2, FR-6.3, FR-6.4, FR-6.5 |
| M5 | Backlog + NFR polish; nothing in §3 of `REQUIREMENTS.md` may be left unmapped before M5 starts |
