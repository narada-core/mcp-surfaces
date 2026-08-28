# AGENTS.md

Guidance for agents working in this repository.

The canonical package inventory is
[docs/package-inventory.md](docs/package-inventory.md). It is checked against
the package manifests; update it when packages are added or renamed.

## Repository Purpose

`mcp-surfaces` contains MCP surface packages shared by Narada sites and carriers. Some surfaces are standalone and can be used outside Narada; package READMEs and wiring docs carry the setup details. See `docs/mcp-taxonomy.md` for the generic-versus-Narada-specific split.

Current packages:

- `@narada-core/mcp-transport`: shared MCP payload/output-ref helpers.
- `@narada-core/mcp-telemetry`: shared optional MCP telemetry helpers.
- `@narada-core/mcp-affordances`: shared UI-neutral MCP affordance schema and validation helpers.
- `@narada-core/mcp-runtime-proxy`: shared carrier stdio proxy for MCP startup diagnostics.
- `@narada-core/mcp-surface-runtime`: policy-neutral authority-bound surface execution engine with factory and stdio adapters.
- `@narada-core/mcp-runtime-observation`: mandatory-but-best-effort sanitized runtime ownership and lifecycle observation producer.
- `@narada-core/mcp-runtime-client`: bounded production client for invoking Site MCP fabric surfaces.
- `@narada-core/mcp-e2e-harness`: shared bounded mechanics for real MCP end-to-end tests.
- `@narada-core/mcp-fabric-contracts`: shared versioned MCP descriptor, manifest, carrier projection, observation, and reconciliation contracts.
- `@narada-core/mcp-fabric-compiler`: pure manifest and Codex/Kimi/OpenCode carrier projection compiler with strict Moonshot schema validation.
- `@narada-core/execution-contract`: shared typed execution binding and request fingerprint contract.
- `@narada-core/ledger-domain-epistemic`: static `narada.ledger-domain.v1` descriptor for the epistemic-graph domain (data only; behavior lives in the engine).
- `@narada-core/ledger-domain-mcp`: generic `narada.ledger-domain.v1` engine; hosts one static domain descriptor as a complete event-ledger MCP surface.
- `@narada-core/provider-registry`: shared typed, policy-neutral provider/model capability registry loading and resolution.
- `@narada-core/local-filesystem-mcp`: governed filesystem MCP surface.
- `@narada-core/structured-command-mcp`: policy-gated structured command MCP surface.
- `@narada-core/git-mcp`: governed Git inspection and publication MCP surface.
- `@narada-core/site-inbox-mcp`: governed inbox intake and triage MCP surface.
- `@narada-core/mailbox-mcp`: read-only synced mailbox projection MCP surface.
- `@narada-core/graph-mail-mcp`: policy-gated Microsoft Graph mail MCP surface for live reads and draft management.
- `@narada-core/calendar-mcp`: policy-gated Microsoft Graph calendar MCP surface for live calendar reads and guarded event management.
- `@narada-core/task-lifecycle-mcp`: task lifecycle MCP surface.
- `@narada-core/agent-context-mcp`: agent context MCP surface.
- `worker-delegation` surface: native Rust policy-gated worker delegation hosted by `@narada-core/mcp-surfaces-native`.
- `delegated-task` surface: native Rust outcome-oriented task orchestration hosted by `@narada-core/mcp-surfaces-native`.
- `@narada-core/sop-mcp`: versioned standard operating procedure runbook engine with SQLite-backed execution.
- `@narada-core/scheduler-mcp`: Windows Task Scheduler MCP surface for governed task registration, inspection, and execution.
- `@narada-core/mcp-registrar`: MCP surface registrar for binding/unbinding surfaces across Narada sites and carriers.
- `@narada-core/launcher-mcp`: read-only launcher registry, option matrix, plan, and coherence MCP surface.
- `@narada-core/mcp-loader-mcp`: policy-gated runtime MCP surface loader and proxy.
- `@narada-core/runtime-introspection-mcp`: Narada-owned runtime trace and session composition analysis MCP surface.
- `@narada-core/speech-mcp`: host-level speech MCP surface for TTS, bounded capture, transcription, prompt-response, and listen sessions.
- `@narada-core/media-operations-mcp`: Rust CLI and MCP surface for remote YouTube and X downloads, clips, transcripts, thumbnails, and job control.
- `@narada-core/cloudflare-carrier-mcp`: Cloudflare-carrier live operations MCP surface wrapping product-read, session status, and continuity health.
- `@narada-core/site-coherence-mcp`: Site-level continuity coherence readback MCP surface for detecting posture mismatches between local and Cloudflare embodiments.
- `@narada-core/site-lifecycle-mcp`: governed MCP surface aligned with `narada sites ...` CLI commands for Site creation planning, lifecycle inspection, relations, and gated configuration mutations.
- `@narada-core/site-registry-mcp`: User Site MCP surface for canonical cross-site registry inspection and reconciliation planning.
- `@narada-core/project-state-mcp`: read-only Local Site MCP projection for a virtual project-state registry owned by a Narada project.
- `@narada-core/operator-routing-mcp`: User Site operator routing surface for transcript-to-target decisions and inbox fallback packaging.
- `@narada-core/operator-communication-mcp`: schema-governed operator-only response projection surface.
- `@narada-core/artifacts-mcp`: NARS session artifact registration and renderable artifact reference MCP surface.
- `@narada-core/nars-session-mcp`: governed input and bounded readback for existing NARS sessions.
- `@narada-core/quota-meter-mcp`: host-level quota-meter glide status and desktop overlay lifecycle surface.
- `@narada-core/browser-control-mcp`: bounded host-level browser-control surface for authenticated UX verification.
- `event-ledger-native`: shared Rust-only crate (`narada-mcp-event-ledger`) hosting the event-ledger regime machinery; consumed via path dependency by `@narada-core/mcp-surfaces-native` and `@narada-core/ledger-domain-mcp`.

