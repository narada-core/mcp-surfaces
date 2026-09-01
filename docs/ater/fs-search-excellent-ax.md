# Filesystem Search: Excellent Agent Experience

Status: target interaction definition with the first implementation increment
verified on 2026-08-31.

Assessment target: `@narada-core/local-filesystem-mcp` search tools, with
`fs_grep_search` as the current implementation and a versioned filesystem
search contract as the target.

This document applies the
[Agent Tool Ergonomics Review](../agent-tool-ergonomics-review.md). It defines
excellent agent experience (AX) as observable task performance:

> An agent can express a search intent in one ordinary call, receive the
> smallest sufficient authoritative result, understand its completeness, and
> continue or recover without guessing, flooding context, or repeating
> expensive or stateful work.

Excellent AX is not a compact renderer over an oversized protocol result. It is
coherent behavior across discovery, producer limits, structured results,
transport materialization, model context, operator UI, continuation, and
restart.

The first implementation increment adds `fs_search`, one authoritative item
array, literal-safe defaults, explicit result kinds, small count and item-text
bounds, opaque direct continuation, on-demand diagnostics, and immutable
readback for an oversized returned page. The compatibility
`fs_grep_search` path now uses lower defaults and materializes oversized
results. Cross-child-restart continuation of an unmaterialized search snapshot
remains an explicit follow-up; process-local cursors must not be described as
durable.

## 1. Charter

### Scope

- content search, file discovery, match counting, and bounded context;
- tool discovery and argument construction;
- producer, transport, model-visible, and Pi-visible projections;
- empty, partial, timeout, malformed-pattern, and restart behavior;
- continuation, snapshots, and full-result recovery;
- compatibility from `fs_grep_search`.

### Users

- first-time coding agent;
- experienced coding agent;
- programmatic MCP client;
- operator observing Pi;
- surface maintainer diagnosing search behavior.

### Non-goals

- replacing Git-aware search or Git status;
- indexing repository semantics;
- unbounded repository export;
- arbitrary shell or raw ripgrep access;
- making generated and dependency directories part of ordinary search.

### Authority boundary

Search remains read-only and constrained to admitted filesystem roots. Search
scope does not imply Git, Site, identity, or mutation authority. Materialized
results remain bound to the serving filesystem authority.

## 2. Canonical task corpus

| ID | Task | Success condition |
| --- | --- | --- |
| S1 | Find a known symbol definition in one package | Exact path, line, and matching text are returned |
| S2 | Find files referring to a tool name | Unique authoritative paths are returned |
| S3 | Count matches by file | Per-file counts and completeness are explicit |
| S4 | Search a repository with no explicit mode | Common intent succeeds with conservative defaults |
| S5 | Search for a literal beginning with `-` | Literal is not misread as an option or regex |
| S6 | Use a regular expression intentionally | Syntax choice and errors are explicit |
| S7 | Search with no matches | Empty success is distinct from stale, partial, and failed |
| S8 | Search a common token producing hundreds of matches | First evidence stays within context budget and continuation is usable |
| S9 | Encounter one multi-megabyte matching line | The item is clipped with exact clipping metadata |
| S10 | Request surrounding context | Context is bounded per item and globally |
| S11 | Continue a result | Continuation preserves query, scope, ordering, and captured state |
| S12 | Continue after child restart | Result is readable or expiry is explicit and safe |
| S13 | Search times out | Partial evidence is either recoverable or explicitly absent |
| S14 | Search an invalid regex | Failure identifies syntax and a concrete correction |
| S15 | Accidentally search too broad a root | Hard bounds prevent context or resource exhaustion |
| S16 | Diagnose caching or freshness | Diagnostics are available without polluting ordinary results |
| S17 | Use Pi collapsed and expanded views | Collapsed output is concise; expansion is complete within declared capture |
| S18 | Use a non-Pi client | Authoritative semantics match Pi |

## 3. Current journey

The ordinary current journey is:

