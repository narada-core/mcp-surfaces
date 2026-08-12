import { closeSync, existsSync, openSync, readFileSync, readSync, statSync } from 'node:fs';
import { resolve } from 'node:path';
import type { WorkerOutputParseResult } from './output-contract.js';
import { eventTimestamp, eventType, extractSessionEventEvidence, latestEventText, normalizeActivityKind } from './runtime-events.js';
import type { WorkerProgressPreview } from './worker-types.js';

export function buildRuntimeDiagnostics(options: {
  runtime: string;
  codexResult: { exit_code: number | null; signal: string | null; cancelled: boolean; error: string | null; event_error?: string | null; runtime_error?: string | null; assistant_extraction?: Record<string, unknown> };
  parsed: WorkerOutputParseResult;
  outcomeError: string | null;
  eventsPath: string;
  diagnosticPath: string;
}): Record<string, unknown> | undefined {
  if (!options.outcomeError && options.parsed.ok && !options.codexResult.error && !options.codexResult.event_error && !options.codexResult.runtime_error) return undefined;
  const diagnosticTail = readDiagnosticTail(options.diagnosticPath);
  const stdoutTail = readTextTail(options.eventsPath, 1200);
  const errorProvenance = buildErrorProvenance(options.codexResult, options.parsed, options.outcomeError, stdoutTail);
  const primaryError = typeof errorProvenance.primary_error === 'string' ? errorProvenance.primary_error : options.outcomeError;
  const phase = runtimeFailurePhase(options.codexResult, options.parsed, primaryError);
  const resultExtraction = runtimeResultExtraction(options.parsed);
  const sessionEventEvidence = extractSessionEventEvidence(options.eventsPath);
  return {
    schema: 'narada.worker.runtime_diagnostics.v1',
    phase,
    runtime: options.runtime,
    exit_code: options.codexResult.exit_code,
    signal: options.codexResult.signal,
    cancelled: options.codexResult.cancelled,
    error: primaryError,
    error_provenance: errorProvenance,
    transport_error: options.codexResult.error ?? null,
    provider_error: options.codexResult.runtime_error ?? null,
    artifact_error: errorProvenance.artifact_error,
    runtime_error: options.codexResult.runtime_error ?? null,
    event_error: options.codexResult.event_error ?? null,
    assistant_extraction: options.codexResult.assistant_extraction ?? null,
    session_event_evidence: sessionEventEvidence,
    result_extraction: resultExtraction,
    diagnostic_tail: diagnosticTail,
    stdout_tail: stdoutTail,
    tail_status: {
      diagnostic_tail_available: Boolean(diagnosticTail),
      stdout_tail_available: Boolean(stdoutTail),
      diagnostic_tail_limit_chars: 800,
      stdout_tail_limit_chars: 1200,
    },
    remediation: runtimeFailureRemediation(phase),
  };
}

function buildErrorProvenance(
  result: { error: string | null; event_error?: string | null; runtime_error?: string | null },
  parsed: WorkerOutputParseResult,
  outcomeError: string | null,
  stdoutTail: string | null,
): Record<string, unknown> {
  const transportError = result.error ?? null;
  const providerError = result.runtime_error ?? null;
  const eventError = result.event_error ?? null;
  const artifactError = parsed.ok === false ? `last_message.json:${parsed.reason}: ${parsed.message}` : null;
  const sources: Array<{ source: string; error: string | null }> = [
    { source: 'provider', error: providerError },
    { source: 'transport', error: transportError },
    { source: 'event_stream', error: eventError },
    { source: 'outcome', error: outcomeError },
    { source: 'artifact', error: artifactError },
  ];
  const primary = sources.find((item) => item.error);
  return {
    schema: 'narada.worker.error_provenance.v1',
    primary_error: primary?.error ?? null,
    primary_source: primary?.source ?? null,
    transport_error: transportError,
    provider_error: providerError,
    event_error: eventError,
    artifact_error: artifactError,
    outcome_error: outcomeError,
    observed_error_candidates: observedErrorCandidates(stdoutTail),
  };
}

