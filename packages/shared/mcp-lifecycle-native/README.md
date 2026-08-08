# Native lifecycle authority

This package contains one Rust SQLite authority and two stdio MCP adapters:

- `narada-task-lifecycle-mcp`
- `narada-work-lifecycle-mcp`

The work adapter uses the same task tables and Rust handlers; it does not fork
task semantics. The checked-in `catalog/` files are generated from the current
TypeScript `tools/list` contracts and are verified with `pnpm test:parity`.

Build the Windows artifacts with `pnpm build:native`. They are published under
`dist/native/` and are selected by the registrar only after the runtime matrix
promotes both Rust rows from `experimental` to `admitted`.

Node and Bun remain the explicit compatibility profiles while parity,
migration, lifecycle-refusal, and benchmark suites complete.
