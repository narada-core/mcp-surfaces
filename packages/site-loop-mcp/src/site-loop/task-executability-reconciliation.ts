import { existsSync } from 'node:fs';
import { join } from 'node:path';
import {
  createServerState,
  delegatedTaskResult,
  delegatedTaskRun,
} from '@narada-core/delegated-task-mcp/internal';
import {
  admitTaskExecutabilityAssessment,
  assembleDeclaredEnvironment,
  recordTaskExecutabilityFailure,
} from '@narada-core/task-governance-core/task-executability-service';
import {
  TASK_EXECUTABILITY_ASSESSMENT_SCHEMA as CANONICAL_TASK_EXECUTABILITY_ASSESSMENT_SCHEMA,
  TASK_EXECUTABILITY_EVALUATOR_PROVENANCE_SCHEMA,
  deriveTaskExecutabilityVerdict,
  taskExecutabilityAssessmentId,
  type TaskExecutabilityAssessment,
  type TaskExecutabilityDeclaredEnvironment,
  type TaskExecutabilityFinding,
  type TaskExecutabilityFindingKind,
  type TaskExecutabilityFindingSeverity,
} from '@narada-core/task-governance-core/task-executability-contract';
import {
  SqliteTaskLifecycleStore,
  type TaskExecutabilityRequestRow,
  type TaskSpecRow,
} from '@narada-core/task-governance-core/task-lifecycle-store';
import {
  TaskExecutabilityOrchestrator,
  deterministicIdempotencyKey,
  type DelegatedTaskInvocation,
  type DelegatedTaskPoll,
  type DelegatedTaskPort,
  type DelegatedTaskResult,
  type TaskExecutabilityRequest,
  type TaskLifecyclePort,
} from '@narada-core/task-executability-orchestrator';

export const TASK_EXECUTABILITY_SITE_LOOP_SCHEMA = 'narada.site_loop.task_executability_reconciliation.v1' as const;
const WORKFLOW_TEMPLATE_ID = 'task_executability_assessment_v1';
// Delegated Task's worker envelope and Task Governance's persisted record are
// intentionally different contracts. Keep the translation boundary explicit.
const WORKER_TASK_EXECUTABILITY_ASSESSMENT_SCHEMA = 'narada.task.executability.assessment.v1';
const MAX_BATCH = 10;

type JsonRecord = Record<string, unknown>;

function record(value: unknown): JsonRecord {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonRecord : {};
}

function records(value: unknown): JsonRecord[] {
  return Array.isArray(value) ? value.filter((item) => item && typeof item === 'object' && !Array.isArray(item)) as JsonRecord[] : [];
}

