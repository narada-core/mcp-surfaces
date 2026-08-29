#!/usr/bin/env node
import { buildGuidanceResult } from './guidance.js';
import { guidanceToolDefinition } from './guidance.js';
import { appendFileSync, closeSync, existsSync, mkdirSync, openSync, readFileSync, readdirSync, readSync, realpathSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { mkdir as mkdirAsync, open as openFileAsync, readFile as readFileAsync, realpath as realpathAsync, stat as statAsync, writeFile as writeFileAsync } from 'node:fs/promises';
import { createHash, randomUUID } from 'node:crypto';
import { basename, dirname, extname, isAbsolute, join, relative, resolve } from 'node:path';
import { StringDecoder } from 'node:string_decoder';
import { fileURLToPath } from 'node:url';
import { Worker } from 'node:worker_threads';
import {
  attachPayloadSource,
  listOutputResources,
  readOutputResource,
  resolveToolPayloadArgs,
} from '@narada-core/mcp-transport';
import { buildAllowedRoots, rootEntriesToRoots, resolveAllowedPath as resolvePolicyAllowedPath } from './policy.js';
import { applyDeletePatch as applyParsedDeletePatch, applyFilePatch as applyParsedFilePatch, parsePatch as parseToolPatch } from './patch-apply.js';
import { renderToolResultText as renderFilesystemToolResultText } from './result-rendering.js';
import { RIPGREP_FIELD_SEPARATOR, grepMatchObject as buildGrepMatchObject, runRipgrepPage, runRipgrepPageAsync } from './search.js';

const PROTOCOL_VERSION = '2024-11-05';
const INLINE_RESULT_CHAR_LIMIT = 6000;
const READ_RESULT_INLINE_CHAR_LIMIT = 20_000;
const MAX_READ_LINE_LIMIT = 1000;
const READ_BUFFER_BYTES = 64 * 1024;
const DEFAULT_READ_OPERATION_TIMEOUT_MS = 5_000;
const DEFAULT_FILESYSTEM_OPERATION_TIMEOUT_MS = 10_000;
const TRANSIENT_EXECUTABLE_EXTENSIONS = new Set(['.cmd', '.bat', '.ps1', '.psm1', '.js', '.mjs', '.cjs', '.ts']);
const TRANSIENT_EXECUTABLE_PATH = /(^|[\\/])\.ai[\\/](?:tmp|temp)(?:[\\/]|$)/i;
const ROOTS_LIST_REQUEST_PREFIX = 'local_filesystem_roots_';
const DEFAULT_GLOB_IGNORE_PATTERNS = [
  '**/.git/**',
  '**/node_modules/**',
  '**/dist/**',
  '**/build/**',
  '**/coverage/**',
  '**/.next/**',
  '**/.turbo/**',
  '**/.cache/**',
  '**/.venv/**',
  '**/__pycache__/**',
  '**/target/**',
];
const DEFAULT_REPOSITORY_INVENTORY_IGNORE_PATTERNS = [
  '**/.ai/runtime/**',
  '**/.ai/tmp/**',
  '**/.ai/output/**',
  '**/.narada/runtime/**',
  '**/.narada/tmp/**',
  '**/.narada/local-filesystem-mcp/patch-outcomes/**',
  '**/.tmp-tests/**',
];
const DEFAULT_FILE_METRICS_PATTERN = '**/*';
const MAX_FILE_METRICS_LIMIT = 100;
const DEFAULT_FILE_METRICS_MAX_BYTES_PER_FILE = 8 * 1024 * 1024;
const MAX_FILE_METRICS_MAX_BYTES_PER_FILE = 64 * 1024 * 1024;
const DEFAULT_FILE_METRICS_MAX_TOTAL_SCAN_BYTES = 256 * 1024 * 1024;
const MAX_FILE_METRICS_MAX_TOTAL_SCAN_BYTES = 512 * 1024 * 1024;
const MAX_FILE_METRICS_SNAPSHOT_FILES = 10_000;
const FILE_METRICS_SNAPSHOT_CACHE_MAX_ENTRIES = 4;
const PATH_ARGUMENT_DESCRIPTION = 'Absolute paths are preferred. A relative path resolves against the first allowed root shown by fs_doctor, never against the caller current directory.';
const DIRECTORY_ARGUMENT_DESCRIPTION = 'Directory under an allowed root. Absolute paths are preferred; relative directories resolve against the first allowed root shown by fs_doctor.';
const fileMetricsSnapshotCache = new Map();
const REPOSITORY_GENERATED_PATH_MARKERS = [
  '/.ai/runtime/',
  '/.ai/tmp/',
  '/.ai/output/',
  '/.narada/runtime/',
  '/.narada/tmp/',
  '/.narada/local-filesystem-mcp/patch-outcomes/',
  '/.tmp-tests/',
];
const DEFAULT_GREP_IGNORE_PATTERNS = [
  ...DEFAULT_GLOB_IGNORE_PATTERNS,
  '**/.ai/runtime/**',
  '**/.ai/tmp/**',
  '**/.ai/output/**',
  '**/.narada/runtime/**',
  '**/.narada/tmp/**',
  '**/.tmp-tests/**',
];
let activeToolName: string | null = null;

class McpToolError extends Error {
  codeName: string;
  details: unknown;

  constructor(codeName: string, message: string, details: unknown = {}) {
    super(message);
    this.name = 'McpToolError';
    this.codeName = codeName;
    this.details = details;
  }
}

async function countFileLinesAsync(path: any, checkTimeout: any, abortSignal: any) {
  const handle = await openFileAsync(path, 'r');
  const decoder = new StringDecoder('utf8');
  const buffer = Buffer.allocUnsafe(READ_BUFFER_BYTES);
  let lineCount = 0;
  let pending = '';
  try {
    while (true) {
      checkTimeout('read_file', abortSignal);
      const result = await handle.read(buffer, 0, buffer.length, null);
      checkTimeout('after_read_file', abortSignal);
      if (result.bytesRead === 0) break;
      const chunk = buffer.subarray(0, result.bytesRead);
      if (chunk.includes(0)) return { line_count: null, line_count_status: 'binary' };
      pending += decoder.write(chunk);
      const lines = pending.split(/\r?\n/);
      pending = lines.pop() ?? '';
      lineCount += lines.length;
    }
    pending += decoder.end();
    if (pending.length > 0) lineCount += 1;
    return { line_count: lineCount, line_count_status: 'exact' };
  } finally {
    await handle.close();
  }
}

function delayWithDeadline(ms: any, signal: any, timeoutMs: any, timeoutError: any) {
  return new Promise((resolvePromise: any, rejectPromise: any) => {
    if (signal?.aborted) {
      rejectPromise(Object.assign(new Error('aborted'), { name: 'AbortError' }));
      return;
    }
    let settled = false;
    const cleanup = () => {
      clearTimeout(delayTimer);
      clearTimeout(deadlineTimer);
      signal?.removeEventListener('abort', abortHandler);
    };
    const finish = (fn: any, value: any) => {
      if (settled) return;
      settled = true;
      cleanup();
      fn(value);
    };
    const delayTimer = setTimeout(() => finish(resolvePromise, undefined), ms);
    const deadlineTimer = setTimeout(() => finish(rejectPromise, timeoutError()), timeoutMs);
    const abortHandler = () => finish(rejectPromise, Object.assign(new Error('aborted'), { name: 'AbortError' }));
    signal?.addEventListener('abort', abortHandler, { once: true });
  });
}

async function callReadToolWithRequestDeadline(name: any, args: any, state: any, context: any) {
  const timeoutMs = readOperationTimeoutMs(args);
  const deadline = createReadRequestTimeoutChecker(name, timeoutMs, args, state);
  deadline.check();
  const artificialDelayMs = readHandlerDelayMs(state);
  if (artificialDelayMs > 0) {
    await delayWithDeadline(artificialDelayMs, context.abortSignal, timeoutMs, deadline.timeoutError);
  }
  deadline.check();
  const value = name === 'fs_read_file'
    ? await readFileToolAsync(args, state)
    : await readFileRangeToolAsync(args, state);
  deadline.check();
  return toolResult(value);
}

if (import.meta.url === `file:///${process.argv[1]?.replace(/\\/g, '/')}`) {
  runStdioServer(parseArgs(process.argv.slice(2))).catch((error: any) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  });
}

export async function runStdioServer(options: Record<string, unknown>) {
  const state = createServerState(options);
  const activeRequests = new Map<string, AbortController>();
  const pendingServerRequests = new Map<string, (message: Record<string, unknown>) => void>();
  let nextServerRequestId = 1;
  let buffer = '';
  let sawFramedInput = false;
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) {
    buffer += chunk;
    let requests = [];
    if (buffer.includes('Content-Length:')) {
      sawFramedInput = true;
      const drained = drainJsonRpcFrames(buffer);
      buffer = drained.remaining;
      requests = drained.requests;
    } else {
      const lines = buffer.split(/\r?\n/);
      buffer = lines.pop() ?? '';
      requests = lines.filter((line: any) => line.trim()).map((line: any) => JSON.parse(line));
    }
    for (const request of requests) {
      const record = asRecord(request);
      if (record.method === undefined && record.id !== undefined) {
        const handler = pendingServerRequests.get(String(record.id));
        if (handler) {
          pendingServerRequests.delete(String(record.id));
          handler(record);
        }
        continue;
      }
      if (!record.id && record.method === 'notifications/roots/list_changed' && asRecord(state.clientRoots).supported) {
        requestClientRoots(state, pendingServerRequests, () => `${ROOTS_LIST_REQUEST_PREFIX}${nextServerRequestId++}`, { framed: sawFramedInput });
        continue;
      }
      if (record.method === 'initialize') {
        sendProgress(record, 0, 'started', { framed: sawFramedInput });
        const response = handleRequest(record, state);
        sendProgress(record, 1, 'completed', { framed: sawFramedInput });
        if (response) writeJsonRpcResponse(response, { framed: sawFramedInput });
        if (clientSupportsRoots(asRecord(record.params))) {
          asRecord(state.clientRoots).supported = true;
          requestClientRoots(state, pendingServerRequests, () => `${ROOTS_LIST_REQUEST_PREFIX}${nextServerRequestId++}`, { framed: sawFramedInput });
        }
        continue;
      }
      processStdioRequest(record, state, activeRequests, { framed: sawFramedInput });
    }
  }
}

export function createServerState(options: Record<string, unknown>): Record<string, unknown> {
  const mode = typeof options.mode === 'string' ? options.mode : null;
  if (!['read', 'write'].includes(mode ?? '')) throw diagnosticError('mode_must_be_read_or_write', 'mode_must_be_read_or_write', { mode });
  const siteRoot = resolve(String(options.siteRoot ?? options.outputRoot ?? process.cwd()));
  const stateEnv = { ...process.env };
  loadSiteSecrets(siteRoot, stateEnv);
  const explicitRoots = stringList(options.allowedRoots);
  const anchoredRoots = stringList(options.anchoredAllowedRoots);
  const siteExtraRoots = loadSiteExtraAllowedRoots(siteRoot);
  let allowedRootEntries;
  try {
    allowedRootEntries = buildAllowedRoots({
      codexConfigPath: stringOrNull(options.rootsFromCodexConfig),
      anchoredRoots,
      anchors: options.anchors && typeof options.anchors === 'object' && !Array.isArray(options.anchors) ? asRecord(options.anchors) : undefined,
      explicitRoots: [...siteExtraRoots, ...explicitRoots],
      rootsConfigPath: stringOrNull(options.rootsConfig),
    });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const codeName = message.split(/[:\s]/)[0] || 'allowed_roots_failed';
    throw diagnosticError(codeName, message, {
      roots_from_codex_config: stringOrNull(options.rootsFromCodexConfig),
      roots_config: stringOrNull(options.rootsConfig),
      allowed_roots: [...siteExtraRoots, ...explicitRoots],
      anchored_allowed_roots: anchoredRoots,
    });
  }
  const outputRoot = resolve(stringOrNull(options.outputRoot) ?? process.cwd());
  return {
    mode,
    allowedRoots: rootEntriesToRoots(allowedRootEntries),
    allowedRootEntries,
    outputRoot,
    payloadMaxBytes: Number(options.payloadMaxBytes ?? 5 * 1024 * 1024),
    env: stateEnv,
    auditLogDir: options.auditLogDir ? resolve(String(options.auditLogDir)) : null,
    clientRoots: { supported: false, roots: [], lastUpdatedAt: null },
  };
}

function createReadRequestTimeoutChecker(operation: any, timeoutMs: any, args: any, state: any) {
  const startedAt = Date.now();
  const metadata = readRequestPathMetadata(operation, args, state);
  const details = (elapsedMs: any) => ({
    timeout_kind: 'read_request_timeout',
    operation,
    timeout_ms: timeoutMs,
    elapsed_ms: elapsedMs,
    ...metadata,
    recommended_tool: 'fs_read_file_range',
    recommended_args: readRequestRecommendedArgs(operation, args, timeoutMs),
    remediation: [
      'Retry with fs_read_file_range and a narrower line window.',
      'Use fs_stat or fs_grep_search first when the file may be slow, remote, or virtualized.',
      'Increase timeout_ms only after narrowing the requested line window.',
    ],
  });
  const timeoutError = () => {
    const elapsedMs = Math.max(Date.now() - startedAt, timeoutMs + 1);
    return diagnosticError(`${operation}_timed_out`, `${operation}_timed_out`, details(elapsedMs));
  };
  return {
    check: () => {
      const elapsedMs = Date.now() - startedAt;
      if (elapsedMs <= timeoutMs) return;
      throw diagnosticError(`${operation}_timed_out`, `${operation}_timed_out`, details(elapsedMs));
    },
    timeoutError,
  };
}

function readRequestPathMetadata(operation: any, args: any, state: any) {
  try {
    const resolved = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation });
    return pathMetadata(resolved.path, resolved.root);
  } catch {
    return { path: stringField(args, 'path') ?? null, root: null, relative_path: null };
  }
}

function readRequestRecommendedArgs(operation: any, args: any, timeoutMs: any) {
  const path = stringField(args, 'path') ?? '<path>';
  const startLine = operation === 'fs_read_file_range'
    ? integerField(args, 'start_line') ?? 1
    : Math.max(1, integerField(args, 'offset') ?? 1);
  const requestedLimit = operation === 'fs_read_file_range'
    ? Math.max(1, (integerField(args, 'end_line') ?? startLine) - startLine + 1)
    : Math.min(1000, Math.max(1, integerField(args, 'limit') ?? 400));
  return {
    path,
    start_line: startLine,
    end_line: Math.max(startLine, startLine + Math.min(requestedLimit, 100) - 1),
    timeout_ms: Math.min(60_000, Math.max(timeoutMs * 2, DEFAULT_READ_OPERATION_TIMEOUT_MS)),
  };
}

export function handleRequest(request: Record<string, unknown>, state: Record<string, unknown>) {
  if (!request?.id && typeof request?.method === 'string' && request.method.startsWith('notifications/')) return null;
  try {
    const result = dispatchMethod(request.method, request.params ?? {}, state);
    return { jsonrpc: '2.0', id: request.id ?? null, result };
  } catch (error) {
    const diagnostic = errorDiagnostic(error);
    return {
      jsonrpc: '2.0',
      id: request?.id ?? null,
      error: {
        code: -32000,
        message: diagnostic.message,
        data: diagnostic,
      },
    };
  }
}

function dispatchMethod(method: any, params: any, state: any) {
  switch (method) {
    case 'initialize':
      return {
        protocolVersion: params.protocolVersion ?? PROTOCOL_VERSION,
        capabilities: { tools: {}, resources: {}, prompts: {}, completions: {}, logging: {} },
        serverInfo: { name: `local-filesystem-${state.mode}`, version: '0.1.0' },
      };
    case 'tools/list':
      return { tools: listTools(state.mode) };
    case 'tools/call':
      return callTool(params, state);
    case 'resources/list':
      return listOutputResources({ siteRoot: state.outputRoot });
    case 'resources/read':
      return readOutputResource({ siteRoot: state.outputRoot, uri: params.uri });
    case 'prompts/list':
      return { prompts: listPrompts(state.mode) };
    case 'prompts/get':
      return promptGet(params, state);
    case 'completion/complete':
      return completeArgument(params, state);
    case 'logging/setLevel':
      return {};
    default:
      throw diagnosticError('unsupported_mcp_method', `unsupported_mcp_method: ${method}`, { method });
  }
}

async function dispatchMethodAsync(method: any, params: any, state: any, context: any) {
  switch (method) {
    case 'tools/call':
      return await callToolAsync(params, state, context);
    default:
      return dispatchMethod(method, params, state);
  }
}

async function processStdioRequest(request: Record<string, unknown>, state: Record<string, unknown>, activeRequests: Map<string, AbortController>, options: { framed: boolean }) {
  if (!request?.id && request.method === 'notifications/cancelled') {
    const requestId = String(asRecord(request.params).requestId ?? '');
    activeRequests.get(requestId)?.abort();
    return;
  }
  if (!request?.id && typeof request?.method === 'string' && request.method.startsWith('notifications/')) return;
  const requestId = String(request.id ?? '');
  const abortController = new AbortController();
  activeRequests.set(requestId, abortController);
  sendProgress(request, 0, 'started', options);
  handleRequestAsync(request, state, { abortSignal: abortController.signal }).then((response: any) => {
    sendProgress(request, 1, abortController.signal.aborted ? 'cancelled' : 'completed', options);
    if (response) writeJsonRpcResponse(response, options);
  }).finally(() => {
    activeRequests.delete(requestId);
  });
}

export async function handleRequestAsync(request: Record<string, unknown>, state: Record<string, unknown>, context: { abortSignal?: AbortSignal } = {}) {
  if (!request?.id && typeof request?.method === 'string' && request.method.startsWith('notifications/')) return null;
  try {
    const result = await dispatchMethodAsync(request.method, request.params ?? {}, state, context);
    return { jsonrpc: '2.0', id: request.id ?? null, result };
  } catch (error) {
    const diagnostic = errorDiagnostic(error);
    return {
      jsonrpc: '2.0',
      id: request?.id ?? null,
      error: {
        code: -32000,
        message: diagnostic.message,
        data: diagnostic,
      },
    };
  }
}

function listPrompts(mode: any) {
  return [{
    name: 'local_filesystem_tool_usage',
    title: 'Local Filesystem Tool Usage',
    description: `Guidance for using local-filesystem-${mode} tools safely.`,
    arguments: [],
  }];
}

function promptGet(params: any, state: any) {
  const name = stringField(params, 'name');
  if (name !== 'local_filesystem_tool_usage') throw diagnosticError('unknown_prompt', `unknown_prompt: ${name}`, { name });
  return {
    description: `Guidance for using local-filesystem-${state.mode} tools safely.`,
    messages: [{
      role: 'user',
      content: { type: 'text', text: `Use local-filesystem-${state.mode} tools only within allowed roots. Prefer read/search tools before mutation and preserve structuredContent as authoritative.` },
    }],
  };
}

function completeArgument(params: any, state: any) {
  const argumentName = String(asRecord(asRecord(params).argument).name ?? '');
  const values = argumentName === 'name'
    ? listTools(state.mode).map((tool: any) => tool.name).filter(Boolean).slice(0, 100)
    : ['path', 'directory'].includes(argumentName) ? clientRootCompletionValues(state) : [];
  return { completion: { values, total: values.length, hasMore: false } };
}

