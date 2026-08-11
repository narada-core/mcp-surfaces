#!/usr/bin/env node
import { buildGuidanceResult } from './guidance.js';
import { guidanceToolDefinition } from './guidance.js';
import { existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const SERVER_NAME = 'site-lifecycle-mcp';
const SERVER_VERSION = '0.1.0';
const PROTOCOL_VERSION = '2024-11-05';

type JsonRecord = Record<string, unknown>;
type ServerState = { naradaRoot: string; cliModulePath: string; operatorSurfaceModulePath: string };
type SiteCommandSpec = {
  tool: string;
  cli: string;
  functionName: string;
  module?: 'sites' | 'operator-surface';
  readOnly: boolean;
  requiresExecute?: boolean;
  requiresAuthority?: boolean;
  description: string;
  properties: JsonRecord;
  required?: string[];
};

const COMMANDS: SiteCommandSpec[] = [
  {
    tool: 'site_create_presets_list',
    cli: 'narada sites create-presets',
    functionName: 'sitesCreatePresetsCommand',
    readOnly: true,
    description: 'List greenfield create-site presets from the Narada CLI template catalog.',
    properties: {},
  },
  {
    tool: 'site_create_plan',
    cli: 'narada sites create --dry-run',
    functionName: 'sitesCreateCommand',
    readOnly: true,
    description: 'Plan greenfield Narada Site creation using the same semantics as narada sites create --dry-run.',
    properties: createSiteProperties(),
  },
  {
    tool: 'site_list',
    cli: 'narada sites list',
    functionName: 'sitesListCommand',
    readOnly: true,
    description: 'List discovered Narada Sites using narada sites list semantics.',
    properties: {},
  },
  {
    tool: 'site_discover',
    cli: 'narada sites discover',
    functionName: 'sitesDiscoverCommand',
    readOnly: false,
    requiresExecute: true,
    description: 'Refresh Narada site discovery registry using narada sites discover semantics.',
    properties: mutationProperties(),
  },
  {
    tool: 'site_show',
    cli: 'narada sites show <site-id>',
    functionName: 'sitesShowCommand',
    readOnly: true,
    description: 'Show Site metadata and last-known health using narada sites show semantics.',
    properties: { site_id: stringSchema('Site id to show.') },
    required: ['site_id'],
  },
  {
    tool: 'site_admit_role',
    cli: 'narada operator-surface agent instantiate',
    functionName: 'operatorSurfaceAgentInstantiateCommand',
    module: 'operator-surface',
    readOnly: false,
    requiresExecute: true,
    requiresAuthority: true,
    description: 'Idempotently admit or reuse a durable project-role identity at the explicitly supplied Site authority root.',
    properties: {
      ...mutationProperties(),
      site_id: stringSchema('Canonical Site id; filesystem paths are not accepted as identity ids.'),
      site_root: stringSchema('Explicit project workspace root whose .narada control root owns the identity.'),
      role: stringSchema('Role to admit, e.g. architect, builder, observer.'),
      agent_kind: stringSchema('Agent kind, e.g. codex_cli or kimi_cli.'),
      identity: stringSchema('Optional durable identity id. Defaults to <site-id>.<role>.'),
      by: stringSchema('Principal admitting the identity.'),
      input_capabilities: stringSchema('Optional comma-separated declared input capabilities.'),
      submit_strategy: stringSchema('Optional declared submit strategy.'),
      bind_focused: booleanSchema('Request runtime binding; the result remains deferred unless the owning runtime locus is supplied.'),
      runtime_locus: stringSchema('Optional owning runtime locus for binding handoff.'),
    },
    required: ['site_id', 'site_root', 'role', 'agent_kind', 'by', 'authority_basis'],
  },
  {
    tool: 'site_verify_role',
    cli: 'narada operator-surface doctor',
    functionName: 'operatorSurfaceDoctorCommand',
    module: 'operator-surface',
    readOnly: true,
    description: 'Verify durable project-role admission separately from runtime binding and visible-label projections.',
    properties: {
      site_id: stringSchema('Canonical Site id to inspect.'),
      site_root: stringSchema('Explicit project workspace root whose .narada control root owns the identity.'),
      runtime_locus: stringSchema('Optional runtime locus filter.'),
      limit: numberSchema('Maximum identities to inspect.'),
    },
    required: ['site_id', 'site_root'],
  },
  {
    tool: 'site_observe_runtime',
    cli: 'narada operator-surface status',
    functionName: 'operatorSurfaceStatusCommand',
    module: 'operator-surface',
    readOnly: true,
    description: 'Observe current admitted identities, runtime-locus bindings, handles, and activity projections at an explicit Site authority root.',
    properties: {
      site_id: stringSchema('Canonical Site id to inspect.'),
      site_root: stringSchema('Explicit project workspace root whose .narada control root owns the projection.'),
      limit: numberSchema('Maximum identities to inspect.'),
    },
    required: ['site_id', 'site_root'],
  },
  {
    tool: 'site_bind_runtime',
    cli: 'narada operator-surface bind-focused',
    functionName: 'operatorSurfaceBindFocusedCommand',
    module: 'operator-surface',
    readOnly: false,
    requiresExecute: true,
    requiresAuthority: true,
    description: 'Bind an admitted durable identity to an observed runtime locus and handle; ambient foreground focus is refused.',
    properties: {
      ...mutationProperties(),
      site_root: stringSchema('Explicit project workspace root whose .narada control root owns the identity.'),
      identity: stringSchema('Admitted durable identity id.'),
      runtime_locus: stringSchema('Owning runtime locus.'),
      handle: stringSchema('Observed stable runtime handle or HWND.'),
      observed_handle: stringSchema('Optional independently observed handle for postcondition evidence.'),
      stale_after: stringSchema('Optional expiry timestamp for the volatile binding.'),
    },
    required: ['site_root', 'identity', 'runtime_locus', 'handle', 'authority_basis'],
  },
  {
    tool: 'site_doctor',
    cli: 'narada sites doctor <site-id>',
    functionName: 'sitesDoctorCommand',
    readOnly: true,
    description: 'Validate a Site root and authority posture using narada sites doctor semantics.',
    properties: {
      site_id: stringSchema('Site id to inspect.'),
      root: stringSchema('Optional site workspace/root path to inspect.'),
      authority_locus: stringSchema('Optional authority locus.'),
      kind: stringSchema('Site kind, e.g. windows, client, project.'),
      role: stringSchema('Optional role whose durable identity and runtime binding are required.'),
      role_required: booleanSchema('Require the role even when the Site has not declared one. Project doctor defaults to true.'),
    },
    required: ['site_id'],
  },
  {
    tool: 'site_init',
    cli: 'narada sites init <site-id>',
    functionName: 'sitesInitCommand',
    readOnly: false,
    requiresExecute: true,
    requiresAuthority: true,
    description: 'Initialize a new Narada Site using narada sites init semantics.',
    properties: {
      ...mutationProperties(),
      site_id: stringSchema('Site id to initialize.'),
      substrate: stringSchema('Substrate: windows-native, windows-wsl, macos, linux-user, linux-system.'),
      operation: stringSchema('Optional operation id to bind.'),
      root: stringSchema('Optional site root override.'),
      authority_locus: stringSchema('Optional authority locus.'),
      sync: stringSchema('Optional sync posture.'),
      execution_surface: stringSchema('Optional execution surface.'),
      dry_run: booleanSchema('Preview without making changes. Defaults true unless execute is true.'),
    },
    required: ['site_id', 'substrate', 'authority_basis'],
  },
  {
    tool: 'site_lifecycle_kinds',
    cli: 'narada sites lifecycle kinds',
    functionName: 'sitesLifecycleKindsCommand',
    readOnly: true,
    description: 'List governed Site lifecycle transformation kinds.',
    properties: {},
  },
  {
    tool: 'site_lifecycle_preflight',
    cli: 'narada sites lifecycle preflight <kind>',
    functionName: 'sitesLifecyclePreflightCommand',
    readOnly: true,
    description: 'Preflight a governed Site lifecycle transformation without mutation.',
    properties: {
      kind: stringSchema('Lifecycle transformation kind.'),
      source_site: stringSchema('Optional source Site id or path.'),
      target_site: stringSchema('Optional target Site id or path.'),
      authority_mode: stringSchema('Optional authority mode.'),
    },
    required: ['kind'],
  },
  {
    tool: 'site_relation_list',
    cli: 'narada sites relation list',
    functionName: 'sitesRelationListCommand',
    readOnly: true,
    description: 'List durable Site relation records.',
    properties: {
      kind: stringSchema('Optional relation kind filter.'),
      source_site: stringSchema('Optional source Site filter.'),
      target_site: stringSchema('Optional target Site filter.'),
      status: stringSchema('Optional relation status filter.'),
      limit: numberSchema('Maximum relations.'),
      cwd: stringSchema('Working directory. Defaults to current Narada root.'),
    },
  },
  {
    tool: 'site_relation_validate',
    cli: 'narada sites relation validate',
    functionName: 'sitesRelationValidateCommand',
    readOnly: true,
    description: 'Validate reciprocal and authority posture of Site relation records.',
    properties: { cwd: stringSchema('Working directory. Defaults to current Narada root.') },
  },
  {
    tool: 'site_authority_preflight',
    cli: 'narada sites authority preflight',
    functionName: 'siteMutationAuthorityPreflightCommand',
    readOnly: true,
    description: 'Preflight whether a site mutation would occur at the declared authority locus.',
    properties: {
      cwd: stringSchema('Working directory to inspect.'),
      mutation_family: stringSchema('Mutation family: task_lifecycle, inbox, publication, secret, or site.'),
    },
  },
  {
    tool: 'site_deps_sync',
    cli: 'narada sites deps sync',
    functionName: 'sitesDepsSyncCommand',
    readOnly: false,
    requiresExecute: true,
    requiresAuthority: true,
    description: 'Synchronize shared Narada package links and provenance for a Site.',
    properties: {
      ...mutationProperties(),
      root: stringSchema('Site root or containing workspace root.'),
      apply: booleanSchema('Create or repair package links and provenance.'),
    },
    required: ['authority_basis'],
  },
];

export function createServerState(options: JsonRecord = {}, env: NodeJS.ProcessEnv = process.env): ServerState {
  const sourceRoot = env.NARADA_SRC_ROOT?.trim() || join(homedir(), 'src');
  const naradaRoot = normalizePath(String(
    options.naradaRoot
      ?? options.narada_root
      ?? env.NARADA_ROOT
      ?? env.NARADA_PROPER_ROOT
      ?? join(sourceRoot, 'narada'),
  ));
  const cliModulePath = normalizePath(String(options.cliModulePath ?? options.cli_module_path ?? join(naradaRoot, 'packages', 'layers', 'cli', 'dist', 'commands', 'sites.js')));
  const operatorSurfaceModulePath = normalizePath(String(options.operatorSurfaceModulePath ?? options.operator_surface_module_path ?? join(naradaRoot, 'packages', 'layers', 'cli', 'dist', 'commands', 'operator-surface.js')));
  return { naradaRoot, cliModulePath, operatorSurfaceModulePath };
}

export async function handleRequest(request: JsonRecord, state: ServerState) {
  if (!request.id && typeof request.method === 'string' && request.method.startsWith('notifications/')) return null;
  try {
    const result = await dispatchMethod(String(request.method), asRecord(request.params), state);
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

async function dispatchMethod(method: string, params: JsonRecord, state: ServerState) {
  switch (method) {
    case 'initialize': return { protocolVersion: params.protocolVersion ?? PROTOCOL_VERSION, capabilities: { tools: {} }, serverInfo: { name: SERVER_NAME, version: SERVER_VERSION } };
    case 'tools/list': return { tools: listTools() };
    case 'tools/call': return callTool(params, state);
    default: throw diagnosticError('unsupported_mcp_method', `unsupported_mcp_method:${method}`);
  }
}

export function listTools() {
  return [
    guidanceToolDefinition(),
    tool('site_lifecycle_doctor', 'Inspect site lifecycle MCP posture, Narada root, CLI module availability, and command coverage.', {}, [], true),
    tool('site_lifecycle_command_map', 'List MCP tools and their aligned narada sites CLI commands.', {}, [], true),
    ...COMMANDS.map((spec) => tool(spec.tool, spec.description, spec.properties, spec.required ?? [], spec.readOnly)),
  ];
}

async function callTool(params: JsonRecord, state: ServerState) {
  const name = String(params.name ?? '');
  const args = asRecord(params.arguments);
  let result: JsonRecord;
  if (name === 'site_lifecycle_guidance') result = buildGuidanceResult(args);
  else   if (name === 'site_lifecycle_doctor') result = siteLifecycleDoctor(state);
  else if (name === 'site_lifecycle_command_map') result = siteLifecycleCommandMap();
  else {
    const spec = COMMANDS.find((item) => item.tool === name);
    if (!spec) throw diagnosticError('unknown_tool', `unknown_tool:${name}`, { tool_name: name });
    result = await invokeSiteCommand(spec, args, state);
  }
  return { content: [{ type: 'text', text: renderResult(result) }], structuredContent: result };
}

function siteLifecycleDoctor(state: ServerState): JsonRecord {
  return {
    status: existsSync(state.cliModulePath) ? 'ok' : 'cli_module_missing',
    server_name: SERVER_NAME,
    narada_root: state.naradaRoot,
    cli_module_path: state.cliModulePath,
    cli_module_exists: existsSync(state.cliModulePath),
    command_count: COMMANDS.length,
    coverage: COMMANDS.map(commandSummary),
    remediation: existsSync(state.cliModulePath) ? null : `Build the Narada CLI first, e.g. run pnpm --filter @narada-core/cli build in ${state.naradaRoot}.`,
  };
}

function siteLifecycleCommandMap(): JsonRecord {
  return { status: 'ok', commands: COMMANDS.map(commandSummary), count: COMMANDS.length };
}

async function invokeSiteCommand(spec: SiteCommandSpec, args: JsonRecord, state: ServerState): Promise<JsonRecord> {
  const execute = args.execute === true || args.mutation_authorized === true;
  const normalizedArgs = normalizeCommandArgs(spec, args, { dryRunDefault: spec.tool === 'site_create_plan' });
  if (!spec.readOnly && spec.requiresExecute && !execute) {
    return {
      status: 'planned',
      read_only: true,
      mutation_performed: false,
      tool: spec.tool,
      cli_command: spec.cli,
      reason: 'mutation_requires_execute_true',
      required_arguments: ['execute:true', ...(spec.requiresAuthority ? ['authority_basis'] : [])],
      normalized_args: normalizedArgs,
      next_action: siteActionForCommand(spec, normalizedArgs),
    };
  }
  if (spec.requiresAuthority && !isPlainObject(args.authority_basis)) {
    throw diagnosticError('authority_basis_required', `authority_basis_required:${spec.tool}`, { tool: spec.tool, cli_command: spec.cli });
  }
  const module = await loadCliModule(state, spec.module);
  const fn = module[spec.functionName];
  if (typeof fn !== 'function') {
    throw diagnosticError('cli_function_missing', `cli_function_missing:${spec.functionName}`, {
      tool: spec.tool,
      cli_command: spec.cli,
      cli_module_path: spec.module === 'operator-surface' ? state.operatorSurfaceModulePath : state.cliModulePath,
    });
  }
  const raw = await callCliFunction(fn, spec, normalizedArgs);
  const rawResult = raw?.result ?? raw;
  const nextAction = spec.tool === 'site_doctor'
    ? siteDoctorNextAction(rawResult, args)
    : siteActionForCommand(spec, normalizedArgs);
  const result = spec.tool === 'site_doctor' && rawResult && typeof rawResult === 'object' && !Array.isArray(rawResult)
    ? { ...(rawResult as JsonRecord), next_action: nextAction }
    : rawResult;
  return {
    status: raw?.exitCode && raw.exitCode !== 0 ? 'failed' : 'ok',
    tool: spec.tool,
    cli_command: spec.cli,
    cli_function: spec.functionName,
    mutation_performed: !spec.readOnly && execute,
    result,
    next_action: nextAction,
    exit_code: raw?.exitCode ?? 0,
  };
}

async function loadCliModule(state: ServerState, moduleName: SiteCommandSpec['module'] = 'sites'): Promise<JsonRecord> {
  const modulePath = moduleName === 'operator-surface' ? state.operatorSurfaceModulePath : state.cliModulePath;
  if (!existsSync(modulePath)) {
    throw diagnosticError('narada_cli_module_missing', `narada_cli_module_missing:${modulePath}`, {
      cli_module_path: modulePath,
      remediation: `Build the Narada CLI first, e.g. run pnpm --filter @narada-core/cli build in ${state.naradaRoot}.`,
    });
  }
  return import(pathToFileURL(modulePath).href) as Promise<JsonRecord>;
}

async function callCliFunction(fn: Function, spec: SiteCommandSpec, args: JsonRecord) {
  const context = silentCommandContext();
  if (spec.module === 'operator-surface') {
    return fn(operatorSurfaceCommandArgs(spec.tool, args), context);
  }
  if (['site_show', 'site_doctor', 'site_init'].includes(spec.tool)) {
    const siteId = requiredString(args, 'site_id');
    return fn(siteId, stripKeys(args, ['site_id', 'execute', 'mutation_authorized', 'authority_basis']), context);
  }
  return fn(stripKeys(args, ['execute', 'mutation_authorized', 'authority_basis']), context);
}

function operatorSurfaceCommandArgs(toolName: string, args: JsonRecord): JsonRecord {
  const suppliedCwd = String(args.site_root ?? args.cwd ?? args.root ?? '').trim();
  if (!suppliedCwd) throw diagnosticError('site_root_required', 'site_root_required:' + toolName, { tool: toolName });
  const cwd = /[\\/]\\.narada$/i.test(suppliedCwd) ? suppliedCwd : join(suppliedCwd, '.narada');
  const site = String(args.site_id ?? args.site ?? '').trim();
  if (toolName === 'site_admit_role') {
    return {
      cwd,
      site,
      role: args.role,
      agentKind: args.agent_kind,
      identityName: args.identity,
      by: args.by,
      inputCapabilities: args.input_capabilities,
      submitStrategy: args.submit_strategy,
      bindFocused: args.bind_focused,
      runtimeLocus: args.runtime_locus,
      dryRun: args.dry_run,
      format: 'json',
    };
  }
  if (toolName === 'site_verify_role') {
    return { cwd, site, runtimeLocus: args.runtime_locus, limit: args.limit, format: 'json' };
  }
  if (toolName === 'site_observe_runtime') {
    return { cwd, site, limit: args.limit, format: 'json' };
  }
  if (toolName === 'site_bind_runtime') {
    return {
      cwd,
      identity: args.identity,
      runtimeLocus: args.runtime_locus,
      handle: args.handle,
      observedHandle: args.observed_handle,
      staleAfter: args.stale_after,
      format: 'json',
    };
  }
  throw diagnosticError('operator_surface_tool_unsupported', 'operator_surface_tool_unsupported:' + toolName, { tool: toolName });
}

function normalizeCommandArgs(spec: SiteCommandSpec, args: JsonRecord, options: { dryRunDefault?: boolean } = {}): JsonRecord {
  const normalized: JsonRecord = { ...args, format: 'json', verbose: args.verbose === true };
  for (const [from, to] of [['site_id', 'siteId'], ['site_kind', 'siteKind'], ['authority_locus', 'authorityLocus'], ['execution_surface', 'executionSurface'], ['source_site', 'sourceSite'], ['target_site', 'targetSite'], ['authority_mode', 'authorityMode'], ['mutation_family', 'mutationFamily'], ['role_required', 'roleRequired']] as const) {
    if (normalized[from] !== undefined && normalized[to] === undefined) normalized[to] = normalized[from];
  }
  if (spec.tool === 'site_create_plan') normalized.dryRun = true;
  if (options.dryRunDefault && normalized.dryRun === undefined) normalized.dryRun = true;
  if (spec.tool === 'site_deps_sync' && normalized.apply === undefined) normalized.apply = normalized.execute === true;
  if (spec.module === 'operator-surface' && normalized.site_root === undefined) normalized.site_root = normalized.root ?? normalized.cwd;
  return normalized;
}

function canonicalActionSiteId(value: unknown): string {
  if (typeof value !== 'string' || !value.trim() || /[\\/]/.test(value.trim())) return '<site-id>';
  return value.trim();
}

function actionRoot(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value.trim() : null;
}

function siteAction(
  tool: string,
  status: 'ready' | 'needs_input' | 'planned',
  args: JsonRecord,
  reason: string,
  requiredInputs: string[] = [],
): JsonRecord {
  return {
    schema: 'narada.site_lifecycle.next_action.v1',
    status,
    surface_id: 'site-lifecycle',
    tool,
    arguments: args,
    mutation_authorized: false,
    authority_locus: authorityRootForSiteRoot(args.site_root),
    reason,
    required_inputs: requiredInputs,
  };
}

function authorityRootForSiteRoot(value: unknown): string | null {
  const root = actionRoot(value);
  if (!root) return null;
  return /[\\/]\\.narada$/i.test(root) ? root : join(root, '.narada');
}

function siteActionForCommand(spec: SiteCommandSpec, args: JsonRecord): JsonRecord | null {
  if (spec.tool === 'site_admit_role') {
    return siteAction('site_admit_role', 'planned', {
      site_id: canonicalActionSiteId(args.site_id),
      site_root: args.site_root,
      role: args.role,
      agent_kind: args.agent_kind,
      identity: args.identity ?? null,
      by: args.by,
      execute: true,
      authority_basis: '<operator-authority-basis>',
    }, 'Role admission is mutation-gated and requires explicit operator authority.');
  }
  if (spec.tool === 'site_bind_runtime') {
    const missing = ['runtime_locus', 'handle'].filter((key) => !args[key]);
    return siteAction('site_bind_runtime', missing.length === 0 ? 'planned' : 'needs_input', {
      site_root: args.site_root,
      identity: args.identity,
      runtime_locus: args.runtime_locus ?? null,
      handle: args.handle ?? null,
      execute: true,
      authority_basis: '<operator-authority-basis>',
    }, 'Runtime binding requires an explicitly observed locus and handle; ambient focus is not evidence.', missing);
  }
  return null;
}

function siteDoctorNextAction(rawResult: unknown, args: JsonRecord): JsonRecord | null {
  if (!rawResult || typeof rawResult !== 'object' || Array.isArray(rawResult)) return null;
  const record = rawResult as JsonRecord;
  const readiness = record.readiness;
  if (!readiness || typeof readiness !== 'object' || Array.isArray(readiness)) return null;
  const readinessRecord = readiness as JsonRecord;
  const posture = readinessRecord.posture;
  const target = readinessRecord.target_locus && typeof readinessRecord.target_locus === 'object' && !Array.isArray(readinessRecord.target_locus)
    ? readinessRecord.target_locus as JsonRecord
    : {};
  const coordinates = readinessRecord.coordinates && typeof readinessRecord.coordinates === 'object' && !Array.isArray(readinessRecord.coordinates)
    ? readinessRecord.coordinates as JsonRecord
    : {};
  const operatorPosture = coordinates.operator_surface_posture && typeof coordinates.operator_surface_posture === 'object' && !Array.isArray(coordinates.operator_surface_posture)
    ? coordinates.operator_surface_posture as JsonRecord
    : {};
  const siteRoot = actionRoot(record.siteRoot) ?? actionRoot(target.site_root) ?? actionRoot(args.site_root) ?? actionRoot(args.root);
  const siteId = canonicalActionSiteId(record.siteId ?? target.site_id ?? args.site_id);
  const role = typeof operatorPosture.role === 'string' && operatorPosture.role.trim() ? operatorPosture.role : 'architect';
  if (posture === 'ready_missing_role_binding') {
    return siteAction('site_admit_role', 'ready', {
      site_id: siteId,
      site_root: siteRoot,
      role,
      agent_kind: 'codex_cli',
      identity: siteId === '<site-id>' ? null : siteId + '.' + role,
      by: '<principal>',
      execute: true,
      authority_basis: '<operator-authority-basis>',
    }, 'The Site doctor found no durable identity admission for the required role.', siteRoot ? [] : ['site_root']);
  }
  if (posture === 'ready_missing_transport') {
    const identity = typeof operatorPosture.identity_id === 'string' && operatorPosture.identity_id.trim()
      ? operatorPosture.identity_id
      : null;
    const runtimeLocus = typeof operatorPosture.runtime_locus === 'string' && operatorPosture.runtime_locus.trim()
      ? operatorPosture.runtime_locus
      : null;
    const requiredInputs = ['runtime_locus', 'handle'];
    return siteAction('site_bind_runtime', 'needs_input', {
      site_root: siteRoot,
      identity,
      runtime_locus: runtimeLocus,
      handle: null,
      execute: true,
      authority_basis: '<operator-authority-basis>',
    }, 'The Site doctor separates declared transport from runtime binding; binding must be admitted by the owning runtime locus.', requiredInputs.concat(identity ? [] : ['identity']));
  }
  return null;
}

function silentCommandContext() {
  return { logger: { info() {}, warn() {}, error() {}, debug() {} }, signal: undefined };
}

function commandSummary(spec: SiteCommandSpec) {
  return {
    tool: spec.tool,
    cli_command: spec.cli,
    cli_function: spec.functionName,
    read_only: spec.readOnly,
    requires_execute: spec.requiresExecute === true,
    requires_authority: spec.requiresAuthority === true,
  };
}

function tool(name: string, description: string, properties: JsonRecord, required: string[] = [], readOnly = true) {
  return {
    name,
    description,
    inputSchema: { type: 'object', properties, required, additionalProperties: false },
    annotations: { title: name, readOnlyHint: readOnly, destructiveHint: false, idempotentHint: readOnly, openWorldHint: true },
    outputSchema: { type: 'object', additionalProperties: true },
  };
}

function createSiteProperties() {
  return {
    config: stringSchema('Create-site config JSON path.'),
    preset: stringSchema('Greenfield template preset.'),
    site_id: stringSchema('Site id for shorthand create-site planning.'),
    root: stringSchema('Site root for shorthand create-site planning.'),
    site_kind: stringSchema('Site kind for shorthand create-site planning.'),
    authority_locus: stringSchema('Authority locus for shorthand create-site planning.'),
    output_plan: stringSchema('Optional path to write the dry-run plan JSON artifact.'),
  };
}

function mutationProperties() {
  return {
    execute: booleanSchema('Perform the mutation. Omit or false returns a plan/refusal where supported.'),
    mutation_authorized: booleanSchema('Explicit mutation authorization alias for execute.'),
    authority_basis: { type: 'object', description: 'Required authority basis for mutation tools.', additionalProperties: true },
  };
}

function stringSchema(description: string) { return { type: 'string', description }; }
function booleanSchema(description: string) { return { type: 'boolean', description }; }
function numberSchema(description: string) { return { type: 'number', description }; }

function asRecord(value: unknown): JsonRecord {
  return isPlainObject(value) ? value as JsonRecord : {};
}

function isPlainObject(value: unknown): value is JsonRecord {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function requiredString(args: JsonRecord, key: string): string {
  const value = args[key];
  if (typeof value !== 'string' || value.trim() === '') throw diagnosticError('required_string_missing', `required_string_missing:${key}`, { key });
  return value.trim();
}

function stripKeys(input: JsonRecord, keys: string[]): JsonRecord {
  const result = { ...input };
  for (const key of keys) delete result[key];
  return result;
}

function normalizePath(path: string) {
  return resolve(path).replace(/\\/g, '/');
}

function renderResult(result: JsonRecord) {
  return JSON.stringify(result, null, 2);
}

function diagnosticError(code: string, message: string, detail: JsonRecord = {}) {
  const error = new Error(message) as Error & { code?: string; detail?: JsonRecord };
  error.code = code;
  error.detail = detail;
  return error;
}

function errorDiagnostic(error: unknown): JsonRecord {
  if (error instanceof Error) {
    const anyError = error as Error & { code?: string; detail?: JsonRecord };
    return { code: anyError.code ?? 'error', message: error.message, ...(anyError.detail ?? {}) };
  }
  return { code: 'error', message: String(error) };
}

function drainJsonRpcFrames(buffer: string): { requests: JsonRecord[]; remaining: string; framed: boolean } {
  const requests: JsonRecord[] = [];
  let remaining = buffer;
  while (true) {
    const crlfHeaderEnd = remaining.indexOf('\r\n\r\n');
    const lfHeaderEnd = remaining.indexOf('\n\n');
    const headerEnd = crlfHeaderEnd >= 0 ? crlfHeaderEnd : lfHeaderEnd;
    if (headerEnd < 0) break;
    const separatorLength = crlfHeaderEnd >= 0 ? 4 : 2;
    const header = remaining.slice(0, headerEnd);
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) break;
    const bodyStart = headerEnd + separatorLength;
    const bodyEnd = bodyStart + Number(match[1]);
    if (remaining.length < bodyEnd) break;
    requests.push(JSON.parse(remaining.slice(bodyStart, bodyEnd)));
    remaining = remaining.slice(bodyEnd);
  }
  return { requests, remaining, framed: requests.length > 0 };
}

function drainJsonLines(buffer: string): { requests: JsonRecord[]; remaining: string; framed: boolean } {
  const requests: JsonRecord[] = [];
  const lines = buffer.split(/\r?\n/);
  const remaining = lines.pop() ?? '';
  for (const line of lines) {
    if (line.trim()) requests.push(JSON.parse(line));
  }
  return { requests, remaining, framed: false };
}

function writeJsonRpcResponse(response: JsonRecord, options: { framed: boolean }) {
  const body = JSON.stringify(response);
  if (options.framed) process.stdout.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\n\n${body}`);
  else process.stdout.write(`${body}\n`);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const parsed = parseArgs(process.argv.slice(2));
  runStdioServer(parsed).catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
}

function parseArgs(argv: string[]): JsonRecord {
  const parsed: JsonRecord = {};
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    if (!arg.startsWith('--')) continue;
    const key = arg.slice(2).replace(/-([a-z])/g, (_, c) => c.toUpperCase());
    const next = argv[index + 1];
    if (next && !next.startsWith('--')) {
      parsed[key] = next;
      index++;
    } else {
      parsed[key] = true;
    }
  }
  return parsed;
}