SOP execution and scheduler activation are separate authorities: `@narada-core/sop-mcp` owns procedure runs and `@narada-core/scheduler-mcp` owns activation.

Site-root convention is documented in `docs/site-root-contract.md`: the
workspace is the canonical Site root, `.narada` is the control root, and
`.narada/site.json` is a generated local marker ignored by Git.

- @narada-core/operator-console-overlay-mcp: host-level dedicated MCP surface for the Narada Operator Console overlay; canonical overlay mechanics remain owned by Narada proper.
- `epistemic-graph`: generic Rust-native problem-situation domain descriptor (`ledger-domain-epistemic`) hosted by the `ledger-domain-mcp` engine; tracked events are authoritative and its SQLite projection is disposable. Its ledger machinery is the shared `event-ledger-native` crate (`docs/event-ledger-format.md`).

## Getting Started

- Use `pnpm@10.9.0` (pinned via `packageManager` in the root `package.json`; `corepack enable` provides it).
- Run `pnpm install` after cloning or pulling workspace changes, then `pnpm build`. Package test scripts compile through `tsc -b` into `dist/` and run the compiled output, so a successful build is a prerequisite for any test run. Routine builds preserve the last successful `dist/` artifacts until replacements are emitted; never add destructive workspace-wide pre-build cleanup because interrupted builds must not remove carrier entrypoints.
- After editing the root `tsconfig.json` (or any shared build configuration), run a full rebuild with `pnpm exec tsc -b --force`. Incremental builds will not re-emit unchanged packages, and the `mcp-loader-mcp` freshness test compares build-configuration mtimes against `dist/` and will fail until everything is re-emitted.
- Layout: runnable MCP surfaces live in `packages/*`, shared libraries in `packages/shared/*`, design and doctrine docs in `docs/`, and the root UI-neutrality boundary test in `test/`.
- Root `pnpm test` runs the boundary gates and every package under this repository's `./packages/**`; linked sibling workspaces may provide dependencies but their own test suites remain owned by those repositories.
- The root `README.md` gives repo-level framing; each package has its own `README.md` with setup details.
- Key docs: `docs/mcp-taxonomy.md` (generic versus Narada-specific split), `docs/mcp-wiring.md` and `docs/mcp-injection-scopes.md` (how surfaces reach carriers and sites), `docs/mcp-surfaces-target-shape.md` (target architecture), `docs/mcp-runtime-memory-observation.md` (authority-bound memory attribution), `docs/mcp-output-refusal-conventions.md` (output-ref and refusal patterns).
- Task Lifecycle MCP runtime startup is prepared-only: run `task-lifecycle-mcp --prepare --site-root <site-root>` explicitly before attaching a stateful runtime; see `docs/task-lifecycle-preparation.md` for the readiness contract and remediation path.

