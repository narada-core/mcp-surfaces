// Domain-independent append-only event-ledger machinery implementing the
// `narada.event-ledger.v1` regime (see `docs/event-ledger-format.md`):
// hash-chained immutable JSON event files are the only authority, SQLite
// projections are disposable rebuilds, and every mutation is an idempotent,
// fail-hard append serialized by an exclusive authority lock.
//
// The crate owns no domain concepts. Consuming surfaces supply their error
// envelope schema string, ledger layout (directory + filename prefix), event
// envelope fields, projection DDL, and per-event projection fold.


pub use error::ErrorSchema;

