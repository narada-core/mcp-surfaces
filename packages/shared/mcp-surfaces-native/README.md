# Native MCP surfaces

This package owns the shared Rust stdio executable for MCP surfaces. It is
invoked with `--surface-id <id>` and keeps the MCP protocol boundary in Rust;
surface implementations are added as explicit modules rather than launching a
JavaScript runtime.

## Boundary

The executable hosts only explicitly admitted Rust surface modules. It does not dynamically evaluate JavaScript or infer a surface implementation from a tool name.

## Verification

```powershell
pnpm --filter @narada-core/mcp-surfaces-native test:native
```
