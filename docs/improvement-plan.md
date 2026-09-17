# Smart Review — Reliability and Product Improvement Plan

**Status:** proposed implementation backlog; no implementation PR is complete.\
**Prepared:** 2026-09-14.\
**Baseline:** commit `7f05d1f`. Re-check the current branch before implementing.\
**Evidence:** [architecture, UX, correctness and performance review](app-review-2026-09-14.html).\
**Audience:** agents implementing individual PRs and the owner reviewing them.

## 0. Start here

This is the post-M5 improvement sequence requested by the owner. It is a plan for
repairing and improving the existing application, not a proposal to regenerate it.
Keep the useful domain models, parsing, port seams, fakes, process integration and
terminal lifecycle. Refactor the coordination layer around explicit ownership.

### Reading order for an implementing agent

1. Read the current `AGENTS.md` instructions and follow their required reading order:
   `REQUIREMENTS.md`, `PLAN.md`, `ARCHITECTURE.md`, then `AGENTS.md`.
2. Read the report's executive recommendation and the findings assigned to your PR.
3. Read this plan's shared contracts, your PR, its dependencies, and its acceptance
   tests. Read the adjacent PRs before changing a shared interface.
4. Inspect the actual current implementation. Report line numbers refer to the
   baseline and will move; locate symbols rather than assuming line numbers remain
   accurate. Prefer the code graph when available, then source search as necessary.
5. Check `git status` and preserve existing user work. Do not commit or push unless
   separately asked.

### How to use this plan

- Work in the numbered order by default. The order balances user impact and real
  implementation dependencies. The dependency column is authoritative if the owner
  assigns independent PRs in parallel.
- `IR-01` through `IR-19` are stable follow-up PR IDs. Use titles such as
  `IR-04: Preserve draft anchors and active composers`.
- `F01` through `F17` refer to the HTML review's finding IDs. These are not new
  product requirements. Continue referencing existing FR/NFR IDs where applicable.
- A PR can fix one invariant across several files. It should not also perform a
  broad unrelated cleanup or add a new framework.
- Test names and new types below describe required behavior and intended ownership;
  names may be adjusted to surrounding conventions. Avoid exposing internals as
  public APIs solely to satisfy a suggested test shape.
- For a **code-proven** finding, first reproduce its schedule with a deterministic
  fake. For a **hypothesis**, establish whether it is a defect before changing code.
- Update this file as work lands: status, actual PR link, verification evidence,
  deviations and unresolved follow-ups. Do not mark a PR done because it compiles.
- The completed M0–M5 plan is historical context for this owner-requested follow-up
  sequence. Keep existing hard rules and requirement-update obligations until
  IR-19 deliberately migrates the documentation.

### Definition of success

A reviewer can open a PR, understand its purpose, follow an intelligible suggested
file order, inspect all checks and discussion, write comments on the intended lines,
and publish without losing state or accidentally repeating a submission. The LLM
sees exactly the allowed context and obeys the user's effective request limits. The
interface remains usable while background work happens. Tests cover these journeys
without minutes of unconditional waiting.

---

## 1. Baseline, priorities and scope

### Measured baseline from the review

| Check | Local warm-build result |
|---|---|
| `cargo fmt --all` | Passed, 0.563 s |
| `cargo clippy --all-targets --all-features -- -D warnings` | Passed, 8.232 s |
| `cargo test --all-features` | 987 unit + 11 snapshot tests passed, 2.697 s |
| `scripts/validate/m5.sh` | 32 checks passed, 131.310 s |
| M5 programmed settling waits | 114.9 s, statically counted |
| M4 + M5 programmed settling waits | 175.7 s, statically counted |

These are not cold-build, production-latency or GitHub Actions benchmarks. The
existing Rust tests are already fast. Preserve useful tests and improve the slow
orchestration and missing behavioral coverage.

### Priority meanings

- **P1 / trust:** wrong outbound content, duplicate requests, wrong PR/revision,
  lost writing, false mutation outcomes, broken cancellation, or misleading core
  navigation. Fix before inviting broad usage.
- **P2 / product and maintainability:** make context attribution, storage recovery,
  interaction, speed and test feedback dependable and comprehensible.
- **P3 / documentation closure:** consolidate the living product contract after
  the corrected behavior is established. Documentation updates within earlier PRs
  still happen immediately.

### Scope boundaries

This sequence includes all confirmed findings in the report, its actionable
performance recommendations, the proposed guided-review workflow, testing changes,
documentation migration, and explicit investigation of the two adapter hypotheses.

The plan does not require a forge replacement, an agentic file-editing loop, syntax
highlighting, CI-log ingestion, Windows tier-1 support, a new frontend framework,
or a wholesale workspace/crate split. A new dependency still requires owner approval
with the crate, version, purpose, transitive weight and alternatives. Use `cargo add`
after approval. Existing std/Tokio/Ratatui/Python facilities are sufficient for the
initial approach described here.

---

## 2. PR order and dependencies

All statuses start as **not started**. Size is relative implementation/review effort,
not a time estimate: S = narrow, M = one subsystem, L = cross-cutting invariant.

