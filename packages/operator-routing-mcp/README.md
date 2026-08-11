# @narada-core/operator-routing-mcp

## Verification

```powershell
pnpm --filter @narada-core/operator-routing-mcp test
```

User Site operator routing MCP surface for transcript-to-target routing decisions, durable routing records, and site-inbox fallback draft packaging.

## Tools

- `operator_route_doctor` reports routing posture, fallback policy, and the suggested spoken acknowledgement shape.
- `operator_route_request` compiles a transcript into a routing decision, writes a durable route record, and returns a site-inbox-compatible fallback draft when direct delivery is unavailable.

## Behavior

The surface does not inject into arbitrary runtimes. It records the request, marks direct delivery as unsupported in this surface, and packages a fallback draft for downstream site-inbox submission or other admitted handling.

The suggested spoken acknowledgement uses OpenAI `tts-1` with voice `nova` so the caller can hand it to `speech-mcp` when a spoken fallback is desired.
