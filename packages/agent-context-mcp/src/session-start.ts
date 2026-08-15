import { DatabaseSync } from '@narada-core/sqlite';
import { existsSync, mkdirSync, readdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash, randomUUID } from 'node:crypto';
import {
  assertDeliveryReceiptBoundToBrief,
  assertManifestBoundToAdmission,
  assertOrientationBriefIntegrity,
  buildOrientationBrief,
  issueCarrierSessionOrientationAcknowledgement,
  parseCarrierSessionOrientationAcknowledgement,
  parseOrientationReadCompletionEvidence,
} from '@narada-core/orientation-manifest';
import { isCodexSessionId } from './codex-session-evidence.js';
import {
  assertAdmissionMatchesAgentContext,
  compileAgentContextOrientation,
} from './orientation-manifest.js';
import {
  assertOrientationRequiredReadSourceBound,
  orientationManifestEntryIdFromArtifactRef,
  orientationRequiredReadPageEnd,
  renderExactContinuityReadMaterial,
} from './orientation-read-material.js';
export {
  ORIENTATION_REQUIRED_READ_MAX_TOTAL_PAGES,
  ORIENTATION_REQUIRED_READ_PAGE_BYTES,
  ORIENTATION_REQUIRED_READ_PAGE_JSON_BYTES,
} from './orientation-read-material.js';

// Package-bundled fallback migrations: dist/src/session-start.js -> package root -> migrations/.
const PACKAGE_MIGRATIONS_DIR: any = fileURLToPath(new URL('../../migrations/', import.meta.url));

// Matches narada's legacy sqlite facade: concurrent role launches wait instead of failing with SQLITE_BUSY.
export const DEFAULT_BUSY_TIMEOUT_MS: any = 5000;

const MIGRATIONS: any = [
  { table: 'agent_start_events', path: ['.ai', 'db', 'migrations', '001-agent-context-materializations.sql'] },
  { table: 'agent_events', path: ['.ai', 'db', 'migrations', '002-agent-events.sql'] },
  {
    table: 'codex_session_admissions',
    ddl: `
      CREATE TABLE IF NOT EXISTS codex_session_admissions (
        admission_id TEXT PRIMARY KEY,
        agent_id TEXT NOT NULL,
        runtime TEXT NOT NULL DEFAULT 'codex',
        cwd TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('creating', 'admitted', 'suspect', 'retired')),
        agent_start_event_id TEXT,
        codex_session_id TEXT,
        codex_session_file TEXT,
        evidence_json TEXT NOT NULL DEFAULT '{}',
        created_at TEXT NOT NULL,
        verified_at TEXT
      );
      CREATE INDEX IF NOT EXISTS idx_codex_session_admissions_agent
        ON codex_session_admissions(agent_id, cwd, status, created_at DESC);
      CREATE INDEX IF NOT EXISTS idx_codex_session_admissions_session
        ON codex_session_admissions(codex_session_id);
    `,
  },
];

export function validateIdentityAgainstRoster(siteRoot: any, identity: any) {
  const sqlRosterCheck: any = validateIdentityAgainstTaskLifecycleRoster(siteRoot, identity);
  if (sqlRosterCheck.valid) {
    return sqlRosterCheck;
  }

  const rosterPath: any = join(siteRoot, '.ai', 'agents', 'roster.json');
  if (!existsSync(rosterPath)) {
    return buildInferredRosterCheck(identity, {
      reason: 'roster_unavailable_but_site_session_roster_enforcement_not_enabled',
      prior_error: sqlRosterCheck.error ?? `roster_not_found: ${rosterPath}`,
    });
  }

  let roster: any;
  try {
    roster = JSON.parse(readFileSync(rosterPath, 'utf8'));
  } catch (err: any) {
    return { valid: false, error: `roster_parse_error: ${err.message}` };
  }

  const agent: any = roster.agents?.find((candidate: any) => candidate.agent_id === identity);
  if (!agent) {
    if (!siteEnforcesSessionRoster(roster)) {
      return buildInferredRosterCheck(identity, {
        reason: 'identity_not_in_roster_but_site_session_roster_enforcement_not_enabled',
        prior_error: sqlRosterCheck.error ?? null,
      });
    }
    return { valid: false, error: `identity_not_in_roster: ${identity}` };
  }

  const capabilities: any = Array.isArray(agent.capabilities) ? agent.capabilities : [];
  return {
    valid: true,
    agent,
    role: agent.role,
    role_binding: buildRoleBindingProjection({
      agentId: identity,
      role: agent.role,
      source: 'static_roster_config',
    }),
    capabilities,
    capability_policy: agent.capability_policy ?? defaultCapabilityPolicy(agent.role),
  };
}

