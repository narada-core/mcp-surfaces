import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { WorkerDelegationMode, WorkerRunToolInput } from './worker-types.js';

export type WorkerChange = { path: string; status: string; summary: string };
export type WorkerVerificationCommandClassification = 'focused' | 'broad' | 'not_applicable';
export type WorkerVerification = { tool: string | null; command: string | null; status: string; summary: string; command_classification: WorkerVerificationCommandClassification };
export type WorkerBroadUnrelatedFailure = { command: string | null; status: string; summary: string };
export type WorkerObservedErgonomics = { clarity_before_acting: number; confidence_in_writable_roots: number; predictability_of_tool_behavior: number; diagnostic_usefulness: number; remaining_ceremony: number; remaining_weaknesses: number; non_scoring_observations: string[] };
export type WorkerExitInterview = { ergonomics_feedback: string; friction_points: string[]; missing_affordances: string[]; observed_incoherencies: string[]; suggested_improvements: string[]; observed_ergonomics?: WorkerObservedErgonomics };
export type WorkerOutput = { summary: string; deliverables: { path: string; description: string }[]; open_questions: string[]; next_actions: string[]; edits_performed: boolean; target_state_changed: boolean; changes: WorkerChange[]; verification: WorkerVerification[]; verification_budget_respected: boolean | null; broad_unrelated_failures: WorkerBroadUnrelatedFailure[]; exit_interview: WorkerExitInterview | null; review_verdict: string | null; acceptance_verdict: string | null; verdict: string | null; structured_outputs?: Record<string, unknown> };
export type WorkerOutputParseResult =
  | { ok: true; data: WorkerOutput }
  | { ok: false; reason: 'missing_file' | 'invalid_json' | 'invalid_shape'; message: string };
export type WorkerRunTerminalStatus = 'completed' | 'completed_with_errors' | 'failed' | 'cancelled';

export function parseLastMessage(path: string): WorkerOutputParseResult {
  let parsed: unknown;
  try {
    parsed = JSON.parse(readFileSync(path, 'utf8').replace(/^\uFEFF/, ''));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { ok: false, reason: message.includes('ENOENT') ? 'missing_file' : 'invalid_json', message };
  }
  const normalized = normalizeWorkerOutput(parsed, { strict: true });
  if (!normalized.ok) return normalized;
  return { ok: true, data: normalized.data };
}

export function parseResult(runRecord: { lastMessagePath: string }): WorkerOutputParseResult {
  return parseLastMessage(runRecord.lastMessagePath);
}

export function parseWorkerOutputJson(message: string): WorkerOutput | null {
  const candidates = [
    message.trim(),
    message.trim().replace(/^```(?:json)?\s*/i, '').replace(/\s*```$/i, '').trim(),
    extractJsonObject(message),
  ].filter((candidate): candidate is string => Boolean(candidate));
  for (const candidate of candidates) {
    try {
      const normalized = normalizeWorkerOutput(JSON.parse(candidate), { strict: false });
      if (normalized.ok) return normalized.data;
    } catch {
      // Try next candidate.
    }
  }
  return null;
}

export function workerOutputFromAgentMessage(message: string): WorkerOutput {
  const parsed = parseWorkerOutputJson(message);
  if (parsed) return parsed;
  return {
    summary: message,
    deliverables: [],
    open_questions: [],
    next_actions: [],
    edits_performed: false,
    target_state_changed: false,
    changes: [],
    verification: [{ tool: 'narada-agent-runtime-server', command: null, status: 'passed', summary: 'Recovered final assistant_message after turn_complete', command_classification: 'not_applicable' }],
    verification_budget_respected: null,
    broad_unrelated_failures: [],
    exit_interview: null,
    review_verdict: null,
    acceptance_verdict: null,
    verdict: null,
  };
}

export function workerOutputState(parsed: WorkerOutputParseResult): 'absent' | 'invalid_json' | 'invalid_shape' {
  if (parsed.ok === true) throw new Error('worker_output_state_requires_failed_parse');
  if (parsed.reason === 'missing_file') return 'absent';
  if (parsed.reason === 'invalid_json') return 'invalid_json';
  return 'invalid_shape';
}

