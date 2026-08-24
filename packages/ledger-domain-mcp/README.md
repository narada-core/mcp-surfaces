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
epistemic-graph domain with its exact external contract (24 tools, response
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

## Canonical communication kind

Domain entity kinds are namespaced. Unqualified domain kinds are schema defects,
not synonyms. The epistemic descriptor accepts
`narada.epistemic:communication` as the only communication kind for new
writes. Writes using `communication` or `marici:communication` fail with
`legacy_communication_kind_write_refused`, including the canonical replacement
and remediation in the typed error.

Compatibility reads are descriptor-versioned. During contract version 2,
queries expand both legacy kinds, expose every returned entity as the canonical
kind, and report normalization metadata. Aliases never authorize writes.
Compatibility may be removed only after a ledger-head-bound audit of current
repositories and collaborating agents.

Migration starts with a bounded, cursor-paged preflight census. It then appends
an atomic `entity.kind_canonicalize` batch through the ordinary proposal and
admission machinery, preserving entity identity, payload, thread/read metadata,
and originating event provenance. Evidence mismatch or ambiguous identity stops
the batch with `communication_kind_canonicalization_collision`; completed
pages are idempotent. Fix rejected clients to emit the canonical kind rather
than retrying a legacy write.

Payload-backed proposal tools accept a lone immutable `payload_ref`. If content
validation rejects that revision, the graph has not mutated and the revision
must not be edited or retried indefinitely. Read it as provenance, create the
next unused revision with only the typed remediation applied, verify its digest
and byte size, and retry the original proposal tool with the successor ref. A
legacy-kind refusal on `mcp_payload:example@v1`, for example, recommends a
canonicalized `mcp_payload:example@v2` and returns this recovery as structured
error details.

## Tools

The tool set is descriptor-driven: `tools/list` is generated at runtime from
the loaded domain descriptor, so this engine has no static tool list of its
own. With the epistemic descriptor loaded, the surface exposes the 24
`epistemic_graph_*` tools (guidance, status, query/query_batch, message
read-marking, neighborhood, snapshot, export, source_inspect, the proposal
lifecycle, and sequences); exact names and schemas are discoverable through
MCP `tools/list` on the running surface.

For array-rich atomic contributions, `epistemic_graph_submit_review_admit`
accepts either the ordinary inline argument object or a lone immutable
`payload_ref` created by the Site payload surface. The native engine resolves
`mcp_payload:<id>@v<revision>` under the same Site root, verifies revision
metadata, canonical byte size, and SHA-256, and only then applies the unchanged
proposal/review/admission validation. Mixing inline fields with `payload_ref`,
recursive references, missing revisions, and integrity mismatches are typed
refusals; payload transport never becomes graph authority.

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
