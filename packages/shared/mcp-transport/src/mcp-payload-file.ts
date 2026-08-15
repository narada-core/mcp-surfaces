import { closeSync, existsSync, fsyncSync, linkSync, mkdirSync, openSync, readFileSync, readdirSync, rmSync, statSync, unlinkSync, writeSync } from 'node:fs';
import { readFile as readFileAsync, stat as statAsync } from 'node:fs/promises';
import { dirname, relative, resolve } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { z } from 'zod';

const DEFAULT_MAX_BYTES = 256 * 1024;
const DEFAULT_OUTPUT_MAX_BYTES = 10 * 1024 * 1024;
const DEFAULT_PAYLOAD_DIR = '.ai/tmp/mcp-payloads';
const DEFAULT_OUTPUT_DIR = '.ai/tmp/mcp-outputs';
const DEFAULT_WORKSPACE_DIR = 'workspace';
export const DEFAULT_INLINE_PAYLOAD_CHAR_LIMIT = 20_000;
export const DEFAULT_INLINE_OUTPUT_CHAR_LIMIT = 2_000;
export const DEFAULT_OUTPUT_SHOW_CHAR_LIMIT = 10_000;
export const MAX_OUTPUT_SHOW_CHAR_LIMIT = 20_000;
export const DEFAULT_OUTPUT_READ_TIMEOUT_MS = 5_000;
export const MAX_OUTPUT_READ_TIMEOUT_MS = 15_000;
const MAX_OUTPUT_PAGE_BYTES = 12 * 1024;
const MAX_INLINE_RESPONSE_BYTES = 32 * 1024;
const MIN_INLINE_OUTPUT_CHAR_LIMIT = 512;
const DEFAULT_RESOURCE_PAGE_LIMIT = 100;
const MAX_RESOURCE_PAGE_LIMIT = 1_000;
const REF_PATTERN = /^mcp_payload:([A-Za-z0-9][A-Za-z0-9_-]{2,63})@v([1-9][0-9]*)$/;
const OUTPUT_REF_PATTERN = /^mcp_output:([A-Za-z0-9][A-Za-z0-9_-]{2,63})$/;
const DEFAULT_INLINE_PAYLOAD_EXEMPT_FIELDS = new Set([
  'payload_ref',
  'payload_path',
  'payload_file',
  'ref',
  'source_ref',
  'workflow_ref',
  'operation_id',
  'task_id',
  'task_number',
  'agent_id',
  'identity',
  'identity_name',
  'surface_id',
  'hwnd',
]);
const DEFAULT_INLINE_OBJECT_PAYLOAD_FIELDS = new Set([
  'payload',
  'content',
  'evidence',
  'verification',
  'self_certification',
  'recovery_truthfulness',
  'authority_basis',
  'active_task',
  'worktree_state',
  'scope',
]);

export type McpTransportScope = Readonly<{
  siteRoot: string;
  payloadDir: string;
  outputDir: string;
  maxPayloadBytes: number;
  maxOutputBytes: number;
}>;

type TransportScopeOptions = {
  scope?: McpTransportScope;
  siteRoot?: string;
  payloadDir?: string;
  outputDir?: string;
  maxBytes?: number;
  maxPayloadBytes?: number;
  maxOutputBytes?: number;
};

type PayloadResolutionOptions = TransportScopeOptions & {
  toolName: string;
  args: unknown;
  allowedTools: string[];
  payloadRefMode?: string;
};

type PayloadWorkspaceOptions = TransportScopeOptions & {
  args?: unknown;
};

type PayloadPruneOptions = TransportScopeOptions & {
  payloadIdPrefix: string;
  maxEntries: number;
  maxAgeMs: number;
  now?: number;
};

type OutputCreateOptions = TransportScopeOptions & {
  toolName: string;
  value: unknown;
  fullText: string;
  inlineLimit: number;
  createdBy: string | null;
};

type OutputReadOptions = TransportScopeOptions & {
  ref: string;
};

type PayloadReadOptions = TransportScopeOptions & {
  ref: string;
};

type RevisionWriteOptions = TransportScopeOptions & {
  record: Record<string, unknown>;
};

type OutputBuilderOptions = TransportScopeOptions & {
  toolName?: string;
  value?: unknown;
  isError?: boolean;
  limit?: number;
  outputPageLimit?: number;
  createdBy?: string | null;
  readerTool?: string;
};

type OutputShowOptions = TransportScopeOptions & {
  args?: unknown;
};

type OutputResourceListOptions = TransportScopeOptions & {
  cursor?: unknown;
  offset?: unknown;
  limit?: unknown;
};

type OutputResourceReadOptions = TransportScopeOptions & {
  uri?: unknown;
};

const transportScopeInputSchema = z.object({
  siteRoot: z.string().trim().min(1),
  payloadDir: z.string().trim().min(1).optional(),
  outputDir: z.string().trim().min(1).optional(),
  maxPayloadBytes: z.number().int().positive().optional(),
  maxOutputBytes: z.number().int().positive().optional(),
}).strict();

const transportScopeSchema = z.object({
  siteRoot: z.string().min(1),
  payloadDir: z.string().min(1),
  outputDir: z.string().min(1),
  maxPayloadBytes: z.number().int().positive(),
  maxOutputBytes: z.number().int().positive(),
}).strict();

export function createTransportScope(input: unknown): McpTransportScope {
  const parsed = transportScopeInputSchema.parse(input ?? {});
  const siteRoot = resolveSiteRoot(parsed.siteRoot);
  const payloadDir = resolveManagedDirectory(siteRoot, parsed.payloadDir ?? DEFAULT_PAYLOAD_DIR, 'payload_directory');
  const outputDir = resolveManagedDirectory(siteRoot, parsed.outputDir ?? DEFAULT_OUTPUT_DIR, 'output_directory');
  return Object.freeze({
    siteRoot,
    payloadDir,
    outputDir,
    maxPayloadBytes: parsed.maxPayloadBytes ?? DEFAULT_MAX_BYTES,
    maxOutputBytes: parsed.maxOutputBytes ?? DEFAULT_OUTPUT_MAX_BYTES,
  });
}

function normalizeOutputReadTimeout(value: any): number {
  if (value === undefined || value === null) return DEFAULT_OUTPUT_READ_TIMEOUT_MS;
  const parsed = z.number().finite().int().safeParse(value);
  if (!parsed.success || parsed.data < 1) throw new Error('output_read_timeout_must_be_positive_integer');
  if (parsed.data > MAX_OUTPUT_READ_TIMEOUT_MS) throw new Error(`output_read_timeout_exceeds_transport_maximum: ${parsed.data} > ${MAX_OUTPUT_READ_TIMEOUT_MS}`);
  return parsed.data;
}

async function readOutputRecordAsync({ scope, siteRoot, ref, maxBytes, outputDir }: OutputReadOptions, timeoutMs: number) {
  const transportScope = resolveTransportScope({ scope, siteRoot, outputDir, maxBytes });
  const parsed = parseOutputRef(ref);
  const path = outputPath({ siteRoot: transportScope.siteRoot, outputDir: transportScope.outputDir, outputId: parsed.outputId });
  let stat;
  try {
    stat = await withOutputReadTimeout(statAsync(path), timeoutMs);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') throw new Error(`output_ref_not_found: ${ref}`);
    throw error;
  }
  if (!stat.isFile()) throw new Error(`output_ref_not_file: ${ref}`);
  if (stat.size > transportScope.maxOutputBytes) throw new Error(`output_ref_too_large: ${stat.size} > ${transportScope.maxOutputBytes}`);
  let serialized: string;
  try {
    serialized = await withOutputReadTimeout(readFileAsync(path, { encoding: 'utf8' }), timeoutMs);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') throw new Error(`output_ref_not_found: ${ref}`);
    throw error;
  }
  let record: any;
  try {
    record = JSON.parse(serialized);
  } catch (error) {
    throw new Error(`output_ref_invalid_json: ${error instanceof Error ? error.message : String(error)}`);
  }
  if (!record || typeof record !== 'object' || Array.isArray(record)) throw new Error(`output_ref_record_must_be_object: ${ref}`);
  if (record.schema !== 'narada.mcp_output_ref.v1') throw new Error(`output_ref_schema_unsupported: ${record.schema}`);
  if (record.ref !== ref || record.output_id !== parsed.outputId) throw new Error(`output_ref_metadata_mismatch: ${ref}`);
  const fullText = presentationJson(record.full_output);
  if (record.full_output_char_length !== fullText.length) throw new Error(`output_ref_length_mismatch: ${ref}`);
  if (record.sha256 !== sha256(stableJson(record.full_output))) throw new Error(`output_ref_sha256_mismatch: ${ref}`);
  return { ...record, byte_size: stat.size, output_path: normalizePath(relative(resolveSiteRoot(transportScope.siteRoot), path)) };
}

