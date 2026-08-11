# @narada-core/surface-feedback-mcp

## Verification

```powershell
pnpm --filter @narada-core/surface-feedback-mcp test
```

Cross-site MCP surface feedback intake and routing. Any site may submit feedback about any surface — bugs, improvements, gaps, observations.

## Purpose

Provides a single durable feedback channel for MCP surfaces. Agents across all Narada sites can report surface issues without needing to know which site owns the surface. SQLite-backed for durability.

## Tools

| Tool | Description |
|------|-------------|
| `surface_feedback_submit` | Submit feedback (surface, declared submitter site, principal, kind, summary, details) |
| `surface_feedback_convert_to_task` | Create and link one canonical feedback entry through task-lifecycle using server-bound User Site handoff authority; returns the next lifecycle action but does not execute the task |
| `surface_feedback_list` | List entries with an explicit read scope and bounded metadata filters |
| `surface_feedback_actionable_queue` | Read submitted and acknowledged (unprocessed) feedback with an explicit read scope; selection occurs before pagination and the response reports included statuses plus excluded-status counts |
| `surface_feedback_show` | Show one entry by ID within an explicit read scope |
| `surface_feedback_stats` | Aggregate visible entries within an explicit read scope |

## Read scopes

Every list, queue, show, and stats call must provide `scope` explicitly:

| Scope | Meaning | Required server posture |
|------|---------|-------------------------|
| `all_authorized` | Canonical cross-site feedback view | `feedback_root` must equal `canonical_feedback_root` and server-bound Site authority must be configured |
| `store_reconciliation` | Every row physically present in the canonical store, for existence and task-linkage reconciliation | Same canonical-store and server-authority requirements; read-only and does not broaden mutation authority |
| `authority_visible` | Entries whose declared submitter site matches the bound Site or that are attached to its owned surfaces | Server-bound Site authority |
| `owned_surfaces` | Entries for surfaces owned by the bound Site | Server-bound Site authority and owned surface IDs |
| `authority_site_submissions` | Entries whose declared `submitter_site_id` matches the bound Site | Server-bound Site authority |

`submitter_site_id_filter` is an optional metadata filter for list and queue. It does not authenticate the submitter, establish provenance, or expand authorization. The submitter site recorded in a feedback entry remains declarative submission metadata. Use canonical Site IDs for new submissions; generated server keys and session aliases are not Site IDs.

`store_reconciliation` is the explicit read route for verifying every row and task link in the canonical DB, including rows whose declared submitter Site and surface ownership are outside the bound authority's ordinary visibility. Results mark `submitter_site_id` as declared metadata rather than authenticated provenance. This scope is read-only: `surface_feedback_update_status` and its batch form remain limited to entries submitted by the bound Site or attached to its owned surfaces.

`all_authorized` is both the canonical cross-site read scope and the discovery scope for maintainer task handoff. When the server is bound to a User Site authority and the feedback root is canonical, `surface_feedback_convert_to_task` may hand off any entry in that store. List, queue, and show results expose `task_handoff_capability` so callers can see whether the canonical handoff is ready before mutating.

Scope names remain in `tools/list` for protocol stability, but availability is runtime state. Call `surface_feedback_guidance` or `surface_feedback_doctor` and inspect `capabilities.read_scopes[scope].available` before selecting a scope; unavailable scopes include a reason and remediation. Inspect `capabilities.task_handoff.available` before conversion. An unconfigured server therefore advertises the schema names without implying that `all_authorized` is callable.

The canonical User Site projection should pass `--feedback-root`, `--site-id`, and repeated `--owned-surface-id` arguments explicitly, and must configure the canonical store with `--canonical-feedback-root` or `NARADA_SURFACE_FEEDBACK_ROOT`. There is no machine-specific default canonical root. Do not rely on the current directory or an ambient caller-supplied site filter. A scoped show of an entry outside the scope returns `feedback_not_found` so existence is not disclosed.

## Repository/Site-local federation

At startup, a canonical server automatically materializes feedback from bounded local source stores. Registrar-generated `.narada/allowed-roots.json` is trusted only when it has the `narada.site.allowed_roots.v1` schema and `generated_by: mcp-registrar`; its Site root and `extra_allowed_roots` are discovery roots. Additional roots may be supplied with repeated `--feedback-discovery-root` arguments or `NARADA_FEEDBACK_DISCOVERY_ROOTS` (comma-separated or JSON array).

Each discovery root is checked only at the root itself and its immediate child directories, using the fixed store locations `.feedback/surface-feedback.db` and `.narada/feedback/.feedback/surface-feedback.db`. Source SQLite databases are opened read-only. The canonical database is the sole read/mutation authority; filesystem reachability does not grant cross-Site authority and no arbitrary recursive scan is performed.

Materialized entries expose `source.kind`, `source.db_path`, `source.source_updated_at`, and `source.sync_mode`. `surface_feedback_doctor` reports discovery roots, source sync state, invalid or missing sources, and conflicts. A canonical mutation prevents a later source refresh from overwriting the canonical row; inspect the conflict diagnostic and reconcile deliberately. The existing `surface_feedback_import` tool remains available for explicit, ID-scoped repair.

`surface_feedback_convert_to_task` is idempotent per feedback entry. It uses an isolated task-lifecycle stdio process and a durable handoff ledger. The ledger preserves payload and task references across failures, excludes concurrent conversion with a lease, and links feedback only after successful task creation. Retry the same conversion after a retryable failure; it resumes from the last durable stage.

Mutation authority and audit identity are server-bound. Configure the serving Site with `--site-id` or `NARADA_SITE_ID`, optionally set `NARADA_AGENT_ID` for the audit principal, and optionally repeat `--owned-surface-id` or set `NARADA_OWNED_SURFACE_IDS` for surfaces maintained by that Site. Without an explicit agent identity, the service principal is `surface-feedback@<site-id>`. Caller-supplied authority fields are rejected; legacy `resolved_by` fields are ignored. Canonical task handoff requires the canonical feedback root and a valid task-lifecycle Site root.

Task lifecycle root resolution is, in order: `--task-lifecycle-root`, `NARADA_TASK_LIFECYCLE_ROOT`, `NARADA_SITE_ROOT`, then the feedback root. The selected path must be a Site root containing `.ai`; `surface_feedback_doctor` reports static configuration validity separately from observed child health. Health starts `unverified`, becomes `healthy` after a valid lifecycle response, and becomes `unhealthy` after transport/startup failure. Prefer explicit configuration when feedback storage and task lifecycle belong to different roots.

`surface_feedback_show` includes first-class `audit_events` and the current `task_handoff`, including retry diagnostics and durable task linkage state.

## Kinds

- `bug` — something is broken
- `improvement` — enhancement request
- `gap` — missing capability
- `observation` — general observation

## Quick Start

```
pnpm --filter @narada-core/surface-feedback-mcp test
```
