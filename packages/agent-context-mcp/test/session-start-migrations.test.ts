
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from '@narada-core/sqlite';
import { CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA } from '@narada-core/orientation-manifest';
import {
  DEFAULT_BUSY_TIMEOUT_MS,
  materializeAgentSessionStart,
  openAgentContextDb,
  readOrientationManifestGeneration,
} from '../src/session-start.js';

const GENERATED_AT = '2026-08-08T12:00:00.000Z';

function admissionReceipt(identity: string) {
  return {
    schema: CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA,
    receipt_id: 'receipt:fixture:' + identity,
    decision: 'admitted',
    state: 'starting',
    coordinate: {
      authority_scope: 'test',
      site_ref: 'site:fixture',
      carrier_session_id: 'carrier:' + identity,
      authority_epoch: 1,
    },
    agent_identity: {
      source_authority_ref: 'agent-identity:fixture',
      artifact_ref: 'agent:' + identity,
      revision: 'fixture-1',
      local_agent_id: identity,
      canonical_agent_id: identity,
    },
    carrier_kind: 'kimi',
    admission_policy: {
      source_authority_ref: 'site-policy:fixture',
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
  const siteRoot = mkdtempSync(join(tmpdir(), `agent-context-migrations-${label}-`));
  writeFileSync(join(siteRoot, 'AGENTS.md'), '# Fixture Site\n', 'utf8');
  return siteRoot;
}

function tableNames(dbPath: any) {
  const db = new DatabaseSync(dbPath, { readOnly: true });
  const names = new Set(
    db.prepare("SELECT name FROM sqlite_master WHERE type = 'table'").all().map((row: any) => row.name),
  );
  db.close();
  return names;
}

function columnNames(dbPath: any, table: any) {
  const db = new DatabaseSync(dbPath, { readOnly: true });
  const names = new Set(db.prepare(`PRAGMA table_info(${table})`).all().map((row: any) => row.name));
  db.close();
  return names;
}

// Fresh site without .ai/db/migrations: the package-bundled migrations provision the schema.
{
  const siteRoot = makeSite('bundled');
  const started: any = materializeAgentSessionStart({
    siteRoot,
    siteId: 'fixture',
    identity: 'fixture.resident',
    runtime: 'kimi',
    admissionReceipt: admissionReceipt('fixture.resident'),
    generatedAt: GENERATED_AT,
  });
  assert.equal(started.status, 'materialized');
  assert.equal(started.compatibility_facade.source_authority_mutation, false);
  assert.equal(started.compatibility_facade.local_persistence, true);
  assert.deepEqual(started.compatibility_facade.persisted_records, [
    'orientation_manifest_generations',
    'orientation_brief_generations',
    'identity_state_records',
    'agent_start_events',
  ]);
  assert.equal(started.identity_state.claimed_identity.identity, 'fixture.resident');
  assert.equal(started.identity_state.authentication.status, 'authenticated');
  assert.equal(started.identity_state.authority.granted, false);

  const dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite');
  const tables = tableNames(dbPath);
  for (const table of [
    'agent_start_events',
    'execution_context_materializations',
    'intelligence_context_materializations',
    'proposal_records',
    'residual_records',
    'artifact_refs',
    'agent_events',
    'codex_session_admissions',
    'orientation_manifest_generations',
    'identity_state_records',
  ]) {
    assert.equal(tables.has(table), true, `missing table: ${table}`);
  }

  // agent_events carries the canonical agent_id/event_type shape the module queries.
  const eventColumns = columnNames(dbPath, 'agent_events');
  for (const column of ['event_id', 'agent_id', 'session_id', 'event_type', 'task_number', 'payload_json', 'emitted_at']) {
    assert.equal(eventColumns.has(column), true, `missing agent_events column: ${column}`);
  }

  const db = new DatabaseSync(dbPath, { readOnly: true });
  const eventRow = db.prepare('SELECT event_id, identity_id, status, claimed_identity_json, authentication_json, authority_json FROM agent_start_events WHERE event_id = ?')
    .get(started.agent_start_event);
  const proposalRow = db.prepare('SELECT proposal_id FROM proposal_records WHERE event_id = ?')
    .get(started.agent_start_event);
  const manifestRow = db.prepare(
    'SELECT manifest_id, admission_receipt_ref FROM orientation_manifest_generations WHERE manifest_id = ?',
  ).get(started.orientation_manifest.manifest_id);
  db.close();
  assert.ok(eventRow);
  assert.equal(proposalRow, undefined);
  assert.ok(manifestRow);
  assert.equal(eventRow.identity_id, 'fixture.resident');
  assert.equal(eventRow.status, 'materialized');
  assert.equal(JSON.parse(String(eventRow.claimed_identity_json)).identity, 'fixture.resident');
  assert.equal(JSON.parse(String(eventRow.authentication_json)).status, 'authenticated');
  assert.equal(JSON.parse(String(eventRow.authority_json)).granted, false);
  assert.equal(manifestRow.admission_receipt_ref, started.admission_receipt_ref);
  assert.equal('proposal_id' in started, false);

  const readback: any = readOrientationManifestGeneration({
    siteRoot,
    dbPath,
    manifestId: started.orientation_manifest.manifest_id,
    admissionReceipt: admissionReceipt('fixture.resident'),
  });
  assert.equal(readback.status, 'ok');
  assert.equal(readback.source_mutation, false);
  assert.deepEqual(readback.manifest, started.orientation_manifest);
  assert.throws(
    () => readOrientationManifestGeneration({
      siteRoot,
      dbPath,
      manifestId: 'orientation:missing',
      admissionReceipt: admissionReceipt('fixture.resident'),
    }),
    /agent_context_orientation_manifest_generation_not_found/,
  );

  const writeDb = new DatabaseSync(dbPath);
  assert.throws(
    () => writeDb.prepare(
      'UPDATE orientation_manifest_generations SET readiness = ? WHERE manifest_id = ?',
    ).run('blocked', started.orientation_manifest.manifest_id),
    /orientation_manifest_generations_append_only_no_update/,
  );
  assert.throws(
    () => writeDb.prepare(
      'DELETE FROM orientation_manifest_generations WHERE manifest_id = ?',
    ).run(started.orientation_manifest.manifest_id),
    /orientation_manifest_generations_append_only_no_delete/,
  );
  writeDb.close();
}

// A site-root migration file still wins over the bundled one; other files fall back per-migration.
{
  const siteRoot = makeSite('site-override');
  mkdirSync(join(siteRoot, '.ai', 'db', 'migrations'), { recursive: true });
  writeFileSync(join(siteRoot, '.ai', 'db', 'migrations', '001-agent-context-materializations.sql'), `
CREATE TABLE IF NOT EXISTS agent_start_events (
  event_id TEXT PRIMARY KEY,
  identity_id TEXT NOT NULL,
  runtime TEXT NOT NULL,
  created_at TEXT NOT NULL,
  status TEXT NOT NULL,
  resume_command TEXT,
  bootstrap_artifact_uri TEXT,
  site_marker TEXT
);
`, 'utf8');

  const started = materializeAgentSessionStart({
    siteRoot,
    siteId: 'fixture',
    identity: 'fixture.resident',
    runtime: 'kimi',
    admissionReceipt: admissionReceipt('fixture.resident'),
    generatedAt: GENERATED_AT,
  });
  assert.equal(started.status, 'materialized');

  const dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite');
  const startColumns = columnNames(dbPath, 'agent_start_events');
  assert.equal(startColumns.has('site_marker'), true, 'site-root 001 migration did not win');

  // The site provided no 002 file, so agent_events came from the bundled fallback.
  const tables = tableNames(dbPath);
  assert.equal(tables.has('agent_events'), true, 'bundled 002 migration was not applied');
  const eventColumns = columnNames(dbPath, 'agent_events');
  assert.equal(eventColumns.has('agent_id'), true);
  assert.equal(eventColumns.has('event_type'), true);
}

// openAgentContextDb applies the busy timeout pragma so concurrent launches wait instead of failing with SQLITE_BUSY.
{
  const siteRoot = makeSite('busy-timeout');
  const db = openAgentContextDb(siteRoot);
  try {
    const timeout = db.prepare('PRAGMA busy_timeout').get().timeout;
    assert.equal(timeout, 5000);
    assert.equal(timeout, DEFAULT_BUSY_TIMEOUT_MS);
  } finally {
    db.close();
  }
}

console.log('session-start migrations tests passed');
