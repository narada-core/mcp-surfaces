#!/usr/bin/env node
import { buildGuidanceResult } from './guidance.js';
import { guidanceToolDefinition } from './guidance.js';
import { existsSync, readFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { buildPathMetadataTelemetryDeclaration, emitTelemetryEvent, telemetryErrorCodeFromUnknown, type TelemetryDeclaration, type TelemetryEventKind } from '@narada-core/mcp-telemetry';
import { defineSurface, type DefinedSurface } from '@narada-core/mcp-fabric-contracts';

const SERVER_NAME = 'launcher-mcp';
const SERVER_VERSION = '0.1.0';
const PROTOCOL_VERSION = '2024-11-05';
const DEFAULT_NARADA_ROOT = 'C:/Users/Andrey/Narada';
const DECLARED_OPTIONS = ['Agent', 'All', 'Role', 'Site', 'Profile', 'ConfigPath', 'RegistryPath', 'OperatorSurface', 'Runtime', 'Authority', 'IntelligenceProvider', 'McpScope', 'LauncherUiPort', 'LauncherUiPortFallback', 'EnableNativeShell', 'NoWaitForEnterBeforeExec', 'Smoke', 'DryRun'];
const ADMITTED_MCP_SCOPES = ['all', 'host', 'user-site', 'local-site', 'none'];
const ADMITTED_AUTHORITIES = ['auto', 'read', 'write'];
const SURFACE_ID = 'launcher';
const LAUNCHER_TELEMETRY_TOOL_NAMES = new Set([
  'launcher_doctor',
  'launcher_options_list',
  'launcher_registry_list',
  'launcher_plan',
  'launcher_option_matrix',
  'launcher_coherence_check',
]);

function mcpScopeLoci(scope: string): string[] {
  if (scope === 'none') return [];
  if (scope === 'host') return ['host'];
  if (scope === 'user-site') return ['user-site'];
  if (scope === 'local-site') return ['local-site'];
  return ['host', 'user-site', 'local-site'];
}
const COVERED_OPTIONS = [...DECLARED_OPTIONS];

type JsonRecord = Record<string, unknown>;
type LauncherState = { naradaRoot: string; defaultRegistryPath: string; projectionId: string };
type AgentRecord = {
  agent: string;
  title: string;
  role: string;
  site: string;
  narada_root: string;
  dependency_narada_root: string;
  site_root: string;
  workspace_root: string;
  launcher_path: string;
  operator_surface: string;
  runtime: string;
  authority: string;
  profile: string;
  mcp_scope: string;
  enable_native_shell: boolean;
  config_path: string;
};

export function createServerState(options: JsonRecord = {}): LauncherState {
  const naradaRoot = normalizePath(String(options.naradaRoot ?? options.narada_root ?? process.env.NARADA_ROOT ?? DEFAULT_NARADA_ROOT));
  const defaultRegistryPath = normalizePath(String(options.registryPath ?? options.registry_path ?? join(naradaRoot, 'config', 'launch', 'agents.psd1')));
  const projectionId = String(options.projectionId ?? options.projection_id ?? 'stdio');
  return { naradaRoot, defaultRegistryPath, projectionId };
}

export async function handleRequest(request: JsonRecord, state: LauncherState) {
  if (!request.id && typeof request.method === 'string' && request.method.startsWith('notifications/')) return null;
  try {
    const result = dispatchMethod(String(request.method), asRecord(request.params), state);
    return { jsonrpc: '2.0', id: request.id ?? null, result };
  } catch (error) {
    const diagnostic = errorDiagnostic(error);
    return { jsonrpc: '2.0', id: request.id ?? null, error: { code: -32000, message: diagnostic.message, data: diagnostic } };
  }
}

export async function runStdioServer(options: JsonRecord = {}): Promise<void> {
  const state = createServerState(options);
  let buffer = '';
  let sawFramedInput = false;
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) {
    buffer += chunk;
    const drained = buffer.includes('Content-Length:') ? drainJsonRpcFrames(buffer) : drainJsonLines(buffer);
    sawFramedInput ||= drained.framed;
    buffer = drained.remaining;
    for (const request of drained.requests) {
      const response = await handleRequest(request, state);
      if (response) writeJsonRpcResponse(response, { framed: sawFramedInput });
    }
  }
}

function dispatchMethod(method: string, params: JsonRecord, state: LauncherState) {
  switch (method) {
    case 'initialize': return { protocolVersion: params.protocolVersion ?? PROTOCOL_VERSION, capabilities: { tools: {} }, serverInfo: { name: SERVER_NAME, version: SERVER_VERSION } };
    case 'tools/list': return { tools: listTools() };
    case 'tools/call': return callTool(params, state);
    default: throw diagnosticError('unsupported_mcp_method', `unsupported_mcp_method:${method}`);
  }
}

