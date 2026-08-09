
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { DatabaseSync } from '@narada-core/sqlite';
import { CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA } from '@narada-core/orientation-manifest';
import { materializeAgentSessionStart } from '../src/session-start.js';

const siteRoot = mkdtempSync(join(tmpdir(), 'agent-context-mcp-'));
const siteId = 'narada-revolution';
const canonicalSiteId = 'narada.revolution';
const agentId = 'narada-revolution.resident';
const carrierSessionId = 'carrier_agent_context_fixture';
const admissionReceipt = {
  schema: CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA,
  receipt_id: 'receipt:agent-context-fixture',
  decision: 'admitted',
  state: 'starting',
  coordinate: {
    authority_scope: 'test',
    site_ref: 'site:' + canonicalSiteId,
    carrier_session_id: carrierSessionId,
    authority_epoch: 1,
  },
  agent_identity: {
    source_authority_ref: 'agent-identity:' + canonicalSiteId,
    artifact_ref: 'agent:' + agentId,
    revision: 'fixture-1',
    local_agent_id: agentId,
    canonical_agent_id: agentId,
  },
  carrier_kind: 'codex',
  admission_policy: {
    source_authority_ref: 'site-policy:' + canonicalSiteId,
    artifact_ref: 'carrier-admission:test',
    revision: '1',
  },
  issued_at: '2026-08-08T12:00:00.000Z',
  valid_until: null,
  authority_readback_ref: 'carrier-session-authority:' + carrierSessionId,
  evidence_refs: ['test:agent-context-admission'],
  reason_codes: [],
};
writeFileSync(join(siteRoot, 'AGENTS.md'), '# Fixture Site\n', 'utf8');
mkdirSync(join(siteRoot, '.ai', 'agents'), { recursive: true });
writeFileSync(join(siteRoot, '.ai', 'agents', 'roster.json'), JSON.stringify({
  agents: [
    { agent_id: 'sonar.architect', role: 'architect', capabilities: [] },
    { agent_id: 'narada-revolution.resident', role: 'resident', capabilities: [] },
  ],
}, null, 2), 'utf8');

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
    identity_id TEXT NOT NULL,
    event_kind TEXT NOT NULL,
    created_at TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}'
  );
`);
db.close();
const admittedStart: any = materializeAgentSessionStart({
  siteRoot,
  siteId: canonicalSiteId,
  identity: agentId,
  runtime: 'codex',
  dbPath,
  admissionReceipt,
  generatedAt: '2026-08-08T12:00:01.000Z',
});
assert.equal(admittedStart.status, 'materialized');

const serverPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
const proc = spawn(process.execPath, [serverPath, '--site-root', siteRoot, '--site-id', siteId], {
  cwd: siteRoot,
  env: {
    ...process.env,
    NARADA_AGENT_ID: agentId,
    NARADA_CARRIER_SESSION_ID: carrierSessionId,
    NARADA_CARRIER_SESSION_ADMISSION_RECEIPT: JSON.stringify(admissionReceipt),
    NARADA_ORIENTATION_MANIFEST_ID: admittedStart.orientation_manifest.manifest_id,
    NARADA_SITE_ROOT: siteRoot,
    NARADA_AGENT_CONTEXT_DB: dbPath,
  },
  stdio: ['pipe', 'pipe', 'pipe'],
  windowsHide: true,
});

let stdout = '';
let stderr = '';
proc.stdout.setEncoding('utf8');
proc.stderr.setEncoding('utf8');
proc.stdout.on('data', (chunk: any) => { stdout += chunk; });
proc.stderr.on('data', (chunk: any) => { stderr += chunk; });

function writeMessage(message: any, separator: any = '\r\n\r\n') {
  const body = JSON.stringify(message);
  proc.stdin.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}${separator}${body}`);
}

function writeJsonLine(message: any) {
  proc.stdin.write(`${JSON.stringify(message)}\n`);
}

