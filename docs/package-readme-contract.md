# Package README contract

Every package in this workspace has a package-owned README. The README is the
closest documentation layer to the implementation and is therefore part of the
public package contract, not optional release prose.

## Runnable surface requirements

Each top-level package under `packages/` that exposes an MCP surface documents:

- a `Tools` or `Tool groups` section with the public surface-level tool names
  or tool groups;
- a `Verification` section with a finite command that exercises the package
  test suite;
- the package boundary, including what the surface does not own, whenever the
  surface delegates authority to another runtime.

The exact input and output schemas remain discoverable through MCP
`tools/list`; the README must still explain the stable workflow and the names
that an agent should use to orient itself.

## Shared-library requirements

Shared packages document the exported contract and include a `Verification`
section with the package test command. They do not need a surface-level Tools
section because they are not independently exposed MCP surfaces.

## Consistency rules

The canonical package inventory is `docs/package-inventory.md`. A package
addition, rename, or removal updates that inventory and its README in the same
change. Local Markdown links in the inventory and package READMEs must resolve.

Registrar and wiring documentation must name the supported runtime profiles
(`native`, `bun`, and `node-compat`) and show the all-carrier materialization
path. Recovery steps are canonicalized in
`docs/mcp-materialization-recovery.md`.

`test/documentation-consistency.test.ts` enforces these rules. The test checks
structure and navigability; it does not claim that a package's behavioral
tests passed.
