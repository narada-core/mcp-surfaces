# MCP Wiring

This repository ships standalone MCP surfaces. A surface can run without Narada, but if you want it inside a specific supported CLI or TUI, you still need carrier config for that host. In this repo, that means Codex, opencode, or Kimi through the registrar.

## What To Use

- `@narada-core/local-filesystem-mcp` when you want governed filesystem access.
- `@narada-core/mcp-registrar` when you want Narada to write the carrier config for Codex, opencode, or Kimi.

## Standalone Filesystem Example

```powershell
pnpm --filter @narada-core/local-filesystem-mcp build
bun <installed-package>/dist/src/main.js --mode read --allowed-root <your-workspace-root>
# Compatibility path:
node <installed-package>/dist/src/main.js --mode read --allowed-root <your-workspace-root>
```

## Carrier Wiring Examples

Carrier-native config files are host/user-site bootstrap profiles. Each Site binding declares `loading_mode: "static"` or `"progressive"`. Static bindings materialize their selected surfaces directly. Progressive bindings materialize only an explicit bootstrap allowlist; the built-in Codex, opencode, and Kimi profiles start with `agent-context`, `mcp-registrar`, `mcp-loader`, and `local-filesystem`, while all other admitted surfaces remain available through the loader. Local Site surfaces are never inferred from the current directory or from an unchosen Site; Narada launch/session materialization and the Site fabric remain the authority that binds them.

The registrar emits carrier-specific config, not one universal file.

## Build and materialize before wiring

Build the workspace and materialize every carrier from one coherent generation:

```powershell
cargo native-package
cargo native-materialize
```

The default `NARADA_RUNTIME_PROFILE` is `native`. The supported profiles are
`native`, `bun`, and `node-compat`; select one before building when a different
runtime plan is intentional:

```powershell
$env:NARADA_RUNTIME_PROFILE = 'bun' # or 'native' or 'node-compat'
cargo native-package
cargo native-materialize
```

The registrar selects the executable, runtime proxy, entrypoint, arguments,
contract version, and carrier-specific projection. Do not copy a direct
`node dist/src/main.js` command into a carrier config as a substitute for
materialization. Follow the [materialization recovery
runbook](mcp-materialization-recovery.md) when a carrier reports stale
generation or workspace-preflight evidence.

### Progressive loading

Progressive carriers do not need to start every surface process. Use
`mcp_loader_list_site_surfaces` to discover admissible surfaces, then
`mcp_loader_open_surface` with the exact admitted `binding_id` and `site_root`.
Use `mcp_loader_list_tools` or `mcp_loader_tool_discovery_manifest` for the
attached interface schemas, and invoke the selected surface through the loader
proxy. This does not promote the child tools into native top-level carrier
servers; use an explicit static binding when first-class carrier tools are
required.

Progressive bindings reject `surfaces: "all"`, require the four bootstrap
surfaces above, and reject bulk carrier binding. These guards prevent a
registrar operation from silently rebuilding the full startup inventory.

The Site fabric declares possible bindings; carrier-session authority admits an exact, digest-bound subset before launch. The loader may activate only that subset. A class selector may help the authority compile the set, but adding a later class member does not expand an existing session.

### User-Site carrier admission across Sites

A User-Site-bound carrier may admit more than one registered Site. Materialization compiles that finite Site set from the User Site's carrier contract after the User Site has resolved which registered Site roots fall within its authorized roots. For every Site marked `admit_local_bindings`, one transaction must project both:

- the Site's exact, digest-bound binding identities into the carrier admission envelope; and
- the canonical Site root into the loader's `--allowed-site-root` arguments.

These are two halves of one admission decision. A binding without its Site root is unusable; a Site root without exact admitted bindings grants no activation authority. The materializer must test and publish them together.

Cross-Site admission does not merge authority. Activating a Cintamani binding from a User-Site-bound carrier retains Cintamani's `local_site` authority locus, configuration, and permissions. The User Site authorizes the carrier to discover and activate that exact binding; it does not convert the binding into User-Site authority. Sites added after materialization require a new materialization and carrier restart.

### Codex

Generated shape:

```toml
# Codex Apps/connectors are opt-in for profile-less launches.
[features]
apps = false

[plugins."github@openai-curated-remote"]
enabled = false

[mcp_servers.narada-andrey-user-local-filesystem]
command = "<registrar-selected runtime command>"
args = ["<registrar-selected runtime arguments>"]
default_tools_approval_mode = "approve"
```

The command and arguments above are schematic. The registrar-generated Codex
projection may use the native proxy, Bun, or the Node compatibility path
depending on the selected runtime profile.

Set `NARADA_CODEX_ENABLED_PLUGINS` and/or
`NARADA_CODEX_DISABLED_PLUGINS` to semicolon- or newline-separated exact
Codex plugin IDs before running `cargo native-materialize`.
The generated policy is an explicit override map: unlisted plugins are not
given an override, wildcard IDs are not supported, and hand-edits to the
generated config are not preserved. These settings affect the
Codex carrier's base config; a selected Codex profile may layer over them.
The built-in `codex-andrey` projection disables
`github@openai-curated-remote` by default, matching the current local carrier
posture.

### opencode

