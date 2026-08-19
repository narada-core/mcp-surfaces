# Ledger Domain Descriptor — `narada.ledger-domain.v1`

This document defines the declarative domain descriptor consumed by the
generic ledger-domain engine (`narada-ledger-domain`, Phase B). One descriptor
describes one complete event-ledger MCP surface: identity, storage layout,
vocabulary, operations, ID derivation, projection, query behavior, numeric
caps, optional feature modules, guidance text, and the full tool list.

Every field in this schema exists because it traces to actual, current
behavior of the epistemic-graph surface
(`packages/shared/mcp-surfaces-native/native/src/epistemic_graph.rs`). Nothing
is invented for hypothetical domains. The reference descriptor is
`packages/shared/ledger-domain-epistemic/domain.json`; epistemic-graph's
external contract (tool names and schemas, response envelopes, error codes,
storage layout, ledger bytes) must be reproduced exactly by an engine loading
that descriptor.

A descriptor is a single JSON object with `"schema": "narada.ledger-domain.v1"`
and the top-level sections below. The machine-readable schema is
`packages/shared/ledger-domain-epistemic/domain.schema.json`.

## `identity`

| Field | Type | Description |
| --- | --- | --- |
| `domain_id` | string | Stable domain identifier, e.g. `epistemic-graph`. |
| `tool_prefix` | string | Prefix of every MCP tool name, e.g. `epistemic_graph` (tools are `<tool_prefix>_<verb>`). |
| `schema_namespace` | string | Dotted schema-id namespace, e.g. `narada.epistemic`. All artifact/response schema ids are `<schema_namespace>.<name>`. |
| `error_schema_id` | string | Refusal-envelope schema id, e.g. `narada.epistemic.error.v1`. Errors are `{schema, code, message, details}` (see `docs/event-ledger-format.md`). |
| `implementation` | string | Label reported by the status tool, e.g. `rust-native`. |

## `storage`

| Field | Type | Description |
| --- | --- | --- |
| `control_root_subdir` | string | Authority directory under the site control root, e.g. `epistemic`. The control root is the site root itself when its basename is `.narada`, otherwise `<site_root>/.narada`. |
| `runtime_subdir` | string | Disposable runtime directory under the control root, e.g. `.ai/epistemic-graph`. Holds `projection.sqlite` (+ `.next` rebuild scratch) and `locks/`. |
| `ledger_file_prefix` | string | Event file prefix, e.g. `ev` (event ids are `<prefix>-<sequence:012>-<uuid v4>`). |
| `event_schema_id` | string | Schema id stamped on admitted ledger events, e.g. `narada.epistemic.event.v1`. |
| `event_hash_field` | string | Hash-chain field name on ledger events, e.g. `event_hash`. |
| `subdirs.ledger` | string | Ledger subdirectory under the control subdir (`ledger`). |
| `subdirs.proposals` | string | Proposal store subdirectory (`proposals`). |
| `subdirs.sequences` | string | Sequence store subdirectory (`sequences`). |

Resulting layout for epistemic-graph:

```
<control>/epistemic/ledger/        # authoritative hash-chained events + idem-*.txt markers
<control>/epistemic/proposals/     # immutable proposals, *.review.json, *.rejection.json, idem-*.txt
<control>/epistemic/sequences/     # per-sequence manifest, claims/, idempotency/
<control>/.ai/epistemic-graph/     # projection.sqlite, projection.sqlite.next, locks/
```

## `entities`

| Field | Type | Description |
| --- | --- | --- |
| `core_kinds` | string[] | Core entity kinds: `problem`, `conjecture`, `claim`, `criticism`, `test`, `source`. |
| `required_fields.always` | string[] | Fields every `entity.declare` must carry beyond `op`: `kind`, `title`. |
| `required_fields.conditional` | object[] | Conditional requirements. One entry: `{ "when_kind": "source", "requires": ["version", "locator"] }` — a `source` entity must carry `version` and `locator`. Applies only to the `source` kind, never to extension kinds. |
| `extension_rule` | object | Extension-kind rule: any kind outside `core_kinds` must be namespaced (must contain `:`), e.g. `cintamani:experiment`. Violations refuse with `invalid_entity_kind`. Extension kinds carry their full structured record in additional payload fields. |

