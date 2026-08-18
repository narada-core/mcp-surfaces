# @narada-core/mcp-loader-mcp

Policy-gated runtime attachment and proxying for MCP surfaces admitted by a Site fabric.

## Guidance

Use `mcp_loader_guidance` for model-facing orientation, workflow selection,
recovery guidance, and loader boundaries. Use `mcp_loader_list_tools` for exact
attached-tool schemas. `mcp_loader_tool_discovery_manifest` returns compact
canonical names by default; pass `compact: false` for schemas and
`include_runtime_metadata: true` only when lifecycle evidence is material.

Every loader lifecycle projection uses the `narada.mcp_loader.runtime_lifecycle.v1` shape. Attached replayable projections expose `runtime_lifecycle` with `managed_by: "mcp-loader"`, `restartable: true`, and connection-scoped inspect/restart actions. `session_pinned` and `restart_required` projections expose `restartable: false`, `restartability_status: "unavailable_for_lifecycle"`, no child restart action, and a carrier/runtime-supervisor recovery action instead. The machine-readable `loader_restart_action` describes the operation required to restart the loader itself; its `next_call.tool_name` is the external supervisor capability `restart_mcp_loader_process`, deliberately not a child-surface tool and not implemented by the loader process itself. Pre-attachment guidance exposes the same shape with `restartable: null` and `restartability_status: "available_after_successful_attach"`. Inspect `mcp_loader_surface_status` or `mcp_loader_connection_inventory`, then call `mcp_loader_surface_restart({ connection_id, reason })` only for an attached replayable projection. The agent session does not need to restart for that child replacement. Child-surface domain policy remains authoritative, and restart invalidates refs owned by the replaced child.

Runtime lifecycle and freshness metadata is opt-in on ordinary discovery and proxy calls. Pass `include_runtime_metadata: true` when that evidence is material; compact responses are the default. A proxied child guidance result is augmented with loader lifecycle and freshness only when that flag is set.

Use `mcp_loader_resume_or_open_surface` with a canonical `binding_id` for retryable agent workflows. It reuses a live logical handle within the current loader process and transparently reopens the admitted binding after a loader restart. Raw handles remain process-scoped.
Opened handles and unavailable-handle recovery actions retain that canonical
binding ID, so the returned `resume_or_open_surface` call is directly usable.

## Tools

The loader's own public tools are:

- `mcp_loader_guidance`, `mcp_loader_policy_inspect`, and
  `mcp_loader_runtime_status`.
- `mcp_loader_list_site_surfaces`, `mcp_loader_site_fabric_diagnostics`, and
  `mcp_loader_site_tool_inventory_check`.
- `mcp_loader_attach_surface`, `mcp_loader_open_surface`, `mcp_loader_resume_or_open_surface`,
  `mcp_loader_surface_status`, `mcp_loader_surface_restart`,
  `mcp_loader_detach`, and `mcp_loader_connection_inventory`.
- `mcp_loader_list_tools`, `mcp_loader_tool_discovery_manifest`,
  `mcp_loader_call_tool`, and `mcp_loader_call_surface_tool`.
- `mcp_loader_runtime_observation` and `mcp_loader_process_ownership`.

The exact child tool schemas remain owned by each attached surface and are
discovered through the loader's tools/list projections.

## Runtime observation

Call `mcp_loader_runtime_observation({ connection_id, carrier_kind })` after attach to obtain `RuntimeObservationV2`. The result includes stable logical identity, active generation state, heartbeat/lease freshness, descriptor and live tool-contract digests, lifecycle eligibility, and one bounded recovery actuator. A replayable child names `mcp_loader_surface_restart`; a session-pinned or restart-required projection names the carrier supervisor capability. The loader reports `runtime_state_root: null` because persistent observation records belong to the generic runtime-proxy observation store or another explicitly configured owner.

## Process ownership

`mcp_loader_process_ownership` reports only direct children owned by the
current loader run and bounded cleanup actions for known connections. Host-wide
topology, memory accounting, proxy/conhost reconciliation, and unrelated
processes remain outside Loader authority.

