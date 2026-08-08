#!/usr/bin/env node

import { existsSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { resolve, join, relative, isAbsolute } from 'node:path';
import { pathToFileURL } from 'node:url';
import { buildGuidanceResult, guidanceToolDefinition, type GuidanceRecord } from './guidance.js';

const SERVER_NAME = 'project-state-mcp';
const SERVER_VERSION = '0.2.0';
const PROTOCOL_VERSION = '2024-11-05';
const MAX_OUTPUT_BYTES = 512 * 1024;
const COMMAND_TIMEOUT_MS = 30_000;

export type JsonRecord = Record<string, unknown>;
export type ServerState = { projectRoot: string; cliPath: string };
type CommandSpec = {
  tool: string;
  cli: string;
  args: (input: JsonRecord) => string[];
  description: string;
  properties: JsonRecord;
  required?: string[];
};

const COMMANDS: CommandSpec[] = [
  {
    tool: 'project_state_program_list',
    cli: 'program list',
    args: () => ['program', 'list'],
    description: 'List the durable programs represented by the site-owned virtual project-state registry.',
    properties: {},
  },
  {
    tool: 'project_state_program_show',
    cli: 'program show <program_id>',
    args: (input) => ['program', 'show', requiredString(input, 'program_id')],
    description: 'Show one program and its registered project memberships.',
    properties: { program_id: stringSchema('Canonical program id.') },
    required: ['program_id'],
  },
  {
    tool: 'project_state_project_list',
    cli: 'project list [--program <program_id>]',
    args: (input) => ['project', 'list', ...(optionalFlag(input, 'program_id', '--program'))],
    description: 'List bounded project records, optionally filtered to one program.',
    properties: { program_id: stringSchema('Optional canonical program id filter.') },
  },
  {
    tool: 'project_state_project_show',
    cli: 'project show <project_id>',
    args: (input) => ['project', 'show', requiredString(input, 'project_id')],
    description: 'Show one project and its virtual object matrix.',
    properties: { project_id: stringSchema('Canonical project id.') },
    required: ['project_id'],
  },
  {
    tool: 'project_state_matrix',
    cli: 'matrix [--project] [--object] [--lifecycle]',
    args: (input) => [
      'matrix',
      ...optionalFlag(input, 'project_id', '--project'),
      ...optionalFlag(input, 'object_id', '--object'),
      ...optionalFlag(input, 'lifecycle', '--lifecycle'),
    ],
    description: 'Read the lifecycle matrix, bounded by project, object, or lifecycle state when requested.',
    properties: {
      project_id: stringSchema('Optional canonical project id filter.'),
      object_id: stringSchema('Optional canonical object id filter.'),
      lifecycle: stringSchema('Optional canonical lifecycle state filter.'),
    },
  },
  {
    tool: 'project_state_gaps',
    cli: 'gaps [--program] [--project]',
    args: (input) => ['gaps', ...optionalFlag(input, 'program_id', '--program'), ...optionalFlag(input, 'project_id', '--project')],
    description: 'List explicit lifecycle gaps and their open gates.',
    properties: {
      program_id: stringSchema('Optional canonical program id filter.'),
      project_id: stringSchema('Optional canonical project id filter.'),
    },
  },
  {
    tool: 'project_state_handoff',
    cli: 'handoff [--program] [--project]',
    args: (input) => ['handoff', ...optionalFlag(input, 'program_id', '--program'), ...optionalFlag(input, 'project_id', '--project')],
    description: 'Read the auditable virtual-only release handoff with lifecycle/maturity status, evidence replay commands, deferred gates, and explicit re-entry triggers.',
    properties: {
      program_id: stringSchema('Optional canonical program id filter.'),
      project_id: stringSchema('Optional canonical project id filter.'),
    },
  },
  {
    tool: 'project_state_standards_list',
    cli: 'standards list [--selection <selection>]',
    args: (input) => ['standards', 'list', ...optionalFlag(input, 'selection', '--selection')],
    description: 'List registered standards and their tailored project applicability records.',
    properties: { selection: stringSchema('Optional selection filter: core, conditional, or reference.') },
  },
  {
    tool: 'project_state_standard_show',
    cli: 'standards show <standard_id>',
    args: (input) => ['standards', 'show', requiredString(input, 'standard_id')],
    description: 'Show one standard reference, its internal control obligations, and trace mappings.',
    properties: { standard_id: stringSchema('Canonical standard reference id.') },
    required: ['standard_id'],
  },
  {
    tool: 'project_state_applicability',
    cli: 'applicability [--program] [--project] [--standard] [--status]',
    args: (input) => [
      'applicability',
      ...optionalFlag(input, 'program_id', '--program'),
      ...optionalFlag(input, 'project_id', '--project'),
      ...optionalFlag(input, 'standard_id', '--standard'),
      ...optionalFlag(input, 'status', '--status'),
    ],
    description: 'Read project-level standard applicability, tailoring status, and rationale.',
    properties: {
      program_id: stringSchema('Optional canonical program id filter.'),
      project_id: stringSchema('Optional canonical project id filter.'),
      standard_id: stringSchema('Optional canonical standard reference id filter.'),
      status: stringSchema('Optional applicability status: selected, conditional, reference, or not_applicable.'),
    },
  },
  {
    tool: 'project_state_standard_trace',
    cli: 'trace [--program] [--project] [--standard] [--obligation] [--object] [--lifecycle] [--status]',
    args: (input) => [
      'trace',
      ...optionalFlag(input, 'program_id', '--program'),
      ...optionalFlag(input, 'project_id', '--project'),
      ...optionalFlag(input, 'standard_id', '--standard'),
      ...optionalFlag(input, 'obligation_id', '--obligation'),
      ...optionalFlag(input, 'object_id', '--object'),
      ...optionalFlag(input, 'lifecycle', '--lifecycle'),
      ...optionalFlag(input, 'status', '--status'),
    ],
    description: 'Trace internal standard obligations to program, project, object, lifecycle cell, evidence, review gate, and open gap.',
    properties: {
      program_id: stringSchema('Optional canonical program id filter.'),
      project_id: stringSchema('Optional canonical project id filter.'),
      standard_id: stringSchema('Optional canonical standard reference id filter.'),
      obligation_id: stringSchema('Optional canonical internal obligation id filter.'),
      object_id: stringSchema('Optional canonical project object id filter.'),
      lifecycle: stringSchema('Optional lifecycle state filter.'),
      status: stringSchema('Optional alignment status: virtually_supported, open_gap, or not_applicable.'),
    },
  },
  {
    tool: 'project_state_standard_gaps',
    cli: 'standards gaps [--program] [--project] [--standard]',
    args: (input) => [
      'standards',
      'gaps',
      ...optionalFlag(input, 'program_id', '--program'),
      ...optionalFlag(input, 'project_id', '--project'),
      ...optionalFlag(input, 'standard_id', '--standard'),
    ],
    description: 'List standards mappings whose virtual applicability remains an explicit open gap.',
    properties: {
      program_id: stringSchema('Optional canonical program id filter.'),
      project_id: stringSchema('Optional canonical project id filter.'),
      standard_id: stringSchema('Optional canonical standard reference id filter.'),
    },
  },
  {
    tool: 'project_state_validate',
    cli: 'validate',
    args: () => ['validate'],
    description: 'Validate and return the complete virtual-only project-state payload.',
    properties: {},
  },
];

export function createServerState(options: JsonRecord = {}): ServerState {
  const projectRoot = normalizePath(String(options.projectRoot ?? options.project_root ?? process.cwd()));
  const cliPath = normalizePath(join(projectRoot, 'scripts', 'project-state-cli.mjs'));
  return { projectRoot, cliPath };
}

export async function handleRequest(request: JsonRecord, state: ServerState): Promise<JsonRecord | null> {
  if (request.id === undefined && typeof request.method === 'string' && request.method.startsWith('notifications/')) return null;
  try {
    const result = await dispatchMethod(String(request.method), asRecord(request.params), state);
    return { jsonrpc: '2.0', id: request.id ?? null, result };
  } catch (error) {
    const diagnostic = errorDiagnostic(error);
    return { jsonrpc: '2.0', id: request.id ?? null, error: { code: -32000, message: diagnostic.message, data: diagnostic } };
  }
}

export async function runStdioServer(options: JsonRecord = {}): Promise<void> {
  const state = createServerState(options);
  let buffer = '';
  let queue = Promise.resolve();
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk: string) => {
    buffer += chunk;
    const drained = buffer.includes('Content-Length:') ? drainJsonRpcFrames(buffer) : drainJsonLines(buffer);
    buffer = drained.remaining;
    for (const request of drained.requests) {
      queue = queue.then(async () => {
        const response = await handleRequest(request, state);
        if (response) writeJsonRpcResponse(response, { framed: drained.framed });
      });
    }
  });
}

