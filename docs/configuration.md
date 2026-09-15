# Configuration

smart-review reads `${SMART_REVIEW_HOME:-~/.smart-review}/config.toml`. A missing file
is valid: every setting below has a default except the active LLM selection. Values in
a malformed setting fall back to their default and are reported by `:doctor`.

```toml
[ui]
theme = "dark"             # dark | light | a file stem in themes/
mouse = true
leader = "<Space>"
timeoutlen = 500            # milliseconds to disambiguate key sequences
icons = true
date_format = "relative"   # relative | absolute

[review]
state = "open"             # open | closed | merged | all
page_size = 50
max_pages = 10
context_lines = 3
ignore_whitespace = false
order = "recommended"      # recommended | path

[workspace]
mode = "worktree"          # worktree | none
keep_on_exit = true
auto_clean_days = 14

[llm]
max_context_tokens = 100000
max_file_bytes = 262144
max_tool_calls = 8

[catalog]
url = "https://models.dev/api.json"
ttl_hours = 24

[forge]
# remote = "origin"        # otherwise origin, then the first GitHub remote
gh_path = "gh"
page_size = 50
dry_run = false

[cache]
ttl_list_secs = 60
ttl_detail_secs = 300

[log]
level = "info"             # error | warn | info | debug | trace
# path = "/absolute/path/to/smart-review.log"
```

`max_context_tokens` is a hard input ceiling. The catalog can reduce it for a smaller
model window after reserving output tokens, but it never increases it. The complete
prompt framing and chat history are accounted for; `:context` identifies omitted
material. `max_tokens` is sent to supported providers and capped by catalog output
metadata. A configured `temperature` or `reasoning` setting that the selected model
does not support prevents selection rather than being silently ignored.

## Model selection

There is deliberately no provider/model default. Pick one in the TUI with
`<leader>m` or `:model`; smart-review writes this section while preserving the rest
of the file and first creates `config.toml.bak`.

```toml
[llm.active]
provider = "openai"
model = "gpt-5"
temperature = 0.2
max_tokens = 4096
# reasoning = { type = "effort", value = "high" }
```

`reasoning` is optional and follows the catalog: `{ type = "toggle", value = true }`,
`{ type = "effort", value = "high" }`, or `{ type = "budget_tokens", value = 4096 }`.

## Keybindings and state

Key overrides belong in `keybinds.toml`; see [the generated default map](keymaps.md).
`state.toml` is managed by smart-review and remembers UI choices. Do not put API keys
in `config.toml`: set them in the model picker, which writes private
`credentials.toml` instead.

## External editor

While an inline comment composer is open, `<C-e>` restores the terminal and runs
`$EDITOR` on a private scratch file under smart-review's home. When the editor exits,
the file replaces the composer contents; a non-zero editor exit still keeps the text.
If smart-review cannot resume the terminal, it leaves the scratch file in place and
records its path in `logs/smart-review.log` for recovery.

## Dry run

Set `[forge].dry_run = true` or start with `--dry-run` to record mutating GitHub and
workspace commands in `logs/dry-run.log` without running them. Reads still run so the
preview describes the real PR. Review/reply bodies are never copied into argv or the
ordinary diagnostic log. A dry run that needs a body writes an exact private (`0600`)
payload under `exports/dry-run/`; the command log references that path so it can be
inspected or replayed deliberately.

## Context privacy

The same eligibility decision applies to a changed file's full contents and its diff,
including deleted lines and both old/new names of a rename. Credential-like names,
paths matched by repository ignore rules (even when tracked), binary files and files
over `llm.max_file_bytes` contribute no source content. `:context` is generated from
the filtered payload and explains each exclusion. When the repository rules or file
bytes cannot be checked, source content is not sent.

This is a file/path boundary, not a generic secret scanner. Pull-request descriptions,
commit messages and questions are user-visible prose included in the pre-send preview;
smart-review does not guess which substrings in that prose are secrets.