export function launcherSurfaceDefinition(): DefinedSurface {
  const definitions = launcherToolDefinitions();
  return defineSurface({
    surface_id: SURFACE_ID,
    surface_version: SERVER_VERSION,
    package: '@narada-core/launcher-mcp',
    tools: definitions.map((definition) => ({
      definition,
      effect: { class: 'read', idempotency: 'replayable', confirmation: 'never' },
    })),
    projections: [{
      id: 'stdio',
      transport: {
        kind: 'stdio',
        command: 'node',
        args: ['{mcp_surfaces_root}/launcher-mcp/dist/src/main.js', '--narada-root', '{site_root}'],
        env: [],
      },
      execution: {
        adapter: 'stdio',
        tenancy: 'session_isolated',
        replacement: 'manual',
      },
      injection_scope: 'user_site',
      default_injection: 'disabled',
      runtime_requirements: [],
      authority_requirements: ['scope.user_site'],
      lifecycle: {
        mode: 'replayable',
        reason: 'Launcher tools are read-only plans and registry reads; reconnecting to a fresh stdio process is safe.',
      },
    }, {
      id: 'factory',
      transport: {
        kind: 'stdio',
        command: 'node',
        args: ['{mcp_surfaces_root}/launcher-mcp/dist/src/runtime-factory.js'],
        env: [],
      },
      execution: {
        adapter: 'surface_factory',
        tenancy: 'authority_shared',
        replacement: 'generation_swap',
      },
      injection_scope: 'user_site',
      default_injection: 'disabled',
      runtime_requirements: [],
      authority_requirements: ['scope.user_site'],
      lifecycle: {
        mode: 'replayable',
        reason: 'Launcher tools are read-only and may share one authority-partitioned Site runtime across carrier sessions.',
      },
    }],
  });
}

export function listTools() {
  return launcherSurfaceDefinition().tools;
}

function launcherToolDefinitions() {
  return [
    guidanceToolDefinition(),
    {
      name: 'launcher_doctor',
      description: 'Inspect launcher MCP posture, default root, registry path, and source script presence.',
      inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      annotations: readOnlyTool('launcher_doctor'),
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'launcher_options_list',
      description: 'List the Narada workspace launcher option surface and read-only MCP coverage status.',
      inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      annotations: readOnlyTool('launcher_options_list'),
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'launcher_registry_list',
      description: 'List resolved launcher registry agent records without launching agents.',
      inputSchema: {
        type: 'object',
        properties: {
          registry_path: { type: 'string', description: 'Registry .psd1 path. Defaults to the configured Narada launch registry.' },
          agent: { type: 'array', items: { type: 'string' }, description: 'Optional exact agent id filter.' },
          role: { type: 'array', items: { type: 'string' }, description: 'Optional role filter.' },
          site: { type: 'array', items: { type: 'string' }, description: 'Optional site filter; accepts site id, short site, or agent prefix.' },
          profile: { type: 'array', items: { type: 'string' }, description: 'Optional profile filter.' },
          limit: { type: 'number', default: 100 },
        },
        additionalProperties: false,
      },
      annotations: readOnlyTool('launcher_registry_list'),
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'launcher_plan',
      description: 'Plan a Windows Terminal launch argv from registry selection and overrides without executing it.',
      inputSchema: {
        type: 'object',
        properties: {
          registry_path: { type: 'string' },
          agent: { type: 'array', items: { type: 'string' }, description: 'Exact agent ids to launch.' },
          all: { type: 'boolean', description: 'Select all registry agents before filters.' },
          config_path: { type: 'array', items: { type: 'string' }, description: 'Alternate registry path(s); selecting config_path implies all records in those files.' },
          role: { type: 'array', items: { type: 'string' } },
          site: { type: 'array', items: { type: 'string' } },
          profile: { type: 'array', items: { type: 'string' } },
          operator_surface: { type: 'string', description: 'Operator surface override for selected agents.' },
          runtime: { type: 'string' },
          authority: { type: 'string', enum: ADMITTED_AUTHORITIES, description: 'Runtime authority posture for selected agents.' },
          mcp_scope: { type: 'string', enum: ADMITTED_MCP_SCOPES, description: 'MCP injection scope override for selected agents.' },
          launch_profile: { type: 'string', description: 'Override the profile used by the startup planning projection.' },
          startup_stagger_seconds: { type: 'integer', minimum: 0, maximum: 300, description: 'Plan-only stagger interval between selected profile-aware startup entries.' },
          intelligence_provider: { type: 'string' },
          enable_native_shell: { type: 'boolean' },
          no_wait_for_enter_before_exec: { type: 'boolean' },
        },
        additionalProperties: false,
      },
      annotations: readOnlyTool('launcher_plan'),
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'launcher_option_matrix',
      description: 'Return the modeled launcher option matrix coverage and representative cases without executing PowerShell.',
      inputSchema: { type: 'object', properties: { registry_path: { type: 'string' } }, additionalProperties: false },
      annotations: readOnlyTool('launcher_option_matrix'),
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'launcher_coherence_check',
      description: 'Check launcher registry coherence: duplicate agents, missing roots, launcher files, and option coverage metadata.',
      inputSchema: { type: 'object', properties: { registry_path: { type: 'string' } }, additionalProperties: false },
      annotations: readOnlyTool('launcher_coherence_check'),
      outputSchema: { type: 'object', additionalProperties: true },
    },
  ];
}

