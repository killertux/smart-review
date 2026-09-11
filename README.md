# smart-review

A terminal client for reviewing GitHub pull requests, with LLM-assisted analysis
and a review order that follows the project's architecture instead of the
filesystem.

> **Status: M0 — the shell.** Navigation, themes, keybindings, configuration, the
> command line and the environment report are live. The pull request list, the
> diff, the LLM analysis, chat and review publishing arrive in M1–M4 (see
> [`PLAN.md`](PLAN.md)). The app tells you this on its first screen.

## Prerequisites

| Tool | Minimum | Why |
|---|---|---|
| Rust | 1.88 | Ratatui 0.30.2 requires it |
| `git` | 2.30 | Repository and worktree operations (M2) |
| `gh` | 2.40, authenticated | Pull requests, review submission |

`gh` is only needed once M1 lands; M0 runs without it and `--check` tells you
what is missing.

## Quick start

```sh
cargo run                # open the interface
cargo run -- --check     # environment report, exit 0 ready / 1 degraded / 2 unusable
cargo run -- --help
```

Inside the app:

| Key | Action |
|---|---|
| `j` / `k`, `<Down>` / `<Up>` | move |
| `gg` / `G` | first / last |
| `<Tab>` / `<S-Tab>` | next / previous pane |
| `?` | help |
| `<Space>` | leader menu |
| `<Space>t` | theme picker, previewed live |
| `<Space>T` | next theme (wraps through all of them) |
| `:` | command line |
| `<C-c>`, `:q`, `<Space>q` | quit |

`:help`, `:doctor`, `:theme <name>|next|reload`, `:set ui.timeoutlen=250`, `:keymap`,
`:version`. `Esc` closes a popup or cancels a half-typed key sequence.

## Where things live

Everything the application owns goes under `$SMART_REVIEW_HOME`
(default `~/.smart-review`). **Nothing is ever written into your repositories.**

```
~/.smart-review/
  config.toml        settings (optional; every default is built in)
  keybinds.toml      keybinding overrides only
  credentials.toml   API keys entered in the TUI, mode 0600 (M2)
  themes/*.toml      your themes; each inherits from `base`
  state.toml         remembered theme and last session
  cache/             disposable: PR lists, diffs, analyses, chats
  worktrees/         per-pull-request checkouts owned by the app (M2)
  logs/              rotated logs; never contains secrets
```

Set `SMART_REVIEW_HOME` (or pass `--home`) to relocate all of it, which is also
how the tests isolate themselves.

## Documentation

- [`REQUIREMENTS.md`](REQUIREMENTS.md) — the source of truth: goals, numbered
  requirements with acceptance criteria, the open decisions and the decision log.
- [`PLAN.md`](PLAN.md) — milestones, each ending in a runnable binary, plus the
  dependency approval ledger and the risk register.
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — layers, ports and adapters, and the
  concurrency model.
- [`AGENTS.md`](AGENTS.md) — the hard rules for working in this repository.

## Development

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
scripts/validate/m0.sh
```

Snapshot tests cover the rendered screens; regenerate them after an intentional
UI change and review the diff:

```sh
UPDATE_SNAPSHOTS=1 cargo test --test shell_snapshots
```

No test touches the network or needs `gh`.

## Licence

Not yet chosen.