function jsonArray(value: string | null | undefined): unknown[] {
  if (!value) return [];
  try {
    const parsed = JSON.parse(value);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function taskPacket(row: TaskExecutabilityRequestRow, spec: TaskSpecRow | undefined): JsonRecord {
  return {
    schema: 'narada.task.executability.packet.v1',
    task_id: row.task_id,
    task_number: row.task_number,
    title: spec?.title ?? null,
    goal: spec?.goal_markdown ?? null,
    context: spec?.context_markdown ?? null,
    required_work: spec?.required_work_markdown ?? null,
    non_goals: spec?.non_goals_markdown ?? null,
    acceptance_criteria: jsonArray(spec?.acceptance_criteria_json),
    dependencies: jsonArray(spec?.dependencies_json),
    source: spec ? 'task_lifecycle.task_specs' : 'task_lifecycle.request_only',
    missing_spec: !spec,
  };
}

function makeLifecyclePort(
  store: SqliteTaskLifecycleStore,
  siteRoot: string,
  requestContexts: Map<string, TaskExecutabilityRequest>,
): TaskLifecyclePort {
  return {
    async leaseNextExecutabilityRequest({ consumer_id, lease_duration_minutes }) {
      const row = store.leaseNextExecutabilityRequest(consumer_id, lease_duration_minutes);
      if (!row) return undefined;
      const spec = store.getTaskSpecByNumber(row.task_number);
      const environment = assembleDeclaredEnvironment(siteRoot);
      const attempt = store.getLatestExecutabilityAttempt(row.request_id);
      const latestAttempt = attempt && attempt.state !== 'leased'
        ? {
            delegated_task_id: attempt.delegated_task_id,
            worker_run_id: attempt.worker_run_id,
            state: attempt.state as 'dispatched' | 'completed' | 'failed_retryable' | 'failed_terminal',
          }
        : undefined;
      const request = {
        ...row,
        lease_owner: row.lease_owner ?? consumer_id,
        lease_expires_at: row.lease_expires_at ?? new Date(Date.now() + lease_duration_minutes * 60_000).toISOString(),
        task_packet: taskPacket(row, spec),
        environment,
        ...(latestAttempt ? { latest_attempt: latestAttempt } : {}),
      } satisfies TaskExecutabilityRequest;
      requestContexts.set(request.request_id, request);
      // The orchestrator addresses the delegated boundary with its canonical
      // idempotency key, while lifecycle storage addresses the lease by the
      // raw request id. Keep both aliases in the short-lived adapter context;
      // otherwise a real Site Loop dispatch cannot recover its leased packet.
      requestContexts.set(deterministicIdempotencyKey(request.request_id), request);
      return request;
    },

    async recordExecutabilityDispatch(args) {
      store.recordExecutabilityDispatch({
        request_id: args.request_id,
        state: args.state,
        delegated_task_id: args.delegated_task_id,
        worker_run_id: args.worker_run_id,
        error_json: args.error ? JSON.stringify(args.error) : null,
      });
    },

    async completeExecutabilityAssessment(args) {
      const current = store.getExecutabilityRequest(args.request_id);
      if (!current || (current.state !== 'leased' && current.state !== 'dispatched')) {
        return { status: 'stale', reason: 'request_not_currently_leased' };
      }
      if (current.lease_owner !== args.lease_owner || !current.lease_expires_at || Date.parse(current.lease_expires_at) <= Date.now()) {
        return { status: 'stale', reason: 'lease_not_owned_or_expired' };
      }
      try {
        admitTaskExecutabilityAssessment({
          store,
          requestId: args.request_id,
          assessment: args.assessment,
        });
        return { status: 'completed' };
      } catch (error) {
        return { status: 'rejected', reason: error instanceof Error ? error.message : String(error) };
      }
    },

    async failExecutabilityRequest(args) {
      const current = store.getExecutabilityRequest(args.request_id);
      if (!current || current.lease_owner !== args.lease_owner) return;
      recordTaskExecutabilityFailure({
        store,
        requestId: args.request_id,
        state: args.state,
        failure: args.failure,
      });
    },
  };
}

function findAssessment(value: unknown, depth = 0): JsonRecord | null {
  if (depth > 8 || value === null || value === undefined) return null;
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findAssessment(item, depth + 1);
      if (found) return found;
    }
    return null;
  }
  if (typeof value !== 'object') return null;
  const current = value as JsonRecord;
  if (
    current.schema === WORKER_TASK_EXECUTABILITY_ASSESSMENT_SCHEMA
    && current.version === 1
    && Array.isArray(current.findings)
    && Array.isArray(current.dimensions)
  ) return current;
  for (const child of Object.values(current)) {
    const found = findAssessment(child, depth + 1);
    if (found) return found;
  }
  return null;
}

function findProvenance(value: unknown, depth = 0): JsonRecord {
  if (depth > 8 || value === null || value === undefined) return {};
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findProvenance(item, depth + 1);
      if (Object.keys(found).length > 0) return found;
    }
    return {};
  }
  if (typeof value !== 'object') return {};
  const current = value as JsonRecord;
  if (current.evaluator_provenance && typeof current.evaluator_provenance === 'object') return record(current.evaluator_provenance);
  if (typeof current.provider === 'string' || typeof current.model === 'string') return current;
  for (const child of Object.values(current)) {
    const found = findProvenance(child, depth + 1);
    if (Object.keys(found).length > 0) return found;
  }
  return {};
}

