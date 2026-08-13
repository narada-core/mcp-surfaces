# MCP materialization and recovery

This is the canonical recovery contract for a stale or incompatible MCP
carrier. It applies to Codex, Kimi, OpenCode, and any other carrier generated
by the registrar.

## What materialization means

The native Rust materializer turns the declared Site capability registry,
carrier contract, artifact manifest, implementation matrix, and runtime
bindings into carrier configuration plus a generation
sidecar. The sidecar records the artifact-manifest fingerprint, runtime plan,
runtime-contract version, registrar/proxy fingerprints, and the carrier
identity. A carrier must use one internally consistent generation.

Materialization is not a server restart. It does not reload an already-running
Codex, Kimi, OpenCode, loader, or surface process. Restart the affected carrier
after successful materialization.

The current runtime contract version is `6`.

## Declared runtime profile

The canonical authority accepts only registry bindings admitted to the
`native` runtime profile and requires one absolute native proxy executable for
the whole carrier set. The `bun` and `node-compat` registrar profiles remain compatibility
tools, not alternate authorities. Runtime choices for individual child
surfaces belong in the capability registry and implementation matrix; they are
not inferred or silently rewritten during carrier publication.

## Canonical all-carrier procedure

Publish the native authority after changing its source:

```powershell
cargo native-build
```

This writes a content-addressed executable under
`packages/shared/mcp-materializer-native/dist/native/versions/<fingerprint>/`
and atomically updates `current.json`. Normal materialization resolves that
immutable executable and does not build the workspace:

```powershell
cargo native-materialize
```

The equivalent direct Windows adapter is:

```powershell
./tools/materialize-all-carriers.ps1 -NoNotification
```

The Rust authority writes every declared carrier and its
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

## Native recovery

Every native generation records the immutable materializer executable. A stale
proxy returns a structured recovery command equivalent to:

```powershell
<materializer.exe> recover-generation --generation <carrier-sidecar>
```

The authority reconstructs the declared registry, workspace, matrix, home, and
installed-index inputs from that generation and the embedded carrier contract,
then transactionally rematerializes all carriers. This path does not invoke
Bun, Node, `node_modules`, generated registrar JavaScript, pnpm, or a workspace
build. If the artifact manifest itself is stale, rebuild it separately before
materialization; materialization does not pretend to repair source artifacts.

Verify all published evidence directly with:

```powershell
<materializer.exe> verify-all --installed-index "$env:USERPROFILE/.narada/carriers/installed-carriers.json"
```

Verification checks the installed index; generation, config, plan, manifest,
matrix, proxy, and materializer fingerprints; carrier/config pairing; and the
runtime contract version.

Materialization records durable per-carrier restart pressure in
`.ai/runtime/carrier-restart-pressure.json`. Later no-op checks continue to
report `restart_required` until a successful governed successor acknowledges
the exact materialization evidence reference. Missing or stale references fail
closed, so an old restart cannot clear pressure created by a newer generation.

Project Site bootstrap can invoke this gate before its first write. Workspace
discovery prefers an explicit CLI root, `NARADA_MCP_WORKSPACE_ROOT`, or
`NARADA_SRC_ROOT`; it then consults installed Codex, Kimi, and OpenCode carrier
generation sidecars before trying bounded checkout conventions.
All-carrier materialization also writes the canonical installed-carrier index at
`~/.narada/carriers/installed-carriers.json` (override with
`NARADA_INSTALLED_CARRIER_INDEX_PATH`). The index records each carrier's actual
config and generation-sidecar path, so custom locations are discoverable
without conventions. `NARADA_CARRIER_HOME` overrides the shared user-home base
used for conventional sidecars. Conflicting valid sidecars or index entries are
refused rather than silently choosing one workspace. `NARADA_MCP_AUTO_DISCOVERY=0`
disables inferred candidates. A host with no installed/materialized MCP checkout
reports recovery as unavailable rather than downloading or fabricating one.

For a composed recovery plus controlled activation of one managed carrier, use:

```powershell
narada carrier recover --carrier-id <carrier-id> --lifecycle-adapter nars-successor-v1 --site-root <site-root> --site-id <site-id> --carrier-session-id <session-id> --operation-id <operation-id> --requested-by <principal> --expected-state-json <json> --reason <reason> --mutating-authorized <token>
```

Materialization still converges every registered carrier. The lifecycle phase
requires the explicit `nars-successor-v1` adapter and restarts only the selected
NARS-managed carrier session through the PC-owned successor/drain supervisor.
It reports affected sibling carriers as outstanding; it cannot silently restart
carrier sessions outside the command's authority or claim that a carrier
restarted itself. The adapter verifies the durable NARS session record's
`materialized_carrier_id` against `--carrier-id`; missing and cross-carrier
bindings are refused. Materialized runtime proxies propagate their `--carrier-id`
to child surfaces as `NARADA_MATERIALIZED_CARRIER_ID`; a launcher running in
that surface chain passes the identity into the NARS session record.

Native lifecycle executables are published into content-addressed
`dist/native/versions/<fingerprint>/` directories with an atomically updated
`current.json`. Builds therefore do not overwrite executables held open by
running Windows MCP processes. Existing processes keep their immutable binary;
new materialization resolves the current version.

The canonical native authority intentionally has no single-carrier escape.
Carrier generations are one publication unit; repair and rollback cover the
whole declared set. Do not edit generated carrier configuration by hand.

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
the resolved immutable authority while bounded materialization output is
captured in `%TEMP%\\narada-materialize-all.log`; successful runs close after
the completion notification, while failed shortcut runs print the log path and
remain open until Enter before returning a non-zero exit code.

The launcher validates that `current.json` points inside the content-addressed
artifact directory, invokes only that native executable, and returns a non-zero
exit code on failure. It does not discover pnpm, Bun, or Node, build the
workspace, or open an editor.
