import { existsSync } from 'node:fs';
import { basename, dirname, isAbsolute, relative, resolve } from 'node:path';
import { normalizeExecutionBinding, type ExecutionBinding } from '@narada-core/execution-contract';
import { normalizeTaskTags, requireTaskTagsArray } from '@narada-core/task-governance-core/task-tags';
import {
  enqueueTaskExecutabilityRequest,
} from '@narada-core/task-governance-core/task-executability-service';
import {
  bindTaskExecution,
  ensureTaskExecutionTables,
  getTaskCreationReservation,
  markTaskCreationStatus,
  reserveTaskCreation,
  taskCreationPayloadFingerprint,
} from './task-execution-state.js';

type TaskLifecyclePayload = Record<string, unknown>;

function asPayload(value: unknown): TaskLifecyclePayload {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as TaskLifecyclePayload : {};
}

const TASK_EXECUTABILITY_FOLLOW_UP_SCHEMA = 'narada.task.executability.follow_up.v1';

function taskExecutabilityFollowUp(request: Record<string, unknown> | null, status: 'enqueued' | 'existing'): Record<string, unknown> | null {
  if (!request) return null;
  return {
    schema: TASK_EXECUTABILITY_FOLLOW_UP_SCHEMA,
    version: 1,
    source: { surface: 'task_lifecycle', operation: 'task_lifecycle_create' },
    trigger: 'on_create',
    status,
    request_id: request.request_id,
    task_id: request.task_id,
    task_number: request.task_number,
    task_spec_digest: request.task_spec_digest,
    environment_digest: request.environment_digest,
    evaluator_profile: request.evaluator_profile,
    evaluator_profile_version: request.evaluator_profile_version,
  };
}

export function assertExecutionBindingScope(binding: ExecutionBinding, siteRoot: string): void {
  const currentSiteRoot = resolve(siteRoot);
  const bindingSiteRoot = binding.site_root ? resolve(binding.site_root) : currentSiteRoot;
  const containedSiteRoot = resolve(currentSiteRoot, '.narada');
  const containedSiteDeclared = bindingSiteRoot === containedSiteRoot
    && existsSync(resolve(currentSiteRoot, '.git'))
    && existsSync(resolve(containedSiteRoot, 'config.json'));
  if (bindingSiteRoot !== currentSiteRoot && !containedSiteDeclared) {
    throw new Error('task_lifecycle_execution_binding_site_root_mismatch');
  }
  if (!executionRootAuthorizedForSite(binding.workspace_root, bindingSiteRoot)) {
    throw new Error('task_lifecycle_execution_binding_workspace_outside_site_root');
  }
  if (binding.repository_root && !executionRootAuthorizedForSite(binding.repository_root, bindingSiteRoot)) {
    throw new Error('task_lifecycle_execution_binding_repository_outside_site_root');
  }
}

function executionRootAuthorizedForSite(candidateRoot: string, siteRoot: string): boolean {
  if (pathWithinRoot(candidateRoot, siteRoot)) return true;
  if (basename(siteRoot).toLowerCase() !== '.narada') return false;
  const projectRoot = dirname(siteRoot);
  return resolve(candidateRoot) === projectRoot && existsSync(resolve(projectRoot, '.git'));
}

function pathWithinRoot(path: string, root: string): boolean {
  const child = resolve(path);
  const relativePath = relative(root, child);
  return relativePath === '' || (!relativePath.startsWith('..') && !isAbsolute(relativePath));
}

export const TASK_LIFECYCLE_CREATE_RECURRING_TOOL_NAMES = Object.freeze([
  "task_lifecycle_roster_admit",
  "task_lifecycle_create",
  "task_lifecycle_recurring_create",
  "task_lifecycle_recurring_show",
  "task_lifecycle_recurring_list",
  "task_lifecycle_recurring_suspend",
  "task_lifecycle_recurring_retire",
  "task_lifecycle_recurring_trigger",
  "task_lifecycle_recurring_run_due",
  "task_lifecycle_recurring_runs"
]);

