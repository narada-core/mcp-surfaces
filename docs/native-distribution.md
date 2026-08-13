# Native distribution

Cargo is the sole authority for the native MCP distribution. The native path
does not invoke Node, Bun, pnpm, tsx, or PowerShell.

```powershell
cargo native-build
cargo native-test
cargo native-package
cargo native-materialize
```

`cargo native-test` serializes native tests because some surface fixtures make
temporary process-wide environment changes.

`cargo native-package` builds every admitted Rust runtime executable, publishes
immutable versioned artifacts, writes `current.json` pointers, creates the
native-only workspace artifact manifest, and seals its artifact build set.
`cargo native-materialize` performs that packaging transaction and invokes the
published Rust materializer directly.

`cargo native-release` runs native tests, packages the distribution, and
materializes configured carriers. `cargo native-verify` checks the published
distribution and its runtime-independence invariant.

The TypeScript implementations are compatibility artifacts. Their independent
workflow remains:

```powershell
pnpm build:compat
pnpm test:compat
```

Compatibility artifacts are not inputs to, and cannot block, the Cargo-native
release path.

See [pnpm boundary](pnpm-boundary.md) for the repository-wide inventory and
the decisions governing remaining package-manager references.
