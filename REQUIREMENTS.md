# Smart Review — Requirements Specification

**Status:** Draft v0.9 (DEC-1 … DEC-6, DEC-17, DEC-19, DEC-20, DEC-21 resolved; DEC-7 … DEC-16, DEC-18 pending, each with a proposed default). M0–M3 are implemented: the shell, browsing and diffs, the workspace and model configuration, the analysis with its review order, and chat. §5's keymap reflects them. M4 (review publishing) is next.
**Scope:** v1 (MVP) + post-v1 backlog
**Source of truth:** this file. If code and this file disagree, the file wins or the file is updated in the same change.

---

## 0. How to read this document

- **Requirement IDs are stable.** Never renumber. Add new IDs; mark removed ones `~~deprecated~~`.
- **RFC 2119 keywords:** `MUST` = mandatory for the milestone that owns it, `SHOULD` = strongly recommended, `MAY` = optional.
- **Confidence markers:** `[DECIDED]` = agreed with the project owner. `[PROPOSED — see DEC-x]` = a recommended default that has **not** been approved. Do not implement a `[PROPOSED]` option without confirmation.
- **Every FR states its milestone and its acceptance criteria.** Acceptance criteria are the definition of done and are written as testable checkboxes.
- **Open questions live in §11 Open Decisions.** Each has an ID (`DEC-n`), a proposed default, and the blast radius if chosen differently. When a `DEC-n` is resolved, flip it to `[DECIDED]`, update the affected FRs, and append a line to §11.1 Decision log.

### 0.1 What this document is not
This is a *requirements* document that also pins the architectural constraints an agent must respect (§4). It deliberately does **not** specify final type names, module-by-module implementation, or prompt wording. Those belong in `ARCHITECTURE.md`, which SHOULD be split out at M0 and kept subordinate to this file.

---

## 1. Product overview

### 1.1 One-liner
`smart-review` is a terminal (TUI) client for reviewing GitHub pull requests, with an LLM co-pilot that explains the change and reorders the diff into a review-friendly sequence, plus an in-app chat over the PR's code.

