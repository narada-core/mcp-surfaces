# pnpm boundary

`pnpm` is the package manager for the JavaScript/TypeScript compatibility tree.
It is not part of the Cargo-native build, test, packaging, materialization, or
recovery path.

## Inventory

The 2026-08-13 audit found 589 matching tracked lines in 160 files, excluding
`pnpm-lock.yaml`, `target`, and generated `dist` directories:

| Class | Files | Decision |
| --- | ---: | --- |
| Documentation | 63 | Keep package-specific compatibility commands; native operational instructions use Cargo. |
| Package manifests | 49 | Keep. These define the compatibility workspace and its tests. |
| Compatibility source and scripts | 26 | Keep where they build or operate JS/TS artifacts. |
| Tests and fixtures | 15 | Keep compatibility tests; native launcher tests must not require pnpm. |
| Native Rust | 7 | Apply the file-level decisions below. |

## Native Rust decisions

| Location | Decision |
| --- | --- |
| `native-distribution` | Keep negative assertions proving pnpm is absent from native subprocesses. |
| Native registrar | Remove pnpm recovery guidance; use `cargo native-package` or `cargo native-materialize`. |
| Native runtime proxy | Remove pnpm recovery actions; use Cargo-native commands. |
| Native structured-command | Keep pnpm only as an explicit compatibility-workspace command. Refuse `pnpm exec cargo`; invoke Cargo directly. On Windows, an admitted pnpm command resolves directly to Corepack's `node.exe` entrypoint without a shell. |
| Native loader | Keep `pnpm-lock.yaml` observation and JS compatibility launch metadata; neither is a native build dependency. |
| Native materializer test | Keep the pnpm sentinel proving runtime-environment independence. |
| Native launcher | Retain the `pnpm --dir ... narada operator-surface ...` launch plan temporarily. It targets the separate Narada TypeScript CLI, for which no behaviorally equivalent native operator-surface CLI currently exists. Replace it only when that authority exists. |

## Operational rules

1. Native repository work uses `cargo` directly.
2. `cargo native-release` is the complete native validation and carrier
   promotion command.
3. `pnpm build:compat` and `pnpm test:compat` are explicitly compatibility-only.
4. Do not write `pnpm exec cargo`, `pnpm exec rustc`, or equivalent package
   manager wrappers around native tools.
5. Recovery messages must name the authority that can actually repair the
   failing artifact class.

The remaining pnpm references are not candidates for mechanical replacement.
They disappear only if the compatibility implementation or the Narada
TypeScript CLI it operates is deliberately retired.

## Separate findings

- A compatibility build from a worktree under `C:\Users\andrey\wt` cannot
  resolve repository-relative `../narada` and `../narada-core` references.
  Compatibility worktrees either need sibling dependency worktrees or a
  location under `C:\Users\andrey\src` that preserves the repository topology.
- `pnpm install` reports Windows bin-link targets ending in `.js.EXE` for some
  unbuilt workspace packages. This is packaging/link-generation debt, not a
  reason to put pnpm back into the native path.
