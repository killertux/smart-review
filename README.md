# smart-review

A terminal client for reviewing GitHub pull requests, with LLM-assisted analysis
and a review order that follows the project's architecture instead of the
filesystem.

> **Status: M5 — ready to hand over.** List and read pull requests, analyse them
> with an LLM and get a review order, talk about them, and publish a review with
> inline comments — one batched call, confirmed in a modal that shows exactly what
> will be sent. Reply to and resolve review threads, comment on the PR conversation,
> and use `$EDITOR` for a comment when the inline box is not enough.

## Prerequisites

| Tool | Minimum | Why |
|---|---|---|
| Rust | 1.88 | Ratatui 0.30.2 requires it |
| `git` | 2.30 | Repository and worktree operations (M2) |
| `gh` | 2.40, authenticated | Pull requests, and publishing a review (`gh pr review`, `gh api`) |

`--check` runs the same detection the interface does and exits 0 ready / 1 degraded
/ 2 unusable, naming the first thing to fix.

## Quick start

```sh
cargo run                # open the interface
cargo run -- --check     # environment report, exit 0 ready / 1 degraded / 2 unusable
cargo run -- --help
```

## Releases

A `v*` tag builds native archives for Linux x86_64, macOS Intel, and macOS Apple
Silicon and attaches them to the GitHub release. Extract the archive and put
`smart-review` on `PATH`; it still needs `git` and an authenticated `gh`. Windows is
not a supported release target yet.

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
| `<leader>c` | talk about the pull request (`Tab` reaches the pane too) |
| `Enter` | send the question (`Alt-Enter` or `Ctrl-J` adds a line) |
| `<C-r>` | repeat the last question |
| `Esc` | stop the answer that is arriving, then leave the pane |
| `c` | comment on the line under the cursor (`Enter` stages it, `Esc` cancels) |
| `<C-e>` | open the comment composer in `$EDITOR` |
| `v` / `V` | mark one end of a range, then move and press `c` |
| `<leader>rd` | the staged comments: `j`/`k` walk them, `x` removes one |
| `<leader>rr` | publish the review: the modal shows it verbatim, `Enter` twice sends |
| `r` / `<leader>pr` | reply to the thread under the diff cursor |
| `<leader>pt` | resolve or reopen that thread (it asks first) |
| `<leader>pc` / `<leader>pw` | view / write on the pull request conversation |
| `<leader>ra` / `rc` / `rm` | stage an approval / request changes / a comment with no verdict |
| `<leader>rx` | throw the staged review away (it asks first) |
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
`:analyze [--force|raw]`, `:plan [reset|path|move <file> <group>]`,
`:context [add|remove <path>]`, `:chat [new|list|open <id>|export [md|json]|retry]`,
`:draft [list|remove <n>|clear|decision <d>|body <text>|export [md|json]]`,
`:model [show]`, `:key [clear <provider>]`, `:catalog [refresh]`,
`:workspace [clean [--all]]`, `:set ui.timeoutlen=250`, `:keymap`, `:version`.
`Esc` closes a popup, cancels a half-typed key sequence, or stops an analysis that is
running.

`c` on a line opens the comment composer: `Enter` stages the comment, `Alt-Enter` adds
a line, `Esc` throws it away, and `v` first turns it into a range. Press `<C-e>` to
hand that composer to `$EDITOR`; when the editor exits its file is read back into the
same composer. Staged comments are
marked `●` in the diff gutter, listed by `<leader>rd` (where `x` removes one), and saved
as you write them in `~/.smart-review/drafts/` — a draft is the one thing here that
cannot be fetched again, so it does not live under `cache/`. `<leader>rr` opens the
publish modal: the decision, the body and every comment, verbatim, and nothing is sent
until you press `Enter` twice. A review with inline comments goes to GitHub in **one**
request, so it arrives as a single review rather than as N notifications; a failure
leaves the draft exactly where it was and says what GitHub said, in words. Threads that
are already on the pull request are drawn under the lines they are about: `r` writes a
reply, and `<leader>pt` resolves or reopens the whole thread after confirmation. The PR
conversation is available through `<leader>pc`; `<leader>pw` writes a top-level comment
through GitHub's issue-comment endpoint. Review and reply prose is passed to `gh` in a
private payload file rather than copied into process arguments or ordinary logs.
`--dry-run` (or `[forge] dry_run = true`) records every command that would change
something — publishing, `:workspace clean` — in `logs/dry-run.log` and runs none of
them. Exact review/reply payloads requested by a dry run remain as mode-0600 artifacts
under `exports/dry-run/`, so the recorded commands are replayable.

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
order saved beside the analysis. A `.env`, credential-like path, ignored path, binary
or file over `max_file_bytes` cannot contribute content through either its full body or
its diff. Both old and new names of a rename are checked; `:context` lists the actual
filtered payload and explains exclusions. If repository eligibility cannot be checked,
source content is not sent. This path policy does not claim to scan arbitrary prose in
the PR description or the user's question. Changed-file contents come from the worktree
at the pull request's exact commits, and the analysis is cached per commit, model and
thinking setting, so re-opening it costs
nothing. If the model answers with prose instead of JSON it is asked once more with
the reason, and if it still does not, the text is shown rather than swallowed
(`:analyze raw`).

`<leader>c` (or `Tab`) opens a conversation about the pull request. The model sees the
same bundle an analysis gets — the diff, the changed files, the commit messages and the
repository's `AGENTS.md`, exactly what `:context` lists — and nothing else: it cannot
read the repository, and when it needs something that is not there it says so instead of
guessing. When you want it to have that file, `:context add src/domain/invoice.rs` puts
it in the bundle for every later question. Answers stream in, with the paths they name
listed as being in the change, and sentences the model marks as general knowledge shown
differently from the ones it grounded in your code. `Esc` stops an answer and keeps what
arrived; the conversation is stored per pull request, so restarting finds it, and
`:chat export md` writes a transcript to `~/.smart-review/exports/`.

Not every provider streams, and the app does not pretend otherwise: it asks for a
streamed answer where the provider supports one, then for the same answer with text-only
deltas, then through the provider's OpenAI-compatible endpoint, and finally as a single
request. The answer arrives either way — `logs/smart-review.log` records which of the
four produced it — and a provider that refuses or cannot be reached says so in the pane
rather than leaving it looking as if it were still thinking.

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
  cache/             disposable: PR lists, diffs and analyses
  chats/             persistent conversations — not disposable
  reviews/           persistent manual review-order preferences and records
  drafts/            staged reviews, one file per pull request — not disposable
  worktrees/         per-pull-request checkouts owned by the app (M2)
  exports/dry-run/   private exact payloads retained only when you request a dry run
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
- [`docs/keymaps.md`](docs/keymaps.md) — generated compiled-in keybindings.
- [`docs/configuration.md`](docs/configuration.md) — every configuration default.
- [`docs/themes.md`](docs/themes.md) — theme files, styles and colour formats.
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