function withOutputReadTimeout<T>(operation: Promise<T>, timeoutMs: number): Promise<T> {
  return new Promise<T>((resolvePromise, rejectPromise) => {
    const timer = setTimeout(() => rejectPromise(Object.assign(new Error(`output_ref_read_timed_out:${timeoutMs}`), { code: 'OUTPUT_READ_TIMED_OUT' })), timeoutMs);
    operation.then((value) => { clearTimeout(timer); resolvePromise(value); }, (error) => { clearTimeout(timer); rejectPromise(error); });
  });
}

function isOutputReadTimeout(error: unknown): boolean {
  return Boolean(error && typeof error === 'object' && (error as { code?: string }).code === 'OUTPUT_READ_TIMED_OUT');
}

export async function outputShowAsync({ scope, siteRoot, args, maxBytes, outputDir }: OutputShowOptions = {}) {
  const transportScope = resolveTransportScope({ scope, siteRoot, outputDir, maxBytes });
  const input = parseArgumentRecord(args);
  if (Object.prototype.hasOwnProperty.call(input, 'target_site_root')) {
    throw new Error('output_target_site_root_not_supported: output refs are bound to the current MCP site scope');
  }
  const ref = requireOutputRef(input, 'output_show_requires_ref');
  const offset = normalizeOutputShowOffset(input.offset);
  const outputLimit = normalizeOutputShowLimit(input.limit ?? input.output_limit);
  const timeoutMs = normalizeOutputReadTimeout(input.timeout_ms);
  try {
    const record = await readOutputRecordAsync({ scope: transportScope, ref }, timeoutMs);
    return publicOutputShowRecord(record, { outputLimit, offset });
  } catch (error) {
    if (isOutputReadTimeout(error)) {
      return {
        schema: 'narada.mcp_output_page.v1',
        status: 'timed_out',
        ref,
        offset,
        limit: outputLimit,
        next_offset: offset,
        output_limit: outputLimit,
        output_truncated: true,
        output_text: '',
        retryable: true,
        error: { code: 'output_ref_read_timed_out', timeout_ms: timeoutMs },
      };
    }
    throw error;
  }
}

function resolveTransportScope({ scope, siteRoot, payloadDir, outputDir, maxBytes, maxPayloadBytes, maxOutputBytes }: TransportScopeOptions = {}): McpTransportScope {
  if (scope !== undefined) {
    const parsed = transportScopeSchema.parse(scope);
    const validated = createTransportScope({
      siteRoot: parsed.siteRoot,
      payloadDir: parsed.payloadDir,
      outputDir: parsed.outputDir,
      maxPayloadBytes: parsed.maxPayloadBytes,
      maxOutputBytes: parsed.maxOutputBytes,
    });
    if (siteRoot !== undefined || payloadDir !== undefined || outputDir !== undefined || maxBytes !== undefined || maxPayloadBytes !== undefined || maxOutputBytes !== undefined) {
      throw new Error('transport_scope_cannot_be_combined_with_legacy_scope_overrides');
    }
    return validated;
  }
  return createTransportScope({
    siteRoot: siteRoot ?? process.cwd(),
    payloadDir,
    outputDir,
    maxPayloadBytes: maxPayloadBytes ?? maxBytes,
    maxOutputBytes,
  });
}

export function resolveToolPayloadArgs({
  scope,
  siteRoot,
  toolName,
  args,
  allowedTools,
  maxBytes,
  payloadDir,
  payloadRefMode = 'replace_args',
}: PayloadResolutionOptions) {
  const transportScope = resolveTransportScope({ scope, siteRoot, payloadDir, maxBytes });
  siteRoot = transportScope.siteRoot;
  maxBytes = transportScope.maxPayloadBytes;
  payloadDir = transportScope.payloadDir;
  const input = parseArgumentRecord(args);
  const payloadPath = typeof input.payload_path === 'string' && input.payload_path.trim().length > 0
    ? input.payload_path.trim()
    : null;
  const payloadRef = typeof input.payload_ref === 'string' && input.payload_ref.trim().length > 0
    ? input.payload_ref.trim()
    : null;
  if (payloadPath && payloadRef) throw new Error('payload_transport_must_choose_one_of_payload_path_or_payload_ref');
  if (!payloadPath && !payloadRef) return { args: input, payloadSource: null };
  if (!allowedTools.includes(toolName)) {
    throw new Error(`${payloadPath ? 'payload_path' : 'payload_ref'}_not_supported_for_tool: ${toolName}`);
  }

  if (payloadRef) {
    const revision = readPayloadRevision({ scope: transportScope, ref: payloadRef });
    const resolvedArgs = resolvePayloadRefArgs({ input, payload: revision.payload, payloadRefMode });
    return {
      args: resolvedArgs,
      payloadSource: {
        kind: 'ref',
        ref: revision.ref,
        payload_id: revision.payload_id,
        revision: revision.revision,
        byte_size: revision.byte_size,
        sha256: revision.sha256,
        max_bytes: maxBytes,
        transient_not_authority: true,
      },
    };
  }

  if (!payloadPath) throw new Error('payload_path_required');
  const root = resolveSiteRoot(siteRoot);
  const allowedRoot = resolveManagedDirectory(root, payloadDir, 'payload_directory');
  const absolutePath = resolve(root, payloadPath);
  if (!isPathInside(absolutePath, allowedRoot)) {
    throw new Error(`payload_path_outside_allowed_staging: ${normalizePath(relative(root, absolutePath))}`);
  }

  let stat;
  try {
    stat = statSync(absolutePath);
  } catch {
    throw new Error(`payload_path_not_found: ${normalizePath(relative(root, absolutePath))}`);
  }
  if (!stat.isFile()) throw new Error(`payload_path_not_file: ${normalizePath(relative(root, absolutePath))}`);
  if (stat.size > maxBytes) throw new Error(`payload_path_too_large: ${stat.size} > ${maxBytes}`);

  let parsed;
  try {
    parsed = JSON.parse(readFileSync(absolutePath, 'utf8'));
  } catch (error) {
    throw new Error(`payload_path_invalid_json: ${error instanceof Error ? error.message : String(error)}`);
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('payload_path_json_must_be_object');
  }

  return {
    args: parsed,
    payloadSource: {
      kind: 'file',
      path: normalizePath(relative(root, absolutePath)),
      byte_size: stat.size,
      max_bytes: maxBytes,
      transient_not_authority: true,
    },
  };
}

export function prunePayloadWorkspaces({
  scope,
  siteRoot,
  payloadIdPrefix,
  maxEntries,
  maxAgeMs,
  payloadDir,
  now = Date.now(),
}: PayloadPruneOptions) {
  const transportScope = resolveTransportScope({ scope, siteRoot, payloadDir });
  siteRoot = transportScope.siteRoot;
  payloadDir = transportScope.payloadDir;
  if (typeof payloadIdPrefix !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(payloadIdPrefix)) {
    throw new Error('payload_prune_prefix_invalid');
  }
  if (!Number.isInteger(maxEntries) || maxEntries < 1) throw new Error('payload_prune_max_entries_invalid');
  if (!Number.isFinite(maxAgeMs) || maxAgeMs < 0) throw new Error('payload_prune_max_age_ms_invalid');
  const workspaceRoot = resolve(resolveManagedDirectory(siteRoot, payloadDir, 'payload_directory'), DEFAULT_WORKSPACE_DIR);
  if (!existsSync(workspaceRoot)) {
    return {
      status: 'ok',
      payload_id_prefix: payloadIdPrefix,
      max_entries: maxEntries,
      max_age_ms: maxAgeMs,
      considered_count: 0,
      retained_count: 0,
      removed_count: 0,
      retained_payload_ids: [],
      removed_payload_ids: [],
    };
  }
  const entries = readdirSync(workspaceRoot, { withFileTypes: true })
    .filter((entry: any) => entry.isDirectory() && entry.name.startsWith(payloadIdPrefix))
    .filter((entry: any) => entry.isDirectory() && entry.name.startsWith(payloadIdPrefix))
    .map((entry: any) => {
      const path = resolve(workspaceRoot, entry.name);
      return { payloadId: entry.name, path, mtimeMs: statSync(path).mtimeMs };
    })
    .sort((left: any, right: any) => right.mtimeMs - left.mtimeMs || right.payloadId.localeCompare(left.payloadId));
  const retainedPayloadIds: string[] = [];
  const removedPayloadIds: string[] = [];
  for (const [index, entry] of entries.entries()) {
    const expired = now - entry.mtimeMs > maxAgeMs;
    if (index >= maxEntries || expired) {
      rmSync(entry.path, { recursive: true, force: true });
      removedPayloadIds.push(entry.payloadId);
    } else {
      retainedPayloadIds.push(entry.payloadId);
    }
  }
  return {
    status: 'ok',
    payload_id_prefix: payloadIdPrefix,
    max_entries: maxEntries,
    max_age_ms: maxAgeMs,
    considered_count: entries.length,
    retained_count: retainedPayloadIds.length,
    removed_count: removedPayloadIds.length,
    retained_payload_ids: retainedPayloadIds,
    removed_payload_ids: removedPayloadIds,
  };
}

