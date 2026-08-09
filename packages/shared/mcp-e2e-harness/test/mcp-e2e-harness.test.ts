import assert from 'node:assert/strict';
import { join } from 'node:path';
import { readFileSync, rmSync, writeFileSync } from 'node:fs';
import {
  asRecord,
  assertFileStateUnchanged,
  assertSiteFabricEnvIsolated,
  createJsonlClient,
  createSiteFabricIsolation,
  createTestProcessScope,
  createTemporaryE2eRoot,
  readMcpOutputText,
  removeTemporaryE2eRoot,
  resolveDefaultUserSiteRegistryPath,
  runBoundedProcess,
  runMcpProtocolSmoke,
  siteFabricChildEnv,
  snapshotFileState,
  spawnContentLengthMcpServer,
  spawnJsonlMcpServer,
  structured,
  tomlPath,
} from '../src/main.js';

const processScope = createTestProcessScope({ label: 'mcp-e2e-harness-test' });

const fixture = spawnJsonlMcpServer(process.execPath, [
  '-e',
  [
    "process.stdin.setEncoding('utf8');",
    "process.stdin.on('data', (chunk) => { for (const line of chunk.split(/\\r?\\n/).filter(Boolean)) { const request = JSON.parse(line); process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { structuredContent: { schema: 'fixture.response.v1', method: request.method } } }) + '\\n'); } });",
  ].join('\n'),
], { label: 'mcp-e2e-harness-fixture', timeoutMs: 2_000, scope: processScope });

const response = await fixture.client.request(1, 'initialize', { protocolVersion: '2024-11-05' });
assert.equal(structured(response).schema, 'fixture.response.v1');
assert.equal(structured(response).method, 'initialize');
await fixture.close();

const framedFixture = spawnContentLengthMcpServer(process.execPath, [
  '-e',
  [
    "let buffer = Buffer.alloc(0);",
    "const separator = String.fromCharCode(13, 10, 13, 10);",
    "process.stdin.on('data', (chunk) => {",
    "  buffer = Buffer.concat([buffer, chunk]);",
    "  while (true) {",
    "    const headerEnd = buffer.indexOf(separator);",
    "    if (headerEnd < 0) break;",
    "    const header = buffer.subarray(0, headerEnd).toString('utf8');",
    "    const match = /Content-Length:[ ]*([0-9]+)/i.exec(header);",
    "    if (!match) throw new Error('missing content length');",
    "    const bodyStart = headerEnd + separator.length;",
    "    const length = Number(match[1]);",
    "    if (buffer.length < bodyStart + length) break;",
    "    const request = JSON.parse(buffer.subarray(bodyStart, bodyStart + length).toString('utf8'));",
    "    buffer = buffer.subarray(bodyStart + length);",
    "    const body = JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { structuredContent: { schema: 'framed.fixture.v1', method: request.method } } });",
    "    process.stdout.write('Content-Length: ' + Buffer.byteLength(body, 'utf8') + separator + body);",
    "  }",
    "});",
  ].join(String.fromCharCode(10)),
], { label: 'mcp-e2e-harness-framed-fixture', timeoutMs: 2_000, scope: processScope });

const framedResponse = await framedFixture.client.request(2, 'initialize', { protocolVersion: '2024-11-05' });
assert.equal(structured(framedResponse).schema, 'framed.fixture.v1');
assert.equal(structured(framedResponse).method, 'initialize');
await framedFixture.close();

const protocolFixture = spawnJsonlMcpServer(process.execPath, [
  '-e',
  [
    "process.stdin.setEncoding('utf8');",
    "process.stdin.on('data', (chunk) => { for (const line of chunk.split(/\\r?\\n/).filter(Boolean)) { const request = JSON.parse(line); if (request.method === 'initialize') process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { serverInfo: { name: 'protocol-fixture' }, capabilities: { tools: {} } } }) + '\\n'); if (request.method === 'tools/list') process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { tools: [{ name: 'fixture_tool' }] } }) + '\\n'); } });",
  ].join('\n'),
], { label: 'mcp-e2e-harness-protocol-fixture', timeoutMs: 2_000, scope: processScope });
const protocol = await runMcpProtocolSmoke(protocolFixture.client, {
  expectedServerName: 'protocol-fixture',
  requiredTools: ['fixture_tool'],
});
assert.deepEqual(protocol.toolNames, ['fixture_tool']);
await protocolFixture.close();

const modernFixture = spawnJsonlMcpServer(process.execPath, [
  '-e',
  [
    "process.stdin.setEncoding('utf8');",
    "process.stdin.on('data', (chunk) => { for (const line of chunk.split(/\\r?\\n/).filter(Boolean)) { const request = JSON.parse(line); if (request.method === 'server/discover') process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { resultType: 'complete', supportedVersions: ['2026-07-28'], capabilities: { tools: {} }, _meta: { 'io.modelcontextprotocol/serverInfo': { name: 'modern-fixture', version: '1.0.0' } }, ttlMs: 3600000, cacheScope: 'public' } }) + '\\n'); if (request.method === 'tools/list') process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { resultType: 'complete', tools: [{ name: 'modern_fixture_tool' }], ttlMs: 300000, cacheScope: 'public' } }) + '\\n'); } });",
  ].join('\n'),
], { label: 'mcp-e2e-harness-modern-fixture', timeoutMs: 2_000, scope: processScope, protocolMode: 'modern' });
const modernProtocol = await runMcpProtocolSmoke(modernFixture.client, {
  protocolMode: 'modern',
  expectedServerName: 'modern-fixture',
  requiredTools: ['modern_fixture_tool'],
});
assert.deepEqual(modernProtocol.toolNames, ['modern_fixture_tool']);
assert.equal(modernProtocol.discover?.resultType, 'complete');
await modernFixture.close();