Generated shape:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "narada-andrey-user-local-filesystem": {
      "type": "local",
      "command": ["<registrar-selected runtime command>", "<registrar-selected runtime arguments>"],
      "enabled": true
    }
  }
}
```

### Kimi

Generated shape:

```json
{
  "mcpServers": {
    "narada-andrey-user-local-filesystem": {
      "transport": "stdio",
      "command": "<registrar-selected runtime command>",
      "args": ["<registrar-selected runtime arguments>"],
      "approval_mode": "approve"
    }
  }
}
```

These carrier examples describe projection shape, not a hand-authored
configuration. The generated file and its generation sidecar are authoritative.

## Where The Truth Lives

- docs/mcp-injection-scopes.md explains host, user-site, and local-site ownership.
- docs/mcp-materialization-recovery.md is the canonical build, materialize,
  restart, and stale-generation recovery procedure.
- docs/package-inventory.md is the package/README inventory checked by the
  documentation gate.
- packages/mcp-registrar/README.md explains the registrar tools.
- packages/local-filesystem-mcp/README.md explains standalone usage.
- `docs/mcp-injection-scopes.md` explains host, user-site, and local-site ownership.
- `packages/mcp-registrar/README.md` explains the registrar tools.
- `packages/local-filesystem-mcp/README.md` explains standalone usage.

The surface itself does not need Narada. The wiring workflow may.

## V2 Descriptor And Runtime Boundaries

The registrar catalog is materialized from each package's native V2 descriptor. The descriptor owns the live `tools/list` contract, effect metadata, projection transport, injection scope, runtime requirements, and lifecycle requirement. Carrier-specific files are projections of that descriptor; they are not a second source of tool or scope truth.

Runtime observation is separate from config wiring. The runtime proxy records generation, heartbeat, lease, freshness, health, and contract-digest state. `mcp-loader_runtime_observation` reports the loader's stable logical connection and active/draining generations. A child replacement is requested through `mcp_loader_surface_restart`; a loader-process restart belongs to the carrier or runtime supervisor. Registrar config apply, loader generation replacement, and carrier restart remain separate actuators.

Projection metadata also carries an explicit `execution` declaration. Existing
bindings default to `stdio` + `session_isolated` + `manual`; this preserves the
current process model. A package can opt into `surface_factory`,
`authority_shared`, or `generation_swap` only through its package-owned
descriptor. `mcp-loader` refuses factory projections with
`surface_execution_adapter_not_supported_by_loader`: it remains the stdio
compatibility adapter, while the PC Site runtime is the intended factory
actuator. A generic loader approval never authorizes a nested mutating call.

The PC Site service is authenticated over loopback and reads the selected
binding from the registrar-generated Site capability registry on every call.
NARS performs action admission before dispatch. Gateway close releases its
carrier-session handles; bounded idle eviction is the fallback for interrupted
sessions. The gateway resolves the PC Site from explicit launch context first,
then from the admitted binding's User Site authority locus; it does not infer
the service root from a Local Site working directory. The current production
canary is `launcher::factory` on the Andrey User Site, with
`launcher::stdio` retained as rollback and a hidden external watchdog proving
bounded service recovery.

For an eligible `surface_factory` + `generation_swap` binding, the PC Site
service exposes an authenticated control-plane generation-replacement action.
The action targets an existing logical instance by its expected active
generation and resolves the candidate only from the current Site registry; it
does not accept an arbitrary entrypoint. Compatible implementation replacement
drains the old generation and preserves connected carrier sessions. Refusal or
candidate failure leaves the old generation active. Replacement events are
reported in authenticated service status and persisted in the Site runtime
event log. Descriptor or tool-contract changes still require registrar review
and materialization; stdio remains the explicit rollback projection.

At execution time, three records have distinct roles: the package descriptor is
the authored contract, the admitted Site-registry binding is the authority
contract, and the live handler inventory is observed implementation evidence.
The registrar compiles and materializes those records but is not runtime binding
authority.

## Native descriptor coverage gate

Every registered package must expose a package-owned `./surface-definition` export. The registrar's native catalog is the only catalog authority; loader fallback entries must use the same built package entrypoint and argument placeholders as the native projection. Operator-specific roots must not be embedded in a descriptor: use `{site_root}`, `{site_control_root}`, `{site_runtime_root}`, `{workspace_root}`, or `{mcp_surfaces_root}` and let the selected binding interpolate them.

The native registrar test checks descriptor coverage, tool-contract conformance, projection transport equivalence, package-version agreement, explicit lifecycle metadata, and portable path interpolation. The shared descriptor builder rejects stale or duplicate read-only inventories. `mcp_loader_site_tool_inventory_check` remains the runtime gate for comparing a live child process with its declared descriptor.

For lifecycle discovery, read `metadata.lifecycle_readback` on the descriptor. First call its `discovery.tool_name` (`mcp_loader_connection_inventory`), select the entry whose declared `select.field` equals `select.equals`, and take the selected `result_field` (`connection_id`). Substitute that value into `status.arguments` and call `mcp_loader_surface_status`. Never fabricate a connection id from `surface_id`; the inventory is the authoritative mapping. This reports the child generation and lifecycle posture without implying that a direct standalone process can be restarted by the loader.
