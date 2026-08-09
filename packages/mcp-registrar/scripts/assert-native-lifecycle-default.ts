import assert from 'node:assert/strict';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  buildSiteBindConfig,
  type RegistrarSurfaceRecord,
} from '../src/main.js';

function record(value: unknown, label: string): Record<string, unknown> {
  assert.ok(value !== null && typeof value === 'object' && !Array.isArray(value), `${label} must be an object`);
  return value as Record<string, unknown>;
}

if (process.platform !== 'win32') {
  console.log(JSON.stringify({
    status: 'skipped',
    reason: 'native lifecycle artifacts are Windows-only',
  }));
} else {
  const root = fileURLToPath(new URL('../../../', import.meta.url));
  const surfaces = [
    {
      id: 'task-lifecycle',
      package: 'task-lifecycle-mcp',
      entrypoint: join(root, 'packages', 'task-lifecycle-mcp', 'dist', 'src', 'task-lifecycle', 'task-mcp-server.js'),
      kind: 'mcp_surface',
      args: ['--site-root', '{site_root}'],
      tools: ['task_lifecycle_doctor'],
    },
    {
      id: 'work-lifecycle',
      package: 'work-lifecycle-mcp',
      entrypoint: join(root, 'packages', 'work-lifecycle-mcp', 'dist', 'src', 'main.js'),
      kind: 'mcp_surface',
      args: ['--site-root', '{site_root}'],
      tools: ['work_lifecycle_doctor'],
    },
  ] satisfies RegistrarSurfaceRecord[];

  const results = surfaces.map((surface) => {
    const binding = buildSiteBindConfig({
      site_id: 'default-lifecycle-audit',
      root,
      config_path: join(root, 'default-lifecycle-audit.json'),
      surfaces: [],
    }, surface);
    const mcpServers = record(binding.config.mcpServers, 'mcpServers');
    const server = record(mcpServers[binding.serverKey], binding.serverKey);
    assert.ok(Array.isArray(server.args), `${surface.id}: server args`);
    const args = server.args.map(String);
    const childCommandIndex = args.indexOf('--child-command');
    const invocationKindIndex = args.indexOf('--child-invocation-kind');

    assert.ok(
      String(server.command).toLowerCase().endsWith('narada-mcp-runtime.exe'),
      `${surface.id}: native proxy`,
    );
    assert.ok(childCommandIndex >= 0, `${surface.id}: child command`);
    assert.ok(
      String(args[childCommandIndex + 1]).toLowerCase().endsWith(`narada-${surface.id}-mcp.exe`),
      `${surface.id}: Rust child`,
    );
    assert.ok(invocationKindIndex >= 0, `${surface.id}: invocation kind`);
    assert.equal(
      args[invocationKindIndex + 1],
      'native_entrypoint',
      `${surface.id}: invocation value`,
    );
    return {
      surface: surface.id,
      child_invocation_kind: args[invocationKindIndex + 1],
    };
  });

  console.log(JSON.stringify({ status: 'passed', results }));
}
