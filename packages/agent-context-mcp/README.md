# @narada-core/agent-context-mcp

Carrier-entry orientation projection and evidence adapter, with administrative
checkpoint and continuation compatibility operations.

The immutable Orientation Manifest is canonical authority evidence, not the
ordinary occupant interface. Agent Start binds it to an admitted Carrier
Session and derives one bounded Orientation Brief. The normal surface exposes
only the brief-driven ceremony needed to receive required material and open the
ordinary-work gate.

## Boundary

- Allowed: resolve the exact admitted Agent/Carrier Session binding from a
  validated admission receipt.
- Allowed: compile and persist one immutable Orientation Manifest generation
  against an exact admission receipt; the start event is a downstream trace.
- Allowed: return one exact inline Orientation Brief plus its canonical
  `manifest_ref`.
- Allowed: execute bounded required reads, persist append-only page/completion
  evidence, and record an acknowledgement only after all reads complete.
- Allowed: project the canonical acknowledgement for Carrier/runtime gate
  readback.
- Allowed: expose an exact persisted manifest generation on the explicit
  administrative projection as a diagnostic MCP resource.
- Allowed: compile a separately identified, read-only diagnostic candidate.
- Allowed: write and read durable checkpoints.
- Not allowed: infer identity or startup context from latest checkpoints,
  latest start events, names, or conversational hints.
- Not allowed: let a local roster lookup overrule an exact owner-issued
  admission receipt. Missing or rejected role evidence remains an explicit
  optional residual.
- Not allowed: issue admission, activation, or delivery receipts; Agent Context
  only validates and records the exact owner-issued delivery receipt.
- Not allowed: treat diagnostic hydration as the admitted generation.
- Not allowed: task lifecycle mutation.
- Not allowed: arbitrary filesystem, shell, Git, mailbox, or worker delegation behavior.

## Runtime

This is a native Rust surface. Registrar-generated carrier bindings launch an
immutable `narada-agent-context-mcp` artifact through the native runtime proxy;
Node and Bun are not runtime dependencies.

The server uses a site root and these environment variables when present:

- `NARADA_AGENT_ID`
- `NARADA_CARRIER_SESSION_ID`
- `NARADA_CARRIER_SESSION_ADMISSION_RECEIPT`
- `NARADA_CARRIER_SESSION_ACTIVATION_RECEIPT`
- `NARADA_ORIENTATION_MANIFEST_ID`
- `NARADA_ORIENTATION_BRIEF`
- `NARADA_ORIENTATION_DELIVERY_RECEIPT`
- `NARADA_ORIENTATION_ENTRY_FILE`
- `NARADA_SITE_ROOT`
- `NARADA_SITE_ID`
- `NARADA_AGENT_CONTEXT_DB`

Receipt and manifest identifiers may also be supplied explicitly to the
corresponding tools. Explicit and inherited values must agree.

```powershell
cargo run --locked --manifest-path packages/agent-context-mcp/native/Cargo.toml -- --site-root <src-root>/site --site-id narada.example --tool-projection occupant
```

Use `cargo native-release` at workspace scope to test, promote, and materialize
the immutable carrier artifact graph.

## Normal occupant tools

- `agent_orientation_read({})`: return one thin inline occupant brief, the
  canonical `manifest_ref`, progress, and an exact `next_call`. When the carrier
  was not launched through an admitted Narada session, return a bounded
  `orientation_unavailable` result naming the carrier-session launcher as the
  recovery owner; absence of hidden entry evidence is not a transport failure.
- `agent_orientation_read({ continuation })`: replay the opaque continuation
  returned by `next_call`. It delivers one bounded page or performs the final
  acknowledgement. The caller never supplies step ids, offsets, hashes,
  timestamps, receipts, or completion evidence.
- `mcp_output_show({ ref, offset?, limit? })`: shared carrier transport readback
  for outputs from other projected surfaces. `ref` is the canonical required
  argument; the runtime retains `output_ref` only as a legacy compatibility
  alias. Offset defaults to 0, and limit defaults to 10000 with a maximum of
  20000. The orientation operation itself never emits an output reference.

The default projection exposes exactly one domain operation,
`agent_orientation_read`, plus shared transport readback. Every orientation
response is compact, bounded, and inline; the ceremony fails closed instead of
replacing it with an output reference. Resources, prompts, direct
acknowledgement, selection readback, doctor,
identity, manifest materialization, checkpoint, continuation, diagnostic
hydration, and legacy `agent_context_startup_sequence` operations require the
explicit administrative projection.
The blocked occupant proxy never forwards a direct
`agent_orientation_acknowledge` call; administrative callers must launch the
explicit administrative projection outside that occupant ceremony.

Each admitted required-read source is at most 192 KiB, and one manifest may
require at most 128 bounded read pages in aggregate. Compilation refuses an
entry that exceeds either limit before publishing a generation, so startup
latency cannot grow without a declared contract change.

## Enforced Carrier entry

