#!/usr/bin/env node
import { buildGuidanceResult } from './guidance.js';
import { guidanceToolDefinition } from './guidance.js';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { appendFileSync, existsSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { buildCommandMetadataTelemetryDeclaration, emitTelemetryEvent, telemetryErrorCodeFromUnknown, telemetryRefusalCodeFromResult, type TelemetryDeclaration, type TelemetryEventKind } from '@narada-core/mcp-telemetry';
import { buildBoundedToolResult, outputShowAsync } from '@narada-core/mcp-transport';
import {
  buildAllowedRoots,
  createExecutionPolicy,
  decideStructuredCommandExecution,
  publicExecutionPolicy,
} from './policy.js';
import {
  commandResolutionNotAttempted,
  CommandResolutionError,
  resolveCommandInvocation,
  type CommandResolutionEvidence,
} from './command-resolution.js';

const PROTOCOL_VERSION = '2024-11-05';
const TOOL_RESULT_CHAR_LIMIT = 4000;
const STREAM_PREVIEW_CHAR_LIMIT = 1000;
const TOOL_OUTPUT_SHOW_MAX_LIMIT = 20000;
const TOOL_INPUT_CHAR_LIMIT = 20000;
const MAX_SYNCHRONOUS_TIMEOUT_MS = 240_000;
const REF_PATTERN = /^structured_command_(input|execution):([A-Za-z0-9_-]{8,80})$/;
const ROOTS_LIST_REQUEST_PREFIX = 'structured_command_roots_';
const SURFACE_ID = 'structured-command';
const STRUCTURED_COMMAND_TELEMETRY_TOOL_NAMES = new Set([
  'structured_command_execution_policy_inspect',
  'structured_command_execute',
  'structured_command_start',
  'structured_command_execution_show',
  'structured_command_powershell_parse_check',
  'structured_command_input_create',
  'structured_command_elevated_window_execute',
]);

export type StructuredCommandState = Record<string, unknown> & {
  siteRoot: string;
  policy: ReturnType<typeof createExecutionPolicy>;
  auditLogDir: string | null;
  storageRoot: string;
  env: NodeJS.ProcessEnv;
  clientRoots: {
    supported: boolean;
    roots: Array<{ uri: string; name?: string }>;
    lastUpdatedAt: string | null;
  };
};

type SpawnStructuredOptions = {
  cwd: string;
  timeoutMs: number;
  maxOutputBytes: number;
  env?: NodeJS.ProcessEnv;
  abortSignal?: AbortSignal;
};

type SpawnStructuredResult = {
  exit_code: number | null;
  timed_out: boolean;
  cancelled: boolean;
  stdout: string;
  stderr: string;
  stdout_truncated: boolean;
  stderr_truncated: boolean;
  command_resolution: CommandResolutionEvidence;
  resolution_error_code: string | null;
};

type RequestContext = {
  abortSignal?: AbortSignal;
  progress?: (progress: number, message: string) => void;
};

type BackgroundExecutionRequest = {
  schema: 'narada.structured_command.background_request.v0';
  execution_ref: string;
  storage_root: string;
  audit_log_dir: string | null;
  command: string;
  args: string[];
  working_directory: string;
  timeout_ms: number;
  max_output_bytes: number;
  started_at: string;
  execution_posture: Record<string, unknown>;
  input_ref: unknown;
};

class StructuredCommandError extends Error {
  codeName: string;
  details: unknown;

  constructor(codeName: string, message: string, details: unknown = {}) {
    super(message);
    this.name = 'StructuredCommandError';
    this.codeName = codeName;
    this.details = details;
  }
}

if (isMainModule()) {
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
      if (!record.id && record.method === 'notifications/roots/list_changed' && state.clientRoots.supported) {
        requestClientRoots(state, pendingServerRequests, () => `${ROOTS_LIST_REQUEST_PREFIX}${nextServerRequestId++}`, { framed: sawFramedInput });
        continue;
      }
      if (record.method === 'initialize') {
        const response = await handleRequest(record, state);
        if (response) writeJsonRpcMessage(response, { framed: sawFramedInput });
        if (clientSupportsRoots(asRecord(record.params))) {
          state.clientRoots.supported = true;
          requestClientRoots(state, pendingServerRequests, () => `${ROOTS_LIST_REQUEST_PREFIX}${nextServerRequestId++}`, { framed: sawFramedInput });
        }
        continue;
      }
      const processing = processStdioRequest(record, state, activeRequests, { framed: sawFramedInput });
      if (record.method !== 'tools/call') await processing;
    }
  }
}

export function createServerState(options: Record<string, unknown> = {}, env: NodeJS.ProcessEnv = process.env): StructuredCommandState {
  const siteRoot = resolve(String(options.siteRoot ?? options.storageRoot ?? firstOption(options.allowedRoot) ?? firstOption(options.allowedRoots) ?? process.cwd()));
  const stateEnv = { ...env };
  loadSiteSecrets(siteRoot, stateEnv);
  const siteExtraRoots = loadSiteExtraAllowedRoots(siteRoot);
  const allowedRoots = buildAllowedRoots({
    trustConfigPaths: optionList(options.rootsFromTrustConfig),
    explicitRoots: [...siteExtraRoots, ...optionList(options.allowedRoot), ...optionList(options.allowedRoots)],
  });
  if (allowedRoots.length === 0) throw new Error('structured_command_requires_allowed_root');
  return {
    siteRoot,
    policy: createExecutionPolicy({
      allowedRoots,
      allowedCommands: optionList(options.allowCommand ?? options.allowedCommands),
      allowedPrefixes: optionList(options.allowPrefix ?? options.allowedPrefixes),
      blockedCommands: optionList(options.blockCommand ?? options.blockedCommands),
      maxTimeoutMs: options.maxTimeoutMs,
      maxOutputBytes: options.maxOutputBytes,
    }),
    auditLogDir: options.auditLogDir ? resolve(String(options.auditLogDir)) : null,
    storageRoot: resolve(String(options.storageRoot ?? allowedRoots[0])),
    env: stateEnv,
    clientRoots: { supported: false, roots: [], lastUpdatedAt: null },
  };
}

async function processStdioRequest(request: Record<string, unknown>, state: StructuredCommandState, activeRequests: Map<string, AbortController>, options: { framed: boolean }) {
  if (!request?.id && request.method === 'notifications/cancelled') {
    const requestId = String(asRecord(request.params).requestId ?? '');
    activeRequests.get(requestId)?.abort();
    return;
  }
  if (!request?.id && typeof request?.method === 'string' && request.method.startsWith('notifications/')) return;
  const requestId = String(request.id ?? '');
  const abortController = new AbortController();
  activeRequests.set(requestId, abortController);
  const progressToken = asRecord(asRecord(request.params)._meta).progressToken;
  const progress = (progressValue: number, message: string) => {
    if (progressToken === undefined) return;
    writeJsonRpcMessage({
      jsonrpc: '2.0',
      method: 'notifications/progress',
      params: { progressToken, progress: progressValue, total: 1, message },
    }, options);
  };
  progress(0, 'started');
  return handleRequest(request, state, { abortSignal: abortController.signal, progress }).then((response: any) => {
    progress(abortController.signal.aborted ? 1 : 1, abortController.signal.aborted ? 'cancelled' : 'completed');
    if (response) writeJsonRpcMessage(response, options);
  }).finally(() => {
    activeRequests.delete(requestId);
  });
}

