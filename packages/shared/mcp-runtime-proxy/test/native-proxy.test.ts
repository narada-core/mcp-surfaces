import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { requireNativeArtifact } from '../src/native-artifact.js';
import { fileURLToPath } from 'node:url';
import { MCP_RUNTIME_CONTRACT_VERSION } from '../src/materialization-contract.js';
import { fingerprintWorkspaceArtifactManifest } from '../src/workspace-artifact-manifest.js';

type JsonRecord = Record<string, any>;

const root = mkdtempSync(join(tmpdir(), 'mcp-native-proxy-'));
const childPath = join(root, 'child.mjs');
const manifestPath = join(root, 'workspace-artifact-manifest.json');
const bunProxyPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const nativeProxyPath = requireNativeArtifact(packageRoot, 'narada-mcp-runtime.exe');

function artifact(path: string) {
  const bytes = readFileSync(path);
  const stat = statSync(path);
  return { path, sha256: createHash('sha256').update(bytes).digest('hex'), size: stat.size, mtime_ms: stat.mtimeMs };
}

function writeManifest(paths: string[] = [childPath]): void {
  const unsigned = {
    schema: 'narada.workspace_artifact_manifest.v1',
    generated_at: '2026-08-05T00:00:00.000Z',
    workspace_root: root,
    packages: [],
    artifacts: paths.map(artifact),
  };
  writeFileSync(manifestPath, JSON.stringify({ ...unsigned, manifest_fingerprint: fingerprintWorkspaceArtifactManifest(unsigned) }, null, 2) + '\n');
}

function commonArgs(diagnostics: string): string[] {
  return [
    '--surface-id', 'native-parity',
    '--carrier-id', 'fixture-carrier',
    '--artifact-manifest', manifestPath,
    '--runtime-contract-version', String(MCP_RUNTIME_CONTRACT_VERSION),
    '--child-command', process.execPath,
    '--entrypoint', childPath,
    '--diagnostics-dir', diagnostics,
    '--orphan-grace-ms', '1000',
    '--',
  ];
}

async function exchange(kind: 'bun' | 'native', framed = false): Promise<{ responses: JsonRecord[]; diagnostics: JsonRecord; stderr: string }> {
  const diagnostics = join(root, `diagnostics-${kind}-${framed ? 'framed' : 'jsonl'}`);
  mkdirSync(diagnostics, { recursive: true });
  const child = kind === 'native'
    ? spawn(nativeProxyPath, ['proxy', ...commonArgs(diagnostics)], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true })
    : spawn(process.execPath, [bunProxyPath, ...commonArgs(diagnostics)], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let stdout = Buffer.alloc(0);
  let stderr = '';
  child.stdout.on('data', (chunk: Buffer) => { stdout = Buffer.concat([stdout, chunk]); });
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  const requests = [
    { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } },
    { jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} },
    { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'fixture_echo', arguments: { value: 'ok' } } },
    { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'mcp_runtime_proxy_status', arguments: {} } },
  ];
  for (const request of requests) {
    const body = JSON.stringify(request);
    child.stdin.write(framed ? `Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}` : `${body}\n`);
  }
  child.stdin.end();
  const exitCode = await Promise.race([
    new Promise<number | null>((resolve) => child.on('close', resolve)),
    new Promise<never>((_, reject) => setTimeout(() => { child.kill(); reject(new Error(`native_proxy_${kind}_timeout`)); }, 5_000)),
  ]);
  assert.equal(exitCode, 0, `${kind}:${stderr}\nstdout=${stdout.toString('utf8')}`);
  const text = stdout.toString('utf8');
  const responses = framed ? parseFrames(text) : text.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
  const instanceName = readdirSync(diagnostics).find((name) => /^instance-\d+\.json$/.test(name));
  assert.ok(instanceName);
  return { responses, diagnostics: JSON.parse(readFileSync(join(diagnostics, instanceName), 'utf8')), stderr };
}

function parseFrames(text: string): JsonRecord[] {
  const values: JsonRecord[] = [];
  let remaining = text;
  while (remaining.length > 0) {
    const end = remaining.indexOf('\r\n\r\n');
    assert.ok(end >= 0, remaining);
    const match = /Content-Length:\s*(\d+)/i.exec(remaining.slice(0, end));
    assert.ok(match);
    const start = end + 4;
    const length = Number(match[1]);
    values.push(JSON.parse(remaining.slice(start, start + length)));
    remaining = remaining.slice(start + length);
  }
  return values;
}

