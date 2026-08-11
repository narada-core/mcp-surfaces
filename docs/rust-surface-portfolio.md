# Rust Surface Portfolio

This is the runtime portfolio for Narada MCP surfaces. It is an inventory and
decision ledger, not a claim that the portfolio is complete.

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
entrypoint; Bun/Node remain explicit rollback profiles. `Rust-native target`
means Rust is the intended primary implementation after contract parity.
`Intentionally dual` means a Rust implementation may be useful, but both
runtimes remain legitimate until workload evidence selects a default.
`JavaScript-native` means a Rust rewrite is not currently a coherent use of
effort; the existing implementation remains the authority.

| Surface | Decision | Current evidence and next proof |
|---|---|---|
| `local-filesystem` | Intentionally dual | Rust read applet plus the bounded low-level mutation set (`fs_write_file`, exact string/range edits, move, create/rename directory, and delete directory) are protocol-tested and pass the realistic write workload. The 100-sample order-reversed Rust/Rhai comparison shows no reliable command-latency advantage for Rhai and higher memory/refusal cost; direct Rust remains the simpler native implementation. JavaScript remains authoritative for `fs_apply_patch`, whose parser, durable recovery, and patch-outcome semantics are still a separate authority boundary. |
| `structured-command` | Rust-native default | Rust owns the bounded policy, synchronous argv execution, timeout/cancellation, input refs, paging, output refs, and parse-check contract. Durable background execution and confirmed Windows UAC elevation remain explicit authority boundaries; the native profile selects the Rust applet and JavaScript remains an opt-in rollback. |
| `git` | Rust-native default | Rust owns the bounded Git inspection/policy contract and the native profile selects the Rust applet. Scoped mutation, conflict recovery, and publication remain explicit authority boundaries; JavaScript remains an opt-in rollback. |
| mcp-loader | Rust-native default | The full Rust loader preserves the 20-tool MCP contract, Site-fabric resolution, child supervision, lifecycle/restart, timeout, freshness, inventory, observation, and bounded-result behavior. A 20-sample cold / 100-call warm benchmark makes Rust/Node decisively faster than Node/Node and Bun/Bun; the native runtime profile selects Rust, with TypeScript retained as the explicit fallback. |
| `mcp-registrar` | JavaScript-native | The registrar composes every package descriptor and carrier schema, then projects carrier-specific configuration; moving that compiler to Rust would create a second authority. |
| `runtime-introspection` | Rust-native default | The native shared surface owns the bounded inspection contract; process/runtime authority remains explicit at the host boundary. The native profile selects Rust and Node remains an opt-in rollback. |
| `launcher` | Rust-native default | The native shared surface owns the bounded launcher contract and exposes host-console authority boundaries explicitly. The native profile selects Rust and Node remains an opt-in rollback. |
| `scheduler` | Rust-native default | The native shared surface owns the scheduler contract and makes task activation/outbox/Task Scheduler authority boundaries explicit. The native profile selects Rust and Node remains an opt-in rollback. |
| `agent-context` | JavaScript-native | Session, checkpoint, continuation, and hydration semantics are Narada domain behavior backed by shared SQLite and filesystem contracts. |
| `artifacts` | Rust-native default | The native shared surface owns the bounded artifact contract and uses explicit NARS authority adapters where required; the native profile selects Rust and Node remains an opt-in rollback. |
| `browser-control` | Rust-native default | The native shared surface owns the MCP contract and exposes loopback CDP/UX authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `calendar` | Rust-native default | The native shared surface owns the calendar contract and uses the native Graph authority adapter for guarded reads/writes; the native profile selects Rust and Node remains an opt-in rollback. |
| `catalog-observation` | Rust-native default | The native shared surface owns the bounded catalog/fabric observation contract; descriptor authority remains explicit at the boundary. |
| `cloudflare-carrier` | Rust-native default | The native shared surface owns the carrier contract and exposes Cloudflare/provider authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `delegated-task` | Rust-native default | The native shared surface owns the bounded delegated-task contract and exposes durable domain authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `graph-mail` | Rust-native default | The native shared surface owns the mail contract and uses the native Graph authority adapter for guarded operations; the native profile selects Rust and Node remains an opt-in rollback. |
| `mailbox` | Rust-native default | The native shared surface owns bounded projection reads, durable outbox consumers, first-observation reconciliation, admission, and Graph-backed synchronization. Same-runtime and Node/Rust replay parity cover the shared SQLite, fact, projection, cursor, and receipt contracts; the native profile selects Rust and Node remains an opt-in rollback. |
| `nars-session` | Rust-native default | The native shared surface owns the NARS session adapter and uses the native session/health authority bridge; the native profile selects Rust and Node remains an opt-in rollback. |
| `operator-console-overlay` | Rust-native default | The native shared surface owns the overlay contract and exposes host-console lifecycle authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `operator-routing` | Rust-native default | The native shared surface owns the bounded routing contract and exposes operator-domain decisions explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `quota-meter` | Rust-native default | The native shared surface owns the quota contract and exposes provider/desktop authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `site-coherence` | Rust-native default | The native shared surface owns the bounded continuity projection contract and exposes local/Cloudflare authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `site-inbox` | Rust-native default | The native shared surface owns the bounded intake/triage contract and exposes site-domain authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `site-lifecycle` | Rust-native default | The native shared surface owns the lifecycle contract and exposes gated site-domain mutations explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `site-loop` | Rust-native default | The native shared surface owns the bounded loop/config contract and exposes orchestration authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `site-registry` | Rust-native default | The native shared surface owns the bounded registry/reconciliation contract and exposes shared-SQLite authority explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `sop` | Rust-native default | The native shared surface owns the bounded SOP/template/run contract and exposes action/execution authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `speech` | Rust-native default | The native shared surface owns the speech contract and exposes host TTS/capture/transcription authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `surface-feedback` | Rust-native default | The native shared surface owns the bounded feedback contract and exposes routing/cross-site authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |
| `task-lifecycle` | Rust-native target | The shared Rust authority and native adapter implement the 69-tool contract, SQLite preparation/migration, payload revisions, evidence/review dependency gates, output resources, and Markdown compatibility. Catalog parity, smoke, refusal, migration, Node/Rust cross-runtime parity, and 12-sample native benchmark evidence admit Rust for the native profile; the TypeScript implementation remains an explicit rollback. |
| `work-lifecycle` | Rust-native target | The same Rust authority and native adapter implement the 80-tool ticket/outbox contract, dynamic task revision triggers, SQLite transactions, and task/work cross-surface parity. Catalog parity, smoke, refusal, migration, Node/Rust cross-runtime parity, and 12-sample native benchmark evidence admit Rust for the native profile; the TypeScript implementation remains an explicit rollback. |
| `worker-delegation` | Rust-native default | The native shared surface owns the bounded worker-delegation contract and exposes runtime admission/affinity/handoff authority boundaries explicitly; the native profile selects Rust and Node remains an opt-in rollback. |

