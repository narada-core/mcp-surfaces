# Native lifecycle authority

This package contains one Rust SQLite authority and two stdio MCP adapters:

- `narada-task-lifecycle-mcp`
- `narada-work-lifecycle-mcp`

The work adapter uses the same task tables and Rust handlers; it does not fork
task semantics. The checked-in `catalog/` files are generated from the current
TypeScript `tools/list` contracts and are verified with `pnpm test:parity`.

Build the Windows artifacts with `pnpm build:native`. They are published under
`dist/native/` and are selected by the registrar for the `native` profile because
both lifecycle Rust rows are admitted in the runtime matrix.

Node and Bun remain explicit compatibility/reference profiles. The bounded
parity, migration, lifecycle-refusal, cross-runtime, and benchmark suites run
against the native adapters and the Node reference.

## Verification

```powershell
pnpm --filter @narada-core/mcp-lifecycle-native test:parity
pnpm --filter @narada-core/mcp-lifecycle-native test:cross-runtime
```
