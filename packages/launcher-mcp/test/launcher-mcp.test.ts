import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { assertLiveToolsConform } from '@narada-core/mcp-fabric-contracts';
import { createServerState, handleRequest, launcherSurfaceDefinition } from '../src/main.js';
import { createSurfaceRuntime } from '../src/runtime-factory.js';

const root = mkdtempSync(join(tmpdir(), 'launcher-mcp-'));
const registryPath = join(root, 'agents.psd1');
writeFileSync(registryPath, `@{
  NaradaRoot = "${root}"
  WorkspaceRoot = "${root}"
  SiteRoot = "${root}"
  Launcher = "narada-test.ps1"
  OperatorSurface = "agent-cli"
  Runtime = "narada-agent-runtime-server"
  Authority = "auto"
  Agents = @(
    @{ Agent = "narada-test.architect"; Title = "Test Architect"; Site = "narada-test"; Role = "architect"; Profile = "planning"; EnableNativeShell = $false }
    @{ Agent = "narada-test.builder2"; Title = "Test Builder"; Site = "narada-test"; Profile = "implementation"; EnableNativeShell = $true }
  )
}
`, 'utf8');

try {
  const state = createServerState({ naradaRoot: root, registryPath });
  const surface = launcherSurfaceDefinition();
  assert.equal(surface.descriptor.projections[0]!.lifecycle.mode, 'replayable');
  const factoryProjection = surface.descriptor.projections.find((projection) => projection.id === 'factory');
  assert.deepEqual(factoryProjection?.execution, {
    adapter: 'surface_factory',
    tenancy: 'authority_shared',
    replacement: 'generation_swap',
  });
  assert.equal(surface.descriptor.guidance_tool, 'launcher_guidance');
  const liveList = await ((handleRequest(
    { jsonrpc: '2.0', id: 99, method: 'tools/list', params: {} },
    state,
  )) as any) as any as Record<string, any>;
  assertLiveToolsConform(surface.descriptor, liveList.result.tools);
  async function call(name: string, args: Record<string, unknown>): Promise<Record<string, any>> {
    return handleRequest({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args } }, state) as Promise<Record<string, any>>;
  }
  function view(res: Record<string, any>): Record<string, any> { return res.result.structuredContent as Record<string, any>; }

  const doctor = view(await call('launcher_doctor', {}));
  assert.equal(doctor.registry_exists, true);
  assert.equal(doctor.execution_posture, 'read_only_no_launch_no_shell');
  assert.equal(doctor.fabric_lifecycle.mode, 'replayable');
  assert.equal(doctor.fabric_lifecycle.projection_id, 'stdio');
  assert.deepEqual(doctor.fabric_lifecycle.execution, {
    adapter: 'stdio',
    tenancy: 'session_isolated',
    replacement: 'manual',
  });
  assert.equal(doctor.fabric_lifecycle.reconnect, 'start_fresh_stdio_process');
  assert.deepEqual(doctor.mcp_injection_scope_doctrine.scopes, ['host', 'user_site', 'local_site']);
  assert.equal(doctor.mcp_injection_scope_doctrine.canonical_host_example, 'speech');

  const runtime = await createSurfaceRuntime({
    binding_id: 'launcher-factory',
    site_id: 'test-site',
    authority_ref: 'site:test-site:mcp-surfaces',
    surface_id: 'launcher',
    projection_id: 'factory',
    generation_id: 'generation-1',
    tool_contract_digest: surface.tool_contract_digest,
    site_root: root,
    configuration: { narada_root: root, registry_path: registryPath },
  });
  const factoryDoctor = await runtime.callTool({ tool_name: 'launcher_doctor', arguments: {} }, {
    request_id: 'request-1',
    carrier_session_id: 'session-1',
    carrier_id: 'test-carrier',
    agent_id: 'test-agent',
    site_id: 'test-site',
    authority_ref: 'site:test-site:mcp-surfaces',
    admission: {
      decision: 'admitted',
      decision_ref: 'decision-1',
      authority_ref: 'site:test-site:mcp-surfaces',
      surface_id: 'launcher',
      tool_name: 'launcher_doctor',
      reason: 'test',
    },
  });
  assert.equal((factoryDoctor.structuredContent as Record<string, any>).schema, doctor.schema);
  assert.equal((factoryDoctor.structuredContent as Record<string, any>).fabric_lifecycle.projection_id, 'factory');
  assert.equal((factoryDoctor.structuredContent as Record<string, any>).fabric_lifecycle.execution.adapter, 'surface_factory');
  assert.equal((factoryDoctor.structuredContent as Record<string, any>).fabric_lifecycle.reconnect, 'reuse_authority_shared_site_runtime');
  assert.equal((await runtime.health()).status, 'healthy');
  await runtime.dispose();

  const options = view(await call('launcher_options_list', {}));
  assert.ok((options.declared_options as string[]).includes('IntelligenceProvider'));
  assert.ok((options.declared_options as string[]).includes('OperatorSurface'));
  assert.ok((options.declared_options as string[]).includes('McpScope'));
  assert.ok((options.declared_options as string[]).includes('Profile'));
  assert.ok((options.declared_options as string[]).includes('LauncherUiPort'));
  assert.ok((options.declared_options as string[]).includes('LauncherUiPortFallback'));

  const toolList = await ((handleRequest({ jsonrpc: '2.0', id: 1, method: 'tools/list', params: {} }, state)) as any) as any as Record<string, any>;
  const launcherPlanTool = toolList.result.tools.find((tool: Record<string, any>) => tool.name === 'launcher_plan');
  assert.deepEqual(launcherPlanTool.inputSchema.properties.mcp_scope.enum, ['all', 'host', 'user-site', 'local-site', 'none']);
  assert.deepEqual(launcherPlanTool.inputSchema.properties.authority.enum, ['auto', 'read', 'write']);
  assert.equal(launcherPlanTool.inputSchema.properties.operator_surface.type, 'string');

  const registry = view(await call('launcher_registry_list', { site: ['test'] }));
  assert.equal(registry.total_count, 2);
  assert.equal(registry.records[1].role, 'builder');

  const registryByFullSite = view(await call('launcher_registry_list', { site: ['narada-test'] }));
  assert.equal(registryByFullSite.total_count, 2);

  const registryByProfile = view(await call('launcher_registry_list', { profile: ['implementation'] }));
  assert.equal(registryByProfile.total_count, 1);
  assert.equal(registryByProfile.records[0].agent, 'narada-test.builder2');
  assert.equal(registryByProfile.records[0].operator_surface, 'agent-cli');
  assert.equal(registryByProfile.records[0].runtime, 'narada-agent-runtime-server');

  const plan = view(await call('launcher_plan', { agent: ['narada-test.builder2'], operator_surface: 'agent-cli', authority: 'write', launch_profile: 'implementation-fast', intelligence_provider: 'codex-subscription' }));
  assert.equal(plan.schema, 'narada.workspace_launch.dry_run.v1');
  assert.equal(plan.windows_terminal_invoked, false);
  assert.equal(plan.count, 1);
  assert.deepEqual(plan.wt_args.slice(0, 4), ['new-tab', '--title', 'Test Builder', '-d']);
  assert.ok((plan.wt_args as string[]).includes('--enable-native-shell'));
  assert.deepEqual((plan.wt_args as string[]).slice(5, 7), ['pnpm', '--dir']);
  const naradaCommandIndex = (plan.wt_args as string[]).indexOf('narada');
  assert.ok(naradaCommandIndex > 0);
  assert.equal((plan.wt_args as string[])[naradaCommandIndex - 1], 'exec');
  assert.deepEqual((plan.wt_args as string[]).slice(naradaCommandIndex, naradaCommandIndex + 5), ['narada', 'operator-surface', 'runtime', 'start', 'agent-cli']);
  assert.ok((plan.wt_args as string[]).includes('--target-site-id'));
  assert.ok((plan.wt_args as string[]).includes('--workspace-root'));
  assert.ok((plan.wt_args as string[]).includes('--authority'));
  assert.ok((plan.wt_args as string[]).includes('write'));
  assert.equal((plan.wt_args as string[]).some((arg) => arg.includes('Start-NaradaAgent.ps1')), false);
  assert.equal((plan.wt_args as string[]).includes('-LauncherPath'), false);
  assert.equal((plan.wt_args as string[]).includes('-Profile'), false);
  assert.equal((plan.wt_args as string[]).includes('codex-subscription'), false);
  assert.equal((plan.wt_args as string[]).includes('--wait'), false);
  assert.equal(plan.command_contract, 'narada.operator_surface.runtime_start.v1');
  assert.equal(plan.planner_contract.schema, 'narada.launcher.planner_contract.v1');
  assert.deepEqual(plan.planner_contract.required_fields, [
    'operator_surface',
    'runtime',
    'target_site_id',
    'authority',
    'mcp_scope',
    'wait_policy',
    'execution_posture',
  ]);
  assert.equal(plan.compatibility_diagnostics[0].code, 'intelligence_provider_not_projected');
  assert.deepEqual(plan.mcp_scope_plan.admitted_scopes, ['all', 'host', 'user-site', 'local-site', 'none']);
  assert.equal(plan.mcp_scope_plan.agents[0].requested, 'all');
  assert.deepEqual(plan.mcp_scope_plan.agents[0].requested_loci, ['host', 'user-site', 'local-site']);
  assert.equal(plan.startup_profile_plan.execution_posture, 'planned_not_started_by_mcp');
  assert.deepEqual(plan.startup_profile_plan.profiles, ['implementation-fast']);
  assert.equal(plan.startup_profile_plan.entries[0].registry_profile, 'implementation');
  assert.equal(plan.startup_profile_plan.entries[0].launch_profile, 'implementation-fast');

  const noWait = view(await call('launcher_plan', { all: true, role: ['architect'], no_wait_for_enter_before_exec: true, startup_stagger_seconds: 7 }));
  assert.equal(noWait.count, 1);
  assert.equal((noWait.wt_args as string[]).includes('--wait'), false);
  assert.equal(noWait.startup_profile_plan.entries[0].start_after_seconds, 0);

  const localSitePlan = view(await call('launcher_plan', { agent: ['narada-test.architect'], mcp_scope: 'local-site' }));
  assert.equal(localSitePlan.mcp_scope_plan.agents[0].requested, 'local-site');
  assert.deepEqual(localSitePlan.mcp_scope_plan.agents[0].requested_loci, ['local-site']);
  assert.ok((localSitePlan.wt_args as string[]).includes('--mcp-scope'));
  assert.ok((localSitePlan.wt_args as string[]).includes('local-site'));

  const staggered = view(await call('launcher_plan', { all: true, startup_stagger_seconds: 7 }));
  assert.deepEqual(staggered.startup_profile_plan.entries.map((entry: Record<string, any>) => entry.start_after_seconds), [0, 7]);
  assert.deepEqual(staggered.startup_profile_plan.profiles, ['planning', 'implementation']);

  const matrix = view(await call('launcher_option_matrix', {}));
  assert.equal(matrix.status, 'modeled');
  assert.equal(matrix.representative_agent, 'narada-test.architect');
  assert.equal(matrix.representative_profile, 'planning');
  assert.equal(matrix.case_count, 15);

  const coherence = view(await call('launcher_coherence_check', {}));
  assert.equal(coherence.status, 'valid_with_warnings');
  assert.equal(coherence.errors, 0);
  assert.ok(coherence.warnings >= 1);

  mkdirSync(join(root, '.ai'), { recursive: true });
  writeFileSync(join(root, '.ai', 'mcp-telemetry.json'), JSON.stringify({
    enabled: true,
    level: 'all',
    surfaces: {
      launcher: { enabled: true, level: 'all' },
    },
  }, null, 2), 'utf8');

  const telemetryPlan = view(await call('launcher_plan', { agent: ['narada-test.architect'], operator_surface: 'agent-cli' }));
  const telemetryPath = join(root, '.ai', 'telemetry', 'launcher.jsonl');
  const telemetryLines = readFileSync(telemetryPath, 'utf8').trim().split('\n').filter(Boolean);
  assert.ok(telemetryLines.length >= 1);
  const telemetryEvent = JSON.parse(telemetryLines[telemetryLines.length - 1]);
  assert.equal(telemetryEvent.surface_id, 'launcher');
  assert.equal(telemetryEvent.tool_name, 'launcher_plan');
  assert.equal(JSON.stringify(telemetryEvent).includes('new-tab'), false);
  assert.equal(telemetryPlan.count, 1);

  console.log('launcher-mcp behavior ok');
} finally {
  rmSync(root, { recursive: true, force: true });
}
