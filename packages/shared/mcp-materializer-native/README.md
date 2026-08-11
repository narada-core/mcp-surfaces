# Native MCP carrier materializer

This package is the standalone Rust authority for deterministic, transactional publication of every registered MCP carrier generation.

## Boundary

It consumes the declared carrier contract, Site capability registry, runtime implementation matrix, and workspace artifact evidence. It validates the complete publication set before atomically replacing carrier configuration and generation sidecars. It intentionally exposes no single-carrier materialization path: one generation is one all-carrier consistency unit.

The package does not build JavaScript artifacts, restart running carriers, infer missing registry entries, or edit generated carrier configuration interactively.

## Exported contract

The native executable supports all-carrier materialization, generation recovery, and installed-generation verification. Content-addressed publication keeps running Windows processes bound to immutable binaries while later materialization selects the current artifact.

## Verification

```powershell
pnpm --filter @narada-core/mcp-materializer-native test
```
