import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  createTemporaryE2eRoot,
  installE2eArtifactRecorder,
  removeTemporaryE2eRoot,
  runMcpProtocolSmoke,
  siteFabricChildEnv,
  spawnJsonlMcpServer,
  structured,
  type JsonRecord,
} from '@narada-core/mcp-e2e-harness';

const siteRoot = createTemporaryE2eRoot('registrar-catalog-site-fabric-e2e');
const resultPath = join(fileURLToPath(new URL('../..', import.meta.url)), '.tmp', 'e2e-results', 'mcp-registrar.site-fabric.full-catalog-sweep.json');
const evidence = installE2eArtifactRecorder(resultPath, { test_id: 'mcp-registrar.site-fabric.full-catalog-sweep', authority: 'A0' });
const serverPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
const loaderPath = fileURLToPath(new URL('../../../mcp-loader-mcp/dist/src/main.js', import.meta.url));
const server = spawnJsonlMcpServer(process.execPath, [serverPath, '--narada-root', siteRoot], {
  cwd: siteRoot,
  env: siteFabricChildEnv(siteRoot, { NARADA_ROOT: siteRoot }),
  label: 'mcp-registrar catalog Site fabric e2e',
});
let loader: ReturnType<typeof spawnJsonlMcpServer> | null = null;

function interpolateCatalogArg(value: string): string {
  return value
    .replace(/\{site_root\}/g, siteRoot)
    .replace(/\{workspace_root\}/g, siteRoot)
    .replace(/\{site_runtime_root\}/g, join(siteRoot, '.narada', 'runtime'))
    .replace(/\{site_id\}/g, 'registrar-catalog-site-fabric-e2e');
}