function callTool(params: JsonRecord, state: LauncherState) {
  const name = String(params.name ?? '');
  const args = asRecord(params.arguments);
  const startedAt = Date.now();
  try {
    let result: JsonRecord;
    switch (name) {
      case 'launcher_guidance': result = buildGuidanceResult(args); break;
      case 'launcher_doctor': result = launcherDoctor(state); break;
      case 'launcher_options_list': result = launcherOptionsList(); break;
      case 'launcher_registry_list': result = launcherRegistryList(args, state); break;
      case 'launcher_plan': result = launcherPlan(args, state); break;
      case 'launcher_option_matrix': result = launcherOptionMatrix(args, state); break;
      case 'launcher_coherence_check': result = launcherCoherenceCheck(args, state); break;
      default: throw diagnosticError('unknown_tool', `unknown_tool:${name}`, { tool_name: name });
    }
    emitLauncherTelemetry(name, result, state, startedAt);
    return { content: [{ type: 'text', text: renderResult(result) }], structuredContent: result };
  } catch (error) {
    emitLauncherTelemetry(name, {}, state, startedAt, error);
    throw error;
  }
}

function emitLauncherTelemetry(toolName: string, result: JsonRecord, state: LauncherState, startedAt: number, error?: unknown): void {
  if (!LAUNCHER_TELEMETRY_TOOL_NAMES.has(toolName)) return;
  const declaration = launcherTelemetryDeclaration(toolName);
  if (!declaration) return;
  const status = error ? 'error' : String(result.status ?? 'ok');
  const eventKind: TelemetryEventKind = error ? 'tool_failed' : status === 'refused' ? 'tool_refused' : 'tool_completed';
  try {
    emitTelemetryEvent({
      context: {
        siteRoot: state.naradaRoot,
        siteId: process.env.NARADA_SITE_ID ?? null,
        surfaceId: SURFACE_ID,
        agentId: process.env.NARADA_AGENT_ID ?? null,
        carrierSessionId: process.env.NARADA_CARRIER_SESSION_ID ?? null,
      },
      declaration,
      event: {
        toolName,
        eventKind,
        status,
        startedAt,
        completedAt: Date.now(),
        errorCode: error ? telemetryErrorCodeFromUnknown(error) : null,
      },
    });
  } catch (telemetryError) {
    process.stderr.write(`launcher_telemetry_error:${telemetryError instanceof Error ? telemetryError.message : String(telemetryError)}\n`);
  }
}

function launcherTelemetryDeclaration(toolName: string): TelemetryDeclaration | null {
  if (!LAUNCHER_TELEMETRY_TOOL_NAMES.has(toolName)) return null;
  return buildPathMetadataTelemetryDeclaration({ sensitivity: /coherence|registry/.test(toolName) ? 'medium' : 'low' });
}

function launcherDoctor(state: LauncherState): JsonRecord {
  const launcherScript = join(state.naradaRoot, 'Start-NaradaWorkspace.ps1');
  const matrixScript = join(state.naradaRoot, 'tools', 'agent-start', 'Test-LauncherOptionMatrix.ps1');
  const projection = launcherSurfaceDefinition().descriptor.projections.find((candidate) => candidate.id === state.projectionId)
    ?? launcherSurfaceDefinition().descriptor.projections[0];
  return {
    schema: 'narada.launcher.doctor.v1',
    status: existsSync(state.defaultRegistryPath) && existsSync(launcherScript) ? 'ok' : 'degraded',
    server_name: SERVER_NAME,
    server_version: SERVER_VERSION,
    protocol_version: PROTOCOL_VERSION,
    narada_root: state.naradaRoot,
    registry_path: state.defaultRegistryPath,
    registry_exists: existsSync(state.defaultRegistryPath),
    start_workspace_script: launcherScript,
    start_workspace_script_exists: existsSync(launcherScript),
    option_matrix_script: matrixScript,
    option_matrix_script_exists: existsSync(matrixScript),
    execution_posture: 'read_only_no_launch_no_shell',
    fabric_lifecycle: {
      projection_id: projection?.id ?? state.projectionId,
      execution: projection?.execution,
      ...projection?.lifecycle,
      reconnect: projection?.execution?.adapter === 'surface_factory'
        ? 'reuse_authority_shared_site_runtime'
        : 'start_fresh_stdio_process',
      restart_actuator: null,
    },
    mcp_injection_scope_doctrine: {
      scopes: ['host', 'user_site', 'local_site'],
      ownership_rule: 'Session visibility is not ownership; launcher diagnostics must preserve the surface authority locus and restart owner.',
      canonical_host_example: 'speech',
      scope_source: 'mcp-registrar',
    },
  };
}