export function listTools(mode: any) {
  const readTools = [
    guidanceToolDefinition(),
    {
      name: 'fs_read_file',
      description: 'Read a text file under an allowed root with line offset and an explicit maximum of 1000 lines. Larger requests are refused with pagination guidance; they are never silently clamped.',
      inputSchema: objectSchema({
        path: { type: 'string', description: PATH_ARGUMENT_DESCRIPTION },
        offset: { type: 'integer', default: 1 },
        limit: { type: 'integer', minimum: 1, maximum: MAX_READ_LINE_LIMIT, default: 400 },
        timeout_ms: { type: 'integer', description: 'Optional read timeout in milliseconds. Defaults to 5000.' },
      }, ['path']),
    },
    {
      name: 'fs_read_file_range',
      description: 'Read a text file line range under an allowed root. Lines are 1-based and inclusive.',
      inputSchema: objectSchema({
        path: { type: 'string', description: PATH_ARGUMENT_DESCRIPTION },
        start_line: { type: 'integer' },
        end_line: { type: 'integer' },
        timeout_ms: { type: 'integer', description: 'Optional read timeout in milliseconds. Defaults to 5000.' },
      }, ['path', 'start_line', 'end_line']),
    },
    {
      name: 'fs_stat',
      description: 'Return file or directory metadata under an allowed root.',
      inputSchema: objectSchema({ path: { type: 'string', description: PATH_ARGUMENT_DESCRIPTION } }, ['path']),
    },
    {
      name: 'fs_glob_search',
      description: 'List files under an allowed root using ripgrep file globbing. Empty matches return ok with count 0.',
      inputSchema: objectSchema({
        pattern: { type: 'string' },
        directory: { type: 'string', default: '.', description: DIRECTORY_ARGUMENT_DESCRIPTION },
        ignore: { type: 'array', items: { type: 'string' }, description: 'Additional glob patterns to exclude. Defaults also exclude generated dependency/build directories.' },
        offset: { type: 'integer', default: 0 },
        limit: { type: 'integer', default: 100 },
        timeout_ms: { type: 'integer', description: 'Optional search timeout in milliseconds.' },
        cache_policy: { type: 'string', enum: ['auto', 'snapshot', 'refresh', 'bypass'], default: 'auto', description: 'auto uses cached complete snapshots when available; snapshot materializes a reusable snapshot; refresh rebuilds and stores a snapshot; bypass does not use cached snapshots.' },
        snapshot_id: { type: 'string', description: 'Optional snapshot_id from a previous complete search response to page consistently.' },
      }, ['pattern']),
    },
    {
      name: 'fs_repository_inventory',
      description: 'Return a bounded candidate-source inventory under an allowed root, excluding generated runtime artifacts by default. Use git-mcp for authoritative tracked and ignored state.',
      inputSchema: objectSchema({
        pattern: { type: 'string', default: '**/*' },
        directory: { type: 'string', default: '.', description: DIRECTORY_ARGUMENT_DESCRIPTION },
        ignore: { type: 'array', items: { type: 'string' }, description: 'Additional glob patterns to exclude.' },
        include_generated: { type: 'boolean', default: false, description: 'Include known generated runtime/artifact paths in the inventory.' },
        offset: { type: 'integer', default: 0 },
        limit: { type: 'integer', default: 100 },
        timeout_ms: { type: 'integer', description: 'Optional search timeout in milliseconds.' },
        cache_policy: { type: 'string', enum: ['auto', 'snapshot', 'refresh', 'bypass'], default: 'auto' },
        snapshot_id: { type: 'string', description: 'Optional snapshot_id from a previous complete inventory response to page consistently.' },
      }),
    },
    {
      name: 'fs_file_metrics',
      description: 'Return bounded metadata-only file metrics under an allowed root. It reports paths, exact byte counts, bounded line counts for text files, file type, scope classification, aggregate page totals, and the explicit include/ignore boundary without returning file contents.',
      inputSchema: objectSchema({
        pattern: { type: 'string', default: DEFAULT_FILE_METRICS_PATTERN, description: 'Include glob pattern. Defaults to all files under directory.' },
        directory: { type: 'string', default: '.', description: DIRECTORY_ARGUMENT_DESCRIPTION },
        root: { type: 'string', description: `Alias for directory; ${DIRECTORY_ARGUMENT_DESCRIPTION}` },
        ignore: { type: 'array', items: { type: 'string' }, description: 'Additional ignore glob patterns.' },
        exclude: { type: 'array', items: { type: 'string' }, description: 'Alias for additional ignore glob patterns.' },
        offset: { type: 'integer', default: 0 },
        limit: { type: 'integer', default: MAX_FILE_METRICS_LIMIT, description: `Maximum metric rows to return; capped at ${MAX_FILE_METRICS_LIMIT}.` },
        max_bytes_per_file: { type: 'integer', default: DEFAULT_FILE_METRICS_MAX_BYTES_PER_FILE, description: 'Maximum bytes scanned for one text file; larger files return byte metadata with line_count_status=too_large.' },
        max_total_scan_bytes: { type: 'integer', default: DEFAULT_FILE_METRICS_MAX_TOTAL_SCAN_BYTES, description: 'Maximum cumulative bytes scanned for exact line counts across one request; files beyond the budget return line_count_status=scan_budget_exceeded.' },
        timeout_ms: { type: 'integer', description: 'Optional operation timeout in milliseconds. Defaults to 10000.' },
        cache_policy: { type: 'string', enum: ['auto', 'snapshot', 'refresh', 'bypass'], default: 'auto' },
        snapshot_id: { type: 'string', description: 'Optional snapshot_id from a previous metrics request to page consistently.' },
      }),
    },
    {
      name: 'fs_grep_search',
      description: 'Search file contents under an allowed root using ripgrep. directory is canonical scope; path remains a compatibility alias. Use glob to constrain included files and ignore/exclude to constrain omitted files. Use output_mode content for line-numbered matches; empty matches return ok with count 0.',
      inputSchema: objectSchema({
        pattern: { type: 'string', description: 'Regular-expression search pattern.' },
        directory: { type: 'string', description: DIRECTORY_ARGUMENT_DESCRIPTION },
        path: { type: 'string', description: `Compatibility alias for directory; ${DIRECTORY_ARGUMENT_DESCRIPTION}` },
        glob: { type: 'string', description: 'Optional ripgrep include glob applied within directory/path.' },
        output_mode: { type: 'string', enum: ['files_with_matches', 'count_matches', 'content'], default: 'files_with_matches' },
        ignore: { type: 'array', items: { type: 'string' }, description: 'Additional glob patterns to exclude. Defaults also exclude generated dependency/build/runtime directories.' },
        exclude: { type: 'array', items: { type: 'string' }, description: 'Alias for additional glob patterns to exclude.' },
        offset: { type: 'integer', default: 0 },
        limit: { type: 'integer', default: 80 },
        timeout_ms: { type: 'integer', description: 'Optional search timeout in milliseconds.' },
        cache_policy: { type: 'string', enum: ['auto', 'snapshot', 'refresh', 'bypass'], default: 'auto', description: 'auto uses cached complete snapshots when available; snapshot materializes a reusable snapshot; refresh rebuilds and stores a snapshot; bypass does not use cached snapshots.' },
        snapshot_id: { type: 'string', description: 'Optional snapshot_id from a previous complete search response to page consistently.' },
      }, ['pattern']),
    },
    {
      name: 'fs_doctor',
      description: 'Inspect local-filesystem MCP policy posture: mode, allowed roots with provenance, output root, audit log dir, client roots, and effective permissions.',
      inputSchema: objectSchema({}),
    },
    {
      name: 'fs_patch_outcome_show',
      description: 'Read and durably reconcile the outcome for an fs_apply_patch operation_id after a timeout or transport interruption.',
      inputSchema: objectSchema({ operation_id: { type: 'string' } }, ['operation_id']),
    },
  ];
  const writeTools = [
    {
      name: 'fs_write_file',
      description: 'Write a text file under an allowed root and append an audit record. Refuses executable scripts under .ai/tmp or .ai/temp.',
      inputSchema: objectSchema({
        payload_ref: { type: 'string', description: 'Optional MCP payload ref carrying the complete fs_write_file argument object, including large content.' },
        payload_path: { type: 'string', description: 'Optional JSON payload path under the site MCP payload staging directory.' },
        path: { type: 'string' },
        content: { type: 'string' },
        overwrite: { type: 'boolean', default: true },
        create_only: { type: 'boolean', default: false },
        create_parent_directories: { type: 'boolean', default: true, description: 'Create missing parent directories before writing.' },
        timeout_ms: { type: 'integer', description: 'Optional operation timeout in milliseconds. Defaults to 10000.' },
        expected_sha256: { type: 'string', description: 'Optional expected current file sha256 before overwriting.' },
      }),
    },
    {
      name: 'fs_str_replace_file',
      description: 'Replace exactly one string occurrence in a text file under an allowed root and append an audit record. Refuses executable scripts under .ai/tmp or .ai/temp.',
      inputSchema: objectSchema({
        path: { type: 'string' },
        old: { type: 'string' },
        new: { type: 'string' },
        expected_sha256: { type: 'string', description: 'Optional expected current file sha256 before editing.' },
      }, ['path', 'old', 'new']),
    },
    {
      name: 'fs_replace_range',
      description: 'Replace an inclusive 1-based line range in a text file under an allowed root and append an audit record. Refuses executable scripts under .ai/tmp or .ai/temp.',
      inputSchema: objectSchema({
        path: { type: 'string' },
        start_line: { type: 'integer' },
        end_line: { type: 'integer' },
        replacement: { type: 'string' },
        expected_sha256: { type: 'string', description: 'Optional expected current file sha256 before editing.' },
      }, ['path', 'start_line', 'end_line', 'replacement']),
    },
    {
      name: 'fs_apply_patch',
      description: 'Apply a unified diff or Codex-style apply_patch patch to files under allowed roots. Refuses executable scripts under .ai/tmp or .ai/temp. Supply operation_id to recover a durable outcome after transport loss via fs_patch_outcome_show.',
      inputSchema: objectSchema({
        patch: { type: 'string' },
        operation_id: { type: 'string', description: 'Caller-chosen idempotency/recovery identifier; use fs_patch_outcome_show with this value after a transport problem.' },
        dry_run: { type: 'boolean', default: false },
        timeout_ms: { type: 'integer', description: 'Optional operation timeout in milliseconds. Defaults to 10000.' },
        expected_sha256: { type: 'object', additionalProperties: { type: 'string' }, description: 'Optional map from patch paths to expected current file sha256 values.' },
      }, ['patch']),
    },
    {
      name: 'fs_move_path',
      description: 'Move or rename a file or directory under allowed roots and append an audit record. Refuses overwrite unless overwrite is true.',
      inputSchema: objectSchema({
        from: { type: 'string' },
        to: { type: 'string' },
        overwrite: { type: 'boolean', default: false },
        expected_from_mtime: { type: 'string', description: 'Optional expected source mtime ISO string.' },
        expected_from_size: { type: 'integer', description: 'Optional expected source byte size.' },
        expected_from_sha256: { type: 'string', description: 'Optional expected source sha256 for file sources.' },
        expected_from_tree_sha256: { type: 'string', description: 'Optional expected source tree sha256 for directory sources.' },
        expected_from_entry_count: { type: 'integer', description: 'Optional expected source direct entry count for directory sources.' },
        expected_to_mtime: { type: 'string', description: 'Optional expected destination mtime ISO string when overwriting.' },
        expected_to_size: { type: 'integer', description: 'Optional expected destination byte size when overwriting.' },
        expected_to_tree_sha256: { type: 'string', description: 'Optional expected destination tree sha256 for directory destinations.' },
        expected_to_entry_count: { type: 'integer', description: 'Optional expected destination direct entry count for directory destinations.' },
        expected_from: { type: 'object', additionalProperties: false, properties: expectedMetadataSchemaProperties(), description: 'Structured source metadata guard.' },
        expected_to: { type: 'object', additionalProperties: false, properties: expectedMetadataSchemaProperties(), description: 'Structured destination metadata guard.' },
      }, ['from', 'to']),
    },
    {
      name: 'fs_create_directory',
      description: 'Create a directory under an allowed root and append an audit record. Parent creation requires recursive true.',
      inputSchema: objectSchema({
        path: { type: 'string' },
        recursive: { type: 'boolean', default: false },
      }, ['path']),
    },
    {
      name: 'fs_rename_directory',
      description: 'Rename or move a directory under allowed roots and append an audit record. Refuses existing destinations.',
      inputSchema: objectSchema({
        from: { type: 'string' },
        to: { type: 'string' },
        expected_from_mtime: { type: 'string', description: 'Optional expected source mtime ISO string.' },
        expected_from_size: { type: 'integer', description: 'Optional expected source byte size.' },
        expected_from_tree_sha256: { type: 'string', description: 'Optional expected source tree sha256.' },
        expected_from_entry_count: { type: 'integer', description: 'Optional expected source direct entry count.' },
        expected_to_mtime: { type: 'string', description: 'Optional expected destination mtime ISO string if it exists.' },
        expected_to_size: { type: 'integer', description: 'Optional expected destination byte size if it exists.' },
        expected_to_tree_sha256: { type: 'string', description: 'Optional expected destination tree sha256 if it exists.' },
        expected_to_entry_count: { type: 'integer', description: 'Optional expected destination direct entry count if it exists.' },
        expected_from: { type: 'object', additionalProperties: false, properties: expectedMetadataSchemaProperties(), description: 'Structured source metadata guard.' },
        expected_to: { type: 'object', additionalProperties: false, properties: expectedMetadataSchemaProperties(), description: 'Structured destination metadata guard.' },
      }, ['from', 'to']),
    },
    {
      name: 'fs_delete_directory',
      description: 'Delete a directory under an allowed root and append an audit record. Non-empty deletion requires recursive true.',
      inputSchema: objectSchema({
        path: { type: 'string' },
        recursive: { type: 'boolean', default: false },
        expected_mtime: { type: 'string', description: 'Optional expected directory mtime ISO string before deletion.' },
        expected_size: { type: 'integer', description: 'Optional expected directory byte size before deletion.' },
        expected_tree_sha256: { type: 'string', description: 'Optional expected directory tree sha256 before deletion.' },
        expected_entry_count: { type: 'integer', description: 'Optional expected direct entry count before deletion.' },
        expected: { type: 'object', additionalProperties: false, properties: expectedMetadataSchemaProperties(), description: 'Structured target metadata guard.' },
      }, ['path']),
    },
  ];
  return decorateTools(mode === 'read' ? readTools : [...readTools, ...writeTools]);
}

function callTool(params: any, state: any) {
  const record = asRecord(params);
  const name = stringField(record, 'name');
  let args = asRecord(record.arguments);
  activeToolName = name;
  if (!name) throw diagnosticError('tools_call_requires_name', 'tools_call_requires_name');
  if (!listTools(state.mode).some((tool: any) => tool.name === name)) throw diagnosticError(`tool_not_available_in_${state.mode}_mode`, `tool_not_available_in_${state.mode}_mode: ${name}`, { tool_name: name, mode: state.mode });
  switch (name) {
    case 'fs_guidance': return toolResult(buildGuidanceResult(args));
    case 'fs_read_file': return toolResult(readFileTool(args, state));
    case 'fs_read_file_range': return toolResult(readFileRangeTool(args, state));
    case 'fs_stat': return toolResult(statTool(args, state));
    case 'fs_glob_search': return toolResult(globSearchTool(args, state));
    case 'fs_repository_inventory': return toolResult(repositoryInventoryTool(args, state));
    case 'fs_file_metrics': return toolResult(fileMetricsTool(args, state));
    case 'fs_grep_search': return toolResult(grepSearchTool(args, state), { grepOutputMode: stringField(args, 'output_mode') ?? 'files_with_matches' });
    case 'fs_doctor': return toolResult(doctorTool(state));
    case 'fs_write_file': {
      const payload = resolveFilesystemPayloadArgs(name, args, state);
      return toolResult(attachPayloadSource(writeFileTool(payload.args, state), payload.payloadSource));
    }
    case 'fs_str_replace_file': return toolResult(strReplaceTool(args, state));
    case 'fs_replace_range': return toolResult(replaceRangeTool(args, state));
    case 'fs_apply_patch': return toolResult(applyPatchTool(args, state));
    case 'fs_patch_outcome_show': return toolResult(patchOutcomeShowTool(args, state));
    case 'fs_move_path': return toolResult(movePathTool(args, state));
    case 'fs_create_directory': return toolResult(createDirectoryTool(args, state));
    case 'fs_rename_directory': return toolResult(renameDirectoryTool(args, state));
    case 'fs_delete_directory': return toolResult(deleteDirectoryTool(args, state));
    default: throw diagnosticError('unknown_tool', `unknown_tool: ${name}`, { tool_name: name });
  }
}

function readFileTool(args: any, state: any) {
  const { path, root } = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation: 'fs_read_file' });
  const offset = Math.max(1, integerField(args, 'offset') ?? 1);
  const limit = explicitReadLimit(args, 'fs_read_file');
  const timeoutMs = readOperationTimeoutMs(args);
  const value = readFileRange({ path, root, offset, limit, timeoutMs, operation: 'fs_read_file' });
  return capReadFileResult(value);
}

function readFileRangeTool(args: any, state: any) {
  const startLine = integerField(args, 'start_line');
  const endLine = integerField(args, 'end_line');
  if (startLine === null || startLine < 1) throw diagnosticError('start_line_must_be_positive_integer', 'start_line_must_be_positive_integer', { start_line: startLine ?? null });
  if (endLine === null || endLine < startLine) throw diagnosticError('end_line_must_be_greater_than_or_equal_start_line', 'end_line_must_be_greater_than_or_equal_start_line', { start_line: startLine ?? null, end_line: endLine ?? null });
  assertReadSpanWithinLimit(endLine - startLine + 1, 'fs_read_file_range', { start_line: startLine, end_line: endLine });
  const { path, root } = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation: 'fs_read_file_range' });
  const timeoutMs = readOperationTimeoutMs(args);
  const value = readFileRange({ path, root, offset: startLine, limit: endLine - startLine + 1, timeoutMs, operation: 'fs_read_file_range' });
  return capReadFileResult(value);
}

async function readFileToolAsync(args: any, state: any) {
  const { path, root } = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation: 'fs_read_file' });
  const offset = Math.max(1, integerField(args, 'offset') ?? 1);
  const limit = explicitReadLimit(args, 'fs_read_file');
  const timeoutMs = readOperationTimeoutMs(args);
  const value = await readFileRangeAsync({ path, root, offset, limit, timeoutMs, operation: 'fs_read_file' }, state);
  return capReadFileResult(value);
}

function explicitReadLimit(args: any, operation: string): number {
  const requested = integerField(args, 'limit') ?? 400;
  assertReadSpanWithinLimit(requested, operation, { offset: Math.max(1, integerField(args, 'offset') ?? 1) });
  return Math.max(1, requested);
}

function assertReadSpanWithinLimit(requested: number, operation: string, details: Record<string, unknown>): void {
  if (requested <= MAX_READ_LINE_LIMIT) return;
  throw diagnosticError(`${operation}_limit_exceeds_max`, `${operation}_limit_exceeds_max`, {
    ...details,
    requested_limit: requested,
    max_limit: MAX_READ_LINE_LIMIT,
    mutation_started: false,
    pagination_required: true,
    remediation: 'Retry with limit at most 1000 and follow next_offset, or use fs_read_file_range with a span of at most 1000 lines.',
  });
}

async function readFileRangeToolAsync(args: any, state: any) {
  const startLine = integerField(args, 'start_line');
  const endLine = integerField(args, 'end_line');
  if (startLine === null || startLine < 1) throw diagnosticError('start_line_must_be_positive_integer', 'start_line_must_be_positive_integer', { start_line: startLine ?? null });
  if (endLine === null || endLine < startLine) throw diagnosticError('end_line_must_be_greater_than_or_equal_start_line', 'end_line_must_be_greater_than_or_equal_start_line', { start_line: startLine ?? null, end_line: endLine ?? null });
  assertReadSpanWithinLimit(endLine - startLine + 1, 'fs_read_file_range', { start_line: startLine, end_line: endLine });
  const { path, root } = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation: 'fs_read_file_range' });
  const timeoutMs = readOperationTimeoutMs(args);
  const value = await readFileRangeAsync({ path, root, offset: startLine, limit: endLine - startLine + 1, timeoutMs, operation: 'fs_read_file_range' }, state);
  return capReadFileResult(value);
}

function capReadFileResult(value: any) {
  const rendered = renderFilesystemToolResultText(value);
  if (rendered.length <= READ_RESULT_INLINE_CHAR_LIMIT) return value;
  const endLine = value.returned_lines > 0 ? value.offset + value.returned_lines - 1 : value.offset - 1;
  const suggestedSpan = Math.max(1, Math.min(100, Math.floor(value.returned_lines / 2) || 1));
  return {
    schema: 'local.filesystem.read_window_too_large.v1',
    status: 'truncated',
    error: 'read_window_too_large',
    path: value.path,
    root: value.root,
    relative_path: value.relative_path,
    offset: value.offset,
    limit: value.limit,
    returned_lines: value.returned_lines,
    requested_line_window: { start_line: value.offset, end_line: endLine },
    total_lines: value.total_lines,
    total_lines_exact: value.total_lines_exact,
    total_lines_status: value.total_lines_status,
    line_window_complete: value.line_window_complete,
    next_offset: value.next_offset,
    content_sha256: value.content_sha256,
    rendered_text_char_length: rendered.length,
    content_char_length: value.content.length,
    content_omitted: true,
    remediation: [
      'Retry with a smaller fs_read_file limit and adjusted offset.',
      'Prefer fs_read_file_range for precise bounded slices of large or dense files.',
    ],
    recommended_tool: 'fs_read_file_range',
    recommended_args: {
      path: value.path,
      start_line: value.offset,
      end_line: Math.min(endLine, value.offset + suggestedSpan - 1),
    },
  };
}

function readFileRange({ path, root, offset, limit, timeoutMs, operation }: any) {
  const window = readTextLineWindow({ path, root, offset, limit, timeoutMs, operation });
  const content = window.selected.join('\n');
  return {
    schema: 'local.filesystem.read.v1',
    path,
    root,
    relative_path: relative(root, path).replace(/\\/g, '/'),
    total_lines: window.totalLines,
    total_lines_exact: window.totalLinesExact,
    total_lines_status: window.totalLinesExact ? 'exact' : 'unknown_after_window',
    line_window_complete: window.totalLinesExact,
    offset,
    limit,
    returned_lines: window.selected.length,
    next_offset: window.nextOffset,
    content,
    content_sha256: window.contentSha256,
    content_window_sha256: sha256(content),
    content_hash_scope: 'full_file',
    hash_source: 'live_file_bytes',
    cache_used: false,
    max_limit: MAX_READ_LINE_LIMIT,
    limit_adjusted: false,
    pagination_required: window.nextOffset !== null,
    timeout_ms: timeoutMs,
  };
}

