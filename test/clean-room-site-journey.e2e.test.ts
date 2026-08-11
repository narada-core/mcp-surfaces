import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from '../../narada-core/packages/sqlite/dist/index.js';
import { CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA, issueCarrierSessionOrientationDeliveryReceipt } from '../../narada/packages/orientation-manifest/dist/index.js';
import { materializeAgentSessionStart, recordOrientationDeliveryReceipt } from '../packages/agent-context-mcp/src/session-start.js';
import { siteFabricChildEnv, spawnJsonlMcpServer } from '../packages/shared/mcp-e2e-harness/dist/src/main.js';

const surfacesRoot = resolve(fileURLToPath(new URL('..', import.meta.url)));
const naradaRoot = resolve(process.env.NARADA_CORE_ROOT ?? join(surfacesRoot, '..', 'narada'));
const cli = join(naradaRoot, 'packages', 'layers', 'cli', 'dist', 'main.js');
const taskServer = join(surfacesRoot, 'packages', 'task-lifecycle-mcp', 'dist', 'src', 'task-lifecycle', 'task-mcp-server.js');
const feedbackServer = join(surfacesRoot, 'packages', 'surface-feedback-mcp', 'dist', 'src', 'main.js');
const contextServer = join(surfacesRoot, 'packages', 'agent-context-mcp', 'dist', 'src', 'main.js');
const externallyManagedRoot = process.env.NARADA_CLEAN_ROOM_SITE_ROOT;
const root = externallyManagedRoot ? resolve(externallyManagedRoot) : mkdtempSync(join(tmpdir(), 'narada-clean-room-site-'));
const profile = join(root, 'profile');
const workspace = join(root, 'workspace');
const siteRoot = join(workspace, '.narada');
const siteId = 'clean-room-site';
mkdirSync(profile, { recursive: true });
mkdirSync(join(workspace, '.git'), { recursive: true });