export function projectOrientationAcknowledgement({
  siteRoot,
  entryFile,
  acknowledgement,
}: any = {}) {
  if (!siteRoot) throw new Error('siteRoot is required');
  if (typeof entryFile !== 'string' || !entryFile.trim()) {
    throw new Error('agent_context_exact_orientation_entry_file_required');
  }
  const canonicalAcknowledgement: any = parseCarrierSessionOrientationAcknowledgement(
    acknowledgement,
  );
  const exactEntryFile: any = resolve(entryFile.trim());
  const admittedRoot: any = resolve(siteRoot, '.ai', 'runtime', 'orientation-entry');
  const entryRelative: any = relative(admittedRoot, exactEntryFile);
  if (
    entryRelative === '..'
    || entryRelative.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`)
    || isAbsolute(entryRelative)
  ) {
    throw new Error(`agent_context_orientation_entry_file_outside_admitted_root:${exactEntryFile}`);
  }
  if (!existsSync(exactEntryFile)) {
    throw new Error(`agent_context_orientation_entry_file_not_found:${exactEntryFile}`);
  }
  const packet: any = JSON.parse(readFileSync(exactEntryFile, 'utf8'));
  if (packet?.schema !== 'narada.carrier_entry.orientation_packet.v1') {
    throw new Error('agent_context_orientation_entry_packet_invalid');
  }
  const brief: any = assertOrientationBriefIntegrity(packet.orientation_brief);
  const delivery: any = packet.delivery_receipt;
  if (
    delivery?.receipt_id !== canonicalAcknowledgement.delivery_receipt_ref
    || brief.manifest_ref.manifest_id !== canonicalAcknowledgement.manifest_id
    || brief.manifest_ref.manifest_digest !== canonicalAcknowledgement.manifest_digest
    || brief.brief_id !== canonicalAcknowledgement.brief_id
    || brief.brief_digest !== canonicalAcknowledgement.brief_digest
    || brief.coordinate.carrier_session_id
      !== canonicalAcknowledgement.coordinate.carrier_session_id
    || brief.coordinate.authority_epoch
      !== canonicalAcknowledgement.coordinate.authority_epoch
  ) {
    throw new Error('agent_context_orientation_acknowledgement_projection_binding_mismatch');
  }
  const relativePath: any = packet.acknowledgement_projection?.relative_path;
  if (relativePath !== 'acknowledgement.json') {
    throw new Error('agent_context_orientation_acknowledgement_projection_path_invalid');
  }
  const projectionPath: any = resolve(dirname(exactEntryFile), relativePath);
  if (dirname(projectionPath) !== dirname(exactEntryFile)) {
    throw new Error('agent_context_orientation_acknowledgement_projection_path_escape');
  }
  const projection: any = {
    schema: 'narada.carrier_entry.orientation_acknowledgement_projection.v1',
    status: 'open',
    ordinary_work_gate: 'open',
    delivery_receipt_ref: canonicalAcknowledgement.delivery_receipt_ref,
    manifest_id: canonicalAcknowledgement.manifest_id,
    manifest_digest: canonicalAcknowledgement.manifest_digest,
    brief_id: canonicalAcknowledgement.brief_id,
    brief_digest: canonicalAcknowledgement.brief_digest,
    coordinate: canonicalAcknowledgement.coordinate,
    acknowledgement_ref: canonicalAcknowledgement.acknowledgement_id,
    acknowledged_at: canonicalAcknowledgement.acknowledged_at,
    acknowledgement_semantics: canonicalAcknowledgement.acknowledgement_semantics,
    action_admission: canonicalAcknowledgement.action_admission,
    canonical_readback_ref: canonicalAcknowledgement.authority_readback_ref,
    projection_posture: 'derived_readback_not_independent_authority',
  };
  const serialized: any = JSON.stringify(projection, null, 2) + '\n';
  if (existsSync(projectionPath)) {
    const existing: any = readFileSync(projectionPath, 'utf8');
    if (existing !== serialized) {
      throw new Error('agent_context_orientation_acknowledgement_projection_conflict');
    }
    return {
      status: 'already_projected',
      projection_ref: `orientation-entry:${canonicalAcknowledgement.coordinate.carrier_session_id}:acknowledgement`,
      projection_path: projectionPath,
    };
  }
  const temporaryPath: any = `${projectionPath}.${randomUUID()}.tmp`;
  writeFileSync(temporaryPath, serialized, 'utf8');
  renameSync(temporaryPath, projectionPath);
  return {
    status: 'projected',
    projection_ref: `orientation-entry:${canonicalAcknowledgement.coordinate.carrier_session_id}:acknowledgement`,
    projection_path: projectionPath,
  };
}

function buildInferredRosterCheck(identity: any, { reason, prior_error = null } : any= {}) {
  const role: any = inferRoleFromIdentity(identity);
  return {
    valid: true,
    agent: {
      agent_id: identity,
      role,
      capabilities: [],
      roster_source: 'identity_inference_non_authoritative',
    },
    role,
    role_binding: buildRoleBindingProjection({
      agentId: identity,
      role,
      source: 'identity_inference_non_authoritative',
      bindingAuthority: 'identity_inference_non_authoritative',
    }),
    capabilities: [],
    capability_policy: defaultCapabilityPolicy(role),
    roster_source: 'identity_inference_non_authoritative',
    roster_enforcement: 'disabled',
    reason,
    prior_error,
  };
}

function siteEnforcesSessionRoster(roster: any) {
  return roster?.enforce_session_roster === true;
}

function inferRoleFromIdentity(identity: any) {
  const suffix: any = String(identity ?? '').split('.').pop();
  if (['architect', 'builder', 'builder2', 'resident'].includes(suffix)) return suffix;
  return null;
}

function validateIdentityAgainstTaskLifecycleRoster(siteRoot: any, identity: any) {
  const dbPath: any = join(siteRoot, '.ai', 'task-lifecycle.db');
  if (!existsSync(dbPath)) {
    return { valid: false, error: `task_lifecycle_roster_db_not_found: ${dbPath}` };
  }

  let db: any = null;
  try {
    db = new DatabaseSync(dbPath, { readOnly: true });
    const hasRoster: any = db.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'agent_roster'").get();
    if (!hasRoster) return { valid: false, error: 'task_lifecycle_roster_table_not_found' };
    const row: any = db.prepare('SELECT * FROM agent_roster WHERE agent_id = ?').get(identity);
    if (!row) return { valid: false, error: `identity_not_in_task_lifecycle_roster: ${identity}` };
    const capabilities: any = parseCapabilitiesJson(row.capabilities_json);
    const agent: any = {
      agent_id: row.agent_id,
      role: row.role,
      capabilities,
      first_seen_at: row.first_seen_at ?? null,
      last_active_at: row.last_active_at ?? null,
      status: row.status ?? null,
      task: row.task_number ?? null,
      last_done: row.last_done ?? null,
      updated_at: row.updated_at ?? null,
      roster_source: 'task_lifecycle_sqlite_agent_roster',
    };
    return {
      valid: true,
      agent,
      role: row.role,
      role_binding: buildRoleBindingProjection({
        agentId: identity,
        role: row.role,
        source: 'task_lifecycle_sqlite_agent_roster',
      }),
      capabilities,
      capability_policy: defaultCapabilityPolicy(row.role),
      roster_source: 'task_lifecycle_sqlite_agent_roster',
    };
  } catch (err: any) {
    return { valid: false, error: `task_lifecycle_roster_read_error: ${err.message}` };
  } finally {
    if (db) db.close();
  }
}

function parseCapabilitiesJson(value: any) {
  try {
    const parsed: any = JSON.parse(value ?? '[]');
    return Array.isArray(parsed) ? parsed.filter((entry: any) => typeof entry === 'string') : [];
  } catch {
    return [];
  }
}

export function buildRoleBindingProjection({ agentId, role, source, bindingAuthority = 'agent_roster' }: any) {
  return {
    schema: 'narada.agent.role_binding.v0',
    agent_id: agentId,
    role_name: role ?? null,
    binding_source: source ?? 'unknown',
    binding_authority: bindingAuthority,
    semantics: bindingAuthority === 'agent_roster'
      ? 'Roster role binding is used for identity read models, routing, and eligibility; it is not activation authority or a capability grant.'
      : bindingAuthority === 'identity_inference_non_authoritative'
        ? 'Role was inferred from identity shape because the Site has not opted into session roster enforcement; this is a read-model hint, not activation authority or a capability grant.'
        : 'No authoritative role binding was available. This residual projection cannot create identity, block an owner-issued admission, or grant capability.',
    capability_policy_ref: 'capability_policy',
  };
}

function rosterProjectionForOrientation(rosterCheck: any, identity: string) {
  if (rosterCheck?.valid) return rosterCheck;
  return {
    ...rosterCheck,
    role: null,
    role_binding: buildRoleBindingProjection({
      agentId: identity,
      role: null,
      source: 'unavailable',
      bindingAuthority: 'unavailable',
    }),
    capabilities: [],
    capability_policy: null,
    roster_projection_status: 'unavailable',
  };
}

export function defaultCapabilityPolicy(role: any) {
  return {
    schema: 'narada.agent.capability_policy.v0',
    direct_substrate_script_execution: 'forbidden',
    script_execution_surface: 'mcp_only',
    direct_substrate_shell_access: 'forbidden',
    mcp_shell_execution: 'allowed',
    shell_access: 'mcp_only',
    filesystem_discovery: 'mcp_only',
    lifecycle_mutations: 'mcp_only',
    exception_authority: 'operator_explicit_break_glass_only',
    rules: [
      'Do not run shell commands, scripts, rg, node, PowerShell, Python, or raw SQL directly.',
      'Use declared MCP surfaces for task lifecycle, filesystem discovery, inbox, operator surface, and approved shell-like operations.',
      'If no MCP capability exists, stop and report missing MCP capability instead of using direct script execution.',
      'Task lifecycle mutations are MCP-only.',
      'No role has standing direct terminal authority; break-glass requires explicit operator authorization.',
    ],
  };
}

export function openAgentContextDb(siteRoot: any, dbPath : any= join(siteRoot, '.ai', 'state', 'agent-context.sqlite')) {
  const dbDir: any = dirname(dbPath);
  if (!existsSync(dbDir)) {
    mkdirSync(dbDir, { recursive: true });
  }

  const db: any = new DatabaseSync(dbPath);
  db.exec(`PRAGMA busy_timeout = ${Math.trunc(DEFAULT_BUSY_TIMEOUT_MS)}`);
  applyAgentContextMigrations(db, siteRoot);
  ensureAgentStartEventCompatibility(db);
  ensureCodexAdmissionColumns(db);
  return db;
}

export function listAgentStartSessions({
  db,
  identity = null,
  dateFrom = null,
  dateTo = null,
  substrate = null,
  now = new Date(),
  limit = 100,
  offset = 0,
} : any= {}) {
  if (!db) throw new Error('agent_context_db_not_available');

  const filters: any[] = [];
  const params: any = {};
  const normalizedLimit: any = Math.min(Math.max(parseInt(limit ?? '100', 10) || 100, 1), 500);
  const normalizedOffset: any = Math.max(parseInt(offset ?? '0', 10) || 0, 0);

  if (identity) {
    filters.push('identity_id = @identity');
    params.identity = String(identity);
  }
  if (substrate) {
    filters.push('runtime = @substrate');
    params.substrate = String(substrate);
  }
  if (dateFrom) {
    params.dateFrom = normalizeIsoDateFilter(dateFrom, 'date_from');
    filters.push('created_at >= @dateFrom');
  }
  if (dateTo) {
    params.dateTo = normalizeIsoDateFilter(dateTo, 'date_to');
    filters.push('created_at <= @dateTo');
  }

  const where: any = filters.length > 0 ? `WHERE ${filters.join(' AND ')}` : '';
  const totalRow: any = db.prepare(`
    SELECT COUNT(*) AS total_count
    FROM agent_start_events
    ${where}
  `).get(params);
  const totalCount: any = Number(totalRow?.total_count ?? 0);
  const rows: any = db.prepare(`
    SELECT event_id, identity_id, runtime, created_at, status, resume_command, bootstrap_artifact_uri
    FROM agent_start_events
    ${where}
    ORDER BY created_at DESC, event_id DESC
    LIMIT @limit
    OFFSET @offset
  `).all({ ...params, limit: normalizedLimit, offset: normalizedOffset });

  const asOf: any = now instanceof Date ? now : new Date(now);
  const asOfIso: any = Number.isNaN(asOf.getTime()) ? new Date().toISOString() : asOf.toISOString();
  const sessions: any = rows.map((row: any) => sessionRowToProjection(row, asOf));
  const latestByIdentity: any = new Map();
  for (const session of sessions) {
    if (!latestByIdentity.has(session.identity)) latestByIdentity.set(session.identity, session);
  }

  return {
    status: 'ok',
    schema: 'narada.agent_context.sessions.v0',
    authority: 'agent_context_sqlite',
    generated_at: asOfIso,
    filters: {
      identity: identity ?? null,
      date_from: dateFrom ?? null,
      date_to: dateTo ?? null,
      substrate: substrate ?? null,
      limit: normalizedLimit,
      offset: normalizedOffset,
    },
    session_count: sessions.length,
    total_count: totalCount,
    has_more: normalizedOffset + sessions.length < totalCount,
    next_offset: normalizedOffset + sessions.length < totalCount ? normalizedOffset + sessions.length : null,
    truncated: normalizedOffset + sessions.length < totalCount,
    truncation_reason: normalizedOffset + sessions.length < totalCount ? 'session_page_limit' : null,
    sessions,
    latest_session_per_identity: Object.fromEntries(latestByIdentity.entries()),
    duration_estimate_note: 'agent_start_events has no end timestamp; duration is elapsed time from created_at to generated_at.',
  };
}

function normalizeIsoDateFilter(value: any, fieldName: any) {
  const date: any = new Date(String(value));
  if (Number.isNaN(date.getTime())) throw new Error(`invalid_${fieldName}: ${value}`);
  return date.toISOString();
}

function sessionRowToProjection(row: any, asOf: any) {
  const startedAt: any = new Date(row.created_at);
  const seconds: any = Number.isNaN(startedAt.getTime())
    ? null
    : Math.max(0, Math.floor((asOf.getTime() - startedAt.getTime()) / 1000));
  return {
    event_id: row.event_id,
    identity: row.identity_id,
    substrate: row.runtime,
    runtime: row.runtime,
    status: row.status,
    created_at: row.created_at,
    resume_command: row.resume_command ?? null,
    bootstrap_artifact_uri: row.bootstrap_artifact_uri ?? null,
    duration_estimate: {
      seconds,
      basis: 'elapsed_since_start_no_end_event',
      as_of: asOf.toISOString(),
    },
  };
}

export function beginCodexSessionAdmission({
  siteRoot,
  identity,
  runtime = 'codex',
  dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite'),
  cwd = siteRoot,
  dryRun = false,
  evidence = {},
} : any= {}) {
  if (runtime !== 'codex') throw new Error(`codex_session_admission_requires_codex_runtime: ${runtime}`);
  if (!siteRoot) throw new Error('siteRoot is required');
  if (!identity) throw new Error('identity is required');

  const rosterCheck: any = validateIdentityAgainstRoster(siteRoot, identity);
  if (!rosterCheck.valid) throw new Error(rosterCheck.error);

  const admissionId: any = `codexadm_${randomUUID().replace(/-/g, '')}`;
  const now: any = new Date().toISOString();
  const payload: any = {
    schema: 'narada.codex.session_admission.v0',
    admission_id: admissionId,
    identity,
    agent_id: identity,
    runtime,
    cwd,
    status: dryRun ? 'planned' : 'creating',
    agent_start_event_id: null,
    codex_session_id: null,
    codex_session_file: null,
    evidence_json: {
      ...evidence,
      authority_note: 'Narada admission UUID is authority; Codex session id/file is carrier evidence.',
      start_event_status: 'not_materialized_admission_intent_only',
      codex_mcp_registration: 'Stable global MCP registration is a prerequisite; launcher-bound identity is supplied through inherited carrier process environment.',
    },
    created_at: now,
    verified_at: null,
  };

  if (!dryRun) {
    const db: any = openAgentContextDb(siteRoot, dbPath);
    try {
      db.prepare(`
        INSERT INTO codex_session_admissions (
          admission_id,
          agent_id,
          runtime,
          cwd,
          status,
          agent_start_event_id,
          codex_session_id,
          codex_session_file,
          evidence_json,
          created_at,
          verified_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(
        payload.admission_id,
        payload.agent_id,
        payload.runtime,
        payload.cwd,
        payload.status,
        payload.agent_start_event_id,
        payload.codex_session_id,
        payload.codex_session_file,
        JSON.stringify(payload.evidence_json),
        payload.created_at,
        payload.verified_at
      );
    } finally {
      db.close();
    }
  }

  return {
    ...payload,
    role: rosterCheck.role,
    role_binding: rosterCheck.role_binding,
    capabilities: rosterCheck.capabilities,
    capability_policy: rosterCheck.capability_policy,
    db_path: dbPath,
    required_environment: {
      NARADA_AGENT_ID: identity,
      NARADA_CODEX_ADMISSION_ID: admissionId,
    },
  };
}

