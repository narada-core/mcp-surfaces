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
| catalog-observation | pre-restart complete | Contract-only posture is explicit; unavailable result preserves requested access mode; precise invalid-field diagnostics; standalone verifier no longer requires workspace JS package | Rust focused 3/3; real stdio legacy/2026 discovery, guidance, unavailable, invalid instant/mode, credential non-disclosure | `4a84399` | pending |
| git | re-audit | Prior ergonomics repair; completion audit pending | pending | prior commits | pending |
| calendar | re-audit | Prior native Graph authority repair; completion audit pending | pending | prior commits | pending |
| site-loop | in progress | Native durable attention/control authority and allowlisted test execution implemented; test output is bounded and terminal UI inheritance removed. Proof, recovery drill, and loop execution still require a faithful native coordinator over their existing resident/scheduler/task authorities. | focused Rust 4/4; real native stdio config/docs/test-list/status/configured-test execution | `61d48e0`, `7b2c1fe` | pending |
| surface-feedback | re-audit | Prior named-schema and workflow repair; completion audit pending | pending | prior commits | pending |
| epistemic-graph | re-audit | Prior multi-pass ergonomics repair; completion audit pending | pending | prior commits | pending |
| sop | pre-restart complete | Named closed mutation schemas; one-call claim-and-advance; lease recovery metadata | focused Rust 12/12; full release 121/121 | `4ea4762` | pending |
| operator-routing | pre-restart complete | Restored typed role-admission/runtime-binding handoffs; closed and bounded every argument; exposed authority contract in doctor; stable request ids now provide durable idempotent replay and conflict refusal | focused Rust 1/1 covering persistence, handoffs, schemas, retry/conflict; real native stdio covers doctor, both handoffs, fallback-disabled, invalid input, replay/conflict, durable readback | `dd3afdd` | pending |
| site-inbox | pre-restart complete | Corrected false `node_sqlite` posture; bounded schemas and serialized envelopes; added durable submission idempotency/conflict refusal and disposition replay without duplicate events | focused Rust 2/2; real native stdio exercises all 12 tools, empty state, retry/conflict, all dispositions, CAPA, audit, paging, and cross-process persistence | `f075168` | pending |
| task-lifecycle | in progress | Prior schema preparation, recurrence, and ergonomics repair. Re-audit found all 69 task and all 80 work tool schemas lacked names, with 551 unbounded strings and 45 unbounded arrays across the shared native catalog. Native publication now deterministically names schemas and bounds strings/arrays, while dispatch enforces type, enum, required, closed top-level argument, length, collection-size, and composition constraints before authority code runs. Per-tool normal/retry/paging/recovery stdio coverage remains to be completed. | full Rust baseline 13/13 after adding both-surface schema traversal and pre-dispatch invalid-input enforcement tests | current thematic commit | pending |
| site-lifecycle | complete before restart | Added the four operator-surface authorities over Narada-compatible site-local identity and runtime-binding artifacts; explicit execution/authority gates, target-evidence postconditions, replay, and conflict refusal. Replaced read placeholders with canonical registry list/show, complete lifecycle kinds and preflight, bounded relation list/validation, mutation-locus preflight, transactional discovery, and registry-resolving doctor behavior. Restored all five creation presets and bounded greenfield planning. `site_init` now plans and applies natively with conflict preflight, exact retry, canonical registry repair, and no hidden writes in plan mode. Retired the JavaScript `node_modules` synchronizer and exposed an honest native dependency posture instead. Every public input schema is named, closed, and top-level strings are bounded. | focused Rust tests plus real native stdio cover all public schemas, operator authority, presets/plans/refusals, registry reads, discovery apply/retry/audit, init plan/apply/retry/conflict/recovery, doctor registry resolution/missing site, lifecycle kinds/preflight, relation validation, mutation-locus classification, confinement, dependency posture/retirement, replay/conflict, readback, and artifacts | `64155c7`, `0170e73`, `456bd88`, `1b0d962`, `f94e69b`, `04bdf7f`, current thematic commit | final live proof pending |
| site-registry | pre-restart complete | Replaced empty native placeholders with the canonical user-locus SQLite read authority; added bounded paging, alias/audit resolution, honest missing/unprepared diagnostics, and bounded filesystem plus launch-registry dry-run discovery with merged evidence and no mutation | focused Rust 1/1; real native stdio exercises all 6 tools, schemas, doctor, paging, alias/audit show, not-found and invalid inputs, merged discovery, and byte-identical read-only persistence | `1d39ca0` | pending |
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
| scheduler | in progress | Replaced the obsolete Bun-built parity oracle with native behavioral proof. The Rust scheduler and native no-window supervisor report fresh; the verifier now proves every public tool rejects invalid input and exercises the entire durable activation authority (prepare, binding create/read/page/pause/resume/retire, event admit/show, activation page/claim/admit/resolve/fail/unblock). Windows task reads and native action dry-run are covered; a reversible host-task lifecycle drill remains before completion. | full shared-native Rust 128/128 (one deployment-only ignored); real native stdio covers all schemas/invalid inputs plus the complete activation lifecycle and bounded task reads/action planning | current thematic commit | pending |
| speech | queued | pending | pending | pending | pending |
| browser-control | queued | pending | pending | pending | pending |
| operator-console-overlay | queued | pending | pending | pending | pending |
| cloudflare-carrier | queued | pending | pending | pending | pending |

## Runtime infrastructure audit

These are not user-facing domain surfaces, but must not invalidate the native claim.

| Component | Status | Required evidence |
|---|---|---|
| MCP runtime proxy | re-audit | Native proxy, transport, lifecycle, cancellation, and no JavaScript subprocess |
| Native input-contract boundary | pre-restart complete | The shared Rust server now names every published tool schema, closes every top-level argument object, deterministically bounds previously unbounded strings and arrays, and rejects type/required/unknown-field/length/collection-size violations before authority dispatch. All 25 currently admitted shared-native surface catalogs are traversed by regression proof; individual surface audits still own semantic accuracy and domain diagnostics. |
| JavaScript fallback runtime | pending audit | Determine whether the native-profile Bun fallback is still reachable by any admitted surface after all surface reviews; remove it only with reachability evidence |

## Final gate

- All rows pre-restart complete.
- Full native and workspace suites pass.
- One native artifact graph is published and materialized.
- One carrier restart occurs.
- Every row receives live MCP proof.
- Owned repositories are clean and pushed; temporary worktrees and test artifacts are removed.
