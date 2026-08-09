# @narada-core/mcp-runtime-proxy

## Verification

```powershell
pnpm --filter @narada-core/mcp-runtime-proxy test
```

Small stdio proxy for carrier-launched MCP servers.

The package also exports `./generation-manager`, a transport-neutral logical
endpoint manager for V2 replacements. It models `starting`, `warming`,
`active`, `draining`, `terminated`, and `failed` generations. Warm-up
performs initialize, initialized notification where applicable, tools/list
contract verification, and an optional declared read-only health call before
atomic activation.

Replayable stdio replacements route new calls to the active generation and
allow old in-flight calls to drain. Streamable HTTP sessions remain pinned to
their original generation while new sessions select the replacement. Drain
expiry returns `session_generation_retired` with reconnect guidance and asks
the adapter to terminate the old process tree. A failed warm-up leaves the old
generation active. `restart_required` descriptors are refused with the exact
carrier/session restart owner; the manager never assumes that authority.

## Runtime observation records

`AtomicRuntimeObservationStore` persists normalized generation observations as exclusive temp-file plus rename records under the configured runtime root. `observe()` is process-inspection readback and marks expired leases stale/unreachable; `createRuntimeObservationSink()` is optional and does not grant Narada Site authority. A carrier or loader adapter may emit observations, but the proxy remains transport diagnostics only and never applies reconciliation plans.

The proxy launches an MCP entrypoint with the explicitly materialized
`--child-command`, forwards stdin/stdout, captures stderr,
and turns child startup exits into JSON-RPC errors for pending requests. It
does not grant domain or action authority, choose policy, or interpret MCP
domain behavior. It does enforce lifecycle admission: while declared
Carrier-entry orientation is incomplete, it filters every request and
notification category, admits only the contract's bounded bootstrap and
transport operations, and refuses ordinary work. That lifecycle fence is not
action admission.

## Build artifact preflight

Every launch must provide `--artifact-manifest <path>`. The workspace build
creates `.ai/runtime/workspace-artifact-manifest.json`; it records the package
metadata, TypeScript source fingerprints, local dependency metadata, declared
runtime export targets, and their emitted artifact fingerprints. Before the
entrypoint is spawned, the proxy verifies the manifest fingerprint and refuses
with a structured preflight error if the manifest is missing, stale, or no
longer matches an export target. Re-run the workspace build before retrying;
the proxy never starts a server against an unverified workspace.

Carrier materialization adds a second contract gate. Every generated proxy
launch declares `--runtime-contract-version 6`, the current
`--artifact-manifest`, and, for a materialized carrier file, a
`--materialization-sidecar` path. The registrar validates every generated
proxy, child entrypoint, and manifest reference before writing the carrier
file. It records `<carrier-config>.narada-generation.json` with the config,
manifest, registrar-build, and contract fingerprints. The proxy refuses to
spawn the child when that sidecar is missing or stale, including after the
carrier config or registrar build changes.

Materialization requests run in a fresh built registrar subprocess rather than
using a resident registrar's loaded module graph. A failed validation is a
structured refusal; the registrar does not rebuild or retry automatically.

Workspace build and carrier materialization are intentionally separate
lifecycles: `pnpm build` refreshes the workspace artifact manifest but does not
silently rewrite a user's Codex, Kimi, or OpenCode configuration. After a
successful build, refresh a carrier explicitly with:

```powershell
pnpm materialize:carrier -- --materialize-all
```

The command is owned by the built registrar and remains usable when the MCP
registrar surface itself cannot start. It rewrites every registered carrier;
use `--output-dir <directory>` only when an inspection copy is wanted. The
generated sidecars are the proof that the carrier configs and current workspace
generation were produced together. A targeted carrier escape hatch exists only
behind the registrar's explicit `--allow-single-carrier` direct-CLI flag and is
not used by runtime recovery.

When a proxy refuses a stale generation, its structured error includes a
`narada.mcp_runtime_proxy.materialization_recovery.v1` record. Use its
`recovery_group_id` to report one recovery action for all bootstrap surfaces
with the same carrier failure, run the supplied registrar command once, then
follow the `restart` instruction. Regeneration never restarts the carrier
implicitly; Codex, Kimi, and OpenCode must reload their carrier configuration
in a new or restarted session.

## Native Windows proxy

The package builds a Rust multicall executable whose first applet is `proxy`,
plus the benchmark-only `narada-mcp-rhai-filesystem.exe` applet. Each native
build is published under `dist/native/versions/<build-fingerprint>/` and an
atomic `dist/native/current.json` pointer selects the current generation for
new materializations. The registrar resolves that pointer dynamically; an
already-materialized carrier keeps its concrete old path until it drains. The
legacy `dist/native/narada-mcp-runtime.exe` and
`dist/native/narada-mcp-rhai-filesystem.exe` paths are compatibility artifacts
and are created once but never overwritten, so live Windows processes cannot
block a later build. In native mode this process performs
preflight, stdio framing, timeout/cancellation, diagnostics, and process-tree
ownership itself. It creates the MCP server suspended, assigns it to a
kill-on-close Windows Job Object, and only then resumes its main thread. This
removes the assignment race and the separate supervisor process while keeping
the domain surface in its declared Bun or Node runtime.

