# MCP Surfaces Target Shape

This document defines the implementation-driving target shape for `mcp-surfaces` as Narada's governed crossing layer. It is scoped to this repository: it does not replace Narada doctrine, but it translates the doctrine into package-level expectations that can drive implementation tasks and review.

## Purpose

An MCP surface is a typed boundary where a caller asks for inspection, proposal, mutation, execution, or orchestration. The surface owns the mechanical contract for that boundary: schemas, policy checks, output limits, materialized references, typed errors, audit evidence, and precise refusal.

The target shape is:

```text
caller intent
-> typed MCP input
-> surface-owned policy / authority check
-> governed effect or typed refusal
-> durable evidence or bounded observation
-> readback / review / reconciliation path
```

Surfaces must be ergonomic, but ergonomics must compose governed primitives rather than hiding authority or evidence.

Surface visibility inside a session is not the same thing as surface ownership.
See `mcp-injection-scopes.md` for the doctrine that separates host, user-site,
and local-site MCP injection scopes from session aliases.

## Surface Contract Invariants

Every surface should make these properties inspectable or mechanically enforced when relevant:

- **Typed inputs**: schemas reject invalid shapes instead of silently coercing them. Long or nested companion data uses `payload_ref`, `payload_json`, or another explicitly documented transport path.
- **Typed outputs**: successful calls return structured content plus deterministic rendered text. Large outputs materialize behind explicit output refs and reader tools.
- **Policy readback**: policy-gated surfaces expose an inspect/doctor tool that reports allowed roots, roles, commands, profiles, limits, and unsafe toggles without leaking secrets.
- **Bounded execution**: command/process surfaces use argv/config structures, not raw command strings; adapters receive resolved policy, not raw tool input.
- **Authority evidence**: mutations record who requested them, what authority basis admitted them, what target was changed, and what readback or review remains.
- **Observation/evidence separation**: readback, CLI output, and generated summaries are observations until a surface admits them as evidence under its lifecycle rules.
- **Precise refusal**: expected failures use domain-specific error/status codes with actionable remediation; no policy failure should fall through to a generic unhandled error.
- **No host-policy leakage**: Codex, OpenCode, `agent-cli`, and other MCP hosts may invoke the same surface, but host identity must not silently change policy.
- **Explicit injection scope**: bound surfaces should distinguish session alias from authority locus, mutation locus, and restart owner. A host-injected or user-site-injected surface may be visible in a local site session without becoming local-site-owned. New producers should expose this as `narada_scope`; readers should prefer `narada_scope`, then fall back to legacy flattened fields, then catalog/default scope.

## Checkpoint and Continuation Contract

Agent-context checkpoint state remains the canonical operational state. A
checkpoint may carry one canonical, bounded `narada.continuation.v1` subobject
and one exact reference to a portable continuation artifact. Both live in the
existing checkpoint payload; the Markdown artifact is a handoff projection,
not a second authority or an independent state store.

The canonical continuation subobject contains the bounded fresh-session state:

- objective and current-state summary;
- completed work, decisions, evidence references, open blockers, and next action;
- canonical sources, constraints, and resume mode;
- a generated continuation ID and creation timestamp;
- a generated source checkpoint reference and SHA-256 content hash.

The subobject is capped at 64 KiB. Its provenance is derived when the
checkpoint is written, and its content hash covers the canonical state without
the generated provenance link. A caller therefore cannot silently point the
canonical state at a different checkpoint. Existing checkpoint tables and
callers remain compatible because the subobject is optional and stored in
`payload_json`.

The reference has schema `narada.continuation.handoff.v1` and contains:

- a site-relative artifact path;
- the artifact SHA-256;
- its creation timestamp;
- the artifact schema identifier.

The checkpoint surface verifies the artifact exists, is bounded, and matches
the supplied SHA-256 before replacing the active checkpoint. It preserves the
reference through history and rehydration, and refuses absolute or cross-site
paths. A fresh agent reads the referenced handoff first, then
verifies live state against Git, agent-context, task, and other authoritative
surfaces. Continuation artifacts should be bounded snapshots; they must not be
raw transcripts or diff-only state.

The automated projection workflow is:

