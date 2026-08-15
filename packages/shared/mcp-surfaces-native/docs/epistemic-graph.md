# Epistemic Graph MCP

The `epistemic-graph` surface is the protocol adapter for a Site-owned problem-situation graph. The mechanism is generic; the selected Site supplies the root and therefore the authority, ledger, and projection locations.

## Authority boundary

The immutable, hash-linked ledger under `.narada/epistemic/ledger` is authoritative. SQLite under `.narada/.ai/epistemic-graph` is a disposable read projection and may always be rebuilt from the ledger.

Clients must not write either location. They call the Site-bound authority through MCP (and, for the Operator Console, through the HTTP adapter to the same Rust authority). There must be one serialization point for mutations. Adapters do not fall back to direct storage when the authority is unavailable.

Admission proves only structural validity, provenance, and policy compliance. It does not establish that a conjecture is true.

## Read workflow

1. Call `epistemic_graph_status` and retain `ledger_head`.
2. Use `epistemic_graph_query`, `epistemic_graph_query_batch`, or `epistemic_graph_neighborhood` for bounded inspection.
3. Use `epistemic_graph_snapshot` for visualization. Page entities and relations independently and pass `expected_ledger_head` on every later page.
4. If a page refuses a mismatched head, discard the partial snapshot and restart from offset zero. Never merge pages from different ledger heads.
5. Use `epistemic_graph_source_inspect` only for bounded Site-local source inspection and `epistemic_graph_export` for JSON-LD export.

## Mutation workflow

Mutations are proposal based:

1. Submit an immutable proposal with `epistemic_graph_proposal_submit`, or create one from bounded sources with `epistemic_graph_capture_sources`.
2. Read the stored proposal with `epistemic_graph_proposal_read`.
3. Review it explicitly with `epistemic_graph_proposal_review`.
4. Admit or reject it with the corresponding proposal tool.
5. Use `epistemic_graph_submit_review_admit` only when one authenticated operator intentionally performs the compound workflow; the immutable proposal and review records are still preserved.
6. Use `epistemic_graph_proposal_resubmit` to replace identified operations without mutating the original proposal.

Every mutation adapter must overwrite caller-supplied actor/authority fields with the authenticated principal and Site binding. Optimistic concurrency uses the expected ledger head where the tool contract provides it.

## Snapshot contract

`epistemic_graph_snapshot` accepts:

- `entity_offset` and `relation_offset`, each starting at zero;
- `limit`, from 1 through 1000, applied independently to each record kind;
- `expected_ledger_head`, string or null.

It returns a stable ledger head, total counts, bounded entity and relation pages, and independent next offsets. This supports a bounded browser cache without treating the browser as graph authority.

## Failure posture

The surface fails closed for malformed proposals, invalid relations, Site-root escapes, stale ledger heads, unavailable authority, and projection/ledger inconsistencies. HTTP and Cloudflare adapters must preserve the structured refusal rather than translating it into an apparent success.