On supported Windows hosts, carrier materialization uses the native Rust proxy
by default:

```powershell
bun packages/mcp-registrar/dist/src/main.js --materialize-all
```

Pass `--runtime-proxy-implementation bun` for the JavaScript rollback path.
Native mode is Windows-only; non-Windows hosts and Windows hosts without the
built executable fall back to Bun. An explicit `native` selection still fails
clearly when the artifact is unavailable. Both modes use runtime contract v6 and therefore carry explicit
`--child-command` and `--registrar-command` values; the proxy executable never
guesses which JavaScript runtime should launch a domain surface or recovery
registrar.

When a native carrier is consumed by the Rust mcp-loader, the loader unwraps
native child records and supervises the native child directly. The proxy's
preflight therefore applies at the carrier boundary; it is not a second
supervisor for that attached child. The native materialization contract keeps
`--entrypoint` as a validated identity equal to `--child-command`, and records
`--child-applet` when the child is a multicall applet.

### Orientation enforcement substitutability

The TypeScript and native Rust proxies implement the same narrow enforcement
contract. An independent `NARADA_ORIENTATION_REQUIRED` materialization signal
prevents omission of the packet path from reopening ordinary work. They read
the Carrier-entry packet and its derived acknowledgement projection, validate
their exact binding fields, return the same structured refusal and `next_call`,
and re-evaluate the gate on every carrier request or notification. Direct
`agent_orientation_acknowledge` calls are administrative and are never admitted
through the blocked occupant projection; the occupant reaches acknowledgement
only through `agent_orientation_read`'s opaque final continuation. Neither
implementation compiles manifests, delivers required-read pages, records
completion evidence, or creates acknowledgements. Those remain canonical
Agent Context responsibilities; selecting Rust changes the enforcement
embodiment, never the evidence authority.

`test/fixtures/orientation-entry-admission.v1.json` is the shared adversarial
corpus. The black-box parity test runs that corpus through the built TypeScript
and Rust executables, including raw duplicate keys and unusual numeric
encodings before either parser can normalize them, malformed material, missing
binding fields, tampered acknowledgements, exact refusal payloads, all blocked
request/notification categories, and a live blocked-to-open transition. Agent
Context's Carrier E2E then performs the complete Codex and
Kimi ceremony through both implementations before admitting an observable
ordinary effect. Rust remains optional and substitutable; parity is an
executable contract rather than a claim based on similar source code.

The native executable is a multicall host. Its filesystem applet provides the
read-only local-filesystem MCP surface: bounded reads, stat, glob, grep,
inventory, metrics, doctor, and patch-outcome readback. Read-mode
`local-filesystem` surfaces use this applet by default when the native artifact
is available. The applet also accepts explicit `--mode write` launches and
currently exposes the governed `fs_write_file` vertical slice (direct text
content, allowed-root checks, audit logging, and `.ai/tmp`/`.ai/temp` script
refusal).
The registrar keeps write-mode surfaces on JavaScript until the remaining
mutation tools have native parity. JavaScript is also the fallback for
unsupported hosts, missing artifacts, and an explicit `surface_implementation=js`
override.
Native launches declare an explicit child invocation kind and applet so
entrypoint paths are not overloaded with applet semantics.

The Rust + Rhai filesystem executable keeps filesystem operations in the Rust
host and uses a fixed, capability-limited Rhai dispatch script. It supports
the same read and governed low-level write modes for benchmark comparison; it
is a benchmark lane, not the production default.

The selected implementation, executable path, and executable fingerprint are
recorded in the carrier sidecar and checked before child launch. Rust sources,
Cargo inputs, and the native executable are also covered by the workspace
artifact manifest. A build failure preserves the last successful native
artifact rather than publishing a partial executable.

The benchmark target and fixed topology matrix are documented in
[`BENCHMARK-TARGET.md`](./BENCHMARK-TARGET.md). Run the user benchmark with:

```powershell
pnpm --filter @narada-core/mcp-runtime-proxy benchmark:runtime
```

It emits a canonical JSON report and an offline interactive HTML report. It
reports p95 initialization, full-topology private/working-set bytes, and
warm-call latency with per-process attribution across Bun/Bun, Node/Node,
Deno/Deno, native/Bun, native/Node, native/Deno, and diagnostic Native/Boa
when available. Deno is an experimental compatibility lane; a skipped Deno
topology means that Deno was not available or executable on the host, not that
the other lanes failed. Runtime performance is measurements-only: the report
includes descriptive baseline comparisons but no predeclared thresholds.
Harness, protocol, or lifecycle failure returns nonzero.
The benchmark discovers `deno` on `PATH`; for a shell whose environment has
not refreshed after installation, set `NARADA_MCP_BENCHMARK_DENO` to the Deno
executable path.

