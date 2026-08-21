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
epistemic-graph domain with its exact external contract (22 tools, response
envelopes, error codes, and byte-compatible ledgers).

Boundary notes:

- The engine owns no domain behavior. Entity kinds, relations, operations,
  projections, caps, features, and guidance all come from the descriptor.
- Domains are static descriptor packages (data only); they contain no code.
- One process hosts exactly one domain; multiplexed multi-domain hosting is
  out of scope.

The query tool has three explicit modes: legacy filters (`kind`, `text`, and
similar), a descriptor-owned named template, or raw normalized-datom Datalog.
Named filters require `template`; raw `query` and `template` cannot be combined,
and raw Datalog cannot silently accept legacy or named filters. Legacy queries use
offset pagination; cursors belong to raw or named query modes. The epistemic
inbox uses canonical `participant`, with `recipient` retained as a compatibility
alias; `direction: "outgoing"` selects the sender side and `to` selects the
target. The published schemas expose the mode fields; the engine enforces
mutual exclusion and reports missing inbox participant/recipient (including
through `match`) or thread root as domain diagnostics. Page responses
identify `count` as the returned-page count and expose
an opaque, domain-namespaced cursor. `query_batch` accepts the same legacy,
named, and raw forms: legacy items stay compact by default, while named/raw items
preserve hydrated pulls. Batch response `query_batch.v2` has one flat page shape
per item (`items`, `count`, `has_more`, and `next_cursor`) and does not duplicate
that page under a nested `result`; each item also includes a bounded request
summary for correlation. Batch limits and total response bytes are bounded;
nested predicates share the raw clause budget, and raw `one_of` plus named/legacy
kind alias expansion share the descriptor's alternative-value cap. Malformed
nested query shapes and typed named filters are refused.
Raw pulls hydrate entities, relations, and durable records; normalized records
advertise `narada.ledger:record/id` and `narada.ledger:record/kind` datoms.
The raw shape is a `find` item such as
`{"pull":{"var":"?object","fields":["*"]}}`; `*` means all canonical
fields, while a field list keeps the response narrow. Use pull `target_kind`
(`entity`, `relation`, or `record`) when an id can exist in more than one
projection; untyped collisions are refused.
The evaluator plans ordinary triple, comparison, and reachability clauses after
their dependencies are available, including inside correlated nested predicates.
Derived
message/reply state is available under `_narada_query` without overwriting
domain payload fields.

## Tools

The tool set is descriptor-driven: `tools/list` is generated at runtime from
the loaded domain descriptor, so this engine has no static tool list of its
own. With the epistemic descriptor loaded, the surface exposes the 22
`epistemic_graph_*` tools (guidance, status, query/query_batch, message
read-marking, neighborhood, snapshot, export, source_inspect, the proposal
lifecycle, and sequences); exact names and schemas are discoverable through
MCP `tools/list` on the running surface.

## Verification

```powershell
pnpm --filter @narada-core/ledger-domain-mcp test
```

This builds the native engine (`cargo build --release --locked`), publishes
the immutable artifact under `dist/native/`, and runs the protocol smoke test:
it spawns the built binary with the epistemic domain descriptor against a
temporary site root and exercises the real stdio handshake
(initialize/tools/list/status). The same command also runs the native child-
process protocol suite, including query/template, cursor, projection,
tamper, concurrency, sequence, and modern Content-Length coverage.

Deep engine behavior (mutation pipeline, proposals, sequences, byte-compat
golden fixtures) is covered by the cargo suite:

```powershell
cargo test --locked --manifest-path native/Cargo.toml
```

The repository alias for that native suite is:

```powershell
pnpm test:ledger-domain:native
```