## `relations`

| Field | Type | Description |
| --- | --- | --- |
| `core` | string[] | Core relations: `addresses`, `criticizes`, `tests`, `depends_on`, `derived_from`, `transforms`, `supersedes`. |
| `extension_pattern` | string (regex) | Schema-level extension relation pattern: `^[A-Za-z][A-Za-z0-9_.-]*:[A-Za-z][A-Za-z0-9_.-]*$`, e.g. `marici:refines`. |
| `extension_rule` | string | Validation-level rule: any relation outside `core` must be namespaced (must contain `:`); violations refuse with `invalid_relation_type`. |

## `operations`

Five operation kinds: `entity.declare`, `relation.declare`,
`assessment.record`, `test_outcome.record`, `sweep.record`.

Per-operation required fields (post-normalization validation; unknown fields
are allowed on every operation — `additionalProperties: true`):

| Operation | Required fields | Conditional |
| --- | --- | --- |
| `entity.declare` | `op`, `entity_id`, `kind`, `title` | `kind == "source"` → also `version`, `locator`. `entity_id` is derived during normalization when omitted (see `id_derivation`). |
| `relation.declare` | `op`, `relation_id`, `relation_type`, `source_id`, `target_id` | `relation_id` derived when omitted. `source_id`/`target_id` may be supplied indirectly as `source_ref`/`target_ref` and wired during normalization; the tool schema requires `source_id` or `source_ref`, and `target_id` or `target_ref`. |
| `assessment.record` | `op`, `assessment_id`, `subject_id`, `judgment`, `actor`, `reason` | `evidence` required (see below). |
| `test_outcome.record` | `op`, `outcome_id`, `test_id`, `actor`, `outcome` | `evidence` required (see below). |
| `sweep.record` | `op`, `sweep_id`, `interval_start`, `interval_end`, `method`, `result` | — |

| Field | Value | Description |
| --- | --- | --- |
| `additional_properties` | `true` | Operations may carry arbitrary extra fields (extension payload). |
| `evidence_entry` | object | Evidence entries are exactly `{source_id, locator, paraphrase}`, all non-empty strings, all required, `additionalProperties: false`. |
| `evidence_required_at_review` | string[] | `["assessment.record", "test_outcome.record"]`. The tool input schema also lists `evidence` (minItems 1) as required for these two operations; the validation layer independently refuses empty/missing evidence at review with `evidence_required`. |
| `reference_bindings` | object[] | Dangling-reference checks: `relation.declare` → `source_id`, `target_id`; `assessment.record` → `subject_id`; `test_outcome.record` → `test_id`; every operation's `evidence[]` → `source_id`. Evidence `locator` and `paraphrase` must additionally be non-blank at review (`evidence_location_incomplete`). Unknown references refuse with `dangling_reference`. |
| `reference_resolution_scope` | string[] | `["projection_entities", "intra_proposal_entity_declares"]` — references resolve against the current projection's entity set plus entities declared earlier in the same proposal. |

The full `oneOf` operation schema (embedded verbatim in proposal tool input
schemas) is recorded in the descriptor at `operations.schema`.

## `id_derivation`

All digests follow the shared convention (`docs/event-ledger-format.md`):
hex SHA-256 of the compact `serde_json::to_vec` encoding — insertion-order
dependent by design. `safe_name` keeps ASCII alphanumerics, `-`, `_`, bounded
to 120 characters.