The minimal `benchmark:runtime` profile answers what overhead the proxy adds to a small fixture. The `benchmark:strong` profile answers whether a realistic surface and workload make that overhead noticeable; its acceptance checks remain workload-specific.

The stronger user-runnable profile is available with:

```powershell
pnpm --filter @narada-core/mcp-runtime-proxy benchmark:strong
```

It exercises five workloads over the shared transport harness and writes a canonical JSON report plus an offline interactive HTML report under `.ai/runtime/mcp-runtime-benchmarks/<report-id>/`:

- `representative`: 32 domain tools plus one proxy-owned status tool, imported schemas, 24 deterministic data files loaded at initialization, and 20 warm domain calls;
- `payload-load`: 32-byte, 4-KB, and 64-KB payloads, sequential calls, and two eight-request concurrent batches;
- `restart-soak`: 200 cold restart cycles and 2,000 warm calls with per-process memory, process-tree, and leak evidence;
- `filesystem-search-load`: a deterministic 2,048-file (~54 MB) haystack, eight sequential local-filesystem MCP commands, and eight concurrent searches per sample;
- `real-structured-command`: the actual structured-command entrypoint, policy inspection, and a safe allowlisted command.

Use `--samples`, `--load-repetitions`, `--soak-cycles`, `--soak-warm-calls`, `--filesystem-files`, `--filesystem-lines`, `--filesystem-concurrent`, `--workloads`, and `--topologies` to make a reproducible smaller or focused run. Deno remains an experimental lane: unavailable Deno is reported as `not_run`, while a measured protocol or tool-call failure remains a real failure. The payload latency gate is explicit about fixed transport cost: native Node must be within 1.05x of the Node baseline or within 1 ms of it, whichever threshold is greater.

In Bun mode on Windows, the JavaScript proxy starts the existing native Rust
process supervisor after preflight. The supervisor owns the MCP server in a Job
Object and monitors the proxy PID. Its diagnostic instance record identifies
`proxy_pid`, `supervisor_pid`, and `managed_child_pid`/`server_pid` separately.
In native mode `supervisor_pid` is null and the Rust proxy directly owns the
server PID. Both modes terminate their complete managed process tree when the
carrier disappears.

Every proxied surface advertises one proxy-owned read-only tool,
`mcp_runtime_proxy_status`, in its normal `tools/list` response. Call it when
a carrier-bound surface may be running an old build. Its
`runtime_freshness.status` distinguishes `current`, `stale`, and `unknown`
using runtime-file content hashes, manifest identity, and TypeScript
source/build-order evidence. Metadata-only rewrites do not make a runtime
stale. `runtime_freshness.reload_action` is the machine-readable operation
for the carrier or runtime supervisor; it never implies an automatic restart.

Pending child requests have a proxy-owned deadline. If the child stays alive but
does not answer, the proxy returns a structured `child_request_timeout` JSON-RPC
error to the carrier, sends `notifications/cancelled` to the child, terminates
the child, and exits non-zero so the carrier can restart the surface cleanly.
Use `--request-timeout-ms <ms>` before `--` to override the default timeout.

The watchdog never interprets a surface's tool arguments. A caller that owns a
surface-level timeout may carry the transport contract in
`params._meta.narada_request_timeout_ms`; the proxy then waits for that
transport timeout plus a bounded grace margin
(`--tool-timeout-grace-ms <ms>`, default 15000) before declaring the child
unresponsive. The admitted transport timeout is capped at 15 minutes and the
grace is additive, so the effective watchdog deadline can be at most 15 minutes
plus the configured grace. Callers that use a surface-owned timeout should
forward this metadata so the surface can return its own bounded result without
losing the shared transport.

The proxy also writes a heartbeat lease at
`<diagnostics-dir>/instance-<proxy-pid>.json`. The lease includes parent,
proxy, supervisor/server PIDs, artifact freshness evidence, and
live/stale/reclaiming/closed state. If carrier stdin closes or the captured
parent PID dies, the proxy first closes the managed server's stdin, waits the
bounded orphan grace period, then terminates the owned process tree. On
Windows this is a supervisor-tree termination; on other platforms it is the
existing signal-based child termination. A live parent and open carrier stream
are never reclaimed. Defaults are a 5-second liveness check and a 15-second
grace; tests/supervisors may set `--liveness-check-ms` and
`--orphan-grace-ms`.

Operators can list all recorded instances without starting a child:

```powershell
node dist/src/main.js --list-runtime-instances --diagnostics-dir <dir>
dist/native/versions/<build-fingerprint>/narada-mcp-runtime.exe proxy --list-runtime-instances --diagnostics-dir <dir>
```

The listing classifies each record from PID liveness and lease expiry, so stale
and live server pairs are explicit rather than inferred from process names.
