# ledger-domain-mcp

`@narada-core/ledger-domain-mcp` is the generic ledger-domain engine. Its
native binary `narada-ledger-domain` loads one static
[`narada.ledger-domain.v1`](../../docs/ledger-domain-descriptor.md) domain
descriptor (`--domain <path>`) and serves the complete MCP surface that the
descriptor describes, rooted at `--site-root <path>` and built on the shared
`narada-mcp-event-ledger` crate.

This package is a host/engine package, not a bound surface: bound surfaces are
domain descriptors loaded by this engine. The first descriptor is
`@narada-core/ledger-domain-epistemic`
(`packages/shared/ledger-domain-epistemic/domain.json`), which re-hosts the
epistemic-graph domain with its exact external contract (21 tools, response
envelopes, error codes, and byte-compatible ledgers).

Boundary notes:

- The engine owns no domain behavior. Entity kinds, relations, operations,
  projections, caps, features, and guidance all come from the descriptor.
- Domains are static descriptor packages (data only); they contain no code.
- One process hosts exactly one domain; multiplexed multi-domain hosting is
  out of scope.

## Tools

The tool set is descriptor-driven: `tools/list` is generated at runtime from
the loaded domain descriptor, so this engine has no static tool list of its
own. With the epistemic descriptor loaded, the surface exposes the 21
`epistemic_graph_*` tools (guidance, status, query/query_batch, neighborhood,
snapshot, export, source_inspect, the proposal lifecycle, and sequences); exact
names and schemas are discoverable through MCP `tools/list` on the running
surface.

## Verification

```powershell
pnpm --filter @narada-core/ledger-domain-mcp test
```

This builds the native engine (`cargo build --release --locked`), publishes
the immutable artifact under `dist/native/`, and runs the protocol smoke test:
it spawns the built binary with the epistemic domain descriptor against a
temporary site root and exercises the real stdio handshake
(initialize/tools/list/status).

Deep engine behavior (mutation pipeline, proposals, sequences, byte-compat
golden fixtures) is covered by the cargo suite:

```powershell
cargo test --locked --manifest-path native/Cargo.toml
```
