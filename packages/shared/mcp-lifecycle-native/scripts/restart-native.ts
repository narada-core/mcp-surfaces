import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { parseRpcLines, type JsonObject, type RpcId, type RpcRecord } from './script-support.js';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const extension = process.platform === 'win32' ? '.exe' : '';
const executable = join(packageRoot, 'dist', 'native', `narada-task-lifecycle-mcp${extension}`);

function run(root: string, requests: readonly JsonObject[]): RpcRecord[] {
  const input = `${requests.map((request) => JSON.stringify(request)).join('\n')}\n`;
  const result = spawnSync(executable, ['--site-root', root], { input, encoding: 'utf8', windowsHide: true });
  assert.equal(result.status, 0, result.stderr || 'native restart process failed');
  return parseRpcLines(String(result.stdout), 'native restart process');
}

const root = mkdtempSync(join(tmpdir(), 'narada-native-restart-'));
try {
  const prepare = spawnSync(executable, ['--prepare', '--site-root', root], { encoding: 'utf8', windowsHide: true });
  assert.equal(prepare.status, 0, prepare.stderr || 'native restart preparation failed');
  const call = (id: RpcId, mode: string): JsonObject => ({
    jsonrpc: '2.0',
    id,
    method: 'tools/call',
    params: { name: 'task_lifecycle_restart', arguments: { mode } },
  });

  const sameProcess = run(root, [call(1, 'request'), call(2, 'acknowledge')]);
  assert.equal(sameProcess[0]?.result?.structuredContent?.status, 'restart_requested');
  assert.equal(sameProcess[1]?.result?.structuredContent?.status, 'restart_acknowledgement_rejected');
  assert.equal(sameProcess[1]?.result?.structuredContent?.validation?.reason, 'post_request_boot_evidence_missing');

  const freshProcess = run(root, [call(3, 'acknowledge')]);
  assert.equal(freshProcess[0]?.result?.structuredContent?.status, 'restart_acknowledged');
  const requestPath = freshProcess[0]?.result?.structuredContent?.request_path;
  assert.ok(typeof requestPath === 'string');
  assert.equal(existsSync(requestPath), false);
  console.log(JSON.stringify({
    schema: 'narada.mcp_lifecycle_native.restart.v1',
    status: 'passed',
    checks: ['same_process_refused', 'fresh_process_acknowledged', 'marker_cleared'],
  }));
} finally {
  rmSync(root, { recursive: true, force: true });
}
