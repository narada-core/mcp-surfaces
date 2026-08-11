# `@narada-core/provider-registry`

Typed, policy-neutral provider/model capability registry loading and
resolution. This is a shared library, not an MCP server. It validates an
explicit `narada.provider_registry.v2` document and resolves capability
selections without owning credentials, provider calls, or Site policy.

## API

- `loadProviderRegistry` and `loadProviderRegistrySync` load and validate JSON.
- `parseProviderRegistry` validates an in-memory document.
- `resolveCapabilitySelection` applies request, Site-policy, then registry
  defaults and validates provider/model/capability availability.
- `listCapabilityCatalog` returns bounded catalog rows for one capability.
- `ProviderRegistryError` exposes typed refusal codes and details.

## Resolution contract

Resolution precedence is request selection, then Site policy, then the
registry default. A model selection is provider-rooted; disabled or
capability-incompatible models are refused. The library is policy-neutral:
the caller still owns whether a resolved adapter may perform an operation.

## Verification

```powershell
pnpm --filter @narada-core/provider-registry test
```

The test covers v2 schema validation, precedence, provider-rooted model
selection, disabled-model refusal, and capability catalog behavior.