| Recipe | Value |
| --- | --- |
| `entity` | When `entity_id` is absent and `kind`/`title` are non-empty: `{safe_name(kind)}:{digest[..20]}` where the digest is over `{"kind": kind, "title": title, "version": version-or-null, "locator": locator-or-null}` (`version`/`locator` are JSON `null` for non-source kinds). |
| `relation` | When `relation_id` is absent and `relation_type`/`source_id`/`target_id` are non-empty: `rel:{safe_name(relation_type)}-{sha256[..16]}` over the raw bytes `{relation_type}\0{source_id}\0{target_id}`. |
| `local_ref_wiring` | An `entity.declare` may carry `local_ref`; it must be unique within a proposal (`duplicate_local_ref`). A `relation.declare` with `source_ref`/`target_ref` (and no `source_id`/`target_id`) resolves them through the proposal's local-ref map; unresolved references refuse with `local_ref_not_found`. |
| `operation_identity_prefixes` | `entity.declare` → `entity:{entity_id}`; `relation.declare` → `relation:{relation_id}`; `assessment.record` → `assessment:{assessment_id}`; `test_outcome.record` → `test_outcome:{outcome_id}`; `sweep.record` → `sweep:{sweep_id}`. Used for resubmission drop ids. |
| `derived_idempotency_keys` | Proposal: `auto-proposal-{sha256[..24]}` over `{"actor", "authority_basis", "operations"}` (post-normalization). Admission: `auto-admission-{sha256[..24]}` over `{"proposal_id", "proposal_digest"}`. Applied only when the caller omits `idempotency_key`. |
| `generated_ids` | `proposal_id` = `ep_{uuid v4}`; `sequence_id` = `seq-{sha256(sequence_name)[..24]}`; `claim_id` = `seqclaim-{sha256("{sequence_name}\0{idempotency_key}")[..24]}`. |

## `projection`

Disposable SQLite fold projection, rebuilt from the ledger on every read path.
The exact DDL:

```sql
pragma journal_mode=delete;
create table entities(entity_id text primary key,kind text not null,payload_json text not null,event_id text not null);
create table relations(relation_id text primary key,relation_type text not null,source_id text not null,target_id text not null,payload_json text not null,event_id text not null);
create table records(record_id text primary key,record_kind text not null,payload_json text not null,event_id text not null);
```

Op → (table, key-field) fold map (`insert or replace`; `payload_json` is the
full operation object; `event_id` is the admitting event):

| Operation | Table | Key field | Extra columns |
| --- | --- | --- | --- |
| `entity.declare` | `entities` | `entity_id` | `kind` ← op `kind` |
| `relation.declare` | `relations` | `relation_id` | `relation_type`, `source_id`, `target_id` ← op fields |
| `assessment.record` | `records` | `assessment_id` | `record_kind` ← op name |
| `test_outcome.record` | `records` | `outcome_id` | `record_kind` ← op name |
| `sweep.record` | `records` | `sweep_id` | `record_kind` ← op name |

## `query`

| Field | Value |
| --- | --- |
| `record_kind_enum` | `["assessment.record", "test_outcome.record", "sweep.record"]` — setting `record_kind` switches a query from entities to durable records. |
| `entity_compact_projection` | `{entity_id, kind, title, event_id}` (`title` from payload). Full form replaces `title` with `payload`. |
| `record_compact_projection` | `{record_id, record_kind, subject_id, judgment, status, event_id}` (`subject_id`/`judgment`/`status` from payload). Full form replaces them with `payload`. |
| `text_filter` | Case-sensitive `payload_json LIKE '%text%'` substring filter on both entity and record queries. |
| `neighborhood_relation_fields` | Relations where `source_id = entity_id OR target_id = entity_id`; emitted as `{relation_id, relation_type, source_id, target_id, payload}`. |
| `neighborhood_record_fields` | Records where payload `subject_id` or `test_id` equals `entity_id`; emitted as `{record_id, record_kind, payload, event_id}`. |

## `caps`

All numeric bounds (descriptor values are exact current behavior):

