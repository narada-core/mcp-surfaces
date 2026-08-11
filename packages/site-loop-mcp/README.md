# @narada-core/site-loop-mcp

Config-governed Site Loop MCP runtime and site operating-loop tools.

Site Loop behavior is generic. Site-specific identity, resident target, refs, command strings, policy defaults, docs, and approved test selectors belong in config, not hardcoded adapter code.

The config file path is:

```text
.narada/capabilities/site-loop-config.json
```

When the file is absent, doctor/readback reports `status: "missing"` with a neutral generic template, but active site-loop tools refuse to run. Production sites must provide this file explicitly so site identity, resident target, refs, policy, docs, and tests are site-owned config.

The canonical JSON Schema is published at:

```text
schemas/site-loop-config.schema.json
```

The runtime validator uses the same schema, then applies semantic checks that JSON Schema cannot express cleanly, such as safe relative paths and Narada schema-name conventions.

Minimal valid config:

```json
{
  "schema": "narada.site_loop.config.v2",
  "loop_id": "example.loop",
  "site_id": "narada-example",
  "display_name": "Example loop",
  "resident": {
    "agent_id": "example.resident",
    "role": "resident"
  },
  "refs": {
    "ticket_projection": {
      "kind": "ticket_projection",
      "ref": "example"
    }
  },
  "persistence": {
    "schema": "narada.site_loop.persistence.v2",
    "evidence_root": ".ai/site-loop-evidence",
    "raw_retention_days": 7,
    "summary_retention_days": 90,
    "inline_summary_bytes": 16384,
    "compression": "gzip"
  }
}
```

## Boundary

- Allowed: inspect configured Site Loop readiness and allowlisted docs/tests.
- Allowed: run approved local test selectors.
- Allowed: inspect and control the configured Site Operating Loop.
- Allowed: run one bounded site-loop pass.
- Not allowed: arbitrary shell commands.
- Not allowed: general task lifecycle mutation outside the explicit site-loop/task dispatch behavior.
- Not allowed: mailbox reading or Graph draft operations; those belong to `mailbox-mcp` and `graph-mail-mcp`.

## Tools

General Site Loop:

- `site_loop_guidance`
- `site_loop_doctor`
- `site_loop_config_validate`
- `site_docs_list`
- `site_docs_show`
- `site_test_list`
- `site_test_run`

Compatibility aliases retained for existing callers:


Configured site loop:

- `site_loop_status`
- `site_loop_health`
- `site_loop_operating_status`
- `site_loop_proof_status`
- `site_loop_proof_run`
- `site_loop_recovery_drill`
- `site_loop_readiness`
- `site_loop_coherence`
- `site_loop_runs_list`
- `site_loop_run_show`
- `site_loop_attention_list`
- `site_loop_attention_show`
- `site_loop_attention_ack`
- `site_loop_control_set`
- `site_loop_run_once`

## Site Loop Notes

`site_loop_run_once` runs a bounded configured Site operating pass over already-admitted Site work. It reconciles pending Task Executability Assessment requests and dispatches resident directives. Mailbox sync, admission, SOP activation, and ticket association remain owned by Mailbox, Scheduler, SOP, and Work Lifecycle respectively; Site Loop does not execute or prove those domain workflows.

Production non-dry execution remains refused through MCP and belongs to the scheduler/supervisor path. An explicitly declared `test_authority` may opt into a non-dry MCP run only with `wait_for_completion: true`, `requireLiveCarrier: false`, no live resident edge, and `timeout_ms <= 10000`; the result identifies the isolated authority roots. Pass `test_authority: true` to `site_loop_runs_list` or `site_loop_run_show` to read that isolated run store explicitly.

The `task_executability_reconciliation` phase is a recovery coordinator, not a second authority. Task Lifecycle owns request, lease, attempt, and assessment state; the Site Loop only leases a bounded batch and invokes the shared task-executability orchestrator. The phase is capped at ten requests per pass, reclaims expired leases after restart, and uses the same Task Lifecycle database as the other task phases. Its result is `deferred` with warning attention when the store/orchestrator binding is absent, `ok` when work is idle/progressing without execution failures, and `attention` when bounded evaluator/delegation failures require review. It never turns evaluator execution failure into a task verdict and never hardcodes a provider or model.

