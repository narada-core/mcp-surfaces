/**
 * Inbox-to-Task-Lifecycle Bridge
 *
 * Evaluates unprocessed inbox envelopes and materializes high-severity items
 * as claimable tasks. Implements Phase 2 of the inbox-visibility-bridge
 * architecture.
 */
import { existsSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';
import { allocateTaskNumbers } from '@narada-core/task-governance-core/task-governance';
import { renderTaskBodyFromSpec } from '@narada-core/task-governance-core/task-spec';
import { openTaskLifecycleStore } from '@narada-core/task-governance-core/task-lifecycle-store';
import { readAdmissionLog, getLatestEventsByEnvelope, appendAdmissionEvent, acknowledgeEnvelope, dismissEnvelope } from '../inbox/admission-log.js';
import { readTaskLifecycleSitePolicy } from './task-lifecycle-site-policy.js';
import {
  evaluateEnvelopeSeverity,
  findDuplicateTaskRows,
  hasEnvelopeCoverageEvidence,
  levenshteinDistance,
} from '../inbox/inbox-policy.js';

const INBOX_DIR: any = '.ai/inbox-envelopes';
const TASKS_DIR: any = '.ai/do-not-open/tasks';
type TaskLifecyclePayload = Record<string, unknown>;

function asPayload(value: unknown): TaskLifecyclePayload {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as TaskLifecyclePayload : {};
}

const AUTO_MATERIALIZE_THRESHOLD: any = 50;

export { evaluateEnvelopeSeverity, levenshteinDistance };

const OWNERSHIP_FIELD_PRECEDENCE: any = [
  'preferred_agent_id',
  'assigned_agent_id',
  'responsible_agent_id',
  'owner',
];
const ROLE_FIELD_PRECEDENCE: any = ['target_role', 'requested_role'];

function normalizedNonEmptyString(value: any) {
  return typeof value === 'string' && value.trim().length > 0 ? value.trim() : null;
}

function firstPayloadString(payload: any, fields: any) {
  for (const field of fields) {
    const value: any = normalizedNonEmptyString(payload?.[field]);
    if (!value) continue;
    return { field, value };
  }
  return null;
}

function firstEnvelopeOrPayloadString(envelope: any, field: any) {
  const envelopeValue: any = normalizedNonEmptyString(envelope?.[field]);
  if (envelopeValue) return { field: `envelope.${field}`, value: envelopeValue };
  const payloadValue: any = normalizedNonEmptyString(envelope?.payload?.[field]);
  return payloadValue ? { field: `payload.${field}`, value: payloadValue } : null;
}

function isCanonicalScheduledSopReplacement(envelope: any) {
  return envelope?.kind === 'command_request'
    && envelope?.source?.kind === 'scheduler_schedule'
    && normalizedNonEmptyString(envelope?.supersedes_envelope_id) !== null
    && normalizedNonEmptyString(envelope?.payload?.sop_id) !== null;
}

function resolveAgentRoleFromStore(store: any, agentId: any) {
  if (!store || !agentId) return null;
  try {
    const row: any = store.db.prepare('SELECT role FROM agent_roster WHERE agent_id = ?').get(agentId);
    return normalizedNonEmptyString(row?.role);
  } catch {
    return null;
  }
}

function ensureTaskRolePreferencesTable(store: any) {
  store.db.exec(`
    CREATE TABLE IF NOT EXISTS narada_andrey_task_role_preferences (
      task_id TEXT PRIMARY KEY,
      preferred_role TEXT,
      target_role TEXT,
      preferred_agent_id TEXT,
      updated_at TEXT NOT NULL
    )
  `);
  try {
    store.db.exec('ALTER TABLE narada_andrey_task_role_preferences ADD COLUMN target_role TEXT');
  } catch {
    // column already exists
  }
  try {
    store.db.exec('ALTER TABLE narada_andrey_task_role_preferences ADD COLUMN preferred_agent_id TEXT');
  } catch {
    // column already exists
  }
}

export function deriveRoutingFromEnvelopePayload(envelope: any, severityResult: TaskLifecyclePayload = {}, store : any= null) {
  const payload: any = envelope?.payload ?? {};
  const ownership: any = firstPayloadString(payload, OWNERSHIP_FIELD_PRECEDENCE);
  const explicitRole: any = firstEnvelopeOrPayloadString(envelope, 'target_role')
    ?? firstPayloadString(payload, ROLE_FIELD_PRECEDENCE);
  const preferredAgentId: any = ownership?.value ?? null;
  const agentRole: any = resolveAgentRoleFromStore(store, preferredAgentId);
  let targetRole: any = explicitRole?.value ?? agentRole ?? severityResult.targetRole ?? null;
  const warnings: any[] = [];

  const ownershipValues: any = new Map();
  for (const field of OWNERSHIP_FIELD_PRECEDENCE) {
    const value: any = normalizedNonEmptyString(payload[field]);
    if (value) ownershipValues.set(field, value);
  }
  const uniqueOwners: any = new Set(ownershipValues.values());
  if (uniqueOwners.size > 1) {
    warnings.push({
      kind: 'ambiguous_payload_ownership',
      selected_field: ownership?.field ?? null,
      selected_agent_id: preferredAgentId,
      fields: Object.fromEntries(ownershipValues),
    });
  }

  if (explicitRole?.value && agentRole && explicitRole.value !== agentRole) {
    warnings.push({
      kind: 'payload_role_agent_role_mismatch',
      target_role_field: explicitRole.field,
      target_role: explicitRole.value,
      preferred_agent_id: preferredAgentId,
      resolved_agent_role: agentRole,
    });
    targetRole = explicitRole.value;
  }

  return {
    targetRole,
    preferredAgentId,
    source: {
      targetRoleField: explicitRole?.field ?? (agentRole ? 'agent_roster' : 'severity_result'),
      preferredAgentField: ownership?.field ?? null,
      resolvedAgentRole: agentRole,
    },
    warnings,
  };
}

/**
 * Check whether an envelope already has a corresponding open task.
 * Returns { isDuplicate, duplicateTaskId, duplicateTaskNumber, matchType }.
 */
export function checkDuplicateTask(store: any, envelope: any) {
  const envelopeId: any = envelope.envelope_id;
  const title: any = String(envelope.title ?? envelope.payload?.title ?? '').trim();

  // 1. Fast path: check durable envelope_task_mappings table
  if (envelopeId && store.getTaskByEnvelopeId) {
    const mapping: any = store.getTaskByEnvelopeId(envelopeId);
    if (mapping) {
      return {
        isDuplicate: true,
        duplicateTaskId: mapping.task_id,
        duplicateTaskNumber: mapping.task_number,
        matchType: 'mapping_table',
      };
    }
  }

  // 2. Scan ALL tasks (including closed and in_review) to prevent re-materialization
  // of envelopes that were already processed, regardless of final disposition.
  const sql: any = `
    SELECT s.task_id, s.task_number, s.title, s.context_markdown, s.goal_markdown,
           s.required_work_markdown, s.non_goals_markdown, l.status
    FROM task_specs s
    INNER JOIN task_lifecycle l ON s.task_id = l.task_id
  `;

  const rows: any = store.db.prepare(sql).all();

  for (const row of rows) {
    if (hasEnvelopeCoverageEvidence(row, envelopeId)) {
      return {
        isDuplicate: true,
        duplicateTaskId: row.task_id,
        duplicateTaskNumber: Number(row.task_number),
        matchType: 'envelope_id_in_context',
      };
    }
  }

  // A canonical scheduled-SOP replacement is an explicit hard-cutover
  // boundary. It may intentionally retain the legacy title, so fuzzy title
  // matching must not route it back into the superseded task. Exact mapping
  // and envelope-ID evidence above still remain authoritative for retries.
  if (isCanonicalScheduledSopReplacement(envelope)) {
    return {
      isDuplicate: false,
      duplicateTaskId: null,
      duplicateTaskNumber: null,
      matchType: null,
    };
  }

  return findDuplicateTaskRows(rows, envelope);
}

/**
 * Build a task spec from an inbox envelope and severity evaluation.
 */
export function buildTaskSpecFromEnvelope(envelope: any, severityResult: any, options: TaskLifecyclePayload = {}) {
  const payload: any = envelope.payload ?? {};
  const title: any = `[From Inbox] ${envelope.title ?? payload.title ?? 'Untitled'}`;
  const goal: any = envelope.summary ?? payload.summary ?? payload.description ?? '';
  const routing: any = asPayload(options.routing ?? deriveRoutingFromEnvelopePayload(envelope, severityResult, options.store ?? null));

  const evidence: any = Array.isArray(payload.evidence) ? payload.evidence : [];
  const proposals: any = Array.isArray(payload.proposal) ? payload.proposal : [];

  const contextLines: any = [
    `**Envelope ID:** ${envelope.envelope_id}`,
    `**Received:** ${envelope.received_at}`,
    `**Kind:** ${envelope.kind}`,
    `**Title:** ${envelope.title ?? payload.title ?? 'Untitled'}`,
    `**Summary:** ${envelope.summary ?? payload.summary ?? payload.description ?? ''}`,
    `**Target Role:** ${envelope.target_role ?? payload.target_role ?? 'unknown'}`,
    `**Authority:** ${envelope.authority?.level ?? 'unknown'} (${envelope.authority?.principal ?? 'unknown'})`,
    `**Source:** ${envelope.source?.ref ?? 'unknown'}`,
  ];
  const routingWarnings: any = Array.isArray(routing.warnings) ? routing.warnings : [];
  if (routing.preferredAgentId || routing.targetRole || routingWarnings.length > 0) {
    contextLines.push(
      '',
      '**Lifecycle Routing:**',
      JSON.stringify({
        target_role: routing.targetRole,
        preferred_agent_id: routing.preferredAgentId,
        source: routing.source,
        warnings: routingWarnings,
      }, null, 2)
    );
  }
  contextLines.push(
    '',
    '**Payload:**',
    JSON.stringify(payload, null, 2),
  );
  const context: any = contextLines.join('\n');

  const workItems: any[] = [];
  if (proposals.length > 0) {
    for (let i: any = 0; i < proposals.length; i++) {
      workItems.push(`${i + 1}. ${proposals[i]}`);
    }
  } else if (typeof payload.sop_id === 'string' && payload.sop_id.trim()) {
    const triggeredBy: any = routing.preferredAgentId ?? envelope.target_role ?? 'operator';
    workItems.push(
      `Start SOP ${payload.sop_id.trim()} through SOP MCP with trigger_source_kind="inbox_event", trigger_source_ref="${envelope.envelope_id}", and triggered_by="${triggeredBy}".`,
    );
  } else {
    workItems.push('1. Review envelope content and determine disposition');
  }

  const acceptanceCriteria: any[] = [];
  if (evidence.length > 0) {
    acceptanceCriteria.push('Review and acknowledge evidence');
  }
  if (proposals.length > 0) {
    for (const p of proposals) {
      acceptanceCriteria.push(`Address proposal: ${p}`);
    }
  }
  if (typeof payload.sop_id === 'string' && payload.sop_id.trim()) {
    acceptanceCriteria.push(`Record the SOP run evidence with trigger_source_ref="${envelope.envelope_id}"`);
  }
  acceptanceCriteria.push('Submit disposition to inbox (acknowledge / dismiss / escalate)');

  const nonGoals: any = ['Do not leave envelope in unprocessed state'];

  return {
    title,
    goal,
    context,
    requiredWork: workItems,
    nonGoals,
    acceptanceCriteria,
    preferredRole: routing.targetRole,
    targetRole: routing.targetRole,
    preferredAgentId: routing.preferredAgentId,
    routingWarnings: routing.warnings,
    routingSource: routing.source,
    relativePriority: severityResult.relativePriority ?? severityResult.severity ?? 0,
    priorityReason: severityResult.reason,
  } as TaskLifecyclePayload;
}

/**
 * Build the read-side bridge decision before write-side effects run.
 * Outcome statuses are intentionally effect-free:
 * - ignored: severity/action says not to materialize
 * - duplicate: an existing task/mapping already covers the envelope
 * - materializable: write-side handler may create a task and mark the envelope
 */
export function decideEnvelopeBridgeOutcome({ store, envelope, severityResult, dryRun = false }: any) {
  const routing: any = deriveRoutingFromEnvelopePayload(envelope, severityResult, store);
  const base: any = {
    schema: 'narada.bridge.outcome.v0',
    envelopeId: envelope.envelope_id,
    kind: envelope.kind,
    severity: severityResult.severity,
    action: severityResult.action,
    targetRole: routing.targetRole,
    preferredAgentId: routing.preferredAgentId,
    routingSource: routing.source,
    routingWarnings: routing.warnings,
    reason: severityResult.reason ?? null,
  };

  if (severityResult.action !== 'materialize') {
    return {
      ...base,
      status: 'ignored',
      outcome: 'ignored',
    };
  }

  const dupCheck: any = checkDuplicateTask(store, envelope);
  if (dupCheck.isDuplicate) {
    return {
      ...base,
      status: 'duplicate',
      outcome: 'duplicate',
      duplicateTaskId: dupCheck.duplicateTaskId,
      duplicateTaskNumber: dupCheck.duplicateTaskNumber,
      matchType: dupCheck.matchType,
    };
  }

  return {
    ...base,
    status: 'materializable',
    outcome: dryRun ? 'dry_run_materializable' : 'materializable',
    dryRun,
    wouldCreate: true,
  };
}

export function summarizeBridgeOutcome(outcome: any) {
  return {
    envelopeId: outcome.envelopeId,
    kind: outcome.kind,
    severity: outcome.severity,
    action: outcome.action,
    targetRole: outcome.targetRole,
    preferredAgentId: outcome.preferredAgentId,
    routingWarnings: outcome.routingWarnings,
    outcome: outcome.outcome,
    status: outcome.status,
  };
}

function slugify(text: any) {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '')
    .slice(0, 40);
}