function observedErrorCandidates(text: string | null): string[] {
  if (!text) return [];
  const candidates: string[] = [];
  const collect = (value: unknown, errorNode = false): void => {
    if (typeof value === 'string') {
      if (errorNode && value.trim()) candidates.push(value.trim());
      return;
    }
    if (!value || typeof value !== 'object' || Array.isArray(value)) return;
    const record = value as Record<string, unknown>;
    const type = String(record.type ?? '').toLowerCase();
    const nodeIsError = errorNode || type === 'error' || type.includes('failure') || type.includes('failed');
    for (const [key, nested] of Object.entries(record)) {
      const keyIsError = nodeIsError || /^(error|error_message|failure|failure_reason|reason)$/.test(key.toLowerCase());
      collect(nested, keyIsError);
    }
  };
  for (const line of text.split(/\r?\n/).map((item) => item.trim()).filter(Boolean)) {
    try { collect(JSON.parse(line)); }
    catch { if (/capacity|quota|rate.?limit|model.*(?:capacity|limit)/i.test(line)) candidates.push(line); }
  }
  return [...new Set(candidates.map((value) => value.replace(/\s+/g, ' ').slice(0, 400)))].slice(0, 12);
}

export function runtimeResultExtraction(parsed: WorkerOutputParseResult): Record<string, unknown> {
  if (parsed.ok === true) return { status: 'ok' };
  return { status: 'failed', reason: parsed.reason, message: parsed.message };
}

export function runtimeFailurePhase(
  result: { exit_code: number | null; error: string | null; event_error?: string | null; runtime_error?: string | null },
  parsed: WorkerOutputParseResult,
  outcomeError: string | null,
): string {
  const error = String(outcomeError ?? result.error ?? result.event_error ?? result.runtime_error ?? '').toLowerCase();
  if (error.includes('mcp runtime fault') || error.includes('mcp_runtime_fault') || error.includes('surface_registry_tool_not_declared') || error.includes('admission_required') || error.includes('tool_not_declared')) return 'mcp_tool_failure';
  if (error.includes('completed_without_assistant_output')) return 'completed_without_assistant_output';
  if (error.includes('exited_before_assistant_output')) return 'pre_first_assistant_failure';
  if (parsed.ok === false && parsed.reason === 'missing_file' && !result.error && !result.event_error && !result.runtime_error) return 'pre_first_assistant_failure';
  if (error.includes('without assistant_message') || error.includes('did_not_produce_last_message')) return 'pre_first_assistant_failure';
  if (parsed.ok === false && !result.error && !result.event_error && !result.runtime_error) return 'result_extraction_failure';
  if (result.exit_code === null && result.error) return 'startup_failure';
  if (result.event_error) return 'event_stream_failure';
  if (result.runtime_error) return 'runtime_reported_failure';
  return 'runtime_process_failure';
}

export function runtimeFailureRemediation(phase: string): string[] {
  if (phase === 'mcp_tool_failure') return ['Declare exact constraints.required_mcp_tools so the worker receives an explicit MCP allowlist, or keep the worker MCP-free.', 'Inspect stdout_tail and diagnostic_tail for the refused or unavailable tool name.'];
  if (phase === 'startup_failure') return ['Check runtime command availability and argv.', 'Inspect diagnostic_tail for process launch errors.'];
  if (phase === 'pre_first_assistant_failure') return ['Inspect stdout_tail for startup/session events and diagnostic_tail for stderr.', 'Retry with the same run_id only if the runtime supports resume; otherwise route to runtime repair.'];
  if (phase === 'completed_without_assistant_output') return ['Inspect runtime_diagnostics.assistant_extraction and session_event_evidence.terminal_events.', 'If assistant_message_seen is false, fix the runtime/provider to emit assistant_message before terminal events; if true but not extracted, repair assistant event text projection.'];
  if (phase === 'result_extraction_failure') return ['Inspect result_extraction.message and last_message.json artifact.', 'Repair worker output JSON shape or parser normalization.'];
  if (phase === 'event_stream_failure') return ['Inspect stdout_tail for malformed JSONL or protocol drift.', 'Verify the runtime is emitting raw JSONL events.'];
  if (phase === 'worker_delegation_exception') return ['Inspect worker-delegation exception and bounded tails.', 'Route to worker-delegation surface repair if the runtime process reached a normal terminal state.'];
  return ['Inspect runtime_diagnostics exit_code, signal, stdout_tail, and diagnostic_tail.', 'Use worker_run_status or worker_run_wait with the run_id for current persisted state.'];
}

