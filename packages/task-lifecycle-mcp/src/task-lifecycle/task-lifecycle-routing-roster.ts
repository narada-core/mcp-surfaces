import { randomUUID } from 'crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'path';

export function buildTaskLifecycleIdentityState({ agentId, operation, status = 'not_evaluated', evidenceRefs = [], authorityBasis = null }: any) {
  const identity = typeof agentId === 'string' && agentId.trim() ? agentId.trim() : null;
  const granted = status === 'authorized';
  return {
    schema: 'narada.agent.identity_state.v1',
    claimed_identity: {
      identity,
      status: identity ? 'claimed' : 'unclaimed',
      source: identity ? 'task_lifecycle_request' : null,
      asserted_at: identity ? new Date().toISOString() : null,
      evidence_refs: Array.isArray(evidenceRefs) ? evidenceRefs : [],
      authority_granted: false,
    },
    authentication: {
      status: 'missing',
      authenticated_identity: null,
      evidence_refs: [],
    },
    authority: {
      status: status === 'denied' ? 'denied' : granted ? 'authorized' : 'not_evaluated',
      operation: operation ?? null,
      granted,
      evidence_refs: authorityBasis?.summary ? [authorityBasis.summary] : (Array.isArray(evidenceRefs) ? evidenceRefs : []),
    },
  };
}

export function readTaskRouting(store: any, taskId: any, spec : any= null) {
  let rolePref = null;
  try {
    rolePref = store.db.prepare(
      'SELECT target_role, preferred_role, preferred_agent_id FROM narada_andrey_task_role_preferences WHERE task_id = ?'
    ).get(taskId);
  } catch {
    rolePref = null;
  }
  return {
    policy: 'preferred_agent_id_is_soft_affinity_target_role_is_role_gate',
    target_role: rolePref?.target_role || rolePref?.preferred_role || spec?.target_role || spec?.preferred_role || null,
    preferred_agent_id: rolePref?.preferred_agent_id || spec?.preferred_agent_id || null,
    override_authority_required_when_claiming_nonpreferred: true,
    allowed_override_authority_kinds: ['operator_direct_instruction', 'directed_obligation', 'task_owner_handoff'],
  };
}

export function buildRoutingAssignmentDivergence({ lifecycle, routing, assignment, reports }: any) {
  const preferredAgentId = routing?.preferred_agent_id ?? null;
  const activeAgentId = assignment?.agent_id ?? null;
  const reportAgentIds = [...new Set((reports || []).map((report: any) => report.agent_id).filter(Boolean))];
  const finishedBy = lifecycle.closed_by ?? (reportAgentIds.length === 1 ? reportAgentIds[0] : null);
  const activeMismatch = Boolean(preferredAgentId && activeAgentId && preferredAgentId !== activeAgentId);
  const finishedMismatch = Boolean(preferredAgentId && finishedBy && preferredAgentId !== finishedBy);
  return {
    policy: 'preferred_agent_id_is_not_assignment',
    preferred_agent_id: preferredAgentId,
    active_assignment_agent_id: activeAgentId,
    finished_by: finishedBy,
    report_agent_ids: reportAgentIds,
    active_assignment_diverges_from_preferred: activeMismatch,
    finished_assignment_diverges_from_preferred: finishedMismatch,
    explanation: activeMismatch || finishedMismatch
      ? 'Preferred routing diverges from active or finished assignment; this is allowed only when claim intent evidence records override authority.'
      : 'No preferred-agent divergence observed.',
  };
}

