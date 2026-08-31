export function renderToolResultText(value: any, renderContext: Record<string, unknown>= {}) {
  const record = asRecord(value);
  if (record.schema === 'narada.mcp_output_page.v1') return String(record.output_text ?? '');
  if (record.schema === 'narada.mcp_surface.guidance.v0') return renderGuidanceResult(record);
  if (record.schema === 'local.filesystem.doctor.v1') return renderDoctorResult(record);
  if (record.schema === 'narada.mcp_output_locator.v1' || typeof record.output_ref === 'string') {
    return compactLines([
      `status: ${record.status ?? 'ok'}`,
      'result: materialized',
      `output_ref: ${record.output_ref ?? record.ref ?? ''}`,
      `reader_tool: ${record.reader_tool ?? 'none'}`,
      `render_truncated: ${record.render_truncated ?? record.original_truncated ?? true}`,
      record.count !== undefined ? `count: ${record.count ?? 'unknown'}` : null,
      record.count_exact !== undefined ? `count_exact: ${record.count_exact}` : null,
      record.scanned_unit === 'matched_entries' && record.scanned !== undefined ? `matched_entries_scanned: ${record.scanned}` : record.scanned !== undefined ? `scanned: ${record.scanned}` : null,
      record.scanned_unit !== undefined ? `scanned_unit: ${record.scanned_unit}` : null,
      record.returned !== undefined ? `returned: ${record.returned}` : null,
      record.order !== undefined ? `order: ${record.order}` : null,
      record.cache_hit !== undefined ? `cache_hit: ${record.cache_hit}` : null,
      record.has_more !== undefined ? `has_more: ${record.has_more}` : null,
      record.next_offset !== undefined ? `next_offset: ${record.next_offset ?? 'null'}` : null,
      record.full_output_char_length !== undefined ? `full_output_char_length: ${record.full_output_char_length}` : null,
    ]);
  }
  if (isReadFileResult(record)) return renderReadFileResult(record);
  if (record.schema === 'local.filesystem.glob.v1') return renderSearchResult('fs_glob_search', record);
  if (record.schema === 'local.filesystem.file_metrics.v1') return renderFileMetricsResult(record);
  if (record.schema === 'local.filesystem.grep.v1') return renderSearchResult('fs_grep_search', record, renderContext);
  if (record.schema === 'local.filesystem.apply_patch.v1') {
    const changedFiles = Array.isArray(record.changed_files) ? record.changed_files : [];
    return compactLines([
      `fs_apply_patch: ${record.status ?? 'ok'}`,
      `changed_files: ${changedFiles.length}`,
      ...changedFiles.map((file: any) => {
        const fileRecord = asRecord(file);
        return `- ${fileRecord.relative_path ?? fileRecord.path ?? ''} deleted=${fileRecord.deleted ?? false} before_sha256=${fileRecord.before_sha256 ?? ''} after_sha256=${fileRecord.after_sha256 ?? 'null'}`;
      }),
    ]);
  }
  if (record.schema || record.status || record.path || record.relative_path || record.type) return renderCompactRecord(record);
  return JSON.stringify(value, null, 2);
}

function renderGuidanceResult(record: any) {
  const firstUse = Array.isArray(record.first_use) ? record.first_use.map(String) : [];
  const recovery = Array.isArray(record.recovery) ? record.recovery.map(String) : [];
  return compactLines([
    `fs_guidance: ${record.status ?? 'ok'}`,
    `purpose: ${record.purpose ?? ''}`,
    `surface_id: ${record.surface_id ?? 'local-filesystem'}`,
    'first_use:',
    ...firstUse.map((item: any) => `- ${item}`),
    'recovery:',
    ...recovery.map((item: any) => `- ${item}`),
  ]);
}

function renderDoctorResult(record: any) {
  const roots = Array.isArray(record.allowed_roots) ? record.allowed_roots.map(String) : [];
  const permissions = asRecord(record.effective_permissions);
  return compactLines([
    `fs_doctor: ${record.status ?? 'ok'}`,
    `mode: ${record.mode ?? 'unknown'}`,
    `output_root: ${record.output_root ?? 'none'}`,
    `can_read: ${permissions.can_read ?? false}`,
    `can_write: ${permissions.can_write ?? false}`,
    'allowed_roots:',
    ...roots.map((root: any) => `- ${root}`),
  ]);
}