const FINDING_KINDS = new Set<TaskExecutabilityFindingKind>([
  'unresolved_reference',
  'undecided_choice',
  'unavailable_authority',
  'unavailable_tool',
  'unmapped_acceptance_criterion',
  'missing_information',
  'ambiguity',
  'evaluator_note',
]);
const FINDING_SEVERITIES = new Set<TaskExecutabilityFindingSeverity>(['info', 'warning', 'blocking']);

function normalizedFinding(value: unknown, index: number, defaultKind: TaskExecutabilityFindingKind = 'evaluator_note'): TaskExecutabilityFinding {
  const item = record(value);
  const kind = FINDING_KINDS.has(item.kind as TaskExecutabilityFindingKind)
    ? item.kind as TaskExecutabilityFindingKind
    : defaultKind;
  const severity = FINDING_SEVERITIES.has(item.severity as TaskExecutabilityFindingSeverity)
    ? item.severity as TaskExecutabilityFindingSeverity
    : item.blocking === true ? 'blocking' : 'info';
  const message = typeof item.message === 'string' && item.message.trim()
    ? item.message
    : typeof item.summary === 'string' && item.summary.trim()
      ? item.summary
      : JSON.stringify(value);
  return {
    schema: 'narada.task_executability_finding.v1',
    kind,
    severity,
    code: typeof item.code === 'string' && item.code.trim() ? item.code : `worker_finding_${index + 1}`,
    message,
    ...(typeof item.ref === 'string' ? { ref: item.ref } : {}),
  };
}

/**
 * Translate the Delegated Task worker envelope into the canonical Task
 * Governance assessment admitted to SQLite. The worker uses the dotted
 * schema and `evaluator_provenance`; the durable record uses the core schema
 * and `evaluator`. No caller should pass the worker object directly to the
 * lifecycle admission service.
 */
export function assessmentForRequest(workerAssessment: JsonRecord, request: TaskExecutabilityRequest, result: JsonRecord): TaskExecutabilityAssessment {
  const findings: TaskExecutabilityFinding[] = [];
  records(workerAssessment.findings).forEach((item, index) => findings.push(normalizedFinding(item, index)));
  records(workerAssessment.required_decisions).forEach((item, index) => findings.push(normalizedFinding(item, findings.length + index, 'undecided_choice')));
  records(workerAssessment.reference_resolutions)
    .filter((item) => item.resolved === false || item.status === 'unresolved' || item.status === 'missing')
    .forEach((item, index) => findings.push(normalizedFinding(item, findings.length + index, 'unresolved_reference')));
  records(workerAssessment.acceptance_mappings)
    .filter((item) => item.mapped === false || item.status === 'unmapped' || item.status === 'missing')
    .forEach((item, index) => findings.push(normalizedFinding(item, findings.length + index, 'unmapped_acceptance_criterion')));

  // The canonical NARS result may intentionally omit provider/model selector
  // fields at the worker boundary. The worker assessment's validated
  // evaluator_provenance owns those fields; the delegated result still owns
  // transport provenance such as the worker run id. Merge both contracts with
  // the assessment provenance taking precedence for evaluator identity.
  const identity = resultIdentity(result);
  const provenance: JsonRecord = {
    ...findProvenance(result),
    ...record(workerAssessment.evaluator_provenance),
    ...(identity.worker_run_id ? { run_id: identity.worker_run_id } : {}),
  };
  const createdAt = new Date().toISOString();
  return {
    schema: CANONICAL_TASK_EXECUTABILITY_ASSESSMENT_SCHEMA,
    assessment_id: taskExecutabilityAssessmentId({ request_id: request.request_id, created_at: createdAt }),
    request_id: request.request_id,
    task_id: request.task_id,
    task_number: request.task_number,
    task_spec_digest: request.task_spec_digest,
    environment_digest: request.environment_digest,
    verdict: deriveTaskExecutabilityVerdict(findings),
    findings,
    evaluator: {
      schema: TASK_EXECUTABILITY_EVALUATOR_PROVENANCE_SCHEMA,
      profile: request.evaluator_profile,
      profile_version: request.evaluator_profile_version,
      cognition: 'low',
      ...(typeof provenance.provider === 'string' ? { provider: provenance.provider } : {}),
      ...(typeof provenance.model === 'string' ? { model: provenance.model } : {}),
      ...(typeof result.task_id === 'string' ? { delegated_task_id: result.task_id } : {}),
      ...(typeof provenance.run_id === 'string' ? { worker_run_id: provenance.run_id } : {}),
    },
    created_at: createdAt,
  };
}

