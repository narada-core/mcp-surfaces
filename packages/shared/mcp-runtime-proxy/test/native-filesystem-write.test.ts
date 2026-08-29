import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { requireNativeArtifact } from '../src/native-artifact.js';

type JsonRecord = Record<string, any>;

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const executable = resolve(process.env.NARADA_NATIVE_FILESYSTEM_TEST_EXECUTABLE ?? requireNativeArtifact(packageRoot, 'narada-mcp-runtime.exe'));
const expectedServerName = process.env.NARADA_NATIVE_FILESYSTEM_TEST_VARIANT === 'rhai'
  ? 'local-filesystem-write-rhai'
  : 'local-filesystem-write-native';
const appletArgument = process.env.NARADA_NATIVE_FILESYSTEM_TEST_VARIANT === 'rhai'
  ? 'rhai-filesystem'
  : 'filesystem';

function run(mode: 'read' | 'write', root: string, requests: JsonRecord[], auditLogDir?: string, outputRoot?: string): Promise<JsonRecord[]> {
  return new Promise((resolvePromise, rejectPromise) => {
    const args = [appletArgument, '--mode', mode, '--allowed-root', root];
    if (auditLogDir) args.push('--audit-log-dir', auditLogDir);
    if (outputRoot) args.push('--output-root', outputRoot);
    const child = spawn(executable, args, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    const timer = setTimeout(() => { child.kill(); rejectPromise(new Error(`native_filesystem_write_timeout:${stderr}`)); }, 10_000);
    child.on('error', (error) => { clearTimeout(timer); rejectPromise(error); });
    child.on('close', (code) => {
      clearTimeout(timer);
      if (code !== 0) { rejectPromise(new Error(`native_filesystem_write_exit:${code}:${stderr}`)); return; }
      try { resolvePromise(stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))); }
      catch (error) { rejectPromise(new Error(`native_filesystem_write_invalid_output:${String(error)}:${stdout.slice(0, 1000)}`)); }
    });
    child.stdin.end(requests.map((request) => JSON.stringify(request)).join('\n') + '\n');
  });
}