function todayYmd() {
  const d: any = new Date();
  const y: any = d.getFullYear();
  const m: any = String(d.getMonth() + 1).padStart(2, '0');
  const day: any = String(d.getDate()).padStart(2, '0');
  return `${y}${m}${day}`;
}

/**
 * Materialize a single inbox envelope as a task.
 * Returns { status, taskNumber?, taskId?, filePath?, error?, envelopeId? }.
 */
export async function materializeEnvelopeAsTask(cwd: any, envelope: any) {
  const severityResult: any = evaluateEnvelopeSeverity(envelope);
  if (severityResult.action !== 'materialize') {
    return {
      status: 'skipped_not_materializable',
      envelopeId: envelope.envelope_id,
      severity: severityResult.severity,
      action: severityResult.action,
    };
  }

  const store: any = openTaskLifecycleStore(cwd, { mode: 'runtime' });
  const routing: any = deriveRoutingFromEnvelopePayload(envelope, severityResult, store);
  const spec: any = buildTaskSpecFromEnvelope(envelope, severityResult, { routing });
  const specRecord: any = asPayload(spec);
  const requiredWork: any = Array.isArray(specRecord.requiredWork) ? specRecord.requiredWork.map(String) : [];
  const nonGoals: any = Array.isArray(specRecord.nonGoals) ? specRecord.nonGoals.map(String) : [];
  const acceptanceCriteria: any = Array.isArray(specRecord.acceptanceCriteria) ? specRecord.acceptanceCriteria.map(String) : [];
  const rolesAreObligationTargets: any = readTaskLifecycleSitePolicy(cwd).policy.roster.roles_are_obligation_targets;
  const preferredRole: any = rolesAreObligationTargets && typeof specRecord.preferredRole === 'string' ? specRecord.preferredRole : null;
  const targetRole: any = rolesAreObligationTargets && typeof specRecord.targetRole === 'string' ? specRecord.targetRole : null;
  const preferredAgentId: any = typeof specRecord.preferredAgentId === 'string' ? specRecord.preferredAgentId : null;
  const relativePriority: any = typeof specRecord.relativePriority === 'number' ? specRecord.relativePriority : 0;
  const priorityReason: any = typeof specRecord.priorityReason === 'string' ? specRecord.priorityReason : null;
  const taskNumber: any = (await allocateTaskNumbers(cwd, 1))[0];
  const slug: any = slugify(String(specRecord.title ?? 'inbox-task'));
  const taskId: any = `${todayYmd()}-${taskNumber}-${slug}`;
  const tasksDir: any = join(resolve(cwd), TASKS_DIR);
  const filePath: any = join(tasksDir, `${taskId}.md`);

  // Extract proposal content from envelope payload to seed Execution Notes
  const payload: any = envelope.payload ?? {};
  const proposals: any = Array.isArray(payload.proposal) ? payload.proposal : [];
  const evidence: any = Array.isArray(payload.evidence) ? payload.evidence : [];
  let executionNotes: any = null;
  if (proposals.length > 0 || evidence.length > 0) {
    const parts: any[] = [];
    if (evidence.length > 0) {
      parts.push('Evidence:', ...evidence.map((e: any) => `- ${e}`));
    }
    if (proposals.length > 0) {
      parts.push('Proposals:', ...proposals.map((p: any) => `- ${p}`));
    }
    executionNotes = parts.join('\n');
  }

  const body: any = renderTaskBodyFromSpec({
    spec: {
      title: String(specRecord.title ?? ''),
      goal: String(specRecord.goal ?? ''),
      context: String(specRecord.context ?? ''),
      chapter: typeof specRecord.chapter === 'string' ? specRecord.chapter : null,
      required_work: requiredWork.join('\n'),
      non_goals: nonGoals.join('\n'),
      acceptance_criteria: acceptanceCriteria,
    },
    executionNotes,
    verification: null,
  });

  const frontMatterLines: any = [
    '---',
    `number: ${taskNumber}`,
    `governed_by: ${preferredRole || 'unknown'}`,
    'status: opened',
  ];
  if (preferredRole) {
    frontMatterLines.push(`preferred_role: ${preferredRole}`);
    frontMatterLines.push(`target_role: ${targetRole ?? preferredRole}`);
  }
  if (preferredAgentId) {
    frontMatterLines.push(`preferred_agent_id: ${preferredAgentId}`);
  }
  if (relativePriority) {
    frontMatterLines.push(`relative_priority: ${relativePriority}`);
  }
  if (priorityReason) {
    frontMatterLines.push(`priority_reason: ${priorityReason}`);
  }
  frontMatterLines.push('---');

  const fileContent: any = `${frontMatterLines.join('\n')}\n${body}`;
  writeFileSync(filePath, fileContent, 'utf8');

  const now: any = new Date().toISOString();
  try {
    store.upsertLifecycle({
      task_id: taskId,
      task_number: taskNumber,
      status: 'opened',
      governed_by: preferredRole,
      closed_at: null,
      closed_by: null,
      reopened_at: null,
      reopened_by: null,
      continuation_packet_json: null,
      updated_at: now,
      relative_priority: relativePriority,
      priority_reason: priorityReason,
    });
    store.upsertTaskSpec({
      task_id: taskId,
      task_number: taskNumber,
      title: String(specRecord.title ?? ''),
      chapter_markdown: null,
      goal_markdown: String(specRecord.goal ?? ''),
      context_markdown: String(specRecord.context ?? ''),
      required_work_markdown: requiredWork.join('\n'),
      non_goals_markdown: nonGoals.join('\n'),
      acceptance_criteria_json: JSON.stringify(acceptanceCriteria),
      dependencies_json: '[]',
      updated_at: now,
    });
    if (preferredRole || preferredAgentId) {
      ensureTaskRolePreferencesTable(store);
      store.db.prepare(`
        INSERT INTO narada_andrey_task_role_preferences (task_id, preferred_role, target_role, preferred_agent_id, updated_at)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(task_id) DO UPDATE SET
          preferred_role = excluded.preferred_role,
          target_role = excluded.target_role,
          preferred_agent_id = excluded.preferred_agent_id,
          updated_at = excluded.updated_at
      `).run(taskId, preferredRole, targetRole ?? preferredRole, preferredAgentId, now);
    }
  } finally {
    store.db.close();
  }

  return {
    status: 'materialized',
    envelopeId: envelope.envelope_id,
    taskNumber,
    taskId,
    filePath,
    severity: severityResult.severity,
    targetRole: spec.targetRole,
    preferredAgentId: spec.preferredAgentId,
    routingWarnings: spec.routingWarnings,
  };
}

