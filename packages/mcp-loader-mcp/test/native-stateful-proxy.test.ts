import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { test } from 'node:test';

const packageRoot = resolve(fileURLToPath(new URL('..', import.meta.url)));
const executable = join(packageRoot, 'dist', 'native', process.platform === 'win32' ? 'narada-mcp-loader.exe' : 'narada-mcp-loader');

test('native loader keeps a runtime-proxy child across stateful task-lifecycle calls', async (t) => {
  if (!existsSync(executable)) {
    t.skip('native loader artifact is not built; run pnpm build:native first');
    return;
  }

  const root = mkdtempSync(join(tmpdir(), 'narada-loader-native-stateful-'));
  mkdirSync(join(root, '.ai', 'mcp'), { recursive: true });
  const child = join(root, 'stateful-child.mjs');
  const childSource = [
    "let buffer = '';",
    'let payloadRef = null;',
    "process.stdin.setEncoding('utf8');",
    "function write(message) { process.stdout.write(JSON.stringify(message) + '\\n'); }",
    "process.stdin.on('data', (chunk) => {",
    '  buffer += chunk;',
    '  const lines = buffer.split(/\\r?\\n/);',
    "  buffer = lines.pop() ?? '';",
    '  for (const line of lines) {',
    '    if (!line.trim()) continue;',
    '    const request = JSON.parse(line);',
    "    if (request.method === 'notifications/initialized') continue;",
    "    if (request.method === 'initialize') {",
    "      write({ jsonrpc: '2.0', id: request.id, result: { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'stateful-native-test-child', version: '1' } } });",
    "    } else if (request.method === 'tools/list') {",
    "      write({ jsonrpc: '2.0', id: request.id, result: { tools: [{ name: 'mcp_payload_create', inputSchema: { type: 'object' } }, { name: 'task_lifecycle_create', inputSchema: { type: 'object' } }, { name: 'task_lifecycle_show', inputSchema: { type: 'object' } }] } });",
    "    } else if (request.method === 'tools/call') {",
    '      const name = request.params?.name;',
    '      const args = request.params?.arguments ?? {};',
    '      let structuredContent;',
    "      if (name === 'mcp_payload_create') {",
    "        payloadRef = 'mcp_payload:stateful-native-test@v1';",
    "        structuredContent = { status: 'created', ref: payloadRef };",
    "      } else if (name === 'task_lifecycle_create') {",
    "        structuredContent = args.payload_ref === payloadRef ? { schema: 'narada.task.create.v0', status: 'created', task_number: 2433 } : { status: 'error', code: 'missing_payload_ref' };",
    "      } else if (name === 'task_lifecycle_show') {",
    "        structuredContent = { schema: 'narada.producer_output_page.v1', status: 'ok', task_number: args.task_number, output_ref: 'mcp_output:stateful-native-test' };",
    "      } else {",
    "        structuredContent = { status: 'error', code: 'unknown_tool' };",
    '      }',
    "      write({ jsonrpc: '2.0', id: request.id, result: { content: [{ type: 'text', text: JSON.stringify(structuredContent) }], structuredContent } });",
    '    }',
    '  }',
    '});',
  ].join('\n');
  writeFileSync(child, childSource, 'utf8');

  const proxyEntrypoint = join(root, 'narada-mcp-runtime.exe');
  writeFileSync(join(root, '.ai', 'mcp', 'config.json'), JSON.stringify({
    mcpServers: {
      'task-lifecycle': {
        command: proxyEntrypoint,
        args: [
          'proxy',
          '--surface-id',
          'task-lifecycle',
          '--child-command',
          process.execPath,
          '--entrypoint',
          child,
          '--',
          '--fixture-stateful',
        ],
        surface_id: 'task-lifecycle',
      },
    },
  }), 'utf8');

  const processHandle = spawn(executable, ['--allowed-site-root', root, '--allowed-entrypoint-prefix', root, '--attach-timeout-ms', '3000', '--tool-call-timeout-ms', '3000'], {
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
  });
  let buffer = '';
  const pending: Array<{ id: number; resolve: (value: any) => void; reject: (error: Error) => void }> = [];
  processHandle.stdout.setEncoding('utf8');
  processHandle.stdout.on('data', (chunk) => {
    buffer += chunk;
    const lines = buffer.split(/\r?\n/);
    buffer = lines.pop() ?? '';
    for (const line of lines) {
      if (!line.trim()) continue;
      const message = JSON.parse(line);
      const index = pending.findIndex((entry) => entry.id === message.id);
      if (index < 0) continue;
      const [waiter] = pending.splice(index, 1);
      if (message.error) waiter.reject(new Error(JSON.stringify(message.error)));
      else waiter.resolve(message.result?.structuredContent ?? message.result);
    }
  });
  const call = (method: string, params: Record<string, unknown>, id: number) => new Promise<any>((resolvePromise, reject) => {
    pending.push({ id, resolve: resolvePromise, reject });
    processHandle.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    setTimeout(() => reject(new Error('timeout:' + method)), 10000).unref();
  });

  try {
    const initialized = await call('initialize', { protocolVersion: '2024-11-05' }, 1);
    assert.equal(initialized.serverInfo.name, 'mcp-loader-mcp');

    const opened = await call('tools/call', {
      name: 'mcp_loader_open_surface',
      arguments: { site_root: root, surface_id: 'task-lifecycle' },
    }, 2);
    assert.equal(opened.schema, 'narada.mcp_loader.surface_handle_opened.v1');
    const connectionId = opened.connection_id;

    const payloadCreated = await call('tools/call', {
      name: 'mcp_loader_call_tool',
      arguments: {
        connection_id: connectionId,
        tool_name: 'mcp_payload_create',
        arguments: { payload: { title: 'native loader stateful regression' }, created_by: 'native-stateful.test' },
      },
    }, 3);
    assert.equal(payloadCreated.schema, 'narada.mcp_loader.tool_result.v1');
    assert.equal(payloadCreated.result.structuredContent.status, 'created');
    const payloadRef = payloadCreated.result.structuredContent.ref;

    const taskCreated = await call('tools/call', {
      name: 'mcp_loader_call_tool',
      arguments: { connection_id: connectionId, tool_name: 'task_lifecycle_create', arguments: { payload_ref: payloadRef } },
    }, 4);
    assert.equal(taskCreated.result.structuredContent.schema, 'narada.task.create.v0');
    assert.equal(taskCreated.result.structuredContent.status, 'created');
    assert.equal(taskCreated.result.structuredContent.task_number, 2433);

    const taskShown = await call('tools/call', {
      name: 'mcp_loader_call_tool',
      arguments: { connection_id: connectionId, tool_name: 'task_lifecycle_show', arguments: { task_number: 2433 } },
    }, 5);
    assert.equal(taskShown.result.structuredContent.schema, 'narada.producer_output_page.v1');
    assert.equal(taskShown.result.structuredContent.status, 'ok');

    const inventory = await call('tools/call', { name: 'mcp_loader_connection_inventory', arguments: {} }, 6);
    const connection = inventory.connections.find((entry: any) => entry.connection_id === connectionId);
    assert.equal(connection.status, 'live');
    assert.equal(inventory.closed_count, 0);

    const detached = await call('tools/call', {
      name: 'mcp_loader_detach',
      arguments: { connection_id: connectionId },
    }, 7);
    assert.equal(detached.status, 'detached');
  } finally {
    processHandle.kill();
    rmSync(root, { recursive: true, force: true });
  }
});