export function resultStatus(codexResult: { exit_code: number | null; cancelled: boolean; error: string | null; event_error?: string | null; runtime_error?: string | null }, parsed: WorkerOutputParseResult): { status: WorkerRunTerminalStatus; error: string | null; warnings: string[] } {
  if (codexResult.cancelled) return { status: 'cancelled', error: 'cancelled', warnings: [] };
  const warnings = [codexResult.runtime_error].filter((value): value is string => typeof value === 'string' && value.length > 0);
  const runtimeError = codexResult.error
    ?? codexResult.event_error
    ?? (codexResult.exit_code !== 0 && codexResult.exit_code !== null ? codexResult.runtime_error ?? `worker runtime exited with code ${codexResult.exit_code}` : null);
  if (runtimeError && parsed.ok) return { status: 'completed_with_errors', error: runtimeError, warnings };
  if (runtimeError) return { status: 'failed', error: runtimeError, warnings };
  if (parsed.ok === false && parsed.reason === 'missing_file') return { status: 'failed', error: `absent last_message.json: ${parsed.message}`, warnings };
  if (parsed.ok === false) return { status: 'failed', error: `invalid last_message.json: ${parsed.reason}: ${parsed.message}`, warnings };
  return { status: 'completed', error: null, warnings };
}

export function workerOutputSchema(outputContract: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    type: 'object',
    additionalProperties: false,
    required: ['summary', 'deliverables', 'open_questions', 'next_actions', 'edits_performed', 'target_state_changed', 'changes', 'verification', 'verification_budget_respected', 'broad_unrelated_failures', 'exit_interview', 'review_verdict', 'acceptance_verdict', 'verdict', 'structured_outputs'],
    properties: {
      summary: { type: 'string' },
      deliverables: { type: 'array', items: { type: 'object', required: ['path', 'description'], properties: { path: { type: 'string' }, description: { type: 'string' } }, additionalProperties: false } },
      open_questions: { type: 'array', items: { type: 'string' } },
      next_actions: { type: 'array', items: { type: 'string' } },
      edits_performed: { type: 'boolean' },
      target_state_changed: { type: 'boolean' },
      changes: { type: 'array', items: { type: 'object', required: ['path', 'status', 'summary'], properties: { path: { type: 'string' }, status: { type: 'string' }, summary: { type: 'string' } }, additionalProperties: false } },
      verification: { type: 'array', items: { type: 'object', required: ['tool', 'command', 'status', 'summary', 'command_classification'], properties: { tool: { type: ['string', 'null'] }, command: { type: ['string', 'null'] }, status: { type: 'string' }, summary: { type: 'string' }, command_classification: { type: 'string', enum: ['focused', 'broad', 'not_applicable'] } }, additionalProperties: false } },
      verification_budget_respected: { type: ['boolean', 'null'] },
      broad_unrelated_failures: { type: 'array', items: { type: 'object', required: ['command', 'status', 'summary'], properties: { command: { type: ['string', 'null'] }, status: { type: 'string' }, summary: { type: 'string' } }, additionalProperties: false } },
      exit_interview: {
        type: ['object', 'null'],
        required: ['ergonomics_feedback', 'friction_points', 'missing_affordances', 'observed_incoherencies', 'suggested_improvements', 'observed_ergonomics'],
        properties: {
          ergonomics_feedback: { type: 'string' },
          friction_points: { type: 'array', items: { type: 'string' } },
          missing_affordances: { type: 'array', items: { type: 'string' } },
          observed_incoherencies: { type: 'array', items: { type: 'string' } },
          suggested_improvements: { type: 'array', items: { type: 'string' } },
          observed_ergonomics: {
            type: 'object',
            required: ['clarity_before_acting', 'confidence_in_writable_roots', 'predictability_of_tool_behavior', 'diagnostic_usefulness', 'remaining_ceremony', 'remaining_weaknesses', 'non_scoring_observations'],
            properties: {
              clarity_before_acting: scoreSchema(),
              confidence_in_writable_roots: scoreSchema(),
              predictability_of_tool_behavior: scoreSchema(),
              diagnostic_usefulness: scoreSchema(),
              remaining_ceremony: scoreSchema(),
              remaining_weaknesses: scoreSchema(),
              non_scoring_observations: { type: 'array', items: { type: 'string' } },
            },
            additionalProperties: false,
          },
        },
        additionalProperties: false,
      },
      review_verdict: { type: ['string', 'null'] },
      acceptance_verdict: { type: ['string', 'null'] },
      verdict: { type: ['string', 'null'] },
      structured_outputs: structuredOutputsSchema(outputContract),
    },
  };
}