### 1.2 Problem
Reviewing a PR in a browser tab is context-poor: the reviewer cannot easily see the surrounding code, cannot ask questions about intent, and gets a diff ordered by filesystem path rather than by conceptual importance. `smart-review` runs where the code already lives (the developer's terminal, inside a clone) so the reviewer keeps full repository context and shell-adjacent speed.

### 1.3 Goals (v1)
- **G1** Browse, search, and filter PRs of the current repository without leaving the terminal.
- **G2** Read a PR's diff, metadata, checks and discussion with vim-native navigation.
- **G3** Give the reviewer the PR head checked out locally so the whole codebase (and `AGENTS.md`) is available as context.
- **G4** Produce an LLM analysis: what the change does, why, what is risky, and a recommended review order that follows the project's architecture (e.g. domain → application → infrastructure).
- **G5** Chat with an LLM about the PR, grounded in the diff and the repository.
- **G6** Approve / request changes / comment, including inline line comments, from inside the app.
- **G7** Be configurable: every keybinding and every theme overrideable in `~/.smart-review`, with dark and light defaults.
- **G8** Reliable on large PRs and large repositories: never block the UI, never lose a draft.

### 1.4 Non-goals (explicitly out of scope for v1)
- **NG1** Forges other than GitHub. (Pluggable via ports, but only a GitHub adapter ships.)
- **NG2** Hosting/executing code changes: `smart-review` never edits the PR's code. It is a read-and-comment tool. (Exception: it creates git worktrees/branches.)
- **NG3** Managing PR lifecycle: merge, close, reopen, rebase, edit title/body, assign, label. Read + review only.
- **NG4** Resolving or replying to existing review threads. (Backlog.)
- **NG5** Reviewing local uncommitted changes or arbitrary diffs not backed by a PR.
- **NG6** Multi-repository dashboards, notifications, or a server/daemon component.
- **NG7** An editor. Comment bodies are plain multi-line text (optional `$EDITOR` escape hatch only).
- **NG8** Deep CI integration: check status is displayed, logs are not fetched.
- **NG9** Windows tier-1 support (see NFR-2.3).

### 1.5 Glossary (use this vocabulary in code, UI, and issues)
| Term | Meaning |
|---|---|
| **Forge** | The hosting platform for a PR (v1: GitHub). Accessed through `ForgePort`. |
| **PR / PullRequest** | A pull request identified by `(repo, number)`. |
| **Base / Head** | The target branch/SHA and the source branch/SHA of a PR. |
| **Merge base** | `git merge-base <base> <head>`; the diff of a PR is defined as `merge_base..head` (git's `A...B`). |
| **Workspace** | The on-disk checkout of a PR head that the app creates and owns. |
| **File diff / hunk / line** | Parsed representation of a unified diff. A line carries `Side::{Old,New}`. |
| **Analysis** | The LLM's structured output for a PR: summary, intent, risks, review plan, per-file notes. |
| **Review plan** | The Analysis' ordered grouping of changed files into a review sequence. |
| **Draft** | A review that exists only locally: decision + body + inline comments, not yet published. |
| **Decision** | One of `Approve`, `RequestChanges`, `Comment`. |
| **Context bundle** | The exact payload sent to the LLM for a request. Must be inspectable by the user. |
| **Mode** | Input context of the TUI: `normal`, `insert`, `command`, `search`, `popup`, `visual`. |
| **Leader** | Configurable prefix key for the action menu (default: `space`). |
| **Action ID** | Stable dotted identifier for a user-triggerable command (e.g. `review.approve`); the unit of keymapping. |
| **Model catalog** | Provider/model metadata fetched from `https://models.dev/api.json` and cached locally; the source of the picker and of thinking/limit/cost metadata. |
| **Active selection** | The provider + model + thinking settings in use. There is no built-in default; the user creates it in the TUI. |
| **Thinking mode** | A model's reasoning behaviour: off/on, plus an optional effort level or token budget, as declared by the catalog's `reasoning_options`. |

---

## 2. Users and primary workflows

### 2.1 Persona
A developer who reviews several PRs per day, lives in a terminal, knows vim, and is mildly annoyed by the browser. Secondary persona: a team lead doing an architectural review of a large PR.

### 2.2 Happy path (must work end-to-end by end of M4)
1. `cd ~/code/acme-service && smart-review`
2. App validates the environment (git repo, GitHub remote, `gh` installed and authenticated) and shows the PR list, newest first.
3. User types `/retry webhook` and narrows by `author:alice`; list updates.
4. User presses `Enter` on PR #142. App fetches metadata and the diff, and ensures a local workspace for the PR head.
5. User presses `<leader>a`. The LLM streams an analysis and the file tree is re-ordered into a review plan (domain → application → infra). A banner states that the order is LLM-recommended and that `o` toggles back to path order.
6. User walks hunks with `]c` / `[c`, and presses `c` on a line to leave an inline comment; the comment is staged in the draft panel (nothing published yet).
7. User presses `<leader>c` and asks "does this break the webhook retry contract?" The answer streams in with references to files.
8. User presses `<leader>r r` → confirms the publish modal → the review (decision + body + 3 inline comments) is submitted to GitHub as **one** review.
9. User quits with `:q`. Drafts, chat history, cache and preferences persist under `~/.smart-review`.

### 2.3 Secondary workflows
- **W1** Review a PR without LLM configured: everything except FR-4/FR-5 works, and the absence is explained, not a crash.
- **W2** Re-open a PR reviewed yesterday: analysis and chat are restored from cache (no re-billing).
- **W3** New commits were pushed after the analysis: the app detects the head SHA change and marks the analysis stale.
- **W4** Offline: cached PR list is shown with an "offline/cached" indicator; actions requiring network fail with an actionable message.
- **W5** Fork PR: the workspace is created from the fork's head ref without polluting the user's branches.

---

## 3. Functional requirements

### FR-1 Repository & environment

**FR-1.1 Environment detection** — MUST — M0
On startup the app MUST resolve, before rendering the first PR:
- whether the current directory is inside a git work tree (`git rev-parse --show-toplevel`);
- the forge remote: `origin` if it is GitHub, else the first GitHub remote; overridable by `--remote` / config;
- whether `gh` exists and satisfies the minimum version (MUST be ≥ 2.40; verified by parsing `gh --version`);
- whether `gh auth status` reports an authenticated account with `repo` scope.

Acceptance criteria:
- [ ] Each failure produces one specific, actionable message (install `gh`, run `gh auth login`, choose a remote, not a git repo) and offers the matching next step.
- [ ] `--repo OWNER/NAME` and `SMART_REVIEW_REPO` allow running outside a clone (degraded: no local workspace, so FR-4/FR-5 code-context features are disabled with an explanation).
- [ ] Detection result (host, owner, name, default branch, remote name, gh path+version, account) is available via `:doctor`.

**FR-1.2 Launch options** — MUST — M0
CLI: `smart-review [--repo OWNER/NAME] [--pr N] [--path DIR] [--remote NAME] [--config FILE] [--theme NAME] [--check] [--version] [--help]`. `--check` runs FR-9.3 doctor and exits non-zero on failure.

**FR-1.3 Repository identity & cache partitioning** — MUST — M1
All persisted per-repository data MUST be keyed by `host/owner/name` so that opening a second clone of the same repo shares cache, and two different repos never collide.

### FR-2 PR discovery and filtering

**FR-2.1 PR list** — MUST — M1
The list MUST come from `gh pr list --limit <n> --json <fields>` and default to state `open`, ordered by creation date descending (explicit `sort:created-desc` in the search query rather than relying on server default). Minimum fields: `number,title,author,createdAt,updatedAt,isDraft,baseRefName,headRefName,headRefOid,additions,deletions,changedFiles,labels,reviewDecision,statusCheckRollup,url`.
Acceptance criteria:
- [ ] Rows show: number, draft/WIP marker, title, author, relative updated-at, additions/deletions, check summary, review decision.
- [ ] Pagination is explicit: initial `--limit` from config (default 50), `:load-more` appends up to a configured cap (default 500) and never silently truncates — the status line says `showing 50 of ≥137`.
- [ ] State filter supports `open` (default), `closed`, `merged`, `all` and is reflected in the header.

**FR-2.2 Search & filter** — MUST — M1
Two complementary mechanisms:
- **Server-side** (`gh pr list --search`), built from structured filter chips: title (`in:title`), description (`in:body`), author (`author:`), number (`#N` — resolved directly), label (`label:`), base branch (`base:`), state (`is:open|is:closed|is:merged`), draft (`draft:true|false`), review (`review:required|approved|changes_requested`).
- **Client-side** incremental fuzzy match over the cached list, debounced ≤ 100 ms, active while the user types in `/` search.
Acceptance criteria:
- [ ] Filter chips are visible and individually removable; `:clear-filters` resets.
- [ ] A repository with 300 PRs filters without a network round trip when the result is already cached (client-side path).
- [ ] Empty results render a helpful empty state that echoes the effective query string.
- [ ] Invalid qualifiers are rejected with a message instead of being sent to GitHub.

**FR-2.3 Refresh & cache** — MUST — M1
PR lists and PR details are cached on disk with a TTL (default 60 s for lists, 300 s for details; configurable). `R` forces refresh. Cache is consulted first for instant first paint, then revalidated.
Acceptance criteria:
- [ ] First paint from cache is < 200 ms; the network result replaces it in place without losing cursor or scroll position.
- [ ] Stale-but-offline shows the cached data plus an `offline` indicator (W4).

**FR-2.4 PR detail fetch** — MUST — M1
Detail MUST include metadata (title, body, author, state, draft, base/head refs+SHAs, `isCrossRepository`, timestamps, labels, reviewers, `reviewDecision`, `mergeStateStatus`), commits, check rollup, existing reviews, and existing inline comments — obtained via `gh pr view --json` and, where the CLI's field set is insufficient, `gh api`.
Acceptance criteria:
- [ ] Missing/optional data degrades (e.g. no checks configured) without error.
- [ ] `baseRefOid` is **not** in the `gh` JSON field list (verified on gh 2.45); the base SHA MUST be resolved via `git fetch` + `git merge-base` (see Appendix A).

### FR-3 Workspace, diff and code viewing

**FR-3.1 PR workspace** — MUST — M2 — *DEC-1 `[DECIDED]`: managed git worktree*
The app MUST materialize the PR head on disk in an app-owned git worktree so the LLM and the user can read whole files, without ever modifying the user's working tree.
Acceptance criteria:
- [ ] Workspace path is `<SMART_REVIEW_HOME>/worktrees/<owner>-<repo>/pr-<N>`.
- [ ] Creation uses, in order: reuse existing valid workspace for the same head SHA → `git fetch origin <baseRefName> refs/pull/<N>/head` → `git worktree add --detach <path> <fetched-head-sha>`.
- [ ] The user's working tree, index, HEAD and branches are never modified; a dirty working tree is never a precondition for using the app.
- [ ] Fork PRs work through the same `refs/pull/<N>/head` path.
- [ ] Workspaces are listed/removed by `:workspace clean [--all]`; removal runs `git worktree remove` and prunes. On failure the user is told exactly which path to delete manually.
- [ ] A missing/expired workspace is recreated transparently on demand.

**FR-3.2 Diff acquisition** — MUST — M1 (remote-only) / M2 (local)
The diff MUST be computed locally in the workspace as `git diff --unified=<ctx> <base>...<head>` (three-dot), with `--find-renames`, `--no-color`, and `--no-ext-diff`. Remote-only mode MAY fall back to `gh pr diff --patch`.
Acceptance criteria:
- [ ] Default context = 3 lines, adjustable at runtime (0/3/10) without refetching the whole PR when the workspace exists.
- [ ] Whitespace-ignoring mode (`-w`) is toggleable and visibly indicated.
- [ ] Rename detection is surfaced as a rename, not as delete+add.
- [ ] The diff is parsed into `FileDiff → Hunk → Line` with old/new line numbers; the parser is a pure, unit-tested function.
- [ ] A changed file marked `binary`, a mode-only change, a symlink, a submodule pointer, and an empty diff all render a specific placeholder row (no crash, no silent skip).

**FR-3.3 Diff rendering** — MUST — M1 — *DEC-4 `[DECIDED]`: unified default + split toggle, highlighting deferred*
Requirements: file tree sidebar with per-file +/− stats and folder grouping; unified diff pane; stable line numbers for both sides; hunk headers; add/del/context styling; current-line highlight; virtualized rendering (only visible lines are laid out/rendered).
Acceptance criteria:
- [ ] A 10 000-line / 400-file diff opens in the UI in < 1.5 s and scrolls at ≥ 30 fps with bounded memory (no full-buffer re-layout per frame).
- [ ] The sidebar and diff pane keep independent cursor/scroll state; `Tab`/`Shift-Tab` move focus.
- [ ] Side-by-side (split) view MUST be available when the terminal is ≥ 140 columns, toggled with `<leader>d s`; below that width the toggle is unavailable with an explanation, not a broken layout.
- [ ] Syntax highlighting is **not** in v1 (DEC-4): the diff uses theme colors only. Any highlighting dependency requires a new approval.
- [ ] A file's hunks collapse/expand (`za`-style) and the tree shows collapsed state.

**FR-3.4 Navigation** — MUST — M1
`,]c`/`[c` next/prev hunk; `}`/`{` next/prev file; `j/k` line; `Ctrl-d/u`, `Ctrl-f/b`; `gg/G`; `n/N` for search hits; `Enter` opens the file in the diff pane from the tree; `h`/`Esc` returns. All bindings are remappable (FR-7.2).
Acceptance criteria:
- [ ] Every movement keeps the diff, tree and status line in sync.
- [ ] `:copy-path` (and a bindable action) copies the current file path via OSC 52 with a message if unsupported.

**FR-3.5 Layer-ordered view** — MUST — M2 — *see DEC-10*
The diff MUST be re-orderable using the LLM review plan (FR-4.2) with a clearly-labelled toggle between `path order` and `recommended order`, plus a heuristic fallback ordering (path/name rules) when no analysis exists.
Acceptance criteria:
- [ ] Toggling order preserves the current file when possible and shows both orders' file positions.
- [ ] Reordering never mutates the underlying diff model and never re-runs git.

### FR-4 LLM analysis

**FR-4.1 Analysis output** — MUST — M2 — *DEC-2 `[DECIDED]`: analysis + chat, no agentic tool loop in v1*
One request MUST produce a structured Analysis: `summary` (what changed), `intent` (why, inferred), `risk_areas[]`, `review_plan[]` (ordered groups with rationale and file lists), `per_file_notes[]`, `suggested_questions[]`, plus `model`, `prompt_version`, `head_sha`, `created_at`, `token_usage`. The schema is in Appendix B; it MUST be validated before use.
Acceptance criteria:
- [ ] Malformed/partial JSON is repaired once, then reported as a failed analysis with the raw text viewable — never silently dropped or panicked on.
- [ ] Unknown files referenced by the LLM are ignored with a warning; changed files missing from the plan are appended in path order under an "unclassified" group.
- [ ] Every claim about a file is attributable to a path present in the diff.

**FR-4.2 Review plan & ordering** — MUST — M2
The plan groups changed files by architectural role. For a DDD-style repo the expected shape is domain → application → infrastructure → interfaces/config → tests/docs. Grouping MUST be derived from the analysis, not hard-coded, with the heuristic classifier only as fallback.
Acceptance criteria:
- [ ] The plan panel explains *why* each group is reviewed in that position.
- [ ] The user can override the order manually (move group up/down, pin file to group) and the override wins and persists per PR.
- [ ] `o` toggles recommended vs path order (FR-3.5).

**FR-4.3 Caching & invalidation** — MUST — M2
Analysis MUST be cached per `(repo, pr, head_sha, provider, model, thinking settings, prompt_version)`.
Acceptance criteria:
- [ ] Reopening the same PR at the same SHA serves from cache with zero network calls; the UI shows the cache age and the model used.
- [ ] A head SHA change marks the analysis `stale` and offers re-run; it is never silently presented as current.
- [ ] `:analyze --force` recomputes; the cache write is atomic (temp file + rename).

**FR-4.4 Streaming, progress, cancellation** — MUST — M2
Analysis and chat responses MUST stream token-by-token into the UI, show a cancellable progress state, and never block input.
Acceptance criteria:
- [ ] `Esc` cancels an in-flight request; partial text stays visible and is marked partial.
- [ ] The event loop never blocks > 50 ms (NFR-1.2); a stale response from a cancelled/superseded request is discarded by job id.

**FR-4.5 Provider, model and key configuration (in the TUI)** — MUST — M2 — *DEC-5 `[DECIDED]`, DEC-6 `[DECIDED]`*
There is **no default provider and no default model**. The active selection is chosen by the user inside the TUI and persisted; LLM features stay inert (with a clear call to action) until it exists.
Acceptance criteria:
- [ ] `<leader>m` / `:model` opens a three-step picker: provider → model → thinking options (FR-4.8). Every step is searchable; `Esc` backs out without changing the active selection.
- [ ] The selection is written to `[llm.active]` in `config.toml` and takes effect immediately — no restart.
- [ ] If the chosen provider has no stored key, the picker continues into a masked key-entry prompt and writes it to `credentials.toml` (FR-4.7, §7.4). The value is never echoed, logged, or placed in `config.toml`.
- [ ] The status line always shows the active `provider/model` plus a thinking indicator (e.g. `thinking:high`); `:model show` prints provider, model, base URL, key source and thinking settings.
- [ ] A provider's documented environment variable (the catalog's `env` field) is honored as an override, and the **source** (`env` vs `file`) is displayed so precedence is never a mystery.
- [ ] User-saved presets (`:model save <name>`, `:model use <name>`) are optional convenience; nothing in the app requires a preset to exist.
- [ ] First run with no model configured MUST still allow browsing PRs and reading diffs; `<leader>a` explains that a model must be selected and opens the picker.

**FR-4.6 Context assembly & privacy guardrails** — MUST — M2
The context bundle MUST be assembled deterministically and be inspectable with `:context` (files included, byte/token estimate, and what was truncated).
Acceptance criteria:
- [ ] Bundle = PR metadata + commit messages + full diff + contents of changed files at head + repo conventions (`AGENTS.md`, then `CLAUDE.md`, then `README.md`, first hit wins per file) + optional user-added files.
- [ ] `.env*`, files matched by `.gitignore`, credentials, and files > `max_file_bytes` (default 256 KiB) are never included; binary files are replaced by a placeholder.
- [ ] Total context is capped by `max_context_tokens` (default 100k) with a documented truncation order: per-file elision → diff context reduction → oldest chat turns dropped.
- [ ] Token/byte estimate is shown before the first send for a given PR, with a one-time opt-in notice per repository.

**FR-4.7 Model catalog** — MUST — M2 — *DEC-6 `[DECIDED]`: catalog sourced from models.dev*
The provider and model lists MUST be sourced from `https://models.dev/api.json` and cached locally, so the picker is data-driven rather than a hand-maintained list.
Acceptance criteria:
- [ ] The catalog is cached at `<root>/cache/models.json` with a TTL (`catalog.ttl_hours`, default 24) and refreshed by `:catalog refresh`; a stale cache is always preferred over an empty picker.
- [ ] Only providers reachable through the adapter mapping (DEC-17) appear in the picker; unsupported providers are hidden rather than shown as broken.
- [ ] Model rows display, when the catalog provides them: reasoning support, context and output limits, cost per 1M tokens, tool/structured-output support, and `release_date`. Old/deprecated models are de-emphasized, never silently hidden.
- [ ] Model search matches id, name and family, case-insensitively, ranking exact prefix matches first.
- [ ] When the catalog cannot be fetched (offline, first run), the user MAY enter provider, base URL and model id manually, and the app MUST state that capability metadata (thinking options, limits, cost) is unknown for that entry.
- [ ] Catalog limits drive budgeting: `limit.context` seeds the offered `max_context_tokens`, and `limit.output` caps `max_tokens`.
- [ ] The catalog is advisory only for capability claims: a model the provider rejects surfaces the provider error verbatim.

**FR-4.8 Thinking mode** — MUST — M2 — *DEC-6 `[DECIDED]`*
Thinking/reasoning behaviour MUST be configurable per active selection and constrained by the chosen model's catalog metadata.
Acceptance criteria:
- [ ] The picker offers only the controls the model declares in `reasoning_options`: `toggle` → on/off, `effort` → only the declared values, `budget_tokens` → a token-budget input. A model with `reasoning: false` MUST NOT offer any thinking control.
- [ ] Mapping onto the `llm` crate is explicit and documented (Appendix B): toggle → `.reasoning(bool)`, effort → `.reasoning_effort(..)`, budget → `.reasoning_budget_tokens(u32)`. Where the crate cannot express a catalog option (e.g. an `effort` value the crate does not model), the app MUST refuse the combination with an explanation instead of silently downgrading it.
- [ ] Reasoning token consumption from the provider is displayed alongside the response's token usage (the crate exposes `usage.reasoning_tokens`).
- [ ] **v1 does not display the reasoning/thinking trace.** Nothing streams reasoning (`StreamDelta` carries `content` and `tool_calls` only), and only the Anthropic backend's non-streamed response exposes one, so the UI MUST NOT imply a trace is viewable when thinking is on. (Displaying it is DEC-18; Appendix B records what the pinned crate exposes as of M2a.)
- [ ] Thinking settings are part of the analysis cache key (FR-4.3), so flipping thinking never yields a stale cache hit. *(M2b: `AnalysisKey::digest` covers provider, model and thinking; tested.)*
- [ ] Thinking changes are shown in the status line so the user can never be unsure which mode produced an answer.

### FR-5 Chat

**FR-5.1 Sessions** — MUST — M3
One persistent chat session per `(repo, PR)`, resumable across runs; `:chat new` starts a fresh session while keeping the old one accessible (`:chat list` / `:chat open <id>`). Messages are append-only; edit/delete are MAY.
Acceptance criteria:
- [ ] History survives restart and is scoped to the PR; opening another PR shows its own history.
- [ ] Referenced paths in answers are clickable/jumpable to the diff or file.
- [ ] `:chat export <md|json>` writes a transcript.

**FR-5.2 Interaction** — MUST — M3
`Enter` sends, `Shift-Enter`/`Alt-Enter` inserts a newline (with documented terminal fallbacks), `Esc` cancels streaming, `:chat retry` regenerates the last answer, `r` on a message re-asks with the same prompt.

**FR-5.3 Grounding** — MUST — M3 — *DEC-2 `[DECIDED]`: no agentic tool loop in v1*
Chat SHALL use the same deterministic context bundle as analysis, plus the conversation. There is **no** tool loop in v1: the model sees exactly what `:context` reports and nothing more.
Acceptance criteria:
- [ ] When the answer needs a file that was not in the bundle, the app MUST surface that the user can add it (`:context add <path>`) and re-ask, rather than the model guessing.
- [ ] If agentic reads are ever added (M5+), they MUST go through a path allow-list (inside the workspace only, excluding ignored/secret files), be bounded by `max_tool_calls` (default 8), each call MUST be cancellable, and every read MUST be listed in `:context`.
- [ ] Chat answers MUST NOT be presented as verified facts about code the model was not given; the UI distinguishes "from provided context" vs "general knowledge" (prompt-level instruction + visible context summary).

**FR-5.4 Cost & limits** — SHOULD — M3
Per-session token usage is displayed; `max_tokens`/`temperature` are configurable per profile; an optional per-session cost estimate is shown when the provider reports pricing.

*Implementation note (M3):* a provider reports token usage on a streamed answer only if the request asks for it (`stream_options.include_usage` on the OpenAI-compatible route). The pinned `llm` crate sets that field for its native OpenAI backend alone, so the passthrough configuration sets it explicitly — Appendix B records the detail. Without it, this requirement would be met by an empty field on most of the catalog.

### FR-6 Review actions

**FR-6.1 Draft model** — MUST — M4 — *DEC-3 `[DECIDED]`: publish decision + body + batched inline comments*
A review exists as a local Draft containing `decision: Option<Decision>`, `body: Option<String>`, `comments: Vec<DraftComment { path, side: Old|New, line, start_line: Option<u32>, body }>`.
Acceptance criteria:
- [ ] Drafts persist across restarts and are per PR.
- [ ] The draft panel lists staged comments with file/line; `x`/`:draft remove` deletes one; `:draft clear` clears all (with confirmation).
- [ ] Staged comments are visible in the diff gutter as a distinct marker.

**FR-6.2 Inline comments** — MUST — M4 — *DEC-3 `[DECIDED]`: inline comments are published in v1*
From the diff pane the user can create a comment on a line or a selected range on the new side or the old side.
Acceptance criteria:
- [ ] The comment editor shows file, side and line range; empty bodies are rejected.
- [ ] A range selection requires start ≤ end on the same side and same file; invalid ranges are refused with a reason.
- [ ] The composer supports multi-line input, and MAY open `$EDITOR` via `:edit` (on exit, content is re-read; a terminal-restore failure MUST be recoverable).

**FR-6.3 Publishing** — MUST — M4 — *DEC-3 `[DECIDED]`: batched review via GraphQL*
Publishing MUST be explicit, confirmed, and atomic from the user's perspective.
Acceptance criteria:
- [ ] A publish modal shows the decision, the body, and every inline comment verbatim before sending.
- [ ] With inline comments present, publishing MUST use a **batched** review (create pending review with comments → submit with the decision) so the PR receives one review, not N comments. `gh pr review` only supports `--approve/--request-changes/--comment --body` (verified on gh 2.45), so batching requires `gh api graphql` (Appendix A).
- [ ] `gh pr review` is used only when there are no inline comments; the adapter exposes both paths behind one `submit_review` port method.
- [ ] Double-submit is prevented (in-flight guard + idempotency); the button is disabled after success.
- [ ] GitHub's "cannot approve/request changes on your own PR" and "no commits between" errors are surfaced as human sentences, with the draft preserved.
- [ ] After a successful publish, the draft is cleared, the PR detail is refreshed, and the status line confirms what was sent.

**FR-6.4 Existing discussion** — SHOULD — M4
Existing reviews, inline comments and replies MUST be visible (read-only in v1), grouped by thread, and reachable from the affected diff line.

**FR-6.5 Safety** — MUST — M4
No network-mutating action may occur without an explicit user confirmation. Destructive local actions (draft clear, workspace removal) also confirm. `--dry-run` (or `dry_run = true`) prints the exact `gh`/`git` commands that *would* run, and MUST be honored by all adapters.

### FR-7 UI shell

**FR-7.1 Modes** — MUST — M0
Modes: `normal` (navigation), `insert` (chat/comment composition), `command` (`:`), `search` (`/` and `?`), `popup` (help, leader menu, pickers, modals), `visual` (SHOULD, M1, for line/range selection). The active mode MUST be visible in the status line; `Esc` always returns to `normal` (a second `Esc` cancels the current operation).

**FR-7.2 Keybindings engine** — MUST — M0
Requirements: multi-key sequences with a configurable ambiguity timeout (`timeoutlen`, default 500 ms); `<leader>` expansion; per-mode tables plus a `global` table; unknown action IDs and duplicate bindings reported at startup as warnings naming the file, the section and the key (line numbers would need span-preserving parsing, `toml_edit` — DEC-19); the effective map is dumpable via `:keymap`. A `global` binding applies in every mode, except that inside a text-entry mode only combinations with a real modifier (`<C-…>`, `<A-…>`) are treated as bindings so that ordinary characters — including the leader key, `?` and `:` — are typed as text.
Acceptance criteria:
- [ ] Defaults are compiled in; user file only overrides (it never has to repeat defaults).
- [ ] A user binding can unbind a default (`"x" = "none"` or `action = "nop"`).
- [ ] `:keymap <action>` shows the current bindings for an action; `:keymap` lists all.
- [ ] Key notation supports modifiers and special keys: `<C-d>`, `<S-Tab>`, `<leader>`, `<Esc>`, `<CR>`, `<BS>`, `<Space>`, `<F1>`–`<F12>`.

**FR-7.3 Help & discoverability** — MUST — M0
`?` opens a help popup; `<leader>` opens a menu popup listing the available continuations with descriptions; `:` with no text opens a command palette with fuzzy completion. All three MUST be generated from the same action registry (FR-7.2) so documentation cannot drift.

The leader menu is a **hint**: it appears as soon as the leader is pressed rather than after `timeoutlen`, and the sequence stays open, so a continuation key still completes a longer binding and `Esc` dismisses it. Hints are declared in the registry, so another menu can opt in without special-casing an action id.

**FR-7.4 Command line** — MUST — M0/M1
Minimum commands: `:q`/`:qa`, `:help`, `:pr <N>`, `:filter <query>`, `:clear-filters`, `:sort <field>`, `:theme [<name>|next|reload]`, `:set <key>=<value>`, `:keymap`, `:model [show|save <name>|use <name>]`, `:key [set|clear] <provider>`, `:catalog refresh`, `:analyze [--force]`, `:context [add|remove <path>]`, `:chat new|list|export`, `:draft clear|remove`, `:workspace clean`, `:refresh`, `:doctor`, `:dry-run on|off`. Unknown commands produce an inline error; `Tab` completes.

**FR-7.5 Mouse** — MUST — M1
Enabled by default, toggleable via `:set mouse=false`. Scroll wheel scrolls the hovered/focused pane; left click focuses a pane; click selects a list row; click on the file tree toggles/opens; click on a diff line moves the cursor there (and is the entry point for a comment). Text drag-selection and right-click menus are MAY.

**FR-7.6 Status line, notifications, progress** — MUST — M0
Persistent left segment: mode, repo, PR, provider/model, draft count. Right segment: transient notifications (info/warn/error) with dedup and auto-expiry; errors persist until dismissed and are also written to the log. Long operations show a spinner with a label and `Esc` to cancel.

**FR-7.7 Theming** — MUST — M0/M1
Two built-in themes (`dark`, `light`) and any number of user themes in `~/.smart-review/themes/*.toml`. `<leader>t` opens the picker, which previews each theme as the cursor moves; `<leader>T` cycles to the next available theme, wrapping around, so N themes work without one binding per theme. `:theme <name>`, `:theme next` and `:theme reload` cover the same ground from the command line. The list of themes is read once at startup, so cycling never touches the disk (NFR-1.2). All colors MUST come from the theme — no hard-coded colors in widgets (enforced by review + a test that renders with an all-default theme).
Acceptance criteria:
- [ ] Themeable elements: app background/foreground, borders, titles, cursor line, selection, status line (normal/insert/command/error), notification levels, tree (dir/file/modified/added/deleted), diff (add, add-emphasis, del, del-emphasis, context, hunk header, line numbers, stale marker), comment/draft markers, chat (user/assistant/system), syntax tokens.
- [ ] Missing keys fall back to the theme's declared `base` (default `dark`), then to built-in defaults.
- [ ] Cycling with `<leader>T` visits every theme the picker lists, in the same order, and remembers the choice in `state.toml`.
- [ ] Invalid color values report the file, the element, the rejected value and the accepted formats (named, `#RRGGBB`, `#RGB`, `indexed:N`). Line numbers would need span-preserving parsing (`toml_edit`, DEC-19); the same trade-off as FR-7.2.
- [ ] Respecting `NO_COLOR` is MAY but if implemented must keep the UI usable.

**FR-7.8 Layout & responsiveness** — MUST — M0
Minimum supported size 80×24; below that show a single centered "terminal too small" panel. Panels collapse in priority order (comments → chat → analysis → file tree) as width shrinks; the diff pane is the last to go. Layout MUST reflow on `SIGWINCH` without losing cursor/scroll state.

### FR-8 Configuration & persistence

**FR-8.1 Directory layout** — MUST — M0
Root: `$SMART_REVIEW_HOME` if set, else `~/.smart-review`.
```
~/.smart-review/
  config.toml                 # app + LLM settings (optional; defaults compiled in)
  keybinds.toml               # overrides only (optional)
  credentials.toml            # API keys written by the in-TUI flow, chmod 0600 (optional but expected)
  themes/<name>.toml          # user themes (optional)
  state.toml                  # window/layout prefs, last repo+PR, seen markers, per-PR overrides
  cache/
    models.json                # models.dev catalog cache + fetch timestamp
    repos/<host>_<owner>_<name>/prs.json
    repos/<host>_<owner>_<name>/pr-<N>/detail.json
    repos/<host>_<owner>_<name>/pr-<N>/analysis-<head_sha>.json
    repos/<host>_<owner>_<name>/pr-<N>/chat-<session>.json
    repos/<host>_<owner>_<name>/pr-<N>/draft.json
  worktrees/<owner>-<repo>/pr-<N>/
  logs/smart-review.log       # rotated, max 5 files × 2 MiB
  README.md                   # generated on first run: where things live
```
Acceptance criteria:
- [ ] Directory and files are created on first run with correct permissions (dirs `0700`, `credentials.toml` `0600`).
- [ ] `credentials.toml` is written atomically and its mode is verified on every load; a group/world-readable file is reported as a warning and re-chmodded with consent (NFR-3.1).
- [ ] `$SMART_REVIEW_HOME` fully relocates everything, including worktrees and logs (integration tests use a temp root).
- [ ] No file is written under the user's repository working tree (`.git` included) except managed worktrees; no `.smart-review` is created in cwd.

**FR-8.2 `config.toml`** — MUST — M0
Sections: `[ui]` (theme, mouse, timeoutlen, leader, icons, date format, min sizes), `[review]` (default state, page size, max pages, context lines, whitespace, order), `[workspace]` (mode = `worktree|none`, root, keep-on-exit, auto-clean-days), `[llm]` (active provider/model/thinking, optional presets, max_context_tokens, max_file_bytes, max_tool_calls, temperature, max_tokens), `[catalog]` (url, ttl_hours), `[forge]` (remote, gh path, page size), `[cache]` (ttl_list_secs, ttl_detail_secs), `[log]` (level, path). Every key has a documented default **except the provider and model selection, which has no default by design (DEC-6)**; the app MUST start with an empty file, with no file at all, and with `[llm]` absent.
Example of the shape (values are defaults):
```toml
[ui]
theme = "dark"
mouse = true
leader = "<Space>"
timeoutlen = 500

[review]
state = "open"
page_size = 50
max_pages = 10
context_lines = 3
ignore_whitespace = false
order = "recommended"      # recommended | path

[workspace]
mode = "worktree"
keep_on_exit = true
auto_clean_days = 14

[catalog]
url = "https://models.dev/api.json"
ttl_hours = 24

[llm]
# No provider/model defaults: the user selects them in the TUI (<leader>m / :model).
# The keys below are limits; [llm.active] appears only after a selection is made.
max_context_tokens = 100000
max_file_bytes = 262144
max_tool_calls = 8

# Written by the picker. `reasoning` shape depends on the model's reasoning_options.
[llm.active]
provider = "deepseek"
model = "deepseek-chat"
temperature = 0.2
max_tokens = 4096
reasoning = { enabled = true, effort = "medium" }

# Optional, user-created via `:model save <name>`.
[llm.presets.cheap]
provider = "deepseek"
model = "deepseek-chat"
reasoning = { enabled = false }
```
Acceptance criteria:
- [ ] With `[llm]` absent, the app starts, browses PRs and reads diffs; only FR-4/FR-5 features prompt for setup.
- [ ] `[llm.active]` is written back by the picker without discarding unknown keys elsewhere in the file (FR-8.6). Preserving *comments* through a write requires `toml_edit` (DEC-19), which must be approved before M2 writes configuration; until then the app never rewrites the file.

**FR-8.3 `keybinds.toml`** — MUST — M0
```toml
[keys]
leader = "<Space>"
timeoutlen = 500

[keys.global]
"<C-c>" = "app.quit"
"?"     = "app.help"

[keys.normal]
"j"          = "nav.down"
"gg"         = "nav.top"
"<leader>a"  = "llm.analyze"
"<leader>rr" = "review.publish"
"]c"         = "diff.next_hunk"
```
- [ ] Sequences are matched longest-first with `timeoutlen` ambiguity resolution.
- [ ] `"<leader>"` is substituted at load time; `<leader>` inside a sequence is supported.
- [ ] Action IDs are validated against the action registry; a typo fails loudly at startup with a "did you mean" suggestion.
- [ ] The default map is documented in `docs/keymaps.md` generated from the registry.

**FR-8.4 Themes** — MUST — M0
```toml
name = "my-dark"
base = "dark"          # inherit, then override

[colors]
bg = "#1b1f23"
fg = "#c9d1d9"

[diff]
add = { fg = "#aff5b4", bg = "#033a16" }
add_emph = { fg = "#000000", bg = "#2ea043" }
del = { fg = "#ffdcd7", bg = "#67060c" }
del_emph = { fg = "#000000", bg = "#f85149" }
```
Acceptance criteria:
- [ ] A theme file with only two colors loads and produces a usable UI.
- [ ] Themes can be selected by file stem and hot-reloaded with `:theme reload`.

**FR-8.5 State & cache** — MUST — M0/M1
`state.toml` holds only small, user-meaningful state: last repo/PR, layout and pane sizes, current theme, sort/filter defaults, per-PR manual review-order overrides, seen/visited markers, session ids. Cached network payloads live under `cache/` and MUST be treated as disposable (deleting `cache/` never loses a draft or chat).

**FR-8.6 Config robustness** — MUST — M0
Unknown keys are preserved (not rewritten away) and reported as warnings; unusable values produce a message naming the file, the key and the expected type (TOML syntax errors also carry the line and column) and fall back to the default for that key, so the app still starts. A config file is never silently rewritten.

### FR-9 Diagnostics

**FR-9.1 Error handling UX** — MUST — M0
No IO/network/parse failure may panic or corrupt the terminal. Errors surface as a status-line notification plus a log entry; a modal offers details and a copyable command. The app MUST restore the terminal (raw mode, alternate screen, mouse capture, cursor) on every exit path, including panic (`catch_unwind`/`Drop` guard) and signals.

**FR-9.2 Logging** — MUST — M0
Structured logs to `<root>/logs/`, level from `--log-level`/`RUST_LOG`/config. Never log API keys, tokens, full request bodies, or file contents. `:doctor` reports the log path and the last N warnings.

**FR-9.3 `smart-review doctor`** — MUST — M1
Prints a checklist: git version, repo detection, remote detection, `gh` presence/version/auth/scopes, config/keybind/theme parse status, cache and log paths, workspace mode and writability, LLM profiles and key presence (presence only, never the value), terminal and locale info. Exit code 0 = ready, 1 = degraded, 2 = unusable.

---

## 4. Architecture constraints

### ARCH-1 Clean architecture and the dependency rule
Layers, inward-dependent only:
```
tui (presentation)  →  application (use cases)  →  domain
                             ↓ depends on
                          ports (traits)
                             ↑ implemented by
adapters (gh, git, llm, fs, clock, config)
```
- `domain` MUST NOT depend on any crate except `std`/`serde`/`chrono`/`thiserror`-class utilities (data formats and time, not IO), and MUST NOT know about terminals, processes or HTTP. `chrono` was approved in M1 in place of `time`; its `DateTime<Utc>` is the `Timestamp` type the fixtures, the cache and the tests share.
- `application` orchestrates use cases in terms of ports and emits domain events; it MUST NOT import `ratatui`, `crossterm`, or `tokio::process`.
- `adapters` implement ports and MUST NOT be imported by `application`/`domain`.
- `tui` MUST be a pure function of application state + a stream of events; it MUST NOT call adapters directly.

### ARCH-2 Ports (v1 sketch — names may change, responsibilities may not)
```rust
trait ForgePort {
    fn capabilities(&self) -> ForgeCapabilities;      // inline comments, threads, batched review…
    fn list_pull_requests(&self, q: PrQuery) -> Result<Page<PullRequestSummary>>;
    fn get_pull_request(&self, n: u64) -> Result<PullRequestDetail>;
    fn list_reviews(&self, n: u64) -> Result<Vec<Review>>;
    fn list_review_comments(&self, n: u64) -> Result<Vec<ReviewComment>>;
    fn list_checks(&self, n: u64) -> Result<Vec<CheckRun>>;
    fn submit_review(&self, n: u64, r: ReviewSubmission) -> Result<ReviewId>;   // dry-run aware
}

trait WorkspacePort {                                  // git
    fn detect_repo(&self) -> Result<RepoInfo>;
    fn ensure_workspace(&self, pr: &PullRequestRef) -> Result<Workspace>;       // { path, head_sha, merge_base }
    fn remove_workspace(&self, pr: &PullRequestRef) -> Result<()>;
    fn diff(&self, w: &Workspace, o: DiffOptions) -> Result<Vec<FileDiff>>;
    fn read_file(&self, w: &Workspace, p: &RelPath, max: usize) -> Result<FileContent>;
    fn list_files(&self, w: &Workspace) -> Result<Vec<RelPath>>;                // tracked, .gitignore-aware
}

trait LlmPort {
    fn analyze(&self, req: AnalysisRequest) -> Result<AnalysisStream>;   // streaming + cancel
    fn chat(&self, req: ChatRequest) -> Result<ChatStream>;
    fn probe(&self, sel: &ModelSelection) -> Result<ModelProbe>;         // verify provider+model+key; MAY
}

trait ModelCatalogPort {                       // models.dev, cached
    fn providers(&self) -> Result<Vec<CatalogProvider>>;
    fn models(&self, provider: &ProviderId) -> Result<Vec<CatalogModel>>;
    fn refresh(&self) -> Result<CatalogMeta>;  // respects ttl, offline-tolerant
}

trait CredentialsStore {
    fn get(&self, provider: &ProviderId) -> Result<Option<SecretString>>; // env override applied here
    fn set(&self, provider: &ProviderId, key: &SecretString) -> Result<()>;
    fn clear(&self, provider: &ProviderId) -> Result<()>;
    fn source(&self, provider: &ProviderId) -> Result<KeySource>;         // Env | File | None
}

trait ConfigStore { fn load(&self) -> Result<Config>; /* + keybinds, themes */ }
trait StateStore  { fn load(&self) -> Result<AppState>; fn save(&self, s: &AppState) -> Result<()>; }
trait Clock       { fn now(&self) -> Timestamp; }                               // testability
```
Acceptance criteria:
- [ ] Every port has an in-memory fake used by application tests; no test requires network, `gh`, or an API key.
- [ ] All network-mutating calls pass through a dry-run-aware boundary (FR-6.5).

### ARCH-3 Adapters (v1)
| Port | v1 adapter | Notes |
|---|---|---|
| `ForgePort` | `GhCliForge` | `gh pr list/view/diff/review`, `gh api`/`gh api graphql`. Requires gh ≥ 2.40. |
| `WorkspacePort` | `GitCliWorkspace` | `git fetch`, `git worktree`, `git diff`, `git merge-base`, `git ls-files`. |
| `LlmPort` | `LlmCrateProvider` | `llm` crate with `openrouter` + `deepseek` features; streaming. |
| `ConfigStore` | `TomlConfigStore` | `serde` + `toml`, `serde(deny_unknown_fields)` **off** (FR-8.6). |
| `StateStore` | `TomlStateStore` / JSON cache | atomic writes. |
| `Clock` | `SystemClock` / `FakeClock` | |

Process invocation rules: never `sh -c` with interpolated user input; pass argv arrays; capture stdout/stderr separately with a size cap; enforce a timeout; surface exit code + stderr tail in errors. `gh` is always invoked with `--repo <owner>/<name>` so cwd ambiguity cannot change the target.

### ARCH-4 Domain model (shape, not final code)
`PullRequestRef{repo, number}`, `PullRequestSummary`, `PullRequestDetail`, `Commit`, `CheckRun`, `Review`, `ReviewComment`, `ReviewThread`, `ReviewDecision{Approve|RequestChanges|Comment}`, `Draft`, `DraftComment`, `Workspace{path, head_sha, base_sha, merge_base}`, `FileDiff{old_path, new_path, status: Added|Deleted|Modified|Renamed|Binary|ModeOnly|Submodule, additions, deletions, hunks}`, `Hunk`, `DiffLine{kind, old_ln, new_ln, content, emphasis}`, `Analysis`, `ReviewPlan{groups: Vec<PlanGroup>}`, `ChatSession`, `Message`, `ContextBundle`, `ModelSelection{provider, model, base_url, reasoning: ReasoningOption, temperature, max_tokens}`, `ReasoningOption{Off | On | Effort(v) | Budget(u32)}` mirroring the catalog's `reasoning_options` shape.

### ARCH-5 Concurrency
- The **main thread** owns the ratatui terminal and the crossterm event loop, and drains a single `AppEvent` channel. It MUST NOT perform IO.
- A **tokio runtime** (multi-thread) runs LLM streaming and async process supervision. Blocking process/git work runs on `spawn_blocking` or a bounded thread pool (max 4 concurrent process jobs).
- All long operations are **jobs** with an id, a progress channel, and a cancellation handle; results carry their job id and are dropped if superseded or cancelled.
- No shared mutable state without a message; no `Arc<Mutex<App>>`. State transitions happen only on the main thread in response to `AppEvent`.
- Any new event variant MUST be handled exhaustively (no catch-all `_ =>` in the reducer).

### ARCH-6 Module layout (target)
```
src/
  main.rs            # wiring, CLI parsing, panic/terminal guard
  domain/            # pure types + invariants
  application/       # use cases: list_prs, open_pr, analyze, chat, publish_review, …
  ports/             # traits + DTOs
  adapters/
    gh/  git/  llm/  fs/  clock/
  tui/
    app.rs  event.rs  action.rs  update.rs
    components/  (list, diff, tree, chat, analysis, draft, popup, statusline)
    keymap/  theme/  layout.rs
```

### ARCH-7 Errors
`thiserror` enums per layer with context (`which command`, `which path`, `exit code`, `stderr tail`); `anyhow` only at the `main.rs`/CLI boundary. Ports return typed errors that the application maps to user-facing messages and notification levels. `unwrap`/`expect`/`panic!` are forbidden outside tests and genuinely-unreachable invariants (which must carry a justifying message).

### ARCH-8 Dependency policy
See DEV-1/DEP-1 in §8. Baseline expected dependencies (each still requires approval before adding): `ratatui`, `crossterm`, `tokio`, `serde`, `serde_json`, `toml`, `thiserror`, `anyhow`, `llm`, `clap`, `tracing`/`tracing-subscriber`, `unicode-width`, `similar` (only if a diff algorithm is needed beyond git's), `directories` (MAY be skipped in favour of explicit `$HOME` handling), and an HTTP client for the models.dev catalog — `reqwest` is already in the tree via `llm`, so reuse an exposed client if possible; otherwise prefer a minimal choice over adding a second HTTP stack (decide at M2, do not add two). **Explicitly not in v1:** `syntect` / `tree-sitter-*` (DEC-4 deferred syntax highlighting).

---

## 5. UI specification

### 5.1 PR list (M1)
```
┌ Smart Review ─ acme/service ─ open · 137 PRs ────────────────────────────────┐
│ filters: [is:open] [author:alice] query: "retry webhook"        / edit  x clr │
├──────────────────────────────────────────────────────────────────────────────┤
│  #    │ T │ Title                                  │ Author   │ Updated │ ✓   │
│  142  │   │ Add retry to webhook dispatcher        │ alice    │ 2h      │ 3/3 │
│ ▶141  │ ◌ │ WIP refactor of billing domain         │ bruno    │ 5h      │ 1/3 │
│  138  │   │ Bump tokio to 1.53                     │ dependabot│ 1d     │ 2/2 │
├──────────────────────────────────────────────────────────────────────────────┤
│ NORMAL │ acme/service │ #141 │ deepseek/deepseek-chat │ draft 2 │ ✓ synced    │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 5.2 Review screen (M1–M4)
```
┌ #141 refactor of billing domain ─────────────── [1 Diff] 2 Checks 3 Discussion 4 Chat 5 Analysis ┐
├────────────────┬──────────────────────────────────────────────────────────────────────────────────┤
│ Files (7)  +412│ src/domain/invoice.rs                                          modified  +18 −4 │
│ ▾ domain    (3)│ @@ -12,6 +14,9 @@ impl Invoice {                                                 │
│   M invoice.rs │      pub fn total(&self) -> Money {                                               │
│   A tax.rs     │  -        self.lines.sum()                                                        │
│   M money.rs   │  +        let gross = self.lines.sum();                                           │
│ ▾ app       (2)│  +        gross - self.discount                                                   │
│   M api.rs     │  +    }                                                                           │
│ ▸ infra     (2)│                                                                                   │
│ ── order: recommended (o toggles) ──────────────────────────────────────────────────────────────  │
├────────────────┴──────────────────────────────────────────────────────────────────────────────────┤
│ Analysis 1/3 · invoice::total now subtracts discount — rounding risk when discount is a Percent    │
├──────────────────────────────────────────────────────────────────────────────────────────────────┤
│ NORMAL │ draft: 2 comments · decision: approve? │ [leader] menu │ ? help                             │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
```

### 5.3 Leader popup, help, publish modal, chat (M0–M4)
```
 ╭ leader ─────────────────────────╮   ╭ publish review ────────────────────────╮
 │ a  analyze PR                   │   │ acme/service#141                       │
 │ c  chat about this PR           │   │ decision: REQUEST CHANGES              │
 │ d  diff options (split/context) │   │ body: (37 chars)                       │
 │ l  order: recommended / path    │   │ comments: 2                            │
 │ r  review (approve/…/publish)   │   │  src/domain/invoice.rs:31  …           │
 │ t  theme picker                 │   │  src/infra/pg.rs:88        …           │
 │ q  quit                         │   │ [Enter] publish   [Esc] cancel         │
 ╰─────────────────────────────────╯   ╰────────────────────────────────────────╯
```

### 5.4 Modes and default keymap
| Mode | Enter | Exit | Notes |
|---|---|---|---|
| `normal` | default | `Esc` | navigation, action prefixes |
| `insert` | from chat/comment compose | `Esc` | text entry; not vim-modal-editing |
| `command` | `:` | `Esc`, `<CR>` | line editor with completion |
| `search` | `/`, `?` | `Esc`, `<CR>` | incremental; `n`/`N` repeat |
| `popup` | any popup | `Esc` | keys scoped to the popup |
| `visual` | `v` (M5: line/range selection for comments, with publishing) | `Esc` | reserved; no action is bound yet |

Default bindings (all remappable; this is the compiled-in default set):
| Keys | Action | Scope |
|---|---|---|
| `j k` / `↓ ↑` | `nav.down` / `nav.up` | global lists & panes |
| `h l` / `← →` | `nav.left` / `nav.right` | focus & tree collapse |
| `g g` / `G` | `nav.top` / `nav.bottom` | |
| `<C-d>` `<C-u>` | `nav.half_down` / `nav.half_up` | |
| `<C-f>` `<C-b>` | `nav.page_down` / `nav.page_up` | |
| `Enter` | `nav.open` | open PR / open file |
| `Esc` | `nav.back` | popup → diff → list; on the list it clears filters and search |
| `/` | `search.open` | incremental, client side (DEC-20) |
| `Esc` | `search.close` | |
| `n` `N` | `search.next` / `search.prev` | |
| `:` | `app.command` | |
| `?` | `app.help` | also `<leader>?`; `?` is *not* backward search (DEC-20) |
| `<leader>` | `app.leader_menu` | shown immediately, sequence stays open (FR-7.3) |
| `Enter` | `nav.open` | open the selected PR |
| `<Tab>` | `pane.next` | tree ↔ diff (in the review screen) |
| `] c` `[ c` | `diff.next_hunk` / `diff.prev_hunk` | diff |
| `} {` | `diff.next_file` / `diff.prev_file` | diff |
| `z a` | `diff.toggle_hunk` | diff; on a file banner, the whole file |
| `<leader> d s` | `diff.toggle_split` | needs ≥ 140 columns (DEC-4, DEC-21) |
| `<leader> d c` | `diff.cycle_context` | 3 → 10 → 0 → 3; 0/3 are local-mode only |
| `<leader> d w` | `diff.toggle_whitespace` | local mode only; explains itself otherwise |
| `o` | `review_order.toggle` | diff |
| `c` | `review.comment_line` | diff (normal) |
| `v` | `visual.start` | diff |
| `R` | `app.refresh` | |
| `q` / `:q` / `:qa` | `app.quit` | |
| `<leader> a` | `llm.analyze` | |
| `<leader> c` | `chat.open` | |
| `<leader> T` | `theme.toggle` | next theme, wrapping |
| `<leader> f` | `filter.menu` | opens `:filter ` on the command line |
| `<leader> l` | `review_order.menu` | |
| `<leader> r r` | `review.publish` | |
| `<leader> r a` | `review.approve` | sets decision |
| `<leader> r c` | `review.request_changes` | |
| `<leader> r m` | `review.comment_only` | |
| `<leader> r x` | `review.discard` | with confirmation |
| `<leader> s` | `sort.menu` | opens `:sort ` on the command line |
| `<leader> m` | `model.picker` | provider → model → thinking |
| `<leader> t` | `theme.picker` | |
| `<leader> w` | `workspace.menu` | refresh/clean |
| `<leader> q` | `app.quit` | |

### 5.5 Mouse
| Gesture | Effect |
|---|---|
| wheel over pane | scroll that pane |
| left click pane | focus it (and move cursor to clicked row/line) |
| left click tree row | open file / toggle folder |
| left click list row then `Enter` | open PR |
| click status line segment | related action (mode → command, provider → profile picker) |
| `<C-click>` on file path in chat | open file at line |

---

## 6. Non-functional requirements

**NFR-1.1 Startup** — first render < 300 ms with warm cache, < 1.5 s cold on a 300-PR repo; `--version`/`--help` < 50 ms.
**NFR-1.2 Responsiveness** — the event loop MUST NOT block > 50 ms. All IO is job-based (ARCH-5).
**NFR-1.3 Large inputs** — 10 000-line diffs and 400-file diffs remain usable (virtualized rendering, no full re-layout per frame); memory should stay under ~300 MB for such a PR, excluding cached JSON.
**NFR-1.4 Cancellation** — every long operation is cancellable within 200 ms of `Esc`.
**NFR-2.1 Portability** — Linux and macOS are tier 1 (CI runs on Linux for now; the macOS CI job was removed until the pty steps are portable — see DEC-22). Terminal support: any xterm-compatible terminal with 256-color; truecolor used when detected.
**NFR-2.2 Dependencies on the environment** — requires `git` ≥ 2.30 and `gh` ≥ 2.40 on `PATH`; absence is a clean, explained failure, never a crash.
**NFR-2.3 Windows** — best effort; no tier-1 guarantees in v1; paths and process spawning MUST avoid Unix-only assumptions where cheap to do so.
**NFR-3.1 Secrets** — API keys are stored only in `${SMART_REVIEW_HOME}/credentials.toml` (`0600`, atomic write) and MUST never appear in `config.toml`, cache, logs, error messages or the UI. Key entry is masked; overwriting an existing key requires confirmation; `:key clear <provider>` deletes it. `doctor` and `:model show` report key presence and source (`env` vs `file`), never the value. Provider environment variables (per the catalog's `env` field) are still honored and take precedence when set.
**NFR-3.2 Privacy** — the only outbound data is to the chosen LLM provider and to GitHub via `gh`. No telemetry, no update pings. `:context` shows exactly what leaves the machine; excluded paths (`.env*`, ignored files, oversize/binary) are listed explicitly.
**NFR-3.3 Command safety** — argument-array process spawning only (ARCH-3); a config-supplied `gh` path is validated to be an executable file; no shell interpolation of PR titles, branch names, or file paths.
**NFR-3.4 Publishing safety** — irreversible actions require the confirmation modal (FR-6.3); drafts survive failed publishes.
**NFR-4.1 Resilience** — network failures, `gh` rate limits, malformed JSON, expired tokens and missing workspaces produce retry-able, explained states. Drafts and chat history are never lost to a crash (atomic writes).
**NFR-4.2 Terminal integrity** — the terminal is restored on normal exit, error exit, panic and SIGINT/SIGTERM.
**NFR-5.1 Maintainability** — `cargo fmt` clean; `cargo clippy -- -D warnings` clean; no file over ~800 lines without justification; domain/application unit-test coverage of branches that encode requirements.
**NFR-5.2 Testability** — ports have fakes; time and randomness injected; no test performs network IO or requires `gh` (adapter contract tests are opt-in behind a feature/env var).
**NFR-5.3 Observability** — tracing spans per job (`job id`, kind, duration, result); log level from config.

---

## 7. Data formats

### 7.1 Analysis document (cached JSON; also the LLM's required output schema)
```json
{
  "version": 1,
  "prompt_version": 1,
  "model": "deepseek/deepseek-chat",
  "head_sha": "9f2ac1e…",
  "created_at": "2026-01-01T00:00:00Z",
  "token_usage": { "prompt": 0, "completion": 0 },
  "summary": "string (what changed, ≤ 6 sentences)",
  "intent": "string (inferred goal and motivation)",
  "risk_areas": [
    { "title": "Rounding in Money arithmetic", "severity": "high|medium|low", "files": ["src/domain/money.rs"], "why": "string" }
  ],
  "review_plan": [
    { "order": 1, "group": "domain", "rationale": "string", "files": ["src/domain/invoice.rs"] }
  ],
  "per_file_notes": [
    { "path": "src/domain/invoice.rs", "change": "string", "notes": "string", "review_focus": ["string"] }
  ],
  "suggested_questions": ["string"]
}
```
Rules: unknown fields tolerated; `review_plan[].files` MUST be a subset of the changed files after normalization (unknowns dropped with a warning; missing files appended to an `unclassified` group); `version` enables migrations.

### 7.2 Draft document
```json
{ "pr": 141, "decision": "request_changes", "body": "…",
  "comments": [ { "path": "src/domain/invoice.rs", "side": "new", "line": 31, "start_line": 28, "body": "…" } ],
  "updated_at": "…" }
```

### 7.3 Prompt contract (normative properties, wording is free)
- System prompt: role (senior reviewer), output format (the JSON schema above, no prose outside JSON for analysis), grounding rules (only claim what is in the provided context and name the file when asserting), language (mirror the PR's language), and the repository conventions extracted from `AGENTS.md`.
- Analysis is requested once for the whole PR; per-file explanation is a separate, smaller request (FR-4.1 granularity, M2).
- **The analysis is one streamed request, and a repair is a second one** (M2b): the first answer is normalized against the diff, and only a failure to parse triggers the retry, which quotes the reason and the previous text. A model that answers with prose is a normal event, so the raw text is kept and shown (`:analyze raw`) whether or not the retry worked.
- Chat uses a separate system prompt: cite file paths for claims, admit uncertainty, prefer asking for a file when the context lacks it (only if tool use is enabled), never fabricate line numbers.
- Every request records `prompt_version`; changing prompt semantics bumps it and invalidates cache.

### 7.4 Credentials document (`credentials.toml`)
```toml
# Written by the in-TUI key-entry flow (FR-4.5). Mode 0600. Never logged, never cached.
version = 1

[providers.deepseek]
api_key = "sk-…"

[providers.openrouter]
api_key = "sk-or-…"
```
Rules: keys are keyed by **catalog provider id**; a provider's documented environment variable shadows the file value and the shadowing is reported in the UI; unknown provider ids are preserved; writes are atomic and file mode is verified on every load.

### 7.5 Model catalog cache (`cache/models.json`)
A copy of `https://models.dev/api.json` plus a fetch timestamp, disposable and TTL'd (`catalog.ttl_hours`, default 24). Fields the app consumes:
```jsonc
{
  "fetched_at": "2026-01-01T00:00:00Z",
  "providers": {
    "openrouter": {
      "id": "openrouter", "name": "OpenRouter",
      "env": ["OPENROUTER_API_KEY"],        // key env var(s)
      "api": "https://openrouter.ai/api/v1", // base URL for passthrough (DEC-17)
      "doc": "https://…",
      "models": {
        "deepseek/deepseek-chat": {
          "id": "deepseek/deepseek-chat", "name": "…", "family": "…",
          "reasoning": true,
          "reasoning_options": [ { "type": "toggle" },
                                 { "type": "effort", "values": ["low","high","max"] },
                                 { "type": "budget_tokens" } ],
          "tool_call": true, "structured_output": true, "temperature": true,
          "limit": { "context": 1000000, "output": 384000 },
          "cost": { "input": 0.15, "output": 0.6, "reasoning": 0.6, "cache_read": 0.003 },
          "modalities": { "input": ["text"], "output": ["text"] },
          "release_date": "…", "last_updated": "…"
        }
      }
    }
  }
}
```
Rules: unknown fields are tolerated and preserved; `cost` values are per 1M tokens and used only for clearly-labelled estimates; the cache never overrides a provider error and is never a correctness dependency.

---

## 8. Working agreements (for the implementing agent)

- **DEV-1** Rust edition 2024; MSRV **1.88** (not the edition minimum of 1.85: ratatui 0.30.2 declares `rust-version = 1.88.0` and is the highest in the tree, verified at M0); `rust-version` is set in `Cargo.toml`; CI builds and tests on stable.
- **DEV-2** Add every dependency with `cargo add <crate>` (latest compatible), never by hand-editing `Cargo.toml`.
- **DEP-1** **Ask the project owner before adding any new dependency**, stating: crate, version, purpose, size/transitive weight, and why the standard library or an existing dependency is insufficient. This applies to `[build-dependencies]` and `[dev-dependencies]` too.
- **DEP-2** Prefer one well-maintained crate over two narrow ones; avoid crates that require `unsafe` FFI unless approved explicitly.
- **DEV-3** Work in milestone order (M0 → M5). Do not start a milestone before the previous milestone's acceptance criteria pass, unless the owner redirects.
- **DEV-4** Every behavior change ships with tests at the lowest sufficient level; requirement IDs are referenced in test names or module docs where practical (e.g. `/// FR-3.2`).
- **DEV-5** Keep this file current: when an answer to a DEC item is given, update the FRs and §11.1 in the same change. Never leave a `[PROPOSED]` marker on something implemented as decided.
- **DEV-6** Never commit or push unless asked; never run a network-mutating command against a real PR outside a dry-run unless the user asked for it.
- **DEV-7** Any user-facing string is written in English, sentence case, and states the next action ("run `gh auth login` to continue"), not just the failure.
- **DEV-8** Lint policy lives once in `Cargo.toml` under `[workspace.lints]`, with crates opting in via `[lints] workspace = true`. It enables `clippy::pedantic`, `missing_errors_doc`, `missing_panics_doc`, `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable` and `dbg_macro`, and forbids `unsafe_code`. CI runs clippy with `-D warnings`, so every warning is an error there. Fix the code rather than silencing the lint; an `#[allow]` must carry a comment explaining why the lint is wrong in that spot and must be scoped to the item, never the module or crate.

---

## 9. Testing & quality gates

- **Unit** (fast, no IO): unified-diff parser (including renames, binary, mode-only, submodule, CRLF, no-newline-at-EOF, `\ No newline` markers); keymap sequence resolution and conflict detection; config/keybind/theme parsing and fallbacks; filter-query builder; analysis JSON normalization/repair; context-bundle truncation and redaction; layer-order heuristic; token estimator.
- **Snapshot** (ratatui `TestBackend`): PR list, diff pane (unified, split, empty, too-small terminal), analysis panel, chat, leader/help popups, publish modal, status-line variants. Snapshots are the primary defense against UI regressions.
- **Application** (fakes): every use case, including cancel-mid-stream, stale-head-SHA invalidation, publish failure preserving the draft, offline paths, and superseded-job discard.
- **Adapter contract** (opt-in, `--features contract-tests`, requires network/auth): `gh` command construction assertions via a fake `gh` executable on `PATH` for the offline part; live tests for `list/detail/diff` only. No live test may mutate a real PR except a designated sandbox repo behind `SMART_REVIEW_CONTRACT_REPO`.
- **Manual E2E checklist** per milestone (Appendix C).
- **Gates**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and a docs check that generated `docs/keymaps.md` matches the registry.

---

## 10. Milestones

Each milestone is "done" when its FR acceptance criteria pass, tests exist, and the manual checklist passes.

**M0 — Walking skeleton.** CLI + env detection (FR-1.1/1.2), config/keybinds/themes loading with defaults (FR-8.x), TUI shell with modes, leader menu, help popup, command line basics, status line, terminal guard (FR-7.1–7.4, 7.6–7.8, FR-9.1/9.2), `:q` works, empty screens render themed placeholders. *Demo:* launch, open help, change theme, quit cleanly.

**M1 — PR browsing and diff reading.** PR list + pagination + filters + client-side search (FR-2.x), detail fetch (FR-2.4), diff fetch/render/navigation without a local workspace (FR-3.2 remote mode, FR-3.3, FR-3.4), mouse (FR-7.5), themes on real content, cache, doctor (FR-9.3). *Demo:* find PR by title+author, read its full diff, navigate hunks, quit.

**M2 — Workspace + LLM analysis + ordered review.** Workspace creation (FR-3.1, DEC-1), local diff with context/whitespace toggles, analysis request/stream/cache (FR-4.1–4.4), review plan panel and ordered diff (FR-3.5, FR-4.2), in-TUI provider/model/thinking picker backed by the models.dev catalog, key entry and storage (FR-4.5, FR-4.7, FR-4.8), context bundle + `:context` + privacy guardrails (FR-4.6). *Demo:* analyze a real PR, see the review order change, toggle back to path order, re-open and get a cache hit.

**M3 — Chat.** Sessions, streaming, cancellation, retry, export, persistence (FR-5.x) over the deterministic context bundle (no tool loop, DEC-2). *Demo:* ask a question about a symbol that exists in the repo, get a cited answer, restart and find the history.

**M4 — Review publishing.** Draft model + inline comment UX + existing discussion display (FR-6.1/6.2/6.4), batched publish with confirmation and error handling (FR-6.3), dry-run for everything (FR-6.5). *Demo:* stage three inline comments, publish a `RequestChanges` review as a single review on a sandbox PR, verify on GitHub, confirm the draft is cleared.

**M5 — Polish & backlog.** Visual-mode ranges, thread replies/resolution, manual review-order overrides, `NO_COLOR`, agentic file reading for chat (DEC-2 backlog), syntax highlighting (requires a new decision + dependency approval), docs (`README.md`, `docs/keymaps.md`, `docs/themes.md`), packaging/release (versioned binary, `--version`).

---

## 11. Open decisions

> **Every row below is `[PROPOSED]` until it is marked `[DECIDED]`.** Answering a row means flipping it to `[DECIDED]` in this table, updating the referenced FRs, and appending a line to §11.1. Do not implement a proposed default that has not been confirmed.

| ID | Question | Proposed default | Impact if changed |
|---|---|---|---|
| **DEC-1** | How is the PR head materialized on disk? | `[DECIDED]` **Managed git worktree** at `~/.smart-review/worktrees/<owner>-<repo>/pr-<N>`, detached at the head SHA; the user's working tree, index, HEAD and branches are never modified. | Touches FR-3.1, FR-3.2, FR-4.6, persistence layout, safety story. |
| **DEC-2** | Which LLM capabilities are in v1: analysis only, analysis + chat, or analysis + chat + agentic file reading (tool loop over the repo)? | `[DECIDED]` **Analysis + chat** over a deterministic context bundle. Agentic file reading is deferred to M5+ (cost, latency, privacy predictability); its guardrails are pre-specified in FR-5.3. | FR-5.3, ARCH-2 `LlmPort` (no tool surface in v1), prompt design, M2/M3 scope. |
| **DEC-3** | Review publishing scope in v1: (a) local drafts only, (b) publish with `gh pr review` (no inline comments), (c) publish decision + body + batched inline comments via GraphQL. | `[DECIDED]` **(c)** — one batched review containing decision, body and inline comments, via `gh api graphql`; `gh pr review` only as the no-inline-comment shortcut. | FR-6.1–6.3, Appendix A, and the amount of GitHub API surface to absorb. |
| **DEC-4** | Diff presentation: unified only, unified + side-by-side toggle, and is syntax highlighting required? | `[DECIDED]` **Unified default + side-by-side toggle at width ≥ 140**, delivered in M1. **Syntax highlighting deferred** (large dependency + per-language risk); no highlighting crate in v1. | FR-3.3, ARCH-8 dependency list, M1 scope. |
| **DEC-5** | Where do LLM API keys live, and how are they set? | `[DECIDED]` Entered **in the TUI** (masked prompt inside the model picker) and saved to `${SMART_REVIEW_HOME}/credentials.toml` at `0600`. Provider env vars are honored as an override and the active source is shown in the UI. Keychain integration deferred. | FR-4.5, FR-8.1, §7.4, NFR-3.1, `--check`/doctor behavior. |
| **DEC-6** | Default models per provider, and how many profiles? | `[DECIDED]` **No defaults at all.** On first run the user must choose provider + model + thinking in the TUI; provider/model metadata comes from `https://models.dev/api.json` (cached), which also supplies reasoning options, context limits and cost (FR-4.7). Presets are optional and user-created. | FR-4.5, FR-4.7, FR-4.8, §7.5, M2 scope. |
| **DEC-7** | Is `gh` a hard requirement, or should there be a fallback path (GitHub REST via token, or degraded “no-forge” mode with local git only)? | **`gh` is required** for v1 (it reuses the user's existing auth); a clear error otherwise. | FR-1.1, ARCH-3, offline story W4. |
| **DEC-8** | Milestone priority if time is short: is M2 (analysis + ordered review) more valuable than M3 (chat), and is M4 (publishing) allowed to slip past v1? | Order as written: M2 > M3 > M4, all in v1. | Roadmap, and whether v1 can ship read-only. |
| **DEC-9** | Chat history retention: unlimited on disk, or capped (e.g. last 50 sessions / 2 MB per PR)? | **Cap per PR** (50 sessions, 2 MB each) with `:chat export` before pruning; pruning is announced. | FR-5.1, FR-8.5, disk growth. |
| **DEC-10** | Review ordering source: pure LLM plan, LLM plan + heuristic fallback, or heuristic only? | **LLM plan + heuristic fallback**, user override wins, cached per head SHA. | FR-3.5, FR-4.2/4.3, behavior without an LLM. |
| **DEC-11** | State file format: TOML (human-editable, consistent with config) or JSON (structured, atomic-write friendly)? | **TOML for state**, JSON for cache payloads. | FR-8.1/8.5, tooling. |
| **DEC-12** | Is Windows a supported target in v1? | **No tier-1**; best-effort only. | NFR-2.3, CI matrix, path/process handling. |
| **DEC-13** | Can the user switch repositories inside the app (picker over recent repos), or is one session = one repo? | **One repo per session** with `:repo` to relaunch/switch explicitly (recent list in state). | TUI navigation, cache keys, workspace lifecycle. |
| **DEC-14** | Offline behavior: read-only cached mode (proposed) or hard failure with a retry? | **Cached read-only mode** with an offline indicator; publishing and analysis disabled. | FR-2.3, NFR-4.1, user trust. |
| **DEC-15** | When a workspace must be re-fetched after new commits, do we auto-refresh the analysis (costs money) or ask? | **Ask**, showing the new commit range and the cost implication. | FR-4.3, UX trust. |
| **DEC-16** | Scope of comment targets: PR conversation comments (top-level) in addition to inline and review-body? | Inline + review body in v1; top-level PR comments are M5. | FR-6.1/6.3, `gh` surface. |
| **DEC-17** | *(decided)* How do catalog provider ids (213 of them) map onto `llm` crate backends? | **Curated mapping** for the crate's native backends (openrouter, deepseek, openai, anthropic, google, groq, mistral, xai, ollama, …), and **OpenAI-compatible passthrough** using the catalog's `api` base URL + `env` key for the rest. Providers needing special auth (Bedrock, Vertex, Azure) are excluded in v1. | FR-4.5, FR-4.7, ARCH-2 `ModelCatalogPort`, surface area and support burden. |
| **DEC-18** | Should the reasoning/thinking trace be displayed in the UI? | **No in v1** — the `llm` crate exposes only assistant text and tool calls. Revisit if upstream surfaces `reasoning_content`, or via a dedicated provider adapter. Until then the UI shows thinking *settings* and *token counts* only. | FR-4.8, prompt/UX expectations, possible upstream contribution. |
| **DEC-19** | *(decided)* How does the model picker write `[llm.active]` back without destroying the user's file? | **Add `toml_edit` in M2** and edit the document in place, so comments and formatting survive. Alternative: keep never writing configuration and require the user to edit it by hand. `toml` 1.x has no comment-preserving API (verified at M0). | FR-4.5, FR-8.2, FR-8.6, the M2 dependency ledger in PLAN.md §5. |
| **DEC-20** | `?` was listed both as help and as backward search. Which wins? | **Help.** `?` is the TUI convention for the keybinding popup and the app already shows `? help` in its status line. Backward search entry is dropped; `N` repeats a search backwards, and `/` re-opens the prompt. Recorded because the same key cannot mean two things and silently picking one later would change a habit. | FR-7.3, FR-7.4, the §5.4 keymap. |
| **DEC-21** | Are the diff options a submenu popup or leader continuations? | **Continuations**: `<leader>d s`, `<leader>d c`, `<leader>d w`. The keymap engine already resolves multi-key sequences and the leader menu lists them, so a popup would add a mode for no gain. The cost is that `<leader>d` alone does nothing (like vim's `g`), which the leader menu makes discoverable. | FR-3.2, FR-3.3, FR-7.2, the §5.4 keymap. |
| **DEC-22** | *(decided)* Is macOS kept in CI alongside Ubuntu? | **Not for now.** The milestone validators (m0–m2b) only run on Ubuntu because they need GNU `script`, so the macOS job ran only fmt/clippy/test/build and added wall-clock without covering the milestone gates. CI is a single Ubuntu `tests` job until the pty steps are portable. | NFR-2.1, the CI workflow. |

### 11.1 Decision log
| Date | ID | Decision | By |
|---|---|---|---|
| — | DEC-1 | PR head is materialized in an app-owned `git worktree`, detached at the head SHA; the user's clone is never modified. | owner |
| — | DEC-2 | v1 ships analysis + chat over a deterministic context bundle; agentic file reading deferred to M5+. | owner |
| — | DEC-3 | v1 publishes one batched review (decision + body + inline comments) via `gh api graphql`. | owner |
| — | DEC-4 | Unified diff by default, side-by-side toggle at ≥140 cols, no syntax highlighting in v1. | owner |
| — | DEC-5 | Provider/model/thinking are configured **inside the TUI**; keys are entered there and saved to `credentials.toml` (0600), with env vars as an override. | owner |
| — | DEC-6 | No default provider or model. The user must configure one; the model list comes from `https://models.dev/api.json`, which also drives thinking options, limits and cost. | owner |
| — | DEC-19 (decided) | Config write-back in M2a needs `toml_edit` to preserve comments; recorded here so the dependency is approved with the M2 batch rather than discovered mid-implementation. | — |
| 2026-09-12 | DEC-22 (decided) | macOS CI removed for now: the milestone validators need GNU `script` and only ran on Ubuntu, so the macOS job added time without covering the milestone gates. CI is a single Ubuntu `tests` job. | owner |

---

## Appendix A — Command map (v1 adapters)

Verified against `gh` 2.45 / `git` 2.43 on the development machine. `gh` always receives `--repo <owner>/<name>`.

| Use case | Command |
|---|---|
| Repo detection | `git rev-parse --show-toplevel`, `git remote -v`, `git rev-parse --abbrev-ref HEAD` |
| gh readiness | `gh --version`, `gh auth status` |
| List PRs | `gh pr list --state open --limit N --search "sort:created-desc <user query>" --json number,title,author,createdAt,updatedAt,isDraft,baseRefName,headRefName,headRefOid,additions,deletions,changedFiles,labels,reviewDecision,statusCheckRollup,url` |
| PR detail | `gh pr view N --json <fields>` (see FR-2.4) |
| PR commits | `gh pr view N --json commits` |
| Checks | `gh pr view N --json statusCheckRollup` |
| Reviews | `gh pr view N --json reviews,latestReviews` |
| Inline comments | `gh api --paginate repos/{owner}/{repo}/pulls/N/comments` |
| Fetch head | `git fetch origin <baseRefName> refs/pull/N/head --no-tags` |
| Base SHA / merge base | `git rev-parse refs/remotes/origin/<baseRefName>`, `git merge-base <base> <head>` — **note: `gh pr view --json` has no `baseRefOid` on gh 2.45, so the base SHA must come from git** |
| Worktree | `git worktree add --detach <path> <head_sha>`, `git worktree remove <path>`, `git worktree prune` |
| Diff (local) | `git -C <ws> diff --unified=<n> [--ignore-all-space] --find-renames --no-color --no-ext-diff <merge_base> <head_sha>` |
| Diff (remote fallback) | `gh pr diff N --patch --color never` |
| File list | `git -C <ws> ls-files --cached --others --exclude-standard` |
| File content | `git -C <ws> show <head_sha>:<path>` (preferred: reads exactly the PR revision, independent of worktree state) |
| Publish (body/decision) | `gh pr review N --approve|--request-changes|--comment --body-file -` (body via stdin) |
| Publish (batched with inline comments) | `gh api graphql` → `addPullRequestReview(input:{pullRequestId, body, comments:[{path, line, side, startLine?, body}]})` then `submitPullRequestReview(input:{pullRequestReviewId, event: APPROVE|REQUEST_CHANGES|COMMENT})`; `<owner>/<repo>/pull/<n>` PR node id via `gh api repos/.../pulls/N --jq .node_id` |
| Current user | `gh api user --jq .login` (for own-PR detection, FR-6.3) |

## Appendix B — Prompt & model notes

### B.1 `llm` crate (verified against 1.3.8 source)
- Features include `openrouter` and `deepseek` (plus `openai`, `anthropic`, `google`, `groq`, `mistral`, `xai`, `ollama`, `cohere`, `huggingface`, `azure_openai`, `bedrock`, …). The crate is async and needs a tokio runtime (ARCH-5).
- Metadata is provided by `src/bin/llm-cli/provider/registry.rs` and `capabilities.rs`; check the `SUPPORTS_REASONING_EFFORT` capability flag before offering effort for a provider.
- **Reasoning is supported outbound:** `LLMBuilder::reasoning(bool)`, `.reasoning_effort(ReasoningEffort::{Low,Medium,High})`, `.reasoning_budget_tokens(u32)`; `Usage.reasoning_tokens` reports reasoning consumption. `ReasoningEffort` lives in `llm::chat`.
- **Reasoning text is returned by *some* backends, verified at M2a against 1.3.8.** `StreamDelta` has only `content` and `tool_calls`, so nothing streams reasoning; the non-streamed `ChatResponse` trait has a `thinking()` hook that the **Anthropic** backend implements (from `type: "thinking"` content blocks) and the OpenAI-compatible path does not. `adapters/llm.rs` therefore passes a trace through when one is present and never claims one exists. FR-4.8's rule stands as written: the UI MUST NOT imply a trace is viewable (DEC-18), and this is the state to revisit if a `reasoning_content` field appears upstream — models.dev already publishes which providers stream it (`interleaved.field`).
- Note the crate offers only three effort levels, while the catalog may advertise values such as `max` or `budget_tokens`. The mapping and its refusals are specified in FR-4.8.
- Re-export paths and the streaming API were re-verified at M2a against `llm` 1.3.8 and are as recorded here. `LLMBuilder` is reached at `llm::builder::{LLMBuilder, LLMBackend}` and the chat traits at `llm::chat::*`; the crate's `providers::openai_compatible` module is the shared implementation behind the passthrough route (DEC-17), which is why `LLMBackend::OpenAI` with an explicit `base_url` covers the OpenAI-compatible providers.

### B.2 Request shaping
- Analysis = one async request returning JSON (schema §7.1) with a repair retry; per-file explanation = one small request per file, cached individually (FR-4.1); chat = streaming, history trimmed per FR-4.6.
- Streaming MUST be used for analysis and chat (FR-4.4).
- Pin the resolved crate version in `Cargo.toml` and record it plus the re-verified API surface here at M2. The `agent` feature is **not** needed in v1 (DEC-2: no tool loop).
- **The passthrough route must not use `LLMBackend::OpenAI` (verified at M2b, `llm` 1.3.8).** That backend sends both chat and streaming to the **Responses API** (`{base_url}/responses`), which OpenAI implements and most "OpenAI-compatible" providers do not — pointing it at another provider's `api` URL 404s on every request. The passthrough therefore builds `providers::openai_compatible::OpenAICompatibleProvider<T>` directly with a `T` whose `CHAT_ENDPOINT` is `chat/completions`; the crate's own OpenAI-compatible backends (openrouter, mistral, groq, cohere, xai, huggingface) are the same generic provider, which is why only the OpenAI entry needed the workaround. `adapters/llm.rs` holds the adapter as `Box<dyn ChatProvider>` (the crate's `LLMProvider` is a `ChatProvider`; the box upcast is stable since Rust 1.86) so both kinds travel the same path.
- **Streamed answers carry no usage unless the request asks (verified at M3, `llm` 1.3.8).** The crate sets `stream_options: {include_usage: true}` only for its *native* OpenAI backend (`OpenAI::SUPPORTS_STREAM_OPTIONS = true`); `OpenAICompatibleProvider<T>` defaults it to `false`. Without that field a provider reports no token counts on a streamed answer, so FR-4.8's accounting and FR-5.4's per-session cost would be permanently empty on the whole passthrough route (most of the catalog, DEC-17). The passthrough configuration in `adapters/llm.rs` therefore sets `SUPPORTS_STREAM_OPTIONS = true`, and the M3 validator asserts the field is on the wire.
- Extend Appendix C at M2 with: pick a provider/model with no key → masked prompt → key stored `0600` → analysis runs; then flip thinking on/off and confirm the analysis cache misses.
- **Chat keeps the context in the system prompt (M3).** Not a requirement, a consequence of one: FR-4.6's truncation order ends with "oldest chat turns dropped", and a conversation whose first message *is* the context cannot drop turns without dropping its subject. The role separation in the crate (`ChatRole` has no `System`) is why the context goes through the builder rather than into the first user message, which is also where the analysis prompt puts it.

### B.3 Budgeting
- Token estimation: `ceil(bytes / 4)` is acceptable until a real tokenizer is approved as a dependency.
- `limit.context` from the catalog (FR-4.7) is preferred over a fixed `max_context_tokens` when the active model declares one.

## Appendix C — Manual E2E checklist (run at each milestone)

- [ ] Fresh home (`SMART_REVIEW_HOME=$(mktemp -d)`) starts, creates the layout, and works with zero config.
- [ ] Not a git repo / no GitHub remote / no `gh` / `gh` not authed → four distinct, actionable messages.
- [ ] 80×24 terminal is usable; resizing never loses the cursor position.
- [ ] `Ctrl-C`, `SIGTERM`, and a forced panic all restore the terminal.
- [ ] Kill the network mid-analysis and mid-chat → cancellable, explained, no lost draft.
- [ ] `<leader>a` on a real pull request: the estimate and the file list first, then the analysis streaming in, then the tree in the recommended order; `o` returns to path order, `J`/`K` moves a group, and restarting the app shows the analysis from cache without calling the provider (verify in `logs/`).
- [ ] `<leader>c` on a real pull request: ask something the diff answers and something it does not, watch the second answer say what is missing, `:context add <that file>` and ask again; `Esc` mid-answer stops it and keeps what arrived; restarting shows the conversation; `:chat export md` opens in an editor as something a colleague could read.
- [ ] `--dry-run` prints the exact commands for fetch, worktree, and publish and performs no mutation.
- [ ] Second launch reuses cache and workspace and makes no unnecessary network calls (`:` shows the job log).
