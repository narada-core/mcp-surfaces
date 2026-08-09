# @narada-core/local-filesystem-mcp

Canonical local filesystem MCP server.

## Standalone Usage

This package can be installed and run without Narada. It needs only a Node.js runtime, the package build, and the filesystem roots you choose to admit.

The example below is path-agnostic. Replace the entrypoint path with the location of your installed package.

Example:

```powershell
pnpm --filter @narada-core/local-filesystem-mcp build
node <installed-package>/dist/src/main.js --mode read --allowed-root <your-workspace-root>
```

If you want Narada to inject the surface into a CLI or TUI, use `@narada-core/mcp-registrar` to write the carrier config.

Tool results use `structuredContent` as the authoritative machine payload. The text content is a deterministic, compact rendering for agent transcripts. Large read and search results are bounded by the producing tool's own offset/limit or snapshot paging arguments.

## Tools

### Read mode tools

- `fs_read_file`
- `fs_read_file_range`
- `fs_stat`
- `fs_glob_search`
- `fs_repository_inventory`
- `fs_file_metrics`
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
- `fs_read_file` and `fs_read_file_range` return line-window metadata, `content_sha256`, and explicit line-completeness fields without reading the whole file just to satisfy small windows. `total_lines_status: "unknown_after_window"` means the tool stopped after the requested window plus lookahead. Request later windows by re-calling the same read tool with adjusted line offsets/ranges.
- `fs_stat` returns `sha256` for files and `entry_count`, `tree_entry_count`, `tree_truncated`, and `tree_sha256` for directories so callers can build stale-state guards without hashing locally.
- `fs_glob_search` and `fs_grep_search` return newline-separated matches in text and bounded match arrays in `structuredContent`. Empty glob and grep searches are successful responses with `count: 0`, `returned: 0`, and empty match arrays. Search paging uses `has_more` and `next_offset`; `count_exact: false` means ripgrep was stopped after the requested page plus lookahead. `cache_policy` accepts `auto`, `snapshot`, `refresh`, and `bypass`; complete snapshot responses include a reusable `snapshot_id`, and callers can pass `snapshot_id` for consistent continuation. Directory freshness includes a bounded tree fingerprint. `order: \"ripgrep_traversal\"` means page order follows ripgrep emission order, not sorted path order.
- `fs_repository_inventory` is a bounded repository-oriented view built on filesystem search. It excludes known `.ai`/`.narada` runtime, temporary, output, and patch-outcome locations by default, returns candidate-source and generated-artifact classifications, and accepts `include_generated: true` for explicit artifact investigations. It does not infer Git state; use `git_changed_summary` from `@narada-core/git-mcp` for authoritative tracked and ignored paths.
- `fs_file_metrics` is a bounded metadata-only file table. Pass an explicit `directory` (or `root`), include `pattern`, ignore/exclude patterns, `limit`, and optionally `max_bytes_per_file` and `max_total_scan_bytes`; it returns paged path, exact byte-size, bounded text line-count, file-type, and scope-classification rows plus totals for the returned page. Larger text files keep their byte metadata and report `line_count_status: "too_large"`; files beyond the cumulative scan budget report `line_count_status: "scan_budget_exceeded"`. Snapshot and refresh requests materialize metric values in a process-local bounded LRU cache (maximum four entries); snapshots do not survive restart or eviction, so page promptly and rerun if a snapshot is not found. The tool never returns file contents. Prefer it over concurrent full-content `fs_read_file` calls for source inventories and line counts.
- `fs_grep_search` uses `directory` as canonical scope and accepts legacy `path` as an exclusive alias; passing both is refused. `glob` limits included files, while `ignore`/`exclude` add omitted globs. The structured result echoes the resolved scope (`requested_path`, `root`, `include_glob`, and `excluded_globs`) so a search cannot silently widen to the first allowed root. It includes `output_mode`, humanized `matches`, and parsed `match_objects`; `match_objects_authoritative: true` indicates the parsed objects are the stable machine payload. Use `output_mode: \"content\"` for content or symbol discovery with line-numbered matches.
- `fs_write_file` supports `overwrite`, `create_only`, `create_parent_directories`, and `expected_sha256` guards. For large writes, pass `payload_ref` or `payload_path` carrying the complete argument object, including `path` and `content`, instead of sending large inline content.
- `fs_str_replace_file` supports `expected_sha256` for stale-file detection.
- `fs_replace_range` supports an `expected_sha256` guard for stale-file detection.
- `fs_create_directory` is idempotent for existing directories and returns `status: "exists"`.
- `fs_apply_patch` accepts unified diffs and Codex-style `*** Begin Patch` patches, including add, update, delete, and move targets. It supports `dry_run: true`, operation labels per changed file, and an `expected_sha256` map keyed by patch path or resolved path; unmatched expected-hash keys fail instead of being ignored.
- Supply a stable `operation_id` for durable patch recovery. `accepted` and `applying` outcomes record the owner, deadline, and captured filesystem fingerprints. After an owner-surface exit, `fs_patch_outcome_show` persists a terminal `interrupted_before_mutation`, `patched_recovered`, `interrupted_partial`, or `interrupted_unknown` result. Retry the identical operation only when `retry_safe: true`; if the deadline has elapsed while the owner is alive, restart that MCP surface and read the outcome again.
- `fs_move_path`, `fs_rename_directory`, and `fs_delete_directory` support optional expected metadata guards for stale-path detection. Callers can use structured `expected`, `expected_from`, and `expected_to` objects with `mtime`, `size`, `sha256`, `tree_sha256`, and `entry_count` fields, while older flat expected fields remain accepted.
- Tool errors use `schema: "local.filesystem.error.v1"` and normalize `details.operation` when the active tool is known.

Example:

```powershell
pnpm --filter @narada-core/local-filesystem-mcp build
node <src-root>/mcp-surfaces/packages/local-filesystem-mcp/dist/src/main.js --mode read --allowed-root <src-root>/narada
```

## Verification

```powershell
pnpm --filter @narada-core/local-filesystem-mcp test
```