export function writeWorkerOutputSchema(path: string, outputContract: Record<string, unknown> = {}): void {
  writeFileSync(path, `${JSON.stringify(workerOutputSchema(outputContract), null, 2)}\n`, 'utf8');
}

function structuredOutputsSchema(outputContract: Record<string, unknown>): Record<string, unknown> {
  const key = typeof outputContract.structured_output_key === 'string'
    ? outputContract.structured_output_key.trim()
    : '';
  const declared = outputContract.structured_output_schema ?? outputContract.output_schema;
  if (!key || !declared || typeof declared !== 'object' || Array.isArray(declared)) {
    return { type: 'object', properties: {}, required: [], additionalProperties: false };
  }
  return {
    type: 'object',
    properties: { [key]: closeStructuredOutputSchema(declared) },
    required: [key],
    additionalProperties: false,
  };
}

function closeStructuredOutputSchema(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(closeStructuredOutputSchema);
  if (!value || typeof value !== 'object') return value;
  const source = value as Record<string, unknown>;
  const closed: Record<string, unknown> = {};
  for (const [key, nested] of Object.entries(source)) {
    if (key === 'additionalProperties') continue;
    closed[key] = closeStructuredOutputSchema(nested);
  }
  if (source.type === 'object' || (source.properties && typeof source.properties === 'object')) {
    closed.additionalProperties = false;
  }
  return closed;
}

export function outputContractForRequest(request: WorkerRunToolInput, mode: WorkerDelegationMode): Record<string, unknown> {
  const base = outputContractForMode(mode);
  const targetPaths = request.constraints.preflight_paths?.map((item) => resolve(item.path)) ?? [];
  const authority = request.constraints.authority ?? 'read';
  return {
    ...base,
    ...(request.intent.output_contract ?? {}),
    effective_authority: authority,
    tool_capability_note: authority === 'read' ? 'If a raw MCP surface advertises write-capable roots or mutation tools, treat them as unavailable for this delegation unless the requested authority is escalated by the caller.' : null,
    target_paths: targetPaths,
    forbidden_adjacent_paths: targetPaths.length > 0 ? ['Paths outside target_paths and allowed roots unless explicitly required by the task.'] : [],
    verification_budget: request.constraints.verification_budget ?? null,
    test_budget: request.constraints.test_budget ?? null,
  };
}

export function outputContractForMode(mode: WorkerDelegationMode): Record<string, unknown> {
  const auditLike = mode === 'audit_only' || mode === 'plan_only';
  return {
    schema: 'narada.worker.output_contract.v1',
    requested_mode: mode,
    confidence_level: { type: 'number', minimum: 0, maximum: 1, meaning: '0 means unsupported, 1 means fully evidenced' },
    evidence_basis: { type: 'array', items: 'short evidence references such as file:line, command output, or MCP tool result' },
    findings: auditLike ? {
      required_for_audit_only: true,
      item_shape: { severity: 'info|low|medium|high|critical', path: 'string|null', recommendation: 'string', confidence_level: 'number 0..1', evidence_refs: 'string[]' },
    } : null,
    shell_fallback_reason: 'When verification or discovery uses shell because an MCP tool was unavailable or insufficient, explain that reason in verification.summary.',
    verification_command_classification: {
      required: true,
      allowed_values: ['focused', 'broad', 'not_applicable'],
      meaning: 'focused commands directly validate the touched package/task; broad commands scan larger or unrelated surfaces and must be justified.',
    },
    verification_budget_respected: { type: ['boolean', 'null'], required: true, meaning: 'true if verification/test budget and stop discipline were respected, false if exceeded, null if no budget was supplied and no verification was run' },
    broad_unrelated_failures: { type: 'array', required: true, meaning: 'Failures from broad commands that appear unrelated to the delegated target; do not mix them with focused verification failures.' },
    stop_discipline: 'Run focused checks first. Stop after the requested tests or first blocking focused failure when stop_on_first_failure is true. Do not run broad suites unless requested, needed, or allowed by budget.',
    focused_readback: auditLike ? {
      required: true,
      behavior: 'Inspect ordinary target source files directly with filesystem read/search MCP tools available to the worker; do not require the delegating caller to pre-materialize output_refs for normal source files.',
      bounds: 'Keep large/generated/secret outputs bounded and summarize them instead of pasting full content.',
    } : null,
  };
}