async function readFileRangeAsync({ path, root, offset, limit, timeoutMs, operation }: any, state: any) {
  const startedAt = Date.now();
  const worker = new Worker(READ_LINE_WINDOW_WORKER_SOURCE, {
    eval: true,
    workerData: {
      path,
      root,
      offset,
      limit,
      operation,
      blockMs: readWorkerBlockMs(state),
      timeoutMs,
      deadlineAt: Date.now() + timeoutMs,
    },
  });
  let settled = false;
  return await new Promise((resolvePromise: any, rejectPromise: any) => {
    const timeout = setTimeout(() => {
      if (settled) return;
      settled = true;
      void worker.terminate();
      rejectPromise(readTimeoutError(operation, timeoutMs, {
        path,
        root,
        offset,
        limit,
        elapsedMs: Math.max(Date.now() - startedAt, timeoutMs + 1),
      }));
    }, timeoutMs);
    const cleanup = () => {
      clearTimeout(timeout);
      worker.removeAllListeners();
    };
    worker.on('message', (message: any) => {
      if (settled) return;
      settled = true;
      cleanup();
      const record = asRecord(message);
      if (record.ok === true) {
        const window = asRecord(record.window);
        const content = Array.isArray(window.selected) ? window.selected.join('\n') : '';
        resolvePromise({
          schema: 'local.filesystem.read.v1',
          path,
          root,
          relative_path: relative(root, path).replace(/\\/g, '/'),
          total_lines: window.totalLines ?? null,
          total_lines_exact: window.totalLinesExact === true,
          total_lines_status: window.totalLinesExact === true ? 'exact' : 'unknown_after_window',
          line_window_complete: window.totalLinesExact === true,
          offset,
          limit,
          returned_lines: Array.isArray(window.selected) ? window.selected.length : 0,
          next_offset: window.nextOffset ?? null,
          content,
          content_sha256: String(window.contentSha256 ?? ''),
          content_window_sha256: sha256(content),
          content_hash_scope: 'full_file',
          hash_source: 'live_file_bytes',
          cache_used: false,
          max_limit: MAX_READ_LINE_LIMIT,
          limit_adjusted: false,
          pagination_required: window.nextOffset !== null,
          timeout_ms: timeoutMs,
        });
        return;
      }
      const error = asRecord(record.error);
      rejectPromise(diagnosticError(
        stringField(error, 'codeName') ?? `${operation}_failed`,
        stringField(error, 'message') ?? `${operation}_failed`,
        error.details ?? {},
      ));
    });
    worker.on('error', (error: any) => {
      if (settled) return;
      settled = true;
      cleanup();
      rejectPromise(error);
    });
    worker.on('exit', (code: any) => {
      if (settled) return;
      settled = true;
      cleanup();
      rejectPromise(diagnosticError(`${operation}_worker_exited`, `${operation}_worker_exited`, { code, ...pathMetadata(path, root) }));
    });
  });
}

function readWorkerBlockMs(state: any) {
  const value = Number(asRecord(state.env).NARADA_LOCAL_FILESYSTEM_READ_WORKER_BLOCK_MS ?? 0);
  return Number.isFinite(value) ? Math.max(0, Math.trunc(value)) : 0;
}

const READ_LINE_WINDOW_WORKER_SOURCE = String.raw`
const { parentPort, workerData } = require('node:worker_threads');
const { openSync, readSync, closeSync } = require('node:fs');
const { createHash } = require('node:crypto');
const { StringDecoder } = require('node:string_decoder');

const READ_BUFFER_BYTES = ${READ_BUFFER_BYTES};
const timeoutMs = Number.isFinite(Number(workerData.timeoutMs)) ? Number(workerData.timeoutMs) : 5000;
const deadlineAt = Number.isFinite(Number(workerData.deadlineAt))
  ? Number(workerData.deadlineAt)
  : Date.now() + timeoutMs;

function pathMetadata(path, root) {
  const relative = require('node:path').relative(root, path).replace(/\\/g, '/');
  return { path, root, relative_path: relative };
}

function fail(codeName, message, details = {}) {
  const error = new Error(message);
  error.codeName = codeName;
  error.details = details;
  throw error;
}

function checkTimeout(phase) {
  const now = Date.now();
  if (now <= deadlineAt) return;
  fail(workerData.operation + '_timed_out', workerData.operation + '_timed_out', {
    timeout_kind: 'read_timeout',
    timeout_ms: timeoutMs,
    elapsed_ms: now - (deadlineAt - timeoutMs),
    phase,
    ...pathMetadata(workerData.path, workerData.root),
  });
}

function readTextLineWindow({ path, root, offset, limit }) {
  const fd = openSync(path, 'r');
  const decoder = new StringDecoder('utf8');
  const buffer = Buffer.allocUnsafe(READ_BUFFER_BYTES);
  const selected = [];
  const hash = createHash('sha256');
  let pending = '';
  let lineNumber = 0;
  let reachedEof = false;
  let nextOffset = null;
  let windowBounded = false;
  try {
    while (true) {
      checkTimeout('before_read_file');
      const bytesRead = readSync(fd, buffer, 0, buffer.length, null);

      if (bytesRead === 0) {
        reachedEof = true;
        break;
      }
      const chunk = buffer.subarray(0, bytesRead);
      if (chunk.includes(0)) fail('binary_file_not_supported', 'binary_file_not_supported: ' + path, pathMetadata(path, root));
      hash.update(chunk);
      if (windowBounded) continue;
      pending += decoder.write(chunk);
      const lines = pending.split(/\r?\n/);
      pending = lines.pop() ?? '';
      for (const line of lines) {
        lineNumber += 1;
        if (lineNumber >= offset && selected.length < limit) selected.push(line);
        else if (lineNumber >= offset + limit) {
          nextOffset = lineNumber;
          windowBounded = true;
          pending = '';
          break;
        }
      }
    }
    if (!windowBounded) pending += decoder.end();
    if (!windowBounded && pending.length > 0) {
      lineNumber += 1;
      if (lineNumber >= offset && selected.length < limit) selected.push(pending);
      else if (lineNumber >= offset + limit) nextOffset = lineNumber;
    }
    return { selected, nextOffset, totalLines: windowBounded ? null : lineNumber, totalLinesExact: reachedEof && !windowBounded, contentSha256: hash.digest('hex') };
  } finally {
    closeSync(fd);
  }
}

(async () => {
  try {
    if (workerData.blockMs > 0) await new Promise((resolve) => setTimeout(resolve, workerData.blockMs));
    parentPort.postMessage({ ok: true, window: readTextLineWindow(workerData) });
  } catch (error) {
    parentPort.postMessage({
      ok: false,
      error: {
        codeName: error && error.codeName ? error.codeName : 'read_worker_failed',
        message: error && error.message ? error.message : String(error),
        details: error && error.details ? error.details : {},
      },
    });
  }
})();
`;

function statTool(args: any, state: any) {
  const { path, root } = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation: 'fs_stat' });
  const stat = statSync(path);
  const type = stat.isDirectory() ? 'directory' : stat.isFile() ? 'file' : 'other';
  const directoryFingerprint = type === 'directory' ? directoryTreeFingerprint(path, path) : null;
  return {
    schema: 'local.filesystem.stat.v1',
    path,
    root,
    relative_path: relative(root, path).replace(/\\/g, '/'),
    type,
    size: stat.size,
    mtime: stat.mtime.toISOString(),
    ...(type === 'file' ? { sha256: createHash('sha256').update(readFileSync(path)).digest('hex') } : {}),
    ...(directoryFingerprint ? { entry_count: directoryFingerprint.entry_count, tree_entry_count: directoryFingerprint.tree_entry_count, tree_truncated: directoryFingerprint.tree_truncated, tree_sha256: directoryFingerprint.tree_sha256 } : {}),
  };
}

function doctorTool(state: any) {
  const writeTools = listTools('write').map((tool: any) => tool.name);
  const readTools = listTools('read').map((tool: any) => tool.name);
  const effectiveTools = state.mode === 'write' ? [...readTools, ...writeTools] : readTools;
  return {
    schema: 'local.filesystem.doctor.v1',
    status: 'ok',
    mode: state.mode,
    allowed_roots: state.allowedRoots,
    allowed_root_entries: (state.allowedRootEntries ?? []).map((entry: any) => ({
      root: entry.root,
      provenance: entry.provenance,
    })),
    relative_path_resolution: {
      base: state.allowedRoots[0] ?? null,
      rule: 'first_allowed_root',
      relative_paths: 'Resolve relative filesystem paths against base; the process current directory is not used.',
      absolute_paths: 'Resolve absolute paths as given, then enforce containment under an allowed root.',
      recommendation: 'Pass an absolute path when multiple allowed roots are active or when the target root matters.',
    },
    output_root: state.outputRoot,
    audit_log_dir: state.auditLogDir,
    client_roots: state.clientRoots,
    effective_permissions: {
      can_read: true,
      can_write: state.mode === 'write',
      can_mutate_paths: state.mode === 'write',
      can_delete_directories: state.mode === 'write',
    },
    available_tools: effectiveTools,
    read_tools: readTools,
    write_tools: writeTools,
    default_glob_ignore_patterns: DEFAULT_GLOB_IGNORE_PATTERNS,
    default_grep_ignore_patterns: DEFAULT_GREP_IGNORE_PATTERNS,
  };
}

function globSearchTool(args: any, state: any) {
  return globSearchToolWithOptions(args, state);
}

function globSearchToolWithOptions(args: any, state: any, { ignorePatterns: ignorePatternsOverride = null }: any = {}) {
  const pattern = stringField(args, 'pattern');
  if (!pattern) throw diagnosticError('glob_requires_pattern', 'glob_requires_pattern');
  const { path: directory } = resolveAllowedToolPath(stringField(args, 'directory') ?? '.', state.allowedRoots, { operation: 'fs_glob_search' });
  const offset = Math.max(0, integerField(args, 'offset') ?? 0);
  const limit = Math.min(500, Math.max(1, integerField(args, 'limit') ?? 100));
  const timeoutMs = searchTimeoutMs(args);
  const cachePolicy = searchCachePolicy(args);
  const snapshotId = stringField(args, 'snapshot_id');
  const ignorePatterns = ignorePatternsOverride ?? [...DEFAULT_GLOB_IGNORE_PATTERNS, ...stringList(args.ignore)];
  const rgArgs = ['--files', '--hidden', '--no-ignore', '-g', pattern, ...ignorePatterns.flatMap((ignore: any) => ['-g', negateGlob(ignore)]), directory];
  const freshness = searchFreshness(directory);
  return cappedSearchResult({ state, kind: 'glob', args, page: runRipgrepPage(rgArgs, { operation: 'fs_glob_search', noMatchStatus: 1, offset, limit, timeoutMs, freshness, cachePolicy, snapshotId, diagnosticError, env: state.env }), offset, limit, freshness, cachePolicy });
}

async function globSearchToolAsync(args: any, state: any, context: any) {
  return await globSearchToolAsyncWithOptions(args, state, context);
}

async function globSearchToolAsyncWithOptions(args: any, state: any, context: any, { ignorePatterns: ignorePatternsOverride = null }: any = {}) {
  const pattern = stringField(args, 'pattern');
  if (!pattern) throw diagnosticError('glob_requires_pattern', 'glob_requires_pattern');
  const { path: directory } = resolveAllowedToolPath(stringField(args, 'directory') ?? '.', state.allowedRoots, { operation: 'fs_glob_search' });
  const offset = Math.max(0, integerField(args, 'offset') ?? 0);
  const limit = Math.min(500, Math.max(1, integerField(args, 'limit') ?? 100));
  const timeoutMs = searchTimeoutMs(args);
  const cachePolicy = searchCachePolicy(args);
  const snapshotId = stringField(args, 'snapshot_id');
  const ignorePatterns = ignorePatternsOverride ?? [...DEFAULT_GLOB_IGNORE_PATTERNS, ...stringList(args.ignore)];
  const rgArgs = ['--files', '--hidden', '--no-ignore', '-g', pattern, ...ignorePatterns.flatMap((ignore: any) => ['-g', negateGlob(ignore)]), directory];
  const freshness = searchFreshness(directory);
  const page = await runRipgrepPageAsync(rgArgs, { operation: 'fs_glob_search', noMatchStatus: 1, offset, limit, timeoutMs, freshness, cachePolicy, snapshotId, diagnosticError, abortSignal: context.abortSignal, env: state.env });
  return cappedSearchResult({ state, kind: 'glob', args, page, offset, limit, freshness, cachePolicy });
}

function repositoryInventoryTool(args: any, state: any) {
  const includeGenerated = booleanField(args, 'include_generated') ?? false;
  const value = globSearchTool(repositoryInventorySearchArgs(args, includeGenerated), state);
  return formatRepositoryInventory(value, args, includeGenerated);
}

async function repositoryInventoryToolAsync(args: any, state: any, context: any) {
  const includeGenerated = booleanField(args, 'include_generated') ?? false;
  const value = await globSearchToolAsync(repositoryInventorySearchArgs(args, includeGenerated), state, context);
  return formatRepositoryInventory(value, args, includeGenerated);
}

function fileMetricsTool(args: any, state: any) {
  const normalizedArgs = fileMetricsSearchArgs(args);
  const timeoutMs = filesystemOperationTimeoutMs(args);
  const deadlineAt = Date.now() + timeoutMs;
  const directoryInfo = resolveFileMetricsDirectorySync(normalizedArgs, state);
  const requestedSnapshotId = stringField(args, 'snapshot_id');
  if (requestedSnapshotId) {
    const snapshot = loadFileMetricsSnapshot(requestedSnapshotId, normalizedArgs, directoryInfo);
    return formatStoredFileMetricsSnapshot(snapshot, normalizedArgs, state, directoryInfo, { timeoutMs, cacheHit: true });
  }
  const cachePolicy = searchCachePolicy(args);
  const value = globSearchToolWithOptions(withRemainingFileMetricsTimeout(
    cachePolicy === 'snapshot' || cachePolicy === 'refresh'
      ? { ...normalizedArgs, cache_policy: cachePolicy, snapshot_id: null, offset: 0, limit: 500 }
      : normalizedArgs,
    deadlineAt,
    timeoutMs,
  ), state);
  if (cachePolicy === 'snapshot' || cachePolicy === 'refresh') {
    const collected = collectMetricMatchesSync(value, normalizedArgs, state, deadlineAt, timeoutMs);
    const excluded: any = collectExcludedPathsSync(normalizedArgs, value, state, directoryInfo, deadlineAt, timeoutMs, collected.matches);
    const rows = buildFileMetricRowsSync(collected.matches, normalizedArgs, state, directoryInfo, deadlineAt, timeoutMs, createFileMetricsScanBudget(normalizedArgs));
    excluded.out_of_scope_paths = rows.out_of_scope_paths;
    const snapshot = rememberFileMetricsSnapshot({
      args: normalizedArgs,
      directoryInfo,
      files: rows.files,
      matchedCount: collected.count,
      excluded,
      freshness: collected.freshness,
      scanBytesReserved: rows.scan_bytes_reserved,
      scanBudgetBytes: rows.scan_budget_bytes,
    });
    return formatStoredFileMetricsSnapshot(snapshot, normalizedArgs, state, directoryInfo, { timeoutMs, cacheHit: false });
  }
  const excluded: any = collectExcludedPathsSync(normalizedArgs, value, state, directoryInfo, deadlineAt, timeoutMs);
  const rows = buildFileMetricRowsSync(value.matches, normalizedArgs, state, directoryInfo, deadlineAt, timeoutMs, createFileMetricsScanBudget(normalizedArgs));
  excluded.out_of_scope_paths = rows.out_of_scope_paths;
  return formatFileMetricsPage({
    value,
    args: normalizedArgs,
    state,
    directoryInfo,
    files: rows.files,
    excluded,
    timeoutMs,
    snapshotId: null,
    requestedSnapshotId: null,
    snapshotComplete: false,
    cacheHit: value.cache_hit === true,
    cachePolicy: value.cache_policy ?? cachePolicy,
    freshness: value.freshness ?? null,
    scanBytesReserved: rows.scan_bytes_reserved,
    scanBudgetBytes: rows.scan_budget_bytes,
  });
}

async function fileMetricsToolAsync(args: any, state: any, context: any) {
  const normalizedArgs = fileMetricsSearchArgs(args);
  const timeoutMs = filesystemOperationTimeoutMs(args);
  const deadlineAt = Date.now() + timeoutMs;
  const directoryInfo = await resolveFileMetricsDirectoryAsync(normalizedArgs, state, deadlineAt, timeoutMs, context.abortSignal);
  const requestedSnapshotId = stringField(args, 'snapshot_id');
  if (requestedSnapshotId) {
    const snapshot = loadFileMetricsSnapshot(requestedSnapshotId, normalizedArgs, directoryInfo);
    return formatStoredFileMetricsSnapshot(snapshot, normalizedArgs, state, directoryInfo, { timeoutMs, cacheHit: true });
  }
  const cachePolicy = searchCachePolicy(args);
  const value = await globSearchToolAsyncWithOptions(withRemainingFileMetricsTimeout(
    cachePolicy === 'snapshot' || cachePolicy === 'refresh'
      ? { ...normalizedArgs, cache_policy: cachePolicy, snapshot_id: null, offset: 0, limit: 500 }
      : normalizedArgs,
    deadlineAt,
    timeoutMs,
  ), state, context);
  if (cachePolicy === 'snapshot' || cachePolicy === 'refresh') {
    const collected = await collectMetricMatchesAsync(value, normalizedArgs, state, deadlineAt, timeoutMs, context.abortSignal);
    const excluded: any = await collectExcludedPathsAsync(normalizedArgs, value, state, directoryInfo, deadlineAt, timeoutMs, context.abortSignal, collected.matches);
    const rows = await buildFileMetricRowsAsync(collected.matches, normalizedArgs, state, directoryInfo, deadlineAt, timeoutMs, context.abortSignal, createFileMetricsScanBudget(normalizedArgs));
    excluded.out_of_scope_paths = rows.out_of_scope_paths;
    const snapshot = rememberFileMetricsSnapshot({
      args: normalizedArgs,
      directoryInfo,
      files: rows.files,
      matchedCount: collected.count,
      excluded,
      freshness: collected.freshness,
      scanBytesReserved: rows.scan_bytes_reserved,
      scanBudgetBytes: rows.scan_budget_bytes,
    });
    return formatStoredFileMetricsSnapshot(snapshot, normalizedArgs, state, directoryInfo, { timeoutMs, cacheHit: false });
  }
  const excluded: any = await collectExcludedPathsAsync(normalizedArgs, value, state, directoryInfo, deadlineAt, timeoutMs, context.abortSignal);
  const rows = await buildFileMetricRowsAsync(value.matches, normalizedArgs, state, directoryInfo, deadlineAt, timeoutMs, context.abortSignal, createFileMetricsScanBudget(normalizedArgs));
  excluded.out_of_scope_paths = rows.out_of_scope_paths;
  return formatFileMetricsPage({
    value,
    args: normalizedArgs,
    state,
    directoryInfo,
    files: rows.files,
    excluded,
    timeoutMs,
    snapshotId: null,
    requestedSnapshotId: null,
    snapshotComplete: false,
    cacheHit: value.cache_hit === true,
    cachePolicy: value.cache_policy ?? cachePolicy,
    freshness: value.freshness ?? null,
    scanBytesReserved: rows.scan_bytes_reserved,
    scanBudgetBytes: rows.scan_budget_bytes,
  });
}

function fileMetricsSearchArgs(args: any) {
  const directory = stringField(args, 'directory');
  const root = stringField(args, 'root');
  if (directory && root) throw diagnosticError('file_metrics_directory_ambiguous', 'file_metrics_directory_ambiguous', { directory, root, remediation: 'Pass either directory or root, not both.' });
  return {
    ...args,
    pattern: stringField(args, 'pattern') ?? DEFAULT_FILE_METRICS_PATTERN,
    directory: directory ?? root ?? '.',
    ignore: [...stringList(args.ignore), ...stringList(args.exclude)],
    timeout_ms: integerField(args, 'timeout_ms') ?? DEFAULT_FILESYSTEM_OPERATION_TIMEOUT_MS,
    limit: Math.min(MAX_FILE_METRICS_LIMIT, Math.max(1, integerField(args, 'limit') ?? MAX_FILE_METRICS_LIMIT)),
    max_bytes_per_file: fileMetricsMaxBytesPerFile(args),
    max_total_scan_bytes: fileMetricsMaxTotalScanBytes(args),
  };
}

function formatStoredFileMetricsSnapshot(snapshot: any, args: any, state: any, directoryInfo: any, { timeoutMs, cacheHit }: any) {
  const offset = Math.max(0, integerField(args, 'offset') ?? 0);
  const limit = Math.min(MAX_FILE_METRICS_LIMIT, Math.max(1, integerField(args, 'limit') ?? MAX_FILE_METRICS_LIMIT));
  const pageFiles = snapshot.files.slice(offset, offset + limit);
  return formatFileMetricsPage({
    value: null,
    args,
    state,
    directoryInfo,
    files: pageFiles,
    excluded: snapshot.excluded,
    timeoutMs,
    matchedCount: snapshot.matched_count,
    hasMore: offset + pageFiles.length < snapshot.files.length,
    nextOffset: offset + pageFiles.length < snapshot.files.length ? offset + pageFiles.length : null,
    snapshotId: snapshot.snapshot_id,
    requestedSnapshotId: stringField(args, 'snapshot_id'),
    snapshotComplete: true,
    cacheHit,
    cachePolicy: searchCachePolicy(args),
    freshness: snapshot.freshness,
    scanBytesReserved: snapshot.scan_bytes_reserved,
    scanBudgetBytes: snapshot.scan_budget_bytes,
  });
}