## Tool Call Timeouts

`mcp_loader_call_tool` forwards the nested `arguments` object unchanged. When `arguments` include `timeout_ms`, the child tool bounds itself at that value and the loader honors it up to its bounded maximum (`--tool-call-timeout-ms`, default 120000, max 900000). The loader's own outer wait deadline is the declared timeout plus a bounded grace (`--tool-timeout-grace-ms`, default 1000 ms, max 60000 ms), including at the maximum; the outer deadline may therefore reach 960000 ms. This lets a child return its own bounded timeout result instead of losing the race to the loader's `child_timeout` error. Calls without a nested `timeout_ms` are bounded by the policy default with no grace; the loader's deadline is the only timer.

The full timeout stack, shortest to longest: the tool's own `timeout_ms` < the loader's outer deadline (tool timeout + grace) < the runtime proxy watchdog (`--request-timeout-ms`). The proxy never interprets tool arguments; a caller that owns a surface-level timeout carries it in the transport-level `params._meta.narada_request_timeout_ms` field, and the proxy waits for that transport timeout plus its own bounded grace (`--tool-timeout-grace-ms`). Each layer yields to the layer below it, so a bounded tool returns its own result and the transport survives.


## Loader Runtime Freshness

A long-lived loader process can outlive a source, dependency, build-configuration, or runtime rebuild. Call `mcp_loader_runtime_status` to compare the running loader files with their source files and to inspect dependency/configuration evidence. `status: "stale"` means the loader process must be restarted through its carrier or runtime supervisor; invoke `reload_action.next_call` (`restart_mcp_loader_process`) through that supervisor. `mcp_loader_surface_restart` replaces only an attached child and does not hot-reload the loader. `status: "unknown"` means required freshness evidence is unavailable and should not be treated as current.

The default allowed roots and entrypoint prefixes derive from the loader's resolved surfaces root, the active `NARADA_SITE_ROOT`, optional `NARADA_MCP_ALLOWED_SITE_ROOTS`/`NARADA_MCP_ALLOWED_ENTRYPOINT_PREFIXES`, and the current user's Narada root. They do not depend on a fixed checkout path or User Site identifier.

## Live Tool Inventory

`mcp_loader_site_tool_inventory_check` starts fresh child surfaces, compares each live `tools/list` response with the Site fabric, and materializes the complete observation as an immutable `mcp_payload` ref. Its compact model-facing result includes each finding's status plus bounded missing, extra, duplicate, and unclassified tool names; probe failures include their diagnostic. Pass the returned `observation_ref` to `registrar_site_registry_conformance_check`; do not copy the observation maps into a new request.

Inventory observations use the `site-tools-` payload-id namespace. Loader retains at most 32 observations per Site and removes observations older than seven days. Each result includes `observation_retention` with the applied limits and removals. If a runtime-affined surface is skipped because no compatible `runtime_kind` was supplied, the overall observation status is `partial`, never `ok`; pass the required runtime kind for complete coverage.

Site fabric resolution prefers a non-empty `.ai/mcp/config.json`. When that compatibility path exists but declares no MCP servers, the loader falls through to the canonical Site aggregate or fragments; the empty file is used only when no aggregate exists. This prevents retired empty sidecars from shadowing the active fabric while preserving intentionally empty Sites.

## Runtime-Affined Projections

Site fabric entries may declare `surface_projection.runtime_requirements`. The loader never infers a runtime from the entrypoint, process name, or current directory. `mcp_loader_attach_surface` and `mcp_loader_site_tool_inventory_check` accept an explicit `runtime_kind`; omitting it selects only runtime-neutral projections, while a runtime-affined surface is refused with `surface_runtime_required`. A supplied but incompatible runtime is refused with `surface_runtime_not_supported`.

Inventory results carry `runtime_kind` and `runtime_skipped_surface_ids`. A skipped runtime-affined surface is reported as `runtime_not_selected` at finding level and makes the aggregate observation `partial`, not as a missing or drifted surface. To inspect the NARS projection, pass `runtime_kind: "nars"`.