export function createTaskLifecycleCreateRecurringHandlers(context: any) {
  const {
    store,
    siteRoot,
    jsonToolResult,
    stringField,
    numberField,
    booleanField,
    arrayOfStrings,
    admitRosterIdentity,
    enforceSessionIdentity,
    allocateTaskNumbers,
    slugify,
    todayYmd,
    renderTaskBodyFromSpec,
    writeFileSync,
    join,
    randomUUID,
    attachPayloadSource,
    roleExistsInRoster,
    normalizeRecurringAuthorityBasis,
    requireRecurringAuthorityActor,
    ensureTaskRoutingTables,
    ensureRecurringTaskTables,
    insertRecurringDefinition,
    insertRecurringEvent,
    hydrateRecurringDefinition,
    getRecurringDefinition,
    listRecurringRuns,
    listRecurringDefinitions,
    updateRecurringDefinitionStatus,
    parseIsoOrNow,
    listDueRecurringDefinitions,
    recurringDueKey,
    createRecurringTaskInstance,
    insertRecurringRun,
    getSitePolicy,
  } = context;

  async function dispatchCreateRecurringTool(canonicalName: any, args: any, dispatchContext: TaskLifecyclePayload = {}) {
    switch (canonicalName) {
    case 'task_lifecycle_roster_admit': {
      return jsonToolResult(admitRosterIdentity(args));
    }

















    case 'task_lifecycle_create': {
      const payloadSource = asPayload(dispatchContext.payloadSource);
      const title = stringField(args, 'title');
      if (!title) throw new Error('title_required');
      const goal = stringField(args, 'goal') || title;
      const context = stringField(args, 'context') || null;
      const requiredWork = stringField(args, 'required_work') || '1. TBD';
      const nonGoals = stringField(args, 'non_goals') || null;
      const tags = args.tags === undefined ? [] : requireTaskTagsArray(args.tags);
      const preferredRole = stringField(args, 'preferred_role') || null;
      const targetRole = stringField(args, 'target_role') || null;
      const recoveryTruthfulnessRequired = args.recovery_truthfulness_required === true;
      if ((preferredRole || targetRole) && !rolesAreObligationTargets()) {
        return jsonToolResult(roleObligationTargetsDisabledResult({ preferredRole, targetRole }), true);
      }
      const acceptanceCriteria = Array.isArray(args.acceptance_criteria) && args.acceptance_criteria.length > 0
        ? args.acceptance_criteria
        : ['TBD'];
      ensureTaskExecutionTables(store);
      const idempotencyKey = stringField(args, 'idempotency_key') || String(payloadSource.ref || '');
      if (!idempotencyKey) throw new Error('task_lifecycle_create_idempotency_key_required');
      const executionBinding = normalizeExecutionBinding(args.execution_binding, {
        workspace_root: siteRoot,
        executor_kind: 'manual',
        site_root: siteRoot,
        correlation_key: idempotencyKey,
      });
      assertExecutionBindingScope(executionBinding, siteRoot);
      const payloadSha256 = taskCreationPayloadFingerprint(args, payloadSource.sha256);
      const existingReservation = getTaskCreationReservation(store, idempotencyKey);
      const taskNumberCandidate = existingReservation?.task_number ?? (await allocateTaskNumbers(siteRoot, 1))[0];
      const slug = slugify(title);
      const taskIdCandidate = existingReservation?.task_id ?? `${todayYmd()}-${taskNumberCandidate}-${slug}`;
      const tasksDir = join(siteRoot, '.ai', 'do-not-open', 'tasks');
      const filePathCandidate = existingReservation?.file_path ?? join(tasksDir, `${taskIdCandidate}.md`);
      const reservationResult = reserveTaskCreation(store, {
        idempotency_key: idempotencyKey,
        payload_sha256: payloadSha256,
        task_id: taskIdCandidate,
        task_number: taskNumberCandidate,
        file_path: filePathCandidate,
        execution_binding_json: JSON.stringify(executionBinding),
      });
      const reservation = reservationResult.reservation;
      const taskNumber = reservation.task_number;
      const taskId = reservation.task_id;
      const filePath = reservation.file_path;
      if (reservation.status === 'created') {
        let executabilityRequest: Record<string, unknown> | null = null;
        try {
          executabilityRequest = enqueueTaskExecutabilityRequest({
            store,
            siteRoot,
            taskId,
            taskNumber,
            spec: {
              title,
              goal,
              context,
              required_work: requiredWork,
              non_goals: nonGoals,
              acceptance_criteria: Array.isArray(acceptanceCriteria) ? acceptanceCriteria : [],
              dependencies: [],
            },
          }) as unknown as Record<string, unknown>;
        } catch {
          // A later Site Loop reconciliation can recover an existing task without
          // making an idempotent create retry fail for this secondary concern.
        }
        return jsonToolResult(attachPayloadSource({
          schema: 'narada.task.create.v0',
          status: 'already_exists',
          task_number: taskNumber,
          task_id: taskId,
          file_path: filePath,
          title,
          tags,
          idempotency_key: idempotencyKey,
          execution_binding: executionBinding,
          recovered: false,
          follow_up: taskExecutabilityFollowUp(executabilityRequest, 'existing'),
        }, payloadSource));
      }

      const body = renderTaskBodyFromSpec({
        spec: {
          title,
          goal,
          context,
          required_work: requiredWork,
          non_goals: nonGoals,
          acceptance_criteria: acceptanceCriteria,
        },
        executionNotes: null,
        verification: null,
      });

      const frontMatterLines = [
        '---',
        `number: ${taskNumber}`,
        `governed_by: ${preferredRole || 'unknown'}`,
        'status: opened',
      ];
      if (preferredRole) {
        frontMatterLines.push(`preferred_role: ${preferredRole}`);
      }
      if (targetRole) {
        frontMatterLines.push(`target_role: ${targetRole}`);
      }
      if (tags.length > 0) {
        frontMatterLines.push(`tags: ${tags.join(', ')}`);
      }
      if (recoveryTruthfulnessRequired) {
        frontMatterLines.push('recovery_truthfulness_required: true');
      }
      if (payloadSource.ref) {
        frontMatterLines.push(`creation_payload_ref: ${payloadSource.ref}`);
      }
      if (payloadSource.sha256) {
        frontMatterLines.push(`creation_payload_sha256: ${payloadSource.sha256}`);
      }
      frontMatterLines.push(`idempotency_key: ${idempotencyKey}`);
      frontMatterLines.push(`execution_binding_json: ${JSON.stringify(executionBinding)}`);
      frontMatterLines.push('---');

      const fileContent = `${frontMatterLines.join('\n')}\n${body}`;
      if (!existsSync(filePath)) writeFileSync(filePath, fileContent, 'utf8');

      const now = new Date().toISOString();
      store.db.exec('BEGIN IMMEDIATE');
      try {
        store.upsertLifecycle({
          task_id: taskId,
          task_number: taskNumber,
          status: 'opened',
          governed_by: preferredRole || null,
          closed_at: null,
          closed_by: null,
          reopened_at: null,
          reopened_by: null,
          continuation_packet_json: null,
          updated_at: now,
        });
        store.upsertTaskSpec({
          task_id: taskId,
          task_number: taskNumber,
          title,
          chapter_markdown: null,
          goal_markdown: goal,
          context_markdown: context,
          required_work_markdown: requiredWork,
          non_goals_markdown: nonGoals,
          acceptance_criteria_json: JSON.stringify(acceptanceCriteria),
          dependencies_json: '[]',
          tags_json: JSON.stringify(tags),
          updated_at: now,
        });
        bindTaskExecution(store, { task_id: taskId, task_number: taskNumber }, executionBinding);
        ensureTaskRoutingTables(store);
        if (preferredRole || targetRole) {
          store.db.prepare(`
            INSERT INTO narada_andrey_task_role_preferences (task_id, preferred_role, target_role, preferred_agent_id, updated_at)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(task_id) DO UPDATE SET
              preferred_role = excluded.preferred_role,
              target_role = excluded.target_role,
              preferred_agent_id = excluded.preferred_agent_id,
              updated_at = excluded.updated_at
          `).run(taskId, preferredRole, targetRole || preferredRole, null, now);
        }
        markTaskCreationStatus(store, idempotencyKey, 'created');
        store.db.exec('COMMIT');
      } catch (error) {
        try { store.db.exec('ROLLBACK'); } catch { /* preserve the original failure */ }
        throw error;
      }

      // Enqueue an executability request for every newly created task. The call
      // is idempotent and superseded older in-flight requests automatically.
      let executabilityRequest: Record<string, unknown> | null = null;
      try {
        executabilityRequest = enqueueTaskExecutabilityRequest({
          store,
          siteRoot,
          taskId,
          taskNumber,
          spec: {
            title,
            goal,
            context,
            required_work: requiredWork,
            non_goals: nonGoals,
            acceptance_criteria: Array.isArray(acceptanceCriteria) ? acceptanceCriteria : [],
            dependencies: [],
          },
        }) as unknown as Record<string, unknown>;
      } catch {
        // Executability enqueue is best-effort on creation; failures are surfaced
        // by the dedicated executability tools and do not block task creation.
      }

      return jsonToolResult(attachPayloadSource({
        schema: 'narada.task.create.v0',
        status: reservationResult.created ? 'created' : 'recovered',
        task_number: taskNumber,
        task_id: taskId,
        file_path: filePath,
        title,
        tags,
        idempotency_key: idempotencyKey,
        execution_binding: executionBinding,
        recovered: !reservationResult.created,
        target_role: targetRole || preferredRole,
        preferred_role: preferredRole,
        recovery_truthfulness_required: recoveryTruthfulnessRequired,
        payload_ref: payloadSource.ref ?? null,
        payload_sha256: payloadSource.sha256 ?? null,
        follow_up: taskExecutabilityFollowUp(executabilityRequest, 'enqueued'),
      }, payloadSource));
    }

    case 'task_lifecycle_recurring_create': {
      const title = stringField(args, 'title');
      const actorAgentId = stringField(args, 'actor_agent_id');
      const authorityBasis = normalizeRecurringAuthorityBasis(args.authority_basis);
      if (!title) throw new Error('title_required');
      if (!actorAgentId) throw new Error('actor_agent_id_required');
      if (!authorityBasis) throw new Error('valid_authority_basis_required');
      enforceSessionIdentity(actorAgentId);
      const actorRole = requireRecurringAuthorityActor({ store, siteRoot, actorAgentId });
      const initialStatus = stringField(args, 'initial_status') || 'active';
      if (!['draft', 'active'].includes(initialStatus)) throw new Error('invalid_initial_status');
      const targetRole = stringField(args, 'target_role') || null;
      const preferredRole = stringField(args, 'preferred_role') || targetRole;
      const tags = args.tags === undefined ? [] : requireTaskTagsArray(args.tags);
      if ((targetRole || preferredRole) && !rolesAreObligationTargets()) {
        return jsonToolResult(roleObligationTargetsDisabledResult({ preferredRole, targetRole }), true);
      }
      if (targetRole && !roleExistsInRoster(store, siteRoot, targetRole)) {
        return jsonToolResult({ status: 'blocked', reason: 'target_role_not_in_roster', target_role: targetRole }, true);
      }
      if (preferredRole && !roleExistsInRoster(store, siteRoot, preferredRole)) {
        return jsonToolResult({ status: 'blocked', reason: 'preferred_role_not_in_roster', preferred_role: preferredRole }, true);
      }
      const triggerMode = stringField(args, 'trigger_mode') || 'manual';
      if (!['manual', 'schedule'].includes(triggerMode)) throw new Error('invalid_trigger_mode');
      const scheduleKind = stringField(args, 'schedule_kind') || (triggerMode === 'schedule' ? 'daily' : null);
      if (triggerMode === 'schedule' && scheduleKind !== 'daily') throw new Error('unsupported_schedule_kind');
      if (triggerMode === 'manual' && scheduleKind) throw new Error('schedule_kind_requires_schedule_trigger_mode');
      const scheduleTimezone = stringField(args, 'schedule_timezone') || (triggerMode === 'schedule' ? 'UTC' : null);
      if (scheduleTimezone && scheduleTimezone !== 'UTC') throw new Error('unsupported_schedule_timezone');
      const recurrenceId = `rtask_${randomUUID()}`;
      const now = new Date().toISOString();
      const definition = {
        recurrence_id: recurrenceId,
        title,
        status: initialStatus,
        trigger_mode: triggerMode,
        trigger_description: stringField(args, 'trigger_description') || null,
        schedule_kind: scheduleKind,
        schedule_interval: triggerMode === 'schedule' ? 1 : null,
        schedule_timezone: scheduleTimezone,
        last_due_key: null,
        last_auto_triggered_at: null,
        target_role: targetRole,
        preferred_role: preferredRole,
        goal_markdown: stringField(args, 'goal') || title,
        context_markdown: stringField(args, 'context') || null,
        required_work_markdown: stringField(args, 'required_work') || '1. Execute the recurring task instance.',
        non_goals_markdown: stringField(args, 'non_goals') || null,
        acceptance_criteria_json: JSON.stringify(arrayOfStrings(args.acceptance_criteria, ['Complete the recurring task instance with verification evidence.'])),
        tags,
        evidence_requirements_json: JSON.stringify(arrayOfStrings(args.evidence_requirements, [])),
        created_by: actorAgentId,
        created_at: now,
        updated_at: now,
        suspended_at: null,
        retired_at: null,
      };
      ensureRecurringTaskTables(store);
      store.db.exec('BEGIN');
      try {
        insertRecurringDefinition(store, definition);
        insertRecurringEvent(store, {
          recurrenceId,
          eventType: 'created',
          stateAfter: initialStatus,
          actorAgentId,
          authorityBasis,
          event: { actor_role: actorRole, title },
          now,
        });
        store.db.exec('COMMIT');
      } catch (error) {
        try { store.db.exec('ROLLBACK'); } catch { /* ignore rollback failure */ }
        throw error;
      }
      return jsonToolResult({
        schema: 'narada.task.recurring.definition.v0',
        status: 'created',
        recurrence_id: recurrenceId,
        definition: hydrateRecurringDefinition(definition),
      });
    }

    case 'task_lifecycle_recurring_show': {
      const recurrenceId = stringField(args, 'recurrence_id');
      if (!recurrenceId) throw new Error('recurrence_id_required');
      const definition = getRecurringDefinition(store, recurrenceId);
      if (!definition) return jsonToolResult({ status: 'not_found', recurrence_id: recurrenceId }, true);
      const includeRuns = booleanField(args, 'include_runs') ?? true;
      return jsonToolResult({
        schema: 'narada.task.recurring.show.v0',
        status: 'ok',
        definition,
        runs: includeRuns ? listRecurringRuns(store, recurrenceId, 20) : [],
      });
    }

    case 'task_lifecycle_recurring_list': {
      const status = stringField(args, 'status');
      const limit = numberField(args, 'limit') ?? 50;
      return jsonToolResult({
        schema: 'narada.task.recurring.list.v0',
        status: 'ok',
        definitions: listRecurringDefinitions(store, { status, limit }),
      });
    }

    case 'task_lifecycle_recurring_suspend': {
      return jsonToolResult(updateRecurringDefinitionStatus({
        store,
        siteRoot,
        recurrenceId: stringField(args, 'recurrence_id'),
        actorAgentId: stringField(args, 'actor_agent_id'),
        authorityBasis: normalizeRecurringAuthorityBasis(args.authority_basis),
        nextStatus: 'suspended',
        eventType: 'suspended',
        reason: stringField(args, 'reason'),
      }));
    }

    case 'task_lifecycle_recurring_retire': {
      return jsonToolResult(updateRecurringDefinitionStatus({
        store,
        siteRoot,
        recurrenceId: stringField(args, 'recurrence_id'),
        actorAgentId: stringField(args, 'actor_agent_id'),
        authorityBasis: normalizeRecurringAuthorityBasis(args.authority_basis),
        nextStatus: 'retired',
        eventType: 'retired',
        reason: stringField(args, 'reason'),
      }));
    }

    case 'task_lifecycle_recurring_trigger': {
      const recurrenceId = stringField(args, 'recurrence_id');
      const actorAgentId = stringField(args, 'actor_agent_id');
      const authorityBasis = normalizeRecurringAuthorityBasis(args.authority_basis);
      const runReason = stringField(args, 'run_reason');
      if (!recurrenceId) throw new Error('recurrence_id_required');
      if (!actorAgentId) throw new Error('actor_agent_id_required');
      if (!authorityBasis) throw new Error('valid_authority_basis_required');
      if (!runReason) throw new Error('run_reason_required');
      enforceSessionIdentity(actorAgentId);
      const actorRole = requireRecurringAuthorityActor({ store, siteRoot, actorAgentId });
      const definition = getRecurringDefinition(store, recurrenceId);
      if (!definition) return jsonToolResult({ status: 'not_found', recurrence_id: recurrenceId }, true);
      if (definition.status !== 'active') {
        return jsonToolResult({
          status: 'blocked',
          reason: 'recurrence_not_active',
          recurrence_id: recurrenceId,
          current_status: definition.status,
        }, true);
      }
      const now = new Date().toISOString();
      const taskNumber = (await allocateTaskNumbers(siteRoot, 1))[0];
      const taskTitle = `${definition.title} (${now.slice(0, 10)})`;
      const taskId = `${todayYmd()}-${taskNumber}-${slugify(taskTitle)}`;
      const tasksDir = join(siteRoot, '.ai', 'do-not-open', 'tasks');
      const filePath = join(tasksDir, `${taskId}.md`);
      const evidenceRequirements = definition.evidence_requirements;
      const tags = normalizeTaskTags(definition.tags);
      const recurrenceContext = [
        definition.context_markdown,
        '',
        `Recurring task definition: ${recurrenceId}`,
        `Manual run reason: ${runReason}`,
        evidenceRequirements.length > 0 ? `Evidence requirements: ${evidenceRequirements.join('; ')}` : null,
      ].filter(Boolean).join('\n');
      const body = renderTaskBodyFromSpec({
        spec: {
          title: taskTitle,
          goal: definition.goal_markdown || definition.title,
          context: recurrenceContext,
          required_work: definition.required_work_markdown || 'Execute the recurring task instance.',
          non_goals: definition.non_goals_markdown,
          acceptance_criteria: definition.acceptance_criteria,
        },
        executionNotes: null,
        verification: null,
      });
      const frontMatterLines = [
        '---',
        `number: ${taskNumber}`,
        `governed_by: ${rolesAreObligationTargets() ? (definition.preferred_role || definition.target_role || 'unknown') : 'unknown'}`,
        'status: opened',
        `recurring_task_id: ${recurrenceId}`,
      ];
      if (tags.length > 0) frontMatterLines.push(`tags: ${tags.join(', ')}`);
      if (rolesAreObligationTargets() && definition.preferred_role) frontMatterLines.push(`preferred_role: ${definition.preferred_role}`);
      if (rolesAreObligationTargets() && definition.target_role) frontMatterLines.push(`target_role: ${definition.target_role}`);
      frontMatterLines.push('---');
      const runId = `rtrun_${randomUUID()}`;
      store.db.exec('BEGIN');
      try {
        writeFileSync(filePath, `${frontMatterLines.join('\n')}\n${body}`, 'utf8');
        store.upsertLifecycle({
          task_id: taskId,
          task_number: taskNumber,
          status: 'opened',
          governed_by: rolesAreObligationTargets() ? (definition.preferred_role || definition.target_role || null) : null,
          closed_at: null,
          closed_by: null,
          reopened_at: null,
          reopened_by: null,
          continuation_packet_json: null,
          updated_at: now,
        });
        store.upsertTaskSpec({
          task_id: taskId,
          task_number: taskNumber,
          title: taskTitle,
          chapter_markdown: null,
          goal_markdown: definition.goal_markdown || definition.title,
          context_markdown: recurrenceContext,
          required_work_markdown: definition.required_work_markdown || 'Execute the recurring task instance.',
          non_goals_markdown: definition.non_goals_markdown,
          acceptance_criteria_json: JSON.stringify(definition.acceptance_criteria),
          dependencies_json: '[]',
          tags_json: JSON.stringify(tags),
          updated_at: now,
        });
        ensureTaskRoutingTables(store);
        if (rolesAreObligationTargets() && (definition.preferred_role || definition.target_role)) {
          store.db.prepare(`
            INSERT INTO narada_andrey_task_role_preferences (task_id, preferred_role, target_role, preferred_agent_id, updated_at)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(task_id) DO UPDATE SET
              preferred_role = excluded.preferred_role,
              target_role = excluded.target_role,
              preferred_agent_id = excluded.preferred_agent_id,
              updated_at = excluded.updated_at
          `).run(taskId, definition.preferred_role, definition.target_role || definition.preferred_role, null, now);
        }
        insertRecurringRun(store, {
          run_id: runId,
          recurrence_id: recurrenceId,
          task_id: taskId,
          task_number: taskNumber,
          due_key: null,
          trigger_mode: 'manual',
          reason: runReason,
          actor_agent_id: actorAgentId,
          authority_basis_json: JSON.stringify(authorityBasis),
          created_at: now,
        });
        insertRecurringEvent(store, {
          recurrenceId,
          eventType: 'manual_triggered',
          stateAfter: definition.status,
          actorAgentId,
          authorityBasis,
          event: { actor_role: actorRole, run_id: runId, task_id: taskId, task_number: taskNumber, run_reason: runReason },
          now,
        });
        store.db.exec('COMMIT');
      } catch (error) {
        try { store.db.exec('ROLLBACK'); } catch { /* ignore rollback failure */ }
        throw error;
      }
      return jsonToolResult({
        schema: 'narada.task.recurring.trigger.v0',
        status: 'triggered',
        recurrence_id: recurrenceId,
        run_id: runId,
        task_number: taskNumber,
        task_id: taskId,
        file_path: filePath,
      });
    }

    case 'task_lifecycle_recurring_run_due': {
      const actorAgentId = stringField(args, 'actor_agent_id');
      const authorityBasis = normalizeRecurringAuthorityBasis(args.authority_basis);
      if (!actorAgentId) throw new Error('actor_agent_id_required');
      if (!authorityBasis) throw new Error('valid_authority_basis_required');
      enforceSessionIdentity(actorAgentId);
      const actorRole = requireRecurringAuthorityActor({ store, siteRoot, actorAgentId });
      const now = parseIsoOrNow(stringField(args, 'current_time'));
      const limit = Math.max(1, Math.min(numberField(args, 'limit') ?? 20, 100));
      const dueDefinitions = listDueRecurringDefinitions(store, { now, limit });
      const created = [];
      const skipped = [];
      for (const definition of dueDefinitions) {
        const dueKey = recurringDueKey(definition, now);
        if (!dueKey || definition.last_due_key === dueKey) {
          skipped.push({ recurrence_id: definition.recurrence_id, reason: 'not_due_or_already_created', due_key: dueKey });
          continue;
        }
        const result = await createRecurringTaskInstance({
          store,
          siteRoot,
          definition,
          actorAgentId,
          actorRole,
          authorityBasis,
          triggerMode: 'schedule',
          runReason: `Scheduled daily run for ${dueKey}`,
          eventType: 'scheduled_triggered',
          now,
          dueKey,
        });
        created.push(result);
      }
      return jsonToolResult({
        schema: 'narada.task.recurring.run_due.v0',
        status: 'ok',
        trigger_mode: 'schedule',
        schedule_kind: 'daily',
        evaluated_at: now.toISOString(),
        created_count: created.length,
        skipped_count: skipped.length,
        created,
        skipped,
      });
    }

    case 'task_lifecycle_recurring_runs': {
      const recurrenceId = stringField(args, 'recurrence_id');
      if (!recurrenceId) throw new Error('recurrence_id_required');
      const limit = numberField(args, 'limit') ?? 20;
      return jsonToolResult({
        schema: 'narada.task.recurring.runs.v0',
        status: 'ok',
        recurrence_id: recurrenceId,
        runs: listRecurringRuns(store, recurrenceId, limit),
      });
    }

      default:
        throw new Error(`task_mcp_refused: ${canonicalName}`);
    }
  }

  function rolesAreObligationTargets() {
    return getSitePolicy().policy.roster.roles_are_obligation_targets;
  }

  return Object.fromEntries(TASK_LIFECYCLE_CREATE_RECURRING_TOOL_NAMES.map((name: any) => [name, (args: any, dispatchContext: any) => dispatchCreateRecurringTool(name, args, dispatchContext)]));
}

function roleObligationTargetsDisabledResult({ preferredRole, targetRole }: any) {
  return {
    status: 'blocked',
    reason: 'roles_are_obligation_targets_false',
    preferred_role: preferredRole ?? null,
    target_role: targetRole ?? null,
    message: 'Role-targeted task creation is disabled by site task-lifecycle policy. Set [roster].roles_are_obligation_targets = true in <site-root>/.narada/task-lifecycle.toml to permit role obligation targets.',
    site_policy: {
      roster: {
        roles_are_obligation_targets: false,
      },
    },
  };
}