export function validatePreferredAgentMismatchAuthority({ args, eligibility, lifecycle, taskNumber, agentId }: any) {
  if (!eligibility.warning || !eligibility.preferredAgentId || eligibility.preferredAgentId === agentId) {
    return { status: 'not_required', authority_basis: null, preferred_agent_warning: null };
  }
  const preferredAgentWarning = {
    kind: 'preferred_agent_mismatch',
    severity: 'requires_authority',
    warning: 'preferred_agent_mismatch',
    task_number: taskNumber,
    preferred_agent_id: eligibility.preferredAgentId,
    claiming_agent: agentId,
    message: eligibility.warning,
  };
  const authorityBasis = normalizeClaimAuthorityBasis(args.authority_basis);
  if (!authorityBasis) {
    return {
      status: 'blocked',
      authority_basis: null,
      preferred_agent_warning: preferredAgentWarning,
    };
  }
  return {
    status: 'ok',
    authority_basis: {
      ...authorityBasis,
      task_id: lifecycle.task_id,
      task_number: taskNumber,
      preferred_agent_id: eligibility.preferredAgentId,
      claiming_agent: agentId,
    },
    preferred_agent_warning: preferredAgentWarning,
  };
}

export function normalizeClaimAuthorityBasis(value: any) {
  const record = asRecord(value);
  const kind = stringField(record, 'kind');
  const summary = stringField(record, 'summary');
  const allowedKinds = new Set(['operator_direct_instruction', 'directed_obligation', 'task_owner_handoff']);
  if (!kind || !allowedKinds.has(kind) || !summary) return null;
  return { kind, summary };
}

export function recordClaimIntent({ store, lifecycle, taskNumber, agentId, status, assignmentId = null, rejectionReason = null, authorityBasis = null, preferredAgentWarning = null }: any) {
  if (!store.upsertAssignmentIntent) return;
  const now = new Date().toISOString();
  const record = {
    request_id: `claim-${randomUUID()}`,
    kind: 'claim',
    task_id: sqliteText(lifecycle.task_id),
    task_number: sqliteInteger(taskNumber),
    agent_id: sqliteText(agentId),
    requested_by: sqliteText(agentId),
    requested_at: now,
    reason: sqliteText(authorityBasis?.summary ?? preferredAgentWarning?.message),
    no_claim: status === 'claimed' ? 0 : 1,
    status: sqliteText(status),
    rejection_reason: sqliteText(rejectionReason),
    assignment_id: sqliteText(assignmentId),
    previous_agent_id: null,
    lifecycle_status_before: sqliteText(lifecycle.status),
    lifecycle_status_after: sqliteText(status === 'claimed' ? 'claimed' : lifecycle.status),
    roster_status_after: status === 'claimed' ? 'busy' : null,
    confirmation_json: JSON.stringify({
      authority_basis: authorityBasis ?? null,
      preferred_agent_warning: preferredAgentWarning ?? null,
      identity_state: {
        schema: 'narada.agent.identity_state.v1',
        claimed_identity: {
          identity: agentId,
          status: 'claimed',
          source: 'task_lifecycle_claim_request',
          asserted_at: now,
          evidence_refs: [],
          authority_granted: false,
        },
        authentication: {
          status: 'missing',
          authenticated_identity: null,
          evidence_refs: [],
        },
        authority: {
          status: status === 'claimed' ? 'authorized' : 'denied',
          operation: 'task_lifecycle.claim',
          granted: status === 'claimed',
          evidence_refs: [],
        },
      },
    }),
    warnings_json: preferredAgentWarning ? JSON.stringify([preferredAgentWarning]) : null,
    updated_at: now,
  };
  store.upsertAssignmentIntent(record);
}

export function normalizeRosterAuthorityBasis(value: any) {
  const record = asRecord(value);
  const kind = stringField(record, 'kind');
  const summary = stringField(record, 'summary');
  const allowedKinds = new Set(['operator_direct_instruction', 'directed_obligation', 'task_owner_handoff']);
  if (!kind || !allowedKinds.has(kind) || !summary) return null;
  return { kind, summary };
}

export function validateRosterIdentifier(value: any, fieldName: any) {
  if (!value) throw new Error(`${fieldName}_required`);
  if (!/^[A-Za-z0-9._-]+$/.test(value)) {
    throw new Error(`${fieldName}_invalid: expected letters, numbers, dot, underscore, or hyphen only`);
  }
}