async function runNativeSingle(
  fixturePath: string,
  request: JsonRecord,
  extraArgs: string[] = [],
): Promise<{ response: JsonRecord; exitCode: number | null; stderr: string }> {
  writeManifest([fixturePath]);
  const diagnostics = join(root, `diagnostics-scenario-${Date.now()}-${Math.random().toString(16).slice(2)}`);
  mkdirSync(diagnostics, { recursive: true });
  const args = commonArgs(diagnostics);
  const entrypointIndex = args.indexOf('--entrypoint') + 1;
  args[entrypointIndex] = fixturePath;
  args.splice(args.length - 1, 0, ...extraArgs);
  const child = spawn(nativeProxyPath, ['proxy', ...args], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let stdout = '';
  let stderr = '';
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  child.stdout.on('data', (chunk) => { stdout += chunk; });
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  child.stdin.write(JSON.stringify(request) + '\n');
  const response = await new Promise<JsonRecord>((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`native_scenario_timeout:${stderr}`)), 5_000);
    const poll = setInterval(() => {
      const line = stdout.split(/\r?\n/).find(Boolean);
      if (!line) return;
      clearInterval(poll);
      clearTimeout(timeout);
      resolve(JSON.parse(line));
    }, 10);
  });
  child.stdin.end();
  const exitCode = await Promise.race([
    new Promise<number | null>((resolve) => child.on('close', resolve)),
    new Promise<never>((_, reject) => setTimeout(() => { child.kill(); reject(new Error(`native_scenario_exit_timeout:${stderr}`)); }, 5_000)),
  ]);
  return { response, exitCode, stderr };
}

function processAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function stalePreflight(kind: 'bun' | 'native'): Promise<JsonRecord> {
  const stalePath = join(root, `stale-${kind}.mjs`);
  writeFileSync(stalePath, 'process.stdin.resume();\n');
  writeManifest([stalePath]);
  writeFileSync(stalePath, 'process.stdin.resume(); // changed after manifest\n');
  const diagnostics = join(root, `diagnostics-stale-${kind}`);
  const args = commonArgs(diagnostics);
  args[args.indexOf('--entrypoint') + 1] = stalePath;
  const child = kind === 'native'
    ? spawn(nativeProxyPath, ['proxy', ...args], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true })
    : spawn(process.execPath, [bunProxyPath, ...args], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let stdout = '';
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (chunk) => { stdout += chunk; });
  child.stdin.end(JSON.stringify({ jsonrpc: '2.0', id: 'stale', method: 'initialize', params: {} }) + '\n');
  await Promise.race([
    new Promise<number | null>((resolve) => child.on('close', resolve)),
    new Promise<never>((_, reject) => setTimeout(() => { child.kill(); reject(new Error(`stale_preflight_timeout:${kind}`)); }, 5_000)),
  ]);
  return JSON.parse(stdout.trim());
}

writeFileSync(childPath, [
  "let buffer = '';",
  "process.stdin.setEncoding('utf8');",
  "process.stdin.on('data', chunk => {",
  '  buffer += chunk;',
  "  let end; while ((end = buffer.indexOf('\\n')) >= 0) {",
  '    const line = buffer.slice(0, end).trim(); buffer = buffer.slice(end + 1); if (!line) continue;',
  '    const request = JSON.parse(line);',
  "    const result = request.method === 'initialize'",
  "      ? { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'fixture', version: '1' } }",
  "      : request.method === 'tools/list'",
  "        ? { tools: [{ name: 'fixture_echo', description: 'Echo.', inputSchema: { type: 'object' } }] }",
  "        : { content: [{ type: 'text', text: `${request.params.arguments.value}:${process.env.NARADA_MATERIALIZED_CARRIER_ID ?? ''}` }] };",
  "    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\\n');",
  '  }',
  '});',
].join('\n'));
writeManifest();

