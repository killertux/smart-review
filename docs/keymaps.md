# Default keymap

<!-- Generated from `src/tui/action.rs` and `DEFAULT_BINDINGS`; do not edit by hand. -->

These are the compiled-in defaults. Add overrides to `${SMART_REVIEW_HOME:-~/.smart-review}/keybinds.toml`; the **scope** is the table name below `[keys]`. `<leader>` means the configured leader (default: Space).

## App

| Action | Default binding(s) | Scope | Description |
| --- | --- | --- | --- |
| `app.quit` | `<C-c>`<br>`q`<br>`<leader>q` | `global`<br>`normal`<br>`normal` | Quit smart-review |
| `app.help` | `?`<br>`<leader>?` | `global`<br>`normal` | Show the help popup |
| `app.command` | `:` | `global` | Open the command line |
| `app.leader_menu` | `<leader>` | `global` | Show the available leader bindings |
| `app.cancel` | `<Esc>` | `global` | Close the current popup, or cancel pending keys |
| `app.refresh` | `R` | `normal` | Refresh the current view |
| `app.doctor` | — | — | Show environment checks |
| `app.version` | — | — | Show the version |
| `notice.clear` | — | — | Dismiss the current notification |
| `app.model_picker` | `<leader>m` | `normal` | Choose the provider, model and thinking settings |
| `app.analyze_panel` | `<leader>a` | `normal` | Analyse the pull request, or open the analysis |
| `plan.move_up` | `K` | `normal` | Move the selected review-plan group up |
| `plan.move_down` | `J` | `normal` | Move the selected review-plan group down |
| `app.load_more` | — | — | Fetch the next page of pull requests |

## Navigation

| Action | Default binding(s) | Scope | Description |
| --- | --- | --- | --- |
| `nav.up` | `k`<br>`<Up>` | `normal`<br>`normal` | Move up |
| `nav.down` | `j`<br>`<Down>` | `normal`<br>`normal` | Move down |
| `nav.top` | `gg` | `normal` | Jump to the first item |
| `nav.bottom` | `G` | `normal` | Jump to the last item |
| `nav.open` | `<Enter>` | `normal` | Open the selected pull request, or the selected file |
| `nav.half_down` | `<C-d>` | `normal` | Move half a screen down |
| `nav.half_up` | `<C-u>` | `normal` | Move half a screen up |
| `nav.page_down` | `<C-f>` | `normal` | Move a screen down |
| `nav.page_up` | `<C-b>` | `normal` | Move a screen up |
| `nav.back` | `<Esc>`<br>`h` | `normal`<br>`normal` | Close the review, or clear the search and filters |

## Panes

| Action | Default binding(s) | Scope | Description |
| --- | --- | --- | --- |
| `pane.next` | `<Tab>`<br>`<Tab>` | `normal`<br>`insert` | Focus the next pane |
| `pane.prev` | `<S-Tab>`<br>`<S-Tab>` | `normal`<br>`insert` | Focus the previous pane |

## Search

| Action | Default binding(s) | Scope | Description |
| --- | --- | --- | --- |
| `search.open` | `/` | `normal` | Filter the loaded pull requests as you type |
| `search.close` | `<Enter>`<br>`<Esc>` | `search`<br>`search` | Stop editing the search |
| `search.next` | `n` | `normal` | Next match |
| `search.prev` | `N` | `normal` | Previous match |
| `filter.menu` | `<leader>f` | `normal` | Add a filter chip |
| `filter.clear` | `x` | `normal` | Clear the filters and the search |
| `sort.menu` | `<leader>s` | `normal` | Change the sort order |

## Diff

| Action | Default binding(s) | Scope | Description |
| --- | --- | --- | --- |
| `diff.next_hunk` | `]c` | `normal` | Next hunk |
| `diff.prev_hunk` | `[c` | `normal` | Previous hunk |
| `diff.next_file` | `}` | `normal` | Next file |
| `diff.prev_file` | `{` | `normal` | Previous file |
| `diff.toggle_hunk` | `za` | `normal` | Fold or unfold the hunk under the cursor |
| `diff.toggle_split` | `<leader>ds` | `normal` | Side-by-side view, when the terminal is wide enough |
| `diff.cycle_context` | `<leader>dc` | `normal` | Cycle the context lines: 3, 10, 0 |
| `diff.toggle_whitespace` | `<leader>dw` | `normal` | Ignore whitespace-only changes |
| `review.copy_path` | `y` | `normal` | Copy the current file path |
| `diff.toggle_order` | `o` | `normal` | Switch between the recommended and path orders |

## Theme

| Action | Default binding(s) | Scope | Description |
| --- | --- | --- | --- |
| `app.theme_picker` | `<leader>t` | `normal` | Choose a theme |
| `theme.toggle` | `<leader>T` | `normal` | Switch to the next theme |

## Chat

| Action | Default binding(s) | Scope | Description |
| --- | --- | --- | --- |
| `chat.open` | `<leader>c` | `normal` | Talk about this pull request |
| `chat.send` | `<Enter>` | `insert` | Send the question |
| `chat.newline` | `<S-Enter>`<br>`<A-Enter>`<br>`<C-j>` | `insert`<br>`insert`<br>`insert` | Add a line to the question |
| `chat.retry` | `<C-r>`<br>`<C-r>` | `insert`<br>`normal` | Ask the last question again |
| `chat.cancel` | `<Esc>` | `insert` | Stop the answer that is arriving |
| `chat.list` | — | — | List the conversations about this pull request |
| `chat.export` | — | — | Write a transcript |

## Review

| Action | Default binding(s) | Scope | Description |
| --- | --- | --- | --- |
| `review.comment_line` | `c` | `normal` | Comment on the line under the cursor (c) |
| `review.range` | `v`<br>`V` | `normal`<br>`normal` | Start a range: V, move, then c |
| `review.approve` | `<leader>ra` | `normal` | Stage an approval (<leader>ra) |
| `review.request_changes` | `<leader>rc` | `normal` | Stage a request for changes (<leader>rc) |
| `review.comment_only` | `<leader>rm` | `normal` | Stage a comment with no verdict (<leader>rm) |
| `review.discard` | `<leader>rx` | `normal` | Throw the staged review away (<leader>rx) |
| `review.reply` | `r`<br>`<leader>pr` | `normal`<br>`normal` | Answer the comment under the cursor (r) |
| `review.edit_composer` | `<C-e>` | `insert` | Edit the open comment in $EDITOR (Ctrl-E) |
| `review.toggle_resolved` | `<leader>pt` | `normal` | Resolve or reopen the thread under the cursor (<leader>pt) |
| `review.conversation` | `<leader>pc` | `normal` | The pull request's own conversation (<leader>pc) |
| `review.comment_conversation` | `<leader>pw` | `normal` | Write a comment on the conversation |
| `review.drafts` | `<leader>rd` | `normal` | Show the staged comments (<leader>rd) |
| `review.publish` | `<leader>rr` | `normal` | Publish the staged review (<leader>rr) |
| `review.remove` | `x` | `popup` | Remove the selected staged comment (x) |

## Override example

```toml
[keys.normal]
# Bind `x` to a known action, or use `none` to remove a default binding.
x = "review.comment_line"
"<leader>ra" = "none"
```

Run `:keymap` inside smart-review to see the effective map after overrides.
