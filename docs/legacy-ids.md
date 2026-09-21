# Legacy requirement ID index

The retired milestone specification and implementation plan remain in Git history. This
compact index preserves their identifiers so source comments, tests, issues, and PRs are
still interpretable. IDs are never reused or renumbered. Current behavior belongs in
[`product.md`](product.md), [`../ARCHITECTURE.md`](../ARCHITECTURE.md),
[`testing.md`](testing.md), and [`decisions.md`](decisions.md).

## Functional requirements

| ID | Historical subject | Current contract |
|---|---|---|
| FR-1.1 | Environment detection | Product: launch/offline; architecture: composition |
| FR-1.2 | Launch options | README: install and run |
| FR-1.3 | Repository identity/cache partitioning | Architecture: identity |
| FR-2.1 | PR list | Product: review journey |
| FR-2.2 | Search and filter | Product: review journey |
| FR-2.3 | Refresh and cache | Product: offline/storage |
| FR-2.4 | PR detail fetch | Product: review journey |
| FR-3.1 | PR workspace | Architecture: IO/storage |
| FR-3.2 | Diff acquisition | Product: review journey |
| FR-3.3 | Diff rendering | Product: review journey; DEC-4 |
| FR-3.4 | Navigation | README/keymaps |
| FR-3.5 | Layer/review order | Product: guided review; DEC-10/23 |
| FR-4.1 | Analysis output | Product: guided review/analysis |
| FR-4.2 | Review plan and ordering | Product: guided review |
| FR-4.3 | Analysis caching/invalidation | Product: context; DEC-15 |
| FR-4.4 | Streaming/progress/cancellation | Product and architecture: jobs |
| FR-4.5 | Provider/model/key configuration | Product: analysis; DEC-5/6 |
| FR-4.6 | Context/privacy | Product: context and cost |
| FR-4.7 | Model catalog | Configuration/product |
| FR-4.8 | Thinking mode and usage | Product: cost; DEC-18 |
| FR-5.1 | Chat sessions | Product: chat; DEC-9 |
| FR-5.2 | Chat interaction | Product: chat |
| FR-5.3 | Chat grounding | Product: chat; DEC-2 |
| FR-5.4 | Chat cost/limits | Product: context and cost |
| FR-6.1 | Review draft | Product: drafts/mutations |
| FR-6.2 | Inline comments | Product: drafts/mutations |
| FR-6.3 | Publishing | Product: drafts/mutations; DEC-3/23 |
| FR-6.4 | Existing discussion | Product: tabs/mutations; DEC-16 |
| FR-6.5 | Mutation safety/dry run | Product: drafts/mutations |
| FR-7.1 | UI modes | Keymaps/product |
| FR-7.2 | Keybinding engine | Keymaps/architecture |
| FR-7.3 | Help/discoverability | README/keymaps; DEC-20/21 |
| FR-7.4 | Command line | README/keymaps |
| FR-7.5 | Mouse | Product/testing |
| FR-7.6 | Status/notifications/progress | Product |
| FR-7.7 | Theming | Themes/configuration |
| FR-7.8 | Layout/responsiveness | Product/testing |
| FR-8.1 | App-owned directory layout | Product: storage |
| FR-8.2 | Configuration file | Configuration |
| FR-8.3 | Keybinding overrides | Keymaps/configuration |
| FR-8.4 | Themes | Themes/configuration |
| FR-8.5 | State/cache durability | Product/architecture: storage |
| FR-8.6 | Config robustness | Configuration; DEC-19 |
| FR-9.1 | Actionable errors/terminal lifecycle | AGENTS/architecture |
| FR-9.2 | Content-redacted logging | Product/AGENTS |
| FR-9.3 | Doctor/check report | README |

## Non-functional requirements

| ID | Historical subject | Current contract |
|---|---|---|
| NFR-1.1 | Startup latency | Performance record |
| NFR-1.2 | Event-loop responsiveness/no IO | Architecture: jobs |
| NFR-1.3 | Large-input bounds | Performance record |
| NFR-1.4 | Cancellation latency | Product/architecture |
| NFR-2.1 | Linux/macOS portability | Product/DEC-22 |
| NFR-2.2 | Git/gh environment dependencies | README |
| NFR-2.3 | Windows best effort | Product/DEC-12 |
| NFR-3.1 | Secret handling | Product/AGENTS |
| NFR-3.2 | Outbound privacy/no telemetry | Product/AGENTS |
| NFR-3.3 | argv-only command safety | Architecture/AGENTS |
| NFR-3.4 | Confirmed publishing/draft survival | Product |
| NFR-4.1 | Recoverable failures/atomic user data | Product/architecture |
| NFR-4.2 | Terminal restoration | Architecture/AGENTS |
| NFR-5.1 | Maintainability/lints | AGENTS |
| NFR-5.2 | Hermetic deterministic scenarios/smoke | Testing |
| NFR-5.3 | Content-free job observability | Architecture/AGENTS |

## Architecture and development constraints

| IDs | Historical subject | Current contract |
|---|---|---|
| ARCH-1 | Clean dependency direction | Architecture |
| ARCH-2 | Port boundaries | Architecture |
| ARCH-3 | Adapter/process boundaries | Architecture |
| ARCH-4 | Domain model ownership | Architecture |
| ARCH-5 | Background jobs and single-owner state | Architecture |
| ARCH-6 | Module layout | Architecture |
| ARCH-7 | Typed errors | AGENTS |
| ARCH-8 | Dependency approval | AGENTS |
| DEV-1 | Rust edition/MSRV | `Cargo.toml`, README |
| DEV-2, DEP-1, DEP-2 | Dependency process | AGENTS |
| DEV-3 | Milestone ordering | Retired with the completed plan |
| DEV-4 | Lowest-sufficient tests | AGENTS/testing |
| DEV-5 | Specification/decision maintenance | Replaced by living-doc/decision rules |
| DEV-6 | Git and remote-mutation authorization | AGENTS |
| DEV-7 | Actionable English messages | AGENTS |
| DEV-8 | Workspace lint policy | `Cargo.toml`, AGENTS |

## Decisions

DEC-1 through DEC-23 retain their original meanings. Accepted outcomes and the only
remaining open choice, DEC-15, are in [`decisions.md`](decisions.md). DEC-8 is retired
because it only ordered completed milestones.

## Historical milestones and improvement IDs

M0–M5 identify the completed original delivery sequence; M2a and M2b were its workspace/
model and analysis subdivisions. IR-01–IR-19 identify the completed reliability/product
sequence recorded in [`improvement-plan.md`](improvement-plan.md). They are historical
references, not gates that order new work.
