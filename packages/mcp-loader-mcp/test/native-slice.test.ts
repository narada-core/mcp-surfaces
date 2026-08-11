import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { requireNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const executable = requireNativeArtifact(packageRoot, process.platform === 'win32' ? 'narada-mcp-loader.exe' : 'narada-mcp-loader');

test('native loader slice attaches, calls, restarts, and detaches one MCP child', async (t) => {
  if (!existsSync(executable)) {
    t.skip('native loader artifact is not built; run pnpm build:native first');
    return;
  }
  const root = mkdtempSync(join(tmpdir(), 'narada-loader-native-'));
  const child = join(root, 'child.mjs');
  writeFileSync(child, `
let buffer = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  buffer += chunk;
  const lines = buffer.split(/\\r?\\n/);
  buffer = lines.pop() ?? '';
  for (const line of lines) {
    if (!line.trim()) continue;
    const request = JSON.parse(line);
    if (request.method === 'notifications/initialized') continue;
    let result = {};
    if (request.method === 'initialize') result = { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'native-test-child', version: '1' } };
    else if (request.method === 'tools/list') result = { tools: [{ name: 'echo', description: 'Echo', inputSchema: { type: 'object' } }] };
    else if (request.method === 'tools/call') result = { content: [{ type: 'text', text: JSON.stringify(request.params.arguments) }], structuredContent: { status: 'ok', args: request.params.arguments } };
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\\n');
  }
});
`, 'utf8');

  const processHandle = spawn(executable, ['--child-command', process.execPath, '--allowed-entrypoint-prefix', root, '--attach-timeout-ms', '3000', '--tool-call-timeout-ms', '3000'], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let buffer = '';
  const pending: Array<{ resolve: (value: any) => void; reject: (error: Error) => void }> = [];
  processHandle.stdout.setEncoding('utf8');
  processHandle.stdout.on('data', (chunk) => {
    buffer += chunk;
    const lines = buffer.split(/\r?\n/);
    buffer = lines.pop() ?? '';
    for (const line of lines) {
      if (!line.trim()) continue;
      const waiter = pending.shift();
      if (waiter) waiter.resolve(JSON.parse(line));
    }
  });
  const call = (method: string, params: Record<string, unknown> = {}, id: number) => new Promise<any>((resolvePromise, reject) => {
    pending.push({ resolve: resolvePromise, reject });
    processHandle.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    setTimeout(() => reject(new Error(`timeout:${method}`)), 5000).unref();
  });
  try {
    const initialized = await call('initialize', { protocolVersion: '2024-11-05' }, 1);
    assert.equal(initialized.result.serverInfo.name, 'mcp-loader-mcp');
    const listed = await call('tools/list', {}, 2);
    assert.ok(listed.result.tools.some((tool: any) => tool.name === 'mcp_loader_attach_surface'));
    const attached = await call('tools/call', { name: 'mcp_loader_attach_surface', arguments: { entrypoint: child, surface_id: 'echo' } }, 3);
    const attachedData = attached.result.structuredContent;
    assert.equal(attachedData.schema, 'narada.mcp_loader.surface_attached.v1');
    const connectionId = attachedData.connection_id;
    const called = await call('tools/call', { name: 'mcp_loader_call_tool', arguments: { connection_id: connectionId, tool_name: 'echo', arguments: { value: 'ok' } } }, 4);
    assert.equal(called.result.structuredContent.result.structuredContent.args.value, 'ok');
    const restarted = await call('tools/call', { name: 'mcp_loader_surface_restart', arguments: { connection_id: connectionId } }, 5);
    assert.equal(restarted.result.structuredContent.status, 'restarted');
    const replacementId = restarted.result.structuredContent.connection_id;
    const detached = await call('tools/call', { name: 'mcp_loader_detach', arguments: { connection_id: replacementId } }, 6);
    assert.equal(detached.result.structuredContent.status, 'detached');
  } finally {
    processHandle.kill();
    rmSync(root, { recursive: true, force: true });
  }
});