function renderFileMetricsResult(record: any) {
  const files = Array.isArray(record.files) ? record.files.map(asRecord) : [];
  const totals = asRecord(record.totals);
  return compactLines([
    `fs_file_metrics: ${record.status ?? 'ok'}`,
    `directory: ${record.directory ?? ''}`,
    `pattern: ${record.pattern ?? '**/*'}`,
    `count: ${record.count ?? 'unknown'}`,
    `count_exact: ${record.count_exact ?? false}`,
    `returned: ${record.returned ?? files.length}`,
    `has_more: ${record.has_more ?? false}`,
    `next_offset: ${record.next_offset ?? 'null'}`,
    `totals_scope: ${record.totals_scope ?? 'returned_page'}`,
    `total_files: ${totals.file_count ?? files.length}`,
    `total_lines: ${totals.line_count ?? 'partial'}`,
    `total_bytes: ${totals.byte_count ?? 0}`,
    `binary_files: ${totals.binary_file_count ?? 0}`,
    `too_large_files: ${totals.too_large_file_count ?? 0}`,
    `scan_budget_exceeded_files: ${totals.scan_budget_exceeded_file_count ?? 0}`,
    `unavailable_files: ${totals.unavailable_file_count ?? 0}`,
    'files:',
    ...files.map((file: any) => `${file.relative_path ?? file.path ?? ''} lines=${file.line_count ?? 'unknown'} bytes=${file.byte_count ?? 'unknown'} type=${file.file_type ?? 'unknown'} scope=${file.scope_classification ?? 'unknown'}`),
  ]);
}

function isReadFileResult(record: any) {
  return typeof record.path === 'string'
    && typeof record.content === 'string'
    && typeof record.offset === 'number'
    && typeof record.returned_lines === 'number';
}

function renderReadFileResult(record: any) {
  const startLine = Number(record.offset);
  const returnedLines = Number(record.returned_lines);
  const endLine = returnedLines > 0 ? startLine + returnedLines - 1 : startLine - 1;
  return [
    `path: ${record.path}`,
    `lines: ${startLine}-${endLine} of ${record.total_lines ?? 'unknown'}`,
    record.total_lines_exact !== undefined ? `total_lines_exact: ${record.total_lines_exact}` : null,
    record.total_lines_status !== undefined ? `total_lines_status: ${record.total_lines_status}` : null,
    record.line_window_complete !== undefined ? `line_window_complete: ${record.line_window_complete}` : null,
    `returned_lines: ${record.returned_lines}`,
    `next_offset: ${record.next_offset ?? 'null'}`,
    record.content_sha256 !== undefined ? `content_sha256: ${record.content_sha256}` : null,
    'content:',
    String(record.content ?? ''),
  ].filter((line: any) => line !== null).join('\n');
}

function renderSearchResult(toolName: any, record: any, renderContext: Record<string, unknown>= {}) {
  const matches = Array.isArray(record.matches)
    ? record.matches.map(String)
    : Array.isArray(record.match_objects)
      ? record.match_objects.map((item: any) => item.text !== undefined ? `${item.path}:${item.line}: ${item.text}` : item.count !== undefined ? `${item.path}: ${item.count}` : String(item.path))
      : [];
  const mode = toolName === 'fs_grep_search' ? [`mode: ${record.output_mode ?? renderContext.grepOutputMode ?? 'files_with_matches'}`] : [];
  return compactLines([
    `${toolName}: ${record.status ?? 'ok'}`,
    ...mode,
    `count: ${record.count ?? 'unknown'}`,
    `count_exact: ${record.count_exact ?? true}`,
    record.scanned_unit === 'matched_entries' && record.scanned !== undefined ? `matched_entries_scanned: ${record.scanned}` : record.scanned !== undefined ? `scanned: ${record.scanned}` : null,
    record.scanned_unit !== undefined ? `scanned_unit: ${record.scanned_unit}` : null,
    `returned: ${record.returned ?? matches.length}`,
    record.order !== undefined ? `order: ${record.order}` : null,
    record.cache_hit !== undefined ? `cache_hit: ${record.cache_hit}` : null,
    record.cache_policy !== undefined ? `cache_policy: ${record.cache_policy}` : null,
    record.snapshot_id !== undefined ? `snapshot_id: ${record.snapshot_id ?? 'null'}` : null,
    record.requested_snapshot_id !== undefined ? `requested_snapshot_id: ${record.requested_snapshot_id ?? 'null'}` : null,
    record.snapshot_complete !== undefined ? `snapshot_complete: ${record.snapshot_complete}` : null,
    record.cache_memory_bytes !== undefined ? `cache_memory_bytes: ${record.cache_memory_bytes ?? 'null'}` : null,
    record.timeout_ms !== undefined ? `timeout_ms: ${record.timeout_ms ?? 'null'}` : null,
    renderFreshnessLine(record.freshness),
    record.matches_format !== undefined ? `matches_format: ${record.matches_format}` : null,
    record.match_objects_authoritative !== undefined ? `match_objects_authoritative: ${record.match_objects_authoritative}` : null,
    `has_more: ${record.has_more ?? false}`,
    `next_offset: ${record.next_offset ?? 'null'}`,
    'matches:',
    ...matches,
  ]);
}

