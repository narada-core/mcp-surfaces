
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from '@narada-core/sqlite';
import { CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA } from '@narada-core/orientation-manifest';
import {
  materializeAgentSessionStart,
  validateIdentityAgainstRoster,
} from '../src/session-start.js';

const INFERRED_SOURCE = 'identity_inference_non_authoritative';
const INFERRED_SEMANTICS = 'Role was inferred from identity shape because the Site has not opted into session roster enforcement; this is a read-model hint, not activation authority or a capability grant.';
const ROSTER_SEMANTICS = 'Roster role binding is used for identity read models, routing, and eligibility; it is not activation authority or a capability grant.';
const UNAVAILABLE_SEMANTICS = 'No authoritative role binding was available. This residual projection cannot create identity, block an owner-issued admission, or grant capability.';
const GENERATED_AT = '2026-08-08T12:00:00.000Z';

function admissionReceipt(siteId: string, identity: string, carrierKind: string) {
  return {
    schema: CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA,
    receipt_id: 'receipt:' + siteId + ':' + identity,
    decision: 'admitted',
    state: 'starting',
    coordinate: {
      authority_scope: 'test',
      site_ref: 'site:' + siteId,
      carrier_session_id: 'carrier:' + identity,
      authority_epoch: 1,
    },
    agent_identity: {
      source_authority_ref: 'agent-identity:' + siteId,
      artifact_ref: 'agent:' + identity,
      revision: 'fixture-1',
      local_agent_id: identity,
      canonical_agent_id: identity,
    },
    carrier_kind: carrierKind,
    admission_policy: {
      source_authority_ref: 'site-policy:' + siteId,
      artifact_ref: 'carrier-admission:test',
      revision: '1',
    },
    issued_at: GENERATED_AT,
    valid_until: null,
    authority_readback_ref: 'carrier-session-authority:' + identity,
    evidence_refs: ['test:admission'],
    reason_codes: [],
  };
}

function makeSite(label: any) {
  const siteRoot = mkdtempSync(join(tmpdir(), `agent-context-roster-${label}-`));
  writeFileSync(join(siteRoot, 'AGENTS.md'), '# Fixture Site\n', 'utf8');
  return siteRoot;
}

function writeRoster(siteRoot: any, roster: any) {
  mkdirSync(join(siteRoot, '.ai', 'agents'), { recursive: true });
  writeFileSync(join(siteRoot, '.ai', 'agents', 'roster.json'), JSON.stringify(roster, null, 2), 'utf8');
}

function seedAgentContextDb(siteRoot: any) {
  const dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite');
  mkdirSync(join(siteRoot, '.ai', 'state'), { recursive: true });
  const db = new DatabaseSync(dbPath);
  db.exec(`
    CREATE TABLE agent_start_events (
      event_id TEXT PRIMARY KEY,
      identity_id TEXT NOT NULL,
      runtime TEXT NOT NULL,
      created_at TEXT NOT NULL,
      status TEXT NOT NULL,
      resume_command TEXT,
      bootstrap_artifact_uri TEXT
    );
    CREATE TABLE agent_events (
      event_id TEXT PRIMARY KEY,
      agent_id TEXT NOT NULL,
      session_id TEXT NOT NULL,
      event_type TEXT NOT NULL,
      task_number INTEGER,
      payload_json TEXT,
      emitted_at TEXT NOT NULL
    );
  `);
  db.close();
  return dbPath;
}