function normalizeWorkerOutput(value: unknown, options: { strict: boolean }): WorkerOutputParseResult {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return { ok: false, reason: 'invalid_shape', message: 'last_message must be an object' };
  const record = value as Record<string, unknown>;
  const summary = typeof record.summary === 'string' ? record.summary : !options.strict && typeof record.message === 'string' ? record.message : null;
  if (!summary) return { ok: false, reason: 'invalid_shape', message: 'summary must be a string' };
  const requiredArray = (key: string): unknown[] | null => {
    if (Array.isArray(record[key])) return record[key] as unknown[];
    return options.strict ? null : [];
  };
  const deliverablesRaw = requiredArray('deliverables');
  if (!deliverablesRaw) return { ok: false, reason: 'invalid_shape', message: 'deliverables must be an array' };
  const openQuestionsRaw = requiredArray('open_questions');
  if (!openQuestionsRaw) return { ok: false, reason: 'invalid_shape', message: 'open_questions must be an array' };
  const nextActionsRaw = requiredArray('next_actions');
  if (!nextActionsRaw) return { ok: false, reason: 'invalid_shape', message: 'next_actions must be an array' };
  const changesRaw = requiredArray('changes');
  if (!changesRaw) return { ok: false, reason: 'invalid_shape', message: 'changes must be an array' };
  const verificationRaw = verificationInput(record.verification, options);
  if (!verificationRaw) return { ok: false, reason: 'invalid_shape', message: 'verification must be an array' };
  if (options.strict && typeof record.edits_performed !== 'boolean') return { ok: false, reason: 'invalid_shape', message: 'edits_performed must be a boolean' };
  if (options.strict && typeof record.target_state_changed !== 'boolean') return { ok: false, reason: 'invalid_shape', message: 'target_state_changed must be a boolean' };
  const deliverables = arrayOf(deliverablesRaw, asDeliverable);
  if (deliverables.length !== deliverablesRaw.length) return { ok: false, reason: 'invalid_shape', message: 'deliverables entries must have string path and description' };
  const openQuestions = stringArray(openQuestionsRaw) ? openQuestionsRaw : null;
  if (!openQuestions) return { ok: false, reason: 'invalid_shape', message: 'open_questions entries must be strings' };
  const nextActions = stringArray(nextActionsRaw) ? nextActionsRaw : null;
  if (!nextActions) return { ok: false, reason: 'invalid_shape', message: 'next_actions entries must be strings' };
  const changes = arrayOf(changesRaw, asChange);
  if (changes.length !== changesRaw.length) return { ok: false, reason: 'invalid_shape', message: 'changes entries must have string path, status, and summary' };
  const verification = normalizeVerification(verificationRaw, options);
  if (verification.length !== verificationRaw.length) return { ok: false, reason: 'invalid_shape', message: 'verification entries must have nullable string tool and command, plus string status and summary' };
  const verificationBudgetRespected = record.verification_budget_respected === undefined || record.verification_budget_respected === null
    ? null
    : typeof record.verification_budget_respected === 'boolean'
      ? record.verification_budget_respected
      : null;
  if (record.verification_budget_respected !== undefined && record.verification_budget_respected !== null && typeof record.verification_budget_respected !== 'boolean') return { ok: false, reason: 'invalid_shape', message: 'verification_budget_respected must be boolean or null' };
  const broadUnrelatedFailuresRaw = record.broad_unrelated_failures === undefined ? [] : record.broad_unrelated_failures;
  if (!Array.isArray(broadUnrelatedFailuresRaw)) return { ok: false, reason: 'invalid_shape', message: 'broad_unrelated_failures must be an array' };
  const broadUnrelatedFailures = arrayOf(broadUnrelatedFailuresRaw, asBroadUnrelatedFailure);
  if (broadUnrelatedFailures.length !== broadUnrelatedFailuresRaw.length) return { ok: false, reason: 'invalid_shape', message: 'broad_unrelated_failures entries must have nullable string command plus string status and summary' };
  const exitInterview = record.exit_interview === undefined || record.exit_interview === null ? null : asExitInterview(record.exit_interview);
  if (record.exit_interview !== undefined && record.exit_interview !== null && !exitInterview) return { ok: false, reason: 'invalid_shape', message: 'exit_interview must be null or include ergonomics_feedback, friction_points, missing_affordances, observed_incoherencies, and suggested_improvements' };
  const optionalString = (key: string): string | null | undefined => {
    if (record[key] === undefined || record[key] === null) return null;
    return typeof record[key] === 'string' ? record[key] as string : undefined;
  };
  const reviewVerdict = optionalString('review_verdict');
  const acceptanceVerdict = optionalString('acceptance_verdict');
  const verdict = optionalString('verdict');
  if (reviewVerdict === undefined || acceptanceVerdict === undefined || verdict === undefined) return { ok: false, reason: 'invalid_shape', message: 'review_verdict, acceptance_verdict, and verdict must be strings or null' };
  return {
    ok: true,
    data: {
      summary,
      deliverables,
      open_questions: openQuestions,
      next_actions: nextActions,
      edits_performed: typeof record.edits_performed === 'boolean' ? record.edits_performed : false,
      target_state_changed: typeof record.target_state_changed === 'boolean' ? record.target_state_changed : false,
      changes,
      verification,
      verification_budget_respected: verificationBudgetRespected,
      broad_unrelated_failures: broadUnrelatedFailures,
      exit_interview: exitInterview,
      review_verdict: reviewVerdict,
      acceptance_verdict: acceptanceVerdict,
      verdict,
      structured_outputs: structuredOutputsFromRecord(record, summary),
    },
  };
}