export function getCodexSessionAdmission({
  siteRoot,
  admissionId,
  dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite'),
} : any= {}) {
  if (!siteRoot) throw new Error('siteRoot is required');
  if (!admissionId) throw new Error('admissionId is required');

  const db: any = openAgentContextDb(siteRoot, dbPath);
  try {
    const row: any = db.prepare('SELECT * FROM codex_session_admissions WHERE admission_id = ?').get(admissionId);
    if (!row) return { status: 'not_found', admission_id: admissionId };
    return {
      status: 'ok',
      admission: {
        ...row,
        evidence_json: parseJsonObject(row.evidence_json),
      },
    };
  } finally {
    db.close();
  }
}

export function completeCodexSessionAdmission({
  siteRoot,
  siteId,
  admissionId,
  identity,
  codexSessionId,
  codexSessionFile = null,
  runtime = 'codex',
  dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite'),
  cwd = siteRoot,
  evidence = {},
  carrierSessionId = null,
  admissionReceipt = null,
  activationReceipt = null,
  generatedAt = null,
} : any= {}) {
  if (!siteRoot) throw new Error('siteRoot is required');
  if (!admissionId) throw new Error('admissionId is required');
  if (!identity) throw new Error('identity is required');
  if (!codexSessionId) throw new Error('codex_session_id is required');
  if (!isCodexSessionId(codexSessionId)) throw new Error(`codex_session_id_invalid: ${codexSessionId}`);
  if (runtime !== 'codex') throw new Error(`codex_session_completion_requires_codex_runtime: ${runtime}`);

  const rosterCheck: any = validateIdentityAgainstRoster(siteRoot, identity);
  if (!rosterCheck.valid) throw new Error(rosterCheck.error);
  if (!siteId) throw new Error('agent_context_exact_site_id_required');
  if (!admissionReceipt) throw new Error('agent_context_exact_admission_receipt_required');
  const completedAt: any = canonicalTimestamp(generatedAt ?? new Date(), 'generated_at');
  const admitted: any = assertAdmissionMatchesAgentContext(admissionReceipt, {
    siteId,
    identity,
    carrierSessionId,
    observedAt: completedAt,
  });
  const compilation: any = compileAgentContextOrientation({
    siteRoot,
    siteId,
    admissionReceipt: admitted,
    activationReceipt,
    observedAt: completedAt,
    roleBinding: rosterCheck.role_binding,
    mcpServers: deriveMcpServersFromFabric(siteRoot),
  });

  const db: any = openAgentContextDb(siteRoot, dbPath);
  try {
    const row: any = db.prepare('SELECT * FROM codex_session_admissions WHERE admission_id = ?').get(admissionId);
    if (!row) throw new Error(`codex_session_admission_not_found: ${admissionId}`);
    if (row.runtime !== 'codex') throw new Error(`codex_session_admission_wrong_runtime: ${row.runtime}`);
    if (row.agent_id !== identity) throw new Error(`codex_session_admission_identity_mismatch: expected ${row.agent_id}, got ${identity}`);
    if (row.status !== 'creating') throw new Error(`codex_session_admission_not_creating: ${row.status}`);

    let startResult: any;
    runTransaction(db, () => {
      startResult = writeSessionMaterialization(db, {
        identity,
        runtime,
        dbPath,
        cwd,
        rosterCheck,
        admissionReceipt: admitted,
        compilation,
        withinTransaction: true,
      });
      const completionStatus: any = startResult.status === 'materialized' ? 'admitted' : 'suspect';
      const mergedEvidence: any = {
        ...parseJsonObject(row.evidence_json),
        ...evidence,
        authority_claimed: false,
        posture: 'codex_runtime_evidence_adapter',
        start_event_status: startResult.status,
        agent_start_event_id: startResult.agent_start_event,
        codex_session_id: codexSessionId,
        codex_session_file: codexSessionFile,
        completed_by: 'agent_context_complete_codex_admission',
        completed_at: completedAt,
      };
      db.prepare(`
        UPDATE codex_session_admissions
        SET status = ?,
            agent_start_event_id = ?,
            codex_session_id = ?,
            codex_session_file = ?,
            evidence_json = ?,
            verified_at = ?
        WHERE admission_id = ?
      `).run(
        completionStatus,
        startResult.agent_start_event,
        codexSessionId,
        codexSessionFile,
        JSON.stringify(mergedEvidence),
        completedAt,
        admissionId
      );
    });

    const updated: any = db.prepare('SELECT * FROM codex_session_admissions WHERE admission_id = ?').get(admissionId);
    return {
      schema: 'narada.codex.runtime_evidence.completion.v1',
      status: startResult.status === 'materialized' ? 'recorded' : 'orientation_blocked',
      authority_claimed: false,
      admission_id: admissionId,
      agent_id: identity,
      agent_start_event_id: startResult.agent_start_event,
      codex_session_id: codexSessionId,
      codex_session_file: codexSessionFile,
      verified_at: completedAt,
      start_session: startResult,
      compatibility_environment_addition: {
        NARADA_CODEX_ADMISSION_ID: admissionId,
      },
      admission: {
        ...updated,
        evidence_json: parseJsonObject(updated.evidence_json),
      },
    };
  } finally {
    db.close();
  }
}

function parseJsonObject(value: any) {
  try {
    const parsed: any = JSON.parse(value ?? '{}');
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {};
  } catch {
    return {};
  }
}

export function applyAgentContextMigrations(db: any, siteRoot: any) {
  for (const migration of MIGRATIONS) {
    const hasTable: any = db.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?").get(migration.table);
    if (hasTable) continue;

    if (migration.ddl) {
      db.exec(migration.ddl);
      continue;
    }

    const migrationPath: any = resolveMigrationPath(siteRoot, migration);
    if (!migrationPath) {
      throw new Error(`agent_context_migration_not_found: ${join(siteRoot, ...migration.path)}`);
    }
    db.exec(readFileSync(migrationPath, 'utf8'));
  }
}

function resolveMigrationPath(siteRoot: any, migration: any) {
  const sitePath: any = join(siteRoot, ...migration.path);
  if (existsSync(sitePath)) return sitePath;
  const bundledPath: any = join(PACKAGE_MIGRATIONS_DIR, migration.path[migration.path.length - 1]);
  if (existsSync(bundledPath)) return bundledPath;
  return null;
}

export function ensureAgentStartEventCompatibility(db: any) {
  const hasEvents: any = db.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'agent_start_events'").get();
  if (!hasEvents) return;

  const columns: any = new Set(db.prepare('PRAGMA table_info(agent_start_events)').all().map((column: any) => column.name));
  const addColumn: any = (name: any, type: any) => {
    if (!columns.has(name)) {
      db.exec(`ALTER TABLE agent_start_events ADD COLUMN ${name} ${type}`);
      columns.add(name);
    }
  };

  addColumn('identity_id', 'TEXT');
  addColumn('runtime', 'TEXT');
  addColumn('created_at', 'TEXT');
  addColumn('status', 'TEXT');
  addColumn('resume_command', 'TEXT');
  addColumn('bootstrap_artifact_uri', 'TEXT');
  addColumn('carrier_session_id', 'TEXT');
  addColumn('admission_receipt_ref', 'TEXT');
  addColumn('authority_epoch', 'INTEGER');
  addColumn('orientation_manifest_id', 'TEXT');

  if (columns.has('identity')) {
    db.prepare("UPDATE agent_start_events SET identity_id = identity WHERE identity_id IS NULL AND identity IS NOT NULL").run();
  }
  if (columns.has('agent_id')) {
    db.prepare("UPDATE agent_start_events SET identity_id = agent_id WHERE identity_id IS NULL AND agent_id IS NOT NULL").run();
  }
  if (columns.has('substrate')) {
    db.prepare("UPDATE agent_start_events SET runtime = substrate WHERE runtime IS NULL AND substrate IS NOT NULL").run();
  }
  if (columns.has('materialized_at')) {
    db.prepare("UPDATE agent_start_events SET created_at = materialized_at WHERE created_at IS NULL AND materialized_at IS NOT NULL").run();
  }
  db.prepare("UPDATE agent_start_events SET status = 'materialized' WHERE status IS NULL").run();

  db.exec(`
    CREATE TABLE IF NOT EXISTS execution_context_materializations (
      materialization_id TEXT PRIMARY KEY,
      event_id TEXT NOT NULL,
      runtime TEXT NOT NULL,
      cwd TEXT,
      payload_json TEXT NOT NULL,
      created_at TEXT NOT NULL,
      expires_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS intelligence_context_materializations (
      materialization_id TEXT PRIMARY KEY,
      event_id TEXT NOT NULL,
      schema_id TEXT NOT NULL,
      payload_json TEXT NOT NULL,
      created_at TEXT NOT NULL,
      expires_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS proposal_records (
      proposal_id TEXT PRIMARY KEY,
      event_id TEXT NOT NULL,
      materialization_id TEXT,
      proposal_type TEXT NOT NULL,
      payload_json TEXT NOT NULL,
      verdict TEXT NOT NULL DEFAULT 'pending',
      verdict_at TEXT,
      verdict_by TEXT,
      created_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS residual_records (
      residual_id TEXT PRIMARY KEY,
      event_id TEXT,
      materialization_id TEXT,
      label TEXT NOT NULL,
      payload_json TEXT NOT NULL,
      status TEXT NOT NULL DEFAULT 'noted',
      promoted_task_id TEXT,
      created_at TEXT NOT NULL,
      status_at TEXT
    );

    CREATE TABLE IF NOT EXISTS artifact_refs (
      artifact_id TEXT PRIMARY KEY,
      uri TEXT NOT NULL,
      sha256 TEXT,
      mime_type TEXT,
      byte_size INTEGER,
      created_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS orientation_manifest_generations (
      manifest_id TEXT PRIMARY KEY,
      admission_receipt_ref TEXT NOT NULL,
      carrier_session_id TEXT NOT NULL,
      authority_epoch INTEGER NOT NULL,
      readiness TEXT NOT NULL,
      delivery TEXT NOT NULL,
      manifest_json TEXT NOT NULL,
      generated_at TEXT NOT NULL
    );

    CREATE TRIGGER IF NOT EXISTS orientation_manifest_generations_no_update
    BEFORE UPDATE ON orientation_manifest_generations
    BEGIN
      SELECT RAISE(ABORT, 'orientation_manifest_generations_append_only_no_update');
    END;

    CREATE TRIGGER IF NOT EXISTS orientation_manifest_generations_no_delete
    BEFORE DELETE ON orientation_manifest_generations
    BEGIN
      SELECT RAISE(ABORT, 'orientation_manifest_generations_append_only_no_delete');
    END;

    CREATE TABLE IF NOT EXISTS orientation_brief_generations (
      brief_id TEXT PRIMARY KEY,
      manifest_id TEXT NOT NULL,
      brief_digest TEXT NOT NULL,
      brief_json TEXT NOT NULL,
      generated_at TEXT NOT NULL
    );

    CREATE TRIGGER IF NOT EXISTS orientation_brief_generations_no_update
    BEFORE UPDATE ON orientation_brief_generations
    BEGIN
      SELECT RAISE(ABORT, 'orientation_brief_generations_append_only_no_update');
    END;

    CREATE TRIGGER IF NOT EXISTS orientation_brief_generations_no_delete
    BEFORE DELETE ON orientation_brief_generations
    BEGIN
      SELECT RAISE(ABORT, 'orientation_brief_generations_append_only_no_delete');
    END;

    CREATE TABLE IF NOT EXISTS orientation_delivery_receipts (
      receipt_id TEXT PRIMARY KEY,
      manifest_id TEXT NOT NULL,
      brief_id TEXT NOT NULL,
      carrier_session_id TEXT NOT NULL,
      authority_epoch INTEGER NOT NULL,
      receipt_json TEXT NOT NULL,
      delivered_at TEXT NOT NULL
    );

    CREATE TRIGGER IF NOT EXISTS orientation_delivery_receipts_no_update
    BEFORE UPDATE ON orientation_delivery_receipts
    BEGIN
      SELECT RAISE(ABORT, 'orientation_delivery_receipts_append_only_no_update');
    END;

    CREATE TRIGGER IF NOT EXISTS orientation_delivery_receipts_no_delete
    BEFORE DELETE ON orientation_delivery_receipts
    BEGIN
      SELECT RAISE(ABORT, 'orientation_delivery_receipts_append_only_no_delete');
    END;

    CREATE TABLE IF NOT EXISTS orientation_acknowledgements (
      acknowledgement_id TEXT PRIMARY KEY,
      delivery_receipt_ref TEXT NOT NULL,
      manifest_id TEXT NOT NULL,
      brief_id TEXT NOT NULL,
      carrier_session_id TEXT NOT NULL,
      authority_epoch INTEGER NOT NULL,
      acknowledgement_json TEXT NOT NULL,
      acknowledged_at TEXT NOT NULL
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_orientation_acknowledgements_delivery
      ON orientation_acknowledgements(delivery_receipt_ref);

    CREATE TRIGGER IF NOT EXISTS orientation_acknowledgements_no_update
    BEFORE UPDATE ON orientation_acknowledgements
    BEGIN
      SELECT RAISE(ABORT, 'orientation_acknowledgements_append_only_no_update');
    END;

    CREATE TRIGGER IF NOT EXISTS orientation_acknowledgements_no_delete
    BEFORE DELETE ON orientation_acknowledgements
    BEGIN
      SELECT RAISE(ABORT, 'orientation_acknowledgements_append_only_no_delete');
    END;

    CREATE TABLE IF NOT EXISTS orientation_required_read_pages (
      page_id TEXT PRIMARY KEY,
      delivery_receipt_ref TEXT NOT NULL,
      manifest_id TEXT NOT NULL,
      brief_id TEXT NOT NULL,
      step_id TEXT NOT NULL,
      byte_offset INTEGER NOT NULL,
      next_byte_offset INTEGER,
      page_json TEXT NOT NULL,
      delivered_at TEXT NOT NULL
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_orientation_required_read_page
      ON orientation_required_read_pages(delivery_receipt_ref, step_id, byte_offset);

    CREATE TRIGGER IF NOT EXISTS orientation_required_read_pages_no_update
    BEFORE UPDATE ON orientation_required_read_pages
    BEGIN
      SELECT RAISE(ABORT, 'orientation_required_read_pages_append_only_no_update');
    END;

    CREATE TRIGGER IF NOT EXISTS orientation_required_read_pages_no_delete
    BEFORE DELETE ON orientation_required_read_pages
    BEGIN
      SELECT RAISE(ABORT, 'orientation_required_read_pages_append_only_no_delete');
    END;

    CREATE TABLE IF NOT EXISTS orientation_required_read_completions (
      completion_id TEXT PRIMARY KEY,
      delivery_receipt_ref TEXT NOT NULL,
      manifest_id TEXT NOT NULL,
      brief_id TEXT NOT NULL,
      step_id TEXT NOT NULL,
      completion_json TEXT NOT NULL,
      completed_at TEXT NOT NULL
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_orientation_required_read_completion_step
      ON orientation_required_read_completions(delivery_receipt_ref, step_id);

    CREATE TRIGGER IF NOT EXISTS orientation_required_read_completions_no_update
    BEFORE UPDATE ON orientation_required_read_completions
    BEGIN
      SELECT RAISE(ABORT, 'orientation_required_read_completions_append_only_no_update');
    END;

    CREATE TRIGGER IF NOT EXISTS orientation_required_read_completions_no_delete
    BEFORE DELETE ON orientation_required_read_completions
    BEGIN
      SELECT RAISE(ABORT, 'orientation_required_read_completions_append_only_no_delete');
    END;
  `);
}