function resolvePayloadRefArgs({ input, payload, payloadRefMode }: any) {
  if (payloadRefMode === 'merge_args') return { ...asRecord(payload), ...withoutPayloadTransport(input) };
  if (payloadRefMode === 'merge_args_prefer_payload_placeholders') return mergeArgsPreferPayloadPlaceholders({ input, payload });
  if (payloadRefMode === 'payload_field' && hasPayloadRefCompanionArgs(input)) return { ...withoutPayloadTransport(input), payload };
  return payload;
}

function mergeArgsPreferPayloadPlaceholders({ input, payload }: any) {
  const resolved = { ...asRecord(payload) };
  for (const [key, value] of Object.entries(withoutPayloadTransport(input))) {
    if (Object.prototype.hasOwnProperty.call(resolved, key) && isPlaceholderString(value)) continue;
    resolved[key] = value;
  }
  return resolved;
}

function isPlaceholderString(value: any) {
  if (typeof value !== 'string') return false;
  const trimmed = value.trim();
  return /^<!--[\s\S]*-->$/.test(trimmed)
    || /^<[^>]+>$/.test(trimmed)
    || /^<move original\b/i.test(trimmed);
}

function hasPayloadRefCompanionArgs(input: any) {
  return Object.keys(withoutPayloadTransport(input)).length > 0;
}

function withoutPayloadTransport(input: any) {
  const { payload_ref, payload_path, payload, payload_file, ...rest } = input;
  return rest;
}

export function attachPayloadSource(result: any, payloadSource: any) {
  if (!payloadSource || !result || typeof result !== 'object' || Array.isArray(result)) return result;
  return { ...result, payload_source: payloadSource };
}

export function enforceInlinePayloadLimit({
  toolName,
  args,
  limit = DEFAULT_INLINE_PAYLOAD_CHAR_LIMIT,
  exemptFields = DEFAULT_INLINE_PAYLOAD_EXEMPT_FIELDS,
  objectPayloadFields = DEFAULT_INLINE_OBJECT_PAYLOAD_FIELDS,
  allowPayloadCreation = false,
}: Record<string, unknown>= {}) {
  const input = parseArgumentRecord(args);
  const currentToolName = typeof toolName === 'string' ? toolName : '';
  const inlineLimit = typeof limit === 'number' ? limit : DEFAULT_INLINE_PAYLOAD_CHAR_LIMIT;
  const exemptFieldSet = exemptFields instanceof Set ? exemptFields : DEFAULT_INLINE_PAYLOAD_EXEMPT_FIELDS;
  const objectPayloadFieldSet = objectPayloadFields instanceof Set ? objectPayloadFields : DEFAULT_INLINE_OBJECT_PAYLOAD_FIELDS;
  if (allowPayloadCreation === true && isPayloadWorkspaceTool(currentToolName)) return;
  const violations: any[] = [];
  visitInlinePayload(input, [], { limit: inlineLimit, exemptFields: exemptFieldSet, objectPayloadFields: objectPayloadFieldSet, violations });
  const first = violations[0];
  if (!first) return;
  const createArgs = buildPayloadCreateTemplate(violations);
  const retryArgs = buildPayloadRetryTemplate(currentToolName);
  throw new Error(
    `inline_payload_too_long: field=${first.field} length=${first.length} threshold=${inlineLimit} remediation=call mcp_payload_create then retry_with_payload_ref mcp_payload_create_args=${JSON.stringify(createArgs)} retry_args=${JSON.stringify(retryArgs)}`
  );
}

function buildPayloadCreateTemplate(violations: any) {
  const payload = {};
  for (const violation of violations.slice(0, 10)) {
    assignPath(payload, String(violation.field).split('.'), `<move original ${violation.field} here>`);
  }
  return { payload, created_by: '<agent_id_or_principal>' };
}

function buildPayloadRetryTemplate(toolName: any) {
  return { tool: toolName || '<original_tool>', args: { payload_ref: 'mcp_payload:<id>@v1' } };
}

function assignPath(target: any, path: any, value: any) {
  let current = target;
  for (let index = 0; index < path.length; index++) {
    const key = path[index];
    if (index === path.length - 1) {
      current[containerKey(key)] = value;
      return;
    }
    const targetKey = containerKey(key);
    const nextContainer = /^\d+$/.test(path[index + 1] ?? '') ? [] : {};
    current[targetKey] = isPlainObject(current[targetKey]) || Array.isArray(current[targetKey]) ? current[targetKey] : nextContainer;
    current = current[targetKey];
  }
}

function containerKey(key: any) {
  return /^\d+$/.test(String(key)) ? Number(key) : key;
}

function visitInlinePayload(value: any, path: any, context: any) {
  if (typeof value === 'string') {
    const field = path[path.length - 1] ?? '<root>';
    if (!context.exemptFields.has(field) && value.length > context.limit) {
      context.violations.push({ field: pathToField(path), length: value.length, threshold: context.limit });
    }
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((item: any, index: any) => visitInlinePayload(item, [...path, String(index)], context));
    return;
  }
  if (!isPlainObject(value)) return;

  const field = path[path.length - 1];
  if (field && context.objectPayloadFields.has(field)) {
    const length = stableJson(value).length;
    if (length > context.limit) {
      context.violations.push({ field: pathToField(path), length, threshold: context.limit });
    }
  }
  for (const [key, child] of Object.entries(value)) {
    visitInlinePayload(child, [...path, key], context);
  }
}

function pathToField(path: any) {
  return path.length > 0 ? path.join('.') : '<root>';
}

function isPayloadWorkspaceTool(toolName: any) {
  return ['mcp_payload_create', 'mcp_payload_derive'].includes(toolName);
}

export function payloadCreate({ scope, siteRoot, args, maxBytes, payloadDir }: PayloadWorkspaceOptions & { payloadId?: string } = {}) {
  const transportScope = resolveTransportScope({ scope, siteRoot, payloadDir, maxBytes });
  siteRoot = transportScope.siteRoot;
  maxBytes = transportScope.maxPayloadBytes;
  payloadDir = transportScope.payloadDir;
  const input = parseArgumentRecord(args);
  const payload = payloadObjectFromArgs(input, {
    objectField: 'payload',
    jsonField: 'payload_json',
    objectMessage: 'payload_create_payload_must_be_object',
    jsonMessage: 'payload_create_payload_json_must_be_object',
    ambiguityMessage: 'payload_create_must_choose_one_of_payload_or_payload_json: send either non-empty payload object or payload_json string; empty payload object may accompany payload_json only as a client placeholder',
  });
  if (Object.keys(payload).length === 0 && input.allow_empty !== true) {
    throw new Error('payload_create_empty_payload_requires_allow_empty: payload object is empty; pass allow_empty=true only when an empty immutable payload is intentional. Use either {"payload":{"summary":"..."}} or {"payload_json":"{\\"summary\\":\\"...\\"}"}. For OpenCode-style clients, payload:{} may accompany payload_json only as a placeholder.');
  }
  const payloadId = input.payload_id ? validatePayloadId(String(input.payload_id)) : randomPayloadId();
  const createdAt = new Date().toISOString();
  const ref = buildPayloadRef(payloadId, 1);
  const revision = buildRevisionRecord({
    payloadId,
    revision: 1,
    payload,
    createdAt,
    createdBy: stringOrNull(input.created_by),
    source: { kind: 'create' },
    maxBytes,
  });
  const written = writeRevision({ scope: transportScope, record: revision });
  return publicRevisionResult({ status: written.status === 'existing' ? 'existing' : 'created', record: written.record, ref: written.record.ref });
}

