import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { assertLoaderRuntimeFreshnessCurrent, assertSurfaceLaunchMetadata, classifyLoaderRuntimeFreshness, createServerState, handleRequest } from '../src/main.js';
import { bindingAdmissionEntryDigest, bindingAdmissionEnvelopeDigest } from '@narada-core/mcp-fabric-contracts';
import { runMcpProtocolSmoke, spawnJsonlMcpServer } from '@narada-core/mcp-e2e-harness';
import { resolveNativeArtifact, requireNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';

const root = mkdtempSync(join(tmpdir(), 'mcp-loader-mcp-protocol-'));
const serverPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
const nativeLoader = process.env.MCP_LOADER_NATIVE === '1';
const nativeExecutable = nativeLoader ? requireNativeArtifact(resolve(dirname(serverPath), '..', '..'), process.platform === 'win32' ? 'narada-mcp-loader.exe' : 'narada-mcp-loader') : '';
const loaderCommand = nativeLoader ? nativeExecutable : process.execPath;
const loaderArgs = nativeLoader ? [] : [serverPath];
const server = spawnJsonlMcpServer(loaderCommand, [...loaderArgs, '--standalone-ambient-attachment', '--allowed-site-root', root], { label: nativeLoader ? 'mcp-loader-mcp native protocol smoke' : 'mcp-loader-mcp protocol smoke' });

try {
  const defaultState = createServerState();
  const unauthorisedState = createServerState({ allowedSiteRoots: [root] });
  const duplicateRootState = createServerState({
    allowedSiteRoots: process.platform === 'win32' ? [root, root.toUpperCase()] : [root, root],
  });
  assert.equal(duplicateRootState.policy.allowedSiteRoots.length, 1);
  const unauthorised = await handleRequest({ jsonrpc: '2.0', id: 991, method: 'tools/call', params: { name: 'mcp_loader_list_site_surfaces', arguments: { site_root: root } } }, unauthorisedState) as any;
  assert.equal(unauthorised.error?.data?.code, 'mcp_binding_admission_required');

  const governedRoot = join(root, 'governed');
  const governedMcp = join(governedRoot, '.ai', 'mcp');
  mkdirSync(governedMcp, { recursive: true });
  const serverRecord: any = {
    binding_id: 'governed-echo', surface_id: 'echo', projection_id: 'default', transport: 'stdio',
    command: process.execPath, args: [], env: {}, env_vars: [], target_site_root: governedRoot.replaceAll('\\', '/'),
    injection_scope: 'local_site', authority_locus: { kind: 'local_site', site_root: governedRoot.replaceAll('\\', '/') },
  };
  writeFileSync(join(governedMcp, 'config.json'), JSON.stringify({ mcpServers: { 'narada-governed-echo': serverRecord } }));
  const unsignedEntry: any = {
    binding_id: 'governed-echo', surface_id: 'echo', projection_id: 'default',
    authority_locus: serverRecord.authority_locus, injection_scope: 'local_site', operations: ['attach', 'discover', 'restart'],
  };
  const launchIdentity = {
    schema: 'narada.mcp.binding_identity.v1', binding_id: 'governed-echo', surface_id: 'echo', projection_id: 'default', injection_scope: 'local_site',
    authority_locus: serverRecord.authority_locus, transport: 'stdio', command: process.execPath, args: [], env: {}, env_vars: [],
    target_site_root: serverRecord.target_site_root, surface_projection: null,
  };
  const entry = { ...unsignedEntry, binding_identity: launchIdentity, binding_digest: bindingAdmissionEntryDigest({ ...unsignedEntry, launch_identity: launchIdentity }) };
  const unsignedEnvelope: any = {
    schema: 'narada.mcp.binding_admission_envelope.v1', envelope_id: 'governed-envelope', decision: 'admitted',
    issued_at: new Date().toISOString(), valid_until: null, principal_key: 'local:test:agent', site_id: 'test-site',
    carrier_session_id: 'test-session', carrier_kind: 'codex', runtime_kind: 'test-runtime', authority_epoch: 1,
    carrier_session_admission_receipt_ref: 'receipt:test', authority_readback_ref: 'authority:test',
    fabric_digest: 'a'.repeat(64), bindings: [entry],
  };
  const envelopePath = join(governedRoot, 'binding-admission.json');
  writeFileSync(envelopePath, JSON.stringify({ ...unsignedEnvelope, envelope_digest: bindingAdmissionEnvelopeDigest(unsignedEnvelope) }));
  const governedState = createServerState({ allowedSiteRoots: [governedRoot], bindingAdmissionPath: envelopePath });
  const listed = await handleRequest({ jsonrpc: '2.0', id: 992, method: 'tools/call', params: { name: 'mcp_loader_list_site_surfaces', arguments: { site_root: governedRoot } } }, governedState) as any;
  assert.equal(listed.error, undefined, JSON.stringify(listed));
  assert.deepEqual(listed.result?.structuredContent?.surfaces?.map((surface: any) => surface.binding_id) ?? listed.result?.surfaces?.map((surface: any) => surface.binding_id), ['governed-echo']);
  if (process.platform === 'win32') {
    const differentlyCasedRoot = governedRoot.toUpperCase();
    const caseInsensitive = await handleRequest({ jsonrpc: '2.0', id: 994, method: 'tools/call', params: { name: 'mcp_loader_list_site_surfaces', arguments: { site_root: differentlyCasedRoot } } }, governedState) as any;
    assert.equal(caseInsensitive.error, undefined, JSON.stringify(caseInsensitive));
  }
  const absent = await handleRequest({ jsonrpc: '2.0', id: 993, method: 'tools/call', params: { name: 'mcp_loader_attach_surface', arguments: { site_root: governedRoot, binding_id: 'not-admitted' } } }, governedState) as any;
  assert.equal(absent.error?.data?.code, 'mcp_binding_not_admitted');
  const surfacesRoot = resolve(dirname(serverPath), '..', '..', '..');
  const userProfile = process.env.USERPROFILE || process.env.HOME;
  assert.ok(defaultState.policy.allowedSiteRoots.includes(resolve(surfacesRoot, '..').replace(/\\/g, '/')));
  assert.ok(defaultState.policy.allowedEntrypointPrefixes.includes(surfacesRoot.replace(/\\/g, '/')));
  if (userProfile) {
    assert.ok(defaultState.policy.allowedSiteRoots.includes(resolve(userProfile, 'Narada').replace(/\\/g, '/')));
    assert.ok(defaultState.policy.allowedEntrypointPrefixes.includes(resolve(userProfile, 'Narada', 'tools').replace(/\\/g, '/')));
    assert.ok(defaultState.policy.allowedEnvVars.includes(process.env.USERPROFILE ? 'USERPROFILE' : 'HOME'));
  }
  const previousSourceRoot = process.env.NARADA_SRC_ROOT;
  process.env.NARADA_SRC_ROOT = join(root, 'source-root');
  try {
    const sourceRootState = createServerState();
    assert.ok(sourceRootState.policy.allowedSiteRoots.includes(resolve(root, 'source-root').replace(/\\/g, '/')));
    assert.ok(sourceRootState.policy.allowedEnvVars.includes('NARADA_SRC_ROOT'));
  } finally {
    if (previousSourceRoot === undefined) delete process.env.NARADA_SRC_ROOT;
    else process.env.NARADA_SRC_ROOT = previousSourceRoot;
  }

  const syntheticObservation = (path: string, mtime_ms: number) => ({
    path,
    exists: true,
    mtime_ms,
    mtime: new Date(mtime_ms).toISOString(),
  });
  const syntheticFreshness = classifyLoaderRuntimeFreshness({
    processStartedAtMs: 100,
    filePairs: [
      {
        name: 'loader_entrypoint',
        source: syntheticObservation('loader/main.ts', 50),
        runtime: syntheticObservation('loader/main.js', 50),
      },
      {
        name: 'mcp_transport',
        source: syntheticObservation('transport/mcp-payload-file.ts', 200),
        runtime: syntheticObservation('transport/mcp-payload-file.js', 50),
      },
    ],
    configFiles: [
      { name: 'workspace_lockfile', observation: syntheticObservation('pnpm-lock.yaml', 150) },
    ],
  });
  assert.equal(syntheticFreshness.status, 'current');
  assert.equal(syntheticFreshness.reload_required, false);
  assert.deepEqual(syntheticFreshness.reasons, []);
  assert.equal(syntheticFreshness.freshness_scope, 'native_loader_artifact');
  assert.equal((syntheticFreshness.reload_action as Record<string, unknown>).schema, 'narada.mcp_loader.supervisor_restart_action.v1');
  assert.equal((assertLoaderRuntimeFreshnessCurrent({ status: 'current' }, 'test_current').status), 'current');
  assert.equal(assertLoaderRuntimeFreshnessCurrent(syntheticFreshness, 'test_native_current').status, 'current');
  assert.throws(
    () => assertSurfaceLaunchMetadata('entrypoint', 'C:/native/narada-mcp-runtime.exe', 'node'),
    /surface_native_invocation_metadata_missing/,
  );
  assert.doesNotThrow(() => assertSurfaceLaunchMetadata('native_entrypoint', 'C:/native/narada-mcp-runtime.exe', 'C:/native/narada-mcp-runtime.exe'));
  const protocol = await runMcpProtocolSmoke(server.client, { expectedServerName: 'mcp-loader-mcp' });
  const tools = protocol.tools.tools as { name: string; description: string; annotations: Record<string, unknown>; inputSchema: Record<string, any>; outputSchema: Record<string, any> }[];
  assert.deepEqual(tools.map((t) => t.name), [
    'mcp_loader_guidance',
    'mcp_loader_runtime_status',
    'mcp_loader_policy_inspect',
    'mcp_loader_connection_inventory',
    'mcp_loader_process_ownership',
    'mcp_loader_topology_diagnostics',
    'mcp_loader_runtime_observation',
    'mcp_loader_list_site_surfaces',
    'mcp_loader_site_fabric_diagnostics',
    'mcp_loader_site_tool_inventory_check',
    'mcp_loader_attach_surface',
    'mcp_loader_open_surface',
    'mcp_loader_surface_handle_inventory',
    'mcp_loader_list_tools',
    'mcp_loader_surface_status',
    'mcp_loader_tool_discovery_manifest',
    'mcp_loader_call_tool',
    'mcp_loader_call_surface_tool',
    'mcp_loader_read_result',
    'mcp_loader_detach',
    'mcp_loader_surface_restart',
  ]);

  const guidanceTool = tools.find((t) => t.name === 'mcp_loader_guidance');
  assert.equal(guidanceTool?.description, 'Show model-facing operating guidance for mcp-loader MCP workflows.');
  assert.equal(guidanceTool?.annotations.readOnlyHint, true);
  assert.equal(guidanceTool?.annotations.idempotentHint, true);
  assert.equal(guidanceTool?.annotations.openWorldHint, false);
  assert.deepEqual(guidanceTool?.inputSchema.properties, {
    workflow: { type: 'string', description: 'Optional workflow name or area to focus guidance on.' },
    tool: { type: 'string', description: 'Optional tool name for tool-specific guidance.' },
  });

  const listTool = tools.find((t) => t.name === 'mcp_loader_list_site_surfaces');
  assert.equal(listTool?.annotations.readOnlyHint, true);

  const connectionInventoryTool = tools.find((t) => t.name === 'mcp_loader_connection_inventory');
  assert.equal(connectionInventoryTool?.annotations.readOnlyHint, true);

  const processOwnershipTool = tools.find((t) => t.name === 'mcp_loader_process_ownership');
  assert.equal(processOwnershipTool?.annotations.readOnlyHint, true);

  const topologyDiagnosticsTool = tools.find((t) => t.name === 'mcp_loader_topology_diagnostics');
  assert.equal(topologyDiagnosticsTool?.annotations.readOnlyHint, true);

  const runtimeStatusTool = tools.find((t) => t.name === 'mcp_loader_runtime_status');
  assert.equal(runtimeStatusTool?.annotations.readOnlyHint, true);

  const diagnosticsTool = tools.find((t) => t.name === 'mcp_loader_site_fabric_diagnostics');
  assert.equal(diagnosticsTool?.annotations.readOnlyHint, true);

  const inventoryTool = tools.find((t) => t.name === 'mcp_loader_site_tool_inventory_check');
  assert.equal(inventoryTool?.annotations.readOnlyHint, true);

  const attachTool = tools.find((t) => t.name === 'mcp_loader_attach_surface');
  assert.equal(attachTool?.annotations.readOnlyHint, false);

  const statusTool = tools.find((t) => t.name === 'mcp_loader_surface_status');
  assert.equal(statusTool?.annotations.readOnlyHint, true);

  const restartTool = tools.find((t) => t.name === 'mcp_loader_surface_restart');
  assert.equal(restartTool?.annotations.readOnlyHint, false);
  assert.equal((restartTool?.annotations as Record<string, unknown>).destructiveHint, true);

  console.log('mcp-loader-mcp protocol smoke ok');
} finally {
  await server.close();
  rmSync(root, { recursive: true, force: true });
}