```text
choose fs_grep_search
  -> choose path/directory alias
  -> know whether files_with_matches, count_matches, or content is needed
  -> estimate a safe match-count limit
  -> receive one large structured envelope
  -> distinguish exact count from page state
  -> manually construct offset/snapshot continuation
```

Friction and failure points:

- the implementation-oriented word “grep” presumes prior knowledge;
- `path` and `directory` express one concept;
- `ignore` and `exclude` express one concept;
- the default mode returns files when many coding tasks require matching lines;
- `limit` appears to bound output but only bounds match count;
- raw offset is the visible continuation abstraction;
- stable continuation requires understanding cache policy and snapshot IDs;
- ordinary results include cache, freshness, traversal, and memory diagnostics;
- every match is represented in both `matches` and `match_objects`;
- the function named `cappedToolValue` is a no-op;
- Pi's 8k model-context cap contains damage after the producer has already
  constructed an oversized result.

## 4. Current baseline

Measured on the repository implementation on 2026-08-31:

| Task | Returned | Structured characters | Duplicate projection characters |
| --- | ---: | ---: | ---: |
| Find `cappedToolValue` | 2 | 3,791 | at least 930 |
| Broad `const` search, limit 80 | 80 of 746 | 38,521 | at least 11,439 |
| Files referring to `fs_grep_search` | 7 | 2,837 | at least 601 |
| Empty search | 0 | 1,477 | 0 |

Current limits:

- default match count: 80;
- maximum requested match count: 500;
- page match-byte ceiling: 512 KiB;
- individual match ceiling: 16 KiB;
- default helper timeout: 60 seconds;
- complete-snapshot capture ceiling: 2 MiB by default;
- Pi model-visible containment ceiling: 8,000 characters.

The broad task demonstrates the category error: a bounded count is not bounded
interaction output. The producer emitted 38.5k structured characters for an
ordinary first page. Pi now prevents all of that from entering model context,
but the producer contract remains uneconomical.

## 5. Cognitive walkthrough findings

### Discovery

The current description explains mechanics and aliases but does not lead with
the jobs “find matching lines,” “find files,” or “count occurrences.” A new
agent must translate intent into ripgrep concepts.

### Construction

The agent must choose among duplicated scope and exclusion names, understand
regular-expression syntax, select a projection, estimate a count limit, and
sometimes reason about cache policy before the first useful result.

### Feedback

Success is mechanically distinguishable, but ordinary feedback mixes task data
with implementation diagnostics. The authoritative representation is declared
only after a duplicate human representation has already been included.

### Continuation

`next_offset` is insufficient as a stable public abstraction. Without a
snapshot, a repeated traversal may observe changed files or ordering. With a
snapshot, the caller must coordinate `cache_policy`, `snapshot_id`,
`offset`, and `limit`.

### Error prevention

The 512 KiB page ceiling and 16 KiB item ceiling are capture protections, not
agent-interaction budgets. A plausible broad query can consume substantial
context before the agent has evidence that it should narrow the search.

## 6. Heuristic scorecard

| Heuristic | Score | Finding |
| --- | ---: | --- |
| Discoverability | 1 | Tool and modes expose implementation vocabulary |
| Argument economy | 1 | Duplicated aliases and manually coordinated paging |
| Output economy | 0 | 512 KiB page, duplicate data, no-op cap |
| State legibility | 2 | Many states are explicit, but ordinary detail is noisy |
| Error prevention | 1 | Capture is bounded but AX/context is not producer-bounded |
| Continuation and recovery | 1 | Offset plus optional process-local snapshot is fragile |
| Consistency | 1 | Producer, transport, and carrier boundaries compensate independently |
| Trust and provenance | 2 | Scope and freshness exist, but overwhelm ordinary evidence |

Assessment: reassessment required. The output-economy score of 0 is
release-blocking for an excellent-AX claim.

## 7. Hazard register

