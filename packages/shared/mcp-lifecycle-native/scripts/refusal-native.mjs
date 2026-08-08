import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const extension = process.platform === 'win32' ? '.exe' : '';
const executable = (name) => join(root, 'dist', 'native', `${name}${extension}`);
const rpc = (id, name, argumentsValue = {}) => JSON.stringify({ jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: argumentsValue } });
const run = (name, args, input = '') => spawnSync(executable(name), args, { input, encoding: 'utf8', windowsHide: true });
const missingRoot = mkdtempSync(join(tmpdir(), 'narada-native-missing-'));
const rootTask = mkdtempSync(join(tmpdir(), 'narada-native-refusal-task-'));
const rootWork = mkdtempSync(join(tmpdir(), 'narada-native-refusal-work-'));
try {
  const missing = run('narada-task-lifecycle-mcp', ['--site-root', missingRoot]);
  assert.notEqual(missing.status, 0);
  assert.match(String(missing.stderr), /task_lifecycle_store_not_prepared/);
  assert.equal(run('narada-task-lifecycle-mcp', ['--prepare', '--site-root', rootTask]).status, 0);
  const task = run('narada-task-lifecycle-mcp', ['--site-root', rootTask], [
    rpc(1, 'mcp_payload_create', { payload_id: 'refusalpayload', payload: { title: 'Refusal task', goal: 'Check refusal', required_work: ['none'], acceptance_criteria: ['none'], idempotency_key: 'refusal-task' } }),
    rpc(2, 'task_lifecycle_create', { payload_ref: 'mcp_payload:refusalpayload@v1' }),
    rpc(3, 'task_lifecycle_claim', { task_number: 1, agent_id: 'native.refusal' }),
    rpc(4, 'task_lifecycle_finish', { task_number: 1, agent_id: 'native.refusal', summary: 'finish without evidence', no_files_changed: true }),
    rpc(5, 'task_lifecycle_close', { task_number: 1, agent_id: 'native.refusal', mode: 'operator_direct' }),
    rpc(6, 'task_lifecycle_test_mcp_tool', { server_path: 'missing.mjs', tool_name: 'ping' }),
    JSON.stringify({ jsonrpc: '2.0', id: 7, method: 'resources/read', params: { uri: 'mcp-output:bad%ZZ' } }),
  ].join('\n') + '\n');  const taskLines = String(task.stdout).trim().split(/\r?\n/).filter(Boolean).map(JSON.parse);
  assert.equal(task.status, 0);
  assert.equal(taskLines[4].result.structuredContent.status, 'blocked');
  assert.deepEqual(taskLines[4].result.structuredContent.close_blockers, ['evidence_admission_required']);
  assert.equal(taskLines[5].result.structuredContent.status, 'refused');
  assert.equal(taskLines[6].error.data.code, 'output_resource_uri_invalid');
  assert.equal(run('narada-work-lifecycle-mcp', ['--prepare', '--site-root', rootWork]).status, 0);
  const work = run('narada-work-lifecycle-mcp', ['--site-root', rootWork], [
    JSON.stringify({ jsonrpc: '2.0', id: 7, method: 'resources/list', params: {} }),
  ].join('\n') + '\n');
  const workLine = JSON.parse(String(work.stdout).trim());
  assert.equal(workLine.error.data.code, 'unsupported_mcp_method');
  process.stdout.write(JSON.stringify({ schema: 'narada.mcp_lifecycle_native.refusal.v1', status: 'passed', checks: 6 }) + '\n');
} finally {
  rmSync(missingRoot, { recursive: true, force: true });
  rmSync(rootTask, { recursive: true, force: true });
  rmSync(rootWork, { recursive: true, force: true });
}