const root = mkdtempSync(join(tmpdir(), 'narada-native-filesystem-write-'));
const extraRoot = mkdtempSync(join(tmpdir(), 'narada-native-filesystem-extra-'));
const auditLogDir = join(root, 'audit');
try {
  mkdirSync(join(root, '.narada'), { recursive: true });
  writeFileSync(join(root, '.narada', 'allowed-roots.json'), JSON.stringify({ extra_allowed_roots: [extraRoot] }), 'utf8');
  const responses = await run('write', root, [
    { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } },
    { jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} },
    { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'fs_doctor', arguments: {} } },
    { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'fs_write_file', arguments: { path: 'nested/note.txt', content: 'hello native\n' } } },
    { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'fs_read_file', arguments: { path: 'nested/note.txt', offset: 1, limit: 10 } } },
    { jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'fs_write_file', arguments: { path: 'nested/note.txt', content: 'changed\n', expected_sha256: 'deadbeef' } } },
    { jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name: 'fs_write_file', arguments: { path: '.ai/tmp/hook.js', content: 'console.log(1);' } } },
    { jsonrpc: '2.0', id: 8, method: 'tools/call', params: { name: 'fs_str_replace_file', arguments: { path: 'nested/note.txt', old: 'hello native', new: 'hello replaced' } } },
    { jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name: 'fs_replace_range', arguments: { path: 'nested/note.txt', start_line: 1, end_line: 1, replacement: 'range replaced' } } },
    { jsonrpc: '2.0', id: 10, method: 'tools/call', params: { name: 'fs_read_file', arguments: { path: 'nested/note.txt', offset: 1, limit: 10 } } },
    { jsonrpc: '2.0', id: 11, method: 'tools/call', params: { name: 'fs_create_directory', arguments: { path: 'nested/dir', recursive: true } } },
    { jsonrpc: '2.0', id: 12, method: 'tools/call', params: { name: 'fs_rename_directory', arguments: { from: 'nested/dir', to: 'nested/renamed' } } },
    { jsonrpc: '2.0', id: 13, method: 'tools/call', params: { name: 'fs_write_file', arguments: { path: 'nested/renamed/moved.txt', content: 'move me\n' } } },
    { jsonrpc: '2.0', id: 14, method: 'tools/call', params: { name: 'fs_move_path', arguments: { from: 'nested/renamed/moved.txt', to: 'nested/moved.txt' } } },
    { jsonrpc: '2.0', id: 15, method: 'tools/call', params: { name: 'fs_delete_directory', arguments: { path: 'nested/renamed' } } },
    { jsonrpc: '2.0', id: 16, method: 'tools/call', params: { name: 'fs_create_directory', arguments: { path: 'nested/nonempty', recursive: true } } },
    { jsonrpc: '2.0', id: 17, method: 'tools/call', params: { name: 'fs_write_file', arguments: { path: 'nested/nonempty/file.txt', content: 'not empty\n' } } },
    { jsonrpc: '2.0', id: 18, method: 'tools/call', params: { name: 'fs_delete_directory', arguments: { path: 'nested/nonempty' } } },
    { jsonrpc: '2.0', id: 19, method: 'tools/call', params: { name: 'fs_delete_directory', arguments: { path: 'nested/nonempty', recursive: true } } },
    { jsonrpc: '2.0', id: 20, method: 'tools/call', params: { name: 'fs_move_path', arguments: { from: 'nested/moved.txt', to: 'nested/stale.txt', expected_from_sha256: 'deadbeef' } } },
    { jsonrpc: '2.0', id: 21, method: 'tools/call', params: { name: 'fs_move_path', arguments: { from: 'nested/moved.txt', to: '../outside.txt' } } },
    { jsonrpc: '2.0', id: 22, method: 'tools/call', params: { name: 'fs_write_file', arguments: { path: 'nested/existing.txt', content: 'existing\n' } } },
    { jsonrpc: '2.0', id: 23, method: 'tools/call', params: { name: 'fs_move_path', arguments: { from: 'nested/moved.txt', to: 'nested/existing.txt' } } },
  ], auditLogDir, root);
  const byId = new Map(responses.map((response) => [response.id, response]));
  assert.equal(byId.get(1)?.result?.serverInfo?.name, expectedServerName);
  assert.equal(byId.get(2)?.result?.tools?.some((tool: JsonRecord) => tool.name === 'fs_write_file'), true);
  assert.equal(byId.get(2)?.result?.tools?.some((tool: JsonRecord) => tool.name === 'fs_str_replace_file'), true);
  assert.equal(byId.get(2)?.result?.tools?.some((tool: JsonRecord) => tool.name === 'fs_replace_range'), true);
  assert.equal(byId.get(2)?.result?.tools?.some((tool: JsonRecord) => tool.name === 'fs_move_path'), true);
  assert.equal(byId.get(2)?.result?.tools?.some((tool: JsonRecord) => tool.name === 'fs_create_directory'), true);
  assert.equal(byId.get(2)?.result?.tools?.some((tool: JsonRecord) => tool.name === 'fs_rename_directory'), true);
  assert.equal(byId.get(2)?.result?.tools?.some((tool: JsonRecord) => tool.name === 'fs_delete_directory'), true);
  assert.equal(byId.get(3)?.result?.structuredContent?.effective_permissions?.can_write, true);
  assert.equal(byId.get(3)?.result?.structuredContent?.allowed_roots?.includes(extraRoot), true);
  assert.equal(byId.get(3)?.result?.structuredContent?.allowed_root_entries?.some((entry: JsonRecord) => entry.provenance?.source === 'site_allowed_roots_config'), true);
  assert.equal(byId.get(4)?.result?.structuredContent?.schema, 'local.filesystem.write_file.v1');
  assert.equal(byId.get(4)?.result?.structuredContent?.status, 'written');
  assert.equal(byId.get(5)?.result?.structuredContent?.content, undefined);
  assert.equal(byId.get(5)?.result?.structuredContent?.content_delivery?.duplicated_in_structured_content, false);
  assert.match(byId.get(5)?.result?.content?.[0]?.text ?? '', /hello native/);
  assert.equal(JSON.stringify(byId.get(5)?.result).split('hello native').length - 1, 1);
  assert.equal(byId.get(6)?.error?.data?.code, 'fs_write_file_expected_sha256_mismatch');
  assert.equal(byId.get(7)?.error?.data?.code, 'transient_executable_write_disallowed');
  assert.equal(byId.get(8)?.result?.structuredContent?.schema, 'local.filesystem.str_replace_file.v1');
  assert.equal(byId.get(8)?.result?.structuredContent?.status, 'replaced');
  assert.equal(byId.get(9)?.result?.structuredContent?.schema, 'local.filesystem.replace_range.v1');
  assert.equal(byId.get(9)?.result?.structuredContent?.status, 'replaced_range');
  assert.equal(byId.get(10)?.result?.structuredContent?.content, undefined);
  assert.match(byId.get(10)?.result?.content?.[0]?.text ?? '', /range replaced/);
  assert.equal(JSON.stringify(byId.get(10)?.result).split('range replaced').length - 1, 1);
  assert.equal(byId.get(11)?.result?.structuredContent?.status, 'created');
  assert.equal(byId.get(12)?.result?.structuredContent?.schema, 'local.filesystem.rename_directory.v1');
  assert.equal(byId.get(14)?.result?.structuredContent?.schema, 'local.filesystem.move_path.v1');
  assert.equal(byId.get(15)?.result?.structuredContent?.status, 'deleted');
  assert.equal(byId.get(18)?.error?.data?.code, 'delete_directory_not_empty');
  assert.equal(byId.get(19)?.result?.structuredContent?.status, 'deleted');
  assert.equal(byId.get(20)?.error?.data?.code, 'fs_move_path_expected_metadata_mismatch');
  assert.equal(byId.get(21)?.error?.data?.code, 'path_outside_allowed_roots');
  assert.equal(byId.get(22)?.result?.structuredContent?.status, 'written');
  assert.equal(byId.get(23)?.error?.data?.code, 'move_destination_exists');
  assert.match(readFileSync(join(auditLogDir, 'filesystem-mcp-audit.jsonl'), 'utf8'), /"operation":"fs_write_file"/);

  const readResponses = await run('read', root, [
    { jsonrpc: '2.0', id: 8, method: 'tools/list', params: {} },
    { jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name: 'fs_write_file', arguments: { path: 'blocked.txt', content: 'nope' } } },
  ], undefined, root);
  const readById = new Map(readResponses.map((response) => [response.id, response]));
  assert.equal(readById.get(8)?.result?.tools?.some((tool: JsonRecord) => tool.name === 'fs_write_file'), false);
  assert.equal(readById.get(9)?.error?.data?.code, 'tool_not_available_in_read_mode');
} finally {
  rmSync(root, { recursive: true, force: true });
  rmSync(extraRoot, { recursive: true, force: true });
}
