# @narada-core/task-lifecycle-mcp

Task lifecycle MCP stdio runtime and tool dispatch surface.

This package exposes governed task lifecycle operations for Narada sites: task discovery, claiming, routing, evidence admission, finish/review/close flows, inbox bridge materialization, and selected test evidence capture.

## Boundary

- Allowed: mutate task lifecycle state through explicit lifecycle tools.
- Allowed: route, claim, continue, defer, reopen, review, finish, and close tasks under lifecycle rules.
- Allowed: admit task evidence and bridge inbox envelopes into task work.
- Not allowed: arbitrary filesystem, shell, Git, mailbox, or worker delegation behavior.
- Not allowed: bypassing lifecycle gates such as evidence admission, review findings disposition, or closure authority.

## Runtime

The stdio server binary is:

```text
task-lifecycle-mcp
```

It is launched against a site root and uses site-local task governance state and projections.

```powershell
pnpm --filter @narada-core/task-lifecycle-mcp build
node packages/task-lifecycle-mcp/dist/src/task-lifecycle/task-mcp-server.js --prepare --site-root <src-root>/site
node packages/task-lifecycle-mcp/dist/src/task-lifecycle/task-mcp-server.js --site-root <src-root>/site
```

Preparation is an explicit preflight. It creates/upgrades the site-local
`.ai/task-lifecycle.db`, prepares Task Lifecycle auxiliary tables, reconciles
legacy Markdown task specifications into the SQLite projection, prints one
JSON result, and exits. The normal runtime never performs SQLite preparation
or migration during startup or a tool call. See
`docs/task-lifecycle-preparation.md` for the readiness contract and recovery
steps.

Before preparation, protocol discovery plus `task_lifecycle_doctor`,
`task_lifecycle_restart`, `task_lifecycle_chapter_show`, guidance, and
payload/output transport helpers remain available. Stateful calls return a
structured `task_lifecycle_store_not_prepared:<reason>` error with the doctor,
explicit `--prepare`, and restart/reattach remediation sequence.

When `--site-root` is omitted, the runtime resolves its Site root in this
order: `NARADA_TASK_LIFECYCLE_ROOT`, `NARADA_SITE_ROOT`, then the process
working directory. `task_lifecycle_doctor` reports both the effective root and
the source that selected it; carrier/site wiring should pass an explicit
`--site-root` or one of these environment bindings rather than relying on a
developer-machine default.

Runtime reconfiguration is staged: the candidate site root and SQLite store
are prepared before `siteRoot`, `store`, and the configured flag are published.
If opening or publishing the candidate fails, the prior valid configuration
remains authoritative. Reconfiguration is refused while another request or
store transition is active. Store-error recovery may refresh from within the
owning request; its lease transfers to the replacement handle and is released
when that request finishes. Shutdown waits for request leases before closing
the store.

## Tool Groups

Inspection and workboard:

- `task_lifecycle_doctor` returns a concise startup-safe readiness summary by default; pass `verbose=true` or `detail=full` for the full diagnostic dump.
- `task_lifecycle_list`
- `task_lifecycle_show`
- `task_lifecycle_roster`
- `task_lifecycle_next`
- `task_lifecycle_workboard_snapshot`
- `task_lifecycle_obligations`
- `task_lifecycle_inspect`
- `task_lifecycle_audit`
- `task_lifecycle_search`
- `task_lifecycle_related`

Descriptive site-local tags:

- `task_lifecycle_tags_update` replaces the complete tag set and records an audit event with the before/after values, actor, reason, and timestamp.
- `task_lifecycle_list` accepts `tags` plus `tag_match: "any" | "all"`; `task_lifecycle_show` returns normalized `tags` and recent tag updates.
- Tags are trimmed, lowercased kebab-case labels, limited to 20 per task and 64 characters per label. They are discovery metadata only: they never authorize, route, prioritize, create dependencies, affect review, or close work.
- `task_lifecycle_related` prefers explicit tag overlap and falls back to derived title/goal/context terms for untagged or legacy tasks.
- `task_lifecycle_list` and `task_lifecycle_show` expose a compact
  `task_reference` containing the authoritative `task_id`, `task_number`, and
  human-readable `task_ref`; use that tuple when goal/chapter numbering differs
  from task projection filenames.

Assignment and routing:

- `task_lifecycle_roster_admit`
- `task_lifecycle_claim`
- `task_lifecycle_continue`
- `task_lifecycle_unclaim`
- `task_lifecycle_set_routing`

Evidence and closeout:

- `task_lifecycle_submit_work` performs ordinary notes, proof, admission, and finish. If an earlier call already admitted the same agent's report but a restart or later gate interrupted closeout, call it with `resume_existing_work: true`; existing substantive task sections, report evidence, and a previously satisfying outcome are reused, while proof and admission are not duplicated unless explicitly requested.
- `task_lifecycle_admit_evidence`
- `task_lifecycle_prove_criteria`
- `task_lifecycle_disposition_closeout`
- `task_lifecycle_finish`
- `task_lifecycle_close`
- `task_lifecycle_submit_observation`