function ensureCodexAdmissionColumns(db: any) {
  const hasTable: any = db.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'codex_session_admissions'").get();
  if (!hasTable) return;

  const columns: any = new Set(db.prepare('PRAGMA table_info(codex_session_admissions)').all().map((column: any) => column.name));
  if (!columns.has('agent_start_event_id')) {
    db.exec('ALTER TABLE codex_session_admissions ADD COLUMN agent_start_event_id TEXT');
  }
}

function parseJsonValue(value: any, fallback: any) {
  if (value == null || value === '') return fallback;
  try {
    return JSON.parse(value);
  } catch {
    return fallback;
  }
}

function checkpointRowForExactSelection(db: any, identity: string, checkpointId: string) {
  const current = db.prepare(`
    SELECT * FROM agent_checkpoints
    WHERE agent_id = ? AND checkpoint_id = ?
    LIMIT 1
  `).get(identity, checkpointId);
  if (current) return current;
  const hasHistory: any = db.prepare(
    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'agent_checkpoint_history'",
  ).get();
  if (!hasHistory) return null;
  return db.prepare(`
    SELECT * FROM agent_checkpoint_history
    WHERE agent_id = ? AND checkpoint_id = ?
    ORDER BY archived_at DESC
    LIMIT 1
  `).get(identity, checkpointId);
}

function exactCheckpointProjection(row: any) {
  const payload = parseJsonValue(row.payload_json, {});
  return {
    status: 'ok',
    checkpoint_id: row.checkpoint_id,
    agent_id: row.agent_id,
    session_id: row.session_id ?? null,
    checkpoint_at: row.checkpoint_at,
    active_task: parseJsonValue(row.active_task_json, null),
    files_touched: parseJsonValue(row.files_touched_json, []),
    key_decisions: parseJsonValue(row.key_decisions_json, []),
    open_questions: parseJsonValue(row.open_questions_json, []),
    git_head: row.git_head ?? null,
    last_workboard_check_at: payload.last_workboard_check_at ?? null,
    next_intended_action: payload.next_intended_action ?? null,
    authority_basis: payload.authority_basis ?? null,
    continuation_blockers: payload.continuation_blockers ?? [],
    evidence_refs: payload.evidence_refs ?? [],
    worktree_state: payload.worktree_state ?? null,
    tactical_resume_notes: payload.tactical_resume_notes ?? [],
    continuation: payload.continuation ?? null,
    continuation_ref: payload.continuation_ref ?? null,
    continuation_projection: payload.continuation_projection ?? null,
  };
}

export function readExactCheckpointForOrientation({
  siteRoot,
  dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite'),
  identity,
  checkpointId,
}: any = {}) {
  if (!siteRoot) throw new Error('siteRoot is required');
  if (typeof identity !== 'string' || identity.trim() === '') {
    throw new Error('agent_context_exact_checkpoint_identity_required');
  }
  if (typeof checkpointId !== 'string' || checkpointId.trim() === '') {
    throw new Error('agent_context_exact_checkpoint_id_required');
  }
  const db: any = openAgentContextDb(siteRoot, dbPath);
  try {
    const row: any = checkpointRowForExactSelection(db, identity.trim(), checkpointId.trim());
    if (!row) {
      return {
        status: 'checkpoint_not_found',
        checkpoint_id: checkpointId.trim(),
        agent_id: identity.trim(),
        message: 'The exact current or archived checkpoint was not found; no latest fallback was attempted.',
      };
    }
    return exactCheckpointProjection(row);
  } finally {
    db.close();
  }
}

export function readExactWorkForOrientation({
  siteRoot,
  taskNumber,
  taskDbPath = join(siteRoot, '.ai', 'task-lifecycle.db'),
}: any = {}) {
  if (!siteRoot) throw new Error('siteRoot is required');
  const exactTaskNumber = Number(taskNumber);
  if (!Number.isSafeInteger(exactTaskNumber) || exactTaskNumber < 1) {
    throw new Error('agent_context_exact_work_task_number_required');
  }
  if (!existsSync(taskDbPath)) {
    return {
      status: 'task_store_not_found',
      task_number: exactTaskNumber,
      message: 'The Site task lifecycle store is unavailable; no work fallback was attempted.',
    };
  }
  const db: any = new DatabaseSync(taskDbPath, { readOnly: true });
  try {
    const hasLifecycle: any = db.prepare(
      "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'task_lifecycle'",
    ).get();
    if (!hasLifecycle) {
      return {
        status: 'task_store_incompatible',
        task_number: exactTaskNumber,
        message: 'The Site task lifecycle table is unavailable; no work fallback was attempted.',
      };
    }
    const lifecycle: any = db.prepare(`
      SELECT task_id, task_number, status, governed_by, relative_priority,
        priority_reason, continuation_packet_json, updated_at
      FROM task_lifecycle
      WHERE task_number = ?
      LIMIT 1
    `).get(exactTaskNumber);
    if (!lifecycle) {
      return {
        status: 'task_not_found',
        task_number: exactTaskNumber,
        message: 'The exact task was not found; no next-work or latest fallback was attempted.',
      };
    }
    const hasSpecs: any = db.prepare(
      "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'task_specs'",
    ).get();
    const spec: any = hasSpecs
      ? db.prepare(`
          SELECT task_id, task_number, title, chapter_markdown, goal_markdown,
            context_markdown, required_work_markdown, non_goals_markdown,
            acceptance_criteria_json, dependencies_json, tags_json, updated_at
          FROM task_specs
          WHERE task_number = ?
          LIMIT 1
        `).get(exactTaskNumber)
      : null;
    return {
      status: 'ok',
      task_id: lifecycle.task_id,
      task_number: Number(lifecycle.task_number),
      lifecycle: {
        status: lifecycle.status,
        governed_by: lifecycle.governed_by ?? null,
        relative_priority: lifecycle.relative_priority ?? 0,
        priority_reason: lifecycle.priority_reason ?? null,
        continuation_packet: parseJsonValue(lifecycle.continuation_packet_json, null),
        updated_at: lifecycle.updated_at,
      },
      specification: spec
        ? {
            title: spec.title,
            chapter_markdown: spec.chapter_markdown ?? null,
            goal_markdown: spec.goal_markdown ?? null,
            context_markdown: spec.context_markdown ?? null,
            required_work_markdown: spec.required_work_markdown ?? null,
            non_goals_markdown: spec.non_goals_markdown ?? null,
            acceptance_criteria: parseJsonValue(spec.acceptance_criteria_json, []),
            dependencies: parseJsonValue(spec.dependencies_json, []),
            tags: parseJsonValue(spec.tags_json, []),
            updated_at: spec.updated_at,
          }
        : null,
      selection_semantics: 'orientation_only_not_action_authority',
    };
  } finally {
    db.close();
  }
}

