# Agent Tool Ergonomics Review

Status: normative repository process.

The Agent Tool Ergonomics Review (ATER) is the required end-to-end assessment
process for MCP tools whose discovery, arguments, execution, result projection,
continuation, or recovery materially affects agent task completion. It combines
service assessment, cognitive task walkthrough, human-factors use-error
analysis, and executable contract verification.

ATER evaluates a tool as a service journey:

```text
intent
  -> discovery
  -> selection
  -> argument construction
  -> execution
  -> result projection
  -> interpretation
  -> continuation or recovery
  -> verified task outcome
```

A technically valid response is not sufficient. The journey must remain
discoverable, bounded, interpretable, recoverable, and safe for both the model
consuming the MCP result and the operator observing the carrier UI.

## When ATER is required

Run an ATER when:

- a new surface or materially new tool is introduced;
- a tool can return variable or large output;
- a tool introduces paging, output references, caching, snapshots, leases, or
  lifecycle state;
- several tools form one ordinary user journey;
- agents repeatedly select the wrong tool or construct the wrong arguments;
- result projection differs between carriers;
- an incident reveals context flooding, silent truncation, unsafe retry,
  ambiguous success, or inaccessible results;
- a compatibility change modifies defaults, result states, continuation, or
  recovery behavior.

Small schema-compatible corrections may use a focused ATER covering only the
affected journeys. A focused review still requires a task, baseline, hazard
disposition, target contract, and executable acceptance evidence.

## Review authority and roles

The surface owner owns preparation and remediation. Assessment should include
perspectives independent of the implementation.

Required roles:

- **service owner** — owns user outcomes and accepts residual ergonomic risk;
- **surface implementer** — explains the implementation and performs repairs;
- **agent consumer** — evaluates model-facing discovery and task completion;
- **transport or carrier reviewer** — evaluates projection, context, and UI
  behavior;
- **authority and safety reviewer** — evaluates foreseeable misuse, mutation,
  retry, and provenance;
- **assessor** — records the decision and may not rely solely on implementer
  judgment.

One person may fill several roles for a focused review, but the implementer
must not be the sole assessor for a new surface or a high-severity incident.

## Required inputs

The review starts from evidence:

- current tool schemas and descriptions;
- representative real transcripts;
- defects, refusals, and operator feedback;
- carrier-visible and model-visible result projections;
- implementation limits and transport limits;
- existing tests and compatibility commitments;
- authority, mutation, and retry semantics.

Do not begin by redesigning the schema. First establish what users attempt,
where the current journey fails, and how much interaction and context it costs.

## Phase 1: Charter

Create a review charter containing:

- surface and tools in scope;
- user classes and carriers in scope;
- authority boundaries;
- known incidents;
- dependencies and adjacent surfaces;
- explicit non-goals;
- assessment owner and participants.

The default user classes are:

- a first-time agent;
- an experienced agent;
- a programmatic MCP client;
- an operator reading collapsed and expanded carrier output;
- a maintainer diagnosing a failure.

## Phase 2: Canonical task corpus

Define concrete tasks with implementation-independent success conditions.
Cover, where applicable:

- ordinary high-frequency work;
- ambiguous discovery or selection;
- empty results;
- malformed input and refusal;
- timeout or unavailable dependency;
- large result sets;
- one individually large result;
- paging and continuation;
- restart, cache, snapshot, or generation boundaries;
- mutation success and partial failure;
- recovery without unsafe repetition.

Use realistic paths, payload sizes, and data distributions. Include incidents
as permanent regression tasks. A task must say what evidence constitutes
success; “the tool returned ok” is not a success condition.

## Phase 3: Journey reconstruction

For each task, record:

| Stage | Required evidence |
| --- | --- |
| Discovery | Tools, descriptions, and schemas visible to the agent |
| Selection | Why the chosen tool and mode appear appropriate |
| Construction | Supplied fields and information the surface could derive |
| Execution | Cost, authority, timeout, and producer bounds |
| Projection | Model-visible, UI-visible, and materialized content |
| Interpretation | How complete, empty, partial, refused, and failed differ |
| Continuation | Exact next call and stability of its state |
| Outcome | Evidence that the user task was completed correctly |

Measure the whole journey, including inspection, lease, reader, and recovery
calls. Moving cost into a helper call does not remove it.

## Phase 4: Cognitive walkthrough

Reviewers execute each task from the perspective of a first-time user and ask
at every step:

1. Is the intended outcome expressible?
2. Is the correct action discoverable?
3. Are required arguments understandable and minimal?
4. Does feedback explain what happened?
5. Can the user distinguish complete, partial, empty, refused, and failed?
6. If more work is required, is the next action explicit and executable?
7. Can a foreseeable mistake create excessive cost, context use, mutation, or
   unsafe repetition?

Record the first point of failure. Do not explain the interface to the
walkthrough participant; required explanation is evidence of a discoverability
defect.

## Phase 5: MCP ergonomics heuristics

Score each applicable heuristic from 0 to 3:

- **0 — absent:** ordinary use is unsafe or materially blocked;
- **1 — weak:** recovery requires guesswork or specialist knowledge;
- **2 — adequate:** ordinary work succeeds with bounded friction;
- **3 — strong:** behavior is self-evident, economical, and mechanically
  recoverable.

### Discoverability

- Names describe user intent rather than implementation machinery.
- Descriptions explain when to use the tool and when not to use it.
- The ordinary path does not require bulk schema discovery.
- Related tools form a visible, consistent workflow.

### Argument economy

- Conservative defaults complete the common task.
- The caller supplies only information the surface cannot safely derive.
- Repeated scope, authority, lease, paging, and projection data is minimized.
- Modes are few, coherent, and mutually exclusive where appropriate.

### Output economy

- Model-visible output has a small default character or byte budget.
- Result-count limits are not treated as serialized-size limits.
- One datum is not duplicated in human and machine projections.
- Ordinary results omit runtime diagnostics.
- Complete results remain recoverable when inline output is bounded.

### State legibility

- Success, empty, partial, truncated, refused, timed out, and failed are
  mechanically distinct.
- Exact and estimated counts are distinguished.
- Mutation responses include durable evidence and recovery state.
- Cache, snapshot, freshness, and generation details appear when they affect
  interpretation, not as unconditional noise.

### Error prevention

- Broad or expensive requests have conservative defaults and hard bounds.
- Scope is visible before or in the compact result.
- Long individual values are bounded independently of collection size.
- A compact UI renderer is not used as a substitute for context containment.

### Continuation and recovery

- Continuation is structured and directly callable.
- Output references are immutable within their declared scope.
- The exact reader tool and arguments are supplied.
- Retry safety and idempotency are explicit.
- Recovery does not require rerunning a state-changing operation.

### Consistency

- Adjacent tools share naming, paging, result-state, and output-ref conventions.
- Carrier projections preserve authoritative semantics.
- Collapsed UI, expanded UI, and model content intentionally differ only in
  presentation and boundedness.

### Trust and provenance

- Scope, authority, freshness, and completeness support the conclusion drawn.
- Human summaries are not presented as authoritative machine data.
- Transport does not imply mutation or identity authority.
- Results needed as evidence have stable identifiers or digests.

Any score of 0 is release-blocking. A score of 1 requires an explicit repair or
accepted residual-risk disposition. Aggregate scores do not cancel a severe
defect.

## Phase 6: Use-error and hazard analysis

Record foreseeable misuse and adverse outcomes:

| Field | Meaning |
| --- | --- |
| Hazard | Undesired interaction state |
| Initiating condition | Data, request, or runtime condition |
| Foreseeable action | Plausible agent or operator behavior |
| Local effect | Immediate tool or carrier consequence |
| Downstream effect | Wrong conclusion, unsafe mutation, lost evidence, or cost |
| Detectability | Whether the user can recognize the condition |
| Existing control | Current prevention or containment |
| Required control | Producer, transport, carrier, or documentation repair |
| Verification | Executable acceptance evidence |

Assess severity using the worst credible effect across:

- unsafe or repeated mutation;
- inaccessible execution evidence;
- false task completion;
- silent omission or truncation;
- model-context exhaustion;
- connection or process exhaustion;
- operator misinterpretation;
- latency and unnecessary calls.

Renderer collapse is containment for the operator display only. It does not
control model-visible context. Producer bounds, transport materialization, and
carrier projection are separate controls and must be assessed separately.

## Phase 7: Baseline

Measure each canonical task before redesign:

- successful completion;
- correct-first-call rate;
- total MCP calls;
- discovery or inspection calls;
- model-visible characters;
- largest single result;
- duplicate-data ratio;
- time to first actionable evidence;
- continuation and recovery success;
- silent-truncation and incorrect-completion incidence;
- fields supplied by the caller that could be derived.

“Model-visible characters per correctly completed task” is a mandatory metric
for agent-facing tools.

## Phase 8: Target interaction contract

Define the target before changing implementation:

- user jobs and non-goals;
- ordinary and advanced call shapes;
- defaults and hard ceilings;
- authoritative result representation;
- compact model projection;
- collapsed and expanded UI projection;
- result state machine;
- per-item and per-response size bounds;
- paging and continuation semantics;
- output materialization and reader semantics;
- error and refusal taxonomy;
- mutation, retry, and idempotency behavior;
- compatibility and migration policy;
- telemetry needed to reassess ergonomics.

Every potentially large tool must declare:

1. default inline character or byte budget;
2. hard producer capture budget;
3. per-item clipping behavior;
4. continuation behavior;
5. full-result recovery behavior;
6. model-visible projection behavior.

Names such as `capped`, `bounded`, or `compact` must correspond to enforced,
tested behavior.

## Phase 9: Prototype comparison

For material contract changes, compare at least two plausible interaction
designs against the same task corpus. Examples include page-oriented results,
immutable result references, or a compact ordinary tool plus a diagnostic
reader.

Choose using measured task completion, context cost, ambiguity, recovery, and
compatibility—not schema elegance alone.

## Phase 10: Validation

Validation uses:

- deterministic task-corpus tests;
- several model behaviors or agent strategies where interpretation matters;
- operator review of collapsed and expanded carrier output;
- restart, timeout, and capacity boundary tests;
- realistic normal and pathological data.

Required automated gates, when applicable:

- maximum model-visible character test;
- maximum serialized producer response test;
- no-duplicate-authoritative-projection test;
- per-item clipping test;
- page reconstruction or output-ref reconstruction test;
- continuation stability test;
- empty/partial/truncated/refused/failed distinction test;
- mutation receipt and safe-recovery test;
- collapsed and expanded carrier rendering test;
- restart and generation-boundary test;
- cross-client semantic parity test.

Tests must assert both presence of actionable evidence and absence of oversized
or duplicated content.

## Phase 11: Assessment

The assessor reviews the charter, task evidence, scorecard, hazard register,
target contract, implementation, and test evidence.

Decisions:

- **pass** — no release-blocking defects and all required gates pass;
- **conditional pass** — bounded non-severe follow-ups have owners and dates;
- **reassessment required** — task completion, safety, boundedness, or recovery
  remains materially defective.

The assessment report contains:

- scope and participants;
- tested versions and carriers;
- task results and baseline comparison;
- unresolved scores of 1;
- residual hazards and owner decisions;
- decision and required follow-ups.

## Phase 12: Live reassessment

Ergonomics is a live service property. After release:

- retain incident-derived tasks permanently;
- monitor result sizes, continuation use, refusals, retries, and abandonment;
- review agent and operator feedback;
- repeat benchmarks after material changes;
- reopen ATER when live evidence contradicts the assessment.

## Required artifact set

A complete ATER produces:

1. review charter;
2. user classes and canonical task corpus;
3. current journey traces;
4. incident inventory;
5. heuristic scorecard;
6. use-error and hazard register;
7. baseline measurements;
8. target interaction contract;
9. prototype comparison, when required;
10. compatibility and migration plan;
11. acceptance-test matrix and results;
12. assessment report;
13. live reassessment plan.

These may be sections of one document for a focused review. Evidence must be
repository-addressable and tests must live with the owning package.

## Pull-request checklist

- [ ] ATER scope is identified.
- [ ] Canonical tasks have implementation-independent success conditions.
- [ ] Current and target journeys are documented.
- [ ] Model-visible and UI-visible output are measured separately.
- [ ] Foreseeable misuse and unsafe retry are assessed.
- [ ] Defaults and all output bounds are explicit.
- [ ] Large results have continuation or materialized recovery.
- [ ] Authoritative data is not duplicated.
- [ ] Required acceptance gates pass.
- [ ] Compatibility and rollout are addressed.
- [ ] An assessor records pass, conditional pass, or reassessment required.

## Initial application

`fs_grep_search` is the first full ATER application. Its review must include:

- broad common-pattern searches;
- very long matching lines;
- duplicate human and structured matches;
- independent result-count and serialized-size limits;
- compact ordinary metadata and explicit diagnostics;
- continuation and immutable full-result recovery;
- model-visible output in Pi and at least one non-Pi client;
- empty, partial, timeout, invalid-pattern, and restart behavior.

The Pi model-context cap is a final containment control. It does not discharge
the filesystem producer's obligation to provide economical authoritative
results and complete recovery.

The current assessment and excellent-AX target are recorded in
`docs/ater/fs-search-excellent-ax.md`.
