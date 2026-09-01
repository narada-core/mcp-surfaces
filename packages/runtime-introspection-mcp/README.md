# Runtime Introspection MCP

The shared observation package emits source records; the external Narada PC
Site runtime observer owns the canonical SQLite database and sampling
authority. This adapter is a read-only reader. See the
[memory-observation ownership matrix](../../docs/mcp-runtime-memory-observation.md#ownership-and-capability).
The selected implementation is the shared native Rust surface. It provides
read-only analysis of runtime traces and authority-bound MCP memory evidence.
Memory tools resolve only the server-bound Site root and open the canonical
observer SQLite database read-only; callers cannot supply arbitrary roots or
database paths. Node and Bun are not admitted authorities for this surface.

Start with `runtime_introspection_guidance`, then use `runtime_introspection_memory_status` before owner, timeline, attribution, or incident reads. A stale or unavailable observer is reported explicitly. This surface never restarts runtimes, writes incident review state, or captures heap snapshots.

## Tools

- `runtime_introspection_guidance` - orientation and evidence boundaries.
- `runtime_introspection_memory_status`, `runtime_introspection_memory_owners`,
  `runtime_introspection_memory_timeline`, and
  `runtime_introspection_memory_attribution` - read bounded memory evidence.
- `runtime_introspection_memory_incidents` and
  `runtime_introspection_memory_incident_show` - inspect incident evidence.
- `runtime_introspection_formats`, `runtime_introspection_top_events`,
  `runtime_introspection_analyze_trace`, `runtime_introspection_analyze`,
  `runtime_introspection_top`, `runtime_introspection_show`, and
  `runtime_introspection_show_event` - analyze bounded inline runtime traces.
  The ranking and focused-view tools accept the trace directly, so callers do
  not need to copy an analysis result into a second request.

## Verification

Run the focused Rust unit and real stdio integration proof with:

```powershell
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml runtime_introspection
pnpm --filter @narada-core/runtime-introspection-mcp test
```