export function compactRunError(run: Record<string, unknown>): string | null {
  const error = previewString(run.error, 120);
  const diagnosticTail = previewString(run.diagnostic_tail, 220);
  if (error && diagnosticTail) return `${error}: ${diagnosticTail}`;
  return error ?? diagnosticTail;
}

export function partialFailurePosture(run: Record<string, unknown>): Record<string, unknown> {
  const changes = Array.isArray(run.changes) ? run.changes : [];
  const deliverables = Array.isArray(run.deliverables) ? run.deliverables : [];
  const progress = asRecord(run.progress);
  const status = String(run.status ?? 'unknown');
  const errorClassification = typeof run.error_classification === 'string' ? run.error_classification : classifyRuntimeError(String(run.error ?? ''));
  const productive = changes.length > 0 || deliverables.length > 0 || Number(progress.event_count ?? 0) > 0;
  return {
    status: status === 'failed' || status === 'completed_with_errors' || status === 'cancelled' ? (productive ? 'productive_partial_failure' : 'unproductive_failure') : 'not_failed',
    error_classification: errorClassification,
    changed_file_count: changes.length,
    deliverable_count: deliverables.length,
    progress_event_count: typeof progress.event_count === 'number' ? progress.event_count : 0,
    provider_quota_limited: errorClassification === 'provider_rate_limited',
  };
}

export function enrichFailedRunDiagnostics(run: Record<string, unknown>): Record<string, unknown> {
  if (run.status !== 'failed') return run;
  const progress = asRecord(run.progress);
  if (progress.event_count !== 0 || run.worker_session_id) return run;
  const runDir = typeof run.run_dir === 'string' ? run.run_dir : null;
  if (!runDir) return run;
  const diagnosticTail = readDiagnosticTail(resolve(runDir, 'diagnostic.log'));
  if (!diagnosticTail) return run;
  return {
    ...run,
    diagnostic_tail: diagnosticTail,
    error_classification: classifyDiagnosticTail(diagnosticTail),
  };
}

export function readDiagnosticTail(path: string): string | null {
  return readTextTail(path, 800);
}

export function readTextTail(path: string, limit: number): string | null {
  if (!existsSync(path)) return null;
  try {
    const text = readFileSync(path, 'utf8').trim();
    if (!text) return null;
    const normalized = text.replace(/\s+/g, ' ').trim();
    return normalized.length <= limit ? normalized : normalized.slice(-limit);
  } catch {
    return null;
  }
}

export function readJsonPreview(path: string): Record<string, unknown> | null {
  if (!existsSync(path)) return null;
  try {
    const parsed = JSON.parse(readFileSync(path, 'utf8')) as Record<string, unknown>;
    return redactLargeRecord(parsed, 20);
  } catch { return null; }
}

