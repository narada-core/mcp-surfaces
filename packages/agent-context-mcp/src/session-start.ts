import { DatabaseSync } from '@narada-core/sqlite';
import { existsSync, mkdirSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { assertManifestBoundToAdmission } from '@narada-core/orientation-manifest';
import { isCodexSessionId } from './codex-session-evidence.js';
import {
  assertAdmissionMatchesAgentContext,
  compileAgentContextOrientation,
} from './orientation-manifest.js';

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
} : any= {}) {
  if (!db) throw new Error('agent_context_db_not_available');

  const filters: any[] = [];
  const params: any = {};
  const normalizedLimit: any = Math.min(Math.max(parseInt(limit ?? '100', 10) || 100, 1), 500);

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
  const rows: any = db.prepare(`
    SELECT event_id, identity_id, runtime, created_at, status, resume_command, bootstrap_artifact_uri
    FROM agent_start_events
    ${where}
    ORDER BY created_at DESC, event_id DESC
    LIMIT @limit
  `).all({ ...params, limit: normalizedLimit });

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
    },
    session_count: sessions.length,
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
  const compilation: any = compileAgentContextOrientation({
    siteRoot,
    siteId,
    admissionReceipt: admitted,
    activationReceipt,
    observedAt,
    roleBinding: rosterCheck.role_binding,
    exactCheckpoint,
    portableContinuation,
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

