# MCP Output References and Refusal Conventions

This repository intentionally does not force every MCP surface into one domain schema. Surfaces own their domain result shape. They should still converge on a small set of transport and refusal conventions so callers can recover from large outputs and policy denials predictably.

## Output References

Use a materialized output reference when a successful structured result is too large to return inline.

Common fields for materialized producer pages:

- `schema`: use `narada.producer_output_page.v1` for transport-level materialized result envelopes.
- `status`: keep the domain status if known; otherwise use `ok`.
- `output_ref`: stable `mcp_output:<id>` reference.
- `ref`: alias for `output_ref` when the surface already exposes `ref` conventions.
- `result_materialized`: `true` when the complete structured result has been stored outside inline tool content.
- `reader_tool`: the surface-local reader tool, for example `mcp_output_show`, `git_output_show`, `worker_output_show`, or `structured_command_output_show`.
- `read_command`: copy-pasteable call shape that uses the reader tool.
- `offset`, `limit`, `next_offset`: page coordinates for the transport envelope.
- `transport_offset`, `transport_limit`, `transport_next_offset`: explicit transport page coordinates when the domain result also has its own paging fields.
- `full_output_char_length`: total character length of the materialized JSON wrapper or rendered output.

Reader tools should accept both `ref` and `output_ref` unless the surface has a strong reason not to. If both are supplied and differ, refuse with an alias-conflict code.

## Domain Paging

Do not overload transport paging with domain paging.

When a domain result has its own `offset`, `limit`, or `next_offset`, preserve those fields inside the materialized result. The reader tool pages the materialized wrapper. The remediation text should tell callers to use the original domain `next_offset` for another producer call, and the reader `offset` only for reading the materialized wrapper.

## Payload References

Use `payload_ref` for large or structured inputs that would otherwise exceed inline limits. Tool arguments should stay authoritative for routing and identity fields such as `task_number`, `agent_id`, `message_id`, or `working_directory`. Payload fields may fill companion content such as summaries, findings, task definitions, or long command input.

Payload references are immutable transport, not authority. A loader call targeting one admitted Site may resolve a reference authored in another admitted Site. The loader verifies the complete revision record, requires all admitted copies of that reference to be identical, stages the exact record into the target Site's payload namespace, and reports `payload_transport` in its result. It refuses missing, corrupt, or divergent references. This transport step does not authorize the child mutation: the target binding and child surface still enforce target-Site authority.

Surfaces should reject ambiguous payload forms:

- Non-empty `payload` and `payload_json` in the same call.
- Empty object payloads unless `allow_empty` is explicit.
- Placeholder empty object plus missing `payload_json`.

## Refusals

Expected policy failures should return structured refusal results or typed MCP errors. They should not fall through to generic exceptions.

A useful refusal includes:

- `status`: usually `refused`, `blocked`, or `error`, following the surface's domain vocabulary.
- A machine-readable code such as `refusal_code`, `code`, `error`, or the first entry of `refusal_reasons`.
- Human-readable `message` or `remediation` that says what to do next.
- The attempted tool/action and normalized important inputs, excluding secrets.
- Policy evidence such as allowed roots, allowed commands, required confirmation, active identity, or target locus.
- A false-positive or feedback route when the refusal depends on policy classification.

Do not expose secrets, bearer tokens, upload URLs, raw attachment bytes, or full command output in refusal text.

## Surface Notes

- `local-filesystem-mcp`: refusals should include path classification, allowed-root posture, and false-positive route.
- `structured-command-mcp`: refusals should expose `refusal_reasons` inline and in structured content.
- `git-mcp`: materialized diffs should use `git_output_show` and clarify transport vs domain paging.
- `worker-delegation-mcp`: worker run output should distinguish absent worker-authored output from runtime failure, and `worker_output_show` should read stable run artifacts.
- `task-lifecycle-mcp`: payload-ref helpers should keep task identity fields inline/top-level while moving long evidence fields into payloads.