function formatFileMetricsPage({ value, args, state, directoryInfo, files, excluded, timeoutMs, matchedCount = null, hasMore = null, nextOffset = undefined, snapshotId = null, requestedSnapshotId = null, snapshotComplete = false, cacheHit = false, cachePolicy = 'auto', freshness = null, scanBytesReserved = null, scanBudgetBytes = null }: any) {
  const pageTotals = aggregateFileMetrics(files);
  const count = matchedCount ?? value?.count ?? files.length;
  const effectiveHasMore = hasMore ?? value?.has_more === true;
  const effectiveNextOffset = nextOffset === undefined ? value?.next_offset ?? null : nextOffset;
  const requestedOffset = Math.max(0, integerField(args, 'offset') ?? value?.offset ?? 0);
  const effectiveFreshness = freshness ?? value?.freshness ?? null;
  const appliedIgnorePatterns = [
    ...DEFAULT_GLOB_IGNORE_PATTERNS,
    ...stringList(args.ignore),
    ...stringList(args.exclude),
  ];
  return {
    schema: 'local.filesystem.file_metrics.v1',
    status: 'ok',
    directory: directoryInfo.path,
    pattern: stringField(args, 'pattern') ?? DEFAULT_FILE_METRICS_PATTERN,
    offset: Math.max(0, integerField(args, 'offset') ?? value?.offset ?? 0),
    limit: Math.min(MAX_FILE_METRICS_LIMIT, Math.max(1, integerField(args, 'limit') ?? value?.limit ?? MAX_FILE_METRICS_LIMIT)),
    count,
    count_exact: matchedCount !== null || value?.count_exact === true,
    returned: files.length,
    has_more: effectiveHasMore,
    next_offset: effectiveNextOffset,
    order: value?.order ?? 'ripgrep_traversal',
    cache_hit: cacheHit,
    cache_policy: cachePolicy,
    snapshot_id: snapshotId,
    requested_snapshot_id: requestedSnapshotId,
    snapshot_complete: snapshotComplete,
    snapshot_lifecycle: snapshotId || requestedSnapshotId ? {
      scope: 'process_local',
      survives_restart: false,
      eviction_policy: 'least_recently_used',
      max_entries: FILE_METRICS_SNAPSHOT_CACHE_MAX_ENTRIES,
      recovery: 'If the snapshot is missing, rerun fs_file_metrics with cache_policy=snapshot or refresh.',
    } : null,
    timeout_ms: timeoutMs,
    scan_budget_bytes: scanBudgetBytes ?? fileMetricsMaxTotalScanBytes(args),
    scan_bytes_reserved: scanBytesReserved,
    freshness: effectiveFreshness,
    scope: {
      directory: directoryInfo.path,
      allowed_root: directoryInfo.root,
      allowed_roots: state.allowedRoots,
      include_pattern: stringField(args, 'pattern') ?? DEFAULT_FILE_METRICS_PATTERN,
      ignore_patterns: [...new Set(appliedIgnorePatterns)],
      ignored_paths: excluded.ignored_paths,
      ignored_path_count: excluded.ignored_count,
      ignored_paths_complete: excluded.ignored_paths_complete,
      ignored_paths_truncated: excluded.ignored_paths_truncated === true,
      out_of_scope_paths: excluded.out_of_scope_paths,
      out_of_scope_path_count: excluded.out_of_scope_paths.length,
      out_of_scope_paths_complete: snapshotComplete === true || (effectiveHasMore !== true && requestedOffset === 0),
      boundary: {
        allowed_root: directoryInfo.root,
        directory: directoryInfo.path,
        realpath_enforced: true,
      },
      contents_returned: false,
    },
    totals: pageTotals,
    totals_scope: 'returned_page',
    files,
  };
}

function fileMetricsMaxBytesPerFile(args: any) {
  const value = integerField(args, 'max_bytes_per_file');
  if (value === null) return DEFAULT_FILE_METRICS_MAX_BYTES_PER_FILE;
  return Math.min(MAX_FILE_METRICS_MAX_BYTES_PER_FILE, Math.max(1, value));
}

function fileMetricsMaxTotalScanBytes(args: any) {
  const value = integerField(args, 'max_total_scan_bytes');
  if (value === null) return DEFAULT_FILE_METRICS_MAX_TOTAL_SCAN_BYTES;
  return Math.min(MAX_FILE_METRICS_MAX_TOTAL_SCAN_BYTES, Math.max(1, value));
}

function createFileMetricsScanBudget(args: any) {
  return { max_bytes: fileMetricsMaxTotalScanBytes(args), reserved_bytes: 0 };
}

function reserveFileMetricsScanBytes(scanBudget: any, byteCount: any) {
  const bytes = typeof byteCount === 'number' && Number.isFinite(byteCount) ? Math.max(0, byteCount) : 0;
  if (scanBudget.reserved_bytes + bytes > scanBudget.max_bytes) return false;
  scanBudget.reserved_bytes += bytes;
  return true;
}

function withRemainingFileMetricsTimeout(args: any, deadlineAt: any, timeoutMs: any) {
  const remaining = Math.max(1, deadlineAt - Date.now());
  checkFileMetricsDeadline(deadlineAt, timeoutMs, 'before_search');
  return { ...args, timeout_ms: Math.min(timeoutMs, remaining) };
}

function fileMetricsTimeoutError(timeoutMs: any, phase: any, elapsedMs: any) {
  return diagnosticError('fs_file_metrics_timed_out', 'fs_file_metrics_timed_out', {
    timeout_kind: 'filesystem_operation_timeout',
    operation: 'fs_file_metrics',
    phase,
    timeout_ms: timeoutMs,
    elapsed_ms: elapsedMs,
    remediation: [
      'Reduce the requested directory or include pattern.',
      'Use offset and limit to page bounded metric rows.',
      'Lower max_bytes_per_file when exact line counts are not required for large files.',
      'Use fs_file_metrics instead of concurrent full-content fs_read_file calls when only line counts or sizes are needed.',
    ],
  });
}

function checkFileMetricsDeadline(deadlineAt: any, timeoutMs: any, phase: any, abortSignal: any = null) {
  if (abortSignal?.aborted) {
    throw diagnosticError('fs_file_metrics_cancelled', 'fs_file_metrics_cancelled', {
      operation: 'fs_file_metrics',
      phase,
      timeout_ms: timeoutMs,
      elapsed_ms: Date.now() - (deadlineAt - timeoutMs),
    });
  }
  const elapsedMs = Date.now() - (deadlineAt - timeoutMs);
  if (Date.now() <= deadlineAt) return;
  throw fileMetricsTimeoutError(timeoutMs, phase, elapsedMs);
}

function resolveFileMetricsDirectorySync(args: any, state: any) {
  const input = stringField(args, 'directory') ?? stringField(args, 'root') ?? '.';
  const lexical = resolveAllowedToolPath(input, state.allowedRoots, { operation: 'fs_file_metrics', field: 'directory' });
  try {
    const realRoot = realpathSync(lexical.root);
    const realPath = realpathSync(lexical.path);
    if (!isPathWithinOrEqual(realPath, realRoot)) {
      throw diagnosticError('file_metrics_directory_outside_allowed_root', 'file_metrics_directory_outside_allowed_root', {
        operation: 'fs_file_metrics',
        path: lexical.path,
        root: lexical.root,
        realpath: realPath,
      });
    }
    return { ...lexical, real_path: realPath, real_root: realRoot };
  } catch (error) {
    if (error instanceof McpToolError) throw error;
    throw diagnosticError('file_metrics_directory_unavailable', 'file_metrics_directory_unavailable', {
      operation: 'fs_file_metrics',
      path: lexical.path,
      root: lexical.root,
      error: error instanceof Error ? error.message : String(error),
    });
  }
}

async function resolveFileMetricsDirectoryAsync(args: any, state: any, deadlineAt: any, timeoutMs: any, abortSignal: any) {
  const input = stringField(args, 'directory') ?? stringField(args, 'root') ?? '.';
  const lexical = resolveAllowedToolPath(input, state.allowedRoots, { operation: 'fs_file_metrics', field: 'directory' });
  checkFileMetricsDeadline(deadlineAt, timeoutMs, 'before_directory', abortSignal);
  try {
    const realRoot = await realpathAsync(lexical.root);
    const realPath = await realpathAsync(lexical.path);
    checkFileMetricsDeadline(deadlineAt, timeoutMs, 'after_directory', abortSignal);
    if (!isPathWithinOrEqual(realPath, realRoot)) {
      throw diagnosticError('file_metrics_directory_outside_allowed_root', 'file_metrics_directory_outside_allowed_root', {
        operation: 'fs_file_metrics',
        path: lexical.path,
        root: lexical.root,
        realpath: realPath,
      });
    }
    return { ...lexical, real_path: realPath, real_root: realRoot };
  } catch (error) {
    if (error instanceof McpToolError) throw error;
    throw diagnosticError('file_metrics_directory_unavailable', 'file_metrics_directory_unavailable', {
      operation: 'fs_file_metrics',
      path: lexical.path,
      root: lexical.root,
      error: error instanceof Error ? error.message : String(error),
    });
  }
}

function buildFileMetricRowsSync(matches: any, args: any, state: any, directoryInfo: any, deadlineAt: any, timeoutMs: any, scanBudget: any = createFileMetricsScanBudget(args)) {
  const files = [];
  const outOfScopePaths = [];
  const maxBytesPerFile = fileMetricsMaxBytesPerFile(args);
  const checkTimeout = (phase: any) => checkFileMetricsDeadline(deadlineAt, timeoutMs, phase);
  for (const match of Array.isArray(matches) ? matches : []) {
    checkTimeout('before_file');
    const filePath = String(match);
    try {
      const resolved = resolveAllowedToolPath(filePath, state.allowedRoots, { operation: 'fs_file_metrics', field: 'match' });
      const realPath = realpathSync(resolved.path);
      const realRoot = samePath(resolved.root, directoryInfo.root) ? directoryInfo.real_root : realpathSync(resolved.root);
      if (!isPathWithinOrEqual(realPath, realRoot)) {
        outOfScopePaths.push(metricDisplayPath(resolved.path, directoryInfo.path));
        continue;
      }
      const stat = statSync(realPath);
      if (!stat.isFile()) continue;
      const lineMetrics = stat.size > maxBytesPerFile
        ? { line_count: null, line_count_status: 'too_large' }
        : reserveFileMetricsScanBytes(scanBudget, stat.size)
          ? countFileLines(realPath, checkTimeout)
          : { line_count: null, line_count_status: 'scan_budget_exceeded' };
      const relativePath = relative(directoryInfo.path, resolved.path).replace(/\\/g, '/');
      const rootRelativePath = relative(resolved.root, resolved.path).replace(/\\/g, '/');
      files.push({
        path: resolved.path,
        relative_path: relativePath,
        root_relative_path: rootRelativePath,
        line_count: lineMetrics.line_count,
        line_count_status: lineMetrics.line_count_status,
        byte_count: stat.size,
        file_type: fileType(resolved.path, lineMetrics.line_count_status),
        scope_classification: classifyRepositoryInventoryPath(rootRelativePath),
        mtime: stat.mtime.toISOString(),
      });
    } catch (error) {
      if (error instanceof McpToolError) throw error;
      files.push({
        path: filePath,
        relative_path: metricDisplayPath(filePath, directoryInfo.path),
        root_relative_path: metricDisplayPath(filePath, directoryInfo.root),
        line_count: null,
        line_count_status: 'unavailable',
        byte_count: null,
        file_type: 'unavailable',
        scope_classification: 'unavailable',
        error_code: 'file_metric_unavailable',
      });
    }
  }
  return { files, out_of_scope_paths: outOfScopePaths, scan_bytes_reserved: scanBudget.reserved_bytes, scan_budget_bytes: scanBudget.max_bytes };
}

async function buildFileMetricRowsAsync(matches: any, args: any, state: any, directoryInfo: any, deadlineAt: any, timeoutMs: any, abortSignal: any, scanBudget: any = createFileMetricsScanBudget(args)) {
  const files = [];
  const outOfScopePaths = [];
  const maxBytesPerFile = fileMetricsMaxBytesPerFile(args);
  const checkTimeout = (phase: any, signal: any = abortSignal) => checkFileMetricsDeadline(deadlineAt, timeoutMs, phase, signal);
  for (const match of Array.isArray(matches) ? matches : []) {
    checkTimeout('before_file');
    const filePath = String(match);
    try {
      const resolved = resolveAllowedToolPath(filePath, state.allowedRoots, { operation: 'fs_file_metrics', field: 'match' });
      const realPath = await realpathAsync(resolved.path);
      const realRoot = samePath(resolved.root, directoryInfo.root) ? directoryInfo.real_root : await realpathAsync(resolved.root);
      checkTimeout('after_realpath');
      if (!isPathWithinOrEqual(realPath, realRoot)) {
        outOfScopePaths.push(metricDisplayPath(resolved.path, directoryInfo.path));
        continue;
      }
      const stat = await statAsync(realPath);
      checkTimeout('after_stat');
      if (!stat.isFile()) continue;
      const lineMetrics = stat.size > maxBytesPerFile
        ? { line_count: null, line_count_status: 'too_large' }
        : reserveFileMetricsScanBytes(scanBudget, stat.size)
          ? await countFileLinesAsync(realPath, checkTimeout, abortSignal)
          : { line_count: null, line_count_status: 'scan_budget_exceeded' };
      const relativePath = relative(directoryInfo.path, resolved.path).replace(/\\/g, '/');
      const rootRelativePath = relative(resolved.root, resolved.path).replace(/\\/g, '/');
      files.push({
        path: resolved.path,
        relative_path: relativePath,
        root_relative_path: rootRelativePath,
        line_count: lineMetrics.line_count,
        line_count_status: lineMetrics.line_count_status,
        byte_count: stat.size,
        file_type: fileType(resolved.path, lineMetrics.line_count_status),
        scope_classification: classifyRepositoryInventoryPath(rootRelativePath),
        mtime: stat.mtime.toISOString(),
      });
    } catch (error) {
      if (error instanceof McpToolError) throw error;
      files.push({
        path: filePath,
        relative_path: metricDisplayPath(filePath, directoryInfo.path),
        root_relative_path: metricDisplayPath(filePath, directoryInfo.root),
        line_count: null,
        line_count_status: 'unavailable',
        byte_count: null,
        file_type: 'unavailable',
        scope_classification: 'unavailable',
        error_code: 'file_metric_unavailable',
      });
    }
  }
  return { files, out_of_scope_paths: outOfScopePaths, scan_bytes_reserved: scanBudget.reserved_bytes, scan_budget_bytes: scanBudget.max_bytes };
}

function collectMetricMatchesSync(firstValue: any, args: any, state: any, deadlineAt: any, timeoutMs: any) {
  const matches = [];
  let page = firstValue;
  let snapshotId = page.snapshot_id ?? null;
  while (true) {
    checkFileMetricsDeadline(deadlineAt, timeoutMs, 'before_snapshot_page');
    matches.push(...(Array.isArray(page.matches) ? page.matches : []));
    if (matches.length > MAX_FILE_METRICS_SNAPSHOT_FILES) {
      throw diagnosticError('file_metrics_snapshot_too_large', 'file_metrics_snapshot_too_large', { max_files: MAX_FILE_METRICS_SNAPSHOT_FILES });
    }
    if (page.has_more !== true) break;
    if (!snapshotId) {
      throw diagnosticError('file_metrics_snapshot_unavailable', 'file_metrics_snapshot_unavailable', { reason: 'path_snapshot_not_available' });
    }
    page = globSearchToolWithOptions({
      ...withRemainingFileMetricsTimeout(args, deadlineAt, timeoutMs),
      offset: matches.length,
      snapshot_id: snapshotId,
      cache_policy: 'auto',
    }, state);
  }
  return { matches, count: page.count ?? matches.length, freshness: page.freshness ?? firstValue.freshness ?? null };
}

async function collectMetricMatchesAsync(firstValue: any, args: any, state: any, deadlineAt: any, timeoutMs: any, abortSignal: any) {
  const matches = [];
  let page = firstValue;
  let snapshotId = page.snapshot_id ?? null;
  while (true) {
    checkFileMetricsDeadline(deadlineAt, timeoutMs, 'before_snapshot_page', abortSignal);
    matches.push(...(Array.isArray(page.matches) ? page.matches : []));
    if (matches.length > MAX_FILE_METRICS_SNAPSHOT_FILES) {
      throw diagnosticError('file_metrics_snapshot_too_large', 'file_metrics_snapshot_too_large', { max_files: MAX_FILE_METRICS_SNAPSHOT_FILES });
    }
    if (page.has_more !== true) break;
    if (!snapshotId) {
      throw diagnosticError('file_metrics_snapshot_unavailable', 'file_metrics_snapshot_unavailable', { reason: 'path_snapshot_not_available' });
    }
    page = await globSearchToolAsyncWithOptions({
      ...withRemainingFileMetricsTimeout(args, deadlineAt, timeoutMs),
      offset: matches.length,
      snapshot_id: snapshotId,
      cache_policy: 'auto',
    }, state, { abortSignal });
  }
  return { matches, count: page.count ?? matches.length, freshness: page.freshness ?? firstValue.freshness ?? null };
}

function collectExcludedPathsSync(args: any, selectedValue: any, state: any, directoryInfo: any, deadlineAt: any, timeoutMs: any, selectedMatches: any = null) {
  const requestedOffset = Math.max(0, integerField(args, 'offset') ?? 0);
  const selectedComplete = Array.isArray(selectedMatches) || (requestedOffset === 0 && selectedValue.has_more !== true);
  const selected = selectedComplete
    ? (Array.isArray(selectedMatches) ? selectedMatches : (Array.isArray(selectedValue.matches) ? selectedValue.matches : []))
    : [];
  const allValue = globSearchToolWithOptions({
    ...withRemainingFileMetricsTimeout({ ...args, ignore: [], exclude: [], offset: 0, limit: 500, cache_policy: 'bypass', snapshot_id: null }, deadlineAt, timeoutMs),
  }, state, { ignorePatterns: [] });
  const selectedSet = new Set(selected.map((path: any) => normalizePathKey(path).toLowerCase()));
  const ignoredCandidates = (selectedComplete && Array.isArray(allValue.matches) ? allValue.matches : [])
    .filter((path: any) => !selectedSet.has(normalizePathKey(path).toLowerCase()))
    .map((path: any) => metricDisplayPath(path, directoryInfo.path));
  const ignoredPaths = ignoredCandidates.slice(0, MAX_FILE_METRICS_LIMIT);
  const ignoredPathsTruncated = ignoredCandidates.length > MAX_FILE_METRICS_LIMIT;
  const ignoredCount = typeof allValue.count === 'number' && typeof selectedValue.count === 'number'
    ? Math.max(0, allValue.count - selectedValue.count)
    : null;
  return {
    ignored_paths: ignoredPaths,
    ignored_count: ignoredCount,
    ignored_paths_complete: allValue.has_more !== true && selectedComplete && !ignoredPathsTruncated,
    ignored_paths_truncated: ignoredPathsTruncated,
    out_of_scope_paths: [],
  };
}

async function collectExcludedPathsAsync(args: any, selectedValue: any, state: any, directoryInfo: any, deadlineAt: any, timeoutMs: any, abortSignal: any, selectedMatches: any = null) {
  const requestedOffset = Math.max(0, integerField(args, 'offset') ?? 0);
  const selectedComplete = Array.isArray(selectedMatches) || (requestedOffset === 0 && selectedValue.has_more !== true);
  const selected = selectedComplete
    ? (Array.isArray(selectedMatches) ? selectedMatches : (Array.isArray(selectedValue.matches) ? selectedValue.matches : []))
    : [];
  const allValue = await globSearchToolAsyncWithOptions({
    ...withRemainingFileMetricsTimeout({ ...args, ignore: [], exclude: [], offset: 0, limit: 500, cache_policy: 'bypass', snapshot_id: null }, deadlineAt, timeoutMs),
  }, state, { abortSignal }, { ignorePatterns: [] });
  const selectedSet = new Set(selected.map((path: any) => normalizePathKey(path).toLowerCase()));
  const ignoredCandidates = (selectedComplete && Array.isArray(allValue.matches) ? allValue.matches : [])
    .filter((path: any) => !selectedSet.has(normalizePathKey(path).toLowerCase()))
    .map((path: any) => metricDisplayPath(path, directoryInfo.path));
  const ignoredPaths = ignoredCandidates.slice(0, MAX_FILE_METRICS_LIMIT);
  const ignoredPathsTruncated = ignoredCandidates.length > MAX_FILE_METRICS_LIMIT;
  const ignoredCount = typeof allValue.count === 'number' && typeof selectedValue.count === 'number'
    ? Math.max(0, allValue.count - selectedValue.count)
    : null;
  return {
    ignored_paths: ignoredPaths,
    ignored_count: ignoredCount,
    ignored_paths_complete: allValue.has_more !== true && selectedComplete && !ignoredPathsTruncated,
    ignored_paths_truncated: ignoredPathsTruncated,
    out_of_scope_paths: [],
  };
}

function fileMetricsSnapshotKey(args: any, directoryInfo: any) {
  return sha256(JSON.stringify({
    directory: directoryInfo.path,
    pattern: stringField(args, 'pattern') ?? DEFAULT_FILE_METRICS_PATTERN,
    ignore: stringList(args.ignore),
    max_bytes_per_file: fileMetricsMaxBytesPerFile(args),
    max_total_scan_bytes: fileMetricsMaxTotalScanBytes(args),
  }));
}

