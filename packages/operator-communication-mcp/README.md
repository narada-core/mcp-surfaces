# Operator Communication MCP

Validates a complete typed response and returns only its operator projection.

## Tools

- `operator_communication_guidance`
- `operator_communication_project`

The project tool is classified as a local write because inline calls persist by
default. A `response_ref` replay does not append another record.

Schema precedence is explicit input, then `.narada/schemas/operator-communication.toml`
under the bound Site root, then the bundled `schema/typed-response.v1.toml`.
An invalid higher-priority schema fails closed.

Operator display preferences have their own precedence chain: per-call input,
then `.narada/preferences/operator-communication.toml` under the bound Site,
then bundled defaults in `display/operator-display-preferences.v1.toml`. The
default is the `short` policy rendered as `prose`.

Short prose suppresses the redundant `verified` status. Any non-verified
epistemic status and its required uncertainty remain visible; brevity cannot
erase qualification.

`display_policy` accepts `minimal`, `short`, `medium`, `all-limited`, or
`all-unlimited`. Each policy declares its displayed fields and value limits.
Only `all-unlimited` removes display truncation. `format` accepts `prose`,
`code`, or an array of field names. A field array is an exact projection and
uses code rendering. Display filtering occurs after validation and persistence,
so it cannot erase information from the immutable response record. A replay
may select different display preferences without changing that record.

Every accepted inline response is appended to the gitignored SQLite log at
`.narada/runtime/operator-communication/operator-communication.sqlite`. Rows form a
SHA-256 chain, and database triggers reject updates and deletions.

Responses of at most 20,000 canonical JSON characters are stored in their
immutable SQLite row. Larger bodies are stored once as
`.narada/runtime/operator-communication/bodies/<sha256>.json`; the row contains the
digest, length, and relative file reference. The exact validation-schema
snapshot remains in the row.

The immutable `operator_response:<uuid>` reference is returned in MCP result
metadata. Projection by reference verifies the row hash, chain link, schema
digest, body digest, length, and content-addressed path before rendering.

Persistence defaults to `true`. Use `persist: false` only for an intentional
ephemeral inline projection. A replay call accepts `response_ref` alone:
`schema`, `persist`, and `created_by` are rejected because the immutable
record already fixes those properties.

When `created_by` is omitted, the surface uses `NARADA_AGENT_ID` when that
runtime identity is available; otherwise the immutable row records a null
creator without inventing an identity.

Examples:

    {"response":{"schema":"marici.typed-response.v1","response_id":"r1","created_at":"2026-08-28T00:00:00Z","agent_id":"marici.Nima","operator":{"items":[]},"agent":{"state":"completed","objective":"Answer.","stop_condition":"Delivered.","constraints":["Preserve typing."],"items":[],"communication":{"opening_sequence":1,"closing_sequence":1,"actionable_messages":[],"reply_events":[]}}},"created_by":"marici.Nima"}

    {"response":{"schema":"marici.typed-response.v1","response_id":"r2","created_at":"2026-08-28T00:00:00Z","agent_id":"marici.Nima","operator":{"items":[]},"agent":{"state":"completed","objective":"Answer.","stop_condition":"Delivered.","constraints":["Preserve typing."],"items":[],"communication":{"opening_sequence":1,"closing_sequence":1,"actionable_messages":[],"reply_events":[]}}},"persist":false}

    {"response_ref":"operator_response:00000000-0000-0000-0000-000000000000","display_policy":"minimal","format":"prose"}

    {"response_ref":"operator_response:00000000-0000-0000-0000-000000000000","format":["statement","impact"]}

## Verification

Run `pnpm test:operator-communication` from the repository root.
