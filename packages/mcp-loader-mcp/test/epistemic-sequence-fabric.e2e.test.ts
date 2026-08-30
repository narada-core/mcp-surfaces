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
  installE2eArtifactRecorder,
} from '@narada-core/mcp-e2e-harness';
import { resolveNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';

const packageRoot = resolve(fileURLToPath(new URL('../..', import.meta.url)));
const workspaceRoot = resolve(packageRoot, '..', '..');
const executableSuffix = process.platform === 'win32' ? '.exe' : '';
const loaderExecutable = resolveNativeArtifact(packageRoot, `narada-mcp-loader${executableSuffix}`);
const registrarExecutable = resolveNativeArtifact(
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

test('registrar-bound epistemic graph survives loader restart', { timeout: 60_000 }, async (t) => {
  const resultPath = join(packageRoot, '.tmp-tests', 'e2e-results', 'epistemic-graph-fabric-e2e.json');
  const evidence = installE2eArtifactRecorder(resultPath, {
    test_id: 'ledger-domain-registrar-loader-query',
    surface: 'epistemic-graph',
    authority: 'A0',
    boundaries: ['B1', 'B2', 'B3'],
  });
  let outcome: 'passed' | 'failed' = 'failed';
  if (
    !loaderExecutable
    || !registrarExecutable
    || !existsSync(loaderExecutable)
    || !existsSync(registrarExecutable)
  ) {
    evidence.finalize({
      status: 'not_run',
      prerequisite: 'native loader and registrar artifacts',
      reason: 'native loader and registrar artifacts are required; run cargo native-package',
      cleanup: { status: 'not_required' },
    });
    t.skip('native loader and registrar artifacts are required; run cargo native-package');
    return;
  }

  const e2eRoot = createTemporaryE2eRoot('epistemic-graph-fabric');
  const isolation = createSiteFabricIsolation(e2eRoot);
  const siteId = 'epistemic-graph-fixture';
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
    label: 'epistemic graph fabric registrar',
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
    ], { cwd: workspaceRoot, env, label: 'epistemic graph fabric loader', timeoutMs: 15_000 });

    loader = startLoader();
    const opened = structured(await loader.client.request(2, 'tools/call', {
      name: 'mcp_loader_open_surface',
      arguments: { site_root: siteRoot, binding_id: bound.binding_id, surface_id: 'epistemic-graph' },
    }));
    assert.equal(opened.status, 'opened');
    let surfaceHandle = String(opened.surface_handle);
    const call = async (id: number, toolName: string, args: Record<string, unknown>) => {
      const inspected = structured(await loader!.client.request(id * 1000, 'tools/call', {
        name: 'mcp_loader_inspect_binding_tool',
        arguments: { site_root: siteRoot, binding_id: bound.binding_id, surface_id: 'epistemic-graph', tool_name: toolName },
      }));
      surfaceHandle = String((inspected.binding_resolution as Record<string, unknown>).surface_handle);
      return childResult(structured(await loader!.client.request(id, 'tools/call', {
        name: 'mcp_loader_call_surface_tool',
        arguments: { surface_handle: surfaceHandle, tool_name: toolName, schema_lease: inspected.schema_lease, arguments: args },
      })));
    };
    const authorityBasis = { kind: 'e2e_fixture', summary: 'Prove Site-bound epistemic graph behavior.' };
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

    const graph = await call(6, 'epistemic_graph_submit_review_admit', {
      actor: 'fixture-agent',
      authority_basis: authorityBasis,
      idempotency_key: 'fixture-graph-1',
      operations: [
        { op: 'entity.declare', entity_id: 'communication:root', kind: 'narada.epistemic:communication', title: 'Root', sender: 'marici.Caroline', recipient: 'marici.Grothendieck', body: 'Root body', intent: 'result', sent_at: '2026-08-20T00:00:00Z' },
        { op: 'entity.declare', entity_id: 'communication:reply', kind: 'narada.epistemic:communication', title: 'Reply', sender: 'marici.Benincasa', recipient: 'marici.Grothendieck', body: 'Reply body', intent: 'reply', sent_at: '2026-08-20T00:01:00Z' },
        { op: 'relation.declare', relation_id: 'relation:fixture-reply', relation_type: 'replies_to', source_id: 'communication:reply', target_id: 'communication:root' },
      ],
    });
    assert.equal(graph.admission?.status, 'admitted', JSON.stringify(graph));
    const inbox = await call(7, 'epistemic_graph_query', {
      template: 'epistemic:inbox', recipient: 'marici.Grothendieck', include_body: false, limit: 10,
    });
    assert.equal(inbox.query_mode, 'datalog', JSON.stringify(inbox));
    assert.equal(inbox.count, 2, JSON.stringify(inbox));
    assert.ok(inbox.items.every((item: any) => item.body === undefined), JSON.stringify(inbox));
    const thread = await call(8, 'epistemic_graph_query', {
      template: 'epistemic:thread', root: 'communication:root', max_depth: 1, limit: 10,
    });
    assert.equal(thread.count, 1, JSON.stringify(thread));
    assert.equal(thread.items[0].entity_id, 'communication:reply', JSON.stringify(thread));

    const markedRead = await call(12, 'epistemic_graph_message_mark_read', {
      message_id: 'communication:root',
      reader: 'marici.Grothendieck',
      actor: 'fixture-agent',
      authority_basis: authorityBasis,
    });
    assert.equal(markedRead.status, 'read', JSON.stringify(markedRead));
    const unread = await call(13, 'epistemic_graph_query', {
      template: 'inbox',
      recipient: 'marici.Grothendieck',
      match: { read_state: 'unread', reply_state: 'unreplied' },
      limit: 10,
    });
    assert.equal(unread.count, 1, JSON.stringify(unread));
    assert.equal(unread.items[0].entity_id, 'communication:reply', JSON.stringify(unread));
    assert.equal(unread.items[0].message_state.status, 'unread', JSON.stringify(unread));

    await loader.close();
    loader = startLoader();
    const reopened = structured(await loader.client.request(9, 'tools/call', {
      name: 'mcp_loader_open_surface',
      arguments: { site_root: siteRoot, binding_id: bound.binding_id, surface_id: 'epistemic-graph' },
    }));
    surfaceHandle = String(reopened.surface_handle);
    const status = await call(10, 'epistemic_graph_sequence_status', { sequence_name: 'fixture-records' });
    assert.equal(status.next_value, 8);
    assert.equal(status.claim_count, 1);
    const persistedInbox = await call(11, 'epistemic_graph_query', {
      template: 'inbox', recipient: 'marici.Grothendieck', limit: 10,
    });
    assert.equal(persistedInbox.count, 2, JSON.stringify(persistedInbox));
    assert.ok(persistedInbox.items.some((item: any) => item.body === 'Root body'), JSON.stringify(persistedInbox));
    const persistedRead = await call(15, 'epistemic_graph_query', {
      template: 'inbox', recipient: 'marici.Grothendieck', read_state: 'read', limit: 10,
    });
    assert.equal(persistedRead.count, 1, JSON.stringify(persistedRead));
    assert.equal(persistedRead.items[0].message_state.status, 'read', JSON.stringify(persistedRead));
    outcome = 'passed';
  } finally {
    if (loader) await loader.close();
    await registrar.close();
    assert.equal(removeTemporaryE2eRoot(e2eRoot), true);
    evidence.finalize({
      status: outcome,
      site_id: siteId,
      binding_id: `${siteId}-epistemic-graph`,
      cleanup: { status: 'passed', temporary_root_removed: true, children_closed: true },
    });
  }
});
