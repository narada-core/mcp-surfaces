# MCP materialization and recovery

This is the canonical recovery contract for a stale or incompatible MCP
carrier. It applies to Codex, Kimi, OpenCode, and any other carrier generated
by the registrar.

## What materialization means

The registrar turns the current Site fabric, package build artifacts, runtime
proxy, and carrier bindings into carrier configuration plus a generation
sidecar. The sidecar records the artifact-manifest fingerprint, runtime plan,
runtime-contract version, registrar/proxy fingerprints, and the carrier
identity. A carrier must use one internally consistent generation.

Materialization is not a server restart. It does not reload an already-running
Codex, Kimi, OpenCode, loader, or surface process. Restart the affected carrier
after successful materialization.

The current runtime contract version is `6`.

## Supported runtime profiles

`NARADA_RUNTIME_PROFILE` selects the materialized runtime plan:

- `native` is the default profile and uses the available native runtime
  components where they are built and admitted;
- `bun` uses Bun entrypoints and the Bun TypeScript runtime path;
- `node-compat` uses the Node-compatible TypeScript path for compatibility and
  rollback.

The profile is a coherent plan across the registrar, runtime proxy, loader,
and child surfaces. It is not an instruction to change one child command by
hand. The generated carrier configuration is the source of the selected
profile.

## Canonical all-carrier procedure

From the mcp-surfaces checkout:

```powershell
pnpm build
pnpm materialize:carrier -- --materialize-all
```

`pnpm build` uses the Bun build path by default. Use `pnpm build:node` when the
Node-compatible build is intentionally selected. The build and artifact
manifest must complete successfully before materialization.

For explicit profile selection:

```powershell
$env:NARADA_RUNTIME_PROFILE = 'native' # or 'bun' or 'node-compat'
pnpm build
pnpm materialize:carrier -- --materialize-all
```

The registrar writes every configured carrier and its
`<carrier-config>.narada-generation.json` sidecar atomically. The default
operation is all-carrier materialization so that sibling carriers do not retain
different runtime plans or contract generations.

Restart every affected carrier after the command completes. For Codex this
means ending the current session and starting a new one. For Kimi or OpenCode,
restart the corresponding carrier process/session. A surface-level restart
only replaces an attached child and cannot repair a stale carrier generation.

## Reading startup failures

Use the error code as the first routing decision:

| Error | Meaning | Required recovery |
| --- | --- | --- |
| `materialization_generation_stale` | The carrier sidecar or artifact generation does not match the workspace. | Build, materialize all carriers, restart the carrier. |
| `workspace_manifest_stale` | A source or package manifest changed after artifact generation. | Build, materialize all carriers, restart the carrier. |
| `workspace_dependency_unverified` | A local dependency changed after artifact generation. | Build, materialize all carriers, restart the carrier. |
| `workspace_export_target_missing` | A declared package export is absent from the build output. | Build first; if it persists, fix the build/export defect before materializing. |
| `runtime_contract_version_mismatch` | The carrier and runtime proxy were generated for different contract versions. | Materialize all carriers with the current registrar, then restart. |
| `child_exited_before_response` | The selected child process failed before MCP initialize completed. | Inspect the reported stderr and runtime; do not assume rematerialization alone fixes a child defect. |

The error's recovery group is a diagnostic deduplication key, not permission to
materialize only the named surface. It identifies one carrier-wide recovery
action shared by all affected surfaces.

## Automatic stale-only recovery

The repository-owned entrypoint is:

```powershell
node scripts/recover-carrier-materialization.mjs
```

It inspects the workspace artifact manifest first, runs the workspace build only
when artifacts are stale, inspects every registered carrier projection, and
invokes transactional `--materialize-all` only when at least one projection
differs. A current workspace is a true no-op. Project Site bootstrap can invoke
this gate before its first write via `--mcp-workspace-root` or
`NARADA_MCP_WORKSPACE_ROOT`.

Native lifecycle executables are published into content-addressed
`dist/native/versions/<fingerprint>/` directories with an atomically updated
`current.json`. Builds therefore do not overwrite executables held open by
running Windows MCP processes. Existing processes keep their immutable binary;
new materialization resolves the current version.
## Emergency single-carrier escape

The registrar deliberately makes a single-carrier operation difficult. The
explicit `--materialize-carrier <carrier-id>` path requires
`--allow-single-carrier` and the recovery escape hatch. It is for controlled
emergency inspection or recovery only:

```powershell
pnpm materialize:carrier -- --materialize-carrier codex-andrey --allow-single-carrier --recovery-escape-hatch
```

After using this escape, run the canonical all-carrier procedure before
considering the workspace recovered. Do not edit generated carrier
configuration by hand.

## Verification after restart

1. Confirm the carrier starts without a materialization or workspace-preflight
   error.
2. Confirm the registrar and loader surfaces initialize.
3. Call the loader runtime status/inventory tools when available and inspect
   the reported generation, runtime profile, and artifact evidence.
4. If one surface still fails, classify its child stderr separately; do not
   repeatedly restart only that surface while the carrier generation remains
   stale.

The authoritative build/materialization evidence is the generated artifact
manifest, carrier sidecar, and startup diagnostics. A successful desktop
shortcut or command exit without those artifacts does not prove recovery.

## Desktop shortcut

The maintained installer is:

```powershell
pnpm run install:materialize-shortcut
```

It targets the version-independent Windows PowerShell 5.1 executable instead
of a versioned `WindowsApps` PowerShell path. Re-run it after moving the
checkout or recreating the Desktop shortcut. The shortcut invokes
`tools/materialize-all-carriers.ps1` in a visible PowerShell window. It prints
its active phase and a bounded spinner while build/materialization output is
captured in `%TEMP%\\narada-materialize-all.log`; successful runs close after
the completion notification, while failed shortcut runs print the log path and
remain open until Enter before returning a non-zero exit code.

The launcher tolerates harmless native stderr from build tools, resolves the
available `pnpm` launcher, forwards the all-carrier flag explicitly, and
returns a non-zero exit code on failure. It does not open an editor
unexpectedly; inspect the log and startup diagnostics when the shortcut
fails.