function launcherOptionsList(): JsonRecord {
  return {
    schema: 'narada.launcher.options.v1',
    status: 'ok',
    declared_options: DECLARED_OPTIONS,
    covered_options: COVERED_OPTIONS,
    options: DECLARED_OPTIONS.map((name) => ({ name, kind: optionKind(name), covered_by_mcp: true, mutates_processes: false })),
    tools: ['launcher_doctor', 'launcher_options_list', 'launcher_registry_list', 'launcher_plan', 'launcher_option_matrix', 'launcher_coherence_check'],
  };
}

function launcherRegistryList(args: JsonRecord, state: LauncherState): JsonRecord {
  const records = selectRecords(args, state, { requireSelection: false }).records;
  const limit = clampNumber(args.limit, 100, 1, 1000);
  return {
    schema: 'narada.launcher.registry.v1',
    status: 'ok',
    count: Math.min(records.length, limit),
    total_count: records.length,
    records: records.slice(0, limit),
  };
}

function launcherPlan(args: JsonRecord, state: LauncherState): JsonRecord {
  const selected = selectRecords(args, state, { requireSelection: true });
  const wtArgs: string[] = [];
  const mcpScopePlan = [];
  const staggerSeconds = clampNumber(args.startup_stagger_seconds, 0, 0, 300);
  const provider = optionalString(args.intelligence_provider);
  const compatibilityDiagnostics: JsonRecord[] = [];
  if (provider) {
    compatibilityDiagnostics.push({
      code: 'intelligence_provider_not_projected',
      message: 'Intelligence provider selection is resolved by current runtime authority and is not an operator-surface runtime-start argument.',
      requested: provider,
    });
  }
  for (const record of selected.records) {
    const launchRuntime = optionalString(args.runtime) ?? record.runtime;
    const launchProfile = optionalString(args.launch_profile) ?? record.profile;
    const operatorSurface = optionalString(args.operator_surface) ?? record.operator_surface;
    const authority = optionalString(args.authority) ?? record.authority;
    if (!ADMITTED_AUTHORITIES.includes(authority)) throw new Error(`authority_not_admitted: ${authority}. Admitted authorities: ${ADMITTED_AUTHORITIES.join(', ')}`);
    const mcpScope = optionalString(args.mcp_scope) || record.mcp_scope;
    if (mcpScope && !ADMITTED_MCP_SCOPES.includes(mcpScope)) throw new Error(`mcp_scope_not_admitted: ${mcpScope}. Admitted scopes: ${ADMITTED_MCP_SCOPES.join(', ')}`);
    mcpScopePlan.push({ agent: record.agent, requested: mcpScope, requested_loci: mcpScopeLoci(mcpScope), registry_default: record.mcp_scope });
    if (wtArgs.length > 0) wtArgs.push(';');
    wtArgs.push(
      'new-tab', '--title', record.title, '-d', record.workspace_root || record.narada_root,
      'pnpm', '--dir', record.dependency_narada_root, 'exec',
      'narada', 'operator-surface', 'runtime', 'start', operatorSurface,
      '--site-root', record.site_root,
      '--agent', record.agent,
      '--target-site-id', record.site,
      '--runtime', launchRuntime,
      '--exec', '--format', 'human',
    );
    if (record.workspace_root) wtArgs.push('--workspace-root', record.workspace_root);
    if (args.enable_native_shell === true || record.enable_native_shell) wtArgs.push('--enable-native-shell');
    wtArgs.push('--authority', authority);
    if (mcpScope) wtArgs.push('--mcp-scope', mcpScope);
    if (args.no_wait_for_enter_before_exec !== true && launchRuntime.toLowerCase() !== 'narada-agent-runtime-server') wtArgs.push('--wait');
  }
  return {
    schema: 'narada.workspace_launch.dry_run.v1',
    status: 'planned',
    count: selected.records.length,
    windows_terminal_invoked: false,
    registry_paths: selected.registryPaths,
    wt_args: wtArgs,
    command_contract: 'narada.operator_surface.runtime_start.v1',
    planner_contract: {
      schema: 'narada.launcher.planner_contract.v1',
      version: 1,
      canonical_contract: 'narada.operator_surface.runtime_start.v1',
      projection: 'mcp_launcher_plan',
      required_fields: [
        'operator_surface',
        'runtime',
        'target_site_id',
        'authority',
        'mcp_scope',
        'wait_policy',
        'execution_posture',
      ],
      legacy_projection: 'not_emitted',
    },
    compatibility_diagnostics: compatibilityDiagnostics,
    mcp_scope_plan: {
      admitted_scopes: ADMITTED_MCP_SCOPES,
      agents: mcpScopePlan,
    },
    records: selected.records,
    startup_profile_plan: startupProfilePlan(selected.records, optionalString(args.launch_profile), staggerSeconds),
  };
}