function renderFreshnessLine(value: any) {
  const freshness = asRecord(value);
  if (!freshness.type && !freshness.sha256 && !freshness.tree_sha256) return null;
  const parts = [
    freshness.type ? `type=${freshness.type}` : null,
    freshness.sha256 ? `sha256=${freshness.sha256}` : null,
    freshness.tree_sha256 ? `tree_sha256=${freshness.tree_sha256}` : null,
    freshness.tree_entry_count !== undefined ? `tree_entry_count=${freshness.tree_entry_count}` : null,
    freshness.tree_truncated !== undefined ? `tree_truncated=${freshness.tree_truncated}` : null,
  ].filter((part: any) => typeof part === 'string');
  return parts.length > 0 ? `freshness: ${parts.join(' ')}` : null;
}

function renderCompactRecord(record: any) {
  if (record.schema === 'local.filesystem.stat.v1' || record.type) {
    return compactLines([
      'fs_stat: ok',
      `path: ${record.path ?? ''}`,
      record.relative_path !== undefined ? `relative_path: ${record.relative_path}` : null,
      record.type !== undefined ? `type: ${record.type}` : null,
    record.size !== undefined ? `size: ${record.size}` : null,
    record.mtime !== undefined ? `mtime: ${record.mtime}` : null,
      record.sha256 !== undefined ? `sha256: ${record.sha256}` : null,
      record.entry_count !== undefined ? `entry_count: ${record.entry_count}` : null,
      record.tree_entry_count !== undefined ? `tree_entry_count: ${record.tree_entry_count}` : null,
      record.tree_truncated !== undefined ? `tree_truncated: ${record.tree_truncated}` : null,
      record.tree_sha256 !== undefined ? `tree_sha256: ${record.tree_sha256}` : null,
    ]);
  }
  const lines = [
    `${filesystemToolLabel(record)}: ${record.status ?? 'ok'}`,
    record.path !== undefined ? `path: ${record.path}` : null,
    record.relative_path !== undefined ? `relative_path: ${record.relative_path}` : null,
    record.size !== undefined ? `size: ${record.size}` : null,
    record.occurrences !== undefined ? `occurrences: ${record.occurrences}` : null,
    record.start_line !== undefined ? `start_line: ${record.start_line}` : null,
    record.end_line !== undefined ? `end_line: ${record.end_line}` : null,
    record.inserted_lines !== undefined ? `inserted_lines: ${record.inserted_lines}` : null,
    record.recursive !== undefined ? `recursive: ${record.recursive}` : null,
    record.created !== undefined ? `created: ${record.created}` : null,
    record.create_parent_directories !== undefined ? `create_parent_directories: ${record.create_parent_directories}` : null,
    record.operation !== undefined ? `operation: ${record.operation}` : null,
    record.overwrite !== undefined ? `overwrite: ${record.overwrite}` : null,
    record.before_sha256 !== undefined ? `before_sha256: ${record.before_sha256}` : null,
    record.after_sha256 !== undefined ? `after_sha256: ${record.after_sha256}` : null,
  ];
  const from = asRecord(record.from);
  const to = asRecord(record.to);
  if (from.path || from.relative_path) lines.push(`from: ${from.relative_path ?? from.path}`);
  if (to.path || to.relative_path) lines.push(`to: ${to.relative_path ?? to.path}`);
  return compactLines(lines);
}

function filesystemToolLabel(record: any) {
  const schema = typeof record.schema === 'string' ? record.schema : '';
  const match = schema.match(/^local\.filesystem\.(.+)\.v1$/);
  return match ? `fs_${match[1]}` : 'fs_result';
}

function compactLines(lines: any) {
  return lines.filter((line: any) => typeof line === 'string' && line.length > 0).join('\n');
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}