/**
 * Update an envelope's status after materialization.
 *
 * Primary: append envelope_promoted event to admission log.
 * Fallback: rewrite filesystem JSON for backward compatibility.
 */
export function markEnvelopeMaterialized(cwd: any, envelope: any, taskNumber: any, taskId: any) {
  // Primary: append promotion event to admission log
  let logEvent: any = null;
  try {
    logEvent = appendAdmissionEvent(cwd, {
      event_kind: 'envelope_promoted',
      envelope_id: envelope.envelope_id,
      principal: 'inbox-bridge',
      authority_level: 'system_generated',
      payload_hash: null,
      payload_uri: null,
      promotion: {
        target_kind: 'task',
        target_ref: `task:${taskNumber}`,
        task_id: taskId,
        promoted_at: new Date().toISOString(),
        promoted_by: 'inbox-bridge',
      },
    });
  } catch {
    // Log append failed; continue with filesystem fallback
  }

  // Fallback: rewrite filesystem JSON for backward compatibility
  const envelopeDir: any = join(resolve(cwd), INBOX_DIR);
  const fileName: any = `${envelope.envelope_id}.json`;
  const filePath: any = join(envelopeDir, fileName);
  const altFilePath: any = join(envelopeDir, envelope.received_at
    ? `${envelope.received_at.replace(/[:.]/g, '-').replace('Z', 'Z')}-${fileName}`
    : fileName);

  let pathToUpdate: any = null;
  if (existsSync(filePath)) {
    pathToUpdate = filePath;
  } else if (existsSync(altFilePath)) {
    pathToUpdate = altFilePath;
  } else {
    const files: any = readdirSync(envelopeDir).filter((f: any) => f.endsWith('.json'));
    for (const f of files) {
      if (f.endsWith(fileName)) {
        pathToUpdate = join(envelopeDir, f);
        break;
      }
    }
  }

  if (pathToUpdate) {
    const updated: any = {
      ...envelope,
      status: 'promoted',
      promotion: {
        target_kind: 'task',
        target_ref: `task:${taskNumber}`,
        task_id: taskId,
        promoted_at: new Date().toISOString(),
        promoted_by: 'inbox-bridge',
      },
    };
    writeFileSync(pathToUpdate, JSON.stringify(updated, null, 2), 'utf8');
  }

  return {
    status: 'marked',
    envelopeId: envelope.envelope_id,
    path: pathToUpdate,
    log_event_id: logEvent?.event_id ?? null,
  };
}