function runCli(args: string[]): Record<string, any> {
  const result = spawnSync(process.execPath, [cli, ...args, '--format', 'json'], {
    cwd: workspace,
    encoding: 'utf8',
    timeout: 120_000,
    env: { ...process.env, HOME: profile, USERPROFILE: profile, NARADA_USER_SITE_ROOT: join(profile, 'Narada') },
  });
  assert.equal(result.status, 0, `${args.join(' ')}\n${result.stdout}\n${result.stderr}`);
  const start = result.stdout.search(/[\[{]/);
  assert.notEqual(start, -1, result.stdout);
  return JSON.parse(result.stdout.slice(start));
}
function body(response: Record<string, any>): Record<string, any> {
  assert.equal(response.error, undefined, JSON.stringify(response));
  return response.result?.structuredContent ?? response.result;
}
async function full(response: Record<string, any>, client: any, id: number): Promise<Record<string, any>> {
  const value = body(response);
  if (!value.output_ref) return value;
  let offset = 0; let text = '';
  for (let page = 0; page < 32; page += 1) {
    const valuePage = body(await client.request(id + page, 'tools/call', { name: 'mcp_output_show', arguments: { ref: value.output_ref, offset, limit: 20000 } }));
    text += String(valuePage.output_text ?? '');
    if (valuePage.next_offset == null) return JSON.parse(text);
    offset = Number(valuePage.next_offset);
  }
  throw new Error('clean_room_output_page_limit');
}

let task: ReturnType<typeof spawnJsonlMcpServer> | null = null;
let reviewer: ReturnType<typeof spawnJsonlMcpServer> | null = null;
let feedback: ReturnType<typeof spawnJsonlMcpServer> | null = null;
let context: ReturnType<typeof spawnJsonlMcpServer> | null = null;
try {
  const bootstrap = runCli(['sites', 'bootstrap-project', '--workspace', workspace, '--site-id', siteId, '--execute']);
  assert.equal(bootstrap.status, 'success');
  assert.equal(bootstrap.mutation_performed, true);
  assert.equal(bootstrap.mcp_materialization_recovery?.schema, 'narada.carrier_materialization_recovery.v1');
  assert.match(String(bootstrap.mcp_materialization_recovery?.status), /^(current|recovered)$/);
  runCli(['operator-surface', 'identity', 'add', `${siteId}.architect`, '--site', siteId, '--role', 'architect', '--agent-kind', 'codex_cli', '--by', 'operator', '--cwd', siteRoot]);
  runCli(['operator-surface', 'identity', 'add', `${siteId}.builder`, '--site', siteId, '--role', 'builder', '--agent-kind', 'codex_cli', '--by', 'operator', '--cwd', siteRoot]);
  runCli(['operator-surface', 'identity', 'admit-task-authority', `${siteId}.architect`, '--by', 'operator', '--cwd', siteRoot]);
  runCli(['operator-surface', 'identity', 'admit-task-authority', `${siteId}.builder`, '--by', 'operator', '--cwd', siteRoot]);
  runCli(['operator-surface', 'bind-focused', '--identity', `${siteId}.builder`, '--runtime-locus', siteId, '--handle', 'session:clean-builder', '--observed-handle', 'session:clean-builder', '--window-title', `${siteId}.builder`, '--window-class', 'NaradaCarrierSession', '--process-name', 'codex', '--process-id', '4242', '--cwd', siteRoot]);
  const rolePlane = JSON.parse(readFileSync(join(siteRoot, '.ai', 'agents', 'role-plane.json'), 'utf8'));
  assert.equal(rolePlane.roles.find((role: any) => role.role_id === 'builder').declaration_status, 'active');


  task = spawnJsonlMcpServer(process.execPath, [taskServer, '--site-root', workspace], { cwd: workspace, env: siteFabricChildEnv(workspace, { NARADA_AGENT_ID: `${siteId}.builder`, NARADA_SITE_ID: siteId }), label: 'clean-room lifecycle' });
  const payload = body(await task.client.request(1, 'tools/call', { name: 'mcp_payload_create', arguments: { payload: { title: 'Clean room journey task', goal: 'Prove execution and reviewed closure.', required_work: ['Execute controlled work.'], acceptance_criteria: ['Reviewed closure is durable.'], target_role: 'builder' } } }));
  const created = body(await task.client.request(2, 'tools/call', { name: 'task_lifecycle_create', arguments: { payload_ref: payload.ref } }));
  const taskNumber = Number(created.task_number); assert.ok(taskNumber > 0, JSON.stringify(created));
  assert.equal(body(await task.client.request(3, 'tools/call', { name: 'task_lifecycle_claim', arguments: { task_number: taskNumber, agent_id: `${siteId}.builder` } })).status, 'claimed');
  await task.client.request(4, 'tools/call', { name: 'task_lifecycle_disposition_closeout', arguments: { task_number: taskNumber, agent_id: `${siteId}.builder`, disposition: 'acknowledged', summary: 'Clean-room work executed.', no_files_changed: true } });
  await task.client.request(5, 'tools/call', { name: 'task_lifecycle_prove_criteria', arguments: { task_number: taskNumber, agent_id: `${siteId}.builder` } });
  const finished = await full(await task.client.request(6, 'tools/call', { name: 'task_lifecycle_finish', arguments: { task_number: taskNumber, agent_id: `${siteId}.builder`, summary: 'Clean-room work complete.', no_files_changed: true, reviewer: `${siteId}.architect` } }), task.client, 600);
  const reviewNumber = Number(finished.review_dependency?.required_task_number); assert.ok(reviewNumber > 0, JSON.stringify(finished));
  reviewer = spawnJsonlMcpServer(process.execPath, [taskServer, '--site-root', workspace], { cwd: workspace, env: siteFabricChildEnv(workspace, { NARADA_AGENT_ID: `${siteId}.architect`, NARADA_SITE_ID: siteId }), label: 'clean-room reviewer' });
  assert.equal(body(await reviewer.client.request(7, 'tools/call', { name: 'task_lifecycle_claim', arguments: { task_number: reviewNumber, agent_id: `${siteId}.architect` } })).status, 'claimed');
  await reviewer.client.request(8, 'tools/call', { name: 'task_lifecycle_finish', arguments: { task_number: reviewNumber, agent_id: `${siteId}.architect`, summary: 'Accepted clean-room evidence.', outcome: 'accepted', findings: [], no_files_changed: true } });
  await task.client.request(9, 'tools/call', { name: 'task_lifecycle_admit_evidence', arguments: { task_number: taskNumber, agent_id: `${siteId}.builder` } });
  const closed = await full(await task.client.request(10, 'tools/call', { name: 'task_lifecycle_close', arguments: { task_number: taskNumber, agent_id: `${siteId}.builder`, mode: 'peer_reviewed' } }), task.client, 900);
  assert.equal(closed.new_status, 'closed', JSON.stringify(closed));

  feedback = spawnJsonlMcpServer(process.execPath, [feedbackServer, '--feedback-root', join(siteRoot, '.ai', 'feedback'), '--canonical-feedback-root', join(siteRoot, '.ai', 'feedback'), '--task-lifecycle-root', workspace, '--site-id', siteId, '--owned-surface-id', 'surface-feedback'], { cwd: workspace, env: siteFabricChildEnv(workspace, { NARADA_AGENT_ID: `${siteId}.architect`, NARADA_SITE_ROOT: workspace }), label: 'clean-room feedback' });
  const submitted = body(await feedback.client.request(11, 'tools/call', { name: 'surface_feedback_submit', arguments: { surface_id: 'surface-feedback', submitter_site_id: siteId, submitter_principal: `${siteId}.architect`, kind: 'observation', summary: 'Clean-room feedback route', details: 'Prove feedback converts into governed work on the same fresh Site.' } }));
  const converted = body(await feedback.client.request(12, 'tools/call', { name: 'surface_feedback_convert_to_task', arguments: { feedback_id: submitted.feedback_id } }));
  assert.equal(converted.status, 'converted');

  const dbPath = join(workspace, '.ai', 'state', 'agent-context.sqlite'); mkdirSync(join(workspace, '.ai', 'state'), { recursive: true });
  const db = new DatabaseSync(dbPath); db.exec('CREATE TABLE IF NOT EXISTS agent_start_events (event_id TEXT PRIMARY KEY, identity_id TEXT NOT NULL, runtime TEXT NOT NULL, created_at TEXT NOT NULL, status TEXT NOT NULL, resume_command TEXT, bootstrap_artifact_uri TEXT); CREATE TABLE IF NOT EXISTS agent_events (event_id TEXT PRIMARY KEY, identity_id TEXT NOT NULL, event_kind TEXT NOT NULL, created_at TEXT NOT NULL, payload_json TEXT NOT NULL DEFAULT "{}");'); db.close();
  const carrierSessionId = 'carrier_clean_room';
  const admission: any = { schema: CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA, receipt_id: 'receipt:clean-room', decision: 'admitted', state: 'starting', coordinate: { authority_scope: 'test', site_ref: `site:${siteId}`, carrier_session_id: carrierSessionId, authority_epoch: 1 }, agent_identity: { source_authority_ref: `agent-identity:${siteId}`, artifact_ref: `agent:${siteId}.builder`, revision: '1', local_agent_id: `${siteId}.builder`, canonical_agent_id: `${siteId}.builder` }, carrier_kind: 'codex', admission_policy: { source_authority_ref: `site-policy:${siteId}`, artifact_ref: 'carrier-admission:clean-room', revision: '1' }, issued_at: new Date().toISOString(), valid_until: null, authority_readback_ref: `carrier-session-authority:${carrierSessionId}`, evidence_refs: ['e2e:clean-room'], reason_codes: [] };
  const started: any = materializeAgentSessionStart({ siteRoot: workspace, siteId, identity: `${siteId}.builder`, runtime: 'codex', dbPath, admissionReceipt: admission, generatedAt: new Date().toISOString() });
  const delivery: any = issueCarrierSessionOrientationDeliveryReceipt({ admissionReceipt: admission, brief: started.orientation_brief, deliveredAt: new Date().toISOString() });
  recordOrientationDeliveryReceipt({ siteRoot: workspace, dbPath, admissionReceipt: admission, brief: started.orientation_brief, deliveryReceipt: delivery });
  const contextEnv = siteFabricChildEnv(workspace, { NARADA_AGENT_ID: `${siteId}.builder`, NARADA_CARRIER_SESSION_ID: carrierSessionId, NARADA_CARRIER_SESSION_ADMISSION_RECEIPT: JSON.stringify(admission), NARADA_ORIENTATION_MANIFEST_ID: started.orientation_manifest.manifest_id, NARADA_ORIENTATION_DELIVERY_RECEIPT: JSON.stringify(delivery), NARADA_SITE_ROOT: workspace, NARADA_AGENT_CONTEXT_DB: dbPath });
  context = spawnJsonlMcpServer(process.execPath, [contextServer, '--site-root', workspace, '--site-id', siteId, '--tool-projection', 'admin'], { cwd: workspace, env: contextEnv, label: 'clean-room context first process' });
  const startup = await full(await context.client.request(13, 'tools/call', { name: 'agent_context_startup_sequence', arguments: {} }), context.client, 1300);
  assert.equal(startup.status, 'orientation_required');
  const checkpoint = body(await context.client.request(14, 'tools/call', { name: 'agent_context_checkpoint', arguments: { agent_id: `${siteId}.builder`, key_decisions: ['Fresh Site journey reached reviewed work.'], authority_basis: { kind: 'clean-room-e2e', summary: 'Controlled restart continuity.' }, next_intended_action: { kind: 'resume', summary: 'Rehydrate after process replacement.' } } }));
  assert.equal(checkpoint.status, 'checkpointed');
  await context.close(); context = null;
  context = spawnJsonlMcpServer(process.execPath, [contextServer, '--site-root', workspace, '--site-id', siteId, '--tool-projection', 'admin'], { cwd: workspace, env: contextEnv, label: 'clean-room context resumed process' });
  const rehydrated = await full(await context.client.request(15, 'tools/call', { name: 'agent_context_rehydrate', arguments: { agent_id: `${siteId}.builder`, checkpoint_id: checkpoint.checkpoint_id } }), context.client, 1500);
  assert.equal(rehydrated.checkpoint_id, checkpoint.checkpoint_id);
  console.log(JSON.stringify({ status: 'passed', site_id: siteId, task_number: taskNumber, feedback_task_number: converted.task_number, checkpoint_id: checkpoint.checkpoint_id }));
} finally {
  if (context) await context.close(); if (feedback) await feedback.close(); if (reviewer) await reviewer.close(); if (task) await task.close(); if (!externallyManagedRoot) rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
}