Executability has an explicit translation boundary: the delegated worker emits the dotted `narada.task.executability.assessment.v1` envelope with `evaluator_provenance` inside `structured_outputs.task_executability_assessment_v1`; this adapter converts it to Task Governance's canonical persisted `narada.task_executability_assessment.v1` record with normalized `evaluator` provenance. Task Lifecycle MCP's `task_lifecycle_executability_status` remains compact by default; pass `include_assessment: true` to read the admitted assessment and evaluator provenance through MCP. E2E and operational verification should use that readback rather than opening the Task Lifecycle SQLite file directly.

The target shape is “just config”: if site behavior differs, express it in `site-loop-config.json`. If behavior cannot be expressed there, add a generic config primitive or delegate to an existing MCP/tool command that is itself referenced by config. Do not add a new site-specific adapter branch.

Core config fields:

- `loop_id`, `site_id`, `display_name`
- `resident.agent_id`, `resident.role`, required resident task tools
- `schemas` for result packet names
- `commands` for status, readiness, and resident-proof operator hints
- `policy` carrier admission and attention defaults
- `production_proof` freshness window
- `docs` and `tests` allowlists

Readiness/coherence tools report whether Site operating prerequisites and resident production proof are satisfied. Domain-workflow readiness belongs to the surfaces that own those workflows.

## Canonical Site Operating Runtime Host migration

The MCP surface now uses Narada's canonical Site Operating Runtime Host boundary
for bounded `site_loop_run_once` execution. The adapter claims one durable host
lease, records `created -> binding -> ready -> serving -> closing -> stopped`
evidence, refuses a concurrent active supervisor, and exposes the resulting
`runtime_host` in loop status. The existing Site Loop engine remains the domain
body adapter during migration; domain phases, source sync, and effect admission
remain owned here. Long-running supervision will move to the same canonical host
boundary. The `site-loop-supervisor` entry point now routes the long-running
`superviseSiteLoop` domain engine through that same host lease and lifecycle
evidence. The engine remains the domain body adapter during migration; domain
phases, source sync, the compatibility file heartbeat, and effect admission
remain owned here. The file heartbeat is a probe/compatibility signal, not a
second authority. The old `site-loop-runner --supervise` path remains available
for compatibility, but new supervision must use `site-loop-supervisor`.

## Persistence v3 hard cutover

Site Loop operational SQLite storage uses `narada.site_loop.storage.v3`. SQLite
keeps bounded summaries, counts, digests, and content-addressed evidence refs;
large historical payloads are gzip-compressed under the configured
`persistence.evidence_root`. Summary reads never hydrate raw evidence. Use
`site_loop_run_show` with `detail: "full"` when the raw run/step payload is
needed.

The runtime deliberately refuses a pre-v3 or partially migrated database with
`site_loop_storage_cutover_required`. Perform the one-way migration explicitly
under the Task Lifecycle write lock:

The cutover also requires the Task Lifecycle database to have completed its
explicit preparation contract; it does not silently prepare or migrate that
database. For an existing site, prepare it first, then run the acknowledged
cutover:

```powershell
node packages/task-lifecycle-mcp/dist/src/task-lifecycle/task-mcp-server.js --prepare --site-root <src-root>/site
```

If preparation has not happened, the cutover fails fast with
`task_lifecycle_store_not_prepared` and the remediation is the same explicit
prepare-then-retry sequence.

```powershell
pnpm --filter @narada-core/site-loop-mcp exec site-loop-storage-cutover --site-root <src-root>/site --ack-cutover
```

The cutover does not retain a legacy schema or runtime fallback. It preserves
Task Lifecycle authority and control/health state, rehydrates only the current
classification projection and latest directive outcomes, resets stale loop
locks, drops old oversized Site Loop run/step history, and starts the v3
retention window at cutover.

If a database contains only the bounded current-state Site Loop tables from an
interrupted or pre-v3 initialization, the same acknowledged cutover completes
the missing v3 tables, rehydrates those current projections, resets locks, and
drops the partial tables in the cutover transaction. It does not guess at or
retain an incomplete legacy schema.

The cutover runs SQLite `VACUUM` after committing the new schema so dropping
the old tables also releases their pages from the database file. If the
irreversible cutover commits but compaction cannot complete, retry it through
explicit maintenance.

An acknowledged idempotent cutover retry also recreates missing v3 indexes
before validating an already-cut-over database.

Retention is explicit maintenance, not run-finalization work. Run it with an
explicit site root and acknowledgement; each invocation is bounded and
advances its evidence cursor:

```powershell
pnpm --filter @narada-core/site-loop-mcp exec site-loop-storage-maintenance --site-root <src-root>/site --ack-maintenance
```

Add `--compact` when a full SQLite rewrite is wanted after pruning:

```powershell
pnpm --filter @narada-core/site-loop-mcp exec site-loop-storage-maintenance --site-root <src-root>/site --ack-maintenance --compact
```

Full run reads fail closed when a referenced evidence artifact is missing,
corrupt, or the configured evidence root no longer matches the root pinned in
SQLite. Repair the storage/configuration boundary before attempting a full
read; summary reads remain bounded and do not silently claim raw evidence is
available.

## Run

```powershell
pnpm --filter @narada-core/site-loop-mcp build
site-loop-mcp --site-root <src-root>/site
```

## Agent Guidance

Agents should use the docs and test selector tools instead of shelling out. Loop control mutations should include a reason and principal. Use `site_loop_run_once` for bounded operation, not as a general-purpose automation shell. When behavior looks site-specific, inspect `site_loop_doctor.site_loop_config` and the site config file before assuming the package owns that behavior.

See `docs/site-loop-doctrine.md` at the repository root for the doctrine,
surface-boundary contract, and target `test_authority` shape for full non-dry
e2e tests.

## Task Executability Proof

The deterministic cross-surface proof is the Site Loop closure gate:

```powershell
pnpm --filter @narada-core/site-loop-mcp test:e2e:task-executability
```

It uses separate real Task Lifecycle MCP, Site Loop runner, and NARS runtime child processes, including the production NARS task-executability dispatch hook. The only controlled boundary is a local OpenAI-compatible HTTP evaluator fixture; its response and the bounded race delay are synthetic test controls, not claims about external-provider correctness. The emitted evidence distinguishes the real NARS hook path, Site Loop reconciliation, persisted assessment, delegated/worker provenance, and exactly-one-completed/one-locked concurrent run. Store ownership and temporary-root cleanup are asserted; promise barriers, not sleeps, establish ordering. This does not prove that the task, its outcome, or an external provider is correct. The optional live provider proof and recovery runbook are documented in Narada's `docs/operations/task-executability-e2e-and-recovery.md` runbook.

## Site Loop E2E gates

The isolated gate is the deterministic closure test. It spawns real Site Loop,
Mailbox, Task Lifecycle, Work Lifecycle, Scheduler, NARS, and supervisor
processes; only the mailbox and model provider boundaries are contract
fixtures:

```powershell
pnpm --filter @narada-core/site-loop-mcp test:e2e:isolated
```

The isolated gate is a complete closure proof, not a smoke test. Every run
must execute all six interruption boundaries:

- mailbox generation cursor committed
- mailbox observation receipt
- ticket source projection
- scheduler activation admitted
- agent decision receipt
- terminal proposal projection

Each boundary uses an isolated Site root and proves interruption, replay,
durable receipts, cursor advancement, both terminal routes (response draft and
follow-up task), exactly-once projection, and no silent phase skips. The gate
deadline is 180 seconds. Scenarios may run concurrently because their roots
are isolated, but NARS decisions within one Site remain serialized because
they share that Site's SQLite intelligence registry. NARS is a real child
process; its ambient MCP bootstrap is disabled only because the supervisor and
bridge separately spawn and exercise the real mechanical MCP surfaces. Cleanup
runs after every scenario worker has settled and any remaining temporary root
fails the gate.

The production gate is opt-in and never substitutes a default Site, scheduler
task, or mailbox source. It returns `status: "not_run"` with exit code 2 until
all four declarations are present:

```powershell
$env:NARADA_E2E_SITE_LOOP_PRODUCTION = '1'
$env:NARADA_E2E_SITE_LOOP_SITE_ROOT = 'C:/Users/Andrey/Narada'
$env:NARADA_E2E_SITE_LOOP_TASK_NAME = '<declared Windows scheduled-task name>'
$env:NARADA_E2E_SITE_LOOP_CONTROLLED_MAILBOX_SOURCE = '<operator-approved controlled source ref>'
pnpm --filter @narada-core/site-loop-mcp test:e2e:production
```

An admitted production run must observe the real scheduled task, retire and
replace the resident carrier, complete a live controlled-mailbox proof through
the configured connector, and finish with fresh production/mailbox proof and
strict coherence. Missing or invalid prerequisites are `not_run`; an admitted
run that does not complete the contract is failed. The watchdog bounds the
gate at 150 seconds.

The production task-executability runbook and NARS host boundary are external
Narada contracts. Use the [cross-repository contract
register](../../docs/cross-repository-contracts.md#contract-register) for
source ownership and revision evidence.

## Verification

```powershell
pnpm --filter @narada-core/site-loop-mcp test
```
