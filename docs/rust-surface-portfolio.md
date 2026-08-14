# Rust Surface Portfolio

This is the runtime portfolio for Narada MCP surfaces. It documents the
currently selected runtime defaults, retained rollback implementations, and
development-tooling runtimes. The authoritative selection source is
`../narada/packages/operator-surface-runtime-contract/contracts/runtime-implementation-matrix.json`;
this document explains that machine-readable contract rather than replacing it.

## Current runtime posture

The `native` profile selects admitted Rust implementations for the NARS
runtime, MCP runtime proxy, and every concrete MCP component represented in the
current Site fabric. The materialized `andrey-user` Site currently contains 30
surfaces; all 30 use the native Rust proxy and a Rust executable or native
applet as the actual child. Codex, OpenCode, and Kimi are materialized from the
same matrix-backed native profile.

This does **not** mean the repositories contain no JavaScript:

- `bun` and `node-compat` remain explicit rollback/compatibility profiles.
- TypeScript implementations remain for parity tests and rollback where the
  matrix admits them.
- Build, test, benchmark, and publication workflows still use Node, pnpm, and
  PowerShell where appropriate. Those tools are not MCP runtime children.
- The `mcp-javascript-fallback-runtime` policy row still selects Bun for the
  native profile. It is a generic fallback category, not the selected runtime
  of any of the 30 currently materialized concrete Site surfaces.

Consequently, “native by default” means that a normal native-profile carrier
or dynamic Site-fabric load launches no Bun or Node child for the selected
concrete MCP surface. It does not mean JavaScript has been deleted from the
source tree or from rollback and developer workflows.

## Fit test

A surface is a Rust candidate when all of these remain true:

1. Its contract can be preserved without carrying a JavaScript runtime or
   provider SDK.
2. Its core work is mechanical, bounded, and testable: process, filesystem,
   Git, protocol, or host-system control.
3. Rust can own the same failure, cancellation, and audit semantics.
4. A realistic workload can show a useful operational benefit: startup,
   memory, throughput, or lifecycle reliability.
5. The Rust version does not create a second authority implementation whose
   drift costs more than it saves.

The native-profile decision is separate from authority ownership. A Rust MCP
entrypoint may own the wire contract and bounded projections while calling an
explicit provider/domain authority or returning a structured authority
boundary. The Bun/Node implementation remains an explicit rollback until the
Rust path owns the required behavior.

## Current decisions

`Rust-native default` means the native runtime profile selects the Rust
entrypoint; Bun/Node remain explicit rollback profiles. A retained JavaScript
implementation is therefore evidence of compatibility coverage, not evidence
that JavaScript is the default. The matrix's `profile_runtime_engine_kinds`
field, not package entrypoints or source-language presence, decides the default.

