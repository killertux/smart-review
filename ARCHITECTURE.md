# Architecture

This document describes the current implementation. Product behavior is in
[`docs/product.md`](docs/product.md), contributor constraints are in
[`AGENTS.md`](AGENTS.md), and historical IDs are indexed in
[`docs/legacy-ids.md`](docs/legacy-ids.md).

## Dependency rule

```text
tui  ──▶  application  ──▶  ports  ◀──  adapters
  │             │             │
  └─────────────┴─────────────┴────▶ domain/config/state types
```

| Layer | Responsibility | Boundary |
|---|---|---|
| `domain` | Pure PR, diff, context, plan, chat, draft, mutation, and model invariants | No terminal, process, filesystem, HTTP, or other project layer |
| `ports` | Traits and DTOs for forge, workspace, LLM, storage, time, and cancellation | No adapter implementation details |
| `application` | Stateless use cases that orchestrate ports | No Ratatui/Crossterm and no direct process or filesystem IO |
| `adapters` | `gh`, Git, LLM, HTTP, catalog, process, and file-backed port implementations | Never imported by `domain` or `application` |
| `tui` | State, reducer, rendering, event loop, effects, and background-job routing | Reducer and render paths perform no IO |

The boundaries are review-enforced inside one crate. Adding a second crate is not a
substitute for following the dependency direction.

## Module map

```text
src/
  main.rs, cli.rs       process entry and command-line surface
  bootstrap.rs          config/state loading and adapter assembly
  config.rs, state.rs   typed configuration and small persisted state
  paths.rs, logging.rs  private home layout and redacted diagnostics
  domain/               pure identities and invariants
  ports/                external capability traits
  application/          environment, PR, context, analysis, chat, draft, post use cases
  adapters/
    gh/                 GitHub CLI reads and mutations
    git/                source detection and app-owned workspaces
    llm.rs              provider routing, streaming, and usage
    *_store.rs          durable chats, drafts, and mutation records
    cache.rs            disposable forge cache
    analysis_cache.rs   keyed analyses plus durable review-plan state
    process.rs          bounded argv-based child execution
  tui/
    app.rs              owned UI/session state and completion reducer
    syntax.rs           bounded Tree-sitter parsing into semantic diff spans
    update.rs           decoded action dispatch
    jobs.rs             slots, worker queue, cancellation, effect/job mapping
    components/         pure Ratatui rendering
    test_support.rs     deterministic action/job/frame scenarios
```

`Startup::load` is the composition root. It resolves CLI/config precedence, creates
the private home layout, migrates legacy durable documents, and constructs adapters.

## Identity and state ownership

`RepoId` is normalized as `host/owner/name`; repository plus PR number identifies
drafts, chats, plans, workspaces, and mutation journals. Revision-sensitive values
also carry head SHA, base SHA, context fingerprint, model settings, and schema/prompt
versions as applicable. A basename alone is never accepted as source identity.

One event-loop thread owns `App`. Opening or refreshing a PR advances a
`ReviewSession`; completions carry job and session identities. A completion is applied
only if it still belongs to the active slot/session/revision. State is not shared as
`Arc<Mutex<App>>`.

## Action, job, and frame flow

```text
terminal event → keymap/action → App reducer → Effect
                                      │
                                      ▼
                              JobRunner slot/queue
                                      │
                    application use case → port → adapter
                                      │
                                      ▼
                progress/completion + job id → App reducer → dirty frame
```

The loop drains bounded progress and completions before input, follows completion
effects, and draws at most once per reduction pass. Active work caps polling at 32 ms.
At most four jobs run concurrently. Replacing work in a slot cancels it and stale
results are discarded; state, plan, and draft saves are serialized rather than
superseded.

Patch workers also parse supported old/new hunk streams with compiled Tree-sitter
grammars and retain only semantic byte ranges in `DiffView`. Theme resolution and span
clipping remain pure render operations; parsing never runs during a frame. Unsupported,
failed, cancelled, or over-budget highlighting falls back to the existing plain diff.

Cancellation is cooperative at the port boundary. Owned `git`/`gh` process groups are
killed within their polling interval, and stalled LLM awaits are abandoned. Cancellation
cannot prove that a remote mutation did not reach GitHub after dispatch; that uncertainty
is represented durably instead.

## Save and mutation flows

Local edits update reducer state immediately and emit a persistence effect. A worker
writes an immutable snapshot with a revision, using a synchronized temporary sibling
and atomic rename. The acknowledgement clears dirty state only if it matches the latest
revision. Chat, plan, draft, and mutation stores use per-document advisory locks to
detect or serialize concurrent writers.

Remote writes have an additional protocol:

```text
preview snapshot → durable Queued record → Dispatching → adapter request
                                             ├─ Succeeded
                                             ├─ Rejected
                                             ├─ Simulated (--dry-run)
                                             └─ OutcomeUnknown
```

`Dispatching` and `OutcomeUnknown` block automatic replay. The local operation ID is a
journal key, not a GitHub idempotency key. A lost response therefore remains unknown
until the user checks GitHub; the app never claims exactly-once remote delivery.

## IO and storage boundaries

All app-owned files are below `$SMART_REVIEW_HOME`. `cache/` and cached model/analysis
responses are disposable. `state.toml`, `chats/`, `drafts/`, `reviews/`, exports,
configuration, credentials, and app-owned Git data are durable. Durable writes are
atomic; directories and secret/content-bearing files are private on Unix.

Git fetches, refs, object storage, and worktree metadata live in an app-owned bare
repository. The source clone is detection input only. External commands always receive
an executable plus argv array; user input is never interpolated into a shell command.
Mutating commands respect `--dry-run`.

The `tui` event loop owns the terminal and delegates external work to `JobRunner`.
`TerminalGuard` restores raw mode, alternate screen, cursor, and mouse state on normal
exit, errors, panic, `SIGHUP`, and `SIGTERM`; `Ctrl-C` is handled as a raw-mode key.

## Recovery and migrations

Missing optional state uses defaults. Corrupt disposable analyses are misses; corrupt
forge cache entries identify the file to delete. Chat indexes are rebuilt from session
documents. Legacy chats and durable plan state formerly under `cache/` are validated and
copied before migration markers are written; source files remain for interrupted-migration
recovery. Newer unsupported document versions are rejected without rewriting them.

## Extension points

- Add a use case in `application` when orchestration is independent of UI.
- Add a port only for an external boundary with a real fake or imminent second adapter.
- Add an adapter without exposing its process, HTTP, or file details inward.
- Add a tab by extending the tab/action registry, reducer state, layout/hit testing, and
  pure component rendering; cover routing and a `TestBackend` frame.
- Add a job with a resource-specific slot, cancellation behavior, stale-result identity,
  and bounded progress before wiring its effect.

Do not perform IO from rendering/reducers, import adapters into application/domain, or
weaken repository isolation to make an integration easier.