function startupProfilePlan(selectedRecords: AgentRecord[], launchProfile: string | null | undefined, staggerSeconds: number): JsonRecord {
  const entries = selectedRecords.map((record, index) => ({
    agent: record.agent,
    site: record.site,
    role: record.role,
    registry_profile: record.profile || null,
    launch_profile: launchProfile ?? (record.profile || null),
    start_after_seconds: index * staggerSeconds,
    diagnostics: record.profile ? [] : ['registry_profile_missing'],
  }));
  const profiles = uniqueStrings(entries.map((entry) => String(entry.launch_profile ?? '')).filter(Boolean));
  const diagnostics = entries.flatMap((entry) => (entry.diagnostics as string[]).map((diagnostic) => ({ agent: entry.agent, diagnostic })));
  return {
    schema: 'narada.launcher.profile_startup_plan.v1',
    execution_posture: 'planned_not_started_by_mcp',
    selected_count: selectedRecords.length,
    stagger_seconds: staggerSeconds,
    profile_count: profiles.length,
    profiles,
    entries,
    diagnostics,
  };
}

function launcherOptionMatrix(args: JsonRecord, state: LauncherState): JsonRecord {
  const registryPath = resolveRegistryPath(args, state);
  const records = loadRegistryRecords(registryPath);
  const representative = chooseRepresentative(records);
  const site = representative?.site ?? '';
  const role = representative?.role ?? '';
  const agent = representative?.agent ?? '';
  const runtime = representative?.runtime ?? 'codex';
  const profile = representative?.profile ?? '';
  const caseNames = ['selection_required', 'unknown_agent', 'missing_role_filter', 'missing_site_filter', 'missing_profile_filter', 'agent_exact_dry_run', 'all_site_role_dry_run', 'profile_aware_startup', 'config_path_selects_without_all', 'runtime_override', 'native_shell_flag', 'intelligence_provider_flag', 'no_wait_flag', 'smoke_agent_cli_contract', 'smoke_site_role_filter_contract'];
  return {
    schema: 'narada.launcher.option_matrix_model.v1',
    status: 'modeled',
    registry_path: registryPath,
    execution_posture: 'not_executed_by_mcp',
    representative_agent: agent,
    representative_site: site,
    representative_role: role,
    representative_runtime: runtime,
    representative_profile: profile,
    declared_options: DECLARED_OPTIONS,
    covered_options: COVERED_OPTIONS,
    case_count: caseNames.length,
    cases: caseNames.map((name) => ({ case: name, modeled: true })),
  };
}

function launcherCoherenceCheck(args: JsonRecord, state: LauncherState): JsonRecord {
  const registryPath = resolveRegistryPath(args, state);
  const findings: JsonRecord[] = [];
  if (!existsSync(registryPath)) findings.push(finding('error', 'launcher_registry_missing', `Registry path does not exist: ${registryPath}`, registryPath));
  const records = existsSync(registryPath) ? loadRegistryRecords(registryPath) : [];
  const seen = new Map<string, AgentRecord>();
  for (const record of records) {
    const prior = seen.get(record.agent);
    if (prior) findings.push(finding('error', 'launcher_duplicate_agent', `Duplicate agent in launch registry: ${record.agent}`, registryPath, { agent: record.agent, first_config_path: prior.config_path }));
    seen.set(record.agent, record);
    for (const [field, path] of Object.entries({ narada_root: record.narada_root, site_root: record.site_root, workspace_root: record.workspace_root })) {
      if (path && !existsSync(path)) findings.push(finding('warning', `launcher_${field}_missing`, `${field} does not exist for ${record.agent}: ${path}`, path, { agent: record.agent }));
    }
    if (record.launcher_path && !existsSync(record.launcher_path)) findings.push(finding('warning', 'launcher_script_missing', `Launcher path does not exist for ${record.agent}: ${record.launcher_path}`, record.launcher_path, { agent: record.agent }));
  }
  const missingCoverage = DECLARED_OPTIONS.filter((option) => !COVERED_OPTIONS.includes(option));
  for (const option of missingCoverage) findings.push(finding('error', 'launcher_option_coverage_missing', `Declared launcher option is not covered: -${option}`, registryPath, { option }));
  const errors = findings.filter((item) => item.severity === 'error').length;
  const warnings = findings.filter((item) => item.severity === 'warning').length;
  return { schema: 'narada.launcher.coherence.v1', status: errors > 0 ? 'invalid' : warnings > 0 ? 'valid_with_warnings' : 'valid', registry_path: registryPath, agent_count: records.length, errors, warnings, findings };
}

