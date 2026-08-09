export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[];

export interface JsonObject {
  [key: string]: JsonValue | undefined;
}

export type RpcId = string | number | null;

export interface RpcRecord {
  [key: string]: unknown;
  id?: RpcId;
  jsonrpc?: string;
  method?: string;
  params?: RpcRecord;
  result?: RpcRecord;
  error?: RpcRecord;
  data?: RpcRecord;
  structuredContent?: RpcRecord;
  serverInfo?: RpcRecord;
  lifecycle?: RpcRecord;
  validation?: RpcRecord;
  concurrency?: RpcRecord;
  request?: RpcRecord;
  source_ref?: RpcRecord;
  capabilities?: RpcRecord;
  tools?: RpcRecord[];
  resources?: RpcRecord[];
  contents?: RpcRecord[];
  events?: RpcRecord[];
  prompts?: RpcRecord[];
  memberships?: RpcRecord[];
  close_blockers?: unknown[];
  conflict_guards?: unknown[];
  status?: string;
  new_status?: string;
  schema?: string;
  name?: string;
  version?: string;
  protocolVersion?: string;
  code?: string | number;
  message?: string;
  description?: string;
  mimeType?: string;
  text?: string;
  body?: string;
  detail?: string;
  site_root?: string;
  site_root_source?: string;
  authority_posture?: string;
  surface_type?: string;
  tool_posture?: string;
  site_policy?: string;
  target_locus_guard?: string;
  posture?: string;
  can_self_restart?: boolean;
  restart_mechanism?: string;
  chapter_id?: string;
  event_id?: string;
  request_path?: string;
  refusal_code?: string;
  reason?: string;
  count?: number;
  compacted?: number;
  membership_count?: number;
}

export function asRpcRecord(value: unknown, label = 'JSON value'): RpcRecord {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new TypeError(`${label} must be a JSON object`);
  }
  return value as RpcRecord;
}

export function parseRpcRecord(text: string, label = 'JSON-RPC response'): RpcRecord {
  return asRpcRecord(JSON.parse(text) as unknown, label);
}

export function parseRpcLines(stdout: string, label: string): RpcRecord[] {
  const lines = stdout.trim().split(/\r?\n/).filter(Boolean);
  if (lines.length === 0) throw new Error(`${label}: no JSON-RPC output`);
  return lines.map((line, index) => parseRpcRecord(line, `${label} line ${index + 1}`));
}

export function rpc(id: RpcId, method: string, params: JsonObject = {}): string {
  return JSON.stringify({ jsonrpc: '2.0', id, method, params });
}

export function toolCall(id: RpcId, name: string, args: JsonObject = {}): string {
  return rpc(id, 'tools/call', { name, arguments: args });
}

export function structured(response: RpcRecord | undefined): RpcRecord {
  if (response === undefined) return {};
  const result = response.result ?? {};
  return result.structuredContent ?? result;
}

export function byId(lines: readonly RpcRecord[]): Map<string, RpcRecord> {
  return new Map(lines.map((line) => [String(line.id), line]));
}
