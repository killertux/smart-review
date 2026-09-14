# Themes

smart-review includes `dark` and `light`. Select one with `:theme dark`,
`:theme light`, `:theme next`, or `<leader>t`. User themes live in
`${SMART_REVIEW_HOME:-~/.smart-review}/themes/<name>.toml` and are available by file
stem.

A theme inherits from `dark` unless `base` says `light`. Missing styles retain their
base values, so a two-colour theme is valid.

```toml
name = "Night review"
base = "dark"

[colors]
bg = "#10131a"
fg = "#d7dde8"
accent = "#79c0ff"

[diff]
add = { fg = "#9be9a8", bg = "#153d2b", modifiers = ["bold"] }
del = "#ff9b9b"

[ui]
border = "#586174"
border_focused = "#79c0ff"
```

A shorthand colour supplies foreground except for an element named `*.bg`, which
supplies background. The detailed form accepts optional `fg`, `bg`, and `modifiers`.
Colours may be named terminal colours (such as `red` or `lightblue`), `#RRGGBB`,
`#RGB`, or `indexed:N` (0–255). Supported modifiers are `bold`, `dim`, `italic`,
`underlined`, `reversed`, `crossedout`, `hidden`, and `blink`.

## Style names

Theme section/key pairs are the exact style names below. Unknown names are ignored
with a warning rather than making the UI unusable.

| Section | Keys |
| --- | --- |
| `colors` | `bg`, `fg`, `accent` |
| `ui` | `border`, `border_focused`, `title`, `cursor_line`, `selection`, `muted` |
| `status` | `normal`, `insert`, `command`, `error` |
| `notice` | `info`, `warn`, `error`, `success` |
| `help` | `group`, `key`, `description` |
| `command` | `prompt`, `error` |
| `picker` | `selected` |
| `diff` | `add`, `add_emphasis`, `del`, `del_emphasis`, `context`, `hunk_header`, `line_number`, `stale`, `folded` |
| `tree` | `dir`, `file`, `modified`, `added`, `deleted` |
| `comment` | `marker` |
| `draft` | `marker` |
| `list` | `draft`, `check_ok`, `check_fail`, `check_pending`, `stale` |
| `chat` | `code`, `question`, `answer`, `general`, `stopped`, `reference`, `input` |

Run `:theme reload` after changing a file. A bad value leaves its inherited style in
place and adds a diagnostic visible through `:doctor`.