| Surface | Decision | Current evidence and next proof |
|---|---|---|
| `local-filesystem` | Rust-native default | The native profile selects the Rust filesystem applet for reads and writes. Protocol, realistic load, mutation, patch, anchored-root, and loader tests cover the selected implementation; Node and Bun remain explicit compatibility profiles. |
| `structured-command` | Rust-native default | Rust owns the bounded policy, synchronous argv execution, timeout/cancellation, input refs, paging, output refs, and parse-check contract. Durable background execution and confirmed Windows UAC elevation remain explicit authority boundaries; the native profile selects the Rust applet and JavaScript remains an opt-in rollback. |
| `git` | Rust-native default | Rust owns the bounded Git inspection/policy contract and the native profile selects the Rust applet. Scoped mutation, conflict recovery, and publication remain explicit authority boundaries; JavaScript remains an opt-in rollback. |
| mcp-loader | Rust-native default | The full Rust loader preserves the 20-tool MCP contract, Site-fabric resolution, child supervision, lifecycle/restart, timeout, freshness, inventory, observation, and bounded-result behavior. A 20-sample cold / 100-call warm benchmark makes Rust/Node decisively faster than Node/Node and Bun/Bun; the native runtime profile selects Rust, with TypeScript retained as the explicit fallback. |
| `mcp-registrar` | Rust-native default | The complete Rust registrar owns catalog reads, validation, materialization planning, mutations, refusals, diagnostics, and native materializer recovery. Tool-catalog and read-model/mutation parity tests preserve the TypeScript contract; TypeScript remains a rollback implementation. |
| `runtime-introspection` | Rust-only authority | Rust owns all 14 bounded trace-analysis and server-bound observer tools, including freshness, latest-owner measurements, observer overhead, incident evidence, and direct one-call ranking/views. Every runtime profile resolves this surface to Rust; Node and Bun are unavailable. |
| `launcher` | Rust-native default | The native shared surface owns the bounded launcher contract and exposes host-console authority boundaries explicitly. The native profile selects Rust and Node remains an opt-in rollback. |
| `scheduler` | Rust-native default | The Rust shared surface owns task and activation authority, contract parity, dry-run mutation behavior, and activation transitions. The native profile selects Rust; Node and Bun remain compatibility profiles. |
| `agent-context` | Rust-native default | The complete Rust surface preserves tool schemas, orientation/checkpoint state, continuation, persistence, authorization, diagnostics, JSONL/framed protocol behavior, and occupant/admin projections. Tool-catalog and state parity tests preserve the TypeScript contract. |
| `artifacts` | Rust-only authority | Rust owns all seven tools, bound-session enforcement, Site-confined source admission, bounded paging, NARS HTTP authority, and durable registration/presentation retry semantics. Every runtime profile resolves this surface to Rust; Node and Bun are unavailable. |
| `browser-control` | Rust-native default | The native shared surface owns the MCP contract and exposes loopback CDP/UX authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `calendar` | Rust-native default | The native shared surface owns the calendar contract and uses the native Graph authority adapter for guarded reads/writes; the native profile selects Rust and Node remains an opt-in rollback. |
| `catalog-observation` | Rust-native default | The native shared surface owns the bounded catalog/fabric observation contract; descriptor authority remains explicit at the boundary. |
| `cloudflare-carrier` | Rust-native default | The native shared surface owns the carrier contract and exposes Cloudflare/provider authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `delegated-task` | Node operational fallback | Rust currently implements only bounded reads and explicitly lacks worker mutation/execution authority. The native profile selects Node until end-to-end worker authority parity is proven. |
| `graph-mail` | Rust-native default | The native shared surface owns the mail contract and uses the native Graph authority adapter for guarded operations; the native profile selects Rust and Node remains an opt-in rollback. |
| `mailbox` | Rust-native default | The native shared surface owns bounded projection reads, durable outbox consumers, first-observation reconciliation, admission, and Graph-backed synchronization. Same-runtime and Node/Rust replay parity cover the shared SQLite, fact, projection, cursor, and receipt contracts; the native profile selects Rust and Node remains an opt-in rollback. |
| `nars-session` | Rust-native default | The native shared surface owns the NARS session adapter and uses the native session/health authority bridge; the native profile selects Rust and Node remains an opt-in rollback. |
| `operator-console-overlay` | Rust-native default | The native shared surface owns the overlay contract and exposes host-console lifecycle authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `operator-routing` | Rust-native default | The native shared surface owns the bounded routing contract and exposes operator-domain decisions explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `quota-meter` | Rust-only authority | Rust owns bounded Codex app-server and Kimi HTTPS reads, glide calculation, credential non-disclosure, and the overlay lifecycle. The WPF host refreshes through the native executable; every runtime profile resolves to Rust and Node/Bun are unavailable. |
| `site-coherence` | Rust-only authority | Rust owns local continuity readback and the bounded Cloudflare `site.read` comparison, including server-bound cookie handling, sanitized remote failures, mismatch/attention classification, and invalid-local diagnostics. Every runtime profile resolves this surface to Rust; Node and Bun are unavailable. |
| `site-inbox` | Rust-native default | The native shared surface owns the bounded intake/triage contract and exposes site-domain authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `site-lifecycle` | Rust-native default | The native shared surface owns the lifecycle contract and exposes gated site-domain mutations explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `site-loop` | Rust-native default | The native shared surface owns the bounded loop/config contract and exposes orchestration authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `site-registry` | Rust-native default | The native shared surface owns the bounded registry/reconciliation contract and exposes shared-SQLite authority explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `sop` | Rust-native default | The native shared surface owns template registry mutations, the run engine, manual handoffs, governed actions, child SOPs, cancellation, retry, and terminal outbox durability. Independent Node/Rust parity covers replay and conflict paths plus the final SQLite snapshot; Node remains an explicit rollback. |
| `speech` | Rust-native default | The native shared surface owns the speech contract and exposes host TTS/capture/transcription authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `surface-feedback` | Rust-native default | The native shared surface owns the bounded feedback contract and exposes routing/cross-site authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `task-lifecycle` | Rust-native default | The shared Rust authority and native adapter implement the 69-tool contract, SQLite preparation/migration, payload revisions, evidence/review dependency gates, output resources, and Markdown compatibility. Catalog parity, smoke, refusal, migration, cross-runtime parity, and benchmark evidence admit and select Rust; TypeScript remains an explicit rollback. |
| `work-lifecycle` | Rust-native default | The same Rust authority and native adapter implement the 80-tool ticket/outbox contract, dynamic task revision triggers, SQLite transactions, and task/work cross-surface parity. Catalog parity, smoke, refusal, migration, cross-runtime parity, and benchmark evidence admit and select Rust; TypeScript remains an explicit rollback. |
| `worker-delegation` | Node operational fallback | Rust currently implements bounded worker projections but not the complete launch/execution authority. The native profile selects Node until end-to-end execution parity is proven. |

The Rust proxy itself is shared infrastructure rather than a catalog surface;
it is already Rust-native and is benchmarked independently from child-surface
implementations.