function readOne() {
  if (stdout.startsWith('{')) {
    const lineEnd = stdout.indexOf('\n');
    if (lineEnd < 0) return null;
    const line = stdout.slice(0, lineEnd);
    stdout = stdout.slice(lineEnd + 1);
    return JSON.parse(line);
  }
  const headerEnd = stdout.indexOf('\r\n\r\n');
  if (headerEnd < 0) return null;
  const header = stdout.slice(0, headerEnd);
  const match = header.match(/Content-Length:\s*(\d+)/i);
  if (!match) throw new Error(`bad_header:${header}`);
  const length = Number(match[1]);
  const bodyStart = headerEnd + 4;
  if (stdout.length < bodyStart + length) return null;
  const body = stdout.slice(bodyStart, bodyStart + length);
  stdout = stdout.slice(bodyStart + length);
  return JSON.parse(body);
}

async function waitFor(id: any) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const message = readOne();
    if (message?.id === id) return message;
    await new Promise((resolve: any) => setTimeout(resolve, 20));
  }
  throw new Error(`timeout:${id}; stderr=${stderr}`);
}

async function readMaterializedJson(ref: string, idBase: number) {
  let offset = 0;
  let text = '';
  for (let pageIndex = 0; pageIndex < 32; pageIndex += 1) {
    const id = idBase + pageIndex;
    writeMessage({ jsonrpc: '2.0', id, method: 'tools/call', params: { name: 'mcp_output_show', arguments: { ref, offset, limit: 20000 } } });
    const response = await waitFor(id);
    assert.equal(response.error, undefined);
    const body = JSON.parse(response.result.content[0].text);
    text += String(body.output_text ?? '');
    if (body.next_offset === null) return JSON.parse(text);
    offset = Number(body.next_offset);
  }
  throw new Error('materialized_output_page_limit_exceeded');
}