export async function applyMaterializableBridgeOutcome({ cwd, store, envelope, outcome }: any) {
  if (outcome.status !== 'materializable') {
    return { status: 'not_materializable', outcome };
  }
  if (outcome.dryRun) {
    return {
      status: 'dry_run',
      envelopeId: envelope.envelope_id,
      severity: outcome.severity,
      targetRole: outcome.targetRole,
      preferredAgentId: outcome.preferredAgentId,
      routingWarnings: outcome.routingWarnings,
      wouldCreate: true,
      outcome,
    };
  }

  const result: any = await materializeEnvelopeAsTask(cwd, envelope);
  if (result.status !== 'materialized') {
    return {
      status: result.status,
      envelopeId: envelope.envelope_id,
      severity: result.severity,
      reason: result.status,
      outcome,
    };
  }

  const markResult: any = markEnvelopeMaterialized(cwd, envelope, result.taskNumber, result.taskId);
  let materializationEventId: any = null;
  try {
    const event: any = appendAdmissionEvent(cwd, {
      event_kind: 'bridge_materialized',
      envelope_id: envelope.envelope_id,
      principal: 'inbox-bridge',
      authority_level: 'system_generated',
      payload_hash: null,
      payload_uri: null,
      materialization: {
        task_number: result.taskNumber,
        task_id: result.taskId,
        severity: result.severity,
        target_role: result.targetRole,
        preferred_agent_id: result.preferredAgentId ?? null,
        routing_warnings: result.routingWarnings ?? [],
      },
    });
    materializationEventId = event?.event_id ?? null;
  } catch {
    // Non-blocking: log emission failure should not prevent materialization
  }

  let mappingWritten: any = false;
  let mappingError: any = null;
  try {
    store.upsertEnvelopeTaskMapping(
      envelope.envelope_id,
      result.taskId,
      result.taskNumber,
      new Date().toISOString()
    );
    mappingWritten = true;
  } catch (err: any) {
    mappingError = err instanceof Error ? err.message : String(err);
  }

  const committablePathSet: any = buildBridgeCommittablePathSet(cwd, result, markResult);
  return {
    status: 'materialized',
    envelopeId: envelope.envelope_id,
    taskNumber: result.taskNumber,
    taskId: result.taskId,
    severity: result.severity,
    targetRole: result.targetRole,
    preferredAgentId: result.preferredAgentId,
    routingWarnings: result.routingWarnings,
    markResult,
    materialization_event_id: materializationEventId,
    mapping_written: mappingWritten,
    mapping_error: mappingError,
    committable_path_set: committablePathSet,
    commit_ready: committablePathSet,
    outcome,
  };
}

