export function compactQuantity(value: number): string {
  if (value < 1_000) return value.toLocaleString('en-US');
  const units = ['k', 'm', 'b'];
  let scaled = value;
  let unitIndex = -1;
  do {
    scaled /= 1_000;
    unitIndex += 1;
  } while (scaled >= 999.5 && unitIndex < units.length - 1);
  const digits = scaled < 10 ? 1 : 0;
  return `${Number(scaled.toFixed(digits))}${units[unitIndex]}`;
}

function counted(value: number, singular: string, plural = `${singular}s`): string {
  return `${compactQuantity(value)} ${value === 1 ? singular : plural}`;
}

function authoritativeCharacterCount(structured: any, fallback: number): number {
  const candidates = [
    structured?.full_output_char_length,
    structured?.result?.full_output_char_length,
    structured?.rendered_text_char_length,
  ];
  return candidates.find((value) => Number.isInteger(value) && value >= 0) ?? fallback;
}

export function summarizeMcpResult(result: any, fullText: string): string {
  const structured = result?.structuredContent;
  const path = structured?.relative_path ?? structured?.path;
  const lines = structured?.returned_lines;
  if (typeof path === 'string' && typeof lines === 'number') {
    if (lines === 0 && typeof structured?.total_lines === 'number') {
      const start = structured?.requested_start_line ?? structured?.offset;
      const end = structured?.requested_end_line;
      const range = typeof start === 'number'
        ? `${start}${typeof end === 'number' ? `–${end}` : ''}`
        : 'requested range';
      if (typeof start === 'number' && start > structured.total_lines) {
        return `range ${range} is past EOF; ${path} has ${structured.total_lines} lines`;
      }
      return `no lines returned from ${range}; ${path} has ${structured.total_lines} lines`;
    }
    return `read ${path} (${counted(lines, 'line')})`;
  }
  if (structured?.schema === 'local.filesystem.stat.v1' && typeof path === 'string') {
    const type = typeof structured?.type === 'string' ? structured.type : 'path';
    const size = typeof structured?.size === 'number'
      ? ` · ${counted(structured.size, 'byte')}`
      : '';
    return `${type} ${path}${size}`;
  }
  if (structured?.schema === 'local.filesystem.str_replace_file.v1' && typeof path === 'string') {
    const occurrences = typeof structured?.occurrences === 'number' ? structured.occurrences : 1;
    return `replaced ${counted(occurrences, 'occurrence')} in ${path}`;
  }
  if (structured?.schema === 'narada.task.inbox.bridge.v1') {
    const count = typeof structured?.count === 'number' ? structured.count : 0;
    const status = typeof structured?.status === 'string' ? structured.status : 'ok';
    return `${status} · ${counted(count, 'envelope')}`;
  }
  const summary = structured?.result_summary;
  const schema = summary?.schema ?? structured?.schema;
  const status = summary?.status ?? structured?.status;
  if (typeof status === 'string' && typeof path === 'string') return `${status} ${path}`;
  const chars = authoritativeCharacterCount(structured, fullText.length);
  if (schema === 'narada.mcp_loader.result_page.v1') {
    return `MCP loader result page${typeof status === 'string' ? ` · ${status}` : ''} · ${counted(chars, 'character')}`;
  }
  const identity = [schema, status].filter((value) => typeof value === 'string').join(': ');
  return `${identity || 'MCP result'} (${counted(chars, 'character')})`;
}

export function collapseMcpResultByDefault(): boolean {
  return true;
}