async function dispatchMethod(method: string, params: JsonRecord, state: ServerState): Promise<JsonRecord> {
  switch (method) {
    case 'initialize':
      return { protocolVersion: params.protocolVersion ?? PROTOCOL_VERSION, capabilities: { tools: {} }, serverInfo: { name: SERVER_NAME, version: SERVER_VERSION } };
    case 'tools/list':
      return { tools: listTools() };
    case 'tools/call':
      return callTool(params, state);
    default:
      throw diagnosticError('unsupported_mcp_method', `unsupported_mcp_method:${method}`);
  }
}

export function listTools() {
  return [
    guidanceToolDefinition(),
    tool('project_state_doctor', 'Inspect project-state MCP posture, site root, CLI availability, and command coverage.', {}),
    tool('project_state_command_map', 'List the read-only MCP tools and their aligned project-state CLI commands.', {}),
    ...COMMANDS.map((spec) => tool(spec.tool, spec.description, spec.properties, spec.required)),
  ];
}

async function callTool(params: JsonRecord, state: ServerState): Promise<JsonRecord> {
  const name = requiredString(params, 'name');
  const args = asRecord(params.arguments);
  let result: JsonRecord;
  if (name === 'project_state_guidance') result = buildGuidanceResult(args as GuidanceRecord);
  else if (name === 'project_state_doctor') result = projectStateDoctor(state);
  else if (name === 'project_state_command_map') result = projectStateCommandMap();
  else {
    const spec = COMMANDS.find((item) => item.tool === name);
    if (!spec) throw diagnosticError('unknown_tool', `unknown_tool:${name}`, { tool_name: name });
    result = await invokeProjectStateCommand(spec, args, state);
  }
  return { content: [{ type: 'text', text: JSON.stringify(result, null, 2) }], structuredContent: result };
}

