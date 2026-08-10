import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import { resolveNativeArtifact } from '../src/native-artifact.js';

type JsonRecord = Record<string, any>;

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const executable = resolve(process.env.NARADA_NATIVE_GIT_TEST_EXECUTABLE ?? resolveNativeArtifact(packageRoot, 'narada-mcp-runtime.exe') ?? resolve(packageRoot, 'dist', 'native', 'narada-mcp-runtime.exe'));

function git(root: string, args: string[]): string {
  return execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true }).trim();
}

function run(root: string, requests: JsonRecord[], mode = 'read'): Promise<JsonRecord[]> {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(executable, ['git', '--mode', mode, '--allowed-root', root, '--output-root', root], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    const timer = setTimeout(() => { child.kill(); rejectPromise(new Error(`native_git_timeout:${stderr}`)); }, 20_000);
    child.on('error', (error) => { clearTimeout(timer); rejectPromise(error); });
    child.on('close', (code) => {
      clearTimeout(timer);
      if (code !== 0) { rejectPromise(new Error(`native_git_exit:${code}:${stderr}`)); return; }
      try { resolvePromise(stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))); }
      catch (error) { rejectPromise(new Error(`native_git_invalid_output:${String(error)}:${stdout.slice(0, 1000)}`)); }
    });
    child.stdin.end(requests.map((request) => JSON.stringify(request)).join('\n') + '\n');
  });
}

