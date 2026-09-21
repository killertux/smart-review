# Product behavior

`smart-review` is a keyboard-first GitHub pull-request review client. It keeps source
inspection local and revision-pinned, uses an LLM only with an inspectable bounded
context, and makes every remote write pass through a verbatim confirmation preview.

## Review journey

The list combines server-side filter chips with a client-side `/` search over fetched
rows. Opening a PR first shows cached data when available, refreshes metadata and the
remote final-state diff, then prepares a detached worktree in app-owned Git storage.
The status line identifies whether the displayed patch came from GitHub or the local
worktree. Context-line and whitespace toggles require the local patch.

The review screen has five real destinations:

| Tab | What it shows |
|---|---|
| **Overview** | PR metadata, concise analysis brief, risks, review steps, coverage, limitations, and suggested questions |
| **Files** | Ordered file tree, diff, inline threads/drafts, What/Why/Verify guidance, and explicit human progress |
| **Checks** | Actual GitHub check runs and conclusions |
| **Discussion** | Reviews and inline threads, including resolved and outdated threads with filters |
| **Ask** | Grounded, persistent conversations about the same context used for analysis |

All tabs are reachable by `1`–`5`, the tab cycle, or mouse. The same layout geometry
drives rendering and hit testing. At 80×24 the compact guidance keeps code visible;
wide terminals can use split diff view.

## Guided review and human state

Without an analysis, files use a deterministic heuristic order. An analysis adds a
brief and contextual, human-named review steps; contracts or migrations may lead, and
tests can accompany the behavior they verify. `o` switches between suggested and path
order, while manual group/file moves override the suggestion until reset.

Files show concise What, inferred Why, and Verify guidance. Validated evidence can jump
to an exact source coordinate; inference is labelled rather than presented as fact.
Suggested questions populate Ask for editing and never send automatically.

Human progress is explicit: not reviewed, reviewed, or needs revisit. It is stored with
a file-change fingerprint outside analysis cache. A new head preserves a reviewed mark
only for a provably identical change; changed reviewed files become needs revisit.
Compatible manual order is retained, and incompatible order is reset with an explanation.

## Analysis, context, and cost

No model is selected by default. `<leader>m` chooses a provider, model, and supported
thinking setting from the cached models.dev catalog, accepts a masked key, and performs
a connection check. File keys use mode `0600`; a provider environment variable overrides
the file and only its presence/source is displayed.

The first analysis in a repository is a two-step opt-in. The first press shows the
estimated size and included/excluded material; a second press sends. `:context` always
shows the effective manifest. Analysis and Ask use one deterministic context identity:
PR description/metadata, commit messages, the canonical diff, eligible changed-file
content, applicable `AGENTS.md`, and user-added paths at the opened revisions.

`.env*`, credential-like, ignored, binary, non-blob, and oversized paths cannot supply
source or diff content. Both names of a rename are checked. If eligibility cannot be
established, content is excluded. This is a path/type/size policy, not a secret scanner
for arbitrary prose in a PR description or user question.

`max_context_tokens` is the user's input ceiling. The model catalog may lower it after
reserving the effective output limit, never raise it. Complete serialized framing,
history, headings, and omission placeholders count. Oversize material is reduced in a
deterministic order and `:context` explains omissions.

Usage and reasoning-token counts come from the provider when available; absent provider
usage remains unknown rather than estimated as fact. Displayed cost combines reported
usage with catalog prices. The UI exposes thinking settings and token counts, not a
hidden reasoning trace.

The app requests one logical answer. Provider-specific streaming, text-delta,
OpenAI-compatible, and single-response routes are lazy fallbacks: a successful accepted
route prevents later attempts. Partial streamed text is retained when cancelled.

## Chat

Ask cannot browse the repository or run tools. It sees the inspected context manifest
and must identify missing evidence instead of pretending to have read it. User-added
context applies to later analysis and chat requests for that PR.