## Carrier and Site MCP Fabric

Carrier-native config files are host/user-site bootstrap profiles. Each Site binding declares `loading_mode: "static"` or `"progressive"`. Static bindings materialize their selected surfaces directly; progressive bindings materialize only their explicit bootstrap allowlist and use `mcp-loader` for runtime discovery and attachment. The launch must not infer Local Site surfaces from the current directory or from an unchosen Site. The built-in carrier profiles use progressive loading with `agent-context`, `mcp-registrar`, `mcp-loader`, and `local-filesystem` as the bootstrap set.

Local Site MCP fabric is injected by Narada launch/session materialization, not by creating carrier profiles named for individual sites. Do not add site-specific carrier profiles such as `opencode-sonar`; bind the Site through the launcher/site fabric instead. If a carrier needs a different local Site, launch it through Narada so the Site-owned MCP aggregate is selected at session start.

Generated Codex carrier configs keep `[features].apps = false` for naked
profile-less launches. Exact plugin startup overrides may be supplied through
`NARADA_CODEX_ENABLED_PLUGINS` and `NARADA_CODEX_DISABLED_PLUGINS` before
all-carrier materialization; these are semicolon- or newline-separated exact
plugin IDs, with no wildcard or implicit discovery. Unlisted plugins receive
no generated override; hand-edits to generated config are not preserved. The
overrides apply only to Codex's base carrier config; a
selected Codex profile may layer over them. The built-in `codex-andrey`
projection keeps `github@openai-curated-remote` disabled by default.

Registrar tests must cover generated carrier configs for all supported carrier kinds and prove that shared surfaces use shared package entrypoints and current tool metadata. Generated carrier configs must not preserve legacy Site-local entrypoints or obsolete tool names after a surface migrates to a shared package.

Every registered surface package must expose a package-owned `./surface-definition` export. The registrar native catalog imports those exports as the descriptor authority; loader fallback entries must remain aligned with the same built entrypoint and descriptor argument placeholders. Descriptor paths must remain portable by using fabric interpolation placeholders rather than operator-specific absolute roots.

## MCP Guidance Commands

Most MCP surface packages should expose a read-only `_guidance` command using the surface's normal tool prefix, for example `task_lifecycle_guidance`, `git_guidance`, `fs_guidance`, or `graph_mail_guidance`.

These commands are for model-facing operating guidance. They should explain the surface's purpose, first-use workflow, preferred tool sequence, state semantics, examples, anti-patterns, recovery steps, payload/output-ref conventions when relevant, and boundary notes. They must not mutate state, weaken policy, or replace authoritative tool schemas and policy checks.

When a model is unfamiliar with a surface, uncertain about the correct workflow, or recovering from a refusal/error, prefer calling that surface's `_guidance` command before guessing. If the guidance is missing, unclear, stale, or contradicted by live behavior, submit feedback through `@narada-core/surface-feedback-mcp`.

## Surface Feedback

Agents can submit feedback about any MCP surface via `@narada-core/surface-feedback-mcp`:

- `surface_feedback_submit` — submit a bug, improvement, gap, or observation about a surface.
- `surface_feedback_list` — list feedback with an explicit server-bound read scope.
- `surface_feedback_actionable_queue` — read the bounded actionable queue with an explicit server-bound read scope.
- `surface_feedback_show` — show one feedback entry within an explicit read scope.
- `surface_feedback_stats` — aggregated counts by surface, kind, and status within an explicit read scope.

Read calls must pass `scope` explicitly. `all_authorized` and `store_reconciliation` require the canonical feedback store (`feedback_global_read_requires_canonical_store` otherwise); `authority_visible` and `authority_site_submissions` are server-bound submitter-Site views; `owned_surfaces` requires server-bound owned surfaces. Submitter-site visibility compares server-bound authority to declared metadata and is not authenticated provenance; `submitter_site_id_filter` is declarative metadata filtering only and never establishes provenance or authorization.

