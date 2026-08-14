# Native MCP surface ergonomics audit

This ledger is the durable completion record for the all-surface review begun on 2026-08-14.

## Completion contract

A surface is complete only when the ledger records evidence for all applicable items:

1. Every public tool exercised through MCP for normal and invalid inputs.
2. Empty, retry, paging, timeout, cancellation, persistence, and recovery behavior exercised where the tool semantics expose them.
3. Named, closed, accurate, bounded input schemas verified.
4. Native Rust authority and absence of runtime Node/Bun/TypeScript fallback verified.
5. Evidenced ergonomics gaps repaired in Rust.
6. Focused Rust tests and a real stdio integration proof pass.
7. The thematic commit is pushed.
8. Live post-materialization proof passes after the one final carrier restart.

Builds during review are permitted. Carrier materialization and restart are deferred until all pre-restart rows are complete.

## Surface ledger

`re-audit` means prior work exists but has not yet been re-proven against this completion contract.

| Surface/component | Pre-restart status | Findings/fixes | Test evidence | Commit | Post-restart proof |
|---|---|---|---|---|---|
| local-filesystem | re-audit | Prior ergonomics repair; completion audit pending | pending | prior commits | pending |
| agent-context | re-audit | Prior native authority repair; completion audit pending | pending | prior commits | pending |
| mcp-loader | re-audit | Prior loader ergonomics/recovery repair; completion audit pending | pending | prior commits | pending |
| mcp-registrar | re-audit | Prior native registrar/materializer repair; completion audit pending | pending | prior commits | pending |
| structured-command | pre-restart complete | Closed selector-aware schemas; native detached background runner; native policy-gated elevation; removed JavaScript fallbacks | Rust 20/20; real stdio start/poll, cancellation, timeout, paging, refs, terminal isolation | `04b81ef`, artifact `94523d8` | pending |
| catalog-observation | queued | pending | pending | pending | pending |
| git | re-audit | Prior ergonomics repair; completion audit pending | pending | prior commits | pending |
| calendar | re-audit | Prior native Graph authority repair; completion audit pending | pending | prior commits | pending |
| site-loop | queued | pending | pending | pending | pending |
| surface-feedback | re-audit | Prior named-schema and workflow repair; completion audit pending | pending | prior commits | pending |
| epistemic-graph | re-audit | Prior multi-pass ergonomics repair; completion audit pending | pending | prior commits | pending |
| sop | pre-restart complete | Named closed mutation schemas; one-call claim-and-advance; lease recovery metadata | focused Rust 12/12; full release 121/121 | `4ea4762` | pending |
| operator-routing | queued | pending | pending | pending | pending |
| site-inbox | queued | pending | pending | pending | pending |
| task-lifecycle | re-audit | Prior schema preparation, recurrence, and ergonomics repair; completion audit pending | pending | prior commits | pending |
| site-lifecycle | queued | pending | pending | pending | pending |
| site-registry | queued | pending | pending | pending | pending |
| project-state | queued | pending | pending | pending | pending |
| work-lifecycle | queued | pending | pending | pending | pending |
| runtime-introspection | queued | pending | pending | pending | pending |
| site-coherence | queued | pending | pending | pending | pending |
| launcher | queued | pending | pending | pending | pending |
| delegated-task | re-audit | Prior native command implementation and ergonomics feedback; completion audit pending | pending | prior commits | pending |
| worker-delegation | re-audit | Prior native implementation, cognition/model mapping, and ergonomics repair; completion audit pending | pending | prior commits | pending |
| artifacts | queued | pending | pending | pending | pending |
| nars-session | re-audit | Prior native runtime work; completion audit pending | pending | prior commits | pending |
| quota-meter | queued | pending | pending | pending | pending |
| mailbox | queued | pending | pending | pending | pending |
| graph-mail | queued | pending | pending | pending | pending |
| scheduler | queued | pending | pending | pending | pending |
| speech | queued | pending | pending | pending | pending |
| browser-control | queued | pending | pending | pending | pending |
| operator-console-overlay | queued | pending | pending | pending | pending |
| cloudflare-carrier | queued | pending | pending | pending | pending |

## Runtime infrastructure audit

These are not user-facing domain surfaces, but must not invalidate the native claim.

| Component | Status | Required evidence |
|---|---|---|
| MCP runtime proxy | re-audit | Native proxy, transport, lifecycle, cancellation, and no JavaScript subprocess |
| JavaScript fallback runtime | contradiction to resolve | The runtime matrix still marks the native profile as Bun for `mcp-javascript-fallback-runtime`; determine whether this is an obsolete declaration or a real remaining dependency |

## Final gate

- All rows pre-restart complete.
- Full native and workspace suites pass.
- One native artifact graph is published and materialized.
- One carrier restart occurs.
- Every row receives live MCP proof.
- Owned repositories are clean and pushed; temporary worktrees and test artifacts are removed.