The Rust proxy itself is shared infrastructure rather than a catalog surface;
it is already Rust-native and is benchmarked independently from child-surface
implementations.

The runtime matrix records the requested shared surfaces as `rust: admitted`
and selects Rust in the native profile. “Admitted” means the native entrypoint
is the default process and its contract/boundary tests pass; it does not hide
an authority boundary behind a fake success. Bun/Node remain available as
explicit rollback profiles, while agent-context, mcp-registrar, and the
generic JavaScript surface remain outside this Rust-default set.

## Default and rollback controls

The native proxy default retains `--runtime-proxy-implementation bun` as its
carrier-wide rollback. The native profile selects Rust for the requested MCP
surface set (plus the already-admitted loader, task lifecycle, and work
lifecycle) when the native artifact is available; materialization refuses a
missing artifact. `surface_implementation=js` is the explicit lifecycle
rollback, while the `bun` and `node-compat` profiles retain their JavaScript
engines. Structured-command and Git remain benchmark canaries, and provider or
domain authority boundaries remain explicit in the Rust adapters.

## Evidence ledger

| Area | Existing evidence | Missing evidence |
|---|---|---|
| Runtime proxy | Native protocol tests; minimal and strong runtime benchmarks; native startup/memory measurements; registrar unit test confirms native proxy default when available | Per-surface lifecycle workload attribution beyond the candidate matrix |
| Local filesystem | Native read tests; native low-level mutation protocol test (write, string/range edits, move, directory lifecycle, delete refusal/recursion); direct write test; `filesystem-write-load` strong workload across JavaScript, Rust, and Rust+Rhai lanes; 60-sample order-reversed Rust-versus-Node read comparison; 100-sample order-reversed Rust/Rhai write comparison | `fs_apply_patch` remains JavaScript authority for parser/durable recovery/patch outcomes; broaden failure/cancellation evidence only if that boundary is reconsidered |
| Structured command | JavaScript contract tests and realistic command workload; Rust policy/guidance/synchronous slice, direct protocol/timeout test, native-child integrated benchmark lane, and 60-sample order-reversed statistical comparison | Background durability and confirmed UAC remain explicit authority boundaries; JavaScript is the rollback path |
| Git | JavaScript contract tests and bounded Git policy; Rust read canary, direct protocol test, `real-git` workload, and 60-sample order-reversed statistical comparison cover policy, status, sync state, branches, dirty summary, diff, log, show, and refusal behavior | Mutation/recovery/publication remain explicit authority boundaries; JavaScript is the rollback path |
| mcp-loader | Native Rust contract/parity suite, exact tools/list comparison, focused lifecycle test, and bounded 20-sample loader benchmark cover the complete loader surface and child lifecycle | None for the admitted native profile; retain the TypeScript implementation as the explicit rollback path |
| Lifecycle surfaces | Native Rust authority/adapters; 69/80-tool catalog parity; smoke, refusal, migration, Node/Rust cross-runtime parity, review/dependency/resource checks, and 12-sample task/work benchmarks | None for the admitted native profile; retain TypeScript as the explicit rollback and keep compatibility tests running |
| Rust-default shared surfaces | Native protocol parity, matrix admission, boundary/refusal tests, and native artifact checks | Replace an explicit authority boundary with a Rust owner when the required domain/provider semantics are ported; retain Bun/Node rollback coverage |
| JavaScript-native surfaces | Package contract tests and domain-specific e2e tests | No Rust default is selected for agent-context, mcp-registrar, or generic JavaScript surface |

## Work order

1. Keep the bounded native filesystem mutation slice healthy; retain
   `fs_apply_patch` in the JavaScript authority until its parser/recovery
   boundary has a concrete Rust replacement hypothesis.
2. Keep structured-command on the Rust native profile; retain the JavaScript
   implementation as the explicit rollback while durable/UAC authority remains
   outside the native applet.
3. Keep the Rust Git implementation on the native profile; retain the
   JavaScript implementation as the explicit rollback while guarded
   write/recovery authority remains outside the native applet.
4. Add focused workload rows for filesystem write, structured command, and Git
   inspection/publication to the benchmark report.
5. Keep the native-profile matrix and materialized carrier configuration in
   sync; promote a boundary to Rust ownership only after its authority adapter
   has parity evidence.

Each Rust promotion must pass contract equivalence before a registrar default
changes. Benchmarks are measurements, not predeclared latency thresholds; the
decision is based on correctness plus total operational simplicity.
