import assert from 'node:assert/strict';
import { requireNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { parseRpcLines, toolCall } from './script-support.js';

const root = fileURLToPath(new URL('..', import.meta.url));
const extension = process.platform === 'win32' ? '.exe' : '';
const executable = (name: string): string => requireNativeArtifact(root, `${name}${extension}`);
const run = (
  name: string,
  args: readonly string[],
  input = '',
  env: NodeJS.ProcessEnv = process.env,
) => spawnSync(executable(name), [...args], { input, encoding: 'utf8', windowsHide: true, env });

const missingRoot = mkdtempSync(join(tmpdir(), 'narada-native-missing-'));
const rootTask = mkdtempSync(join(tmpdir(), 'narada-native-refusal-task-'));
const rootWork = mkdtempSync(join(tmpdir(), 'narada-native-refusal-work-'));
try {
  const missing = run(
    'narada-task-lifecycle-mcp',
    ['--site-root', missingRoot],
    `${toolCall(99, 'task_lifecycle_create', { payload_ref: 'mcp_payload:missing@v1' })}\n`,
  );
  assert.equal(missing.status, 0);
  const missingLine = parseRpcLines(String(missing.stdout), 'missing-store refusal')[0];
  assert.equal(missingLine?.error?.code, -32000);
  assert.match(String(missingLine?.error?.message), /task_lifecycle_store_not_prepared/);

  assert.equal(run('narada-task-lifecycle-mcp', ['--prepare', '--site-root', rootTask]).status, 0);
  const mismatch = run(
    'narada-task-lifecycle-mcp',
    ['--site-root', rootTask],
    `${toolCall(0, 'task_lifecycle_create', { payload_ref: 'mcp_payload:refused@v1' })}\n`,
    {
      ...process.env,
      NARADA_TARGET_SITE_ROOT: join(tmpdir(), 'narada-native-wrong-locus'),
    },
  );
  const mismatchLine = parseRpcLines(String(mismatch.stdout), 'target-locus refusal')[0];
  assert.equal(mismatch.status, 0);
  assert.equal(mismatchLine?.result?.structuredContent?.status, 'refused');
  assert.equal(mismatchLine?.result?.structuredContent?.refusal_code, 'target_locus_preflight_required');
  assert.equal(mismatchLine?.result?.isError, true);

  const task = run('narada-task-lifecycle-mcp', ['--site-root', rootTask], `${[
    toolCall(1, 'mcp_payload_create', {
      payload_id: 'refusalpayload',
      payload: {
        title: 'Refusal task',
        goal: 'Check refusal',
        required_work: ['none'],
        acceptance_criteria: ['none'],
        idempotency_key: 'refusal-task',
      },
    }),
    toolCall(2, 'task_lifecycle_create', { payload_ref: 'mcp_payload:refusalpayload@v1' }),
    toolCall(3, 'task_lifecycle_claim', { task_number: 1, agent_id: 'native.refusal' }),
    toolCall(4, 'task_lifecycle_finish', {
      task_number: 1,
      agent_id: 'native.refusal',
      summary: 'finish without evidence',
      no_files_changed: true,
    }),
    toolCall(5, 'task_lifecycle_close', {
      task_number: 1,
      agent_id: 'native.refusal',
      mode: 'operator_direct',
    }),
    toolCall(6, 'task_lifecycle_test_mcp_tool', { server_path: 'missing.ts', tool_name: 'ping' }),
    JSON.stringify({ jsonrpc: '2.0', id: 7, method: 'resources/read', params: { uri: 'mcp-output:bad%ZZ' } }),
  ].join('\n')}\n`);
  const taskLines = parseRpcLines(String(task.stdout), 'task refusal');
  assert.equal(task.status, 0);
  assert.equal(taskLines[4]?.result?.structuredContent?.status, 'blocked');
  assert.deepEqual(taskLines[4]?.result?.structuredContent?.close_blockers, ['evidence_admission_required']);
  assert.equal(taskLines[5]?.result?.structuredContent?.status, 'refused');
  assert.equal(taskLines[6]?.error?.data?.code, 'output_resource_uri_invalid');

  assert.equal(run('narada-work-lifecycle-mcp', ['--prepare', '--site-root', rootWork]).status, 0);
  const work = run(
    'narada-work-lifecycle-mcp',
    ['--site-root', rootWork],
    `${JSON.stringify({ jsonrpc: '2.0', id: 7, method: 'resources/list', params: {} })}\n`,
  );
  const workLine = parseRpcLines(String(work.stdout), 'work refusal')[0];
  assert.equal(workLine?.error?.data?.code, 'unsupported_mcp_method');
  process.stdout.write(`${JSON.stringify({
    schema: 'narada.mcp_lifecycle_native.refusal.v1',
    status: 'passed',
    checks: 9,
  })}\n`);
} finally {
  rmSync(missingRoot, { recursive: true, force: true });
  rmSync(rootTask, { recursive: true, force: true });
  rmSync(rootWork, { recursive: true, force: true });
}