try {
  await runMcpProtocolSmoke(server.client, {
    expectedServerName: 'mcp-registrar',
    requiredTools: ['registrar_surface_list', 'registrar_surface_tool_inventory_check'],
  });

  const catalog = structured(await server.client.request(3, 'tools/call', {
    name: 'registrar_surface_list',
    arguments: {},
  }));
  const items = catalog.items as JsonRecord[];
  assert.ok(items.length > 10, JSON.stringify(catalog));
  assert.equal(catalog.count, items.length, JSON.stringify(catalog));
  const ids = items.map((item) => String(item.id));
  assert.equal(new Set(ids).size, ids.length, JSON.stringify(ids));
  for (const item of items) {
    assert.ok(String(item.package), JSON.stringify(item));
    assert.ok(String(item.entrypoint), JSON.stringify(item));
    const tools = item.tools as unknown[];
    assert.ok(Array.isArray(tools) && tools.length > 0, JSON.stringify(item));
    assert.equal(new Set(tools.map(String)).size, tools.length, JSON.stringify(item));
  }

  const workLifecycle = items.find((item) => item.id === 'work-lifecycle');
  assert.ok(workLifecycle, 'work-lifecycle must be present in the registrar catalog');
  const preparation = spawnSync(process.execPath, [
    String(workLifecycle.entrypoint),
    '--prepare',
    '--site-root',
    siteRoot,
  ], {
    cwd: siteRoot,
    env: siteFabricChildEnv(siteRoot),
    encoding: 'utf8',
    windowsHide: true,
    timeout: 30_000,
    maxBuffer: 128 * 1024,
  });
  assert.equal(preparation.status, 0, preparation.stderr || preparation.stdout);
  assert.equal((JSON.parse(preparation.stdout.trim()) as JsonRecord).status, 'prepared');

  writeFileSync(join(siteRoot, 'AGENTS.md'), '# Registrar catalog Site fabric e2e\n', 'utf8');
  mkdirSync(join(siteRoot, '.ai', 'mcp'), { recursive: true });
  const mcpServers = Object.fromEntries(items.map((item) => {
    const surfaceId = String(item.id);
    const entrypoint = String(item.entrypoint);
    const args = Array.isArray(item.args) ? item.args.map(String).map(interpolateCatalogArg) : [];
    return [surfaceId, {
      command: process.execPath,
      args: [entrypoint, ...args],
      surface_id: surfaceId,
      tools: item.tools,
    }];
  }));
  writeFileSync(join(siteRoot, '.ai', 'mcp', 'config.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    site_id: 'registrar-catalog-site-fabric-e2e',
    mcpServers,
  }, null, 2), 'utf8');

  loader = spawnJsonlMcpServer(process.execPath, [
    loaderPath,
    '--standalone-ambient-attachment',
    '--allowed-site-root', siteRoot,
    '--allowed-entrypoint-prefix', fileURLToPath(new URL('../../../', import.meta.url)),
    '--attach-timeout-ms', '120000',
  ], {
    cwd: siteRoot,
    env: siteFabricChildEnv(siteRoot),
    label: 'registrar catalog-to-loader Site fabric e2e',
    timeoutMs: 180_000,
    closeTimeoutMs: 5_000,
  });
  await runMcpProtocolSmoke(loader.client, {
    expectedServerName: 'mcp-loader-mcp',
    requiredTools: ['mcp_loader_list_site_surfaces', 'mcp_loader_site_tool_inventory_check'],
  });

  const listed = structured(await loader.client.request(5, 'tools/call', {
    name: 'mcp_loader_list_site_surfaces',
    arguments: { site_root: siteRoot },
  }));
  assert.equal(listed.schema, 'narada.mcp_loader.site_surfaces.v1', JSON.stringify(listed));
  assert.deepEqual(
    (listed.surfaces as JsonRecord[]).map((item) => String(item.surface_id)).sort(),
    ids.slice().sort(),
    JSON.stringify(listed),
  );

  const liveObservation = structured(await loader.client.request(6, 'tools/call', {
    name: 'mcp_loader_site_tool_inventory_check',
    arguments: { site_root: siteRoot, surface_ids: ids, include_ok: true },
  }));
  assert.equal(liveObservation.schema, 'narada.mcp_loader.site_tool_inventory_check.v1', JSON.stringify(liveObservation));
  assert.equal(liveObservation.status, 'ok', JSON.stringify(liveObservation));
  assert.equal(liveObservation.violation_count, 0, JSON.stringify(liveObservation));
  assert.deepEqual(liveObservation.observed_surface_ids, ids.slice().sort(), JSON.stringify(liveObservation));
  assert.deepEqual(liveObservation.unobserved_surface_ids, [], JSON.stringify(liveObservation));
  const liveFindings = liveObservation.findings as JsonRecord[];
  assert.equal(liveFindings.length, items.length, JSON.stringify(liveObservation));
  assert.equal(liveFindings.every((finding) => finding.status === 'ok'), true, JSON.stringify(liveObservation));

  const observedTools = liveObservation.observed_tools as JsonRecord;
  const inventory = structured(await server.client.request(4, 'tools/call', {
    name: 'registrar_surface_tool_inventory_check',
    arguments: { include_ok: true, observed_tools: observedTools },
  }));
  assert.equal(inventory.status, 'ok', JSON.stringify(inventory));
  assert.equal(inventory.checked_count, items.length, JSON.stringify(inventory));
  assert.equal((inventory.findings as JsonRecord[]).length, items.length, JSON.stringify(inventory));
  assert.equal((inventory.findings as JsonRecord[]).every((finding) => finding.status === 'ok'), true, JSON.stringify(inventory));

  console.log(JSON.stringify({
    status: 'passed',
    test_id: 'mcp-registrar.site-fabric.full-catalog-sweep',
    authority: 'A0',
    live_loader_admission: true,
    live_entrypoint_count: items.length,
    observation_ref: liveObservation.observation_ref,
    catalog_count: items.length,
    live_child: true,
    cleanup: 'completed_after_finally',
  }));
  evidence.update({
    status: 'passed',
    live_loader_admission: true,
    live_entrypoint_count: items.length,
    observation_ref: liveObservation.observation_ref,
  });
} finally {
  if (loader) await loader.close();
  await server.close();
  const cleanupOk = removeTemporaryE2eRoot(siteRoot);
  evidence.finalize({ status: cleanupOk ? 'passed' : 'failed', cleanup: { status: cleanupOk ? 'completed_after_finally' : 'failed' } });
  assert.equal(cleanupOk, true);
}