export function readRunProgress(eventsPath: string): WorkerProgressPreview {
  const empty: WorkerProgressPreview = { event_count: 0, latest_event_type: null, latest_event_preview: null, latest_event_at: null, readable: true, tail_truncated: false };
  if (!existsSync(eventsPath)) return empty;
  try {
    const stat = statSync(eventsPath);
    if (stat.size === 0) return empty;
    const limit = 64 * 1024;
    const start = Math.max(0, stat.size - limit);
    const length = stat.size - start;
    const buffer = Buffer.alloc(length);
    const fd = openSync(eventsPath, 'r');
    try {
      readSync(fd, buffer, 0, length, start);
    } finally {
      closeSync(fd);
    }
    const text = buffer.toString('utf8');
    const rawLines = text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
    const lines = start === 0 ? rawLines : rawLines.slice(1);
    let latest: unknown = null;
    let eventCount = 0;
    let parseError: string | null = null;
    for (const line of lines) {
      try {
        latest = JSON.parse(line) as unknown;
        eventCount += 1;
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        parseError ||= message;
      }
    }
    return {
      event_count: eventCount,
      latest_event_type: eventType(latest),
      latest_event_preview: previewString(latestEventText(latest), 240),
      latest_event_at: eventTimestamp(latest)?.toISOString() ?? (eventCount > 0 ? stat.mtime.toISOString() : null),
      readable: parseError === null,
      tail_truncated: start > 0,
      ...(parseError ? { error_preview: previewString(parseError, 180) ?? undefined } : {}),
    };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { ...empty, readable: false, error_preview: previewString(message, 180) ?? undefined };
  }
}

export function workerBudgetStatus(run: Record<string, unknown>): Record<string, unknown> {
  const timing = asRecord(run.timing);
  const liveness = asRecord(run.status_liveness);
  const progress = asRecord(run.progress);
  const config = asRecord(run.resolved_worker_config);
  const startedAt = typeof liveness.started_at === 'string' ? liveness.started_at : typeof timing.started_at === 'string' ? timing.started_at : null;
  const startedAtMs = startedAt ? Date.parse(startedAt) : NaN;
  const elapsedMs = typeof liveness.elapsed_ms === 'number' ? liveness.elapsed_ms : Number.isFinite(startedAtMs) ? Math.max(0, Date.now() - startedAtMs) : null;
  const maxRunMs = typeof liveness.max_run_ms === 'number' ? liveness.max_run_ms : typeof config.max_run_ms === 'number' ? config.max_run_ms : null;
  const remainingMs = elapsedMs !== null && maxRunMs !== null ? Math.max(0, maxRunMs - elapsedMs) : null;
  return {
    started_at: startedAt,
    elapsed_ms: elapsedMs,
    max_run_ms: maxRunMs,
    remaining_ms: remainingMs,
    percent_used: elapsedMs !== null && maxRunMs ? Math.min(1, elapsedMs / maxRunMs) : null,
    stale_for_ms: typeof liveness.stale_for_ms === 'number' ? liveness.stale_for_ms : null,
    event_count: typeof progress.event_count === 'number' ? progress.event_count : 0,
  };
}

export function workerProgressState(run: Record<string, unknown>, recentActivity: Record<string, unknown>[], budgetStatus: Record<string, unknown>): Record<string, unknown> {
  const status = String(run.status ?? 'unknown');
  const liveness = asRecord(run.status_liveness);
  const progress = asRecord(run.progress);
  const latest = recentActivity.at(-1) ?? null;
  const terminal = isTerminalRunStatus(status);
  const stale = liveness.state === 'stale';
  const eventCount = typeof progress.event_count === 'number' ? progress.event_count : 0;
  const state = terminal ? status : stale ? 'idle_stale' : eventCount === 0 ? 'starting' : classifyProgressState(String(progress.latest_event_type ?? ''), String(progress.latest_event_preview ?? ''));
  return {
    state,
    current_action: latest ? latest.summary ?? latest.preview ?? null : progress.latest_event_preview ?? null,
    current_target: null,
    current_command: null,
    since: typeof liveness.last_activity_at === 'string' ? liveness.last_activity_at : typeof progress.latest_event_at === 'string' ? progress.latest_event_at : asRecord(run.timing).started_at ?? null,
    last_event_at: progress.latest_event_at ?? null,
    stale_for_ms: typeof liveness.stale_for_ms === 'number' ? liveness.stale_for_ms : null,
    confidence: eventCount > 0 ? 'observed' : 'unknown',
    liveness: liveness.state ?? null,
    recommended_action: recommendedProgressAction(status, liveness, budgetStatus),
  };
}

