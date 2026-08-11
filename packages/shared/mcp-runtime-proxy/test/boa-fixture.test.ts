import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { requireNativeArtifact } from '../src/native-artifact.js';

type JsonRecord = Record<string, any>;

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const executable = requireNativeArtifact(packageRoot, 'narada-mcp-boa-fixture.exe');

if (process.platform !== 'win32' || !existsSync(executable)) {
  console.log(JSON.stringify({ schema: 'narada.mcp_runtime_proxy.boa_fixture_test.v1', status: 'skipped', reason: 'boa_artifact_unavailable' }));
} else {
  const root = mkdtempSync(join(tmpdir(), 'narada-boa-fixture-test-'));
  const handler = join(root, 'handler.js');
  writeFileSync(handler, [
    'globalThis.naradaFixtureHandle = function(request) {',
    "  if (request.method === 'initialize') return { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'boa-fixture', version: '1' } };",
    "  if (request.method === 'tools/list') return { tools: [{ name: 'fixture_echo', inputSchema: { type: 'object' } }] };",
    "  return { content: [{ type: 'text', text: String(request.params.arguments.value) }] };",
    '};',
  ].join('\n'));

  try {
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } },
      { jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'fixture_echo', arguments: { value: 'boa-ok' } } },
    ];
    const result = await new Promise<{ responses: JsonRecord[]; exitCode: number | null }>((resolve, reject) => {
      const child = spawn(executable, [handler], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
      let stdout = '';
      let stderr = '';
      let settled = false;
      const timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        child.kill();
        reject(new Error(`boa_fixture_timeout:${stderr}`));
      }, 10_000);
      child.stdout.setEncoding('utf8');
      child.stderr.setEncoding('utf8');
      child.stdout.on('data', (chunk) => { stdout += chunk; });
      child.stderr.on('data', (chunk) => { stderr += chunk; });
      child.on('error', (error) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        reject(error);
      });
      child.on('close', (exitCode) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        if (exitCode !== 0) {
          reject(new Error(`boa_fixture_exit:${exitCode}:${stderr}`));
          return;
        }
        try {
          const responses = stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line) as JsonRecord);
          resolve({ responses, exitCode });
        } catch (error) {
          reject(new Error(`boa_fixture_invalid_output:${String(error)}:${stdout.slice(0, 1000)}`));
        }
      });
      child.stdin.end(`${requests.map((request) => JSON.stringify(request)).join('\n')}\n`);
    });
    assert.equal(result.exitCode, 0);
    assert.equal(result.responses.length, 3);
    const byId = new Map(result.responses.map((response) => [response.id, response]));
    assert.equal(byId.get(1)?.result?.serverInfo?.name, 'boa-fixture');
    assert.deepEqual(byId.get(2)?.result?.tools?.map((tool: JsonRecord) => tool.name), ['fixture_echo']);
    assert.equal(byId.get(3)?.result?.content?.[0]?.text, 'boa-ok');
    console.log(JSON.stringify({ schema: 'narada.mcp_runtime_proxy.boa_fixture_test.v1', status: 'passed' }));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}
