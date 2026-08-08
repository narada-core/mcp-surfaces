import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const extension = process.platform === 'win32' ? '.exe' : '';
const executable = (name) => join(root, 'dist', 'native', `${name}${extension}`);
const run = (name, args, input = '') => {
  const result = spawnSync(executable(name), args, { input, encoding: 'utf8', windowsHide: true });
  assert.equal(result.status, 0, `${name}: ${result.stderr}`);
  return result.stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
};
const request = (id, name, args = {}) => JSON.stringify({ jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: args } });
const rpc = (id, method, params = {}) => JSON.stringify({ jsonrpc: '2.0', id, method, params });
const rootTask = mkdtempSync(join(tmpdir(), 'narada-native-task-'));
const rootWork = mkdtempSync(join(tmpdir(), 'narada-native-work-'));
try {
  run('narada-task-lifecycle-mcp', ['--prepare', '--site-root', rootTask]);
  const outputDir = join(rootTask, '.ai', 'tmp', 'mcp-outputs', 'workspace');
  mkdirSync(outputDir, { recursive: true });
  const fullOutput = { status: 'ok', value: 1 };
  const outputText = JSON.stringify(fullOutput, null, 2);
  const outputRecord = {
    schema: 'narada.mcp_output_ref.v1',
    ref: 'mcp_output:o_smoke123',
    output_id: 'o_smoke123',
    tool_name: 'native_smoke',
    truncated: false,
    full_output_char_length: outputText.length,
    sha256: createHash('sha256').update(JSON.stringify(fullOutput)).digest('hex'),
    full_output: fullOutput,
  };
  writeFileSync(join(outputDir, 'o_smoke123.json'), `${JSON.stringify(outputRecord)}\n`, 'utf8');
  const taskLines = run('narada-task-lifecycle-mcp', ['--site-root', rootTask], [
    rpc(1, 'initialize', {}),
    rpc(2, 'tools/list', {}),
    request(3, 'mcp_payload_create', { payload_id: 'smoketask', payload: { title: 'Native smoke task', goal: 'Exercise native authority', required_work: ['Persist state'], acceptance_criteria: ['State is durable'], idempotency_key: 'native-smoke-task' }, created_by: 'native.smoke' }),
    request(4, 'task_lifecycle_create', { payload_ref: 'mcp_payload:smoketask@v1' }),
    request(5, 'task_lifecycle_claim', { task_number: 1, agent_id: 'native.smoke' }),
    request(6, 'task_lifecycle_finish', { task_number: 1, agent_id: 'native.smoke', summary: 'Native smoke finish', no_files_changed: true }),
    request(7, 'task_lifecycle_admit_evidence', { task_number: 1, agent_id: 'native.smoke' }),
    request(8, 'task_lifecycle_close', { task_number: 1, agent_id: 'native.smoke', mode: 'operator_direct' }),
    request(9, 'task_lifecycle_show', { task_number: 1 }),
    rpc(10, 'resources/list', {}),
    rpc(11, 'resources/read', { uri: 'mcp-output:mcp_output%3Ao_smoke123' }),
    request(12, 'mcp_payload_create', { payload_id: 'smokepayload', payload: { summary: 'one', nested: { a: 1 } }, created_by: 'native.smoke' }),
    request(13, 'mcp_payload_derive', { source_ref: 'mcp_payload:smokepayload@v1', overlay: { nested: { b: 2 } }, delete_paths: ['/summary'], created_by: 'native.smoke' }),
    request(14, 'mcp_payload_validate', { ref: 'mcp_payload:smokepayload@v2' }),
  ].join('\n') + '\n');
  assert.equal(taskLines[0].result.serverInfo.name, 'narada-task-lifecycle-mcp');
  assert.equal(taskLines[1].result.tools.length, 69);
  assert.equal(taskLines[2].result.structuredContent.status, 'created');
  assert.equal(taskLines[3].result.structuredContent.status, 'created');
  assert.equal(taskLines[4].result.structuredContent.status, 'claimed');
  assert.equal(taskLines[6].result.structuredContent.status, 'admitted');
  assert.equal(taskLines[7].result.structuredContent.new_status, 'closed');
  assert.equal(taskLines[8].result.structuredContent.lifecycle.status, 'closed');
  assert.equal(taskLines[9].result.resources.length, 1);
  assert.match(taskLines[10].result.contents[0].text, /narada\.mcp_output_page\.v1/);
  assert.equal(taskLines[11].result.structuredContent.status, 'created');
  assert.equal(taskLines[12].result.structuredContent.status, 'derived');
  assert.equal(taskLines[13].result.structuredContent.status, 'valid');
  run('narada-work-lifecycle-mcp', ['--prepare', '--site-root', rootWork]);
  const workLines = run('narada-work-lifecycle-mcp', ['--site-root', rootWork], [
    rpc(1, 'initialize', {}),
    rpc(2, 'tools/list', {}),
    request(3, 'ticket_admit_source', { source_kind: 'smoke', source_scope: 'native', immutable_source_id: 'one', idempotency_key: 'native-smoke-ticket', causation_id: 'native-smoke', policy_version: 'v1', summary: 'Native smoke ticket', source_ref: {}, correlation_keys: [] }),
    request(4, 'ticket_admit_proposal', { ticket_id: 'ticket-1', expected_revision: 1, route: 'followup_task', idempotency_key: 'native-smoke-proposal', causation_id: 'native-smoke', actor_id: 'native.smoke', summary: 'Create follow-up task', task: { title: 'Native follow-up', goal: 'Exercise shared task authority', required_work: 'Persist the follow-up', acceptance_criteria: ['Task exists'] } }),
    request(5, 'ticket_admit_proposal', { ticket_id: 'ticket-1', expected_revision: 1, route: 'blocked_operator', idempotency_key: 'native-smoke-stale', causation_id: 'native-smoke', actor_id: 'native.smoke', summary: 'Must refuse stale revision' }),
    request(6, 'work_outbox_consumer_register', { topic: 'work.ticket-work-due.v1', consumer_id: 'native.smoke' }),
    request(7, 'work_outbox_list', { consumer_id: 'native.smoke', limit: 10 }),
    request(8, 'work_lifecycle_storage_inspect', {}),
  ].join('\n') + '\n');
  assert.equal(workLines[0].result.serverInfo.name, 'work-lifecycle-mcp');
  assert.equal(workLines[1].result.tools.length, 80);
  assert.equal(workLines[2].result.structuredContent.result.status, 'created');
  assert.equal(workLines[3].result.structuredContent.result.status, 'admitted');
  assert.equal(workLines[4].error.data.code, 'ticket_revision_conflict');
  assert.equal(workLines[5].result.structuredContent.status, 'registered');
  assert.ok(workLines[6].result.structuredContent.count >= 1);
  assert.equal(workLines[7].result.structuredContent.status, 'ok');
  const eventId = workLines[6].result.structuredContent.events[0].event_id;
  const ackLines = run('narada-work-lifecycle-mcp', ['--site-root', rootWork], [
    request(9, 'work_outbox_ack', { event_id: eventId, consumer_id: 'native.smoke', receipt: { status: 'processed', observer: 'native.smoke' } }),
    request(10, 'work_outbox_compact', { before: '2999-01-01T00:00:00Z' }),
  ].join('\n') + '\n');
  assert.equal(ackLines[0].result.structuredContent.status, 'acknowledged');
  assert.ok(Number.isInteger(ackLines[1].result.structuredContent.compacted));
  process.stdout.write(JSON.stringify({ schema: 'narada.mcp_lifecycle_native.smoke.v1', status: 'passed', task_tools: 69, work_tools: 80, resources: 1, payload_revisions: 2, work_transaction_checks: 3 }) + '\n');
} finally {
  rmSync(rootTask, { recursive: true, force: true });
  rmSync(rootWork, { recursive: true, force: true });
}