function resultIdentity(result: JsonRecord): { delegated_task_id: string | null; worker_run_id: string | null } {
  const nestedResult = record(result.result);
  const refs = [
    ...records(result.worker_refs),
    ...records(result.run_refs),
    ...records(result.worker_summaries),
    ...records(nestedResult.worker_refs),
    ...records(nestedResult.run_refs),
    ...records(nestedResult.worker_summaries),
  ];
  const workerRunId = refs
    .map((ref) => typeof ref.run_id === 'string' ? ref.run_id.trim() : '')
    .find((runId) => runId.length > 0) ?? null;
  return {
    delegated_task_id: typeof result.task_id === 'string' ? result.task_id : null,
    worker_run_id: workerRunId,
  };
}

function mapDelegatedResult(result: JsonRecord, request: TaskExecutabilityRequest): DelegatedTaskResult {
  const identity = resultIdentity(result);
  const taskStatus = String(result.task_status ?? result.execution_status ?? '');
  if (taskStatus === 'failed' || taskStatus === 'cancelled') {
    return { status: 'failed', ...identity, error: { kind: 'delegated_task_failed', message: String(result.summary ?? taskStatus) } };
  }
  const assessment = findAssessment(result);
  if (taskStatus === 'completed' && assessment) {
    return { status: 'completed', ...identity, output: assessmentForRequest(assessment, request, result) };
  }
  return { status: taskStatus === 'accepted_for_execution' ? 'accepted' : 'running', ...identity };
}

function makeDelegatedTaskPort(
  state: ReturnType<typeof createServerState>,
  requestContexts: Map<string, TaskExecutabilityRequest>,
): DelegatedTaskPort {
    return {
    async run(args: DelegatedTaskInvocation) {
      const request = requestContexts.get(args.idempotency_key);
      if (!request) return { status: 'failed', error: { kind: 'request_context_missing', message: 'No leased Task Lifecycle request was found for the dispatch.' } };
      const response = await delegatedTaskRun({
        objective: `Assess executability of task ${args.task_number}.`,
        request_id: args.idempotency_key,
        intent: {
          objective: `Assess executability of task ${args.task_number}.`,
          instructions: `Inspect only this canonical task packet and declared environment.\nTask packet: ${JSON.stringify(request.task_packet)}\nDeclared environment: ${JSON.stringify(request.environment)}`,
        },
        constraints: {
          authority: 'read',
          cognition: 'low',
          runtime: 'narada-agent-runtime-server',
          cwd: state.siteRoot,
          site_root: state.siteRoot,
          max_run_ms: args.constraints.max_run_ms,
          max_retries: 0,
          max_concurrency: 1,
          resumable: false,
          skip_git_repo_check: true,
        },
        workflow: { template_id: WORKFLOW_TEMPLATE_ID },
        acceptance: { required_tools: [], residual_risk_policy: 'allow' },
        // Site Loop runs as a bounded process rather than a resident delegated
        // task server. Keep the worker child inside this invocation until its
        // terminal result is persisted; an async child would lose its in-memory
        // completion supervisor when the Site Loop runner exits.
        execution: {
          start: true,
          wait_for_completion: true,
          timeout_ms: args.constraints.max_run_ms,
          poll_ms: 100,
          resumable: false,
          max_retries: 0,
        },
        execution_binding: {
          workspace_root: state.siteRoot,
          site_root: state.siteRoot,
          executor_kind: 'site_loop',
          correlation_key: args.idempotency_key,
        },
        idempotency_key: args.idempotency_key,
      }, state);
      return mapDelegatedResult(record(response), request);
    },
    async poll(args: DelegatedTaskPoll) {
      if (!args.delegated_task_id) return { status: 'failed', error: { kind: 'delegated_task_identity_missing', message: 'No delegated task id was persisted for the assessment.' } };
      const request = requestContexts.get(args.idempotency_key);
      if (!request) return { status: 'failed', error: { kind: 'request_context_missing', message: 'No leased Task Lifecycle request was found for the poll.' } };
      const response = await delegatedTaskResult({ task_id: args.delegated_task_id, refresh: true, include_diagnostics: true }, state);
      return mapDelegatedResult(record(response), request);
    },
  };
}