export async function handleRequest(request: Record<string, unknown>, state: StructuredCommandState, context: RequestContext = {}) {
  if (!request?.id && typeof request?.method === 'string' && request.method.startsWith('notifications/')) return null;
  try {
    const result = await dispatchMethod(String(request.method), asRecord(request.params), state, context);
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

async function dispatchMethod(method: string, params: Record<string, unknown>, state: StructuredCommandState, context: RequestContext = {}): Promise<unknown> {
  if (method === 'initialize') {
    return {
      protocolVersion: params.protocolVersion ?? PROTOCOL_VERSION,
      capabilities: { tools: {}, resources: {}, prompts: {}, completions: {}, logging: {} },
      serverInfo: { name: 'structured-command-mcp', version: '0.1.0' },
    };
  }
  if (method === 'tools/list') return { tools: listTools() };
  if (method === 'tools/call') return callTool(params, state, context);
  if (method === 'resources/list') return listStructuredCommandResources(state);
  if (method === 'resources/read') return readStructuredCommandResource(params, state);
  if (method === 'prompts/list') return { prompts: listPrompts() };
  if (method === 'prompts/get') return promptGet(params);
  if (method === 'completion/complete') return completeArgument(params, state);
  if (method === 'logging/setLevel') return {};
  throw diagnosticError('unsupported_mcp_method', `unsupported_mcp_method:${method}`, { method });
}

function listPrompts() {
  return [{ name: 'structured_command_safe_execution', title: 'Structured Command Safe Execution', description: 'Guidance for argv-only command execution.', arguments: [] }];
}

function promptGet(params: Record<string, unknown>) {
  const name = String(params.name ?? '');
  if (name !== 'structured_command_safe_execution') throw diagnosticError('unknown_prompt', `unknown_prompt:${name}`, { name });
  return {
    description: 'Guidance for argv-only command execution.',
    messages: [{ role: 'user', content: { type: 'text', text: 'Use structured_command_execute with explicit argv arrays only. Inspect policy before relying on command availability, and use output refs for long results.' } }],
  };
}

function completeArgument(params: Record<string, unknown>, state: StructuredCommandState) {
  const argumentName = String(asRecord(asRecord(params).argument).name ?? '');
  const values = argumentName === 'name'
    ? listTools().map((tool: any) => tool.name).filter(Boolean).slice(0, 100)
    : ['working_directory', 'cwd', 'directory'].includes(argumentName) ? clientRootCompletionValues(state) : [];
  return { completion: { values, total: values.length, hasMore: false } };
}

export function listTools() {
  return decorateTools([
    guidanceToolDefinition(),
    {
      name: 'structured_command_execution_policy_inspect',
      description: 'Inspect the policy governing structured command execution.',
      inputSchema: objectSchema({}),
    },
    {
      name: 'structured_command_output_show',
      description: 'Read a materialized structured-command MCP output ref with offset/limit paging.',
      inputSchema: objectSchema({
        ref: { type: 'string', description: 'Materialized output ref, e.g. mcp_output:<id>. Alias: output_ref.' },
        output_ref: { type: 'string', description: 'Alias for ref.' },
        offset: { type: 'integer', default: 0, description: 'Character offset into the materialized JSON output.' },
        limit: { type: 'integer', default: 20000, minimum: 1, maximum: 20000, description: 'Maximum output characters to return; the transport hard-caps this value.' },
      }),
    },
    {
      name: 'structured_command_execute',
      description: `Execute a structured argv command under allowed-root and command policy. Synchronous execution is capped at ${MAX_SYNCHRONOUS_TIMEOUT_MS}ms; use structured_command_start for longer work.`,
      inputSchema: objectSchema({
        input_ref: { type: 'string', description: 'Structured command input ref from structured_command_input_create.' },
        execution_ref: { type: 'string', description: 'Prior execution ref returned by structured_command_execute; use to read later stdout/stderr pages without re-running.' },
        command: { type: 'string', description: 'Executable name or absolute executable path admitted by policy.' },
        args: { type: 'array', items: { type: 'string' }, description: 'Argument vector. No shell parsing is performed.' },
        working_directory: { type: 'string', description: 'Working directory under an allowed root.' },
        timeout_ms: { type: 'integer', description: 'Timeout in milliseconds.' },
        wait_for_completion: { type: 'boolean', description: 'Defaults true. Set false only with test_scope "known_slow" to return an execution_ref immediately and poll it for completion.' },
        test_scope: { type: 'string', enum: ['focused', 'broad', 'known_slow', 'unknown'], description: 'Optional caller-declared verification scope/cost posture for test commands.' },
        expected_cost: { type: 'string', enum: ['low', 'medium', 'high', 'unknown'], description: 'Optional caller-declared expected cost for this command.' },
        stdout_offset: { type: 'integer', description: 'Character offset for stdout page. Defaults 0.' },
        stderr_offset: { type: 'integer', description: 'Character offset for stderr page. Defaults 0.' },
        stdout_limit: { type: 'integer', description: `Stdout page size. Default ${STREAM_PREVIEW_CHAR_LIMIT}, max ${TOOL_OUTPUT_SHOW_MAX_LIMIT}.` },
        stderr_limit: { type: 'integer', description: `Stderr page size. Default ${STREAM_PREVIEW_CHAR_LIMIT}, max ${TOOL_OUTPUT_SHOW_MAX_LIMIT}.` },
      }),
    },
    {
      name: 'structured_command_start',
      description: 'Start a durable asynchronous structured argv command and return an execution_ref immediately. Completion survives MCP surface process restart.',
      inputSchema: objectSchema({
        input_ref: { type: 'string', description: 'Structured command input ref from structured_command_input_create.' },
        command: { type: 'string', description: 'Executable name or absolute executable path admitted by policy.' },
        args: { type: 'array', items: { type: 'string' }, description: 'Argument vector. No shell parsing is performed.' },
        working_directory: { type: 'string', description: 'Working directory under an allowed root.' },
        timeout_ms: { type: 'integer', description: 'Timeout in milliseconds, bounded by surface policy.' },
        test_scope: { type: 'string', enum: ['focused', 'broad', 'known_slow', 'unknown'] },
        expected_cost: { type: 'string', enum: ['low', 'medium', 'high', 'unknown'] },
      }),
    },
    {
      name: 'structured_command_execution_show',
      description: 'Read one durable structured command execution by execution_ref without rerunning it.',
      inputSchema: objectSchema({
        execution_ref: { type: 'string', description: 'Execution ref returned by structured_command_start or structured_command_execute.' },
        stdout_offset: { type: 'integer', description: 'Character offset for stdout page. Defaults 0.' },
        stderr_offset: { type: 'integer', description: 'Character offset for stderr page. Defaults 0.' },
        stdout_limit: { type: 'integer', description: `Stdout page size. Default ${STREAM_PREVIEW_CHAR_LIMIT}, max ${TOOL_OUTPUT_SHOW_MAX_LIMIT}.` },
        stderr_limit: { type: 'integer', description: `Stderr page size. Default ${STREAM_PREVIEW_CHAR_LIMIT}, max ${TOOL_OUTPUT_SHOW_MAX_LIMIT}.` },
      }, ['execution_ref']),
    },
    {
      name: 'structured_command_powershell_parse_check',
      description: 'Parse-check one allowed-root PowerShell script without admitting arbitrary pwsh command execution.',
      inputSchema: objectSchema({
        path: { type: 'string', description: 'PowerShell script path under an allowed root.' },
        working_directory: { type: 'string', description: 'Optional working directory under an allowed root. Defaults to the script directory.' },
        timeout_ms: { type: 'integer', description: 'Timeout in milliseconds.' },
      }, ['path']),
    },
    {
      name: 'structured_command_input_create',
      description: 'Create a scoped structured command input ref.',
      inputSchema: objectSchema({
        input_id: { type: 'string', description: 'Optional caller-chosen id, max 80 chars.' },
        command: { type: 'string' },
        args: { type: 'array', items: { type: 'string' } },
        working_directory: { type: 'string' },
        timeout_ms: { type: 'integer' },
        wait_for_completion: { type: 'boolean', description: 'Defaults true. Set false only with test_scope "known_slow" to return an execution_ref immediately.' },
        test_scope: { type: 'string', enum: ['focused', 'broad', 'known_slow', 'unknown'] },
        expected_cost: { type: 'string', enum: ['low', 'medium', 'high', 'unknown'] },
      }, ['command']),
    },
    {
      name: 'structured_command_elevated_window_execute',
      description: 'On Windows, launch a policy-approved command in a visible elevated UAC window. Output is not captured from the elevated process.',
      inputSchema: objectSchema({
        command: { type: 'string', description: 'Executable name or absolute executable path admitted by policy.' },
        args: { type: 'array', items: { type: 'string' }, description: 'Argument vector for the elevated process.' },
        working_directory: { type: 'string', description: 'Working directory under an allowed root.' },
        confirm_elevation: { type: 'boolean', description: 'Must be true to show a UAC/elevated execution prompt.' },
        wait: { type: 'boolean', description: 'When true, the broker waits for the elevated process to exit. Defaults false.' },
        dry_run: { type: 'boolean', description: 'When true, return the planned broker command without invoking UAC.' },
      }, ['command', 'working_directory']),
    },
  ]);
}

async function callTool(params: Record<string, unknown>, state: StructuredCommandState, context: RequestContext = {}) {
  const name = params?.name;
  const args = asRecord(params.arguments);
  const startedAt = Date.now();
  try {
    let result: unknown;
    if (name === 'structured_command_guidance') result = buildGuidanceResult(args);
    else {
      enforceInputCharLimit(args);
      if (name === 'structured_command_output_show') {
        const page = await structuredCommandOutputShow(args, state);
        return buildBoundedToolResult({
          siteRoot: state.siteRoot,
          toolName: String(name),
          value: page,
          limit: TOOL_RESULT_CHAR_LIMIT,
          readerTool: 'structured_command_output_show',
        });
      }
      if (name === 'structured_command_execution_policy_inspect') result = publicExecutionPolicy(state.policy);
      else if (name === 'structured_command_execute') result = await executeStructuredCommand(args, state, context);
      else if (name === 'structured_command_start') result = await executeStructuredCommand({ ...args, wait_for_completion: false, test_scope: args.test_scope ?? 'known_slow' }, state, context, true);
      else if (name === 'structured_command_execution_show') result = await executeStructuredCommand(args, state, context);
      else if (name === 'structured_command_powershell_parse_check') result = await powershellParseCheck(args, state, context);
      else if (name === 'structured_command_input_create') result = createStructuredCommandInput(args, state);
      else if (name === 'structured_command_elevated_window_execute') result = await executeStructuredCommandElevatedWindow(args, state);
      else throw diagnosticError('structured_command_unknown_tool', `structured_command_unknown_tool:${name}`, { tool_name: name ?? null });
    }
    emitStructuredCommandTelemetry(String(name ?? ''), asRecord(result), state, startedAt);
    return toolResult(result, state, String(name ?? 'unknown_tool'));
  } catch (error) {
    emitStructuredCommandTelemetry(String(name ?? ''), {}, state, startedAt, error);
    throw error;
  }
}

async function structuredCommandOutputShow(args: any, state: any) {
  try {
    const page = await outputShowAsync({ siteRoot: state.siteRoot, args });
    return {
      ...asRecord(page),
      output_scope: {
        reader_tool: 'structured_command_output_show',
        server_output_root: state.siteRoot,
        scope: 'bound_server_output_root',
      },
    };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw diagnosticError('structured_command_output_ref_scope_unreadable', message, {
      output_root: state.siteRoot,
      requested_ref: args.ref ?? args.output_ref ?? null,
      remediation: 'Use the same structured-command MCP site scope that created the output_ref; cross-site filesystem roots are not accepted by the shared transport reader.',
    });
  }
}

function emitStructuredCommandTelemetry(toolName: string, result: Record<string, unknown>, state: StructuredCommandState, startedAt: number, error?: unknown): void {
  if (!STRUCTURED_COMMAND_TELEMETRY_TOOL_NAMES.has(toolName)) return;
  const declaration = structuredCommandTelemetryDeclaration(toolName);
  if (!declaration) return;
  const status = error ? 'error' : String(result.status ?? 'ok');
  const eventKind: TelemetryEventKind = error ? 'tool_failed' : status === 'refused' ? 'tool_refused' : 'tool_completed';
  try {
    emitTelemetryEvent({
      context: {
        siteRoot: state.siteRoot,
        siteId: process.env.NARADA_SITE_ID ?? null,
        surfaceId: SURFACE_ID,
        agentId: process.env.NARADA_AGENT_ID ?? null,
        carrierSessionId: process.env.NARADA_CARRIER_SESSION_ID ?? null,
      },
      declaration,
      event: {
        toolName,
        eventKind,
        status,
        startedAt,
        completedAt: Date.now(),
        refusalCode: telemetryRefusalCodeFromResult(result),
        errorCode: error ? telemetryErrorCodeFromUnknown(error) : null,
        policyDecision: asRecord(result.decision ?? null),
      },
    });
  } catch (telemetryError) {
    process.stderr.write(`structured_command_telemetry_error:${telemetryError instanceof Error ? telemetryError.message : String(telemetryError)}\n`);
  }
}

function structuredCommandTelemetryDeclaration(toolName: string): TelemetryDeclaration | null {
  if (!STRUCTURED_COMMAND_TELEMETRY_TOOL_NAMES.has(toolName)) return null;
  const highSensitivity = /execute|elevated_window|input_create/.test(toolName);
  return buildCommandMetadataTelemetryDeclaration({
    sensitivity: highSensitivity ? 'high' : 'medium',
    policyDecision: /execute|elevated_window/.test(toolName),
  });
}

export async function executeStructuredCommand(args: unknown, state: StructuredCommandState, context: RequestContext = {}, explicitStart : any= false): Promise<unknown> {
  const argsRecord = asRecord(args);
  enforceInputCharLimit(argsRecord);
  if (argsRecord.execution_ref) {
    const execution = readStructuredCommandExecution(String(argsRecord.execution_ref), state);
    return buildPagedExecutionResult(execution.result, argsRecord, String(argsRecord.execution_ref));
  }
  const effectiveArgs = argsRecord.input_ref ? asRecord(readStructuredCommandInput(String(argsRecord.input_ref), state).input) : argsRecord;
  const timeoutMs = Math.min(state.policy.maxTimeoutMs, Math.max(1, Number(effectiveArgs.timeout_ms ?? 60_000)));
  const workingDirectory = effectiveArgs.working_directory ? resolve(String(effectiveArgs.working_directory)) : state.policy.allowedRoots[0];
  const executionPosture = structuredCommandExecutionPosture(effectiveArgs);
  const waitForCompletion = argsRecord.wait_for_completion !== undefined
    ? argsRecord.wait_for_completion !== false
    : effectiveArgs.wait_for_completion !== false;
  const decision = decideStructuredCommandExecution({
    command: effectiveArgs.command,
    args: Array.isArray(effectiveArgs.args) ? effectiveArgs.args : [],
    workingDirectory,
  }, state.policy);
  if (decision.status !== 'allowed') {
    return {
      schema: 'narada.structured_command.execution_result.v0',
      status: 'refused',
      decision,
      refusal_reasons: decision.reasons,
      remediation_hints: decision.remediation_hints,
      mcp_fallbacks: decision.mcp_fallbacks,
      command: decision.command,
      args: decision.args,
    working_directory: decision.working_directory,
    execution_posture: executionPosture,
    test_scope: executionPosture.test_scope,
    expected_cost: executionPosture.expected_cost,
    executed: false,
    };
  }

  if (!waitForCompletion && !explicitStart && executionPosture.test_scope !== 'known_slow') {
    return {
      schema: 'narada.structured_command.execution_result.v0',
      status: 'refused',
      executed: false,
      decision,
      refusal_reasons: ['background_requires_known_slow_test_scope'],
      remediation_hints: ['Set test_scope to "known_slow" when using wait_for_completion:false for a governed long-running verification command.'],
      mcp_fallbacks: [],
      command: decision.command,
      args: decision.args,
      working_directory: decision.working_directory,
      execution_posture: executionPosture,
      test_scope: executionPosture.test_scope,
      expected_cost: executionPosture.expected_cost,
      wait_for_completion: false,
    };
  }

  if (waitForCompletion && timeoutMs > MAX_SYNCHRONOUS_TIMEOUT_MS) {
    return {
      schema: 'narada.structured_command.execution_result.v0',
      status: 'refused',
      executed: false,
      decision,
      refusal_reasons: ['synchronous_timeout_exceeds_reliable_bound'],
      remediation_hints: [`Use structured_command_start for commands requiring more than ${MAX_SYNCHRONOUS_TIMEOUT_MS}ms, then poll structured_command_execution_show.`],
      command: decision.command,
      args: decision.args,
      working_directory: decision.working_directory,
      timeout_ms: timeoutMs,
      max_synchronous_timeout_ms: MAX_SYNCHRONOUS_TIMEOUT_MS,
    };
  }

  const startedAt = new Date().toISOString();
  context.progress?.(0.1, 'executing');
  const spawnOptions = {
    cwd: decision.working_directory,
    timeoutMs,
    maxOutputBytes: state.policy.maxOutputBytes,
    env: state.env,
    ...(waitForCompletion ? { abortSignal: context.abortSignal } : {}),
  };
  if (!waitForCompletion) {
    const pendingPayload = {
      schema: 'narada.structured_command.execution_result.v0',
      status: 'running',
      executed: true,
      command: decision.command,
      args: decision.args,
      working_directory: decision.working_directory,
      started_at: startedAt,
      finished_at: null,
      timeout_ms: timeoutMs,
      execution_posture: executionPosture,
      test_scope: executionPosture.test_scope,
      expected_cost: executionPosture.expected_cost,
      execution_mode: 'background',
      wait_for_completion: false,
      pending: true,
      exit_code: null,
      stdout: '',
      stderr: '',
      stdout_truncated: false,
      stderr_truncated: false,
      timed_out: false,
      cancelled: false,
      command_resolution: commandResolutionNotAttempted(decision.command, 'background_runner_pending'),
      resolution_error_code: null,
      input_ref: argsRecord.input_ref ?? null,
    };
    const executionRef = createStructuredCommandExecution(pendingPayload, state);
    audit(state, pendingPayload);
    if (!executionRef) throw diagnosticError('structured_command_execution_persistence_unavailable', 'structured_command_execution_persistence_unavailable');
    startDetachedBackgroundRunner({
      schema: 'narada.structured_command.background_request.v0',
      execution_ref: executionRef,
      storage_root: state.storageRoot,
      audit_log_dir: state.auditLogDir,
      command: decision.command,
      args: decision.args,
      working_directory: decision.working_directory,
      timeout_ms: timeoutMs,
      max_output_bytes: state.policy.maxOutputBytes,
      started_at: startedAt,
      execution_posture: executionPosture,
      input_ref: argsRecord.input_ref ?? null,
    }, state);
    return buildPagedExecutionResult(pendingPayload, argsRecord, executionRef);
  }

  const result = await spawnStructured(decision.command, decision.args, spawnOptions);
  const payload = buildStructuredCommandExecutionPayload({
    decision,
    result,
    startedAt,
    timeoutMs,
    executionPosture,
    inputRef: argsRecord.input_ref ?? null,
    executionMode: 'synchronous',
    waitForCompletion: true,
  });
  audit(state, payload);
  const executionRef = createStructuredCommandExecution(payload, state);
  return buildPagedExecutionResult(payload, argsRecord, executionRef);
}

function startDetachedBackgroundRunner(request: BackgroundExecutionRequest, state: StructuredCommandState): void {
  const { id } = parseRef(request.execution_ref, 'execution');
  const requestPath = join(state.storageRoot, 'background-requests', `${id}.json`);
  writeJsonRecord(requestPath, request);
  const runnerPath = fileURLToPath(new URL('./background-runner.js', import.meta.url));
  const requestSha256 = sha256Json(request);
  const child = spawn(process.execPath, [runnerPath, requestPath, requestSha256, request.execution_ref, request.storage_root, request.audit_log_dir ?? ''], {
    cwd: request.working_directory,
    detached: true,
    windowsHide: true,
    stdio: 'ignore',
    env: state.env,
  });
  child.once('error', (error: any) => {
    const payload = buildStructuredCommandExecutionPayload({
      decision: {
        command: request.command,
        args: request.args,
        working_directory: request.working_directory,
      },
      result: {
        exit_code: null,
        stdout: '',
        stderr: error.message,
        stdout_truncated: false,
        stderr_truncated: false,
        timed_out: false,
        cancelled: false,
        command_resolution: commandResolutionNotAttempted(request.command, 'background_runner_spawn_failed'),
        resolution_error_code: 'background_runner_spawn_failed',
      },
      startedAt: request.started_at,
      timeoutMs: request.timeout_ms,
      executionPosture: request.execution_posture,
      inputRef: request.input_ref,
      executionMode: 'background',
      waitForCompletion: false,
    });
    audit(state, payload);
    updateStructuredCommandExecution(request.execution_ref, payload, state);
    rmSync(requestPath, { force: true });
  });
  child.unref();
}

export function buildStructuredCommandExecutionPayload({ decision, result, startedAt, timeoutMs, executionPosture, inputRef, executionMode, waitForCompletion }: any) {
  const finishedAt = new Date().toISOString();
  return {
    schema: 'narada.structured_command.execution_result.v0',
    status: result.cancelled ? 'cancelled' : result.timed_out ? 'timed_out' : result.exit_code === 0 ? 'ok' : 'failed',
    executed: true,
    command: decision.command,
    args: decision.args,
    working_directory: decision.working_directory,
    started_at: startedAt,
    finished_at: finishedAt,
    timeout_ms: timeoutMs,
    execution_posture: executionPosture,
    test_scope: executionPosture.test_scope,
    expected_cost: executionPosture.expected_cost,
    execution_mode: executionMode,
    wait_for_completion: waitForCompletion,
    pending: false,
    exit_code: result.exit_code,
    stdout: result.stdout,
    stderr: result.stderr,
    stdout_truncated: result.stdout_truncated,
    stderr_truncated: result.stderr_truncated,
    timed_out: result.timed_out,
    cancelled: result.cancelled,
    command_resolution: result.command_resolution,
    resolution_error_code: result.resolution_error_code,
    input_ref: inputRef,
  };
}

export function createStructuredCommandInput(args: any, state: any) {
  const inputId = normalizeRefId(args.input_id ?? `i_${randomUUID().replace(/-/g, '').slice(0, 24)}`);
  const input = {
    command: String(args.command ?? ''),
    args: Array.isArray(args.args) ? args.args.map(String) : [],
    ...(args.working_directory ? { working_directory: String(args.working_directory) } : {}),
    ...(args.timeout_ms ? { timeout_ms: Number(args.timeout_ms) } : {}),
    ...(args.wait_for_completion !== undefined ? { wait_for_completion: args.wait_for_completion === false ? false : true } : {}),
    ...structuredCommandInputPosture(args),
  };
  const record = {
    schema: 'narada.structured_command.input.v0',
    ref: `structured_command_input:${inputId}`,
    created_at: new Date().toISOString(),
    sha256: sha256Json(input),
    input,
  };
  writeJsonRecord(inputPath(state, inputId), record);
  return {
    schema: 'narada.structured_command.input_create_result.v0',
    status: 'created',
    input_ref: record.ref,
    sha256: record.sha256,
  };
}

async function powershellParseCheck(args: Record<string, unknown>, state: StructuredCommandState, context: RequestContext = {}): Promise<unknown> {
  const scriptPath = resolve(String(args.path ?? ''));
  if (!scriptPath || !scriptPath.toLowerCase().endsWith('.ps1')) {
    throw diagnosticError('structured_command_powershell_parse_check_requires_ps1', 'structured_command_powershell_parse_check_requires_ps1', { path: String(args.path ?? '') });
  }
  if (!isInsideAnyRoot(scriptPath, state.policy.allowedRoots)) {
    throw diagnosticError('structured_command_powershell_parse_check_path_outside_allowed_roots', 'structured_command_powershell_parse_check_path_outside_allowed_roots', { path: scriptPath, allowed_roots: state.policy.allowedRoots });
  }
  if (!existsSync(scriptPath) || !statSync(scriptPath).isFile()) {
    throw diagnosticError('structured_command_powershell_parse_check_file_not_found', 'structured_command_powershell_parse_check_file_not_found', { path: scriptPath });
  }
  const workingDirectory = args.working_directory ? resolve(String(args.working_directory)) : dirname(scriptPath);
  if (!isInsideAnyRoot(workingDirectory, state.policy.allowedRoots)) {
    throw diagnosticError('structured_command_powershell_parse_check_cwd_outside_allowed_roots', 'structured_command_powershell_parse_check_cwd_outside_allowed_roots', { working_directory: workingDirectory, allowed_roots: state.policy.allowedRoots });
  }
  const timeoutMs = Math.min(state.policy.maxTimeoutMs, Math.max(1, Number(args.timeout_ms ?? 30_000)));
  const parseScript = [
    '$ErrorActionPreference = "Stop"',
    '$tokens = $null',
    '$errors = $null',
    `[System.Management.Automation.Language.Parser]::ParseFile(${psSingleQuote(scriptPath)}, [ref]$tokens, [ref]$errors) > $null`,
    'if ($errors.Count -gt 0) { $errors | ForEach-Object { Write-Error ($_.ToString()) }; exit 1 }',
    'Write-Output "parse_ok"',
  ].join('; ');
  const result = await spawnStructured('pwsh', ['-NoProfile', '-Command', parseScript], {
    cwd: workingDirectory,
    timeoutMs,
    maxOutputBytes: state.policy.maxOutputBytes,
    env: state.env,
    abortSignal: context.abortSignal,
  });
  const payload = {
    schema: 'narada.structured_command.powershell_parse_check.v0',
    status: result.cancelled ? 'cancelled' : result.timed_out ? 'timed_out' : result.exit_code === 0 ? 'ok' : 'failed',
    path: scriptPath,
    working_directory: workingDirectory,
    timeout_ms: timeoutMs,
    exit_code: result.exit_code,
    stdout: result.stdout,
    stderr: result.stderr,
    stdout_truncated: result.stdout_truncated,
    cancelled: result.cancelled,
    command_resolution: result.command_resolution,
    resolution_error_code: result.resolution_error_code,
    timed_out: result.timed_out,
    arbitrary_command_execution_admitted: false,
    parser_api: 'System.Management.Automation.Language.Parser.ParseFile',
  };
  audit(state, payload);
  return payload;
}

export function spawnStructured(command: string, args: string[], { cwd, timeoutMs, maxOutputBytes, env, abortSignal }: SpawnStructuredOptions): Promise<SpawnStructuredResult> {
  if (abortSignal?.aborted) {
    return Promise.resolve({
      exit_code: null,
      stdout: '',
      stderr: '',
      stdout_truncated: false,
      stderr_truncated: false,
      timed_out: false,
      cancelled: true,
      command_resolution: commandResolutionNotAttempted(command, 'request_cancelled_before_spawn'),
      resolution_error_code: null,
    });
  }

  let invocation: ReturnType<typeof resolveCommandInvocation>;
  try {
    invocation = resolveCommandInvocation(command, args, { cwd, env });
  } catch (error) {
    if (!(error instanceof CommandResolutionError)) return Promise.reject(error);
    return Promise.resolve({
      exit_code: null,
      stdout: '',
      stderr: `${error.codeName}:${error.message}`,
      stdout_truncated: false,
      stderr_truncated: false,
      timed_out: false,
      cancelled: false,
      command_resolution: error.evidence,
      resolution_error_code: error.codeName,
    });
  }

  return new Promise<SpawnStructuredResult>((resolvePromise) => {
    const child = spawn(invocation.command, invocation.args, {
      cwd,
      shell: false,
      windowsHide: true,
      // On POSIX the child leads its own process group so a timeout can signal
      // the whole tree without touching the server process group.
      detached: process.platform !== 'win32',
      stdio: ['ignore', 'pipe', 'pipe'],
      env,
    });
    let stdout = '';
    let stderr = '';
    let stdoutTruncated = false;
    let stderrTruncated = false;
    let timedOut = false;
    let cancelled = false;
    let settled = false;
    let terminationPromise: Promise<void> | null = null;
    const terminate = () => {
      terminationPromise ??= killChildProcessTree(child);
      return terminationPromise;
    };
    const timer = setTimeout(() => {
      if (cancelled || settled) return;
      timedOut = true;
      void terminate();
    }, timeoutMs);
    const abortHandler = () => {
      if (timedOut || settled) return;
      cancelled = true;
      clearTimeout(timer);
      void terminate();
    };
    abortSignal?.addEventListener('abort', abortHandler, { once: true });
    child.stdout.on('data', (chunk: any) => {
      const next = stdout + chunk.toString();
      stdoutTruncated ||= Buffer.byteLength(next, 'utf8') > maxOutputBytes;
      stdout = truncateUtf8(next, maxOutputBytes);
    });
    child.stderr.on('data', (chunk: any) => {
      const next = stderr + chunk.toString();
      stderrTruncated ||= Buffer.byteLength(next, 'utf8') > maxOutputBytes;
      stderr = truncateUtf8(next, maxOutputBytes);
    });
    child.on('error', (error: any) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      abortSignal?.removeEventListener('abort', abortHandler);
      const result: SpawnStructuredResult = {
        exit_code: null,
        stdout,
        stderr: `${stderr}${stderr ? '\\n' : ''}${error.message}`,
        stdout_truncated: stdoutTruncated,
        stderr_truncated: stderrTruncated,
        timed_out: timedOut,
        cancelled,
        command_resolution: invocation.evidence,
        resolution_error_code: 'spawn_error',
      };
      void (terminationPromise ?? Promise.resolve()).then(() => resolvePromise(result));
    });
    child.on('close', (code: any) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      abortSignal?.removeEventListener('abort', abortHandler);
      const result: SpawnStructuredResult = {
        exit_code: code,
        stdout,
        stderr,
        stdout_truncated: stdoutTruncated,
        stderr_truncated: stderrTruncated,
        timed_out: timedOut,
        cancelled,
        command_resolution: invocation.evidence,
        resolution_error_code: null,
      };
      void (terminationPromise ?? Promise.resolve()).then(() => resolvePromise(result));
    });
  });
}
// Bounded grace between process-group SIGTERM and the SIGKILL escalation for
// descendants that ignore SIGTERM (POSIX only; Windows uses taskkill /T /F).
const POSIX_KILL_GRACE_MS = 1_000;
const POSIX_KILL_FORCE_WAIT_MS = 5_000;
const WINDOWS_KILL_WAIT_MS = 1_500;

