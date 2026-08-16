# Native MCP surfaces

This package owns the shared Rust stdio executable for MCP surfaces. It is
invoked with `--surface-id <id>` and keeps the MCP protocol boundary in Rust;
surface implementations are added as explicit modules rather than launching a
JavaScript runtime.

It also hosts the generic `epistemic-graph` surface. That surface keeps
immutable, hash-linked proposal-admission events beneath the Site's tracked
`.narada/epistemic/ledger` authority and rebuilds its SQLite query projection
beneath ignored `.narada/.ai/epistemic-graph` state.

The initial entity kinds are `problem`, `conjecture`, `claim`, `criticism`,
`test`, and `source`. Admission means a contribution satisfies structural and provenance
policy; it never means that a conjecture is true. External search remains
outside the surface. JSON-LD export maps scholarly and provenance concepts to
FaBiO, CiTO, and PROV-O without making those vocabularies storage authority.

The complete tool workflow, authority boundary, snapshot pagination contract,
and failure posture are documented in [docs/epistemic-graph.md](docs/epistemic-graph.md).

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

## Verification

```powershell
pnpm --filter @narada-core/mcp-surfaces-native test:native
```