export function createTaskExecutabilityOrchestratorForSiteLoop(args: {
  siteRoot: string;
  store: unknown;
  orchestrator?: TaskExecutabilityOrchestrator;
  maxAttempts?: number;
  maxRunMs?: number;
}): TaskExecutabilityOrchestrator {
  if (args.orchestrator) return args.orchestrator;
  const db = (args.store as { db: ConstructorParameters<typeof SqliteTaskLifecycleStore>[0]['db'] }).db;
  const lifecycleStore = new SqliteTaskLifecycleStore({ db });
  const requestContexts = new Map<string, TaskExecutabilityRequest>();
  const lifecycle = makeLifecyclePort(lifecycleStore, args.siteRoot, requestContexts);
  const workerPolicyConfig = join(args.siteRoot, '.narada', 'worker-policy.toml');
  const delegatedState = createServerState({
    taskRoot: args.siteRoot,
    siteRoot: args.siteRoot,
    outputRoot: args.siteRoot,
    allowedRoots: [args.siteRoot],
    ...(existsSync(workerPolicyConfig) ? { workerPolicy: { config: workerPolicyConfig } } : {}),
  });
  // The Site Loop already owns the disciplined lifecycle connection for this
  // run. Bind the delegated-task gate to that connection instead of letting
  // it open a second connection to the same database while the loop holds its
  // write lease. A second synchronous SQLite connection can block the
  // Site Loop event loop before the worker completion supervisor can observe
  // the child NARS terminal event.
  delegatedState.taskLifecycleStore = lifecycleStore;
  const delegated = makeDelegatedTaskPort(delegatedState, requestContexts);
  return new TaskExecutabilityOrchestrator(lifecycle, delegated, {
    consumer_id: `site-loop:${args.siteRoot}`,
    max_attempts: args.maxAttempts ?? 3,
    max_run_ms: args.maxRunMs ?? 120_000,
  });
}

export async function runTaskExecutabilityReconciliation(siteRoot: string, options: JsonRecord = {}): Promise<JsonRecord> {
  if (!options.store && !options.orchestrator) {
    return {
      schema: TASK_EXECUTABILITY_SITE_LOOP_SCHEMA,
      status: 'deferred',
      reason: 'task_lifecycle_store_not_bound',
      attention: { code: 'task_executability_reconciliation_not_bound', severity: 'warning' },
    };
  }
  const orchestrator = createTaskExecutabilityOrchestratorForSiteLoop({
    siteRoot,
    store: options.store,
    orchestrator: options.orchestrator as TaskExecutabilityOrchestrator | undefined,
    maxAttempts: Number(options.max_attempts ?? options.maxAttempts ?? 3),
    maxRunMs: Number(options.max_run_ms ?? options.maxRunMs ?? 120_000),
  });
  const limit = Math.max(1, Math.min(MAX_BATCH, Number(options.limit ?? 1)));
  const batch = await orchestrator.reconcileAll(limit);
  const failures = batch.results.filter((result) => result.outcome === 'failed_retryable' || result.outcome === 'failed_terminal');
  return {
    schema: TASK_EXECUTABILITY_SITE_LOOP_SCHEMA,
    status: failures.length > 0 ? 'attention' : 'ok',
    outcome: batch.stopped,
    limit,
    results: batch.results,
    counts: {
      completed: batch.results.filter((result) => result.outcome === 'completed').length,
      dispatched: batch.results.filter((result) => result.outcome === 'dispatched' || result.outcome === 'pending').length,
      idle: batch.results.filter((result) => result.outcome === 'idle').length,
      failures: failures.length,
    },
    attention: failures.length > 0
      ? { code: 'task_executability_reconciliation_failure', severity: failures.some((result) => result.outcome === 'failed_terminal') ? 'error' : 'warning', failures }
      : null,
  };
}