try {
  const bun = await exchange('bun');
  const native = await exchange('native');
  for (const result of [bun, native]) {
    const byId = new Map(result.responses.map((response) => [response.id, response]));
    assert.equal(byId.get(1)?.result?.serverInfo?.name, 'fixture');
    assert.deepEqual(byId.get(2)?.result?.tools?.map((tool: JsonRecord) => tool.name), ['fixture_echo', 'mcp_runtime_proxy_status']);
    assert.equal(byId.get(3)?.result?.content?.[0]?.text, 'ok:fixture-carrier');
    assert.equal(byId.get(4)?.result?.structuredContent?.runtime_freshness?.schema, 'narada.mcp_runtime_proxy.runtime_freshness.v2');
    assert.equal(byId.get(4)?.result?.structuredContent?.runtime_freshness?.reload_action?.kind, 'restart_carrier_bound_surface');
    assert.equal(byId.get(4)?.result?.structuredContent?.liveness?.observed_state, 'live');
  }
  assert.equal(bun.diagnostics.supervisor_pid > 0, true);
  assert.equal(native.diagnostics.supervisor_pid, null);
  assert.equal(native.diagnostics.managed_child_pid, native.diagnostics.server_pid);
  assert.equal(native.diagnostics.liveness_evidence.proxy_implementation, 'native');

  const framed = await exchange('native', true);
  assert.equal(framed.responses.length, 4);
  assert.equal(framed.responses.find((response) => response.id === 3)?.result?.content?.[0]?.text, 'ok:fixture-carrier');

  const silentChildPath = join(root, 'silent-child.mjs');
  writeFileSync(silentChildPath, "process.stdin.resume();\n");
  const timedOut = await runNativeSingle(
    silentChildPath,
    { jsonrpc: '2.0', id: 'timeout', method: 'initialize', params: {} },
    ['--request-timeout-ms', '100', '--tool-timeout-grace-ms', '10'],
  );
  assert.equal(timedOut.response.error?.data?.code, 'child_request_timeout');
  assert.notEqual(timedOut.exitCode, 0);

  const failingChildPath = join(root, 'failing-child.mjs');
  writeFileSync(failingChildPath, "process.stderr.write('native-fixture-failure\\n'); process.exit(7);\n");
  const failed = await runNativeSingle(
    failingChildPath,
    { jsonrpc: '2.0', id: 'failure', method: 'initialize', params: {} },
  );
  assert.equal(failed.response.error?.data?.code, 'child_exited_before_response');
  assert.match(JSON.stringify(failed.response), /native-fixture-failure/);
  assert.equal(failed.exitCode, 1);

  const descendantChildPath = join(root, 'descendant-child.mjs');
  writeFileSync(descendantChildPath, [
    "import { spawn } from 'node:child_process';",
    "let buffer = ''; process.stdin.setEncoding('utf8');",
    "process.stdin.on('data', chunk => { buffer += chunk; if (!buffer.includes('\\n')) return;",
    "  const request = JSON.parse(buffer.slice(0, buffer.indexOf('\\n')));",
    "  const descendant = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore', windowsHide: true });",
    "  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { descendant_pid: descendant.pid } }) + '\\n');",
    "});",
  ].join('\n'));
  const descendant = await runNativeSingle(
    descendantChildPath,
    { jsonrpc: '2.0', id: 'descendant', method: 'initialize', params: {} },
    ['--orphan-grace-ms', '100'],
  );
  const descendantPid = Number(descendant.response.result?.descendant_pid);
  assert.ok(Number.isSafeInteger(descendantPid) && descendantPid > 0);
  await new Promise((resolve) => setTimeout(resolve, 100));
  assert.equal(processAlive(descendantPid), false, `descendant ${descendantPid} survived native proxy teardown`);

  for (const kind of ['bun', 'native'] as const) {
    const stale = await stalePreflight(kind);
    assert.equal(stale.error?.data?.code, 'workspace_manifest_stale');
    assert.equal(stale.error?.data?.details?.recovery?.schema, 'narada.mcp_runtime_proxy.workspace_recovery.v1');
    assert.equal(stale.error?.data?.details?.recovery?.steps?.[1]?.action, 'materialize_all_carriers');
  }
  console.log('native mcp runtime proxy parity ok');
} finally {
  rmSync(root, { recursive: true, force: true });
}