function rememberFileMetricsSnapshot({ args, directoryInfo, files, matchedCount, excluded, freshness, scanBytesReserved, scanBudgetBytes }: any) {
  const cacheKey = fileMetricsSnapshotKey(args, directoryInfo);
  for (const [id, existing] of fileMetricsSnapshotCache.entries()) {
    if (existing.cache_key === cacheKey) fileMetricsSnapshotCache.delete(id);
  }
  const snapshot = {
    snapshot_id: 'fm_' + randomUUID().replace(/-/g, ''),
    cache_key: cacheKey,
    directory: directoryInfo.path,
    allowed_root: directoryInfo.root,
    files,
    matched_count: matchedCount,
    excluded,
    freshness,
    scan_bytes_reserved: scanBytesReserved,
    scan_budget_bytes: scanBudgetBytes,
  };
  fileMetricsSnapshotCache.set(snapshot.snapshot_id, snapshot);
  while (fileMetricsSnapshotCache.size > FILE_METRICS_SNAPSHOT_CACHE_MAX_ENTRIES) {
    const oldest = fileMetricsSnapshotCache.keys().next().value;
    fileMetricsSnapshotCache.delete(oldest);
  }
  return snapshot;
}

function loadFileMetricsSnapshot(snapshotId: any, args: any, directoryInfo: any) {
  const snapshot = fileMetricsSnapshotCache.get(snapshotId);
  if (!snapshot || snapshot.cache_key !== fileMetricsSnapshotKey(args, directoryInfo)) {
    throw diagnosticError('fs_file_metrics_snapshot_not_found', 'fs_file_metrics_snapshot_not_found: ' + snapshotId, {
      snapshot_id: snapshotId,
      requested_directory: directoryInfo.path,
    });
  }
  fileMetricsSnapshotCache.delete(snapshotId);
  fileMetricsSnapshotCache.set(snapshotId, snapshot);
  return snapshot;
}

function metricDisplayPath(path: any, directory: any) {
  try {
    return relative(directory, path).replace(/\\/g, '/');
  } catch {
    return String(path);
  }
}

function countFileLines(path: any, checkTimeout: any) {
  const fd = openSync(path, 'r');
  const decoder = new StringDecoder('utf8');
  const buffer = Buffer.allocUnsafe(READ_BUFFER_BYTES);
  let lineCount = 0;
  let pending = '';
  try {
    while (true) {
      checkTimeout('read_file');
      const bytesRead = readSync(fd, buffer, 0, buffer.length, null);
      checkTimeout('after_read_file');
      if (bytesRead === 0) break;
      const chunk = buffer.subarray(0, bytesRead);
      if (chunk.includes(0)) return { line_count: null, line_count_status: 'binary' };
      pending += decoder.write(chunk);
      const lines = pending.split(/\r?\n/);
      pending = lines.pop() ?? '';
      lineCount += lines.length;
    }
    pending += decoder.end();
    if (pending.length > 0) lineCount += 1;
    return { line_count: lineCount, line_count_status: 'exact' };
  } finally {
    closeSync(fd);
  }
}

function fileType(path: any, lineCountStatus: any) {
  if (lineCountStatus === 'binary') return 'binary';
  const extension = extname(path).replace(/^\./, '').toLowerCase();
  return extension || 'no_extension';
}

function aggregateFileMetrics(files: any) {
  let lineCount = 0;
  let lineCountKnown = true;
  let byteCount = 0;
  let binaryFileCount = 0;
  let tooLargeFileCount = 0;
  let unavailableFileCount = 0;
  let scanBudgetExceededFileCount = 0;
  for (const file of files) {
    if (typeof file.byte_count === 'number') byteCount += file.byte_count;
    if (typeof file.line_count === 'number') lineCount += file.line_count;
    else {
      lineCountKnown = false;
      if (file.line_count_status === 'binary') binaryFileCount += 1;
      if (file.line_count_status === 'too_large') tooLargeFileCount += 1;
      if (file.line_count_status === 'unavailable') unavailableFileCount += 1;
      if (file.line_count_status === 'scan_budget_exceeded') scanBudgetExceededFileCount += 1;
    }
  }
  return {
    file_count: files.length,
    byte_count: byteCount,
    line_count: lineCountKnown ? lineCount : null,
    line_count_status: lineCountKnown ? 'exact' : 'partial',
    binary_file_count: binaryFileCount,
    too_large_file_count: tooLargeFileCount,
    unavailable_file_count: unavailableFileCount,
    scan_budget_exceeded_file_count: scanBudgetExceededFileCount,
  };
}

function repositoryInventorySearchArgs(args: any, includeGenerated: any) {
  return {
    ...args,
    pattern: stringField(args, 'pattern') ?? '**/*',
    ignore: [
      ...(includeGenerated ? [] : DEFAULT_REPOSITORY_INVENTORY_IGNORE_PATTERNS),
      ...stringList(args.ignore),
    ],
  };
}

function formatRepositoryInventory(value: any, args: any, includeGenerated: any) {
  const matches = Array.isArray(value.matches) ? value.matches.map((item: any) => String(item)) : [];
  const classifications = matches.map((path: any) => ({ path, classification: classifyRepositoryInventoryPath(path) }));
  const candidateSourcePaths = classifications.filter((item: any) => item.classification === 'candidate_source').map((item: any) => item.path);
  const generatedArtifactPaths = classifications.filter((item: any) => item.classification === 'generated_artifact').map((item: any) => item.path);
  const appliedIgnorePatterns = [
    ...DEFAULT_GLOB_IGNORE_PATTERNS,
    ...(includeGenerated ? [] : DEFAULT_REPOSITORY_INVENTORY_IGNORE_PATTERNS),
    ...stringList(args.ignore),
  ];
  return {
    ...value,
    schema: 'local.filesystem.repository_inventory.v1',
    directory: stringField(args, 'directory') ?? '.',
    pattern: stringField(args, 'pattern') ?? '**/*',
    include_generated: includeGenerated,
    matches,
    classifications,
    candidate_source_paths: candidateSourcePaths,
    candidate_source_count: candidateSourcePaths.length,
    generated_artifact_paths: generatedArtifactPaths,
    generated_artifact_count: generatedArtifactPaths.length,
    applied_ignore_patterns: [...new Set(appliedIgnorePatterns)],
    generated_artifacts_excluded_by_default: !includeGenerated,
    git_tracking_boundary: {
      tracked_paths: null,
      ignored_paths: null,
      authority: 'git-mcp',
      next_tool: 'git_changed_summary',
      note: 'This filesystem inventory identifies bounded candidate and generated paths; Git-tracked and Git-ignored state is authoritative in git-mcp.',
    },
  };
}

function classifyRepositoryInventoryPath(value: any) {
  const normalized = '/' + String(value).replaceAll('\\', '/').replace(/^\/+|\/+$/g, '').toLowerCase() + '/';
  return REPOSITORY_GENERATED_PATH_MARKERS.some((marker: any) => normalized.includes(marker))
    ? 'generated_artifact'
    : 'candidate_source';
}

function readSummary(value: any) {
  return {
    path: value.path,
    relative_path: value.relative_path,
    offset: value.offset,
    limit: value.limit,
    returned_lines: value.returned_lines,
    next_offset: value.next_offset,
    total_lines: value.total_lines,
    total_lines_exact: value.total_lines_exact,
    total_lines_status: value.total_lines_status,
    line_window_complete: value.line_window_complete,
    content_sha256: value.content_sha256,
  };
}

function resolveGrepSearchScope(args: any, state: any) {
  const directory = stringOrNull(stringField(args, 'directory'));
  const pathArgument = stringOrNull(stringField(args, 'path'));
  if (directory && pathArgument) {
    throw diagnosticError('grep_scope_ambiguous', 'grep_scope_ambiguous', {
      operation: 'fs_grep_search',
      directory,
      path: pathArgument,
      remediation: 'Pass exactly one of directory or path. directory is canonical; path remains a compatibility alias.',
    });
  }
  const requestedPath = directory ?? pathArgument ?? '.';
  const resolved = resolveAllowedToolPath(requestedPath, state.allowedRoots, { operation: 'fs_grep_search' });
  const includeGlob = stringOrNull(stringField(args, 'glob'));
  const excludedGlobs = [
    ...DEFAULT_GREP_IGNORE_PATTERNS,
    ...stringList(args.ignore),
    ...stringList(args.exclude),
  ];
  return {
    path: resolved.path,
    root: resolved.root,
    requested_path: requestedPath,
    argument: directory ? 'directory' : pathArgument ? 'path' : 'default_allowed_root',
    include_glob: includeGlob,
    excluded_globs: excludedGlobs,
    ripgrep_glob_args: [
      ...(includeGlob ? ['-g', includeGlob] : []),
      ...excludedGlobs.flatMap((ignore: any) => ['-g', negateGlob(ignore)]),
    ],
  };
}

function grepSearchTool(args: any, state: any) {
  const pattern = stringField(args, 'pattern');
  if (!pattern) throw diagnosticError('grep_requires_pattern', 'grep_requires_pattern');
  const scope = resolveGrepSearchScope(args, state);
  const mode = stringField(args, 'output_mode') ?? 'files_with_matches';
  if (!['files_with_matches', 'count_matches', 'content'].includes(mode)) throw diagnosticError('grep_output_mode_unsupported', `grep_output_mode_unsupported: ${mode}`, { output_mode: mode });
  const offset = Math.max(0, integerField(args, 'offset') ?? 0);
  const limit = Math.min(500, Math.max(1, integerField(args, 'limit') ?? 80));
  const timeoutMs = searchTimeoutMs(args);
  const cachePolicy = searchCachePolicy(args);
  const snapshotId = stringField(args, 'snapshot_id');
  const modeArgs = mode === 'content' ? ['-n'] : mode === 'count_matches' ? ['-c'] : ['-l'];
  const freshness = searchFreshness(scope.path);
  return cappedSearchResult({ state, kind: 'grep', args: { ...args, output_mode: mode, grep_scope: scope }, page: runRipgrepPage(['--field-match-separator', RIPGREP_FIELD_SEPARATOR, '--with-filename', ...modeArgs, ...scope.ripgrep_glob_args, '--', pattern, scope.path], { operation: 'fs_grep_search', noMatchStatus: 1, offset, limit, timeoutMs, freshness, cachePolicy, snapshotId, diagnosticError, env: state.env }), offset, limit, freshness, cachePolicy });
}

async function grepSearchToolAsync(args: any, state: any, context: any) {
  const pattern = stringField(args, 'pattern');
  if (!pattern) throw diagnosticError('grep_requires_pattern', 'grep_requires_pattern');
  const scope = resolveGrepSearchScope(args, state);
  const mode = stringField(args, 'output_mode') ?? 'files_with_matches';
  if (!['files_with_matches', 'count_matches', 'content'].includes(mode)) throw diagnosticError('grep_output_mode_unsupported', `grep_output_mode_unsupported: ${mode}`, { output_mode: mode });
  const offset = Math.max(0, integerField(args, 'offset') ?? 0);
  const limit = Math.min(500, Math.max(1, integerField(args, 'limit') ?? 80));
  const timeoutMs = searchTimeoutMs(args);
  const cachePolicy = searchCachePolicy(args);
  const snapshotId = stringField(args, 'snapshot_id');
  const modeArgs = mode === 'content' ? ['-n'] : mode === 'count_matches' ? ['-c'] : ['-l'];
  const freshness = searchFreshness(scope.path);
  const page = await runRipgrepPageAsync(['--field-match-separator', RIPGREP_FIELD_SEPARATOR, '--with-filename', ...modeArgs, ...scope.ripgrep_glob_args, '--', pattern, scope.path], { operation: 'fs_grep_search', noMatchStatus: 1, offset, limit, timeoutMs, freshness, cachePolicy, snapshotId, diagnosticError, abortSignal: context.abortSignal, env: state.env });
  return cappedSearchResult({ state, kind: 'grep', args: { ...args, output_mode: mode, grep_scope: scope }, page, offset, limit, freshness, cachePolicy });
}

function cappedSearchResult({ state, kind, args, page, offset, limit, freshness, cachePolicy }: any) {
  const matches = page.matches;
  const nextOffset = page.has_more ? offset + matches.length : null;
  const grepMode = stringField(args, 'output_mode') ?? 'files_with_matches';
  const noMatchDiagnostics = matches.length === 0 && page.count === 0 ? {
    status: 'no_matches_observed',
    cache_hit: page.cache_hit === true,
    cache_policy: page.cache_policy ?? cachePolicy,
    snapshot_complete: page.snapshot_complete === true,
    freshness,
    stale_cache_evidence: false,
    remediation: 'No matches were returned for the current path freshness fingerprint. If files are known to exist, retry with cache_policy="refresh" or cache_policy="bypass" and verify the directory/pattern pair.',
  } : null;
  const value = {
    schema: `local.filesystem.${kind}.v1`,
    status: 'ok',
    ...(kind === 'grep' ? { output_mode: stringField(args, 'output_mode') ?? 'files_with_matches' } : {}),
    ...(kind === 'grep' ? { scope: args.grep_scope ?? null } : {}),
    offset,
    limit,
    count: page.count,
    count_exact: page.count_exact,
    scanned: page.scanned,
    scanned_unit: 'matched_entries',
    returned: matches.length,
    order: 'ripgrep_traversal',
    cache_hit: page.cache_hit === true,
    cache_policy: page.cache_policy ?? cachePolicy,
    snapshot_id: page.snapshot_id ?? null,
    requested_snapshot_id: page.requested_snapshot_id ?? stringField(args, 'snapshot_id'),
    snapshot_complete: page.snapshot_complete === true,
    cache_memory_bytes: page.cache_memory_bytes ?? null,
    page_match_bytes: page.page_match_bytes ?? null,
    page_match_bytes_limit: page.page_match_bytes_limit ?? null,
    page_matches_truncated: page.page_matches_truncated ?? 0,
    timeout_ms: page.timeout_ms ?? null,
    freshness,
    has_more: page.has_more,
    next_offset: nextOffset,
    matches_format: kind === 'grep' ? 'human' : 'path',
    matches: kind === 'grep' ? matches.map((match: any) => renderGrepMatch(match, grepMode)) : matches,
    ...(noMatchDiagnostics ? { no_match_diagnostics: noMatchDiagnostics } : {}),
    ...(kind === 'grep' ? { match_objects_authoritative: true, match_objects: matches.map((match: any) => buildGrepMatchObject(match, grepMode)) } : {}),
  };
  return cappedToolValue({ state, value, summary: { count: value.count, count_exact: value.count_exact, scanned: value.scanned, scanned_unit: value.scanned_unit, returned: value.returned, order: value.order, cache_hit: value.cache_hit, cache_policy: value.cache_policy, snapshot_id: value.snapshot_id, snapshot_complete: value.snapshot_complete, cache_memory_bytes: value.cache_memory_bytes, page_match_bytes: value.page_match_bytes, page_match_bytes_limit: value.page_match_bytes_limit, page_matches_truncated: value.page_matches_truncated, timeout_ms: value.timeout_ms, freshness: value.freshness, matches_format: value.matches_format, has_more: value.has_more, next_offset: value.next_offset } });
}

function cappedToolValue({ state, value, summary = {} }: any) {
  void state;
  void summary;
  return value;
}

function writeFileTool(args: any, state: any) {
  const { path, root } = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation: 'fs_write_file' });
  assertFilesystemMutationTargetAllowed(path, root, 'fs_write_file');
  const content = stringField(args, 'content') ?? '';
  const overwrite = booleanField(args, 'overwrite') ?? true;
  const createOnly = booleanField(args, 'create_only') ?? false;
  const createParentDirectories = booleanField(args, 'create_parent_directories') ?? true;
  const before = existsSync(path) ? readFileSync(path, 'utf8') : null;
  if (before !== null && createOnly) throw diagnosticError('write_file_destination_exists', `write_file_destination_exists: ${path}`, pathMetadata(path, root));
  if (before !== null && !overwrite) throw diagnosticError('write_file_overwrite_refused', `write_file_overwrite_refused: ${path}`, pathMetadata(path, root));
  assertExpectedSha256(args, before, { operation: 'fs_write_file', path, root });
  const parent = dirname(path);
  if (!existsSync(parent) && !createParentDirectories) throw diagnosticError('write_file_parent_not_found', `write_file_parent_not_found: ${parent}`, { requested_path: path, parent: pathMetadata(parent, root) });
  if (!existsSync(parent)) mkdirSync(parent, { recursive: true });
  writeFileSync(path, content, 'utf8');
  appendAudit(state, 'fs_write_file', path, root, { size: content.length, create_parent_directories: createParentDirectories, before_sha256: before === null ? null : sha256(before), after_sha256: sha256(content) });
  return { schema: 'local.filesystem.write_file.v1', status: 'written', ...pathMetadata(path, root), size: content.length, create_parent_directories: createParentDirectories, before_sha256: before === null ? null : sha256(before), after_sha256: sha256(content) };
}

async function writeFileToolAsync(args: any, state: any, context: any) {
  const { path, root } = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation: 'fs_write_file' });
  assertFilesystemMutationTargetAllowed(path, root, 'fs_write_file');
  const content = stringField(args, 'content') ?? '';
  const overwrite = booleanField(args, 'overwrite') ?? true;
  const createOnly = booleanField(args, 'create_only') ?? false;
  const createParentDirectories = booleanField(args, 'create_parent_directories') ?? true;
  const timeoutMs = filesystemOperationTimeoutMs(args);
  const checkTimeout = createOperationTimeoutChecker('fs_write_file', timeoutMs);
  const abortController = new AbortController();
  const timeout = setTimeout(() => abortController.abort(), timeoutMs);
  const abortHandler = () => abortController.abort();
  context.abortSignal?.addEventListener('abort', abortHandler, { once: true });
  try {
    checkTimeout('read_existing_start');
    const before = existsSync(path) ? await readFileAsync(path, 'utf8') : null;
    checkTimeout('read_existing_complete');
    if (before !== null && createOnly) throw diagnosticError('write_file_destination_exists', `write_file_destination_exists: ${path}`, pathMetadata(path, root));
    if (before !== null && !overwrite) throw diagnosticError('write_file_overwrite_refused', `write_file_overwrite_refused: ${path}`, pathMetadata(path, root));
    assertExpectedSha256(args, before, { operation: 'fs_write_file', path, root });
    const parent = dirname(path);
    if (!existsSync(parent) && !createParentDirectories) throw diagnosticError('write_file_parent_not_found', `write_file_parent_not_found: ${parent}`, { requested_path: path, parent: pathMetadata(parent, root) });
    if (!existsSync(parent)) await mkdirAsync(parent, { recursive: true });
    const testDelayMs = Math.max(0, Number(state.env?.NARADA_LOCAL_FILESYSTEM_WRITE_DELAY_MS ?? 0));
    if (testDelayMs > 0) await delayWithSignal(testDelayMs, abortController.signal);
    checkTimeout('write_file_start');
    await writeFileAsync(path, content, { encoding: 'utf8', signal: abortController.signal });
    checkTimeout('write_file_complete');
    appendAudit(state, 'fs_write_file', path, root, { size: content.length, create_parent_directories: createParentDirectories, before_sha256: before === null ? null : sha256(before), after_sha256: sha256(content) });
    return { schema: 'local.filesystem.write_file.v1', status: 'written', ...pathMetadata(path, root), size: content.length, create_parent_directories: createParentDirectories, before_sha256: before === null ? null : sha256(before), after_sha256: sha256(content), timeout_ms: timeoutMs };
  } catch (error) {
    if (abortController.signal.aborted && !(error instanceof McpToolError)) {
      throw diagnosticError('fs_write_file_timed_out', 'fs_write_file_timed_out', {
        timeout_kind: 'filesystem_operation_timeout',
        operation: 'fs_write_file',
        path,
        root,
        timeout_ms: timeoutMs,
        cancelled: context.abortSignal?.aborted === true,
        remediation: [
          'Use payload_ref or payload_path for large writes so payload transfer is explicit and auditable.',
          'Split large writes into smaller files where possible.',
          'Retry only after fs_doctor confirms the surface is responsive.',
        ],
      });
    }
    throw error;
  } finally {
    clearTimeout(timeout);
    context.abortSignal?.removeEventListener('abort', abortHandler);
  }
}

function strReplaceTool(args: any, state: any) {
  const { path, root } = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation: 'fs_str_replace_file' });
  assertFilesystemMutationTargetAllowed(path, root, 'fs_str_replace_file');
  const oldText = stringField(args, 'old') ?? '';
  const newText = stringField(args, 'new') ?? '';
  if (!oldText) throw diagnosticError('str_replace_requires_old', 'str_replace_requires_old', pathMetadata(path, root));
  const before = readFileSync(path, 'utf8');
  assertExpectedSha256(args, before, { operation: 'fs_str_replace_file', path, root });
  const count = before.split(oldText).length - 1;
  if (count === 0) throw diagnosticError('str_replace_not_found', 'str_replace_not_found', buildStrReplaceNotFoundDetails({ path, root, before, oldText }));
  if (count > 1) {
    throw diagnosticError('str_replace_ambiguous', `str_replace_ambiguous: ${count}`, {
      ...pathMetadata(path, root),
      occurrences: count,
      matches: findTextOccurrences(before, oldText).slice(0, 20),
    });
  }
  const after = before.replace(oldText, newText);
  writeFileSync(path, after, 'utf8');
  appendAudit(state, 'fs_str_replace_file', path, root, { old_length: oldText.length, new_length: newText.length, before_sha256: sha256(before), after_sha256: sha256(after) });
  return {
    schema: 'local.filesystem.str_replace_file.v1',
    status: 'replaced',
    ...pathMetadata(path, root),
    occurrences: 1,
    before_sha256: sha256(before),
    after_sha256: sha256(after),
  };
}

