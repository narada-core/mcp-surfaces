# MCP Runtime Observation

## Verification

```powershell
pnpm --filter @narada-core/mcp-runtime-observation test
```

This package emits sanitized, authority-bound MCP runtime ownership and lifecycle records to the canonical Site spool at `.narada/runtime/mcp-runtime-observer/sources/`.

Emission is mandatory for participating runtimes but deliberately best-effort: a missing or unwritable observation sink must never block a tool call or change control decisions. Records contain identities, lifecycle state, counters, and PIDs; they never contain tool arguments, results, environment values, or credentials.