export function admitRosterIdentity(args: any, { store, enforceSessionIdentity }: any) {
  const agentId = stringField(args, 'agent_id');
  const role = stringField(args, 'role');
  const actorAgentId = stringField(args, 'actor_agent_id');
  const capabilitiesProvided = Object.prototype.hasOwnProperty.call(args, 'capabilities');
  const capabilities = stringArrayField(args, 'capabilities') ?? [];
  const operatorIdentity = stringField(args, 'operator_identity') ?? null;
  const authorityBasis = normalizeRosterAuthorityBasis(args.authority_basis);
  const reason = stringField(args, 'reason') ?? authorityBasis?.summary ?? null;
  const dryRun = booleanField(args, 'dry_run') === true;

  validateRosterIdentifier(agentId, 'agent_id');
  validateRosterIdentifier(role, 'role');
  validateRosterIdentifier(actorAgentId, 'actor_agent_id');
  enforceSessionIdentity(actorAgentId);
  if (!authorityBasis) throw new Error('authority_basis_required: kind must be operator_direct_instruction, directed_obligation, or task_owner_handoff and summary is required');

  ensureAgentRosterEventsTable(store);
  const now = new Date().toISOString();
  const existing = store.db.prepare('SELECT * FROM agent_roster WHERE agent_id = ?').get(agentId);
  const operatorIdentityCol = store.db.prepare('PRAGMA table_info(agent_roster)').all().some((column: any) => column.name === 'operator_identity');
  const projectedCapabilitiesJson = capabilitiesProvided
    ? JSON.stringify(capabilities)
    : (existing?.capabilities_json ?? JSON.stringify(capabilities));
  const projectedRosterEntry = existing ? {
    ...existing,
    role,
    capabilities_json: projectedCapabilitiesJson,
    updated_at: now,
    ...(operatorIdentityCol ? { operator_identity: operatorIdentity ?? existing.operator_identity ?? null } : {}),
  } : {
    agent_id: agentId,
    role,
    capabilities_json: JSON.stringify(capabilities),
    status: 'idle',
    task_number: null,
    last_done: null,
    ...(operatorIdentityCol ? { operator_identity: operatorIdentity } : {}),
  };
  const capabilitiesChanged = existing
    ? JSON.stringify(existing?.capabilities_json ? JSON.parse(existing.capabilities_json) : []) !== projectedCapabilitiesJson
    : false;
  const projectionChanged = !existing || capabilitiesChanged || role !== (existing.role ?? null) || (operatorIdentityCol && operatorIdentity !== (existing.operator_identity ?? null));
  const event = {
    event_id: `roster-${randomUUID()}`,
    event_type: 'admit_agent',
    agent_id: agentId,
    role,
    capabilities_json: JSON.stringify(capabilities),
    operator_identity: operatorIdentity,
    requested_by: actorAgentId,
    requested_at: now,
    authority_basis_json: JSON.stringify(authorityBasis),
    admission_status: existing ? (projectionChanged ? 'updated' : 'already_present') : 'admitted',
    admitted_by: actorAgentId,
    admitted_at: now,
    reason,
    payload_json: JSON.stringify({
      dry_run: dryRun,
      projection_target: 'agent_roster',
      existing_agent_present: Boolean(existing),
      capabilities_changed: capabilitiesChanged,
    }),
    supersedes_event_id: null,
  };

  if (dryRun) {
    return {
      status: existing ? (projectionChanged ? 'would_update' : 'already_present') : 'would_admit',
      schema: 'narada.task.roster_admission.v0',
      dry_run: true,
      event,
      projected_roster_entry: projectedRosterEntry,
    };
  }

  const insertEvent = store.db.prepare(`
    INSERT INTO agent_roster_events (
      event_id, event_type, agent_id, role, capabilities_json, operator_identity,
      requested_by, requested_at, authority_basis_json, admission_status,
      admitted_by, admitted_at, reason, payload_json, supersedes_event_id
    ) VALUES (
      @event_id, @event_type, @agent_id, @role, @capabilities_json, @operator_identity,
      @requested_by, @requested_at, @authority_basis_json, @admission_status,
      @admitted_by, @admitted_at, @reason, @payload_json, @supersedes_event_id
    )
  `);
  insertEvent.run(event);

  if (existing) {
    if (operatorIdentityCol) {
      store.db.prepare(`
        UPDATE agent_roster
        SET role = ?, capabilities_json = ?, operator_identity = ?, updated_at = ?
        WHERE agent_id = ?
      `).run(role, projectedCapabilitiesJson, projectedRosterEntry.operator_identity, now, agentId);
    } else {
      store.db.prepare(`
        UPDATE agent_roster
        SET role = ?, capabilities_json = ?, updated_at = ?
        WHERE agent_id = ?
      `).run(role, projectedCapabilitiesJson, now, agentId);
    }
  } else {
    if (operatorIdentityCol) {
      store.db.prepare(`
        INSERT INTO agent_roster (
          agent_id, role, capabilities_json, first_seen_at, last_active_at,
          status, task_number, last_done, operator_identity, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(agentId, role, JSON.stringify(capabilities), now, now, 'idle', null, null, operatorIdentity, now);
    } else {
      store.db.prepare(`
        INSERT INTO agent_roster (
          agent_id, role, capabilities_json, first_seen_at, last_active_at,
          status, task_number, last_done, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(agentId, role, JSON.stringify(capabilities), now, now, 'idle', null, null, now);
    }
  }

  return {
    status: existing ? (projectionChanged ? 'updated' : 'already_present') : 'admitted',
    schema: 'narada.task.roster_admission.v0',
    dry_run: false,
    event_id: event.event_id,
    agent_id: agentId,
    role,
    capabilities,
    capabilities_changed: capabilitiesChanged,
    append_only_event_recorded: true,
    roster_projection_changed: projectionChanged,
    projection: existing
      ? (projectionChanged ? 'agent_roster_existing_row_updated_from_admitted_event' : 'agent_roster_existing_row_preserved')
      : 'agent_roster_inserted_from_admitted_event',
  };
}

export function ensureStaticRosterAgentInSql(taskStore: any, root: any, agentId: any) {
  if (!agentId) return;
  try {
    const existing = taskStore.db.prepare('SELECT agent_id FROM agent_roster WHERE agent_id = ?').get(agentId);
    if (existing) return;
  } catch {
    return;
  }

  const rosterPath = join(root, '.ai', 'agents', 'roster.json');
  let staticAgent = null;
  try {
    const roster = JSON.parse(readFileSync(rosterPath, 'utf8'));
    staticAgent = Array.isArray(roster.agents) ? roster.agents.find((agent: any) => agent?.agent_id === agentId) : null;
  } catch {
    return;
  }
  if (!staticAgent?.role) return;

  const now = new Date().toISOString();
  taskStore.upsertRosterEntry({
    agent_id: staticAgent.agent_id,
    role: staticAgent.role,
    capabilities_json: normalizeCapabilitiesJson(JSON.stringify(staticAgent.capabilities ?? [])),
    first_seen_at: staticAgent.first_seen_at ?? now,
    last_active_at: staticAgent.last_active_at ?? now,
    status: staticAgent.status ?? 'idle',
    task_number: staticAgent.task_number ?? staticAgent.task ?? null,
    last_done: staticAgent.last_done ?? null,
    updated_at: now,
    ...(staticAgent.operator_identity ? { operator_identity: staticAgent.operator_identity } : {}),
  });
}

export async function withAuthoredRosterJsonPreserved(root: any, fn: any, store : any= null) {
  const rosterPath = join(root, '.ai', 'agents', 'roster.json');
  let before = null;
  try {
    before = readFileSync(rosterPath, 'utf8');
    const sanitized = sanitizeRosterCapabilitiesJson(before);
    if (sanitized && sanitized !== before) writeFileSync(rosterPath, sanitized, 'utf8');
  } catch {
    before = null;
  }
  if (store) sanitizeSqlRosterCapabilities(store);
  try {
    return await fn();
  } finally {
    if (before !== null) {
      try {
        const after = readFileSync(rosterPath, 'utf8');
        if (after !== before) {
          writeFileSync(rosterPath, before, 'utf8');
        }
      } catch {
        // Roster JSON is static compatibility config; preservation is best-effort.
      }
    }
  }
}

export function sanitizeSqlRosterCapabilities(store: any) {
  try {
    const rows = store.db.prepare('SELECT agent_id, capabilities_json FROM agent_roster').all();
    const update = store.db.prepare('UPDATE agent_roster SET capabilities_json = ? WHERE agent_id = ?');
    for (const row of rows) {
      const normalized = normalizeCapabilitiesJson(row.capabilities_json);
      if (normalized !== row.capabilities_json) update.run(normalized, row.agent_id);
    }
  } catch {
    // Best-effort compatibility normalization before shared governance services run.
  }
}

export function sanitizeRosterCapabilitiesJson(text: any) {
  try {
    const parsed = JSON.parse(text);
    if (!Array.isArray(parsed?.agents)) return text;
    let changed = false;
    const agents = parsed.agents.map((agent: any) => {
      if (!agent || typeof agent !== 'object' || Array.isArray(agent)) return agent;
      const capabilities = agent.capabilities;
      if (Array.isArray(capabilities)) return agent;
      const nested = capabilities && typeof capabilities === 'object' && Array.isArray(capabilities.capabilities)
        ? capabilities.capabilities.filter((item: any) => typeof item === 'string')
        : [];
      changed = true;
      return { ...agent, capabilities: nested };
    });
    return changed ? JSON.stringify({ ...parsed, agents }, null, 2) : text;
  } catch {
    return text;
  }
}

export function normalizeCapabilitiesJson(value: any) {
  try {
    const parsed = JSON.parse(String(value ?? '[]'));
    if (Array.isArray(parsed)) return JSON.stringify(parsed.filter((item: any) => typeof item === 'string'));
    if (parsed && typeof parsed === 'object' && Array.isArray(parsed.capabilities)) return JSON.stringify(parsed.capabilities.filter((item: any) => typeof item === 'string'));
    return JSON.stringify([]);
  } catch {
    return JSON.stringify([]);
  }
}

function asRecord(value: any) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function booleanField(record: any, key: any) {
  return record[key] === true;
}

function stringField(record: any, key: any) {
  return typeof record[key] === 'string' && record[key].trim().length > 0 ? record[key].trim() : null;
}

function stringArrayField(record: any, key: any) {
  return Array.isArray(record[key]) ? record[key].filter((value: any) => typeof value === 'string' && value.trim().length > 0) : null;
}

function sqliteText(value: any) {
  if (value === undefined || value === null) return null;
  if (typeof value === 'string') return value;
  if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') return String(value);
  return JSON.stringify(value);
}

function sqliteInteger(value: any) {
  const number = Number(value);
  return Number.isInteger(number) ? number : null;
}

function ensureAgentRosterEventsTable(store: any) {
  store.db.exec(`
    CREATE TABLE IF NOT EXISTS agent_roster_events (
      event_id TEXT PRIMARY KEY,
      event_type TEXT NOT NULL,
      agent_id TEXT NOT NULL,
      role TEXT,
      capabilities_json TEXT,
      operator_identity TEXT,
      requested_by TEXT,
      requested_at TEXT,
      authority_basis_json TEXT,
      admission_status TEXT,
      admitted_by TEXT,
      admitted_at TEXT,
      reason TEXT,
      payload_json TEXT,
      supersedes_event_id TEXT
    )
  `);
}