function selectRecords(args: JsonRecord, state: LauncherState, options: { requireSelection: boolean }): { records: AgentRecord[]; registryPaths: string[]; naradaRoot: string } {
  const configPaths = stringArray(args.config_path);
  const registryPaths = configPaths.length > 0 ? configPaths.map(normalizePath) : [resolveRegistryPath(args, state)];
  const allRecords = registryPaths.flatMap((path) => loadRegistryRecords(path));
  let selected: AgentRecord[];
  const agents = stringArray(args.agent);
  if (agents.length > 0) {
    selected = [];
    for (const agent of agents) {
      const matches = allRecords.filter((record) => record.agent === agent);
      if (matches.length === 0) throw diagnosticError('agent_not_found_in_launch_registry', `agent_not_found_in_launch_registry:${agent}`);
      if (matches.length > 1) throw diagnosticError('agent_duplicate_in_launch_registry', `agent_duplicate_in_launch_registry:${agent}`);
      selected.push(matches[0]);
    }
  } else if (args.all === true || configPaths.length > 0 || !options.requireSelection) {
    selected = allRecords;
  } else {
    throw diagnosticError('launch_selection_required', 'launch_selection_required: specify agent, all, or config_path');
  }
  const roles = lowerSet(stringArray(args.role));
  if (roles.size > 0) {
    selected = selected.filter((record) => roles.has(record.role.toLowerCase()));
    if (selected.length === 0) throw diagnosticError('no_agents_match_role_filter', `no_agents_match_role_filter:${stringArray(args.role).join(', ')}`);
  }
  const sites = lowerSet(stringArray(args.site));
  if (sites.size > 0) {
    selected = selected.filter((record) => siteAliases(record).some((alias) => sites.has(alias.toLowerCase())));
    if (selected.length === 0) throw diagnosticError('no_agents_match_site_filter', `no_agents_match_site_filter:${stringArray(args.site).join(', ')}`);
  }
  const profiles = lowerSet(stringArray(args.profile));
  if (profiles.size > 0) {
    selected = selected.filter((record) => record.profile && profiles.has(record.profile.toLowerCase()));
    if (selected.length === 0) throw diagnosticError('no_agents_match_profile_filter', `no_agents_match_profile_filter:${stringArray(args.profile).join(', ')}`);
  }
  return { records: selected, registryPaths, naradaRoot: state.naradaRoot };
}

function loadRegistryRecords(path: string): AgentRecord[] {
  if (!existsSync(path)) throw diagnosticError('launch_registry_missing', `launch_registry_missing:${path}`, { path });
  const parsed = parsePowerShellDataFile(readFileSync(path, 'utf8'));
  const root = asRecord(parsed);
  const agents = Array.isArray(root.Agents) ? root.Agents.map(asRecord) : [];
  return agents.map((agent) => toAgentRecord(agent, root, path));
}

function toAgentRecord(agent: JsonRecord, config: JsonRecord, configPath: string): AgentRecord {
  const agentId = requiredText(agent.Agent, `agent_missing:${configPath}`);
  const naradaRoot = requiredText(agent.NaradaRoot ?? config.NaradaRoot, `agent_narada_root_missing:${agentId}`);
  const dependencyNaradaRoot = text(agent.DependencyNaradaRoot ?? agent.NaradaProperRoot ?? config.DependencyNaradaRoot ?? config.NaradaProperRoot)
    || resolveDependencyNaradaRoot(naradaRoot);
  const siteRoot = text(agent.SiteRoot ?? config.SiteRoot) || naradaRoot;
  const workspaceRoot = text(agent.WorkspaceRoot ?? config.WorkspaceRoot);
  const launcher = text(agent.Launcher ?? config.Launcher);
  const launcherPath = text(agent.LauncherPath ?? config.LauncherPath) || (launcher ? join(naradaRoot, launcher) : '');
  if (!launcherPath) throw diagnosticError('launcher_path_missing', `launcher_path_missing:${agentId}`);
  const operatorSurface = text(agent.OperatorSurface ?? agent.Carrier ?? config.OperatorSurface ?? config.Carrier) || 'codex';
  const runtime = text(agent.Runtime ?? config.Runtime) || 'narada-agent-runtime-server';
  const authority = text(agent.Authority ?? config.Authority) || 'auto';
  if (!ADMITTED_AUTHORITIES.includes(authority)) throw diagnosticError('authority_not_admitted', `authority_not_admitted:${authority}`, { admitted_authorities: ADMITTED_AUTHORITIES });
  const profile = text(agent.Profile ?? config.Profile);
  const mcpScope = text(agent.McpScope ?? config.McpScope) || 'all';
  if (!ADMITTED_MCP_SCOPES.includes(mcpScope)) throw diagnosticError('mcp_scope_not_admitted', `mcp_scope_not_admitted:${mcpScope}`, { admitted_scopes: ADMITTED_MCP_SCOPES });
  return {
    agent: agentId,
    title: text(agent.Title) || agentId.split('.').at(-1) || agentId,
    role: text(agent.Role) || ((agentId.split('.').at(-1) || agentId).replace(/\d+$/, '')),
    site: text(agent.Site) || (agentId.includes('.') ? agentId.split('.')[0] : agentId),
    narada_root: normalizePath(naradaRoot),
    dependency_narada_root: normalizePath(dependencyNaradaRoot),
    site_root: normalizePath(siteRoot),
    workspace_root: workspaceRoot ? normalizePath(workspaceRoot) : '',
    launcher_path: normalizePath(launcherPath),
    operator_surface: operatorSurface,
    runtime,
    authority,
    profile,
    mcp_scope: mcpScope,
    enable_native_shell: Boolean(agent.EnableNativeShell),
    config_path: normalizePath(configPath),
  };
}