1. Carrier Session Authority issues admission; Agent Start compiles and persists
   one immutable manifest generation against that exact coordinate.
2. Agent Start derives the bounded brief, issues and records one delivery
   receipt, and materializes the Carrier-entry packet.
3. Launch preflight proves the orientation tools are in the selected fabric.
   Runtime/proxy gates refuse every ordinary request or notification until
   acknowledgement, apart from the bounded bootstrap and transport allowlist.
4. The Carrier-entry bootstrap calls `agent_orientation_read({})` and follows
   every returned `next_call`. Each continuation is opaque and bound to the
   exact delivery. No caller-supplied coordinates, hashes, timestamps, or
   completion payloads are accepted. Exact selected continuity is delivered in
   this same chain from the immutable manifest generation; the thin summary is
   not its substitute.
5. The final content page records completion. A subsequent continuation on the
   same tool records canonical acknowledgement, returns a compact ready
   projection, and opens ordinary work.
6. The first Narada-hosted ordinary provider turn receives an entry-handoff
   Orientation Card derived from the exact brief. Later turns receive only a
   smaller position/authority card plus the live work-inspection call; stale
   entry summaries are not repeated. Isolated retry/resume calls receive the
   entry handoff because they deliberately omit conversation history.

The inline occupant brief deliberately omits receipt ids, digests, completion
contracts, negative-claim prose, and raw role bindings. Those remain in the
canonical brief and manifest evidence retained by the Carrier.

On the administrative projection, the manifest remains available through its exact
`narada-agent-context://orientation-manifest/<manifest-id>` MCP resource for
diagnostics. An ordinary occupant does not need to read or understand receipts,
digests, database rows, or compatibility tools.

## Administrative checkpoint and continuation compatibility

Checkpoints can include active task context, files touched, key decisions, open questions, Git head, workboard freshness, next intended action, authority basis, continuation blockers, evidence refs, worktree state, and tactical resume notes.

An optional `continuation` object uses schema `narada.continuation.v1` and is persisted inside the existing checkpoint payload. It is the canonical bounded state for fresh-session handoff: objective, current state, completed work, decisions, evidence references, blockers, next action, canonical sources, constraints, and resume mode. The surface derives `source_checkpoint_ref` and `content_hash`; it does not create a second persistence table. Keep the object below 64 KiB and never use it for raw transcripts or unbounded history.

An optional `continuation_ref` links the checkpoint to a portable Markdown projection using schema `narada.continuation.handoff.v1`. The referenced artifact must be Site-relative, no larger than 256 KiB, and match its supplied SHA-256.

Use `agent_context_continuation_export` after checkpointing to create a projection under `.ai/continuations`. The default filename is derived from the agent and checkpoint ID; an explicit path must remain under that directory and end in `.md`. Existing projections are reused when identical, refused when different unless `overwrite: true` is explicit, and never become a second authority.

For `agent_context_rehydrate` and `agent_context_continuation_read`, omit
`checkpoint_id` to inspect the latest current checkpoint, or pass an exact id
to select current or archived state scoped to the requested Agent. An explicit
missing id returns `checkpoint_not_found` and never falls back.
`agent_context_continuation_export` remains latest-only.

For `agent_context_hydrate_current`, omission means continuity is omitted.
`agent_context_startup_sequence` rejects checkpoint arguments altogether:
continuity can enter admitted startup only as an entry already bound into the
persisted Orientation Manifest generation.

Use `agent_context_continuation_read` to verify the selected reference, artifact size, artifact SHA-256, and the embedded canonical continuation content hash. `agent_context_hydrate_current` includes the same result as `portable_continuation`; stale projections are reported with `status: stale` while live checkpoint hydration remains available.

## Agent guidance

Occupants consume the exact admitted generation through the one-tool ceremony;
they are not expected to discover or invoke hidden checkpoint tools. Carrier
lifecycle and explicit administrative workflows own checkpoint/continuation
capture until a separate normal closeout capability is defined. Agent Context
evidence is not admission to act and is not task-completion evidence; task
lifecycle reports still go through `task-lifecycle-mcp`.

## Verification

The required Carrier-projection E2E launches the actual Agent Context process
behind both the TypeScript and built native Rust runtime proxies for Codex and
Kimi materializations. Each topology performs the one-tool ceremony, proves an
ordinary observable effect is refused before acknowledgement, proves
escape-heavy required material stays inline, delivers the exact selected
checkpoint, and proves that effect is admitted afterward. Agent Context owns
manifest compilation, delivery, page/completion evidence, and acknowledgement
creation in every topology; the proxies own only substitutable enforcement.
The Narada-hosted E2E additionally executes the provider tool loop through the
NARS runtime. For external Codex/Kimi carriers, the mechanical gate covers
Narada-projected MCP effects; native carrier capabilities require their own
launch policy and are not falsely claimed by this test.

```powershell
pnpm --filter @narada-core/agent-context-mcp test
pnpm --filter @narada-core/agent-context-mcp run test:node
```