Compatibility migration:

- `task_lifecycle_review` legacy review-call migration over dependency/outcome authority

Lifecycle mutation:

- `task_lifecycle_create`
- `task_lifecycle_defer`
- `task_lifecycle_reopen`

Inbox bridge:

- `task_lifecycle_bridge_poll`
- `task_lifecycle_inbox_target`

Verification helpers:

- `task_lifecycle_test_mcp_tool`
- `task_lifecycle_run_tests`

Transport helpers:

- payload tools from `@narada-core/mcp-transport`

## Payload Refs

Some tools accept `payload_ref` for large structured companion payloads. `task_lifecycle_create` requires an immutable payload ref carrying the task definition. `task_lifecycle_test_mcp_tool` also accepts a payload ref for the child call: keep `server_path`, `tool_name`, and `timeout_seconds` at the outer level, put required child fields in `arguments`, and the parent merges the immutable payload into those arguments with explicit `arguments` fields winning. Payload transport is generic and comes from `@narada-core/mcp-transport`; task lifecycle tools remain responsible for domain validation.

The special completion-truthfulness guard is activated only by structured fields, never by words found in titles, summaries, or task prose. Set `recovery_truthfulness_required: true` in a task creation payload to project the opt-in into task front matter, or supply an explicit structured trigger in a completion packet.

## Agent Guidance

Agents should call `task_lifecycle_next` for work selection, claim before doing task work when required, and use `task_lifecycle_finish` to submit work reports or complete outcome-contract tasks. Every ordinary finish now creates or reuses one review-contract dependency by default; callers may name `reviewer`, otherwise routing uses the first reviewer-capable roster agent and then the `reviewer` role. Reviewers claim that generated task and admit its outcome with `task_lifecycle_finish` using `outcome`, `summary`, and `findings`. `task_lifecycle_review` remains compatibility migration only for pre-existing tasks without a review dependency. Singleton reviewer sites need no boolean flag: same-operator review is annotated automatically, while multi-reviewer conflict policy still requires explicit authorization. Existing `single_operator_review` annotations retain their readback shape. Closeout and closure can be blocked by evidence, unsatisfied dependencies, conflict-policy gates, or undisposed blocking outcomes; do not treat a successful tool call as authority to skip those gates.

Every task-lifecycle tool description begins with a semantic canonical action
hint (for example, `submit work`, `show`, or `tags update`). Carrier projections
may still assign hashed transport aliases, but completion/help text remains
anchored to the canonical action name so read and mutation calls are not
visually interchangeable.

`task_lifecycle_test_mcp_tool` is the bounded one-shot recovery route when the
session-bound task-lifecycle server is stale or wedged. Its `server_path`
contract is explicit: relative paths resolve under the Site root; absolute
paths are admitted only under the Site root, the running
`@narada-core/task-lifecycle-mcp` package root, or roots configured through
`NARADA_TASK_LIFECYCLE_FRESH_SERVER_ALLOWED_ROOTS`. Only existing
`.js`, `.mjs`, and `.cjs` scripts are accepted. A one-shot result proves
the fresh child call, not that the carrier-bound process reloaded. Carrier-bound
sessions should discover `mcp_runtime_proxy_status` through `tools/list` and
inspect `runtime_freshness.reload_action` for the supervisor-owned restart.

A session-bound task-lifecycle child pins the build it loaded at process start.
Source or build fixes therefore require a real child restart by the carrier/runtime
supervisor; a one-shot fresh call does not update the bound child. A pending
`task_lifecycle_restart` marker is automatically acknowledged and cleared when
the replacement child starts with post-request self-observed boot evidence.
`acknowledge` and `clear` remain idempotent confirmation modes after that
automatic reconciliation, so manual marker deletion is never part of recovery.

Task projections carrying legacy `superseded/*`, `superseded-*`, or
`replacement-*` lineage labels are blocked at claim, continue, and finish
boundaries. Proceed only with an explicit
`authority_basis.kind=operator_direct_instruction` and a substantive summary
when the operator intentionally authorizes work on the superseded lineage.

## Task Executability Proof

Task Lifecycle is the authority for assessment requests, leases, attempts, admitted assessments, currency, and verdicts. Its cross-surface deterministic proof is run through Site Loop and covers lifecycle/recovery mechanics, including Windows SQLite ownership, no-NARS recovery, restart recovery, stale replacement, and strict dispatch enforcement. An `executable` verdict only says the task can be attempted in the declared environment; neither the verdict nor this proof establishes task correctness. The optional live provider check belongs to Worker Delegation. See Narada's `docs/operations/task-executability-e2e-and-recovery.md` runbook for commands and recovery boundaries.

The task-executability recovery runbook is maintained by Narada proper. Use the
[cross-repository contract register](../../docs/cross-repository-contracts.md#contract-register)
before treating an external runbook or authority result as current.

## Verification

```powershell
pnpm --filter @narada-core/task-lifecycle-mcp test
```
