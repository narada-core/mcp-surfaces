# @narada-core/local-filesystem-mcp

Canonical local filesystem MCP server.

## Standalone Usage

The supported runtime is the native Rust binary published by this package. It
has no Node, Bun, TypeScript, or package-manager runtime dependency; the
TypeScript sources only describe the surface and its generated catalog.

Build and run the package-owned binary:

```powershell
pnpm --filter @narada-core/local-filesystem-mcp build:native
cargo run --release --locked --manifest-path packages/local-filesystem-mcp/native/Cargo.toml -- --mode read --allowed-root <your-workspace-root>
```

The build publishes an immutable versioned binary and `dist/native/current.json`.
Use `@narada-core/mcp-registrar` to inject the surface into a CLI or TUI; the
registrar resolves the native artifact pointer and never starts the retired
JavaScript runtime.

If you want Narada to inject the surface into a CLI or TUI, use `@narada-core/mcp-registrar` to write the carrier config.

Tool results use `structuredContent` as the authoritative machine payload. The text content is a deterministic, compact rendering for agent transcripts. For `fs_read_file` and `fs_read_file_range`, the file body is delivered exactly once in `content[0].text`; `structuredContent` retains the authoritative path, hashes, range, and pagination metadata and identifies that delivery through `content_delivery`. Consumers must not expect a duplicate `structuredContent.content` field. Large read and search results are bounded by the producing tool's own offset/limit or snapshot paging arguments.

## Tools

### Read mode tools

- `fs_read_file`
- `fs_read_file_range`
- `fs_stat`
- `fs_glob_search`
- `fs_repository_inventory`
- `fs_file_metrics`
- `fs_search`
- `fs_search_results_read`
- `fs_grep_search`
- `fs_doctor`

### Write mode tools

Write mode tools are exposed only when launched with `--mode write`.

- `fs_write_file`
- `fs_str_replace_file`
- `fs_replace_range`
- `fs_apply_patch`
- `fs_move_path`
- `fs_create_directory`
- `fs_rename_directory`
- `fs_delete_directory`

Behavior notes:

