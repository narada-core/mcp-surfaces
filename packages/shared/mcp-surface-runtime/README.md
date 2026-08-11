# @narada-core/mcp-surface-runtime

## Verification

```powershell
pnpm --filter @narada-core/mcp-surface-runtime test
```

Policy-neutral execution substrate for authority-bound MCP surface instances.

The package owns worker and stdio adapter lifecycle, instance tenancy,
generation replacement, exact live-handler checks, and bounded dispatch. It
does not discover Sites, admit tool effects, authenticate carriers, or own any
surface/domain mutation authority. Callers must provide an admitted Site
binding and an exact Carrier Action Admission decision for every invocation.

Existing surfaces default to stdio, session-isolated, manual replacement.
Factory hosting, authority sharing, and generation swapping are explicit
projection declarations.

Authority-shared reuse requires the complete admitted binding and resolved
adapter fingerprints to match. Replacement is serialized per logical instance
and requires the caller's `expected_generation_id`; stale or concurrent swaps
are refused before a candidate becomes authoritative.