function buildBridgeCommittablePathSet(cwd: any, materializeResult: any, markResult: any) {
  const taskPath: any = relativeSitePath(cwd, materializeResult.filePath);
  const envelopePath: any = markResult?.path ? relativeSitePath(cwd, markResult.path) : null;
  const ignoredEnvelopeProjectionPaths: any = envelopePath && envelopePath.startsWith(`${INBOX_DIR}/`) ? [envelopePath] : [];
  return {
    schema: 'narada.inbox_bridge.committable_path_set.v0',
    task_owned_paths: [taskPath],
    ordinary_task_closeout_paths: [taskPath],
    ignored_envelope_projection_paths: ignoredEnvelopeProjectionPaths,
    envelope_handoff_tool: 'git_handoff_inbox_envelope_export',
    guidance: ignoredEnvelopeProjectionPaths.length > 0
      ? 'Use ordinary task closeout commits for ordinary_task_closeout_paths only. If the ignored envelope projection must be exported, use git_handoff_inbox_envelope_export for that exact .ai/inbox-envelopes JSON path.'
      : 'Use ordinary task closeout commits for ordinary_task_closeout_paths.',
  };
}

function relativeSitePath(cwd: any, path: any) {
  return relative(resolve(cwd), path).replace(/\\/g, '/');
}