1. `agent_context_continuation_export` reads the latest canonical continuation
   and writes a Markdown projection under `.ai/continuations`.
2. The exporter computes the artifact SHA-256 and attaches the verified
   `narada.continuation.handoff.v1` reference to the active checkpoint.
3. `agent_context_continuation_read` or `agent_context_hydrate_current` verifies
   the reference and, when canonical state exists, the embedded continuation
   content hash.
4. A missing, changed, or mismatched artifact is reported as stale; it does
   not silently override live checkpoint, Git, task, or Site authority.

The exporter’s default path is immutable per checkpoint. Explicit replacement
is limited to the `.ai/continuations` subtree and requires `overwrite: true`.

See `mcp-output-refusal-conventions.md` for the cross-surface output reference,
payload reference, and refusal conventions that support these invariants without
forcing all surfaces into one domain schema.

## Shared Transport Contract

`@narada-core/mcp-transport` is a reusable substrate, but each instance is bound to
one site authority scope. The transport package may materialize and read payload
or output references for that bound scope; it must not turn local filesystem
reachability into cross-site authority.

The target contract is:

- A server constructs one explicit transport scope containing the bound site
  root, managed payload/output roots, and separate storage and inline-response
  limits. Tool calls do not supply replacement roots or arbitrary managed
  directories.
- Output pages are always bounded by a hard serialized-response budget. Stored
  output size, page size, and inline envelope size are distinct limits. Invalid
  page sizes, including zero, are refused rather than silently expanded or
  treated as complete.
- Pagination uses a validated cursor or another explicitly Unicode-safe position
  model. A page reports a continuation whenever content remains. Tool content,
  `structuredContent`, and resource readback use the same bounded page contract.
- Payload and output revisions are immutable under concurrent writers. Creation
  is exclusive/atomic; identical retries are idempotent, while conflicting
  content for an existing revision is refused. Recorded byte sizes equal the
  actual UTF-8 bytes stored.
- All successful result paths use one bounded result builder. Inline and
  materialized responses preserve the same structured envelope and deterministic
  rendered text; no helper may bypass the response budget.

### Cross-Site Boundary

The shared transport contract does not expose a raw `target_site_root` or any
equivalent path-based cross-site reader. Cross-site data movement, when needed,
belongs to an explicitly authorized User Site or artifact/export surface. That
surface identifies the source site, establishes the authority basis, records the
handoff, and gives the receiving site an owned bounded copy or explicit
capability. A local transport reader must never infer that authority from a
second path being readable on the same machine.

These rules are implementation acceptance criteria for transport changes. The
package README summarizes them; this section is the canonical package-level
target and should be used to derive implementation tasks and regression tests.

## Execution Boundary Contract

Task and delegated-task surfaces must make the transition from intent to
execution explicit. The shared `@narada-core/execution-contract` package provides
the typed `ExecutionBinding` and request fingerprint primitives; the owning
surface remains responsible for policy and durable state.

- A binding records the absolute workspace root, executor kind/profile/id,
  optional repository and Site roots, and a stable correlation key.
- The owning surface validates the binding before persistence and passes the
  bound workspace to the executor. A path is a target-locus fact, not an
  authorization grant.
- An idempotency key identifies one request. Repeating it with the same
  canonical request is a read/replay or explicit continuation; reusing it with
  a different request is a typed conflict.
- A store or transport failure must not blindly retry a mutation after an
  attempt may have committed. Automatic retry requires an authoritative
  idempotency contract or read-only/idempotent tool metadata; otherwise the
  surface reports that the caller must inspect operation state.
- External task dependencies are durable gates. A dependent task remains in a
  waiting posture until every referenced task has a completed outcome; missing,
  failed, cancelled, or self-referential dependencies are reported explicitly.
- Recovery output must describe the actual lifecycle state and the next valid
  action. It must not tell an operator to continue a task that is still
  deferred, blocked, or awaiting review.

These invariants are tested by the execution-contract package, delegated-task
execution-boundary tests, and task-lifecycle handler/recovery tests.

## Boundary Rules By Package Family