export function materializeAgentSessionStart({
  siteRoot,
  siteId,
  identity,
  runtime = 'kimi',
  dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite'),
  cwd = siteRoot,
  dryRun = false,
  carrierSessionId = null,
  admissionReceipt = null,
  activationReceipt = null,
  generatedAt = null,
  exactCheckpoint = null,
  portableContinuation = null,
  exactCheckpointId = null,
  exactWork = null,
  exactWorkTaskNumber = null,
} : any= {}) {
  if (!siteRoot) {
    throw new Error('siteRoot is required');
  }
  if (!identity) {
    throw new Error('identity is required');
  }

  const rosterCheck: any = rosterProjectionForOrientation(
    validateIdentityAgainstRoster(siteRoot, identity),
    identity,
  );

  if (dryRun) {
    return buildDryRunResult({ siteRoot, identity, runtime, dbPath, cwd, rosterCheck });
  }
  if (exactCheckpoint !== null && exactCheckpointId !== null) {
    throw new Error('agent_context_exact_checkpoint_source_ambiguous');
  }
  if (exactWork !== null && exactWorkTaskNumber !== null) {
    throw new Error('agent_context_exact_work_source_ambiguous');
  }

  if (!siteId) {
    throw new Error('agent_context_exact_site_id_required');
  }
  if (!admissionReceipt) {
    throw new Error('agent_context_exact_admission_receipt_required');
  }
  const observedAt: any = canonicalTimestamp(generatedAt ?? new Date(), 'generated_at');
  const admitted: any = assertAdmissionMatchesAgentContext(admissionReceipt, {
    siteId,
    identity,
    carrierSessionId,
    observedAt,
  });
  const selectedCheckpoint: any = exactCheckpointId === null
    ? exactCheckpoint
    : readExactCheckpointForOrientation({
        siteRoot,
        dbPath,
        identity,
        checkpointId: exactCheckpointId,
      });
  const selectedWork: any = exactWorkTaskNumber === null
    ? exactWork
    : readExactWorkForOrientation({
        siteRoot,
        taskNumber: exactWorkTaskNumber,
      });
  const compilation: any = compileAgentContextOrientation({
    siteRoot,
    siteId,
    admissionReceipt: admitted,
    activationReceipt,
    observedAt,
    roleBinding: rosterCheck.role_binding,
    exactCheckpoint: selectedCheckpoint,
    portableContinuation: portableContinuation
      ?? selectedCheckpoint?.continuation_projection
      ?? null,
    exactWork: selectedWork,
    mcpServers: deriveMcpServersFromFabric(siteRoot),
  });

  const db: any = openAgentContextDb(siteRoot, dbPath);
  try {
    return writeSessionMaterialization(db, {
      siteRoot,
      identity,
      runtime,
      dbPath,
      cwd,
      rosterCheck,
      admissionReceipt: admitted,
      compilation,
    });
  } finally {
    db.close();
  }
}

export function writeSessionMaterialization(db: any, {
  identity,
  runtime,
  dbPath,
  cwd,
  rosterCheck,
  admissionReceipt,
  compilation,
  withinTransaction = false,
}: any) {
  assertManifestBoundToAdmission(compilation?.manifest, admissionReceipt);
  const manifest: any = compilation.manifest;
  const now: any = manifest.generated_at;
  const eventId: any = 'evt-' + now.replace(/[:.]/g, '-').replace('T', '_').slice(0, 19)
    + '_' + randomUUID().slice(0, 8);
  const eventStatus: any = manifest.delivery === 'deliverable'
    ? 'materialized'
    : 'orientation_blocked';
  const manifestJson: any = JSON.stringify(manifest);
  const brief: any = manifest.delivery === 'deliverable'
    ? buildOrientationBrief({
        manifest,
        manifestArtifactRef: 'narada-agent-context://orientation-manifest/'
          + encodeURIComponent(manifest.manifest_id),
      })
    : null;
  const briefJson: any = brief ? JSON.stringify(brief) : null;

  const persist: any = () => {
    const existing: any = db.prepare(
      'SELECT manifest_json FROM orientation_manifest_generations WHERE manifest_id = ?',
    ).get(manifest.manifest_id);
    if (existing && existing.manifest_json !== manifestJson) {
      throw new Error('agent_context_orientation_manifest_generation_conflict');
    }
    if (!existing) {
      db.prepare(`
        INSERT INTO orientation_manifest_generations (
          manifest_id, admission_receipt_ref, carrier_session_id, authority_epoch,
          readiness, delivery, manifest_json, generated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
      `).run(
        manifest.manifest_id,
        admissionReceipt.receipt_id,
        admissionReceipt.coordinate.carrier_session_id,
        admissionReceipt.coordinate.authority_epoch,
        manifest.readiness,
        manifest.delivery,
        manifestJson,
        now,
      );
    }
    if (brief) {
      const existingBrief: any = db.prepare(
        'SELECT brief_json FROM orientation_brief_generations WHERE brief_id = ?',
      ).get(brief.brief_id);
      if (existingBrief && existingBrief.brief_json !== briefJson) {
        throw new Error('agent_context_orientation_brief_generation_conflict');
      }
      if (!existingBrief) {
        db.prepare(`
          INSERT INTO orientation_brief_generations (
            brief_id, manifest_id, brief_digest, brief_json, generated_at
          ) VALUES (?, ?, ?, ?, ?)
        `).run(
          brief.brief_id,
          manifest.manifest_id,
          brief.brief_digest,
          briefJson,
          brief.generated_at,
        );
      }
    }

    db.prepare(`
      INSERT INTO agent_start_events (
        event_id, identity_id, runtime, created_at, status, resume_command,
        bootstrap_artifact_uri, carrier_session_id, admission_receipt_ref,
        authority_epoch, orientation_manifest_id
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      eventId,
      identity,
      runtime,
      now,
      eventStatus,
      null,
      null,
      admissionReceipt.coordinate.carrier_session_id,
      admissionReceipt.receipt_id,
      admissionReceipt.coordinate.authority_epoch,
      manifest.manifest_id,
    );
  };

  if (withinTransaction) persist();
  else runTransaction(db, persist);

  return {
    schema: 'narada.agent_context.session_start.v1',
    status: manifest.delivery === 'deliverable' ? 'materialized' : 'blocked',
    compatibility_facade: {
      authority: 'none',
      event_posture: 'downstream_trace',
      source_authority_mutation: false,
      local_persistence: true,
      persisted_records: [
        'orientation_manifest_generations',
        ...(brief ? ['orientation_brief_generations'] : []),
        'agent_start_events',
      ],
    },
    agent_start_event: eventId,
    identity,
    role: rosterCheck.role,
    role_binding: rosterCheck.role_binding,
    runtime_request: runtime,
    cwd_request: cwd,
    db_path: dbPath,
    carrier_session: admissionReceipt.coordinate,
    admission_receipt: admissionReceipt,
    admission_receipt_ref: admissionReceipt.receipt_id,
    orientation_manifest: manifest,
    orientation_brief: brief,
    orientation_manifest_ref: brief?.manifest_ref ?? {
      source_authority_ref: 'agent-context:orientation-manifest-store',
      artifact_ref: 'agent-context:orientation_manifest_generations:' + manifest.manifest_id,
      revision: manifest.manifest_digest,
      manifest_id: manifest.manifest_id,
      manifest_digest: manifest.manifest_digest,
    },
    entry_procedure: manifest.entries.filter(
      (entry: any) => entry.compartment === 'entry_procedure',
    ),
  };
}

export function readOrientationManifestGeneration({
  siteRoot,
  dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite'),
  manifestId,
  admissionReceipt,
}: any = {}) {
  if (!siteRoot) throw new Error('siteRoot is required');
  if (typeof manifestId !== 'string' || manifestId.trim() === '') {
    throw new Error('agent_context_exact_orientation_manifest_id_required');
  }
  if (!admissionReceipt) throw new Error('agent_context_exact_admission_receipt_required');
  if (!existsSync(dbPath)) {
    throw new Error('agent_context_orientation_manifest_store_not_found:' + dbPath);
  }

  const db: any = new DatabaseSync(dbPath, { readOnly: true });
  try {
    const row: any = db.prepare(`
      SELECT manifest_id, admission_receipt_ref, carrier_session_id,
             authority_epoch, readiness, delivery, manifest_json, generated_at
      FROM orientation_manifest_generations
      WHERE manifest_id = ?
      LIMIT 1
    `).get(manifestId.trim());
    if (!row) {
      throw new Error('agent_context_orientation_manifest_generation_not_found:' + manifestId.trim());
    }
    let stored: any;
    try {
      stored = JSON.parse(row.manifest_json);
    } catch {
      throw new Error('agent_context_orientation_manifest_generation_json_invalid:' + manifestId.trim());
    }
    const manifest: any = assertManifestBoundToAdmission(stored, admissionReceipt);
    if (
      row.manifest_id !== manifest.manifest_id
      || row.admission_receipt_ref !== manifest.admission_receipt_ref
      || row.carrier_session_id !== manifest.coordinate.carrier_session_id
      || Number(row.authority_epoch) !== manifest.coordinate.authority_epoch
      || row.readiness !== manifest.readiness
      || row.delivery !== manifest.delivery
      || row.generated_at !== manifest.generated_at
    ) {
      throw new Error('agent_context_orientation_manifest_generation_index_mismatch:' + manifestId.trim());
    }
    return {
      schema: 'narada.agent_context.orientation_manifest_readback.v1',
      status: 'ok',
      source_mutation: false,
      storage_ref: 'agent-context:orientation_manifest_generations:' + manifest.manifest_id,
      admission_receipt_ref: manifest.admission_receipt_ref,
      carrier_session: manifest.coordinate,
      manifest,
    };
  } finally {
    db.close();
  }
}

function readStoredOrientationBrief(db: any, manifestId: string) {
  const row: any = db.prepare(`
    SELECT brief_id, manifest_id, brief_digest, brief_json, generated_at
    FROM orientation_brief_generations
    WHERE manifest_id = ?
    LIMIT 1
  `).get(manifestId);
  if (!row) {
    throw new Error('agent_context_orientation_brief_generation_not_found:' + manifestId);
  }
  const brief: any = assertOrientationBriefIntegrity(JSON.parse(row.brief_json));
  if (
    row.brief_id !== brief.brief_id
    || row.manifest_id !== brief.manifest_ref.manifest_id
    || row.brief_digest !== brief.brief_digest
    || row.generated_at !== brief.generated_at
  ) {
    throw new Error('agent_context_orientation_brief_generation_index_mismatch:' + manifestId);
  }
  return brief;
}

function orientationRequiredReadSiteFile(siteRoot: string, artifactRef: any) {
  if (typeof artifactRef !== 'string' || !artifactRef.startsWith('site-file:')) {
    throw new Error(
      `agent_context_orientation_required_read_source_unsupported:${String(artifactRef)}`,
    );
  }
  const relativeRef: any = artifactRef.slice('site-file:'.length).trim();
  if (!relativeRef || isAbsolute(relativeRef)) {
    throw new Error(
      `agent_context_orientation_required_read_source_invalid:${artifactRef}`,
    );
  }
  const root: any = resolve(siteRoot);
  const sourcePath: any = resolve(root, relativeRef);
  const relativePath: any = relative(root, sourcePath);
  if (
    !relativePath
    || relativePath === '..'
    || relativePath.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`)
    || isAbsolute(relativePath)
  ) {
    throw new Error(
      `agent_context_orientation_required_read_source_escape:${artifactRef}`,
    );
  }
  return sourcePath;
}