/**
 * Read all unprocessed inbox envelopes.
 *
 * Hybrid approach: uses admission log as primary source of truth for
 * processed/unprocessed status, but falls back to filesystem scan for
 * envelopes that predate the log.
 */
function readEnvelopeFiles(cwd: any) {
  const envelopeDir: any = join(resolve(cwd), INBOX_DIR);
  if (!existsSync(envelopeDir)) {
    return [];
  }
  return readdirSync(envelopeDir)
    .filter((f: any) => f.endsWith('.json'))
    .map((fileName: any) => {
      const filePath: any = join(envelopeDir, fileName);
      try {
        return { envelope: JSON.parse(readFileSync(filePath, 'utf8')), fileName, filePath };
      } catch {
        return null;
      }
    })
    .filter((entry: any) => entry?.envelope);
}

export function readEnvelopeById(cwd: any, envelopeId: any) {
  return readEnvelopeFiles(cwd).find((entry: any) => entry.envelope?.envelope_id === envelopeId) ?? null;
}

function updateEnvelopeDispositionFile(entry: any, status: any, resolution: any) {
  if (!entry?.filePath) return false;
  const updated: any = { ...entry.envelope, status, resolution };
  writeFileSync(entry.filePath, JSON.stringify(updated, null, 2), 'utf8');
  entry.envelope = updated;
  return true;
}

export function readUnprocessedEnvelopes(cwd: any) {
  const fileEnvelopes: any = readEnvelopeFiles(cwd).map((entry: any) => entry.envelope);

  // Try admission log first
  try {
    const latestEvents: any = getLatestEventsByEnvelope(cwd);
    const processedKinds: any = new Set(['envelope_promoted', 'envelope_dismissed', 'envelope_acknowledged', 'bridge_materialized']);
    const logEnvelopes: any[] = [];

    for (const envelope of fileEnvelopes) {
      const latest: any = latestEvents.get(envelope.envelope_id);
      if (latest) {
        if (!processedKinds.has(latest.event_kind)) {
          logEnvelopes.push(envelope);
        }
      } else {
        // Envelope has no log event yet (predates log) — fall back to filesystem status
        if ((envelope.status ?? 'received') === 'received') {
          logEnvelopes.push(envelope);
        }
      }
    }
    return logEnvelopes;
  } catch {
    // Admission log unavailable — fall back to pure filesystem scan
    return fileEnvelopes.filter((e: any) => (e.status ?? 'received') === 'received');
  }
}