function structuredOutputsFromRecord(record: Record<string, unknown>, summary: string): Record<string, unknown> | undefined {
  const direct = record.structured_outputs;
  if (direct && typeof direct === 'object' && !Array.isArray(direct) && Object.keys(direct).length > 0) {
    return direct as Record<string, unknown>;
  }
  return recoverExplicitStructuredOutputs(summary);
}

function recoverExplicitStructuredOutputs(summary: string): Record<string, unknown> | undefined {
  const embeddedJson = extractJsonObject(summary);
  if (!embeddedJson) return undefined;
  let embedded: unknown;
  try {
    embedded = JSON.parse(embeddedJson);
  } catch {
    return undefined;
  }
  if (!embedded || typeof embedded !== 'object' || Array.isArray(embedded)) return undefined;
  const record = embedded as Record<string, unknown>;
  const key = typeof record.structured_output_key === 'string' ? record.structured_output_key.trim() : '';
  if (!key) return undefined;
  const declared = record.structured_outputs;
  if (declared && typeof declared === 'object' && !Array.isArray(declared) && Object.hasOwn(declared, key)) {
    return { [key]: (declared as Record<string, unknown>)[key] };
  }
  if (!Object.hasOwn(record, key)) return undefined;
  return { [key]: record[key] };
}

function verificationInput(value: unknown, options: { strict: boolean }): unknown[] | null {
  if (Array.isArray(value)) return value;
  if (!options.strict && value && typeof value === 'object') {
    const record = value as Record<string, unknown>;
    if (Array.isArray(record.checks)) return record.checks;
    return [record];
  }
  return options.strict ? null : [];
}

function normalizeVerification(value: unknown, options: { strict: boolean }): WorkerVerification[] {
  if (!Array.isArray(value)) return [];
  return arrayOf(value, (item) => asVerification(item, options));
}

function arrayOf<T>(value: unknown, mapper: (item: unknown) => T | null): T[] {
  if (!Array.isArray(value)) return [];
  return value.map(mapper).filter((item): item is T => item !== null);
}

