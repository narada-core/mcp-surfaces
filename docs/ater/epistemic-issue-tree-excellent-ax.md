# Epistemic Issue Trees: Excellent Agent Experience

Status: Excellent AX verified by the executable A1–A24 acceptance gates on
2026-08-31. The machine-readable assessment is
[`epistemic-issue-tree-excellent-ax.verification.json`](epistemic-issue-tree-excellent-ax.verification.json).

Assessment target: the `epistemic-graph` issue-tree workflow, currently exposed
through `epistemic_graph_issue_tree_frontier` and
`epistemic_graph_issue_tree_transition` by the generic ledger-domain engine and
its epistemic domain descriptor.

This document applies the
[Agent Tool Ergonomics Review](../agent-tool-ergonomics-review.md). It defines
excellent agent experience as an observable service outcome:

> An agent can identify or create the issue tree for an objective, resume its
> unique selected leaf, record a tested disposition, and select the next
> bounded frontier item without reconstructing graph conventions, flooding
> context, repeating mutation, or treating coordination state as evidence.

Issue trees are coordination state. Scores, selection, graph admission, and
communications do not certify scientific or operational truth.

## 1. Charter

### Scope

- issue-tree discovery, creation, resume, traversal, and transition;
- active-leaf identity and version semantics;
- score ordering, hierarchy, blockers, evidence links, and dispositions;
- compaction and session-restart rehydration;
- concurrent writers, retry safety, and admission uncertainty;
- producer, transport, model-visible, and carrier-visible projections;
- compatibility for the current frontier and transition tools.

### Users and carriers

- first-time coding or research agent;
- experienced agent maintaining a long-running programme;
- programmatic MCP client;
- operator reading collapsed and expanded Pi output;
- maintainer diagnosing event-ledger or projection behavior.

### Authority boundary

The epistemic graph owns append-only coordination records under its admitted
Site authority. Tree access does not imply agent identity, filesystem access,
Git authority, task ownership, or scientific evidence. `evidence_ids` are
provenance links; their presence neither validates the linked object nor
promotes the issue node to evidence.

### Dependencies

- ledger-domain-epistemic descriptor;
- ledger-domain-mcp engine;
- event-ledger-native admission, head-CAS, idempotency, and projection;
- MCP transport and carrier projection;
- optional client compaction or traversal extensions.

### Non-goals

- storing full conversational history or chain-of-thought;
- replacing compaction, task lifecycle, Git, or artifacts;
- automatic scientific claim promotion;
- selecting work across unrelated Site authorities;
- unbounded graph export.

### Roles

- service owner: epistemic-graph domain owner;
- implementer: ledger-domain engine or descriptor maintainer;
- agent consumer: independent first-use reviewer;
- transport reviewer: Pi plus one non-Pi client reviewer;
- authority reviewer: event-ledger/admission reviewer;
- assessor: reviewer independent of sole implementation judgment.

## 2. Current public contract

### Frontier

```json
{
  "tree_id": "string",
  "limit": 20
}
```

`epistemic_graph_issue_tree_frontier` returns a bounded score-ordered frontier
of non-disposed, non-superseded issues. The requested limit is 1 through 100.

### Transition

`epistemic_graph_issue_tree_transition` requires:

```json
{
  "actor": "agent-id",
  "authority_basis": {},
  "tree_id": "tree-id",
  "nodes": []
}
```

Optional mutation controls are `idempotency_key` and `expected_ledger_head`.
Each node has a stable caller-supplied `node_id`, title, positive version,
optional parent and predecessor, state, score, disposition, rationale,
blockers, and evidence links.

Current state vocabulary:

```text
active | blocked | disposed
```

Current disposition vocabulary:

```text
resolved | rejected | deferred | superseded | split
```

Current score range is 0 through 1.

## 3. Canonical task corpus

