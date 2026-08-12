import { spawn } from 'node:child_process';
import { createWriteStream } from 'node:fs';
import { extname } from 'node:path';
import { admitWorkerAiProcessInvocation, releaseWorkerAiProcessInvocation, workerAiProcessRefusalError } from './ai-process-invocation.js';
import type { WorkerResolvedExecutionPolicy } from './worker-types.js';
export { parseLastMessage, parseResult, resultStatus } from './output-contract.js';
export type { WorkerBroadUnrelatedFailure, WorkerChange, WorkerExitInterview, WorkerObservedErgonomics, WorkerOutput, WorkerOutputParseResult, WorkerRunTerminalStatus, WorkerVerification, WorkerVerificationCommandClassification } from './output-contract.js';

export type ResolvedWorkerConfig = WorkerResolvedExecutionPolicy;

export type Invocation = { command: string; argv: string[]; cwd: string; environment: Record<string, string> };

const MAX_CONSECUTIVE_PROVIDER_RECONNECT_ERRORS = 5;
const PROVIDER_RECONNECT_ERROR_PATTERN = /\b(?:reconnect(?:ing|ed)?|stream disconnected|error sending request|failed to connect|websocket|socket|network)\b/i;
const DEFINITIVE_PROVIDER_ERROR_PATTERN = /\b(?:invalid[_ ]request(?:_error)?|authentication(?:_error)?|unauthorized|api key|rate[_ ]limit|quota|model not available|forbidden|\b401\b|\b403\b|\b429\b)\b/i;

export function runtimeName(): 'codex' {
  return 'codex';
}

export function supportsResume(): boolean {
  return true;
}

export function buildCodexArgv(options: {
  cwd: string;
  sandbox: string;
  schemaPath: string;
  lastMessagePath: string;
  workerSessionId?: string;
  ephemeral: boolean;
  skipGitRepoCheck: boolean;
  config: Record<string, string | number | boolean>;
  allowedRoots: string[];
}): string[] {
  const argv = ['exec', '--ignore-user-config', '--ignore-rules'];
  if (options.ephemeral) argv.push('--ephemeral');
  argv.push('-C', options.cwd);
  if (options.sandbox === 'workspace-write') argv.push('--approve-for-me');
  else argv.push('--sandbox', options.sandbox);
  for (const root of options.allowedRoots) if (root !== options.cwd) argv.push('--add-dir', root);
  argv.push('--json', '--output-schema', options.schemaPath, '-o', options.lastMessagePath);
  if (options.workerSessionId) argv.push('resume', options.workerSessionId);
  if (options.skipGitRepoCheck) argv.push('--skip-git-repo-check');
  for (const [key, value] of Object.entries(options.config).sort(([a], [b]) => a.localeCompare(b))) argv.push('-c', `${key}=${tomlValue(value)}`);
  argv.push('-');
  return argv;
}

export function buildInvocation(resolvedWorkerConfig: ResolvedWorkerConfig, environment: Record<string, string>): Invocation {
  return {
    command: resolvedWorkerConfig.command,
    argv: [...resolvedWorkerConfig.command_args, ...resolvedWorkerConfig.argv],
    cwd: resolvedWorkerConfig.cwd,
    environment: resolvedWorkerConfig.sandbox === 'workspace-write'
      ? { ...environment, CODEX_PERMISSION_PROFILE: ':workspace-write' }
      : environment,
  };
}

export function commandRequiresWindowsShell(command: string, platform: NodeJS.Platform = process.platform): boolean {
  if (platform !== 'win32') return false;
  const extension = extname(command).toLowerCase();
  return extension === '.cmd' || extension === '.bat' || extension === '.ps1';
}