function orientationRequiredReadSourceContent({
  siteRoot,
  dbPath,
  artifactRef,
  manifestId,
  admissionReceipt,
}: any) {
  if (typeof artifactRef !== 'string' || !artifactRef) {
    throw new Error(
      `agent_context_orientation_required_read_source_unsupported:${String(artifactRef)}`,
    );
  }
  if (artifactRef.startsWith('site-file:')) {
    const sourcePath: any = orientationRequiredReadSiteFile(siteRoot, artifactRef);
    if (!existsSync(sourcePath)) {
      throw new Error(
        `agent_context_orientation_required_read_source_missing:${sourcePath}`,
      );
    }
    return readFileSync(sourcePath, 'utf8');
  }

  const entryId: any = orientationManifestEntryIdFromArtifactRef(artifactRef);
  if (!entryId) {
    throw new Error(
      `agent_context_orientation_required_read_source_unsupported:${artifactRef}`,
    );
  }
  const readback: any = readOrientationManifestGeneration({
    siteRoot,
    dbPath,
    manifestId,
    admissionReceipt,
  });
  const entry: any = readback.manifest.entries.find(
    (candidate: any) => candidate.entry_id === entryId,
  );
  if (!entry) {
    throw new Error(
      `agent_context_orientation_required_read_manifest_entry_missing:${entryId}`,
    );
  }
  if (
    entry.entry_kind !== 'exact_continuity'
    || entry.projection_status !== 'available'
  ) {
    throw new Error(
      `agent_context_orientation_required_read_manifest_entry_ineligible:${entryId}`,
    );
  }
  return renderExactContinuityReadMaterial({
    checkpoint: entry.payload?.checkpoint,
    portableContinuation: entry.payload?.portable_continuation,
  });
}

function parseStoredOrientationRequiredReadPage(value: any) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('agent_context_orientation_required_read_page_invalid');
  }
  if (value.schema !== 'narada.agent_context.orientation_required_read_page.v1') {
    throw new Error('agent_context_orientation_required_read_page_schema_invalid');
  }
  for (const field of [
    'page_id', 'delivery_receipt_ref', 'manifest_id', 'brief_id', 'step_id',
    'content_sha256', 'page_sha256', 'page_ref',
  ]) {
    if (typeof value[field] !== 'string' || !value[field]) {
      throw new Error(`agent_context_orientation_required_read_page_${field}_invalid`);
    }
  }
  if (!Number.isInteger(value.byte_offset) || value.byte_offset < 0) {
    throw new Error('agent_context_orientation_required_read_page_byte_offset_invalid');
  }
  if (!Number.isInteger(value.returned_bytes) || value.returned_bytes < 1) {
    throw new Error('agent_context_orientation_required_read_page_returned_bytes_invalid');
  }
  if (
    !Number.isInteger(value.next_byte_offset)
    || value.next_byte_offset !== value.byte_offset + value.returned_bytes
  ) {
    throw new Error('agent_context_orientation_required_read_page_next_offset_invalid');
  }
  if (typeof value.eof !== 'boolean' || typeof value.content !== 'string') {
    throw new Error('agent_context_orientation_required_read_page_content_invalid');
  }
  const pageBytes: any = Buffer.from(value.content, 'utf8');
  if (pageBytes.length !== value.returned_bytes) {
    throw new Error('agent_context_orientation_required_read_page_byte_count_mismatch');
  }
  const pageSha256: any = createHash('sha256').update(pageBytes).digest('hex');
  if (pageSha256 !== value.page_sha256) {
    throw new Error('agent_context_orientation_required_read_page_digest_mismatch');
  }
  return value;
}

function publicOrientationRequiredReadPage(page: any) {
  const { content: _content, ...metadata } = page;
  return metadata;
}

function orientationRequiredReadPages(db: any, brief: any, delivery: any) {
  const rows: any[] = db.prepare(`
    SELECT step_id, byte_offset, next_byte_offset, page_json
    FROM orientation_required_read_pages
    WHERE delivery_receipt_ref = ?
    ORDER BY step_id ASC, byte_offset ASC
  `).all(delivery.receipt_id);
  const allowedSteps: any = new Set(brief.required_reads.map((step: any) => step.step_id));
  const byStep: any = new Map();
  for (const row of rows) {
    if (!allowedSteps.has(row.step_id)) {
      throw new Error('agent_context_orientation_required_read_page_unknown_step');
    }
    const page: any = parseStoredOrientationRequiredReadPage(JSON.parse(row.page_json));
    if (
      page.delivery_receipt_ref !== delivery.receipt_id
      || page.manifest_id !== brief.manifest_ref.manifest_id
      || page.brief_id !== brief.brief_id
      || page.step_id !== row.step_id
      || page.byte_offset !== row.byte_offset
      || page.next_byte_offset !== row.next_byte_offset
    ) {
      throw new Error('agent_context_orientation_required_read_page_binding_mismatch');
    }
    const pages: any[] = byStep.get(page.step_id) ?? [];
    const expectedOffset: any = pages.length === 0
      ? 0
      : pages[pages.length - 1].next_byte_offset;
    if (page.byte_offset !== expectedOffset) {
      throw new Error(
        `agent_context_orientation_required_read_page_chain_gap:${page.step_id}:`
        + `expected=${expectedOffset}:actual=${page.byte_offset}`,
      );
    }
    if (pages.some((prior: any) => prior.eof)) {
      throw new Error(
        `agent_context_orientation_required_read_page_after_eof:${page.step_id}`,
      );
    }
    pages.push(page);
    byStep.set(page.step_id, pages);
  }
  return byStep;
}

function orientationRequiredReadProgress(db: any, brief: any, delivery: any) {
  const rows: any[] = db.prepare(`
    SELECT completion_json
    FROM orientation_required_read_completions
    WHERE delivery_receipt_ref = ?
    ORDER BY completed_at ASC, step_id ASC
  `).all(delivery.receipt_id);
  const byStep: any = new Map();
  for (const row of rows) {
    const completion: any = parseOrientationReadCompletionEvidence(
      JSON.parse(row.completion_json),
    );
    byStep.set(completion.step_id, completion);
  }
  const orderedCompletions: any[] = [];
  const pendingStepIds: string[] = [];
  for (const step of brief.required_reads) {
    const completion: any = byStep.get(step.step_id);
    if (completion) orderedCompletions.push(completion);
    else pendingStepIds.push(step.step_id);
  }
  if (byStep.size !== orderedCompletions.length) {
    throw new Error('agent_context_orientation_required_read_completion_unknown_step');
  }
  const completionRefs: string[] = orderedCompletions.flatMap((completion: any) => (
    completion.evidence_refs.filter((ref: any) => String(ref).startsWith(
      'agent-context:orientation_required_read_completions:',
    ))
  ));
  const pagesByStep: any = orientationRequiredReadPages(db, brief, delivery);
  const activeStepId: any = pendingStepIds[0] ?? null;
  const activePages: any[] = activeStepId ? (pagesByStep.get(activeStepId) ?? []) : [];
  const nextByteOffset: any = activeStepId
    ? (activePages.length > 0
        ? activePages[activePages.length - 1].next_byte_offset
        : 0)
    : null;
  const nextCall: any = pendingStepIds.length > 0
    ? {
        tool: 'agent_orientation_read',
        arguments: { step_id: pendingStepIds[0], offset: nextByteOffset },
      }
    : {
        tool: 'agent_orientation_acknowledge',
        arguments: {},
      };
  return {
    total: brief.required_reads.length,
    completed: orderedCompletions.length,
    pending: pendingStepIds.length,
    completed_step_ids: orderedCompletions.map((completion: any) => completion.step_id),
    pending_step_ids: pendingStepIds,
    completion_refs: completionRefs,
    active_step_id: activeStepId,
    next_byte_offset: nextByteOffset,
    next_call: nextCall,
    completions: orderedCompletions,
  };
}

export function readOrientationEntryPacket({
  siteRoot,
  dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite'),
  manifestId,
  admissionReceipt,
  deliveryReceipt,
}: any = {}) {
  if (!deliveryReceipt) {
    throw new Error('agent_context_exact_orientation_delivery_receipt_required');
  }
  const readback: any = readOrientationManifestGeneration({
    siteRoot,
    dbPath,
    manifestId,
    admissionReceipt,
  });
  const db: any = new DatabaseSync(dbPath, { readOnly: true });
  try {
    const brief: any = readStoredOrientationBrief(db, readback.manifest.manifest_id);
    const delivery: any = assertDeliveryReceiptBoundToBrief({
      deliveryReceipt,
      admissionReceipt,
      brief,
    });
    const storedDelivery: any = db.prepare(`
      SELECT receipt_json
      FROM orientation_delivery_receipts
      WHERE receipt_id = ?
      LIMIT 1
    `).get(delivery.receipt_id);
    if (!storedDelivery || storedDelivery.receipt_json !== JSON.stringify(delivery)) {
      throw new Error(
        'agent_context_orientation_delivery_receipt_not_persisted:' + delivery.receipt_id,
      );
    }
    const acknowledgementRow: any = db.prepare(`
      SELECT acknowledgement_json
      FROM orientation_acknowledgements
      WHERE delivery_receipt_ref = ?
      LIMIT 1
    `).get(delivery.receipt_id);
    const acknowledgement: any = acknowledgementRow
      ? parseCarrierSessionOrientationAcknowledgement(
          JSON.parse(acknowledgementRow.acknowledgement_json),
        )
      : null;
    const progress: any = orientationRequiredReadProgress(db, brief, delivery);
    return {
      schema: 'narada.agent_context.orientation_entry_packet.v2',
      status: acknowledgement ? 'acknowledged' : 'orientation_required',
      source_mutation: false,
      ordinary_work_gate: acknowledgement ? 'open' : 'acknowledgement_required',
      orientation_brief: brief,
      manifest_ref: brief.manifest_ref,
      delivery_receipt_ref: delivery.receipt_id,
      acknowledgement_ref: acknowledgement
        ? `agent-context:orientation_acknowledgements:${acknowledgement.acknowledgement_id}`
        : null,
      required_read_progress: {
        total: progress.total,
        completed: progress.completed,
        pending: progress.pending,
        completed_step_ids: progress.completed_step_ids,
        pending_step_ids: progress.pending_step_ids,
        completion_refs: progress.completion_refs,
        active_step_id: progress.active_step_id,
        next_byte_offset: progress.next_byte_offset,
      },
      next_call: acknowledgement ? null : progress.next_call,
    };
  } finally {
    db.close();
  }
}

