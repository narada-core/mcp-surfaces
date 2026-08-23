# `@narada-core/ledger-domain-epistemic`

## Verification

```powershell
pnpm --filter @narada-core/ledger-domain-epistemic test
```

This package is the static domain descriptor for the epistemic-graph surface:
`domain.json` implements [`narada.ledger-domain.v1`](../../../docs/ledger-domain-descriptor.md)
and is validated against the checked-in `domain.schema.json` by the package
test.

The descriptor is the declarative half of the ledger-domain split. It records
epistemic-graph's exact external contract as data: identity and schema
namespace, storage layout, entity/relation vocabulary, the five graph
operations with their validation and reference-binding rules, ID derivation
recipes, the disposable SQLite projection DDL and fold map, query projections,
numeric caps, the optional feature modules (proposals, sequences,
source_inspect, snapshot, export), the verbatim `narada.epistemic.guidance.v2`
text, and the 24 tool definitions whose input schemas are the engine's
generation target.

The descriptor declares `narada.epistemic:communication` as the sole writable
communication kind. Legacy `communication` and `marici:communication` values
are read-only compatibility aliases during contract version 2: results are
normalized and disclose alias expansion. The bounded preflight/migration tools
append provenance-preserving `entity.kind_canonicalize` events; they never
rewrite the ledger or projection stores. Compatibility reads remain until a
ledger-head-bound audit confirms that current repositories and collaborating
agents no longer depend on them.

Boundary: this package owns only static data. All behavior — descriptor
loading, the mutation pipeline, projection, and feature modules — lives in the
generic engine (`narada-ledger-domain`, Phase B) built on
`narada-mcp-event-ledger` ([`docs/event-ledger-format.md`](../../../docs/event-ledger-format.md)).
The descriptor must match the engine's published behavior and is changed only
together with it.