try {
  writeMessage({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'agent-context-mcp-test', version: '0.1.0' } } });
  const init = await waitFor(1);
  assert.equal(init.error, undefined);
  writeMessage({ jsonrpc: '2.0', method: 'notifications/initialized', params: {} });
  writeMessage({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} });
  const tools = await waitFor(2);
  assert.equal(tools.error, undefined);
  const names = tools.result.tools.map((tool: any) => tool.name);
  assert.equal(names.includes('agent_context_hydrate_current'), true);
  assert.equal(names.includes('agent_context_startup_sequence'), true);
  assert.equal(names.includes('agent_context_continuation_export'), true);
  assert.equal(names.includes('agent_context_continuation_read'), true);
  assert.equal(names.includes('startup_sequence'), false);
  const checkpointTool = tools.result.tools.find((tool: any) => tool.name === 'agent_context_checkpoint');
  assert.equal(checkpointTool.inputSchema.properties.continuation_ref.properties.path.type, 'string');
  assert.equal(checkpointTool.inputSchema.properties.continuation.properties.schema.const, 'narada.continuation.v1');
  for (const toolName of [
    'agent_context_rehydrate',
    'agent_context_continuation_read',
    'agent_context_hydrate_current',
  ]) {
    const tool = tools.result.tools.find((candidate: any) => candidate.name === toolName);
    assert.equal(tool.inputSchema.properties.checkpoint_id.type, 'string');
  }
  const whoamiTool = tools.result.tools.find((tool: any) => tool.name === 'agent_context_whoami');
  assert.equal(whoamiTool.inputSchema.properties.admission_receipt.type, 'object');
  const hydrateTool = tools.result.tools.find((tool: any) => tool.name === 'agent_context_hydrate_current');
  assert.equal('checkpoint_startup' in hydrateTool.inputSchema.properties, false);
  const startupTool = tools.result.tools.find((tool: any) => tool.name === 'agent_context_startup_sequence');
  assert.equal(startupTool.inputSchema.properties.manifest_id.type, 'string');
  assert.equal('checkpoint_id' in startupTool.inputSchema.properties, false);
  writeMessage({ jsonrpc: '2.0', id: 3, method: 'tools/list', params: {} }, '\n\n');
  const lfTools = await waitFor(3);
  assert.equal(lfTools.error, undefined);
  writeJsonLine({ jsonrpc: '2.0', id: 4, method: 'tools/list', params: {} });
  const jsonLineTools = await waitFor(4);
  assert.equal(jsonLineTools.error, undefined);
  writeMessage({ jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'agent_context_whoami', arguments: {} } });
  const whoami = await waitFor(5);
  assert.equal(whoami.error, undefined);
  const identity = JSON.parse(whoami.result.content[0].text);
  assert.equal(identity.identity, agentId);
  assert.equal(identity.canonical_agent_id, agentId);
  assert.equal(identity.confidence, 'exact');
  assert.equal(identity.source, 'carrier_session_admission_receipt');
  assert.equal(identity.carrier_session.carrier_session_id, carrierSessionId);
  writeMessage({ jsonrpc: '2.0', id: 50, method: 'tools/call', params: { name: 'agent_context_startup_sequence', arguments: {} } });
  const startup = await waitFor(50);
  assert.equal(startup.error, undefined);
  let startupBody = JSON.parse(startup.result.content[0].text);
  if (startupBody.output_ref) {
    startupBody = await readMaterializedJson(startupBody.output_ref, 5000);
  }
  assert.equal(startupBody.status, 'ok');
  assert.equal(startupBody.source_mutation, false);
  assert.equal(startupBody.delivery_authority_claimed, false);
  assert.equal(startupBody.delivery_receipt, null);
  assert.equal(startupBody.manifest_readback.exact_generation, true);
  assert.equal(
    startupBody.orientation_manifest.manifest_id,
    admittedStart.orientation_manifest.manifest_id,
  );
  assert.deepEqual(startupBody.orientation_manifest, admittedStart.orientation_manifest);
  writeMessage({
    jsonrpc: '2.0',
    id: 51,
    method: 'tools/call',
    params: {
      name: 'agent_context_startup_sequence',
      arguments: { checkpoint_id: 'chk_forbidden_at_delivery' },
    },
  });
  const startupRecompileAttempt = await waitFor(51);
  assert.equal(startupRecompileAttempt.error, undefined);
  const startupRecompileAttemptBody = JSON.parse(startupRecompileAttempt.result.content[0].text);
  assert.equal(startupRecompileAttemptBody.status, 'blocked');
  assert.equal(startupRecompileAttemptBody.reason, 'orientation_startup_exact_generation_only');
  writeMessage({
    jsonrpc: '2.0',
    id: 52,
    method: 'tools/call',
    params: {
      name: 'agent_context_whoami',
      arguments: {
        admission_receipt: {
          ...admissionReceipt,
          carrier_kind: 'kimi',
        },
      },
    },
  });
  const conflictingReceipt = await waitFor(52);
  assert.match(
    String(conflictingReceipt.error?.message ?? ''),
    /agent_context_conflicting_admission_receipts/,
  );
  const continuationContent = '# Agent-context continuation test\n';
  const continuationPath = join(siteRoot, '.ai', 'continuations', 'agent-context-test.md');
  mkdirSync(join(siteRoot, '.ai', 'continuations'), { recursive: true });
  writeFileSync(continuationPath, continuationContent, 'utf8');
  const continuationRef = {
    schema: 'narada.continuation.handoff.v1',
    path: '.ai/continuations/agent-context-test.md',
    sha256: createHash('sha256').update(continuationContent, 'utf8').digest('hex'),
    created_at: '2026-07-13T00:00:00.000Z',
  };
  const continuation = {
    schema: 'narada.continuation.v1',
    continuation_id: 'continuation-test-1',
    objective: 'Verify canonical continuation state survives checkpoint rehydration.',
    current_state: 'The checkpoint contains one portable, bounded continuation envelope.',
    completed_work: ['Added the first continuation envelope fixture.'],
    decisions: ['Keep continuation state in checkpoint payload_json.'],
    evidence_refs: ['test:agent-context-mcp'],
    open_blockers: [],
    next_action: 'Read the checkpoint back and verify its content hash.',
    canonical_sources: ['AGENTS.md', 'packages/agent-context-mcp/src/main.ts'],
    constraints: ['Do not create a second persistence table.'],
    resume_mode: 'fresh_session',
    created_at: '2026-07-13T00:00:00.000Z',
  };
  writeMessage({ jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'agent_context_checkpoint', arguments: { agent_id: 'narada-revolution.resident', key_decisions: ['site-local checkpoint regression'], continuation, continuation_ref: continuationRef } } });
  const checkpoint = await waitFor(6);
  assert.equal(checkpoint.error, undefined);
  const checkpointBody = JSON.parse(checkpoint.result.content[0].text);
  assert.equal(checkpointBody.status, 'checkpointed');
  assert.equal(checkpointBody.site_root, siteRoot);
  assert.deepEqual(checkpointBody.continuation_ref, { ...continuationRef, sha256: continuationRef.sha256.toLowerCase() });
  assert.equal(checkpointBody.continuation.schema, 'narada.continuation.v1');
  assert.equal(checkpointBody.continuation.continuation_id, continuation.continuation_id);
  assert.equal(checkpointBody.continuation.source_checkpoint_ref.startsWith('agent_context_checkpoint:chk_'), true);
  const continuationForHash = { ...checkpointBody.continuation };
  delete continuationForHash.content_hash;
  delete continuationForHash.source_checkpoint_ref;
  assert.equal(
    checkpointBody.continuation.content_hash,
    createHash('sha256').update(JSON.stringify(continuationForHash), 'utf8').digest('hex'),
  );
  writeMessage({ jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name: 'agent_context_checkpoint', arguments: { agent_id: 'narada-revolution.resident', continuation_ref: { ...continuationRef, sha256: 'B'.repeat(64) } } } });
  const invalidContinuationRef = await waitFor(7);
  assert.equal(invalidContinuationRef.error.code, -32000);
  assert.match(invalidContinuationRef.error.message, /continuation_ref_sha256_mismatch/);
  writeMessage({ jsonrpc: '2.0', id: 8, method: 'tools/call', params: { name: 'agent_context_checkpoint', arguments: { agent_id: 'narada-revolution.resident', continuation_ref: { ...continuationRef, path: 'C:/outside.md' } } } });
  const outsideContinuation = await waitFor(8);
  assert.equal(outsideContinuation.error.code, -32000);
  assert.match(outsideContinuation.error.message, /continuation_ref_path_must_be_site_relative/);
  writeMessage({ jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name: 'agent_context_checkpoint', arguments: { agent_id: 'narada-revolution.resident', continuation: { ...continuation, objective: '' } } } });
  const invalidContinuation = await waitFor(9);
  assert.equal(invalidContinuation.error.code, -32000);
  assert.match(invalidContinuation.error.message, /continuation_objective_invalid/);
  writeMessage({ jsonrpc: '2.0', id: 10, method: 'tools/call', params: { name: 'agent_context_rehydrate', arguments: { agent_id: 'narada-revolution.resident' } } });
  const rehydrate = await waitFor(10);
  assert.equal(rehydrate.error, undefined);
  const rehydrateBody = JSON.parse(rehydrate.result.content[0].text);
  assert.equal(rehydrateBody.payload.site_id, 'narada.revolution');
  assert.deepEqual(rehydrateBody.continuation_ref, { ...continuationRef, sha256: continuationRef.sha256.toLowerCase() });
  assert.deepEqual(rehydrateBody.continuation, checkpointBody.continuation);
  writeMessage({ jsonrpc: '2.0', id: 11, method: 'tools/call', params: { name: 'agent_context_continuation_export', arguments: { agent_id: 'narada-revolution.resident' } } });
  const initialExport = await waitFor(11);
  assert.equal(initialExport.error, undefined);
  const initialExportBody = JSON.parse(initialExport.result.content[0].text);
  assert.equal(initialExportBody.status, 'exported');
  assert.equal(initialExportBody.checkpoint_id, checkpointBody.checkpoint_id);
  assert.equal(initialExportBody.artifact.wrote, true);
  writeMessage({ jsonrpc: '2.0', id: 12, method: 'tools/call', params: { name: 'agent_context_checkpoint', arguments: { agent_id: 'narada-revolution.resident', continuation: { ...continuation, current_state: 'A later checkpoint keeps the same canonical continuation contract.' } } } });
  const updatedCheckpoint = await waitFor(12);
  assert.equal(updatedCheckpoint.error, undefined);
  writeMessage({ jsonrpc: '2.0', id: 13, method: 'tools/call', params: { name: 'agent_context_rehydrate', arguments: { agent_id: 'narada-revolution.resident', history: true, limit: 1 } } });
  const history = await waitFor(13);
  assert.equal(history.error, undefined);
  const historyBody = JSON.parse(history.result.content[0].text);
  assert.equal(historyBody.status, 'ok');
  assert.deepEqual(historyBody.checkpoints[0].continuation, checkpointBody.continuation);
  const archivedCheckpointId = historyBody.checkpoints[0].checkpoint_id;
  assert.equal(archivedCheckpointId, checkpointBody.checkpoint_id);
  assert.deepEqual(historyBody.checkpoints[0].continuation_ref, initialExportBody.continuation_ref);

  writeMessage({ jsonrpc: '2.0', id: 14, method: 'tools/call', params: { name: 'agent_context_rehydrate', arguments: { agent_id: 'narada-revolution.resident', checkpoint_id: archivedCheckpointId, history: false } } });
  const exactRehydrate = await waitFor(14);
  assert.equal(exactRehydrate.error, undefined);
  const exactRehydrateBody = JSON.parse(exactRehydrate.result.content[0].text);
  assert.equal(exactRehydrateBody.status, 'ok');
  assert.equal(exactRehydrateBody.checkpoint_id, archivedCheckpointId);
  assert.deepEqual(exactRehydrateBody.continuation, checkpointBody.continuation);

  writeMessage({ jsonrpc: '2.0', id: 15, method: 'tools/call', params: { name: 'agent_context_continuation_read', arguments: { agent_id: 'narada-revolution.resident', checkpoint_id: archivedCheckpointId } } });
  const exactContinuation = await waitFor(15);
  assert.equal(exactContinuation.error, undefined);
  const exactContinuationBody = JSON.parse(exactContinuation.result.content[0].text);
  assert.equal(exactContinuationBody.status, 'ok');
  assert.equal(exactContinuationBody.checkpoint_id, archivedCheckpointId);
  assert.equal(exactContinuationBody.artifact.verified, true);

  const missingCheckpointId = `chk_${'f'.repeat(32)}`;
  writeMessage({ jsonrpc: '2.0', id: 16, method: 'tools/call', params: { name: 'agent_context_rehydrate', arguments: { agent_id: 'narada-revolution.resident', checkpoint_id: missingCheckpointId } } });
  const missingRehydrate = await waitFor(16);
  assert.equal(missingRehydrate.error, undefined);
  const missingRehydrateBody = JSON.parse(missingRehydrate.result.content[0].text);
  assert.equal(missingRehydrateBody.status, 'checkpoint_not_found');
  assert.equal(missingRehydrateBody.checkpoint_id, missingCheckpointId);

  writeMessage({ jsonrpc: '2.0', id: 17, method: 'tools/call', params: { name: 'agent_context_continuation_read', arguments: { agent_id: 'narada-revolution.resident', checkpoint_id: missingCheckpointId } } });
  const missingContinuation = await waitFor(17);
  assert.equal(missingContinuation.error, undefined);
  const missingContinuationBody = JSON.parse(missingContinuation.result.content[0].text);
  assert.equal(missingContinuationBody.status, 'checkpoint_not_found');
  assert.equal(missingContinuationBody.checkpoint_id, missingCheckpointId);

  writeMessage({ jsonrpc: '2.0', id: 18, method: 'tools/call', params: { name: 'agent_context_hydrate_current', arguments: { checkpoint_id: missingCheckpointId } } });
  const missingHydrated = await waitFor(18);
  assert.equal(missingHydrated.error, undefined);
  let missingHydratedBody = JSON.parse(missingHydrated.result.content[0].text);
  if (missingHydratedBody.output_ref) {
    missingHydratedBody = await readMaterializedJson(missingHydratedBody.output_ref, 1800);
  }
  assert.equal(missingHydratedBody.status, 'ok');
  assert.equal(missingHydratedBody.checkpoint.checkpoint_id, missingCheckpointId);
  assert.equal(missingHydratedBody.checkpoint.status, 'checkpoint_not_found');
  assert.equal(missingHydratedBody.orientation_manifest.readiness, 'degraded');
  assert.ok(missingHydratedBody.orientation_manifest.residuals.some(
    (item: any) => item.code === 'exact_continuity_unavailable',
  ));
  assert.equal('startup_checkpoint' in missingHydratedBody, false);

  writeMessage({ jsonrpc: '2.0', id: 19, method: 'tools/call', params: { name: 'agent_context_hydrate_current', arguments: { checkpoint_id: archivedCheckpointId } } });
  const exactHydrated = await waitFor(19);
  assert.equal(exactHydrated.error, undefined);
  let exactHydratedBody = JSON.parse(exactHydrated.result.content[0].text);
  if (exactHydratedBody.output_ref) {
    exactHydratedBody = await readMaterializedJson(exactHydratedBody.output_ref, 2000);
  }
  assert.equal(exactHydratedBody.status, 'ok');
  assert.equal(exactHydratedBody.checkpoint.status, 'ok');
  assert.equal(exactHydratedBody.checkpoint.checkpoint_id, archivedCheckpointId);
  assert.equal(exactHydratedBody.portable_continuation.status, 'ok');
  assert.equal(exactHydratedBody.portable_continuation.checkpoint_id, archivedCheckpointId);
  assert.equal(exactHydratedBody.orientation_manifest.delivery, 'deliverable');
  assert.equal(exactHydratedBody.continuity_selection.mode, 'exact');

  writeMessage({ jsonrpc: '2.0', id: 21, method: 'tools/call', params: { name: 'agent_context_continuation_export', arguments: { agent_id: 'narada-revolution.resident' } } });
  const exported = await waitFor(21);
  assert.equal(exported.error, undefined);
  const exportedBody = JSON.parse(exported.result.content[0].text);
  assert.equal(exportedBody.status, 'exported');
  assert.equal(exportedBody.continuation_ref.schema, 'narada.continuation.handoff.v1');
  assert.match(exportedBody.continuation_ref.path, /^\.ai\/continuations\/narada-revolution\.resident-chk_[a-f0-9]+\.md$/);
  assert.equal(exportedBody.artifact.wrote, true);
  const exportedPath = join(siteRoot, ...exportedBody.continuation_ref.path.split('/'));
  const exportedMarkdown = readFileSync(exportedPath, 'utf8');
  assert.match(exportedMarkdown, /narada\.continuation\.handoff\.v1/);
  assert.match(exportedMarkdown, new RegExp(exportedBody.continuation.content_hash));

  writeMessage({ jsonrpc: '2.0', id: 22, method: 'tools/call', params: { name: 'agent_context_continuation_read', arguments: { agent_id: 'narada-revolution.resident' } } });
  const consumed = await waitFor(22);
  assert.equal(consumed.error, undefined);
  const consumedBody = JSON.parse(consumed.result.content[0].text);
  assert.equal(consumedBody.status, 'ok');
  assert.equal(consumedBody.artifact.verified, true);
  assert.deepEqual(consumedBody.continuation, exportedBody.continuation);
  assert.equal(consumedBody.continuation_ref.sha256, exportedBody.continuation_ref.sha256);
  assert.match(consumedBody.artifact.markdown, /This file is a bounded projection/);

  writeMessage({ jsonrpc: '2.0', id: 23, method: 'tools/call', params: { name: 'agent_context_hydrate_current', arguments: {} } });
  const hydrated = await waitFor(23);
  assert.equal(hydrated.error, undefined);
  let hydratedBody = JSON.parse(hydrated.result.content[0].text);
  if (hydratedBody.output_ref) {
    hydratedBody = await readMaterializedJson(hydratedBody.output_ref, 2400);
  }
  assert.equal(hydratedBody.checkpoint.status, 'omitted');
  assert.equal(hydratedBody.portable_continuation.status, 'omitted');
  assert.equal(hydratedBody.continuity_selection.mode, 'omitted');
  assert.equal('next_required_action' in hydratedBody, false);

  writeFileSync(exportedPath, `${exportedMarkdown}\nmutated`, 'utf8');
  writeMessage({ jsonrpc: '2.0', id: 25, method: 'tools/call', params: { name: 'agent_context_continuation_read', arguments: { agent_id: 'narada-revolution.resident' } } });
  const stale = await waitFor(25);
  assert.equal(stale.error, undefined);
  const staleBody = JSON.parse(stale.result.content[0].text);
  assert.equal(staleBody.status, 'stale');
  assert.match(staleBody.reason, /continuation_ref_sha256_mismatch/);
  console.log('agent context MCP tests passed');
} finally {
  proc.stdin?.destroy();
  proc.stdout?.destroy();
  proc.stderr?.destroy();
  proc.kill();
}