export function payloadShow({ scope, siteRoot, args, maxBytes, payloadDir }: PayloadWorkspaceOptions = {}) {
  const transportScope = resolveTransportScope({ scope, siteRoot, payloadDir, maxBytes });
  siteRoot = transportScope.siteRoot;
  maxBytes = transportScope.maxPayloadBytes;
  payloadDir = transportScope.payloadDir;
  const revision = readPayloadRevision({ scope: transportScope, ref: requireRef(args, 'payload_show_requires_ref') });
  return publicRevisionResult({ status: 'ok', record: revision.record, includePayload: true });
}

export function payloadValidate({ scope, siteRoot, args, maxBytes, payloadDir }: PayloadWorkspaceOptions = {}) {
  const transportScope = resolveTransportScope({ scope, siteRoot, payloadDir, maxBytes });
  siteRoot = transportScope.siteRoot;
  maxBytes = transportScope.maxPayloadBytes;
  payloadDir = transportScope.payloadDir;
  const revision = readPayloadRevision({ scope: transportScope, ref: requireRef(args, 'payload_validate_requires_ref') });
  return publicRevisionResult({ status: 'valid', record: revision.record });
}

export function payloadDerive({ scope, siteRoot, args, maxBytes, payloadDir }: PayloadWorkspaceOptions = {}) {
  const transportScope = resolveTransportScope({ scope, siteRoot, payloadDir, maxBytes });
  siteRoot = transportScope.siteRoot;
  maxBytes = transportScope.maxPayloadBytes;
  payloadDir = transportScope.payloadDir;
  const input = parseArgumentRecord(args);
  const source = readPayloadRevision({ scope: transportScope, ref: requireRef(input, 'payload_derive_requires_source_ref', 'source_ref') });
  const deletePaths = payloadDeletePaths(input.delete_paths);
  const hasOverlayRoute = input.overlay !== undefined || input.overlay_json !== undefined;
  if (!hasOverlayRoute && deletePaths.length === 0) {
    throw new Error('payload_derive_requires_overlay_or_delete_paths');
  }
  const overlay = hasOverlayRoute
    ? payloadObjectFromArgs(input, {
      objectField: 'overlay',
      jsonField: 'overlay_json',
      objectMessage: 'payload_derive_overlay_must_be_object',
      jsonMessage: 'payload_derive_overlay_json_must_be_object',
      ambiguityMessage: 'payload_derive_must_choose_one_of_overlay_or_overlay_json: send either non-empty overlay object or overlay_json string; empty overlay object may accompany overlay_json only as a client placeholder',
    })
    : {};
  const payload = deleteObjectPaths(overlayObject(source.payload, overlay), deletePaths);
  const revision = source.revision + 1;
  const ref = buildPayloadRef(source.payload_id, revision);
  const createdAt = new Date().toISOString();
  const record = buildRevisionRecord({
    payloadId: source.payload_id,
    revision,
    payload,
    createdAt,
    createdBy: stringOrNull(input.created_by),
    source: { kind: 'derive', source_ref: source.ref, overlay_sha256: sha256(stableJson(overlay)), delete_paths: deletePaths },
    maxBytes,
  });
  const written = writeRevision({ scope: transportScope, record });
  return publicRevisionResult({ status: written.status === 'existing' ? 'existing' : 'derived', record: written.record, ref: written.record.ref, sourceRef: source.ref });
}

export function buildOutputRefToolContent({
  scope,
  siteRoot,
  toolName,
  value,
  isError = false,
  limit = DEFAULT_INLINE_OUTPUT_CHAR_LIMIT,
  createdBy = process.env.NARADA_AGENT_ID || null,
  readerTool = 'mcp_output_show',
}: OutputBuilderOptions = {}): any {
  const transportScope = resolveTransportScope({ scope, siteRoot });
  const outputSiteRoot = transportScope.siteRoot;
  const outputToolName = typeof toolName === 'string' ? toolName : 'unknown_tool';
  const inlineLimit = normalizeInlineOutputLimit(limit);
  const outputCreatedBy = typeof createdBy === 'string' ? createdBy : null;
  const valueRecord = isPlainObject(value) ? value as Record<string, unknown> : {};
  if (isOutputLocator(value)) {
    const text = JSON.stringify(value);
    if (!fitsToolResponse(text, value)) throw new Error('output_locator_response_exceeds_transport_budget');
    return { resultType: 'complete', content: [assistantTextContent(text)], structuredContent: value, ...(isError ? { isError: true } : {}) };
  }
  if (isOutputShowResult(value)) {
    const text = JSON.stringify(value, null, 2);
    if (!fitsToolResponse(text, value)) throw new Error('output_page_response_exceeds_transport_budget');
    return { resultType: 'complete', content: [assistantTextContent(text)], structuredContent: value, ...(isError ? { isError: true } : {}) };
  }

  const fullText = presentationJson(value);
  if (fullText.length <= inlineLimit && fitsToolResponse(fullText, value)) {
    return { resultType: 'complete', content: [assistantTextContent(fullText)], structuredContent: value, ...(isError ? { isError: true } : {}) };
  }

  const payloadRef = typeof valueRecord.ref === 'string' && valueRecord.ref.startsWith('mcp_payload:') ? valueRecord.ref : null;
  const materialized = outputCreate({ scope: transportScope, toolName: outputToolName, value, fullText, inlineLimit, createdBy: outputCreatedBy });
  const outputRef = String(materialized.ref ?? '');
  const outputReaderTool = typeof readerTool === 'string' && readerTool.trim().length > 0 ? readerTool.trim() : null;
  const envelope = buildOutputPageEnvelope({
    fullText,
    value,
    outputRef,
    payloadRef,
    outputToolName,
    outputReaderTool,
    outputSiteRoot,
    inlineLimit,
    isError,
  });
  const contentText = fitInlineJson(envelope, inlineLimit);
  return { resultType: 'complete', content: [assistantTextContent(contentText)], structuredContent: envelope, ...(isError ? { isError: true } : {}) };
}

export function buildBoundedToolResult({
  scope,
  siteRoot,
  toolName,
  value,
  isError = false,
  limit = DEFAULT_INLINE_OUTPUT_CHAR_LIMIT,
  outputPageLimit = DEFAULT_OUTPUT_SHOW_CHAR_LIMIT,
  readerTool = 'mcp_output_show',
}: OutputBuilderOptions = {}): any {
  const transportScope = resolveTransportScope({ scope, siteRoot });
  const fullText = presentationJson(value);
  const inlineLimit = normalizeBoundedOutputLimit(limit);
  const pageLimit = typeof outputPageLimit === 'number' ? outputPageLimit : DEFAULT_OUTPUT_SHOW_CHAR_LIMIT;
  if (isOutputShowResult(value) && fitsToolResponse(fullText, value)) {
    return { resultType: 'complete', content: [assistantTextContent(fullText)], structuredContent: value, ...(isError ? { isError: true } : {}) };
  }
  if (fullText.length <= inlineLimit && fitsToolResponse(fullText, value)) {
    return { resultType: 'complete', content: [assistantTextContent(fullText)], structuredContent: value, ...(isError ? { isError: true } : {}) };
  }
  return buildOutputRefToolContent({
    scope: transportScope,
    toolName,
    value,
    isError,
    limit: inlineLimit,
    readerTool,
  });
}

function assistantTextContent(text: string): any {
  return { type: 'text', text, annotations: { audience: ['assistant'] } };
}