The native authority is an append-only event ledger under `<feedback_root>/ledger/` (`narada.event-ledger.v1`; see `docs/event-ledger-format.md`). The SQLite state under `<feedback_root>/.ai/feedback/projection.sqlite` is a disposable fold projection rebuilt from the ledger on every read. A legacy `.feedback/surface-feedback.db` store is migrated once automatically into the ledger and never written again. The TypeScript implementation was removed; the native shared surface (`@narada-core/mcp-surfaces-native`) is the only implementation.

Kinds:

- `bug` — something is broken or fails unexpectedly.
- `improvement` — an enhancement to existing behavior.
- `gap` — missing capability that should exist.
- `observation` — usage note, discoverability finding, or non-urgent concern.

When submitting, include:

- `surface_id` (e.g. `worker-delegation`, `graph-mail`, `mcp-registrar`).
- `submitter_site_id` (e.g. `andrey-user`, `narada-sonar`).
- `submitter_principal` (your agent identity).
- `kind` and a concise `summary`.
- `details` with reproduction steps, expected behavior, and impact.

Use this surface for any MCP usage friction, runtime failures, schema issues, or documentation gaps before opening a task or CAPA.

## Development Rules

- Use TypeScript sources under `packages/*/src` or `packages/shared/*/src` and tests under the matching package `test` directory.
- Do not add `.mjs` or `.js` source files under `packages/*`; MCP package code and package tests are strict TypeScript. The root `test/typescript-source-boundary.test.ts` and `test/ui-neutral-boundary.test.ts` harnesses are the canonical source and UI boundary tests.
- Preserve ESM/NodeNext package behavior.
- Prefer package-local tests for narrow changes, then root tests when shared behavior changes.
- Keep MCP tool schemas explicit and conservative: no broad shell strings, wildcard filesystem access, or implicit mutation paths.
- Keep transport helpers generic. Do not add Narada task-domain behavior to `@narada-core/mcp-transport`.
- Model-facing MCP tool output that can exceed a small inline envelope must pass through the shared `mcp-transport` output-ref boundary or an explicit package-owned equivalent. Large domain results should be materialized and returned with a bounded inline envelope plus a reader tool.
- Keep shared transport readers bound to one site authority scope. Do not accept raw cross-site roots or infer cross-site authority from local filesystem reachability; explicit cross-site transfer belongs to an authorized User Site or artifact/export surface. See `docs/mcp-surfaces-target-shape.md`.
- Shared libraries such as `@narada-core/mcp-transport` live under `packages/shared/*`; runnable MCP surfaces remain top-level packages until the broader `packages/surfaces/*` migration is executed.
- Register every package in the root `tsconfig.json` `references`; root `pnpm build` and `pnpm typecheck` only cover referenced packages.
- When you add or rename a package, root test alias, command, or convention, update this `AGENTS.md` in the same change.

## Common Commands

```powershell
pnpm build
pnpm materialize:carrier:all
pnpm typecheck
pnpm test
pnpm test:build-availability-boundary
pnpm test:ui-boundary
pnpm test:mcp-transport
pnpm test:mcp-telemetry
pnpm test:mcp-affordances
pnpm test:mcp-runtime-proxy
pnpm test:mcp-surface-runtime
pnpm test:mcp-runtime-observation
pnpm test:mcp-e2e-harness
pnpm test:mcp-fabric-contracts
pnpm test:mcp-fabric-compiler
pnpm test:ledger-domain-epistemic
pnpm test:ledger-domain
pnpm test:ledger-domain:native
pnpm test:provider-registry
pnpm test:local-filesystem
pnpm test:structured-command
pnpm test:git
pnpm test:worker-delegation
pnpm test:inbox
pnpm test:mailbox
pnpm test:graph-mail
pnpm test:calendar
pnpm test:task-lifecycle
pnpm test:site-registry
pnpm test:project-state
pnpm test:site-lifecycle
pnpm test:agent-context
pnpm test:delegated-task
pnpm test:sop
pnpm test:scheduler
pnpm test:registrar
pnpm test:registrar:kimi-contract
pnpm test:surface-feedback
pnpm test:launcher
pnpm test:mcp-loader
pnpm test:mcp-loader:e2e
pnpm test:operator-routing
pnpm test:operator-communication
pnpm test:runtime-introspection
pnpm test:speech
pnpm test:cloudflare-carrier
pnpm test:site-coherence
pnpm test:artifacts
pnpm test:nars-session
pnpm test:browser-control
```