// (a) roster.json missing -> inferred fallback succeeds and session start materializes.
{
  const siteRoot = makeSite('missing');
  const dbPath = seedAgentContextDb(siteRoot);

  const check = validateIdentityAgainstRoster(siteRoot, 'fixture.resident');
  assert.equal(check.valid, true);
  assert.equal(check.roster_source, INFERRED_SOURCE);
  assert.equal(check.roster_enforcement, 'disabled');
  assert.equal(check.reason, 'roster_unavailable_but_site_session_roster_enforcement_not_enabled');
  assert.equal(check.prior_error, `task_lifecycle_roster_db_not_found: ${join(siteRoot, '.ai', 'task-lifecycle.db')}`);
  assert.equal(check.role, 'resident');
  assert.equal(check.agent.agent_id, 'fixture.resident');
  assert.equal(check.agent.roster_source, INFERRED_SOURCE);
  assert.deepEqual(check.capabilities, []);
  assert.equal(check.capability_policy.schema, 'narada.agent.capability_policy.v0');
  assert.equal(check.role_binding.binding_source, INFERRED_SOURCE);
  assert.equal(check.role_binding.binding_authority, INFERRED_SOURCE);
  assert.equal(check.role_binding.semantics, INFERRED_SEMANTICS);

  assert.throws(
    () => materializeAgentSessionStart({
      siteRoot,
      siteId: 'fixture',
      identity: 'fixture.resident',
      runtime: 'kimi',
    }),
    /agent_context_exact_admission_receipt_required/,
  );
  const started: any = materializeAgentSessionStart({
    siteRoot,
    siteId: 'fixture',
    identity: 'fixture.resident',
    runtime: 'kimi',
    admissionReceipt: admissionReceipt('fixture', 'fixture.resident', 'kimi'),
    generatedAt: GENERATED_AT,
  });
  assert.equal(started.status, 'materialized');
  assert.equal(started.role, 'resident');
  assert.equal(started.role_binding.binding_authority, INFERRED_SOURCE);
  assert.equal(started.orientation_manifest.delivery, 'deliverable');
  assert.equal(started.orientation_manifest.readiness, 'degraded');
  assert.equal('resume_command' in started, false);
  assert.equal('capability_policy' in started, false);
  assert.ok(started.orientation_manifest.residuals.some(
    (item: any) => item.code === 'role_binding_rejected',
  ));

  const db = new DatabaseSync(dbPath, { readOnly: true });
  const eventRow = db.prepare('SELECT event_id, identity_id, status FROM agent_start_events WHERE event_id = ?')
    .get(started.agent_start_event);
  db.close();
  assert.ok(eventRow);
  assert.equal(eventRow.identity_id, 'fixture.resident');
  assert.equal(eventRow.status, 'materialized');
}

// (b) roster.json present, identity absent, no enforcement flag -> inferred fallback succeeds.
{
  const siteRoot = makeSite('not-in-roster');
  writeRoster(siteRoot, {
    agents: [{ agent_id: 'fixture.architect', role: 'architect', capabilities: [] }],
  });

  const check = validateIdentityAgainstRoster(siteRoot, 'fixture.builder');
  assert.equal(check.valid, true);
  assert.equal(check.roster_source, INFERRED_SOURCE);
  assert.equal(check.roster_enforcement, 'disabled');
  assert.equal(check.reason, 'identity_not_in_roster_but_site_session_roster_enforcement_not_enabled');
  assert.equal(check.prior_error, `task_lifecycle_roster_db_not_found: ${join(siteRoot, '.ai', 'task-lifecycle.db')}`);
  assert.equal(check.role, 'builder');
  assert.equal(check.role_binding.binding_authority, INFERRED_SOURCE);
  assert.equal(check.role_binding.semantics, INFERRED_SEMANTICS);

  const dryRun = materializeAgentSessionStart({ siteRoot, identity: 'fixture.builder', runtime: 'kimi', dryRun: true });
  assert.equal(dryRun.status, 'dry_run');
  assert.equal(dryRun.role, 'builder');
  assert.equal(dryRun.role_binding.binding_authority, INFERRED_SOURCE);
}

// (c) A local roster refusal remains a residual; it cannot overrule an exact owner receipt.
{
  const siteRoot = makeSite('enforced');
  writeRoster(siteRoot, {
    enforce_session_roster: true,
    agents: [{ agent_id: 'fixture.architect', role: 'architect', capabilities: [] }],
  });

  const check = validateIdentityAgainstRoster(siteRoot, 'fixture.builder');
  assert.equal(check.valid, false);
  assert.equal(check.error, 'identity_not_in_roster: fixture.builder');

  const dryRun: any = materializeAgentSessionStart({
    siteRoot,
    identity: 'fixture.builder',
    runtime: 'kimi',
    dryRun: true,
  });
  assert.equal(dryRun.status, 'dry_run');
  assert.equal(dryRun.role, null);
  assert.equal(dryRun.role_binding.binding_authority, 'unavailable');
  assert.equal(dryRun.role_binding.semantics, UNAVAILABLE_SEMANTICS);

  const started: any = materializeAgentSessionStart({
    siteRoot,
    siteId: 'fixture',
    identity: 'fixture.builder',
    runtime: 'kimi',
    admissionReceipt: admissionReceipt('fixture', 'fixture.builder', 'kimi'),
    generatedAt: GENERATED_AT,
  });
  assert.equal(started.status, 'materialized');
  assert.equal(started.role, null);
  assert.equal(started.role_binding.binding_authority, 'unavailable');
  assert.equal(started.orientation_manifest.readiness, 'degraded');
  assert.ok(started.orientation_manifest.residuals.some(
    (item: any) => item.code === 'role_binding_rejected',
  ));
}