async function killChildProcessTree(child: ReturnType<typeof spawn>): Promise<void> {
  const pid = child.pid;
  if (pid === undefined) {
    try { child.kill(); } catch { /* process already exited */ }
    return;
  }
  if (process.platform === 'win32') {
    // child.kill() terminates only the direct process on Windows; taskkill /T
    // terminates the full descendant tree so a timed-out command cannot leave
    // grandchildren running.
    await new Promise<void>((resolve: any) => {
      try {
        const killer = spawn('taskkill', ['/pid', String(pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
        let finished = false;
        const timer = setTimeout(() => {
          try { killer.kill(); } catch { /* taskkill already exited */ }
          try { child.kill(); } catch { /* process already exited */ }
          finish();
        }, WINDOWS_KILL_WAIT_MS);
        const finish = () => {
          if (finished) return;
          finished = true;
          clearTimeout(timer);
          resolve();
        };
        killer.once('error', () => {
          try { child.kill(); } catch { /* process already exited */ }
          finish();
        });
        killer.once('close', (code: any) => {
          if (code !== 0) {
            try { child.kill(); } catch { /* process already exited */ }
          }
          finish();
        });
      } catch {
        try { child.kill(); } catch { /* process already exited */ }
        resolve();
      }
    });
    return;
  }
  // The child was spawned detached, so it leads its own process group; signal
  // the group to reach descendants without touching the server process group.
  try {
    process.kill(-pid, 'SIGTERM');
  } catch {
    try { child.kill(); } catch { /* process already exited */ }
    return;
  }
  // Descendants can ignore SIGTERM. Wait for the bounded grace period, then
  // escalate the whole group and wait for the group to disappear before the
  // structured timeout result is returned.
  if (await waitForProcessGroupExit(pid, POSIX_KILL_GRACE_MS)) return;
  try {
    process.kill(-pid, 'SIGKILL');
  } catch { /* group exited between the probe and the kill */ }
  await waitForProcessGroupExit(pid, POSIX_KILL_FORCE_WAIT_MS);
}

async function waitForProcessGroupExit(pid: number, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (processGroupExists(pid)) {
    if (Date.now() >= deadline) return false;
    await new Promise((resolve: any) => setTimeout(resolve, 25));
  }
  return true;
}

function processGroupExists(pid: number): boolean {
  try {
    process.kill(-pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException)?.code === 'EPERM';
  }
}

export async function executeStructuredCommandElevatedWindow(args: any, state: any) {
  if (process.platform !== 'win32') {
    return {
      schema: 'narada.structured_command.elevated_window_result.v0',
      status: 'refused',
      executed: false,
      refusal_reasons: ['windows_only'],
    };
  }
  const argsRecord = asRecord(args);
  enforceInputCharLimit(argsRecord);
  const workingDirectory = resolve(String(argsRecord.working_directory ?? state.policy.allowedRoots[0]));
  const commandArgs = Array.isArray(argsRecord.args) ? argsRecord.args.map(String) : [];
  const decision = decideStructuredCommandExecution({
    command: argsRecord.command,
    args: commandArgs,
    workingDirectory,
  }, state.policy);
  if (decision.status !== 'allowed') {
    return {
      schema: 'narada.structured_command.elevated_window_result.v0',
      status: 'refused',
      executed: false,
      decision,
      refusal_reasons: decision.reasons,
      command: decision.command,
      args: decision.args,
      working_directory: decision.working_directory,
    };
  }
  const dryRun = argsRecord.dry_run === true;
  if (!dryRun && argsRecord.confirm_elevation !== true) {
    return {
      schema: 'narada.structured_command.elevated_window_result.v0',
      status: 'refused',
      executed: false,
      decision,
      refusal_reasons: ['confirm_elevation_required'],
      command: decision.command,
      args: decision.args,
      working_directory: decision.working_directory,
    };
  }
  const wait = argsRecord.wait === true;
  const broker = buildElevatedWindowBrokerCommand({ command: decision.command, args: decision.args, workingDirectory: decision.working_directory, wait });
  if (dryRun) {
    return {
      schema: 'narada.structured_command.elevated_window_result.v0',
      status: 'planned',
      executed: false,
      decision,
      broker,
      command: decision.command,
      args: decision.args,
      working_directory: decision.working_directory,
      wait,
    };
  }
  const startedAt = new Date().toISOString();
  const result = await spawnStructured(broker.command, broker.args, {
    cwd: decision.working_directory,
    timeoutMs: 60_000,
    maxOutputBytes: state.policy.maxOutputBytes,
    env: state.env,
  });
  const payload = {
    schema: 'narada.structured_command.elevated_window_result.v0',
    status: result.exit_code === 0 ? 'uac_prompt_completed' : 'broker_failed',
    executed: result.exit_code === 0,
    decision,
    broker_exit_code: result.exit_code,
    broker_stdout: result.stdout,
    broker_stderr: result.stderr,
    command_resolution: result.command_resolution,
    resolution_error_code: result.resolution_error_code,
    command: decision.command,
    args: decision.args,
    working_directory: decision.working_directory,
    wait,
    started_at: startedAt,
    finished_at: new Date().toISOString(),
    note: 'The elevated process runs in a separate Windows UAC context; its stdout/stderr are not captured by this MCP call.',
  };
  audit(state, payload);
  const executionRef = createStructuredCommandExecution(payload, state);
  return buildPagedExecutionResult(payload, argsRecord, executionRef);
}

function buildPagedExecutionResult(payload: any, args: any, executionRef: any) {
  if (payload.executed === false) return { ...payload, execution_ref: executionRef ?? null };
  const stdoutPage = pageText(String(payload.stdout ?? ''), args.stdout_offset, args.stdout_limit, STREAM_PREVIEW_CHAR_LIMIT);
  const stderrPage = pageText(String(payload.stderr ?? ''), args.stderr_offset, args.stderr_limit, STREAM_PREVIEW_CHAR_LIMIT);
  return {
    ...payload,
    execution_ref: executionRef,
    stdout: stdoutPage.text,
    stderr: stderrPage.text,
    stdout_offset: stdoutPage.offset,
    stderr_offset: stderrPage.offset,
    stdout_limit: stdoutPage.limit,
    stderr_limit: stderrPage.limit,
    stdout_next_offset: stdoutPage.next_offset,
    stderr_next_offset: stderrPage.next_offset,
    stdout_output_truncated: stdoutPage.output_truncated,
    stderr_output_truncated: stderrPage.output_truncated,
    stdout_char_length: stdoutPage.full_output_char_length,
    stderr_char_length: stderrPage.full_output_char_length,
    page_source: args.execution_ref ? 'persisted_execution' : 'new_execution',
  };
}

function pageText(text: any, offsetValue: any, limitValue: any, defaultLimit: any) {
  const offset = Math.max(0, Number(offsetValue ?? 0));
  const limit = clampInteger(limitValue, 1, TOOL_OUTPUT_SHOW_MAX_LIMIT, defaultLimit);
  const chunk = text.slice(offset, offset + limit);
  const nextOffset = offset + chunk.length < text.length ? offset + chunk.length : null;
  return {
    text: chunk,
    offset,
    limit,
    next_offset: nextOffset,
    output_truncated: nextOffset !== null,
    full_output_char_length: text.length,
  };
}

export function buildElevatedWindowBrokerCommand({ command, args, workingDirectory, wait }: any) {
  const script = [
    "$ErrorActionPreference = 'Stop'",
    `$p = Start-Process -FilePath ${psSingleQuote(command)} -ArgumentList ${psArrayLiteral(args)} -WorkingDirectory ${psSingleQuote(workingDirectory)} -Verb RunAs -WindowStyle Normal -PassThru`,
    wait ? 'if ($p) { $p.WaitForExit(); exit $p.ExitCode }' : 'if ($p) { Write-Output ("started_pid=" + $p.Id) }',
  ].join('; ');
  return {
    command: 'powershell.exe',
    args: ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', script],
    script,
  };
}

function toolResult(payload: any, state: any, toolName : any= 'structured_command') {
  const text = renderToolResultText(payload);
  if (text.length > TOOL_RESULT_CHAR_LIMIT) {
    return buildBoundedToolResult({
      siteRoot: state.siteRoot,
      toolName,
      value: payload,
      limit: TOOL_RESULT_CHAR_LIMIT,
      readerTool: 'structured_command_output_show',
    });
  }
  const truncated = text.length > TOOL_RESULT_CHAR_LIMIT;
  const rendered = truncated ? text.slice(0, TOOL_RESULT_CHAR_LIMIT) : text;
  return {
    content: [assistantTextContent(rendered)],
    structuredContent: buildStructuredContent(payload, {
      truncated,
      renderedTextLength: rendered.length,
      fullTextLength: text.length,
      state,
    }),
  };
}

function assistantTextContent(text: any) {
  return { type: 'text', text, annotations: { audience: ['assistant'] } };
}

function buildStructuredContent(payload: any, { truncated, renderedTextLength, fullTextLength, state }: any) {
  if (payload?.schema === 'narada.structured_command.execution_result.v0') {
    return buildExecutionStructuredContent(payload, { truncated, renderedTextLength, fullTextLength, state });
  }
  if (payload?.schema === 'narada.structured_command.execution_policy.v0') {
    return {
      ...payload,
      truncated,
      rendered_text_char_length: renderedTextLength,
      full_output_char_length: fullTextLength,
    };
  }
  if (payload?.schema === 'narada.structured_command.input_create_result.v0') {
    return {
      ...payload,
      truncated,
      rendered_text_char_length: renderedTextLength,
      full_output_char_length: fullTextLength,
    };
  }
  if (payload?.schema === 'narada.structured_command.elevated_window_result.v0') {
    return {
      ...payload,
      truncated,
      rendered_text_char_length: renderedTextLength,
      full_output_char_length: fullTextLength,
    };
  }
  if (payload?.schema === 'narada.structured_command.powershell_parse_check.v0') {
    return {
      ...payload,
      truncated,
      rendered_text_char_length: renderedTextLength,
      full_output_char_length: fullTextLength,
    };
  }
  return {
    schema: payload?.schema,
    status: payload?.status,
    truncated,
    ...(payload?.input_ref ? { input_ref: payload.input_ref } : {}),
    ...(payload?.sha256 ? { sha256: payload.sha256 } : {}),
    rendered_text_char_length: renderedTextLength,
    full_output_char_length: fullTextLength,
  };
}

function buildExecutionStructuredContent(payload: any, { truncated, renderedTextLength, fullTextLength, state: _state }: any) {
  if (payload.executed === false) {
    return {
      schema: payload.schema,
      status: payload.status,
      executed: false,
      command: payload.command,
      args: payload.args,
      working_directory: payload.working_directory,
      execution_posture: payload.execution_posture ?? null,
      test_scope: payload.test_scope ?? null,
      expected_cost: payload.expected_cost ?? null,
      refusal_reasons: payload.refusal_reasons ?? payload.decision?.reasons ?? [],
      remediation_hints: payload.remediation_hints ?? payload.decision?.remediation_hints ?? [],
      mcp_fallbacks: payload.mcp_fallbacks ?? payload.decision?.mcp_fallbacks ?? [],
      decision: payload.decision ?? null,
      execution_ref: payload.execution_ref ?? null,
      truncated,
      rendered_text_char_length: renderedTextLength,
      full_output_char_length: fullTextLength,
    };
  }
  const stdout = String(payload.stdout ?? '');
  const stderr = String(payload.stderr ?? '');
  return {
    schema: payload.schema,
    status: payload.status,
    executed: payload.executed,
    command: payload.command,
    args: payload.args,
    working_directory: payload.working_directory,
    started_at: payload.started_at ?? null,
    finished_at: payload.finished_at ?? null,
    timeout_ms: payload.timeout_ms,
    execution_posture: payload.execution_posture ?? null,
    test_scope: payload.test_scope ?? null,
    expected_cost: payload.expected_cost ?? null,
    exit_code: payload.exit_code,
    timed_out: payload.timed_out,
    cancelled: payload.cancelled,
    execution_mode: payload.execution_mode ?? 'synchronous',
    wait_for_completion: payload.wait_for_completion ?? true,
    pending: payload.pending ?? false,
    execution_ref: payload.execution_ref,
    page_source: payload.page_source,
    stdout,
    stderr,
    stdout_truncated: payload.stdout_truncated,
    stderr_truncated: payload.stderr_truncated,
    stdout_char_length: payload.stdout_char_length ?? stdout.length,
    stderr_char_length: payload.stderr_char_length ?? stderr.length,
    stdout_offset: payload.stdout_offset ?? 0,
    stderr_offset: payload.stderr_offset ?? 0,
    stdout_limit: payload.stdout_limit ?? STREAM_PREVIEW_CHAR_LIMIT,
    stderr_limit: payload.stderr_limit ?? STREAM_PREVIEW_CHAR_LIMIT,
    stdout_next_offset: payload.stdout_next_offset ?? null,
    stderr_next_offset: payload.stderr_next_offset ?? null,
    stdout_output_truncated: payload.stdout_output_truncated ?? false,
    stderr_output_truncated: payload.stderr_output_truncated ?? false,
    ...(payload.input_ref ? { input_ref: payload.input_ref } : {}),
    truncated,
    rendered_text_char_length: renderedTextLength,
    full_output_char_length: fullTextLength,
  };
}

function renderToolResultText(payload: any) {
  if (payload?.schema === 'narada.structured_command.execution_result.v0' && payload.executed === false) {
    const reasons = payload.refusal_reasons ?? payload.decision?.reasons ?? [];
    const hints = payload.remediation_hints ?? payload.decision?.remediation_hints ?? [];
    return [
    guidanceToolDefinition(),
      `structured_command_execute: ${payload.status}`,
      `command: ${payload.command ?? ''}`,
      `working_directory: ${payload.working_directory ?? ''}`,
      `refusal_reasons: ${Array.isArray(reasons) && reasons.length ? reasons.join('; ') : 'none'}`,
      Array.isArray(hints) && hints.length ? `remediation_hints: ${hints.join('; ')}` : null,
    ].filter(Boolean).join('\n');
  }
  if (payload?.schema === 'narada.structured_command.execution_result.v0' && payload.executed === true) {
    const lines = [
      `structured_command_execute: ${payload.status}`,
      `exit_code: ${payload.exit_code}`,
    ];
    if (payload.status === 'running' && payload.execution_ref) lines.push(`execution_pending: poll ${payload.execution_ref}`);
    const stdoutLines = renderStreamPreviewLines('stdout', payload.stdout, payload.stdout_truncated, payload.stdout_output_truncated);
    const stderrLines = renderStreamPreviewLines('stderr', payload.stderr, payload.stderr_truncated, payload.stderr_output_truncated);
    if (payload.status === 'ok') lines.push(...stdoutLines, ...stderrLines);
    else lines.push(...stderrLines, ...stdoutLines);
    return lines.join('\n');
  }
  if (payload?.schema === 'narada.structured_command.elevated_window_result.v0') {
    const reasons = payload.refusal_reasons ?? [];
    return [
      `structured_command_elevated_window_execute: ${payload.status}`,
      `executed: ${payload.executed === true}`,
      `command: ${payload.command ?? ''}`,
      `working_directory: ${payload.working_directory ?? ''}`,
      Array.isArray(reasons) && reasons.length ? `refusal_reasons: ${reasons.join('; ')}` : null,
      payload.note ? `note: ${payload.note}` : null,
    ].filter(Boolean).join('\n');
  }
  if (payload?.schema === 'narada.structured_command.powershell_parse_check.v0') {
    return [
      `structured_command_powershell_parse_check: ${payload.status}`,
      `path: ${payload.path ?? ''}`,
      `exit_code: ${payload.exit_code ?? ''}`,
      payload.stderr ? `stderr:\n${payload.stderr}` : null,
      payload.stdout ? `stdout:\n${payload.stdout}` : null,
    ].filter(Boolean).join('\n');
  }
  return JSON.stringify(payload, null, 2);
}

function renderStreamPreviewLines(label: any, value: any, streamTruncated: any, pageTruncated: any) {
  if (!value && !streamTruncated) return [];
  const text = String(value ?? '');
  const preview = text.slice(0, STREAM_PREVIEW_CHAR_LIMIT);
  const lines = [`${label}:`, preview];
  if (text.length > preview.length || pageTruncated) lines.push(`[${label} preview truncated]`);
  if (streamTruncated) lines.push(`[${label} truncated]`);
  return lines;
}

function psSingleQuote(value: any) {
  return `'${String(value).replace(/'/g, "''")}'`;
}

function psArrayLiteral(values: any) {
  return `@(${values.map((value: any) => psSingleQuote(value)).join(', ')})`;
}

function enforceInputCharLimit(value: any, path : any= 'arguments') {
  if (typeof value === 'string' && value.length > TOOL_INPUT_CHAR_LIMIT) {
    throw diagnosticError('structured_command_input_too_long', `structured_command_input_too_long:${path}:${value.length}>${TOOL_INPUT_CHAR_LIMIT}`, {
      path,
      length: value.length,
      limit: TOOL_INPUT_CHAR_LIMIT,
    });
  }
  if (Array.isArray(value)) {
    value.forEach((item: any, index: any) => enforceInputCharLimit(item, `${path}[${index}]`));
    return;
  }
  if (value && typeof value === 'object') {
    for (const [key, child] of Object.entries(value)) {
      enforceInputCharLimit(child, `${path}.${key}`);
    }
  }
}

function structuredCommandExecutionPosture(args: Record<string, unknown>): Record<string, unknown> {
  const testScope = stringEnumValue(args.test_scope, ['focused', 'broad', 'known_slow', 'unknown'], inferTestScope(args));
  const expectedCost = stringEnumValue(args.expected_cost, ['low', 'medium', 'high', 'unknown'], inferExpectedCost(args, testScope));
  return {
    schema: 'narada.structured_command.execution_posture.v0',
    test_scope: testScope,
    expected_cost: expectedCost,
    source: args.test_scope || args.expected_cost ? 'caller_declared' : 'derived',
  };
}

function structuredCommandInputPosture(args: Record<string, unknown>): Record<string, unknown> {
  const posture = structuredCommandExecutionPosture(args);
  return {
    test_scope: posture.test_scope,
    expected_cost: posture.expected_cost,
  };
}

function inferTestScope(args: Record<string, unknown>): string {
  const command = String(args.command ?? '').toLowerCase();
  const argv = Array.isArray(args.args) ? args.args.map((item: any) => String(item).toLowerCase()) : [];
  if (command === 'pnpm' && argv.includes('test')) return argv.includes('--filter') ? 'focused' : 'broad';
  if (command === 'npm' && argv.includes('test')) return 'broad';
  return 'unknown';
}

function inferExpectedCost(_args: Record<string, unknown>, testScope: string): string {
  if (testScope === 'focused') return 'low';
  if (testScope === 'broad' || testScope === 'known_slow') return 'high';
  return 'unknown';
}

function stringEnumValue(value: unknown, allowed: string[], fallback: string): string {
  const text = typeof value === 'string' && value.trim() ? value.trim() : fallback;
  if (!allowed.includes(text)) throw diagnosticError('structured_command_invalid_enum', 'structured_command_invalid_enum', { value: text, allowed });
  return text;
}

export function audit(state: any, payload: any) {
  if (!state.auditLogDir) return;
  mkdirSync(state.auditLogDir, { recursive: true });
  appendFileSync(join(state.auditLogDir, 'structured-command.jsonl'), `${JSON.stringify(payload)}\n`, 'utf8');
}

function createStructuredCommandExecution(result: any, state: any) {
  if (!state) return null;
  const executionId = `e_${randomUUID().replace(/-/g, '').slice(0, 24)}`;
  const record = {
    schema: 'narada.structured_command.execution.v0',
    ref: `structured_command_execution:${executionId}`,
    created_at: new Date().toISOString(),
    sha256: sha256Json(result),
    result,
  };
  writeJsonRecord(executionPath(state, executionId), record);
  return record.ref;
}

export function updateStructuredCommandExecution(ref: any, result: any, state: any) {
  const { id } = parseRef(ref, 'execution');
  const path = executionPath(state, id);
  const existing = readJsonRecord(path);
  writeJsonRecord(path, {
    schema: 'narada.structured_command.execution.v0',
    ref,
    created_at: existing.created_at ?? new Date().toISOString(),
    updated_at: new Date().toISOString(),
    sha256: sha256Json(result),
    result,
  });
}

export function readStructuredCommandExecution(ref: any, state: any) {
  const { id } = parseRef(ref, 'execution');
  return readJsonRecord(executionPath(state, id));
}

function readStructuredCommandInput(ref: any, state: any) {
  const { id } = parseRef(ref, 'input');
  return readJsonRecord(inputPath(state, id));
}

function parseRef(ref: any, kind: any) {
  const match = String(ref ?? '').match(REF_PATTERN);
  if (!match || match[1] !== kind) throw diagnosticError(`structured_command_invalid_${kind}_ref`, `structured_command_invalid_${kind}_ref`, { ref: String(ref ?? ''), expected_kind: kind });
  return { kind: match[1], id: match[2] };
}

function inputPath(state: any, id: any) {
  return join(state.storageRoot, 'inputs', `${id}.json`);
}

function executionPath(state: any, id: any) {
  return join(state.storageRoot, 'executions', `${id}.json`);
}

function listStructuredCommandResources(state: any) {
  const dir = join(state.storageRoot, 'executions');
  if (!existsSync(dir)) return { resources: [] };
  return {
    resources: readdirSync(dir).filter((name: any) => name.endsWith('.json')).sort().map((name: any) => {
      const id = name.replace(/\.json$/, '');
      const ref = `structured_command_execution:${id}`;
      return { uri: structuredCommandExecutionUri(ref), name: ref, title: ref, description: 'Structured command execution artifact.', mimeType: 'application/json' };
    }),
  };
}

function readStructuredCommandResource(params: any, state: any) {
  const ref = structuredCommandExecutionRefFromUri(String(params.uri ?? ''));
  const { id } = parseRef(ref, 'execution');
  const record = readJsonRecord(executionPath(state, id));
  return { contents: [{ uri: params.uri, mimeType: 'application/json', text: JSON.stringify(record, null, 2) }] };
}

function structuredCommandExecutionUri(ref: any) {
  return `structured-command-execution:${encodeURIComponent(ref)}`;
}

function structuredCommandExecutionRefFromUri(uri: any) {
  if (!uri.startsWith('structured-command-execution:')) throw diagnosticError('structured_command_resource_uri_invalid', 'structured_command_resource_uri_invalid', { uri });
  return decodeURIComponent(uri.slice('structured-command-execution:'.length));
}

function writeJsonRecord(path: any, record: any) {
  mkdirSync(dirname(path), { recursive: true });
  const temporaryPath = `${path}.${process.pid}.${randomUUID()}.tmp`;
  writeFileSync(temporaryPath, `${JSON.stringify(record, null, 2)}\n`, 'utf8');
  renameSync(temporaryPath, path);
}

function readJsonRecord(path: any) {
  if (!existsSync(path)) throw diagnosticError('structured_command_ref_not_found', 'structured_command_ref_not_found', { path });
  return JSON.parse(readFileSync(path, 'utf8'));
}

function normalizeRefId(value: any) {
  const id = String(value).trim();
  if (!/^[A-Za-z0-9_-]{8,80}$/.test(id)) throw diagnosticError('structured_command_invalid_ref_id', 'structured_command_invalid_ref_id', { input_id: id, pattern: '^[A-Za-z0-9_-]{8,80}$' });
  return id;
}

function clampInteger(value: any, min: any, max: any, fallback: any) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.max(min, Math.min(max, Math.trunc(parsed)));
}

function sha256Json(value: any) {
  return sha256Text(JSON.stringify(value));
}

function sha256Text(value: any) {
  return createHash('sha256').update(String(value)).digest('hex');
}

function truncateUtf8(value: any, maxBytes: any) {
  let out = value;
  if (Buffer.byteLength(out, 'utf8') <= maxBytes) return out;
  const marker = '[structured-command omitted earlier output; preserved tail]\n';
  if (maxBytes <= Buffer.byteLength(marker, 'utf8')) {
    while (Buffer.byteLength(out, 'utf8') > maxBytes) out = out.slice(1);
    return out;
  }
  out = `${marker}${out}`;
  while (Buffer.byteLength(out, 'utf8') > maxBytes) out = `${marker}${out.slice(marker.length + 1)}`;
  return out;
}

function objectSchema(properties: any, required : any= []) {
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
  return {
    title: String(name),
    readOnlyHint: !/execute|create|start/.test(String(name)),
    destructiveHint: false,
    idempotentHint: /inspect|show/.test(String(name)),
    openWorldHint: true,
  };
}

function genericToolOutputSchema() {
  return { type: 'object', additionalProperties: true };
}

function parseArgs(argv: string[]) {
  const parsed: Record<string, unknown> = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (!arg.startsWith('--')) continue;
    const key = arg.slice(2).replace(/-([a-z])/g, (_: any, c: any) => c.toUpperCase());
    const next = argv[i + 1];
    if (next && !next.startsWith('--')) {
      const current = parsed[key];
      parsed[key] = current === undefined ? next : Array.isArray(current) ? [...current, next] : [current, next];
      i++;
    } else {
      parsed[key] = true;
    }
  }
  return parsed;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function optionList(value: unknown): string[] {
  if (value === undefined || value === null || value === true) return [];
  return Array.isArray(value) ? value.map(String) : [String(value)];
}

function diagnosticError(codeName: any, message: any, details: unknown = {}) {
  return new StructuredCommandError(codeName, message, details);
}

function errorDiagnostic(error: any) {
  if (error instanceof StructuredCommandError) {
    return {
      schema: 'narada.structured_command.error.v0',
      code: error.codeName,
      message: error.message,
      details: error.details,
    };
  }
  const message = error instanceof Error ? error.message : String(error);
  const code = message.split(/[:\s]/)[0] || 'structured_command_error';
  return {
    schema: 'narada.structured_command.error.v0',
    code,
    message,
    details: {},
  };
}

function drainJsonRpcFrames(buffer: any) {
  const requests = [];
  let remaining = buffer;
  while (true) {
    const match = remaining.match(/^Content-Length:\s*(\d+)\r?\n\r?\n/i);
    if (!match) break;
    const headerLength = match[0].length;
    const length = Number(match[1]);
    if (remaining.length < headerLength + length) break;
    const body = remaining.slice(headerLength, headerLength + length);
    requests.push(JSON.parse(body));
    remaining = remaining.slice(headerLength + length);
  }
  return { requests, remaining };
}

function writeJsonRpcMessage(message: unknown, options: { framed: boolean }) {
  const body = JSON.stringify(message);
  if (options.framed) process.stdout.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\r\n\r\n${body}`);
  else process.stdout.write(`${body}\n`);
}

function clientSupportsRoots(initializeParams: Record<string, unknown>): boolean {
  return Boolean(asRecord(asRecord(initializeParams).capabilities).roots);
}

function requestClientRoots(state: StructuredCommandState, pendingServerRequests: Map<string, (message: Record<string, unknown>) => void>, nextId: () => string, options: { framed: boolean }): void {
  const id = nextId();
  pendingServerRequests.set(id, (message: any) => {
    updateClientRoots(state, asRecord(message.result));
  });
  writeJsonRpcMessage({ jsonrpc: '2.0', id, method: 'roots/list', params: {} }, options);
}

function updateClientRoots(state: StructuredCommandState, result: Record<string, unknown>): void {
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

function clientRootCompletionValues(state: StructuredCommandState): string[] {
  return state.clientRoots.roots.map((root: any) => {
    const uri = root.uri;
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

function isInsideAnyRoot(path: string, roots: string[]): boolean {
  return roots.some((root: any) => {
    const rel = relative(root, path);
    return rel === '' || (!rel.startsWith('..') && !/^[a-zA-Z]:/.test(rel));
  });
}

function isMainModule() {
  return process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
}

function firstOption(value: unknown): string | null {
  const values = optionList(value);
  return values.length > 0 ? values[0] : null;
}

function siteControlRoot(siteRoot: string): string {
  const root = resolve(siteRoot);
  return basename(root).toLowerCase() === '.narada' ? root : resolve(root, '.narada');
}

function loadSiteExtraAllowedRoots(siteRoot: string): string[] {
  try {
    const configPath = join(siteControlRoot(siteRoot), 'allowed-roots.json');
    if (!existsSync(configPath)) return [];
    const data = JSON.parse(readFileSync(configPath, 'utf8'));
    if (Array.isArray(data.extra_allowed_roots)) return data.extra_allowed_roots.filter((r: unknown) => typeof r === 'string' && r.trim().length > 0);
  } catch {
    // Best-effort.
  }
  return [];
}

function loadSiteSecrets(siteRoot: string, targetEnv: NodeJS.ProcessEnv): void {
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