const output = await readMcpOutputText(
  { output_text: '{"status":"', next_offset: 4 },
  async ({ offset }) => offset === 4
    ? { output_text: 'completed"}', next_offset: null }
    : {},
  { pageSize: 4 },
);
assert.equal(output.text, '{"status":"completed"}');
assert.equal(output.pages, 2);
await assert.rejects(
  () => readMcpOutputText(
    { output_text: '', next_offset: 0 },
    async () => ({ output_text: '', next_offset: 0 }),
    { initialReadOffset: 0 },
  ),
  /offset did not advance/,
);

assert.deepEqual(asRecord({ value: 1 }), { value: 1 });
assert.deepEqual(asRecord(null), {});
assert.equal(tomlPath('C:\\tmp\\value\"x'), 'C:/tmp/value\\"x');

const root = createTemporaryE2eRoot('shared harness test');
assert.equal(removeTemporaryE2eRoot(root), true);

const isolationRoot = createTemporaryE2eRoot('shared harness isolation');
const isolation = createSiteFabricIsolation(isolationRoot);
assert.ok(isolation.userSiteRoot.startsWith(isolationRoot));
assert.equal(isolation.env.NARADA_USER_SITE_ROOT, isolation.userSiteRoot);
const childEnv = siteFabricChildEnv(isolationRoot, { FIXTURE_FLAG: '1' });
assert.equal(childEnv.NARADA_USER_SITE_ROOT, isolation.userSiteRoot);
assert.equal(childEnv.FIXTURE_FLAG, '1');
assertSiteFabricEnvIsolated(childEnv, isolationRoot);
assert.throws(() => assertSiteFabricEnvIsolated({}, isolationRoot), /missing NARADA_USER_SITE_ROOT/);
assert.throws(() => siteFabricChildEnv(isolationRoot, { NARADA_USER_SITE_ROOT: process.cwd() }), /escapes the temporary root/);
assert.ok(resolveDefaultUserSiteRegistryPath().endsWith('registry.db'));
if (process.platform === 'win32') {
  assert.throws(() => resolveDefaultUserSiteRegistryPath({}), /USERPROFILE not set/);
}

const missingSnapshot = snapshotFileState(join(isolationRoot, 'missing.db'));
assert.equal(missingSnapshot.exists, false);
assertFileStateUnchanged(missingSnapshot);
const snapshotPath = join(isolation.userSiteRoot, 'registry.db');
writeFileSync(snapshotPath, 'fixture', 'utf8');
const presentSnapshot = snapshotFileState(snapshotPath);
assertFileStateUnchanged(presentSnapshot);
writeFileSync(snapshotPath, 'fixture-with-more-content', 'utf8');
assert.throws(() => assertFileStateUnchanged(presentSnapshot), /file modified during e2e/);
assert.equal(removeTemporaryE2eRoot(isolationRoot), true);

const rawChild = processScope.spawn(process.execPath, [
  '-e',
  "process.stdin.on('data', () => process.stdout.write('\\n')); process.stdin.resume();",
], { windowsHide: true });
const client = createJsonlClient(rawChild, { label: 'raw-client-fixture', timeoutMs: 2_000 });
await client.close();
const descendantRoot = createTemporaryE2eRoot('shared harness descendant');
const descendantPidPath = join(descendantRoot, 'pid.txt');
const descendantParent = processScope.spawn(process.execPath, [
  '-e',
  "const fs = require('node:fs'); const { spawn } = require('node:child_process'); const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'inherit', windowsHide: true }); fs.writeFileSync(process.argv[1], String(child.pid)); setTimeout(() => process.exit(0), 50);",
  descendantPidPath,
], { windowsHide: true });
await new Promise<void>((resolve, reject) => {
  const timer = setTimeout(() => reject(new Error('process scope helper did not close after terminating an inherited-output descendant')), 1_500);
  descendantParent.once('close', () => {
    clearTimeout(timer);
    resolve();
  });
  descendantParent.once('error', (error) => {
    clearTimeout(timer);
    reject(error);
  });
});
const descendantPid = Number(readFileSync(descendantPidPath, 'utf8'));
assert.ok(Number.isInteger(descendantPid) && descendantPid > 0);
rmSync(descendantRoot, { recursive: true, force: true });
const bounded = await runBoundedProcess(process.execPath, [
  '-e',
  'setInterval(() => {}, 1000);',
], {
  label: 'bounded-timeout-fixture',
  scope: processScope,
  timeoutMs: 250,
});
assert.equal(bounded.timedOut, true, JSON.stringify(bounded));
assert.equal(bounded.durationMs >= 200, true, JSON.stringify(bounded));
await processScope.close();
processScope.assertClean();
assert.throws(() => process.kill(descendantPid, 0));

console.log(JSON.stringify({
  schema: 'narada.mcp.e2e.result.v1',
  test_id: 'mcp-e2e-harness',
  status: 'passed',
  shared_mechanics: ['jsonl_transport', 'content_length_transport', 'protocol_smoke', 'bounded_output_readback', 'bounded_child_cleanup', 'temporary_root', 'result_normalization'],
}));