Attached child surfaces receive `NARADA_SITE_ROOT` set to the requested `site_root`. This is the authoritative Site binding for the child process; the loader does not let an ambient caller Site root override it.

The loader also preserves a narrow, explicit carrier-context allowlist for child surfaces that need caller identity or session binding: `NARADA_AGENT_ID`, `NARADA_OPERATOR_ID`, `NARADA_NARS_SESSION_SOURCE_KIND`, `NARADA_CARRIER_SESSION_ID`, and `NARADA_SITE_ID`. These values identify the caller context; they do not grant authority or bypass the attached surface's own policy.

Surface requests resolve by exact declared `surface_id` metadata or exact fabric server key. The loader does not derive one identifier from another by name parsing.

The payload's declared creator and id namespace are lineage hints and accidental-misrouting guards, not cryptographic provenance or policy authority.

## Native loader (Rust)

`pnpm run build:native` publishes the full contract-compatible Rust loader under `dist/native/versions/<build-fingerprint>/` on Windows and atomically selects it through `dist/native/current.json`; no mutable unversioned executable is published or accepted. The native path covers the public MCP surface, Site-fabric resolution and policy checks, child supervision, initialization and tool routing, bounded timeouts, lifecycle diagnostics, freshness, stable logical handles across replayable child restart, inventory, observation, and bounded output references.

The Rust implementation is the sole admitted loader authority in every runtime profile. The loader does not guess, locate, or substitute Node/Bun runtimes and has no compiled-in TypeScript surface registry. It may still execute an external runtime when that exact command is part of the admitted Site-fabric declaration; that is child execution, not a loader implementation fallback.

Native freshness is anchored to native/src/main.rs, native/src/full.rs, the native
Cargo manifest, and the workspace Cargo lockfile. Legacy TypeScript/JavaScript
sources are not loader dependencies and do not participate in native loader
freshness.

For a registrar-materialized native child, the Rust loader deliberately unwraps the native runtime-proxy record and launches `--child-command` directly. The carrier-level runtime proxy remains responsible for materialization preflight; mcp-loader is responsible for the attached child's policy, ownership, supervision, and lifecycle. In native modes, `--entrypoint` is retained as a validated identity field and must match `--child-command`; `--child-applet` is recorded explicitly for multicall children.

Run the authoritative Rust suite with:

    cargo test --locked --manifest-path native/Cargo.toml

The bounded loader benchmark is pnpm run benchmark:loader. It measures Node/Node, Bun/Bun, and Rust/Node over the same initialize, tools/list, explicit stdio attach, repeated tools/call, and detach workload. It also reports peak loader memory, attached-child memory, and their combined peak; on Windows these are private bytes, while Unix uses RSS. Override NARADA_LOADER_BENCHMARK_SAMPLES and NARADA_LOADER_BENCHMARK_WARM_CALLS for a finite sample size.

## Boundary

MCP Loader owns child attachment, initialization, tool discovery, call proxying, and detachment. It does not own the attached surfaces, authorize their domain operations, or materialize the Site action-admission registry.

Dynamic attachment in a governed carrier session is exact-binding activation, not authority creation. The session authority supplies `narada.mcp.binding_admission_envelope.v1`; discovery, attach, and restart refuse unless the exact `binding_id` and current declaration digest are present. Site roots, surface ids, aliases, entrypoint prefixes, and inherited identity variables are consistency or routing evidence and never substitute for admission. Ambient attachment is available only through the explicit `--standalone-ambient-attachment` development mode.

## Verification

The native gate includes a hermetic full-fabric sequence test: a temporary Site
is registered, Registrar materializes its epistemic-graph binding, Loader calls
the real native surface, and a replacement Loader process verifies persisted
sequence state.

```powershell
pnpm --filter @narada-core/mcp-loader-mcp test:native
```

```powershell
pnpm --filter @narada-core/mcp-loader-mcp test
```