The following variants require a live host, a live carrier, or explicit host authority. They are not part of `pnpm test`; do not run them without operator approval:

```powershell
pnpm test:worker-delegation:e2e
pnpm test:worker-delegation:e2e:edit
pnpm test:worker-delegation:e2e:site-fabric
pnpm test:worker-delegation:e2e:carrier
pnpm test:delegated-task:live
pnpm test:delegated-task:e2e
pnpm test:scheduler:e2e:host
pnpm test:launcher:e2e:host
pnpm test:registrar:kimi-live
```

## Verification Expectations

Before handing off changes:

- Run the most specific package test for the touched package (`pnpm test:<name>`).
- Run `pnpm build` or `pnpm typecheck` when package exports, TypeScript config, or shared types change. These cover exactly the packages listed in the root `tsconfig.json` `references`; if a package is missing there, add it rather than working around the gap.
- Run root `pnpm test` for changes affecting shared MCP behavior or package boundaries.

## Adding a New Package

Do all of the following in the same change:

1. Create the package under `packages/<name>-mcp` (runnable surfaces) or `packages/shared/<name>` (shared libraries), with TypeScript sources in `src/` and tests in `test/`.
2. Add the package to the root `tsconfig.json` `references` and verify `pnpm build` and `pnpm typecheck` pass.
3. Add a root `package.json` `test:<name>` alias following the existing `pnpm --filter <package> test` pattern.
4. Expose a read-only `<prefix>_guidance` tool on new surfaces.
5. Register new surfaces in the registrar catalog and cover them in registrar carrier-config tests.
6. Add the package to the inventory and boundary notes in this `AGENTS.md`.

## Git Workflow

- Do not create a new branch unless the operator explicitly instructs it. Use the current branch by default; do not infer branch creation from task scope.
- This repo does not use changesets; the `narada` repo does — do not copy that convention here.
- Stage only paths explicitly scoped to your change and leave unrelated worktree state untouched.

## Boundary Notes