| ID | Task | Implementation-independent success condition |
| --- | --- | --- |
| I1 | Resume a known tree | Unique tree identity, selected leaf, version, and bounded frontier are returned |
| I2 | Resume from an objective without a tree ID | One unambiguous tree is resolved or ambiguity is typed without mutation |
| I3 | Create a tree for a new objective | Root and initial leaves are admitted once with a durable receipt |
| I4 | Read an empty tree | Empty success differs from missing, refused, stale, and failed |
| I5 | Select the highest-scoring open leaf | Selection uses a complete ordered frontier or states its bounded basis |
| I6 | Resume a unique active leaf | The same leaf and version survive session compaction and client restart |
| I7 | Record completion | The prior version is preserved and the successor is disposed/resolved |
| I8 | Record falsification | Rejected disposition and evidence links are durable without truth certification |
| I9 | Record exhaustion | Exhaustion has an explicit mapping rather than being silently coerced to completion |
| I10 | Block a leaf | Blocker identities are visible and the next selectable leaf is explicit |
| I11 | Split a leaf | Children, predecessor, hierarchy, and disposition are coherent |
| I12 | Handle tied scores | Ordering and tie-breaking are stable and declared |
| I13 | Read more than 100 open leaves | Completeness, omitted count, and direct continuation are explicit |
| I14 | Read one oversized node | Per-field clipping and full recovery are explicit |
| I15 | Concurrent transitions | One transition wins or both compose without silent lost update |
| I16 | Timeout after possible admission | The client reconciles outcome without repeating mutation unsafely |
| I17 | Retry the same transition | Idempotency replay returns the original durable outcome |
| I18 | Child generation replacement | Stable resume/read behavior or typed expiry is returned |
| I19 | Compact conversation then resume | Tree state is rehydrated from a compact pointer without copied chronology |
| I20 | Graph unavailable | Transient work is not presented as durably transitioned |
| I21 | Pi collapsed and expanded projections | Collapsed output is concise and expanded output preserves bounded semantics |
| I22 | Non-Pi client parity | Authoritative nodes, states, ordering, and completeness match Pi |
| I23 | Evidence-link misuse | The result explicitly prevents treating links or admission as evidential force |
| I24 | Invalid score, version, parent, or predecessor | Refusal names the exact defect and correction |

Incident-derived tasks remain permanent regression cases.

## 4. Current journey and baseline gaps

The observed first-use journey is:

```text
list or discover epistemic tools
  -> infer that issue trees exist
  -> inspect frontier and transition schemas
  -> already know or invent tree_id
  -> call frontier
  -> infer selected leaf from returned states
  -> manually map client statuses and /10 scores to graph vocabulary
  -> construct append-only successor nodes
  -> reconcile admission and repeat frontier
```

Known friction:

- frontier requires `tree_id`, but no intent-oriented list, resolve, or resume
  tool is exposed;
- no ordinary create/open call explains whether transition creates a root;
- `active` conflates selectable open issues with the uniquely selected leaf;
- client vocabularies such as `open`, `completed`, `falsified`, and `exhausted`
  do not map mechanically to graph state and disposition;
- client display scores out of 10 differ from graph scores in `[0,1]`;
- frontier accepts at most 100 items but the inspected input contract exposes
  no continuation;
- completeness, total count, tie-breaking, and snapshot stability are not
  established by the tool description;
- mutation recovery requires specialist knowledge of idempotency, ledger-head
  CAS, versions, and proposal admission;
- ordinary output size, per-node clipping, and model-visible budgets are not
  yet measured.

A complete baseline must measure I1 through I24 before implementation changes.
Mandatory metrics are successful completion, correct-first-call rate, total
calls, inspection calls, model-visible characters, largest result, duplicate
ratio, time to actionable leaf, retry/recovery success, and incorrect
completion incidence.

## 5. Cognitive walkthrough findings

### Discovery

A first-time user can discover frontier and transition only after tool listing
or guidance. Tool names describe tree mechanics but do not expose the common
jobs “resume objective” or “record disposition.”

### Construction

The caller supplies tree identity, node identity, versions, state/disposition
mapping, normalized score, authority basis, and mutation concurrency controls.
Several fields are necessary for advanced mutation but should be derivable on
the ordinary resume-and-transition path.

### Feedback

Append-only admission and typed fields support trust, but the ordinary response
must identify the selected leaf, durable event, resulting version, and exact
next call. `ok` alone does not establish traversal completion.

