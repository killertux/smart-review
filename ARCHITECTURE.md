# Architecture

Companion to [`REQUIREMENTS.md`](REQUIREMENTS.md) §4, which states the constraints
this document elaborates on. Where they disagree, the requirements win.

## 1. Layers

```
tui  ──▶  application  ──▶  ports  ◀──  adapters
                              │
                              └── domain (depended on by application)
```

| Layer | Contains | May depend on | Must not |
|---|---|---|---|
| `domain` | Pure types and invariants: pull requests, diffs, hunks, drafts, analyses | `std`, `serde`, `thiserror` | Anything else in the crate; terminals; processes |
| `ports` | Traits and DTOs describing what the outside world must provide | `domain`, `config`, `state`, `error` | Implementation details of any adapter |
| `application` | Use cases that orchestrate ports | `domain`, `ports` | `ratatui`, `crossterm`, `tokio::process` |
| `adapters` | Implementations of the ports: `gh`, `git`, `llm`, filesystem, clock | `ports`, `domain` | Being imported by `application` or `domain` |
| `tui` | Rendering, input, actions, themes, keybindings | `application`, `ports` (types only), `domain` | Performing IO; calling an adapter directly |

The rule is enforced by review, not by the compiler: a dependency in the wrong
direction is a bug even when it builds.

## 2. Module map (as implemented in M0)

```
src/
  main.rs            entry point: parse, bootstrap, dispatch to doctor or TUI
  lib.rs             module wiring and the crate-level lint configuration
  cli.rs             the clap surface (FR-1.2)
  error.rs           the top-level error type and its exit-code mapping
  paths.rs           $SMART_REVIEW_HOME layout, permissions, path shortening
  config.rs          typed configuration + the preserved TOML document (FR-8.6)
  state.rs           small persisted state (FR-8.5)
  logging.rs         rotated file logging (FR-9.2)
  doctor.rs          environment checks shared by --check and :doctor (FR-9.3)
  bootstrap.rs       CLI + file + state precedence, producing `Startup`
  domain/            (empty) arrives with M1/M2
  application/       (empty) arrives with M1/M2
  ports/             Clock, ConfigStore, StateStore
  adapters/
    fs.rs            atomic writes, TOML config and state stores
    clock.rs         system clock
    gh.rs            (declared) M1
    git.rs           (declared) M2
    llm.rs           (declared) M2
  tui/
    mod.rs           the event loop
    app.rs           application state and the reducer
    event.rs         façade over crossterm's event types
    action.rs        the action registry (the spine of help, leader and keymaps)
    update.rs        dispatch of actions and `:` commands
    jobs.rs          the only part of the UI that spawns work
    keymap/          the keybinding engine (FR-7.2)
    theme/           the theme engine (FR-7.7)
    layout.rs        rectangles and the minimum terminal size
    terminal.rs      the RAII terminal guard and the panic hook
    components/      one renderer per screen element
    test_support.rs  (test-only) buffer-to-text helper for snapshots
```

## 3. Ports

Implemented so far:

| Port | Adapter | Notes |
|---|---|---|
| `Clock` | `SystemClock` | Injected so cache lifetimes and relative timestamps are testable. The event loop owns it and hands the reducer a timestamp. |
| `ConfigStore` | `TomlConfigStore` | Reading only; writing arrives with M2 and DEC-19 |
| `StateStore` | `TomlStateStore`, plus an in-memory fake in tests | Atomic writes; the fake proves the port is a real seam |

Arriving with the milestone that needs them: `ForgePort` (M1), `WorkspacePort`
(M2), `ModelCatalogPort` (M2), `CredentialsStore` (M2), `LlmPort` (M2).
Application-layer fakes (a fake forge and a fake LLM) arrive with the use cases in
M1 and M2.

The rule of thumb: a port exists when there is a second implementation (a test
fake) or a real alternative. Empty abstractions are not written "for later".

`WorkspacePort` owns an app-private bare Git object store and its detached managed
worktrees; it may read the source clone only to resolve the selected remote URL. It
never fetches, updates refs, or registers a worktree in that source clone. A
per-repository interprocess lock makes fetching refs, resolving the revision identity,
and replacing or removing its worktrees one lifecycle operation (IR-13).

`WorkspacePort` also evaluates repository ignore rules for context assembly. That
repository-aware check stays outside `domain`; the resulting path decisions are passed
to the pure bundle builder and govern full and reduced diff hunks, full file bodies and
later user additions. The adapter evaluates old paths against base-revision ignore rules
and new paths against head-revision rules (IR-01).

## 4. Data flow

