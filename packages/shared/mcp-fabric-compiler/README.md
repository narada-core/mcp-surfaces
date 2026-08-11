# @narada-core/mcp-fabric-compiler

## Verification

```powershell
pnpm --filter @narada-core/mcp-fabric-compiler test
```

Pure MCP Fabric V2 compilation from package-owned surface descriptors and
resolved bindings to one immutable manifest and deterministic Codex, Kimi, and
OpenCode carrier artifacts.

The compiler derives approval posture from canonical tool effects and authority
requirements. It validates Kimi tool schemas against the strict Moonshot
dialect and applies only explicit semantics-preserving transformations.

This package performs no filesystem mutation and does not start, stop, or
restart carriers. Guarded apply, rollback, and runtime actuation belong to
authorized control-plane adapters.
