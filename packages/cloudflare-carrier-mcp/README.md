# `@narada-core/cloudflare-carrier-mcp`

Read-only Cloudflare-carrier MCP surface for bounded product, session, health,
and continuity readback. It does not silently deploy, mutate a Worker, or
convert connectivity into Site authority.

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
pnpm --filter @narada-core/cloudflare-carrier-mcp test
```

The default suite covers protocol and Site-fabric tests. The live Cloudflare
test is opt-in and records `not_run` when live authority is absent.
