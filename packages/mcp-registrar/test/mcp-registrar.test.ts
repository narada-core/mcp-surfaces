import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from '@narada-core/sqlite';
import { resolveNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';
import { payloadCreate } from '@narada-core/mcp-transport';
import { buildGuidanceResult } from '../src/guidance.js';
import { appendLoaderAllowedSiteRoots, buildSiteBindConfig, buildSiteSurfaceRegistry, checkOutputReaderClosureForRegistry, checkSiteRegistryConformance, checkSiteRegistryConformanceFromObservation, compareCarrierProjection, createServerState, defaultRuntimeProxyImplementation, defaultSurfaceImplementation, handleRequest, parseArgs, readCodexPluginOverrides, readSiteSurfaceOverrides, refreshRegistrarOwnedSiteFabric, sharedSurfaceIdsForBinding, siteBindSidecarRefusal, siteSurfaceServerKey, transactionalWriteFiles, validateSiteMcpFabric, validateSiteToolInventoryObservation } from '../src/main.js';

function findWorkspaceRoot(start: string): string {
  let current = resolve(start);
  while (true) {
    if (existsSync(resolve(current, 'pnpm-workspace.yaml'))) return current;
    const parent = resolve(current, '..');
    if (parent === current) throw new Error('mcp_registrar_test_workspace_root_not_found');
    current = parent;
  }
}

const workspaceRoot = findWorkspaceRoot(fileURLToPath(new URL('.', import.meta.url)));
const portableWorkspaceRoot = workspaceRoot.replace(/\\/g, '/').replace(/\/$/, '');
const workspacePath = (...segments: string[]): string => join(workspaceRoot, ...segments).replace(/\\/g, '/');
const expectedUserSiteRoot = resolve(
  process.env.NARADA_USER_SITE_ROOT?.trim()
    || (process.env.USERPROFILE ? join(process.env.USERPROFILE, 'Narada') : '')
    || (process.env.HOME ? join(process.env.HOME, 'Narada') : '')
    || join(workspaceRoot, '.narada', 'user-site'),
).replace(/\\/g, '/');
const nativeRuntimeArtifactAvailable = process.platform === 'win32'
  && resolveNativeArtifact(join(workspaceRoot, 'packages', 'shared', 'mcp-runtime-proxy'), 'narada-mcp-runtime.exe') !== null;
assert.equal(
  defaultRuntimeProxyImplementation(process.platform, nativeRuntimeArtifactAvailable),
  nativeRuntimeArtifactAvailable ? 'native' : 'bun',
);
const defaultMaterializationArgs = parseArgs(['--materialize-all']);
assert.equal(defaultMaterializationArgs.mode, 'materialize-all');
if (defaultMaterializationArgs.mode === 'materialize-all') {
  assert.equal(defaultMaterializationArgs.runtimeProxyImplementation, nativeRuntimeArtifactAvailable ? 'native' : 'bun');
}
const explicitBunArgs = parseArgs(['--materialize-all', '--runtime-profile', 'bun', '--runtime-proxy-implementation', 'bun']);
assert.equal(explicitBunArgs.mode, 'materialize-all');
if (explicitBunArgs.mode === 'materialize-all') assert.equal(explicitBunArgs.runtimeProxyImplementation, 'bun');
const mismatchedProxyImplementation = nativeRuntimeArtifactAvailable ? 'bun' : 'native';
assert.throws(
  () => parseArgs(['--materialize-all', '--runtime-proxy-implementation', mismatchedProxyImplementation]),
  /registrar_runtime_proxy_override_requires_recovery_escape_hatch/,
);
const recoveryProxyArgs = parseArgs([
  '--materialize-carrier', 'codex-andrey', '--allow-single-carrier',
  '--runtime-proxy-implementation', mismatchedProxyImplementation,
  '--recovery-escape-hatch',
]);
assert.equal(recoveryProxyArgs.mode, 'materialize-carrier');
if (recoveryProxyArgs.mode === 'materialize-carrier') assert.equal(recoveryProxyArgs.recoveryEscapeHatch, true);
assert.equal(defaultSurfaceImplementation('local-filesystem', ['--mode', 'read'], true), 'native');
assert.equal(defaultSurfaceImplementation('local-filesystem', ['--mode', 'write'], true), 'js');
assert.equal(defaultSurfaceImplementation('local-filesystem', ['--mode', 'read'], false), 'js');
assert.equal(defaultSurfaceImplementation('mcp-loader', ['--mode', 'read'], true), undefined);
const root: any = mkdtempSync(join(tmpdir(), 'mcp-registrar-behavior-'));
const transactionalRoot = join(root, 'transactional-write');
const transactionalExisting = join(transactionalRoot, 'existing.txt');
const transactionalBlockingPath = join(transactionalRoot, 'blocking');
mkdirSync(transactionalRoot, { recursive: true });
writeFileSync(transactionalExisting, 'before', 'utf8');
mkdirSync(transactionalBlockingPath, { recursive: true });
assert.throws(
  () => transactionalWriteFiles([
    { path: transactionalExisting, content: 'after' },
    { path: transactionalBlockingPath, content: 'must-not-commit' },
  ]),
  (error: any) => error?.codeName === 'registrar_materialization_transaction_rolled_back',
);
assert.equal(readFileSync(transactionalExisting, 'utf8'), 'before');
const siteOverrideConfig = join(root, 'site-overrides.json');
writeFileSync(siteOverrideConfig, JSON.stringify({ surface_overrides: { 'task-lifecycle': { enabled: false } } }), 'utf8');
assert.deepEqual(readSiteSurfaceOverrides(siteOverrideConfig), { 'task-lifecycle': { enabled: false } });
assert.deepEqual(
  readCodexPluginOverrides({
    NARADA_CODEX_ENABLED_PLUGINS: 'sample-enabled@personal;sample-second@team',
    NARADA_CODEX_DISABLED_PLUGINS: 'sample-disabled@personal',
  }),
  {
    'sample-disabled@personal': false,
    'sample-enabled@personal': true,
    'sample-second@team': true,
  },
);
assert.throws(
  () => readCodexPluginOverrides({
    NARADA_CODEX_ENABLED_PLUGINS: 'sample@personal',
    NARADA_CODEX_DISABLED_PLUGINS: 'sample@personal',
  }),
  /registrar_codex_plugin_policy_conflict/,
);
assert.equal(sharedSurfaceIdsForBinding(
  { site_id: 'fixture', surfaces: ['task-lifecycle', 'work-lifecycle'], prefix: 'fixture' },
  { site_id: 'fixture', root, config_path: siteOverrideConfig, surfaces: [], surface_overrides: readSiteSurfaceOverrides(siteOverrideConfig) },
).includes('task-lifecycle'), false);

const guidance = buildGuidanceResult();
assert.equal(
  (guidance.recovery as string[]).some((item) => item.includes('direct CLI bootstrap recovery') && item.includes('do not wait for mcp-loader or mcp-registrar')),
  true,
);

const nestedCarrierMetadataDiff: any = compareCarrierProjection({
  carrierId: 'fixture-codex',
  configPath: 'fixture.toml',
  generatedContent: '[mcp_servers.fixture]\ncommand = "node"\n[mcp_servers.fixture.tools.new_tool]\napproval_mode = "approve"\n',
  generatedStructured: { mcpServers: { fixture: { command: 'node' } } },
  currentContent: '[mcp_servers.fixture]\ncommand = "node"\n',
  currentStructured: { mcpServers: { fixture: { command: 'node' } } },
});
assert.equal(nestedCarrierMetadataDiff.status, 'diff');
assert.equal(nestedCarrierMetadataDiff.projection_changed, true);
assert.equal(nestedCarrierMetadataDiff.changed_count, 0);
assert.equal(nestedCarrierMetadataDiff.server_projection_changed, false);
assert.equal(nestedCarrierMetadataDiff.carrier_metadata_or_format_only, true);
assert.deepEqual(nestedCarrierMetadataDiff.change_scopes, ['full_projection', 'carrier_metadata_or_format']);
assert.equal(nestedCarrierMetadataDiff.explanation_code, 'carrier_metadata_or_format_changed_without_server_definition_change');
assert.equal(nestedCarrierMetadataDiff.count_semantics, 'added_removed_changed_counts_cover_server_definitions_only');
assert.notEqual(nestedCarrierMetadataDiff.generated_sha256, nestedCarrierMetadataDiff.current_sha256);

const identicalCarrierProjection: any = compareCarrierProjection({
  carrierId: 'fixture-codex',
  configPath: 'fixture.toml',
  generatedContent: 'same\n',
  generatedStructured: { mcpServers: {} },
  currentContent: 'same\n',
  currentStructured: { mcpServers: {} },
});
assert.equal(identicalCarrierProjection.status, 'clean');
assert.equal(identicalCarrierProjection.projection_changed, false);
assert.deepEqual(identicalCarrierProjection.change_scopes, []);
assert.equal(identicalCarrierProjection.explanation_code, 'carrier_projection_exact_match');

async function observeToolsList(entrypoint: string, args: string[]): Promise<string[]> {
  const child: any = spawn(process.execPath, [entrypoint, ...args], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  let stdout: any = '';
  let stderr: any = '';
  return new Promise<string[]>((resolve, reject) => {
    const timeout: any = setTimeout(() => finish(new Error(`tools_list_timeout:${stderr.slice(-2000)}`)), 5000);
    let initialized: any = false;
    const finish: any = (error: Error | null, tools?: string[]) => {
      clearTimeout(timeout);
      if (!child.killed) child.kill();
      if (error) reject(error);
      else resolve(tools ?? []);
    };
    child.stderr.on('data', (chunk: any) => { stderr = (stderr + String(chunk)).slice(-2000); });
    child.once('error', (error: any) => finish(error));
    child.once('exit', (code: any) => {
      if (!initialized && code !== null) finish(new Error(`tools_list_child_exited:${code}:${stderr}`));
    });
    child.stdout.on('data', (chunk: any) => {
      stdout += String(chunk);
      const lines: any = stdout.split(/\r?\n/);
      stdout = lines.pop() ?? '';
      for (const line of lines) {
        if (!line.trim()) continue;
        const message: any = JSON.parse(line) as Record<string, any>;
        if (message.id === 100) {
          initialized = true;
          child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized', params: {} })}\n`);
          child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 101, method: 'tools/list', params: {} })}\n`);
        } else if (message.id === 101) {
          finish(null, (message.result?.tools ?? []).map((tool: Record<string, unknown>) => String(tool.name)));
        }
      }
    });
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 100, method: 'initialize', params: { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'registrar-inventory-test', version: '1.0.0' } } })}\n`);
  });
}

try {
  const state: any = createServerState({});

  async function call(name: string, args: Record<string, unknown>): Promise<Record<string, any>> {
    return handleRequest({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args } }, state) as Promise<Record<string, any>>;
  }
  function view(res: Record<string, any>): Record<string, any> {
    return res.result.structuredContent as Record<string, any>;
  }
  function assertRuntimeProxy(server: Record<string, any>, childEntrypoint: string, runtime = 'bun', proxyImplementation = nativeRuntimeArtifactAvailable ? 'native' : 'bun'): void {
    const args: any = server.args as string[];
    const assertRuntimeExecutable = (value: string, expectedRuntime: string): void => {
      assert.equal(isAbsolute(value), true, `expected absolute ${expectedRuntime} executable, received ${value}`);
      assert.equal(
        basename(value).replace(/\.exe$/i, ''),
        expectedRuntime,
        `expected ${expectedRuntime} runtime executable, received ${value}`,
      );
    };
    if (proxyImplementation === 'native') {
      assert.match(String(server.command).replace(/\\/g, '/'), /packages\/shared\/mcp-runtime-proxy\/dist\/native\/(?:versions\/[^/]+\/)?narada-mcp-runtime\.exe$/i);
      assert.equal(args[0], 'proxy');
      assertRuntimeExecutable(args[args.indexOf('--child-command') + 1], runtime);
    } else {
      assertRuntimeExecutable(String(server.command), proxyImplementation);
      assert.match(args[0].replace(/\\/g, '/'), /packages\/shared\/mcp-runtime-proxy\/dist\/src\/main\.js$/);
    }
    const registrarCommandIndex = args.indexOf('--registrar-command');
    if (registrarCommandIndex >= 0) assert.equal(isAbsolute(args[registrarCommandIndex + 1]), true);
    assert.equal(args[args.indexOf('--entrypoint') + 1].replace(/\\/g, '/'), childEntrypoint.replace(/\\/g, '/'));
    assert.ok(args.includes('--'));
  }
  function registryWithMailboxSurface(registeredLiveTools: string[], readOnlyTools: string[]): Record<string, any> {
    return {
      schema: 'narada.site.capabilities.mcp_surfaces.v1',
      site_id: 'fixture-site',
      surfaces: [{
        surface_id: 'fixture-mailbox.local',
        display_name: 'fixture-mailbox',
        server_name: 'fixture-mailbox',
        authority_boundary: {},
        client_config: {},
        tool_contract: {
          read_only_tools: readOnlyTools,
          mutating_tools: registeredLiveTools.filter((tool) => !readOnlyTools.includes(tool)),
          refused_tools: [],
        },
        registered_live_tools: registeredLiveTools,
        catalog_surface_id: 'mailbox',
      }],
    };
  }
  function registryWithSurface(catalogSurfaceId: string, registeredLiveTools: string[], readOnlyTools: string[]): Record<string, any> {
    return {
      schema: 'narada.site.capabilities.mcp_surfaces.v1',
      site_id: 'fixture-site',
      surfaces: [{
        surface_id: `fixture-${catalogSurfaceId}.local`,
        display_name: `fixture-${catalogSurfaceId}`,
        server_name: `fixture-${catalogSurfaceId}`,
        authority_boundary: {},
        client_config: {},
        tool_contract: {
          read_only_tools: readOnlyTools,
          mutating_tools: registeredLiveTools.filter((tool) => !readOnlyTools.includes(tool)),
          refused_tools: [],
        },
        registered_live_tools: registeredLiveTools,
        catalog_surface_id: catalogSurfaceId,
      }],
    };
  }
  function assertOutputReaderClosure(registry: Record<string, any>, label: string): void {
    const result: any = checkOutputReaderClosureForRegistry(registry, { site_id: label, site_root: root, registry_path: join(root, `${label}-mcp-surfaces.json`) });
    assert.equal(result.status, 'ok', `${label} output reader closure violations: ${JSON.stringify(result.violations)}`);
  }

  const missingReaderCheck: any = checkOutputReaderClosureForRegistry(
    registryWithMailboxSurface(['mailbox_message_show'], ['mailbox_message_show']),
    { site_id: 'missing-reader', site_root: root, registry_path: join(root, 'missing-reader-mcp-surfaces.json') },
  );
  assert.equal(missingReaderCheck.status, 'drift');
  assert.deepEqual((missingReaderCheck.violations as Array<Record<string, any>>).map((violation) => violation.violation), [
    'missing_registered_live_tool',
    'missing_read_only_admission',
  ]);
  assert.deepEqual((missingReaderCheck.violations as Array<Record<string, any>>).map((violation) => violation.required_reader_tool), [
    'mailbox_output_show',
    'mailbox_output_show',
  ]);

  const missingReadOnlyCheck: any = checkOutputReaderClosureForRegistry(
    registryWithMailboxSurface(['mailbox_message_show', 'mailbox_output_show'], ['mailbox_message_show']),
    { site_id: 'missing-read-only', site_root: root, registry_path: join(root, 'missing-read-only-mcp-surfaces.json') },
  );
  assert.equal(missingReadOnlyCheck.status, 'drift');
  assert.deepEqual((missingReadOnlyCheck.violations as Array<Record<string, any>>).map((violation) => violation.violation), [
    'missing_read_only_admission',
  ]);

  const goodReaderCheck: any = checkOutputReaderClosureForRegistry(
    registryWithMailboxSurface(['mailbox_message_show', 'mailbox_output_show'], ['mailbox_message_show', 'mailbox_output_show']),
    { site_id: 'good-reader', site_root: root, registry_path: join(root, 'good-reader-mcp-surfaces.json') },
  );
  assert.equal(goodReaderCheck.status, 'ok');
  assert.deepEqual(goodReaderCheck.violations, []);

  const surfaces: any = await call('registrar_surface_list', {});
  const surfaceData: any = view(surfaces);
  assert.ok(Array.isArray(surfaceData.items));
  assert.ok(surfaceData.count >= 10);
  const sched: any = (surfaceData.items as Array<Record<string, any>>).find((s) => s.id === 'scheduler');
  assert.ok(sched);
  assert.ok(sched.tools.includes('scheduler_task_list'));
  assert.equal(sched.injection_scope, 'local_site');
  const speech: any = (surfaceData.items as Array<Record<string, any>>).find((s) => s.id === 'speech');
  assert.ok(speech);
  assert.equal(speech.injection_scope, 'host');
  assert.deepEqual(speech.authority_locus, { kind: 'host' });
  assert.equal((speech.narada_scope as Record<string, any>).scope_source, 'registrar_surface_catalog');
  assert.equal((speech.narada_scope as Record<string, any>).injection_scope, 'host');
  assert.equal(speech.default_injection, 'all_carrier_sessions');
  assert.deepEqual(speech.args, ['--provider-registry-path', workspacePath('packages', 'speech-mcp', 'config', 'provider-registry.v2.json')]);
  assert.deepEqual(speech.tools, ['speech_guidance', 'speech_speak', 'speech_voices', 'speech_listen_status', 'speech_capture_transcribe', 'speech_prompt_capture_response', 'speech_listen_start', 'speech_listen_stop']);
  const operatorRouting: any = (surfaceData.items as Array<Record<string, any>>).find((s) => s.id === 'operator-routing');
  assert.ok(operatorRouting);
  assert.equal(operatorRouting.injection_scope, 'user_site');
  assert.equal(operatorRouting.default_injection, 'all_site_bound_sessions');
  assert.deepEqual(operatorRouting.tools, ['operator_routing_guidance', 'operator_route_doctor', 'operator_route_request']);
  const artifacts: any = (surfaceData.items as Array<Record<string, any>>).find((s) => s.id === 'artifacts');
  assert.ok(artifacts);
  assert.equal(artifacts.injection_scope, 'local_site');
  assert.equal(artifacts.default_injection, 'all_site_bound_sessions');
  assert.deepEqual(artifacts.env_vars, ['NARADA_SESSION_ID', 'NARADA_SITE_ROOT', 'NARADA_NARS_BASE_URL']);
  assert.ok(artifacts.tools.includes('artifact_register_file'));
  const agentContextSurface: any = (surfaceData.items as Array<Record<string, any>>).find((s) => s.id === 'agent-context');
  assert.ok(agentContextSurface);
  const agentContextEnvironment = [
    'NARADA_AGENT_CONTEXT_DB',
    'NARADA_AGENT_ID',
    'NARADA_AGENT_START_EVENT_ID',
    'NARADA_CARRIER_SESSION_ACTIVATION_RECEIPT',
    'NARADA_CARRIER_SESSION_ADMISSION_RECEIPT',
    'NARADA_CARRIER_SESSION_ID',
    'NARADA_ORIENTATION_BRIEF',
    'NARADA_ORIENTATION_DELIVERY_RECEIPT',
    'NARADA_ORIENTATION_ENTRY_FILE',
    'NARADA_ORIENTATION_MANIFEST_ID',
    'NARADA_SITE_ID',
    'NARADA_SITE_ROOT',
  ];
  assert.equal(agentContextSurface.env_vars, undefined);
  const agentContextProjections: any[] = agentContextSurface.projections;
  assert.deepEqual(
    agentContextProjections.map((projection) => projection.id),
    ['default', 'admin'],
  );
  assert.deepEqual(agentContextProjections[0].env_vars, agentContextEnvironment);
  assert.deepEqual(agentContextProjections[1].env_vars, agentContextEnvironment);
  assert.deepEqual(agentContextProjections[0].exposed_tools, [
    'agent_orientation_read',
    'mcp_output_show',
  ]);
  assert.ok(agentContextProjections[1].exposed_tools.includes('agent_context_checkpoint'));
  const narsSession: any = (surfaceData.items as Array<Record<string, any>>).find((s) => s.id === 'nars-session');
  assert.ok(narsSession);
  assert.equal(narsSession.injection_scope, undefined);
  assert.deepEqual((narsSession.projections as Array<Record<string, any>>).map((projection) => ({
    id: projection.id,
    injection_scope: projection.injection_scope,
    default_injection: projection.default_injection,
    runtime_requirements: projection.runtime_requirements,
  })), [
    {
      id: 'user-site-operator',
      injection_scope: 'user_site',
      default_injection: 'all_site_bound_sessions',
      runtime_requirements: [],
    },
    {
      id: 'local-site-nars-runtime',
      injection_scope: 'local_site',
      default_injection: 'runtime_selected_sessions',
      runtime_requirements: ['nars'],
    },
  ]);
  const fixtureSite: any = {
    site_id: 'fixture-site',
    root,
    config_path: join(root, 'config.json'),
    surfaces: [],
  };
  assert.throws(
    () => buildSiteBindConfig(fixtureSite, agentContextSurface as any),
    /registrar_surface_projection_required:agent-context/,
  );
  const agentContextBindConfig: any = buildSiteBindConfig(
    fixtureSite,
    agentContextSurface as any,
    'default',
  );
  const agentContextBoundServer: any = (agentContextBindConfig.config.mcpServers as Record<string, any>)[agentContextBindConfig.serverKey];
  assertRuntimeProxy(agentContextBoundServer, agentContextSurface.entrypoint, 'bun');
  assert.deepEqual(agentContextBoundServer.env_vars, agentContextEnvironment);
  assert.deepEqual(agentContextBoundServer.tools, [
    'agent_orientation_read',
    'mcp_output_show',
  ]);
  const refreshSiteRoot = join(root, 'registrar-owned-refresh-site');
  const refreshSite: any = { site_id: 'fixture-site', root: refreshSiteRoot, config_path: join(refreshSiteRoot, 'site.json'), surfaces: [] };
  const staleBindConfig: any = buildSiteBindConfig(
    refreshSite,
    agentContextSurface as any,
    'default',
  );
  const staleServer: any = staleBindConfig.config.mcpServers[staleBindConfig.serverKey];
  const staleChildCommandIndex = staleServer.args.indexOf('--child-command');
  staleBindConfig.config.custom_metadata = { preserved: 'top-level' };
  staleServer.custom_metadata = { preserved: 'server-level' };
  staleServer.args[staleChildCommandIndex + 1] = 'bun';
  if (/bun(?:\.exe)?$/i.test(String(staleServer.command))) staleServer.command = 'bun';
  mkdirSync(join(refreshSiteRoot, '.ai', 'mcp'), { recursive: true });
  writeFileSync(join(refreshSiteRoot, '.ai', 'mcp', staleBindConfig.fileName), JSON.stringify(staleBindConfig.config, null, 2) + '\n', 'utf8');
  const refreshResult: any = refreshRegistrarOwnedSiteFabric(refreshSite);
  assert.equal(refreshResult.status, 'refreshed');
  assert.equal(refreshResult.refreshed_count, 1);
  const refreshedConfig: any = JSON.parse(readFileSync(join(refreshSiteRoot, '.ai', 'mcp', staleBindConfig.fileName), 'utf8'));
  assert.deepEqual(refreshedConfig.custom_metadata, { preserved: 'top-level' });
  assert.deepEqual(refreshedConfig.mcpServers[staleBindConfig.serverKey].custom_metadata, { preserved: 'server-level' });
  const idempotentRefresh: any = refreshRegistrarOwnedSiteFabric(refreshSite);
  assert.equal(idempotentRefresh.refreshed_count, 0);
  assert.equal(idempotentRefresh.unchanged_count, 1);
  assert.throws(
    () => buildSiteBindConfig(fixtureSite, narsSession as any),
    /registrar_surface_projection_required:nars-session/,
  );
  const operatorProjectionConfig: any = buildSiteBindConfig(fixtureSite, narsSession as any, 'user-site-operator');
  const operatorProjectionServer: any = (operatorProjectionConfig.config.mcpServers as Record<string, any>)[operatorProjectionConfig.serverKey];
  assert.equal(operatorProjectionServer.surface_projection.projection_id, 'user-site-operator');
  assert.deepEqual(operatorProjectionServer.surface_projection.execution, {
    adapter: 'stdio',
    tenancy: 'session_isolated',
    replacement: 'manual',
  });

  const quotaMeter: any = (surfaceData.items as Array<Record<string, any>>).find((s) => s.id === 'quota-meter');
  assert.ok(quotaMeter);
  assert.equal(quotaMeter.injection_scope, 'host');
  assert.deepEqual(quotaMeter.projections?.map((projection: Record<string, any>) => ({
    id: projection.id,
    injection_scope: projection.injection_scope,
  })), [{ id: 'default', injection_scope: 'host' }]);
  assert.deepEqual(quotaMeter.tools, [
    'quota_meter_guidance',
    'quota_meter_glide_status',
    'quota_meter_overlay_status',
    'quota_meter_overlay_start',
    'quota_meter_overlay_stop',
  ]);
  assert.equal(operatorProjectionServer.surface_projection.injection_scope, 'user_site');
  const narsProjectionConfig: any = buildSiteBindConfig(fixtureSite, narsSession as any, 'local-site-nars-runtime');
  const narsProjectionServer: any = (narsProjectionConfig.config.mcpServers as Record<string, any>)[narsProjectionConfig.serverKey];
  assert.equal(narsProjectionServer.surface_projection.projection_id, 'local-site-nars-runtime');
  assert.equal(narsProjectionServer.surface_projection.injection_scope, 'local_site');
  assert.deepEqual(narsProjectionServer.surface_projection.runtime_requirements, ['nars']);
  const narsRuntimeSelectedConfig: any = buildSiteBindConfig(fixtureSite, narsSession as any, undefined, 'nars');
  const narsRuntimeSelectedServer: any = (narsRuntimeSelectedConfig.config.mcpServers as Record<string, any>)[narsRuntimeSelectedConfig.serverKey];
  assert.equal(narsRuntimeSelectedServer.surface_projection.projection_id, 'local-site-nars-runtime');
  assert.equal(narsRuntimeSelectedServer.surface_projection.runtime_kind, 'nars');
  const neutralRuntimeConfig: any = buildSiteBindConfig(fixtureSite, artifacts as any, undefined, 'nars');
  const neutralRuntimeServer: any = (neutralRuntimeConfig.config.mcpServers as Record<string, any>)[neutralRuntimeConfig.serverKey];
  assert.equal(neutralRuntimeServer.surface_projection.projection_id, 'default');
  assert.equal(neutralRuntimeServer.surface_projection.runtime_kind, 'nars');
  const explicitNeutralProjectionConfig: any = buildSiteBindConfig(fixtureSite, narsSession as any, 'user-site-operator', 'nars');
  const explicitNeutralProjectionServer: any = (explicitNeutralProjectionConfig.config.mcpServers as Record<string, any>)[explicitNeutralProjectionConfig.serverKey];
  assert.equal(explicitNeutralProjectionServer.surface_projection.projection_id, 'user-site-operator');
  assert.equal(explicitNeutralProjectionServer.surface_projection.runtime_kind, 'nars');
  const sharedSurfaceIds: any = sharedSurfaceIdsForBinding({ site_id: 'narada-test', prefix: 'narada-test', surfaces: ['agent-context'] });
  assert.ok(sharedSurfaceIds.includes('speech'));
  assert.ok(sharedSurfaceIds.includes('operator-routing'));
  assert.ok(sharedSurfaceIds.includes('artifacts'));
  assert.ok(sharedSurfaceIds.includes('nars-session'));
  assert.equal(sharedSurfaceIds.filter((surfaceId: any) => surfaceId === 'speech').length, 1);
  const narsRuntimeSurfaceIds: any = sharedSurfaceIdsForBinding({ site_id: 'narada-test', prefix: 'narada-test', runtime_kind: 'nars', surfaces: ['agent-context'] });
  assert.ok(narsRuntimeSurfaceIds.includes('nars-session'));
  const registrar: any = (surfaceData.items as Array<Record<string, any>>).find((s) => s.id === 'mcp-registrar');
  assert.ok(registrar);
  assert.equal(registrar.injection_scope, 'user_site');
  assert.deepEqual(registrar.authority_locus, { kind: 'user_site', site_root: expectedUserSiteRoot });
  assert.ok(registrar.tools.includes('registrar_surface_tool_inventory_check'));
  assert.ok(registrar.tools.includes('registrar_site_registry_conformance_check'));
  assert.ok(registrar.tools.includes('registrar_site_output_reader_closure_check'));
  const bySurface: any = new Map((surfaceData.items as Array<Record<string, any>>).map((surface) => [surface.id, surface]));
  assert.ok((bySurface.get('git')?.tools as string[]).includes('git_changed_summary'));
  assert.ok((bySurface.get('git')?.tools as string[]).includes('git_unstage'));
  assert.ok((bySurface.get('graph-mail')?.tools as string[]).includes('graph_mail_attachment_upload_file'));
  assert.ok((bySurface.get('graph-mail')?.tools as string[]).includes('graph_mail_reply_all_to_last_in_thread_draft_create'));
  assert.ok((bySurface.get('graph-mail')?.tools as string[]).includes('graph_mail_ticket_draft_disposition_scan'));
  assert.ok((bySurface.get('graph-mail')?.tools as string[]).includes('graph_mail_ticket_draft_disposition_list'));
  assert.ok((bySurface.get('graph-mail')?.tools as string[]).includes('graph_mail_ticket_draft_disposition_ack'));
  assert.ok((bySurface.get('task-lifecycle')?.tools as string[]).includes('task_lifecycle_submit_work'));
  assert.ok((bySurface.get('task-lifecycle')?.tools as string[]).includes('task_lifecycle_evidence_supersede'));
  assert.ok((bySurface.get('task-lifecycle')?.tools as string[]).includes('task_lifecycle_tags_update'));
  assert.ok((bySurface.get('site-loop')?.tools as string[]).includes('site_loop_proof_status'));
  assert.ok((bySurface.get('site-loop')?.tools as string[]).includes('site_loop_proof_run'));
  assert.ok((bySurface.get('site-loop')?.tools as string[]).includes('site_loop_output_show'));
  assert.equal((bySurface.get('site-lifecycle')?.tools as string[]).includes('site_registry_list'), false);
  assert.ok((bySurface.get('site-registry')?.tools as string[]).includes('site_registry_list'));
  assert.ok((bySurface.get('site-registry')?.tools as string[]).includes('site_registry_show'));
  assert.ok((bySurface.get('site-registry')?.tools as string[]).includes('site_registry_discover_plan'));
  assert.equal(bySurface.get('site-registry')?.injection_scope, 'user_site');
  assert.ok((bySurface.get('site-inbox')?.tools as string[]).includes('inbox_submit'));
  assert.ok((bySurface.get('site-inbox')?.tools as string[]).includes('inbox_output_show'));
  assert.ok((bySurface.get('mailbox')?.tools as string[]).includes('mailbox_output_show'));
  assert.ok((bySurface.get('graph-mail')?.tools as string[]).includes('graph_mail_output_show'));
  assert.ok((bySurface.get('calendar')?.tools as string[]).includes('calendar_output_show'));
  assert.ok((bySurface.get('worker-delegation')?.tools as string[]).includes('worker_dashboard_describe'));
  assert.ok((bySurface.get('worker-delegation')?.tools as string[]).includes('worker_cognition_defaults_inspect'));
  assert.ok((bySurface.get('worker-delegation')?.tools as string[]).includes('worker_cognition_defaults_update'));
  assert.equal((bySurface.get('worker-delegation')?.tools as string[]).includes('worker_output_show'), true);
  assert.ok((bySurface.get('delegated-task')?.tools as string[]).includes('delegated_task_result'));
  assert.equal((bySurface.get('agent-context')?.tools as string[]).includes('agent_context_list_sessions'), false);
  assert.ok((bySurface.get('agent-context')?.tools as string[]).includes('agent_orientation_read'));
  assert.ok((bySurface.get('agent-context')?.tools as string[]).includes('mcp_output_show'));
  assert.equal((bySurface.get('agent-context')?.tools as string[]).includes('agent_context_output_show'), false);
  assert.deepEqual(bySurface.get('agent-context')?.output_reader_closure, {
    agent_orientation_read: 'mcp_output_show',
  });
  assert.ok((bySurface.get('sop')?.tools as string[]).includes('sop_doctor'));
  assert.ok((bySurface.get('mcp-loader')?.tools as string[]).includes('mcp_loader_site_fabric_diagnostics'));
  assert.ok((bySurface.get('mcp-loader')?.tools as string[]).includes('mcp_loader_site_tool_inventory_check'));
  assert.ok((bySurface.get('mcp-loader')?.tools as string[]).includes('mcp_loader_guidance'));
  assert.ok((bySurface.get('mcp-loader')?.tools as string[]).includes('mcp_loader_surface_status'));
  assert.ok((bySurface.get('mcp-loader')?.tools as string[]).includes('mcp_loader_surface_restart'));
  assert.equal(bySurface.get('mcp-loader')?.injection_scope, 'user_site');
  assert.equal(bySurface.get('mcp-loader')?.default_injection, 'all_site_bound_sessions');
  assert.equal(bySurface.get('mcp-loader')?.restart_owner, 'user_site');
  const surfaceFeedback: any = bySurface.get('surface-feedback');
  assert.ok((surfaceFeedback?.tools as string[]).includes('surface_feedback_import'));
  assert.ok((surfaceFeedback?.tools as string[]).includes('surface_feedback_actionable_queue'));
  assert.deepEqual(surfaceFeedback?.args, [
    '--feedback-root', '{site_control_root}/feedback',
    '--canonical-feedback-root', '{site_control_root}/feedback',
    '--task-lifecycle-root', '{site_root}',
    '--site-id', '{site_id}',
  ]);
  assert.ok(surfaceFeedback?.env_vars?.includes('NARADA_SURFACE_FEEDBACK_ROOT'));
  assert.equal(surfaceFeedback?.args.some((arg: any) => /D:\/code|C:\/Users\/Andrey/i.test(arg)), false);

  const localFilesystemEntrypoint: any = workspacePath('packages', 'local-filesystem-mcp', 'dist', 'src', 'main.js');
  const observedLocalFilesystemTools: any = await observeToolsList(localFilesystemEntrypoint, [
    '--mode', 'write',
    '--allowed-root', root,
    '--output-root', root,
  ]);
  const mailboxEntrypoint: any = workspacePath('packages', 'mailbox-mcp', 'dist', 'src', 'main.js');
  const observedMailboxTools: any = await observeToolsList(mailboxEntrypoint, ['--site-root', root]);
  const liveInventoryCheck: any = view(await call('registrar_surface_tool_inventory_check', {
    observed_tools: {
      'local-filesystem': observedLocalFilesystemTools,
      mailbox: observedMailboxTools,
    },
  }));
  assert.equal(liveInventoryCheck.status, 'ok', JSON.stringify(liveInventoryCheck));
  assert.ok(observedLocalFilesystemTools.includes('fs_guidance'));
  assert.ok(observedLocalFilesystemTools.includes('fs_doctor'));
  assert.ok(observedMailboxTools.includes('mailbox_guidance'));
  assert.ok(observedMailboxTools.includes('mailbox_output_show'));

  const conformanceSiteRoot: any = join(root, 'registry-conformance-site');
  mkdirSync(join(conformanceSiteRoot, '.ai', 'mcp'), { recursive: true });
  const mailboxCatalogTools: any = bySurface.get('mailbox')?.tools as string[];
  writeFileSync(join(conformanceSiteRoot, '.ai', 'mcp', 'fixture-mailbox-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    site_id: 'registry-conformance-site',
    mcpServers: {
      'fixture-mailbox': {
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/mailbox-mcp/dist/src/main.js', '--site-root', conformanceSiteRoot],
        tools: mailboxCatalogTools,
        surface_id: 'mailbox',
      },
    },
  }, null, 2), 'utf8');
  const conformanceSite: any = {
    site_id: 'registry-conformance-site',
    root: conformanceSiteRoot,
    config_path: join(conformanceSiteRoot, 'config.json'),
    surfaces: [],
  };
  assert.equal(validateSiteToolInventoryObservation(conformanceSite, {
    schema: 'narada.mcp_loader.site_tool_inventory_check.v1',
    site_root: conformanceSiteRoot,
    observed_tools: {},
    observed_read_only_tools: {},
    observed_mutating_tools: {},
  }).schema, 'narada.mcp_loader.site_tool_inventory_check.v1');
  assert.throws(() => validateSiteToolInventoryObservation(conformanceSite, {
    schema: 'narada.mcp_loader.site_tool_inventory_check.v1',
    site_root: join(conformanceSiteRoot, 'other'),
    observed_tools: {},
    observed_read_only_tools: {},
    observed_mutating_tools: {},
  }), /registrar_inventory_observation_site_mismatch/);
  const conformingRegistry: any = buildSiteSurfaceRegistry(conformanceSite);
  const conformingSurface: any = (conformingRegistry.surfaces as Array<Record<string, any>>)[0];
  const observedConformanceTools: any = { 'fixture-mailbox': mailboxCatalogTools };
  const observedConformanceReadOnlyTools: any = { 'fixture-mailbox': conformingSurface.tool_contract.read_only_tools as string[] };
  const observedConformanceMutatingTools: any = { 'fixture-mailbox': conformingSurface.tool_contract.mutating_tools as string[] };
  const conformingCheck: any = checkSiteRegistryConformance(
    conformanceSite,
    conformingRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
    true,
  );
  assert.equal(conformingCheck.status, 'ok', JSON.stringify(conformingCheck));
  assert.equal(conformingCheck.violation_count, 0);
  const observationPayload: any = payloadCreate({
    siteRoot: conformanceSiteRoot,
    args: {
      payload_id: 'site-tools-fixture-observation',
      payload: {
        schema: 'narada.mcp_loader.site_tool_inventory_check.v1',
        status: 'ok',
        site_root: conformanceSiteRoot,
        observed_at: new Date().toISOString(),
        observed_tools: observedConformanceTools,
        observed_read_only_tools: observedConformanceReadOnlyTools,
        observed_mutating_tools: observedConformanceMutatingTools,
      },
      created_by: 'mcp-loader-mcp',
    },
  });
  const refConformanceCheck: any = checkSiteRegistryConformanceFromObservation(
    conformanceSite,
    conformingRegistry,
    observationPayload.ref,
  );
  assert.equal(refConformanceCheck.status, 'ok', JSON.stringify(refConformanceCheck));
  assert.equal(refConformanceCheck.observation_ref, observationPayload.ref);
  assert.equal(refConformanceCheck.observation_sha256, observationPayload.sha256);
  const observationLineage: any = refConformanceCheck.observation_lineage as Record<string, any>;
  assert.equal(observationLineage.assurance, 'declarative_lineage_guard_not_cryptographic_provenance');
  assert.equal(observationLineage.authority_effect, 'none');
  const forgedLineagePayload: any = payloadCreate({
    siteRoot: conformanceSiteRoot,
    args: {
      payload_id: 'site-tools-wrong-lineage',
      payload: {
        schema: 'narada.mcp_loader.site_tool_inventory_check.v1',
        site_root: conformanceSiteRoot,
        observed_tools: observedConformanceTools,
        observed_read_only_tools: observedConformanceReadOnlyTools,
        observed_mutating_tools: observedConformanceMutatingTools,
      },
      created_by: 'not-the-loader',
    },
  });
  assert.throws(
    () => checkSiteRegistryConformanceFromObservation(conformanceSite, conformingRegistry, forgedLineagePayload.ref),
    /registrar_inventory_observation_lineage_mismatch/,
  );

  const staleRegistry: any = structuredClone(conformingRegistry);
  const staleSurface: any = (staleRegistry.surfaces as Array<Record<string, any>>)[0];
  staleSurface.registered_live_tools = (staleSurface.registered_live_tools as string[]).filter((tool) => tool !== 'mailbox_output_show');
  staleSurface.tool_contract.read_only_tools = (staleSurface.tool_contract.read_only_tools as string[]).filter((tool: string) => tool !== 'mailbox_output_show');
  const staleCheck: any = checkSiteRegistryConformance(
    conformanceSite,
    staleRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  );
  assert.equal(staleCheck.status, 'drift');
  const staleCodes: any = (staleCheck.violations as Array<Record<string, any>>).map((violation) => violation.code);
  assert.ok(staleCodes.includes('registered_tools_differ_from_live'));
  assert.ok(staleCodes.includes('output_reader_closure_violation'));

  const overlappingRegistry: any = structuredClone(conformingRegistry);
  const overlappingSurface: any = (overlappingRegistry.surfaces as Array<Record<string, any>>)[0];
  overlappingSurface.tool_contract.mutating_tools.push('mailbox_doctor');
  const overlappingCheck: any = checkSiteRegistryConformance(
    conformanceSite,
    overlappingRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  );
  const overlappingCodes: any = (overlappingCheck.violations as Array<Record<string, any>>).map((violation) => violation.code);
  assert.ok(overlappingCodes.includes('tool_contract_partition_overlap'));
  assert.ok(overlappingCodes.includes('mutating_classification_differ_from_live'));

  const missingEvidenceCheck: any = checkSiteRegistryConformance(conformanceSite, conformingRegistry, {}, {}, {});
  const missingEvidenceCodes: any = (missingEvidenceCheck.violations as Array<Record<string, any>>).map((violation) => violation.code);
  assert.ok(missingEvidenceCodes.includes('live_tool_observation_missing'));
  assert.ok(missingEvidenceCodes.includes('live_read_only_observation_missing'));
  assert.ok(missingEvidenceCodes.includes('live_mutating_observation_missing'));

  writeFileSync(join(conformanceSiteRoot, '.ai', 'mcp', 'fixture-git-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'fixture-git': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/git-mcp/dist/src/main.js'],
        tools: ['git_add', 'git_begin_work_scope', 'git_branch_create', 'git_branch_delete', 'git_branch_delete_remote', 'git_branch_list', 'git_branch_rename', 'git_branch_set_upstream', 'git_branch_switch', 'git_branch_unset_upstream', 'git_changed_summary', 'git_commit', 'git_diff', 'git_fetch', 'git_guidance', 'git_log', 'git_merge', 'git_merge_abort', 'git_merge_continue', 'git_output_show', 'git_policy_inspect', 'git_push', 'git_rebase', 'git_rebase_abort', 'git_rebase_continue', 'git_repositories_summary', 'git_show', 'git_status', 'git_sync_status', 'git_unstage', 'git_workflow_record'],
        surface_id: 'git',
      },
    },
  }, null, 2), 'utf8');
  const partialRegistry: any = buildSiteSurfaceRegistry(conformanceSite);
  const partialCheck: any = checkSiteRegistryConformance(
    conformanceSite,
    partialRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  );
  assert.equal(partialCheck.status, 'incomplete', JSON.stringify(partialCheck));
  const partialCoverage: any = partialCheck.observation_coverage as Record<string, any>;
  assert.equal(partialCoverage.status, 'partial');
  assert.ok((partialCoverage.unobserved_server_names as string[]).includes('fixture-git'));
  const partialGitViolations: any = (partialCheck.violations as Array<Record<string, any>>)
    .filter((violation) => violation.server_name === 'fixture-git')
    .map((violation) => violation.code);
  assert.ok(!partialGitViolations.includes('live_tool_observation_missing'));
  assert.ok(!partialGitViolations.includes('live_read_only_observation_missing'));
  assert.ok(!partialGitViolations.includes('live_mutating_observation_missing'));

  const violationCodes: any = (check: Record<string, any>) =>
    new Set((check.violations as Array<Record<string, any>>).map((violation) => violation.code));

  const provenanceRegistry: any = structuredClone(conformingRegistry);
  provenanceRegistry.schema = 'wrong.schema';
  provenanceRegistry.site_id = 'wrong-site';
  provenanceRegistry.generated_by = 'manual';
  provenanceRegistry.generated_at = 'not-a-time';
  provenanceRegistry.generation_policy = { mode: 'manual', source: 'unknown', note: 'unknown' };
  const provenanceCodes: any = violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    provenanceRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  ));
  for (const code of [
    'registry_schema_mismatch',
    'registry_site_id_mismatch',
    'registry_generator_mismatch',
    'registry_generation_policy_mismatch',
    'registry_generation_source_mismatch',
    'registry_generation_note_mismatch',
    'registry_generated_at_invalid',
  ]) assert.ok(provenanceCodes.has(code), code);

  const missingSurfaceRegistry: any = structuredClone(conformingRegistry);
  missingSurfaceRegistry.surfaces = [];
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    missingSurfaceRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  )).has('registry_surface_missing'));

  const extraSurfaceRegistry: any = structuredClone(conformingRegistry);
  (extraSurfaceRegistry.surfaces as Array<Record<string, any>>).push({ ...structuredClone(conformingSurface), server_name: 'not-in-fabric', surface_id: 'not-in-fabric.local' });
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    extraSurfaceRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  )).has('registry_surface_not_in_fabric'));

  const duplicateSurfaceRegistry: any = structuredClone(conformingRegistry);
  (duplicateSurfaceRegistry.surfaces as Array<Record<string, any>>).push(structuredClone(conformingSurface));
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    duplicateSurfaceRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  )).has('registry_surface_server_name_duplicate'));

  const incompleteContractRegistry: any = structuredClone(conformingRegistry);
  const incompleteContractSurface: any = incompleteContractRegistry.surfaces[0];
  incompleteContractSurface.tool_contract.read_only_tools =
    incompleteContractSurface.tool_contract.read_only_tools.filter((tool: string) => tool !== 'mailbox_doctor');
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    incompleteContractRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  )).has('tool_contract_partition_incomplete'));

  const refusedContractRegistry: any = structuredClone(conformingRegistry);
  const refusedContractSurface: any = refusedContractRegistry.surfaces[0];
  refusedContractSurface.tool_contract.read_only_tools =
    refusedContractSurface.tool_contract.read_only_tools.filter((tool: string) => tool !== 'mailbox_doctor');
  refusedContractSurface.tool_contract.refused_tools.push('mailbox_doctor');
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    refusedContractRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  )).has('tool_contract_contains_external_refusals'));

  const duplicateContractRegistry: any = structuredClone(conformingRegistry);
  duplicateContractRegistry.surfaces[0].registered_live_tools.push('mailbox_doctor');
  duplicateContractRegistry.surfaces[0].tool_contract.read_only_tools.push('mailbox_doctor');
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    duplicateContractRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  )).has('tool_contract_contains_duplicates'));

  const incompleteLiveReadOnly: any = {
    'fixture-mailbox': observedConformanceReadOnlyTools['fixture-mailbox'].slice(1),
  };
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    conformingRegistry,
    observedConformanceTools,
    incompleteLiveReadOnly,
    observedConformanceMutatingTools,
  )).has('live_tool_semantics_partition_incomplete'));

  const overlappingLiveMutating: any = {
    'fixture-mailbox': [...observedConformanceMutatingTools['fixture-mailbox'], 'mailbox_doctor'],
  };
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    conformingRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    overlappingLiveMutating,
  )).has('live_tool_semantics_partition_overlap'));

  const duplicateLiveTools: any = {
    'fixture-mailbox': [...mailboxCatalogTools, mailboxCatalogTools[0]],
  };
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    conformingRegistry,
    duplicateLiveTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  )).has('live_tools_duplicate'));

  const projectionDriftRegistry: any = structuredClone(conformingRegistry);
  projectionDriftRegistry.surfaces[0].display_name = 'manually changed';
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    projectionDriftRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  )).has('registry_surface_projection_drift'));

  const missingCatalogRegistry: any = structuredClone(conformingRegistry);
  missingCatalogRegistry.surfaces[0].catalog_surface_id = 'missing-catalog-surface';
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    missingCatalogRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  )).has('catalog_surface_missing'));

  const conformanceFabricPath: any = join(conformanceSiteRoot, '.ai', 'mcp', 'fixture-mailbox-mcp.json');
  const duplicateFabricConfig: any = JSON.parse(readFileSync(conformanceFabricPath, 'utf8'));
  duplicateFabricConfig.mcpServers['fixture-mailbox'].tools.push('mailbox_doctor');
  writeFileSync(conformanceFabricPath, JSON.stringify(duplicateFabricConfig, null, 2), 'utf8');
  assert.ok(violationCodes(checkSiteRegistryConformance(
    conformanceSite,
    conformingRegistry,
    observedConformanceTools,
    observedConformanceReadOnlyTools,
    observedConformanceMutatingTools,
  )).has('fabric_tools_duplicate'));
  duplicateFabricConfig.mcpServers['fixture-mailbox'].tools.pop();
  writeFileSync(conformanceFabricPath, JSON.stringify(duplicateFabricConfig, null, 2), 'utf8');

  const inventoryCheck: any = view(await call('registrar_surface_tool_inventory_check', {
    include_ok: true,
    observed_tools: {
      git: bySurface.get('git')?.tools,
      mailbox: bySurface.get('mailbox')?.tools,
      'graph-mail': bySurface.get('graph-mail')?.tools,
      'worker-delegation': bySurface.get('worker-delegation')?.tools,
    },
  }));
  assert.equal(inventoryCheck.status, 'ok');
  assert.equal(inventoryCheck.checked_count, 4);
  assert.equal((inventoryCheck.findings as Array<Record<string, any>>).length, 4);
  const driftCheck: any = view(await call('registrar_surface_tool_inventory_check', {
    observed_tools: { git: ['git_status', 'git_extra_observed'] },
  }));
  assert.equal(driftCheck.status, 'drift');
  const gitDrift: any = (driftCheck.findings as Array<Record<string, any>>).find((finding: any) => finding.surface_id === 'git');
  assert.ok(gitDrift);
  assert.deepEqual(gitDrift.missing_from_registrar, ['git_extra_observed']);
  assert.ok((gitDrift.extra_in_registrar as string[]).includes('git_policy_inspect'));

  const badMaterializedSiteRoot: any = join(root, 'bad-materialized-site');
  mkdirSync(join(badMaterializedSiteRoot, '.narada', 'capabilities'), { recursive: true });
  writeFileSync(
    join(badMaterializedSiteRoot, '.narada', 'capabilities', 'mcp-surfaces.json'),
    JSON.stringify(registryWithMailboxSurface(['mailbox_message_show'], ['mailbox_message_show']), null, 2),
    'utf8',
  );
  const materializedClosureCheck: any = view(await call('registrar_site_output_reader_closure_check', {
    site_roots: [badMaterializedSiteRoot],
  }));
  assert.equal(materializedClosureCheck.status, 'drift');
  assert.equal(materializedClosureCheck.violation_count, 2);
  const materializedViolations: any = materializedClosureCheck.violations as Array<Record<string, any>>;
  assert.equal(materializedViolations[0].site_root, badMaterializedSiteRoot);
  assert.equal(materializedViolations[0].registry_path, join(badMaterializedSiteRoot, '.narada', 'capabilities', 'mcp-surfaces.json'));
  assert.equal(materializedViolations[0].server_name, 'fixture-mailbox');
  assert.equal(materializedViolations[0].producer_tool, 'mailbox_message_show');
  assert.equal(materializedViolations[0].required_reader_tool, 'mailbox_output_show');
  const missingMaterializedClosureCheck: any = view(await call('registrar_site_output_reader_closure_check', {
    site_roots: [join(root, 'site-without-materialized-registry')],
  }));
  assert.equal(missingMaterializedClosureCheck.status, 'missing');
  assert.equal(missingMaterializedClosureCheck.missing_count, 1);
  assert.equal(missingMaterializedClosureCheck.violation_count, 0);

  const missingCalendarReaderCheck: any = checkOutputReaderClosureForRegistry(
    registryWithSurface('calendar', ['calendar_event_query'], ['calendar_event_query']),
    { site_id: 'missing-calendar-reader', site_root: root, registry_path: join(root, 'missing-calendar-reader-mcp-surfaces.json') },
  );
  assert.equal(missingCalendarReaderCheck.status, 'drift');
  assert.deepEqual((missingCalendarReaderCheck.violations as Array<Record<string, any>>).map((violation) => violation.required_reader_tool), [
    'calendar_output_show',
    'calendar_output_show',
  ]);

  const goodSiteLoopReaderCheck: any = checkOutputReaderClosureForRegistry(
    registryWithSurface('site-loop', ['site_loop_guidance', 'site_loop_output_show'], ['site_loop_guidance', 'site_loop_output_show']),
    { site_id: 'good-site-loop-reader', site_root: root, registry_path: join(root, 'good-site-loop-reader-mcp-surfaces.json') },
  );
  assert.equal(goodSiteLoopReaderCheck.status, 'ok');

  const catalogDbPath: any = join(root, 'site-registry.db');
  const catalogDb: any = new DatabaseSync(catalogDbPath);
  catalogDb.exec(`
    CREATE TABLE site_registry (
      site_id TEXT PRIMARY KEY,
      site_root TEXT NOT NULL,
      created_at TEXT NOT NULL,
      lifecycle_status TEXT NOT NULL DEFAULT 'active'
    );
  `);
  catalogDb.prepare('INSERT INTO site_registry (site_id, site_root, created_at, lifecycle_status) VALUES (?, ?, ?, ?)')
    .run('fixture-canonical', root, '2026-07-10T00:00:00Z', 'active');
  catalogDb.prepare('INSERT INTO site_registry (site_id, site_root, created_at, lifecycle_status) VALUES (?, ?, ?, ?)')
    .run('fixture-retired', join(root, 'retired'), '2026-07-10T00:00:01Z', 'retired');
  catalogDb.close();
  const previousCatalogPath: any = process.env.NARADA_SITE_REGISTRY_DB;
  process.env.NARADA_SITE_REGISTRY_DB = catalogDbPath;
  try {
    const sites: any = await call('registrar_site_list', {});
    const siteData: any = view(sites);
    assert.equal(siteData.catalog_source, 'user_site_site_registry');
    assert.equal(siteData.compatibility_fallback_used, false);
    assert.deepEqual((siteData.items as Array<Record<string, unknown>>).map((site) => site.site_id), ['fixture-canonical']);
  } finally {
    if (previousCatalogPath === undefined) delete process.env.NARADA_SITE_REGISTRY_DB;
    else process.env.NARADA_SITE_REGISTRY_DB = previousCatalogPath;
  }

  const legacyCatalogDbPath: any = join(root, 'site-registry-legacy.db');
  const legacyCatalogDb: any = new DatabaseSync(legacyCatalogDbPath);
  legacyCatalogDb.exec(`
    CREATE TABLE site_registry (
      site_id TEXT PRIMARY KEY,
      site_root TEXT NOT NULL,
      created_at TEXT NOT NULL
    );
  `);
  legacyCatalogDb.prepare('INSERT INTO site_registry (site_id, site_root, created_at) VALUES (?, ?, ?)')
    .run('fixture-legacy', root, '2026-07-10T00:00:02Z');
  legacyCatalogDb.close();
  const previousLegacyCatalogPath: any = process.env.NARADA_SITE_REGISTRY_DB;
  process.env.NARADA_SITE_REGISTRY_DB = legacyCatalogDbPath;
  try {
    const sites: any = await call('registrar_site_list', {});
    const siteData: any = view(sites);
    assert.equal(siteData.catalog_source, 'user_site_site_registry');
    assert.equal(siteData.compatibility_fallback_used, false);
    assert.deepEqual((siteData.items as Array<Record<string, unknown>>).map((site) => site.site_id), ['fixture-legacy']);
  } finally {
    if (previousLegacyCatalogPath === undefined) delete process.env.NARADA_SITE_REGISTRY_DB;
    else process.env.NARADA_SITE_REGISTRY_DB = previousLegacyCatalogPath;
  }

  const carriers: any = await call('registrar_carrier_list', {});
  const carrierData: any = view(carriers);
  assert.ok((carrierData.items as Array<unknown>).length >= 3);
  const registeredSites: any = view(await call('registrar_site_list', {}));
  const registeredUserSiteRoot: any = String((registeredSites.items as Array<Record<string, any>>)
    .find((site) => site.site_id === 'andrey-user')?.root).replace(/\\/g, '/');
  const registeredSiteRoots: any = (registeredSites.items as Array<Record<string, any>>)
    .map((site) => String(site.root).replace(/\\/g, '/'));
  assert.ok(registeredSiteRoots.length > 0);
  const carrierIds: any = (carrierData.items as Array<Record<string, any>>).map((carrier) => carrier.carrier_id);
  assert.deepEqual(carrierIds.sort(), ['codex-andrey', 'kimi-andrey', 'opencode-andrey']);
  assert.equal(carrierIds.includes('opencode-sonar'), false);

  const progressiveBootstrap: any = sharedSurfaceIdsForBinding({
    site_id: 'fixture-site',
    surfaces: ['agent-context', 'mcp-registrar', 'mcp-loader', 'local-filesystem'],
    prefix: 'fixture',
    loading_mode: 'progressive',
  });
  assert.deepEqual(progressiveBootstrap, ['agent-context', 'mcp-registrar', 'mcp-loader', 'local-filesystem']);
  assert.deepEqual(
    appendLoaderAllowedSiteRoots(
      ['--allowed-site-root', 'C:\\workspace\\narada.sonar'],
      ['C:\\workspace\\narada.sonar', 'C:\\workspace\\smart-scheduling'],
    ),
    [
      '--allowed-site-root', 'C:\\workspace\\narada.sonar',
      '--allowed-site-root', 'C:/workspace/smart-scheduling',
    ],
  );
  assert.throws(
    () => sharedSurfaceIdsForBinding({ site_id: 'fixture-site', surfaces: 'all', prefix: 'fixture', loading_mode: 'progressive' }),
    /registrar_progressive_binding_requires_explicit_bootstrap/,
  );

  const materialize: any = await call('registrar_materialize_all', { output_dir: root, runtime_profile: 'bun' });
  const matData: any = view(materialize);
  assert.equal(matData.status, 'materialized_all');
  assert.equal(matData.carrier_count, 3);
  const kimiMaterialization: any = (matData.carriers as Array<Record<string, any>>).find((carrier) => carrier.carrier_id === 'kimi-andrey');
  assert.ok(kimiMaterialization);
  assert.ok(kimiMaterialization.byte_size > 0);
  assert.deepEqual((kimiMaterialization.injection_scopes as Record<string, any>).counts, { host: 0, user_site: 2, local_site: 2 });
  const materializedBinding: any = ((kimiMaterialization.injection_scopes as Record<string, any>).bindings as Array<Record<string, any>>).find((binding) => binding.site_id === 'andrey-user');
  assert.equal(materializedBinding.loading_mode, 'progressive');
  assert.deepEqual(materializedBinding.bootstrap_surface_ids, ['agent-context', 'local-filesystem', 'mcp-registrar', 'mcp-loader']);
  const materializedSurfaceIds: any = ((kimiMaterialization.injection_scopes as Record<string, any>).servers as Array<Record<string, any>>).map((server) => server.surface_id).sort();
  assert.deepEqual(materializedSurfaceIds, ['agent-context', 'local-filesystem', 'mcp-loader', 'mcp-registrar']);
  const materializedPath: any = join(root, 'mcp.json');
  assert.equal((matData.carriers as Array<Record<string, any>>).some((carrier) => carrier.output_path === materializedPath), true);
  const materializedConfig: any = JSON.parse(readFileSync(materializedPath, 'utf8')) as Record<string, any>;
  const materializedFilesystem: any = materializedConfig.mcpServers['narada-site-andrey-user-local-filesystem'];
   assertRuntimeProxy(materializedFilesystem, workspacePath('packages', 'local-filesystem-mcp', 'dist', 'src', 'main.js'), 'bun', 'bun');
  const materializedAgentContext: any = materializedConfig.mcpServers['narada-site-andrey-user-agent-context'];
   assertRuntimeProxy(materializedAgentContext, workspacePath('packages', 'agent-context-mcp', 'dist', 'src', 'main.js'), 'bun', 'bun');
  assert.ok(materializedConfig.mcpServers['narada-site-andrey-user-mcp-loader']);
  assert.ok(materializedConfig.mcpServers['narada-site-andrey-user-mcp-registrar']);
  assert.equal(materializedConfig.mcpServers['narada-site-andrey-user-site-loop'], undefined);
  const generatedPaths: Record<string, string> = {
    'codex-andrey': join(root, 'config.toml'),
    'kimi-andrey': join(root, 'mcp.json'),
    'opencode-andrey': join(root, 'opencode.jsonc'),
  };
  for (const carrierId of ['codex-andrey', 'kimi-andrey', 'opencode-andrey']) {
    const generatedPath = generatedPaths[carrierId];
    assert.equal((matData.carriers as Array<Record<string, any>>).some((carrier) => carrier.carrier_id === carrierId && carrier.output_path === generatedPath), true);
    const generatedText: any = readFileSync(generatedPath, 'utf8');
    const normalizedGeneratedText: any = generatedText.replace(/\\\\/g, '/').replace(/\\/g, '/');
    assert.equal(normalizedGeneratedText.includes('mcp-loader-mcp/dist/src/main.js'), true);
    assert.equal(normalizedGeneratedText.includes('--allowed-site-root'), true);
    assert.equal(normalizedGeneratedText.includes(portableWorkspaceRoot), true);
    for (const registeredSiteRoot of registeredSiteRoots) {
      assert.equal(normalizedGeneratedText.includes(registeredSiteRoot), true);
    }
    assert.equal(normalizedGeneratedText.includes('local-filesystem-mcp/dist/src/main.js'), true);
    assert.equal(normalizedGeneratedText.includes('agent-context-mcp/dist/src/main.js'), true);
    assert.equal(normalizedGeneratedText.includes('mcp-registrar/dist/src/main.js'), true);
    assert.equal(generatedText.includes('opencode-sonar'), false);
    assert.equal(generatedText.includes('tools/typed-mcp/inbox-mcp-server.mjs'), false);
    assert.equal(generatedText.includes('inbox_stage_submission_workflow'), false);
    assert.equal(generatedText.includes('inbox_submit_typed_envelope'), false);
    assert.equal(generatedText.includes('mcp_command_create'), false);
    assert.equal(normalizedGeneratedText.includes('site-loop-mcp/dist/src/main.js'), false);
    assert.equal(normalizedGeneratedText.includes('site-inbox-mcp/dist/src/main.js'), false);
    assert.equal(normalizedGeneratedText.includes('speech-mcp/dist/src/main.js'), false);
    assert.equal(normalizedGeneratedText.includes(registeredUserSiteRoot), true);
    if (carrierId === 'codex-andrey') {
      assert.match(generatedText, /\[features\]\r?\napps = false\r?\n/);
      assert.equal(generatedText.includes('mcp_loader_open_surface'), true);
      assert.equal(generatedText.includes('mcp_loader_call_tool'), true);
      assert.equal(generatedText.includes('inbox_submit'), false);
    }
  }

  const progressiveBulk: any = await call('registrar_sync', { target: 'all_surfaces_to_carriers', carrier_id: 'kimi-andrey' });
  assert.equal(progressiveBulk.error?.data?.code, 'registrar_progressive_bulk_bind_refused');

  const carrierValidate: any = view(await call('registrar_carrier_validate', { carrier_id: 'kimi-andrey', include_ok: true }));
  const validateFindings: any = carrierValidate.findings as Array<Record<string, any>>;
  assert.equal(validateFindings.some((finding: any) => finding.surface_id === 'speech'), false);
  assert.equal(validateFindings.some((finding: any) => finding.surface_id === 'site-loop'), false);
  assert.ok(validateFindings.some((finding: any) => finding.surface_id === 'mcp-loader'));
  assert.equal(
    validateFindings.some((finding: any) => finding.code === 'registrar_runtime_dependency_missing' && finding.dependency === '@narada-core/mcp-fabric-contracts'),
    false,
  );
  assert.equal(
    validateFindings.some((finding: any) => finding.code === 'registrar_runtime_dependency_missing' && finding.dependency === '@narada-core/mcp-loader-mcp'),
    false,
  );
  const filesystemFinding: any = validateFindings.find((finding: any) => finding.surface_id === 'local-filesystem');
  assert.ok(filesystemFinding);
  assert.equal(filesystemFinding.injection_scope, 'local_site');
  assert.equal(filesystemFinding.diagnostic_class, 'local_site_surface_missing_or_misconfigured');
  assert.equal((filesystemFinding.required_repair_locus as Record<string, any>).kind, 'local_site');
  assert.equal((filesystemFinding.narada_scope as Record<string, any>).injection_scope, 'local_site');
  assert.deepEqual(filesystemFinding.required_repair_locus, (filesystemFinding.narada_scope as Record<string, any>).mutation_locus);

  const siteDir: any = join(root, '.ai', 'mcp');
  mkdirSync(siteDir, { recursive: true });
  writeFileSync(join(root, 'site.json'), JSON.stringify({ site_id: 'test-site' }), 'utf8');

  assert.equal(siteSurfaceServerKey('narada-sonar', 'scheduler'), 'narada-sonar-scheduler');
  assert.equal(siteSurfaceServerKey('smart-scheduling', 'scheduler'), 'narada-smart-scheduling-scheduler');
  assert.equal(siteSurfaceServerKey('andrey-user', 'task-lifecycle'), 'narada-site-andrey-user-task-lifecycle');
  const userTaskBindConfig: any = buildSiteBindConfig(
    { site_id: 'andrey-user', root, config_path: join(root, 'site.json'), surfaces: [] },
    { id: 'task-lifecycle', package: 'task-lifecycle-mcp', entrypoint: 'C:/workspace/mcp-surfaces/packages/task-lifecycle-mcp/dist/src/task-lifecycle/task-mcp-server.js', kind: 'mcp_surface', args: ['--site-root', '{site_root}'], tools: ['task_lifecycle_guidance'] },
  );
  const userTaskServer: any = (userTaskBindConfig.config.mcpServers as Record<string, any>)['narada-site-andrey-user-task-lifecycle'];
  assert.ok(userTaskServer.args.includes(root));
  assert.ok(!userTaskServer.args.includes('{site_root}'));
  const bindConfig: any = buildSiteBindConfig(
    { site_id: 'narada-sonar', root, config_path: join(root, 'site.json'), surfaces: [] },
    { id: 'scheduler', package: 'scheduler-mcp', entrypoint: 'C:/workspace/mcp-surfaces/packages/scheduler-mcp/dist/src/main.js', kind: 'mcp_surface', args: ['--allowed-root', '{site_root}'], tools: ['scheduler_task_list'] },
  );
  assert.equal(bindConfig.fileName, 'narada-sonar-scheduler-mcp.json');
  assert.equal(bindConfig.serverKey, 'narada-sonar-scheduler');
  assert.ok((bindConfig.config.mcpServers as Record<string, any>)['narada-sonar-scheduler']);
  assert.ok(!(bindConfig.config.mcpServers as Record<string, any>)['sonar-scheduler']);
  const schedServer: any = (bindConfig.config.mcpServers as Record<string, any>)['narada-sonar-scheduler'];
  assert.equal(schedServer.surface_id, 'scheduler');
  assertRuntimeProxy(schedServer, workspacePath('packages', 'scheduler-mcp', 'dist', 'src', 'main.js'));
  assert.equal(schedServer.injection_scope, 'local_site');
  assert.deepEqual(schedServer.authority_locus, { kind: 'local_site', site_root: root });
  assert.equal(schedServer.narada_scope.scope_source, 'registrar_surface_catalog');
  if (nativeRuntimeArtifactAvailable) {
    const structuredBindConfig: any = buildSiteBindConfig(
      { site_id: 'structured-site', root, config_path: join(root, 'site.json'), surfaces: [] },
      {
        id: 'structured-command',
        package: 'structured-command-mcp',
        entrypoint: 'C:/workspace/mcp-surfaces/packages/structured-command-mcp/dist/src/main.js',
        kind: 'mcp_surface',
        args: ['--allowed-root', '{workspace_root}', '--allow-command', 'node'],
        tools: ['structured_command_guidance'],
      },
    );
    const structuredServer: any = (structuredBindConfig.config.mcpServers as Record<string, any>)['narada-structured-site-structured-command'];
    assert.equal(structuredServer.surface_id, 'structured-command');
    assert.equal(structuredServer.args[structuredServer.args.indexOf('--child-invocation-kind') + 1], 'native_applet');
    assert.equal(structuredServer.args[structuredServer.args.indexOf('--child-applet') + 1], 'structured-command');
    assert.match(structuredServer.command, /narada-mcp-runtime\.exe$/i);
  }
  assert.equal(schedServer.narada_scope.bound_into_site, 'narada-sonar');

  const smartSchedulingBindConfig: any = buildSiteBindConfig(
    { site_id: 'smart-scheduling', root, config_path: join(root, 'site.json'), surfaces: [] },
    { id: 'scheduler', package: 'scheduler-mcp', entrypoint: 'C:/workspace/mcp-surfaces/packages/scheduler-mcp/dist/src/main.js', kind: 'mcp_surface', args: ['--allowed-root', '{site_root}'], tools: ['scheduler_task_list'] },
  );
  assert.equal(smartSchedulingBindConfig.fileName, 'narada-smart-scheduling-scheduler-mcp.json');
  assert.equal(smartSchedulingBindConfig.serverKey, 'narada-smart-scheduling-scheduler');
  assert.ok((smartSchedulingBindConfig.config.mcpServers as Record<string, any>)['narada-smart-scheduling-scheduler']);
  assert.ok(!(smartSchedulingBindConfig.config.mcpServers as Record<string, any>)['smart-scheduling-scheduler']);

  const speechBindConfig: any = buildSiteBindConfig(
    { site_id: 'narada-staccato', root, config_path: join(root, 'site.json'), surfaces: [] },
    {
      id: 'speech',
      package: 'speech-mcp',
      entrypoint: 'C:/workspace/mcp-surfaces/packages/speech-mcp/dist/src/main.js',
      kind: 'mcp_surface',
      args: ['--provider-registry-path', 'C:/workspace/mcp-surfaces/packages/speech-mcp/config/provider-registry.v2.json'],
      tools: ['speech_speak', 'speech_voices', 'speech_capture_transcribe', 'speech_prompt_capture_response', 'speech_listen_status', 'speech_listen_start', 'speech_listen_stop'],
    },
  );
  const speechServer: any = (speechBindConfig.config.mcpServers as Record<string, any>)['narada-staccato-speech'];
  assert.equal(speechServer.injection_scope, 'host');
  assertRuntimeProxy(speechServer, workspacePath('packages', 'speech-mcp', 'dist', 'src', 'main.js'));
  assert.equal(speechServer.authority_posture, 'host_injected_mcp_surface');
  assert.deepEqual(speechServer.authority_locus, { kind: 'host' });
  assert.equal(speechServer.bound_into_site, 'narada-staccato');
  assert.equal(speechServer.narada_scope.scope_source, 'registrar_surface_catalog');
  assert.equal(speechServer.narada_scope.injection_scope, 'host');
  assert.equal(speechServer.narada_scope.bound_into_site, 'narada-staccato');
  assert.deepEqual(speechServer.tools, ['speech_speak', 'speech_voices', 'speech_capture_transcribe', 'speech_prompt_capture_response', 'speech_listen_status', 'speech_listen_start', 'speech_listen_stop']);

  const workerBindConfig: any = buildSiteBindConfig(
    { site_id: 'narada-sonar', root, config_path: join(root, 'site.json'), surfaces: [] },
    {
      id: 'worker-delegation',
      package: 'worker-delegation-mcp',
      entrypoint: 'C:/workspace/mcp-surfaces/packages/worker-delegation-mcp/dist/src/main.js',
      kind: 'mcp_surface',
      args: ['--site-root', '{site_root}', '--allowed-root', '{site_root}', '--run-root', '{site_runtime_root}/worker-delegation'],
      tools: ['worker_run'],
      env_vars: ['DEEPSEEK_API_KEY', 'DEEPSEEK_API_BASE_URL', 'NARADA_WORKER_MCP_CONFIG'],
    },
  );
  const workerServer: any = (workerBindConfig.config.mcpServers as Record<string, any>)['narada-sonar-worker-delegation'];
  assert.equal(workerServer.surface_id, 'worker-delegation');
  assertRuntimeProxy(workerServer, workspacePath('packages', 'worker-delegation-mcp', 'dist', 'src', 'main.js'));
  assert.ok(workerServer.args.includes('--site-root'));
  assert.equal(workerServer.args[workerServer.args.indexOf('--site-root') + 1], root);
  assert.equal(String(workerServer.args[workerServer.args.indexOf('--run-root') + 1]).replace(/\\/g, '/'), join(root, '.narada', 'runtime', 'worker-delegation').replace(/\\/g, '/'));
  assert.ok(workerServer.env_vars.includes('DEEPSEEK_API_KEY'));
  assert.ok(workerServer.env_vars.includes('DEEPSEEK_API_BASE_URL'));
  assert.ok(workerServer.env_vars.includes('NARADA_WORKER_MCP_CONFIG'));

  const controlRootSite: any = join(root, 'control-root-site', '.narada');
  const controlRootWorkspace: any = join(root, 'control-root-site');
  mkdirSync(controlRootSite, { recursive: true });
  writeFileSync(join(controlRootSite, 'config.json'), JSON.stringify({ workspace_root: controlRootWorkspace }), 'utf8');
  const controlRootWorkerBindConfig: any = buildSiteBindConfig(
    { site_id: 'smart-scheduling', root: controlRootSite, config_path: join(controlRootSite, 'config.json'), surfaces: [] },
    {
      id: 'worker-delegation',
      package: 'worker-delegation-mcp',
      entrypoint: 'C:/workspace/mcp-surfaces/packages/worker-delegation-mcp/dist/src/main.js',
      kind: 'mcp_surface',
      args: ['--site-root', '{site_root}', '--allowed-root', '{workspace_root}', '--run-root', '{site_runtime_root}/worker-delegation'],
      tools: ['worker_run'],
    },
  );
  const controlRootWorkerServer: any = (controlRootWorkerBindConfig.config.mcpServers as Record<string, any>)['narada-smart-scheduling-worker-delegation'];
  const controlRootRunRoot: any = String(controlRootWorkerServer.args[controlRootWorkerServer.args.indexOf('--run-root') + 1]);
  assert.equal(controlRootWorkerServer.args[controlRootWorkerServer.args.indexOf('--site-root') + 1], controlRootWorkspace);
  assert.equal(controlRootWorkerServer.args[controlRootWorkerServer.args.indexOf('--allowed-root') + 1], controlRootWorkspace);
  assert.equal(controlRootRunRoot.replace(/\\/g, '/'), join(controlRootSite, 'runtime', 'worker-delegation').replace(/\\/g, '/'));
  assert.equal(controlRootRunRoot.replace(/\\/g, '/').includes('/.narada/.narada/'), false);

  const surfaceFeedbackBindConfig: any = buildSiteBindConfig(
    { site_id: 'narada-staccato', root, config_path: join(root, 'site.json'), surfaces: [] },
    {
      id: 'surface-feedback',
      package: 'surface-feedback-mcp',
      entrypoint: 'C:/workspace/mcp-surfaces/packages/surface-feedback-mcp/dist/src/main.js',
      kind: 'mcp_surface',
      args: [
        '--feedback-root', 'C:/workspace/mcp-surfaces',
        '--canonical-feedback-root', 'C:/workspace/mcp-surfaces',
        '--site-id', 'narada-staccato',
        '--owned-surface-id', 'surface-feedback',
      ],
      tools: ['surface_feedback_submit'],
    },
  );
  const surfaceFeedbackServer: any = (surfaceFeedbackBindConfig.config.mcpServers as Record<string, any>)['narada-staccato-surface-feedback'];
  assertRuntimeProxy(surfaceFeedbackServer, workspacePath('packages', 'surface-feedback-mcp', 'dist', 'src', 'main.js'));
  assert.equal(surfaceFeedbackServer.args[surfaceFeedbackServer.args.indexOf('--feedback-root') + 1], 'C:/workspace/mcp-surfaces');
  assert.equal(surfaceFeedbackServer.args[surfaceFeedbackServer.args.indexOf('--canonical-feedback-root') + 1], 'C:/workspace/mcp-surfaces');
  assert.equal(surfaceFeedbackServer.args[surfaceFeedbackServer.args.indexOf('--site-id') + 1], 'narada-staccato');
  assert.equal(surfaceFeedbackServer.args[surfaceFeedbackServer.args.indexOf('--owned-surface-id') + 1], 'surface-feedback');

  const scopeReadbackRoot: any = join(root, 'scope-readback-site');
  mkdirSync(join(scopeReadbackRoot, '.ai', 'mcp'), { recursive: true });
  writeFileSync(join(scopeReadbackRoot, '.ai', 'mcp', 'narada-staccato-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'staccato-speech': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/speech-mcp/dist/src/main.js', '--provider-registry-path', 'C:/workspace/mcp-surfaces/packages/speech-mcp/config/provider-registry.v2.json'],
        narada_scope: {
          injection_scope: 'host',
          authority_locus: { kind: 'host' },
          mutation_locus: { kind: 'host' },
          restart_owner: 'host',
          bound_into_site: 'narada-staccato',
          scope_source: 'registrar_surface_catalog',
        },
      },
      'staccato-mcp-registrar': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/mcp-registrar/dist/src/main.js'],
        injection_scope: 'user_site',
        authority_locus: { kind: 'user_site', site_root: 'C:/Users/Andrey/Narada' },
        mutation_locus: { kind: 'user_site', site_root: 'C:/Users/Andrey/Narada' },
        restart_owner: 'user_site',
      },
      'staccato-local-filesystem': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/local-filesystem-mcp/dist/src/main.js', '--mode', 'write', '--allowed-root', scopeReadbackRoot, '--output-root', scopeReadbackRoot],
      },
      'narada-staccato-artifacts': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/artifacts-mcp/dist/src/main.js'],
        surface_id: 'artifacts',
      },
    },
  }, null, 2), 'utf8');
  const scopeReadback: any = validateSiteMcpFabric({ site_id: 'narada-staccato', root: scopeReadbackRoot, config_path: join(scopeReadbackRoot, 'site.json'), surfaces: [] }, true);
  const scopeFindings: any = scopeReadback.findings as Array<Record<string, any>>;
  const speechScopeFinding: any = scopeFindings.find((finding: any) => finding.server_key === 'staccato-speech' && finding.code === 'registrar_site_fabric_server_key_ok');
  assert.ok(speechScopeFinding);
  assert.equal(speechScopeFinding.scope_source, 'site_config_narada_scope');
  assert.equal(speechScopeFinding.injection_scope, 'host');
  assert.equal((speechScopeFinding.narada_scope as Record<string, any>).scope_source, 'site_config_narada_scope');
  assert.deepEqual(speechScopeFinding.required_repair_locus, (speechScopeFinding.narada_scope as Record<string, any>).mutation_locus);
  const registrarScopeFinding: any = scopeFindings.find((finding: any) => finding.server_key === 'staccato-mcp-registrar' && finding.code === 'registrar_site_fabric_server_key_ok');
  assert.ok(registrarScopeFinding);
  assert.equal(registrarScopeFinding.scope_source, 'site_config_legacy_top_level');
  assert.equal(registrarScopeFinding.injection_scope, 'user_site');
  assert.equal((registrarScopeFinding.narada_scope as Record<string, any>).scope_source, 'site_config_legacy_top_level');
  const filesystemScopeFinding: any = scopeFindings.find((finding: any) => finding.server_key === 'staccato-local-filesystem' && finding.code === 'registrar_site_fabric_server_key_ok');
  assert.ok(filesystemScopeFinding);
  assert.equal(filesystemScopeFinding.scope_source, 'registrar_surface_catalog');
  assert.equal(filesystemScopeFinding.injection_scope, 'local_site');
  assert.equal((filesystemScopeFinding.narada_scope as Record<string, any>).scope_source, 'registrar_surface_catalog');

  const missingDefaultRoot: any = join(root, 'missing-default-site');
  mkdirSync(join(missingDefaultRoot, '.ai', 'mcp'), { recursive: true });
  writeFileSync(join(missingDefaultRoot, 'site.json'), JSON.stringify({ site_id: 'narada-sonar' }), 'utf8');
  writeFileSync(join(missingDefaultRoot, '.ai', 'mcp', 'narada-sonar-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-sonar-agent-context': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/agent-context-mcp/dist/src/main.js'],
      },
    },
  }, null, 2), 'utf8');
  const missingDefault: any = validateSiteMcpFabric({ site_id: 'narada-sonar', root: missingDefaultRoot, config_path: join(missingDefaultRoot, 'site.json'), surfaces: [] }, false);
  const missingDefaultFinding: any = (missingDefault.findings as Array<Record<string, any>>).find((finding: any) => finding.code === 'registrar_site_fabric_missing_default_surface' && finding.surface_id === 'artifacts');
  assert.ok(missingDefaultFinding);
  assert.equal(missingDefault.status, 'invalid');

  writeFileSync(join(missingDefaultRoot, '.ai', 'mcp', 'narada-sonar-work-lifecycle-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-sonar-work-lifecycle': {
        surface_id: 'work-lifecycle',
        transport: 'stdio',
        command: 'node',
        args: [
          'C:/workspace/mcp-surfaces/packages/work-lifecycle-mcp/dist/src/main.js',
          '--site-root',
          missingDefaultRoot,
        ],
      },
    },
  }, null, 2), 'utf8');
  const replacementDefault: any = validateSiteMcpFabric({ site_id: 'narada-sonar', root: missingDefaultRoot, config_path: join(missingDefaultRoot, 'site.json'), surfaces: [] }, false);
  const replacementFindings: any = replacementDefault.findings as Array<Record<string, any>>;
  assert.equal(replacementFindings.some((finding: any) => finding.code === 'registrar_site_fabric_missing_default_surface' && finding.surface_id === 'task-lifecycle'), false);
  assert.equal(replacementFindings.some((finding: any) => finding.code === 'registrar_site_fabric_missing_default_surface' && finding.surface_id === 'artifacts'), true);

  const staleProjectionRoot: any = join(root, 'stale-carrier-projection-site');
  const staleProjectionMcpRoot: any = join(staleProjectionRoot, '.ai', 'mcp');
  mkdirSync(join(staleProjectionMcpRoot, 'carriers'), { recursive: true });
  writeFileSync(join(staleProjectionRoot, 'site.json'), JSON.stringify({ site_id: 'andrey-user' }), 'utf8');
  writeFileSync(join(staleProjectionMcpRoot, 'narada-site-andrey-user-site-inbox-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-site-andrey-user-site-inbox': {
        surface_id: 'site-inbox',
        command: 'node',
        args: [
          'C:/workspace/mcp-surfaces/packages/shared/mcp-runtime-proxy/dist/src/main.js',
          '--surface-id', 'site-inbox',
          '--entrypoint', 'C:/workspace/mcp-surfaces/packages/site-inbox-mcp/dist/src/main.js',
          '--', '--site-root', staleProjectionRoot,
        ],
      },
    },
  }, null, 2), 'utf8');
  writeFileSync(join(staleProjectionMcpRoot, 'narada-site-andrey-user-inbox-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-site-andrey-user-inbox': {
        surface_id: 'inbox',
        command: 'node',
        args: ['C:/Users/Andrey/Narada/tools/typed-mcp/inbox-mcp-server.mjs', '--site-root', staleProjectionRoot],
      },
    },
  }, null, 2), 'utf8');
  writeFileSync(join(staleProjectionMcpRoot, 'carriers', 'narada-site-andrey-user-kimi.mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-site-andrey-user-inbox': {
        surface_id: 'site-inbox',
        command: 'node',
        args: ['C:/Users/Andrey/Narada/tools/typed-mcp/inbox-mcp-server.mjs', '--site-root', staleProjectionRoot],
      },
      'narada-site-andrey-user-delegated-task': {
        surface_id: 'delegated-task',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/delegated-task-mcp/dist/src/main.js'],
      },
    },
  }, null, 2), 'utf8');
  const staleProjectionValidation: any = validateSiteMcpFabric({
    site_id: 'andrey-user',
    root: staleProjectionRoot,
    config_path: join(staleProjectionRoot, 'site.json'),
    surfaces: [],
  }, false);
  const staleProjectionFindings: any = staleProjectionValidation.findings as Array<Record<string, any>>;
  assert.equal(staleProjectionValidation.status, 'invalid');
  assert.ok(staleProjectionFindings.some((finding: any) => finding.code === 'registrar_carrier_projection_entrypoint_drift' && finding.surface_id === 'site-inbox'));
  assert.ok(staleProjectionFindings.some((finding: any) => finding.code === 'registrar_carrier_projection_missing_site_root' && finding.surface_id === 'delegated-task'));
  assert.ok(staleProjectionFindings.some((finding: any) => finding.code === 'registrar_site_fabric_duplicate_canonical_surface' && finding.canonical_surface_id === 'site-inbox'));
  assert.equal(staleProjectionValidation.carrier_projection_count, 2);

  const allMaterialization: any = view(await call('registrar_materialize_all', { output_dir: root }));
  assert.equal(allMaterialization.status, 'materialized_all');
  assert.equal(allMaterialization.carrier_count, 3);
  const contractGeneratedPaths: Record<string, string> = {
    'opencode-andrey': join(root, 'opencode.jsonc'),
    'kimi-andrey': join(root, 'mcp.json'),
    'codex-andrey': join(root, 'config.toml'),
  };
  for (const carrierId of ['opencode-andrey', 'kimi-andrey', 'codex-andrey']) {
    const outputPath = contractGeneratedPaths[carrierId];
    assert.equal((allMaterialization.carriers as Array<Record<string, any>>).some((carrier) => carrier.carrier_id === carrierId && carrier.output_path === outputPath), true);
    const content: any = readFileSync(outputPath, 'utf8');
    assert.match(content, /mcp-loader/);
    assert.match(content, /local-filesystem/);
    assert.match(content, /agent-context/);
    assert.match(content, /mcp-registrar/);
    assert.doesNotMatch(content, /surface-feedback/);
    assert.doesNotMatch(content, /--feedback-root/);
    assert.doesNotMatch(content, /--canonical-feedback-root/);
    assert.doesNotMatch(content, /--task-lifecycle-root/);
    assert.match(content, /--site-id/);
    assert.doesNotMatch(content, /--owned-surface-id/);
    if (carrierId === 'codex-andrey') {
      assert.match(content, /--anchored-allowed-root/);
      assert.match(content, /user_home:\.codex/);
      assert.match(content, /\[mcp_servers\.narada-site-andrey-user-local-filesystem\][\s\S]*?approval_mode = "approve"/);
      assert.doesNotMatch(content, /\[mcp_servers\.narada-site-andrey-user-site-loop\]/);
      assert.doesNotMatch(content, /\[mcp_servers\.narada-site-andrey-user-calendar\]/);
      assert.match(content, /Generated carrier availability metadata\. Narada MCP surfaces own policy\./);
      assert.match(content, /\[mcp_servers\.narada-site-andrey-user-local-filesystem\.tools\.fs_apply_patch\]\s+approval_mode = "approve"/);
      assert.doesNotMatch(content, /\[mcp_servers\.narada-site-andrey-user-structured-command\.tools\.structured_command_execute\]/);
      assert.doesNotMatch(content, /approval_mode = "auto"/);
    }
  }

  const aggregateSiteRoot: any = join(root, 'aggregate-site');
  mkdirSync(join(aggregateSiteRoot, '.ai', 'mcp'), { recursive: true });
  writeFileSync(join(aggregateSiteRoot, 'site.json'), JSON.stringify({ site_id: 'narada-sonar' }), 'utf8');
  writeFileSync(join(aggregateSiteRoot, '.ai', 'mcp', 'narada-sonar-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-sonar-agent-context': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/agent-context-mcp/dist/src/main.js'],
      },
    },
  }, null, 2), 'utf8');
  const aggregateBindConfig: any = buildSiteBindConfig(
    { site_id: 'narada-sonar', root: aggregateSiteRoot, config_path: join(aggregateSiteRoot, 'site.json'), surfaces: [] },
    sched as any,
  );
  assert.equal(aggregateBindConfig.serverKey, 'narada-sonar-scheduler');
  const artifactsBindConfig: any = buildSiteBindConfig(
    { site_id: 'narada-sonar', root: aggregateSiteRoot, config_path: join(aggregateSiteRoot, 'site.json'), surfaces: [] },
    artifacts as any,
  );
  const artifactsServer: any = (artifactsBindConfig.config.mcpServers as Record<string, any>)['narada-sonar-artifacts'];
  assert.equal(artifactsServer.surface_id, 'artifacts');
  assert.ok((artifactsServer.env_vars as string[]).includes('NARADA_SESSION_ID'));
  assert.equal((artifactsServer.env_vars as string[]).length, new Set(artifactsServer.env_vars as string[]).size);
  writeFileSync(join(aggregateSiteRoot, '.ai', 'mcp', artifactsBindConfig.fileName), JSON.stringify(artifactsBindConfig.config, null, 2), 'utf8');
  const aggregateWithArtifacts: any = validateSiteMcpFabric({ site_id: 'narada-sonar', root: aggregateSiteRoot, config_path: join(aggregateSiteRoot, 'site.json'), surfaces: [] }, true);
  const aggregateArtifactFinding: any = (aggregateWithArtifacts.findings as Array<Record<string, any>>).find((finding: any) => finding.server_key === 'narada-sonar-artifacts' && finding.code === 'registrar_site_fabric_server_key_ok');
  assert.ok(aggregateArtifactFinding);
  assert.equal(aggregateArtifactFinding.surface_id, 'artifacts');
  writeFileSync(join(aggregateSiteRoot, '.ai', 'mcp', 'narada-sonar-inbox-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-sonar-inbox': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/site-inbox-mcp/dist/src/main.js', '--site-root', aggregateSiteRoot],
        tools: ['inbox_doctor', 'inbox_list', 'inbox_show'],
      },
    },
  }, null, 2), 'utf8');
  const narsBindConfig: any = buildSiteBindConfig(
    { site_id: 'narada-sonar', root: aggregateSiteRoot, config_path: join(aggregateSiteRoot, 'site.json'), surfaces: [] },
    narsSession as any,
    'local-site-nars-runtime',
  );
  writeFileSync(join(aggregateSiteRoot, '.ai', 'mcp', narsBindConfig.fileName), JSON.stringify(narsBindConfig.config, null, 2), 'utf8');
  const surfaceRegistry: any = buildSiteSurfaceRegistry({ site_id: 'narada-sonar', root: aggregateSiteRoot, config_path: join(aggregateSiteRoot, 'site.json'), surfaces: [] });
  assert.equal(surfaceRegistry.artifact_role, 'site_capability_surface_registry_not_mcp_client_config');
  assertOutputReaderClosure(surfaceRegistry, 'aggregate surface registry');
  const inboxRegistry: any = (surfaceRegistry.surfaces as Array<Record<string, any>>).find((surface) => surface.server_name === 'narada-sonar-inbox');
  assert.ok(inboxRegistry);
  assert.equal(inboxRegistry.surface_type, 'mcp_surface');
  assert.equal(inboxRegistry.runtime_binding.runtime_kind, 'node-stdio');
  assert.equal(inboxRegistry.runtime_binding.owner_site_id, 'narada-sonar');
  assert.equal(isAbsolute(inboxRegistry.runtime_binding.transport.command), true);
  assert.equal(basename(inboxRegistry.runtime_binding.transport.command).toLowerCase(), process.platform === 'win32' ? 'node.exe' : 'node');
  assert.deepEqual(inboxRegistry.runtime_binding.transport.args, [
    'C:/workspace/mcp-surfaces/packages/site-inbox-mcp/dist/src/main.js',
    '--site-root',
    aggregateSiteRoot,
  ]);
  assert.deepEqual(inboxRegistry.tool_contract.semantic_operations, []);
  assert.deepEqual(inboxRegistry.tool_contract.deprecated_aliases, {});
  assert.deepEqual(inboxRegistry.tool_contract.exposed_tools, inboxRegistry.registered_live_tools);
  assert.deepEqual(inboxRegistry.evidence, {
    source: 'site_mcp_fabric',
    path: '.ai/mcp/narada-sonar-inbox-mcp.json',
    projection_kind: 'site_fabric',
  });
  assert.equal(inboxRegistry.catalog_surface_id, 'site-inbox');
  assert.ok((inboxRegistry.registered_live_tools as string[]).includes('inbox_acknowledge'));
  assert.ok((inboxRegistry.tool_contract.mutating_tools as string[]).includes('inbox_acknowledge'));
  const narsRegistry: any = (surfaceRegistry.surfaces as Array<Record<string, any>>).find((surface) => surface.server_name === 'narada-sonar-nars-session');
  assert.ok(narsRegistry);
  assert.equal(narsRegistry.catalog_surface_id, 'nars-session');
  assert.equal(narsRegistry.surface_projection.projection_id, 'local-site-nars-runtime');
  assert.equal(narsRegistry.surface_projection.injection_scope, 'local_site');
  assert.deepEqual(narsRegistry.surface_projection.runtime_requirements, ['nars']);
  writeFileSync(join(aggregateSiteRoot, '.ai', 'mcp', 'narada-sonar-mailbox-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-sonar-mailbox': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/mailbox-mcp/dist/src/main.js', '--site-root', aggregateSiteRoot],
        tools: ['mailbox_message_show'],
      },
    },
  }, null, 2), 'utf8');
  writeFileSync(join(aggregateSiteRoot, '.ai', 'mcp', 'narada-sonar-graph-mail-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-sonar-graph-mail': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/graph-mail-mcp/dist/src/main.js', '--site-root', aggregateSiteRoot],
        tools: ['graph_mail_message_show'],
      },
    },
  }, null, 2), 'utf8');
  writeFileSync(join(aggregateSiteRoot, '.ai', 'mcp', 'narada-sonar-mcp-loader-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-sonar-mcp-loader': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/mcp-loader-mcp/dist/src/main.js'],
        tools: [
          'mcp_loader_process_ownership',
          'mcp_loader_surface_handle_inventory',
          'mcp_loader_read_result',
          'mcp_loader_runtime_status',
        ],
        surface_id: 'mcp-loader',
      },
    },
  }, null, 2), 'utf8');
  const mailRegistry: any = buildSiteSurfaceRegistry({ site_id: 'narada-sonar', root: aggregateSiteRoot, config_path: join(aggregateSiteRoot, 'site.json'), surfaces: [] });
  assertOutputReaderClosure(mailRegistry, 'mail surface registry');
  const mailboxRegistry: any = (mailRegistry.surfaces as Array<Record<string, any>>).find((surface) => surface.catalog_surface_id === 'mailbox');
  assert.ok(mailboxRegistry);
  assert.ok((mailboxRegistry.registered_live_tools as string[]).includes('mailbox_output_show'));
  assert.ok((mailboxRegistry.tool_contract.read_only_tools as string[]).includes('mailbox_output_show'));
  assert.deepEqual(mailboxRegistry.tool_contract.mutating_tools, [
    'mailbox_sync_generation',
    'mailbox_reconcile_first_observations',
    'mailbox_message_admit',
    'mailbox_outbox_consumer_register',
    'mailbox_outbox_ack',
  ]);
  const graphMailRegistry: any = (mailRegistry.surfaces as Array<Record<string, any>>).find((surface) => surface.catalog_surface_id === 'graph-mail');
  assert.ok(graphMailRegistry);
  assert.ok((graphMailRegistry.registered_live_tools as string[]).includes('graph_mail_output_show'));
  assert.ok((graphMailRegistry.tool_contract.read_only_tools as string[]).includes('graph_mail_output_show'));
  assert.ok((graphMailRegistry.tool_contract.read_only_tools as string[]).includes('graph_mail_ticket_draft_disposition_list'));
  assert.equal((graphMailRegistry.tool_contract.mutating_tools as string[]).includes('graph_mail_ticket_draft_disposition_scan'), true);
  assert.equal((graphMailRegistry.tool_contract.mutating_tools as string[]).includes('graph_mail_ticket_draft_disposition_ack'), true);
  assert.equal((graphMailRegistry.tool_contract.refused_tools as string[]).includes('graph_mail_draft_send'), false);
  assert.equal((graphMailRegistry.tool_contract.mutating_tools as string[]).includes('graph_mail_draft_send'), true);
  const loaderRegistry: any = (mailRegistry.surfaces as Array<Record<string, any>>).find((surface) => surface.catalog_surface_id === 'mcp-loader');
  assert.ok(loaderRegistry);
  for (const toolName of [
    'mcp_loader_process_ownership',
    'mcp_loader_surface_handle_inventory',
    'mcp_loader_read_result',
  ]) {
    assert.ok((loaderRegistry.tool_contract.read_only_tools as string[]).includes(toolName));
    assert.equal((loaderRegistry.tool_contract.mutating_tools as string[]).includes(toolName), false);
  }

  writeFileSync(join(aggregateSiteRoot, '.ai', 'mcp', 'narada-sonar-surface-feedback-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-sonar-surface-feedback': {
        transport: 'stdio',
        command: 'node',
        args: [
          'C:/workspace/mcp-surfaces/packages/surface-feedback-mcp/dist/src/main.js',
          '--feedback-root', 'C:/workspace/mcp-surfaces',
          '--canonical-feedback-root', 'C:/workspace/mcp-surfaces',
          '--site-id', 'narada-sonar',
          '--owned-surface-id', 'surface-feedback',
        ],
        tools: [
          'surface_feedback_guidance',
          'surface_feedback_doctor',
          'surface_feedback_submit',
          'surface_feedback_update_status',
          'surface_feedback_update_status_batch',
          'surface_feedback_import',
          'surface_feedback_list',
          'surface_feedback_actionable_queue',
          'surface_feedback_convert_to_task',
          'surface_feedback_show',
          'surface_feedback_stats',
          'surface_feedback_live_proof_template',
        ],
      },
    },
  }, null, 2), 'utf8');
  const feedbackRegistry: any = buildSiteSurfaceRegistry({ site_id: 'narada-sonar', root: aggregateSiteRoot, config_path: join(aggregateSiteRoot, 'site.json'), surfaces: [] });
  const feedbackSurface: any = (feedbackRegistry.surfaces as Array<Record<string, any>>).find((surface) => surface.catalog_surface_id === 'surface-feedback');
  assert.ok(feedbackSurface);
  assert.ok((feedbackSurface.registered_live_tools as string[]).includes('surface_feedback_actionable_queue'));
  assert.ok((feedbackSurface.registered_live_tools as string[]).includes('surface_feedback_convert_to_task'));
  assert.ok((feedbackSurface.tool_contract.read_only_tools as string[]).includes('surface_feedback_actionable_queue'));
  assert.equal((feedbackSurface.tool_contract.read_only_tools as string[]).includes('surface_feedback_convert_to_task'), false);
  assert.ok((feedbackSurface.tool_contract.mutating_tools as string[]).includes('surface_feedback_convert_to_task'));

  const nestedSiteRoot: any = join(root, 'nested-control-site');
  mkdirSync(join(nestedSiteRoot, '.narada', '.ai', 'mcp'), { recursive: true });
  writeFileSync(join(nestedSiteRoot, '.narada', '.ai', 'mcp', 'narada-sonar-mailbox-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-sonar-mailbox': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/mailbox-mcp/dist/src/main.js', '--site-root', join(nestedSiteRoot, '.narada')],
        tools: ['mailbox_message_show'],
      },
    },
  }, null, 2), 'utf8');
  const nestedRegistry: any = buildSiteSurfaceRegistry({ site_id: 'narada-sonar', root: nestedSiteRoot, config_path: join(nestedSiteRoot, '.narada', 'config.json'), surfaces: [] });
  assertOutputReaderClosure(nestedRegistry, 'nested surface registry');
  assert.equal((nestedRegistry.surfaces as Array<Record<string, any>>).length, 1);
  assert.ok(((nestedRegistry.surfaces as Array<Record<string, any>>)[0].tool_contract.read_only_tools as string[]).includes('mailbox_output_show'));
  const sidecarRefusal: any = siteBindSidecarRefusal(
    { site_id: 'narada-sonar', root: aggregateSiteRoot, config_path: join(aggregateSiteRoot, 'site.json'), surfaces: [] },
    'scheduler',
  );
  assert.equal(sidecarRefusal?.status, 'refused');
  assert.equal(sidecarRefusal?.reason_code, 'registrar_site_bind_refused_aggregate_fabric_exists');
  assert.equal(sidecarRefusal?.aggregate_file, 'narada-sonar-mcp.json');
  assert.equal(siteBindSidecarRefusal(
    { site_id: 'narada-sonar', root: aggregateSiteRoot, config_path: join(aggregateSiteRoot, 'site.json'), surfaces: [] },
    'scheduler',
    { allow_sidecar: true },
  ), null);
  const disabledSidecarRefusal: any = siteBindSidecarRefusal(
    { site_id: 'narada-sonar', root: aggregateSiteRoot, config_path: join(aggregateSiteRoot, 'site.json'), surfaces: [], surface_overrides: { scheduler: { enabled: false } } },
    'scheduler',
  );
  assert.equal(disabledSidecarRefusal?.status, 'refused');
  assert.equal(disabledSidecarRefusal?.reason_code, 'registrar_site_bind_refused_surface_disabled');
  assert.equal(disabledSidecarRefusal?.sidecar_state, 'disabled_by_site_override');
  assert.equal(siteBindSidecarRefusal(
    { site_id: 'narada-sonar', root: aggregateSiteRoot, config_path: join(aggregateSiteRoot, 'site.json'), surfaces: [], surface_overrides: { scheduler: { enabled: false } } },
    'scheduler',
    { allow_disabled_sidecar: true, allow_sidecar: true },
  ), null);

  const unresolvedTemplateRoot: any = join(root, 'unresolved-template-site');
  mkdirSync(join(unresolvedTemplateRoot, '.ai', 'mcp'), { recursive: true });
  writeFileSync(join(unresolvedTemplateRoot, '.ai', 'mcp', 'narada-sonar-task-lifecycle-mcp.json'), JSON.stringify({
    schema: 'narada.mcp.client_config.v0',
    mcpServers: {
      'narada-sonar-task-lifecycle': {
        transport: 'stdio',
        command: 'node',
        args: ['C:/workspace/mcp-surfaces/packages/task-lifecycle-mcp/dist/src/task-lifecycle/task-mcp-server.js', '--site-root', '{site_root}'],
        tools: ['task_lifecycle_guidance'],
      },
    },
  }, null, 2), 'utf8');
  const unresolvedTemplateCheck: any = validateSiteMcpFabric({ site_id: 'narada-sonar', root: unresolvedTemplateRoot, config_path: join(unresolvedTemplateRoot, 'config.json'), surfaces: [] });
  assert.ok((unresolvedTemplateCheck.findings as Array<Record<string, any>>)
    .some((finding: any) => finding.code === 'registrar_site_fabric_unresolved_template'));

  console.log('mcp-registrar behavior ok');
} finally {
  rmSync(root, { recursive: true, force: true });
}