function projectStateDoctor(state: ServerState): JsonRecord {
  const rootExists = existsSync(state.projectRoot);
  const cliExists = existsSync(state.cliPath);
  const relativeCli = relative(state.projectRoot, state.cliPath).replace(/\\/g, '/');
  return {
    schema: 'narada.project_state.mcp_doctor.v1',
    status: rootExists && cliExists ? 'ok' : 'degraded',
    server_name: SERVER_NAME,
    project_root: state.projectRoot,
    project_root_exists: rootExists,
    cli_path: state.cliPath,
    cli_relative_path: relativeCli,
    cli_exists: cliExists,
    node_executable: process.execPath,
    node_version: process.versions.node,
    sqlite_runtime: 'node:sqlite (owned by the project CLI)',
    command_count: COMMANDS.length,
    coverage: COMMANDS.map(commandSummary),
    read_only: true,
    virtual_only: true,
    remediation: rootExists && cliExists ? null : 'Point the local-site projection at a narada.space project root containing scripts/project-state-cli.mjs.',
  };
}

function projectStateCommandMap(): JsonRecord {
  return { schema: 'narada.project_state.command_map.v1', status: 'ok', read_only: true, virtual_only: true, commands: COMMANDS.map(commandSummary), count: COMMANDS.length };
}