function extractJsonObject(message: string): string | null {
  const start = message.indexOf('{');
  const end = message.lastIndexOf('}');
  return start >= 0 && end > start ? message.slice(start, end + 1) : null;
}

function asDeliverable(value: unknown): { path: string; description: string } | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (typeof record.path !== 'string' || typeof record.description !== 'string') return null;
  return { path: record.path, description: record.description };
}

function asChange(value: unknown): WorkerChange | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (typeof record.path !== 'string' || typeof record.status !== 'string' || typeof record.summary !== 'string') return null;
  return { path: record.path, status: record.status, summary: record.summary };
}

function asVerification(value: unknown, options: { strict: boolean }): WorkerVerification | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (options.strict && (!Object.hasOwn(record, 'tool') || !Object.hasOwn(record, 'command'))) return null;
  const tool = Object.hasOwn(record, 'tool') ? record.tool : null;
  const command = Object.hasOwn(record, 'command') ? record.command : null;
  if (!nullableString(tool) || !nullableString(command)) return null;
  const summary = typeof record.summary === 'string'
    ? record.summary
    : !options.strict && typeof record.name === 'string'
      ? record.name
      : null;
  if (typeof record.status !== 'string' || summary === null) return null;
  const commandClassification = verificationCommandClassification(record.command_classification);
  if (record.command_classification !== undefined && commandClassification === null) return null;
  return { tool, command, status: record.status, summary, command_classification: commandClassification ?? inferredCommandClassification(command) };
}

function verificationCommandClassification(value: unknown): WorkerVerificationCommandClassification | null {
  return value === 'focused' || value === 'broad' || value === 'not_applicable' ? value : null;
}

function inferredCommandClassification(command: string | null): WorkerVerificationCommandClassification {
  return command === null ? 'not_applicable' : 'focused';
}

function asBroadUnrelatedFailure(value: unknown): WorkerBroadUnrelatedFailure | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (!nullableString(record.command)) return null;
  if (typeof record.status !== 'string' || typeof record.summary !== 'string') return null;
  return { command: record.command, status: record.status, summary: record.summary };
}

function nullableString(value: unknown): value is string | null {
  return value === null || typeof value === 'string';
}

function asExitInterview(value: unknown): WorkerExitInterview | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (typeof record.ergonomics_feedback !== 'string') return null;
  if (!stringArray(record.friction_points) || !stringArray(record.missing_affordances) || !stringArray(record.observed_incoherencies) || !stringArray(record.suggested_improvements)) return null;
  const observedErgonomics = asObservedErgonomics(record.observed_ergonomics);
  if (record.observed_ergonomics !== undefined && !observedErgonomics) return null;
  return {
    ergonomics_feedback: record.ergonomics_feedback,
    friction_points: record.friction_points,
    missing_affordances: record.missing_affordances,
    observed_incoherencies: record.observed_incoherencies,
    suggested_improvements: record.suggested_improvements,
    ...(observedErgonomics ? { observed_ergonomics: observedErgonomics } : {}),
  };
}

function scoreSchema(): Record<string, unknown> { return { type: 'integer', minimum: 1, maximum: 5 }; }
function asObservedErgonomics(value: unknown): WorkerObservedErgonomics | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const keys = ['clarity_before_acting', 'confidence_in_writable_roots', 'predictability_of_tool_behavior', 'diagnostic_usefulness', 'remaining_ceremony', 'remaining_weaknesses'] as const;
  if (!keys.every((key) => Number.isInteger(record[key]) && Number(record[key]) >= 1 && Number(record[key]) <= 5) || !stringArray(record.non_scoring_observations)) return null;
  return { clarity_before_acting: Number(record.clarity_before_acting), confidence_in_writable_roots: Number(record.confidence_in_writable_roots), predictability_of_tool_behavior: Number(record.predictability_of_tool_behavior), diagnostic_usefulness: Number(record.diagnostic_usefulness), remaining_ceremony: Number(record.remaining_ceremony), remaining_weaknesses: Number(record.remaining_weaknesses), non_scoring_observations: record.non_scoring_observations };
}

function stringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === 'string');
}
