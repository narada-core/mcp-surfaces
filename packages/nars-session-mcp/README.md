# NARS Session MCP

## Verification

```powershell
pnpm --filter @narada-core/nars-session-mcp test
```

`@narada-core/nars-session-mcp` is a Narada-specific MCP adapter for governed
input to an existing NARS session.

The canonical semantic contract is maintained in Narada proper. The
[cross-repository contract register](../../docs/cross-repository-contracts.md#contract-register)
records the external source path and revision-evidence requirement.

The MCP-facing target and boundary notes are in:

`docs/nars-session-mcp-target.md`

## Tools

- `nars_session_guidance`
- `nars_session_list`
- `nars_session_show`
- `nars_session_input_deliver`
- `nars_session_input_status`

## Binding

The adapter has two explicit process bindings. The local Site projection
requires `NARADA_SITE_ROOT` and `NARADA_AGENT_ID`; `NARADA_SITE_ID` and
`NARADA_CARRIER_SESSION_ID` provide additional provenance. The User Site
operator projection is bound with `--projection user-site-operator`, a User
Site root, and an operator id. It reads admitted Site roots from that User
Site's `registry.db`; callers cannot provide an arbitrary root through a tool
argument. `steer` requires explicit `NARADA_NARS_SESSION_ALLOW_STEER=1`.

Registrar materializes this package through explicit projections rather than
duplicating the adapter: `user-site-operator` for User Site operator access,
or `local-site-nars-runtime` when the selected runtime kind is `nars`. The
runtime projection is selected by `runtime_requirements: ["nars"]`; it is not
inferred from a server name or working directory.

The registrar passes the User Site projection binding as process arguments:
`--user-site-root`, `--source-kind operator`, and `--operator-id`. A running
MCP process must be restarted after its materialized binding changes.

Session paths are resolved through `@narada-core/site-paths`; the adapter does not
construct `.narada` or `crew/nars-sessions` paths itself.

## Boundary

The adapter discovers session projections, verifies the live authority health
and write posture, creates a canonical carrier input event, and submits it
through the NARS WebSocket event endpoint. It does not write `control.jsonl`,
own a second queue, invoke a provider, or claim provider completion.