async function invokeProjectStateCommand(spec: CommandSpec, input: JsonRecord, state: ServerState): Promise<JsonRecord> {
  for (const key of spec.required ?? []) requiredString(input, key);
  assertSafeCliPath(state);
  const args = [state.cliPath, '--project-root', state.projectRoot, '--json', ...spec.args(input)];
  const parsed = await spawnProjectStateCli(args);
  if (parsed.status !== 'ok') {
    throw diagnosticError('project_state_cli_error', `project_state_cli_error:${String(parsed.code ?? 'unknown')}`, {
      tool: spec.tool,
      cli_command: spec.cli,
      cli_path: state.cliPath,
      cli_result: parsed,
    });
  }
  return {
    schema: 'narada.project_state.mcp_result.v1',
    status: 'ok',
    tool: spec.tool,
    cli_command: spec.cli,
    read_only: true,
    mutation_performed: false,
    virtual_only: true,
    result: parsed.result ?? parsed,
  };
}

function spawnProjectStateCli(args: string[]): Promise<JsonRecord> {
  return new Promise((resolveResult, rejectResult) => {
    const child = spawn(process.execPath, args, { shell: false, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let settled = false;
    const finishError = (error: Error) => { if (!settled) { settled = true; rejectResult(error); } };
    const finishResult = (result: JsonRecord) => { if (!settled) { settled = true; resolveResult(result); } };
    const timer = setTimeout(() => {
      child.kill();
      finishError(diagnosticError('project_state_cli_timeout', `project_state_cli_timeout:${COMMAND_TIMEOUT_MS}ms`, { timeout_ms: COMMAND_TIMEOUT_MS }));
    }, COMMAND_TIMEOUT_MS);
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk: string) => {
      stdoutBytes += Buffer.byteLength(chunk, 'utf8');
      if (stdoutBytes <= MAX_OUTPUT_BYTES) stdout += chunk;
      else {
        child.kill();
        finishError(diagnosticError('project_state_output_too_large', 'project-state CLI output exceeded the bounded MCP envelope', { max_output_bytes: MAX_OUTPUT_BYTES }));
      }
    });
    child.stderr.on('data', (chunk: string) => {
      stderrBytes += Buffer.byteLength(chunk, 'utf8');
      if (stderrBytes <= 16 * 1024) stderr += chunk;
    });
    child.on('error', (error) => { clearTimeout(timer); finishError(diagnosticError('project_state_cli_spawn_failed', error.message, { cause: error.message })); });
    child.on('close', (code, signal) => {
      clearTimeout(timer);
      if (settled) return;
      let parsed: JsonRecord;
      try { parsed = JSON.parse(stdout) as JsonRecord; }
      catch (error) {
        finishError(diagnosticError('project_state_cli_invalid_json', 'project-state CLI did not return JSON', { exit_code: code, signal, stderr: stderr.trim(), cause: String(error) }));
        return;
      }
      if (code !== 0) {
        finishResult({ ...parsed, status: parsed.status ?? 'error', exit_code: code, signal, stderr: stderr.trim() });
        return;
      }
      finishResult(parsed);
    });
  });
}

function assertSafeCliPath(state: ServerState): void {
  const rel = relative(state.projectRoot, state.cliPath);
  if (isAbsolute(rel) || rel.startsWith('..') || !rel.replace(/\\/g, '/').endsWith('scripts/project-state-cli.mjs')) {
    throw diagnosticError('project_state_cli_path_invalid', 'project-state CLI path escaped the configured project root');
  }
}

