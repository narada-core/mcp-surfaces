# Cross-repository contracts

This repository contains Narada-specific MCP adapters and shared runtime
mechanics. Several authorities live in the Narada proper repository or in a
separately deployed PC Site runtime. This register makes those boundaries
navigable without pretending that an external path is a local source file.

Reviewed: 2026-08-08
Local review revision: eeb464249eda123f844997481aaaf26d3b7a5880

## External source convention

`NARADA_REPO_ROOT` means the operator-supplied checkout of Narada proper. It is
not assumed to be adjacent to this repository, and it is not a local Markdown
link. Every integration change that depends on an external contract records:

- the external source path below;
- the external repository commit or release revision;
- the local adapter revision;
- the evidence or test artifact that exercised the contract.

If the external revision is unavailable, the integration must be labeled
`external-unversioned`; it must not be described as current or independently
verified by this repository.

## Contract register

| Contract or authority | Owner | Adapter/consumer here | External source |
| --- | --- | --- | --- |
| NARS session input and delivery semantics | Narada proper | `packages/nars-session-mcp` | `NARADA_REPO_ROOT/docs/architecture/nars-session-input-contract.md` |
| Carrier admission neutralization concept | Narada proper domain model | `packages/mcp-registrar`, NARS/session adapters | `NARADA_REPO_ROOT/packages/domains/concepts/records/carrier-admission-neutralization.concept.json` |
| Agent embodiment admission receipts and Orientation Manifest compilation | Narada proper domain model | `packages/agent-context-mcp` compatibility, persistence/readback, and diagnostic projection | `NARADA_REPO_ROOT/packages/orientation-manifest` |
| Task executability and recovery operations | Narada proper control plane | delegated-task, task-lifecycle, site-loop integration tests | `NARADA_REPO_ROOT/docs/operations/task-executability-e2e-and-recovery.md` |
| PC Site runtime observer and memory database | Narada proper / PC Site runtime | `packages/shared/mcp-runtime-observation`, `packages/runtime-introspection-mcp` | Narada proper PC Site runtime-observer package and its admitted observer schema |
| Operator Console overlay lifecycle | Narada proper / PC Site host | `packages/operator-console-overlay-mcp`, quota-meter integration | Narada proper Operator Console overlay implementation |
| Agent Web UI live delegated-task workflow | Narada proper Agent Web UI | delegated-task and worker-delegation E2E evidence | `NARADA_REPO_ROOT/packages/agent-web-ui/test/live-delegated-task-launcher-e2e.mjs` |

## Ownership rule

This repository may validate and project an external contract, but it does not
become the authority by copying a path, schema excerpt, or fixture. The owning
repository/runtime controls semantic changes, lifecycle authority, and
deployment. Local adapters own bounded MCP transport, policy integration, and
truthful refusal/readback.

## Evidence rule

Cross-repository E2E evidence names both sides of the boundary. A local
fixture can prove the local protocol and adapter contract, but it cannot claim
the external authority. A live external proof must record the external
revision, authority posture, source/site binding, and cleanup result. Missing
external authority is `not_run`, not `passed`.
