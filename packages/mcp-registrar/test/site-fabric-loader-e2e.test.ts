import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import {
  createTemporaryE2eRoot,
  removeTemporaryE2eRoot,
  runMcpProtocolSmoke,
  siteFabricChildEnv,
  spawnJsonlMcpServer,
  type JsonRecord,
} from '@narada-core/mcp-e2e-harness';

const siteRoot = createTemporaryE2eRoot('registrar-loader-site-fabric-e2e');
const loaderPath = fileURLToPath(new URL('../../../mcp-loader-mcp/dist/src/main.js', import.meta.url));
const mailboxPath = fileURLToPath(new URL('../../../mailbox-mcp/dist/src/main.js', import.meta.url));

mkdirSync(`${siteRoot}/.ai/mcp`, { recursive: true });
writeFileSync(`${siteRoot}/.ai/mcp/fixture-mailbox-mcp.json`, JSON.stringify({
  schema: 'narada.mcp.client_config.v0',
  site_id: 'registrar-loader-site-fabric-e2e',
  mcpServers: {
    'fixture-mailbox': {
      command: 'node',
      args: [mailboxPath, '--site-root', siteRoot],
      surface_id: 'mailbox',
      tools: ['mailbox_guidance', 'mailbox_doctor', 'mailbox_output_show'],
    },
  },
}, null, 2), 'utf8');

const loader = spawnJsonlMcpServer(process.execPath, [
  loaderPath,
  '--standalone-ambient-attachment',
  '--allowed-site-root', siteRoot,
  '--allowed-entrypoint-prefix', fileURLToPath(new URL('../../../', import.meta.url)),
], {
  cwd: siteRoot,
  env: siteFabricChildEnv(siteRoot),
  label: 'registrar-to-loader Site fabric e2e',
});

function structured(response: JsonRecord): JsonRecord {
  assert.equal(response.error, undefined, JSON.stringify(response));
  return (response.result as JsonRecord)?.structuredContent as JsonRecord ?? response.result as JsonRecord;
}

try {
  await runMcpProtocolSmoke(loader.client, {
    expectedServerName: 'mcp-loader-mcp',
    requiredTools: ['mcp_loader_list_site_surfaces', 'mcp_loader_attach_surface', 'mcp_loader_call_tool', 'mcp_loader_surface_status', 'mcp_loader_detach'],
  });

  const listed = structured(await loader.client.request(1, 'tools/call', {
    name: 'mcp_loader_list_site_surfaces',
    arguments: { site_root: siteRoot },
  }));
  assert.equal(listed.schema, 'narada.mcp_loader.site_surfaces.v1');
  assert.equal((listed.surfaces as JsonRecord[]).some((item) => item.surface_id === 'mailbox'), true);

  const attached = structured(await loader.client.request(2, 'tools/call', {
    name: 'mcp_loader_attach_surface',
    arguments: { site_root: siteRoot, surface_id: 'fixture-mailbox' },
  }));
  assert.equal(attached.schema, 'narada.mcp_loader.surface_attached.v1', JSON.stringify(attached));
  const connectionId = String(attached.connection_id);
  assert.ok(connectionId);

  const status = structured(await loader.client.request(3, 'tools/call', {
    name: 'mcp_loader_surface_status',
    arguments: { connection_id: connectionId },
  }));
  assert.equal(status.status, 'live', JSON.stringify(status));
  assert.equal(status.surface_id, 'fixture-mailbox');

  const forwarded = structured(await loader.client.request(4, 'tools/call', {
    name: 'mcp_loader_call_tool',
    arguments: { connection_id: connectionId, tool_name: 'mailbox_guidance', arguments: {} },
  }));
  assert.equal(forwarded.schema, 'narada.mcp_loader.tool_result.v1', JSON.stringify(forwarded));
  assert.equal((forwarded.result as JsonRecord)?.structuredContent != null, true);

  const detached = structured(await loader.client.request(5, 'tools/call', {
    name: 'mcp_loader_detach',
    arguments: { connection_id: connectionId },
  }));
  assert.equal((detached.termination as JsonRecord)?.status, 'terminated', JSON.stringify(detached));

  const staleCall = await loader.client.request(6, 'tools/call', {
    name: 'mcp_loader_call_tool',
    arguments: { connection_id: connectionId, tool_name: 'mailbox_guidance', arguments: {} },
  });
  assert.equal((staleCall.error?.data as JsonRecord)?.code, 'connection_not_found', JSON.stringify(staleCall));

  console.log(JSON.stringify({ status: 'passed', test_id: 'mcp-registrar.loader.site-fabric', cleanup: 'completed' }));
} finally {
  await loader.close();
  assert.equal(removeTemporaryE2eRoot(siteRoot), true);
}

console.log('mcp-registrar to mcp-loader Site fabric e2e ok');
