import assert from 'node:assert/strict';
import { buildSiteBindConfig } from '../dist/src/main.js';

if (process.platform !== 'win32') {
  console.log(JSON.stringify({ status: 'skipped', reason: 'native lifecycle artifacts are Windows-only' }));
  process.exit(0);
}

const root = process.cwd();
const surfaces = [
  {
    id: 'task-lifecycle',
    package: 'task-lifecycle-mcp',
    entrypoint: `${root}/packages/task-lifecycle-mcp/dist/src/task-lifecycle/task-mcp-server.js`,
    kind: 'mcp_surface',
    args: ['--site-root', '{site_root}'],
    tools: ['task_lifecycle_doctor'],
  },
  {
    id: 'work-lifecycle',
    package: 'work-lifecycle-mcp',
    entrypoint: `${root}/packages/work-lifecycle-mcp/dist/src/main.js`,
    kind: 'mcp_surface',
    args: ['--site-root', '{site_root}'],
    tools: ['work_lifecycle_doctor'],
  },
];

const results = surfaces.map((surface) => {
  const binding = buildSiteBindConfig({
    site_id: 'default-lifecycle-audit',
    root,
    config_path: `${root}/default-lifecycle-audit.json`,
    surfaces: [],
  }, surface);
  const server = binding.config.mcpServers[binding.serverKey];
  const args = server.args.map(String);
  const childCommandIndex = args.indexOf('--child-command');
  const invocationKindIndex = args.indexOf('--child-invocation-kind');
  assert.ok(String(server.command).toLowerCase().endsWith('narada-mcp-runtime.exe'), `${surface.id}: native proxy`);
  assert.ok(childCommandIndex >= 0, `${surface.id}: child command`);
  assert.ok(String(args[childCommandIndex + 1]).toLowerCase().endsWith(`narada-${surface.id}-mcp.exe`), `${surface.id}: Rust child`);
  assert.ok(invocationKindIndex >= 0, `${surface.id}: invocation kind`);
  assert.equal(args[invocationKindIndex + 1], 'native_entrypoint', `${surface.id}: invocation value`);
  return { surface: surface.id, child_invocation_kind: args[invocationKindIndex + 1] };
});

console.log(JSON.stringify({ status: 'passed', results }));