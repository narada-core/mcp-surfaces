# @narada-core/mcp-fabric-contracts

## Verification

```powershell
pnpm --filter @narada-core/mcp-fabric-contracts test
```

Versioned, policy-neutral contracts for describing MCP surfaces, compiling Site
fabric manifests, projecting carrier configuration, and reconciling observed
runtime state.

The package exports Zod schemas, inferred TypeScript types, strict parsers,
canonical normalizers, and stable SHA-256 digests. Its postcompile step also
emits JSON Schema documents under `dist/schema` for consumers that do not run
TypeScript.

This package describes fabric state. It does not discover Sites, authorize a
tool invocation, launch a process, or own carrier-specific configuration files.

Package-owned surfaces use `defineSurface` or `defineNativeSurface` so the same
tool registry supplies `tools/list` and `SurfaceDescriptorV2`. Native
descriptors declare effects, guidance identity, authority/runtime requirements,
explicit projections, and lifecycle class. `assertLiveToolsConform` compares a
fresh live tool list with that descriptor before carrier materialization is
trusted.

Each projection may also declare an execution posture. Omission is deliberately
conservative and normalizes to `stdio`, `session_isolated`, and `manual`.
`surface_factory`, `authority_shared`, and `generation_swap` are independent,
explicit opt-ins and participate in descriptor identity. The package also
exports the policy-neutral factory/runtime request types; it does not decide
whether a Site binding or tool effect is admitted.