| Cap | Value | Default | Notes |
| --- | --- | --- | --- |
| `operations_per_proposal` | 1–200 | — | Violations refuse with `invalid_proposal`. |
| `query_limit` | 1–100 | 50 | Also bounds `proposal_read` (default 20) and `neighborhood` (default 50). |
| `query_batch` | 1–20 queries | — | `invalid_batch_query` outside. |
| `query_batch_limit_per_query` | 1–20 | 5 | |
| `neighborhood_limit` | 1–100 | 50 | |
| `snapshot_limit` | 1–1000 | 500 | Independent entity/relation offsets. |
| `export_entities` | 100 | — | Via query at limit 100. |
| `export_relations` | 1000 | — | |
| `export_records` | 1000 | — | |
| `source_inspect_paths` | 1–20 | — | `invalid_source_inspection` outside. |
| `source_inspect_sections_per_file` | 1–50 | 20 | |
| `source_inspect_chars_per_section` | 100–4000 | 1200 | |
| `source_inspect_file_bytes` | 1 048 576 (1 MiB) | — | `source_too_large`. |
| `sequence_name_chars` | 1–120 | — | Non-control characters, no surrounding whitespace (`sequence_name_invalid`). |
| `sequence_page_limit` | 1–100 | 100 | Sequence list/claims pagination. |
| `authority_basis_properties` | 1–32 | — | `maxProperties: 32` applies to the sequence tool schemas; proposal schemas require `minProperties: 1` with no max. |
| `authority_basis_bytes` | 8192 | — | Encoded-size bound enforced on `sequence_create`, `sequence_claim_next`, and `proposal_admit` (`argument_too_large`). |
| `resubmit_drop_operation_ids` | 0–200, unique | — | Resulting operations must be 1–200 (`invalid_proposal_resubmission`). |
| `resubmit_replacements` | 0–200 | — | |
| `capture_sources` | 1–100 sources, ≤199 operations, ≤200 combined | — | `invalid_capture` outside. |
| `actor_chars` | 256 | — | `maxLength` on sequence tool `actor`. |
| `idempotency_key_chars` | 1–256 | — | Sequence tool schemas; proposal schemas require `minLength: 1` with no max. |
| `guidance_routing_hint_chars` | 256 | — | `maxLength` on `epistemic_graph_guidance` `workflow`/`tool`. |

## `features`

Optional descriptor-activated feature modules. Epistemic-graph activates all
five.

### `proposals`

Immutable atomic proposals with preserved review gate and head-CAS admission.

| Field | Value |
| --- | --- |
| `proposal_schema_id` | `narada.epistemic.proposal.v1` (stored proposal) |
| `submission_receipt_schema_id` | `narada.epistemic.proposal_submission.v1` |
| `review_schema_id` | `narada.epistemic.proposal_review.v1` |
| `admission_receipt_schema_id` | `narada.epistemic.proposal_admission.v1` |
| `rejection_schema_id` | `narada.epistemic.proposal_rejection.v1` |
| `resubmission_schema_id` | `narada.epistemic.proposal_resubmission.v1` |
| `source_capture_schema_id` | `narada.epistemic.source_capture.v1` |
| `compound_schema_id` | `narada.epistemic.submit_review_admit.v1` |
| `read_schema_id` | `narada.epistemic.proposal_read.v1` |
| `event_kind` | `proposal_admitted` (value of `event_kind` on ledger events) |
| `compound` | `true` — `submit_review_admit` performs submit → review → admit while preserving the immutable proposal and the review gate (`review_gate_preserved: true`, `certifies_truth: false`). |
| `capture_sources` | Sources declare through the dedicated `sources` field (never as `kind: "source"` operations); the tool reports `existing_identities` (op → identity already present in the projection) before review and admission; admission always requires an explicit call. |
| `resubmit` | New immutable proposal from an earlier one: drop operations by operation identity (`<prefix>:<id>`, unique, ≤200) and append replacements (≤200); dropped ids must exist in the source proposal (`proposal_operation_not_found`). |

