import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, statSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { requireNativeArtifact } from '../src/native-artifact.js';
import { fingerprintWorkspaceArtifactManifest } from '../src/workspace-artifact-manifest.js';
import { MCP_RUNTIME_CONTRACT_VERSION } from '../src/materialization-contract.js';

type JsonRecord = Record<string, any>;

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const workspaceRoot = resolve(packageRoot, '..', '..', '..');
const executable = resolve(process.env.NARADA_NATIVE_FILESYSTEM_TEST_EXECUTABLE ?? requireNativeArtifact(packageRoot, 'narada-mcp-runtime.exe'));

function run(requests: JsonRecord[]): Promise<JsonRecord[]> {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(executable, ['filesystem', '--mode', 'read', '--allowed-root', workspaceRoot], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    const timer = setTimeout(() => { child.kill(); rejectPromise(new Error(`native_filesystem_timeout:${stderr}`)); }, 10_000);
    child.on('error', (error) => { clearTimeout(timer); rejectPromise(error); });
    child.on('close', (code) => {
      clearTimeout(timer);
      if (code !== 0) { rejectPromise(new Error(`native_filesystem_exit:${code}:${stderr}`)); return; }
      try { resolvePromise(stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))); }
      catch (error) { rejectPromise(new Error(`native_filesystem_invalid_output:${String(error)}:${stdout.slice(0, 1000)}`)); }
    });
    child.stdin.end(requests.map((request) => JSON.stringify(request)).join('\n') + '\n');
  });
}

const responses = await run([
  { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } },
  { jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} },
  { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'fs_read_file_range', arguments: { path: 'packages/local-filesystem-mcp/package.json', start_line: 1, end_line: 3 } } },
  { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'fs_grep_search', arguments: { pattern: 'local-filesystem', path: 'packages/local-filesystem-mcp', output_mode: 'files_with_matches', limit: 5 } } },
  { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'fs_read_file', arguments: { path: 'packages/local-filesystem-mcp/package.json', offset: 1, limit: 1 } } },
  { jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'fs_read_file', arguments: { path: 'packages/local-filesystem-mcp/package.json', offset: 1, limit: 1001 } } },
]);
const byId = new Map(responses.map((response) => [response.id, response]));
assert.equal(byId.get(1)?.result?.serverInfo?.name, 'local-filesystem-read-native');
assert.equal(byId.get(2)?.result?.tools?.some((tool: JsonRecord) => tool.name === 'fs_read_file_range'), true);
assert.equal(byId.get(3)?.result?.structuredContent?.schema, 'local.filesystem.read.v1');
assert.equal(byId.get(3)?.result?.structuredContent?.returned_lines, 3);
assert.equal(byId.get(4)?.result?.structuredContent?.schema, 'local.filesystem.grep.v1');
assert.equal(byId.get(4)?.result?.structuredContent?.count > 0, true);
const boundedRead = byId.get(5)?.result?.structuredContent;
assert.equal(boundedRead?.content_sha256, createHash('sha256').update(readFileSync(join(workspaceRoot, 'packages/local-filesystem-mcp/package.json'))).digest('hex'));
assert.equal(boundedRead?.content_hash_scope, 'full_file');
assert.equal(boundedRead?.hash_source, 'live_file_bytes');
assert.equal(boundedRead?.cache_used, false);
assert.equal(boundedRead?.limit_adjusted, false);
assert.equal(boundedRead?.pagination_required, true);
assert.match(JSON.stringify(byId.get(6)), /fs_read_file_limit_exceeds_max/);

const manifestPath = join(tmpdir(), 'native-filesystem-manifest-' + process.pid + '.json');
const bytes = readFileSync(executable);
const stat = statSync(executable);
const unsigned = {
  schema: 'narada.workspace_artifact_manifest.v1',
  generated_at: '2026-08-06T00:00:00.000Z',
  workspace_root: workspaceRoot,
  packages: [],
  artifacts: [{ path: executable, sha256: createHash('sha256').update(bytes).digest('hex'), size: stat.size, mtime_ms: stat.mtimeMs }],
};
const manifestFingerprint = fingerprintWorkspaceArtifactManifest(unsigned);
writeFileSync(manifestPath, JSON.stringify({ ...unsigned, manifest_fingerprint: manifestFingerprint }) + '\n');
const proxy = spawn(executable, [
  'proxy',
  '--artifact-manifest', manifestPath,
  '--runtime-contract-version', String(MCP_RUNTIME_CONTRACT_VERSION),
  '--child-command', executable,
  '--entrypoint', executable,
  '--child-invocation-kind', 'native_applet',
  '--child-applet', 'filesystem',
  '--',
  '--mode', 'read',
  '--allowed-root', workspaceRoot,
], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
let proxyOutput = '';
let proxyStderr = '';
proxy.stdout.setEncoding('utf8');
proxy.stderr.setEncoding('utf8');
proxy.stdout.on('data', (chunk) => { proxyOutput += chunk; });
proxy.stderr.on('data', (chunk) => { proxyStderr += chunk; });
proxy.stdin.end(JSON.stringify({ jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'fs_stat', arguments: { path: 'packages/local-filesystem-mcp/package.json' } } }) + '\n');
await new Promise<void>((resolvePromise, rejectPromise) => {
  const timer = setTimeout(() => { proxy.kill(); rejectPromise(new Error('native_filesystem_proxy_timeout')); }, 10_000);
  proxy.on('close', (code) => {
    clearTimeout(timer);
    if (code !== 0) { rejectPromise(new Error('native_filesystem_proxy_exit:' + code + ':' + proxyStderr)); return; }
    resolvePromise();
  });
});
const proxyResponse = JSON.parse(proxyOutput.trim());
assert.equal(proxyResponse.result?.structuredContent?.schema, 'local.filesystem.stat.v1');
