import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { MCP_RUNTIME_CONTRACT_VERSION } from '@narada-core/mcp-runtime-proxy/materialization-contract';

const root = mkdtempSync(join(tmpdir(), 'mcp-registrar-protocol-'));
const serverPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
const artifactManifestPath = fileURLToPath(new URL('../../../../.ai/runtime/workspace-artifact-manifest.json', import.meta.url));
const child = spawn(process.execPath, [serverPath], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });

let stdout = '';
let stderr = '';
child.stdout.setEncoding('utf8');
child.stderr.setEncoding('utf8');
child.stdout.on('data', (chunk) => { stdout += chunk; });
child.stderr.on('data', (chunk) => { stderr += chunk; });

async function exchangeThroughProxy(): Promise<Record<string, any>[]> {
  const proxyPath = fileURLToPath(new URL('../../../shared/mcp-runtime-proxy/dist/src/main.js', import.meta.url));
  const proxy = spawn(process.execPath, [
    proxyPath,
    '--surface-id', 'mcp-registrar',
    '--artifact-manifest', artifactManifestPath,
    '--runtime-contract-version', String(MCP_RUNTIME_CONTRACT_VERSION),
    '--child-command', process.execPath,
    '--entrypoint', serverPath,
    '--',
  ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let proxyStdout = '';
  let proxyStderr = '';
  proxy.stdout.setEncoding('utf8');
  proxy.stderr.setEncoding('utf8');
  proxy.stdout.on('data', (chunk) => { proxyStdout += chunk; });
  proxy.stderr.on('data', (chunk) => { proxyStderr += chunk; });
  proxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 11, method: 'initialize', params: { protocolVersion: '2024-11-05' } })}\n`);
  proxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 12, method: 'tools/list', params: {} })}\n`);
  proxy.stdin.end();
  const exitCode = await Promise.race([
    new Promise<number | null>((resolve) => proxy.on('close', resolve)),
    new Promise<never>((_, reject) => setTimeout(() => {
      proxy.kill();
      reject(new Error('registrar_proxy_protocol_timeout'));
    }, 5_000)),
  ]);
  assert.equal(exitCode, 0, proxyStderr);
  return proxyStdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
}

try {
  child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } })}\n`);
  child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} })}\n`);
  child.stdin.end();

  const exitCode = await new Promise<number | null>((resolve) => child.on('close', resolve));
  assert.equal(exitCode, 0, stderr);

  const responses = stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
  const init = responses.find((m) => m.id === 1);
  assert.equal((init.result as Record<string, any>).serverInfo.name, 'mcp-registrar');

  const tools = (responses.find((m) => m.id === 2).result as Record<string, any>).tools;
  const expected = ['registrar_guidance', 'registrar_surface_list', 'registrar_site_list', 'registrar_site_surfaces', 'registrar_site_bind', 'registrar_site_unbind', 'registrar_carrier_list', 'registrar_carrier_bind', 'registrar_carrier_unbind', 'registrar_sync', 'registrar_carrier_validate', 'registrar_carrier_diff', 'registrar_surface_usage', 'registrar_site_mcp_fabric_validate', 'registrar_site_surface_registry_sync', 'registrar_surface_tool_inventory_check', 'registrar_site_registry_conformance_check', 'registrar_site_output_reader_closure_check'];
  assert.deepEqual(tools.map((t: { name: string }) => t.name), expected);

  const bindTool = tools.find((t: { name: string }) => t.name === 'registrar_site_bind');
  assert.equal(bindTool.annotations.readOnlyHint, false);

  const unbindTool = tools.find((t: { name: string }) => t.name === 'registrar_site_unbind');
  assert.equal(unbindTool.annotations.destructiveHint, true);

  const conformanceTool = tools.find((t: { name: string }) => t.name === 'registrar_site_registry_conformance_check');
  assert.deepEqual(conformanceTool.inputSchema.required, ['site_id', 'observation_ref']);
  assert.equal(conformanceTool.inputSchema.properties.observed_tools, undefined);

  const proxyResponses = await exchangeThroughProxy();
  assert.equal(proxyResponses.find((message) => message.id === 11)?.result?.serverInfo?.name, 'mcp-registrar');
  assert.deepEqual(
    proxyResponses.find((message) => message.id === 12)?.result?.tools?.map((tool: { name: string }) => tool.name),
    [...expected, 'mcp_runtime_proxy_status'],
  );

  console.log('mcp-registrar protocol smoke ok');
} finally {
  rmSync(root, { recursive: true, force: true });
}