- `mcp-transport` owns generic payload and output-reference mechanics only. It must not acquire task lifecycle, mail, git, or worker domain behavior.
- Filesystem, command, and git surfaces own governed substrate access. They must keep read/write/execute authority separate and disclose scope, freshness, and audit posture.
- Task lifecycle and completion surfaces own work records, evidence admission, reports, reviews, and closure gates. Task Markdown is an authored projection/evidence surface, not lifecycle authority by itself.
- Worker delegation owns policy-gated worker runtime invocation. It is not a shell, task lifecycle surface, recursive worker-control surface, or general orchestrator.
- Delegated task owns durable delegated task records, workflow plans, acceptance contracts, events, and handoff packets. It should coordinate outcomes without taking over worker runtime, filesystem, git, or shell authority.
- Mailbox owns read-only synced mailbox projection. Graph mail owns policy-gated live Graph reads and draft lifecycle. Sending remains disallowed unless explicit site policy admits it.
- Registrar and site lifecycle own the weave between surfaces, sites, and carriers. They may write config under their authority, but they do not start processes or mutate the surface implementations themselves.
- Surface feedback owns MCP usage friction intake. Bugs, gaps, schema failures, and ergonomics observations should enter there before becoming implementation tasks.

## Ergonomic Composition Rules

Higher-level helpers are allowed when they preserve the underlying governed records.

A compound helper may combine steps such as claim, write notes, prove criteria, admit evidence, and submit report, but it must still emit the same claim intent, evidence admission, report, review obligation, and changed-file evidence that primitive calls would emit.

A helper must not:

- infer authority from caller fluency or model judgment;
- hide a policy crossing inside a convenience path;
- mutate through a read surface;
- collapse draft, send, execution, and confirmation into one unreviewable act;
- replace explicit payload refs with larger ad hoc inline limits.

## MCP Fabric V2 Compilation Boundary

The authority-partitioned execution follow-on is specified in
`docs/mcp-surface-runtime-target-shape.md`. That design keeps Fabric V2 as the
declaration/compiler boundary and adds no runtime authority to the registrar.

MCP Fabric V2 treats surface declarations as versioned inputs to a deterministic
compiler rather than as carrier configuration fragments:

1. A surface descriptor declares tool schemas, effects, projections, transport
   requirements, and lifecycle semantics.
2. Site policy selects descriptors and produces a normalized fabric manifest.
3. A pure compiler projects that manifest into carrier-neutral server records
   and then into carrier-specific configuration documents.
4. Runtime observation is recorded separately from desired state.
5. Reconciliation compares desired and observed state and returns explicit
   start, stop, restart, or reconfigure actions for the owning actuator.

`@narada-core/mcp-fabric-contracts` owns the Zod/TypeScript/JSON Schema contracts,
canonicalization rules, and stable digests for these documents. It does not own
Site discovery, authority decisions, process lifecycle, or carrier files.
`@narada-core/mcp-fabric-compiler` consumes those contracts and purely produces
Codex TOML, Kimi MCP JSON with effect-derived approvals, and OpenCode JSONC
documents. Guarded apply and rollback remain control-plane adapter concerns.

The registrar is native-only: every registered entry is compiled from its
package-owned descriptor, and there is no legacy catalog or adapter in the
production path. Native descriptor disagreement is therefore a compile-time
failure rather than a source-selection decision.

Schema major versions are explicit. Unsupported majors, duplicate identities,
invalid effect declarations, and incoherent lifecycle requirements are refused
before compilation. Declaration ordering and object-key ordering do not affect
digests, while order-sensitive transport argv remains preserved.

### Stable logical endpoints and generations

Eligible replayable stdio and session-pinned Streamable HTTP surfaces may be
replaced behind a stable logical endpoint. A replacement progresses through
starting and warming before it can become active. Warm-up proves initialize,
initialized notification where applicable, tools/list contract digest, and an
optional declared read-only health call. Failure leaves the old generation
active.

After atomic activation, new stdio calls route to the replacement while old
in-flight calls drain. Existing HTTP session IDs remain pinned to the old
generation while new sessions select the replacement. At bounded drain expiry,
old work receives `session_generation_retired` with deterministic reconnect
guidance and the old adapter terminates its process tree. Descriptors marked
`restart_required` never enter this path; the named carrier/session actuator
must restart them.

