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
text, and the 21 tool definitions whose input schemas are the engine's
generation target.

Boundary: this package owns only static data. All behavior — descriptor
loading, the mutation pipeline, projection, and feature modules — lives in the
generic engine (`narada-ledger-domain`, Phase B) built on
`narada-mcp-event-ledger` ([`docs/event-ledger-format.md`](../../../docs/event-ledger-format.md)).
Until the engine cutover completes, the serving implementation remains the
Rust module in `packages/shared/mcp-surfaces-native`; the descriptor must
match that behavior byte-for-byte and is changed only together with it.