```
crossterm event ─▶ App::on_key ─▶ Keymap::resolve ─▶ update::dispatch ─▶ App state
                            │                              │
                            │                              ▼
                            │                            Effect
                            │                              │
                    ratatui Frame ◀──────── tui::run applies it (IO)
```

1. `App::on_key` normalises the key press (uppercase implies Shift, `BackTab` is
   `Shift+Tab`) and appends it to the pending sequence.
2. `Keymap::resolve` reports `Match`, `Ambiguous`, `Prefix` or `None`. `Ambiguous`
   starts a timer; when it expires, `App::on_timeout` fires the shorter binding.
   The leader menu is a binding whose action returns `Effect::KeepPending`, which
   keeps the sequence alive so the next key can complete it.
3. `update::dispatch` mutates state and returns an [`Effect`] — it performs no IO
   on behalf of the *world*: no process is spawned, no network is touched, and every
   such job runs on a worker thread (`tui/jobs.rs`) whose result comes back over a
   channel. `tui::run` is the only place that turns an effect into work: persisting
   `state.toml`, applying a terminal change, or starting a job.
   **The one documented exception** is reading the user's own configuration files:
   `:theme`, `:set`, `:keymap` and opening the theme picker read small local files
   (`theme.toml`, `keybinds.toml`) from `<home>`, bounded by the number of files the
   user has written. That is deliberate — the data is needed to answer the key that
   was just pressed, it is local and tiny, and putting it behind a job would make a
   theme change flicker. Everything that could block for tens of milliseconds goes
   through a job.
4. `App::render` reads state and draws. It never reads a file, spawns a process,
   or blocks — anything it needs from disk (the theme list, for instance) was
   captured when the relevant action ran.
5. The registry in `action.rs` is the only place that knows which action ids
   exist, and `update.rs` is the only place that knows what they mean. A default
   binding naming an action outside the registry is a startup warning; a registry
   entry with no dispatch arm falls through to the catch-all, which a test
   catches.

## 5. Concurrency

M0 is deliberately synchronous except for one job, so the loop is a plain
`poll`/`read`/`draw`/`apply-effect` cycle. The doctor probe (`git --version`,
`gh auth status`) already runs on a background thread and reports back over a
channel, because running it inline would block the loop for seconds — the first
instance of the pattern everything else will use.

From M2 the shape is fixed by `REQUIREMENTS.md` (ARCH-5):

- the main thread owns the terminal and drains a single event channel;
- a tokio runtime runs LLM streaming, and blocking git/`gh` work runs on a bounded
  blocking pool (max 4 process jobs);
- every long operation is a job with an id, progress channel and cancellation
  handle; results carry their job id and are dropped when superseded;
- state is mutated only on the main thread, in response to an event.

## 6. Terminal lifecycle

`tui::terminal::TerminalGuard` is the only thing that touches raw mode, the
alternate screen or mouse capture:

- `enter(mouse)` installs a panic hook, enables raw mode, enters the alternate
  screen, hides the cursor, optionally captures the mouse;
- `restore()` is idempotent — the guard's `Drop` and the panic hook both call it,
  and whichever runs second does nothing;
- the panic hook logs the panic, restores the terminal, then chains to the
  previous hook so the message is still printed, on a usable terminal.

Nothing else in the codebase may call `enable_raw_mode` or `execute!` with
terminal control sequences.

## 7. Errors and exit codes

| Exit code | Meaning |
|---|---|
| 0 | Ready (or a normal interactive exit) |
| 1 | Degraded: it runs, but something is missing |
| 2 | Unusable: a fatal error, or a broken configuration |

Layered `thiserror` enums carry the context a user needs (`which path`, `which
key`, `which command`); `anyhow` is used only at the `main.rs` boundary. Failures
that a user can fix degrade rather than abort: a missing theme, an unreadable
state file or a single bad configuration value all produce a warning and a
working app. Only unparseable TOML, an unusable home directory or a failed
terminal takeover are fatal.

## 8. Testing seams

- `Clock` is injected, so TTL and staleness behaviour is deterministic.
- The configuration is parsed into a preserved document before being read into
  typed structs, so round-tripping and unknown-key preservation are testable
  without writing to disk.
- Rendering is a pure function of state, so `TestBackend` snapshots cover the
  screens (`tests/shell_snapshots.rs`).
- Black-box milestone scenarios use one incremental terminal state machine for live
  waits and capture replay. A step can match only cells redrawn after its keys were
  sent, preventing an earlier frame from satisfying a later assertion (IR-15).
- Adapters will get fakes in `application` tests; the real `gh`/LLM paths stay
  opt-in and never run in CI.