function buildStrReplaceNotFoundDetails({ path, root, before, oldText }: any) {
  const normalizedBefore = normalizeNewlines(before);
  const normalizedOld = normalizeNewlines(oldText);
  const lineCandidates = findLineRangeCandidates(before, oldText).slice(0, 10);
  const likelyNewlineMismatch = normalizedOld.length > 0 && normalizedBefore.includes(normalizedOld);
  const likelyVisibleLineMismatch = !likelyNewlineMismatch && lineCandidates.length > 0;
  return {
    ...pathMetadata(path, root),
    old_length: oldText.length,
    old_sha256: sha256(oldText),
    likely_newline_or_context_mismatch: likelyNewlineMismatch || likelyVisibleLineMismatch,
    mismatch_reason: likelyNewlineMismatch
      ? 'normalized_newline_match_only'
      : likelyVisibleLineMismatch
        ? 'visible_line_match_without_exact_surrounding_context'
        : 'exact_text_not_found',
    candidate_line_ranges: lineCandidates,
    recommended_tool: 'fs_replace_range',
    recommended_args: lineCandidates.length === 1 ? {
      path,
      start_line: lineCandidates[0].start_line,
      end_line: lineCandidates[0].end_line,
      replacement: '<replacement text>',
    } : null,
    remediation: [
      'fs_str_replace_file is exact and preserves single-occurrence safety; it does not normalize line endings or guess missing surrounding context.',
      'Use fs_read_file_range to confirm the intended line window, then use fs_replace_range with start_line/end_line when the visible line is correct but exact string replacement fails.',
      'Include expected_sha256 on the write retry when acting from prior readback.',
    ],
  };
}

function normalizeNewlines(text: any) {
  return String(text ?? '').replace(/\r\n/g, '\n').replace(/\r/g, '\n');
}

function findLineRangeCandidates(content: any, oldText: any) {
  const normalizedNeedle = normalizeNewlines(oldText).trim();
  if (!normalizedNeedle) return [];
  const needleLines = normalizedNeedle.split('\n');
  const lines = normalizeNewlines(content).split('\n');
  const candidates = [];
  for (let index = 0; index <= lines.length - needleLines.length; index += 1) {
    const window = lines.slice(index, index + needleLines.length).join('\n').trim();
    if (window === normalizedNeedle || window.includes(normalizedNeedle)) {
      candidates.push({ start_line: index + 1, end_line: index + needleLines.length, preview: window.slice(0, 200) });
    }
  }
  return candidates;
}

function replaceRangeTool(args: any, state: any) {
  const startLine = integerField(args, 'start_line');
  const endLine = integerField(args, 'end_line');
  if (startLine === null || startLine < 1) throw diagnosticError('start_line_must_be_positive_integer', 'start_line_must_be_positive_integer', { start_line: startLine ?? null });
  if (endLine === null || endLine < startLine) throw diagnosticError('end_line_must_be_greater_than_or_equal_start_line', 'end_line_must_be_greater_than_or_equal_start_line', { start_line: startLine ?? null, end_line: endLine ?? null });
  const { path, root } = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation: 'fs_replace_range' });
  assertFilesystemMutationTargetAllowed(path, root, 'fs_replace_range');
  const replacement = stringField(args, 'replacement') ?? '';
  const before = readFileSync(path, 'utf8');
  assertExpectedSha256(args, before, { operation: 'fs_replace_range', path, root });
  const hasTrailingNewline = /\r?\n$/.test(before);
  const newline = before.includes('\r\n') ? '\r\n' : '\n';
  const lines = before.replace(/\r?\n$/, '').split(/\r?\n/);
  if (startLine > lines.length + 1) throw diagnosticError('start_line_out_of_range', `start_line_out_of_range: ${startLine}`, { ...pathMetadata(path, root), start_line: startLine, total_lines: lines.length });
  if (endLine > lines.length) throw diagnosticError('end_line_out_of_range', `end_line_out_of_range: ${endLine}`, { ...pathMetadata(path, root), end_line: endLine, total_lines: lines.length });
  const replacementLines = replacement.length === 0 ? [] : replacement.split(/\r?\n/);
  const afterLines = [...lines.slice(0, startLine - 1), ...replacementLines, ...lines.slice(endLine)];
  const after = `${afterLines.join(newline)}${hasTrailingNewline ? newline : ''}`;
  writeFileSync(path, after, 'utf8');
  appendAudit(state, 'fs_replace_range', path, root, { start_line: startLine, end_line: endLine, before_sha256: sha256(before), after_sha256: sha256(after) });
  return {
    schema: 'local.filesystem.replace_range.v1',
    status: 'replaced_range',
    ...pathMetadata(path, root),
    start_line: startLine,
    end_line: endLine,
    inserted_lines: replacementLines.length,
    before_sha256: sha256(before),
    after_sha256: sha256(after),
  };
}

function applyPatchTool(args: any, state: any) {
  const patch = stringField(args, 'patch');
  if (!patch) throw diagnosticError('patch_required', 'Patch text is required.');
  const dryRun = booleanField(args, 'dry_run') ?? false;
  const operationId = stringField(args, 'operation_id') ?? randomUUID();
  const patchSha256 = sha256(patch);
  const timeoutMs = filesystemOperationTimeoutMs(args);
  const priorOutcome = readPatchOutcome(state, operationId);
  let recoveryCount = 0;
  if (priorOutcome) {
    if (priorOutcome.patch_sha256 !== patchSha256) throw diagnosticError('patch_operation_id_conflict', 'patch_operation_id_conflict', { operation_id: operationId, existing_patch_sha256: priorOutcome.patch_sha256, requested_patch_sha256: patchSha256 });
    if (priorOutcome.status === 'interrupted_before_mutation' && priorOutcome.retry_safe === true) {
      recoveryCount = Number.isInteger(priorOutcome.recovery_count) ? priorOutcome.recovery_count + 1 : 1;
    } else {
      return { ...priorOutcome, operation_replayed: true, outcome_reader: { tool: 'fs_patch_outcome_show', operation_id: operationId } };
    }
  }
  const acceptedAt = new Date();
  const deadlineAt = new Date(acceptedAt.getTime() + timeoutMs).toISOString();
  writePatchOutcome(state, operationId, {
    schema: 'local.filesystem.apply_patch.outcome.v1',
    status: 'accepted',
    operation_id: operationId,
    patch_sha256: patchSha256,
    mutation_started: false,
    owner_pid: process.pid,
    timeout_ms: timeoutMs,
    deadline_at: deadlineAt,
    accepted_at: acceptedAt.toISOString(),
    recovery_count: recoveryCount,
  });
  const expectedSha256 = expectedSha256Map(args);
  const matchedExpectedSha256Keys = new Set();
  const checkTimeout = createOperationTimeoutChecker('fs_apply_patch', timeoutMs);
  checkTimeout('parse_patch_start');
  let files;
  try {
    files = parseToolPatch(patch, { diagnosticError: patchDiagnosticError, checkTimeout });
  } catch (error) {
    writePatchFailureBeforeMutation(state, operationId, patchSha256, error);
    throw error;
  }
  checkTimeout('parse_patch_complete');
  if (files.length === 0) {
    const error = patchDiagnosticError('patch_contains_no_files', patch, {
      expected_format: 'unified_diff_or_codex_apply_patch',
      expected_headers: ['--- <path>', '+++ <path>', '@@ -old,+new @@', '*** Begin Patch', '*** Update File: <path>'],
    });
    writePatchFailureBeforeMutation(state, operationId, patchSha256, error);
    throw error;
  }
  let planned;
  try {
    planned = files.map((filePatch: any) => {
      checkTimeout('plan_file_start');
      const source = resolvePatchSource(filePatch, state);
      const target = resolvePatchTarget(filePatch, state);
      if (!filePatch.deleteFile) assertFilesystemMutationTargetAllowed(target.path, target.root, 'fs_apply_patch');
      if (filePatch.oldPath !== '/dev/null' && !existsSync(source.path)) {
        throw diagnosticError('patch_source_not_found', `patch_source_not_found: ${source.path}`, {
          ...pathMetadata(source.path, source.root),
          expected_format_for_new_files: 'unified diff with --- /dev/null or Codex *** Add File',
        });
      }
      const before = existsSync(source.path) ? readFileSync(source.path, 'utf8') : '';
      const matchedKey = assertExpectedPatchSha256(expectedSha256, filePatch, source, target, before);
      if (matchedKey) matchedExpectedSha256Keys.add(matchedKey);
      checkTimeout('apply_file_patch_start');
      const patchContext = { diagnosticError, checkTimeout };
      const after = filePatch.deleteFile ? applyParsedDeletePatch(before, filePatch, patchContext) : applyParsedFilePatch(before, filePatch, patchContext);
      checkTimeout('apply_file_patch_complete');
      return { filePatch, source, target, before, after };
    });
  } catch (error) {
    writePatchFailureBeforeMutation(state, operationId, patchSha256, error);
    throw error;
  }
  checkTimeout('planned_all_files');
  try {
    assertAllExpectedPatchSha256KeysMatched(expectedSha256, matchedExpectedSha256Keys);
  } catch (error) {
    writePatchFailureBeforeMutation(state, operationId, patchSha256, error);
    throw error;
  }
  if (dryRun) {
    const outcome = {
      schema: 'local.filesystem.apply_patch.v1',
      status: 'checked',
      operation_id: operationId,
      dry_run: true,
      timeout_ms: timeoutMs,
      recovery_count: recoveryCount,
      changed_files: planned.map((item: any) => ({
        ...pathMetadata(item.target.path, item.target.root),
        operation: patchOperation(item.filePatch, item.source, item.target),
        hunks: item.filePatch.hunks.length,
        deleted: item.filePatch.deleteFile === true,
        before_sha256: sha256(item.before),
        after_sha256: item.filePatch.deleteFile ? null : sha256(item.after),
      })),
    };
    writePatchOutcome(state, operationId, { ...outcome, patch_sha256: patchSha256, mutation_started: false, finished_at: new Date().toISOString() });
    return outcome;
  }
  const changed = [];
  const backupPaths = uniquePaths(planned.flatMap((item: any) => [item.source.path, item.target.path]));
  const backups = backupPaths.map((path: any) => ({
    path,
    existed: existsSync(path),
    content: existsSync(path) ? readFileSync(path, 'utf8') : null,
  }));
  const recoveryPlan = buildPatchRecoveryPlan(planned, backups);
  writePatchOutcome(state, operationId, {
    schema: 'local.filesystem.apply_patch.outcome.v1',
    status: 'applying',
    operation_id: operationId,
    patch_sha256: patchSha256,
    mutation_started: true,
    owner_pid: process.pid,
    timeout_ms: timeoutMs,
    deadline_at: deadlineAt,
    started_at: new Date().toISOString(),
    recovery_count: recoveryCount,
    recovery_plan: recoveryPlan,
  });
  try {
    for (const item of planned) {
      checkTimeout('write_file_start');
      if (item.filePatch.deleteFile) {
        if (!existsSync(item.source.path)) throw diagnosticError('patch_delete_target_not_found', `patch_delete_target_not_found: ${item.source.path}`, pathMetadata(item.source.path, item.source.root));
        rmSync(item.source.path, { force: false });
      } else {
        mkdirSync(dirname(item.target.path), { recursive: true });
        writeFileSync(item.target.path, item.after ?? '', 'utf8');
        if (!samePath(item.source.path, item.target.path) && existsSync(item.source.path)) rmSync(item.source.path, { force: false });
      }
      const afterSha256 = item.filePatch.deleteFile ? null : sha256(item.after);
      appendAudit(state, 'fs_apply_patch', item.target.path, item.target.root, { patch_sha256: sha256(patch), before_sha256: sha256(item.before), after_sha256: afterSha256, hunks: item.filePatch.hunks.length });
      changed.push({ ...pathMetadata(item.target.path, item.target.root), operation: patchOperation(item.filePatch, item.source, item.target), hunks: item.filePatch.hunks.length, deleted: item.filePatch.deleteFile === true, before_sha256: sha256(item.before), after_sha256: afterSha256 });
      checkTimeout('write_file_complete');
    }
  } catch (error) {
    rollbackPatch(backups);
    const diagnostic = errorDiagnostic(error);
    writePatchOutcome(state, operationId, { schema: 'local.filesystem.apply_patch.outcome.v1', status: 'failed_rolled_back', operation_id: operationId, patch_sha256: patchSha256, mutation_started: true, rollback_performed: true, rollback_succeeded: true, recovery_count: recoveryCount, finished_at: new Date().toISOString(), error: diagnostic });
    throw error;
  }
  const outcome = { schema: 'local.filesystem.apply_patch.outcome.v1', status: 'patched', operation_id: operationId, patch_sha256: patchSha256, mutation_started: true, rollback_performed: false, recovery_count: recoveryCount, finished_at: new Date().toISOString(), changed_files: changed };
  writePatchOutcome(state, operationId, outcome);
  return { schema: 'local.filesystem.apply_patch.v1', status: 'patched', operation_id: operationId, changed_files: changed, recovery_count: recoveryCount, timeout_ms: timeoutMs, outcome_reader: { tool: 'fs_patch_outcome_show', operation_id: operationId } };
}

function patchOutcomeShowTool(args: any, state: any) {
  const operationId = stringField(args, 'operation_id');
  if (!operationId || !/^[A-Za-z0-9._-]{1,160}$/.test(operationId)) throw diagnosticError('patch_operation_id_required', 'patch_operation_id_required');
  const outcome = readPatchOutcome(state, operationId);
  if (!outcome) throw diagnosticError('patch_outcome_not_found', 'patch_outcome_not_found', { operation_id: operationId });
  return outcome;
}

function readPatchOutcome(state: any, operationId: any) {
  const path = patchOutcomePath(state, operationId);
  if (!existsSync(path)) return null;
  return reconcilePatchOutcome(state, JSON.parse(readFileSync(path, 'utf8')));
}

function reconcilePatchOutcome(state: any, outcome: any) {
  if (!outcome || !['accepted', 'applying'].includes(outcome.status)) return outcome;
  const ownerAlive = patchOwnerIsAlive(outcome.owner_pid);
  const deadlineExceeded = typeof outcome.deadline_at === 'string' && Date.now() >= Date.parse(outcome.deadline_at);
  if (ownerAlive) {
    return {
      ...outcome,
      recovery: {
        status: deadlineExceeded ? 'deadline_exceeded_owner_alive' : 'owner_active',
        terminal: false,
        retry_safe: false,
        remediation: deadlineExceeded
          ? 'Restart the owning MCP surface, then call fs_patch_outcome_show again.'
          : 'Wait for the owning MCP surface to finish, then call fs_patch_outcome_show again.',
      },
    };
  }

  if (outcome.status === 'accepted') {
    return writeRecoveredPatchOutcome(state, outcome, {
      status: 'interrupted_before_mutation',
      mutation_effect_present: false,
      retry_safe: true,
      reason: 'owner_exited_before_mutation_started',
    });
  }

  const plan = outcome.recovery_plan;
  if (!plan || !Array.isArray(plan.before_state) || plan.before_state.length === 0 || !Array.isArray(plan.after_state) || plan.after_state.length === 0) {
    return writeRecoveredPatchOutcome(state, outcome, {
      status: 'interrupted_unknown',
      mutation_effect_present: null,
      retry_safe: false,
      reason: 'recovery_plan_missing',
    });
  }

  const afterMatches = patchFilesystemStateMatches(state, plan.after_state);
  const beforeMatches = patchFilesystemStateMatches(state, plan.before_state);
  if (afterMatches) {
    return writeRecoveredPatchOutcome(state, outcome, {
      status: 'patched_recovered',
      mutation_effect_present: true,
      retry_safe: false,
      changed_files: Array.isArray(plan.changed_files) ? plan.changed_files : [],
      reason: 'filesystem_matches_planned_after_state',
    });
  }
  if (beforeMatches) {
    return writeRecoveredPatchOutcome(state, outcome, {
      status: 'interrupted_before_mutation',
      mutation_effect_present: false,
      retry_safe: true,
      reason: 'filesystem_matches_captured_before_state',
    });
  }
  return writeRecoveredPatchOutcome(state, outcome, {
    status: 'interrupted_partial',
    mutation_effect_present: true,
    retry_safe: false,
    reason: 'filesystem_matches_neither_captured_state',
  });
}

function writeRecoveredPatchOutcome(state: any, outcome: any, recovery: any) {
  const terminal = {
    ...outcome,
    status: recovery.status,
    finished_at: new Date().toISOString(),
    recovered_at: new Date().toISOString(),
    mutation_effect_present: recovery.mutation_effect_present,
    retry_safe: recovery.retry_safe,
    changed_files: recovery.changed_files ?? outcome.changed_files,
    recovery: {
      status: recovery.status,
      terminal: true,
      retry_safe: recovery.retry_safe,
      reason: recovery.reason,
      remediation: recovery.retry_safe
        ? 'Retry fs_apply_patch with the same operation_id and identical patch.'
        : recovery.status === 'patched_recovered'
          ? 'Treat the operation as complete; do not retry it.'
          : 'Inspect the affected files and use a new operation_id only after manual reconciliation.',
    },
  };
  writePatchOutcome(state, outcome.operation_id, terminal);
  return terminal;
}

function patchOwnerIsAlive(ownerPid: any) {
  if (!Number.isInteger(ownerPid) || ownerPid <= 0) return false;
  try {
    process.kill(ownerPid, 0);
    return true;
  } catch {
    return false;
  }
}

function patchFilesystemStateMatches(state: any, entries: any) {
  return entries.every((entry: any) => {
    if (!entry || typeof entry.path !== 'string' || typeof entry.exists !== 'boolean') return false;
    try {
      const resolved = resolveAllowedToolPath(entry.path, state.allowedRoots, { operation: 'fs_patch_outcome_show', field: 'recovery_plan.path' });
      const exists = existsSync(resolved.path);
      if (exists !== entry.exists) return false;
      if (!exists) return true;
      if (typeof entry.sha256 !== 'string') return false;
      return sha256(readFileSync(resolved.path, 'utf8')) === entry.sha256;
    } catch {
      return false;
    }
  });
}

function buildPatchRecoveryPlan(planned: any, backups: any) {
  const beforeState = backups.map((backup: any) => ({
    path: backup.path,
    exists: backup.existed,
    sha256: backup.existed ? sha256(backup.content) : null,
  }));
  const afterState = beforeState.map((entry: any) => ({ ...entry }));
  const setAfterState = (entry: any) => {
    const existingIndex = afterState.findIndex((candidate: any) => samePath(candidate.path, entry.path));
    if (existingIndex >= 0) afterState[existingIndex] = entry;
    else afterState.push(entry);
  };
  const changedFiles = [];
  for (const item of planned) {
    setAfterState({
      path: item.target.path,
      exists: item.filePatch.deleteFile !== true,
      sha256: item.filePatch.deleteFile ? null : sha256(item.after),
    });
    if (!samePath(item.source.path, item.target.path)) {
      setAfterState({ path: item.source.path, exists: false, sha256: null });
    }
    changedFiles.push({
      ...pathMetadata(item.target.path, item.target.root),
      operation: patchOperation(item.filePatch, item.source, item.target),
      hunks: item.filePatch.hunks.length,
      deleted: item.filePatch.deleteFile === true,
      before_sha256: sha256(item.before),
      after_sha256: item.filePatch.deleteFile ? null : sha256(item.after),
    });
  }
  return { before_state: beforeState, after_state: afterState, changed_files: changedFiles };
}

function writePatchFailureBeforeMutation(state: any, operationId: any, patchSha256: any, error: any) {
  writePatchOutcome(state, operationId, { schema: 'local.filesystem.apply_patch.outcome.v1', status: 'failed_before_mutation', operation_id: operationId, patch_sha256: patchSha256, mutation_started: false, rollback_performed: false, finished_at: new Date().toISOString(), error: errorDiagnostic(error) });
}

function writePatchOutcome(state: any, operationId: any, outcome: any) {
  const path = patchOutcomePath(state, operationId);
  mkdirSync(dirname(path), { recursive: true });
  const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
  writeFileSync(temporary, `${JSON.stringify(outcome, null, 2)}\n`, 'utf8');
  renameSync(temporary, path);
}
function patchOutcomePath(state: any, operationId: any) {
  if (!/^[A-Za-z0-9._-]{1,160}$/.test(operationId)) throw diagnosticError('patch_operation_id_invalid', 'patch_operation_id_invalid');
  return join(state.outputRoot, '.narada', 'local-filesystem-mcp', 'patch-outcomes', `${operationId}.json`);
}

function movePathTool(args: any, state: any) {
  const from = resolveAllowedToolPath(stringField(args, 'from'), state.allowedRoots, { operation: 'fs_move_path', field: 'from' });
  const to = resolveAllowedToolPath(stringField(args, 'to'), state.allowedRoots, { operation: 'fs_move_path', field: 'to' });
  assertFilesystemMutationTargetAllowed(to.path, to.root, 'fs_move_path');
  const overwrite = booleanField(args, 'overwrite') ?? false;
  return movePath({ state, operation: 'fs_move_path', args, from, to, overwrite, directoryOnly: false });
}

