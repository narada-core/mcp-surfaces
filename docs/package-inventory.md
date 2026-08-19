# Package Inventory

This is the canonical inventory of the `mcp-surfaces` workspace. It is derived
from the `package.json` manifests under `packages/`; the documentation gate
checks that every manifest appears exactly once and that every package has a
README.

Last reviewed: 2026-08-08
Review revision: `eeb464249eda123f844997481aaaf26d3b7a5880`

## Runnable surfaces

| Package path | Package | Purpose | README |
| --- | --- | --- | --- |
| `packages/agent-context-mcp` | `@narada-core/agent-context-mcp` | Site-local startup hydration and checkpoints. | [README](../packages/agent-context-mcp/README.md) |
| `packages/artifacts-mcp` | `@narada-core/artifacts-mcp` | NARS session artifact registration and renderable projections. | [README](../packages/artifacts-mcp/README.md) |
| `packages/browser-control-mcp` | `@narada-core/browser-control-mcp` | Bounded host browser control for authenticated UX verification. | [README](../packages/browser-control-mcp/README.md) |
| `packages/calendar-mcp` | `@narada-core/calendar-mcp` | Policy-gated Microsoft Graph calendar reads and guarded event management. | [README](../packages/calendar-mcp/README.md) |
| `packages/catalog-observation-mcp` | `@narada-core/catalog-observation-mcp` | Read-only provider catalog observation boundary. | [README](../packages/catalog-observation-mcp/README.md) |
| `packages/cloudflare-carrier-mcp` | `@narada-core/cloudflare-carrier-mcp` | Cloudflare-carrier product, session, and continuity operations. | [README](../packages/cloudflare-carrier-mcp/README.md) |
| `packages/git-mcp` | `@narada-core/git-mcp` | Structured, policy-gated Git management. | [README](../packages/git-mcp/README.md) |
| `packages/graph-mail-mcp` | `@narada-core/graph-mail-mcp` | Policy-gated Microsoft Graph mail reads and draft management. | [README](../packages/graph-mail-mcp/README.md) |
| `packages/launcher-mcp` | `@narada-core/launcher-mcp` | Read-only launcher registry, option, plan, and coherence projections. | [README](../packages/launcher-mcp/README.md) |
| `packages/ledger-domain-mcp` | `@narada-core/ledger-domain-mcp` | Generic `narada.ledger-domain.v1` engine hosting one static domain descriptor as an event-ledger MCP surface. | [README](../packages/ledger-domain-mcp/README.md) |
| `packages/local-filesystem-mcp` | `@narada-core/local-filesystem-mcp` | Governed local filesystem inspection and mutation. | [README](../packages/local-filesystem-mcp/README.md) |
| `packages/mailbox-mcp` | `@narada-core/mailbox-mcp` | Mailbox synchronization, admission, events, and bounded local projection reads. | [README](../packages/mailbox-mcp/README.md) |
| `packages/mcp-loader-mcp` | `@narada-core/mcp-loader-mcp` | Policy-gated runtime attachment and proxying. | [README](../packages/mcp-loader-mcp/README.md) |
| `packages/mcp-registrar` | `@narada-core/mcp-registrar` | Native Rust Site and carrier surface binding; checked-in native contract is authoritative. | [README](../packages/mcp-registrar/README.md) |
| `packages/nars-session-mcp` | `@narada-core/nars-session-mcp` | Governed input and bounded readback for existing NARS sessions. | [README](../packages/nars-session-mcp/README.md) |
| `packages/operator-console-overlay-mcp` | `@narada-core/operator-console-overlay-mcp` | Bounded MCP boundary for the Narada Operator Console overlay. | [README](../packages/operator-console-overlay-mcp/README.md) |
| `packages/operator-routing-mcp` | `@narada-core/operator-routing-mcp` | Transcript-to-target routing and inbox fallback packaging. | [README](../packages/operator-routing-mcp/README.md) |
| `packages/project-state-mcp` | `@narada-core/project-state-mcp` | Read-only virtual project-state projection. | [README](../packages/project-state-mcp/README.md) |
| `packages/quota-meter-mcp` | `@narada-core/quota-meter-mcp` | Quota-meter glide status and overlay lifecycle. | [README](../packages/quota-meter-mcp/README.md) |
| `packages/runtime-introspection-mcp` | `@narada-core/runtime-introspection-mcp` | Runtime trace and authority-bound memory analysis. | [README](../packages/runtime-introspection-mcp/README.md) |
| `packages/scheduler-mcp` | `@narada-core/scheduler-mcp` | Governed Windows Task Scheduler registration, inspection, and execution. | [README](../packages/scheduler-mcp/README.md) |
| `packages/site-coherence-mcp` | `@narada-core/site-coherence-mcp` | Local-versus-Cloudflare continuity coherence readback. | [README](../packages/site-coherence-mcp/README.md) |
| `packages/site-inbox-mcp` | `@narada-core/site-inbox-mcp` | Governed inbox intake and triage. | [README](../packages/site-inbox-mcp/README.md) |
| `packages/site-lifecycle-mcp` | `@narada-core/site-lifecycle-mcp` | Governed Site lifecycle planning and inspection. | [README](../packages/site-lifecycle-mcp/README.md) |
| `packages/site-registry-mcp` | `@narada-core/site-registry-mcp` | Canonical User Site Registry projection. | [README](../packages/site-registry-mcp/README.md) |
| `packages/sop-mcp` | `@narada-core/sop-mcp` | Versioned procedure templates and durable run execution. | [README](../packages/sop-mcp/README.md) |
| `packages/speech-mcp` | `@narada-core/speech-mcp` | Policy-gated speech, capture, and transcription. | [README](../packages/speech-mcp/README.md) |
| `packages/structured-command-mcp` | `@narada-core/structured-command-mcp` | Structured, policy-gated local command execution. | [README](../packages/structured-command-mcp/README.md) |
| `packages/surface-feedback-mcp` | `@narada-core/surface-feedback-mcp` | Cross-site MCP surface feedback intake and routing. | [README](../packages/surface-feedback-mcp/README.md) |
| `packages/task-lifecycle-mcp` | `@narada-core/task-lifecycle-mcp` | Task lifecycle runtime and tool dispatch. | [README](../packages/task-lifecycle-mcp/README.md) |
| `packages/work-lifecycle-mcp` | `@narada-core/work-lifecycle-mcp` | Site-scoped ticket and task lifecycle authority. | [README](../packages/work-lifecycle-mcp/README.md) |

