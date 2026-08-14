# NARS Session MCP

Native Rust MCP authority for discovering existing NARS sessions, delivering governed input, and reading bounded admission/outcome evidence.

## Runtime authority

The admitted implementation is Rust in `packages/shared/mcp-surfaces-native/native`. Node, Bun, and the historical TypeScript adapter are not admitted carrier runtimes. The surface never starts a second NARS session, invokes an inference provider, or writes the session journal behind its owner. It talks directly to the already-running session's bounded loopback WebSocket authority.

## Tools

- `nars_session_guidance`
- `nars_session_list`: bounded local or User-Site-authorized discovery; health probes are opt-in.
- `nars_session_show`: one session projection with health enabled by default and explicitly suppressible.
- `nars_session_input_deliver`: idempotent send, enqueue, or policy-admitted steer using inline content or `directive.content.text`.
- `nars_session_input_status`: bounded authoritative event evidence selected by input, request, or directive ID; selector-free legacy materialized status remains readable.

All input schemas are closed and bounded. Caller-supplied filesystem roots and endpoints are forbidden. A User Site projection selects only roots admitted by its registry. Delivery requires an active, nonsuperseded authority epoch, explicit caller identity, and an explicitly healthy live endpoint; unknown or malformed health is fail-closed. `steer` additionally requires Site policy admission.

## Binding

Local projection uses the materialized Site root and identity environment. User Site projection reads admitted Site roots from the User Site registry. `NARADA_NARS_SESSION_ALLOW_STEER=1` admits steer; it is disabled otherwise. Credentials and provider execution are outside this surface.

## Validation

The default package test exercises the native implementation:

```powershell
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml nars_
```

Legacy TypeScript/Bun/Node scripts remain only for explicit compatibility comparison and are not runtime-authority evidence.
