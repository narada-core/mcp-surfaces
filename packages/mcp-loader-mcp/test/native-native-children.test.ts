import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { test } from 'node:test';
import { requireNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';

const packageRoot = resolve(fileURLToPath(new URL('../..', import.meta.url)));
const workspaceRoot = resolve(packageRoot, '..', '..');
const loaderExecutable = requireNativeArtifact(packageRoot, process.platform === 'win32' ? 'narada-mcp-loader.exe' : 'narada-mcp-loader');
const runtimeExecutable = requireNativeArtifact(resolve(workspaceRoot, 'packages', 'shared', 'mcp-runtime-proxy'), process.platform === 'win32' ? 'narada-mcp-runtime.exe' : 'narada-mcp-runtime');
const taskExecutable = requireNativeArtifact(resolve(workspaceRoot, 'packages', 'shared', 'mcp-lifecycle-native'), process.platform === 'win32' ? 'narada-task-lifecycle-mcp.exe' : 'narada-task-lifecycle-mcp');

type Pending = { resolve: (value: any) => void; reject: (error: Error) => void };

function startLoader(root: string) {
  const child = spawn(loaderExecutable, [
    '--standalone-ambient-attachment',
    '--allowed-site-root', root,
    '--allowed-entrypoint-prefix', root,
    '--allowed-entrypoint-prefix', dirname(process.execPath),
    '--attach-timeout-ms', '5000',
    '--tool-call-timeout-ms', '5000',
  ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let buffer = '';
  const pending = new Map<number, Pending>();
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (chunk) => {
    buffer += chunk;
    const lines = buffer.split(/\r?\n/);
    buffer = lines.pop() ?? '';
    for (const line of lines) {
      if (!line.trim()) continue;
      const message = JSON.parse(line);
      const waiter = pending.get(message.id);
      if (!waiter) continue;
      pending.delete(message.id);
      if (message.error) waiter.reject(new Error(JSON.stringify(message.error)));
      else waiter.resolve(message.result?.structuredContent ?? message.result);
    }
  });
  let nextId = 1;
  const call = (method: string, params: Record<string, unknown>) => new Promise<any>((resolvePromise, reject) => {
    const id = nextId++;
    pending.set(id, { resolve: resolvePromise, reject });
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    setTimeout(() => {
      if (!pending.has(id)) return;
      pending.delete(id);
      reject(new Error(`timeout:${method}`));
    }, 10000).unref();
  });
  return { child, call };
}

function writeFabric(root: string, servers: Record<string, unknown>) {
  mkdirSync(join(root, '.ai', 'mcp'), { recursive: true });
  const boundServers = Object.fromEntries(Object.entries(servers).map(([id, server]) => [
    id,
    { ...(server as Record<string, unknown>), binding_id: id },
  ]));
  writeFileSync(join(root, '.ai', 'mcp', 'config.json'), JSON.stringify({ mcpServers: boundServers }), 'utf8');
}

test('native loader attaches native_entrypoint and native_applet children', async (t) => {
  if (!existsSync(loaderExecutable)) {
    t.skip('native loader artifact is not built; run pnpm build:native first');
    return;
  }
  if (!existsSync(runtimeExecutable) || !existsSync(taskExecutable)) {
    t.skip('native runtime proxy artifact is not built; run the workspace native build first');
    return;
  }

  const root = mkdtempSync(join(tmpdir(), 'narada-loader-native-children-'));

  writeFabric(root, {
    'native-entrypoint': {
      command: runtimeExecutable,
      args: ['proxy', '--surface-id', 'native-entrypoint', '--child-command', taskExecutable, '--entrypoint', taskExecutable, '--child-invocation-kind', 'native_entrypoint', '--', '--site-root', root],
    },
    'native-applet': {
      command: runtimeExecutable,
      args: ['proxy', '--surface-id', 'native-applet', '--child-command', runtimeExecutable, '--entrypoint', runtimeExecutable, '--child-invocation-kind', 'native_applet', '--child-applet', 'filesystem', '--', '--mode', 'read', '--allowed-root', root, '--output-root', root],
    },
    'filesystem-user-home-anchor': {
      command: runtimeExecutable,
      args: ['proxy', '--surface-id', 'filesystem-user-home-anchor', '--child-command', runtimeExecutable, '--entrypoint', runtimeExecutable, '--child-invocation-kind', 'native_applet', '--child-applet', 'filesystem', '--', '--mode', 'read', '--allowed-root', root, '--anchored-allowed-root', 'user_home:.codex', '--output-root', root],
    },
  });

  const loader = startLoader(root);
  const opened: string[] = [];
  try {
    const initialized = await loader.call('initialize', { protocolVersion: '2024-11-05' });
    assert.equal(initialized.serverInfo.name, 'mcp-loader-mcp');

    const directlyAttached = await loader.call('tools/call', {
      name: 'mcp_loader_attach_surface',
      arguments: { site_root: root, binding_id: 'native-entrypoint', surface_id: 'native-entrypoint' },
    });
    assert.equal(directlyAttached.schema, 'narada.mcp_loader.surface_attached.v1');
    assert.equal(directlyAttached.tool_count > 0, true);
    assert.equal(Object.hasOwn(directlyAttached, 'tools'), false);
    assert.equal(directlyAttached.tool_discovery.tool_name, 'mcp_loader_list_tools');
    opened.push(directlyAttached.connection_id);

    const restarted = await loader.call('tools/call', {
      name: 'mcp_loader_surface_restart',
      arguments: { connection_id: directlyAttached.connection_id, reason: 'bounded response regression' },
    });
    assert.equal(restarted.status, 'restarted');
    assert.equal(Object.hasOwn(restarted, 'tools'), false);
    assert.equal(restarted.tool_discovery.tool_name, 'mcp_loader_list_tools');
    opened[opened.length - 1] = restarted.connection_id;

    const [firstInspection, secondInspection] = await Promise.all([
      loader.call('tools/call', {
        name: 'mcp_loader_inspect_binding_tool',
        arguments: { site_root: root, binding_id: 'native-entrypoint', tool_name: 'task_lifecycle_guidance' },
      }),
      loader.call('tools/call', {
        name: 'mcp_loader_inspect_binding_tool',
        arguments: { site_root: root, binding_id: 'native-entrypoint', tool_name: 'task_lifecycle_guidance' },
      }),
    ]);
    assert.equal(firstInspection.connection_id, restarted.connection_id);
    assert.equal(secondInspection.connection_id, restarted.connection_id);
    assert.equal(firstInspection.tool_contract_digest, firstInspection.tool_schema_digest);
    const digestAuthorizedCall = await loader.call('tools/call', {
      name: 'mcp_loader_call_binding_tool',
      arguments: {
        site_root: root,
        binding_id: 'native-entrypoint',
        tool_name: 'task_lifecycle_guidance',
        tool_contract_digest: firstInspection.tool_contract_digest,
        arguments: {},
      },
    });
    assert.equal(digestAuthorizedCall.schema, 'narada.mcp_loader.tool_result.v1');
    const inventoryAfterInspections = await loader.call('tools/call', {
      name: 'mcp_loader_connection_inventory',
      arguments: { compact: true },
    });
    assert.equal(inventoryAfterInspections.connection_count, 1);
    assert.equal(inventoryAfterInspections.compact, true);
    assert.equal(inventoryAfterInspections.runtime_freshness, null);
    assert.equal(Object.hasOwn(inventoryAfterInspections.connections[0], 'runtime_freshness'), false);
    assert.equal(digestAuthorizedCall.authorization_resolution, 'digest_reused');

    const batchInspection = await loader.call('tools/call', {
      name: 'mcp_loader_inspect_binding_tools',
      arguments: {
        site_root: root,
        binding_id: 'native-entrypoint',
        tool_names: ['task_lifecycle_guidance', 'task_lifecycle_bridge_poll'],
      },
    });
    assert.equal(batchInspection.schema, 'narada.mcp_loader.schema_lease_batch.v1');
    assert.equal(batchInspection.lease_count, 2);
    assert.equal(batchInspection.connection_id, restarted.connection_id);

    const nativeEntrypoint = await loader.call('tools/call', { name: 'mcp_loader_open_surface', arguments: { site_root: root, binding_id: 'native-entrypoint', surface_id: 'native-entrypoint' } });
    assert.equal(nativeEntrypoint.schema, 'narada.mcp_loader.surface_handle_opened.v1');
    opened.push(nativeEntrypoint.connection_id);
    const nativeEntrypointTools = await loader.call('tools/call', { name: 'mcp_loader_list_tools', arguments: { connection_id: nativeEntrypoint.connection_id } });
    assert.ok(nativeEntrypointTools.tools.some((tool: any) => tool.name === 'task_lifecycle_guidance'));

    const nativeApplet = await loader.call('tools/call', { name: 'mcp_loader_open_surface', arguments: { site_root: root, binding_id: 'native-applet', surface_id: 'native-applet' } });
    assert.equal(nativeApplet.schema, 'narada.mcp_loader.surface_handle_opened.v1');
    opened.push(nativeApplet.connection_id);
    const nativeAppletTools = await loader.call('tools/call', { name: 'mcp_loader_list_tools', arguments: { connection_id: nativeApplet.connection_id } });
    assert.ok(nativeAppletTools.tools.some((tool: any) => tool.name === 'fs_stat'));

    if (process.env.USERPROFILE || process.env.HOME) {
      const filesystem = await loader.call('tools/call', { name: 'mcp_loader_open_surface', arguments: { site_root: root, binding_id: 'filesystem-user-home-anchor', surface_id: 'filesystem-user-home-anchor' } });
      assert.equal(filesystem.schema, 'narada.mcp_loader.surface_handle_opened.v1');
      opened.push(filesystem.connection_id);
      const filesystemTools = await loader.call('tools/call', { name: 'mcp_loader_list_tools', arguments: { connection_id: filesystem.connection_id } });
      assert.ok(filesystemTools.tools.some((tool: any) => tool.name === 'fs_stat'));
    }

    await assert.rejects(
      loader.call('tools/call', {
        name: 'mcp_loader_open_surface',
        arguments: { site_root: root, binding_id: 'native-entrypoint', surface_id: 'native-entrypoint', entrypoint: taskExecutable },
      }),
      /entrypoint_not_allowed/,
    );
    await assert.rejects(
      loader.call('tools/call', {
        name: 'mcp_loader_open_surface',
        arguments: { site_root: root, binding_id: 'native-entrypoint', surface_id: 'native-entrypoint', args: ['--unexpected'] },
      }),
      /site_fabric_invocation_override_not_allowed/,
    );
  } finally {
    for (const connection_id of opened) {
      const detached = await loader.call('tools/call', { name: 'mcp_loader_detach', arguments: { connection_id } }).catch(() => undefined);
      if (detached) {
        assert.equal(detached.termination.classification, 'expected_protocol_detach');
        assert.equal(detached.termination.child_exit_is_crash, false);
      }
    }
    loader.child.kill();
    rmSync(root, { recursive: true, force: true });
  }
});
