# Work Lifecycle MCP

## Tools

- `work_lifecycle_doctor` - report storage and authority readiness.
- `ticket_list`, `ticket_show`, `ticket_sources_list`, and
  `ticket_processing_context_load` - inspect and prepare ticket inputs.
- `ticket_admit_source`, `ticket_admit_proposal`, and the ticket receipt and
  disposition tools - record governed lifecycle transitions.
- `work_outbox_list`, `work_outbox_consumer_register`, `work_outbox_ack`,
  `work_outbox_compact`, and `work_lifecycle_storage_inspect` - inspect and
  maintain the durable work outbox.
- The existing `task_lifecycle_*` family remains available from this runtime.

## Verification

```powershell
pnpm --filter @narada-core/work-lifecycle-mcp test
```

Work Lifecycle is the single Site-scoped mutation authority for sibling ticket
and task aggregates. It exposes first-class `ticket_*` tools and the existing
`task_lifecycle_*` family from one runtime and one SQLite database.

Runtime startup is preparation-free. Prepare explicitly:

```powershell
node dist/src/main.js --prepare --site-root C:\path\to\site
```

Then start the MCP runtime with `--site-root`. The canonical database is
`.ai/work-lifecycle.db`; no legacy task database is opened as fallback.