- `local-filesystem-mcp` owns governed file inspection and mutation tools.
- `structured-command-mcp` owns argv-based command execution policy.
- `worker-delegation-mcp` owns policy-gated delegation to worker runtimes; it is not a general shell, task lifecycle, or recursive worker-control surface.
- `delegated-task-mcp` owns durable delegated task records, workflow plans, acceptance contracts, events, and handoff packets; it must not become a shell, git, filesystem mutation, worker runtime, or Narada workboard surface.
- `sop-mcp` owns versioned SOP templates and durable run execution; it orchestrates procedural steps but does not own tasks, workers, filesystem access, or shell execution directly — it delegates those to their respective MCP surfaces.
- `scheduler-mcp` owns Windows Task Scheduler registration, inspection, and execution; it must not become a general shell or process orchestration surface — scheduling policy is defined at the caller level.
- `mcp-registrar` owns the surface-to-site-to-carrier weave; it edits config files (JSON/TOML) but does not start or stop servers or mutate the surfaces themselves.
- Registrar catalog entries may expose explicit projections over one package entrypoint. Projection scope and `runtime_requirements` select availability; they never replace surface policy. Multi-projection bindings must provide an explicit `projection_id` or a runtime kind that selects exactly one projection. Do not infer projection from server names, current directories, or entrypoint paths.
- `site-registry-mcp` owns User Site access to the canonical cross-site registry. It is read-only, exposes reconciliation planning rather than apply, and must not acquire Local Site lifecycle responsibilities.
- `project-state-mcp` owns only the read-only Local Site projection of a site's virtual project-state CLI. It must not own the site's SQL authority, mutate generated outputs, or imply fabrication, metrology, external evidence, qualification, or flight credit.
- `mcp-loader-mcp` owns runtime attachment/proxying for allowed MCP surfaces; it does not own the surfaces it attaches to and must not become a general orchestration layer. It honors explicit `surface_projection.runtime_requirements`: omitted runtime context selects only neutral projections, and runtime-affined projections require a matching `runtime_kind`.
- `mcp-transport` owns reusable payload/output reference mechanics.
- `event-ledger-native` owns the shared event-ledger regime machinery: hash-chained immutable JSON event ledger, authority locks, head-CAS admission, idempotency, and the disposable SQLite projection shell. It carries no domain concepts and must preserve the insertion-order digest convention (`docs/event-ledger-format.md`).
- `ledger-domain-epistemic` owns only the static epistemic domain descriptor (`domain.json` + `narada.ledger-domain.v1` schema); all behavior lives in the engine (`narada-ledger-domain`). It must stay byte-identical to the serving implementation's external contract.
- `ledger-domain-mcp` owns the generic descriptor-driven engine (`narada-ledger-domain`): it loads one static `narada.ledger-domain.v1` descriptor per process and serves the surface that descriptor defines. It must not acquire domain behavior — domains are static `narada.ledger-domain.v1` packages such as `ledger-domain-epistemic`.
- `mcp-telemetry` owns optional site-policy-gated telemetry helpers; it must not replace mandatory audit logs or persist raw args/results by default.
- `mcp-affordances` owns UI-neutral MCP affordance document types, builders, and validation helpers. It must not encode renderer-specific components or bypass MCP tool schemas and policy checks.
- `mcp-runtime-proxy` owns carrier-facing startup diagnostics and transport-neutral generation replacement for eligible stdio and Streamable HTTP surfaces. It must not authorize tools, mutate policy, interpret surface domain behavior, or hot-replace `restart_required` surfaces.
- `mcp-surface-runtime` owns authority-bound instance tenancy, worker/stdio adapter lifecycle, admitted-call validation, and explicitly assessed generation swaps. It does not discover Sites, decide admission, authenticate carriers, or provide a security boundary between worker threads.
- `mcp-runtime-observation` owns only sanitized best-effort ownership/lifecycle source spools. It must never carry tool arguments, results, environment values, or become a control dependency.
- `mcp-e2e-harness` owns bounded child-process transport (JSONL and Content-Length), temporary roots, cleanup, and result artifacts for real MCP E2E tests. It must not create Site fabric, define surface policy, or encode domain assertions.
- `mcp-fabric-contracts` owns policy-neutral, versioned fabric document schemas, canonicalization, and digests. It must not discover Sites, authorize tools, launch runtimes, or own carrier configuration.
- `mcp-fabric-compiler` owns deterministic manifest resolution, carrier projection documents, effect-derived approvals, and semantics-preserving carrier schema transforms. It must not write host files or actuate carrier/runtime lifecycle.
- `execution-contract` owns shared execution binding and request fingerprint types only. It must not launch runtimes, authorize paths, or acquire task/domain behavior.
- `nars-session-mcp` owns only the MCP adapter for concrete existing NARS sessions; NARS carrier protocol and session authority remain in Narada proper.
- `quota-meter-mcp` owns only bounded quota-meter status and overlay lifecycle calls; native provider authentication and quota interpretation remain in quota-meter and the provider CLIs.
- `browser-control-mcp` owns only bounded, explicitly attached host-browser UX verification through loopback CDP; it must not extract credentials, cookies, tokens, or arbitrary JavaScript, and login/submission/destructive actions require explicit confirmation.
- `mailbox-mcp` owns read-only access to site-local synced mailbox projections; it must not become a general PowerShell, Graph, Outlook, or message-sending surface.
- `graph-mail-mcp` owns policy-gated Microsoft Graph mail access and draft lifecycle tools; sending drafts must stay disallowed unless explicit site policy enables it.
- `calendar-mcp` owns policy-gated Microsoft Graph calendar access and event lifecycle tools; event writes must stay disallowed unless explicit site policy enables them.
- Task lifecycle/domain behavior belongs in dedicated MCP surface packages with explicit shared-domain dependencies.
