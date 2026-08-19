# Event Ledger Format — `narada.event-ledger.v1`

This document defines the shared append-only event-ledger regime used by
native MCP surfaces whose tracked events are authoritative and whose SQLite
state is a disposable projection. The reference implementation is the
`narada-mcp-event-ledger` Rust crate (`packages/shared/event-ledger-native`);
`epistemic-graph` and `surface-feedback` are consumers.

The regime exists to make the event log the only authority: any derived
state can be deleted and rebuilt, tampering is detectable through a hash
chain, and every mutation is an idempotent, attributed, fail-hard append.

## Directory layout

Rooted under a per-surface authority directory beneath the site control
root (`.narada`):

```
<authority_root>/
  ledger/                 # authoritative event files, create_new-only
    <prefix>-000000000001-<uuid>.json
    <prefix>-000000000002-<uuid>.json
    ...
    idem-<safe_name>.txt  # admission idempotency markers (disposable index)
<runtime_root>/
  projection.sqlite       # disposable derived projection
  projection.sqlite.next  # rebuild scratch, atomically renamed into place
  locks/                  # fs2 exclusive authority locks
```

- `<authority_root>` is durable and must survive projection loss.
- `<runtime_root>` is disposable; deleting it only triggers a rebuild.
- Idempotency markers are a disposable index: they are recoverable by
  scanning the ledger for events carrying the same `idempotency_key`.

## Event envelope

Each event is one immutable JSON object:

```json
{
  "schema": "narada.<domain>.event.v1",
  "sequence": 7,
  "event_id": "<prefix>-000000000007-<uuid>",
  "previous_hash": "<hex sha256 of previous event_hash, or null/absent for the first>",
  "event_hash": "<hex sha256, see below>",
  "idempotency_key": "<optional>",
  "...": "domain payload fields (operations, actor, authority, timestamps, ...)"
}
```

Generic fields owned by the ledger core: `sequence`, `event_id`,
`previous_hash`, `event_hash`. Everything else — including the `schema`
string, actor/authority attribution, and domain payload — is supplied by
the consuming surface. Admission of an event asserts policy-valid
contribution only; it never certifies domain truth.

## Hash chain and digest convention

- `event_hash` is computed by cloning the event object, removing the
  hash field itself (`event_hash`), serializing with `serde_json::to_vec`,
  and hex-encoding the SHA-256 of those bytes.
- **The digest depends on JSON object key insertion order**
  (`serde_json` `preserve_order`). This is deliberate and load-bearing:
  it is *not* the key-sorting canonical JSON used by
  `narada-mcp-materialization-contract`. Do not "fix" this convention —
  changing it invalidates every existing ledger hash.
- Verification (`verify_ledger`) checks: contiguous `sequence` starting
  at 1, `previous_hash` equals the prior event's `event_hash`, and each
  `event_hash` recomputes exactly. Any mismatch refuses with
  `ledger_hash_invalid` (or a domain-prefixed equivalent).
- The same chain pattern is reused by auxiliary authorities (e.g.
  epistemic sequence claims) with their own hash-field names
  (`claim_hash`, `creation_hash`); the verification algorithm is shared,
  parameterized by hash-field name.

## Admission: head-CAS under exclusive lock

Every mutation is serialized:

1. Acquire an `fs2` exclusive file lock in `locks/` keyed by authority
   (10 s timeout, 25 ms poll; contention refuses with `authority_busy`).
2. Optionally compare the caller's `expected_ledger_head` against the
   current head; mismatch refuses with `ledger_head_conflict`.
3. Derive `sequence = event_count + 1` and
   `event_id = <prefix>-{sequence:012}-{uuid v4}`.
4. Compute `previous_hash`/`event_hash`, write the event with
   `create_new` + `sync_all` (never overwrite), write the
   `idem-<safe_name>.txt` marker when an idempotency key is present.
5. Rebuild the projection (below) before releasing the lock.

Retries with the same idempotency key replay to the originally admitted
event (via marker, falling back to a ledger scan); the same key with
different content refuses with `<domain>_idempotency_conflict`.

## Projection

- The projection is rebuilt **from scratch on every read path**: verify
  the ledger, apply every event into `projection.sqlite.next` inside one
  transaction, then remove the old file and atomically rename `.next`
  into place.
- The projection schema (DDL) and the per-event fold (applier callback)
  are owned by the consuming surface. The core owns only the
  verify → build → swap shell.
- Because the fold is defined over the full event stream, "current
  state" (latest status, current attribution) is derived, never stored
  authoritatively. Revisions are modeled as new events, never mutation.

## Error envelope

Refusals are structured and fail-hard:

```json
{ "schema": "narada.<domain>.error.v1", "code": "...", "message": "...", "details": {} }
```

Best-effort or swallowed persistence writes are prohibited in this
regime: an event append either completes durably or the call fails.

## Non-goals (current version)

- No incremental or checkpointed projection; full rebuild per read is
  the accepted cost model at current ledger scales.
- No cross-surface or cross-site ledger replication; federation with
  foreign stores happens through explicit import/snapshot events.
- No lease or expiry semantics; time-dependent state must be modeled as
  events evaluated at fold time.
