# MCP Taxonomy

This repository mixes reusable substrate surfaces with Narada-specific control-plane surfaces. The split is practical, not metaphysical: some packages are generic MCP building blocks, others are Narada-owned orchestration or site surfaces.

## Generic Or Reusable

- `@narada-core/mcp-transport`
- `@narada-core/mcp-telemetry`
- `@narada-core/mcp-affordances`
- `@narada-core/mcp-runtime-proxy`
- `@narada-core/mcp-fabric-contracts`
- `@narada-core/mcp-fabric-compiler`
- `@narada-core/local-filesystem-mcp`
- `@narada-core/structured-command-mcp`
- `@narada-core/git-mcp`
- `epistemic-graph` (Rust-native projection hosted by `@narada-core/mcp-surfaces-native`)

`epistemic-graph` preserves attributed problems, conjectures, criticisms, tests,
sources, and relations. It does not certify truth, embed a literature provider,
or treat standards mappings as epistemic authority.

`mcp-fabric-contracts` is generic because it defines transport-neutral document
contracts and deterministic digests. Narada-specific discovery, authority,
binding policy, carrier materialization, and runtime actuation remain in their
own control-plane surfaces.

The V2 runtime observation and reconciliation contracts are also transport-neutral. They describe observed generations and bounded plans; the registrar, loader, and carrier supervisors remain the authorities that apply config, replace children, or restart carriers.

## Narada-Specific

- `@narada-core/site-inbox-mcp`
- `@narada-core/mailbox-mcp`
- `@narada-core/graph-mail-mcp`
- `@narada-core/calendar-mcp`
- `@narada-core/task-lifecycle-mcp`
- `@narada-core/site-loop-mcp`
- `@narada-core/agent-context-mcp`
- `@narada-core/worker-delegation-mcp`
- `@narada-core/delegated-task-mcp`
- `@narada-core/sop-mcp`
- `@narada-core/scheduler-mcp`
- `@narada-core/mcp-registrar`
- `@narada-core/mcp-loader-mcp`
- `@narada-core/runtime-introspection-mcp`
- `@narada-core/speech-mcp`
- `@narada-core/cloudflare-carrier-mcp`
- `@narada-core/site-coherence-mcp`
- `@narada-core/site-lifecycle-mcp`
- `@narada-core/site-registry-mcp`
- `@narada-core/project-state-mcp`
- `@narada-core/operator-routing-mcp`
- `@narada-core/artifacts-mcp`
- `@narada-core/nars-session-mcp`
- `@narada-core/quota-meter-mcp`
- `@narada-core/surface-feedback-mcp`
- `@narada-core/launcher-mcp`

- @narada-core/operator-console-overlay-mcp

## Ambiguous Infrastructure

These are Narada-owned infrastructure surfaces that can feel generic because they support other surfaces, but they still belong to the Narada control plane:

- `@narada-core/mcp-registrar`
- `@narada-core/mcp-loader-mcp`
- `@narada-core/runtime-introspection-mcp`
- `@narada-core/launcher-mcp`

## How To Use This Split

- Treat generic surfaces as reusable substrate unless a package doc says otherwise.
- Treat Narada-specific surfaces as control-plane or site-owned surfaces unless a package doc explicitly says they are portable.
- When in doubt, follow the package README and the injection-scope doctrine in `docs/mcp-injection-scopes.md`.

`@narada-core/mcp-transport` is generic substrate with a single bound site scope. It
must not provide ambient cross-site filesystem reads. Cross-site transfer belongs
to an explicitly authorized User Site or artifact/export surface; see the
transport contract in `docs/mcp-surfaces-target-shape.md`.

## UI Boundary

`mcp-surfaces` is UI-neutral. MCP packages may expose affordance documents and
validation contracts, but they must not depend on Narada renderer packages,
Vue/React/Svelte runtimes, Tailwind runtime packages, or stylesheet modules.
The repository-owned guard is `pnpm test:ui-boundary`, and it runs as the
first step of the root `pnpm test` command. It scans package manifests,
source imports, and source stylesheet files. Narada-side UI tests must not be
treated as enforcement of this cross-repository boundary.