function commandSummary(spec: CommandSpec) {
  return { tool: spec.tool, cli_command: spec.cli, read_only: true, requires_execute: false, requires_authority: false };
}

function tool(name: string, description: string, properties: JsonRecord, required: string[] = []) {
  return {
    name,
    description,
    inputSchema: { type: 'object', properties, required, additionalProperties: false },
    annotations: { title: name, readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    outputSchema: { type: 'object', additionalProperties: true },
  };
}

function stringSchema(description: string) { return { type: 'string', description }; }

function optionalFlag(input: JsonRecord, key: string, flag: string): string[] {
  const value = input[key];
  if (value === undefined || value === null || value === '') return [];
  return [flag, requiredString(input, key)];
}

function requiredString(args: JsonRecord, key: string): string {
  const value = args[key];
  if (typeof value !== 'string' || !value.trim()) throw diagnosticError('required_argument_missing', `required_argument_missing:${key}`, { key });
  return value.trim();
}

function asRecord(value: unknown): JsonRecord { return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonRecord : {}; }

function normalizePath(path: string) { return resolve(path).replace(/\\/g, '/'); }

function diagnosticError(code: string, message: string, detail: JsonRecord = {}) {
  const error = new Error(message) as Error & { code?: string; detail?: JsonRecord };
  error.code = code;
  error.detail = detail;
  return error;
}

function errorDiagnostic(error: unknown): JsonRecord {
  if (error instanceof Error) {
    const known = error as Error & { code?: string; detail?: JsonRecord };
    return { code: known.code ?? 'error', message: error.message, ...(known.detail ?? {}) };
  }
  return { code: 'error', message: String(error) };
}

function drainJsonRpcFrames(buffer: string): { requests: JsonRecord[]; remaining: string; framed: boolean } {
  const requests: JsonRecord[] = [];
  let remaining = buffer;
  while (true) {
    const crlfHeaderEnd = remaining.indexOf('\r\n\r\n');
    const lfHeaderEnd = remaining.indexOf('\n\n');
    const headerEnd = crlfHeaderEnd >= 0 ? crlfHeaderEnd : lfHeaderEnd;
    if (headerEnd < 0) break;
    const separatorLength = crlfHeaderEnd >= 0 ? 4 : 2;
    const header = remaining.slice(0, headerEnd);
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) break;
    const contentLength = Number(match[1]);
    const bodyStart = headerEnd + separatorLength;
    if (Buffer.byteLength(remaining.slice(bodyStart), 'utf8') < contentLength) break;
    const body = Buffer.from(remaining.slice(bodyStart), 'utf8').subarray(0, contentLength).toString('utf8');
    const consumed = bodyStart + Buffer.byteLength(body, 'utf8');
    requests.push(JSON.parse(body) as JsonRecord);
    remaining = remaining.slice(consumed);
  }
  return { requests, remaining, framed: true };
}

function drainJsonLines(buffer: string): { requests: JsonRecord[]; remaining: string; framed: boolean } {
  const lines = buffer.split(/\r?\n/);
  const remaining = lines.pop() ?? '';
  const requests = lines.filter((line) => line.trim()).map((line) => JSON.parse(line) as JsonRecord);
  return { requests, remaining, framed: false };
}

function writeJsonRpcResponse(response: JsonRecord, options: { framed: boolean }) {
  const body = JSON.stringify(response);
  if (options.framed) process.stdout.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\r\n\r\n${body}`);
  else process.stdout.write(`${body}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  void runStdioServer(parseArgs(process.argv.slice(2)));
}

function parseArgs(argv: string[]): JsonRecord {
  const parsed: JsonRecord = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith('--')) continue;
    const key = arg.slice(2).replace(/-([a-z])/g, (_, character: string) => character.toUpperCase());
    const next = argv[index + 1];
    if (next && !next.startsWith('--')) { parsed[key] = next; index += 1; }
    else parsed[key] = true;
  }
  return parsed;
}
