# @narada-core/operator-console-overlay-mcp

Host-level dedicated MCP surface for the Narada Operator Console Windows overlay.

The surface owns the bounded MCP command boundary only. It delegates overlay lifecycle operations to the canonical Narada proper package at:

    packages/operator-console-overlay/dist/cli.js

For a local URL, the canonical overlay package first asks `@narada-core/operator-console-runtime` to prove or establish the Operator Router plus Console route. If readiness fails, no dead overlay is created and the returned diagnostics include the bounded failure reason and log/state paths. The MCP surface itself does not use structured-command MCP, launch a browser, or terminate arbitrary processes; runtime lifecycle remains owned by Narada proper.

## Tools

- operator_console_overlay_guidance
- operator_console_overlay_status
- operator_console_overlay_open
- operator_console_overlay_refresh
- operator_console_overlay_close

Set NARADA_ROOT when the Narada checkout is not at the host default. The surface validates that the canonical overlay entrypoint remains inside that root. The surface normalizes carrier environments that omit LOCALAPPDATA or have an incomplete PATHEXT; state defaults are under `%LOCALAPPDATA%\\Narada` (or the derived user-local AppData path) for the overlay, runtime, and router. Explicit NARADA_*_STATE_ROOT values still win.

Lifecycle commands accept an optional `timeout_ms` from 100 through 120000. The same bound covers lazy entrypoint materialization and the canonical command. When called through mcp-loader, place it inside the nested arguments so the loader can add its bounded grace period. Timeout diagnostics include bounded stdout/stderr, state roots, environment discovery, and process-tree cleanup results.

If the router state is malformed and its lock owner is not alive, Narada moves the corrupt `routes.json` and stale lock into a timestamped `recovery/corrupt-*` directory before retrying. A live or unreadable lock owner is never quarantined automatically.

The host overlay implementation is external to this MCP adapter. Use the
[cross-repository contract register](../../docs/cross-repository-contracts.md#contract-register)
for the owning implementation and revision evidence.

## Verification

    pnpm --filter @narada-core/operator-console-overlay-mcp test
