# @narada-core/site-coherence-mcp

Site-level continuity coherence readback MCP surface for comparing local Narada posture with Cloudflare embodiment posture. The selected authority is native Rust for every runtime profile; Node and Bun are unavailable for this surface.

This surface is read-only. It does not mutate site continuity state or perform operator actions. Remote reads are bounded by connection/read/write deadlines and a 256 KB response cap. The server-bound operator cookie is sent only to the configured carrier and is never returned in MCP results or diagnostics.

## Tools

- `site_coherence_guidance` - explain the continuity readback boundary.
- `site_coherence_check` - compare local Site posture with Cloudflare embodiment posture.
- `site_coherence_doctor` - report configuration, authority, and telemetry readiness.

## Verification

```powershell
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml site_coherence
pnpm --filter @narada-core/site-coherence-mcp test
```

## Telemetry

Telemetry is optional and disabled unless the site enables `.ai/mcp-telemetry.json`. When enabled, this surface emits metadata-only tool status events and does not persist local health snapshots, Cloudflare responses, or other raw result bodies.
