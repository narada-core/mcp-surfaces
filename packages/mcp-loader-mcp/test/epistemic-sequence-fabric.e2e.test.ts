import assert from 'node:assert/strict';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import {
  createSiteFabricIsolation,
  createTemporaryE2eRoot,
  removeTemporaryE2eRoot,
  siteFabricChildEnv,
  spawnContentLengthMcpServer,
  spawnJsonlMcpServer,
  structured,
} from '@narada-core/mcp-e2e-harness';
import { requireNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';

const packageRoot = resolve(fileURLToPath(new URL('../..', import.meta.url)));
const workspaceRoot = resolve(packageRoot, '..', '..');
const executableSuffix = process.platform === 'win32' ? '.exe' : '';
const loaderExecutable = requireNativeArtifact(packageRoot, `narada-mcp-loader${executableSuffix}`);
const registrarExecutable = requireNativeArtifact(
  resolve(workspaceRoot, 'packages', 'mcp-registrar'),
  `narada-mcp-registrar${executableSuffix}`,
);

function createSiteRegistry(path: string, siteId: string, siteRoot: string): void {
  const registry = new DatabaseSync(path);
  registry.exec('CREATE TABLE site_registry (site_id TEXT PRIMARY KEY, site_root TEXT NOT NULL, created_at TEXT NOT NULL, lifecycle_status TEXT NOT NULL)');
  registry.prepare('INSERT INTO site_registry VALUES (?, ?, ?, ?)').run(siteId, siteRoot, '2026-08-18T00:00:00Z', 'active');
  registry.close();
}

function childResult(value: Record<string, any>): Record<string, any> {
  return value.result?.structuredContent ?? value.result ?? {};
}

test('registrar-bound epistemic sequence survives loader restart', { timeout: 60_000 }, async (t) => {
  if (!existsSync(loaderExecutable) || !existsSync(registrarExecutable)) {
    t.skip('native loader and registrar artifacts are required; run cargo native-package');
    return;
  }

  const e2eRoot = createTemporaryE2eRoot('epistemic-sequence-fabric');
  const isolation = createSiteFabricIsolation(e2eRoot);
  const siteId = 'epistemic-sequence-fixture';
  const siteRoot = join(e2eRoot, 'site');
  const registryPath = join(isolation.userSiteRoot, 'registry.db');
  mkdirSync(join(siteRoot, '.narada'), { recursive: true });
  writeFileSync(join(siteRoot, '.narada', 'site.json'), JSON.stringify({ site_id: siteId }), 'utf8');
  writeFileSync(join(siteRoot, '.narada', 'config.json'), JSON.stringify({ site_id: siteId }), 'utf8');
  createSiteRegistry(registryPath, siteId, siteRoot);
  const env = siteFabricChildEnv(e2eRoot, {
    ...isolation.env,
    NARADA_SITE_REGISTRY_DB: registryPath,
  });

  const registrar = spawnContentLengthMcpServer(registrarExecutable, [], {
    cwd: workspaceRoot,
    env,
    label: 'epistemic sequence fabric registrar',
    protocolMode: 'modern',
    timeoutMs: 15_000,
  });
  let loader: ReturnType<typeof spawnJsonlMcpServer> | null = null;
  try {
    const bound = structured(await registrar.client.request(1, 'tools/call', {
      name: 'registrar_site_bind',
      arguments: { site_id: siteId, surface_id: 'epistemic-graph', projection_id: 'default' },
    }));
    assert.equal(bound.status, 'bound');
    assert.equal(bound.binding_id, `${siteId}-epistemic-graph`);

    const startLoader = () => spawnJsonlMcpServer(loaderExecutable, [
      '--standalone-ambient-attachment',
      '--allowed-site-root', siteRoot,
      '--allowed-entrypoint-prefix', workspaceRoot,
      '--attach-timeout-ms', '10000',
      '--tool-call-timeout-ms', '10000',
    ], { cwd: workspaceRoot, env, label: 'epistemic sequence fabric loader', timeoutMs: 15_000 });

    loader = startLoader();
    const opened = structured(await loader.client.request(2, 'tools/call', {
      name: 'mcp_loader_open_surface',
      arguments: { site_root: siteRoot, binding_id: bound.binding_id, surface_id: 'epistemic-graph' },
    }));
    assert.equal(opened.status, 'opened');
    const surfaceHandle = String(opened.surface_handle);
    const call = async (id: number, toolName: string, args: Record<string, unknown>) => childResult(structured(
      await loader!.client.request(id, 'tools/call', {
        name: 'mcp_loader_call_surface_tool',
        arguments: { surface_handle: surfaceHandle, tool_name: toolName, arguments: args },
      }),
    ));
    const authorityBasis = { kind: 'e2e_fixture', summary: 'Prove Site-bound sequence allocation.' };
    const created = await call(3, 'epistemic_graph_sequence_create', {
      sequence_name: 'fixture-records', actor: 'fixture-agent', authority_basis: authorityBasis, start_at: 7,
    });
    assert.equal(created.status, 'created');
    const claim = await call(4, 'epistemic_graph_sequence_claim_next', {
      sequence_name: 'fixture-records', actor: 'fixture-agent', authority_basis: authorityBasis, idempotency_key: 'fixture-claim-1',
    });
    assert.equal(claim.value, 7);
    const replay = await call(5, 'epistemic_graph_sequence_claim_next', {
      sequence_name: 'fixture-records', actor: 'fixture-agent', authority_basis: authorityBasis, idempotency_key: 'fixture-claim-1',
    });
    assert.equal(replay.value, 7);
    assert.equal(replay.idempotency_replay, true);

    await loader.close();
    loader = startLoader();
    const reopened = structured(await loader.client.request(6, 'tools/call', {
      name: 'mcp_loader_open_surface',
      arguments: { site_root: siteRoot, binding_id: bound.binding_id, surface_id: 'epistemic-graph' },
    }));
    const statusCall = structured(await loader.client.request(7, 'tools/call', {
      name: 'mcp_loader_call_surface_tool',
      arguments: {
        surface_handle: reopened.surface_handle,
        tool_name: 'epistemic_graph_sequence_status',
        arguments: { sequence_name: 'fixture-records' },
      },
    }));
    const status = childResult(statusCall);
    assert.equal(status.next_value, 8);
    assert.equal(status.claim_count, 1);
  } finally {
    if (loader) await loader.close();
    await registrar.close();
    assert.equal(removeTemporaryE2eRoot(e2eRoot), true);
  }
});
