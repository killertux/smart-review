# Product decisions

This is the living decision set. Historical `DEC-*` identifiers remain stable for code,
tests, issues, and old PRs; the full lookup is in [`legacy-ids.md`](legacy-ids.md).
The owner confirmed the previously proposed DEC-7, DEC-9–DEC-14, and DEC-18 outcomes
on 2026-09-21; DEC-8 was retired and DEC-15 remained open.

## Accepted

| ID | Decision |
|---|---|
| DEC-1 | PR heads use app-owned bare repositories and detached worktrees; the source clone and `.git` are never mutated. |
| DEC-2 | Analysis and grounded chat use a deterministic inspected context; there is no agentic repository tool loop. |
| DEC-3 | A verdict, body, and inline comments publish as one review request after exact preview. |
| DEC-4 | Unified diff is default; split view is available at wide sizes; supported languages use statically bundled Tree-sitter syntax highlighting with plain-text fallback. |
| DEC-5 | Keys are entered in the TUI, stored privately in `credentials.toml`, with provider environment variables taking precedence. |
| DEC-6 | No default model; the user selects catalog provider/model/thinking settings. |
| DEC-7 | Authenticated GitHub CLI is required. |
| DEC-9 | Keep at most 50 chat sessions per PR and 2 MiB per session; announce pruning and support export. |
| DEC-10 | Review order uses an LLM plan with deterministic heuristic fallback; explicit user order wins. |
| DEC-11 | Small state is TOML; structured cache/durable documents are JSON. |
| DEC-12 | Windows is best effort, not tier 1. |
| DEC-13 | One repository per process; switch by relaunching in another clone or with `--repo`. |
| DEC-14 | Failed GitHub reads may fall back to clearly marked cached content; remote-dependent actions remain truthful about failure. |
| DEC-16 | The product includes PR conversation comments, inline-thread replies, and resolve/reopen. |
| DEC-17 | Known providers use native `llm` backends; other catalog providers may use OpenAI-compatible passthrough; special-auth providers are excluded. |
| DEC-18 | Show supported thinking settings and reported tokens, but promise no hidden reasoning trace. |
| DEC-19 | Model selection uses `toml_edit` so config comments and unknown keys survive write-back. |
| DEC-20 | `?` opens help; `/` starts search and `N` repeats backward. |
| DEC-21 | Diff options are leader continuations: `<leader>ds`, `<leader>dc`, `<leader>dw`. |
| DEC-22 | CI remains one Linux job until macOS terminal smoke provides meaningful additional coverage. |
| DEC-23 | Guided review uses semantic steps, fingerprint-bound human progress, and one explicit Publish/Post action after immutable preview. |

DEC-8 (milestone priority) is retired: the milestone sequence completed and no longer
governs current work.

## Open decision

### DEC-15 — paid re-analysis after a new head

The app invalidates revision-sensitive analysis when a PR head changes and does not
automatically spend money on another request. It does not yet offer the originally
proposed confirmation showing the commit range and cost implication.

Before implementing such a flow, the owner must choose whether to add that explicit
prompt or keep re-analysis entirely manual. Update this file and affected product/tests
in the implementing change; do not infer approval from the old proposed default.

## Decision policy

Record only durable choices that constrain product behavior or architecture. A proposed
choice is not approval. New dependencies, weaker privacy/safety guarantees, platform
tier changes, or remote mutation semantics always require explicit owner agreement.
