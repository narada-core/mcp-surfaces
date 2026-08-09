
import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { DatabaseSync } from '@narada-core/sqlite';
import { CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA } from '@narada-core/orientation-manifest';
import { materializeAgentSessionStart } from '../src/session-start.js';
import { fileURLToPath } from 'node:url';
import {
  createTemporaryE2eRoot,
  removeTemporaryE2eRoot,
  runMcpProtocolSmoke,
  siteFabricChildEnv,
  spawnJsonlMcpServer,
} from '@narada-core/mcp-e2e-harness';

const siteRoot = createTemporaryE2eRoot('agent-context-site-fabric-e2e');
const dbPath = `${siteRoot}/.ai/state/agent-context.sqlite`;
const carrierSessionId = 'carrier_fixture_resident';
const admissionReceipt = {
  schema: CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA,
  receipt_id: 'receipt:fixture-site:resident',
  decision: 'admitted',
  state: 'starting',
  coordinate: {
    authority_scope: 'test',
    site_ref: 'site:fixture-site',
    carrier_session_id: carrierSessionId,
    authority_epoch: 1,
  },
  agent_identity: {
    source_authority_ref: 'agent-identity:fixture-site',
    artifact_ref: 'agent:fixture.resident',
    revision: 'fixture-1',
    local_agent_id: 'fixture.resident',
    canonical_agent_id: 'fixture.resident',
  },
  carrier_kind: 'codex',
  admission_policy: {
    source_authority_ref: 'site-policy:fixture-site',
    artifact_ref: 'carrier-admission:test',
    revision: '1',
  },
  issued_at: '2026-08-08T12:00:00.000Z',
  valid_until: null,
  authority_readback_ref: 'carrier-session-authority:' + carrierSessionId,
  evidence_refs: ['test:site-fabric-admission'],
  reason_codes: [],
};
mkdirSync(`${siteRoot}/.ai/state`, { recursive: true });
writeFileSync(`${siteRoot}/AGENTS.md`, '# Controlled fixture Site\n', 'utf8');
mkdirSync(`${siteRoot}/.ai/agents`, { recursive: true });
writeFileSync(`${siteRoot}/.ai/agents/roster.json`, JSON.stringify({
  agents: [{ agent_id: 'fixture.resident', role: 'resident', status: 'active', capabilities: [] }],
}, null, 2), 'utf8');
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
    identity_id TEXT NOT NULL,
    event_kind TEXT NOT NULL,
    created_at TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}'
  );