function parsePowerShellDataFile(source: string): unknown {
  const tokens = tokenize(source);
  let i = 0;
  const peek = () => tokens[i];
  const take = (expected?: string) => {
    const token = tokens[i++];
    if (expected && token !== expected) throw diagnosticError('psd1_parse_error', `psd1_parse_error: expected ${expected} got ${token ?? 'eof'}`);
    return token;
  };
  const parseValue = (): unknown => {
    const token = peek();
    if (token === '@') {
      take('@');
      if (peek() === '{') return parseHashtable();
      if (peek() === '(') return parseArray();
      throw diagnosticError('psd1_parse_error', 'psd1_parse_error: expected hashtable or array after @');
    }
    if (token === '$true' || token === '$false') return take() === '$true';
    if (token === '(') return parseArray();
    if (token === undefined) throw diagnosticError('psd1_parse_error', 'psd1_parse_error: unexpected eof');
    return take();
  };
  const parseHashtable = (): JsonRecord => {
    const output: JsonRecord = {};
    take('{');
    while (peek() && peek() !== '}') {
      const key = take();
      take('=');
      output[String(key)] = parseValue();
      if (peek() === ';' || peek() === ',') take();
    }
    take('}');
    return output;
  };
  const parseArray = (): unknown[] => {
    const output: unknown[] = [];
    take('(');
    while (peek() && peek() !== ')') {
      output.push(parseValue());
      if (peek() === ';' || peek() === ',') take();
    }
    take(')');
    return output;
  };
  return parseValue();
}

function tokenize(source: string): string[] {
  const tokens: string[] = [];
  let i = 0;
  while (i < source.length) {
    const ch = source[i];
    if (/\s/.test(ch)) { i++; continue; }
    if (ch === '#') { while (i < source.length && source[i] !== '\n') i++; continue; }
    if ('@{}()=;,'.includes(ch)) { tokens.push(ch); i++; continue; }
    if (ch === '"' || ch === "'") {
      const quote = ch;
      i++;
      let value = '';
      while (i < source.length) {
        const current = source[i++];
        if (current === quote) {
          if (quote === "'" && source[i] === "'") { value += "'"; i++; continue; }
          break;
        }
        value += current;
      }
      tokens.push(value);
      continue;
    }
    let value = '';
    while (i < source.length && !/\s/.test(source[i]) && !'@{}()=;,'.includes(source[i]) && source[i] !== '#') value += source[i++];
    tokens.push(value);
  }
  return tokens;
}

function resolveRegistryPath(args: JsonRecord, state: LauncherState): string {
  return normalizePath(optionalString(args.registry_path) ?? state.defaultRegistryPath);
}

function chooseRepresentative(records: AgentRecord[]): AgentRecord | undefined {
  return records.find((record) => record.agent === 'smart-scheduling.resident') ?? records.find((record) => record.runtime === 'agent-cli') ?? records[0];
}

function optionKind(name: string): string {
  return ['All', 'LauncherUiPortFallback', 'EnableNativeShell', 'NoWaitForEnterBeforeExec', 'Smoke', 'DryRun'].includes(name) ? 'switch' : 'value';
}

function siteAliases(record: AgentRecord): string[] {
  const agentPrefix = record.agent.split('.')[0];
  return [record.site, record.site.replace(/^narada-/, ''), record.site.startsWith('narada-') ? '' : `narada-${record.site}`, agentPrefix, agentPrefix.startsWith('narada-') ? '' : `narada-${agentPrefix}`].filter(Boolean);
}