const root = mkdtempSync(join(tmpdir(), 'narada-native-git-'));
try {
  git(root, ['init', '-q']);
  git(root, ['config', 'user.email', 'native-git@example.invalid']);
  git(root, ['config', 'user.name', 'Native Git']);
  mkdirSync(join(root, 'src'));
  writeFileSync(join(root, 'README.md'), 'native git\n', 'utf8');
  writeFileSync(join(root, 'src', 'main.txt'), 'one\n', 'utf8');
  git(root, ['add', '.']);
  git(root, ['commit', '-qm', 'initial']);
  writeFileSync(join(root, 'src', 'main.txt'), 'one\ntwo\n', 'utf8');
  writeFileSync(join(root, 'untracked.txt'), 'untracked\n', 'utf8');

  const modernMeta = {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    "io.modelcontextprotocol/clientInfo": { name: "native-test", version: "1" },
    "io.modelcontextprotocol/clientCapabilities": {}
  };
  const responses = await run(root, [
    { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } },
    { jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} },
    { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'git_guidance', arguments: {} } },
    { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'git_policy_inspect', arguments: {} } },
    { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'git_begin_work_scope', arguments: { working_directory: root, allowed_paths: ['src'] } } },
    { jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'git_status', arguments: { working_directory: root } } },
    { jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name: 'git_sync_status', arguments: { working_directory: root } } },
    { jsonrpc: '2.0', id: 8, method: 'tools/call', params: { name: 'git_branch_list', arguments: { working_directory: root, scope: 'local' } } },
    { jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name: 'git_changed_summary', arguments: { working_directory: root } } },
    { jsonrpc: '2.0', id: 10, method: 'tools/call', params: { name: 'git_repositories_summary', arguments: { working_directories: [root] } } },
    { jsonrpc: '2.0', id: 11, method: 'tools/call', params: { name: 'git_diff', arguments: { working_directory: root, scope: 'working', limit: 4000 } } },
    { jsonrpc: '2.0', id: 12, method: 'tools/call', params: { name: 'git_log', arguments: { working_directory: root, limit: 2 } } },
    { jsonrpc: '2.0', id: 13, method: 'tools/call', params: { name: 'git_status', arguments: { working_directory: root, pathspec: 'src', format: 'paths' } } },
    { jsonrpc: '2.0', id: 14, method: 'tools/call', params: { name: 'git_workflow_record', arguments: { scope_label: 'native-read-canary', repositories: [{ working_directory: root }] } } },
    { jsonrpc: '2.0', id: 15, method: 'tools/call', params: { name: 'git_add', arguments: { working_directory: root, paths: ['src/main.txt'] } } },
    { jsonrpc: '2.0', id: 16, method: 'tools/call', params: { name: 'git_unstage', arguments: { working_directory: root, paths: ['src/main.txt'] } } },
    { jsonrpc: "2.0", id: 20, method: "server/discover", params: { _meta: modernMeta } },
    { jsonrpc: "2.0", id: 21, method: "tools/list", params: { _meta: modernMeta } },
    { jsonrpc: "2.0", id: 22, method: "tools/call", params: { _meta: modernMeta, name: "git_policy_inspect", arguments: {} } },
    { jsonrpc: "2.0", id: 23, method: "initialize", params: { _meta: modernMeta } },
  ]);
  const byId = new Map(responses.map((response) => [response.id, response]));
  assert.equal(byId.get(20)?.result?.resultType, 'complete');
  assert.equal(byId.get(20)?.result?.supportedVersions?.includes('2026-07-28'), true);
  assert.equal(byId.get(21)?.result?.resultType, 'complete');
  assert.equal(byId.get(21)?.result?.cacheScope, 'public');
  assert.equal(byId.get(22)?.result?.resultType, 'complete');
  assert.equal(byId.get(22)?.result?._meta?.['io.modelcontextprotocol/serverInfo']?.name, 'git-mcp');
  assert.equal(byId.get(23)?.error?.data?.code, 'initialize_removed');
  assert.equal(byId.get(1)?.result?.serverInfo?.name, 'git-mcp');
  assert.deepEqual(byId.get(2)?.result?.tools?.map((tool: JsonRecord) => tool.name), [
    'git_guidance',
    'git_policy_inspect',
    'git_begin_work_scope',
    'git_workflow_record',
    'git_add',
    'git_unstage',
    'git_status',
    'git_sync_status',
    'git_branch_list',
    'git_output_show',
    'git_changed_summary',
    'git_repositories_summary',
    'git_diff',
    'git_log',
    'git_show',
  ]);
  assert.equal(byId.get(3)?.result?.structuredContent?.surface_id, 'git');
  assert.equal(byId.get(4)?.result?.structuredContent?.schema, 'narada.git.policy.v1');
  assert.equal(byId.get(4)?.result?.structuredContent?.mode, 'read');
  const workScope = byId.get(5)?.result?.structuredContent;
  assert.equal(workScope?.schema, 'narada.git.work_scope.v1');
  assert.deepEqual(workScope?.allowed_paths, ['src']);
  assert.equal(byId.get(6)?.result?.structuredContent?.schema, 'narada.git.status.v1');
  assert.equal(byId.get(6)?.result?.structuredContent?.clean, false);
  assert.equal(byId.get(6)?.result?.structuredContent?.untracked?.includes('untracked.txt'), true);
  assert.equal(byId.get(7)?.result?.structuredContent?.schema, 'narada.git.sync_status.v1');
  assert.equal(byId.get(7)?.result?.structuredContent?.in_progress, false);
  assert.equal(byId.get(8)?.result?.structuredContent?.schema, 'narada.git.branch_list.v1');
  assert.equal(byId.get(8)?.result?.structuredContent?.returned >= 1, true);
  assert.equal(byId.get(9)?.result?.structuredContent?.schema, 'narada.git.changed_summary.v1');
  assert.equal(byId.get(9)?.result?.structuredContent?.relevant_changed_count, 0);
  assert.equal(byId.get(10)?.result?.structuredContent?.repository_count, 1);
  assert.equal(byId.get(11)?.result?.structuredContent?.schema, 'narada.git.diff.v1');
  assert.match(String(byId.get(11)?.result?.structuredContent?.diff), /two/);
  assert.equal(byId.get(12)?.result?.structuredContent?.schema, 'narada.git.log.v1');
  const commit = byId.get(12)?.result?.structuredContent?.commits?.[0]?.hash;
  assert.match(String(commit), /^[0-9a-f]{40}$/);
  assert.deepEqual(byId.get(13)?.result?.structuredContent?.paths, ['src/main.txt']);
  assert.equal(byId.get(14)?.error?.data?.code, 'git_write_mode_required');
  assert.equal(byId.get(15)?.error?.data?.code, 'git_write_mode_required');
  assert.equal(byId.get(16)?.error?.data?.code, 'git_write_mode_required');

  const writeResponses = await run(root, [
    { jsonrpc: '2.0', id: 30, method: 'tools/call', params: { name: 'git_workflow_record', arguments: { scope_label: 'native-write-canary', summary: 'bounded audit record', repositories: [{ working_directory: root, push_status: 'not_attempted' }] } } },
    { jsonrpc: '2.0', id: 31, method: 'tools/call', params: { name: 'git_add', arguments: { working_directory: root, paths: ['src/main.txt'] } } },
    { jsonrpc: '2.0', id: 32, method: 'tools/call', params: { name: 'git_unstage', arguments: { working_directory: root, paths: ['src/main.txt'] } } },
  ], 'write');
  const workflow = writeResponses.find((response) => response.id === 30)?.result?.structuredContent;
  assert.equal(workflow?.schema, 'narada.git.workflow_record.v1');
  assert.equal(workflow?.status, 'recorded');
  assert.equal(existsSync(String(workflow?.ledger_path)), true);
  assert.match(readFileSync(String(workflow?.ledger_path), 'utf8'), /native-write-canary/);
  assert.equal(writeResponses.find((response) => response.id === 31)?.result?.structuredContent?.schema, 'narada.git.add.v1', JSON.stringify(writeResponses.find((response) => response.id === 31)));
  assert.equal(writeResponses.find((response) => response.id === 31)?.result?.structuredContent?.post_status?.staged?.includes('src/main.txt'), true);
  assert.equal(writeResponses.find((response) => response.id === 32)?.result?.structuredContent?.schema, 'narada.git.unstage.v1', JSON.stringify(writeResponses.find((response) => response.id === 32)));
  assert.equal(writeResponses.find((response) => response.id === 32)?.result?.structuredContent?.post_status?.staged?.includes('src/main.txt'), false, JSON.stringify(writeResponses.find((response) => response.id === 32)));

  const showResponses = await run(root, [
    { jsonrpc: '2.0', id: 12, method: 'tools/call', params: { name: 'git_show', arguments: { working_directory: root, commit, include_patch: false } } },
    { jsonrpc: '2.0', id: 13, method: 'tools/call', params: { name: 'git_status', arguments: { working_directory: join(root, '..') } } },
    { jsonrpc: '2.0', id: 14, method: 'tools/call', params: { name: 'git_show', arguments: { working_directory: root, commit: 'bad!commit', include_patch: false } } },
  ]);
  const show = showResponses.find((response) => response.id === 12);
  const refused = showResponses.find((response) => response.id === 13);
  const invalidCommit = showResponses.find((response) => response.id === 14);
  assert.equal(show?.result?.structuredContent?.schema, 'narada.git.show.v1');
  assert.equal(show?.result?.structuredContent?.include_patch, false);
  assert.equal(refused?.error?.data?.code, 'git_working_directory_outside_allowed_roots');
  assert.equal(invalidCommit?.error?.data?.code, 'git_invalid_commitish');
  assert.equal(readFileSync(join(root, 'README.md'), 'utf8'), 'native git\n');
} finally {
  rmSync(root, { recursive: true, force: true });
}