function buildOutputPageEnvelope({ fullText, value, outputRef, payloadRef, outputToolName, outputReaderTool, outputSiteRoot, inlineLimit, isError }: any) {
  let preview = takeUtf8Page(fullText, 0, Math.min(inlineLimit, MAX_OUTPUT_SHOW_CHAR_LIMIT), MAX_OUTPUT_PAGE_BYTES).chunk;
  while (true) {
    const nextOffset = preview.length < fullText.length ? preview.length : null;
    const envelope = {
      schema: 'narada.producer_output_page.v1',
      status: outputStatus(value, isError),
      truncated: true,
      truncation_reason: 'inline_transport_bound',
      ...(payloadRef ? { payload_ref: payloadRef } : {}),
      output_ref: outputRef,
      ref: outputRef,
      result_materialized: true,
      tool_name: outputToolName,
      offset: 0,
      limit: inlineLimit,
      next_offset: nextOffset,
      transport_offset: 0,
      transport_limit: inlineLimit,
      transport_next_offset: nextOffset,
      output_text: preview,
      output_truncated: nextOffset !== null,
      reader_tool: outputReaderTool,
      site_root: outputSiteRoot,
      read_command: outputReaderTool ? `${outputReaderTool}({ "ref": "${outputRef}", "offset": 0, "limit": ${DEFAULT_OUTPUT_SHOW_CHAR_LIMIT} })` : null,
      remediation: outputReaderTool
        ? `Use ${outputReaderTool} with output_ref/ref=${outputRef} to read the bounded produced JSON pages; continue with the returned next_offset.`
        : 'Use the output_ref resource to read bounded pages of the produced JSON output.',
      inline_limit: inlineLimit,
      full_output_char_length: fullText.length,
    };
    const contentText = JSON.stringify(envelope);
    if (contentText.length <= inlineLimit && fitsToolResponse(contentText, envelope)) return envelope;
    if (preview.length === 0) throw new Error('inline_output_envelope_limit_too_small');
    preview = takeUtf8Page(fullText, 0, Math.max(0, Math.floor(preview.length * 0.75)), MAX_OUTPUT_PAGE_BYTES).chunk;
  }
}

function fitsToolResponse(contentText: any, structuredContent: any) {
  return Buffer.byteLength(contentText, 'utf8') + Buffer.byteLength(JSON.stringify(structuredContent), 'utf8') <= MAX_INLINE_RESPONSE_BYTES;
}

function normalizeInlineOutputLimit(value: any) {
  const candidate = value === undefined || value === null
    ? DEFAULT_INLINE_OUTPUT_CHAR_LIMIT
    : value;
  const parsed = z.number().finite().int().safeParse(candidate);
  if (!parsed.success || parsed.data < MIN_INLINE_OUTPUT_CHAR_LIMIT) {
    throw new Error(`inline_output_limit_must_be_integer_at_least_${MIN_INLINE_OUTPUT_CHAR_LIMIT}`);
  }
  if (parsed.data > MAX_OUTPUT_SHOW_CHAR_LIMIT) {
    throw new Error(`inline_output_limit_exceeds_transport_maximum: ${parsed.data} > ${MAX_OUTPUT_SHOW_CHAR_LIMIT}`);
  }
  return parsed.data;
}

function normalizeBoundedOutputLimit(value: any) {
  const candidate = value === undefined || value === null ? DEFAULT_INLINE_OUTPUT_CHAR_LIMIT : value;
  const parsed = z.number().finite().safeParse(candidate);
  return normalizeInlineOutputLimit(parsed.success ? Math.min(parsed.data, MAX_OUTPUT_SHOW_CHAR_LIMIT) : candidate);
}

function fitInlineJson(value: any, limit: any) {
  const text = JSON.stringify(value);
  if (text.length > normalizeInlineOutputLimit(limit) || Buffer.byteLength(text, 'utf8') > MAX_INLINE_RESPONSE_BYTES) {
    throw new Error('inline_output_envelope_exceeds_transport_budget');
  }
  return text;
}

export function outputShow({ scope, siteRoot, args, maxBytes, outputDir }: OutputShowOptions = {}) {
  const transportScope = resolveTransportScope({ scope, siteRoot, outputDir, maxBytes });
  siteRoot = transportScope.siteRoot;
  maxBytes = transportScope.maxOutputBytes;
  outputDir = transportScope.outputDir;
  const input = parseArgumentRecord(args);
  if (Object.prototype.hasOwnProperty.call(input, 'target_site_root')) {
    throw new Error('output_target_site_root_not_supported: output refs are bound to the current MCP site scope');
  }
  const record = readOutputRecord({ scope: transportScope, ref: requireOutputRef(input, 'output_show_requires_ref') });
  return publicOutputShowRecord(record, {
    outputLimit: normalizeOutputShowLimit(input.limit ?? input.output_limit),
    offset: normalizeOutputShowOffset(input.offset),
  });
}

export function listOutputResources({ scope, siteRoot, outputDir, cursor, offset, limit }: OutputResourceListOptions = {}) {
  const transportScope = resolveTransportScope({ scope, siteRoot, outputDir });
  siteRoot = transportScope.siteRoot;
  outputDir = transportScope.outputDir;
  const page = normalizeResourcePage({ cursor, offset, limit });
  const dir = resolve(outputDir, DEFAULT_WORKSPACE_DIR);
  if (!existsSync(dir)) return { resources: [], ...page, next_offset: null, nextCursor: null, has_more: false };
  const allResources = readdirSync(dir)
    .filter((name: any) => name.endsWith('.json'))
    .sort()
    .map((name: any) => {
      const outputId = name.replace(/\.json$/, '');
      const ref = buildOutputRef(outputId);
      return {
        uri: outputResourceUri(ref),
        name: ref,
        title: ref,
        description: 'Materialized MCP output ref.',
        mimeType: 'application/json',
      };
    });
  const resources = allResources.slice(page.offset, page.offset + page.limit);
  const nextOffset = page.offset + resources.length < allResources.length ? page.offset + resources.length : null;
  return {
    resources,
    ...page,
    next_offset: nextOffset,
    nextCursor: nextOffset === null ? null : String(nextOffset),
    has_more: nextOffset !== null,
  };
}

export function readOutputResource({ scope, siteRoot, uri, maxBytes, outputDir }: OutputResourceReadOptions = {}) {
  const transportScope = resolveTransportScope({ scope, siteRoot, outputDir, maxBytes });
  siteRoot = transportScope.siteRoot;
  maxBytes = transportScope.maxOutputBytes;
  outputDir = transportScope.outputDir;
  const ref = outputRefFromResourceUri(String(uri ?? ''));
  const record = readOutputRecord({ scope: transportScope, ref });
  const page = publicOutputShowRecord(record, {
    outputLimit: DEFAULT_OUTPUT_SHOW_CHAR_LIMIT,
    offset: 0,
  });
  return {
    contents: [
      {
        uri,
        mimeType: 'application/json',
        text: JSON.stringify(page, null, 2),
      },
    ],
  };
}

function outputResourceUri(ref: any) {
  return `mcp-output:${encodeURIComponent(ref)}`;
}

function outputRefFromResourceUri(uri: any) {
  if (!uri.startsWith('mcp-output:')) throw new Error(`output_resource_uri_invalid: ${uri}`);
  return decodeURIComponent(uri.slice('mcp-output:'.length));
}

export function listOutputTools() {
  return [
    {
      name: 'mcp_output_show',
      description: 'Read a materialized MCP output ref with offset/limit paging.',
      inputSchema: {
        type: 'object',
        properties: {
          ref: { type: 'string', description: 'Materialized output ref, e.g. mcp_output:<id>. Alias: output_ref.' },
          output_ref: { type: 'string', description: 'Alias for ref.' },
          offset: { type: 'integer', default: 0, description: 'Character offset into the materialized JSON output.' },
          limit: { type: 'integer', default: DEFAULT_OUTPUT_SHOW_CHAR_LIMIT, minimum: 1, maximum: MAX_OUTPUT_SHOW_CHAR_LIMIT, description: 'Maximum output characters to return; the transport hard-caps this value.' },
          timeout_ms: { type: 'integer', default: DEFAULT_OUTPUT_READ_TIMEOUT_MS, minimum: 1, maximum: MAX_OUTPUT_READ_TIMEOUT_MS, description: 'Strict upper bound for reading and validating the materialized output file.' },
        },
        required: [],
        additionalProperties: false,
      },
    },
  ];
}

