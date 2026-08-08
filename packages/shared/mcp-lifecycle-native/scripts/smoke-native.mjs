import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
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
const rootTask = mkdtempSync(join(tmpdir(), 'narada-native-task-'));
const rootWork = mkdtempSync(join(tmpdir(), 'narada-native-work-'));
try {
  run('narada-task-lifecycle-mcp', ['--prepare', '--site-root', rootTask]);
  const taskLines = run('narada-task-lifecycle-mcp', ['--site-root', rootTask], [
    JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} }),
    JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} }),
    request(3, 'task_lifecycle_create', { title: 'Native smoke task', goal: 'Exercise native authority', required_work: ['Persist state'], acceptance_criteria: ['State is durable'], idempotency_key: 'native-smoke-task' }),
    request(4, 'task_lifecycle_claim', { task_number: 1, agent_id: 'native.smoke' }),
    request(5, 'task_lifecycle_finish', { task_number: 1, agent_id: 'native.smoke', summary: 'Native smoke finish', no_files_changed: true }),
    request(6, 'task_lifecycle_close', { task_number: 1, agent_id: 'native.smoke', mode: 'operator_direct' }),
    request(7, 'task_lifecycle_show', { task_number: 1 }),
  ].join('\n') + '\n');
  assert.equal(taskLines[0].result.serverInfo.name, 'narada-task-lifecycle-mcp');
  assert.equal(taskLines[1].result.tools.length, 69);
  assert.equal(taskLines[2].result.structuredContent.status, 'created');
  assert.equal(taskLines[3].result.structuredContent.status, 'claimed');
  assert.equal(taskLines[5].result.structuredContent.new_status, 'closed');
  assert.equal(taskLines[6].result.structuredContent.lifecycle.status, 'closed');

  run('narada-work-lifecycle-mcp', ['--prepare', '--site-root', rootWork]);
  const workLines = run('narada-work-lifecycle-mcp', ['--site-root', rootWork], [
    JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} }),
    JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} }),
    request(3, 'ticket_admit_source', { source_kind: 'smoke', source_scope: 'native', immutable_source_id: 'one', idempotency_key: 'native-smoke-ticket', causation_id: 'native-smoke', policy_version: 'v1', summary: 'Native smoke ticket', source_ref: {}, correlation_keys: [] }),
    request(4, 'ticket_list'),
    request(5, 'work_lifecycle_doctor'),
  ].join('\n') + '\n');
  assert.equal(workLines[0].result.serverInfo.name, 'work-lifecycle-mcp');
  assert.equal(workLines[1].result.tools.length, 80);
  assert.equal(workLines[2].result.structuredContent.result.status, 'created');
  assert.equal(workLines[3].result.structuredContent.count, 1);
  assert.equal(workLines[4].result.structuredContent.status, 'ok');
  process.stdout.write(JSON.stringify({ schema: 'narada.mcp_lifecycle_native.smoke.v1', status: 'passed', task_tools: 69, work_tools: 80 }) + '\n');
} finally {
  rmSync(rootTask, { recursive: true, force: true });
  rmSync(rootWork, { recursive: true, force: true });
}
