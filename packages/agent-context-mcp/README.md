# @narada-core/agent-context-mcp

Registrar-bound compatibility and diagnostic surface for admitted Agent
orientation, durable checkpoints, and continuation projections.

The surface does not create Agent identity or Carrier Session admission. It
accepts an owner-issued admission receipt, compiles or reads an Orientation
Manifest against that exact coordinate, and maintains bounded continuity
artifacts.

## Boundary

- Allowed: resolve the exact admitted Agent/Carrier Session binding from a
  validated admission receipt.
- Allowed: compile and persist one immutable Orientation Manifest generation
  against an exact admission receipt; the start event is a downstream trace.
- Allowed: deliver an exact persisted manifest generation selected by id.
- Allowed: compile a separately identified, read-only diagnostic candidate.
- Allowed: write and read durable checkpoints.
- Not allowed: infer identity or startup context from latest checkpoints,
  latest start events, names, or conversational hints.
- Not allowed: let a local roster lookup overrule an exact owner-issued
  admission receipt. Missing or rejected role evidence remains an explicit
  optional residual.
- Not allowed: issue admission, activation, or delivery receipts.
- Not allowed: treat diagnostic hydration as the admitted generation.
- Not allowed: task lifecycle mutation.
- Not allowed: arbitrary filesystem, shell, Git, mailbox, or worker delegation behavior.

## Runtime

This is a Bun-first surface. Registrar-generated carrier bindings launch it with
`bun`; Node remains a supported compatibility runtime through the shared SQLite
adapter.

The server uses a site root and these environment variables when present:

- `NARADA_AGENT_ID`
- `NARADA_CARRIER_SESSION_ID`
- `NARADA_CARRIER_SESSION_ADMISSION_RECEIPT`
- `NARADA_CARRIER_SESSION_ACTIVATION_RECEIPT`
- `NARADA_ORIENTATION_MANIFEST_ID`
- `NARADA_SITE_ROOT`
- `NARADA_SITE_ID`
- `NARADA_AGENT_CONTEXT_DB`

Receipt and manifest identifiers may also be supplied explicitly to the
corresponding tools. Explicit and inherited values must agree.

```powershell
pnpm --filter @narada-core/agent-context-mcp build
bun packages/agent-context-mcp/dist/src/main.js --site-root <src-root>/site --site-id narada.example
```

## Tools

- `agent_context_doctor`: check DB readiness and schema presence.
- `agent_context_whoami`: validate and project the exact identity/session
  binding from an admission receipt.
- `agent_context_start_session`: compatibility materialization of an immutable
  Orientation Manifest plus downstream start trace; exact admission evidence is
  mandatory.
- `agent_context_checkpoint`: write a durable checkpoint and, when needed, one bounded canonical continuation state.
- `agent_context_rehydrate`: read the latest checkpoint, an exact current or archived checkpoint, or bounded checkpoint history for an agent.
- `agent_context_continuation_export`: render the latest canonical continuation to a Site-local Markdown projection and attach its verified reference.
- `agent_context_continuation_read`: verify and read the latest or explicitly selected canonical continuation and its Markdown projection.
- `agent_context_hydrate_current`: compile a read-only diagnostic Orientation
  Manifest candidate. Continuity is included only when an exact checkpoint id
  is supplied; omission never means latest.
- `agent_context_startup_sequence`: read the exact immutable admitted manifest
  generation by id. It never recompiles, selects latest, or writes a
  checkpoint.
- `agent_context_list_sessions`: list local agent start sessions.

## Admitted startup

The normal Carrier entry procedure is:

1. Carrier Session Authority issues an admission receipt.
2. Agent Start validates that receipt, compiles one bounded manifest, persists
   it, and projects `NARADA_ORIENTATION_MANIFEST_ID` with the receipt.
3. The admitted Carrier calls `agent_context_startup_sequence`.
4. The surface reads that exact generation, verifies its digest, deterministic
   id, byte count, database index fields, and admission binding, then returns
   it without mutation.

`agent_context_startup_sequence` makes no delivery-authority claim and emits no
delivery receipt. A future owner-issued delivery receipt remains a separate
contract. Use `agent_context_hydrate_current` only to inspect a newly compiled
diagnostic candidate; its result cannot replace the admitted generation.

## Checkpoint and Continuation Content

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

## Agent Guidance

Agents should consume the exact admitted generation at startup, checkpoint
meaningful state transitions, and use checkpoint/continuation reads for
explicit resumption. Agent Context evidence is not admission to act and is not
task-completion evidence; task lifecycle reports still go through
`task-lifecycle-mcp`.

## Verification

```powershell
pnpm --filter @narada-core/agent-context-mcp test
pnpm --filter @narada-core/agent-context-mcp run test:node
```
