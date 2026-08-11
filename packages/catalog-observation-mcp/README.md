# `@narada-core/catalog-observation-mcp`

Read-only MCP boundary for provider catalog observations delegated to Narada
management. It reports bounded catalog evidence; it does not select providers,
change provider policy, or perform provider calls.

## Tools

- `catalog_observation_guidance` — explain the observation workflow and
  boundary.
- `catalog_observation_observe` — return the admitted provider-catalog
  observation for the configured request.

## First-use workflow

Call `catalog_observation_guidance` first when the surface is unfamiliar, then
call `catalog_observation_observe` with the explicit observation arguments
required by its schema. Treat the result as observation evidence until the
owning Narada management surface admits it.

## Boundary

The package owns transport and input validation only. Provider registration,
selection, credentials, and policy remain outside this package. It is
authority-bound by the serving Site/carrier configuration and does not accept
an arbitrary database or repository root.

## Verification

```powershell
pnpm --filter @narada-core/catalog-observation-mcp test
```

The package test covers protocol startup, tool discovery, bounded observation,
and refusal behavior.
