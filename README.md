# smart-review

`smart-review` is a terminal client for reviewing GitHub pull requests. It combines
local, revision-pinned diffs with a guided LLM review, grounded chat, durable drafts,
and an explicit preview before anything is posted to GitHub.

## Requirements

- Rust 1.88 or newer when building from source
- Git 2.30 or newer
- GitHub CLI 2.40 or newer, authenticated with `gh auth login`
- Linux or macOS; Windows is best effort

## Install and run

```sh
git clone https://github.com/killertux/smart-review.git
cd smart-review
cargo build --release
./target/release/smart-review --check
./target/release/smart-review
```

Run inside a GitHub clone, or choose a repository explicitly:

```sh
smart-review --repo owner/name
smart-review --repo owner/name --pr 141
smart-review --path /path/to/clone
smart-review --dry-run
```

A `v*` tag publishes native archives for Linux x86_64, macOS Intel, and macOS
Apple Silicon. The binary still needs `git` and an authenticated `gh` on `PATH`.

## Five-minute review workflow

1. Run `smart-review --check`; follow any stated next action, then start the app.
2. Filter with `/`, move with `j`/`k`, and press `Enter` on a pull request.
3. Use `1`–`5` for **Overview**, **Files**, **Checks**, **Discussion**, and **Ask**.
   In Files, use `Tab` to switch between the tree and diff, `]c`/`[c` for hunks,
   and `}`/`{` for files.
4. Optional: press `<leader>m` to choose a provider/model and enter its key. Press
   `<leader>a` twice on first use: the first press previews the exact context policy;
   the second sends it. `o` toggles suggested/path order, `e` expands What/Why/Verify,
   and `m` records human review progress.
5. Press `c` on a diff line to stage a comment. Use `v` or `V`, move, then `c` for a
   range. `<C-e>` edits the active composer with `$EDITOR`.
6. Choose a verdict with `<leader>ra`, `<leader>rc`, or `<leader>rm`. Open the exact
   publish preview with `<leader>rr`; only the labelled `Enter` action posts it.

For an isolated rehearsal that cannot contact GitHub, build once and run the named
fake-`gh` smoke contracts:

```sh
cargo build --bin smart-review
scripts/validate/all.sh --smoke-only
```

## Essential controls

| Key | Action |
|---|---|
| `j` / `k`, arrows | Move |
| `gg` / `G` | First / last |
| `/`, `n` / `N` | Search; next / previous match |
| `1`–`5` | Select a review tab |
| `<Tab>` / `<S-Tab>` | Change pane |
| `<leader>a` / `<leader>c` | Analyze / open Ask |
| `c`, `v`, `V`, `<C-e>` | Compose line/range comments or use `$EDITOR` |
| `<leader>rd` / `<leader>rr` | Inspect draft / preview publish |
| `r`, `<leader>pt` | Reply / resolve or reopen a thread |
| `?`, `<Space>`, `:` | Help, leader menu, command line |
| `<C-c>`, `:q`, `<leader>q` | Quit |

See the generated [complete default keymap](docs/keymaps.md). All bindings are
remappable. `--dry-run` records remote mutations without dispatching them and keeps
private replayable payloads under `$SMART_REVIEW_HOME/exports/dry-run/`.

## Data and safety

Everything owned by the app lives under `$SMART_REVIEW_HOME` (default
`~/.smart-review`). It never writes to the source clone or its `.git` directory.
Managed bare repositories and detached worktrees live under `worktrees/`.

`cache/` is disposable. Drafts, chats, review progress, mutation records, exports,
configuration, and credentials are not cache. API keys are stored in
`credentials.toml` with mode `0600`; ordinary logs exclude keys, source, prompts,
responses, and review prose. See [Product behavior](docs/product.md) for persistence,
cancellation, context, cost, and remote-outcome guarantees.

## Documentation

- [Product behavior](docs/product.md)
- [Architecture](ARCHITECTURE.md)
- [Testing](docs/testing.md)
- [Configuration](docs/configuration.md) and [themes](docs/themes.md)
- [Accepted and open decisions](docs/decisions.md)
- [Legacy requirement ID index](docs/legacy-ids.md)
- [Contributor rules](AGENTS.md)

The completed reliability sequence is retained only as a
[historical implementation record](docs/improvement-plan.md).

## Development

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
scripts/validate/all.sh
```

Read [docs/testing.md](docs/testing.md) before choosing a narrower or opt-in gate.

## Licence

Not yet chosen.