### Continuation

A bounded frontier without explicit completeness and a directly callable
continuation cannot justify global highest-score selection. Reissuing a live
query is not equivalent to continuing a captured ordered frontier.

### Error prevention

A plausible client may submit score `9.3` instead of `0.93`, mark all open
nodes `active`, reuse a node version incorrectly, or repeat a transition after
an ambiguous timeout. Schema refusal contains some mistakes; workflow-level
controls are still required.

## 6. Heuristic scorecard

Provisional scores based on the inspected contract, pending canonical-task
measurement:

| Heuristic | Score | Finding |
| --- | ---: | --- |
| Discoverability | 1 | No objective-oriented resolve/resume workflow |
| Argument economy | 1 | Ordinary callers coordinate IDs, versions, vocabulary, authority, and CAS |
| Output economy | 1 | Bounded item count exists; serialized and per-node budgets are undeclared |
| State legibility | 1 | Append-only state is typed, but selected-leaf and client-status mappings are ambiguous |
| Error prevention | 2 | Schemas bound scores, versions, arrays, and enums; workflow misuse remains plausible |
| Continuation and recovery | 1 | Idempotency/CAS exist, but frontier paging and timeout reconciliation are not ordinary-path contracts |
| Consistency | 1 | Scores and statuses differ from common traversal clients |
| Trust and provenance | 3 | Authority, append-only history, blockers, evidence links, and nonpromotion boundary are strong |

Disposition: **superseded baseline**. These provisional scores describe the
pre-repair contract. The executable reassessment in section 16 is authoritative
for the implemented target interaction.

## 7. Use-error and hazard register

| ID | Hazard | Initiating condition and foreseeable action | Downstream effect | Required control |
| --- | --- | --- | --- | --- |
| H1 | Wrong tree resumed | Objective is ambiguous; agent invents tree ID | Work applied to unrelated programme | Typed resolve result and ambiguity refusal |
| H2 | Duplicate trees | No discover/create journey; every session creates another root | Fragmented coordination and repeated rediscovery | Atomic open-or-create keyed by objective identity |
| H3 | Multiple selected leaves | `active` means both open and selected | Parallel work mistaken for one depth-first path | Explicit `selected_node_id` or selected state |
| H4 | Incomplete ranking | Frontier exceeds limit without completeness | Lower observed score selected while higher omitted | Captured ordered continuation and page contract |
| H5 | Score scale error | Client submits display score 9.3 | Refusal or inconsistent scoring | Declared canonical scale and client conversion metadata |
| H6 | Disposition coercion | Exhausted/falsified have no direct mapping | False completion or lost negative result | Explicit traversal-status mapping or expanded enum |
| H7 | Unsafe retry | Timeout follows possible admission | Duplicate successor or version conflict | Mandatory idempotency and outcome reconciliation |
| H8 | Lost concurrent update | Two agents transition same predecessor | Hidden branch or overwritten active pointer | Head/version CAS and typed conflict recovery |
| H9 | Context flooding | Many nodes or long rationale/evidence arrays | Compaction pressure and delayed selection | Producer byte budget and per-field clipping |
| H10 | Admission treated as evidence | Tree links accepted into graph | Unsupported scientific conclusion | Machine-readable noncertification marker |
| H11 | Compaction duplicates graph | Full frontier copied into every summary | Semantic redundancy and stale state | Pointer-only compaction contract |
| H12 | Graph outage smeared as success | Transition cannot be confirmed | Work claimed durable when only conversational | Distinct local-pending and graph-admitted states |
| H13 | Restart breaks continuation | Process-local cursor or projection state disappears | Requery observes changed order | Durable scoped capture or explicit expiry |
| H14 | Oversized evidence IDs | Maximum arrays and long identifiers | Large transport result | Compact IDs, field budgets, materialized details |

## 8. Target interaction contract

### User jobs

The public workflow should expose three intent-oriented jobs:

1. resume or open a tree for an objective;
2. read or continue its bounded frontier;
3. transition the selected node with a tested disposition.

Advanced generic graph proposal tools remain available but are not required for
ordinary traversal.

### Preferred tools