export async function targetInboxEnvelope(cwd: any, options: TaskLifecyclePayload = {}) {
  const envelopeId: any = options.envelopeId ?? options.envelope_id;
  if (!envelopeId) throw new Error('envelope_id_required');

  const dryRun: any = Boolean(options.dryRun ?? options.dry_run ?? false);
  const disposition: any = options.disposition ?? 'materialize';
  const principal: any = options.principal ?? 'task-lifecycle-targeted-inbox';
  const reason: any = options.reason ?? null;

  const entry: any = readEnvelopeById(cwd, envelopeId);
  if (!entry) {
    return {
      schema: 'narada.bridge.target_envelope.v0',
      status: 'not_found',
      envelope_id: envelopeId,
      dry_run: dryRun,
    };
  }

  const envelope: any = entry.envelope;
  const severityResult: any = evaluateEnvelopeSeverity(envelope);
  let store: any = null;
  try {
    store = openTaskLifecycleStore(cwd, { mode: 'runtime' });
    const outcome: any = decideEnvelopeBridgeOutcome({ store, envelope, severityResult, dryRun });
    const base: any = {
      schema: 'narada.bridge.target_envelope.v0',
      status: 'ok',
      envelope_id: envelopeId,
      disposition,
      dry_run: dryRun,
      envelope: {
        kind: envelope.kind,
        status: envelope.status ?? 'received',
        received_at: envelope.received_at ?? null,
        title: envelope.payload?.title ?? envelope.title ?? null,
        source_ref: envelope.source?.ref ?? null,
      },
      severity: severityResult,
      bridge_outcome: outcome,
      evidence: [],
    };

    if (dryRun) {
      return {
        ...base,
        preview: true,
        would_mutate: disposition !== 'preview',
        mutation: disposition === 'materialize'
          ? (outcome.status === 'materializable' ? 'materialize_task' : outcome.status)
          : `append_${disposition}_disposition`,
      };
    }

    if (disposition === 'preview') {
      return { ...base, preview: true, would_mutate: false };
    }

    if (disposition === 'materialize') {
      if (outcome.status !== 'materializable') {
        return { ...base, status: outcome.status, result: { status: outcome.status, outcome } };
      }
      const result: any = await applyMaterializableBridgeOutcome({ cwd, store, envelope, outcome });
      return { ...base, status: result.status, result };
    }

    if (disposition === 'acknowledge' || disposition === 'already_routed') {
      const eventReason: any = reason ?? (disposition === 'already_routed' ? 'Envelope already routed outside broad bridge polling.' : 'Envelope acknowledged through targeted disposition.');
      const event: any = acknowledgeEnvelope(cwd, envelopeId, principal, eventReason);
      const filesystemUpdated: any = updateEnvelopeDispositionFile(entry, 'acknowledged', {
        action: disposition,
        resolved_at: event.timestamp,
        resolved_by: principal,
        reason: eventReason,
      });
      return {
        ...base,
        status: disposition === 'already_routed' ? 'already_routed' : 'acknowledged',
        event_id: event.event_id,
        event_sequence: event.event_sequence,
        filesystem_updated: filesystemUpdated,
        evidence: [{ kind: 'admission_log', event_id: event.event_id, event_kind: event.event_kind }],
      };
    }

    if (disposition === 'dismiss') {
      if (!reason) throw new Error('reason_required_for_dismiss');
      const event: any = dismissEnvelope(cwd, envelopeId, principal, reason);
      const filesystemUpdated: any = updateEnvelopeDispositionFile(entry, 'dismissed', {
        action: 'dismissed',
        resolved_at: event.timestamp,
        resolved_by: principal,
        reason,
      });
      return {
        ...base,
        status: 'dismissed',
        event_id: event.event_id,
        event_sequence: event.event_sequence,
        filesystem_updated: filesystemUpdated,
        evidence: [{ kind: 'admission_log', event_id: event.event_id, event_kind: event.event_kind }],
      };
    }

    if (disposition === 'defer') {
      const event: any = appendAdmissionEvent(cwd, {
        envelope_id: envelopeId,
        event_kind: 'envelope_deferred',
        principal,
        authority_level: 'agent_reported',
        payload_hash: null,
        payload_uri: entry.fileName ? `${INBOX_DIR}/${entry.fileName}` : null,
        event_payload: { reason },
      });
      return {
        ...base,
        status: 'deferred',
        event_id: event.event_id,
        event_sequence: event.event_sequence,
        evidence: [{ kind: 'admission_log', event_id: event.event_id, event_kind: event.event_kind }],
      };
    }

    throw new Error(`unsupported_disposition: ${disposition}`);
  } finally {
    if (store) store.db.close();
  }
}

