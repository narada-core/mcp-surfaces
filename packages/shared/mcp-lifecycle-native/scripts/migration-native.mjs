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
const sourceRoot = mkdtempSync(join(tmpdir(), 'narada-native-legacy-'));
const targetRoot = mkdtempSync(join(tmpdir(), 'narada-native-migrated-'));
try {
  run('narada-task-lifecycle-mcp', ['--prepare', '--site-root', sourceRoot]);
  const sourceDatabase = join(sourceRoot, '.ai', 'task-lifecycle.db');
  const migration = run('narada-work-lifecycle-mcp', ['--migrate-legacy', '--source-database-path', sourceDatabase, '--site-root', targetRoot]);
  assert.equal(migration[0].status, 'migrated');
  const runtime = run('narada-work-lifecycle-mcp', ['--site-root', targetRoot], [
    JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} }),
    JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'ticket_list', arguments: {} } }),
  ].join('\n') + '\n');
  assert.equal(runtime[0].result.serverInfo.name, 'work-lifecycle-mcp');
  assert.equal(runtime[1].result.structuredContent.count, 0);
  process.stdout.write(JSON.stringify({ schema: 'narada.mcp_lifecycle_native.migration.v1', status: 'passed', source_schema: 'task-lifecycle', target_schema: 'work-lifecycle' }) + '\n');
} finally {
  rmSync(sourceRoot, { recursive: true, force: true });
  rmSync(targetRoot, { recursive: true, force: true });
}