| PR | Priority | Title / delivered outcome | Hard dependencies | Size | Status |
|---|---|---|---|---|---|
| [IR-01](#ir-01-prevent-excluded-content-and-request-bodies-from-leaking) | P1 | Outbound content policy and safe diagnostic logging | None | M | [Open — PR #11](https://github.com/killertux/smart-review/pull/11) |
| [IR-02](#ir-02-make-llm-fallback-lazy-and-account-for-every-attempt) | P1 | One intended LLM request, lazy fallback, correct usage | None | S | [Open — PR #12](https://github.com/killertux/smart-review/pull/12) |
| [IR-03](#ir-03-enforce-effective-model-settings-and-request-budgets) | P1 | Enforced model settings and complete payload budgets | IR-02 | M | Implemented |
| [IR-04](#ir-04-preserve-draft-anchors-and-active-composers) | P1 | Original anchors and unsaved writing survive reloads | None | M | [Merged — PR #14](https://github.com/killertux/smart-review/pull/14) |
| [IR-05](#ir-05-own-pr-state-and-jobs-with-a-review-session) | P1 | PR/session isolation and stale-result rejection | IR-04 | L | [Merged — PR #15](https://github.com/killertux/smart-review/pull/15) |
| [IR-06](#ir-06-make-user-storage-atomic-durable-and-recoverable) | P1 foundation | Atomic writes, durable chat, recoverable indexes | IR-05 | L | [Open — PR #16](https://github.com/killertux/smart-review/pull/16) |
| [IR-07](#ir-07-model-remote-mutations-and-unknown-outcomes-explicitly) | P1 | Publish/reply reconciliation and truthful dry-run | IR-05, IR-06 | L | In progress (`ir-07-remote-mutations`) |
| [IR-08](#ir-08-cancel-real-work-and-bound-progress-delivery) | P1 | Real cancellation, bounded progress, worker recovery | IR-02, IR-05, IR-07 | M | In progress (`ir-08-cancellation-progress`) |
| [IR-09](#ir-09-unify-layout-focus-hit-testing-and-visible-selection) | P1 | Input reaches the visible target and correct diff line | IR-04, IR-05 | M | [Merged — PR #20](https://github.com/killertux/smart-review/pull/20) |
| [IR-10](#ir-10-ship-real-pr-tabs-and-complete-checks-and-discussion) | P1 | Reachable Overview, Files, Checks, Discussion and Ask | IR-05, IR-07, IR-09 | L | [Merged — PR #21](https://github.com/killertux/smart-review/pull/21) |
| [IR-11](#ir-11-make-the-review-order-executable-everywhere) | P1 | File, hunk, tree and scrolling honor the selected order | IR-05, IR-09 | M | In progress (`ir-11-executable-review-order`) |
| [IR-12](#ir-12-unify-context-identity-caching-and-evidence-attribution) | P2 | Same inspectable context and correct file evidence | IR-01, IR-03, IR-05 | L | [Merged — PR #25](https://github.com/killertux/smart-review/pull/25) |
| [IR-13](#ir-13-isolate-git-workspaces-and-use-explicit-revision-identities) | P1 adapter correctness | Correct merge base, app-owned Git data, collision-free identity | IR-05, IR-06 | L | [Open — PR #26](https://github.com/killertux/smart-review/pull/26) |
| [IR-14](#ir-14-move-io-out-of-the-ui-and-order-background-saves) | P2 responsiveness | Pure reducer/rendering and ordered asynchronous persistence | IR-05, IR-06, IR-07, IR-08 | L | Not started |
| [IR-15](#ir-15-make-the-terminal-harness-trustworthy-and-remove-repeated-gates) | P2 high leverage | Fail-fast waits, incremental replay, single CI gates | None; coordinate with IR-09/10 | M | Not started |
| [IR-16](#ir-16-integrate-a-concise-guided-review-into-the-file-workflow) | P2 product | Brief, per-file what/why/verify, plan and human progress | IR-03, IR-10, IR-11, IR-12 | L | Not started |
| [IR-17](#ir-17-optimize-measured-runtime-and-context-hotspots) | P2 performance | Prepared views, coalesced frames, batched object reads | IR-08, IR-11, IR-13, IR-14, IR-16 | L | Not started |
| [IR-18](#ir-18-consolidate-deterministic-scenarios-and-a-small-pty-smoke-suite) | P2 tests | Fast scenario coverage and minimal meaningful PTY contracts | IR-15, IR-17 (and their prerequisites) | M | Not started |
| [IR-19](#ir-19-replace-milestone-docs-with-a-small-living-product-contract) | P3 | Accurate product, architecture and testing documentation | IR-01 through IR-18 | M | Not started |

### Execution notes

- IR-01, IR-02 and IR-04 have no functional dependency on one another. They are good
  independent assignments if the owner explicitly wants parallel implementation.
- IR-15 can run early in an isolated lane: it primarily changes scripts and CI.
  Do not make the urgent correctness fixes wait for a new test framework.
- IR-05 establishes the state/job ownership boundary used by later PRs. Agree its
  interfaces before concurrent UI/runtime work; otherwise `app.rs` becomes a merge
  conflict hotspot.
- IR-09 is deliberately before real tabs: fixing geometry and input ownership makes
  the new screens predictable instead of adding more routes to the broken model.
- IR-13 remains P1 even though it appears later: it needs stable identity and storage
  primitives. Its small merge-base regression can be isolated and landed earlier if
  the owner wants immediate containment; keep its acceptance tests with IR-13.
- IR-18 consolidates tests introduced by earlier fixes. It is not permission to defer
  regression tests until the end.
- An L-sized PR may be split into a behavior-preserving extraction and its fix when
  reviewability genuinely requires it. Record sub-PRs under the same IR ID; each must
  build and pass its gates. Do not introduce inert abstractions in a standalone PR.

---

## 3. Shared design contracts

These contracts coordinate implementation across agents. Exact type names can vary;
the ownership and behavior cannot be replaced by independent ad hoc flags.

### 3.1 Review identity and ownership

Distinguish the following:

1. **Repository identity:** host + owner + repository; normalized and stored without
   ambiguous delimiter concatenation.
2. **Review subject:** repository identity + PR number.
3. **Revision identity:** resolved head SHA, base SHA and merge-base SHA where known.
   A remote-only view must explicitly represent unavailable revision data.
4. **Session generation:** monotonically changing token for the current UI subject/
   revision lifecycle. Navigation or supersession invalidates old generations.
5. **Document version:** increasing version for each mutable draft/chat/state document.
6. **Job identity:** unique job ID + owner/generation + kind + request snapshot.

Never infer the origin of a result from the PR currently on screen. Never use one
bare SHA to stand in for repository/PR/base identity. Mutating operations outlive the
modal that initiated them and remain owned by their original subject.

Suggested organization, to be introduced through IR-05/14 rather than all at once:

```text
composition/bootstrap
  creates ports/adapters and the runtime

application / domain
  ReviewSubject, RevisionIdentity, ContextSpec, Draft, MutationOperation
  use cases and pure state transitions

runtime
  effects → jobs → progress/completion
  ordered document persistence and external process/HTTP integration

tui
  App shell + active ReviewSession view state
  ReviewTab + FocusTarget + Overlay
  input → action/effect; state + prepared layout → frame
```

Start with modules, not separate crates. Move wiring and IO out of TUI reducers as
real callers are migrated. Add compiler-enforced crate boundaries only if a later
review demonstrates that module/test enforcement is insufficient.

### 3.2 State transitions are atomic from the user's perspective

- Open/switch/close a review replaces or detaches all PR-scoped state coherently.
- Refresh the same subject preserves draft, composer, cursor, manual order and review
  progress, reconciling only data invalidated by the new revision.
- Replacing remote diff data with local diff data is a data refresh, not a new review.
- A canceled generation cannot become “streaming” again because a delta was queued.
- A completed save only clears dirty state for the exact document version saved.
- A completed publish clears only its submitted draft snapshot, never newer writing.

### 3.3 Outbound LLM contract

- The context inventory is derived from the actual allowed payload.
- Secret/ignored/size/binary policies apply to every representation, including diff
  hunks, deleted content, rename aliases, full bodies, conventions and added files.
- Metadata and user questions are separate segment kinds. Do not claim a pathname
  policy is a general secret detector for arbitrary prose.
- Analysis, chat and inspection consume the same context specification. Conversation
  history is an explicit additional part of chat's request accounting.
- User context/output settings are ceilings, not hints overridden by catalog size.
- Include prompt framing, role/message overhead estimates, history and output reserve
  in the budget. The token number remains explicitly approximate until an approved
  tokenizer exists.
- No fallback or retry happens merely because a paid request's outcome is ambiguous.
  Distinguish unsupported capability from authentication, timeout and partial output.
- Cached results identify the exact effective question, context and model settings.
- Claims about code cite unambiguous paths and valid coordinates. Inference is visibly
  distinguished from the PR author's stated purpose and from verified source facts.

### 3.4 Remote mutation contract

Use a state model along these lines:

```text
editing → preview → confirmed → queued → dispatching
                                      ├─ succeeded
                                      ├─ definitely_not_sent / rejected
                                      └─ outcome_unknown → reconciling
                                                           ├─ succeeded
                                                           └─ still_unknown
dry-run → simulated (never a real remote-state update)
```

“Canceled locally” does not prove “not sent remotely.” A local operation ID does not
create server-side idempotency on an API that does not support it. Reconcile against
observable remote results when possible; when not possible, preserve the draft and
provide a precise next action without automatically retrying.

### 3.5 UI navigation contract

Proposed product structure for implementation:

- Tabs: **Overview**, **Files**, **Checks**, **Discussion**, **Ask**.
- Analysis generation is an explicit action. Overview shows the full brief; Files
  shows concise current-file assistance. There is no second fake Analysis tab.
- Normal-mode `1`–`5` and clickable labels select the same tab model. Digits remain
  ordinary text inside composers, search and commands.
- `Tab`/`Shift-Tab` move among visible focus targets within the current surface. They
  must not silently open an unrelated input destination.
- Overlay → active tab → focused pane is the input ownership order.
- File-order toggle changes every cross-file movement path and preserves current file.
- Discussion includes review bodies, inline threads, top-level conversation, resolved
  and outdated threads. An unavailable current diff line does not hide the thread.
- Checks show status, conclusion and a link to the run. Full CI log ingestion is not
  required to make the Checks tab useful.

These are intended behavior changes. Update the affected requirements, default keymap,
README examples and snapshots in the same implementing PR. Where current proposed
DEC rows conflict or remain unresolved, obtain the owner's decision before calling
the new behavior decided.

### 3.6 Storage categories

| Category | Examples | May be evicted? |
|---|---|---|
| Durable user documents | Drafts, chat, manual order overrides, review markers, mutation journal | No |
| Reconstructible cache | PR metadata, diffs, model catalog, successful AI analyses | Yes |
| App-owned Git data | Object stores and managed worktrees | Explicit cleanup policy |
| Explicit user exports | Chat/review transcripts, requested dry-run payload artifacts | No automatic cache eviction |
| Diagnostics | Structured logs without keys, source content or full request bodies | Bounded rotation |

An index is derived data. Losing it must not make durable documents unreachable.
Automatic migration must be retryable and must retain the old document until the new
copy is verified. A failed migration must never turn into empty successful state.

Every durable format change must state its downgrade behavior. Never recommend
reinstalling an older binary as a rollback if that binary would ignore unresolved
mutation records or overwrite a newer document schema. Preserve readable backups or
exports where appropriate, reject unsupported versions explicitly, and test the recovery
path. Cache formats can be invalidated; user-authored documents need a migration.

---

## 4. Implementation PRs

## IR-01: Prevent excluded content and request bodies from leaking

**Priority:** P1. **Findings:** F01; request-body logging follow-up.\
**Requirements:** FR-4.6, FR-6.5, FR-9.2, NFR-3.1–3.3.\
**Depends on:** none.\
**Status:** implemented and locally verified on `ir-01-outbound-content-policy`;
[PR #11](https://github.com/killertux/smart-review/pull/11) is open for review.

### Delivered outcome

If the context UI calls a path excluded, none of that path's protected content reaches
the LLM through another representation. Ordinary logs contain neither credentials nor
full review/reply/LLM request bodies.

### Start in these areas

- `src/domain/context.rs`: `disposition`, `build`, `render_patch`.
- `src/application/context.rs`: `gather`, conventions and user additions.
- `src/tui/app.rs`: analysis/chat request construction.
- `src/adapters/gh/review.rs`, `gh/comments.rs`, `gh/mod.rs`.
- `src/adapters/process.rs`: command rendering and dry-run logging.
- Existing context tests and `scripts/validate/m2b.sh`, `m3.sh`, `m4.sh`, `m5.sh`.

### Implementation steps

1. [x] Add a failing final-payload regression using synthetic sentinels in a changed
   `.env`, including both removed and added lines. Assert against what the fake LLM
   receives, not just `Disposition::Omit` or a segment flag.
2. [x] Define one exclusion decision per input path/representation. Evaluate both old
   and new paths for a rename so renaming a secret file does not bypass the policy.
3. [x] Apply the policy before rendering diff content. Keep a non-content placeholder
   that explains why the file is absent when appropriate; never include the excluded
   hunk text in the placeholder or diagnostics.
4. [x] Cover deleted files, binary changes, full bodies, convention files and user-added
   paths. Make oversize behavior consistent with the documented per-file policy.
5. [x] Correct the `.gitignore` assumption: being tracked does not prove a path is not
   matched by ignore rules. Obtain eligibility through the workspace boundary, with
   tests for tracked ignored files and nested rules. Do not add filesystem/process IO
   to the domain layer.
6. [x] Derive included/excluded inventory entries from the actual filtered bundle.
   Avoid a separate list that merely repeats the user's intended policy.
7. [x] Classify command arguments/payloads for logging. Log program, operation, subject,
   job ID and outcome; exclude body/key fields. Use existing payload-file/stdin
   facilities for review and reply content where possible.
8. [x] Keep exact user-requested dry-run payloads as private, app-owned artifacts when
   necessary for replay. Ordinary logs should reference their path rather than copy
   their contents. Continue honoring dry-run for every remote write.
9. [x] Update the context/privacy and logging documentation to describe actual rules
   and their limits, including the distinction between path filtering and arbitrary
   prose supplied by a user.

### Required regression cases

- Modified/deleted `.env`; `.env` renamed to an innocuous filename and the reverse.
- Denied nested credential path, binary content, and an oversize file.
- A tracked path matching ignore rules, including nested patterns/negation behavior.
- A tracked old path ignored only at the base revision after the head removes the rule.
- A reduced-budget final payload and a renamed destination added with `:context add`.
- A permitted source file remains present; filtering does not silently drop the rest
  of the diff or all context.
- The inspector's file inclusion agrees with the exact fake-provider request.
- Synthetic token/body sentinels absent from diagnostic logs for success, failure and
  dry-run; payload artifacts have expected private permissions.

### Manual acceptance recipe

Use the repository's local fake-provider fixture, with only synthetic credential
sentinels. Open the PR, inspect `:context`, run analysis, and ask a chat question.
Check the fake provider's captured request and diagnostic log artifacts: the allowed
source sentinel is present in the request; protected sentinels are absent. Run the
fake review/reply and dry-run flows and inspect artifact permissions. Failure includes
an “excluded” row whose content still appears in any request or ordinary log.

**Gates:** shared gates plus context/LLM contracts (`m2b`, `m3`) and affected posting
contracts (`m4`, `m5`). The final PR description must include exact fixture commands.

**Local verification (2026-09-14):** `cargo fmt --all`, Clippy with all targets,
features and warnings denied, and `cargo test --all-features` passed (1002 unit tests and
11 snapshots). Validators passed: `m2b` 35/35, `m3` 59/59, `m4` 23/23 and `m5` 32/32.
Final-wire regressions cover reduced-budget and user-added-rename payloads, and a real
Git fixture covers base-only ignore rules. No live provider, network request or GitHub
mutation was used.

**Out of scope:** replacing the provider SDK or implementing a generic secret scanner.

---

## IR-02: Make LLM fallback lazy and account for every attempt

**Priority:** P1. **Findings:** F15.\
**Requirements:** FR-4.4, FR-5.2, FR-5.4. **Depends on:** none.
**Status:** implemented and locally verified on `ir-02-streaming-attempt-policy`;
[PR #12](https://github.com/killertux/smart-review/pull/12) is open for review.

### Delivered outcome

A successful request is not followed by an unnecessary second streaming request.
Partial failure, cancellation and ambiguous transport failure do not silently restart
the answer. The preview and reported usage belong to the same attempt.

### Start in these areas

- `src/adapters/llm.rs`: `ask_streaming`, structured/string/plain paths, candidates.
- `src/ports/llm.rs`: outcome/error/usage contracts.
- Existing stub providers and fallback tests in the adapter.

### Implementation steps

1. [x] Reproduce the eager array evaluation with a stub that supports both streaming
   methods and records independent counters and distinct text sentinels.
2. [x] Replace eager evaluation with explicit lazy control flow. Inspect the outcome
   of one attempt before invoking the next.
3. [x] Define which failures allow a fallback: unsupported capability is different
   from auth refusal, rate limiting, a timeout, or lost transport after dispatch.
   An absence of emitted text alone does not prove the provider did no billable work.
4. [x] Stop the cascade after success, after any emitted partial answer, and after
   cancellation. Preserve the most informative typed error and partial result.
5. [x] Track attempt identity in diagnostics and preview updates, without request
   content. Reset/swap previews deliberately for a legitimate retry; never concatenate
   independent answers as one stream.
6. [x] Preserve usage for attempted requests when reported. If usage is unavailable,
   label it unknown instead of asserting a zero-cost attempt.
7. [x] Review the separate analysis JSON-repair request. It is an explicit second
   attempt, not a streaming fallback; retain its current bounded retry count and make
   combined usage honest.

### Required regression cases

- Structured success: string and plain counters are zero.
- Structured unsupported: string invoked once; plain remains zero after string success.
- Both stream methods unsupported: exactly one supported next path/plain request.
- Partial structured failure: no second method; partial text preserved.
- Cancel before call and during controlled streaming: no subsequent candidate.
- Authentication/rate-limit/ambiguous transport errors do not launch a blind cascade.
- Distinct attempt sentinels never appear mixed in one accepted response.
- Usage combines only actual, intentionally executed requests; missing usage is visible.

### Manual acceptance recipe

Use a fake provider exposing both supported stream APIs. Run one analysis and one chat
question. Observe a single answer and a completion without a second wait. Inspect
request counters, not only final text. Repeat with unsupported structured streaming and
with a forced partial failure. Failure means an unexpected second request or a preview
that changes to content from an unrequested attempt.

**Gates:** shared gates, targeted adapter tests, `m2b` and `m3` contracts.

**Local verification (2026-09-15):** `cargo fmt --all`, Clippy with all targets,
features and warnings denied, and `cargo test --all-features` passed (1010 unit tests and
11 snapshots). Validators passed: `m2b` 35/35 and `m3` 59/59. Regressions cover a generic
rate-limit failure without a follow-up request, reset repair previews/raw answers, partial
usage in either attempt order, native OpenAI-compatible streaming, repair cancellation, and
monotonic diagnostic attempts. The M2b repair fixture now checks its durable retry notice
instead of relying on duplicate streaming requests to keep a transient popup on screen. No
live provider or network request was used.

---

## IR-03: Enforce effective model settings and request budgets

**Priority:** P1. **Findings:** F02, F04.\
**Requirements:** FR-4.5–4.8, FR-5.4. **Depends on:** IR-02.

### Delivered outcome

Configured output/context limits and temperature reach the provider when supported.
The UI reports the effective settings, and the final serialized request fits the
declared approximate budget without wrappers/placeholder text overrunning it.

### Start in these areas

- `src/application/models.rs`: `ResolvedSelection`, `analysis_chat`, `context_budget`.
- `src/config.rs` and `src/domain/model.rs`: configured settings and capabilities.
- `src/domain/context.rs`: byte accounting and truncation.
- `src/application/chat.rs`: history budgeting.
- `src/tui/app.rs`: duplicated analysis/chat request builders.

### Implementation steps

1. [ ] Introduce one effective request-settings value carrying provider/model route,
   supported temperature, thinking, output ceiling, context ceiling and timeout.
   Do not lose settings between persisted selection and resolved selection.
2. [ ] Use the user ceiling together with model capacity: catalog metadata may lower
   an allowed value or provide a default; it must not silently raise an explicit
   user spending/context ceiling.
3. [ ] Define budget semantics precisely: the configured input/context ceiling versus
   the model's combined input+output window. Reserve output and prompt/message overhead
   before allocating space to source context and chat history.
4. [ ] Remove floor logic that can exceed the actual model/user ceiling. An impossibly
   small budget should produce an actionable refusal or a clearly disclosed empty/
   truncated context, not an invented larger allowance.
5. [ ] Account for complete serialized blocks, including headings, separators,
   placeholders and added files. Budget before appending; never append unbounded
   placeholders after the remaining budget reaches zero.
6. [ ] Share the final accounting path between analysis, chat and estimates. Preserve
   deterministic truncation and show which source context/history was omitted.
7. [ ] Define explicit behavior for unsupported temperature/thinking settings. Do not
   silently label thinking off while sending a default-enabled provider request.
   Refuse unsupported combinations or show the actual applied setting consistently
   with the product requirements.
8. [ ] Show effective output/input limits and setting provenance in model/context
   details. Distinguish estimated cost, reported tokens and unknown usage.
9. [ ] Update configuration docs/examples and relevant decisions in the same PR.

### Required regression cases

- Catalog window 1,000,000, configured input ceiling 100,000: effective source/input
  budget cannot become 967,232 merely because the catalog is larger.
- User output 4,096 reaches the request; a smaller catalog output cap reduces it.
- Temperature propagates for supported models; unsupported behavior is explicit.
- No catalog metadata, explicit zero/tiny/large settings, and invalid settings.
- Exact-fit and one-byte-over boundaries, empty metadata, many binary placeholders,
  long path labels, and multibyte UTF-8 truncation.
- Chat history + system framing + source bundle + output reserve fit the chosen model
  window according to the same documented estimator.
- Reconfigured settings take effect without restart and are accurately displayed.

### Manual acceptance recipe

With a fake large-window model, set a small explicit context/output limit and a
supported temperature. Open `:model show` and `:context`, submit an analysis/chat
request, and inspect the captured request parameters and manifest. Lower the limit
again without restarting. Failure means an ignored setting, an unexpectedly larger
request, or a truncation that the inspector does not disclose.

**Gates:** shared gates, model/context/chat units, `m2a`, `m2b`, `m3` contracts.

**Local verification (2026-09-15):** `cargo fmt --all`, Clippy with all targets,
features and warnings denied, and `cargo test --all-features` passed (1014 unit tests and
11 snapshots). Validators passed: `m2a` 34/34, `m2b` 35/35, and `m3` 59/59. Regressions
cover user ceilings versus catalog capacity, output capping, temperature propagation or
explicit fallback, unsupported thinking disclosure, exact serialized bundle boundaries,
and chat history constrained by the complete system prompt. No live provider or network
request was used.

---

## IR-04: Preserve draft anchors and active composers

**Priority:** P1. **Findings:** F10, F13.\
**Requirements:** FR-6.1–6.3, NFR-3.4, NFR-4.1. **Depends on:** none.
**Status:** merged in [PR #14](https://github.com/killertux/smart-review/pull/14).

### Delivered outcome

An existing comment remains anchored to the commit on which it was written. A local
diff arriving, refresh completing, or display option changing cannot erase an active
composer, reset a pending submission, or relabel old line numbers as new ones.

### Start in these areas

- `src/tui/drafts.rs`: `anchor_to`, `open`, composer and publishing state.
- `src/tui/app.rs`: patch completion, draft load scheduling, publish preparation.
- `src/tui/mod.rs`: `LoadDraft`, `SaveDraft` and publish effects.
- `src/domain/draft.rs` or current draft-domain module; GitHub review serialization.

### Implementation steps

1. [ ] Add regressions for H1→H2 draft loading and a delayed local patch arriving while
   the user is typing. Capture the exact text, anchor and in-flight state before/after.
2. [ ] Separate initial document loading from same-subject diff refresh. Load persisted
   drafts on review-session entry, not on every patch completion.
3. [ ] Restrict anchor initialization to genuinely new/unanchored comments/drafts.
   Preserve a nonempty existing draft's revision. Do not overwrite it when opening a
   publish modal or creating the submission payload.
4. [ ] Represent draft revision drift explicitly. Until validated re-anchoring exists,
   keep the draft and require the user to review/recreate affected anchors before
   submitting them against a different commit.
5. [ ] Decide how mixed-revision comments are represented: either per-comment anchors
   with validation or refusal to append incompatible anchors to a single-revision
   draft. Never silently normalize them to the latest head.
6. [ ] Preserve composer text, selected range, pending reply and publish snapshot during
   diff/context/whitespace/split refreshes. If an anchor is unavailable in the new
   view, show a recoverable stale-target state with the text intact.
7. [ ] Make loaded draft application version-aware so a delayed disk load cannot replace
   edits already made in memory. IR-05/14 will generalize subject/save ownership.
8. [ ] Preserve existing remote confirmation and draft-on-failure behavior.

### Required regression cases

- Nonempty H1 draft loaded on H2 keeps H1 and reports drift; payload cannot mislabel H2.
- Empty/new draft gets its first anchor correctly.
- Local patch arrival during typing preserves text, range and composer focus.
- Display option reload and same-head refresh preserve draft and pending submission.
- Failed initial load cannot replace a newly edited draft with an empty success.
- A changed/deleted target path does not cause the composer text to disappear.

### Manual acceptance recipe

Use a fixture with a held local-workspace completion. Open a line comment in the remote
diff and type a recognizable sentence; release the local patch and verify the sentence
and target remain. Stage it, switch the fixture head to a version moving that line,
reopen, and inspect the drift warning and saved SHA. Failure includes a moved SHA with
unchanged line numbers or any lost writing. Do not publish to a real PR for this check.

**Gates:** shared gates, draft/composer snapshots, `m4` and `m5` contracts.

**Local verification (2026-09-15):** `cargo fmt --all`, Clippy with all targets,
features and warnings denied, and `cargo test --all-features` passed (1020 unit tests
and 11 snapshots). Validators passed: `m4` 23/23 and `m5` 32/32. Regressions cover
preserving an H1 anchor at H2, refusing mixed-revision comments and publication,
replacing A's draft when entering B, and keeping active composer text and its range
through a local patch refresh. Composer and range coordinates carry the revision of
the applied diff job, rather than a newer detail response. No live GitHub mutation was
used.

---

## IR-05: Own PR state and jobs with a review session

**Priority:** P1. **Findings:** F11; late-completion/cancellation lifecycle gaps.\
**Requirements:** ARCH-5, FR-3.1, FR-4.3–4.4, FR-5.1, FR-6.1.\
**Depends on:** IR-04.

### Delivered outcome

Opening B cannot display, send, or persist A's chat, patch, context, plan, draft or
workspace as B's data. Every progress/completion/follow-up is accepted against its
originating identity. The coordinator no longer relies on scattered job-ID edits.

### Start in these areas

- `src/tui/app.rs`: open/close review, apply detail/patch/chat/context, job bookkeeping.
- `src/tui/jobs.rs`: requests, slots, progress, outcomes, scheduling.
- `src/tui/mod.rs`: effect application and workspace scheduling.
- Existing `chat`, `drafts`, `discussion`, `diff_view` state modules.

### Implementation steps

1. [ ] Inventory all PR-scoped fields and job slots before extracting code. Classify
   each as session state, durable document, derived view, or global preference.
2. [ ] Introduce the subject/revision/generation identities from §3.1. Create a coherent
   `ReviewSession` owner and distinguish entering a new subject from refreshing one.
3. [ ] On subject change, detach/reset PR-specific chat, analysis, context estimates,
   workspace/readiness, discussion, draft loader, navigation and request IDs together.
   Preserve global preferences and durable documents through explicit stores.
4. [ ] Centralize job registration, completion, failure and cancellation bookkeeping.
   New job kinds should not require unrelated string-based edits to three match tables
   before their results become visible.
5. [ ] Carry origin identity in every request, progress item, completion and continuation.
   Validate it before modifying state or launching follow-up work. Do not retag a
   returned bundle with the subject current when it arrives.
6. [ ] Apply `ChatLoaded(None)` as an empty/new chat for that subject, not as “keep the
   old conversation.” Validate session ownership again before request construction and
   persistence, even if the view is believed to be correct.
7. [ ] Clear completed job occupancy consistently. Schedule workspace creation/revalidation
   on identity mismatch rather than only `Option::is_none()` and a stale numeric flag.
8. [ ] Handle same-head/different-base PRs as distinct subjects/revision views. Keep
   provisional remote-only revision identity explicit until resolved.
9. [ ] Make cancellation/supersession invalidate update eligibility immediately. Keep
   durable mutations tracked independently; their remote state machine is IR-07.
10. [ ] Extract cohesive feature transition methods/modules while migrating real callers.
    Keep the `App` shell responsible for composition and routing, not every use-case
    detail. Preserve public user behavior apart from the listed fixes.

### Required controlled schedules

1. A chat exists; B has none. Open B, ask a question, and inspect both persisted sessions.
2. A patch is delayed; B detail arrives; then A patch arrives. B remains unchanged.
3. A context gather completes while B is current. No A bytes reach a B request.
4. A analysis/cached plan arrives after B. It never reorders B's files.
5. Workspace A completes; open B and then refresh B to H2. Appropriate work is scheduled
   and local capabilities become available again.
6. Same head SHA, different PR/base: no cross-subject workspace/merge-base readiness.
7. Cancel after success is queued but before reduction: stale read success is discarded.
8. Close a review with a save/mutation pending: its original owner remains correct.
9. Simulated worker failure/panic clears its registry entry and displays the correct
   error; it does not strand unrelated job capacity.

### Manual acceptance recipe

Prepare two fixture PRs with visibly different filenames, authors, chat histories and
heads. Use held completions to switch between them before work finishes. Verify title,
file list, chat, context inventory, local-diff source and draft subject agree at every
step. Reopen both and inspect per-subject data on disk. Failure is any mixed identity,
stale spinner, missing reschedule or unexpected LLM request.

**Gates:** shared gates and all affected browsing/workspace/analysis/chat/publish
contracts. Include state-transition evidence, not only screenshots.

---

## IR-06: Make user storage atomic, durable and recoverable

**Priority:** P1 foundation for mutation recovery. **Findings:** F17.\
**Requirements:** FR-8.1, FR-8.5, NFR-4.1. **Depends on:** IR-05.

### Delivered outcome

Concurrent or interrupted writes cannot publish partially overwritten documents.
Deleting disposable cache does not delete chats or manual review state. A failed index
update does not make successfully saved user content undiscoverable.

### Start in these areas

- `src/adapters/fs.rs`: atomic writer and permissions.
- `src/adapters/chat_store.rs`, `draft_store.rs`, `analysis_cache.rs`.
- `src/paths.rs`, `src/state.rs`, storage ports and bootstrap migration.

### Implementation steps

1. [ ] Reproduce the shared `.tmp` inode interleaving with controlled writers. Replace
   fixed temp names with unique sibling files opened exclusively; sync, close and
   rename only the writer's own file. Clean only files owned by that operation.
2. [ ] Preserve destination permissions, private directory/file defaults and actionable
   errors. Verify platform rename semantics; do not delete the existing document
   before a replacement is safely ready.
3. [ ] Define durability guarantees, including directory sync after rename where
   supported. Separate process-crash recovery from power-loss guarantees in docs.
4. [ ] Add document revision/conflict handling or serialization for two instances
   sharing one home. Unique temp files prevent torn writes but do not solve lost
   logical updates or two writers racing on the same index.
5. [ ] Use existing platform/std facilities where supported by the MSRV. If the chosen
   interprocess-lock strategy requires a dependency, stop for the required approval.
   Detect/report a conflict instead of silently overwriting if safe merging is absent.
6. [x] Move chat and manual review-order overrides into durable app-owned directories.
   Reserve durable locations for review markers and mutation records used by later PRs.
7. [ ] Implement idempotent migration from legacy cache locations: read/validate, write
   new location, verify, then mark migrated. Retain old data on failure. Re-running
   migration must not duplicate sessions or silently choose an older version.
8. [ ] Treat session indexes as rebuildable projections. Recover documents written
   before an index crash, reconcile orphan entries, and expose corruption warnings
   without hiding other valid documents.
9. [ ] Define retention separately from cache eviction. Do not prune unsaved/current
   chat or unexported user data merely because it lives in a formerly cached location.
   Resolve the existing proposed retention decision with the owner before destructive
   policy changes.
10. [ ] Add storage fault-injection seams only where needed for real failure tests.
    Update on-disk layout docs and compatibility notes immediately.

### Required regression cases

- Two controlled writers to the same document cannot corrupt the published bytes.
- Write/sync/rename failure leaves either the valid old document or valid new one;
  never truncated success. Simulate errors without modifying real user files.
- Conflicting document versions produce a defined conflict/serialization outcome.
- Crash between session write and index update still permits discovery/recovery.
- Valid old index plus an unindexed newer session is recoverable.
- Cache deletion preserves drafts, chat, manual order, markers and operation records.
- Migration succeeds once, retries after interruption, and preserves the source on
  destination failure. Unknown/corrupt legacy files are not silently deleted.
- Permissions remain private after replacement and migration.

### Manual acceptance recipe

Create a fixture home containing legacy chat and a manual plan override. Start the app,
confirm the documents appear, quit, remove only the disposable cache directory, and
reopen. Test a deliberately unwritable migration destination and two app instances
editing the same fixture document. Failure includes missing conversation, damaged JSON,
silent last-writer data loss, or deletion of the only valid migration source.

**Gates:** shared gates, storage/migration units and explicit local filesystem contracts.

---

## IR-07: Model remote mutations and unknown outcomes explicitly

**Priority:** P1. **Findings:** F12; dry-run thread-state follow-up.\
**Requirements:** FR-6.3–6.5, NFR-3.4. **Depends on:** IR-05, IR-06.
**Status:** in progress on `ir-07-remote-mutations`.

### Delivered outcome

Reviews, replies, conversation comments and thread resolution use one truthful mutation
lifecycle. Esc/timeout cannot falsely guarantee nothing was sent or make an unresolved
submission immediately retryable. A successful publish cannot clear newer writing.

### Start in these areas

- `src/application/drafts.rs` and discussion/posting use cases.
- `src/tui/drafts.rs`, `app.rs`, `mod.rs`: preview, confirmation and cancel handling.
- `src/tui/jobs.rs`: Review/Post/thread mutation jobs.
- `src/adapters/gh/review.rs`, `gh/comments.rs`, forge ports.
- IR-06 durable operation storage.

### Implementation steps

1. [ ] Introduce a typed operation snapshot: ID, subject/revision, mutation kind,
   submitted draft version/body/comments or target thread, timestamps and state.
   Persist the operation before network dispatch; failure to record it preserves the
   draft and refuses dispatch with a next action.
2. [ ] Separate preview/confirmation from dispatch. All effects use the confirmed
   immutable snapshot, not a mutable modal or the PR currently on screen.
3. [ ] Route cancellation to the actual mutation job kind. Before dispatch it can be
   definitely canceled; after dispatch it may become outcome unknown. Never translate
   killing the local `gh` child directly into “not sent.”
4. [ ] Block duplicate dispatch for the same unresolved operation, including repeated
   Enter, modal close/reopen, navigation, refresh, and restart recovery.
5. [ ] Reconcile unknown outcomes through forge reads using the strongest available
   identifiers: returned review/comment IDs, current-user/subject/revision/payload
   evidence, and thread state. Treat ambiguous matches conservatively; a local hash
   is not a remote idempotency key.
6. [ ] If exact reconciliation is impossible, show “outcome unknown” and a useful
   GitHub link/next action. Do not claim exact-once semantics or silently repost.
   An intentional resubmission must be a new explicit decision with the uncertainty
   visible, not an automatic transport retry.
7. [ ] On known success, clear/remove only the submitted draft version. Preserve edits
   staged after dispatch and keep the remote success even if local cleanup fails.
8. [ ] Make simulated dry-run a distinct outcome. Record commands/payload artifacts,
   preserve the draft, and do not change fetched thread resolution or review state as
   if GitHub acknowledged a real mutation.
9. [ ] Refresh affected remote data after known success without reloading/erasing the
   current composer. Durable operation recovery must work after restart.
10. [ ] Keep existing confirmation interaction unless the owner explicitly approves
    simplifying it to one deliberate Publish action. The HTML mockup is a recommendation,
    not approval to remove an existing safety boundary.

### Required controlled schedules

- Extra Enter while the fake response is held: one POST, not a post-success replay.
- Reply dispatch followed by Esc cancels/tracks Post, not an unrelated Review slot.
- Server accepts POST, response is lost: outcome unknown; retry guard persists.
- Confirmed rejection or canceled-before-dispatch: safe retry path preserves exact text.
- Success arrives after navigating to B: A is reconciled; B is unchanged.
- Submit version 3, edit version 4, then success: version 4 is not cleared.
- Restart in dispatching/unknown state: operation is recovered and reconciled.
- Dry-run reply/conversation/resolve: zero mutating forge calls and no false remote tick.
- GraphQL HTTP-success body containing errors remains a rejection/failure, not success.

### Manual acceptance recipe

Use a fake forge that separately exposes “accepted” and “responded” controls. Publish
and hold the response; press Enter/Esc, close/reopen the modal and restart. Inspect the
operation record, request count, visible uncertainty and draft. Release a success and
confirm correct cleanup. Repeat reply and resolve under dry-run. Failure includes a
second POST, a false “not sent,” a real-looking dry-run tick, or lost newer comments.

**Gates:** shared gates, deterministic mutation scenarios, `m4` and `m5` contracts.

---

## IR-08: Cancel real work and bound progress delivery

**Priority:** P1. **Findings:** F16; progress loss and stalled-worker issues.\
**Requirements:** FR-4.4, FR-5.2, NFR-1.2, NFR-1.4, ARCH-5.\
**Depends on:** IR-02, IR-05, IR-07.

### Delivered outcome

Canceling a read/LLM job interrupts waiting before the first byte and between chunks,
releases its worker slot, and prevents queued updates from restarting the view. Progress
has bounded memory and per-frame work without dropping arbitrary text fragments.

### Start in these areas

- `src/ports`: cancellation contract.
- `src/adapters/llm.rs`, `http.rs`, `process.rs`.
- `src/tui/jobs.rs`: capacity, queueing and progress channels.
- `src/tui/app.rs`: stopped states and progress acceptance.

### Implementation steps

1. [ ] Make cancellation observable by async waits. Race connection/request and each
   stream-next future against cancellation using existing Tokio primitives.
2. [ ] Apply the same behavior to nonstreamed completion and catalog fetch. Enforce
   request deadlines independently from cancellation and distinguish their messages.
3. [ ] Preserve already accepted partial text and mark it partial/stopped. Reject later
   deltas/completions through the session/job generation established in IR-05.
4. [ ] Ensure canceled tasks actually finish/drop their IO futures and release capacity.
   Four stalled canceled LLM jobs must not leave all slots occupied.
5. [ ] Replace the unbounded drain-that-discards-after-256 pattern. Use bounded/coalesced
   progress with an explicit policy: sequenced lossless deltas with backpressure, or
   cumulative preview snapshots with monotonic revision. Terminal completion must carry
   the authoritative full result and must not be lost behind progress traffic.
6. [ ] Bound per-loop drain time/message count. A busy producer cannot starve keyboard,
   mouse, repaint, cancellation or mutation reconciliation.
7. [ ] Show queued, running, waiting, streaming, canceled, failed and completed states
   consistently. Cancellation acknowledgment and worker release are different assertions.
8. [ ] Investigate the process-descendant hypothesis with a local fixture child that
   spawns a helper. Prove whether cancellation/timeout leaves descendants alive. If so,
   implement owned-process-tree cleanup using approved safe platform facilities; ask
   before any new dependency/FFI requirement. Never use broad process-name killing.

### Required regression cases

- Future pending before headers/first chunk; cancel completes and frees a slot.
- First chunk received, next chunk pending; partial text remains and cancellation works.
- Plain nonstreamed request and catalog fetch canceled while pending.
- Successful completion/delta queued before Esc but reduced afterward cannot revive
  stopped state or affect a new subject.
- Four held jobs canceled, then a fifth ordinary job starts promptly.
- Producer flood has bounded queue/memory, contiguous preview semantics and bounded
  reducer work. Final response equals the fake provider's exact content.
- Deadlines produce timeout rather than “user canceled.”
- Local process fixture verifies direct-child and descendant cleanup, recording any
  platform limitation explicitly if it cannot be fixed within approved dependencies.

### Manual acceptance recipe

Run the fake provider in stall-before-first-byte and stall-between-chunks modes. Start
work, press Esc, immediately navigate/open another PR and start a harmless background
action. Verify stopped text stays stopped and job slots recover. Use a deliberately
fast stream and verify no preview holes or frozen controls. Record elapsed cancellation
acknowledgment and actual worker release separately.

**Gates:** shared gates, controlled async/process tests, `m2b`/`m3` cancellation contracts.

---

## IR-09: Unify layout, focus, hit-testing and visible selection

**Priority:** P1. **Findings:** F08, F09.\
**Requirements:** FR-3.3–3.4, FR-6.2, FR-6.4, FR-7.1, FR-7.5, FR-7.8.\
**Depends on:** IR-04, IR-05.

### Delivered outcome

Typing goes to the visibly focused composer. Clicks select the displayed line in both
diff modes. Keyboard selection remains visible. Popups own their input instead of
scrolling or changing the hidden screen underneath.

### Start in these areas

- `src/tui/layout.rs`, `app.rs`, `update.rs`, `input.rs`.
- `src/tui/diff_view.rs`, `chat.rs`, `discussion.rs`, `drafts.rs`.
- `src/tui/components/review.rs`, `conversation.rs`, chat/popup/composer components.

### Implementation steps

1. [ ] Add the seven existing UX probe journeys as appropriate deterministic application/
   TestBackend regressions; this PR owns focus, viewport, split-click and overlay/
   discussion-scroll cases, not the tab/order cases assigned below.
2. [ ] Introduce explicit focus targets for file list, diff, chat transcript, chat input,
   comment composer, conversation list, and overlay controls as each exists.
3. [ ] Compute a layout model once for the current frame/size/state. Rendering,
   viewport preparation, scrolling, hit-testing and focus use the same rectangles.
   Subtract chat/composer/help space consistently; account for borders and titles.
4. [ ] Keep visible-row-to-source mappings for unified and split diff rendering. Hit-test
   against those mappings, including paired additions/deletions, headers, threads,
   folds and noncommentable placeholder rows.
5. [ ] Route input through the topmost overlay first. If a popup does not handle a mouse
   gesture, it must not accidentally pass it through to an obscured background pane.
6. [ ] Define Tab traversal across visible focus targets. An open draft must not capture
   typing after focus moves elsewhere; switching focus preserves its text.
7. [ ] Give every list/viewport an `ensure_visible(selection)` equivalent. Align
   discussion's scroll convention with rendering, select/show the newest comment
   consistently, and handle wrapped multirow comments.
8. [ ] Preserve top-line/selected source anchors across resize. Below minimum size show
   the existing too-small state without losing the underlying review position.
9. [ ] Make the visible focus label/border accurate and update help/keymap descriptions.

### Required regression matrix

- Sizes: 80×24, 120×30, 160×40, below minimum; split toggle around 140 columns.
- Diff alone, chat open, composer open, both where supported, overlay active, resize.
- Comment X → Tab to Chat → Y goes only to chat; return to composer still has X.
- Moving/paging/bottom keeps the selected source row visible at every size.
- Click first/middle/last split rows maps to displayed path/side/line, including unequal
  deletion/addition runs and inline thread rows.
- Wheel/click on discussion/help/model/publish overlay leaves underlying diff unchanged.
- Long conversation j/k/page/wheel reveals selection, including newest comment on open.
- Wide characters, wrapping and border clicks do not produce off-by-one anchors.

### Manual acceptance recipe

Open a long fixture diff at 160×40, add Chat, return to Diff and page to the bottom.
Create a comment on a visible line and verify the target. Switch focus between comment
and chat while typing distinct markers. Toggle split view, click visible added/deleted
lines, then open a long Discussion popup and scroll it. Resize to 80×24. Failure is
invisible selection, wrong text destination, wrong comment coordinates or background
movement through an overlay.

**Gates:** shared gates, intentional snapshot regeneration/review, `m1`, `m4`, `m5`.

---

## IR-10: Ship real PR tabs and complete Checks and Discussion

**Priority:** P1. **Findings:** F06; missing/outdated discussion discoverability.\
**Requirements:** G2, FR-2.4, FR-6.4, FR-7.2–7.5, FR-7.8.\
**Depends on:** IR-05, IR-07, IR-09.

### Delivered outcome

Every visible tab is a real destination. The reviewer can read the author's PR
description, named CI checks, review bodies, all thread states and conversation through
obvious keyboard/mouse navigation. Counts refer to the data they label.

### Start in these areas

- `src/tui/components/review.rs`: static numbered header.
- `src/tui/action.rs`, keymap registry, `update.rs`, `app.rs`.
- `src/domain/pr.rs` and existing forge detail/check/thread mapping.
- Discussion/conversation renderers; new tab view state/components as needed.

### Implementation steps

1. [ ] Implement the §3.5 tab model and actual selected state. Derive header labels,
   counts, keyboard actions, click rectangles and help from the same definitions.
2. [ ] Bind normal-mode 1–5 and expose tab actions in the registry/palette. Preserve
   typing of digits in every text-entry mode. Remove the fake unavailable header
   states and wrong Analysis approval/request-changes count.
3. [ ] Overview: title/author/state/base/head, author description, commits/summary metadata,
   review decision, checks/discussion overview, and analysis/setup/progress state.
   Keep author text clearly separate from later AI-generated inference.
4. [ ] Files: existing corrected tree/diff/composer workflow. Preserve cursor, folds,
   order and scroll while the user visits another tab.
5. [ ] Checks: render names, queued/running/completed state, conclusion and available
   run URLs. Do not treat an empty check list as success. Show unavailable/stale data
   distinctly and retain readable cached data during refresh.
6. [ ] Open selected run/details URLs via a runtime effect and fakeable external-link
   boundary. Use argv arrays and validate supported URL schemes. Opening a browser
   must not perform IO in a renderer or break terminal restoration.
7. [ ] Discussion: combine reachable lists for review bodies, inline threads and PR
   conversation. Provide All/Open/Resolved/Outdated filters with honest counts and
   explicit empty states. Preserve parent/reply grouping.
8. [ ] Keep outdated threads readable even when no current diff anchor exists. Jump to
   code only for a valid current mapping; otherwise explain why that jump is unavailable.
   Never attach the comment to a nearby surviving line.
9. [ ] Route reply/resolve/comment actions to the IR-07 lifecycle. Refresh results
   in-place and keep active selection stable.
10. [ ] Ask: expose chat transcript/input as the real tab. Existing leader shortcuts
    select/focus the appropriate destination instead of creating an unrelated state.
11. [ ] Implement an 80×24 layout with full-width active content and a collapsible file
    list; do not squeeze all panels simultaneously. Update generated docs and snapshots.

### Required regression cases

- Every visible tab reached by numeric key, registry action and mouse click.
- Tab selection independent of pane focus; digits in input never switch tabs.
- Return to Files restores selected source row, fold state and scroll.
- Checks cover queued, running, success, failure, canceled, skipped and no configured
  checks; link effect targets the selected check URL and is easy to fake.
- Discussion includes review body without inline comments, outdated thread, orphan
  reply, resolved thread and top-level comment; no double-counted entries.
- Refresh unavailable/offline data keeps an explicit stale/error label and existing
  readable content; a remote mutation does not masquerade as completed on dry-run.
- All destinations usable at 80×24, including long descriptions and empty states.

### Manual acceptance recipe

Open a fixture containing one failing check, one passing check, a review body, an open
inline thread, an outdated thread and conversation. Use 1–5 and then click each label.
Open the failing run through a fake link opener, filter Discussion, read the outdated
thread and return to the exact Files position. Repeat at 80×24. Failure includes a
label with no destination, wrong count, hidden discussion, or a lost file position.

**Gates:** shared gates, generated keymap check, reviewed snapshots, browsing and M5
contracts updated to the actual new user journey.

---

## IR-11: Make the review order executable everywhere

**Priority:** P1. **Findings:** F07.\
**Requirements:** G4, FR-3.4–3.5, FR-4.2. **Depends on:** IR-05, IR-09.

### Delivered outcome

Recommended order is the actual reading sequence. Tree selection, ordinary cross-file
scrolling, next/previous file, hunk movement and displayed positions agree. Toggling
order never loses files or changes the selected file unnecessarily.

### Start in these areas

- `src/domain/plan.rs`: effective file order, overrides and position lookups.
- `src/tui/diff_view.rs`: tree construction, rows and navigation.
- `src/tui/components/review.rs`, status line and order controls.

### Implementation steps

1. [x] Add the application→domain→TUI patch/recommended-order regression. Assert the
   next file after domain is application, including hunk movement across the boundary.
2. [x] Introduce stable canonical file IDs plus an effective-order projection. Keep the
   underlying parsed diff immutable. Do not use current tree row numbers as file IDs.
3. [x] Make every relevant navigation path consume the projection. Define behavior at
   first/last files and folded/empty/binary files; apply it symmetrically forward/back.
4. [x] Ensure ordinary diff rows across file boundaries follow the selected order, or
   deliberately scope the diff to one file with explicit next/previous navigation.
   Choose the existing architecture's least disruptive option and document it; the
   user must never see tree order disagree with the next file they read.
5. [x] Preserve current path and source-side/line anchor on order toggle/plan arrival.
   Reconcile folds and independent tree/diff cursor state by stable IDs.
6. [x] Guarantee every changed file appears exactly once, including LLM omissions,
   invalid/duplicate plan entries and newly changed files after refresh.
7. [x] Preserve manual group order and file placement when compatible. Show AI,
   heuristic and user override provenance distinctly. Use unclassified as a visible
   fallback, not a hidden filter.
8. [x] Cache file-position maps for both orders rather than reconstructing complete
   path lists every status render. Make group rationale expandable/readable.

### Required regression cases

- Recommended order intentionally differs from patch/path order; all movement paths
  produce the same sequence and reverse sequence.
- Toggle and incoming plan preserve current file/line wherever it remains available.
- Duplicate, unknown and omitted files never hide or duplicate actual changes.
- Folded file, no hunks, mode-only, rename, binary and empty diff boundaries.
- Manual override survives reopening at a compatible revision and shows its source.
- Tree/diff status positions reflect the correct selected order after every action.

### Manual acceptance recipe

Use a three-layer fixture with intentionally conflicting alphabetical and suggested
orders. Start at the first recommended file and walk all files with next-file and
cross-file hunk keys. Toggle path order in the middle, then toggle back. Move a group,
restart and check the order. Failure includes skipping the next plan file, duplicates,
hidden changes, or a jump away from the current file when the plan arrives.

**Gates:** shared gates, order/navigation units and snapshots, `m1`/`m2b` contracts.

---

## IR-12: Unify context identity, caching and evidence attribution

**Priority:** P2. **Findings:** F03, F05; reduced-hunk coordinate follow-up.\
**Requirements:** FR-4.1–4.3, FR-4.6, FR-5.3.\
**Depends on:** IR-01, IR-03, IR-05.

### Delivered outcome

Analysis, chat and `:context` agree about user-added files and exclusions. Cached
analysis cannot be served as an answer to a materially different effective request.
AI references resolve to the right file and valid source coordinates.

### Start in these areas

- `src/application/context.rs`: `ContextSource` and gathering.
- `src/application/analysis.rs`, `chat.rs`, request construction in `tui/app.rs`.
- `src/ports/analysis.rs`, `src/adapters/analysis_cache.rs`.
- `src/domain/analysis.rs`: `PathIndex`, normalization, prompt versions.
- `src/domain/context.rs`: reduced patch rendering.

### Implementation steps

1. [x] Replace divergent analysis/chat context inputs with a shared immutable
   `ContextSpec`: subject/revision, canonical change set, conventions policy,
   user additions/exclusions, budget policy and relevant versions.
2. [x] Make context inspection, estimate and dispatch consume the same resolved bundle/
   manifest. User-added files must affect fresh analysis as well as chat. Changing
   additions, exclusions, model budget or revision invalidates incompatible prepared
   bundles immediately.
3. [x] Define whether presentation-only context/whitespace toggles alter LLM input.
   Recommended default: analysis uses the canonical review diff and explicit context
   controls, independently of visual whitespace hiding. State the choice in the UI
   and contract; if a control changes actual input, include it in context identity.
4. [x] Extend cache identity/provenance to include relevant base/head/merge-base,
   context-spec/manifest fingerprint, prompt/schema/policy versions, endpoint/model
   identity and effective generation settings. Never include API key values.
5. [x] Preserve fast cache reopening: compare immutable revision/object IDs and policy
   metadata before regathering all source bytes. A cache hit must remain free of paid
   provider calls. Use full stored identity verification in addition to a digest so
   a digest collision is a miss, not the wrong answer.
6. [x] Version/migrate cache entries conservatively: old unverified entries can be
   displayed as legacy/stale or ignored as disposable, never silently certified as
   current. Do not delete durable chat to invalidate analysis.
7. [x] Build exact canonical paths first, count all basenames, and create a basename
   alias only when globally unambiguous. Keep rename aliases distinct from canonical
   paths and avoid stripping a real leading `a/` from an exact source path.
8. [x] Preserve valid hunk coordinates when reducing context. Split disjoint surviving
   ranges into separate hunks or render explicit per-line coordinates; do not join
   separated lines under a fictitious contiguous header.
9. [x] Preserve evidence-path information in panel/view models. When normalization
   removes every cited path for a claim, mark that claim unsupported/diagnostic rather
   than presenting it as an evidenced actionable risk. Do not invent file references.
10. [x] Keep historical chat/analyzed provenance distinct from the current context
    inventory. Explain stale analysis and offer an explicit rerun without auto-spending.

### Required regression cases

- Add an unmodified supporting file; inspection, fresh analysis and chat all include it.
- Remove/add/change context after an estimate: old prepared bundle is not dispatched.
- Same head, different base or effective context: no incorrectly current cache hit.
- Same exact identity reopened: no provider call, correct model/age/source metadata.
- Two `mod.rs` paths reject bare `mod.rs`; exact paths and unambiguous rename aliases work.
- Exact canonical path beginning with `a/` resolves correctly.
- Two edits separated by removed context retain their original line numbers in output.
- Unknown risk/note references do not become clickable incorrect paths.
- Older cache schema/policy does not corrupt state or invalidate durable documents.

### Manual acceptance recipe

Choose a PR needing one unchanged supporting file. Add it through `:context add`,
compare the inspector and captured analysis/chat manifests, reopen for a cache hit,
then change context settings and verify the old result is stale/incompatible. Use two
same-basename files and a sparse reduced hunk to inspect links and line coordinates.
Failure is a wrong file jump, hidden context difference or paid call on an exact cache hit.

**Gates:** shared gates, context/cache/path normalization tests, `m2b` and `m3` contracts.

---

## IR-13: Isolate Git workspaces and use explicit revision identities

**Priority:** P1 adapter correctness. **Findings:** F14; .git writes and identity
collisions; remote patch-shape hypothesis.\
**Requirements:** FR-1.3, FR-3.1–3.2, FR-8.1, NFR-3.3; `AGENTS.md` repository isolation.\
**Depends on:** IR-05, IR-06.

### Delivered outcome

Fresh and reused workspaces produce the same intended PR diff. User working trees and
their `.git` remain untouched. Repository/worktree names cannot collide across hosts,
owners and repo names. Remote-only diff semantics are verified against final PR state.

### Start in these areas

- `src/adapters/git/worktree.rs`, `src/adapters/git.rs`.
- `src/domain/repo.rs`, workspace/revision ports, paths and cleanup UI.
- Forge diff command construction and parser fixtures.
- Existing local Git contract fixtures in milestone validators.

### Implementation steps

1. [ ] Reproduce warm-workspace merge-base divergence using stale local main and a
   newer remote main. Fresh/reused paths must resolve the same explicit objects.
2. [ ] Stop passing a bare user-local branch name during reuse. Resolve base/head
   object IDs under the app's own namespace and calculate/store merge base explicitly.
3. [ ] Use complete repository identity and PR/revision identity for workspace readiness,
   reuse and cleanup. Include host and encode components unambiguously; store metadata
   so identity is never reconstructed by splitting a hyphenated directory name.
4. [ ] Follow the current hard rule: create an app-owned bare/object repository under
   `SMART_REVIEW_HOME` and fetch into it; create managed worktrees from that store.
   Do not register new worktrees or update remote refs in the user's clone.
5. [ ] Optional seeding from the local clone must be read-only and must not depend on
   mutable shared alternates or writes to its object store. Prefer correctness over
   a complicated optimization; batching/performance work comes in IR-17.
6. [ ] Keep selected remote/fork handling explicit and pass argv arrays for every
   branch/ref/path. Preserve dirty-worktree independence and detached app worktrees.
7. [ ] Migrate by creating the new app-owned workspace, validating it, then using it.
   Legacy worktrees linked to the user clone must not be auto-removed through a command
   that writes the user's `.git`. Identify them and provide exact manual owner cleanup
   instructions; changing that hard rule requires an owner decision.
8. [ ] Make cleanup operate only on validated app-owned identities and paths. Honor
   dry-run/confirmation and report a stale/missing workspace recoverably.
9. [ ] Investigate `gh pr diff --patch` commit-series semantics with a local fixture
   representing multiple commits, re-edits and renames. Compare remote-only output
   to the final three-dot PR diff. Fix acquisition/parser composition only if the
   mismatch is reproduced; do not guess from the flag's name.
10. [ ] Make remote-only limitations explicit where exact base/head provenance cannot
    be obtained. Do not allow intermediate-commit anchors to look like current PR anchors.

### Required regression cases

- Fresh and reused workspace with stale/missing local base branch.
- Same head but changed base; fork PR; alternate remote; dirty source clone.
- Hosts differ; `a-b/c` versus `a/b-c`; mixed-case identity normalization.
- Snapshot source working-tree/index/HEAD/refs/.git metadata before and after app
  operations; no changes attributable to the application.
- Legacy migration interruption, missing workspace, cleanup dry-run and invalid path.
- Multiple-commit remote patch with a file edited twice, rename, deletion and binary
  change compared with the final PR diff and expected comment anchors.
- Cache reuse keyed to the same resolved revision, never an old merge base.

### Manual acceptance recipe

Create a disposable source clone with uncommitted changes and intentionally stale local
main. Open the fixture PR twice, compare diff and revision provenance, and inspect the
source clone's snapshot/refs before and after. Verify Git data lives entirely in the
fixture home. Open a fork and the colliding-name fixture identities. Run cleanup in
dry-run and real app-owned mode. Failure is source `.git` mutation, changed merge base
on reuse, collided workspace, or intermediate diff anchors.

**Gates:** shared gates, explicit local Git/forge contracts, `m1` and `m2a` plus affected
context/publish contracts. No default unit test requires a real repository or network.

**Implementation status (2026-09-17):** implemented on `ir-13-git-workspace-isolation`.
Workspaces now use an app-owned bare object store, explicit base/head refs and an
encoded host/owner/repository identity. The local Git contract covers source-ref and
`.git/worktrees` isolation, stale-source reuse, collision-free names and legacy cleanup
refusal. The remote-only path remains `gh pr diff <number> --patch`, which the CLI
documents as the selected pull request's changes; it is parsed directly as one final
patch (including rename, binary and mode-only entries), with no intermediate commit
anchors composed by the application.

---

## IR-14: Move IO out of the UI and order background saves

**Priority:** P2 responsiveness and architecture. **Findings:** event-loop IO and
repeated synchronous persistence.\
**Requirements:** ARCH-1, ARCH-5, NFR-1.2, NFR-4.1.\
**Depends on:** IR-05, IR-06, IR-07, IR-08.

### Delivered outcome

Normal input, state reduction and rendering do not read/write files, run Git, or call
adapters. Persistence happens in ordered jobs with explicit acknowledgments. The app
stays responsive on slow storage without lying about whether writing is saved.

### Start in these areas

- `src/tui/mod.rs`: state/draft/plan/model/key/export/prune effects.
- `src/tui/app.rs`: `path_exists_at_head`, chat persistence and request secret loading.
- Composition/bootstrap, job runner and storage/application use cases.
- Theme/keymap/config reload paths and external editor/link integration.

### Implementation steps

1. [ ] Inventory every UI/reducer path reaching filesystem, credentials, logging,
   process, clock or network behavior. Distinguish terminal drawing/input itself from
   unrelated IO and include “small local file” exceptions currently documented.
2. [ ] Move IO into the runtime/application effect boundary. The UI receives values,
   prepared settings, readiness and save outcomes; it does not call concrete adapters.
3. [ ] Make context-add path validation asynchronous, with checking/accepted/rejected
   states. Reuse known immutable workspace inventory when available rather than
   launching `git show` synchronously for existence.
4. [ ] Queue saves by document identity and revision. Coalesce superseded pending
   snapshots only when safe; serialize actual writes. Acknowledgment for version 3
   cannot clear dirty version 4. A PR switch cannot change the save's destination.
5. [ ] Route chat saving, indexes, plan overrides, config/model/key writes, exports,
   retention and explicit reloads through the same runtime conventions where appropriate.
   Sensitive requests must not expose key material in job Debug/progress output.
6. [ ] Keep previously loaded themes/keymaps ready for immediate selection. Explicit
   disk reload can show pending state and atomically replace valid results.
7. [ ] Show saving/saved/save-failed with a recovery next action. Avoid a notification
   storm for every keystroke; use persistent per-document state plus meaningful errors.
8. [ ] Define orderly shutdown: stop accepting new edits, flush the latest durable
   versions or preserve a recoverable journal, restore the terminal on every path,
   and report unresolved save failure without hanging indefinitely. Preserve the
   existing stronger “question stored before asking” behavior for chat dispatch.
9. [ ] Keep IR-07 mutation records on the durable dispatch path: do not send before
   the operation-record acknowledgment just to make the UI appear faster.
10. [ ] Add an architecture boundary check for forbidden adapter/process/IO dependencies
    in reducer/render modules. Prefer a narrow module-level rule plus behavior tests;
    avoid a fragile grep pretending to prove absence of indirect IO.

### Required regression cases

- Fake storage holds a save indefinitely: key/mouse/navigation and frames still process.
- Edits v1→v2→v3 with delayed acknowledgments persist v3 and report the right dirty state.
- Saving A while navigating to B writes only A's document.
- Save error preserves in-memory data and displays a retry/recovery action.
- Context-add slow/failure path never blocks normal navigation.
- Config/key/theme reload completion is generation/version checked.
- Exit/signal with pending save follows the documented flush/recovery behavior and
  terminal guard restoration; mutation dispatch waits for durable-record completion.

### Manual acceptance recipe

Inject slow/failing storage in an isolated fixture. Type/stage comments, switch tabs,
change PRs and issue context-add while saves are pending. Verify responsive movement
and truthful save labels. Quit and reopen, checking that the latest acknowledged/
recovered text belongs to the right PR. Failure includes a frozen UI, older save
overwriting newer text, false “saved,” or a terminal left in raw mode.

**Gates:** shared gates, controlled save-order scenarios, terminal and publishing contracts.

---

## IR-15: Make the terminal harness trustworthy and remove repeated gates

**Priority:** P2, high leverage; can be assigned early.\
**Findings:** repeated CI gates, fixed settles, full replay, ignored wait failures.\
**Requirements:** NFR-5.2; existing CI/milestone validation promises.\
**Depends on:** none; coordinate UI-ready labels with IR-09/10.

### Delivered outcome

A failed wait fails the scenario. Terminal capture is replayed incrementally. CI runs
common gates once and validators consume the prebuilt binary. Fixed settling waits
are replaced with observable conditions rather than simply made shorter.

### Start in these areas

- `scripts/validate/drive.py`, `screen.py`.
- `scripts/validate/m0.sh` through `m5.sh`, fake provider scripts.
- `.github/workflows/ci.yml` and testing instructions.

### Implementation steps

1. [ ] Record current wall time per validator and the number of build/test invocations.
   Keep static wait budgets separate from measured duration. Avoid cold/warm comparison.
2. [ ] Make unmet readiness/step waits, mismatched key/wait lengths, unexpected child
   exit and global timeout return nonzero immediately. Save a useful last frame and
   raw capture. Shell callers must propagate failure rather than continue as success.
3. [ ] Add regression fixtures for the driver itself: one unmet wait must fail even
   if a later screen happens to contain a generic success phrase.
4. [ ] Keep an incremental terminal parser with persistent cursor/buffer/state. Feed
   only new bytes; carry incomplete UTF-8 and escape sequences across read boundaries.
   Avoid repeated suffix copying at every escape sequence.
5. [ ] Test parser compatibility with the sequences the app emits, resize, Unicode,
   alternate-screen/reset and fragmented reads. Preserve exact frame semantics used
   by existing assertions before changing their expected output.
6. [ ] Replace empty `--settle 3` waits with local-diff-ready, modal-ready, job-completed,
   operation-recorded or fake-provider acceptance conditions. Keep deliberate delays
   only for behaviors specifically testing streaming/cancellation.
7. [ ] Replace unconditional shutdown settling with waiting for owned-child exit and
   final capture drain, under a bounded deadline. Verify terminal restoration after exit.
8. [ ] Correct the M4 in-flight test: hold the response, send the extra Enter while
   actually in flight, assert call count, then release the response.
9. [ ] Add explicit prebuilt/scenario-only validator mode; retain standalone mode for
   local use. CI runs fmt, Clippy, full offline Rust tests and build once, then invokes
   scenario-only validators. Consolidate filtered docs/parser checks into the main suite.
10. [ ] Use unique per-invocation temporary roots and tracked child PIDs. Replace broad
    `pkill -f` cleanup. Only after isolation tests pass consider parallel validators.
11. [ ] Update CI artifacts/timing summaries. Keep useful failure captures private to
    test fixtures; do not include actual user credentials/content in diagnostics.

### Required regression cases

- Ready/step timeout exits nonzero and stops later key dispatch.
- Incremental replay equals whole replay for every byte/chunk split of representative
  escape/UTF-8 fixtures; old assertions retain intended meanings.
- Two concurrent fixture drivers do not share paths or kill each other's process.
- Prebuilt mode invokes no nested build/full-test gates; standalone mode still works.
- Waits observe the current scenario/state rather than stale earlier capture text.
- In-flight repeated Enter is injected before success and detects a deliberately
  duplicate-dispatch fake, proving the assertion can fail.

### Manual acceptance recipe

Run the same M4/M5 scenarios before/after with warm artifacts and record totals. Make
one expected pattern impossible; the driver must fail promptly with the relevant frame.
Run two isolated smoke invocations concurrently. Failure includes a zero exit after
an unmet wait, lost ANSI state, cross-process cleanup or weakened assertions.

**Gates:** shared gates, Python driver/parser tests, all changed validators in standalone
and/or prebuilt modes as appropriate. Record actual speedup; do not claim the 175.7 s
static budget is the exact wall-time saving.

---

## IR-16: Integrate a concise guided review into the file workflow

**Priority:** P2 product. **Findings:** report's user-level redesign and explanation gaps.\
**Requirements:** G4, FR-3.5, FR-4.1–4.4, FR-5.3.\
**Depends on:** IR-03, IR-10, IR-11, IR-12.

### Delivered outcome

The app helps the human review rather than presenting a separate long AI essay. The
current file has a brief explanation and clear next step; the full explanation remains
accessible. Human progress is explicit and never inferred from AI completion.

### Start in these areas

- `src/domain/analysis.rs`, `plan.rs`: output contract, prompt and normalization.
- `src/application/analysis.rs`: panel/view data and attribution.
- Overview/Files components from IR-10, order projection from IR-11.
- Durable manual review state from IR-06 and context provenance from IR-12.

### Implementation steps

1. [ ] Use the HTML mockup and §3.5 as the product baseline, with actual terminal widths.
   Confirm material behavior changes with the owner, including grouping semantics,
   reviewed markers and any confirmation simplification. Record approved decisions.
2. [ ] Evolve the structured output contract with a prompt/schema version bump:
   - PR brief: approximately two concise sentences.
   - Inferred purpose, clearly separate from the author's description.
   - Review steps with human names, ordered files and a one-sentence rationale.
   - Per-file `what`, inferred `why`, up to two concrete checks and evidence references.
   - Explicit coverage/limitations and suggested follow-up questions.
3. [ ] Guide the model toward short fields, but keep full useful text in an expandable
   detail view instead of silently discarding an overlong response. Bound parsing/
   rendering and retain the existing one-repair/diagnostic behavior.
4. [ ] Remove the blanket prompt rule “tests and docs last.” Ask for a dependency/
   understanding sequence that fits this PR: contract/examples may lead, tests may
   accompany behavior, generated/mechanical changes may follow but stay included.
5. [ ] Keep ordering advisory. Show AI/heuristic/user provenance, an immediately visible
   path-order toggle, and exact changed-file coverage. Preserve current selection when
   partial/final analysis arrives; do not jump the reviewer away automatically.
6. [ ] Overview shows the brief and full plan. Files shows a compact current-file
   explanation near the diff: What / Why (inferred) / Verify. At 80×24 collapse to
   a few lines with an explicit expand action; the code remains the main surface.
7. [ ] Evidence links jump to validated files/coordinates. Missing evidence or truncated
   source is visible. Suggested questions enter the compose box for user review;
   they do not auto-send paid requests.
8. [ ] Add explicit human review markers: not reviewed, reviewed, needs revisit.
   Bind them to a stable file-change fingerprint. On a new head, carry over only
   provably unchanged changes; changed/ambiguous files need revisit. Never mark a file
   reviewed merely because the LLM analyzed it or the cursor passed through it.
9. [ ] Preserve manual plan overrides and markers durably. Explain invalidated overrides
   after revision changes and offer reset without silently replacing user choices.
10. [ ] Show analysis phase, elapsed time, model/settings, cache age, current/stale status
    and partial coverage without overwhelming the normal reading view.
11. [ ] Add offline/no-model/failed-model journeys: browsing, checks, discussion and local
    drafting remain useful; AI setup and retry have clear next actions.

### Required regression cases

- Short/long/malformed/partial responses, no per-file note, unknown evidence and omitted
  plan file render honest usable states.
- Plan ordering supports implementation+test pairs, migration-first and docs/contract-first
  examples without hard-coded DDD-only expectations.
- Current file stays selected when analysis streams/completes or cached plan loads.
- Compact guidance usable at 80×24; expanded view remains fully readable/scrollable.
- Explicit review marker survives restart/cache eviction; changed content invalidates
  only incompatible markers and never inherits AI completion as human review.
- Suggested question populates input but dispatch count stays zero until user sends.
- New prompt/schema does not label legacy cached output as a fresh matching analysis.

### Manual acceptance recipe

Use three distinct fixture PRs: small bug fix, multi-layer feature and large mechanical
change with a small important behavior edit. Starting without prior knowledge, explain
what changed, why the first step comes first and what to verify using the UI alone.
Walk the plan, expand a note, ask a suggested question, mark files reviewed and reopen
after a new head. Failure is an essay obscuring the code, hidden files, unexplained
order, ungrounded links, auto-spend or falsely preserved human review progress.

**Gates:** shared gates, normalization/prompt contract tests, reviewed UI snapshots and
guided-review smoke. Owner review of actual 80×24 and wide-terminal output is required.

---

## IR-17: Optimize measured runtime and context hotspots

**Priority:** P2 performance. **Findings:** repeated render work, context process count,
event-loop cadence and weak performance assertions.\
**Requirements:** NFR-1.1–1.4, NFR-5.3, FR-3.3.\
**Depends on:** IR-08, IR-11, IR-13, IR-14, IR-16.

### Delivered outcome

The app is fast on realistic large PRs and long conversations, not merely fast at slicing
a vector. Background progress arrives promptly, idle redraw work is limited, and context
gathering does not create one Git process per changed file.

### Start in these areas

- Main event loop/runtime scheduling and render invalidation.
- Chat/analysis components, Markdown rendering and stream preview projection.
- Diff view construction, split mapping, comments, status totals/order maps.
- App-owned Git object reads and context gatherer.
- Logging/job spans and actual performance fixture/runner.

### Implementation steps

1. [ ] Capture a reproducible before baseline in release mode on a named reference
   machine: actual reducer+draw, input-to-frame latency, first usable diff, context
   gather duration, peak memory, process count and idle frame count. Also record warm
   debug behavior for developer ergonomics. Do not compare unlike profiles.
2. [ ] Consolidate the loop into a bounded event/progress drain, reduction and one
   necessary draw. Use dirty state plus animation/deadline scheduling; cap waits to
   meet background presentation deadlines even while a key prefix is pending.
3. [ ] Preserve confirmation visibility: a publish preview must have been rendered
   before its confirmation input can dispatch. Redraw optimization must not erase
   this state-machine boundary.
4. [ ] Cache completed chat-message layout keyed by message revision, width and theme.
   Lay out only necessary viewport content and the changing stream tail. Respect
   explicit follow-tail versus user-scrolled-up state.
5. [ ] Incrementally/coalescedly update analysis preview instead of reparsing complete
   partial JSON on every idle draw. Keep authoritative final normalization unchanged.
6. [ ] Prepare expensive pure diff projections in bounded background work; keep terminal
   drawing on its owning thread. Avoid initial build+comment rebuild duplication and
   build split rows only when needed.
7. [ ] Cache totals, stable row anchors and effective-order position maps. Invalidate
   by actual revision/options/comments, not every frame.
8. [ ] Batch Git object reads from the app-owned repository, using an existing Git batch
   interface behind the workspace adapter. Check blob sizes/types before buffering,
   enforce IR-01 eligibility, and stop promptly on cancellation. Avoid unbounded
   parallel `git show` as a substitute for batching.
9. [ ] Release superseded view/context buffers and cap retained state. Track memory of
   diff projections, raw/parsed analysis and chat layouts separately.
10. [ ] Add structured timings per job/phase with identity, duration, outcome and counts,
    never contents. Report useful progress counts for context/workspace phases and
    elapsed time for unknown-duration provider work; do not invent progress percentages.
11. [ ] Replace the rows-only “thousand frames” assertion with actual meaningful draw/
    reducer measurements. Keep algorithmic/property assertions deterministic and
    machine timing thresholds broad enough for CI variation.

### Required performance workloads

| Workload | What it must exercise |
|---|---|
| 400 files / 10,000 diff lines | Parse, prepare, initial draw, scroll, next file/hunk |
| Long lines + Unicode | Width calculations, split rendering, click mapping |
| Dense discussion/draft markers | Comment projection and viewport correctness |
| Long chat near configured retention limit | Stored-message layout, scroll, stream append |
| Large streamed analysis | Preview parse/projection and final normalization |
| Large context with oversized/binary files | Eligibility, batching, truncation, memory |
| Four background jobs + rapid navigation | Input fairness, cancellation, stale-result discard |
| Idle screen and open leader prefix | Unnecessary renders and delayed progress |

### Targets and interpretation

- Normal navigation p95 input-to-frame below 50 ms on the reference machine.
- Large-diff scrolling at least 30 fps where continuous rendering is requested.
- 10k-line/400-file local fixture becomes usable within the documented 1.5 s opening
  budget; report IO/setup separately when a network checkout is not already available.
- Read/LLM cancellation acknowledged and underlying slot released within the 200 ms
  contract on controlled stall fixtures.
- Retain the documented roughly 300 MB large-PR memory target and state exactly what
  the measurement includes. Investigate before changing the target.
- No arbitrary discarded deltas, wrong row mappings or cursor jumps to gain speed.

### Manual acceptance recipe

Run the reference workload command in release mode and save a before/after table.
Interact with the long diff while context/LLM work runs; scroll up during a stream and
confirm the app does not snap back down. Observe initial cached content and phase cues,
then cancel and immediately start another job. Failure includes unmeasured claims,
slow frames hidden by a rows-only benchmark, unfair input scheduling or excessive memory.

**Gates:** shared gates, correctness fixtures, opt-in reproducible performance runner
and reviewed p50/p95/memory/process-count results.

---

## IR-18: Consolidate deterministic scenarios and a small PTY smoke suite

**Priority:** P2 tests. **Findings:** missing cross-feature coverage and slow/broad E2E.\
**Requirements:** NFR-5.2, ARCH-2/5; relevant feature FRs.\
**Depends on:** IR-15 and IR-17, including their feature prerequisites. Helper extraction
can be prepared earlier, but coverage retirement waits for the completed feature paths.

### Delivered outcome

The default suite quickly proves user-visible state transitions and adversarial job
schedules. A small explicit PTY/adapter suite proves terminal/process wiring. Tests
assert useful invariants instead of mirroring handlers or merely finding header text.

### Start in these areas

- `src/test_support.rs` and existing feature-local tests/fakes.
- `tests/shell_snapshots.rs` and generated keymap checks.
- Runtime/job scenario helpers and fake storage/provider/forge ports.
- `scripts/validate/` and CI scenario-only mode from IR-15.

### Implementation steps

1. [ ] Inventory existing tests and validators by behavior, not historical milestone.
   Map every retained gate to a requirement/finding; identify duplicate setup and
   assertions that never exercise the claimed interval or user action.
2. [ ] Consolidate a minimal deterministic runner around real actions, reducers, job
   outcomes and TestBackend rendering. Use injected time and controlled queues/futures
   to release work in any order without real seconds of sleeping.
3. [ ] Keep domain/property tests at their lowest effective layer. Do not move every
   small pure assertion into a full application scenario.
4. [ ] Reuse the regression schedules added by IR-01–17. Test the composed paths through
   action → effect → fake port → completion → frame, including persistence destinations.
5. [ ] Require observability of the meaningful state: current subject, focused input,
   displayed source anchor, outgoing request counts, document version and mutation
   state. Avoid test-only production shortcuts bypassing actual routing.
6. [ ] Keep snapshots focused on stable representative screens and edge geometry. Strip
   machine-specific paths via production path-shortening behavior; do not hide genuine
   layout changes with permissive text normalization.
7. [ ] Replace historical all-feature PTY repetition with a small set of named smoke
   contracts: startup/restore, key+mouse decoding, remote→local transition, provider
   stream/cancel, publish/reply/dry-run payload, external editor, resize.
8. [ ] Keep default Rust unit/scenario tests free of network, real credentials, `gh` and
   real repositories. Local Git/HTTP/PTY adapter contracts must be explicit opt-in
   validator invocations; `--all-features` alone must not enable live tests.
9. [ ] Make live provider/GitHub contracts separately opt-in and read-only by default.
   Sandbox mutation tests require the owner's explicit target/authorization. Do not
   use real accounts to establish routine correctness.
10. [ ] Decommission old duplicate validator coverage only after the replacement has
    demonstrated the same assertions and useful failure behavior. Keep historical
    wrappers for transition if needed, then simplify commands in IR-19.
11. [ ] Record warm test and smoke timings in CI summaries. Investigate regressions by
    stage; do not make nextest/a new framework a prerequisite when the bottleneck is
    unconditional PTY waiting.

### Mandatory scenario catalog

| Group | Required journey |
|---|---|
| Context trust | Excluded diff sentinel never reaches provider; shared additions; budget enforced |
| LLM lifecycle | Lazy request count; partial failure; stalled cancellation; no mixed preview |
| Session isolation | A→B with out-of-order patch/context/chat/workspace results |
| Draft reliability | H1→H2 anchor drift; local patch while typing; late load/save |
| Mutation truth | Held acceptance/response; unknown outcome; restart; newer draft preserved |
| Navigation | Every tab key/click; focus and overlays; split line targets; 80×24 |
| Guided review | Effective order across every movement; human progress separate from AI |
| Persistence | Torn-write prevention; cache eviction; migration; orphan index recovery |
| Workspace | Reuse/base identity, source-clone isolation, collisions, remote final-diff shape |
| Performance correctness | Bounded progress, viewport-only work, follow-tail intent, fair scheduling |

### Manual acceptance recipe

Run the documented fast suite and named smoke suite. Deliberately inject one wrong
tab target, stale result and duplicate POST into test doubles to show the relevant
scenario fails for the intended reason. Run the smoke with a held provider/forge and
verify waits remain event-driven. Failure includes a suite that passes because no
tests matched a filter, a fake bypassing real routing, or broad PTY sleeps returning.

**Gates:** shared gates, coverage migration map, named smoke and CI timing comparison.
Keep warm Rust tests around the existing sub-5-second reference target when practical;
set a separate measured smoke budget after IR-15 rather than promising an invented one.

---

## IR-19: Replace milestone docs with a small living product contract

**Priority:** P3 documentation closure. **Findings:** stale/contradictory architecture,
requirements, plan and contributor guidance.\
**Depends on:** IR-01 through IR-18; earlier PRs still update changed contracts promptly.

### Delivered outcome

An implementing agent or new user can learn what the app actually does and where its
behavior lives from a small accurate set of documents. Completed milestone paperwork
stops overriding current behavior or forcing redundant test gates.

### Files and intended responsibilities

| File | Contents | Target size, not a hard lint |
|---|---|---|
| `README.md` | Product, install/run, five-minute review workflow, doc links | 80–120 lines |
| `docs/product.md` | Tabs, guided review, context/cost model, mutation states, data guarantees, limitations | 120–180 lines |
| `ARCHITECTURE.md` | Actual modules, identity/state/jobs, IO and storage boundaries, extension points | 120–180 lines |
| `docs/testing.md` | Fast gate, scenarios, smoke, contract opt-in, performance and snapshot workflow | 60–100 lines |
| `AGENTS.md` | Hard contributor rules and current reading/check order | 60–90 lines |
| `docs/decisions.md` | Small set of accepted decisions and genuinely open choices | As needed, concise |
| `docs/keymaps.md` | Generated current default action bindings | Generated |
| `docs/configuration.md`, `docs/themes.md` | Current accurate reference docs | As needed |

### Implementation steps

1. [ ] Build the new product and architecture docs from the corrected code, not by
   compressing old prose. Include a real module map and a representative action/job/
   save/mutation data flow.
2. [ ] Specify durable versus disposable storage, migration/recovery behavior, actual
   cancellation guarantees, unknown mutation outcomes and AI inference limitations.
3. [ ] Resolve proposed decisions with the owner. Distinguish adopted choices from
   unresolved retention/confirmation/portability options; do not mark them decided by
   copying implementation defaults.
4. [ ] Preserve historical requirement/decision IDs through an archive or compact legacy
   index so existing test comments, issues and PRs stay interpretable. Do not renumber.
5. [ ] Recommended migration: move old `REQUIREMENTS.md` and `PLAN.md` into an explicitly
   historical `docs/archive/` location with a banner, then remove active-root links.
   If the owner prefers Git history alone, retain a small ID index. Record the chosen
   approach and fix all links in the same change.
6. [ ] Update `AGENTS.md` reading order and testing commands. Remove the completed
   milestone-order mandate and the requirement to update the retired specification.
   Keep dependency approval, no-unsafe/panic rules, IO boundaries, privacy, confirmation,
   terminal restoration and no-commit/push-without-request rules.
7. [ ] Update README, CI comments, validator help, generated docs and source doc links
   that still refer to “arriving in M1/M2,” fake tabs or cache-as-chat-storage.
8. [ ] Preserve this improvement plan as an implementation record with actual PR links,
   evidence and unresolved follow-ups. Once complete, mark it historical too; do not
   create another permanent competing source of current functionality.
9. [ ] Check relative links, generated keymap parity and configuration examples against
   the binary. Run the exact README quick-start/manual workflow with a fixture.

### Required checks

- No active doc describes empty domain/application modules or nonfunctional tabs.
- No claim that cache deletion preserves chat while the code stores it only in cache.
- No claim of exact-once remote submission or instant cancellation beyond what was tested.
- New testing commands match CI and do not invoke removed/redundant gates.
- Legacy IDs remain discoverable; docs and contributor links resolve after migration.
- Configured values and effective request limits match examples.

### Manual acceptance recipe

Give the README and architecture document to a reviewer who has not read the diff.
They should run the fixture app, find a failed check and an outdated thread, follow a
guided review and identify where to implement a new tab/use case/adapter test. Follow
only the documented commands. Failure includes needing the archived milestone prose
to discover current behavior or a command that silently skips its intended tests.

**Gates:** current shared gates, docs/link/generated-output checks and the final workflow
recipe. Change the gate policy and its documentation atomically.

---

## 5. Shared validation protocol for every PR

### 5.1 Before changing code

1. Confirm dependencies are merged or intentionally stacked on the assigned base.
2. Record the current failing reproduction or establish whether a hypothesis is real.
3. Identify the smallest sufficient test seam: pure function, reducer, fake port,
   TestBackend, or explicit adapter/PTY contract.
4. Inspect affected config/state/cache schemas and migration needs.
5. State any owner decision or dependency approval needed before implementation.

### 5.2 Required automated checks

Until the contributor rules are migrated, run their required commands:

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

Run the relevant existing milestone validator(s) listed in the PR, using their
scenario-only/prebuilt form once IR-15 introduces it. Current commands are:

```sh
scripts/validate/m0.sh
scripts/validate/m1.sh
scripts/validate/m2a.sh
scripts/validate/m2b.sh
scripts/validate/m3.sh
scripts/validate/m4.sh
scripts/validate/m5.sh
```

Choose by affected behavior; a shared lifecycle/runtime change needs the affected
cross-feature contracts, not just a filtered unit test. Do not repeatedly run every
gate after it has passed unless a subsequent change or unresolved failure justifies it.

For intentional TUI snapshot changes:

```sh
UPDATE_SNAPSHOTS=1 cargo test --test shell_snapshots
git diff -- tests
```

Read every changed snapshot line. Do not regenerate snapshots to make an unexplained
failure disappear. If tests are moved/renamed in IR-18, update this recipe and the
contributor instructions together.

### 5.3 New test requirements

- Use meaningful invariant names and the relevant FR/NFR or IR finding reference.
- A test must fail for the pre-fix behavior it claims to catch.
- For races, explicitly hold/release completions; do not rely on scheduler luck.
- A filtered command with zero matched tests is not evidence. Confirm test discovery.
- Default unit/scenario tests use fakes and no network, `gh`, API key or real repository.
- Opt-in adapter contracts use synthetic data and private temporary roots. Never run
  mutation contracts against real GitHub without explicit owner authorization.
- Keep performance scaling assertions separate from environment-sensitive wall times.
- Do not introduce broad test infrastructure to prove a small pure invariant.

### 5.4 Manual test recipe in every PR description

The implementing agent must supply an exact, reproducible recipe. The per-PR manual
sections above specify what to prove; they are not a substitute for setup commands.
Do not leave placeholders or reference temporary audit files that other agents cannot
access. Commit reusable synthetic fixtures/helpers with the PR where necessary.

Required recipe structure:

1. Exact build and fixture-setup commands, including private `SMART_REVIEW_HOME`, fake
   forge/provider configuration and fixture repository when a Git contract needs one.
2. Exact launch command and terminal dimensions.
3. Numbered keystrokes/clicks, expected visible state after each, and what failure looks like.
4. Exact artifact checks: request counts/payload shape, stored subject/revision/version,
   durable document presence, source-clone state and log redaction as relevant.
5. The one or two things a human should judge: a click landing, text remaining visible,
   concise guidance being understandable, a stream arriving, or a publish preview being
   readable before confirmation.
6. Cleanup commands targeting only that fixture's owned paths/processes.

For isolated work, use `/tmp/opencode` when available. A setup may begin with:

```sh
AUDIT_ROOT="$(mktemp -d /tmp/opencode/smart-review-ir-XX.XXXXXX)"
export SMART_REVIEW_HOME="$AUDIT_ROOT/home"
mkdir -p "$SMART_REVIEW_HOME"
cargo build
```

Replace `XX` and add real fixture commands in the final PR description. Do not point
the app at the owner's ordinary home or production credentials for routine testing.

### 5.5 Completion checklist

- [ ] User-visible behavior and invariant are demonstrated, not merely implemented.
- [ ] Required tests genuinely ran and passed; any remaining failure is disclosed.
- [ ] Relevant contracts/snapshots/manual recipe reviewed.
- [ ] No unapproved dependency or broad lint suppression.
- [ ] No new IO in reducer/rendering and no wrong-direction dependency.
- [ ] Config/storage/cache versioning and migration consequences handled.
- [ ] Current requirements/docs updated for changed behavior and accepted decisions.
- [ ] This plan's status and evidence updated with the actual PR link.
- [ ] Working tree reviewed; no unrelated user work altered; no unsolicited commit/push.

---

## 6. Finding-to-PR coverage map

Every report finding has an owner below. An implementing agent discovering a new defect
must add an explicit row/issue rather than leaving a narrative “maybe later” note.

| Finding / improvement | Owning PR(s) | Proof required |
|---|---|---|
| F01: excluded content leaks through diff | IR-01 | Final fake-provider payload contains no protected sentinel |
| F02: ignored settings/context ceiling | IR-03 | Effective settings and serialized request honor user ceilings |
| F03: ambiguous basename maps to first file | IR-12 | Ambiguous alias rejected; exact paths resolve correctly |
| F04: budget overrun from wrappers/placeholders | IR-03 | Exact serialized byte-equivalent budget invariant |
| F05: analysis/chat added-context divergence | IR-12 | Same resolved context manifest in inspection and both request kinds |
| F06: fake Checks/Reviews tabs | IR-10 | Actual key/action/click reaches every destination |
| F07: order only changes tree | IR-11 | All navigation traverses one effective file sequence |
| F08: text focus, hidden cursor, split click | IR-09 | Focused input and displayed source anchor match actual action target |
| F09: overlay pass-through/discussion scroll | IR-09, IR-10 | Topmost surface owns input; selected discussion remains visible |
| F10: overwritten draft anchor | IR-04, IR-07 | Original revision preserved through load and submission |
| F11: cross-PR state/jobs/workspace readiness | IR-05 | Controlled A→B late-result schedules stay isolated |
| F12: false canceled-post certainty/duplicates | IR-07 | Held-response/restart scenarios remain truthful and do not repost |
| F13: patch reload erases composer | IR-04 | Writing and pending state survive unrelated data refresh |
| F14: reuse merge-base mismatch | IR-13 | Fresh/reused final diff and explicit merge base match |
| F15: eager streaming fallback | IR-02 | Second method counter remains zero after first accepted result |
| F16: stalled HTTP cancellation | IR-08 | Pending awaits interrupt and worker slots release |
| F17: atomic temp race/cache chat/index window | IR-06 | Fault/concurrent-writer/migration/index recovery cases |
| Worktree identity collision | IR-13 | Host/owner/repo collision fixtures stay separate |
| Writes into source clone's .git | IR-13 | Before/after source clone snapshot unchanged |
| Dry-run resolve paints real success | IR-07 | Simulated result never mutates displayed remote truth |
| Review/reply bodies in logs | IR-01 | Synthetic bodies absent from ordinary logs, including errors |
| Reduced diff joins disjoint line ranges | IR-12 | Original line coordinates survive context reduction |
| Process descendants survive cancel — hypothesis | IR-08 | Local owned process-tree fixture establishes/fixes behavior |
| Remote --patch is intermediate commit series — hypothesis | IR-13 | Final PR diff comparison proves actual semantics |
| UI filesystem/process IO | IR-14 | Slow fake IO does not stop input; dependency inventory is clean |
| Unbounded progress and discarded deltas | IR-08 | Bounded producer/consumer behavior and exact final content |
| 250 ms background cadence/redundant draws | IR-17 | Measured event-to-frame and idle render counts |
| Whole chat/analysis layout each frame | IR-17 | Actual draw benchmark; proper layout invalidation/follow-tail |
| Diff double construction/position recomputation | IR-11, IR-17 | Cached projections with correct invalidation and measured cost |
| One git-show process per context file | IR-17 | Bounded batch reads, process count and memory measurement |
| Missing meaningful runtime benchmark | IR-17 | Actual reducer+draw/input latency workload rather than row slicing |
| Duplicate CI gates and settling waits | IR-15 | Invocation counts and same-profile before/after wall times |
| Wait failures return success/full terminal replay | IR-15 | Driver negative tests and incremental replay equivalence |
| In-flight test actually runs after success | IR-07, IR-15 | Extra Enter injected while fake response remains held |
| Missing cross-feature regression coverage | All fixes; IR-18 consolidates | Adversarial scenario catalog and replacement coverage map |
| Brief/why/verify integrated beside code | IR-16 | Human-readable small/wide terminal walkthroughs |
| Context-aware suggested order, tests with behavior | IR-16 | Different PR archetypes and no forced tests/docs-last rule |
| Explicit human review progress | IR-16 | Durable markers distinguish human work and invalidate correctly |
| Stale REQUIREMENTS/PLAN/architecture | Every changed contract; IR-19 consolidates | Accurate living docs with preserved historical IDs |

---

## 7. Decisions and implementation questions

Planning a choice is not the same as approving a previously proposed DEC. Resolve
material product choices in the owning PR and update the decision log while it remains
active. The owner has requested the improvement work; new dependencies and relaxation
of hard guarantees still require explicit approval.

| Topic | Recommended implementation direction | Owner / checkpoint |
|---|---|---|
| Source repository isolation | App-owned object store; honor the existing no-.git-write rule | IR-13; ask only if proposing to relax that rule |
| Legacy user-linked worktree cleanup | Read/identify; provide manual owner cleanup instead of automatically mutating source .git | IR-13 |
| Single publish confirmation vs two Enters | Keep current confirmation until owner approves one clear explicit Publish action | IR-07 / IR-16 |
| Request budget semantics | Explicit user ceilings plus catalog capacity, with full framing/output accounting | IR-03; document and resolve affected DEC/FR contradictions |
| Context and display toggles | Canonical review context independent of visual whitespace hiding; explicit context controls affect manifest | IR-12 |
| Concurrent app instances | Preserve data with conflict detection/serialization; no silent overwrite | IR-06; dependency approval if needed |
| Chat retention/pruning | Durable storage first; explicit non-destructive default until DEC-9 is resolved | IR-06 |
| Guided UI and human markers | Five real tabs, compact what/why/verify, explicit reviewed state | IR-10 / IR-16 |
| Unknown remote outcomes | Persist/reconcile; never claim remote exactly-once from a local token | IR-07 |
| Unsupported provider settings | Refuse or explicitly report actual application; never silently downgrade a visible claim | IR-03 |
| Doc retirement | Historical archive plus legacy ID lookup, then one living contract | IR-19 |
| macOS CI/platform coverage | Make current smoke portable where practical; verify tier-1 claims before release; any CI expansion is explicit | IR-15 / final acceptance |

### Questions to answer during implementation, not assume away

- Which provider backends support both streaming methods in the pinned SDK, and which
  errors reliably mean “unsupported” without a dispatched paid request?
- What identifiers can each GitHub mutation return/query for reliable reconciliation?
  Which ambiguous outcomes cannot be safely auto-reconciled?
- Which current callers depend on remote commit-series patch semantics, if any?
- Does the existing process runner leave Git/SSH/editor helpers alive on timeout or
  signal? Test it with owned local processes.
- Which legacy chat/plan files exist in realistic user homes, and how are interrupted
  migrations discovered without scanning unbounded data on the UI thread?
- What cold/warm paths dominate first usable PR display after source-clone isolation?
- Which large-PR costs remain after IO leaves the UI? Measure before adding caches.

---

## 8. Release acceptance after the PR sequence

Use this as a final release checklist, not as an extra all-purpose implementation PR.
Any failure gets a small follow-up assigned to its owning IR area.

### Product journeys

- [ ] Fresh home with no model: browse, read, checks, discussion and local draft work.
- [ ] Model setup explains effective settings/context; one intended request produces
  one stream; malformed output is recoverable and usage is honest.
- [ ] Small bug fix, contract-first change and large mixed PR all have understandable
  what/why/verify guidance and an executable suggested order.
- [ ] Checks/Discussion accessible by mouse and keyboard at 80×24 and wide size.
- [ ] Outdated threads remain readable; valid links land on the intended code/run.
- [ ] Human review progress and manual order survive restart/cache deletion and react
  correctly to new commits.

### Correctness and persistence

- [ ] Cross-PR, refresh, cancellation and superseded-result schedules stay isolated.
- [ ] Writing survives local diff replacement, mode changes and slow/failed storage.
- [ ] Old draft anchors cannot be silently submitted against a new revision.
- [ ] Publish/reply success, rejection, unknown outcome, restart and dry-run are truthful.
- [ ] Known success cannot erase newer edits; uncertain success cannot silently duplicate.
- [ ] Excluded source text/keys/request bodies absent from the wrong outbound/log surfaces.
- [ ] Concurrent writes, migration and index crashes retain recoverable user documents.
- [ ] Source clone and .git unchanged by app-owned workspace lifecycle.

### Responsiveness and platform behavior

- [ ] Reproducible release-mode large-PR benchmark meets or explicitly explains each
  target in IR-17, including input-to-frame and memory rather than only parser time.
- [ ] Busy phases visible; stale cached content remains usable; stopped work really stops.
- [ ] Terminal restores after normal exit, error, panic, Ctrl-C and supported signals.
- [ ] External editor returns to the same composer with text intact; failure preserves
  the recovery artifact and restores terminal state.
- [ ] Linux and macOS tier-1 claims are backed by a documented smoke run or CI coverage.
  Do not claim broad terminal/platform validation from TestBackend alone.

### Test and documentation quality

- [ ] Default Rust suite stays fast and hermetic; no zero-matched-test false evidence.
- [ ] CI common gates run once; smoke uses observable waits and fails fast.
- [ ] Historical validators are removed only after their meaningful coverage is mapped.
- [ ] README walkthrough, keymaps, effective config, storage and architecture agree.
- [ ] All accepted decisions and remaining limitations are explicit and current.

---

## 9. Agent handoff template

Copy this into the implementation handoff/PR description and fill it with evidence.
End the actual PR description with the complete manual recipe, per repository rules.

```text
PR: IR-XX — title
Base commit / merged dependencies:
Status: in progress | ready for review | blocked

User-visible result:
Invariants fixed:
Findings and FR/NFR IDs:

Before-fix reproduction:
  Exact scenario/command and observed failure.

Implementation:
  Ownership changes, affected modules, key transitions.

Compatibility:
  Config/schema/cache/storage versions and migration/recovery behavior.

Automated verification:
  Exact commands, actual test counts, results and relevant timings.
  Confirm filtered tests really matched.

Snapshot review:
  Intentional differences and why they are correct.

Decisions / dependency approvals:
  Owner decision references; no assumed approval.

Remaining concerns:
  Concrete issue/IR owner; distinguish reproduced defect from hypothesis.

Plan update:
  PR link, completion evidence, changed dependencies or scoped follow-up.

Manual test recipe:
  Exact isolated setup and launch commands.
  Numbered inputs, expected screen and failure states.
  Disk/request/source-clone checks.
  Human judgment points and owned-fixture cleanup.
```

### Final instruction to implementing agents

Optimize for a reviewer being able to trust the app. A smaller change with an exact
reproduction, correct ownership and a convincing user journey is better than a large
rewrite with another green snapshot suite. Finish the assigned invariant and its tests,
document the behavior actually shipped, and leave the next agent a clear boundary.
