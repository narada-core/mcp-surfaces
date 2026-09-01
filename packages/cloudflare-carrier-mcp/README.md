# `@narada-core/cloudflare-carrier-mcp`

Read-only Cloudflare-carrier MCP surface for bounded product, session, health,
and continuity readback. It does not silently deploy, mutate a Worker, or
convert connectivity into Site authority.

The admitted runtime authority is the shared native Rust executable
`narada-mcp-surfaces --surface-id cloudflare-carrier --native-authority`.
TypeScript remains a descriptor and parity oracle; native profiles do not use
it as a runtime fallback. Session, health, projection-registry, and worker
coordinates are server-bound rather than caller-supplied.

## Tools

- `cloudflare_carrier_guidance`
- `cloudflare_product_read`
- `cloudflare_session_status`
- `cloudflare_health`
- `cloudflare_doctor`
- `cloudflare_carrier_health`

## First-use workflow

Call `cloudflare_carrier_guidance`, then `cloudflare_doctor` to inspect
configuration and readiness. Use the product/session/health tools for bounded
readback and treat live Cloudflare authority as an explicit prerequisite.
Unauthorized or unavailable live access is a reported result, not a reason to
fall back to an unbounded local operation.

## Boundary

The surface owns bounded request validation, product-read projection, session
status, health, and continuity diagnostics. Cloudflare credentials, Worker
deployment, Site authority, and destructive operations remain owned by Narada
proper or the admitted carrier.

## Verification

```powershell
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml cloudflare_carrier_authority
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml --test cloudflare_carrier_protocol
pnpm --filter @narada-core/cloudflare-carrier-mcp test
```

The Rust protocol test uses bounded loopback carrier and projection fixtures.
External live authority remains a post-materialization verification boundary.