| ID | Hazard | Effect | Required control |
| --- | --- | --- | --- |
| H1 | Broad pattern returns tens or hundreds of kilobytes | Context loss and delayed task completion | Small producer inline budget independent of count |
| H2 | Matches are duplicated | Roughly doubled model and transport cost | One authoritative item array |
| H3 | One matching line is 16 KiB | One item dominates the result | Small per-item text limit with clipping metadata |
| H4 | Offset continuation observes changing traversal | Incoherent pages or missed evidence | Opaque query-bound cursor over captured state |
| H5 | Snapshot disappears on restart or eviction | Continuation fails after apparent success | Durable scoped result reference or explicit expiry contract |
| H6 | Timeout returns no partial evidence | Expensive rerun and lost progress | Materialize admitted partial capture when safe |
| H7 | Diagnostics dominate ordinary result | Agent misses actionable evidence | Separate diagnostic projection or details reader |
| H8 | Default mode returns files, not lines | Extra call before useful evidence | Intent-oriented default and specialized jobs |
| H9 | Regex is implicit | Literal intent produces error or surprising matches | Explicit syntax with literal default |
| H10 | Pi containment hides producer defect | Other clients remain exposed | Enforce producer and transport budgets |
| H11 | “capped” helper is a no-op | Maintainers infer a nonexistent invariant | Implement and test the named invariant |
| H12 | Broad scope is legal and cheap to request | Resource waste before narrowing | Scope summary, hard capture ceilings, narrowing guidance |

## 8. Excellent-AX principles

Filesystem search has excellent AX when:

1. **Intent is primary.** The tool speaks in jobs: matching lines, matching
   files, or counts.
2. **The common path is one call.** Ordinary symbol discovery requires no
   schema dump, cache planning, or reader call.
3. **Literal is safe by default.** Regular expressions are explicit.
4. **Scope is singular.** One canonical directory and one canonical exclusion
   field exist.
5. **Output is sufficient, not maximal.** The first page gives enough evidence
   to decide whether to inspect, continue, or narrow.
6. **Bytes and counts are separate.** Both per-item and whole-result limits are
   enforced.
7. **There is one authority-bearing projection.** Human summaries are derived,
   not duplicated into structured content.
8. **Completeness is explicit.** Complete, partial, clipped, timed out, and
   expired cannot be confused.
9. **Continuation is opaque and direct.** The response supplies the exact next
   tool and arguments.
10. **Recovery survives ordinary lifecycle events.** A successful reference
    remains readable through its declared lifetime.
11. **Diagnostics are on demand.** Cache and freshness internals do not occupy
    the ordinary result.
12. **Carrier containment is defense in depth.** Producer correctness does not
    depend on Pi.

## 9. Target public workflow

The preferred public tool is `fs_search`. `fs_grep_search` remains a
compatibility adapter during migration.

### Ordinary call

```json
{
  "query": "cappedToolValue",
  "directory": "packages/local-filesystem-mcp/src",
  "result_kind": "matches"
}
```

Defaults:

- `syntax: "literal"`;
- `result_kind: "matches"`;
- `max_results: 20`;
- `max_inline_chars: 6000`;
- `max_text_chars_per_match: 500`;
- `context_before: 0`;
- `context_after: 0`;
- generated dependency/build/runtime paths excluded;
- case behavior follows a declared cross-platform rule rather than ripgrep
  ambient behavior.

Advanced fields:

- `syntax: "literal" | "regex"`;
- `result_kind: "matches" | "files" | "counts"`;
- `file_glob`;
- `exclude`;
- `case: "smart" | "sensitive" | "insensitive"`;
- `context_before`, `context_after`, each capped at 10;
- `max_results`, default 20, hard maximum 100;
- `max_inline_chars`, default 6000, hard maximum 20000;
- `diagnostics: false | true`;
- opaque `cursor` only for continuation.

Aliases do not appear in the new schema. Compatibility aliases are accepted
only by the old adapter and canonicalized visibly.

## 10. Target result contract