function outputCreate({ scope, siteRoot, toolName, value, fullText, inlineLimit, createdBy, maxBytes, outputDir }: OutputCreateOptions) {
  const transportScope = resolveTransportScope({ scope, siteRoot, outputDir, maxBytes });
  siteRoot = transportScope.siteRoot;
  maxBytes = transportScope.maxOutputBytes;
  outputDir = transportScope.outputDir;
  const normalizedToolName = toolName.trim().length > 0 ? z.string().trim().max(200).parse(toolName) : null;
  const normalizedCreatedBy = createdBy === null ? null : z.string().trim().max(200).parse(createdBy);
  const outputId = randomOutputId();
  const createdAt = new Date().toISOString();
  const ref = buildOutputRef(outputId);
  const record = {
    schema: 'narada.mcp_output_ref.v1',
    ref,
    output_id: outputId,
    tool_name: normalizedToolName,
    created_at: createdAt,
    created_by: normalizedCreatedBy,
    content_type: 'application/json',
    inline_char_limit: inlineLimit,
    full_output_char_length: fullText.length,
    truncated: true,
    sha256: sha256(stableJson(value)),
    max_bytes: maxBytes,
    full_output: value,
  };
  const serialized = `${JSON.stringify(record)}\n`;
  const byteSize = Buffer.byteLength(serialized, 'utf8');
  if (byteSize > maxBytes) throw new Error(`mcp_output_too_large: ${byteSize} > ${maxBytes}`);
  const path = outputPath({ siteRoot, outputDir, outputId });
  mkdirSync(dirname(path), { recursive: true });
  try {
    writeImmutableFile(path, serialized);
  } catch (error) {
    if (isNodeErrorCode(error, 'EEXIST')) throw new Error(`mcp_output_ref_collision: ${ref}`);
    throw error;
  }
  return { ...publicOutputRecord(record), byte_size: byteSize };
}

function readOutputRecord({ scope, siteRoot, ref, maxBytes, outputDir }: OutputReadOptions) {
  const transportScope = resolveTransportScope({ scope, siteRoot, outputDir, maxBytes });
  siteRoot = transportScope.siteRoot;
  maxBytes = transportScope.maxOutputBytes;
  outputDir = transportScope.outputDir;
  const parsed = parseOutputRef(ref);
  const path = outputPath({ siteRoot, outputDir, outputId: parsed.outputId });
  let stat;
  try {
    stat = statSync(path);
  } catch {
    throw new Error(`output_ref_not_found: ${ref}`);
  }
  if (!stat.isFile()) throw new Error(`output_ref_not_file: ${ref}`);
  if (stat.size > maxBytes) throw new Error(`output_ref_too_large: ${stat.size} > ${maxBytes}`);
  let record;
  try {
    record = JSON.parse(readFileSync(path, 'utf8'));
  } catch (error) {
    throw new Error(`output_ref_invalid_json: ${error instanceof Error ? error.message : String(error)}`);
  }
  if (!record || typeof record !== 'object' || Array.isArray(record)) throw new Error(`output_ref_record_must_be_object: ${ref}`);
  if (record.schema !== 'narada.mcp_output_ref.v1') throw new Error(`output_ref_schema_unsupported: ${record.schema}`);
  if (record.ref !== ref || record.output_id !== parsed.outputId) throw new Error(`output_ref_metadata_mismatch: ${ref}`);
  const fullText = presentationJson(record.full_output);
  if (record.full_output_char_length !== fullText.length) throw new Error(`output_ref_length_mismatch: ${ref}`);
  if (record.sha256 !== sha256(stableJson(record.full_output))) throw new Error(`output_ref_sha256_mismatch: ${ref}`);
  return { ...record, byte_size: stat.size, output_path: normalizePath(relative(resolveSiteRoot(siteRoot), path)) };
}

function publicOutputRecord(record: any) {
  return {
    schema: 'narada.mcp_output_locator.v1',
    status: 'ok',
    ref: record.ref,
    tool_name: record.tool_name ?? null,
    full_output_char_length: record.full_output_char_length ?? null,
    byte_size: record.byte_size ?? null,
    truncated: record.truncated === true,
    path: record.output_path ?? normalizePath(`${DEFAULT_OUTPUT_DIR}/${DEFAULT_WORKSPACE_DIR}/${record.output_id}.json`),
  };
}

function publicOutputShowRecord(record: any, { outputLimit = DEFAULT_OUTPUT_SHOW_CHAR_LIMIT, offset = 0 } : any= {}) {
  const outputText = presentationJson(record.full_output);
  const page = takeUtf8Page(outputText, offset, outputLimit, MAX_OUTPUT_PAGE_BYTES);
  const chunk = page.chunk;
  const outputTruncated = page.end < outputText.length;
  return {
    schema: 'narada.mcp_output_page.v1',
    status: 'ok',
    ref: record.ref,
    tool_name: record.tool_name ?? null,
    full_output_char_length: record.full_output_char_length ?? outputText.length,
    byte_size: record.byte_size ?? null,
    original_truncated: record.truncated === true,
    path: record.output_path ?? normalizePath(`${DEFAULT_OUTPUT_DIR}/${DEFAULT_WORKSPACE_DIR}/${record.output_id}.json`),
    offset,
    limit: outputLimit,
    next_offset: outputTruncated ? page.end : null,
    output_limit: outputLimit,
    output_truncated: outputTruncated,
    output_text: chunk,
  };
}

function isOutputLocator(value: any) {
  return Boolean(
    value
      && typeof value === 'object'
      && !Array.isArray(value)
      && value.schema === 'narada.mcp_output_locator.v1'
      && typeof value.ref === 'string'
  );
}

function isOutputShowResult(value: any) {
  return Boolean(
    value
      && typeof value === 'object'
      && !Array.isArray(value)
      && value.schema === 'narada.mcp_output_page.v1'
      && typeof value.ref === 'string'
      && typeof value.output_text === 'string'
  );
}

function normalizeOutputShowLimit(value: any) {
  if (value === undefined || value === null) return DEFAULT_OUTPUT_SHOW_CHAR_LIMIT;
  const parsed = z.number().finite().int().safeParse(value);
  if (!parsed.success || parsed.data < 1) {
    throw new Error('output_limit_must_be_positive_integer');
  }
  if (parsed.data > MAX_OUTPUT_SHOW_CHAR_LIMIT) {
    throw new Error(`output_limit_exceeds_transport_maximum: ${parsed.data} > ${MAX_OUTPUT_SHOW_CHAR_LIMIT}`);
  }
  return parsed.data;
}

function normalizeOutputShowOffset(value: any) {
  if (value === undefined || value === null) return 0;
  const parsed = z.number().finite().int().safeParse(value);
  if (!parsed.success || parsed.data < 0) {
    throw new Error('offset_must_be_non_negative_integer');
  }
  return parsed.data;
}

function normalizeResourcePage({ cursor, offset, limit }: { cursor?: unknown; offset?: unknown; limit?: unknown } = {}) {
  if (cursor !== undefined && cursor !== null && cursor !== '') {
    if (offset !== undefined && offset !== null && offset !== '') throw new Error('resource_page_cursor_and_offset_are_mutually_exclusive');
    const cursorValue = z.string().regex(/^\d+$/).parse(cursor);
    offset = Number(cursorValue);
  }
  return z.object({
    offset: z.number().finite().int().min(0).default(0),
    limit: z.number().finite().int().min(1).max(MAX_RESOURCE_PAGE_LIMIT).default(DEFAULT_RESOURCE_PAGE_LIMIT),
  }).parse({ offset, limit });
}

function parseOutputRef(ref: any) {
  const value = typeof ref === 'string' ? ref.trim() : '';
  if (REF_PATTERN.test(value)) {
    throw new Error('wrong_ref_family: got=mcp_payload expected=mcp_output reader_tool=mcp_payload_show remediation=use mcp_payload_show');
  }
  const match = value.match(OUTPUT_REF_PATTERN);
  if (!match) throw new Error(`output_ref_invalid: ${value}`);
  return { ref: value, outputId: match[1], output_id: match[1] };
}

function requireOutputRef(args: any, message: any, field : any= 'ref') {
  const record = asRecord(args);
  if (field === 'ref' && typeof record.ref === 'string' && typeof record.output_ref === 'string' && record.ref.trim() !== record.output_ref.trim()) {
    throw new Error('output_show_ref_alias_conflict: provide either ref or output_ref, or provide matching values');
  }
  const value = record[field] ?? (field === 'ref' ? record.output_ref : undefined);
  if (typeof value !== 'string' || value.trim().length === 0) throw new Error(message);
  return value.trim();
}

function outputPath({ siteRoot, outputDir, outputId }: any) {
  return resolve(resolveManagedDirectory(siteRoot, outputDir, 'output_directory'), DEFAULT_WORKSPACE_DIR, `${outputId}.json`);
}

function resolveSiteRoot(siteRoot: any) {
  return resolve(typeof siteRoot === 'string' && siteRoot.trim().length > 0 ? siteRoot : process.cwd());
}

