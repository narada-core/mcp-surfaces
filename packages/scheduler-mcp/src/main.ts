#!/usr/bin/env node
import { buildGuidanceResult } from './guidance.js';
import { guidanceToolDefinition } from './guidance.js';
import { createHash } from 'node:crypto';
import { randomUUID } from 'node:crypto';
import { spawn, type ChildProcess } from 'node:child_process';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { extname, relative, resolve, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import {
  createScheduledCommandLaunchPlan,
  createScheduledCommandPlaceholderPlan,
  decodeScheduledCommandLaunchArguments,
  scheduledCommandEntrypoint,
  scheduledCommandSourceEntrypoint,
  type ScheduledCommandLaunchPlan,
  type ScheduledCommandPlaceholderPlan,
} from '@narada-core/process-launch-posture';
import {
  callSchedulerActivationTool,
  closeSchedulerActivationRuntime,
  isSchedulerActivationMutation,
  isSchedulerActivationTool,
  listSchedulerActivationTools,
  type SchedulerActivationRuntimeState,
} from './activation-mcp.js';

const SERVER_NAME = 'scheduler-mcp';
const SERVER_VERSION = '0.1.0';
const PROTOCOL_VERSION = '2024-11-05';

type JsonRecord = Record<string, unknown>;

export type SchedulerState = SchedulerActivationRuntimeState & {
  allowedRoots: string[];
  implementationId: string;
};

export type SchedulerRuntimeStatus = {
  schema: 'narada.scheduler_runtime_status.v1';
  status: 'fresh' | 'stale' | 'unavailable';
  implementation_id: string;
  runtime_entrypoint: string;
  source_entrypoint: string;
  source_mtime: string | null;
  runtime_mtime: string | null;
  components: Array<{
    name: string;
    runtime_path: string;
    source_path: string;
    runtime_mtime: string | null;
    source_mtime: string | null;
    status: 'fresh' | 'stale' | 'unavailable';
  }>;
  remediation: string | null;
};

const SCHTASKS_TIMEOUT_MS = 10_000;
const TRANSIENT_SCRIPT_EXTENSIONS = new Set(['.ps1', '.psm1', '.js', '.mjs', '.cjs', '.ts']);
const TRANSIENT_SCRIPT_SUFFIX = /\.(?:ps1|psm1|js|mjs|cjs|ts)(?:["'\s]|$)/i;
const WRAPPER_EXTENSIONS = new Set(['.cmd', '.bat']);
const TRANSIENT_WRAPPER_PATH = /(^|\/)\.ai\/(?:tmp|temp)(?:\/|$)/i;

function terminateProcessTree(child: ChildProcess) {
  if (!child.pid) return;
  if (process.platform === 'win32') {
    const killer = spawn('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], {
      windowsHide: true,
      stdio: 'ignore',
    });
    killer.on('error', () => {});
    return;
  }
  child.kill('SIGTERM');
}

export function buildScheduledTaskLaunchPlan(
  command: string,
  cmdArgs?: string | null,
  options: { require_available?: boolean } = {},
): ScheduledCommandLaunchPlan {
  let plan: ScheduledCommandLaunchPlan;
  try {
    plan = createScheduledCommandLaunchPlan(command, cmdArgs ?? '', {
      platform: 'win32',
      env: process.env,
    });
  } catch (error) {
    throw diagnosticError('scheduler_no_window_launch_invalid', 'scheduler_no_window_launch_invalid', {
      error: error instanceof Error ? error.message : String(error),
    });
  }
  if (options.require_available === true) {
    if (process.platform !== 'win32') {
      throw diagnosticError('scheduler_windows_only', `scheduler_windows_only:${process.platform}`);
    }
    if (!existsSync(plan.launcher_path)) {
      throw diagnosticError('scheduler_no_window_launcher_unavailable', 'scheduler_no_window_launcher_unavailable', {
        launcher_path: plan.launcher_path,
        remediation: 'Build @narada-core/process-launch-posture before mutating scheduled tasks.',
      });
    }
  }
  return plan;
}

function buildScheduledTaskPlaceholderPlan(): ScheduledCommandPlaceholderPlan {
  const plan = createScheduledCommandPlaceholderPlan({ platform: 'win32', env: process.env });
  if (process.platform !== 'win32') {
    throw diagnosticError('scheduler_windows_only', `scheduler_windows_only:${process.platform}`);
  }
  if (!existsSync(plan.launcher_path)) {
    throw diagnosticError('scheduler_no_window_launcher_unavailable', 'scheduler_no_window_launcher_unavailable', {
      launcher_path: plan.launcher_path,
      remediation: 'Build @narada-core/process-launch-posture before mutating scheduled tasks.',
    });
  }
  return plan;
}

function actionTokens(value: string | null | undefined): string[] {
  if (!value) return [];
  return value.match(/"[^"]*"|'[^']*'|\S+/g)?.map((token) => token.replace(/^['"]|['"]$/g, '')) ?? [];
}

function isPathWithinRoot(root: string, candidate: string): boolean {
  const remainder = relative(root, candidate);
  return remainder === '' || (!remainder.startsWith('..') && !remainder.includes(':'));
}

const RUNTIME_ENTRYPOINT = fileURLToPath(import.meta.url);
const SOURCE_ENTRYPOINT = resolve(dirname(RUNTIME_ENTRYPOINT), '../../src/main.ts');
const SCHEDULED_COMMAND_LAUNCHER = scheduledCommandEntrypoint({ platform: 'win32' });
const SCHEDULED_COMMAND_SOURCE = scheduledCommandSourceEntrypoint({ platform: 'win32' });
const SCHEDULER_RUNTIME_COMPONENTS = [
  { name: 'main', runtime: RUNTIME_ENTRYPOINT, source: SOURCE_ENTRYPOINT },
  { name: 'activation_mcp', runtime: resolve(dirname(RUNTIME_ENTRYPOINT), 'activation-mcp.js'), source: resolve(dirname(SOURCE_ENTRYPOINT), 'activation-mcp.ts') },
  { name: 'activation_store', runtime: resolve(dirname(RUNTIME_ENTRYPOINT), 'activation-store.js'), source: resolve(dirname(SOURCE_ENTRYPOINT), 'activation-store.ts') },
  ...(process.platform === 'win32' && SCHEDULED_COMMAND_LAUNCHER && SCHEDULED_COMMAND_SOURCE
    ? [{ name: 'scheduled_command_launcher', runtime: SCHEDULED_COMMAND_LAUNCHER, source: SCHEDULED_COMMAND_SOURCE }]
    : []),
] as const;
const FRESHNESS_SKEW_MS = 1000;

function fileFingerprint(path: string): string {
  if (!existsSync(path)) return 'missing';
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

export function schedulerRuntimeStatus(): SchedulerRuntimeStatus {
  const components = SCHEDULER_RUNTIME_COMPONENTS.map((component) => {
    const runtimeExists = existsSync(component.runtime);
    const sourceExists = existsSync(component.source);
    const runtimeMtime = runtimeExists ? statSync(component.runtime).mtimeMs : null;
    const sourceMtime = sourceExists ? statSync(component.source).mtimeMs : null;
    const fresh = runtimeMtime !== null && sourceMtime !== null && sourceMtime <= runtimeMtime + FRESHNESS_SKEW_MS;
    return {
      name: component.name,
      runtime_path: component.runtime,
      source_path: component.source,
      runtime_mtime: runtimeMtime === null ? null : new Date(runtimeMtime).toISOString(),
      source_mtime: sourceMtime === null ? null : new Date(sourceMtime).toISOString(),
      status: (fresh ? 'fresh' : runtimeExists && sourceExists ? 'stale' : 'unavailable') as 'fresh' | 'stale' | 'unavailable',
    };
  });
  const implementationId = createHash('sha256')
    .update(`${SERVER_VERSION}\0${SCHEDULER_RUNTIME_COMPONENTS.map((component) => `${component.name}\0${fileFingerprint(component.runtime)}\0${fileFingerprint(component.source)}`).join('\0')}`)
    .digest('hex');
  const status = components.some((component) => component.status === 'unavailable')
    ? 'unavailable'
    : components.some((component) => component.status === 'stale') ? 'stale' : 'fresh';
  const main = components[0]!;
  return {
    schema: 'narada.scheduler_runtime_status.v1',
    status,
    implementation_id: implementationId,
    runtime_entrypoint: RUNTIME_ENTRYPOINT,
    source_entrypoint: SOURCE_ENTRYPOINT,
    source_mtime: main.source_mtime,
    runtime_mtime: main.runtime_mtime,
    components,
    remediation: status === 'fresh' ? null : 'Rebuild the scheduler package and restart the scheduler MCP surface before mutating scheduled tasks.',
  };
}

export function createServerState(options: JsonRecord = {}): SchedulerState {
  const allowedRoots = optionList(options.allowedRoot ?? options.allowedRoots).map((root) => resolve(root));
  return {
    allowedRoots,
    siteRoot: allowedRoots[0] ?? null,
    activationStore: null,
    implementationId: schedulerRuntimeStatus().implementation_id,
  };
}

function assertSchedulerMutationReady(args: JsonRecord, state: SchedulerState): void {
  const current = schedulerRuntimeStatus();
  if (current.status !== 'fresh') {
    throw diagnosticError('scheduler_runtime_stale', 'scheduler_runtime_stale', current);
  }
  if (current.implementation_id !== state.implementationId) {
    throw diagnosticError('scheduler_runtime_changed_during_session', 'scheduler_runtime_changed_during_session', {
      expected_implementation_id: state.implementationId,
      actual_implementation_id: current.implementation_id,
      remediation: 'Restart the scheduler MCP surface and retry with its current implementation_id.',
    });
  }
  const supplied = requiredString(args.implementation_id, 'scheduler_implementation_id_required');
  if (supplied !== current.implementation_id) {
    throw diagnosticError('scheduler_implementation_id_mismatch', 'scheduler_implementation_id_mismatch', {
      supplied_implementation_id: supplied,
      expected_implementation_id: current.implementation_id,
      remediation: 'Call scheduler_runtime_status and pass its implementation_id unchanged to the mutation.',
    });
  }
}

export function scheduledActionPolicyReasons(command: string, cmdArgs: string | null | undefined, workingDir: string | null | undefined, state: SchedulerState): string[] {
  const reasons: string[] = [];
  const executable = command.trim().replace(/^['"]|['"]$/g, '').replaceAll('\\', '/').split('/').at(-1)?.toLowerCase() ?? '';
  if (executable === 'cmd' || executable === 'cmd.exe') {
    reasons.push(`scheduler_shell_action_disallowed:${command}`);
  }
  for (const value of [command, ...actionTokens(cmdArgs)]) {
    const normalized = value.replaceAll('\\', '/');
    const cleanValue = normalized.replace(/^['"]|['"]$/g, '');
    const extension = extname(cleanValue).toLowerCase();
    if (!WRAPPER_EXTENSIONS.has(extension)) continue;
    const candidate = resolve(workingDir ?? state.allowedRoots[0] ?? process.cwd(), cleanValue);
    const canonicalRepositoryWrapper = !TRANSIENT_WRAPPER_PATH.test(normalized)
      && state.allowedRoots.length > 0
      && state.allowedRoots.some((root) => isPathWithinRoot(root, candidate))
      && existsSync(candidate)
      && statSync(candidate).isFile();
    if (!canonicalRepositoryWrapper) reasons.push(`scheduler_transient_wrapper_refused:${value}`);
  }
  for (const value of [command, cmdArgs ?? '']) {
    const normalized = value.replaceAll('\\', '/');
    const extension = extname(normalized.replace(/^['"]|['"]$/g, '')).toLowerCase();
    if ((TRANSIENT_SCRIPT_EXTENSIONS.has(extension) || TRANSIENT_SCRIPT_SUFFIX.test(normalized))
      && /(^|\/)\.ai\/(?:tmp|temp)(?:\/|$)/i.test(normalized)) {
      reasons.push(`scheduler_transient_script_path_refused:${value}`);
    }
  }
  if (workingDir && state.allowedRoots.length > 0) {
    const resolvedWorkingDir = resolve(workingDir);
    const withinAllowedRoot = state.allowedRoots.some((root) => {
      const remainder = relative(root, resolvedWorkingDir);
      return remainder === '' || (!remainder.startsWith('..') && !remainder.includes(':'));
    });
    if (!withinAllowedRoot) {
      reasons.push(`scheduler_working_dir_outside_allowed_root:${workingDir}`);
    }
  }
  return [...new Set(reasons)];
}

export function assertScheduledActionAllowed(command: string, cmdArgs: string | null | undefined, workingDir: string | null | undefined, state: SchedulerState): void {
  const reasons = scheduledActionPolicyReasons(command, cmdArgs, workingDir, state);
  if (reasons.length > 0) {
    throw diagnosticError('scheduler_action_refused', 'scheduler_action_refused', {
      refusal_reasons: reasons,
      remediation: 'Schedule the owning executable directly from an allowed root. Do not use cmd, .cmd/.bat wrappers, or scripts staged under .ai/tmp or .ai/temp. Use structured_command_start or the owning MCP surface for governed execution and preserve its execution_ref as evidence.',
    });
  }
}

export async function handleRequest(request: JsonRecord, state: SchedulerState) {
  if (!request.id && typeof request.method === 'string' && request.method.startsWith('notifications/')) return null;
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
  try {
    let buffer = '';
    let sawFramedInput = false;
    process.stdin.setEncoding('utf8');
    for await (const chunk of process.stdin) {
      buffer += chunk;
      const drained = buffer.includes('Content-Length:')
        ? drainJsonRpcFrames(buffer)
        : drainJsonLines(buffer);
      sawFramedInput ||= drained.framed;
      buffer = drained.remaining;
      for (const request of drained.requests) {
        const response = await handleRequest(request, state);
        if (response) writeJsonRpcResponse(response, { framed: sawFramedInput });
      }
    }
  } finally {
    closeSchedulerActivationRuntime(state);
  }
}

async function dispatchMethod(method: string, params: JsonRecord, state: SchedulerState) {
  switch (method) {
    case 'initialize':
      return {
        protocolVersion: params.protocolVersion ?? PROTOCOL_VERSION,
        capabilities: { tools: {} },
        serverInfo: { name: SERVER_NAME, version: SERVER_VERSION },
      };
    case 'tools/list':
      return { tools: listTools() };
    case 'tools/call':
      return await callTool(params, state);
    default:
      throw diagnosticError('unsupported_mcp_method', `unsupported_mcp_method:${method}`);
  }
}

export function listTools() {
  return [
    guidanceToolDefinition(),
    {
      name: 'scheduler_runtime_status',
      description: 'Report the scheduler implementation identity required for safe mutations and whether the running package is fresh relative to its source.',
      inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      annotations: { title: 'scheduler_runtime_status', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'scheduler_task_list',
      description: 'List scheduled tasks, optionally filtered by folder path.',
      inputSchema: {
        type: 'object',
        properties: {
          folder: { type: 'string', description: 'Task folder path, e.g. \\Narada. Defaults to \\' },
          limit: { type: 'number', default: 50 },
        },
        additionalProperties: false,
      },
      annotations: { title: 'scheduler_task_list', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'scheduler_task_show',
      description: 'Show full details of one scheduled task.',
      inputSchema: {
        type: 'object',
        properties: {
          task_name: { type: 'string', description: 'Full task path, e.g. \\Narada\\MyTask.' },
        },
        required: ['task_name'],
        additionalProperties: false,
      },
      annotations: { title: 'scheduler_task_show', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'scheduler_task_create',
      description: 'Create a new scheduled task whose target is always launched through the native no-console actuator.',
      inputSchema: {
        type: 'object',
        properties: {
          task_name: { type: 'string', description: 'Task path, e.g. \\Narada\\MyTask.' },
          command: { type: 'string', description: 'Target executable path or command. Scheduler stores it behind the native CREATE_NO_WINDOW actuator.' },
          arguments: { type: 'string', description: 'Command-line arguments.' },
          working_dir: { type: 'string', description: 'Start-in directory. When supplied, the MCP applies it through the Task Scheduler action WorkingDirectory property after creation.' },
          schedule: { type: 'string', enum: ['daily', 'hourly', 'at_startup', 'at_logon', 'once'], description: 'Trigger schedule type.' },
          start_time: { type: 'string', description: 'HH:mm start time (for daily/hourly/once).' },
          interval_minutes: { type: 'number', description: 'Repeat interval in minutes (for hourly).' },
          execution_time_limit_seconds: { type: 'integer', minimum: 1, maximum: 86400, description: 'Hard Task Scheduler wall-time limit.' },
          multiple_instances: { type: 'string', enum: ['ignore_new', 'parallel', 'queue', 'stop_existing'], description: 'Task Scheduler overlap policy.' },
          dry_run: { type: 'boolean', description: 'Validate policy and return the exact runnable-principal, battery, action, and schedule plan without creating a task.' },
          implementation_id: { type: 'string', description: 'Current implementation_id returned by scheduler_runtime_status.' },
        },
        required: ['task_name', 'command', 'schedule', 'implementation_id'],
        additionalProperties: false,
      },
      annotations: { title: 'scheduler_task_create', readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'scheduler_task_delete',
      description: 'Delete a scheduled task.',
      inputSchema: {
        type: 'object',
        properties: {
          task_name: { type: 'string', description: 'Full task path to delete.' },
          implementation_id: { type: 'string', description: 'Current implementation_id returned by scheduler_runtime_status.' },
        },
        required: ['task_name', 'implementation_id'],
        additionalProperties: false,
      },
      annotations: { title: 'scheduler_task_delete', readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'scheduler_task_update_action',
      description: 'Update only the target action for an existing scheduled task, preserving triggers and enabled state while enforcing native no-console execution.',
      inputSchema: {
        type: 'object',
        properties: {
          task_name: { type: 'string', description: 'Full task path to update.' },
          command: { type: 'string', description: 'Executable path or command.' },
          arguments: { type: 'string', description: 'Command-line arguments.' },
          working_dir: { type: 'string', description: 'Start-in directory applied through the Task Scheduler action WorkingDirectory property; never emulated with a shell wrapper.' },
          execution_time_limit_seconds: { type: 'integer', minimum: 1, maximum: 86400, description: 'Hard Task Scheduler wall-time limit.' },
          multiple_instances: { type: 'string', enum: ['ignore_new', 'parallel', 'queue', 'stop_existing'], description: 'Task Scheduler overlap policy.' },
          dry_run: { type: 'boolean', description: 'Return the planned PowerShell action mutation and a non-authoritative schtasks preview without mutating.' },
          implementation_id: { type: 'string', description: 'Current implementation_id returned by scheduler_runtime_status.' },
        },
        required: ['task_name', 'command', 'implementation_id'],
        additionalProperties: false,
      },
      annotations: { title: 'scheduler_task_update_action', readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'scheduler_task_enable',
      description: 'Enable a scheduled task.',
      inputSchema: {
        type: 'object',
        properties: {
          task_name: { type: 'string' },
          implementation_id: { type: 'string', description: 'Current implementation_id returned by scheduler_runtime_status.' },
        },
        required: ['task_name', 'implementation_id'],
        additionalProperties: false,
      },
      annotations: { title: 'scheduler_task_enable', readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'scheduler_task_disable',
      description: 'Disable a scheduled task.',
      inputSchema: {
        type: 'object',
        properties: {
          task_name: { type: 'string' },
          implementation_id: { type: 'string', description: 'Current implementation_id returned by scheduler_runtime_status.' },
        },
        required: ['task_name', 'implementation_id'],
        additionalProperties: false,
      },
      annotations: { title: 'scheduler_task_disable', readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'scheduler_task_stop',
      description: 'Stop the currently running instance of a scheduled task without changing its registration.',
      inputSchema: {
        type: 'object',
        properties: {
          task_name: { type: 'string' },
          implementation_id: { type: 'string', description: 'Current implementation_id returned by scheduler_runtime_status.' },
        },
        required: ['task_name', 'implementation_id'],
        additionalProperties: false,
      },
      annotations: { title: 'scheduler_task_stop', readOnlyHint: false, destructiveHint: true, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'scheduler_task_run',
      description: 'Run a scheduled task immediately.',
      inputSchema: {
        type: 'object',
        properties: {
          task_name: { type: 'string' },
          implementation_id: { type: 'string', description: 'Current implementation_id returned by scheduler_runtime_status.' },
        },
        required: ['task_name', 'implementation_id'],
        additionalProperties: false,
      },
      annotations: { title: 'scheduler_task_run', readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'scheduler_task_history',
      description: 'Show run history for a scheduled task.',
      inputSchema: {
        type: 'object',
        properties: {
          task_name: { type: 'string' },
          limit: { type: 'number', default: 20 },
        },
        required: ['task_name'],
        additionalProperties: false,
      },
      annotations: { title: 'scheduler_task_history', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    ...listSchedulerActivationTools(),
  ];
}

async function callTool(params: JsonRecord, state: SchedulerState) {
  const name = String(params.name ?? '');
  const args = asRecord(params.arguments);
  let result: JsonRecord;
  if (isSchedulerActivationTool(name)) {
    if (isSchedulerActivationMutation(name)) assertSchedulerMutationReady(args, state);
    result = callSchedulerActivationTool(name, args, state);
    return { content: [{ type: 'text', text: renderResult(result) }], structuredContent: result };
  }
  switch (name) {
    case 'scheduler_guidance': result = buildGuidanceResult(args); break;
    case 'scheduler_runtime_status': result = schedulerRuntimeStatus(); break;
    case 'scheduler_task_list': result = await schedulerTaskList(args, state); break;
    case 'scheduler_task_show': result = await schedulerTaskShow(args, state); break;
    case 'scheduler_task_create': result = await schedulerTaskCreate(args, state); break;
    case 'scheduler_task_delete': result = await schedulerTaskDelete(args, state); break;
    case 'scheduler_task_update_action': result = await schedulerTaskUpdateAction(args, state); break;
    case 'scheduler_task_enable': result = await schedulerTaskEnable(args, state); break;
    case 'scheduler_task_disable': result = await schedulerTaskDisable(args, state); break;
    case 'scheduler_task_stop': result = await schedulerTaskStop(args, state); break;
    case 'scheduler_task_run': result = await schedulerTaskRun(args, state); break;
    case 'scheduler_task_history': result = await schedulerTaskHistory(args, state); break;
    default: throw diagnosticError('unknown_tool', `unknown_tool:${name}`, { tool_name: name });
  }
  return { content: [{ type: 'text', text: renderResult(result) }], structuredContent: result };
}

async function schtasks(args: string[], timeoutMs = SCHTASKS_TIMEOUT_MS): Promise<{ stdout: string; stderr: string; exitCode: number; timedOut?: boolean }> {
  return new Promise((resolveExecution) => {
    const child = spawn('schtasks.exe', args, { windowsHide: true });
    let stdout = '';
    let stderr = '';
    let settled = false;
    const finish = (result: { stdout: string; stderr: string; exitCode: number; timedOut?: boolean }) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolveExecution(result);
    };
    const timer = setTimeout(() => {
      stderr = `${stderr}\nschtasks.exe timed out after ${timeoutMs}ms`.trim();
      terminateProcessTree(child);
      finish({ stdout, stderr, exitCode: -2, timedOut: true });
    }, timeoutMs);
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.on('close', (code) => {
      finish({ stdout, stderr, exitCode: code ?? -1 });
    });
    child.on('error', (err) => {
      finish({ stdout, stderr: `${stderr}\n${err.message}`, exitCode: -1 });
    });
  });
}

export function buildScheduledTaskMutationScript(): string {
  return [
    '$ErrorActionPreference = "Stop"',
    '$execute = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_EXECUTE")',
    '$arguments = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_ARGUMENTS")',
    '$workingDirectory = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_WORKING_DIR")',
    '$executionLimitSeconds = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_EXECUTION_LIMIT_SECONDS")',
    '$multipleInstances = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_MULTIPLE_INSTANCES")',
    '$taskName = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_NAME")',
    '$taskPath = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_PATH")',
    '$existingTask = Get-ScheduledTask -TaskName $taskName -TaskPath $taskPath',
    '$wasDisabled = [string]$existingTask.State -eq "Disabled"',
    '$principalUser = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name',
    '$principal = New-ScheduledTaskPrincipal -UserId $principalUser -LogonType InteractiveToken -RunLevel Limited',
    'if ([string]::IsNullOrWhiteSpace($workingDirectory)) { if ([string]::IsNullOrWhiteSpace($arguments)) { $action = New-ScheduledTaskAction -Execute $execute } else { $action = New-ScheduledTaskAction -Execute $execute -Argument $arguments } } else { if ([string]::IsNullOrWhiteSpace($arguments)) { $action = New-ScheduledTaskAction -Execute $execute -WorkingDirectory $workingDirectory } else { $action = New-ScheduledTaskAction -Execute $execute -Argument $arguments -WorkingDirectory $workingDirectory } }',
    '$settingsArguments = @{ Hidden = $true; AllowStartIfOnBatteries = $true; DontStopIfGoingOnBatteries = $true }',
    'if ($wasDisabled) { $settingsArguments.Disable = $true }',
    'if (-not [string]::IsNullOrWhiteSpace($executionLimitSeconds)) { $settingsArguments.ExecutionTimeLimit = [TimeSpan]::FromSeconds([int]$executionLimitSeconds) }',
    'if (-not [string]::IsNullOrWhiteSpace($multipleInstances)) { $settingsArguments.MultipleInstances = $multipleInstances }',
    '$settings = New-ScheduledTaskSettingsSet @settingsArguments',
    'Set-ScheduledTask -TaskName $taskName -TaskPath $taskPath -Action $action -Settings $settings -Principal $principal | Out-Null',
  ].join(';');
}

export function splitScheduledTaskPath(value: string): { taskName: string; taskPath: string } {
  const normalized = value.trim().replace(/\//g, '\\');
  const full = normalized.startsWith('\\') ? normalized : `\\${normalized}`;
  const separator = full.lastIndexOf('\\');
  const taskName = full.slice(separator + 1);
  if (!taskName) throw diagnosticError('scheduler_requires_task_name', 'scheduler_requires_task_name');
  return {
    taskName,
    taskPath: full.slice(0, separator + 1) || '\\',
  };
}

async function setScheduledTaskAction(
  taskName: string,
  launchPlan: ScheduledCommandLaunchPlan,
  workingDir?: string | null,
  executionTimeLimitSeconds?: number | null,
  multipleInstances?: string | null,
  timeoutMs = SCHTASKS_TIMEOUT_MS,
): Promise<{ stdout: string; stderr: string; exitCode: number; timedOut?: boolean }> {
  const script = buildScheduledTaskMutationScript();
  const encodedCommand = Buffer.from(script, 'utf16le').toString('base64');
  const task = splitScheduledTaskPath(taskName);
  return new Promise((resolveExecution) => {
    const child = spawn('powershell.exe', [
      '-NoProfile',
      '-NonInteractive',
      '-ExecutionPolicy',
      'Bypass',
      '-EncodedCommand',
      encodedCommand,
    ], {
      windowsHide: true,
      env: {
        ...process.env,
        NARADA_SCHEDULER_TASK_NAME: task.taskName,
        NARADA_SCHEDULER_TASK_PATH: task.taskPath,
        NARADA_SCHEDULER_EXECUTE: launchPlan.launcher_path,
        NARADA_SCHEDULER_ARGUMENTS: launchPlan.launcher_arguments,
        NARADA_SCHEDULER_WORKING_DIR: workingDir ?? '',
        NARADA_SCHEDULER_EXECUTION_LIMIT_SECONDS: executionTimeLimitSeconds === null || executionTimeLimitSeconds === undefined ? '' : String(executionTimeLimitSeconds),
        NARADA_SCHEDULER_MULTIPLE_INSTANCES: multipleInstances ?? '',
      },
    });
    let stdout = '';
    let stderr = '';
    let settled = false;
    const finish = (result: { stdout: string; stderr: string; exitCode: number; timedOut?: boolean }) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolveExecution(result);
    };
    const timer = setTimeout(() => {
      stderr = (stderr + '\npowershell.exe timed out after ' + timeoutMs + 'ms').trim();
      terminateProcessTree(child);
      finish({ stdout, stderr, exitCode: -2, timedOut: true });
    }, timeoutMs);
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.on('close', (code) => finish({ stdout, stderr, exitCode: code ?? -1 }));
    child.on('error', (err) => finish({ stdout, stderr: stderr + '\n' + err.message, exitCode: -1 }));
  });
}

function parseCSV(csv: string): JsonRecord[] {
  const lines = csv.trim().split(/\r?\n/);
  if (lines.length < 2) return [];
  const headers = parseCSVLine(lines[0]);
  const rows: JsonRecord[] = [];
  for (let i = 1; i < lines.length; i++) {
    const values = parseCSVLine(lines[i]);
    if (values.length !== headers.length) continue;
    const row: JsonRecord = {};
    for (let j = 0; j < headers.length; j++) {
      row[headers[j]] = values[j];
    }
    rows.push(row);
  }
  return rows;
}

function parseCSVLine(line: string): string[] {
  const result: string[] = [];
  let current = '';
  let inQuotes = false;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (inQuotes) {
      if (ch === '"') {
        if (i + 1 < line.length && line[i + 1] === '"') {
          current += '"';
          i++;
        } else {
          inQuotes = false;
        }
      } else {
        current += ch;
      }
    } else {
      if (ch === '"') {
        inQuotes = true;
      } else if (ch === ',') {
        result.push(current.trim());
        current = '';
      } else {
        current += ch;
      }
    }
  }
  result.push(current.trim());
  return result;
}

function compactTask(row: JsonRecord): JsonRecord {
  return {
    task_name: row.TaskName,
    status: row.Status,
    schedule: row['Schedule Type'],
    next_run: row['Next Run Time'],
    last_run: row['Last Run Time'],
    last_result: row['Last Result'],
    command: row['Task To Run'],
  };
}

function compactTrigger(row: JsonRecord): JsonRecord {
  return {
    schedule: row['Schedule Type'],
    start_time: row['Start Time'],
    start_date: row['Start Date'],
    end_date: row['End Date'],
    days: row.Days,
    months: row.Months,
    repeat_every: row['Repeat: Every'],
    repeat_until_time: row['Repeat: Until: Time'],
    repeat_until_duration: row['Repeat: Until: Duration'],
    repeat_stop_if_still_running: row['Repeat: Stop If Still Running'],
    next_run: row['Next Run Time'],
  };
}

export function schedulerFailureDetails({ operation, exitCode, stdout = '', stderr = '', taskName = '', command = '', timedOut = false, timeoutMs = SCHTASKS_TIMEOUT_MS }: { operation: string; exitCode: number; stdout?: string; stderr?: string; taskName?: string; command?: string; timedOut?: boolean; timeoutMs?: number }): JsonRecord {
  const combined = `${stdout}\n${stderr}`.toLowerCase();
  const classification = timedOut
    ? 'scheduler_command_timed_out'
    : combined.includes('access is denied')
    ? 'requires_elevation'
    : combined.includes('folder') || combined.includes('cannot find')
      ? 'invalid_task_path_or_missing_task'
      : combined.includes('invalid') || exitCode === 2147500037
        ? 'invalid_arguments_or_unsupported_scheduler_option'
        : 'scheduler_command_failed';
  const operatorVerb = operation === 'update_action'
    ? 'Change'
    : operation === 'delete'
      ? 'Delete'
      : operation === 'stop'
        ? 'End'
        : operation === 'enable' || operation === 'disable' ? 'Change' : 'Create';
  const operatorArgs = operation === 'update_action'
    ? ` /TR ${quoteCmd(command)}`
    : operation === 'enable'
      ? ' /Enable'
      : operation === 'disable'
        ? ' /Disable'
        : operation === 'delete'
          ? ' /F'
          : command
            ? ` /TR ${quoteCmd(command)} /F`
            : '';
  const operatorCommand = operation !== 'update_action' && (command || ['delete', 'enable', 'disable', 'stop'].includes(operation))
    ? `schtasks.exe /${operatorVerb} /TN ${quoteCmd(taskName)}${operatorArgs}`
    : null;
  return {
    operation,
    exit_code: exitCode,
    classification,
    requires_elevation: classification === 'requires_elevation',
    timed_out: timedOut,
    timeout_ms: timeoutMs,
    task_name: taskName,
    stdout,
    stderr,
    operator_command: operatorCommand,
    operator_command_note: operation === 'update_action'
      ? 'The MCP mutates the action with PowerShell Set-ScheduledTask; any schtasks representation is preview-only and does not apply working_dir.'
      : null,
    remediation: classification === 'scheduler_command_timed_out'
      ? `The scheduler command exceeded its ${timeoutMs}ms bound and its process tree was terminated. Retry after confirming scheduler health; do not emulate the operation with an ungoverned wrapper.`
      : classification === 'requires_elevation'
      ? 'Run the equivalent scheduler command from an elevated PowerShell window, or use structured_command_elevated_window_execute for an explicit UAC prompt.'
      : classification === 'invalid_arguments_or_unsupported_scheduler_option'
        ? 'Inspect task_name, schedule, start_time, and command quoting. The MCP applies working_dir through the Task Scheduler action; do not emulate Start In with a Set-Location or shell wrapper.'
        : 'Inspect stdout/stderr and retry with a concrete task path such as \\TaskName.',
  };
}

function quoteCmd(value: string): string {
  return `"${String(value).replace(/"/g, '\\"')}"`;
}

function sameWindowsExecutablePath(left: string, right: string): boolean {
  const normalize = (value: string) => String(value)
    .trim()
    .replace(/^"|"$/g, '')
    .replace(/\//g, '\\')
    .toLowerCase();
  return normalize(left) === normalize(right);
}

export function compactScheduledTaskRows(rows: JsonRecord[]): JsonRecord[] {
  const grouped = new Map<string, JsonRecord>();
  for (const row of rows) {
    const taskName = String(row.TaskName ?? '');
    if (!taskName || taskName === 'TaskName') continue;
    const trigger = compactTrigger(row);
    const existing = grouped.get(taskName);
    if (existing) {
      (existing.triggers as JsonRecord[]).push(trigger);
      existing.trigger_count = (existing.triggers as JsonRecord[]).length;
      continue;
    }
    grouped.set(taskName, {
      ...compactTask(row),
      trigger_count: 1,
      triggers: [trigger],
    });
  }
  return [...grouped.values()];
}

async function schedulerTaskList(args: JsonRecord, _state: SchedulerState): Promise<JsonRecord> {
  const folder = optionalString(args.folder) ?? '\\';
  const limit = clamp(integer(args.limit, 50, 1, 500), 1, 500);
  const { stdout, stderr, exitCode, timedOut } = await schtasks(['/query', '/fo', 'CSV', '/v', '/tn', folder]);
  if (exitCode !== 0 && exitCode !== 1) throw diagnosticError('scheduler_query_failed', `scheduler_query_failed:${exitCode}`, schedulerFailureDetails({ operation: 'list', exitCode, stdout, stderr, timedOut }));
  const all = compactScheduledTaskRows(parseCSV(stdout));
  const items = all.slice(0, limit);
  return { items, count: items.length, folder };
}

async function schedulerTaskShow(args: JsonRecord, _state: SchedulerState): Promise<JsonRecord> {
  const taskName = requiredString(args.task_name, 'scheduler_requires_task_name');
  const { stdout, stderr, exitCode, timedOut } = await schtasks(['/query', '/fo', 'CSV', '/v', '/tn', taskName]);
  if (exitCode !== 0) {
    if (timedOut) throw diagnosticError('scheduler_query_timed_out', `scheduler_query_timed_out:${taskName}`, schedulerFailureDetails({ operation: 'show', exitCode, stdout, stderr, taskName, timedOut }));
    throw diagnosticError('scheduler_task_not_found', `scheduler_task_not_found:${taskName}`, { exitCode });
  }
  const rows = parseCSV(stdout);
  if (rows.length === 0) throw diagnosticError('scheduler_task_not_found', `scheduler_task_not_found:${taskName}`);
  const definition = await scheduledTaskDefinition(taskName);
  return { task: rows[0], task_compact: compactScheduledTaskRows(rows)[0] ?? null, task_definition: definition };
}

async function scheduledTaskDefinition(taskName: string): Promise<JsonRecord> {
  const task = splitScheduledTaskPath(taskName);
  const script = [
    '$ErrorActionPreference = "Stop"',
    '$taskName = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_NAME")',
    '$taskPath = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_PATH")',
    '$task = Get-ScheduledTask -TaskName $taskName -TaskPath $taskPath',
    '$action = @($task.Actions)[0]',
    '$limitSeconds = [System.Xml.XmlConvert]::ToTimeSpan([string]$task.Settings.ExecutionTimeLimit).TotalSeconds',
    '[ordered]@{ task_name = "$($task.TaskPath)$($task.TaskName)"; state = [string]$task.State; execute = [string]$action.Execute; arguments = [string]$action.Arguments; working_dir = [string]$action.WorkingDirectory; hidden = [bool]$task.Settings.Hidden; execution_time_limit_seconds = [int]$limitSeconds; multiple_instances = [string]$task.Settings.MultipleInstances } | ConvertTo-Json -Compress',
  ].join(';');
  const encodedCommand = Buffer.from(script, 'utf16le').toString('base64');
  const result = await new Promise<{ stdout: string; stderr: string; exitCode: number; timedOut?: boolean }>((resolveExecution) => {
    const child = spawn('powershell.exe', [
      '-NoProfile',
      '-NonInteractive',
      '-ExecutionPolicy',
      'Bypass',
      '-EncodedCommand',
      encodedCommand,
    ], {
      windowsHide: true,
      env: {
        ...process.env,
        NARADA_SCHEDULER_TASK_NAME: task.taskName,
        NARADA_SCHEDULER_TASK_PATH: task.taskPath,
      },
    });
    let stdout = '';
    let stderr = '';
    let settled = false;
    const finish = (value: { stdout: string; stderr: string; exitCode: number; timedOut?: boolean }) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolveExecution(value);
    };
    const timer = setTimeout(() => {
      terminateProcessTree(child);
      finish({ stdout, stderr, exitCode: -2, timedOut: true });
    }, SCHTASKS_TIMEOUT_MS);
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.on('close', (code) => finish({ stdout, stderr, exitCode: code ?? -1 }));
    child.on('error', (error) => finish({ stdout, stderr: `${stderr}\n${error.message}`, exitCode: -1 }));
  });
  if (result.exitCode !== 0) {
    throw diagnosticError('scheduler_task_definition_query_failed', `scheduler_task_definition_query_failed:${taskName}`, schedulerFailureDetails({
      operation: 'show',
      exitCode: result.exitCode,
      stdout: result.stdout,
      stderr: result.stderr,
      taskName,
      timedOut: result.timedOut,
    }));
  }
  const parsed = JSON.parse(result.stdout.trim()) as unknown;
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw diagnosticError('scheduler_task_definition_invalid', `scheduler_task_definition_invalid:${taskName}`);
  }
  return normalizeScheduledTaskDefinition(parsed as JsonRecord);
}

export function normalizeScheduledTaskDefinition(definition: JsonRecord): JsonRecord {
  const rawExecute = String(definition.execute ?? '');
  const rawArguments = String(definition.arguments ?? '');
  const expectedLauncher = scheduledCommandEntrypoint({ platform: 'win32', env: process.env });
  if (!expectedLauncher || !sameWindowsExecutablePath(rawExecute, expectedLauncher)) {
    return {
      ...definition,
      console_window_policy: 'unmanaged_direct_process',
      launcher_execute: null,
      launcher_arguments: null,
    };
  }
  try {
    const target = decodeScheduledCommandLaunchArguments(rawArguments);
    return {
      ...definition,
      execute: target.target_command,
      arguments: target.target_arguments,
      console_window_policy: 'native_create_no_window',
      launcher_execute: rawExecute,
      launcher_arguments: rawArguments,
    };
  } catch (error) {
    return {
      ...definition,
      console_window_policy: 'native_launcher_contract_invalid',
      launcher_execute: rawExecute,
      launcher_arguments: rawArguments,
      launcher_contract_error: error instanceof Error ? error.message : String(error),
    };
  }
}

async function schedulerTaskCreate(args: JsonRecord, state: SchedulerState): Promise<JsonRecord> {
  assertSchedulerMutationReady(args, state);
  const taskName = requiredString(args.task_name, 'scheduler_requires_task_name');
  const command = requiredString(args.command, 'scheduler_requires_command');
  const cmdArgs = optionalString(args.arguments);
  const workingDirInput = optionalString(args.working_dir);
  const workingDir = workingDirInput ? resolve(workingDirInput) : null;
  const schedule = requiredString(args.schedule, 'scheduler_requires_schedule');
  const executionTimeLimitSeconds = args.execution_time_limit_seconds === undefined
    ? null
    : integer(args.execution_time_limit_seconds, 0, 1, 86_400);
  const multipleInstances = schedulerMultipleInstances(args.multiple_instances);
  assertScheduledActionAllowed(command, cmdArgs, workingDir, state);
  const launchPlan = buildScheduledTaskLaunchPlan(command, cmdArgs, { require_available: args.dry_run !== true });
  const placeholderPlan = buildScheduledTaskPlaceholderPlan();
  const taskRun = buildTaskRunCommand(launchPlan.target_command, launchPlan.target_arguments);
  const placeholderTaskRun = buildTaskRunCommand(quoteCmd(placeholderPlan.launcher_path), placeholderPlan.launcher_arguments);
  const schArgs = ['/create', '/tn', taskName, '/tr', placeholderTaskRun, '/f'];
  schArgs.push(...buildCreateScheduleArgs(schedule, args));
  if (args.dry_run === true) {
    return {
      status: 'planned',
      task_name: taskName,
      schedule,
      command: taskRun,
      execute: launchPlan.target_command,
      arguments: launchPlan.target_arguments,
      working_dir: workingDir,
      principal: { source: 'current_windows_identity', logon_type: 'InteractiveToken', run_level: 'Limited' },
      battery_policy: { allow_start: true, stop_when_switching_to_battery: false },
      schtasks_preview_args: schArgs,
      mutation_method: 'schtasks_create_then_powershell_set_explicit_interactive_principal_and_settings',
    };
  }
  const { stdout, stderr, exitCode, timedOut } = await schtasks(schArgs);
  if (exitCode !== 0) throw diagnosticError('scheduler_create_failed', `scheduler_create_failed:${exitCode}`, schedulerFailureDetails({ operation: 'create', exitCode, stdout, stderr, taskName, command: placeholderTaskRun, timedOut }));
  const actionResult = await setScheduledTaskAction(
    taskName,
    launchPlan,
    workingDir,
    executionTimeLimitSeconds,
    multipleInstances,
  );
  if (actionResult.exitCode !== 0) {
    throw diagnosticError('scheduler_create_action_failed', `scheduler_create_action_failed:${actionResult.exitCode}`, {
      ...schedulerFailureDetails({ operation: 'create_action', exitCode: actionResult.exitCode, stdout: actionResult.stdout, stderr: actionResult.stderr, taskName, command: taskRun, timedOut: actionResult.timedOut }),
      task_created: true,
      mutation_method: 'powershell_set_scheduled_task_native_no_window_action_and_hidden_settings',
    });
  }
  return {
    status: 'created',
    task_name: taskName,
    schedule,
    command: taskRun,
    execute: launchPlan.target_command,
    arguments: launchPlan.target_arguments,
    launcher_execute: launchPlan.launcher_path,
    launcher_arguments: launchPlan.launcher_arguments,
    working_dir: workingDir,
    working_dir_applied: Boolean(workingDir),
    task_hidden: true,
    principal: { source: 'current_windows_identity', logon_type: 'InteractiveToken', run_level: 'Limited' },
    battery_policy: { allow_start: true, stop_when_switching_to_battery: false },
    execution_time_limit_seconds: executionTimeLimitSeconds,
    multiple_instances: multipleInstances,
    console_window_policy: launchPlan.console_window_policy,
    mutation_method: 'schtasks_create_then_powershell_set_explicit_interactive_principal_native_no_window_action_and_hidden_settings',
  };
}

async function schedulerTaskDelete(args: JsonRecord, state: SchedulerState): Promise<JsonRecord> {
  assertSchedulerMutationReady(args, state);
  const taskName = requiredString(args.task_name, 'scheduler_requires_task_name');
  const { stdout, stderr, exitCode, timedOut } = await schtasks(['/delete', '/tn', taskName, '/f']);
  if (exitCode !== 0) throw diagnosticError('scheduler_delete_failed', `scheduler_delete_failed:${exitCode}`, schedulerFailureDetails({ operation: 'delete', exitCode, stdout, stderr, taskName, timedOut }));
  return { status: 'deleted', task_name: taskName };
}

export function buildTaskRunCommand(command: string, cmdArgs?: string | null): string {
  return cmdArgs ? `${command} ${cmdArgs}` : command;
}

export function buildCreateScheduleArgs(schedule: string, args: JsonRecord): string[] {
  switch (schedule) {
    case 'daily':
      return ['/sc', 'daily', '/st', optionalString(args.start_time) ?? '09:00'];
    case 'hourly': {
      const interval = clamp(integer(args.interval_minutes, 60, 1, 1440), 1, 1440);
      if (interval < 60) return ['/sc', 'minute', '/mo', String(interval)];
      if (interval % 60 !== 0) return ['/sc', 'minute', '/mo', String(interval)];
      return ['/sc', 'hourly', '/mo', String(Math.max(1, interval / 60))];
    }
    case 'at_startup':
      return ['/sc', 'onstart'];
    case 'at_logon':
      return ['/sc', 'onlogon'];
    case 'once':
      return ['/sc', 'once', '/st', optionalString(args.start_time) ?? '09:00'];
    default:
      throw diagnosticError('scheduler_invalid_schedule', `scheduler_invalid_schedule:${schedule}`);
  }
}

async function schedulerTaskUpdateAction(args: JsonRecord, state: SchedulerState): Promise<JsonRecord> {
  assertSchedulerMutationReady(args, state);
  const taskName = requiredString(args.task_name, 'scheduler_requires_task_name');
  const command = requiredString(args.command, 'scheduler_requires_command');
  const cmdArgs = optionalString(args.arguments);
  const workingDirInput = optionalString(args.working_dir);
  const workingDir = workingDirInput ? resolve(workingDirInput) : null;
  const executionTimeLimitSeconds = args.execution_time_limit_seconds === undefined
    ? null
    : integer(args.execution_time_limit_seconds, 0, 1, 86_400);
  const multipleInstances = schedulerMultipleInstances(args.multiple_instances);
  assertScheduledActionAllowed(command, cmdArgs, workingDir, state);
  const launchPlan = buildScheduledTaskLaunchPlan(command, cmdArgs, { require_available: args.dry_run !== true });
  const taskRun = buildTaskRunCommand(launchPlan.target_command, launchPlan.target_arguments);
  const launcherTaskRun = buildTaskRunCommand(quoteCmd(launchPlan.launcher_path), launchPlan.launcher_arguments);
  const schArgs = ['/change', '/tn', taskName, '/tr', launcherTaskRun];
  if (args.dry_run === true) {
    return {
      status: 'planned',
      task_name: taskName,
      command: taskRun,
      execute: command,
      arguments: cmdArgs ?? '',
      mutation_method: 'powershell_set_scheduled_task_native_no_window_action',
      console_window_policy: launchPlan.console_window_policy,
      launcher_execute: launchPlan.launcher_path,
      launcher_arguments: launchPlan.launcher_arguments,
      schtasks_preview_args: schArgs,
      schtasks_preview_not_used_for_mutation: true,
      preserves_triggers: true,
      preserves_enabled_state: true,
      enabled_state_preservation: 'scheduled_task_settings_disable_flag',
      working_dir: workingDir,
      working_dir_applied: false,
      working_dir_would_apply: Boolean(workingDir),
      execution_time_limit_seconds: executionTimeLimitSeconds,
      multiple_instances: multipleInstances,
    };
  }
  const powershellResult = await setScheduledTaskAction(
    taskName,
    launchPlan,
    workingDir,
    executionTimeLimitSeconds,
    multipleInstances,
  );
  if (powershellResult.exitCode !== 0) {
    throw diagnosticError(
      'scheduler_update_action_failed',
      'scheduler_update_action_failed:' + powershellResult.exitCode,
      {
        ...schedulerFailureDetails({
          operation: 'update_action',
          exitCode: powershellResult.exitCode,
          stdout: powershellResult.stdout,
          stderr: powershellResult.stderr,
          taskName,
          command: taskRun,
          timedOut: powershellResult.timedOut,
        }),
        mutation_method: 'powershell_set_scheduled_task_native_no_window_action',
        schtasks_preview_args: schArgs,
        schtasks_preview_not_used_for_mutation: true,
      },
    );
  }
  return {
    status: 'updated',
    task_name: taskName,
    command: taskRun,
    preserves_triggers: true,
    preserves_enabled_state: true,
    enabled_state_preservation: 'scheduled_task_settings_disable_flag',
    mutation_method: 'powershell_set_scheduled_task_native_no_window_action',
    execute: launchPlan.target_command,
    arguments: launchPlan.target_arguments,
    launcher_execute: launchPlan.launcher_path,
    launcher_arguments: launchPlan.launcher_arguments,
    console_window_policy: launchPlan.console_window_policy,
    working_dir: workingDir,
    working_dir_applied: Boolean(workingDir),
    execution_time_limit_seconds: executionTimeLimitSeconds,
    multiple_instances: multipleInstances,
  };
}

async function schedulerTaskEnable(args: JsonRecord, state: SchedulerState): Promise<JsonRecord> {
  assertSchedulerMutationReady(args, state);
  const taskName = requiredString(args.task_name, 'scheduler_requires_task_name');
  const { stdout, stderr, exitCode, timedOut } = await schtasks(['/change', '/tn', taskName, '/enable']);
  if (exitCode !== 0) throw diagnosticError('scheduler_enable_failed', `scheduler_enable_failed:${exitCode}`, schedulerFailureDetails({ operation: 'enable', exitCode, stdout, stderr, taskName, timedOut }));
  return { status: 'enabled', task_name: taskName };
}

async function schedulerTaskDisable(args: JsonRecord, state: SchedulerState): Promise<JsonRecord> {
  assertSchedulerMutationReady(args, state);
  const taskName = requiredString(args.task_name, 'scheduler_requires_task_name');
  const { stdout, stderr, exitCode, timedOut } = await schtasks(['/change', '/tn', taskName, '/disable']);
  if (exitCode !== 0) throw diagnosticError('scheduler_disable_failed', `scheduler_disable_failed:${exitCode}`, schedulerFailureDetails({ operation: 'disable', exitCode, stdout, stderr, taskName, timedOut }));
  return { status: 'disabled', task_name: taskName };
}

async function schedulerTaskStop(args: JsonRecord, state: SchedulerState): Promise<JsonRecord> {
  assertSchedulerMutationReady(args, state);
  const taskName = requiredString(args.task_name, 'scheduler_requires_task_name');
  const { stdout, stderr, exitCode, timedOut } = await schtasks(['/end', '/tn', taskName]);
  if (exitCode !== 0) throw diagnosticError('scheduler_stop_failed', `scheduler_stop_failed:${exitCode}`, schedulerFailureDetails({ operation: 'stop', exitCode, stdout, stderr, taskName, timedOut }));
  return { status: 'stopped', task_name: taskName };
}

async function schedulerTaskRun(args: JsonRecord, state: SchedulerState): Promise<JsonRecord> {
  assertSchedulerMutationReady(args, state);
  const taskName = requiredString(args.task_name, 'scheduler_requires_task_name');
  const { stdout, stderr, exitCode, timedOut } = await schtasks(['/run', '/tn', taskName]);
  if (exitCode !== 0) throw diagnosticError('scheduler_run_failed', `scheduler_run_failed:${exitCode}`, schedulerFailureDetails({ operation: 'run', exitCode, stdout, stderr, taskName, timedOut }));
  return { status: 'started', task_name: taskName };
}

async function schedulerTaskHistory(args: JsonRecord, _state: SchedulerState): Promise<JsonRecord> {
  const taskName = requiredString(args.task_name, 'scheduler_requires_task_name');
  const limit = clamp(integer(args.limit, 20, 1, 200), 1, 200);
  const { stdout, stderr, exitCode, timedOut } = await schtasks(['/query', '/fo', 'CSV', '/v', '/tn', taskName]);
  if (exitCode !== 0 && exitCode !== 1) throw diagnosticError('scheduler_query_failed', `scheduler_query_failed:${exitCode}`, schedulerFailureDetails({ operation: 'history', exitCode, stdout, stderr, taskName, timedOut }));
  const rows = parseCSV(stdout);
  if (rows.length === 0) throw diagnosticError('scheduler_task_not_found', `scheduler_task_not_found:${taskName}`);
  const history = compactScheduledTaskRows(rows).slice(0, limit).map((task) => ({
    task_name: task.task_name,
    last_run: task.last_run,
    status: task.status,
    last_result: task.last_result,
    next_run: task.next_run,
    schedule: task.schedule,
    trigger_count: task.trigger_count,
    triggers: task.triggers,
  }));
  return { task_name: taskName, items: history, count: history.length };
}

function renderResult(result: JsonRecord): string {
  if (result.items !== undefined) {
    const items = result.items as JsonRecord[];
    const header = `scheduler: ${result.count ?? 0} tasks`;
    const lines = items.map((item) => {
      if (item.last_run) {
        return `  ${item.last_run}: ${item.status ?? ''} (${item.last_result ?? ''})`;
      }
      return `  ${item.task_name ?? ''} [${item.status ?? ''}] ${item.schedule ?? ''} next=${item.next_run ?? 'N/A'}`;
    });
    return [header, ...lines].join('\n');
  }
  if (result.task) {
    const t = result.task as JsonRecord;
    return [
      `task: ${t.TaskName ?? ''}`,
      `status: ${t.Status ?? ''}`,
      `schedule: ${t['Schedule Type'] ?? ''}`,
      `command: ${t['Task To Run'] ?? ''}`,
      `last_run: ${t['Last Run Time'] ?? ''} (${t['Last Result'] ?? ''})`,
      t.Comment ? `description: ${t.Comment}` : '',
    ].filter(Boolean).join('\n');
  }
  return `${result.status ?? 'ok'}: ${result.task_name ?? ''}`;
}

function requiredString(value: unknown, code: string, details: JsonRecord = {}): string {
  const text = String(value ?? '').trim();
  if (!text) throw diagnosticError(code, code, details);
  return text;
}

function optionalString(value: unknown): string | null {
  const text = String(value ?? '').trim();
  return text || null;
}

function integer(value: unknown, fallback: number, min: number, max: number): number {
  const parsed = Number(value ?? fallback);
  return Number.isFinite(parsed) ? Math.max(min, Math.min(max, Math.trunc(parsed))) : fallback;
}

function schedulerMultipleInstances(value: unknown): string | null {
  if (value === undefined || value === null || value === '') return null;
  const mapping: Record<string, string> = {
    ignore_new: 'IgnoreNew',
    parallel: 'Parallel',
    queue: 'Queue',
    stop_existing: 'StopExisting',
  };
  const normalized = String(value);
  const mapped = mapping[normalized];
  if (!mapped) throw diagnosticError('scheduler_multiple_instances_invalid', `scheduler_multiple_instances_invalid:${normalized}`);
  return mapped;
}

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Math.trunc(value)));
}

function optionList(value: unknown): string[] {
  if (value === undefined || value === null || value === true) return [];
  return Array.isArray(value) ? value.map(String).filter(Boolean) : [String(value)].filter(Boolean);
}

function asRecord(value: unknown): JsonRecord {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonRecord : {};
}

function diagnosticError(code: string, message: string = code, details: JsonRecord = {}) {
  const error = new Error(message);
  Object.assign(error, { codeName: code, details });
  return error;
}

function errorDiagnostic(error: unknown) {
  const record = asRecord(error);
  return {
    schema: 'narada.scheduler.error.v1',
    code: String(record.codeName ?? 'scheduler_error'),
    message: error instanceof Error ? error.message : String(error),
    details: asRecord(record.details),
  };
}

function drainJsonLines(buffer: string) {
  const lines = buffer.split(/\r?\n/);
  const remaining = lines.pop() ?? '';
  return {
    framed: false,
    remaining,
    requests: lines.filter((line) => line.trim()).map((line) => asRecord(JSON.parse(line))),
  };
}

function drainJsonRpcFrames(buffer: string) {
  const requests: JsonRecord[] = [];
  let remaining = buffer;
  while (true) {
    const headerEnd = remaining.indexOf('\r\n\r\n');
    if (headerEnd < 0) break;
    const header = remaining.slice(0, headerEnd);
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) break;
    const length = Number(match[1]);
    const bodyStart = headerEnd + 4;
    const bodyEnd = bodyStart + length;
    if (remaining.length < bodyEnd) break;
    requests.push(asRecord(JSON.parse(remaining.slice(bodyStart, bodyEnd))));
    remaining = remaining.slice(bodyEnd);
  }
  return { framed: true, remaining, requests };
}

function writeJsonRpcResponse(response: JsonRecord, { framed }: { framed: boolean }) {
  const body = JSON.stringify(response);
  if (framed) {
    process.stdout.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\r\n\r\n${body}`);
  } else {
    process.stdout.write(`${body}\n`);
  }
}

function parseArgs(argv: string[]) {
  const options: JsonRecord = {};
  const allowedRoots: string[] = [];
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--allowed-root') allowedRoots.push(argv[++i]);
    else throw new Error(`unknown_argument:${arg}`);
  }
  if (allowedRoots.length > 0) options.allowedRoots = allowedRoots;
  return options;
}

export { parseArgs };

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runStdioServer(parseArgs(process.argv.slice(2))).catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  });
}