## Gap Map From Current State

The current dirty tree and `defects.md` indicate these implementation themes:

1. **Worker delegation hardening**
   - Replace partial/fallback parsing and coercion with strict input/config validation.
   - Preserve absent worker output as a domain-specific failure instead of normalizing to fallback empty fields.
   - Convert output-ref read failures to typed materialization errors.
   - Strengthen tests around real binary/package surfaces and prompt/output evidence.

2. **Task lifecycle ergonomics and evidence**
   - Consolidate create payload schema, safe list-field normalization, long-field payload-ref guidance, and generic engineer claim authority into a coherent task-lifecycle ergonomics change set.
   - Keep role crossing narrow: generic `engineer` may accept site-specific `*-engineer` only with recorded operator authority; unrelated roles still reject.
   - Consider a compound governed work-submission helper only after primitive evidence records remain stable.

3. **Local filesystem / structured command / git substrate surfaces**
   - Continue making policy, ignored paths, output pagination, and refusal reasons first-class and consistent across read, write, execute, and publication surfaces.
   - Avoid broad shells, wildcard mutation paths, or silent coercion of policy-relevant flags.

4. **Delegated task and worker workflow split**
   - Keep delegated-task as the durable orchestration/contract surface and worker-delegation as the runtime execution surface.
   - Add fan-out/fan-in review ergonomics only through durable workflow records, not recursive worker-control leakage.

5. **Graph mail attachment and draft workflow**
   - Attachment operations should be first-class Graph mail tools, not ad hoc structured-command scripts that dump large base64 into model context.
   - Draft creation, attachment upload, attachment list/readback, and send policy must remain separate authority steps.

## Task Derivation Rule

When converting gaps into tasks, each task should name:

- the target invariant it advances;
- the current behavior or defect;
- the surface that owns the fix;
- the public schema/tool behavior that changes;
- package-local tests and any cross-surface smoke tests;
- whether the change is primitive behavior or ergonomic composition.

Do not create tasks that merely say "clean up" or "improve ergonomics" without tying them to one of the target invariants above.

## Ready Task Candidates

These candidates are intentionally phrased as task payload material. Before creating them, check for duplicate open work by title and surface.

## Baseline versus residual

`Ready Task Candidates` is a residual-work register, not a release checklist.
Implemented behavior is recorded separately so a completed feature is not
recreated as an open design gap.

Implemented baseline reviewed on 2026-08-08:

- `task_lifecycle_submit_work` provides the compound task-lifecycle submission
  path while preserving primitive evidence, role gates, changed-file checks,
  recovery truthfulness, and review obligations.
- The implementation is in
  `packages/task-lifecycle-mcp/src/task-lifecycle/task-mcp-server.ts` and is
  covered by the task-lifecycle package tests.

The candidates that follow are residual only unless their heading or evidence
explicitly says otherwise.

### Worker delegation strict policy/input hardening

- Target invariant: typed inputs, policy readback, precise refusal.
- Current gap: worker-delegation accepts or masks malformed config/tool inputs through partial TOML parsing, silent trust-config skipping, broad truthiness for `skip_git_repo_check`, non-object `config` coercion, and missing CLI flag values.
- Surface: `worker-delegation-mcp`.
- Public behavior: malformed policy/config/tool input must fail with typed worker errors and actionable details instead of defaulting or silently ignoring values.
- Tests: `pnpm test:worker-delegation`; include package-local regressions for malformed TOML, malformed trust config, non-object `config`, string `skip_git_repo_check`, and missing CLI flag values.

### Worker delegation runtime/output evidence hardening

- Target invariant: typed outputs, bounded execution, precise refusal.
- Current gap: worker-delegation normalizes absent or invalid worker output into fallback empty fields and reports missing last-message output as generic invalid shape.
- Surface: `worker-delegation-mcp`.
- Public behavior: absent output, invalid output shape, runtime startup failure, and output-ref read failure must remain distinguishable typed states in `result.json`, rendered output, and MCP errors.
- Tests: `pnpm test:worker-delegation`; include absent last-message, invalid last-message, output-ref missing file, and spawned-process failure after process creation.