// (d) identity present in roster.json -> static roster path unchanged.
{
  const siteRoot = makeSite('static');
  writeRoster(siteRoot, {
    agents: [{ agent_id: 'fixture.architect', role: 'architect', capabilities: ['review', 'route'] }],
  });

  const check = validateIdentityAgainstRoster(siteRoot, 'fixture.architect');
  assert.equal(check.valid, true);
  assert.equal(check.roster_source, undefined);
  assert.equal(check.roster_enforcement, undefined);
  assert.equal(check.reason, undefined);
  assert.equal(check.role, 'architect');
  assert.deepEqual(check.agent, { agent_id: 'fixture.architect', role: 'architect', capabilities: ['review', 'route'] });
  assert.deepEqual(check.capabilities, ['review', 'route']);
  assert.equal(check.capability_policy.schema, 'narada.agent.capability_policy.v0');
  assert.equal(check.role_binding.binding_source, 'static_roster_config');
  assert.equal(check.role_binding.binding_authority, 'agent_roster');
  assert.equal(check.role_binding.semantics, ROSTER_SEMANTICS);

  const dryRun = materializeAgentSessionStart({ siteRoot, identity: 'fixture.architect', runtime: 'codex', dryRun: true });
  assert.equal(dryRun.status, 'dry_run');
  assert.equal(dryRun.role_binding.binding_authority, 'agent_roster');
}

// (e) sqlite task-lifecycle roster path unchanged and still takes precedence over roster.json.
{
  const siteRoot = makeSite('sqlite');
  mkdirSync(join(siteRoot, '.ai'), { recursive: true });
  const lifecycleDbPath = join(siteRoot, '.ai', 'task-lifecycle.db');
  const lifecycleDb = new DatabaseSync(lifecycleDbPath);
  lifecycleDb.exec(`
    CREATE TABLE agent_roster (
      agent_id TEXT PRIMARY KEY,
      role TEXT,
      capabilities_json TEXT,
      first_seen_at TEXT,
      last_active_at TEXT,
      status TEXT,
      task_number INTEGER,
      last_done TEXT,
      updated_at TEXT
    );
  `);
  lifecycleDb.prepare(`
    INSERT INTO agent_roster (
      agent_id, role, capabilities_json, first_seen_at, last_active_at, status, task_number, last_done, updated_at
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
  `).run('fixture.resident', 'resident', '["checkpoint","hydrate"]', '2026-07-01T00:00:00.000Z', null, 'active', 42, null, null);
  lifecycleDb.close();
  writeRoster(siteRoot, {
    agents: [{ agent_id: 'fixture.architect', role: 'architect', capabilities: [] }],
  });

  const sqlCheck = validateIdentityAgainstRoster(siteRoot, 'fixture.resident');
  assert.equal(sqlCheck.valid, true);
  assert.equal(sqlCheck.roster_source, 'task_lifecycle_sqlite_agent_roster');
  assert.equal(sqlCheck.role, 'resident');
  assert.deepEqual(sqlCheck.capabilities, ['checkpoint', 'hydrate']);
  assert.equal(sqlCheck.agent.roster_source, 'task_lifecycle_sqlite_agent_roster');
  assert.equal(sqlCheck.agent.first_seen_at, '2026-07-01T00:00:00.000Z');
  assert.equal(sqlCheck.agent.status, 'active');
  assert.equal(sqlCheck.agent.task, 42);
  assert.equal(sqlCheck.role_binding.binding_source, 'task_lifecycle_sqlite_agent_roster');
  assert.equal(sqlCheck.role_binding.binding_authority, 'agent_roster');
  assert.equal(sqlCheck.role_binding.semantics, ROSTER_SEMANTICS);

  // Identity absent from the sqlite roster still falls through to roster.json.
  const staticCheck = validateIdentityAgainstRoster(siteRoot, 'fixture.architect');
  assert.equal(staticCheck.valid, true);
  assert.equal(staticCheck.role_binding.binding_source, 'static_roster_config');

  // Identity in neither store falls through to the inferred fallback.
  const inferredCheck = validateIdentityAgainstRoster(siteRoot, 'fixture.builder');
  assert.equal(inferredCheck.valid, true);
  assert.equal(inferredCheck.roster_source, INFERRED_SOURCE);
  assert.equal(inferredCheck.reason, 'identity_not_in_roster_but_site_session_roster_enforcement_not_enabled');
  assert.equal(inferredCheck.prior_error, 'identity_not_in_task_lifecycle_roster: fixture.builder');
}

console.log('session-start roster tests passed');
