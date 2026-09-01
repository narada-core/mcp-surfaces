# @narada-core/artifacts-mcp

NARS session artifact MCP surface for model-facing artifact workflows. Every
runtime profile selects the native Rust authority; Node and Bun are
unavailable for this surface.

The surface is a thin client over the current NARS artifact API and does not
maintain a second artifact store. It verifies that registration sources are
real files under the bound Site root before forwarding their canonical paths.
NARS remains authoritative for artifact ids, content-type policy, metadata,
event publication, and content serving.

## Tools

- `artifacts_guidance` - model-facing workflow guidance.
- `artifacts_doctor` - reports whether the NARS base URL and session id are configured.
- `artifact_register_file` - registers a local file with NARS and returns a
  renderable `artifact_ref` message part.
- `artifact_list` - reads a bounded page of the current session artifact index.
- `artifact_read` - reads one artifact metadata record and returns its message part.
- `artifact_present` - asks NARS to append and broadcast a structured
  `assistant_message` event containing the artifact reference.
- `artifact_message_part_create` - creates a message part from known metadata
  without registering anything.

## Configuration

The process launcher binds the endpoint and session. Tool callers cannot
override the endpoint, and cannot select a different session when the process
has a bound session. Configure the native process with environment variables:

```powershell
$env:NARADA_NARS_BASE_URL = 'http://127.0.0.1:52944'
$env:NARADA_SESSION_ID = 'carrier_...'
```

When `--nars-base-url` is omitted, the surface can discover the endpoint from
the NARS session index if `--site-root` or `NARADA_SITE_ROOT` is available.

Equivalent environment variables:

- `NARADA_NARS_BASE_URL`
- `NARADA_SESSION_ID`
- `NARADA_SITE_ROOT`

`NARADA_CARRIER_SESSION_ID` is still accepted as a compatibility fallback, but
new launch/runtime wiring should prefer `NARADA_SESSION_ID`.

Registration and presentation require an `idempotency_key`. A retry returns
the original artifact or presentation event; reuse with different content is
refused.

## Projection Contract

`artifact_register_file` returns a ready-to-project shape:

```json
{
  "message_part": {
    "type": "artifact_ref",
    "artifact_id": "art_...",
    "kind": "html",
    "title": "Report",
    "render_hint": "inline"
  },
  "assistant_content_parts": [
    {
      "type": "artifact_ref",
      "artifact_id": "art_...",
      "kind": "html",
      "title": "Report",
      "render_hint": "inline"
    }
  ],
  "operator_message": "Artifact ready: Report"
}
```

After registering, call `artifact_present` when the operator should see the
artifact inline. NARS emits the structured `assistant_message` event, so
agent-web-ui can render the artifact, including HTML iframe previews for
`kind: "html"`, without relying on the model to paste JSON or force structured
assistant content.

When only an artifact id is known, prefer `artifact_read` before
`artifact_message_part_create`; the latter creates an unverified reference and is
intended for recovery or bridging code that already has trusted metadata.

## Verification

```powershell
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml artifact
pnpm --filter @narada-core/artifacts-mcp test
```