### Worker delegation packaged runtime smoke coverage

- Target invariant: bounded execution, typed outputs, no artifact-first test false positives.
- Current gap: successful tests use fake Codex-shaped fixtures and protocol smoke starts built source instead of the package bin surface.
- Surface: `worker-delegation-mcp`.
- Public behavior: package bin mapping and real invocation argv/schema behavior must be covered by a smoke path; fixture tests must assert prompt construction evidence instead of allowing fallback branches.
- Tests: `pnpm test:worker-delegation`; add a packaged-bin smoke test and, where real Codex cannot be required in CI, a clearly named optional/manual real-Codex compatibility smoke.

### Graph mail first-class attachment lifecycle

- Target invariant: bounded execution, observation/evidence separation, no command workaround for governed mail effects.
- Current gap: attachment upload workflows can require ad hoc structured-command scripts and large base64 output, risking model-context overflow and hidden Graph behavior.
- Surface: `graph-mail-mcp`.
- Public behavior: expose first-class draft attachment tools for small direct attachments and large upload-session chunking, with list/readback verification and send still separately policy-gated.
- Tests: `pnpm test:graph-mail`; include small attachment create, large upload-session chunking with bounded output, attachment list readback, token/policy failures, and no-send default.

### Cross-surface output/ref and refusal consistency audit

- Target invariant: typed outputs, materialized refs, precise refusal.
- Current gap: local-filesystem, structured-command, git, worker-delegation, and task-lifecycle have converged unevenly on output paging, output refs, refusal details, and payload refs.
- Surfaces: `local-filesystem-mcp`, `structured-command-mcp`, `git-mcp`, `worker-delegation-mcp`, `task-lifecycle-mcp`.
- Public behavior: document and test common output/ref/refusal conventions without forcing all surfaces into one domain schema.
- Tests: package-local tests for touched surfaces plus root `pnpm test` if shared transport helpers change.

The reference implementation uses Zod at the transport boundary for scope, page, and argument-shape validation. Validation is part of the authority boundary, not an optional convenience for individual surfaces. Compatibility entry points may accept legacy root fields temporarily, but an explicit scope and legacy overrides are mutually exclusive.

## Fabric V2 Native Descriptors and Runtime Observation
Every registered surface resolves from a package-owned `SurfaceDescriptorV2`. The package's actual `tools/list` registry is the input to descriptor emission, so schemas, guidance identity, effects, projection authority, runtime requirements, and lifecycle requirements cannot silently diverge in the registrar.
The registrar does not retain a legacy catalog or compatibility adapter in the
production path. Native descriptors are the authored/catalog authority for
registered surface identity, live tool contracts, effects, projections, and
lifecycle semantics. Loader fallback entries are compatibility projections only:
they must resolve to the same built entrypoint, arguments, and live tool
contract, and cannot become a second authority or silently diverge.
Carrier and Site materialization embeds the selected native descriptor plus its descriptor and tool-contract digests in `surface_projection`. This gives the loader a declared contract it can compare with fresh child `tools/list` output.
Projection selection remains explicit: a surface with multiple projections requires `projection_id` or a runtime context that selects exactly one; current directory and server-name heuristics are not valid selectors.
Runtime observations use `RuntimeObservationV2`. A generation record reports logical connection identity, generation state, heartbeat, lease expiry, freshness, health, descriptor/tool digests, and lifecycle-specific recovery actions. Atomic runtime records belong under the configured runtime-state root; a generic observation sink may emit events without taking Narada persistence authority.
Reconciliation is pure planning. It returns exactly one bounded action: `no_op`, `replace_generation`, `reconnect_required`, `rematerialize_all_carrier_configs`, or `unsupported`. Mutation plans carry one responsible actuator, required authority, operation ID, expected-state guards, and a durable outcome lookup.
Registrar applies config, mcp-loader replaces eligible child generations, and carrier supervisors restart restart-required surfaces. No layer implicitly performs another layer's actuation.
A stale or missing observation refuses application before mutation. Guidance and refusal messages name the responsible actuator and exact next call, including `mcp_loader_surface_restart` and the carrier-supervisor capability for loader or restart-required recovery.
