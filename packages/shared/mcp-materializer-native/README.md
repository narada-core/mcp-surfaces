# Native MCP carrier materializer

This package is the standalone Rust authority for deterministic, transactional publication of every registered MCP carrier generation.

## Boundary

It consumes the declared carrier contract, Site capability registry, runtime implementation matrix, and workspace artifact evidence. It validates the complete publication set before atomically publishing carrier configuration and generation sidecars. Kimi and OpenCode documents are generation-owned. For Codex, Narada replaces only the MCP namespace and explicitly recorded carrier-policy selectors; other settings, comments, and ordering survive rematerialization. It intentionally exposes no single-carrier materialization path: one generation is one all-carrier consistency unit.

The package does not build JavaScript artifacts, restart running carriers, infer missing registry entries, or edit generated carrier configuration interactively.

## Exported contract

The native executable supports all-carrier materialization, generation recovery, installed-generation verification, and a file-based compatibility protocol for the shared Rust materialization contract. Configuration content is passed by path, never as process arguments. Content-addressed publication keeps running Windows processes bound to immutable binaries while later materialization selects the current artifact.

Generation v2 records an exact emitted-byte fingerprint separately from the canonical managed projection. Managed drift blocks startup. Exact-byte drift with an unchanged managed projection is diagnostic only. All generated text is UTF-8 without BOM, uses LF, and ends with one newline.

## Verification

```powershell
pnpm --filter @narada-core/mcp-materializer-native test
```