### `sequences`

Site-owned immutable numeric authorities under
`<control>/epistemic/sequences/<sha256(name)>/`.

| Field | Value |
| --- | --- |
| `manifest_schema_id` | `narada.epistemic.sequence.v1` |
| `claim_schema_id` | `narada.epistemic.sequence.claim.v1` |
| `claim_receipt_schema_id` | `narada.epistemic.sequence.claim.receipt.v1` |
| `status_schema_id` | `narada.epistemic.sequence.status.v1` |
| `list_schema_id` | `narada.epistemic.sequence.list.v1` |
| `claims_schema_id` | `narada.epistemic.sequence.claims.v1` |
| `idempotency_schema_id` | `narada.epistemic.sequence.idempotency.v1` |
| `step` | `1` (fixed; manifests with any other step are invalid) |
| `start_at` | ≥ 1 (`sequence_start_invalid` otherwise); re-creating with a different `start_at` refuses with `sequence_configuration_conflict` |
| `claim_file_pattern` | `claims/claim-{value:020}.json` |
| `chain` | Manifest carries `creation_hash`; claims form a hash chain via `previous_claim_hash`/`claim_hash`, contiguous from `start_at`; idempotent replay recovers the disposable `idempotency/` index from canonical history. |

### `source_inspect`

Bounded Markdown section extraction from site-local source files.

| Field | Value |
| --- | --- |
| `keywords` | `record`, `status`, `epistemic boundary`, `decision`, `verdict`, `scope`, `next`, `subsequent`, `forward`, `correction`, `update` (heading match is case-insensitive substring) |
| `response_schema_id` | `narada.epistemic.source_inspection.v1` |
| `containment` | Paths must canonicalize inside the site root (`source_outside_site`); caps per `caps`. |

### `snapshot`

| Field | Value |
| --- | --- |
| `response_schema_id` | `narada.epistemic.graph_snapshot.v1` |
| `stability_retries` | `3` — rebuild the projection until the ledger head is unchanged across a rebuild; repeated change refuses with `ledger_snapshot_unstable`. |
| `expected_head_check` | A supplied `expected_ledger_head` that no longer matches refuses with `ledger_head_mismatch`. |

### `export`

| Field | Value |
| --- | --- |
| `formats` | `json` (default), `jsonld` |
| `response_schema_id` | `narada.epistemic.export.v1` |
| `jsonld_context` | `{"prov": "http://www.w3.org/ns/prov#", "cito": "http://purl.org/spar/cito/", "fabio": "http://purl.org/spar/fabio/", "narada": "https://narada.local/epistemic/"}` (emitted only for `jsonld`, else `@context: null`) |

## `guidance`

Static model-facing guidance emitted by the `<tool_prefix>_guidance` tool as
`narada.epistemic.guidance.v2`. The following fields are static text owned by
the descriptor and must be emitted verbatim (JSON, key order preserved):

