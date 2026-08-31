# Native Epistemic Research Issue Trees

Status: normative implementation contract.

Research issue trees are a typed workflow profile of the epistemic graph, not a
Marici-local document format. A site stores concrete trees; the shared
epistemic descriptor owns their vocabulary and the generic ledger engine owns
validation, atomic admission, and derived frontier reads.

## Model

An issue node is an append-only `research_issue` entity. It carries:

- `tree_id`: stable tree identity;
- `version`: positive integer revision;
- `state`: `active`, `blocked`, or terminal `disposed`;
- `score`: finite number from 0 through 1 used only for frontier ordering;
- optional `disposition`: `resolved`, `rejected`, `deferred`,
  `superseded`, or `split`;
- a title and optional rationale.

Relations are typed:

- `issue_child_of` places a node under one parent;
- `supersedes` links a revision to its immediate predecessor;
- `blocked_by` links an issue to a blocking graph entity;
- `derived_from` or another explicit provenance relation links evidence.

A version after 1 requires exactly one predecessor. A blocked node requires at
least one blocker. An active node cannot carry a terminal disposition. A
disposed node must carry one. Scores rank attention; they do not assert truth.

## Atomic transition

`epistemic_graph_issue_tree_transition` accepts one or more typed successor
nodes and expands them into ordinary entity and relation operations. The whole
expansion passes through the existing immutable proposal, policy review, and
head-CAS admission pipeline as one transaction. Either all nodes and edges are
admitted or none are.

A caller may include `expected_ledger_head` when an earlier read is the
concurrency boundary. Deterministic identities and idempotency apply exactly as
for ordinary proposal admission.

## Derived frontier

`epistemic_graph_issue_tree_frontier` returns a bounded, score-descending
view. A frontier node is in the requested tree, non-terminal, and not
superseded by another node in that tree. Blocked nodes remain visible and are
marked blocked; they are not silently removed.

The response reports the ledger head, selection rules, count semantics, and
bounded continuation metadata. It is a projection, never mutation authority.

## Evidence boundary

Issue transitions do not create assessments, test outcomes, or evidence
promotion records. Evidence identifiers become explicit graph relations only.
No score, disposition, resolution, or frontier position promotes linked
material to evidence or certifies truth.

## Marici use

Marici should call these native tools and store only concrete issue data. It
must not fork the vocabulary or implement a second transition reducer in
repository scripts.