function resolveManagedDirectory(siteRoot: any, directory: any, label: any) {
  const root = resolveSiteRoot(siteRoot);
  const candidate = resolve(root, String(directory ?? ''));
  if (!isPathInside(candidate, root)) throw new Error(`${label}_outside_site_root: ${normalizePath(String(directory ?? ''))}`);
  return candidate;
}

function takeUtf8Page(text: any, offset: any, maxChars: any, maxBytes: any) {
  if (offset > text.length) return { chunk: '', end: text.length };
  if (offset < text.length && isLowSurrogate(text.charCodeAt(offset))) {
    throw new Error('output_offset_splits_unicode_scalar');
  }
  let end = safeCodePointEnd(text, Math.min(text.length, offset + maxChars));
  while (end > offset && Buffer.byteLength(text.slice(offset, end), 'utf8') > maxBytes) {
    end = safeCodePointEnd(text, end - 1);
  }
  return { chunk: text.slice(offset, end), end };
}

function safeCodePointEnd(text: any, end: any) {
  if (end > 0 && isHighSurrogate(text.charCodeAt(end - 1))) return end - 1;
  return end;
}

function isHighSurrogate(value: any) {
  return value >= 0xd800 && value <= 0xdbff;
}

function isLowSurrogate(value: any) {
  return value >= 0xdc00 && value <= 0xdfff;
}

function isNodeErrorCode(error: any, code: any) {
  return Boolean(error && typeof error === 'object' && error.code === code);
}

function buildOutputRef(outputId: any) {
  return `mcp_output:${outputId}`;
}

function randomOutputId() {
  return `o_${randomUUID().replace(/-/g, '').slice(0, 24)}`;
}

function outputStatus(value: any, isError: any) {
  if (value && typeof value === 'object' && !Array.isArray(value) && typeof value.status === 'string' && value.status.length <= 32) {
    return value.status;
  }
  return isError ? 'error' : 'ok';
}

export function listPayloadTools() {
  return [
    toolDefinition({
      name: 'mcp_payload_create',
      description: 'Create immutable transient MCP payload revision v1 under .ai/tmp/mcp-payloads/workspace.',
      behavior: { readOnly: false, destructive: false, idempotent: false },
      inputSchema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          payload_id: { type: 'string', description: 'Optional stable id segment. Defaults to a generated id.' },
          payload: { type: 'object', additionalProperties: true, description: 'Normal route for clients that can transmit nested objects: the non-empty domain object to store, e.g. {"payload":{"summary":"..."}}. Do not send with payload_json unless payload is exactly {} as a client placeholder.' },
          payload_json: { type: 'string', description: 'String route for clients that cannot transmit free-form nested objects, especially OpenCode: JSON object text, e.g. "{\\"summary\\":\\"...\\"}". Authoritative when present; may be accompanied only by payload:{} as a placeholder.' },
          allow_empty: { type: 'boolean', description: 'Set true only when intentionally creating an empty payload object.' },
          created_by: { type: 'string', description: 'Optional agent/principal for audit metadata.' },
        },
      },
    }),
    toolDefinition({
      name: 'mcp_payload_show',
      description: 'Show an immutable transient MCP payload revision by ref.',
      behavior: { readOnly: true, destructive: false, idempotent: true },
      inputSchema: {
        type: 'object',
        additionalProperties: false,
        properties: { ref: { type: 'string', description: 'Payload ref, e.g. mcp_payload:<id>@v1.' } },
        required: ['ref'],
      },
    }),
    toolDefinition({
      name: 'mcp_payload_derive',
      description: 'Derive a new immutable payload revision by applying an optional object overlay and explicit JSON Pointer deletions.',
      behavior: { readOnly: false, destructive: false, idempotent: false },
      inputSchema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          source_ref: { type: 'string', description: 'Source payload ref, e.g. mcp_payload:<id>@v1.' },
          overlay: { type: 'object', additionalProperties: true, description: 'Optional recursive object overlay. Null remains a value and never means deletion. Do not send with overlay_json unless overlay is exactly {} as a client placeholder.' },
          overlay_json: { type: 'string', description: 'Optional JSON-string overlay route for clients that cannot transmit free-form nested objects. Authoritative when present; may be accompanied only by overlay:{} as a placeholder.' },
          delete_paths: { type: 'array', items: { type: 'string' }, description: 'Optional RFC 6901-encoded object-field paths to delete after applying the overlay, e.g. ["/preferred_role", "/constraints/model"]. Array traversal is not supported. At least one overlay route or delete path is required.' },
          created_by: { type: 'string', description: 'Optional agent/principal for audit metadata.' },
        },
        required: ['source_ref'],
      },
    }),
    toolDefinition({
      name: 'mcp_payload_validate',
      description: 'Validate that a payload ref exists, is well-formed, and is within size limits.',
      behavior: { readOnly: true, destructive: false, idempotent: true },
      inputSchema: {
        type: 'object',
        additionalProperties: false,
        properties: { ref: { type: 'string', description: 'Payload ref, e.g. mcp_payload:<id>@v1.' } },
        required: ['ref'],
      },
    }),
  ];
}

function toolDefinition(definition: any) {
  const { behavior, ...tool } = definition;
  return {
    ...tool,
    annotations: {
      title: String(tool.name),
      readOnlyHint: behavior.readOnly,
      destructiveHint: behavior.destructive,
      idempotentHint: behavior.idempotent,
      openWorldHint: false,
    },
    outputSchema: genericToolOutputSchema(),
  };
}

function genericToolOutputSchema() {
  return { type: 'object', additionalProperties: true };
}

function asRecord(value: any) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function parseArgumentRecord(value: any): Record<string, unknown> {
  const parsed = z.record(z.string(), z.unknown()).safeParse(value ?? {});
  if (!parsed.success) throw new Error('tool_arguments_must_be_object');
  return parsed.data;
}

function asPayloadObject(value: any, message: any) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(message);
  return value;
}

export function payloadObjectFromArgs(input: any, { objectField, jsonField, objectMessage, jsonMessage, ambiguityMessage }: any) {
  const rawObject = input[objectField];
  const rawJson = stringOrNull(input[jsonField]);
  if (rawJson) {
    if (rawObject !== undefined && rawObject !== null) {
      const objectPayload = asPayloadObject(rawObject, objectMessage);
      if (Object.keys(objectPayload).length > 0) throw new Error(ambiguityMessage);
    }
    return parsePayloadJsonObject(rawJson, jsonMessage);
  }
  return asPayloadObject(rawObject, objectMessage);
}

function parsePayloadJsonObject(value: any, message: any) {
  let parsed;
  try {
    parsed = JSON.parse(value);
  } catch (error) {
    throw new Error(`${message}: ${error instanceof Error ? error.message : String(error)}`);
  }
  return asPayloadObject(parsed, message);
}

function readPayloadRevision({ scope, siteRoot, ref, maxBytes, payloadDir }: PayloadReadOptions) {
  const transportScope = resolveTransportScope({ scope, siteRoot, payloadDir, maxBytes });
  siteRoot = transportScope.siteRoot;
  maxBytes = transportScope.maxPayloadBytes;
  payloadDir = transportScope.payloadDir;
  const parsed = parsePayloadRef(ref);
  const path = revisionPath({ siteRoot, payloadDir, payloadId: parsed.payloadId, revision: parsed.revision });
  let stat;
  try {
    stat = statSync(path);
  } catch {
    throw new Error(`payload_ref_not_found: ${ref}`);
  }
  if (!stat.isFile()) throw new Error(`payload_ref_not_file: ${ref}`);
  if (stat.size > maxBytes) throw new Error(`payload_ref_too_large: ${stat.size} > ${maxBytes}`);
  let record;
  try {
    record = JSON.parse(readFileSync(path, 'utf8'));
  } catch (error) {
    throw new Error(`payload_ref_invalid_json: ${error instanceof Error ? error.message : String(error)}`);
  }
  validateRevisionRecord(record, parsed, stat.size, maxBytes);
  return {
    ...parsed,
    ref,
    payload: record.payload,
    record,
    byte_size: stat.size,
    sha256: record.sha256,
  };
}

