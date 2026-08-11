# `@narada-core/mcp-runtime-client`

## Verification

```powershell
pnpm --filter @narada-core/mcp-runtime-client test
```

Bounded production JSON-RPC client used by finite Scheduler and SOP workers to call Site-declared MCP surfaces through `mcp-loader`.

The client does not resolve entrypoints, interpret domain authority, or execute arbitrary commands. `mcp-loader` remains the Site fabric and child-lifecycle authority. The client starts one loader process, attaches exact declared `surface_id` values, forwards bounded tool calls, and closes all children when the worker pass ends.

Materialized tool results are read back through `mcp-loader` in validated pages. The client enforces a finite page count, nesting depth, per-call deadline, exact ref/offset/length continuity, and a configurable `maxMaterializedResultChars` ceiling (1,000,000 by default).

By default, `SiteFabricClient` launches the Rust mcp-loader artifact when it exists. Set `loaderImplementation: 'javascript'` (or provide the legacy `loaderEntrypoint`/`nodeExecutable` options) for the TypeScript compatibility path. `NARADA_RUNTIME_PROFILE=bun` and `NARADA_RUNTIME_PROFILE=node-compat` select that compatibility path; `NARADA_RUNTIME_PROFILE=native` refuses a missing native loader artifact.
