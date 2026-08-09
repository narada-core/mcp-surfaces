import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  asRpcRecord,
  byId,
  rpc,
  structured,
  toolCall,
  type RpcRecord,
} from './script-support.js';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const surfacesRoot = resolve(packageRoot, '..', '..', '..');
const extension = process.platform === 'win32' ? '.exe' : '';
const nodeEntrypoint = join(
  surfacesRoot,
  'packages',
  'task-lifecycle-mcp',
  'dist',
  'src',
  'task-lifecycle',
  'task-mcp-server.js',
);
const rustEntrypoint = join(packageRoot, 'dist', 'native', `narada-task-lifecycle-mcp${extension}`);
const requests = `${[
  rpc(1, 'initialize'),
  rpc(2, 'tools/list'),
  toolCall(3, 'task_lifecycle_guidance'),
  toolCall(4, 'task_lifecycle_payload_schema'),
  toolCall(5, 'task_lifecycle_restart', { mode: 'status' }),
  toolCall(6, 'task_lifecycle_chapter_show', { chapter_id: 'missing' }),
  toolCall(7, 'mcp_payload_show', { ref: 'mcp_payload:missing@v1' }),
  toolCall(8, 'mcp_output_show', { ref: 'mcp_output:missing' }),
  toolCall(9, '__missing_task_tool__'),
  rpc(10, 'resources/list'),
  rpc(11, 'prompts/list'),
  rpc(12, 'completion/complete', { argument: { name: 'name' } }),
].join('\n')}\n`;

function run(command: string, args: readonly string[], root: string): Map<string, RpcRecord> {
  const result = spawnSync(command, [...args, '--site-root', root], {
    cwd: surfacesRoot,
    input: requests,
    encoding: 'utf8',
    windowsHide: true,
  });
  assert.equal(result.status, 0, result.stderr || `${command} exited non-zero`);
  const lines = String(result.stdout).trim().split(/\r?\n/).filter(Boolean);
  return byId(lines.map((line, index) => asRpcRecord(
    JSON.parse(line) as unknown,
    `not-ready response ${index + 1}`,
  )));
}

function normalize(value: unknown, root: string): unknown {
  if (typeof value === 'string') {
    const normalized = value.replaceAll('\\', '/');
    const rootNormalized = root.replaceAll('\\', '/');
    let result = normalized;
    if (process.platform === 'win32') {
      const lowerValue = normalized.toLowerCase();
      const lowerRoot = rootNormalized.toLowerCase();
      const index = lowerValue.indexOf(lowerRoot);
      result = index >= 0
        ? `${normalized.slice(0, index)}<site-root>${normalized.slice(index + rootNormalized.length)}`
        : normalized.replaceAll(rootNormalized, '<site-root>');
    } else {
      result = normalized.replaceAll(rootNormalized, '<site-root>');
    }
    return result.replace(/o_[0-9a-f]{24}/gi, '<output-id>');
  }
  if (Array.isArray(value)) return value.map((item) => normalize(item, root));
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .map(([key, item]) => [key, normalize(item, root)]),
    );
  }
  return value;
}

function fullMaterializedResult(response: RpcRecord | undefined, root: string): unknown {
  const page = response?.result?.structuredContent;
  const reference = page?.output_ref;
  if (typeof reference !== 'string') return normalize(page, root);
  const id = reference.slice('mcp_output:'.length);
  const path = join(root, '.ai', 'tmp', 'mcp-outputs', 'workspace', `${id}.json`);
  assert.equal(existsSync(path), true, `materialized output missing: ${path}`);
  const record = asRpcRecord(JSON.parse(readFileSync(path, 'utf8')) as unknown, 'materialized output');
  return normalize(record.full_output, root);
}

function errorSignature(response: RpcRecord | undefined, root: string): unknown {
  return normalize({
    code: response?.error?.code ?? null,
    message: response?.error?.message ?? null,
    data: response?.error?.data ?? null,
  }, root);
}

const nodeRoot = mkdtempSync(join(tmpdir(), 'narada-not-ready-node-'));
const rustRoot = mkdtempSync(join(tmpdir(), 'narada-not-ready-rust-'));
try {
  const node = run(process.execPath, [nodeEntrypoint], nodeRoot);
  const rust = run(rustEntrypoint, [], rustRoot);
  assert.deepEqual(rust.get('1')?.result, node.get('1')?.result, 'initialize parity drifted');
  assert.deepEqual(rust.get('2')?.result, node.get('2')?.result, 'tools/list parity drifted');
  assert.deepEqual(
    fullMaterializedResult(node.get('3'), nodeRoot),
    fullMaterializedResult(rust.get('3'), rustRoot),
    'guidance parity drifted',
  );
  assert.deepEqual(
    fullMaterializedResult(node.get('4'), nodeRoot),
    fullMaterializedResult(rust.get('4'), rustRoot),
    'payload-schema parity drifted',
  );

  const stableRestart = (response: RpcRecord | undefined) => {
    const value = structured(response);
    return {
      status: value.status,
      schema: value.schema,
      can_self_restart: value.can_self_restart,
      restart_mechanism: value.restart_mechanism,
      request: value.request,
    };
  };
  assert.deepEqual(stableRestart(rust.get('5')), stableRestart(node.get('5')), 'restart status parity drifted');

  const stableChapter = (response: RpcRecord | undefined) => {
    const value = structured(response);
    return {
      schema: value.schema,
      status: value.status,
      chapter_id: value.chapter_id,
      membership_count: value.membership_count,
      memberships: value.memberships,
    };
  };
  assert.deepEqual(
    stableChapter(rust.get('6')),
    stableChapter(node.get('6')),
    'chapter startup-safe parity drifted',
  );

  for (const id of ['7', '8', '9']) {
    assert.deepEqual(
      errorSignature(rust.get(id), rustRoot),
      errorSignature(node.get(id), nodeRoot),
      `startup error parity drifted for ${id}`,
    );
  }
  assert.deepEqual(
    normalize(rust.get('10')?.result, rustRoot),
    normalize(node.get('10')?.result, nodeRoot),
    'resources/list startup parity drifted',
  );
  assert.deepEqual(rust.get('11')?.result, node.get('11')?.result, 'prompts/list startup parity drifted');
  assert.deepEqual(rust.get('12')?.result, node.get('12')?.result, 'completion startup parity drifted');
  process.stdout.write(`${JSON.stringify({
    schema: 'narada.mcp_lifecycle_native.not_ready.v1',
    status: 'passed',
    checks: [
      'stdio_live_without_database',
      'guidance',
      'payload_schema',
      'restart',
      'chapter',
      'errors',
      'resources',
      'prompts',
      'completion',
    ],
  })}\n`);
} finally {
  rmSync(nodeRoot, { recursive: true, force: true });
  rmSync(rustRoot, { recursive: true, force: true });
}