```json
{
  "schema": "local.filesystem.search.v2",
  "status": "ok",
  "result_kind": "matches",
  "scope": {
    "directory": "packages/local-filesystem-mcp/src",
    "file_glob": null,
    "default_exclusions_applied": true
  },
  "query": {
    "text": "nativeFilesystemTools",
    "syntax": "literal",
    "case": "smart"
  },
  "items": [
    {
      "path": "packages/local-filesystem-mcp/src/native-tool-catalog.ts",
      "line": 9,
      "text": "export function nativeFilesystemTools(mode: 'read' | 'write')",
      "text_complete": true
    }
  ],
  "page": {
    "returned": 1,
    "complete": true,
    "result_count": 1,
    "result_count_exact": true,
    "inline_chars": 412,
    "inline_char_limit": 6000
  },
  "continuation": null,
  "result_ref": null
}
```

Contract rules:

- `items` is the sole authoritative result array.
- Paths are root-relative unless an explicit authority reason requires an
  absolute path.
- `text_complete: false` includes `original_text_chars` and
  `returned_text_chars`.
- The complete serialized structured result, not only item text, is subject to
  `max_inline_chars`.
- Scope and query are compact and canonical; default exclusion patterns are
  summarized, not enumerated.
- Counts may be null when not known; unknown is never represented as zero.
- `status: "partial"` is used when producer capture ends at timeout or hard
  ceiling and admitted evidence is retained.

### Large result

When more evidence exists than fits inline:

```json
{
  "schema": "local.filesystem.search.v2",
  "status": "ok",
  "items": [
    {
      "path": "packages/example/src/a.ts",
      "line": 12,
      "text": "const example = true",
      "text_complete": true
    },
    {
      "path": "packages/example/src/b.ts",
      "line": 31,
      "text": "const second = true",
      "text_complete": true
    }
  ],
  "page": {
    "returned": 2,
    "complete": false,
    "result_count": 746,
    "result_count_exact": true,
    "inline_chars": 5890,
    "inline_char_limit": 6000
  },
  "continuation": {
    "tool": "fs_search_results_read",
    "arguments": {
      "result_ref": "fs_search_result:...",
      "cursor": "..."
    }
  },
  "result_ref": "fs_search_result:..."
}
```

The example uses two items for brevity; the actual result returns as many
complete bounded items as fit. The continuation object is directly callable
and repeats no query construction.

## 11. Result materialization

A result reference is required when:

- complete captured results exceed the inline budget;
- a timeout or hard ceiling leaves an admitted partial capture worth
  preserving;
- continuation must survive child generation replacement.

Properties:

- immutable within the reference lifetime;
- bound to filesystem authority and canonical query digest;
- paged by an opaque cursor;
- readable through the stable surface handle;
- explicit expiry and eviction behavior;
- content digest and captured completeness available;
- no claim of completeness beyond producer hard ceilings.

`fs_search_results_read` returns the same `items` shape and page contract. It
does not repeat full scope, freshness, or diagnostics on every page.

## 12. Projection contract

### Model-visible

- At most 8,000 characters at the carrier boundary.
- Producer target at most 6,000 characters by default.
- Actionable items first, followed by compact completeness and continuation.
- No duplicate human rendering.

### Collapsed Pi

```text
fs_search · 20 of 746 matches · more available
```

### Expanded Pi

Expanded output displays the bounded inline items and continuation guidance. It
does not imply that the entire captured result was injected into model context.

### Diagnostics

With `diagnostics: true`, return a bounded diagnostics object or
`diagnostics_ref` containing:

- cache decision;
- snapshot or result capture identity;
- freshness fingerprint;
- traversal and timeout details;
- applied exclusion patterns;
- producer limit resolution.

Ordinary calls contain only diagnostic facts that change interpretation.

## 13. Alternatives considered

### A. Repair `fs_grep_search` in place

Pros: smallest compatibility change.

Cons: preserves implementation vocabulary, aliases, mode ambiguity, and offset
semantics. Suitable as an adapter, not the excellent-AX target.