/**
 * Poll the inbox bridge: evaluate all unprocessed envelopes,
 * check deduplication, and materialize high-severity items.
 *
 * Options:
 *   - dryRun: boolean (default false)
 *   - threshold: number (default 50)
 *   - limit: number (default 20)
 *
 * Returns { evaluated, materialized, skipped, duplicates, errors }.
 */
export async function pollInboxBridge(cwd: any, options: TaskLifecyclePayload = {}) {
  const dryRun: any = Boolean(options.dryRun ?? false);
  const threshold: any = options.threshold ?? AUTO_MATERIALIZE_THRESHOLD;
  const limit: any = typeof options.limit === 'number' ? options.limit : 20;

  let envelopes: any = readUnprocessedEnvelopes(cwd);
  // Sort by severity descending so highest-priority items are processed first
  envelopes = envelopes
    .map((e: any) => ({ envelope: e, severityResult: evaluateEnvelopeSeverity(e) }))
    .sort((a: any, b: any) => b.severityResult.severity - a.severityResult.severity)
    .map((item: any) => item.envelope);

  const evaluated: any[] = [];
  const materialized: any[] = [];
  const skipped: any[] = [];
  const duplicates: any[] = [];
  const errors: any[] = [];

  // A Site Loop already owns the disciplined lifecycle-store connection for
  // the whole cycle. Reuse it when supplied; opening a second connection to
  // the same database while the loop holds its write lease can block the
  // first phase before it can record a step.
  let store: any = options.store ?? null;
  const ownsStore = store === null;
  try {
    if (ownsStore) store = openTaskLifecycleStore(cwd, { mode: 'runtime' });
  } catch (e: any) {
    return {
      status: 'error',
      error: `failed_to_open_store: ${e.message}`,
      evaluated,
      materialized,
      skipped,
      duplicates,
      errors,
    };
  }

  try {
    let processed: any = 0;
    for (const envelope of envelopes) {
      if (processed >= limit) break;
      processed++;

      const severityResult: any = evaluateEnvelopeSeverity(envelope);
      const outcome: any = decideEnvelopeBridgeOutcome({ store, envelope, severityResult, dryRun }) as TaskLifecyclePayload;
      evaluated.push(summarizeBridgeOutcome(outcome));

      if (outcome.status === 'ignored') {
        skipped.push({
          envelopeId: outcome.envelopeId,
          severity: outcome.severity,
          action: outcome.action,
          reason: outcome.reason,
          outcome: outcome.outcome,
        });
        continue;
      }

      if (outcome.status === 'duplicate') {
        duplicates.push({
          envelopeId: outcome.envelopeId,
          duplicateTaskId: outcome.duplicateTaskId,
          duplicateTaskNumber: outcome.duplicateTaskNumber,
          matchType: outcome.matchType,
          outcome: outcome.outcome,
        });
        continue;
      }

      if (outcome.dryRun) {
        materialized.push({
          envelopeId: outcome.envelopeId,
          status: 'dry_run',
          severity: outcome.severity,
          targetRole: outcome.targetRole,
          preferredAgentId: outcome.preferredAgentId,
          routingWarnings: outcome.routingWarnings,
          wouldCreate: true,
          outcome: outcome.outcome,
        });
        continue;
      }

      try {
        const result: any = await applyMaterializableBridgeOutcome({ cwd, store, envelope, outcome });
        if (result.status === 'materialized') {
          materialized.push({
            envelopeId: envelope.envelope_id,
            taskNumber: result.taskNumber,
            taskId: result.taskId,
            severity: result.severity,
            targetRole: result.targetRole,
            preferredAgentId: result.preferredAgentId,
            routingWarnings: result.routingWarnings,
            marked: result.markResult?.status === 'marked',
            mapping_written: result.mapping_written,
            outcome: 'materialized',
          });
        } else {
          skipped.push({
            envelopeId: envelope.envelope_id,
            severity: result.severity,
            reason: result.status,
            outcome: result.outcome?.outcome ?? result.status,
          });
        }
      } catch (err: any) {
        errors.push({
          envelopeId: envelope.envelope_id,
          error: err instanceof Error ? err.message : String(err),
        });
      }
    }
  } finally {
    if (ownsStore) store.db.close();
  }

  return {
    schema: 'narada.bridge.poll.v0',
    status: 'ok',
    evaluated: evaluated.length,
    materialized: materialized.length,
    skipped: skipped.length,
    duplicates: duplicates.length,
    errors: errors.length,
    dry_run: dryRun,
    threshold,
    details: { evaluated, materialized, skipped, duplicates, errors },
  };
}