Sessions are durable under `chats/`, capped at the newest 50 per PR and 2 MiB per
session. Pruning is announced; export important conversations first with
`:chat export md` or `:chat export json`. A missing/corrupt index is rebuilt from session
documents. A changed head marks older context as stale rather than silently rebinding it.

## Drafts and GitHub mutations

Line and range comments are staged locally. A draft can also carry a review body and
Approve, Request changes, or Comment verdict. It is saved while editing and survives
restart, refresh, provider failure, and publish rejection. A comment retains the head
and source coordinates where it was authored; a drifted draft cannot be mixed with a
new revision or published silently.

`<leader>rr` opens an immutable preview containing the exact verdict, body, and inline
comments. One labelled `Enter` action publishes that snapshot. Inline reviews use one
GitHub review request. Existing threads can be replied to and resolved/reopened; the PR
conversation supports top-level comments. Those mutations also use a preview/confirmation.

Before dispatch, the confirmed payload is journaled as queued, then dispatching. A
confirmed response becomes succeeded or rejected. If the response is lost after
dispatch, the durable state is outcome unknown and automatic retry is blocked. The user
must inspect GitHub before deliberately trying again: local journals cannot guarantee
exactly-once delivery to a remote API.

Stopping a read, Git process, or provider request stops owned work promptly. Stopping a
mutation after dispatch cannot retract a request that GitHub may already have accepted.
The UI preserves this distinction.

`--dry-run` (or `[forge].dry_run = true`) executes no mutating command. It records the
command and retains private exact payload artifacts for inspection; simulated results
never update displayed remote truth as if GitHub accepted them.

## Storage and recovery

Everything the app owns is under `$SMART_REVIEW_HOME` (default `~/.smart-review`):

| Path | Role |
|---|---|
| `config.toml`, `keybinds.toml`, `themes/` | User configuration |
| `credentials.toml` | API keys, mode `0600` on Unix |
| `state.toml` | Small preferences, last subject, context additions, analysis opt-ins |
| `cache/` | Disposable forge responses, catalog, patches, and model analyses |
| `chats/` | Durable conversations and rebuildable indexes |
| `drafts/` | Durable staged reviews |
| `reviews/` | Durable plan/progress state and remote-mutation journals |
| `worktrees/` | App-owned bare repositories and detached PR checkouts |
| `exports/` | Requested chat/draft/dry-run artifacts |
| `logs/` | Rotated content-redacted diagnostics |

The app never writes to the user's working tree, index, branches, refs, or `.git`.
Durable documents use atomic replacement and per-document locks where concurrent writers
would lose data. Legacy chat and plan files formerly stored under cache are copied only
after validation; interrupted migrations retain their source for recovery.

Deleting `cache/` loses fetched responses and analyses, not chats, drafts, manual order,
human progress, or mutation records. Malformed durable files are retained and reported
rather than overwritten with defaults.

## Offline and platform behavior

When GitHub reads fail and a cached list/detail/diff exists, it remains usable with an
offline indicator and retry action. Operations that need the unavailable remote fail
with a next action; no remote mutation is inferred from cached state. A model provider
is a separate network boundary and may still be available when GitHub is not.

Linux and macOS are release targets; only Linux runs the current CI terminal smoke.
Windows is best effort. `gh` is required. One process reviews one repository; switch by
exiting and relaunching in another clone or with `--repo owner/name`.

## Deliberate limitations

- No agentic repository browsing or tool loop; analysis and Ask use the shown bundle.
- No syntax highlighting and no displayed hidden reasoning trace.
- No automatic paid re-analysis when a PR head changes. Whether to add an explicit
  commit-range/cost confirmation remains [DEC-15](decisions.md#open-decision).
- No claim of perfect secret detection, remote exactly-once mutation, or instant
  cancellation after a third party has accepted a request.
- No in-app repository picker, Windows tier-1 support, or macOS PTY CI today.
