# smart-review

A terminal client for reviewing GitHub pull requests, with LLM-assisted analysis
and a review order that follows the project's architecture instead of the
filesystem.

> **Status: M1 — browsing and reading.** List, filter and search pull requests,
> then read the diff with vim motions, folding, mouse and a side-by-side view. The
> managed worktree, the model picker, the LLM analysis, chat and review publishing
> arrive in M2–M4 (see [`PLAN.md`](PLAN.md)).

## Prerequisites

| Tool | Minimum | Why |
|---|---|---|
| Rust | 1.88 | Ratatui 0.30.2 requires it |
| `git` | 2.30 | Repository and worktree operations (M2) |
| `gh` | 2.40, authenticated | Pull requests, review submission |

`--check` runs the same detection the interface does and exits 0 ready / 1 degraded
/ 2 unusable, naming the first thing to fix.

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
| `<C-d>` / `<C-u>`, `<C-f>` / `<C-b>` | half page, whole page |
| `<Enter>` | open the selected pull request, or the file under the tree cursor |
| `Esc` | close the review; on the list, clear the search and filters |
| `/` | filter what has been fetched (client side, as you type) |
| `n` / `N` | next / previous match |
| `<Tab>` / `<S-Tab>` | tree ↔ diff |
| `]c` / `[c` | next / previous hunk |
| `}` / `{` | next / previous file |
| `za` | fold the hunk (or the whole file, from its banner) |
| `y` | copy the current file path (OSC 52) |
| `<leader>m` | choose the provider, model and thinking settings |
| `<leader>a` | analyse the pull request, or open the analysis |
| `o` | switch between the recommended and path orders |
| `J` / `K` | move the selected review-plan group |
| `<leader>dc` | cycle the diff context: 3, 10, 0 lines |
| `<leader>dw` | ignore whitespace-only changes |
| wheel | scroll the pane under the pointer |
| click | focus a pane and put the cursor on the row you clicked |
| `<Space>f` / `<Space>s` | add a filter / change the order |
| `<Space>d` then `s` `c` `w` | split view, context lines, whitespace |
| `<Space>t` / `<Space>T` | theme picker / next theme |
| `?` | help |
| `<Space>` | leader menu |
| `:` | command line |
| `<C-c>`, `:q`, `<Space>q` | quit |

`:help`, `:doctor`, `:pr 141`, `:filter author:alice`, `:clear-filters`,
`:sort updated desc`, `:load-more`, `:copy-path`, `:theme <name>|next|reload`,
`:analyze [--force|raw]`, `:plan [reset|path|move <file> <group>]`, `:context`,
`:model [show]`, `:key [clear <provider>]`, `:catalog [refresh]`,
`:workspace [clean [--all]]`, `:set ui.timeoutlen=250`, `:keymap`, `:version`.
`Esc` closes a popup, cancels a half-typed key sequence, or stops an analysis that is
running.

Opening a pull request also materialises it as a managed git worktree under
`~/.smart-review/worktrees/`, so the diff can be produced locally: the context and
whitespace toggles only mean something for a locally produced diff, and the status
line says which source answered (`worktree` or `github`). Your checkout's `HEAD`,
branches, index and working tree are never touched.

Choosing a model happens in the TUI (`<leader>m`): pick a provider, search the models
the [models.dev](https://models.dev) catalog lists for it, choose a thinking mode, and
paste the key into a masked prompt. The key goes to `credentials.toml` (mode 0600) and
nowhere else; the choice is written back to `config.toml` without disturbing your
comments, and is then checked against the provider.

Opening a pull request fetches its detail and then its diff, so the wait shows a
centred indicator naming the pull request, which step it is on and how long it has
been going; `Esc` gives up on it.

`<leader>a` asks the chosen model to read the pull request and say what changed, why,
what is risky and in what order the files should be read. The first press for a
repository shows what would be sent — the estimate, and the list of files, included
and not — and sends nothing until you press it again. The answer streams into a panel,
and the file tree reorders to the plan it returned; `o` reads the same files in path
order, `J`/`K` move a group, and `:plan move <file> <group>` pins a file, with your
order saved beside the analysis. A `.env`, a binary and a file over `max_file_bytes`
are replaced by a note, the changed files' contents come from the worktree at the
pull request's commit, and `:context` lists everything that would be sent — the
analysis is cached per commit, model and thinking setting, so re-opening it costs
nothing. If the model answers with prose instead of JSON it is asked once more with
the reason, and if it still does not, the text is shown rather than swallowed
(`:analyze raw`).

Two mechanisms filter the list, and the interface keeps them visibly apart: the
**chips** change what GitHub is asked (`gh pr list --search`), while the `/` box
filters what has already arrived, so 300 cached pull requests narrow without a
round trip.

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
  cache/             disposable: PR lists, diffs, analyses and their plans, chats
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