- `epistemic_graph_issue_tree_resume`
- `epistemic_graph_issue_tree_frontier_read`
- `epistemic_graph_issue_tree_transition`

The existing frontier tool may serve as a compatibility adapter if its result
contract is upgraded without silently changing successful legacy semantics.

### Resume call

```json
{
  "tree_id": "optional-known-id",
  "objective": "optional objective text",
  "create_if_missing": false,
  "max_frontier_items": 20,
  "max_inline_chars": 6000
}
```

Rules:

- exactly one of known `tree_id` or objective resolution is ordinarily needed;
- if both are supplied, objective must match the resolved tree;
- ambiguity returns candidates without mutation;
- creation requires `create_if_missing: true`, actor, and authority basis;
- ordinary reads do not require mutation authority fields;
- the response identifies a unique selected node or explicitly reports none.

### Resume result

```json
{
  "schema": "narada.epistemic.issue-tree.resume.v1",
  "status": "ok",
  "tree": {
    "tree_id": "tree:cosmology-tau",
    "objective": "Construct or reject the sourced tau comparison",
    "version": 12
  },
  "selected": {
    "node_id": "phys-dual",
    "version": 2,
    "title": "Test physical dual annihilation",
    "state": "selected",
    "score": 0.9
  },
  "frontier": {
    "items": [],
    "returned": 5,
    "complete": true,
    "total": 5,
    "total_exact": true,
    "ordering": "score_desc_then_stable_id",
    "captured_at_event": "ev-..."
  },
  "continuation": null,
  "result_ref": null,
  "noncertification": "coordination state; not evidence"
}
```

### State model

Target traversal vocabulary:

```text
open | selected | blocked | disposed
```

Target dispositions:

```text
resolved | rejected | exhausted | deferred | superseded | split
```

If the stored domain keeps the current smaller vocabulary, the public tool
must return a lossless explicit mapping. `selected` must be unique per tree
version. A transition that would create two selected leaves is refused or
atomically demotes the predecessor according to a declared rule.

### Scores

Canonical graph scores remain in `[0,1]`. Results include:

```json
{
  "score": 0.93,
  "display_score_out_of_10": 9.3
}
```

The public schema never accepts an ambiguous unlabelled `/10` score.
Tie-breaking is `score descending, then stable node ID` unless a versioned
alternative is declared.

### Transition call

The ordinary transition accepts the intended state change rather than a full
manually assembled proposal:

```json
{
  "actor": "agent-id",
  "authority_basis": {
    "kind": "operator_instruction",
    "summary": "Traverse the issue tree"
  },
  "tree_id": "tree:cosmology-tau",
  "selected_node_id": "phys-dual",
  "expected_node_version": 2,
  "idempotency_key": "stable-operation-key",
  "transition": {
    "disposition": "resolved",
    "rationale": "Both prime checks passed",
    "evidence_ids": ["artifact:receipt"],
    "successors": []
  },
  "select_next": true
}
```

The surface derives successor version, predecessor relation, hierarchy, current
ledger head, and next selection when safe. Advanced callers may continue using
the compact node-batch contract.

### Transition result

The result includes:

- durable event ID and ledger head;
- idempotency replay status;
- prior and resulting selected node/version;
- admitted disposition and evidence links;
- conflict or partial state;
- exact resume call;
- `certifies_truth: false`.

A timeout with unknown admission state returns an immutable operation reference
or a reconciliation call. It never instructs blind mutation retry.

## 9. Frontier paging and materialization

Every potentially large frontier declares:

- default maximum 20 nodes;
- hard inline maximum 100 nodes;
- default complete serialized result at most 6,000 characters;
- hard inline maximum 20,000 characters;
- default title limit 300 characters;
- default rationale excerpt limit 500 characters;
- blocker and evidence summaries bounded independently;
- one authoritative node array;
- immutable captured result when more data exists than fits inline.

Large results return:

```json
{
  "frontier": {
    "items": [],
    "returned": 20,
    "complete": false,
    "total": 143,
    "total_exact": true,
    "inline_chars": 5900,
    "inline_char_limit": 6000
  },
  "continuation": {
    "tool": "epistemic_graph_issue_tree_frontier_read",
    "arguments": {
      "result_ref": "issue-tree-frontier:...",
      "cursor": "..."
    }
  },
  "result_ref": "issue-tree-frontier:..."
}
```

The capture is authority-bound, immutable for its declared lifetime, and
ordered against one ledger event/head. Expiry is typed. Reissuing the live
frontier query is not described as continuation.

## 10. Projection contract

### Model-visible

- default producer result at most 6,000 characters;
- carrier hard containment at most 8,000 characters;
- selected node and actionable frontier first;
- no duplicate prose rendering of authoritative node arrays;
- diagnostics omitted unless they change interpretation.

### Collapsed Pi

```text
issue tree · phys-dual selected · 5 open · version 12
```

### Expanded Pi

Displays bounded selected-node detail, frontier items, completeness, durable
transition receipt, and directly callable continuation. Expansion does not
imply that uncaptured graph data entered model context.

### Diagnostics

On demand only:

- projection rebuild/freshness state;
- capture identity and expiry;
- effective limits;
- ledger-head and version resolution;
- authority and idempotency resolution.

## 11. Compaction integration

Graph state replaces repeated issue-tree narration, not compaction itself.
After compaction the minimum continuity kernel is:

```markdown
## Traversal
- tree_id: tree:cosmology-tau
- selected_node_id: phys-dual
- selected_node_version: 2
- last_transition_event: ev-...
- rehydrate_with: epistemic_graph_issue_tree_resume

## Transient work
- active executions:
- owned uncommitted files:
- reasoning not yet materialized:
```

Do not copy full frontiers, disposed-node chronology, repeated score rationales,
or cumulative read-file inventories into compaction. If the graph is
unavailable, the summary records local pending state without claiming graph
admission.

## 12. Alternatives

### A. Session-local issue tree

Pros: no graph calls and low initial latency.

Cons: weak restart continuity, repeated rediscovery, and summary bloat.

Disposition: unsuitable as canonical state.

### B. Full issue tree copied into compaction

Pros: no runtime graph dependency after compaction.

Cons: stale duplicated state, semantic redundancy, and context growth.

Disposition: reject.

### C. Generic graph query and node-batch transition

Pros: current primitives are expressive and append-only.

Cons: specialist discovery, argument construction, status mapping, and
recovery burden.

Disposition: compatibility/advanced path.

### D. Graph-canonical tree with resume/transition workflow and pointer-only compaction

Pros: durable continuity, compact context, typed mutation, and economical
rehydration.

Cons: requires intent-oriented tools, explicit selected state, paging, and
acceptance work.

Decision: D.

## 13. Compatibility and migration

1. Preserve current frontier and node-batch transition calls.
2. Add target result completeness and bounded projection without widening
   legacy scope.
3. Add resume/open and frontier-reader tools.
4. Add ordinary transition projection that compiles to the canonical proposal
   boundary.
5. Expose explicit state, disposition, and score mappings in guidance.
6. Update traversal clients to persist only tree/node pointers through
   compaction.
7. Monitor generic query and compatibility-tool use before any deprecation.

No legacy successful transition may silently acquire creation authority,
change score meaning, or select a different successor.

## 14. Acceptance matrix