export function withFreshProgress(run: Record<string, unknown>): Record<string, unknown> {
  const runDir = typeof run.run_dir === 'string' ? run.run_dir : null;
  if (!runDir) return run;
  return { ...run, progress: readRunProgress(resolve(runDir, 'events.jsonl')) };
}

export function withRunningLiveness(run: Record<string, unknown>, maxRunMs: number): Record<string, unknown> {
  if (run.status !== 'running') return run;
  const timing = asRecord(run.timing);
  const startedAtMs = Date.parse(String(timing.started_at ?? ''));
  if (!Number.isFinite(startedAtMs)) return run;
  const progress = asRecord(run.progress);
  const latestEventAtMs = Date.parse(String(progress.latest_event_at ?? ''));
  const lastActivityMs = Number.isFinite(latestEventAtMs) ? latestEventAtMs : startedAtMs;
  const resolvedConfig = asRecord(run.resolved_worker_config);
  const maxRunMsValue = typeof resolvedConfig.max_run_ms === 'number' ? resolvedConfig.max_run_ms : maxRunMs;
  const staleAfterMs = Math.min(300_000, Math.max(60_000, Math.trunc(maxRunMsValue / 10)));
  const now = Date.now();
  const staleForMs = Math.max(0, now - lastActivityMs - staleAfterMs);
  const elapsedMs = Math.max(0, now - startedAtMs);
  const livenessState = staleForMs > 0 ? 'stale' : 'active';
  return {
    ...run,
    completion_state: livenessState === 'stale' ? 'partial' : run.completion_state,
    status_liveness: {
      state: livenessState,
      process_liveness: 'unknown',
      started_at: new Date(startedAtMs).toISOString(),
      last_event_at: Number.isFinite(latestEventAtMs) ? new Date(latestEventAtMs).toISOString() : null,
      last_activity_at: new Date(lastActivityMs).toISOString(),
      stale_after_ms: staleAfterMs,
      stale_for_ms: staleForMs,
      elapsed_ms: elapsedMs,
      max_run_ms: maxRunMsValue,
    },
  };
}

export function withProgressObservability(run: Record<string, unknown>): Record<string, unknown> {
  const runDir = typeof run.run_dir === 'string' ? run.run_dir : null;
  const recentActivity = runDir ? compactEventStream(resolve(runDir, 'events.jsonl'), 8) : [];
  const budgetStatus = workerBudgetStatus(run);
  const progressState = workerProgressState(run, recentActivity, budgetStatus);
  return { ...run, progress_state: progressState, budget_status: budgetStatus, recent_activity: recentActivity };
}

export function compactEventStream(eventsPath: string, limit: number): Record<string, unknown>[] {
  if (!existsSync(eventsPath)) return [];
  try {
    const text = readFileSync(eventsPath, 'utf8').trim();
    if (!text) return [];
    return text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean).slice(-limit).map((line) => {
      try {
        const parsed = JSON.parse(line) as unknown;
        const type = eventType(parsed);
        const preview = previewString(latestEventText(parsed), 180);
        return {
          type,
          kind: normalizeActivityKind(type, preview),
          timestamp: eventTimestamp(parsed)?.toISOString() ?? null,
          preview,
          summary: preview,
        };
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return { type: 'parse_error', timestamp: null, preview: previewString(message, 180) };
      }
    });
  } catch {
    return [];
  }
}

