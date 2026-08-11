# MCP protocol

This package is the single protocol boundary for Narada MCP clients, test
harnesses, and native adapters. It supports the legacy initialization era and
the stateless MCP 2026-07-28 era without treating a version string as proof of
implementation.

## Exported contract

The package exports protocol-version negotiation and response helpers used by clients, harnesses, and native adapters so version support is established by behavior rather than version-string inference.

## Verification

```powershell
pnpm --filter @narada-core/mcp-protocol test
```