```json
{
  "schema": "narada.epistemic.guidance.v2",
  "purpose": "Preserve evolving problem situations, not certify truth.",
  "workflow": [
    {"step": 1, "tool": "epistemic_graph_submit_review_admit", "preferred": true, "why": "Perform the ordinary submit, preserved policy review, and admission workflow atomically. Omit expected_ledger_head to snapshot the current head and omit idempotency_key for deterministic retry safety."},
    {"step": 2, "tool": "epistemic_graph_capture_sources", "alternative": true, "why": "Create a reviewable source proposal when manual review before admission is intended; operations may be empty for pure source capture."},
    {"step": 3, "tool": "epistemic_graph_proposal_submit", "alternative": true, "why": "Persist a reviewable proposal without source batching."},
    {"step": 4, "tools": ["epistemic_graph_proposal_review", "epistemic_graph_proposal_admit"], "manual_only": true, "why": "Use separate calls only when the operator wants an explicit pause between proposal, review, and admission."},
    {"step": 5, "tool": "epistemic_graph_neighborhood", "why": "Verify the admitted problem situation and its relations."}
  ],
  "sequence_workflow": [
    {"step": 1, "tool": "epistemic_graph_sequence_create", "why": "Create a named Site-owned numeric authority once; start_at defaults to 1."},
    {"step": 2, "tool": "epistemic_graph_sequence_claim_next", "why": "Claim one permanent number using a unique idempotency key for the claim intent."},
    {"step": 3, "tools": ["epistemic_graph_sequence_status", "epistemic_graph_sequence_claims"], "why": "Verify current allocation state or audit bounded immutable claims."}
  ],
  "sequence_semantics": {
    "authority": "Separate immutable coordination records under .narada/epistemic/sequences; not epistemic assertions or graph events.",
    "claim": "Permanent, monotonic, increment-by-one, never released or reused.",
    "formatting": "The authority returns unsigned integers; callers own prefixes, padding, and display formatting."
  },
  "extension_relation_rule": "Any relation outside core_relations must be namespaced, for example marici:refines or marici:generalizes.",
  "extension_entity_kind_rule": "Any entity kind outside entity_kinds must be namespaced, for example cintamani:experiment or cintamani:equipment_type. Extension kinds carry their full structured record in additional payload fields; the version/locator requirement applies only to the source kind.",
  "identity_rule": {
    "relations": "Omit relation_id to derive it deterministically from relation_type, source_id, and target_id. Supply an override only when parallel duplicate relations are intentional.",
    "idempotency": "Omit idempotency_key for deterministic content-hash retry identity; supply one only to name a wider caller-defined retry scope."
  },
  "revision_pattern": {
    "entity_title_correction": "Declare a successor entity with the corrected title and connect it to the prior entity using supersedes. Keep the prior identity as immutable history.",
    "discovery": "Query or inspect the predecessor neighborhood before declaring the successor.",
    "reason": "The graph is append-only; revision is explicit explanation, not silent record mutation."
  },
  "provenance_choices": [
    "Represent a document as a versioned source entity and connect claims with derived_from.",
    "For an assessment or test outcome, include evidence entries with source_id, locator, and paraphrase.",
    "Do not manufacture an assessment merely to attach provenance; conjecture plus derived_from is valid."
  ],
  "minimal_example": {
    "tool": "epistemic_graph_submit_review_admit",
    "arguments": {
      "actor": "agent-id",
      "authority_basis": {"kind": "operator_request", "summary": "Capture one bounded source claim."},
      "operations": [
        {"op": "entity.declare", "local_ref": "source", "kind": "source", "title": "Example source", "version": "1", "locator": "src/ledger/example.md"},
        {"op": "entity.declare", "local_ref": "conjecture", "kind": "conjecture", "title": "Example explanatory conjecture"},
        {"op": "relation.declare", "relation_type": "derived_from", "source_ref": "conjecture", "target_ref": "source"}
      ]
    }
  },
  "concurrency_rule": "Omit expected_ledger_head to snapshot the live head during submission while retaining CAS protection through admission. Supply a concrete status.ledger_head only when an external read must be the concurrency boundary. If review reports stale, query again and submit a new proposal; do not rewrite the immutable proposal.",
  "admission_meaning": "policy-valid contribution; never truth certification",
  "search_boundary": "Use external providers for discovery. Record a sweep only when it explains coverage or changes the graph.",
  "problem_policy": "Transform apparent solutions into successor problems; record closure only as an attributed assessment."
}
```