function createDirectoryTool(args: any, state: any) {
  const target = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation: 'fs_create_directory' });
  const recursive = booleanField(args, 'recursive') ?? false;
  if (existsSync(target.path)) {
    if (!statSync(target.path).isDirectory()) throw diagnosticError('create_directory_destination_not_directory', `create_directory_destination_not_directory: ${target.path}`, pathMetadata(target.path, target.root));
    appendAudit(state, 'fs_create_directory', target.path, target.root, { recursive, created: false });
    return {
      schema: 'local.filesystem.create_directory.v1',
      status: 'exists',
      ...pathMetadata(target.path, target.root),
      recursive,
      created: false,
    };
  }
  const parent = dirname(target.path);
  if (!recursive && !existsSync(parent)) {
    throw diagnosticError('create_directory_parent_not_found', `create_directory_parent_not_found: ${parent}`, {
      operation: 'fs_create_directory',
      requested_path: target.path,
      parent: pathMetadata(parent, target.root),
    });
  }
  mkdirSync(target.path, { recursive });
  appendAudit(state, 'fs_create_directory', target.path, target.root, { recursive });
  return {
    schema: 'local.filesystem.create_directory.v1',
    status: 'created',
    ...pathMetadata(target.path, target.root),
    recursive,
    created: true,
  };
}

function renameDirectoryTool(args: any, state: any) {
  const from = resolveAllowedToolPath(stringField(args, 'from'), state.allowedRoots, { operation: 'fs_rename_directory', field: 'from' });
  const to = resolveAllowedToolPath(stringField(args, 'to'), state.allowedRoots, { operation: 'fs_rename_directory', field: 'to' });
  return movePath({ state, operation: 'fs_rename_directory', args, from, to, overwrite: false, directoryOnly: true });
}

function deleteDirectoryTool(args: any, state: any) {
  const target = resolveAllowedToolPath(stringField(args, 'path'), state.allowedRoots, { operation: 'fs_delete_directory' });
  const recursive = booleanField(args, 'recursive') ?? false;
  if (!existsSync(target.path)) throw diagnosticError('delete_directory_not_found', `delete_directory_not_found: ${target.path}`, pathMetadata(target.path, target.root));
  const targetStat = statSync(target.path);
  if (!targetStat.isDirectory()) throw diagnosticError('delete_directory_target_not_directory', `delete_directory_target_not_directory: ${target.path}`, pathMetadata(target.path, target.root));
  assertExpectedMetadata(args, target, { operation: 'fs_delete_directory', objectKey: 'expected', mtimeKey: 'expected_mtime', sizeKey: 'expected_size', treeShaKey: 'expected_tree_sha256', entryCountKey: 'expected_entry_count' });
  const entryCount = readdirSync(target.path).length;
  if (entryCount > 0 && !recursive) throw diagnosticError('delete_directory_not_empty', `delete_directory_not_empty: ${target.path}`, { ...pathMetadata(target.path, target.root), entry_count: entryCount });
  rmSync(target.path, { recursive, force: false });
  appendAudit(state, 'fs_delete_directory', target.path, target.root, { recursive, entry_count: entryCount });
  return {
    schema: 'local.filesystem.delete_directory.v1',
    status: 'deleted',
    ...pathMetadata(target.path, target.root),
    recursive,
  };
}

function movePath({ state, operation, args, from, to, overwrite, directoryOnly }: any) {
  if (samePath(from.path, to.path)) throw diagnosticError('move_source_and_destination_same', `move_source_and_destination_same: ${from.path}`, { operation, from: pathMetadata(from.path, from.root), to: pathMetadata(to.path, to.root) });
  if (!existsSync(from.path)) throw diagnosticError('move_source_not_found', `move_source_not_found: ${from.path}`, { operation, ...pathMetadata(from.path, from.root) });
  const fromStat = statSync(from.path);
  if (directoryOnly && !fromStat.isDirectory()) throw diagnosticError('rename_directory_source_not_directory', `rename_directory_source_not_directory: ${from.path}`, pathMetadata(from.path, from.root));
  assertExpectedMetadata(args, from, { operation, objectKey: 'expected_from', mtimeKey: 'expected_from_mtime', sizeKey: 'expected_from_size', shaKey: 'expected_from_sha256', treeShaKey: 'expected_from_tree_sha256', entryCountKey: 'expected_from_entry_count' });
  if (fromStat.isDirectory() && isPathInside(to.path, from.path)) throw diagnosticError('move_destination_inside_source', `move_destination_inside_source: ${to.path}`, { operation, from: pathMetadata(from.path, from.root), to: pathMetadata(to.path, to.root) });
  if (existsSync(to.path)) {
    if (!overwrite) throw diagnosticError('move_destination_exists', `move_destination_exists: ${to.path}`, { operation, ...pathMetadata(to.path, to.root) });
    const toStat = statSync(to.path);
    if (fromStat.isDirectory() !== toStat.isDirectory()) throw diagnosticError('move_destination_type_mismatch', `move_destination_type_mismatch: ${to.path}`, { operation, ...pathMetadata(to.path, to.root) });
    assertExpectedMetadata(args, to, { operation, objectKey: 'expected_to', mtimeKey: 'expected_to_mtime', sizeKey: 'expected_to_size', treeShaKey: 'expected_to_tree_sha256', entryCountKey: 'expected_to_entry_count' });
    const backupPath = uniqueSiblingPath(to.path, 'overwrite-backup');
    renameSync(to.path, backupPath);
    try {
      mkdirSync(dirname(to.path), { recursive: true });
      renameSync(from.path, to.path);
      rmSync(backupPath, { recursive: true, force: true });
    } catch (error) {
      if (!existsSync(to.path) && existsSync(backupPath)) renameSync(backupPath, to.path);
      throw error;
    }
  } else {
    mkdirSync(dirname(to.path), { recursive: true });
    renameSync(from.path, to.path);
  }
  appendAudit(state, operation, to.path, to.root, {
    from: from.path,
    from_root: from.root,
    to: to.path,
    to_root: to.root,
    overwrite,
  });
  return {
    schema: operation === 'fs_rename_directory' ? 'local.filesystem.rename_directory.v1' : 'local.filesystem.move_path.v1',
    status: 'moved',
    from: pathMetadata(from.path, from.root),
    to: pathMetadata(to.path, to.root),
    overwrite,
  };
}

function pathMetadata(path: any, root: any) {
  return {
    path,
    root,
    relative_path: relative(root, path).replace(/\\/g, '/'),
  };
}

function readTextLineWindow({ path, root, offset, limit, timeoutMs, operation }: any) {
  const fd = openSync(path, 'r');
  const decoder = new StringDecoder('utf8');
  const buffer = Buffer.allocUnsafe(READ_BUFFER_BYTES);
  const selected = [];
  const hash = createHash('sha256');
  let pending = '';
  let lineNumber = 0;
  let reachedEof = false;
  let nextOffset = null;
  let windowBounded = false;
  const checkTimeout = createReadTimeoutChecker(operation, timeoutMs, { path, root, offset, limit });
  try {
    while (true) {
      checkTimeout();
      const bytesRead = readSync(fd, buffer, 0, buffer.length, null);
      checkTimeout();
      if (bytesRead === 0) {
        reachedEof = true;
        break;
      }
      const chunk = buffer.subarray(0, bytesRead);
      if (chunk.includes(0)) throw diagnosticError('binary_file_not_supported', `binary_file_not_supported: ${path}`, pathMetadata(path, root));
      hash.update(chunk);
      if (windowBounded) continue;
      pending += decoder.write(chunk);
      const lines = pending.split(/\r?\n/);
      pending = lines.pop() ?? '';
      for (const line of lines) {
        lineNumber += 1;
        if (lineNumber >= offset && selected.length < limit) selected.push(line);
        else if (lineNumber >= offset + limit) {
          nextOffset = lineNumber;
          windowBounded = true;
          pending = '';
          break;
        }
      }
    }
    if (!windowBounded) pending += decoder.end();
    if (!windowBounded && pending.length > 0) {
      lineNumber += 1;
      if (lineNumber >= offset && selected.length < limit) selected.push(pending);
      else if (lineNumber >= offset + limit) nextOffset = lineNumber;
    }
    return {
      selected,
      nextOffset,
      totalLines: windowBounded ? null : lineNumber,
      totalLinesExact: reachedEof && !windowBounded,
      contentSha256: hash.digest('hex'),
    };
  } finally {
    closeSync(fd);
  }
}

async function callToolAsync(params: any, state: any, context: any) {
  const record = asRecord(params);
  const name = stringField(record, 'name');
  const args = asRecord(record.arguments);
  activeToolName = name;
  if (!name) throw diagnosticError('tools_call_requires_name', 'tools_call_requires_name');
  if (!listTools(state.mode).some((tool: any) => tool.name === name)) throw diagnosticError(`tool_not_available_in_${state.mode}_mode`, `tool_not_available_in_${state.mode}_mode: ${name}`, { tool_name: name, mode: state.mode });
  switch (name) {
    case 'fs_read_file': return await callReadToolWithRequestDeadline(name, args, state, context);
    case 'fs_read_file_range': return await callReadToolWithRequestDeadline(name, args, state, context);
    case 'fs_glob_search': return toolResult(await globSearchToolAsync(args, state, context));
    case 'fs_repository_inventory': return toolResult(await repositoryInventoryToolAsync(args, state, context));
    case 'fs_file_metrics': return toolResult(await fileMetricsToolAsync(args, state, context));
    case 'fs_grep_search': return toolResult(await grepSearchToolAsync(args, state, context), { grepOutputMode: stringField(args, 'output_mode') ?? 'files_with_matches' });
    case 'fs_write_file': {
      const payload = resolveFilesystemPayloadArgs(name, args, state);
      return toolResult(attachPayloadSource(await writeFileToolAsync(payload.args, state, context), payload.payloadSource));
    }
    default: return callTool(params, state);
  }
}

function searchTimeoutMs(args: any) {
  const value = integerField(args, 'timeout_ms');
  if (value === null) return undefined;
  return Math.min(300_000, Math.max(1, value));
}

function readOperationTimeoutMs(args: any) {
  const value = integerField(args, 'timeout_ms');
  if (value === null) return DEFAULT_READ_OPERATION_TIMEOUT_MS;
  return Math.min(60_000, Math.max(1, value));
}

function readHandlerDelayMs(state: any) {
  const value = Number(asRecord(state.env).NARADA_LOCAL_FILESYSTEM_READ_HANDLER_DELAY_MS ?? 0);
  return Number.isFinite(value) ? Math.max(0, Math.trunc(value)) : 0;
}

function filesystemOperationTimeoutMs(args: any) {
  const value = integerField(args, 'timeout_ms');
  if (value === null) return DEFAULT_FILESYSTEM_OPERATION_TIMEOUT_MS;
  return Math.min(300_000, Math.max(1, value));
}

function createReadTimeoutChecker(operation: any, timeoutMs: any, { path, root, offset, limit }: any) {
  const startedAt = Date.now();
  return () => {
    const elapsedMs = Date.now() - startedAt;
    if (elapsedMs <= timeoutMs) return;
    throw readTimeoutError(operation, timeoutMs, { path, root, offset, limit, elapsedMs });
  };
}

function readTimeoutError(operation: any, timeoutMs: any, { path, root, offset, limit, elapsedMs }: any) {
  return diagnosticError(`${operation}_timed_out`, `${operation}_timed_out`, {
    timeout_kind: 'read_timeout',
    timeout_ms: timeoutMs,
    elapsed_ms: elapsedMs,
    ...pathMetadata(path, root),
    offset,
    limit,
    recommended_tool: 'fs_read_file_range',
    recommended_args: {
      path,
      start_line: offset,
      end_line: Math.max(offset, offset + Math.min(limit, 100) - 1),
      timeout_ms: Math.min(60_000, Math.max(timeoutMs * 2, DEFAULT_READ_OPERATION_TIMEOUT_MS)),
    },
    remediation: [
      'Retry with fs_read_file_range and a narrower line window.',
      'Use fs_grep_search or fs_stat first when the file may be on a slow or virtualized filesystem.',
      'Increase timeout_ms only after narrowing the requested line window.',
    ],
  });
}

function createOperationTimeoutChecker(operation: any, timeoutMs: any) {
  const startedAt = Date.now();
  return (phase: any = null) => {
    const elapsedMs = Date.now() - startedAt;
    if (elapsedMs <= timeoutMs) return;
    throw diagnosticError(`${operation}_timed_out`, `${operation}_timed_out`, {
      timeout_kind: 'filesystem_operation_timeout',
      operation,
      phase,
      timeout_ms: timeoutMs,
      elapsed_ms: elapsedMs,
      remediation: [
        'Split the patch into smaller files or hunks and retry.',
        'Inspect patch context for very large repeated blocks that can make hunk matching expensive.',
        'Increase timeout_ms only after narrowing the patch scope.',
      ],
    });
  };
}

function delayWithSignal(ms: any, signal: any) {
  return new Promise((resolvePromise: any, rejectPromise: any) => {
    if (signal.aborted) {
      rejectPromise(Object.assign(new Error('aborted'), { name: 'AbortError' }));
      return;
    }
    const timeout = setTimeout(() => {
      signal.removeEventListener('abort', abortHandler);
      resolvePromise(undefined);
    }, ms);
    const abortHandler = () => {
      clearTimeout(timeout);
      rejectPromise(Object.assign(new Error('aborted'), { name: 'AbortError' }));
    };
    signal.addEventListener('abort', abortHandler, { once: true });
  });
}

function searchCachePolicy(args: any) {
  const value = stringField(args, 'cache_policy') ?? 'auto';
  if (!['auto', 'snapshot', 'refresh', 'bypass'].includes(value)) throw diagnosticError('search_cache_policy_unsupported', `search_cache_policy_unsupported: ${value}`, { cache_policy: value });
  return value;
}

function searchFreshness(path: any) {
  const stat = statSync(path);
  const type = stat.isDirectory() ? 'directory' : stat.isFile() ? 'file' : 'other';
  const directoryFingerprint = type === 'directory' ? directoryTreeFingerprint(path, path) : null;
  return {
    path,
    type,
    size: stat.size,
    mtime: stat.mtime.toISOString(),
    mtime_ms: Math.trunc(stat.mtimeMs),
    ...(type === 'file' ? { sha256: createHash('sha256').update(readFileSync(path)).digest('hex') } : {}),
    ...(directoryFingerprint ? { entry_count: directoryFingerprint.entry_count, tree_entry_count: directoryFingerprint.tree_entry_count, tree_truncated: directoryFingerprint.tree_truncated, tree_sha256: directoryFingerprint.tree_sha256 } : {}),
  };
}

function directoryTreeFingerprint(path: any, root: any, { maxEntries = 5000 }: any = {}) {
  const entries: string[] = [];
  let treeTruncated = false;
  walkDirectoryFingerprint(path);
  return {
    entry_count: safeReaddir(path).length,
    tree_entry_count: entries.length,
    tree_truncated: treeTruncated,
    tree_sha256: createHash('sha256').update(entries.join('\n')).digest('hex'),
  };

  function walkDirectoryFingerprint(currentPath: any) {
    if (entries.length >= maxEntries) {
      treeTruncated = true;
      return;
    }
    const children = safeReaddir(currentPath).sort((left: any, right: any) => left.name.localeCompare(right.name));
    for (const child of children) {
      if (entries.length >= maxEntries) {
        treeTruncated = true;
        return;
      }
      const childPath = resolve(currentPath, child.name);
      let stat;
      try {
        stat = statSync(childPath);
      } catch {
        continue;
      }
      const relativePath = relative(root, childPath).replace(/\\/g, '/');
      const type = stat.isDirectory() ? 'directory' : stat.isFile() ? 'file' : 'other';
      entries.push(`${relativePath}\t${type}\t${stat.size}\t${Math.trunc(stat.mtimeMs)}`);
      if (stat.isDirectory()) walkDirectoryFingerprint(childPath);
    }
  }
}

function safeReaddir(path: any) {
  try {
    return readdirSync(path, { withFileTypes: true });
  } catch {
    return [];
  }
}

function renderGrepMatch(match: any, mode: any) {
  const parsed = buildGrepMatchObject(match, mode);
  if (mode === 'count_matches') return `${parsed.path}: ${parsed.count ?? 'unknown'}`;
  if (mode === 'content') return `${parsed.path}:${parsed.line ?? '?'}:${parsed.text ?? ''}`;
  return String(parsed.path ?? match);
}

function resolvePatchSource(filePatch: any, state: any) {
  const patchPath = stripPatchPrefix(filePatch.oldPath === '/dev/null' ? filePatch.newPath : filePatch.oldPath);
  return resolveAllowedToolPath(patchPath, state.allowedRoots, { operation: 'fs_apply_patch', field: 'patch_source_path' });
}

function resolvePatchTarget(filePatch: any, state: any) {
  const patchPath = stripPatchPrefix(filePatch.newPath === '/dev/null' ? filePatch.oldPath : filePatch.newPath);
  return resolveAllowedToolPath(patchPath, state.allowedRoots, { operation: 'fs_apply_patch', field: 'patch_path' });
}

function resolveAllowedToolPath(inputPath: any, allowedRoots: any, context: Record<string, unknown> = {}) {
  try {
    return resolvePolicyAllowedPath(inputPath, allowedRoots);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const codeName = message.split(/[:\s]/)[0] || 'path_resolution_failed';
    if (codeName === 'path_required' || codeName === 'path_outside_allowed_roots' || codeName === 'allowed_root_not_found') {
      const roots = Array.isArray(allowedRoots) && allowedRoots.length > 0 && typeof allowedRoots[0] === 'object'
        ? allowedRoots.map((entry: any) => entry.root)
        : allowedRoots;
      const relativeBase = Array.isArray(roots) && roots.length > 0 ? roots[0] : null;
      throw diagnosticError(codeName, message, {
        ...context,
        requested_path: inputPath ?? null,
        active_resolution_base: relativeBase,
        resolution_rule: 'first_allowed_root_for_relative_paths',
        relative_path_resolution: {
          base: relativeBase,
          rule: 'first_allowed_root',
          process_current_directory_used: false,
        },
        remediation: 'Pass an absolute path under an allowed root, or inspect fs_doctor for relative_path_resolution and allowed_roots before retrying relative paths.',
        allowed_roots: roots,
      });
    }
    throw error;
  }
}

function assertFilesystemMutationTargetAllowed(path: any, root: any, operation: any) {
  const normalized = String(path).replaceAll('\\', '/');
  const extension = extname(normalized).toLowerCase();
  if (!TRANSIENT_EXECUTABLE_PATH.test(normalized) || !TRANSIENT_EXECUTABLE_EXTENSIONS.has(extension)) return;
  throw diagnosticError('transient_executable_write_disallowed', 'transient_executable_write_disallowed', {
    operation,
    ...pathMetadata(path, root),
    refusal_reason: `transient_executable_write_disallowed:${path}`,
    remediation: 'Do not create or edit executable wrappers/scripts under .ai/tmp or .ai/temp. Use structured_command_start or the owning MCP surface directly and preserve its execution_ref as evidence.',
  });
}

function siteControlRoot(siteRoot: any) {
  const root = resolve(siteRoot);
  return basename(root).toLowerCase() === '.narada' ? root : resolve(root, '.narada');
}

function loadSiteExtraAllowedRoots(siteRoot: any) {
  try {
    const configPath = join(siteControlRoot(siteRoot), 'allowed-roots.json');
    if (!existsSync(configPath)) return [];
    const data = JSON.parse(readFileSync(configPath, 'utf8'));
    return [
      ...stringList(data.extra_allowed_roots),
      ...stringList(data.temp_allowed_roots),
    ];
  } catch {
    // Best-effort.
  }
  return [];
}

function loadSiteSecrets(siteRoot: any, targetEnv: any) {
  try {
    const configPath = join(siteControlRoot(siteRoot), 'secrets.json');
    if (!existsSync(configPath)) return;
    const data = JSON.parse(readFileSync(configPath, 'utf8'));
    const secretEnv = data.env;
    if (secretEnv && typeof secretEnv === 'object' && !Array.isArray(secretEnv)) {
      for (const [key, value] of Object.entries(secretEnv)) {
        if (typeof value === 'string' && value.trim() && !targetEnv[key]) {
          targetEnv[key] = value;
        }
      }
    }
  } catch {
    // Best-effort.
  }
}

function stripPatchPrefix(path: any) {
  const cleaned = normalizePatchPath(String(path ?? '').trim());
  if (cleaned.startsWith('a/') || cleaned.startsWith('b/')) return cleaned.slice(2);
  return cleaned;
}

