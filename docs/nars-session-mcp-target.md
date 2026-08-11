# NARS Session MCP Target

Status: implemented Narada-specific adapter target; runtime registration and
site admission are explicit deployment steps.

The canonical semantic contract is maintained in Narada proper. See the
[external NARS session input contract](cross-repository-contracts.md#contract-register)
register for the source path, owner, and required external revision evidence.

This document defines the MCP-facing boundary only.

## Purpose

The `@narada-core/nars-session-mcp` surface allows an authorized caller
to inspect and deliver bounded input to an already-existing NARS session. It is
a facade over the NARS session authority and `carrier.input.deliver`; it is not
a general messaging bus, worker launcher, task surface, or provider adapter.

## Target Tools

- `nars_session_guidance` - first-use workflow, delivery semantics, refusal and
  recovery rules.
- `nars_session_list` - bounded session discovery within the admitted scope.
- `nars_session_show` - session identity, liveness, authority epoch, and
  bounded status readback.
- `nars_session_input_deliver` - submit one explicit `send`, `enqueue`, or
  `steer` request to a concrete session.
- `nars_session_input_status` - read authoritative admission and lifecycle
  evidence for a submitted input.

The package should not expose a generic `message_send` command because that
name hides the distinction between a live session stimulus, a durable
directive, and governed work.

## Delivery Command

```text
nars_session_input_deliver({
  site_id,
  session_id,
  content? | directive?,
  delivery: "send" | "enqueue" | "steer",
  idempotency_key,
  expected_authority_epoch?,
  payload_ref?
})
```

The implementation must derive the caller identity from the bound MCP/site
context. It must reject caller-supplied authority impersonation and must emit
agent-originated input with the canonical agent-control metadata when the
caller is an agent.

## Admission and Output

Before delivery, the surface resolves the session, checks its health and
authority locator, fences stale or superseded sessions, validates policy scope,
and normalizes the request into `narada.carrier.input_event.v1`.

The result is a bounded receipt with:

- input or directive id;
- site and session identity;
- authority runtime and epoch;
- canonical protocol method;
- admission/queue state;
- deduplication result;
- evidence reference for later status inspection.

The result must not claim provider completion merely because transport returned.
Unknown terminal state remains unknown and is read through
`nars_session_input_status`.
The status readback keeps `status` as a backwards-compatible admission alias
and marks it with `status_semantics: "admission"`; `admission_status` is the
explicit admission field, while `outcome` is the delivery result.

## Policy Boundary

Surface registration must explicitly declare:

- admitted site roots or site ids;
- allowed target session scope;
- allowed delivery modes;
- whether `steer` is permitted;
- whether cross-site delivery is permitted;
- caller capabilities and source kinds;
- output and payload limits.

The default posture should be local-site, non-steering, agent-source delivery.
Cross-site and interruptive delivery require explicit admission.

## Explicit Projections

The adapter has one package implementation and two registrar projections:

| Projection | Scope | Selection | Purpose |
|---|---|---|---|
| `user-site-operator` | `user_site` | User Site binding/default | Operator-facing session discovery and governed input delivery |
| `local-site-nars-runtime` | `local_site` | `runtime_kind: "nars"` or explicit `projection_id` | Site-bound agent access when the selected runtime is NARS |

The Local Site projection declares `runtime_requirements: ["nars"]`. Runtime
affinity selects availability only; it does not replace the adapter's caller,
site, authority-epoch, or delivery-policy checks. A launcher must pass the
runtime kind into site materialization, and the resulting config records
`surface_projection.runtime_kind`. The carrier fabric loader then includes the
projection only when the selected runtime kind satisfies its requirements. The
registrar refuses ambiguous
multi-projection materialization instead of selecting from a server name,
current directory, or entrypoint path.

### User Site operator binding

`user-site-operator` is a process-bound operator projection. The registrar
passes the User Site root and operator identity as launch arguments; the
adapter reads `registry.db` and admits only the registered Site roots. The
MCP tool schema does not accept a filesystem root. Session discovery may span
those admitted Sites, `site_id` remains an optional filter, and a session id
that resolves to more than one admitted Site is refused as ambiguous. Delivery
uses the same Site-root, health, write-posture, authority-epoch, and
idempotency checks as local delivery, with the caller source fixed to the
bound operator identity.

Materialized projection changes take effect when the MCP process is restarted;
they are not a live reload contract.

## Non-Goals

This surface must not:

- write NARS control files directly;
- own a queue outside the NARS queue;
- invoke an LLM/provider directly;
- select a target by ambiguous role/name during delivery;
- silently route to Site Inbox or task lifecycle;
- replace `operator-routing`, `worker-delegation`, `delegated-task`, or
  `site-inbox`;
- report completion without authoritative session evidence.

## Implementation Boundary

The package belongs in the Narada-specific section of the MCP taxonomy. Its
protocol types and semantic fixtures belong in Narada's carrier-protocol
package. The MCP package contains only the adapter, policy integration,
bounded output, guidance, and adapter tests. The registrar owns explicit
projection metadata; Narada proper owns runtime/session authority and must
materialize the NARS projection when the selected runtime is `nars`.