### B. Separate `fs_find_matches`, `fs_find_files`, and `fs_count_matches`

Pros: excellent discoverability and small schemas.

Cons: duplicates query and continuation contracts and complicates switching
projection after capture.

### C. One intent-oriented `fs_search` plus result reader

Pros: one coherent query contract, explicit result job, shared durable capture,
and direct continuation.

Cons: requires a versioned migration and carefully bounded schema.

Decision: C. Keep descriptions task-oriented and the result-kind enum small.

## 14. Migration

1. Implement the producer-size invariant and remove duplicate projections from
   `fs_grep_search` immediately.
2. Add `fs_search` and `fs_search_results_read` with the v2 contract.
3. Make `fs_grep_search` an adapter:
   - `pattern` maps to `query`;
   - legacy calls retain regex syntax;
   - `output_mode` maps to `result_kind`;
   - `path` visibly canonicalizes to `directory`;
   - `ignore` and `exclude` merge into canonical `exclude`;
   - offset continuation is supported only for compatibility.
4. Update guidance to prefer `fs_search`.
5. Observe compatibility use and remove aliases only in a declared breaking
   version.

No successful legacy call may become broader or switch from regex to literal
silently.

## 15. Acceptance matrix

| Gate | Required assertion |
| --- | --- |
| A1 Ordinary one-call success | S1 completes with one call and no inspection |
| A2 Producer inline bound | Default structured result is at most 6,000 characters |
| A3 Carrier context bound | Model-visible result is at most 8,000 characters |
| A4 Per-item bound | No returned match text exceeds 500 characters by default |
| A5 No duplication | Each authoritative match appears once in structured content |
| A6 Direct continuation | Returned continuation can be called without reconstructing query |
| A7 Reconstruction | Reading all pages reproduces the captured result exactly |
| A8 Stable reference | Result remains readable across child generation replacement |
| A9 Explicit expiry | Expired references return typed recovery, not not-found ambiguity |
| A10 State distinction | Empty, partial, clipped, timeout, refused, and failed differ |
| A11 Count integrity | Unknown, inexact, and exact counts are mechanically distinct |
| A12 Scope integrity | Canonical scope never widens silently |
| A13 Literal safety | Leading dash and regex metacharacters are literal by default |
| A14 Regex intent | Explicit regex supports valid patterns and typed parse errors |
| A15 Context bound | Context lines respect per-item and whole-response budgets |
| A16 Diagnostic separation | Ordinary result omits cache/freshness internals |
| A17 Cross-client parity | Pi and non-Pi clients agree on authoritative items and state |
| A18 Restart behavior | Continuation either survives restart or reports declared expiry |
| A19 Broad-query containment | S8 cannot exceed producer, transport, or context ceilings |
| A20 Long-line containment | S9 reports clipping without oversized output |

## 16. Success measures

Targets for the canonical corpus:

- at least 90% correct-first-call completion for S1–S7;
- median one MCP call for ordinary search tasks;
- zero schema-list or lease-inspection calls for statically exposed search;
- default producer result at most 6,000 characters;
- model-visible result at most 8,000 characters;
- duplicate-data ratio below 2%;
- 100% typed completeness and continuation state;
- 100% safe recovery for retained result references;
- zero incorrect-completion decisions caused by truncation or timeout.

## 17. Assessment disposition

Current implementation: **reassessment required**.

The Pi containment fix is valid defense in depth, but excellent AX requires
producer-level economy, one authoritative projection, direct durable
continuation, and intent-oriented defaults. The acceptance matrix above is the
release gate for changing this disposition to pass.

## 18. Implementation increments

Recommended order:

1. enforce serialized producer and per-item budgets;
2. remove `matches`/`match_objects` duplication;
3. separate ordinary and diagnostic projection;
4. implement immutable result capture and reader;
5. add `fs_search` v2 with literal-safe defaults;
6. adapt legacy `fs_grep_search`;
7. add Pi and non-Pi end-to-end acceptance tests;
8. rerun ATER and record the independent assessment.