function normalizePatchPath(path: any) {
  if (/^[A-Za-z]:\//.test(path)) return path;
  if (/^[A-Za-z]:\\/.test(path)) return path.replace(/\\/g, '/');
  return path.replace(/\\/g, '/');
}

function rollbackPatch(backups: any) {
  for (const backup of backups) {
    if (backup.existed) {
      mkdirSync(dirname(backup.path), { recursive: true });
      writeFileSync(backup.path, backup.content, 'utf8');
    } else if (existsSync(backup.path)) {
      rmSync(backup.path, { recursive: true, force: true });
    }
  }
}

function uniquePaths(paths: any) {
  const seen = new Set();
  const out = [];
  for (const path of paths) {
    const key = resolve(path).toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(path);
  }
  return out;
}

function appendAudit(state: any, operation: any, path: any, root: any, detail: any) {
  if (!state.auditLogDir) return;
  mkdirSync(state.auditLogDir, { recursive: true });
  appendFileSync(resolve(state.auditLogDir, 'filesystem-mcp-audit.jsonl'), `${JSON.stringify({
    schema: 'local.filesystem.audit.v1',
    at: new Date().toISOString(),
    operation,
    path,
    root,
    relative_path: relative(root, path).replace(/\\/g, '/'),
    detail,
  })}\n`, 'utf8');
}

function toolResult(value: any, renderContext: Record<string, unknown> = {}) {
  if (isToolResult(value)) {
    const structuredContent = value.structuredContent ?? parseToolResultStructuredContent(value);
    return {
      ...value,
      content: [assistantTextContent(renderFilesystemToolResultText(structuredContent, renderContext))],
      structuredContent: withoutDuplicatedReadContent(structuredContent),
    };
  }
  return {
    content: [assistantTextContent(renderFilesystemToolResultText(value, renderContext))],
    structuredContent: withoutDuplicatedReadContent(value),
  };
}

function withoutDuplicatedReadContent(value: any) {
  if (value?.schema !== 'local.filesystem.read.v1' || typeof value.content !== 'string') return value;
  const { content: _content, ...structuredContent } = value;
  return {
    ...structuredContent,
    content_delivery: {
      channel: 'content',
      block_index: 0,
      format: 'filesystem_read_text',
      duplicated_in_structured_content: false,
    },
  };
}

function assistantTextContent(text: string) {
  return { type: 'text', text, annotations: { audience: ['assistant'] } };
}

function isToolResult(value: any) {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value) && Array.isArray(value.content));
}

function parseToolResultStructuredContent(value: any) {
  const text = value.content.find((item: any) => item?.type === 'text' && typeof item.text === 'string')?.text;
  if (!text) return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

function samePath(left: any, right: any) {
  return resolve(left).toLowerCase() === resolve(right).toLowerCase();
}

function uniqueSiblingPath(path: any, label: any) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const candidate = `${path}.mcp-${label}-${process.pid}-${Date.now()}-${attempt}`;
    if (!existsSync(candidate)) return candidate;
  }
  throw diagnosticError('temporary_path_unavailable', `temporary_path_unavailable: ${path}`, { path, label });
}

function isPathInside(path: any, root: any) {
  const rel = relative(root, path);
  return rel !== '' && !rel.startsWith('..') && !isAbsolute(rel);
}

function isPathWithinOrEqual(path: any, root: any) {
  const rel = relative(root, path);
  return rel === '' || (!rel.startsWith('..') && !isAbsolute(rel));
}

function objectSchema(properties: any, required: any = []) {
  return { type: 'object', properties, required, additionalProperties: false };
}

function decorateTools(tools: any) {
  return tools.map((tool: any) => ({
    ...tool,
    canonical_name: tool.name,
    annotations: { ...toolAnnotations(tool.name), canonicalName: tool.name },
    outputSchema: genericToolOutputSchema(),
  }));
}

function toolAnnotations(name: any) {
  const toolName = String(name);
  const reconcilesPatchOutcome = toolName === 'fs_patch_outcome_show';
  const write = /^fs_(write|str_replace|replace|apply|move|create|rename|delete)/.test(toolName) || reconcilesPatchOutcome;
  return {
    title: toolName,
    readOnlyHint: !write,
    destructiveHint: /^fs_(str_replace|replace|apply|move|rename|delete)/.test(toolName),
    idempotentHint: /^fs_(read|stat|glob|grep|repository_inventory|file_metrics)/.test(toolName) || reconcilesPatchOutcome,
    openWorldHint: false,
  };
}

function genericToolOutputSchema() {
  return { type: 'object', additionalProperties: true };
}

function expectedMetadataSchemaProperties() {
  return {
    mtime: { type: 'string' },
    size: { type: 'integer' },
    sha256: { type: 'string' },
    tree_sha256: { type: 'string' },
    entry_count: { type: 'integer' },
  };
}

function parseArgs(argv: string[]): Record<string, unknown> {
  const options: Record<string, unknown> & { allowedRoots: string[], anchoredAllowedRoots: string[] } = { mode: 'read', allowedRoots: [], anchoredAllowedRoots: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = argv[i + 1];
    if (arg === '--mode') { options.mode = next; i += 1; }
    else if (arg === '--roots-from-trust-config' || arg === '--roots-from-codex-config') { options.rootsFromCodexConfig = next; i += 1; }
    else if (arg === '--allowed-root') { options.allowedRoots.push(next); i += 1; }
    else if (arg === '--anchored-allowed-root') { options.anchoredAllowedRoots.push(next); i += 1; }
    else if (arg === '--roots-config') { options.rootsConfig = next; i += 1; }
    else if (arg === '--audit-log-dir') { options.auditLogDir = next; i += 1; }
    else if (arg === '--output-root') { options.outputRoot = next; i += 1; }
    else if (arg === '--help') {
      process.stdout.write('Usage: node dist/src/main.js --mode read|write --roots-from-trust-config <path> [--allowed-root <path>] [--anchored-allowed-root user_home:.codex] [--roots-config <json>] [--audit-log-dir <path>]\n');
      process.exit(0);
    }
  }
  return options;
}

function drainJsonRpcFrames(buffer: any) {
  const requests = [];
  let remaining = buffer;
  while (true) {
    const crlfHeaderEnd = remaining.indexOf('\r\n\r\n');
    const lfHeaderEnd = remaining.indexOf('\n\n');
    const headerEnd = crlfHeaderEnd >= 0 ? crlfHeaderEnd : lfHeaderEnd;
    if (headerEnd < 0) break;
    const separatorLength = crlfHeaderEnd >= 0 ? 4 : 2;
    const header = remaining.slice(0, headerEnd);
    const match = header.match(/Content-Length:\s*(\d+)/i);
    if (!match) break;
    const length = Number(match[1]);
    const start = headerEnd + separatorLength;
    if (remaining.length < start + length) break;
    requests.push(JSON.parse(remaining.slice(start, start + length)));
    remaining = remaining.slice(start + length);
  }
  return { requests, remaining };
}

function writeJsonRpcResponse(response: any, { framed = false }: any = {}) {
  const body = JSON.stringify(response);
  if (!framed) {
    process.stdout.write(`${body}\n`);
    return;
  }
  process.stdout.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\r\n\r\n${body}`);
}

function sendProgress(request: any, progress: any, message: any, options: any) {
  const progressToken = asRecord(asRecord(request.params)._meta).progressToken;
  if (progressToken === undefined) return;
  writeJsonRpcResponse({
    jsonrpc: '2.0',
    method: 'notifications/progress',
    params: { progressToken, progress, total: 1, message },
  }, options);
}

function clientSupportsRoots(initializeParams: any) {
  return Boolean(asRecord(asRecord(initializeParams).capabilities).roots);
}

function requestClientRoots(state: any, pendingServerRequests: any, nextId: any, options: any) {
  const id = nextId();
  pendingServerRequests.set(id, (message: any) => {
    updateClientRoots(state, asRecord(message.result));
  });
  writeJsonRpcResponse({ jsonrpc: '2.0', id, method: 'roots/list', params: {} }, options);
}

function updateClientRoots(state: any, result: any) {
  const roots = Array.isArray(result.roots) ? result.roots.map((root: any) => asRecord(root)).filter((root: any) => typeof root.uri === 'string') : [];
  state.clientRoots = {
    supported: true,
    roots: roots.map((root: any) => ({
      uri: String(root.uri),
      ...(typeof root.name === 'string' ? { name: root.name } : {}),
    })),
    lastUpdatedAt: new Date().toISOString(),
  };
}

function clientRootCompletionValues(state: any) {
  const rootsValue = asRecord(state.clientRoots).roots;
  const roots = Array.isArray(rootsValue) ? rootsValue : [];
  return roots.map((root: any) => {
    const uri = String(asRecord(root).uri ?? '');
    if (!uri) return '';
    if (uri.startsWith('file:')) {
      try {
        return fileURLToPath(uri);
      } catch {
        return uri;
      }
    }
    return uri;
  }).filter(Boolean).slice(0, 100);
}

function splitLines(value: any) {
  return String(value ?? '').split(/\r?\n/).filter((line: any) => line.trim().length > 0);
}

function splitFileLines(value: any) {
  const text = String(value ?? '');
  if (text.length === 0) return [];
  return text.replace(/\r?\n$/, '').split(/\r?\n/);
}

function assertExpectedSha256(args: any, before: any, { operation, path, root }: any) {
  const expected = stringField(args, 'expected_sha256');
  if (!expected) return;
  const actual = before === null ? null : sha256(before);
  if (actual !== expected) {
    throw diagnosticError(`${operation}_expected_sha256_mismatch`, `${operation}_expected_sha256_mismatch: ${path}`, {
      ...pathMetadata(path, root),
      expected_sha256: expected,
      actual_sha256: actual,
      concurrency_diagnosis: {
        reason: 'file_content_changed_since_observation_or_guard_is_not_full_file_hash',
        expected_hash_scope: 'full_file',
        actual_hash_scope: 'full_file',
        actual_hash_source: 'live_file_bytes',
        cache_used: false,
        attribution: 'external_or_unobserved_writer_unless_a_matching_filesystem_audit_record_exists',
      },
      remediation: 'Read the file again, use the returned full-file content_sha256, reconcile intervening changes, then retry.',
    });
  }
}

function expectedSha256Map(args: any) {
  const value = asRecord(args).expected_sha256;
  if (value === undefined || value === null) return new Map();
  const record = asRecord(value);
  if (record !== value) throw diagnosticError('expected_sha256_must_be_object', 'expected_sha256_must_be_object');
  const entries = Object.entries(record);
  for (const [key, entryValue] of entries) {
    if (typeof entryValue !== 'string' || entryValue.trim().length === 0) {
      throw diagnosticError('expected_sha256_value_must_be_string', `expected_sha256_value_must_be_string: ${key}`, { key });
    }
  }
  return new Map(entries);
}

function assertExpectedPatchSha256(expectedSha256: any, filePatch: any, source: any, target: any, before: any) {
  if (expectedSha256.size === 0) return;
  const keys = [
    relative(source.root, source.path).replace(/\\/g, '/'),
    relative(target.root, target.path).replace(/\\/g, '/'),
    stripPatchPrefix(filePatch.oldPath),
    stripPatchPrefix(filePatch.newPath),
    normalizePathKey(source.path),
    normalizePathKey(target.path),
  ].filter((key: any) => key && key !== '/dev/null');
  const matchedKey = keys.find((key: any) => expectedSha256.has(key));
  if (!matchedKey) return;
  const expected = expectedSha256.get(matchedKey);
  const actual = source.path && existsSync(source.path) ? sha256(before) : null;
  if (actual !== expected) {
    throw diagnosticError('fs_apply_patch_expected_sha256_mismatch', `fs_apply_patch_expected_sha256_mismatch: ${source.path}`, {
      ...pathMetadata(source.path, source.root),
      expected_sha256: expected,
      actual_sha256: actual,
      expected_sha256_key: matchedKey,
    });
  }
  return matchedKey;
}

function assertAllExpectedPatchSha256KeysMatched(expectedSha256: any, matchedKeys: any) {
  const unmatched = [...expectedSha256.keys()].filter((key: any) => !matchedKeys.has(key));
  if (unmatched.length === 0) return;
  throw diagnosticError('fs_apply_patch_expected_sha256_unmatched', 'fs_apply_patch_expected_sha256_unmatched', {
    unmatched_expected_sha256_keys: unmatched,
    matched_expected_sha256_keys: [...matchedKeys],
  });
}

function assertExpectedMetadata(args: any, target: any, { operation, objectKey = null, mtimeKey, sizeKey, shaKey = null, treeShaKey = null, entryCountKey = null }: any) {
  const expectedObject = objectKey ? asRecord(asRecord(args)[objectKey]) : {};
  const useObjectGuard = !isEmptyStructuredMetadataGuard(expectedObject);
  const expectedMtime = (useObjectGuard ? stringField(expectedObject, 'mtime') : null) ?? stringField(args, mtimeKey);
  const expectedSize = (useObjectGuard ? integerField(expectedObject, 'size') : null) ?? integerField(args, sizeKey);
  const expectedSha = (useObjectGuard ? stringField(expectedObject, 'sha256') : null) ?? (shaKey ? stringField(args, shaKey) : null);
  const expectedTreeSha = (useObjectGuard ? stringField(expectedObject, 'tree_sha256') : null) ?? (treeShaKey ? stringField(args, treeShaKey) : null);
  const expectedEntryCount = (useObjectGuard ? integerField(expectedObject, 'entry_count') : null) ?? (entryCountKey ? integerField(args, entryCountKey) : null);
  if (!expectedMtime && expectedSize === null && !expectedSha && !expectedTreeSha && expectedEntryCount === null) return;
  const stat = statSync(target.path);
  const actualMtime = stat.mtime.toISOString();
  const actualSize = stat.size;
  const directoryFingerprint = stat.isDirectory() ? directoryTreeFingerprint(target.path, target.path) : null;
  const details = {
    ...pathMetadata(target.path, target.root),
    operation,
    expected_mtime: expectedMtime,
    actual_mtime: actualMtime,
    expected_size: expectedSize,
    actual_size: actualSize,
    expected_tree_sha256: expectedTreeSha,
    actual_tree_sha256: directoryFingerprint?.tree_sha256 ?? null,
    tree_sha256_basis: 'target_relative',
    expected_entry_count: expectedEntryCount,
    actual_entry_count: directoryFingerprint?.entry_count ?? null,
  };
  if (expectedMtime && actualMtime !== expectedMtime) {
    throw diagnosticError(`${operation}_expected_metadata_mismatch`, `${operation}_expected_metadata_mismatch: ${target.path}`, details);
  }
  if (expectedSize !== null && actualSize !== expectedSize) {
    throw diagnosticError(`${operation}_expected_metadata_mismatch`, `${operation}_expected_metadata_mismatch: ${target.path}`, details);
  }
  if (expectedSha) {
    if (!stat.isFile()) {
      throw diagnosticError(`${operation}_expected_sha256_not_supported`, `${operation}_expected_sha256_not_supported: ${target.path}`, { ...details, type: stat.isDirectory() ? 'directory' : 'other' });
    }
    const actualSha = createHash('sha256').update(readFileSync(target.path)).digest('hex');
    if (actualSha !== expectedSha) {
      throw diagnosticError(`${operation}_expected_metadata_mismatch`, `${operation}_expected_metadata_mismatch: ${target.path}`, { ...details, expected_sha256: expectedSha, actual_sha256: actualSha });
    }
  }
  if (expectedTreeSha && directoryFingerprint?.tree_truncated === true) {
    throw diagnosticError(`${operation}_expected_tree_sha256_unstable`, `${operation}_expected_tree_sha256_unstable: ${target.path}`, {
      ...details,
      actual_tree_truncated: true,
      actual_tree_entry_count: directoryFingerprint.tree_entry_count,
      tree_hash_guard_supported: false,
      remediation: 'tree_sha256 guards require a complete directory fingerprint; narrow the target directory or use mtime/entry_count guards for large trees.',
    });
  }
  if (expectedTreeSha && directoryFingerprint?.tree_sha256 !== expectedTreeSha) {
    throw diagnosticError(`${operation}_expected_metadata_mismatch`, `${operation}_expected_metadata_mismatch: ${target.path}`, details);
  }
  if (expectedEntryCount !== null && directoryFingerprint?.entry_count !== expectedEntryCount) {
    throw diagnosticError(`${operation}_expected_metadata_mismatch`, `${operation}_expected_metadata_mismatch: ${target.path}`, details);
  }
}

function isEmptyStructuredMetadataGuard(value: Record<string, unknown>): boolean {
  const keys = Object.keys(value);
  if (keys.length === 0) return true;
  const allowedKeys = new Set(['mtime', 'size', 'sha256', 'tree_sha256', 'entry_count']);
  if (keys.some((key: any) => !allowedKeys.has(key))) return false;
  if (keys.length !== allowedKeys.size) return false;
  return keys.every((key: any) => {
    const field = value[key];
    if (typeof field === 'string') return field.trim().length === 0;
    if (typeof field === 'number') return field === 0;
    return field === null || field === undefined;
  });
}

function normalizePathKey(path: any) {
  return String(path ?? '').replace(/\\/g, '/');
}

function patchOperation(filePatch: any, source: any, target: any) {
  if (filePatch.deleteFile) return 'delete';
  if (filePatch.oldPath === '/dev/null' || filePatch.kind === 'codex_add') return 'add';
  if (!samePath(source.path, target.path)) return 'move';
  return 'update';
}

function findTextOccurrences(text: any, needle: any) {
  const matches = [];
  let index = 0;
  while (matches.length < 100) {
    const found = text.indexOf(needle, index);
    if (found < 0) break;
    const before = text.slice(0, found);
    const line = before.split(/\r?\n/).length;
    const lineStart = Math.max(0, text.lastIndexOf('\n', found - 1) + 1);
    const lineEndRaw = text.indexOf('\n', found);
    const lineEnd = lineEndRaw < 0 ? text.length : lineEndRaw;
    matches.push({ line, column: found - lineStart + 1, line_text: text.slice(lineStart, lineEnd) });
    index = found + Math.max(needle.length, 1);
  }
  return matches;
}

function sha256(value: any) {
  return createHash('sha256').update(String(value)).digest('hex');
}

function diagnosticError(codeName: any, message: any, details: unknown = {}) {
  return new McpToolError(codeName, message, normalizeDiagnosticDetails(details));
}

function normalizeDiagnosticDetails(details: any) {
  const record = asRecord(details);
  const normalized = { ...record };
  if (activeToolName && String(activeToolName).startsWith('fs_') && !normalized.operation) normalized.operation = activeToolName;
  normalized.diagnostic_owner = 'local-filesystem-mcp';
  normalized.diagnostic_rule = normalized.diagnostic_rule ?? 'surface_policy_or_tool_validation';
  normalized.false_positive_route = 'Submit surface feedback with surface_id=local-filesystem, the refusal code, requested_path, and why the path classification is wrong. Do not include secret content.';
  return normalized;
}

function patchDiagnosticError(codeName: any, patch: any, details: unknown = {}) {
  const firstNonEmptyLine = splitLines(patch)[0] ?? '';
  const detectedFormat = firstNonEmptyLine === '*** Begin Patch'
    ? 'codex_apply_patch'
    : firstNonEmptyLine.startsWith('diff --git ')
      ? 'git_unified_diff'
      : firstNonEmptyLine.startsWith('--- ')
        ? 'unified_diff_incomplete'
        : 'unknown';
  return diagnosticError(codeName, `${codeName}: expected unified diff headers or Codex-style apply_patch markers`, {
    ...asRecord(details),
    detected_format: detectedFormat,
    first_non_empty_line: firstNonEmptyLine,
  });
}
export function errorDiagnostic(error: unknown) {
  if (error instanceof McpToolError) {
    return {
      schema: 'local.filesystem.error.v1',
      code: error.codeName,
      message: error.message,
      details: error.details,
    };
  }
  const record = asRecord(error);
  const nested = firstRecord(record.data, record.diagnostic, record.error);
  const source = nested ?? record;
  const messageValue = error instanceof Error
    ? error.message
    : record.message ?? source.message ?? source.reason;
  const message = typeof messageValue === 'string' && messageValue.length > 0
    ? messageValue
    : describeUnknownValue(messageValue ?? error);
  const codeValue = source.code ?? source.codeName ?? record.codeName;
  const code = typeof codeValue === 'string' && codeValue.length > 0
    ? codeValue
    : message.split(/[:\s]/)[0] || 'tool_error';
  const sourceSchema = typeof source.schema === 'string' && source.schema !== 'local.filesystem.error.v1'
    ? { source_schema: source.schema }
    : {};
  return {
    schema: 'local.filesystem.error.v1',
    code,
    message,
    details: { ...asRecord(source.details), ...sourceSchema },
  };
}

function firstRecord(...values: unknown[]): Record<string, unknown> | null {
  for (const value of values) {
    const record = asRecord(value);
    if (Object.keys(record).length > 0) return record;
  }
  return null;
}

function describeUnknownValue(value: unknown): string {
  if (typeof value === 'string' && value.length > 0) return value;
  try {
    const serialized = JSON.stringify(value);
    if (typeof serialized === 'string') return serialized;
  } catch {
    // Fall through to a non-opaque type label for cyclic or otherwise unserializable values.
  }
  return `<unserializable ${Object.prototype.toString.call(value).slice(8, -1) || 'unknown'}>`;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function resolveFilesystemPayloadArgs(toolName: any, args: any, state: any) {
  try {
    return resolveToolPayloadArgs({
      siteRoot: state.outputRoot,
      toolName,
      args,
      allowedTools: ['fs_write_file'],
      maxBytes: Number(state.payloadMaxBytes ?? 5 * 1024 * 1024),
    });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw diagnosticError(message.split(/[:\s]/)[0] || 'payload_resolution_failed', message, { tool_name: toolName });
  }
}

function stringOrNull(value: unknown): string | null {
  return typeof value === 'string' && value.trim().length > 0 ? value : null;
}

function negateGlob(pattern: string): string {
  return pattern.startsWith('!') ? pattern : `!${pattern}`;
}

function stringList(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is string => typeof item === 'string');
}

function stringField(record: unknown, key: string): string | null {
  const value = asRecord(record)[key];
  return typeof value === 'string' ? value : null;
}

function integerField(record: unknown, key: string): number | null {
  const value = asRecord(record)[key];
  return Number.isInteger(value) ? Number(value) : null;
}

function booleanField(record: unknown, key: string): boolean | null {
  const value = asRecord(record)[key];
  return typeof value === 'boolean' ? value : null;
}