- Allowed roots may be concrete paths via `--allowed-root <path>` or anchored relative roots via `--anchored-allowed-root user_home:.codex`. Anchored roots are resolved at startup to concrete canonical roots and reported by `fs_doctor` with anchor provenance. Roots config files may also include `anchored_allowed_roots`, for example `{ "anchored_allowed_roots": ["user_home:.codex"] }`. Site `.narada/allowed-roots.json` supports `extra_allowed_roots` and explicit `temp_allowed_roots` for active handoff directories such as `D:/tmp`; this is still concrete-root admission, not wildcard temp access.
- Tool paths do not expand shell environment syntax such as `%USERPROFILE%`; expand it before the call or pass an absolute path. Returned search paths use `/` consistently on every platform.
- `fs_read_file` and `fs_read_file_range` stream files through fixed-size buffers, retain only a bounded line window, and return full-file `content_sha256` plus explicit line-completeness fields. `total_lines_status: "unknown_after_window"` means line parsing stopped after the requested window plus lookahead while hashing continued. Request later windows by re-calling the same read tool with adjusted line offsets/ranges. Individual retained lines and the aggregate returned window have hard byte limits.
- `fs_stat` returns `sha256` for files and `entry_count`, `tree_entry_count`, `tree_truncated`, and `tree_sha256` for directories so callers can build stale-state guards without hashing locally.
- `fs_glob_search` and `fs_grep_search` return newline-separated matches in text and bounded match arrays in `structuredContent`. Empty searches succeed with empty arrays. Search paging uses `has_more` and `next_offset`; `count_exact: false` and `snapshot_complete: false` mean the native capture reached its declared entry or byte ceiling. `cache_policy` accepts `auto`, `snapshot`, `refresh`, and `bypass`; snapshots support consistent continuation within the captured prefix. The process retains at most four snapshots and kills ripgrep at timeout or capture bounds. `snapshot_reused: true` and `cache_hit: true` identify explicit continuation. Directory freshness includes a bounded tree fingerprint. `order: \"ripgrep_traversal\"` means page order follows ripgrep emission order, not sorted path order.
- `fs_repository_inventory` is a bounded repository-oriented view built on filesystem search. Pass `directory` as the canonical scope or `root` as its exclusive compatibility alias; passing both is refused, and either one is resolved and echoed rather than silently replaced by the first allowed root. It excludes known `.ai`/`.narada` runtime, temporary, output, and patch-outcome locations by default, returns candidate-source and generated-artifact classifications, and accepts `include_generated: true` for explicit artifact investigations. It does not infer Git state; use `git_changed_summary` from `@narada-core/git-mcp` for authoritative tracked and ignored paths.
- `fs_file_metrics` is a bounded metadata-only file table. Pass an explicit `directory` (or `root`), include `pattern`, ignore/exclude patterns, `limit`, and optionally `max_bytes_per_file` and `max_total_scan_bytes`; it returns paged path, exact byte-size, bounded text line-count, file-type, and scope-classification rows plus totals for the returned page. Larger text files keep their byte metadata and report `line_count_status: "too_large"`; files beyond the cumulative scan budget report `line_count_status: "scan_budget_exceeded"`. Snapshot and refresh requests materialize metric values in a process-local bounded LRU cache (maximum four entries); snapshots do not survive restart or eviction, so page promptly and rerun if a snapshot is not found. The tool never returns file contents. Prefer it over concurrent full-content `fs_read_file` calls for source inventories and line counts.
- `fs_search` is the preferred agent-facing search contract. It defaults to literal line matching, one canonical `directory`, 20 results, 500 characters per match, and a 6,000-character inline result budget. Its `items` array is the sole authoritative result projection. Choose `syntax: \"regex\"` explicitly for regular expressions and `result_kind: \"files\"` or `\"counts\"` for those jobs. Continue with the opaque cursor returned in `continuation.arguments`; materialized oversized results advertise `fs_search_results_read` and an immutable output reference.
- `fs_grep_search` is the compatibility contract for existing regex-oriented calls. It accepts legacy `path`, `glob`, `ignore`, raw offsets, and output modes, but no longer duplicates every match into both human and structured arrays. `match_objects` is authoritative, ordinary runtime diagnostics are compacted, the default result count is 20, and results over the 6,000-character producer budget are materialized for `fs_search_results_read`.
- `fs_write_file` supports `overwrite`, `create_only`, `create_parent_directories`, and `expected_sha256` guards. For large writes, pass `payload_ref` or `payload_path` carrying the complete argument object, including `path` and `content`, instead of sending large inline content.
- Successful file writes and replacements return the resulting full-file hash as `sha256`, `content_sha256`, and the compatibility field `after_sha256`, so the next guarded mutation does not require an intervening `fs_stat`.
- `fs_str_replace_file` supports `expected_sha256` for stale-file detection.
- `fs_replace_range` supports an `expected_sha256` guard for stale-file detection.
- `fs_create_directory` is idempotent for existing directories and returns `status: "exists"`.
- `fs_apply_patch` accepts unified diffs and Codex-style `*** Begin Patch` patches, including add, update, delete, and move targets. It supports `dry_run: true`, operation labels per changed file, and an `expected_sha256` map keyed by patch path or resolved path; unmatched expected-hash keys fail instead of being ignored.
- Supply a stable `operation_id` for durable patch recovery. `accepted` and `applying` outcomes record the owner, deadline, and captured filesystem fingerprints. After an owner-surface exit, `fs_patch_outcome_show` persists a terminal `interrupted_before_mutation`, `patched_recovered`, `interrupted_partial`, or `interrupted_unknown` result. Retry the identical operation only when `retry_safe: true`; if the deadline has elapsed while the owner is alive, restart that MCP surface and read the outcome again.
- `fs_move_path`, `fs_rename_directory`, and `fs_delete_directory` support optional expected metadata guards for stale-path detection. Callers can use structured `expected`, `expected_from`, and `expected_to` objects with `mtime`, `size`, `sha256`, `tree_sha256`, and `entry_count` fields, while older flat expected fields remain accepted.
- Tool errors use `schema: "local.filesystem.error.v1"` and normalize `details.operation` when the active tool is known.

## Verification

```powershell
pnpm --filter @narada-core/local-filesystem-mcp test:native
```
