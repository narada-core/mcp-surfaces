# MCP E2E Test Debt

This is the canonical inventory of end-to-end test gaps for the MCP surfaces
repository. It records which real boundaries still need proof. Package test
files and task records may link here, but they do not replace this inventory.

## Review metadata

Last reviewed: 2026-08-08
Review revision: eeb464249eda123f844997481aaaf26d3b7a5880
Evidence manifest: `docs/mcp-e2e-evidence/20260712-non-pc-host-batch.json`
Evidence generated at: 2026-07-13
Evidence freshness: historical, not a current pass

External Narada and PC Site references in this register are governed by the
[cross-repository contract register](cross-repository-contracts.md#contract-register);
the external revision must be recorded before a new result can be called
current.

The committed manifest is the latest historical evidence artifact in this
repository. It is an inventory of a prior bounded run, not proof that the
current source revision passes. A source, dependency, carrier, or test change
invalidates a current-pass claim.

For a new pass, record the source revision, workspace/lockfile or artifact
manifest fingerprint, result-artifact SHA-256, prerequisite/authority posture,
cleanup result, and exact `not_run` or `passed` authority outcome. Do not
replace historical evidence with a narrative execution note.

## Meaning Of Real E2E

An E2E test must cross the boundary that production relies on. A test is not
E2E merely because it calls a function named `handleRequest`, uses a temporary
directory, or has `live` in its filename.

For an MCP surface, real E2E means all of the following that apply:

- Start the actual built MCP entrypoint as a child process or connect through
  the actual production transport.
- Send requests through MCP JSON-RPC framing and consume the real response
  path, including output references and reader tools when applicable.
- Materialize the real Site or carrier configuration used by the boundary under
  test. Do not bypass the registrar, loader, launcher, or admission fabric.
- Use real SQLite, filesystem, process, scheduler, or provider boundaries. A
  fake boundary may be used only in a separately labelled contract test.
- Use a controlled fixture, tenant, mailbox, or task namespace for external
  authorities. Customer-visible sends, destructive operations, and broad host
  mutations are forbidden by default.
- Fail as `not_run` when a required authority is absent. An unavailable
  prerequisite must never become a passing assertion or a silent skip.
- Record bounded evidence: test id, surface, site/carrier binding, start/end
  time, prerequisite result, operation result, and cleanup result.

These are integration or contract tests, not E2E proof:

- Direct calls to an exported `handleRequest` without a real child process.
- Injected worker, Graph, scheduler, filesystem, carrier, or transport mocks.
- Tests that only inspect generated JSON without starting the declared entrypoint.
- Tests that skip missing credentials or providers and still report success.
- Tests that exercise a temporary in-memory store without the Site fabric.

## Proof Dimensions

These are orthogonal proof claims, not a strict quality ladder. `B1`-`B4`
describe boundaries. `W1` describes composition across boundaries; it is not
a fifth boundary and does not automatically prove every direct surface
contract. Every debt item should name its required boundary and composition
claims explicitly.

| Claim | Meaning |
| --- | --- |
| `B1` | Actual surface child process and MCP transport |
| `B2` | Site fabric, registrar/loader admission, and configured entrypoint |
| `B3` | Real local authority such as SQLite, filesystem, Git, scheduler, or process lifecycle |
| `B4` | Real external authority or carrier: Graph, provider runtime, speech API, Windows host, or carrier session |
| `W1` | Complete user workflow across launcher, carrier, Site fabric, surface, authority, and durable evidence |

Authority posture is a separate dimension:

| Posture | Meaning |
| --- | --- |
| `A0` | Controlled local fixture or deterministic local authority |
| `A1` | Explicitly admitted controlled external tenant or account |
| `A2` | Production external authority |

An `A0` proof does not claim `A1` or `A2` authority. A `W1` proof may compose
`B1`-`B4` while still using `A0` for a provider or external authority.

## Current Evidence

The rows below summarize the latest committed historical evidence and current
test coverage. They do not assert that the current checkout has reproduced the
recorded result. `Debt status` describes implementation/evidence debt;
`Authority result` describes the authority actually exercised by the recorded
run.

| Surface | Existing test | Honest classification | What it does not prove |
| --- | --- | --- | --- |
| `site-loop-mcp` | [`test/site-loop-configured-surface-e2e.test.ts`](../packages/site-loop-mcp/test/site-loop-configured-surface-e2e.test.ts), [`test/site-loop-production-e2e.test.ts`](../packages/site-loop-mcp/test/site-loop-production-e2e.test.ts) | B1 configured-surface MCP proof plus opt-in production scheduler/resident/recovery proof | The default configured-surface test does not claim production authority; the production test is `not_run` without an explicitly admitted site root, scheduler, and live resident carrier |
| `delegated-task-mcp` | [`test/site-fabric-worker-e2e.test.ts`](../packages/delegated-task-mcp/test/site-fabric-worker-e2e.test.ts), `test/live-worker-integration.test.ts`, external `NARADA_REPO_ROOT/packages/agent-web-ui/test/live-delegated-task-launcher-e2e.mjs` | B1-B3 Site-fabric delegation proof plus W1 launcher-to-carrier workflow through the actual launcher, NARS carrier, `nars-session-mcp`, delegated-task child, worker carrier, and controlled provider; verifies task/event persistence, review acceptance, binding evidence, negative admission, durable worker artifacts, and cleanup | External provider service behavior remains separate; both controlled tests use a bounded local HTTP provider fixture (A0); external path ownership is defined in the [cross-repository contract register](cross-repository-contracts.md#contract-register) |
| `worker-delegation-mcp` | [`test/live-edit-e2e.test.ts`](../packages/worker-delegation-mcp/test/live-edit-e2e.test.ts), [`test/site-fabric-provider-e2e.test.ts`](../packages/worker-delegation-mcp/test/site-fabric-provider-e2e.test.ts), [`test/real-carrier-e2e.test.ts`](../packages/worker-delegation-mcp/test/real-carrier-e2e.test.ts) | B1-B3 edit and Site-fabric proofs plus B4 real `narada-agent-runtime-server` carrier execution through the built MCP child; verifies provider request, model/reasoning binding, lifecycle events, durable run artifacts, refusal, and cleanup | External provider service behavior; the B4 test deliberately uses a bounded local HTTP fixture authority (A0) |
| `surface-feedback-mcp` | [`test/site-fabric-feedback-e2e.test.ts`](../packages/surface-feedback-mcp/test/site-fabric-feedback-e2e.test.ts), `test/surface-feedback-task-lifecycle-integration.test.ts` | B1-B3 nested child handoff, durable feedback/task state, and idempotent conversion | External User Site authority remains separate |
| `task-lifecycle-mcp` | [`test/site-fabric-lifecycle-e2e.test.ts`](../packages/task-lifecycle-mcp/test/site-fabric-lifecycle-e2e.test.ts), `stdio-smoke.test.ts`, `protocol-smoke.test.ts`, `inbox-bridge.test.ts` | B1-B3 Site-fabric carrier lifecycle with SQLite and Markdown closure evidence | A carrier-launched production Site session and external delivery |
| `agent-context-mcp` | [`test/site-fabric-agent-context-e2e.test.ts`](../packages/agent-context-mcp/test/site-fabric-agent-context-e2e.test.ts), `test/agent-context-mcp.test.ts` | B1-B3 startup hydration, SQLite checkpoints, bounded output reader, and root refusal | Production carrier/session authority |
| `sop-mcp` | [`test/site-fabric-sop-e2e.test.ts`](../packages/sop-mcp/test/site-fabric-sop-e2e.test.ts) | B1-B3 durable run, restart/resume, operator gate, and event history | Production Site Loop scheduling |
| `artifacts-mcp` | [`test/site-fabric-artifacts-e2e.test.ts`](../packages/artifacts-mcp/test/site-fabric-artifacts-e2e.test.ts) | B1-B3 child registration/read/presentation through a controlled NARS HTTP authority | Real NARS carrier authority remains separate |
| `operator-routing-mcp` | [`test/site-fabric-routing-e2e.test.ts`](../packages/operator-routing-mcp/test/site-fabric-routing-e2e.test.ts) | B1-B3 durable fallback envelope and route log with truthful unsupported-direct-delivery semantics | Runtime-specific message injection |
| `launcher-mcp` | [`test/pc-host-launcher-lifecycle-e2e.test.ts`](../packages/launcher-mcp/test/pc-host-launcher-lifecycle-e2e.test.ts) | Opt-in PC-host B2-B4/W1 proof through the canonical launcher, NARS-session MCP, Site-local MCP inheritance, process ownership, hidden posture, restart, and descendant teardown; defaults to an explicit `not_run` artifact without host authority | Passed with explicit PC-host authority on 2026-07-13; default remains `not_run` |
| all other surfaces | Package tests and protocol smoke tests | Unit, contract, or protocol coverage | Real child-process Site-bound workflows unless listed below |

## Debt Register

Status vocabulary: `missing`, `partial`, `blocked`, `complete`,
`not_run`, and `excluded_pc_host`. This register tracks two independent
outcomes:

- `Debt status` records whether the required bounded E2E test has been
  implemented, executed, evidenced, cleaned up, and reviewed. `complete` means
  the implementation debt for that row is closed.
- `Authority result` records what authority the test actually exercised.
  `passed` is a real exercised authority result; `not_run` means the test ran
  its bounded readiness path but a required external authority was unavailable;
  `excluded_pc_host` means the authority is intentionally outside this scope.

`not_run` is never a pass. It remains an explicit authority gap and must not be
silently promoted by a local fixture. It does not reopen implementation debt
when the bounded test itself has been executed and reviewed. A row may carry
`Debt status: complete` only when its acceptance evidence is linked from the
test, its structured artifact records cleanup, and the result is reproducible
without hidden operator state.

## Debt Closure

The objective is zero outstanding non-PC-host implementation debt, not an
untruthful claim that unavailable authorities were exercised. The current
register has 22 non-PC-host rows with `Debt status: complete`, including six
external-authority rows whose `Authority result` is honestly `not_run`. The
launcher and Scheduler PC-host boundaries are separately proven; the remaining
PC-host authority rows remain explicitly excluded. Therefore implementation
debt is zero while external and remaining PC-host authority gaps remain visible
and non-passing.

## PC-host Scope Exclusions

The non-PC-host objective still excludes PC-host authority for boundaries not
listed as passed below. The following remain explicitly excluded from that
count: standalone real NARS
session control, live host process introspection outside the launcher lifecycle,
and real NARS artifact authority. The Site Loop boundary now has an opt-in
production test, but it remains non-passing until run with explicit authority.
Their local
child-process contracts remain useful evidence, but they do not claim host
authority.

The launcher lifecycle is the explicit PC-host exception: its opt-in test starts
the canonical launcher twice, proves exact launch-session-to-session binding,
waits for runtime health, verifies Site-local MCP inheritance and hidden process
posture, closes each session, and confirms zero descendants plus temporary-root
cleanup. The default invocation remains non-authoritative and records `not_run`.

The Scheduler lifecycle is the second explicit PC-host exception: its opt-in
test registers a uniquely scoped disposable Windows task through Scheduler MCP,
executes it through the real Task Scheduler boundary, verifies the fixture's
started/completed evidence and Scheduler result code, deletes the task, proves
the task is absent, and removes the fixture command, process, MCP child, and
temporary root. The default invocation remains non-authoritative and records
`not_run`.

The worker-delegation provider/cognition row below is complete only for local
binding and projection through the admitted worker runtime. External provider
authority remains a separate B4/A1-A2 claim and is not established by this
controlled fixture.

Controlled B4/W1 coverage is now present for delegation. The worker B4 test
uses the production `narada-agent-runtime-server` carrier with a bounded local
HTTP provider fixture (A0). The Narada Agent Web UI W1 test starts the real launcher,
carrier, Site-local MCP fabric, `nars-session-mcp`, delegated-task surface, and
worker carrier, then verifies durable task and worker artifacts. These tests
prove the local production topology and carrier protocol; they do not claim
live external-provider authority.

| Priority | Surface | Boundary claim | Required proof | Debt status | Authority result |
| --- | --- | --- | --- | --- | --- |
| P0 | `site-loop-mcp` | Production Site Loop operation | [`test/site-loop-production-e2e.test.ts`](../packages/site-loop-mcp/test/site-loop-production-e2e.test.ts) runs only with `NARADA_E2E_SITE_LOOP_PRODUCTION=1` and `NARADA_E2E_SITE_LOOP_SITE_ROOT`; through MCP it proves scheduler status, resident replacement, production proof, recovery drill, recovery plan, and fixture cleanup | complete | not_run |
| P0 | `task-lifecycle-mcp` | Site-bound carrier lifecycle | [`test/site-fabric-lifecycle-e2e.test.ts`](../packages/task-lifecycle-mcp/test/site-fabric-lifecycle-e2e.test.ts) launches the actual child, claims/finishes/reviews a controlled task, and verifies SQLite plus Markdown closure evidence | complete | passed |
| P0 | `mcp-registrar` | Registry-to-live-surface conformance | [`test/site-fabric-loader-e2e.test.ts`](../packages/mcp-registrar/test/site-fabric-loader-e2e.test.ts), [`test/site-fabric-catalog-e2e.test.ts`](../packages/mcp-registrar/test/site-fabric-catalog-e2e.test.ts), and `test/mcp-registrar.test.ts` prove live child handoff, the complete 26-entry catalog sweep, and drift checks | complete | passed |
| P0 | `mcp-loader-mcp` | Runtime attachment workflow | [`test/site-fabric-loader-e2e.test.ts`](../packages/mcp-registrar/test/site-fabric-loader-e2e.test.ts) plus `test/mcp-loader-mcp.test.ts` attach/call/status/detach and replace/drift behavior through real children | complete | passed |
| P0 | `launcher-mcp` | Launcher-to-carrier inheritance | [`test/pc-host-launcher-lifecycle-e2e.test.ts`](../packages/launcher-mcp/test/pc-host-launcher-lifecycle-e2e.test.ts) starts the canonical launcher twice with explicit launch-session bindings, drives each session through `nars-session-mcp`, verifies Site-local MCP tool inheritance, runtime/MCP ownership evidence, hidden child posture, restart isolation, and descendant teardown; run only with `NARADA_E2E_PC_HOST_AUTHORITY=1` | complete | passed |
| P0 | `delegated-task-mcp` | Real worker runtime and launcher workflow | [`test/site-fabric-worker-e2e.test.ts`](../packages/delegated-task-mcp/test/site-fabric-worker-e2e.test.ts) proves the B1-B3 Site-fabric boundary; external `NARADA_REPO_ROOT/packages/agent-web-ui/test/live-delegated-task-launcher-e2e.mjs` proves the W1 launcher/carrier/Site-fabric/delegated-task/worker/artifact workflow with a controlled provider (A0); see the [cross-repository contract register](cross-repository-contracts.md#contract-register) | complete | passed |
| P0 | `worker-delegation-mcp` | Provider/cognition binding and real carrier | [`test/site-fabric-provider-e2e.test.ts`](../packages/worker-delegation-mcp/test/site-fabric-provider-e2e.test.ts) proves controlled provider binding; [`test/real-carrier-e2e.test.ts`](../packages/worker-delegation-mcp/test/real-carrier-e2e.test.ts) proves B4 execution through the production NARS carrier and durable artifacts with a controlled provider (A0) | complete | passed |
| P0 | `worker-delegation-mcp` | External provider authority | [`test/external-provider-e2e.test.ts`](../packages/worker-delegation-mcp/test/external-provider-e2e.test.ts) executes the real child and records the controlled provider prerequisites; the current authority result is `not_run` because no A1/A2 provider authority was supplied | complete | not_run |
| P1 | `local-filesystem-mcp` | Real governed filesystem child | [`test/site-fabric-filesystem-e2e.test.ts`](../packages/local-filesystem-mcp/test/site-fabric-filesystem-e2e.test.ts) proves child read/write/range/stat/glob, allowed-root refusal, bounded line windows, and deterministic blocked-read timeout handling | complete | passed |
| P1 | `structured-command-mcp` | Real command policy boundary | [`test/site-fabric-command-e2e.test.ts`](../packages/structured-command-mcp/test/site-fabric-command-e2e.test.ts) proves child argv execution, bounded output, timeout, refusal classification, and no shell escape | complete | passed |
| P1 | `git-mcp` | Real repository publication and branch lifecycle path | [`test/site-fabric-git-e2e.test.ts`](../packages/git-mcp/test/site-fabric-git-e2e.test.ts) proves child status/diff/local and remote branch listing/create/switch/rename/merged-only local and remote deletion/unmerged remote-deletion refusal/upstream configuration/add/commit/push/log, broad-path refusal, disposable remote, and cleanup | complete | passed |
| P1 | `site-inbox-mcp` | Real Site inbox bridge | [`test/site-fabric-inbox-e2e.test.ts`](../packages/site-inbox-mcp/test/site-fabric-inbox-e2e.test.ts) proves child admission, durable envelope state, acknowledgment, audit, and repeated-ack behavior | complete | passed |
| P1 | `mailbox-mcp` | Real synced mailbox projection | [`test/site-fabric-mailbox-e2e.test.ts`](../packages/mailbox-mcp/test/site-fabric-mailbox-e2e.test.ts) proves child projection discovery, bounded message/thread reads, output paging, and foreign-root isolation | complete | passed |
| P0 | `graph-mail-mcp` | Real Graph delegated auth and mail lifecycle | [`test/site-fabric-graph-mail-e2e.test.ts`](../packages/graph-mail-mcp/test/site-fabric-graph-mail-e2e.test.ts) executes the real child, tools/list, doctor, and bounded lifecycle path; the current authority result is `not_run` because no controlled Graph authority was supplied | complete | not_run |
| P1 | `calendar-mcp` | Real Graph calendar lifecycle | [`test/site-fabric-calendar-e2e.test.ts`](../packages/calendar-mcp/test/site-fabric-calendar-e2e.test.ts) executes the real child, tools/list, doctor, and bounded calendar path; the current authority result is `not_run` because no controlled Graph calendar authority was supplied | complete | not_run |
| P1 | `speech-mcp` | Real remote speech path | [`test/site-fabric-speech-e2e.test.ts`](../packages/speech-mcp/test/site-fabric-speech-e2e.test.ts) executes the real child, registry, policy, and listen path; the current authority result is `not_run` because remote provider egress, credentials, and a controlled capture adapter were not supplied | complete | not_run |
| P1 | `scheduler-mcp` | Real Windows Task Scheduler boundary | [`test/site-fabric-scheduler-e2e.test.ts`](../packages/scheduler-mcp/test/site-fabric-scheduler-e2e.test.ts), run with `--pc-host-authority` through `test:e2e:host`, registers/runs/inspects/history-reads/deletes one uniquely scoped disposable task and verifies action completion plus full cleanup | complete | passed |
| P1 | `agent-context-mcp` | Real startup hydration | [`test/site-fabric-agent-context-e2e.test.ts`](../packages/agent-context-mcp/test/site-fabric-agent-context-e2e.test.ts) launches a Site-bound child, hydrates, persists checkpoints, reads bounded output pages, and verifies roster/root authority | complete | passed |
| P1 | `sop-mcp` | Real SOP durable run | [`test/site-fabric-sop-e2e.test.ts`](../packages/sop-mcp/test/site-fabric-sop-e2e.test.ts) starts/advances a controlled run, restarts the child, resumes it, and verifies durable events and operator gates | complete | passed |
| P1 | `surface-feedback-mcp` | Real feedback-to-task fabric | [`test/site-fabric-feedback-e2e.test.ts`](../packages/surface-feedback-mcp/test/site-fabric-feedback-e2e.test.ts) submits through a child, converts through its nested task-lifecycle child, retries idempotently, and reads durable task/handoff state | complete | passed |
| P1 | `artifacts-mcp` | Real artifact registration/readback | [`test/site-fabric-artifacts-e2e.test.ts`](../packages/artifacts-mcp/test/site-fabric-artifacts-e2e.test.ts) registers/reads/presents through the child and controlled NARS HTTP authority; real NARS carrier authority is explicitly PC-host excluded | complete | excluded_pc_host |
| P1 | `nars-session-mcp` | Existing NARS session control | [`test/site-fabric-session-e2e.test.ts`](../packages/nars-session-mcp/test/site-fabric-session-e2e.test.ts) proves Site-bound discovery, health readback, and stale/missing-event refusal; real existing NARS session control is PC-host excluded | complete | excluded_pc_host |
| P2 | `runtime-introspection-mcp` | Real runtime trace | [`test/site-fabric-introspection-e2e.test.ts`](../packages/runtime-introspection-mcp/test/site-fabric-introspection-e2e.test.ts) proves bounded child trace analysis/readback; live launcher/carrier process authority is PC-host excluded | complete | excluded_pc_host |
| P2 | `operator-routing-mcp` | Real operator routing | [`test/site-fabric-routing-e2e.test.ts`](../packages/operator-routing-mcp/test/site-fabric-routing-e2e.test.ts) submits a controlled transcript, records the Site-inbox fallback, persists the route log, and verifies unsupported carrier behavior truthfully | complete | passed |
| P2 | `cloudflare-carrier-mcp` | Real Cloudflare carrier | [`test/site-fabric-cloudflare-e2e.test.ts`](../packages/cloudflare-carrier-mcp/test/site-fabric-cloudflare-e2e.test.ts) proves the bounded A0 child/health/product contract; [`test/cloudflare-carrier-live-e2e.test.ts`](../packages/cloudflare-carrier-mcp/test/cloudflare-carrier-live-e2e.test.ts) starts the production MCP child against the real Worker, verifies authenticated site/operation reads, unauthorized refusal, bounded cleanup, and explicit `not_run` readiness evidence | complete | not_run |
| P2 | `site-coherence-mcp` | Cross-embodiment coherence | [`test/site-fabric-coherence-e2e.test.ts`](../packages/site-coherence-mcp/test/site-fabric-coherence-e2e.test.ts) proves the real child and local/Cloudflare comparison contract; the real Cloudflare authority result is `not_run` | complete | not_run |
| P2 | `site-lifecycle-mcp` | Real Site lifecycle | [`test/site-fabric-lifecycle-e2e.test.ts`](../packages/site-lifecycle-mcp/test/site-fabric-lifecycle-e2e.test.ts) launches the real child, creates a disposable Site through the real CLI, verifies config durability and truthful doctor output, and cleans the Site | complete | passed |
| P2 | `site-registry-mcp` | User Site registry workflow | [`test/site-fabric-registry-e2e.test.ts`](../packages/site-registry-mcp/test/site-fabric-registry-e2e.test.ts) launches the real child and exercises the complete read/list/show/discovery-plan catalog against a disposable User Site registry root | complete | passed |

## Current Batch Evidence

The latest bounded non-PC-host execution manifest is [`20260712-non-pc-host-batch.json`](mcp-e2e-evidence/20260712-non-pc-host-batch.json).
It links each newly added or strengthened test to its execution reference and
package-local result artifact. The manifest records external authority gaps as
`not_run`; it does not promote them to passing evidence.

The PC-host launcher verification passed on 2026-07-13 with two launches and
the explicit `--pc-host-authority` opt-in. Its result artifact is
`packages/launcher-mcp/.tmp-tests/e2e-results/launcher-mcp-pc-host-lifecycle-e2e.json`
(generated output is ignored). Both launches reported healthy runtime health,
complete ownership evidence with no validation errors, one inherited fixture
tool request, zero descendants after close, and successful temporary-root
cleanup. The same test without the opt-in produced an explicit `not_run` result.

The Scheduler PC-host verification passed on 2026-07-13 with the explicit
`--pc-host-authority` opt-in. Its result artifact is
`packages/scheduler-mcp/.tmp-tests/e2e-results/scheduler-mcp-pc-host-lifecycle-e2e.json`
(generated output is ignored). The scheduled task returned history result `0`,
the fixture recorded started and completed timestamps, and cleanup proved task
absence, fixture-process exit, Scheduler MCP exit, command-file removal, and
temporary-root removal. The default invocation produced an explicit `not_run`
artifact with `cleanup.status=not_required`.

## Required Test Artifacts

Every new real E2E test must provide:

- A package-local test and a root command discoverable from `package.json`.
- An explicit authority matrix: credentials, Site root, carrier, provider, and
  destructive-operation policy.
- A bounded setup and teardown protocol. Orphaned child processes, scheduler
  tasks, mailbox drafts, files, and temporary Sites are failures.
- A structured result artifact with `passed`, `failed`, or `not_run`; skipped
  prerequisites must include a machine-readable reason.
- At least one negative assertion for the governing refusal or boundary.
- A link from the corresponding debt row to the test and the implementing task.

The non-PC-host implementation debt is closed: every non-PC-host row has a
real bounded test, durable evidence, cleanup evidence, and a reviewed
transition to `Debt status: complete`. External rows with `Authority result:
not_run` remain explicit authority gaps and are not passing evidence. They do
not get silently promoted by a local fixture. PC-host authority remains
excluded by scope.

Host-authority tests are opt-in when they create or remove host state. The
Scheduler proof is exposed as `pnpm run test:scheduler:e2e:host`; the ordinary
package test does not mutate Windows Task Scheduler. External-authority rows remain open until a controlled tenant,
provider account, carrier session, or production Site is explicitly supplied.
A local HTTP fixture may prove the child/protocol contract, but it must not
promote a Graph, speech, NARS, Cloudflare, or production Site claim to
`complete`.