function writeRevision({ scope, siteRoot, payloadDir, record }: RevisionWriteOptions) {
  const transportScope = resolveTransportScope({ scope, siteRoot, payloadDir });
  siteRoot = transportScope.siteRoot;
  payloadDir = transportScope.payloadDir;
  const path = revisionPath({ siteRoot, payloadDir, payloadId: record.payload_id, revision: record.revision });
  mkdirSync(dirname(path), { recursive: true });
  const serialized = `${stableJson(record)}\n`;
  try {
    writeImmutableFile(path, serialized);
    return { status: 'created', record };
  } catch (error) {
    if (!isNodeErrorCode(error, 'EEXIST')) throw error;
    if (!isNodeErrorCode(error, 'EEXIST')) throw error;
    let existing;
    try {
      existing = JSON.parse(readFileSync(path, 'utf8'));
    } catch {
      throw new Error(`payload_revision_conflict: existing revision is unreadable: ${record.ref}`);
    }
    if (existing?.ref === record.ref && existing?.sha256 === record.sha256 && existing?.byte_size === record.byte_size) {
      return { status: 'existing', record: existing };
    }
    throw new Error(`payload_revision_conflict: immutable revision already contains different content: ${record.ref}`);
  }
}

function buildRevisionRecord({ payloadId, revision, payload, createdAt, createdBy, source, maxBytes }: any) {
  const payloadJson = stableJson(payload);
  const byteSize = Buffer.byteLength(payloadJson, 'utf8');
  if (byteSize > maxBytes) throw new Error(`payload_too_large: ${byteSize} > ${maxBytes}`);
  return {
    schema: 'narada.mcp_payload.revision.v1',
    ref: buildPayloadRef(payloadId, revision),
    payload_id: payloadId,
    revision,
    created_at: createdAt,
    created_by: createdBy,
    source,
    sha256: sha256(payloadJson),
    byte_size: byteSize,
    max_bytes: maxBytes,
    transient_not_authority: true,
    immutable_revision: true,
    payload,
  };
}

function validateRevisionRecord(record: any, parsed: any, statSize: any, maxBytes: any) {
  if (!record || typeof record !== 'object' || Array.isArray(record)) throw new Error(`payload_ref_record_must_be_object: ${parsed.ref}`);
  if (record.schema !== 'narada.mcp_payload.revision.v1') throw new Error(`payload_ref_schema_unsupported: ${record.schema}`);
  if (record.ref !== parsed.ref || record.payload_id !== parsed.payloadId || record.revision !== parsed.revision) {
    throw new Error(`payload_ref_metadata_mismatch: ${parsed.ref}`);
  }
  asPayloadObject(record.payload, 'payload_ref_payload_must_be_object');
  if (statSize > maxBytes || record.byte_size > maxBytes) throw new Error(`payload_ref_too_large: ${statSize} > ${maxBytes}`);
  const payloadJson = stableJson(record.payload);
  const payloadByteSize = Buffer.byteLength(payloadJson, 'utf8');
  if (record.byte_size !== payloadByteSize) throw new Error(`payload_ref_byte_size_mismatch: ${parsed.ref}`);
  if (sha256(payloadJson) !== record.sha256) throw new Error(`payload_ref_sha256_mismatch: ${parsed.ref}`);
}

function parsePayloadRef(ref: any) {
  const value = typeof ref === 'string' ? ref.trim() : '';
  if (OUTPUT_REF_PATTERN.test(value)) {
    throw new Error('wrong_ref_family: got=mcp_output expected=mcp_payload remediation=re-call_original_producing_tool_with_paging_args');
  }
  const match = value.match(REF_PATTERN);
  if (!match) throw new Error(`payload_ref_invalid: ${value}`);
  return { ref: value, payloadId: match[1], payload_id: match[1], revision: Number(match[2]) };
}

function requireRef(args: any, message: any, field : any= 'ref') {
  const value = asRecord(args)[field];
  if (typeof value !== 'string' || value.trim().length === 0) throw new Error(message);
  return value.trim();
}

function revisionPath({ siteRoot, payloadDir, payloadId, revision }: any) {
  return resolve(resolveManagedDirectory(siteRoot, payloadDir, 'payload_directory'), DEFAULT_WORKSPACE_DIR, payloadId, `v${revision}.json`);
}

function buildPayloadRef(payloadId: any, revision: any) {
  return `mcp_payload:${payloadId}@v${revision}`;
}

function validatePayloadId(value: any) {
  const match = value.trim().match(/^[A-Za-z0-9][A-Za-z0-9_-]{2,63}$/);
  if (!match) throw new Error(`payload_id_invalid: ${value}`);
  return value.trim();
}

function randomPayloadId() {
  return `p_${randomUUID().replace(/-/g, '').slice(0, 24)}`;
}

function overlayObject(base: any, overlay: any) {
  const output = { ...base };
  for (const [key, value] of Object.entries(overlay)) {
    if (isPlainObject(value) && isPlainObject(output[key])) {
      output[key] = overlayObject(output[key], value);
    } else {
      output[key] = value;
    }
  }
  return output;
}

function payloadDeletePaths(value: any) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value) || value.length === 0 || value.some((path: any) => typeof path !== 'string')) {
    throw new Error('payload_derive_delete_paths_must_be_non_empty_string_array');
  }
  const unique = [...new Set(value)];
  if (unique.length !== value.length) throw new Error('payload_derive_delete_paths_must_be_unique');
  return unique.map((path: any) => {
    if (!path.startsWith('/')) throw new Error(`payload_derive_delete_path_invalid: ${path}`);
    return path;
  });
}

function deleteObjectPaths(payload: any, paths: any) {
  return paths.reduce((current: any, path: any) => deleteObjectPath(current, decodeJsonPointer(path), path), payload);
}

function decodeJsonPointer(path: any) {
  return path.slice(1).split('/').map((segment: any) => {
    if (/~(?:[^01]|$)/.test(segment)) throw new Error(`payload_derive_delete_path_invalid_escape: ${path}`);
    return segment.replace(/~1/g, '/').replace(/~0/g, '~');
  });
}

function deleteObjectPath(value: any, segments: any, originalPath: any) {
  if (!isPlainObject(value)) throw new Error(`payload_derive_delete_path_parent_not_object: ${originalPath}`);
  const [head, ...tail] = segments;
  if (!Object.prototype.hasOwnProperty.call(value, head)) throw new Error(`payload_derive_delete_path_not_found: ${originalPath}`);
  const output = { ...value };
  if (tail.length === 0) {
    delete output[head];
    return output;
  }
  output[head] = deleteObjectPath(output[head], tail, originalPath);
  return output;
}


function isPlainObject(value: any) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function publicRevisionResult({ status, record, includePayload = false, ref = record.ref, sourceRef = null }: any) {
  return {
    status,
    ref,
    payload_id: record.payload_id,
    revision: record.revision,
    source_ref: sourceRef ?? record.source?.source_ref ?? null,
    byte_size: record.byte_size,
    sha256: record.sha256,
    created_at: record.created_at,
    created_by: record.created_by,
    transient_not_authority: true,
    immutable_revision: true,
    payload: includePayload ? record.payload : undefined,
  };
}

function stableJson(value: any) {
  return JSON.stringify(sortJson(value)) ?? 'null';
}

function presentationJson(value: any) {
  return JSON.stringify(value, null, 2) ?? 'null';
}

function writeImmutableFile(path: any, serialized: any) {
  const tempPath = `${path}.${randomUUID()}.tmp`;
  let descriptor = null;
  try {
    descriptor = openSync(tempPath, 'wx');
    const buffer = Buffer.from(serialized, 'utf8');
    let offset = 0;
    while (offset < buffer.length) offset += writeSync(descriptor, buffer, offset, buffer.length - offset);
    fsyncSync(descriptor);
    closeSync(descriptor);
    descriptor = null;
    linkSync(tempPath, path);
  } finally {
    if (descriptor !== null) closeSync(descriptor);
    try {
      unlinkSync(tempPath);
    } catch (error) {
      if (!isNodeErrorCode(error, 'ENOENT')) throw error;
    }
  }
}

function sortJson(value: any): any {
  if (Array.isArray(value)) return value.map(sortJson);
  if (!isPlainObject(value)) return value;
  const keys = Object.keys(value).sort((left: any, right: any) => left.localeCompare(right));
  return Object.fromEntries(keys.map((key: any) => [key, sortJson(value[key])]));
}

function sha256(value: any) {
  return createHash('sha256').update(value, 'utf8').digest('hex');
}

function stringOrNull(value: any) {
  return typeof value === 'string' && value.trim().length > 0 ? value.trim() : null;
}

function isPathInside(candidate: any, root: any) {
  const rel = relative(root, candidate);
  return rel === '' || (!rel.startsWith('..') && !/^[A-Za-z]:/.test(rel));
}

function normalizePath(value: any): string {
  return value.replace(/\\/g, '/');
}