The runtime matrix selects Rust only where the implementation has operational
parity for the authority advertised by the surface. A boundary/refusal test is
not execution-parity evidence. Components with read-only Rust slices use an
admitted Node fallback until their mutation and execution authority is proven. Only the abstract generic JavaScript-surface row
remains outside the Rust-default set.

## Default and rollback controls

The native profile selects the Rust proxy and Rust surface implementation when
the immutable native artifact is available; materialization refuses a missing
or stale artifact. The `bun` and `node-compat` profiles are explicit
carrier-wide rollback/compatibility choices. Surface-specific JavaScript
projections may remain in the catalog for parity and rollback, but they are not
selected by normal native-profile materialization. Provider or domain
authority boundaries remain explicit in Rust adapters.

## Evidence ledger

| Area | Existing evidence | Missing evidence |
|---|---|---|
| Runtime proxy | Native protocol tests; minimal and strong runtime benchmarks; native startup/memory measurements; registrar unit test confirms native proxy default when available | Per-surface lifecycle workload attribution beyond the candidate matrix |
| Local filesystem | Native read/write/patch protocol tests; anchored-root loader test; realistic filesystem workloads across JavaScript, Rust, and Rust+Rhai lanes; statistical Rust-versus-Node and Rust-versus-Rhai comparisons | Keep patch recovery, cancellation, and Windows anchored-root coverage healthy |
| Structured command | JavaScript contract tests and realistic command workload; Rust policy/guidance/synchronous slice, direct protocol/timeout test, native-child integrated benchmark lane, and 60-sample order-reversed statistical comparison | Background durability and confirmed UAC remain explicit authority boundaries; JavaScript is the rollback path |
| Git | JavaScript contract tests and bounded Git policy; Rust read canary, direct protocol test, `real-git` workload, and 60-sample order-reversed statistical comparison cover policy, status, sync state, branches, dirty summary, diff, log, show, and refusal behavior | Mutation/recovery/publication remain explicit authority boundaries; JavaScript is the rollback path |
| mcp-loader | Native Rust contract/parity suite, exact tools/list comparison, focused lifecycle test, and bounded 20-sample loader benchmark cover the complete loader surface and child lifecycle | None for the admitted native profile; retain the TypeScript implementation as the explicit rollback path |
| Lifecycle surfaces | Native Rust authority/adapters; 69/80-tool catalog parity; smoke, refusal, migration, Node/Rust cross-runtime parity, review/dependency/resource checks, and 12-sample task/work benchmarks | None for the admitted native profile; retain TypeScript as the explicit rollback and keep compatibility tests running |
| SOP | Native Rust template registry, run engine, handoff/action completion, child-run, cancellation, retry, and terminal-outbox authority; independent Node/Rust replay, conflict, and SQLite snapshot parity; full shared protocol suite | None for the admitted native profile; retain Node/Bun as explicit rollback and compatibility lanes |
| Rust-default shared surfaces | Native protocol parity, matrix admission, boundary/refusal tests, and native artifact checks | Replace an explicit authority boundary with a Rust owner when the required domain/provider semantics are ported; retain Bun/Node rollback coverage |
| JavaScript compatibility | Node/Bun package tests, cross-runtime parity, and explicit rollback profiles | The `mcp-javascript-fallback-runtime` policy row remains Bun-backed; no currently materialized concrete Site surface depends on it |

## Work order

1. Keep the native-profile matrix, immutable artifact manifest, Site fabric,
   and all-carrier materialization in sync.
2. Keep TypeScript/Node/Bun parity and rollback tests healthy without allowing
   those compatibility implementations to silently become native defaults.
3. Preserve explicit provider/domain authority boundaries and promote new
   native behavior only after contract and persistence parity.
4. Keep focused workload and process-tree evidence for startup, memory,
   filesystem, lifecycle, loader, registrar, and NARS behavior.
5. Treat a Bun/Node child under a concrete native-profile MCP surface as drift
   requiring matrix/materialization diagnosis, not as an implicit fallback.

Each Rust promotion must pass contract equivalence before a registrar default
changes. Benchmarks are measurements, not predeclared latency thresholds; the
decision is based on correctness plus total operational simplicity.

## Verification and drift

Three different observations answer three different questions:

1. The runtime implementation matrix answers what each profile selects.
2. A carrier runtime-plan sidecar and Site `.ai/mcp/*-mcp.json` fabric answer
   what was materialized from that selection.
3. The live process tree answers what is actually running.

A complete native-default verification checks all three. For a concrete Site
surface, the materialized command must be the native proxy, the declared child
must have `child-invocation-kind` `native_entrypoint` or `native_applet`, and
the live proxy subtree must contain no Bun or Node process. Dynamic loading is
held to the same standard: the loader resolves the exact Site-fabric
declaration, starts the native child, and rejects caller substitutions.

Generated validation fixtures under `.ai/validation` are historical test
artifacts unless regenerated. They are not runtime authority and must not be
used to infer the current default over the matrix, current carrier sidecar, or
current Site fabric.