function classifyProgressState(eventTypeValue: string, preview: string): string {
  const text = `${eventTypeValue} ${preview}`.toLowerCase();
  if (/command|exec|shell|structured_command/.test(text)) return 'running_command';
  if (/apply_patch|edit|write|modified|file_change/.test(text)) return 'editing';
  if (/read|grep|glob|search/.test(text)) return 'reading';
  if (/tool|call/.test(text)) return 'waiting_tool';
  if (/result|last_message|final/.test(text)) return 'writing_result';
  return 'thinking';
}

function recommendedProgressAction(status: string, liveness: Record<string, unknown>, budgetStatus: Record<string, unknown>): string {
  if (isTerminalRunStatus(status)) return 'inspect_result';
  if (liveness.state !== 'stale') return 'wait';
  const remainingMs = typeof budgetStatus.remaining_ms === 'number' ? budgetStatus.remaining_ms : null;
  if (remainingMs === 0) return 'recover_or_fail';
  return 'inspect_artifacts';
}

function isTerminalRunStatus(status: string): boolean {
  return status === 'completed' || status === 'completed_with_errors' || status === 'failed' || status === 'cancelled';
}

export function previewString(value: unknown, limit: number): string | null {
  if (value === undefined || value === null || value === '') return null;
  const text = String(value).replace(/\s+/g, ' ').trim();
  return text.length <= limit ? text : `${text.slice(0, Math.max(0, limit - 3))}...`;
}

function redactLargeRecord(record: Record<string, unknown>, maxKeys: number): Record<string, unknown> {
  const entries = Object.entries(record).slice(0, maxKeys).map(([key, value]) => [key, previewJsonValue(value)]);
  return Object.fromEntries(entries);
}

function previewJsonValue(value: unknown): unknown {
  if (typeof value === 'string') return previewString(value, 240) ?? '';
  if (Array.isArray(value)) return value.slice(0, 20).map(previewJsonValue);
  if (value && typeof value === 'object') return redactLargeRecord(value as Record<string, unknown>, 20);
  return value;
}

function classifyDiagnosticTail(text: string): string {
  return classifyRuntimeError(text) ?? 'runtime_prestart_diagnostic';
}

export function classifyRuntimeError(text: string): string | null {
  const lower = text.toLowerCase();
  if (lower.includes('orchestrator_helper_launch_failed') && (lower.includes('os error 206') || lower.includes('filename or extension is too long'))) return 'windows_sandbox_setup_payload_too_long';
  if (lower.includes('writing is blocked by read-only sandbox') || lower.includes('read-only filesystem sandbox')) return 'write_admission_mismatch';
  if (lower.includes('outside allowed roots') || lower.includes('outside_allowed_roots')) return 'cwd_outside_allowed_roots';
  if (lower.includes('rejected by policy') || lower.includes('command syntax not admitted')) return 'command_shape_not_admitted';
  if (lower.includes('mcp runtime fault') || lower.includes('mcp_runtime_fault') || lower.includes('surface_registry_tool_not_declared') || lower.includes('admission_required') || lower.includes('tool_not_declared')) return 'mcp_tool_failure';
  if (lower.includes('rate_limit') || lower.includes('rate limit') || lower.includes('429')) return 'provider_rate_limited';
  if (lower.includes('unauthorized') || lower.includes('authentication') || lower.includes('api key') || lower.includes('401') || lower.includes('403')) return 'provider_auth';
  if (lower.includes('invalid_request') || lower.includes('invalid request') || lower.includes('function name is invalid') || lower.includes('400')) return 'provider_invalid_request';
  if (lower.includes('stream disconnected') || lower.includes('error sending request') || lower.includes('failed to connect') || lower.includes('websocket') || lower.includes('socket') || lower.includes('network')) return 'provider_network';
  if (lower.includes('not inside a trusted directory') || lower.includes('--skip-git-repo-check')) return 'codex_untrusted_directory';
  if (lower.includes('permission denied') || lower.includes('access is denied')) return 'permission_denied';
  if (lower.includes('command not found') || lower.includes('not recognized as')) return 'runtime_command_unavailable';
  return null;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}