## Shared libraries

| Package path | Package | Purpose | README |
| --- | --- | --- | --- |
| `packages/shared/execution-contract` | `@narada-core/execution-contract` | Typed execution binding and request fingerprint contract. | [README](../packages/shared/execution-contract/README.md) |
| `packages/shared/ledger-domain-epistemic` | `@narada-core/ledger-domain-epistemic` | Static `narada.ledger-domain.v1` descriptor for the epistemic-graph domain. | [README](../packages/shared/ledger-domain-epistemic/README.md) |
| `packages/shared/mcp-affordances` | `@narada-core/mcp-affordances` | UI-neutral MCP affordance schema and validation helpers. | [README](../packages/shared/mcp-affordances/README.md) |
| `packages/shared/mcp-e2e-harness` | `@narada-core/mcp-e2e-harness` | Bounded mechanics for real MCP end-to-end tests. | [README](../packages/shared/mcp-e2e-harness/README.md) |
| `packages/shared/mcp-fabric-compiler` | `@narada-core/mcp-fabric-compiler` | Manifest and carrier projection compiler. | [README](../packages/shared/mcp-fabric-compiler/README.md) |
| `packages/shared/mcp-fabric-contracts` | `@narada-core/mcp-fabric-contracts` | Versioned descriptor, manifest, projection, and reconciliation contracts. | [README](../packages/shared/mcp-fabric-contracts/README.md) |
| `packages/shared/mcp-lifecycle-native` | `@narada-core/mcp-lifecycle-native` | Shared Rust lifecycle authority and native task/work MCP adapters. | [README](../packages/shared/mcp-lifecycle-native/README.md) |
| `packages/shared/mcp-materializer-native` | `@narada-core/mcp-materializer-native` | Deterministic transactional native all-carrier materialization authority. | [README](../packages/shared/mcp-materializer-native/README.md) |
| `packages/shared/mcp-protocol` | `@narada-core/mcp-protocol` | Shared dual-era MCP negotiation and result helpers. | [README](../packages/shared/mcp-protocol/README.md) |
| `packages/shared/mcp-runtime-client` | `@narada-core/mcp-runtime-client` | Bounded production client for invoking Site MCP fabric surfaces. | [README](../packages/shared/mcp-runtime-client/README.md) |
| `packages/shared/mcp-runtime-observation` | `@narada-core/mcp-runtime-observation` | Sanitized runtime ownership and lifecycle observation producer. | [README](../packages/shared/mcp-runtime-observation/README.md) |
| `packages/shared/mcp-runtime-proxy` | `@narada-core/mcp-runtime-proxy` | Carrier stdio proxy and startup diagnostics. | [README](../packages/shared/mcp-runtime-proxy/README.md) |
| `packages/shared/mcp-surface-runtime` | `@narada-core/mcp-surface-runtime` | Authority-bound surface execution engine with worker and stdio adapters. | [README](../packages/shared/mcp-surface-runtime/README.md) |
| `packages/shared/mcp-surfaces-native` | `@narada-core/mcp-surfaces-native` | Shared native executable hosting explicitly admitted Rust MCP surfaces. | [README](../packages/shared/mcp-surfaces-native/README.md) |
| `packages/shared/mcp-telemetry` | `@narada-core/mcp-telemetry` | Optional metadata-only MCP telemetry helpers. | [README](../packages/shared/mcp-telemetry/README.md) |
| `packages/shared/mcp-transport` | `@narada-core/mcp-transport` | Payload, output-reference, and transport helpers. | [README](../packages/shared/mcp-transport/README.md) |
| `packages/shared/provider-registry` | `@narada-core/provider-registry` | Typed provider/model capability registry loading and resolution. | [README](../packages/shared/provider-registry/README.md) |

`packages/shared/event-ledger-native` is a Rust-only shared crate
(`narada-mcp-event-ledger`) with no `package.json` manifest, so it has no row
above; it is consumed as a Cargo path dependency by
`@narada-core/mcp-surfaces-native` and implements the
[`narada.event-ledger.v1`](event-ledger-format.md) regime.

## Documentation contract

Every package has a package-owned README. Runnable surface READMEs contain a
tools section and a verification section; shared-library READMEs describe the
exported contract and test command. `test/documentation-consistency.test.ts`
checks this inventory, README presence, required headings, local links, and
the runtime-profile names used by the registrar docs.