const boundSiteRoot = mkdtempSync(join(tmpdir(), 'agent-context-bound-'));
const foreignSiteRoot = mkdtempSync(join(tmpdir(), 'agent-context-foreign-'));
for (const root of [boundSiteRoot, foreignSiteRoot]) {
  writeFileSync(join(root, 'AGENTS.md'), '# Fixture Site\n', 'utf8');
  mkdirSync(join(root, '.ai', 'agents'), { recursive: true });
  writeFileSync(join(root, '.ai', 'agents', 'roster.json'), JSON.stringify({
    agents: [{ agent_id: 'narada-revolution.resident', role: 'resident', capabilities: [] }],
  }, null, 2), 'utf8');
}

const mismatchProc = spawn(process.execPath, [serverPath, '--site-root', boundSiteRoot, '--site-id', 'narada-bound'], {
  cwd: foreignSiteRoot,
  env: {
    ...process.env,
    NARADA_AGENT_ID: 'narada-revolution.resident',
    NARADA_SITE_ROOT: foreignSiteRoot,
    NARADA_AGENT_CONTEXT_DB: join(boundSiteRoot, '.ai', 'state', 'agent-context.sqlite'),
  },
  stdio: ['pipe', 'pipe', 'pipe'],
  windowsHide: true,
});
let mismatchStderr = '';
mismatchProc.stderr.setEncoding('utf8');
mismatchProc.stderr.on('data', (chunk: any) => { mismatchStderr += chunk; });
const mismatchExit = await waitForExit(mismatchProc) as { code: number | null; signal: string | null };
assert.notEqual(mismatchExit.code, 0);
assert.match(mismatchStderr, /agent_context_site_root_mismatch/);

const foreignDbProc = spawn(process.execPath, [serverPath, '--site-root', boundSiteRoot, '--site-id', 'narada-bound'], {
  cwd: boundSiteRoot,
  env: {
    ...process.env,
    NARADA_AGENT_ID: 'narada-revolution.resident',
    NARADA_SITE_ROOT: boundSiteRoot,
    NARADA_AGENT_CONTEXT_DB: join(foreignSiteRoot, '.ai', 'state', 'agent-context.sqlite'),
  },
  stdio: ['pipe', 'pipe', 'pipe'],
  windowsHide: true,
});
let foreignDbStderr = '';
foreignDbProc.stderr.setEncoding('utf8');
foreignDbProc.stderr.on('data', (chunk: any) => { foreignDbStderr += chunk; });
const foreignDbExit = await waitForExit(foreignDbProc) as { code: number | null; signal: string | null };
assert.notEqual(foreignDbExit.code, 0);
assert.match(foreignDbStderr, /agent_context_db_path_outside_site_root/);

function waitForExit(child: any) {
  return new Promise((resolve: any) => {
    child.once('exit', (code: any, signal: any) => resolve({ code, signal }));
  });
}