Five guidance fields are **vocabulary-derived** and assembled by the engine
from the descriptor's vocabulary sections rather than stored as static text:
`entity_kinds` (from `entities.core_kinds`), `core_relations` (from
`relations.core`), `extension_relation_rule` (templated from the relation
extension rule), `extension_entity_kind_rule` (templated from the entity
extension rule), and `operation_kinds` (from `operations.kinds`, emitted
between `identity_rule` and `provenance_choices`). The emitted guidance object
also carries a `requested` echo of the caller's `workflow`/`tool` routing
hints.

## `tools`

The descriptor enumerates every MCP tool with its exact input schema; the
engine generates `tools/list` from this section, so it must match the current
surface byte-for-byte. Annotations: `readOnlyHint` and `idempotentHint` are
`true` for read tools and `false` for mutating tools; `destructiveHint` is
always `false`.

Classification: `guidance` (the `_guidance` tool), `core` (status, query,
neighborhood, and the primitive proposal lifecycle), `feature` (tools owned by
a feature module, with the module named).

| # | Tool | Class | Read-only | Feature |
| --- | --- | --- | --- | --- |
| 1 | `epistemic_graph_guidance` | guidance | yes | — |
| 2 | `epistemic_graph_status` | core | yes | — |
| 3 | `epistemic_graph_query` | core | yes | — |
| 4 | `epistemic_graph_query_batch` | core | yes | — |
| 5 | `epistemic_graph_neighborhood` | core | yes | — |
| 6 | `epistemic_graph_proposal_submit` | core | no | proposals |
| 7 | `epistemic_graph_proposal_read` | core | yes | proposals |
| 8 | `epistemic_graph_proposal_review` | core | no | proposals |
| 9 | `epistemic_graph_proposal_admit` | core | no | proposals |
| 10 | `epistemic_graph_proposal_reject` | core | no | proposals |
| 11 | `epistemic_graph_proposal_resubmit` | core | no | proposals |
| 12 | `epistemic_graph_submit_review_admit` | feature | no | proposals |
| 13 | `epistemic_graph_capture_sources` | feature | no | proposals |
| 14 | `epistemic_graph_sequence_create` | feature | no | sequences |
| 15 | `epistemic_graph_sequence_status` | feature | yes | sequences |
| 16 | `epistemic_graph_sequence_list` | feature | yes | sequences |
| 17 | `epistemic_graph_sequence_claim_next` | feature | no | sequences |
| 18 | `epistemic_graph_sequence_claims` | feature | yes | sequences |
| 19 | `epistemic_graph_source_inspect` | feature | yes | source_inspect |
| 20 | `epistemic_graph_snapshot` | feature | yes | snapshot |
| 21 | `epistemic_graph_export` | feature | yes | export |

The exact `inputSchema` JSON for each tool is recorded in
`packages/shared/ledger-domain-epistemic/domain.json` (`tools[].inputSchema`)
and is normative; it is transcribed verbatim from `list_tools()` and the
schema builders in `epistemic_graph.rs`. Notes on schema details that matter
for byte-compat:

- `epistemic_graph_status` input is `{"type":"object","required":[],"additionalProperties":false}`.
- `epistemic_graph_proposal_submit` and `epistemic_graph_submit_review_admit`
  share one proposal schema (`operations` 1–200 of the operation `oneOf`).
- `epistemic_graph_capture_sources.operations` allows 0–199 operations and
  defaults to `[]`; its `sources` items are exactly
  `{source_id, title, version, locator}` with `additionalProperties: false`.
- `epistemic_graph_proposal_resubmit.replacements` reuses the operation
  `oneOf` items schema.
- Sequence tool schemas share the `sequence_name` property (1–120 chars with
  the documented description) and the bounded `authority_basis` property
  (1–32 properties, 8192-byte note in the description).

## Non-goals (v1)

- One process serves exactly one domain; multiplexed multi-domain hosting is
  a v2 question.
- The descriptor carries only behavior exercised by epistemic-graph today;
  no speculative feature modules or fold rules.
- surface-feedback remains a native surface on the shared crate and is the
  documented boundary case; it is not re-hosted on the engine.