function spawnCommandForInvocation(invocation: Invocation): { command: string; argv: string[]; shell: boolean } {
  const extension = extname(invocation.command).toLowerCase();
  if (process.platform === 'win32' && extension === '.ps1') {
    const command = `& ${powershellSingleQuoted(invocation.command)} ${invocation.argv.map(powershellSingleQuoted).join(' ')}`;
    return { command: 'powershell.exe', argv: ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', command], shell: false };
  }
  return { command: invocation.command, argv: invocation.argv, shell: commandRequiresWindowsShell(invocation.command) };
}

function powershellSingleQuoted(value: string): string {
  return `'${value.replace(/'/g, "''")}'`;
}

export async function runCodexInvocation(options: {
  invocation: Invocation;
  prompt: string;
  eventsPath: string;
  diagnosticPath: string;
  lastMessagePath: string;
  maxRunMs: number;
  abortSignal?: AbortSignal;
}): Promise<{ exit_code: number | null; signal: string | null; cancelled: boolean; worker_session_id: string | null; error: string | null; event_error: string | null; runtime_error: string | null }> {
  return new Promise((resolvePromise) => {
    if (options.abortSignal?.aborted) {
      resolvePromise({ exit_code: null, signal: null, cancelled: true, worker_session_id: null, error: null, event_error: null, runtime_error: null });
      return;
    }
    const admission = admitWorkerAiProcessInvocation(options.invocation, { projection: 'worker-delegation', purpose: 'codex_worker_runtime' });
    if (!admission.admitted) {
      resolvePromise({ exit_code: null, signal: null, cancelled: false, worker_session_id: null, error: workerAiProcessRefusalError(admission), event_error: null, runtime_error: null });
      return;
    }
    const spawnSpec = spawnCommandForInvocation(options.invocation);
    const child = spawn(spawnSpec.command, spawnSpec.argv, {
      cwd: options.invocation.cwd,
      env: options.invocation.environment,
      shell: spawnSpec.shell,
      windowsHide: true,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    const events = createWriteStream(options.eventsPath, { flags: 'a' });
    const diagnostics = createWriteStream(options.diagnosticPath, { flags: 'a' });
    let stdoutBuffer = '';
    let workerSessionId: string | null = null;
    let settled = false;
    let released = false;
    let cancelled = false;
    let eventError: string | null = null;
    let runtimeError: string | null = null;
    let runtimeFailure: string | null = null;
    let consecutiveProviderReconnectErrors = 0;

    const stopForRuntimeFailure = (message: string) => {
      if (settled || runtimeFailure) return;
      runtimeFailure = message;
      runtimeError = message;
      try { child.kill(); } catch { /* ignore */ }
    };

    const timer = setTimeout(() => {
      cancelled = true;
      try { child.kill(); } catch { /* ignore */ }
    }, options.maxRunMs);

    const abortHandler = () => {
      cancelled = true;
      try { child.kill(); } catch { /* ignore */ }
    };
    if (options.abortSignal) options.abortSignal.addEventListener('abort', abortHandler, { once: true });

    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      const text = String(chunk);
      events.write(text);
      stdoutBuffer += text;
      while (true) {
        const idx = stdoutBuffer.indexOf('\n');
        if (idx === -1) break;
        const line = stdoutBuffer.slice(0, idx).trim();
        stdoutBuffer = stdoutBuffer.slice(idx + 1);
        if (!line) continue;
        try {
          const parsed = JSON.parse(line);
          workerSessionId ||= findSessionId(parsed);
          const eventRuntimeError = findRuntimeError(parsed);
          if (!eventRuntimeError) {
            consecutiveProviderReconnectErrors = 0;
            continue;
          }
          if (PROVIDER_RECONNECT_ERROR_PATTERN.test(eventRuntimeError)) {
            consecutiveProviderReconnectErrors += 1;
            if (consecutiveProviderReconnectErrors >= MAX_CONSECUTIVE_PROVIDER_RECONNECT_ERRORS) {
              stopForRuntimeFailure(`provider reconnect failure after ${consecutiveProviderReconnectErrors} consecutive errors: ${eventRuntimeError}`);
            }
            continue;
          }
          consecutiveProviderReconnectErrors = 0;
          runtimeError ||= eventRuntimeError;
          if (DEFINITIVE_PROVIDER_ERROR_PATTERN.test(eventRuntimeError)) stopForRuntimeFailure(eventRuntimeError);
        } catch (error) {
          const message = error instanceof Error ? error.message : String(error);
          eventError ||= `invalid json event: ${message}`;
        }
      }
    });
    child.stderr.on('data', (chunk) => diagnostics.write(String(chunk)));
    child.on('error', (error) => {
      if (settled) return;
      settled = true;
      if (!released) { released = true; releaseWorkerAiProcessInvocation(admission, { exitCode: null, signal: null }); }
      clearTimeout(timer);
      events.end();
      diagnostics.end();
      if (options.abortSignal) options.abortSignal.removeEventListener('abort', abortHandler);
      resolvePromise({ exit_code: null, signal: null, cancelled: false, worker_session_id: workerSessionId, error: runtimeFailure ?? error.message, event_error: eventError, runtime_error: runtimeError });
    });
    child.on('close', (code, signal) => {
      if (settled) return;
      settled = true;
      if (!released) { released = true; releaseWorkerAiProcessInvocation(admission, { exitCode: code, signal }); }
      clearTimeout(timer);
      events.end();
      diagnostics.end();
      if (options.abortSignal) options.abortSignal.removeEventListener('abort', abortHandler);
      if (stdoutBuffer.trim()) eventError ||= 'unterminated json event';
      resolvePromise({ exit_code: code, signal, cancelled, worker_session_id: workerSessionId, error: runtimeFailure, event_error: eventError, runtime_error: runtimeError });
    });
    if (child.stdin) child.stdin.end(options.prompt, 'utf8');
  });
}


function findRuntimeError(value: unknown): string | null {
  if (!value || typeof value !== 'object') return null;
  const record = value as Record<string, unknown>;
  if (record.type === 'error') {
    const direct = stringField(record, 'message') ?? nestedErrorMessage(record.error);
    if (direct) return direct;
  }
  const nested = nestedErrorMessage(record.error);
  if (nested) return nested;
  for (const item of Object.values(record)) {
    const found = findRuntimeError(item);
    if (found) return found;
  }
  return null;
}

function nestedErrorMessage(value: unknown): string | null {
  if (typeof value === 'string' && value.trim()) return value.trim();
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  return stringField(record, 'message') ?? stringField(record, 'error') ?? null;
}

function stringField(record: Record<string, unknown>, key: string): string | null {
  return typeof record[key] === 'string' && record[key].trim() ? record[key].trim() : null;
}

function tomlValue(value: string | number | boolean): string {
  return typeof value === 'string' ? JSON.stringify(value) : String(value);
}

function findSessionId(value: unknown): string | null {
  if (!value || typeof value !== 'object') return null;
  const record = value as Record<string, unknown>;
  for (const key of ['thread_id', 'threadId', 'session_id', 'sessionId', 'conversation_id', 'conversationId']) {
    if (typeof record[key] === 'string' && record[key]) return record[key];
  }
  for (const nested of Object.values(record)) {
    const found = findSessionId(nested);
    if (found) return found;
  }
  return null;
}