export function recordOrientationRequiredRead({
  siteRoot,
  dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite'),
  admissionReceipt,
  deliveryReceipt,
  brief,
  stepId,
  byteOffset = 0,
  completedAt = new Date().toISOString(),
  resultValidator = null,
}: any = {}) {
  if (!siteRoot) throw new Error('siteRoot is required');
  if (typeof stepId !== 'string' || !stepId.trim()) {
    throw new Error('agent_context_orientation_required_read_step_id_required');
  }
  if (!Number.isInteger(byteOffset) || byteOffset < 0) {
    throw new Error('agent_context_orientation_required_read_offset_invalid');
  }
  if (resultValidator !== null && typeof resultValidator !== 'function') {
    throw new Error('agent_context_orientation_required_read_result_validator_invalid');
  }
  const deliverResult: any = (value: any) => {
    resultValidator?.(value);
    return value;
  };
  const canonicalBrief: any = assertOrientationBriefIntegrity(brief);
  const delivery: any = assertDeliveryReceiptBoundToBrief({
    deliveryReceipt,
    admissionReceipt,
    brief: canonicalBrief,
  });
  const step: any = canonicalBrief.required_reads.find(
    (candidate: any) => candidate.step_id === stepId.trim(),
  );
  if (!step) {
    throw new Error(
      `agent_context_orientation_required_read_step_unknown:${stepId.trim()}`,
    );
  }
  if (
    step.tool.name !== 'agent_orientation_read'
    || step.tool.arguments?.step_id !== step.step_id
  ) {
    throw new Error(
      `agent_context_orientation_required_read_step_not_owned:${step.step_id}`,
    );
  }
  const content: any = orientationRequiredReadSourceContent({
    siteRoot,
    dbPath,
    artifactRef: step.source.artifact_ref,
    manifestId: canonicalBrief.manifest_ref.manifest_id,
    admissionReceipt,
  });
  const contentSha256: any = createHash('sha256').update(content).digest('hex');
  if (contentSha256 !== step.source.revision) {
    throw new Error(
      `agent_context_orientation_required_read_source_stale:${step.step_id}:`
      + `expected=${step.source.revision}:actual=${contentSha256}`,
    );
  }
  assertOrientationRequiredReadSourceBound(content, step.source.artifact_ref);
  const contentBytes: any = Buffer.from(content, 'utf8');
  const totalBytes: any = contentBytes.length;
  const lines: any[] = content.split(/\r?\n/);
  const normalizedWindow: any = lines.join('\n');
  const completionResultEvidence: any = {
    content_sha256: contentSha256,
    content_window_sha256: createHash('sha256').update(normalizedWindow).digest('hex'),
    offset: 1,
    returned_lines: lines.length,
  };
  const completionId: any = [
    'orientation-read',
    delivery.receipt_id,
    step.step_id,
  ].join(':');
  const completionRef: any =
    `agent-context:orientation_required_read_completions:${completionId}`;
  const proposedCompletion: any = parseOrientationReadCompletionEvidence({
    step_id: step.step_id,
    tool_name: step.tool.name,
    arguments: step.tool.arguments,
    result_evidence: completionResultEvidence,
    completed_at: completedAt,
    evidence_refs: [completionRef, `sha256:${contentSha256}`],
  });
  const db: any = openAgentContextDb(siteRoot, dbPath);
  try {
    return runTransaction(db, () => {
      const storedDelivery: any = db.prepare(`
        SELECT receipt_json
        FROM orientation_delivery_receipts
        WHERE receipt_id = ?
        LIMIT 1
      `).get(delivery.receipt_id);
      if (!storedDelivery || storedDelivery.receipt_json !== JSON.stringify(delivery)) {
        throw new Error(
          'agent_context_orientation_delivery_receipt_not_persisted:' + delivery.receipt_id,
        );
      }
      const existingCompletionRow: any = db.prepare(`
        SELECT completion_json
        FROM orientation_required_read_completions
        WHERE delivery_receipt_ref = ? AND step_id = ?
        LIMIT 1
      `).get(delivery.receipt_id, step.step_id);
      if (existingCompletionRow) {
        const existingCompletion: any = parseOrientationReadCompletionEvidence(
          JSON.parse(existingCompletionRow.completion_json),
        );
        if (
          existingCompletion.tool_name !== proposedCompletion.tool_name
          || JSON.stringify(existingCompletion.arguments) !== JSON.stringify(proposedCompletion.arguments)
          || JSON.stringify(existingCompletion.result_evidence) !== JSON.stringify(proposedCompletion.result_evidence)
        ) {
          throw new Error(
            `agent_context_orientation_required_read_completion_conflict:${step.step_id}`,
          );
        }
        const existingPageRow: any = db.prepare(`
          SELECT page_json
          FROM orientation_required_read_pages
          WHERE delivery_receipt_ref = ? AND step_id = ? AND byte_offset = ?
          LIMIT 1
        `).get(delivery.receipt_id, step.step_id, byteOffset);
        const existingPage: any = existingPageRow
          ? parseStoredOrientationRequiredReadPage(JSON.parse(existingPageRow.page_json))
          : null;
        if (existingPage) {
          const expectedPageContent: any = contentBytes
            .subarray(existingPage.byte_offset, existingPage.next_byte_offset)
            .toString('utf8');
          if (
            existingPage.content_sha256 !== contentSha256
            || existingPage.content !== expectedPageContent
          ) {
            throw new Error(
              `agent_context_orientation_required_read_page_source_conflict:${step.step_id}`,
            );
          }
        }
        const progress: any = orientationRequiredReadProgress(db, canonicalBrief, delivery);
        return deliverResult({
          schema: 'narada.agent_context.orientation_required_read.v1',
          status: 'already_completed',
          source_mutation: false,
          local_persistence: true,
          ordinary_work_gate: 'acknowledgement_required',
          step_id: step.step_id,
          source: step.source,
          content: existingPage?.content ?? null,
          page: existingPage ? publicOrientationRequiredReadPage(existingPage) : null,
          result_evidence: existingCompletion.result_evidence,
          completion_ref: completionRef,
          required_read_progress: {
            total: progress.total,
            completed: progress.completed,
            pending: progress.pending,
            completed_step_ids: progress.completed_step_ids,
            pending_step_ids: progress.pending_step_ids,
            completion_refs: progress.completion_refs,
            active_step_id: progress.active_step_id,
            next_byte_offset: progress.next_byte_offset,
          },
          next_call: progress.next_call,
        });
      }

      const progressBefore: any = orientationRequiredReadProgress(
        db,
        canonicalBrief,
        delivery,
      );
      if (progressBefore.active_step_id !== step.step_id) {
        throw new Error(
          `agent_context_orientation_required_read_step_out_of_order:${step.step_id}:`
          + `expected=${progressBefore.active_step_id ?? 'none'}`,
        );
      }

      const existingPageRow: any = db.prepare(`
        SELECT page_json
        FROM orientation_required_read_pages
        WHERE delivery_receipt_ref = ? AND step_id = ? AND byte_offset = ?
        LIMIT 1
      `).get(delivery.receipt_id, step.step_id, byteOffset);
      if (existingPageRow) {
        const existingPage: any = parseStoredOrientationRequiredReadPage(
          JSON.parse(existingPageRow.page_json),
        );
        const expectedPageContent: any = contentBytes
          .subarray(existingPage.byte_offset, existingPage.next_byte_offset)
          .toString('utf8');
        if (
          existingPage.content_sha256 !== contentSha256
          || existingPage.content !== expectedPageContent
        ) {
          throw new Error(
            `agent_context_orientation_required_read_page_source_conflict:${step.step_id}`,
          );
        }
        const progress: any = orientationRequiredReadProgress(db, canonicalBrief, delivery);
        return deliverResult({
          schema: 'narada.agent_context.orientation_required_read.v1',
          status: 'page_already_emitted',
          source_mutation: false,
          local_persistence: true,
          ordinary_work_gate: 'acknowledgement_required',
          step_id: step.step_id,
          source: step.source,
          content: existingPage.content,
          page: publicOrientationRequiredReadPage(existingPage),
          result_evidence: null,
          completion_ref: null,
          required_read_progress: {
            total: progress.total,
            completed: progress.completed,
            pending: progress.pending,
            completed_step_ids: progress.completed_step_ids,
            pending_step_ids: progress.pending_step_ids,
            completion_refs: progress.completion_refs,
            active_step_id: progress.active_step_id,
            next_byte_offset: progress.next_byte_offset,
          },
          next_call: progress.next_call,
        });
      }

      if (byteOffset !== progressBefore.next_byte_offset) {
        throw new Error(
          `agent_context_orientation_required_read_offset_out_of_order:${step.step_id}:`
          + `expected=${progressBefore.next_byte_offset}:actual=${byteOffset}`,
        );
      }
      if (byteOffset > totalBytes) {
        throw new Error(
          `agent_context_orientation_required_read_offset_out_of_range:${step.step_id}:`
          + `total=${totalBytes}:actual=${byteOffset}`,
        );
      }

      if (totalBytes === 0 && byteOffset === 0) {
        const pagesByStep: any = orientationRequiredReadPages(db, canonicalBrief, delivery);
        const pages: any[] = pagesByStep.get(step.step_id) ?? [];
        if (pages.length > 0) {
          throw new Error(
            `agent_context_orientation_required_read_empty_source_page_conflict:${step.step_id}`,
          );
        }
        const completionJson: any = JSON.stringify(proposedCompletion);
        db.prepare(`
          INSERT INTO orientation_required_read_completions (
            completion_id, delivery_receipt_ref, manifest_id, brief_id,
            step_id, completion_json, completed_at
          ) VALUES (?, ?, ?, ?, ?, ?, ?)
        `).run(
          completionId,
          delivery.receipt_id,
          canonicalBrief.manifest_ref.manifest_id,
          canonicalBrief.brief_id,
          step.step_id,
          completionJson,
          proposedCompletion.completed_at,
        );
        const progress: any = orientationRequiredReadProgress(db, canonicalBrief, delivery);
        return deliverResult({
          schema: 'narada.agent_context.orientation_required_read.v1',
          status: 'completed',
          source_mutation: false,
          local_persistence: true,
          ordinary_work_gate: 'acknowledgement_required',
          step_id: step.step_id,
          source: step.source,
          content: '',
          page: null,
          result_evidence: completionResultEvidence,
          completion_ref: completionRef,
          required_read_progress: {
            total: progress.total,
            completed: progress.completed,
            pending: progress.pending,
            completed_step_ids: progress.completed_step_ids,
            pending_step_ids: progress.pending_step_ids,
            completion_refs: progress.completion_refs,
            active_step_id: progress.active_step_id,
            next_byte_offset: progress.next_byte_offset,
          },
          next_call: progress.next_call,
        });
      }

      const pageEnd: any = orientationRequiredReadPageEnd(
        contentBytes,
        byteOffset,
      );
      if (pageEnd <= byteOffset) {
        throw new Error(
          `agent_context_orientation_required_read_page_boundary_invalid:${step.step_id}`,
        );
      }
      const pageBytes: any = contentBytes.subarray(byteOffset, pageEnd);
      const pageContent: any = pageBytes.toString('utf8');
      const pageId: any = [
        'orientation-read-page',
        delivery.receipt_id,
        step.step_id,
        byteOffset,
      ].join(':');
      const pageRef: any = `agent-context:orientation_required_read_pages:${pageId}`;
      const page: any = parseStoredOrientationRequiredReadPage({
        schema: 'narada.agent_context.orientation_required_read_page.v1',
        page_id: pageId,
        delivery_receipt_ref: delivery.receipt_id,
        manifest_id: canonicalBrief.manifest_ref.manifest_id,
        brief_id: canonicalBrief.brief_id,
        step_id: step.step_id,
        byte_offset: byteOffset,
        returned_bytes: pageBytes.length,
        next_byte_offset: pageEnd,
        eof: pageEnd === totalBytes,
        content_sha256: contentSha256,
        page_sha256: createHash('sha256').update(pageBytes).digest('hex'),
        page_ref: pageRef,
        content: pageContent,
      });
      db.prepare(`
        INSERT INTO orientation_required_read_pages (
          page_id, delivery_receipt_ref, manifest_id, brief_id, step_id,
          byte_offset, next_byte_offset, page_json, delivered_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(
        page.page_id,
        delivery.receipt_id,
        canonicalBrief.manifest_ref.manifest_id,
        canonicalBrief.brief_id,
        step.step_id,
        page.byte_offset,
        page.next_byte_offset,
        JSON.stringify(page),
        completedAt,
      );
      if (page.eof) {
        db.prepare(`
          INSERT INTO orientation_required_read_completions (
            completion_id, delivery_receipt_ref, manifest_id, brief_id,
            step_id, completion_json, completed_at
          ) VALUES (?, ?, ?, ?, ?, ?, ?)
        `).run(
          completionId,
          delivery.receipt_id,
          canonicalBrief.manifest_ref.manifest_id,
          canonicalBrief.brief_id,
          step.step_id,
          JSON.stringify(proposedCompletion),
          proposedCompletion.completed_at,
        );
      }
      const progress: any = orientationRequiredReadProgress(db, canonicalBrief, delivery);
      return deliverResult({
        schema: 'narada.agent_context.orientation_required_read.v1',
        status: page.eof ? 'completed' : 'page_emitted',
        source_mutation: false,
        local_persistence: true,
        ordinary_work_gate: 'acknowledgement_required',
        step_id: step.step_id,
        source: step.source,
        content: page.content,
        page: publicOrientationRequiredReadPage(page),
        result_evidence: page.eof ? completionResultEvidence : null,
        completion_ref: page.eof ? completionRef : null,
        required_read_progress: {
          total: progress.total,
          completed: progress.completed,
          pending: progress.pending,
          completed_step_ids: progress.completed_step_ids,
          pending_step_ids: progress.pending_step_ids,
          completion_refs: progress.completion_refs,
          active_step_id: progress.active_step_id,
          next_byte_offset: progress.next_byte_offset,
        },
        next_call: progress.next_call,
      });
    });
  } finally {
    db.close();
  }
}

export function recordOrientationDeliveryReceipt({
  siteRoot,
  dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite'),
  admissionReceipt,
  brief,
  deliveryReceipt,
}: any = {}) {
  if (!siteRoot) throw new Error('siteRoot is required');
  const delivery: any = assertDeliveryReceiptBoundToBrief({
    deliveryReceipt,
    admissionReceipt,
    brief,
  });
  const canonicalBrief: any = assertOrientationBriefIntegrity(brief);
  const receiptJson: any = JSON.stringify(delivery);
  const db: any = openAgentContextDb(siteRoot, dbPath);
  try {
    return runTransaction(db, () => {
      const storedBrief: any = readStoredOrientationBrief(
        db,
        canonicalBrief.manifest_ref.manifest_id,
      );
      if (storedBrief.brief_digest !== canonicalBrief.brief_digest) {
        throw new Error('agent_context_orientation_delivery_brief_mismatch');
      }
      const existing: any = db.prepare(
        'SELECT receipt_json FROM orientation_delivery_receipts WHERE receipt_id = ?',
      ).get(delivery.receipt_id);
      if (existing && existing.receipt_json !== receiptJson) {
        throw new Error('agent_context_orientation_delivery_receipt_conflict');
      }
      if (!existing) {
        db.prepare(`
          INSERT INTO orientation_delivery_receipts (
            receipt_id, manifest_id, brief_id, carrier_session_id,
            authority_epoch, receipt_json, delivered_at
          ) VALUES (?, ?, ?, ?, ?, ?, ?)
        `).run(
          delivery.receipt_id,
          delivery.manifest_id,
          delivery.brief_id,
          delivery.coordinate.carrier_session_id,
          delivery.coordinate.authority_epoch,
          receiptJson,
          delivery.delivered_at,
        );
      }
      return {
        schema: 'narada.agent_context.orientation_delivery_record.v1',
        status: existing ? 'already_recorded' : 'recorded',
        source_mutation: false,
        local_persistence: true,
        delivery_receipt: delivery,
      };
    });
  } finally {
    db.close();
  }
}

export function recordOrientationAcknowledgement({
  siteRoot,
  dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite'),
  admissionReceipt,
  deliveryReceipt,
  brief,
  requiredReadCompletions,
  acknowledgedAt = new Date().toISOString(),
}: any = {}) {
  if (!siteRoot) throw new Error('siteRoot is required');
  const canonicalBrief: any = assertOrientationBriefIntegrity(brief);
  const delivery: any = assertDeliveryReceiptBoundToBrief({
    deliveryReceipt,
    admissionReceipt,
    brief: canonicalBrief,
  });
  const db: any = openAgentContextDb(siteRoot, dbPath);
  try {
    return runTransaction(db, () => {
      const storedDelivery: any = db.prepare(`
        SELECT receipt_json
        FROM orientation_delivery_receipts
        WHERE receipt_id = ?
        LIMIT 1
      `).get(delivery.receipt_id);
      if (!storedDelivery || storedDelivery.receipt_json !== JSON.stringify(delivery)) {
        throw new Error(
          'agent_context_orientation_delivery_receipt_not_persisted:' + delivery.receipt_id,
        );
      }
      const progress: any = orientationRequiredReadProgress(
        db,
        canonicalBrief,
        delivery,
      );
      if (progress.pending_step_ids.length > 0) {
        throw new Error(
          'agent_context_orientation_required_reads_incomplete:'
          + progress.pending_step_ids.join(',')
          + `:next=${progress.next_call.tool}(${JSON.stringify(progress.next_call.arguments)})`,
        );
      }
      if (requiredReadCompletions !== undefined && requiredReadCompletions !== null) {
        const supplied: any = requiredReadCompletions.map(
          (completion: any) => parseOrientationReadCompletionEvidence(completion),
        );
        if (JSON.stringify(supplied) !== JSON.stringify(progress.completions)) {
          throw new Error(
            'agent_context_orientation_required_read_completions_not_server_recorded',
          );
        }
      }
      const existing: any = db.prepare(`
        SELECT acknowledgement_json
        FROM orientation_acknowledgements
        WHERE delivery_receipt_ref = ?
        LIMIT 1
      `).get(delivery.receipt_id);
      const effectiveAcknowledgedAt: any = existing
        ? JSON.parse(existing.acknowledgement_json).acknowledged_at
        : acknowledgedAt;
      const acknowledgement: any = issueCarrierSessionOrientationAcknowledgement({
        admissionReceipt,
        deliveryReceipt: delivery,
        brief: canonicalBrief,
        requiredReadCompletions: progress.completions,
        acknowledgedAt: effectiveAcknowledgedAt,
        authorityReadbackRef:
          'agent-context:orientation_acknowledgements:' + delivery.receipt_id,
      });
      const acknowledgementJson: any = JSON.stringify(acknowledgement);
      if (existing && existing.acknowledgement_json !== acknowledgementJson) {
        throw new Error('agent_context_orientation_acknowledgement_conflict');
      }
      if (!existing) {
        db.prepare(`
          INSERT INTO orientation_acknowledgements (
            acknowledgement_id, delivery_receipt_ref, manifest_id, brief_id,
            carrier_session_id, authority_epoch, acknowledgement_json, acknowledged_at
          ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        `).run(
          acknowledgement.acknowledgement_id,
          acknowledgement.delivery_receipt_ref,
          acknowledgement.manifest_id,
          acknowledgement.brief_id,
          acknowledgement.coordinate.carrier_session_id,
          acknowledgement.coordinate.authority_epoch,
          acknowledgementJson,
          acknowledgement.acknowledged_at,
        );
      }
      return {
        schema: 'narada.agent_context.orientation_acknowledgement_record.v1',
        status: existing ? 'already_acknowledged' : 'acknowledged',
        source_mutation: false,
        local_persistence: true,
        ordinary_work_gate: 'open',
        acknowledgement,
      };
    });
  } finally {
    db.close();
  }
}

function runTransaction(db: any, fn: any) {
  db.exec('BEGIN');
  try {
    const result: any = fn();
    db.exec('COMMIT');
    return result;
  } catch (error: any) {
    try {
      db.exec('ROLLBACK');
    } catch {
      // Preserve the original transaction failure.
    }
    throw error;
  }
}

function buildDryRunResult({ siteRoot, identity, runtime, dbPath, cwd, rosterCheck }: any) {
  return {
    schema: 'narada.agent_context.session_start.v1',
    status: 'dry_run',
    authority_claimed: false,
    identity,
    role: rosterCheck.role,
    role_binding: rosterCheck.role_binding,
    runtime_request: runtime,
    root_dir: siteRoot,
    cwd_request: cwd,
    db_path: dbPath,
    would_validate: {
      roster_or_identity_projection: true,
      exact_admission_receipt: true,
      orientation_manifest: true,
    },
    would_write: [
      'orientation_manifest_generations',
      'agent_start_events_downstream_trace',
    ],
    orientation_manifest: null,
    required_for_materialization: [
      'site_id',
      'carrier_session_admission_receipt',
    ],
  };
}

function canonicalTimestamp(value: any, field: string) {
  const date: any = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) throw new Error('agent_context_invalid_' + field);
  return date.toISOString();
}

function deriveMcpServersFromFabric(siteRoot: any) {
  const mcpFabricDir: any = join(siteRoot, '.ai', 'mcp');
  if (!existsSync(mcpFabricDir)) {
    return [];
  }

  const servers: any[] = [];
  for (const entry of readdirSync(mcpFabricDir).sort()) {
    if (!entry.endsWith('.json')) continue;

    const configPath: any = join(mcpFabricDir, entry);
    let config: any;
    try {
      config = JSON.parse(readFileSync(configPath, 'utf8'));
    } catch {
      continue;
    }

    for (const [name, server] of Object.entries(config.mcpServers ?? {})) {
      servers.push({
        name,
        transport: typeof server === 'object' && server !== null && typeof (server as Record<string, unknown>).transport === 'string'
          ? (server as Record<string, unknown>).transport
          : 'stdio',
      });
    }
  }

  return servers;
}
