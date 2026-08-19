# @narada-core/surface-feedback-mcp

## Verification

```powershell
pnpm --filter @narada-core/surface-feedback-mcp test
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml surface_feedback
```

Cross-site MCP surface feedback intake and routing. Any site may submit feedback about any surface — bugs, improvements, gaps, observations.

The default implementation is the native Rust `surface-feedback` applet in
`narada-mcp-surfaces`; Node and Bun are not carrier runtime dependencies.

## Purpose

Provides a single durable feedback channel for MCP surfaces. Agents across all Narada sites can report surface issues without needing to know which site owns the surface.

## Storage

The authoritative store is an append-only, hash-chained event ledger under `<feedback_root>/ledger/` (`narada.surface_feedback.event.v1` events on the shared `narada.event-ledger.v1` regime; see [docs/event-ledger-format.md](../../docs/event-ledger-format.md)). The SQLite database at `<feedback_root>/.ai/feedback/projection.sqlite` is a disposable fold projection rebuilt from the ledger on every read; deleting it only triggers a rebuild.

A legacy `<feedback_root>/.feedback/surface-feedback.db` store is migrated once automatically into the ledger — preserving original timestamps and feedback-event history, marked by `migration-complete.json` — and is never written again. The TypeScript implementation in this package is frozen legacy/rollback and reads only pre-migration state.

## Tools

| Tool | Description |
|------|-------------|
| `surface_feedback_submit` | Submit feedback; site/principal default from server authority, and an optional `idempotency_key` makes retries replay-safe with conflict refusal |
| `surface_feedback_convert_to_task` | Create and link one canonical feedback entry through task-lifecycle using server-bound User Site handoff authority; returns the next lifecycle action but does not execute the task |
| `surface_feedback_list` | List entries with an explicit read scope and bounded metadata filters |
| `surface_feedback_actionable_queue` | Read submitted and acknowledged (unprocessed) feedback with an explicit read scope; selection occurs before pagination and the response reports included statuses plus excluded-status counts |
| `surface_feedback_show` | Show one entry by ID within an explicit read scope |
| `surface_feedback_stats` | Aggregate visible entries within an explicit read scope |

## Read scopes

Every list, queue, show, and stats call must provide `scope` explicitly:

| Scope | Meaning | Required server posture |
|------|---------|-------------------------|
| `all_authorized` | Canonical local feedback-store view | Canonical feedback store (`feedback_global_read_requires_canonical_store` otherwise) |
| `store_reconciliation` | Every row physically present in the store, for existence and task-linkage reconciliation | Canonical feedback store; read-only and does not broaden mutation authority |
| `authority_visible` | Authority-filtered view of the bound Site's submissions | Server-bound Site authority |
| `owned_surfaces` | Surface-owner view over the bound authority's owned surfaces | Server-bound owned surfaces (`--owned-surface-id` or `NARADA_OWNED_SURFACE_IDS`) |
| `authority_site_submissions` | Submitter-Site view of the bound Site's submissions | Server-bound Site authority |

`submitter_site_id_filter` is an optional metadata filter for list and queue. It does not authenticate the submitter, establish provenance, or expand authorization. The submitter site recorded in a feedback entry remains declarative submission metadata. Use canonical Site IDs for new submissions; generated server keys and session aliases are not Site IDs.

`store_reconciliation` is the explicit read route for verifying every row and task link in the canonical DB, including rows whose declared submitter Site and surface ownership are outside the bound authority's ordinary visibility. Results mark `submitter_site_id` as declared metadata rather than authenticated provenance. This scope is read-only: `surface_feedback_update_status` and its batch form remain limited to entries submitted by the bound Site or attached to its owned surfaces.

`all_authorized` is the normal native discovery scope. Task conversion remains a separate mutation capability and requires both configured Site authority and a resolvable task-lifecycle authority adapter.

Scope names remain in `tools/list` for protocol stability, but availability is runtime state. Call `surface_feedback_guidance` or `surface_feedback_doctor` and inspect `capabilities.read_scopes[scope].available` before selecting a scope. Inspect `capabilities.mutations.task_handoff.available` before conversion.

The canonical User Site projection should pass `--feedback-root` and `--site-id` explicitly, and must configure the same store with `--canonical-feedback-root` or `NARADA_SURFACE_FEEDBACK_ROOT`. A server whose feedback root is that canonical root is the canonical store maintainer and may resolve every row in that store; noncanonical projections remain limited to their submitter Site and explicitly owned surfaces. There is no machine-specific default canonical root. Do not rely on the current directory or an ambient caller-supplied site filter. A scoped show of an entry outside the scope returns `feedback_not_found` so existence is not disclosed.

## Explicit store import

The native surface does not scan repositories or federate stores implicitly.
`surface_feedback_import` performs explicit, ID-scoped repair from a named
SQLite source. Its canonical contract requires the exact `source_db_path` and
one or more `feedback_ids`; root-based source resolution remains only as a
legacy transport compatibility form.

`surface_feedback_convert_to_task` delegates task creation to the configured
native task-lifecycle authority and records the returned task link on the
feedback entry. An existing task link makes a retry idempotent.

Mutation authority and audit identity are server-bound. Configure the serving Site with `--site-id` or `NARADA_SITE_ID`, optionally set `NARADA_AGENT_ID` for the audit principal, and optionally repeat `--owned-surface-id` or set `NARADA_OWNED_SURFACE_IDS` for surfaces maintained by that Site. Without an explicit agent identity, the service principal is `surface-feedback@<site-id>`. Caller-supplied authority fields are rejected; legacy `resolved_by` fields are ignored. Canonical task handoff requires the canonical feedback root and a valid task-lifecycle Site root.

Task lifecycle root resolution is `NARADA_TASK_LIFECYCLE_ROOT`, then
`NARADA_SITE_ROOT`, then the feedback root. Prefer explicit configuration when
feedback storage and task lifecycle belong to different roots.

## Kinds

- `bug` — something is broken
- `improvement` — enhancement request
- `gap` — missing capability
- `observation` — general observation

## Quick Start

```
cargo native-release
```