| Gate | Required assertion |
| --- | --- |
| A1 One-call resume | I1 returns selected leaf and bounded frontier without inspection |
| A2 Objective resolution | I2 resolves uniquely or returns typed ambiguity |
| A3 Atomic creation | I3 cannot create duplicate roots under retry/concurrency |
| A4 Producer inline bound | Default complete structured result is at most 6,000 characters |
| A5 Carrier bound | Model-visible result is at most 8,000 characters |
| A6 Per-node bound | Oversized title/rationale/evidence cannot dominate output |
| A7 Sole authority projection | Each node appears once in structured content |
| A8 Completeness | Complete, partial, clipped, expired, refused, and failed differ |
| A9 Direct continuation | Returned continuation is callable without query reconstruction |
| A10 Reconstruction | Reading all pages reproduces the captured frontier exactly |
| A11 Stable ordering | Score ordering and ties are stable within one capture |
| A12 Selected uniqueness | At most one selected leaf exists for each tree version |
| A13 State mapping | All client traversal statuses map losslessly or are refused |
| A14 Score integrity | `[0,1]` and `/10` values cannot be confused |
| A15 Idempotent mutation | Retry returns the original transition outcome |
| A16 Unknown-outcome recovery | Timeout reconciliation never requires blind mutation retry |
| A17 Concurrent writer safety | Version/head conflicts are typed with an executable next action |
| A18 Restart recovery | Resume and captured continuation survive or report declared expiry |
| A19 Compaction rehydration | Pointer-only summary reconstructs the same selected state |
| A20 Noncertification | Every ordinary read/mutation result preserves coordination-only semantics |
| A21 Cross-client parity | Pi and non-Pi clients agree on nodes, states, order, and completeness |
| A22 Collapsed/expanded projection | Both views preserve authoritative result semantics |
| A23 Graph outage | Local pending work is never reported as admitted |
| A24 No rediscovery loop | Median ordinary traversal requires no generic graph query or schema inspection |

Tests assert both presence of actionable evidence and absence of oversized,
duplicated, stale, or authority-smearing content.

## 15. Success measures

Targets across I1 through I24:

- at least 90% correct-first-call completion for ordinary resume and transition;
- median one MCP call to resume known trees;
- median one mutation call plus at most one reconciliation call for transition;
- zero bulk schema discovery on the ordinary path;
- default producer result at most 6,000 characters;
- model-visible result at most 8,000 characters;
- duplicate-data ratio below 2%;
- 100% explicit completeness and selected-node identity;
- 100% safe retained-result and mutation-outcome recovery;
- zero false completion caused by admission ambiguity or frontier truncation;
- zero evidence promotions caused solely by issue-tree state or graph admission.

## 16. Assessment and live reassessment

Current assessment: **Excellent AX verified**.

The black-box `issue_tree_excellent_ax` protocol suite names and executes I1
through I24 individually. It includes concurrent creation and transition
processes, a 143-node captured frontier reconstructed through direct paging,
oversized Unicode fields, process restart, pointer-only rehydration, unknown
outcome reconciliation, graph outage, exact retry, typed invalid-input
correction, and the noncertification boundary. The materialized Pi extension
suite compares its authoritative structured result with non-Pi JSON-RPC data
and verifies concise collapsed plus bounded expanded projections.

The checked-in verification record reports 24 of 24 corpus tasks passing,
100% correct-first-call completion in the measured ordinary journeys, one-call
median resume and mutation paths, zero schema inspections or generic graph
queries, exact completeness, zero duplicated authoritative node arrays, and
zero coordination-driven evidence promotions.

The current primitives establish strong append-only authority and provenance,
but excellent AX additionally requires objective-oriented resume, unique
selected-leaf semantics, explicit status/score mapping, byte-bounded output,
direct frontier continuation, and ordinary-path mutation recovery.

Before changing disposition, the assessor records:

- tested engine and descriptor versions;
- Pi and non-Pi journey traces;
- baseline and target metrics;
- heuristic score updates;
- hazard dispositions;
- acceptance results;
- residual scores of 1 and owners.

After release:

- retain ambiguity, timeout, duplicate-root, frontier-overflow, and compaction
  incidents as regression tasks;
- monitor output size, resume calls, generic-query fallback, idempotency replay,
  conflicts, abandonment, and incorrect-completion reports;
- rerun the corpus after descriptor, engine, projection, or carrier changes;
- reopen ATER when live evidence contradicts the assessment.

## 17. Implementation increments

Recommended order:

1. define selected-leaf and traversal-status semantics in the domain contract;
2. add deterministic objective identity and resolve/open behavior;
3. add `issue_tree_resume` with compact bounded projection;
4. enforce per-node and serialized producer budgets;
5. add captured frontier paging and direct reader continuation;
6. add ordinary transition compilation with mandatory idempotent recovery;
7. add pointer-only compaction integration examples;
8. add Pi and non-Pi canonical-task tests;
9. run the independent assessment and record pass, conditional pass, or
   reassessment required.
