# Testing

Tests are selected by boundary: pure invariant first, then use-case fake, composed
scenario, rendering snapshot, and only finally a real terminal/process smoke. Routine
tests require no network, installed `gh`, credential, account, or user repository.

## Default gate

Run the same complete gate used for review:

```sh
scripts/validate/all.sh
```

It runs terminal-driver unit tests, local Markdown-link validation, format, Clippy with
warnings denied, all Rust tests/features, one debug build, then six named smoke validators
against that binary. CI performs the same stages once and records Rust/smoke timings.

During iteration, run the smallest relevant Rust filter or validator, then run the
complete gate before declaring the change done:

```sh
cargo test module_or_test_name
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

## Deterministic tests and scenarios

Pure domain/parser/budget/identity rules stay in unit tests. Application tests use fake
ports. TUI behavior uses `TestBackend` and, for cross-feature schedules,
`tui::test_support::Scenario`: decoded input goes through the production reducer and
effect-to-job router, concrete fake completions can be held/released out of order, and
state plus rendered frame are asserted without sleeps.

A race regression must control completion order; scheduler luck and fixed sleeps are
not evidence. A filtered command is valid only when it discovers at least one intended
test.

## Named smoke contracts

Use an already-built binary with:

```sh
scripts/validate/all.sh --smoke-only
```

| Validator | Wiring proved |
|---|---|
| `shell.sh` | startup/restore, persisted theme, resize/SIGWINCH, clean quit |
| `pull-requests.sh` | key decoding and SGR mouse targeting |
| `workspace-models.sh` | fake forge ref to app-owned worktree and local diff |
| `chat.sh` | loopback provider stream, cancel, durable partial transcript |
| `review-publishing.sh` | saved draft, immutable preview, one confirmed fake-`gh` payload, deletion |
| `review-collaboration.sh` | reply routing, dry run, editor suspension/restoration |

The Python PTY driver waits for observable screen/disk postconditions and fails on an
unmet wait, unexpected exit, mismatched steps, or global deadline. Smoke fixtures use
temporary repositories, fake executables, and loopback servers only.

## Opt-in suites

The broad milestone-era validators are retained as a migration oracle, not a default
gate:

```sh
SMART_REVIEW_SKIP_CARGO=1 SMART_REVIEW_FULL_VALIDATION=1 \
  scripts/validate/all.sh --scenarios-only
```

Live provider/catalog checks require `SMART_REVIEW_LIVE_TESTS=1`. Never run a live
mutation contract against a real PR unless the owner explicitly authorizes a designated
sandbox target. Performance workloads are separately opt-in:

```sh
scripts/performance/ir-17.sh
```

See the [IR-18 coverage/measurement record](testing/ir-18.md) and
[IR-17 performance record](performance/ir-17.md).

## Snapshots and generated docs

For an intentional TUI change:

```sh
UPDATE_SNAPSHOTS=1 cargo test --test shell_snapshots
git diff -- tests/snapshots
```

Read every changed line. Snapshots must not contain machine-specific paths. The default
`cargo test --all-features` gate also checks `docs/keymaps.md` byte-for-byte against the
action registry; update generated output from `default_keymap_markdown`, never by
inventing bindings in the document.

Run `python3 scripts/validate/docs.py` for the hermetic local-link/fragment check alone.

## PR evidence

Every behavior change needs a lowest-level regression and every PR ends with an isolated
manual recipe: exact setup/build commands, terminal size, numbered keys/clicks, expected
and failure states, disk/request checks, human-only judgments, and cleanup. Remote writes
remain dry-run/fake unless explicitly authorized.
