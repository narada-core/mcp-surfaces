# Native MCP surfaces

This package owns the shared Rust stdio executable for MCP surfaces. It is
invoked with `--surface-id <id>` and keeps the MCP protocol boundary in Rust;
surface implementations are added as explicit modules rather than launching a
JavaScript runtime.

It also hosts the `surface-feedback` surface. The generic `epistemic-graph`
surface moved to the `ledger-domain-mcp` engine (`narada-ledger-domain`),
which loads the static domain descriptor in
`packages/shared/ledger-domain-epistemic` (`domain.json`). Its tracked
`.narada/epistemic/ledger` authority and disposable SQLite projection are
unchanged.

The ledger machinery behind `surface-feedback` (and the `ledger-domain-mcp`
engine) is the
shared Rust crate `narada-mcp-event-ledger`
(`packages/shared/event-ledger-native`), consumed as a Cargo path dependency;
its regime is specified in
[docs/event-ledger-format.md](../../../docs/event-ledger-format.md).

The `epistemic-graph` tool workflow, authority boundary, snapshot pagination
contract, and failure posture are documented in
[docs/epistemic-graph.md](docs/epistemic-graph.md); the descriptor format is
specified in
[docs/ledger-domain-descriptor.md](../../../docs/ledger-domain-descriptor.md).

## Boundary

The executable hosts only explicitly admitted Rust surface modules. It does not dynamically evaluate JavaScript or infer a surface implementation from a tool name.

Worker delegation keeps a fresh Rust NARS process for each run. Codex
subscription turns cross a capability-authenticated loopback broker owned by
the worker/delegated-task surface process; that broker owns one hidden Codex
app-server and creates a fresh ephemeral Codex thread for every provider turn.
It never reuses the outer carrier session or silently falls back to
`codex exec`. DeepSeek and OpenRouter bindings use NARS's native HTTP provider
adapter instead. `worker_policy_inspect` and each run's
`resolved_invocation` expose the selected transport and host generation.
For controlled parity diagnosis, the carrier process may set
`NARADA_WORKER_CODEX_TRANSPORT=codex-exec`; callers cannot override transport
through worker tool arguments.

## Verification

```powershell
pnpm --filter @narada-core/mcp-surfaces-native test:native
```
