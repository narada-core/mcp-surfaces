import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const extension = process.platform === 'win32' ? '.exe' : '';
const executable = (name) => join(root, 'dist', 'native', `${name}${extension}`);
const rpc = (id, name, argumentsValue = {}) => JSON.stringify({ jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: argumentsValue } });
const run = (name, args, input = '') => {
  const result = spawnSync(executable(name), args, { input, encoding: 'utf8', windowsHide: true });
  assert.equal(result.status, 0, `${name}: ${result.stderr}`);
  return result.stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
};
const siteRoot = mkdtempSync(join(tmpdir(), 'narada-native-legacy-'));
try {
  run('narada-task-lifecycle-mcp', ['--prepare', '--site-root', siteRoot]);
  const sourceDatabase = join(siteRoot, '.ai', 'task-lifecycle.db');
  const payload = run('narada-task-lifecycle-mcp', ['--site-root', siteRoot], rpc(1, 'mcp_payload_create', {
    payload_id: 'legacy-task',
    payload: {
      title: 'Legacy migration task',
      goal: 'Prove existing state survives the work migration',
      required_work: ['Read and preserve state'],
      acceptance_criteria: ['Markdown remains readable'],
      idempotency_key: 'legacy-migration-task',
    },
    created_by: 'native.migration',
  }) + '\n');
  assert.equal(payload[0].result.structuredContent.status, 'created');
  const created = run('narada-task-lifecycle-mcp', ['--site-root', siteRoot], rpc(2, 'task_lifecycle_create', { payload_ref: 'mcp_payload:legacy-task@v1' }) + '\n');
  assert.equal(created[0].result.structuredContent.status, 'created');
  const claimed = run('narada-task-lifecycle-mcp', ['--site-root', siteRoot], rpc(3, 'task_lifecycle_claim', { task_number: 1, agent_id: 'native.migration' }) + '\n');
  assert.equal(claimed[0].result.structuredContent.status, 'claimed');

  // The real migration shape is one Site: copy the legacy task DB to its work DB in place,
  // leaving the existing Markdown evidence beside it.
  const migration = run('narada-work-lifecycle-mcp', ['--migrate-legacy', '--source-database-path', sourceDatabase, '--site-root', siteRoot]);
  assert.equal(migration[0].status, 'migrated');
  const runtime = run('narada-work-lifecycle-mcp', ['--site-root', siteRoot], [
    JSON.stringify({ jsonrpc: '2.0', id: 4, method: 'initialize', params: {} }),
    rpc(5, 'ticket_list'),
    rpc(6, 'task_lifecycle_show', { task_number: 1 }),
  ].join('\n') + '\n');
  assert.equal(runtime[0].result.serverInfo.name, 'work-lifecycle-mcp');
  assert.equal(runtime[1].result.structuredContent.count, 0);
  assert.equal(runtime[2].result.structuredContent.status, 'ok');
  assert.match(runtime[2].result.structuredContent.body, /Legacy migration task/);
  const taskFiles = readdirSync(join(siteRoot, '.ai', 'do-not-open', 'tasks')).filter((name) => name.endsWith('.md'));
  assert.equal(taskFiles.length, 1);
  assert.match(readFileSync(join(siteRoot, '.ai', 'do-not-open', 'tasks', taskFiles[0]), 'utf8'), /status: claimed/);
  process.stdout.write(JSON.stringify({ schema: 'narada.mcp_lifecycle_native.migration.v1', status: 'passed', source_schema: 'task-lifecycle', target_schema: 'work-lifecycle', checks: ['populated_sqlite', 'markdown_readback', 'same_site_migration'] }) + '\n');
} finally {
  rmSync(siteRoot, { recursive: true, force: true });
}