`);
db.close();
const admittedStart: any = materializeAgentSessionStart({
  siteRoot,
  siteId: 'fixture-site',
  identity: 'fixture.resident',
  runtime: 'codex',
  dbPath,
  admissionReceipt,
  generatedAt: '2026-08-08T12:00:01.000Z',
});
assert.equal(admittedStart.status, 'materialized');

const serverPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
const server = spawnJsonlMcpServer(process.execPath, [serverPath, '--site-root', siteRoot, '--site-id', 'fixture-site'], {
  cwd: siteRoot,
  env: siteFabricChildEnv(siteRoot, {
    NARADA_AGENT_ID: 'fixture.resident',
    NARADA_CARRIER_SESSION_ID: carrierSessionId,
    NARADA_CARRIER_SESSION_ADMISSION_RECEIPT: JSON.stringify(admissionReceipt),
    NARADA_ORIENTATION_MANIFEST_ID: admittedStart.orientation_manifest.manifest_id,
    NARADA_SITE_ROOT: siteRoot,
    NARADA_AGENT_CONTEXT_DB: dbPath,
  }),
  label: 'agent-context Site-fabric e2e',
});

function structured(response: Record<string, unknown>): Record<string, unknown> {
  assert.equal(response.error, undefined, JSON.stringify(response));
  const result = response.result as Record<string, unknown>;
  return (result.structuredContent as Record<string, unknown>) ?? result;
}

async function structuredFull(
  response: Record<string, unknown>,
  idBase: number,
): Promise<Record<string, unknown>> {
  const initial = structured(response);
  const ref = typeof initial.output_ref === 'string' ? initial.output_ref : null;
  if (!ref) return initial;
  let offset = 0;
  let text = '';
  for (let pageIndex = 0; pageIndex < 32; pageIndex += 1) {
    const page = structured(await server.client.request(idBase + pageIndex, 'tools/call', {
      name: 'mcp_output_show',
      arguments: { ref, offset, limit: 20000 },
    }));
    text += String(page.output_text ?? '');
    if (page.next_offset === null) return JSON.parse(text);
    offset = Number(page.next_offset);
  }
  throw new Error('materialized_output_page_limit_exceeded');
}

try {
  await runMcpProtocolSmoke(server.client, {
    expectedServerName: 'fixture-site-agent-context-mcp',
    requiredTools: ['agent_context_doctor', 'agent_context_startup_sequence', 'agent_context_checkpoint', 'agent_context_rehydrate', 'mcp_output_show'],
  });

  const doctor = structured(await server.client.request(1, 'tools/call', { name: 'agent_context_doctor', arguments: {} }));
  assert.equal(doctor.status, 'ok', JSON.stringify(doctor));
  assert.equal(doctor.site_root, siteRoot);

  const whoami = structured(await server.client.request(2, 'tools/call', { name: 'agent_context_whoami', arguments: {} }));
  assert.equal(whoami.identity, 'fixture.resident', JSON.stringify(whoami));
  assert.equal(whoami.source, 'carrier_session_admission_receipt');
  assert.equal((whoami.carrier_session as Record<string, unknown>).carrier_session_id, carrierSessionId);

  const startup = await structuredFull(await server.client.request(3, 'tools/call', {
    name: 'agent_context_startup_sequence',
    arguments: {},
  }), 300);
  assert.equal(startup.status, 'ok', JSON.stringify(startup));
  assert.equal((startup.orientation_manifest as Record<string, unknown>).delivery, 'deliverable');
  assert.equal(
    (startup.orientation_manifest as Record<string, unknown>).manifest_id,
    admittedStart.orientation_manifest.manifest_id,
  );
  assert.deepEqual(startup.orientation_manifest, admittedStart.orientation_manifest);
  assert.equal((startup.manifest_readback as Record<string, unknown>).exact_generation, true);
  assert.equal(startup.delivery_authority_claimed, false);
  assert.equal('startup_checkpoint' in startup, false);
  const readOnlyDb = new DatabaseSync(dbPath, { readOnly: true });
  const startupCheckpointCount = (
    readOnlyDb.prepare('SELECT COUNT(*) AS count FROM agent_checkpoints').get() as any
  )?.count ?? -1;
  const startEventCount = (
    readOnlyDb.prepare('SELECT COUNT(*) AS count FROM agent_start_events').get() as any
  )?.count ?? -1;
  const generationCount = (
    readOnlyDb.prepare('SELECT COUNT(*) AS count FROM orientation_manifest_generations').get() as any
  )?.count ?? -1;
  readOnlyDb.close();
  assert.equal(startupCheckpointCount, 0);
  assert.equal(startEventCount, 1);
  assert.equal(generationCount, 1);

  const checkpoint = structured(await server.client.request(4, 'tools/call', {
    name: 'agent_context_checkpoint',
    arguments: {
      agent_id: 'fixture.resident',
      key_decisions: Array.from({ length: 300 }, (_: any, index: any) => `bounded-decision-${index}`),
      authority_basis: { kind: 'site-fabric-e2e', summary: 'Controlled checkpoint persistence.' },
      next_intended_action: { kind: 'verify', summary: 'Read the persisted checkpoint.' },
    },
  }));
  assert.equal(checkpoint.status, 'checkpointed', JSON.stringify(checkpoint));

  const rehydrated = structured(await server.client.request(5, 'tools/call', {
    name: 'agent_context_rehydrate',
    arguments: { agent_id: 'fixture.resident' },
  }));
  assert.match(String(rehydrated.output_ref), /^mcp_output:/, JSON.stringify(rehydrated));

  const outputPage = structured(await server.client.request(6, 'tools/call', {
    name: 'mcp_output_show',
    arguments: { ref: rehydrated.output_ref, offset: 0, limit: 1000 },
  }));
  assert.equal(outputPage.schema, 'narada.mcp_output_page.v1', JSON.stringify(outputPage));
  assert.ok(String(outputPage.output_text).includes('fixture.resident'), JSON.stringify(outputPage));

  console.log(JSON.stringify({ status: 'passed', test_id: 'agent-context.site-fabric.orientation-readonly-rehydrate', site_root: siteRoot, cleanup: 'pending_until_finally' }));
} finally {
  await server.close();
  assert.equal(removeTemporaryE2eRoot(siteRoot), true);
}

console.log('agent-context Site fabric e2e ok');