function lowerSet(values: string[]): Set<string> { return new Set(values.map((value) => value.toLowerCase())); }
function uniqueStrings(values: string[]): string[] { return Array.from(new Set(values)); }
function normalizePath(value: string): string { return value ? resolve(value).replace(/\\/g, '/') : value; }

function resolveDependencyNaradaRoot(naradaRoot: string): string {
  const targetRoot = resolve(naradaRoot);
  const parent = dirname(targetRoot);
  const sibling = join(parent, 'narada');
  const userSource = join(parent, 'src', 'narada');
  const candidates = [targetRoot, sibling, userSource];
  const existing = candidates.find((candidate) => (
    existsSync(join(candidate, 'package.json'))
    && existsSync(join(candidate, 'packages', 'layers', 'cli', 'package.json'))
  ));
  if (existing) return existing;
  if (basename(targetRoot).toLowerCase() === 'narada') return targetRoot;
  if (['src', 'code'].includes(basename(parent).toLowerCase())) return sibling;
  return userSource;
}
function optionalString(value: unknown): string | undefined { const v = text(value); return v || undefined; }
function text(value: unknown): string { return typeof value === 'string' ? value : ''; }
function requiredText(value: unknown, code: string): string { const v = text(value); if (!v) throw diagnosticError(code.split(':')[0], code); return v; }
function stringArray(value: unknown): string[] { if (Array.isArray(value)) return value.map((item) => String(item)).filter(Boolean); if (typeof value === 'string' && value) return [value]; return []; }
function clampNumber(value: unknown, fallback: number, min: number, max: number): number { const n = typeof value === 'number' && Number.isFinite(value) ? Math.trunc(value) : fallback; return Math.max(min, Math.min(max, n)); }
function asRecord(value: unknown): JsonRecord { return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonRecord : {}; }
function readOnlyTool(title: string) { return { title, readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false }; }
function finding(severity: string, code: string, message: string, path: string, extra: JsonRecord = {}): JsonRecord { return { severity, code, message, path, ...extra }; }

function renderResult(result: JsonRecord): string { return JSON.stringify(result, null, 2); }
function diagnosticError(code: string, message: string, data: JsonRecord = {}): Error & { diagnostic?: JsonRecord } { const error = new Error(message) as Error & { diagnostic?: JsonRecord }; error.diagnostic = { code, message, ...data }; return error; }
function errorDiagnostic(error: unknown): JsonRecord { if (error && typeof error === 'object' && 'diagnostic' in error) return (error as { diagnostic: JsonRecord }).diagnostic; return { code: 'launcher_mcp_error', message: error instanceof Error ? error.message : String(error) }; }

function drainJsonLines(buffer: string): { requests: JsonRecord[]; remaining: string; framed: boolean } {
  const lines = buffer.split(/\r?\n/);
  const remaining = lines.pop() ?? '';
  return { requests: lines.filter((line) => line.trim()).map((line) => JSON.parse(line) as JsonRecord), remaining, framed: false };
}
function drainJsonRpcFrames(buffer: string): { requests: JsonRecord[]; remaining: string; framed: boolean } {
  const requests: JsonRecord[] = [];
  let rest = buffer;
  while (true) {
    const headerEnd = rest.indexOf('\r\n\r\n');
    const altHeaderEnd = rest.indexOf('\n\n');
    const end = headerEnd >= 0 ? headerEnd : altHeaderEnd;
    const sep = headerEnd >= 0 ? 4 : 2;
    if (end < 0) break;
    const header = rest.slice(0, end);
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) break;
    const length = Number(match[1]);
    const start = end + sep;
    if (rest.length < start + length) break;
    requests.push(JSON.parse(rest.slice(start, start + length)) as JsonRecord);
    rest = rest.slice(start + length);
  }
  return { requests, remaining: rest, framed: true };
}
function writeJsonRpcResponse(response: JsonRecord, options: { framed: boolean }): void {
  const json = JSON.stringify(response);
  if (options.framed) process.stdout.write(`Content-Length: ${Buffer.byteLength(json, 'utf8')}\r\n\r\n${json}`);
  else process.stdout.write(`${json}\n`);
}

function parseCliArgs(argv: string[]): JsonRecord {
  const options: JsonRecord = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--narada-root' && argv[i + 1]) options.naradaRoot = argv[++i];
    else if (arg === '--registry-path' && argv[i + 1]) options.registryPath = argv[++i];
    else if (arg === '--projection-id' && argv[i + 1]) options.projectionId = argv[++i];
  }
  return options;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  runStdioServer(parseCliArgs(process.argv.slice(2))).catch((error) => {
    console.error(errorDiagnostic(error));
    process.exit(1);
  });
}
