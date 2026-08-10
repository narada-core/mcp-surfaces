#!/usr/bin/env node
import { buildGuidanceResult } from './guidance.js';
import { guidanceToolDefinition } from './guidance.js';
import { createHash } from 'node:crypto';
import { DatabaseSync } from '@narada-core/sqlite';
import { spawn } from 'node:child_process';
import { payloadShow } from '@narada-core/mcp-transport';
import { existsSync, mkdirSync, readdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { defineNativeSurface, surfaceDescriptorDigest, surfaceExecutionDeclaration, surfaceToolContractDigest, type DefinedSurface, type McpToolDefinition, type SurfaceDescriptorV2, type SurfaceExecutionDeclaration } from '@narada-core/mcp-fabric-contracts';
import {
  MCP_RUNTIME_CONTRACT_VERSION,
  buildMaterializationGeneration,
  materializationSidecarPath,
  validateMaterializedConfiguration,
  writeMaterializationGeneration,
} from '@narada-core/mcp-runtime-proxy/materialization-contract';
import { NATIVE_SURFACE_DEFINITIONS } from './native-catalog.js';
import { isNativeArtifactEntrypoint, resolveNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';
import { resolveRuntimeMaterializationPlan, runtimeImplementationMatrixContractPath, runtimeImplementationMatrixFingerprint, runtimeMaterializationPlanFingerprint, runtimeMaterializationPlanEntry } from '@narada-core/operator-surface-runtime-contract/runtime-materialization-plan';

const SERVER_NAME = 'mcp-registrar';
const SERVER_VERSION = '0.1.0';
const PROTOCOL_VERSION = '2024-11-05';

type ValidationFinding = {
  severity: 'error' | 'warning' | 'info';
  code: string;
  message: string;
  server_key?: string;
  surface_id?: string;
  entrypoint?: string;
  detail?: JsonRecord;
};

type JsonRecord = Record<string, unknown>;

export type McpInjectionScope = 'host' | 'user_site' | 'local_site';
type McpRestartOwner = McpInjectionScope;
type McpRuntimeKind = 'nars';
type McpDefaultInjection = 'all_site_bound_sessions' | 'all_carrier_sessions' | 'runtime_selected_sessions';

export type McpSurfaceProjection = {
  id: string;
  injection_scope: McpInjectionScope;
  execution: SurfaceExecutionDeclaration;
  command?: string;
  default_injection?: McpDefaultInjection;
  restart_owner?: McpRestartOwner;
  runtime_requirements?: McpRuntimeKind[];
  env_vars?: string[];
  entrypoint?: string;
  args?: string[];
};

type McpAuthorityLocus =
  | { kind: 'host' }
  | { kind: 'user_site'; site_root: string }
  | { kind: 'local_site'; site_root: string };

export type RegistrarSurfaceRecord = {
  id: string;
  package: string;
  entrypoint: string;
  kind: 'mcp_surface' | 'site_tool';
  args: string[];
  tools: string[];
  output_reader_closure?: Record<string, string>;
  output_reader_policy_note?: string;
  projections?: McpSurfaceProjection[];
  injection_scope?: McpInjectionScope;
  default_injection?: McpDefaultInjection;
  restart_owner?: McpRestartOwner;
  env_vars?: string[];
  sops_dir?: string;
  codex_startup_timeout_sec?: number;
};

type SurfaceScopeMetadata = {
  injection_scope: McpInjectionScope;
  authority_locus: McpAuthorityLocus;
  mutation_locus: McpAuthorityLocus;
  restart_owner: McpRestartOwner;
};

type NaradaScopeMetadata = SurfaceScopeMetadata & {
  bound_into_site?: string;
  scope_source: 'registrar_surface_catalog' | 'site_config_narada_scope' | 'site_config_legacy_top_level';
};

type SiteLocalSurface = {
  surface_id: string;
  kind: 'mcp_entrypoint';
  command: string;
  path: string;
  canonical_tool_prefix?: string;
  replaces?: string;
};

type SiteDef = {
  site_id: string;
  root: string;
  config_path: string;
  surfaces: string[];
  local_surface_allowlist?: string[];
  surface_overrides?: Record<string, SurfaceOverride>;
};

type SiteMcpFabricMode = 'empty' | 'aggregate' | 'sidecar';
type McpCarrierLoadingMode = 'static' | 'progressive';

const PROGRESSIVE_BOOTSTRAP_SURFACES = [
  'agent-context',
  'mcp-registrar',
  'mcp-loader',
  'local-filesystem',
] as const;

const DEFAULT_SURFACE_REPLACEMENTS: Readonly<Record<string, readonly string[]>> = {
  'task-lifecycle': ['work-lifecycle'],
};

type SiteBinding = {
  site_id: string;
  surfaces: 'all' | string[];
  prefix: string;
  loading_mode?: McpCarrierLoadingMode;
  runtime_kind?: McpRuntimeKind;
  extra_allowed_roots?: string[];
};

type SurfaceOverride = {
  entrypoint?: string;
  args?: string[];
  env_vars?: string[];
  surface_implementation?: 'js' | 'native';
  approval_mode?: 'auto' | 'approve';
  enabled?: boolean;
};

type CodexPluginOverrides = Record<string, boolean>;

type MaterializedServer = {
  kind: 'shared' | 'local';
  entrypoint: string;
  command?: string;
  args: string[];
  surface?: RegistrarSurfaceRecord;
  projection?: McpSurfaceProjection;
  local?: SiteLocalSurface;
  env_vars?: string[];
  enabled?: boolean;
  surface_implementation?: 'js' | 'native';
  narada_scope: NaradaScopeMetadata;
} & SurfaceScopeMetadata;

type CarrierDef = {
  carrier_id: string;
  kind: 'opencode' | 'kimi' | 'codex';
  config_path: string;
  surfaces: string[];
  site_bindings: SiteBinding[];
  extra_allowed_roots?: string[];
  trust_projects?: string[];
  surface_overrides?: Record<string, SurfaceOverride>;
  codex_plugin_overrides?: CodexPluginOverrides;
};

function findPackageRoot(moduleDirectory: string): string {
  let current = resolve(moduleDirectory);
  for (let depth = 0; depth < 6; depth += 1) {
    if (existsSync(join(current, 'package.json'))) return current;
    const parent = dirname(current);
    if (parent === current) break;
    current = parent;
  }
  return resolve(moduleDirectory, '..');
}

function portablePathLiteral(path: string): string {
  return resolve(path).replace(/\\/g, '/');
}

function tomlBasicString(value: string): string {
  return JSON.stringify(value);
}

const MCP_REGISTRAR_PACKAGE_ROOT = findPackageRoot(dirname(fileURLToPath(import.meta.url)));
const MCP_WORKSPACE_ROOT = resolve(process.env.NARADA_MCP_WORKSPACE_ROOT ?? join(MCP_REGISTRAR_PACKAGE_ROOT, '..', '..'));
const configuredSourceRoot = process.env.NARADA_SRC_ROOT?.trim();
const MCP_WORKSPACE_PARENT = resolve(configuredSourceRoot || resolve(MCP_WORKSPACE_ROOT, '..'));
const MCP_SURFACES_ROOT = portablePathLiteral(process.env.NARADA_MCP_SURFACES_ROOT ?? join(MCP_WORKSPACE_ROOT, 'packages'));
const MCP_RUNTIME_PROXY_ENTRYPOINT = `${MCP_SURFACES_ROOT}/shared/mcp-runtime-proxy/dist/src/main.js`;
const MCP_RUNTIME_PROXY_PACKAGE_ROOT = resolve(MCP_SURFACES_ROOT, 'shared', 'mcp-runtime-proxy');
const MCP_NATIVE_RUNTIME_PROXY_LEGACY_ENTRYPOINT = `${MCP_SURFACES_ROOT}/shared/mcp-runtime-proxy/dist/native/narada-mcp-runtime${process.platform === 'win32' ? '.exe' : ''}`;
function nativeRuntimeProxyEntrypoint(): string {
  return portablePath(resolveNativeArtifact(MCP_RUNTIME_PROXY_PACKAGE_ROOT, 'narada-mcp-runtime.exe') ?? MCP_NATIVE_RUNTIME_PROXY_LEGACY_ENTRYPOINT);
}
const MCP_REGISTRAR_RUNTIME_ENTRYPOINT = `${MCP_SURFACES_ROOT}/mcp-registrar/dist/src/main.js`;
const MCP_RUNTIME_IMPLEMENTATION_MATRIX_PATH = resolve(runtimeImplementationMatrixContractPath());
const MCP_MCP_LOADER_PACKAGE_ROOT = resolve(MCP_SURFACES_ROOT, 'mcp-loader-mcp');
const MCP_NATIVE_MCP_LOADER_ENTRYPOINT = portablePathLiteral(join(MCP_MCP_LOADER_PACKAGE_ROOT, 'dist', 'native', `narada-mcp-loader${process.platform === 'win32' ? '.exe' : ''}`));
const MCP_LIFECYCLE_NATIVE_PACKAGE_ROOT = resolve(MCP_SURFACES_ROOT, 'shared', 'mcp-lifecycle-native');
const MCP_NATIVE_TASK_LIFECYCLE_ENTRYPOINT = portablePathLiteral(join(MCP_LIFECYCLE_NATIVE_PACKAGE_ROOT, 'dist', 'native', 'narada-task-lifecycle-mcp' + (process.platform === 'win32' ? '.exe' : '')));
const MCP_NATIVE_WORK_LIFECYCLE_ENTRYPOINT = portablePathLiteral(join(MCP_LIFECYCLE_NATIVE_PACKAGE_ROOT, 'dist', 'native', 'narada-work-lifecycle-mcp' + (process.platform === 'win32' ? '.exe' : '')));
const MCP_SHARED_SURFACES_NATIVE_PACKAGE_ROOT = resolve(MCP_SURFACES_ROOT, 'shared', 'mcp-surfaces-native');
const MCP_NATIVE_SHARED_SURFACES_ENTRYPOINT = portablePathLiteral(join(MCP_SHARED_SURFACES_NATIVE_PACKAGE_ROOT, 'dist', 'native', 'narada-mcp-surfaces' + (process.platform === 'win32' ? '.exe' : '')));
const PROCESS_REGISTRAR_ENTRYPOINT_FINGERPRINT = existsSync(MCP_REGISTRAR_RUNTIME_ENTRYPOINT)
  ? createHash('sha256').update(readFileSync(MCP_REGISTRAR_RUNTIME_ENTRYPOINT)).digest('hex')
  : null;
const MCP_WORKSPACE_ARTIFACT_MANIFEST = portablePathLiteral(join(MCP_WORKSPACE_ROOT, '.ai', 'runtime', 'workspace-artifact-manifest.json'));
const MCP_REGISTRAR_ENTRYPOINT = '{mcp_surfaces_root}/mcp-registrar/dist/src/main.js';
const SPEECH_PROVIDER_REGISTRY_PATH = `${MCP_SURFACES_ROOT}/speech-mcp/config/provider-registry.v2.json`;

type RuntimeProxyImplementation = 'bun' | 'node' | 'native';
type RuntimeProfileKind = 'native' | 'bun' | 'node-compat';
type RuntimeEngineKind = 'node' | 'bun' | 'rust';

type RuntimeMaterializationPlan = JsonRecord;

function acceptedRuntimeMaterializationPlan(value: unknown): RuntimeMaterializationPlan {
  const plan = resolveRuntimeMaterializationPlan(value ?? 'native') as RuntimeMaterializationPlan;
  if (plan.status !== 'accepted') {
    throw diagnosticError('registrar_runtime_profile_refused', `registrar_runtime_profile_refused:${String(plan.candidate_runtime_profile ?? value ?? '')}`, { plan });
  }
  return plan;
}

function runtimePlanMatrixFingerprint(plan: RuntimeMaterializationPlan): string {
  const source = asRecord(plan.source);
  const fingerprint = source.matrix_fingerprint;
  if (typeof fingerprint !== 'string' || !fingerprint) {
    throw diagnosticError('registrar_runtime_implementation_matrix_fingerprint_missing', 'The resolved runtime materialization plan has no matrix fingerprint.', { runtime_profile_kind: plan.runtime_profile_kind });
  }
  return fingerprint;
}

function currentRuntimeImplementationMatrixFingerprint(): string {
  return runtimeImplementationMatrixFingerprint(MCP_RUNTIME_IMPLEMENTATION_MATRIX_PATH);
}

function assertRuntimeMaterializationPlanCurrent(plan: RuntimeMaterializationPlan = runtimeMaterializationPlan): void {
  const expected = runtimePlanMatrixFingerprint(plan);
  const actual = currentRuntimeImplementationMatrixFingerprint();
  if (expected !== actual) {
    throw diagnosticError('registrar_runtime_implementation_matrix_stale', 'The runtime implementation matrix changed after the registrar resolved its materialization plan.', {
      runtime_profile_kind: plan.runtime_profile_kind,
      runtime_implementation_matrix_path: MCP_RUNTIME_IMPLEMENTATION_MATRIX_PATH,
      expected_matrix_fingerprint: expected,
      actual_matrix_fingerprint: actual,
      remediation: 'Restart the registrar process or retry materialization so the current Narada matrix is resolved.',
    });
  }
}

let runtimeMaterializationPlan: RuntimeMaterializationPlan = acceptedRuntimeMaterializationPlan(process.env.NARADA_RUNTIME_PROFILE?.trim() || 'native');
let runtimeProfileKind: RuntimeProfileKind = String(runtimeMaterializationPlan.runtime_profile_kind) as RuntimeProfileKind;

function setRuntimeMaterializationProfile(value: unknown): void {
  runtimeMaterializationPlan = acceptedRuntimeMaterializationPlan(value ?? 'native');
  runtimeProfileKind = String(runtimeMaterializationPlan.runtime_profile_kind) as RuntimeProfileKind;
  runtimeProxyImplementation = runtimeProxyImplementationForPlan();
}

async function withRuntimeMaterializationProfile<T>(value: unknown, callback: () => Promise<T> | T): Promise<T> {
  const previousPlan = runtimeMaterializationPlan;
  const previousProfileKind = runtimeProfileKind;
  const previousProxyImplementation = runtimeProxyImplementation;
  setRuntimeMaterializationProfile(value !== null && value !== undefined && String(value).trim() ? value : runtimeProfileKind);
  try {
    return await callback();
  } finally {
    runtimeMaterializationPlan = previousPlan;
    runtimeProfileKind = previousProfileKind;
    runtimeProxyImplementation = previousProxyImplementation;
  }
}


function matrixPlanEntry(componentKind: string, plan: RuntimeMaterializationPlan = runtimeMaterializationPlan): JsonRecord {
  const entry = runtimeMaterializationPlanEntry(plan, componentKind) as JsonRecord | null;
  if (!entry || entry.implementation_status !== 'admitted') {
    throw diagnosticError('registrar_runtime_implementation_unavailable', `registrar_runtime_implementation_unavailable:${componentKind}`, {
      component_kind: componentKind,
      runtime_profile_kind: runtimeProfileKind,
      entry: entry ?? null,
    });
  }
  return entry;
}

function componentKindForSurface(surfaceId: string): string {
  if (surfaceId === 'mcp-loader' || surfaceId === 'mcp-loader-mcp.local') return 'mcp-loader-mcp';
  if (surfaceId === 'local-filesystem' || surfaceId === 'local-filesystem-mcp.local') return 'filesystem-mcp';
  if (surfaceId === 'structured-command' || surfaceId === 'structured-command-mcp.local') return 'structured-command-mcp';
  if (surfaceId === 'git' || surfaceId === 'git-mcp.local') return 'git-mcp';
  if (surfaceId === 'agent-context' || surfaceId === 'agent-context-mcp.local') return 'agent-context-mcp';
  if (surfaceId === 'mcp-registrar' || surfaceId === 'mcp-registrar-mcp.local') return 'mcp-registrar';
  if (surfaceId === 'task-lifecycle' || surfaceId === 'task-lifecycle-mcp.local') return 'task-lifecycle-mcp';
  if (surfaceId === 'work-lifecycle' || surfaceId === 'work-lifecycle-mcp.local') return 'work-lifecycle-mcp';
  if (surfaceId === 'catalog-observation' || surfaceId === 'catalog-observation-mcp.local') return 'catalog-observation-mcp';
  if (surfaceId === 'operator-routing' || surfaceId === 'operator-routing-mcp.local') return 'operator-routing-mcp';
  if (surfaceId === 'site-inbox' || surfaceId === 'site-inbox-mcp.local') return 'site-inbox-mcp';
  if (surfaceId === 'site-lifecycle' || surfaceId === 'site-lifecycle-mcp.local') return 'site-lifecycle-mcp';
  if (surfaceId === 'site-registry' || surfaceId === 'site-registry-mcp.local') return 'site-registry-mcp';
  if (surfaceId === 'project-state' || surfaceId === 'project-state-mcp.local') return 'project-state-mcp';
  if (surfaceId === 'runtime-introspection' || surfaceId === 'runtime-introspection-mcp.local') return 'runtime-introspection-mcp';
  if (surfaceId === 'site-coherence' || surfaceId === 'site-coherence-mcp.local') return 'site-coherence-mcp';
  if (surfaceId === 'launcher' || surfaceId === 'launcher-mcp.local') return 'launcher-mcp';
  if (surfaceId === 'mailbox' || surfaceId === 'mailbox-mcp.local') return 'mailbox-mcp';
  if (surfaceId === 'graph-mail' || surfaceId === 'graph-mail-mcp.local') return 'graph-mail-mcp';
  if (surfaceId === 'calendar' || surfaceId === 'calendar-mcp.local') return 'calendar-mcp';
  if (surfaceId === 'site-loop' || surfaceId === 'site-loop-mcp.local') return 'site-loop-mcp';
  if (surfaceId === 'worker-delegation' || surfaceId === 'worker-delegation-mcp.local') return 'worker-delegation-mcp';
  if (surfaceId === 'delegated-task' || surfaceId === 'delegated-task-mcp.local') return 'delegated-task-mcp';
  if (surfaceId === 'sop' || surfaceId === 'sop-mcp.local') return 'sop-mcp';
  if (surfaceId === 'scheduler' || surfaceId === 'scheduler-mcp.local') return 'scheduler-mcp';
  if (surfaceId === 'surface-feedback' || surfaceId === 'surface-feedback-mcp.local') return 'surface-feedback-mcp';
  if (surfaceId === 'speech' || surfaceId === 'speech-mcp.local') return 'speech-mcp';
  if (surfaceId === 'artifacts' || surfaceId === 'artifacts-mcp.local') return 'artifacts-mcp';
  if (surfaceId === 'nars-session' || surfaceId === 'nars-session-mcp.local') return 'nars-session-mcp';
  if (surfaceId === 'quota-meter' || surfaceId === 'quota-meter-mcp.local') return 'quota-meter-mcp';
  if (surfaceId === 'operator-console-overlay' || surfaceId === 'operator-console-overlay-mcp.local') return 'operator-console-overlay-mcp';
  if (surfaceId === 'browser-control' || surfaceId === 'browser-control-mcp.local') return 'browser-control-mcp';
  if (surfaceId === 'cloudflare-carrier' || surfaceId === 'cloudflare-carrier-mcp.local') return 'cloudflare-carrier-mcp';
  return 'mcp-javascript-surface';
}

function selectedSurfaceRuntimeEngine(surfaceId: string, explicitImplementation?: 'js' | 'native', plan: RuntimeMaterializationPlan = runtimeMaterializationPlan): RuntimeEngineKind {
  const componentKind = componentKindForSurface(surfaceId);
  if (explicitImplementation === 'native') {
    const entry = matrixPlanEntry(componentKind, plan);
    if (entry.runtime_engine_kind !== 'rust') {
      throw diagnosticError('registrar_native_surface_not_admitted', `registrar_native_surface_not_admitted:${componentKind}`, {
        component_kind: componentKind,
        runtime_profile_kind: runtimeProfileKind,
        runtime_engine_kind: entry.runtime_engine_kind,
      });
    }
    return 'rust';
  }
  if (explicitImplementation === 'js') return String(matrixPlanEntry('mcp-javascript-surface', plan).runtime_engine_kind) as RuntimeEngineKind;
  return String(matrixPlanEntry(componentKind, plan).runtime_engine_kind) as RuntimeEngineKind;
}


function javascriptRuntimeCommand(engine: RuntimeEngineKind): string {
  if (engine === 'bun') return 'bun';
  if (engine === 'node') return 'node';
  throw diagnosticError('registrar_native_surface_command_required', `registrar_native_surface_command_required:${engine}`);
}

function runtimeProxyImplementationForResolvedPlan(plan: RuntimeMaterializationPlan): RuntimeProxyImplementation {
  const entry = runtimeMaterializationPlanEntry(plan, 'mcp-runtime-proxy');
  const engine = String(entry?.runtime_engine_kind ?? '') as RuntimeEngineKind;
  return engine === 'rust' ? 'native' : engine as RuntimeProxyImplementation;
}

function runtimeProxyImplementationForPlan(): RuntimeProxyImplementation {
  return runtimeProxyImplementationForResolvedPlan(runtimeMaterializationPlan);
}

function runtimeMaterializationPlanPath(configPath: string): string {
  return `${resolve(configPath)}.narada-runtime-plan.json`;
}

function nativeRuntimeProxyAvailable(): boolean {
  return process.platform === 'win32' && existsSync(nativeRuntimeProxyEntrypoint());
}

export function defaultRuntimeProxyImplementation(
  platform: NodeJS.Platform = process.platform,
  nativeAvailable = nativeRuntimeProxyAvailable(),
): RuntimeProxyImplementation {
  return platform === 'win32' && nativeAvailable ? 'native' : 'bun';
}

export function defaultSurfaceImplementation(
  surfaceId: string,
  args: string[],
  nativeAvailable = nativeRuntimeProxyAvailable(),
): 'js' | 'native' | undefined {
  if (surfaceId !== 'local-filesystem') return undefined;
  const modeIndex = args.indexOf('--mode');
  const mode = modeIndex >= 0 ? args[modeIndex + 1] : undefined;
  return mode === 'read' && nativeAvailable ? 'native' : 'js';
}

let runtimeProxyImplementation: RuntimeProxyImplementation = runtimeProxyImplementationForPlan();

function selectedRuntimeProxyEntrypoint(implementation: RuntimeProxyImplementation = runtimeProxyImplementation): string {
  return implementation === 'native'
    ? nativeRuntimeProxyEntrypoint()
    : MCP_RUNTIME_PROXY_ENTRYPOINT;
}

function nativeEntrypoint(value: string): string {
  return value.replace('{mcp_surfaces_root}', MCP_SURFACES_ROOT);
}

function nativeProjectionToRegistrarProjection(
  projection: SurfaceDescriptorV2['projections'][number],
): McpSurfaceProjection {
  if (projection.transport.kind !== 'stdio') {
    throw new Error(`mcp_fabric_transport_unsupported: ${projection.id}`);
  }
  const [entrypoint, ...args] = projection.transport.args;
  if (!entrypoint) {
    throw new Error(`mcp_fabric_entrypoint_missing: ${projection.id}`);
  }
  return {
    id: projection.id,
    injection_scope: projection.injection_scope,
    execution: surfaceExecutionDeclaration(projection.execution),
    default_injection: projection.default_injection === 'enabled'
      ? projection.injection_scope === 'host' ? 'all_carrier_sessions' : 'all_site_bound_sessions'
      : projection.runtime_requirements.length > 0 ? 'runtime_selected_sessions' : undefined,
    restart_owner: projection.lifecycle.restart_owner && (
      projection.lifecycle.restart_owner === 'host'
      || projection.lifecycle.restart_owner === 'user_site'
      || projection.lifecycle.restart_owner === 'local_site'
    ) ? projection.lifecycle.restart_owner : projection.injection_scope,
    runtime_requirements: projection.runtime_requirements.filter((value): value is McpRuntimeKind => value === 'nars'),
    env_vars: projection.transport.env,
    command: projection.transport.command,
    entrypoint: nativeEntrypoint(entrypoint),
    args,
  };
}

const SURFACE_OUTPUT_READER_CLOSURES: Readonly<Record<string, Record<string, string>>> = {
  git: { git_status: 'git_output_show', git_diff: 'git_output_show', git_log: 'git_output_show', git_show: 'git_output_show' },
  'site-inbox': { inbox_list: 'inbox_output_show', inbox_show: 'inbox_output_show', inbox_audit: 'inbox_output_show' },
  mailbox: { mailbox_messages_list: 'mailbox_output_show', mailbox_message_show: 'mailbox_output_show', mailbox_search: 'mailbox_output_show', mailbox_thread_show: 'mailbox_output_show' },
  'graph-mail': { graph_mail_query: 'graph_mail_output_show', graph_mail_message_show: 'graph_mail_output_show' },
  calendar: { calendar_list: 'calendar_output_show', calendar_event_query: 'calendar_output_show', calendar_event_show: 'calendar_output_show' },
  'site-loop': { site_loop_guidance: 'site_loop_output_show' },
  'agent-context': { agent_context_hydrate_current: 'mcp_output_show', agent_context_startup_sequence: 'mcp_output_show' },
};

function nativeSurfaceToRegistrarRecord(native: DefinedSurface): RegistrarSurfaceRecord {
  const descriptor = native.descriptor;
  const projections = descriptor.projections.map(nativeProjectionToRegistrarProjection);
  const first = projections[0];
  if (!first?.entrypoint) {
    throw new Error(`mcp_fabric_surface_projection_missing: ${descriptor.surface_id}`);
  }
  const singleProjection = projections.length === 1 ? first : undefined;
  const startupTimeout = descriptor.metadata?.codex_startup_timeout_sec;
  return {
    id: descriptor.surface_id,
    package: descriptor.package.replace('@narada-core/', ''),
    entrypoint: first.entrypoint,
    kind: 'mcp_surface',
    args: (first.args ?? []).map(nativeEntrypoint),
    tools: descriptor.tools.map((tool) => tool.name),
    output_reader_closure: SURFACE_OUTPUT_READER_CLOSURES[descriptor.surface_id],
    projections,
    ...(singleProjection
      ? {
          injection_scope: singleProjection.injection_scope,
          default_injection: singleProjection.default_injection,
          restart_owner: singleProjection.restart_owner,
          env_vars: singleProjection.env_vars,
        }
      : {}),
    ...(typeof startupTimeout === 'number' ? { codex_startup_timeout_sec: startupTimeout } : {}),
  };
}

function nativeSurfaceCatalog(): RegistrarSurfaceRecord[] {
  const definitions: DefinedSurface[] = [
    ...Object.values(NATIVE_SURFACE_DEFINITIONS),
    registrarSurfaceDefinition(),
  ];
  return definitions.map(nativeSurfaceToRegistrarRecord);
}

export const SURFACES: RegistrarSurfaceRecord[] = nativeSurfaceCatalog();

const KNOWN_SITES: SiteDef[] = [
  { site_id: 'andrey-user', root: defaultUserNaradaRoot(), config_path: siteConfigPathForRoot(defaultUserNaradaRoot()), surfaces: [] },
  { site_id: 'narada-proper', root: join(MCP_WORKSPACE_PARENT, 'narada'), config_path: siteConfigPathForRoot(join(MCP_WORKSPACE_PARENT, 'narada')), surfaces: [] },
  { site_id: 'narada-sonar', root: join(MCP_WORKSPACE_PARENT, 'narada.sonar'), config_path: siteConfigPathForRoot(join(MCP_WORKSPACE_PARENT, 'narada.sonar')), surfaces: [] },
  { site_id: 'narada-revolution', root: join(MCP_WORKSPACE_PARENT, 'narada.revolution'), config_path: siteConfigPathForRoot(join(MCP_WORKSPACE_PARENT, 'narada.revolution')), surfaces: [] },
  { site_id: 'narada-staccato', root: join(MCP_WORKSPACE_PARENT, 'narada.staccato'), config_path: siteConfigPathForRoot(join(MCP_WORKSPACE_PARENT, 'narada.staccato')), surfaces: [] },
  { site_id: 'narada-cpy', root: join(MCP_WORKSPACE_PARENT, 'narada.cpy'), config_path: siteConfigPathForRoot(join(MCP_WORKSPACE_PARENT, 'narada.cpy')), surfaces: [] },
  { site_id: 'narada-utz', root: join(MCP_WORKSPACE_PARENT, 'narada.utz'), config_path: siteConfigPathForRoot(join(MCP_WORKSPACE_PARENT, 'narada.utz')), surfaces: [] },
  { site_id: 'narada-timour', root: join(MCP_WORKSPACE_PARENT, 'narada.timour-marketing-agent'), config_path: siteConfigPathForRoot(join(MCP_WORKSPACE_PARENT, 'narada.timour-marketing-agent')), surfaces: [] },
  { site_id: 'smart-scheduling', root: join(MCP_WORKSPACE_PARENT, 'smart-scheduling'), config_path: siteConfigPathForRoot(join(MCP_WORKSPACE_PARENT, 'smart-scheduling')), surfaces: [] },
];

type SiteRegistryCatalog = {
  status: 'ready' | 'unavailable';
  path: string;
  items: SiteDef[];
  error?: string;
};

type SiteRegistryRow = {
  site_id?: unknown;
  site_root?: unknown;
  lifecycle_status?: unknown;
};

function comparableSiteRoot(root: string): string {
  return canonicalWorkspaceRoot(root).replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
}

function projectionLaunchArgs(projection: McpSurfaceProjection): string[] {
  if (projection.id !== 'user-site-operator') return [];
  return [
    '--projection', projection.id,
    '--user-site-root', defaultUserNaradaRoot(),
    '--source-kind', 'operator',
    '--operator-id', defaultOperatorId(),
  ];
}

function projectionEnvVars(surface: RegistrarSurfaceRecord, projection: McpSurfaceProjection): string[] {
  if (projection.id === 'user-site-operator') {
    return uniqueStrings([
      'NARADA_OPERATOR_ID',
      'NARADA_USER_SITE_ROOT',
      'NARADA_SITE_REGISTRY_DB',
      'NARADA_NARS_SESSION_ALLOW_STEER',
      ...(projection.env_vars ?? []),
    ]);
  }
  return uniqueStrings([...(surface.env_vars ?? []), ...(projection.env_vars ?? [])]);
}

function assertCanonicalSiteId(siteId: string, fieldName = 'site_id'): string {
  if (siteId === 'narada-andrey' || siteId === 'narada-user-site') {
    throw diagnosticError(
      'registrar_legacy_site_id_rejected',
      `registrar_legacy_site_id_rejected:${fieldName}`,
      { field: fieldName, received: siteId, required: 'andrey-user' },
    );
  }
  return siteId;
}

function siteConfigPathForRoot(root: string): string {
  const rootConfig = join(root, 'config.json');
  const nestedConfig = join(root, '.narada', 'config.json');
  if (existsSync(nestedConfig)) return nestedConfig;
  return rootConfig;
}

function defaultUserNaradaRoot(): string {
  const configured = process.env.NARADA_USER_SITE_ROOT?.trim();
  if (configured) return portablePathLiteral(configured);
  if (process.env.USERPROFILE) return portablePathLiteral(join(process.env.USERPROFILE, 'Narada'));
  if (process.env.HOME) return portablePathLiteral(join(process.env.HOME, 'Narada'));
  return portablePathLiteral(join(MCP_WORKSPACE_ROOT, '.narada', 'user-site'));
}

function defaultOperatorId(): string {
  const configured = process.env.NARADA_OPERATOR_ID?.trim();
  if (configured) return configured;
  const userRoot = defaultUserNaradaRoot();
  const parent = basename(dirname(userRoot));
  return parent && parent !== '.' ? parent.toLowerCase() : 'operator';
}

function canonicalWorkspaceRoot(root: string): string {
  const resolved = resolve(root);
  return basename(resolved).toLowerCase() === '.narada' ? dirname(resolved) : resolved;
}

export function readSiteSurfaceOverrides(
  configPath: string,
  fallback: Record<string, SurfaceOverride> = {},
): Record<string, SurfaceOverride> {
  if (!existsSync(configPath)) return fallback;
  let parsed: JsonRecord;
  try {
    parsed = asRecord(JSON.parse(readFileSync(configPath, 'utf8').replace(/^\uFEFF/, '')));
  } catch (error) {
    throw diagnosticError('registrar_site_config_parse_failed', `registrar_site_config_parse_failed:${configPath}`, {
      config_path: configPath,
      error: error instanceof Error ? error.message : String(error),
    });
  }
  const raw = asRecord(parsed.surface_overrides);
  const overrides: Record<string, SurfaceOverride> = { ...fallback };
  for (const [surfaceId, value] of Object.entries(raw)) {
    const record = asRecord(value);
    if (typeof record.enabled !== 'boolean') {
      throw diagnosticError('registrar_site_surface_override_invalid', `registrar_site_surface_override_invalid:${surfaceId}`, {
        config_path: configPath,
        surface_id: surfaceId,
        required_field: 'enabled:boolean',
      });
    }
    const implementation = record.surface_implementation;
    if (implementation !== undefined && implementation !== 'js' && implementation !== 'native') {
      throw diagnosticError('registrar_site_surface_override_invalid', 'registrar_site_surface_override_invalid:' + surfaceId, {
        config_path: configPath,
        surface_id: surfaceId,
        required_field: 'surface_implementation:js|native',
      });
    }
    overrides[surfaceId] = {
      enabled: record.enabled,
      ...(implementation === 'js' || implementation === 'native' ? { surface_implementation: implementation } : {}),
    };
  }
  return overrides;
}

/**
 * Read the User Site's canonical local Site catalog.
 *
 * The static definitions remain only as an explicit fallback when the registry
 * cannot be read. They
 * are never the authoritative output of registrar_site_list when the registry
 * is available.
 */
function readSiteRegistryCatalog(): SiteRegistryCatalog {
  const path = process.env.NARADA_SITE_REGISTRY_DB ?? join(defaultUserNaradaRoot(), 'registry.db');
  if (!existsSync(path)) {
    return { status: 'unavailable', path, items: [], error: 'registry_file_missing' };
  }
  let db: DatabaseSync | null = null;
  try {
    db = new DatabaseSync(path, { readOnly: true });
    // Older User Site registries predate lifecycle_status. Keep those
    // registries readable while treating rows without the column as active.
    const columns = db.prepare('PRAGMA table_info(site_registry)').all() as unknown as Array<{ name?: unknown }>;
    const hasLifecycleStatus = columns.some((column) => column.name === 'lifecycle_status');
    const select = hasLifecycleStatus
      ? 'SELECT site_id, site_root, lifecycle_status FROM site_registry ORDER BY created_at ASC, site_id ASC'
      : 'SELECT site_id, site_root FROM site_registry ORDER BY created_at ASC, site_id ASC';
    const rows = db.prepare(select).all() as unknown as SiteRegistryRow[];
    const items = rows.flatMap((row) => {
      const siteId = typeof row.site_id === 'string' ? row.site_id.trim() : '';
      const rawRoot = typeof row.site_root === 'string' ? row.site_root.trim() : '';
      const lifecycleStatus = typeof row.lifecycle_status === 'string' ? row.lifecycle_status.trim().toLowerCase() : 'active';
      if (lifecycleStatus !== 'active') return [];
      if (!siteId || !rawRoot) return [];
      const root = canonicalWorkspaceRoot(rawRoot);
      const known = KNOWN_SITES.find((site) => comparableSiteRoot(site.root) === comparableSiteRoot(root));
      const configPath = siteConfigPathForRoot(root);
      return [{
        site_id: siteId,
        root,
        config_path: configPath,
        surfaces: known?.surfaces ?? [],
        local_surface_allowlist: known?.local_surface_allowlist,
        surface_overrides: readSiteSurfaceOverrides(configPath, known?.surface_overrides),
      } satisfies SiteDef];
    });
    return { status: 'ready', path, items };
  } catch (error) {
    return {
      status: 'unavailable',
      path,
      items: [],
      error: error instanceof Error ? error.message : String(error),
    };
  } finally {
    db?.close();
  }
}

function defaultCarrierConfigPath(kind: CarrierDef['kind']): string {
  const home = process.env.USERPROFILE ?? process.env.HOME ?? MCP_WORKSPACE_ROOT;
  if (kind === 'opencode') return resolve(process.env.NARADA_OPENCODE_CONFIG_PATH?.trim() ?? join(home, '.config', 'opencode', 'opencode.jsonc'));
  if (kind === 'kimi') return resolve(process.env.NARADA_KIMI_CONFIG_PATH?.trim() ?? join(home, '.kimi-code', 'mcp.json'));
  return resolve(process.env.NARADA_CODEX_CONFIG_PATH?.trim() ?? join(home, '.codex', 'config.toml'));
}

function defaultCarrierExtraAllowedRoots(): string[] {
  const configured = process.env.NARADA_MCP_EXTRA_ALLOWED_ROOTS?.trim();
  if (configured) {
    return uniqueStrings(configured.split(/[;\r\n]+/).map((value) => value.trim()).filter(Boolean));
  }
  return [portablePathLiteral(MCP_WORKSPACE_PARENT)];
}

function delimitedEnvironmentValues(value: string | undefined): string[] {
  if (!value?.trim()) return [];
  return uniqueStrings(value.split(/[;\r\n]+/).map((item) => item.trim()).filter(Boolean));
}

function assertCodexPluginId(pluginId: string): string {
  if (!pluginId || /[\u0000-\u001f\u007f]/.test(pluginId)) {
    throw diagnosticError(
      'registrar_codex_plugin_id_invalid',
      `registrar_codex_plugin_id_invalid:${pluginId}`,
      { plugin_id: pluginId },
    );
  }
  return pluginId;
}

export function readCodexPluginOverrides(environment: NodeJS.ProcessEnv = process.env): CodexPluginOverrides {
  const enabled = delimitedEnvironmentValues(environment.NARADA_CODEX_ENABLED_PLUGINS).map(assertCodexPluginId);
  const disabled = delimitedEnvironmentValues(environment.NARADA_CODEX_DISABLED_PLUGINS).map(assertCodexPluginId);
  const overlap = enabled.filter((pluginId) => disabled.includes(pluginId));
  if (overlap.length > 0) {
    throw diagnosticError(
      'registrar_codex_plugin_policy_conflict',
      `registrar_codex_plugin_policy_conflict:${overlap.join(',')}`,
      { plugin_ids: overlap },
    );
  }
  return Object.fromEntries([
    ...enabled.map((pluginId) => [pluginId, true] as const),
    ...disabled.map((pluginId) => [pluginId, false] as const),
  ].sort(([left], [right]) => left.localeCompare(right)));
}

const DEFAULT_CODEX_PLUGIN_OVERRIDES: CodexPluginOverrides = {
  'github@openai-curated-remote': false,
};

function configuredCodexPluginOverrides(environment: NodeJS.ProcessEnv = process.env): CodexPluginOverrides {
  return {
    ...DEFAULT_CODEX_PLUGIN_OVERRIDES,
    ...readCodexPluginOverrides(environment),
  };
}

const CARRIERS: CarrierDef[] = [
  {
    carrier_id: 'opencode-andrey', kind: 'opencode', config_path: defaultCarrierConfigPath('opencode'), surfaces: [],
    site_bindings: [{
      site_id: 'andrey-user',
      surfaces: [
        'agent-context',
        'local-filesystem',
        'mcp-registrar',
        'mcp-loader',
      ],
      prefix: 'narada-site-andrey-user',
      loading_mode: 'progressive',
      extra_allowed_roots: defaultCarrierExtraAllowedRoots(),
    }],
    extra_allowed_roots: defaultCarrierExtraAllowedRoots(),
  },
  {
    carrier_id: 'kimi-andrey', kind: 'kimi', config_path: defaultCarrierConfigPath('kimi'), surfaces: [],
    site_bindings: [{
      site_id: 'andrey-user',
      surfaces: [
        'agent-context',
        'local-filesystem',
        'mcp-registrar',
        'mcp-loader',
      ],
      prefix: 'narada-site-andrey-user',
      loading_mode: 'progressive',
      extra_allowed_roots: defaultCarrierExtraAllowedRoots(),
    }],
    extra_allowed_roots: defaultCarrierExtraAllowedRoots(),
  },
  {
    carrier_id: 'codex-andrey', kind: 'codex', config_path: defaultCarrierConfigPath('codex'), surfaces: [],
    site_bindings: [{
      site_id: 'andrey-user',
      surfaces: [
        'agent-context',
        'local-filesystem',
        'mcp-registrar',
        'mcp-loader',
      ],
      prefix: 'narada-site-andrey-user',
      loading_mode: 'progressive',
      extra_allowed_roots: defaultCarrierExtraAllowedRoots(),
    }],
    extra_allowed_roots: defaultCarrierExtraAllowedRoots(),
    codex_plugin_overrides: configuredCodexPluginOverrides(),
  },
];

type RegistrarState = JsonRecord;

const FRESH_REGISTRAR_ENV = 'NARADA_MCP_REGISTRAR_FRESH_CHILD';

function assertRegistrarProcessCurrent(operation: string): void {
  if (!PROCESS_REGISTRAR_ENTRYPOINT_FINGERPRINT || !existsSync(MCP_REGISTRAR_RUNTIME_ENTRYPOINT)) return;
  const currentFingerprint = createHash('sha256').update(readFileSync(MCP_REGISTRAR_RUNTIME_ENTRYPOINT)).digest('hex');
  if (currentFingerprint === PROCESS_REGISTRAR_ENTRYPOINT_FINGERPRINT) return;
  throw diagnosticError(
    'registrar_process_stale',
    'This resident registrar process was started from an older compiled registrar entrypoint.',
    {
      operation,
      process_registrar_entrypoint_fingerprint: PROCESS_REGISTRAR_ENTRYPOINT_FINGERPRINT,
      current_registrar_entrypoint_fingerprint: currentFingerprint,
      remediation: 'Restart the resident mcp-registrar process and retry; no carrier configuration write was performed.',
    },
  );
}

export function createServerState(_options: JsonRecord = {}): RegistrarState {
  return {};
}

function duplicateStrings(values: string[]): string[] {
  const seen = new Set<string>();
  const duplicates = new Set<string>();
  for (const value of values) {
    if (seen.has(value)) duplicates.add(value);
    else seen.add(value);
  }
  return [...duplicates].sort();
}

function siteAggregateMcpFileName(siteId: string): string {
  return `${siteId}-mcp.json`;
}

function siteMcpFabricMode(site: SiteDef): SiteMcpFabricMode {
  const configDir = join(siteMcpControlRoot(site), '.ai', 'mcp');
  if (!existsSync(configDir)) return 'empty';
  const files = readdirSync(configDir).filter((f: string) => f.endsWith('.json'));
  if (files.length === 0) return 'empty';
  if (files.includes(siteAggregateMcpFileName(site.site_id))) return 'aggregate';
  return 'sidecar';
}

export function siteBindSidecarRefusal(site: SiteDef, surfaceId: string, options: JsonRecord = {}): JsonRecord | null {
  if (site.surface_overrides?.[surfaceId]?.enabled === false && options.allow_disabled_sidecar !== true) {
    return {
      status: 'refused',
      reason_code: 'registrar_site_bind_refused_surface_disabled',
      site_id: site.site_id,
      surface_id: surfaceId,
      sidecar_state: 'disabled_by_site_override',
      reason: 'This Site explicitly disables the requested surface, so registrar_site_bind will not materialize a sidecar for it.',
      required_next_step: 'Enable the surface in the Site override or pass allow_disabled_sidecar=true only for an intentional compatibility sidecar.',
    };
  }
  const fabricMode = siteMcpFabricMode(site);
  if (fabricMode !== 'aggregate' || options.allow_sidecar === true) return null;
  return {
    status: 'refused',
    reason_code: 'registrar_site_bind_refused_aggregate_fabric_exists',
    site_id: site.site_id,
    surface_id: surfaceId,
    aggregate_file: siteAggregateMcpFileName(site.site_id),
    reason: 'This Site has an authoritative aggregate MCP fabric. registrar_site_bind would create a sidecar snippet, so it is refused unless allow_sidecar is explicitly true.',
    required_next_step: 'Update the aggregate MCP fabric through the Site materialization path, or pass allow_sidecar=true only for an intentional compatibility sidecar.',
  };
}

function naradaScopeMetadata(surfaceId: string, siteRoot = '{site_root}', boundIntoSite?: string, projectionId?: string): NaradaScopeMetadata {
  const metadata = surfaceScopeMetadata(surfaceId, siteRoot, projectionId);
  return {
    ...metadata,
    ...(boundIntoSite ? { bound_into_site: boundIntoSite } : {}),
    scope_source: 'registrar_surface_catalog',
  };
}

function isInjectionScope(value: unknown): value is McpInjectionScope {
  return value === 'host' || value === 'user_site' || value === 'local_site';
}

function locusFromRecord(value: unknown): McpAuthorityLocus | null {
  const record = asRecord(value);
  if (record.kind === 'host') return { kind: 'host' };
  if (record.kind === 'user_site' && typeof record.site_root === 'string') return { kind: 'user_site', site_root: record.site_root };
  if (record.kind === 'local_site' && typeof record.site_root === 'string') return { kind: 'local_site', site_root: record.site_root };
  return null;
}

function naradaScopeFromRecord(record: JsonRecord, source: NaradaScopeMetadata['scope_source']): NaradaScopeMetadata | null {
  const injectionScope = isInjectionScope(record.injection_scope) ? record.injection_scope : null;
  const authorityLocus = locusFromRecord(record.authority_locus);
  const mutationLocus = locusFromRecord(record.mutation_locus);
  const restartOwner = isInjectionScope(record.restart_owner) ? record.restart_owner : null;
  if (!injectionScope || !authorityLocus || !mutationLocus || !restartOwner) return null;
  return {
    injection_scope: injectionScope,
    authority_locus: authorityLocus,
    mutation_locus: mutationLocus,
    restart_owner: restartOwner,
    ...(typeof record.bound_into_site === 'string' ? { bound_into_site: record.bound_into_site } : {}),
    scope_source: source,
  };
}

function readNaradaScope(serverRecord: JsonRecord, fallbackSurfaceId: string, fallbackSiteRoot: string, fallbackBoundSite?: string): NaradaScopeMetadata {
  const nested = naradaScopeFromRecord(asRecord(serverRecord.narada_scope), 'site_config_narada_scope');
  if (nested) return nested;
  const legacy = naradaScopeFromRecord(serverRecord, 'site_config_legacy_top_level');
  if (legacy) return legacy;
  return naradaScopeMetadata(fallbackSurfaceId, fallbackSiteRoot, fallbackBoundSite);
}

export async function handleRequest(request: JsonRecord, state: RegistrarState) {
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

async function dispatchMethod(method: string, params: JsonRecord, state: RegistrarState) {
  switch (method) {
    case 'initialize':
      return { protocolVersion: params.protocolVersion ?? PROTOCOL_VERSION, capabilities: { tools: {} }, serverInfo: { name: SERVER_NAME, version: SERVER_VERSION } };
    case 'tools/list':
      return { tools: listTools() };
    case 'tools/call':
      return await callTool(params, state);
    default:
      throw diagnosticError('unsupported_mcp_method', `unsupported_mcp_method:${method}`);
  }
}

export function listTools() {
  return [
    guidanceToolDefinition(),
    {
      name: 'registrar_surface_list',
      description: 'List all known MCP surfaces with their packages, tools, and entrypoints.',
      inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      annotations: { title: 'registrar_surface_list', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_site_list',
      description: 'List all known Narada sites with their root paths.',
      inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      annotations: { title: 'registrar_site_list', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_site_surfaces',
      description: 'Show which surfaces are bound to a site.',
      inputSchema: { type: 'object', properties: { site_id: { type: 'string' } }, required: ['site_id'], additionalProperties: false },
      annotations: { title: 'registrar_site_surfaces', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_site_bind',
      description: 'Bind a surface to a Narada site MCP config. Creates or updates the site config file.',
      inputSchema: {
        type: 'object',
        properties: {
          site_id: { type: 'string', description: 'Site identifier, e.g. narada-sonar.' },
          surface_id: { type: 'string', description: 'Surface identifier, e.g. scheduler.' },
          projection_id: { type: 'string', description: 'Explicit surface projection identifier when the surface has more than one authority/runtime projection.' },
          runtime_kind: { type: 'string', enum: ['nars'], description: 'Explicit selected runtime kind. Required when selecting a runtime-affined projection without naming projection_id.' },
          allow_sidecar: { type: 'boolean', description: 'Allow creating a compatibility sidecar even when an authoritative aggregate MCP fabric exists.' },
          allow_disabled_sidecar: { type: 'boolean', description: 'Allow binding a surface explicitly disabled by site override; intended only for compatibility repair.' },
        },
        required: ['site_id', 'surface_id'],
        additionalProperties: false,
      },
      annotations: { title: 'registrar_site_bind', readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_site_unbind',
      description: 'Remove a surface from a Narada site MCP config.',
      inputSchema: {
        type: 'object',
        properties: { site_id: { type: 'string' }, surface_id: { type: 'string' } },
        required: ['site_id', 'surface_id'],
        additionalProperties: false,
      },
      annotations: { title: 'registrar_site_unbind', readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_carrier_list',
      description: 'List all known carriers with their config paths.',
      inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      annotations: { title: 'registrar_carrier_list', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_carrier_bind',
      description: 'Bind a surface to a carrier config (opencode, Kimi, or Codex).',
      inputSchema: {
        type: 'object',
        properties: {
          carrier_id: { type: 'string', description: 'Carrier identifier, e.g. codex-andrey.' },
          surface_id: { type: 'string', description: 'Surface identifier, e.g. scheduler.' },
          projection_id: { type: 'string', description: 'Explicit surface projection identifier when the surface has more than one authority/runtime projection.' },
          site_id: { type: 'string', description: 'Site context for arg interpolation, e.g. sonar. Defaults to andrey-user.' },
          runtime_profile: { type: 'string', enum: ['native', 'bun', 'node-compat'], description: 'Runtime implementation profile selected from the Narada implementation matrix.' },
        },
        required: ['carrier_id', 'surface_id'],
        additionalProperties: false,
      },
      annotations: { title: 'registrar_carrier_bind', readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_carrier_unbind',
      description: 'Remove a surface from a carrier config.',
      inputSchema: {
        type: 'object',
        properties: { carrier_id: { type: 'string' }, surface_id: { type: 'string' } },
        required: ['carrier_id', 'surface_id'],
        additionalProperties: false,
      },
      annotations: { title: 'registrar_carrier_unbind', readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_sync',
      description: 'Bind a surface to all sites/carriers, or bind all surfaces to carriers.',
      inputSchema: {
        type: 'object',
        properties: {
          surface_id: { type: 'string', description: 'Surface identifier. Required unless target is all_surfaces_to_carriers or all_surfaces_to_all_carriers.' },
          projection_id: { type: 'string', description: 'Explicit projection identifier for the selected surface.' },
          runtime_kind: { type: 'string', enum: ['nars'], description: 'Explicit selected runtime kind for site materialization.' },
          target: { type: 'string', enum: ['all_sites', 'all_carriers', 'all', 'all_surfaces_to_carriers', 'all_surfaces_to_all_carriers'], description: 'all_sites/all_carriers/all: bind one surface. all_surfaces_to_carriers: bind all surfaces to a specific carrier. all_surfaces_to_all_carriers: bind all surfaces to all carriers.' },
          carrier_id: { type: 'string', description: 'Required when target is all_surfaces_to_carriers.' },
          site_filter: { type: 'string', description: 'Optional prefix filter for site IDs, e.g. narada-.' },
          allow_sidecar: { type: 'boolean', description: 'Allow explicit compatibility sidecar creation for sites with authoritative aggregate MCP fabric.' },
        },
        required: ['target'],
        additionalProperties: false,
      },
      annotations: { title: 'registrar_sync', readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_materialize_all',
      description: 'Generate and atomically replace every registered carrier-native MCP config. Normal materialization is always all-carrier; use the out-of-band CLI escape hatch only for targeted emergency recovery.',
      inputSchema: {
        type: 'object',
        properties: {
          output_dir: { type: 'string', description: 'Optional directory for inspection output. One config and generation sidecar is written for every registered carrier; omit to write canonical carrier paths.' },
          runtime_profile: { type: 'string', enum: ['native', 'bun', 'node-compat'], description: 'Runtime implementation profile selected from the Narada implementation matrix.' },
        },
        additionalProperties: false,
      },
      annotations: { title: 'registrar_materialize_all', readOnlyHint: false, destructiveHint: true, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_carrier_validate',
      description: 'Validate a carrier configuration without writing it: report missing entrypoints, duplicate server keys, missing required flags, and local/shared collisions.',
      inputSchema: {
        type: 'object',
        properties: {
          carrier_id: { type: 'string', description: 'Carrier identifier, e.g. kimi-andrey.' },
          include_ok: { type: 'boolean', description: 'Include passing checks in output.' },
        },
        required: ['carrier_id'],
        additionalProperties: false,
      },
      annotations: { title: 'registrar_carrier_validate', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_carrier_diff',
      description: 'Compare the generated carrier config against the current carrier config file and report additions, removals, and changes.',
      inputSchema: {
        type: 'object',
        properties: {
          carrier_id: { type: 'string', description: 'Carrier identifier, e.g. kimi-andrey.' },
        },
        required: ['carrier_id'],
        additionalProperties: false,
      },
      annotations: { title: 'registrar_carrier_diff', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_surface_usage',
      description: 'Report which sites and carriers include a given MCP surface (shared surface id or site-local surface id ending in .local).',
      inputSchema: {
        type: 'object',
        properties: {
          surface_id: { type: 'string', description: 'Surface identifier, e.g. site-inbox, local-filesystem, or inbox-mcp.local.' },
        },
        required: ['surface_id'],
        additionalProperties: false,
      },
      annotations: { title: 'registrar_surface_usage', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_site_mcp_fabric_validate',
      description: 'Validate a site-local MCP fabric (.ai/mcp/*.json): entrypoints exist, required flags present, duplicate server keys.',
      inputSchema: {
        type: 'object',
        properties: {
          site_id: { type: 'string', description: 'Site identifier, e.g. narada-proper.' },
          include_ok: { type: 'boolean', description: 'Include passing checks in output.' },
        },
        required: ['site_id'],
        additionalProperties: false,
      },
      annotations: { title: 'registrar_site_mcp_fabric_validate', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_site_surface_registry_sync',
      description: 'Regenerate a site action-admission MCP surface registry from the site MCP fabric and registrar surface catalog.',
      inputSchema: {
        type: 'object',
        properties: {
          site_id: { type: 'string', description: 'Site identifier, e.g. narada-sonar.' },
          dry_run: { type: 'boolean', description: 'Return the generated registry without writing it.' },
        },
        required: ['site_id'],
        additionalProperties: false,
      },
      annotations: { title: 'registrar_site_surface_registry_sync', readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_surface_tool_inventory_check',
      description: 'Compare registrar surface tool metadata with observed MCP tools/list names and report per-surface drift.',
      inputSchema: {
        type: 'object',
        properties: {
          observed_tools: { type: 'object', additionalProperties: { type: 'array', items: { type: 'string' } }, description: 'Map of surface id to observed tools/list names.' },
          include_ok: { type: 'boolean', description: 'Include passing surfaces in the output.' },
        },
        additionalProperties: false,
      },
      annotations: { title: 'registrar_surface_tool_inventory_check', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_site_registry_conformance_check',
      description: 'Prove materialized site MCP registry conformance from an immutable Loader observation ref across live tools/list, site fabric, registrar catalog, and admission classification.',
      inputSchema: {
        type: 'object',
        properties: {
          site_id: { type: 'string', description: 'Known site identifier, e.g. smart-scheduling.' },
          observation_ref: { type: 'string', description: 'Immutable mcp_payload ref returned by mcp_loader_site_tool_inventory_check for this Site.' },
          include_ok: { type: 'boolean', description: 'Include passing per-surface findings.' },
        },
        required: ['site_id', 'observation_ref'],
        additionalProperties: false,
      },
      annotations: { title: 'registrar_site_registry_conformance_check', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'registrar_site_output_reader_closure_check',
      description: 'Check materialized site MCP surface registries for output-ref producer tools whose required reader tools are missing from live or read-only admission metadata.',
      inputSchema: {
        type: 'object',
        properties: {
          site_id: { type: 'string', description: 'Single known site identifier to inspect.' },
          site_ids: { type: 'array', items: { type: 'string' }, description: 'Known site identifiers to inspect.' },
          site_root: { type: 'string', description: 'Single explicit site root to inspect.' },
          site_roots: { type: 'array', items: { type: 'string' }, description: 'Explicit site roots to inspect.' },
          include_ok: { type: 'boolean', description: 'Include passing site summaries in output.' },
        },
        additionalProperties: false,
      },
      annotations: { title: 'registrar_site_output_reader_closure_check', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
  ];
}

export function registrarSurfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'mcp-registrar',
    surface_version: SERVER_VERSION,
    package: '@narada-core/mcp-registrar',
    entrypoint: MCP_REGISTRAR_ENTRYPOINT,
    tools: listTools() as McpToolDefinition[],
    read_only_tools: listTools()
      .filter((tool) => tool.annotations?.readOnlyHint === true)
      .map((tool) => tool.name),
    default_effect: 'local_write',
    projections: [{
      id: 'default',
      transport: {
        kind: 'stdio',
        command: 'node',
        args: [],
        env: ['NARADA_SRC_ROOT'],
      },
      injection_scope: 'user_site',
      default_injection: 'enabled',
      runtime_requirements: [],
      authority_requirements: ['scope.user_site'],
      lifecycle: {
        mode: 'replayable',
        reason: 'Registrar mutations are persisted config operations and the registrar process owns no client session.',
      },
    }],
  });
}

async function callTool(params: JsonRecord, _state: RegistrarState) {
  const name = String(params.name ?? '');
  const args = asRecord(params.arguments);
  let result: JsonRecord;
  switch (name) {
    case 'registrar_guidance':
      result = buildGuidanceResult(args);
      break;
    case 'registrar_surface_list': result = registrarSurfaceList(args); break;
    case 'registrar_site_list': result = registrarSiteList(args); break;
    case 'registrar_site_surfaces': result = registrarSiteSurfaces(args); break;
    case 'registrar_site_bind': result = registrarSiteBind(args); break;
    case 'registrar_site_unbind': result = registrarSiteUnbind(args); break;
    case 'registrar_carrier_list': result = registrarCarrierList(args); break;
    case 'registrar_carrier_bind': result = await registrarCarrierBind(args); break;
    case 'registrar_carrier_unbind': result = await registrarCarrierUnbind(args); break;
    case 'registrar_sync': result = await registrarSync(args); break;
    case 'registrar_materialize_all': result = await registrarMaterializeAll(args); break;
    case 'registrar_carrier_validate': result = registrarCarrierValidate(args); break;
    case 'registrar_carrier_diff': result = registrarCarrierDiff(args); break;
    case 'registrar_surface_usage': result = registrarSurfaceUsage(args); break;
    case 'registrar_site_mcp_fabric_validate': result = registrarSiteMcpFabricValidate(args); break;
    case 'registrar_site_surface_registry_sync': result = registrarSiteSurfaceRegistrySync(args); break;
    case 'registrar_surface_tool_inventory_check': result = registrarSurfaceToolInventoryCheck(args); break;
    case 'registrar_site_registry_conformance_check': result = registrarSiteRegistryConformanceCheck(args); break;
    case 'registrar_site_output_reader_closure_check': result = registrarSiteOutputReaderClosureCheck(args); break;
    default: throw diagnosticError('unknown_tool', `unknown_tool:${name}`, { tool_name: name });
  }
  return { content: [{ type: 'text', text: renderResult(result) }], structuredContent: result };
}

function lookupSurface(surfaceId: string): RegistrarSurfaceRecord {
  const surface = SURFACES.find((s) => s.id === surfaceId);
  if (!surface) throw diagnosticError('registrar_unknown_surface', `registrar_unknown_surface:${surfaceId}`, { known: SURFACES.map((s) => s.id) });
  return surface;
}

function lookupSite(siteId: string): SiteDef {
  assertCanonicalSiteId(siteId);
  const catalog = readSiteRegistryCatalog();
  const candidates = catalog.status === 'ready' ? catalog.items : KNOWN_SITES;
  const direct = candidates.find((site) => site.site_id === siteId);
  if (direct) return direct;
  throw diagnosticError('registrar_unknown_site', `registrar_unknown_site:${siteId}`, {
    known: candidates.map((site) => site.site_id),
  });
}

function lookupCarrier(carrierId: string): CarrierDef {
  const carrier = CARRIERS.find((c) => c.carrier_id === carrierId);
  if (!carrier) throw diagnosticError('registrar_unknown_carrier', `registrar_unknown_carrier:${carrierId}`, { known: CARRIERS.map((c) => c.carrier_id) });
  return carrier;
}

function siteCatalogForOperations(): SiteDef[] {
  const catalog = readSiteRegistryCatalog();
  return catalog.status === 'ready' ? catalog.items : KNOWN_SITES;
}

type SitePathInterpolation = {
  siteRoot: string;
  siteControlRoot: string;
  siteRuntimeRoot: string;
  workspaceRoot: string;
};

function sitePathInterpolation(siteRoot: string, workspaceRoot = siteRoot): SitePathInterpolation {
  const canonicalRoot = canonicalWorkspaceRoot(siteRoot);
  const siteControlRoot = basename(resolve(siteRoot)).toLowerCase() === '.narada'
    ? resolve(siteRoot)
    : join(canonicalRoot, '.narada');
  return {
    siteRoot: canonicalRoot,
    siteControlRoot,
    siteRuntimeRoot: join(siteControlRoot, 'runtime'),
    workspaceRoot: canonicalWorkspaceRoot(workspaceRoot),
  };
}

function interpolateArgs(args: string[], siteId: string, siteRoot: string): string[] {
  const paths = sitePathInterpolation(siteRoot);
  return args.map((a) => interpolateArg(a, siteId, paths));
}

function interpolateArg(value: string, siteId: string, paths: SitePathInterpolation | string): string {
  const resolvedPaths = typeof paths === 'string' ? sitePathInterpolation(paths) : paths;
  return value
    .replace(/\{mcp_surfaces_root\}/g, MCP_SURFACES_ROOT)
    .replace(/\{site_root\}/g, resolvedPaths.siteRoot)
    .replace(/\{site_control_root\}/g, resolvedPaths.siteControlRoot)
    .replace(/\{site_runtime_root\}/g, resolvedPaths.siteRuntimeRoot)
    .replace(/\{workspace_root\}/g, resolvedPaths.workspaceRoot)
    .replace(/\{site_id\}/g, siteId);
}

function appendSopsDirs(args: string[]): string[] {
  for (const def of SURFACES) {
    if (def.sops_dir) {
      args.push('--sops-dir', def.sops_dir);
    }
  }
  return args;
}

function stripJsoncComments(text: string): string {
  return text
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^\\:])\/\/.*$/gm, '$1')
    .replace(/^\s*\/\/.*$/gm, '');
}

function readJsonFile(path: string): JsonRecord | null {
  try {
    return asRecord(JSON.parse(stripJsoncComments(readFileSync(path, 'utf8'))));
  } catch {
    return null;
  }
}

function readSiteConfig(site: SiteDef): SiteLocalSurface[] {
  const configPath = site.config_path || join(site.root, 'config.json');
  if (!existsSync(configPath)) return [];
  try {
    const content = stripJsoncComments(readFileSync(configPath, 'utf8'));
    const cfg = asRecord(JSON.parse(content));
    const structural = asRecord(cfg.structural_config);
    const policy = asRecord(structural.agent_execution_policy);
    const entrypoints = policy.allowed_mcp_entrypoints as unknown[] | undefined;
    if (!Array.isArray(entrypoints)) return [];
    const surfaces: SiteLocalSurface[] = [];
    for (const ep of entrypoints) {
      const rec = asRecord(ep);
      const surfaceId = String(rec.surface_id ?? '');
      if (!surfaceId) continue;
      if (site.local_surface_allowlist && !site.local_surface_allowlist.includes(surfaceId)) continue;
      const catalogSurface = catalogSurfaceForLocalSurface(site, surfaceId);
      if (catalogSurface && !surfaceProjections(catalogSurface).some((projection) => projection.injection_scope === 'local_site')) continue;
      surfaces.push({
        surface_id: surfaceId,
        kind: 'mcp_entrypoint',
        command: String(rec.command ?? 'node'),
        path: String(rec.path ?? ''),
        canonical_tool_prefix: rec.canonical_tool_prefix ? String(rec.canonical_tool_prefix) : undefined,
        replaces: rec.replaces ? String(rec.replaces) : undefined,
      });
    }
    return surfaces;
  } catch {
    return [];
  }
}

function catalogSurfaceForLocalSurface(site: SiteDef, localSurfaceId: string): RegistrarSurfaceRecord | undefined {
  const serverKey = localSurfaceId.replace(/\.local$/, '').replace(/-mcp$/, '');
  const canonicalSurfaceId = fabricSurfaceId(serverKey, site);
  return catalogSurface(canonicalSurfaceId) ?? catalogSurfaceAlias(canonicalSurfaceId);
}

function resolveEntrypoint(surface: RegistrarSurfaceRecord, siteId: string, siteRoot: string, projection?: McpSurfaceProjection): string {
  const interpolated = interpolateArg(projection?.entrypoint ?? surface.entrypoint, siteId, siteRoot);
  return resolve(interpolated);
}

function catalogSurface(surfaceId: string): RegistrarSurfaceRecord | undefined {
  return SURFACES.find((surface) => surface.id === surfaceId);
}

export function nativeSurfaceDescriptor(surfaceId: string): SurfaceDescriptorV2 {
  if (surfaceId === 'mcp-registrar') return registrarSurfaceDefinition().descriptor;
  const native = NATIVE_SURFACE_DEFINITIONS[surfaceId];
  if (!native) {
    throw diagnosticError('registrar_native_descriptor_missing', 'registrar_native_descriptor_missing:' + surfaceId, {
      surface_id: surfaceId,
      known: Object.keys(NATIVE_SURFACE_DEFINITIONS).sort(),
    });
  }
  return native.descriptor;
}
function nativeToolNames(surfaceId: string): string[] {
  return nativeSurfaceDescriptor(surfaceId).tools.map((tool) => tool.name);
}
function surfaceProjections(surface: RegistrarSurfaceRecord): McpSurfaceProjection[] {
  return nativeSurfaceDescriptor(surface.id).projections.map((projection) => ({
    id: projection.id,
    injection_scope: projection.injection_scope,
    execution: surfaceExecutionDeclaration(projection.execution),
    default_injection: projection.default_injection === 'enabled'
      ? projection.injection_scope === 'host' ? 'all_carrier_sessions' : 'all_site_bound_sessions'
      : projection.runtime_requirements.length > 0 ? 'runtime_selected_sessions' : undefined,
    restart_owner: projection.lifecycle.restart_owner && (
      projection.lifecycle.restart_owner === 'host'
      || projection.lifecycle.restart_owner === 'user_site'
      || projection.lifecycle.restart_owner === 'local_site'
    ) ? projection.lifecycle.restart_owner : projection.injection_scope,
    runtime_requirements: projection.runtime_requirements.filter((value): value is McpRuntimeKind => value === 'nars'),
    env_vars: projection.transport.kind === 'stdio' ? projection.transport.env : [],
    ...(projection.transport.kind === 'stdio'
      ? {
        command: projection.transport.command,
        entrypoint: nativeEntrypoint(projection.transport.args[0] ?? ''),
        args: projection.transport.args.slice(1),
      }
      : {}),
  }));
}

function projectionSupportsRuntime(projection: McpSurfaceProjection, runtimeKind: McpRuntimeKind | undefined): boolean {
  const requirements = projection.runtime_requirements ?? [];
  return requirements.length === 0 || (runtimeKind !== undefined && requirements.includes(runtimeKind));
}

function selectSurfaceProjection(
  surfaceId: string,
  projectionId?: string | null,
  runtimeKind?: McpRuntimeKind,
  options: { requireExplicit?: boolean } = {},
): { surface: RegistrarSurfaceRecord; projection: McpSurfaceProjection } {
  const surface = catalogSurface(surfaceId);
  if (!surface) throw diagnosticError('registrar_unknown_surface', `registrar_unknown_surface:${surfaceId}`);
  const projections = surfaceProjections(surface);
  if (projectionId) {
    const projection = projections.find((candidate) => candidate.id === projectionId);
    if (!projection) {
      throw diagnosticError('registrar_unknown_surface_projection', `registrar_unknown_surface_projection:${surfaceId}:${projectionId}`, {
        surface_id: surfaceId,
        projection_id: projectionId,
        known_projection_ids: projections.map((candidate) => candidate.id),
      });
    }
    if (runtimeKind !== undefined && !projectionSupportsRuntime(projection, runtimeKind)) {
      throw diagnosticError('registrar_surface_projection_runtime_mismatch', `registrar_surface_projection_runtime_mismatch:${surfaceId}:${projectionId}:${runtimeKind}`, {
        surface_id: surfaceId,
        projection_id: projectionId,
        runtime_kind: runtimeKind,
        runtime_requirements: projection.runtime_requirements ?? [],
      });
    }
    return { surface, projection };
  }

  if (runtimeKind !== undefined) {
    const runtimeMatches = projections.filter((projection) => (projection.runtime_requirements ?? []).includes(runtimeKind));
    if (runtimeMatches.length === 1) return { surface, projection: runtimeMatches[0] };
    if (runtimeMatches.length > 1) {
      throw diagnosticError('registrar_surface_projection_ambiguous_runtime', `registrar_surface_projection_ambiguous_runtime:${surfaceId}:${runtimeKind}`, {
        surface_id: surfaceId,
        runtime_kind: runtimeKind,
        projection_ids: runtimeMatches.map((projection) => projection.id),
      });
    }
    const neutralMatches = projections.filter((projection) => (projection.runtime_requirements ?? []).length === 0);
    if (neutralMatches.length === 1) return { surface, projection: neutralMatches[0] };
    if (neutralMatches.length > 1) {
      throw diagnosticError('registrar_surface_projection_ambiguous_runtime', `registrar_surface_projection_ambiguous_runtime:${surfaceId}:${runtimeKind}`, {
        surface_id: surfaceId,
        runtime_kind: runtimeKind,
        projection_ids: neutralMatches.map((projection) => projection.id),
        reason: 'multiple_runtime_neutral_projections',
      });
    }
  }

  if (!options.requireExplicit) {
    const defaults = projections.filter((projection) => projection.default_injection === 'all_site_bound_sessions' || projection.default_injection === 'all_carrier_sessions');
    if (defaults.length === 1) return { surface, projection: defaults[0] };
    if (projections.length === 1) return { surface, projection: projections[0] };
  }

  throw diagnosticError('registrar_surface_projection_required', `registrar_surface_projection_required:${surfaceId}`, {
    surface_id: surfaceId,
    projection_ids: projections.map((projection) => projection.id),
    runtime_kind: runtimeKind ?? null,
    remediation: 'Select an explicit projection_id or provide a runtime kind that uniquely selects one.',
  });
}

function projectionMetadata(surfaceId: string, projectionId?: string, runtimeKind?: McpRuntimeKind): JsonRecord {
  const { projection } = selectSurfaceProjection(surfaceId, projectionId, runtimeKind);
  const descriptor = nativeSurfaceDescriptor(surfaceId);
  return {
    surface_id: surfaceId,
    projection_id: projection.id,
    injection_scope: projection.injection_scope,
    ...(projection.default_injection ? { default_injection: projection.default_injection } : {}),
    runtime_requirements: projection.runtime_requirements ?? [],
    execution: projection.execution,
    ...(runtimeKind ? { runtime_kind: runtimeKind } : {}),
    descriptor_digest: surfaceDescriptorDigest(descriptor),
    tool_contract_digest: surfaceToolContractDigest(descriptor),
    surface_descriptor: descriptor,
    lifecycle: descriptor.projections.find((candidate) => candidate.id === projection.id)?.lifecycle,
  };
}

function injectionScopeForSurface(surfaceId: string, projectionId?: string): McpInjectionScope {
  if (!catalogSurface(surfaceId)) return 'local_site';
  return selectSurfaceProjection(surfaceId, projectionId).projection.injection_scope;
}

function restartOwnerForSurface(surfaceId: string, injectionScope: McpInjectionScope, projectionId?: string): McpRestartOwner {
  if (!catalogSurface(surfaceId)) return injectionScope;
  return selectSurfaceProjection(surfaceId, projectionId).projection.restart_owner ?? injectionScope;
}

function locusForScope(scope: McpInjectionScope, siteRoot: string): McpAuthorityLocus {
  if (scope === 'host') return { kind: 'host' };
  if (scope === 'user_site') return { kind: 'user_site', site_root: defaultUserNaradaRoot() };
  return { kind: 'local_site', site_root: siteRoot };
}

function surfaceScopeMetadata(surfaceId: string, siteRoot = '{site_root}', projectionId?: string): SurfaceScopeMetadata {
  const injectionScope = injectionScopeForSurface(surfaceId, projectionId);
  const locus = locusForScope(injectionScope, siteRoot);
  return {
    injection_scope: injectionScope,
    authority_locus: locus,
    mutation_locus: locus,
    restart_owner: restartOwnerForSurface(surfaceId, injectionScope, projectionId),
  };
}

function scopeDiagnosticClass(scope: McpInjectionScope): string {
  if (scope === 'host') return 'host_injected_surface_missing_or_misconfigured_in_session';
  if (scope === 'user_site') return 'user_site_injected_surface_missing_or_misconfigured_in_session';
  return 'local_site_surface_missing_or_misconfigured';
}

function validationScopeDetail(surfaceId: string, siteRoot: string): JsonRecord {
  const metadata = surfaceScopeMetadata(surfaceId, siteRoot);
  const naradaScope = naradaScopeMetadata(surfaceId, siteRoot);
  return {
    ...metadata,
    narada_scope: naradaScope,
    diagnostic_class: scopeDiagnosticClass(metadata.injection_scope),
    required_repair_locus: metadata.mutation_locus,
  };
}

function scopeFindingDetail(naradaScope: NaradaScopeMetadata): JsonRecord {
  return {
    ...naradaScope,
    narada_scope: naradaScope,
    diagnostic_class: scopeDiagnosticClass(naradaScope.injection_scope),
    required_repair_locus: naradaScope.mutation_locus,
  };
}

function resolveLocalEntrypoint(local: SiteLocalSurface, siteRoot: string): string {
  const normalized = local.path.replace(/\\/g, '/');
  return resolve(siteRoot, normalized);
}

function rootsNeedingAllowedRoot(surfaceId: string): boolean {
  return ['local-filesystem', 'git', 'structured-command', 'delegated-task', 'worker-delegation'].includes(surfaceId);
}

function resolveSurfaceArgs(surface: RegistrarSurfaceRecord, siteId: string, siteRoot: string, extraRoots: string[], projection?: McpSurfaceProjection): string[] {
  const args = interpolateArgs(projection?.args ?? surface.args, siteId, siteRoot);
  if (extraRoots.length === 0 || !rootsNeedingAllowedRoot(surface.id)) return args;
  const out: string[] = [];
  for (let i = 0; i < args.length; i++) {
    out.push(args[i]);
    if (args[i] === '--allowed-root' && i + 1 < args.length) {
      out.push(args[++i]);
      for (const r of extraRoots) {
        if (!out.includes(r)) {
          out.push('--allowed-root', r);
        }
      }
    }
  }
  return out;
}

function dedupeRoots(roots: string[]): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const r of roots) {
    const normalized = r.replace(/\\/g, '/');
    if (!seen.has(normalized)) {
      seen.add(normalized);
      result.push(normalized);
    }
  }
  return result;
}

export function appendLoaderAllowedSiteRoots(args: string[], siteRoots: string[]): string[] {
  const out = [...args];
  const present = new Set<string>();
  for (let index = 0; index < out.length; index += 1) {
    if (out[index] !== '--allowed-site-root' || index + 1 >= out.length) continue;
    present.add(out[++index]!.replace(/\\/g, '/').toLowerCase());
  }
  for (const root of dedupeRoots(siteRoots.map((value) => canonicalWorkspaceRoot(value)))) {
    const comparable = root.replace(/\\/g, '/').toLowerCase();
    if (present.has(comparable)) continue;
    out.push('--allowed-site-root', root);
    present.add(comparable);
  }
  return out;
}

function writeSiteAllowedRootsConfig(carrier: CarrierDef): void {
  for (const binding of carrier.site_bindings) {
    const site = lookupSite(binding.site_id);
    const siteRoot = canonicalWorkspaceRoot(site.root).replace(/\\/g, '/');
    const siteControlRoot = sitePathInterpolation(siteRoot).siteControlRoot;
    const extraRoots = dedupeRoots([
      ...(carrier.extra_allowed_roots ?? []),
      ...(binding.extra_allowed_roots ?? []),
    ]).filter((r) => r !== siteRoot);

    if (extraRoots.length === 0) continue;

    const naradaDir = siteControlRoot;
    try { mkdirSync(naradaDir, { recursive: true }); } catch { /* existing */ }
    const config = {
      schema: 'narada.site.allowed_roots.v1',
      generated_by: 'mcp-registrar',
      generated_at: new Date().toISOString(),
      site_id: binding.site_id,
      extra_allowed_roots: extraRoots,
    };
    writeFileSync(join(naradaDir, 'allowed-roots.json'), JSON.stringify(config, null, 2) + '\n', 'utf8');
  }
}

function writeSiteSurfaceRegistriesForCarrier(carrier: CarrierDef): JsonRecord[] {
  const seen = new Set<string>();
  const results: JsonRecord[] = [];
  for (const binding of carrier.site_bindings) {
    if (seen.has(binding.site_id)) continue;
    seen.add(binding.site_id);
    results.push(writeSiteSurfaceRegistry(lookupSite(binding.site_id)));
  }
  return results;
}

function materializeSharedSurface(binding: SiteBinding, site: SiteDef, surfaceId: string, extraRoots: string[]): { key: string; server: MaterializedServer } {
  const surface = lookupSurface(surfaceId);
  const selected = selectSurfaceProjection(surfaceId, undefined, binding.runtime_kind);
  const projection = selected.projection;
  const siteRoot = canonicalWorkspaceRoot(site.root);
  let resolvedArgs = [
    ...resolveSurfaceArgs(surface, site.site_id, siteRoot, extraRoots, projection),
    ...projectionLaunchArgs(projection),
  ];
  if (surfaceId === 'mcp-loader') {
    resolvedArgs = appendLoaderAllowedSiteRoots(resolvedArgs, [
      MCP_WORKSPACE_ROOT,
      ...siteCatalogForOperations().map((registeredSite) => registeredSite.root),
    ]);
  }
  const resolvedEntrypoint = resolveEntrypoint(surface, site.site_id, siteRoot, projection);
  if (surfaceId === 'sop') appendSopsDirs(resolvedArgs);
  const serverKey = `${binding.prefix}-${surfaceId}`;
  const naradaScope = naradaScopeMetadata(surfaceId, siteRoot, site.site_id, projection.id);
  return {
    key: serverKey,
    server: {
      kind: 'shared',
      entrypoint: resolvedEntrypoint,
      command: projection.command,
      args: resolvedArgs,
      surface,
      projection,
      env_vars: projectionEnvVars(surface, projection),
      surface_implementation: site.surface_overrides?.[surfaceId]?.surface_implementation,
      ...naradaScope,
      narada_scope: naradaScope,
    },
  };
}

function localSurfaceKey(binding: SiteBinding, local: SiteLocalSurface): string {
  const stripped = local.surface_id.replace(/\.local$/, '');
  if (stripped.endsWith('-mcp')) {
    return `${binding.prefix}-${stripped.replace(/-mcp$/, '')}`;
  }
  return `${binding.prefix}-${stripped}`;
}

function materializeLocalSurface(binding: SiteBinding, site: SiteDef, local: SiteLocalSurface, extraRoots: string[]): { key: string; server: MaterializedServer } {
  const serverKey = localSurfaceKey(binding, local);
  const siteRoot = canonicalWorkspaceRoot(site.root);
  const entrypoint = resolveLocalEntrypoint(local, siteRoot);
  const args = ['--site-root', siteRoot];
  const siteRootIncluded = extraRoots.length > 0;
  if (local.surface_id === 'local-filesystem-mcp.local' && siteRootIncluded) {
    const allRoots = dedupeRoots([siteRoot, ...extraRoots]);
    for (const r of allRoots) {
      if (!args.includes(r)) {
        args.push('--allowed-root', r);
      }
    }
    args.push('--output-root', siteRoot);
  }
  const naradaScope = naradaScopeMetadata(local.surface_id, siteRoot, site.site_id);
  return {
    key: serverKey,
    server: {
      kind: 'local',
      entrypoint,
      command: local.command,
      args,
      local,
      ...naradaScope,
      narada_scope: naradaScope,
    },
  };
}

function automaticProjectionForBinding(surface: RegistrarSurfaceRecord, binding: SiteBinding): McpSurfaceProjection | null {
  const projections = surfaceProjections(surface);
  if (binding.runtime_kind !== undefined) {
    const runtimeMatches = projections.filter((projection) => (projection.runtime_requirements ?? []).includes(binding.runtime_kind as McpRuntimeKind));
    if (runtimeMatches.length === 1) return runtimeMatches[0];
    if (runtimeMatches.length > 1) return null;
  }
  const defaults = projections.filter((projection) => projection.default_injection === 'all_site_bound_sessions' || projection.default_injection === 'all_carrier_sessions');
  return defaults.length === 1 ? defaults[0] : null;
}

function assertCarrierBindingLoadingMode(binding: SiteBinding): void {
  if (binding.loading_mode !== 'progressive') return;
  if (binding.surfaces === 'all') {
    throw diagnosticError(
      'registrar_progressive_binding_requires_explicit_bootstrap',
      `registrar_progressive_binding_requires_explicit_bootstrap:${binding.site_id}`,
      {
        site_id: binding.site_id,
        loading_mode: binding.loading_mode,
        remediation: 'Replace surfaces=all with an explicit progressive bootstrap allowlist; use mcp-loader for all other surfaces.',
      },
    );
  }
  const missing = PROGRESSIVE_BOOTSTRAP_SURFACES.filter((surfaceId) => !binding.surfaces.includes(surfaceId));
  if (missing.length > 0) {
    throw diagnosticError(
      'registrar_progressive_binding_missing_bootstrap_surface',
      `registrar_progressive_binding_missing_bootstrap_surface:${binding.site_id}`,
      {
        site_id: binding.site_id,
        loading_mode: binding.loading_mode,
        required_bootstrap_surfaces: PROGRESSIVE_BOOTSTRAP_SURFACES,
        missing_surfaces: missing,
        remediation: 'Add every required bootstrap surface or switch the binding to static loading.',
      },
    );
  }
}

export function sharedSurfaceIdsForBinding(binding: SiteBinding, site?: SiteDef): string[] {
  assertCarrierBindingLoadingMode(binding);
  const isEnabled = (surfaceId: string) => site?.surface_overrides?.[surfaceId]?.enabled !== false;
  const explicit = binding.surfaces === 'all'
    ? SURFACES.filter((surface) => {
      if (!isEnabled(surface.id)) return false;
      try {
        selectSurfaceProjection(surface.id, undefined, binding.runtime_kind);
        return true;
      } catch {
        return false;
      }
    }).map((surface) => surface.id)
    : binding.surfaces.filter((surfaceId) => !surfaceId.endsWith('.local') && isEnabled(surfaceId));
  if (binding.loading_mode === 'progressive') return Array.from(new Set(explicit));
  const ids = new Set(explicit);
  for (const surface of SURFACES) {
    if (isEnabled(surface.id) && automaticProjectionForBinding(surface, binding)) ids.add(surface.id);
  }
  return Array.from(ids);
}

function collectCarrierServers(carrier: CarrierDef): Record<string, MaterializedServer> {
  const servers: Record<string, MaterializedServer> = {};
  for (const binding of carrier.site_bindings) {
    const site = lookupSite(binding.site_id);
    const siteRoot = canonicalWorkspaceRoot(site.root);
    const extraRoots = dedupeRoots([siteRoot, ...(carrier.extra_allowed_roots ?? []), ...(binding.extra_allowed_roots ?? [])]);
    const sharedSurfaceIds = sharedSurfaceIdsForBinding(binding, site);
    for (const surfaceId of sharedSurfaceIds) {
      const { key, server } = materializeSharedSurface(binding, site, surfaceId, extraRoots);
      if (servers[key]) {
        console.warn(`mcp-registrar: duplicate server key '${key}' from shared surface '${surfaceId}' overwrites previous`);
      }
      servers[key] = server;
    }
    if (binding.surfaces === 'all' || binding.surfaces.some((s) => s.endsWith('.local'))) {
      for (const local of readSiteConfig(site)) {
        if (binding.surfaces !== 'all' && !binding.surfaces.includes(local.surface_id)) continue;
        const { key, server } = materializeLocalSurface(binding, site, local, extraRoots);
        if (servers[key]) {
          console.warn(`mcp-registrar: duplicate server key '${key}' from local surface '${local.surface_id}' overwrites shared/local predecessor`);
        }
        servers[key] = server;
      }
    }
  }
  return servers;
}

function carrierServerKeysForSurface(carrier: CarrierDef, surfaceId: string): string[] {
  return Object.entries(collectCarrierServers(carrier))
    .filter(([, server]) => {
      const serverSurfaceId = server.kind === 'local' ? (server.local as SiteLocalSurface).surface_id : (server.surface as RegistrarSurfaceRecord).id;
      return serverSurfaceId === surfaceId;
    })
    .map(([key]) => key);
}

function carrierInjectionSummary(carrier: CarrierDef): JsonRecord {
  const counts: Record<McpInjectionScope, number> = { host: 0, user_site: 0, local_site: 0 };
  const bindings = carrier.site_bindings.map((binding) => ({
    site_id: binding.site_id,
    loading_mode: binding.loading_mode ?? 'static',
    bootstrap_surface_ids: binding.surfaces === 'all'
      ? 'all'
      : [...binding.surfaces],
  }));
  const servers = Object.entries(collectCarrierServers(carrier)).map(([serverKey, server]) => {
    const surfaceId = server.kind === 'local' ? (server.local as SiteLocalSurface).surface_id : (server.surface as RegistrarSurfaceRecord).id;
    counts[server.injection_scope]++;
    return {
      server_key: serverKey,
      surface_id: surfaceId,
      projection_id: server.kind === 'shared' ? (server.projection as McpSurfaceProjection | undefined)?.id ?? null : null,
      runtime_requirements: server.kind === 'shared' ? (server.projection as McpSurfaceProjection | undefined)?.runtime_requirements ?? [] : [],
      injection_scope: server.injection_scope,
      authority_locus: server.authority_locus,
      restart_owner: server.restart_owner,
      narada_scope: server.narada_scope,
    };
  });
  return { counts, servers, bindings };
}

function applySurfaceOverrides(carrier: CarrierDef, server: MaterializedServer, surfaceId: string): MaterializedServer {
  const overrides = carrier.surface_overrides?.[surfaceId];
  if (!overrides) return server;
  return {
    ...server,
    entrypoint: overrides.entrypoint ?? server.entrypoint,
    args: overrides.args ?? server.args,
    env_vars: overrides.env_vars ?? server.env_vars,
    surface_implementation: overrides.surface_implementation ?? server.surface_implementation,
    enabled: overrides.enabled ?? server.enabled,
  };
}

type CarrierLaunchCommand = {
  command: string;
  args: string[];
  uses_runtime_proxy: boolean;
  runtime_proxy_entrypoint?: string;
  runtime_proxy_implementation?: RuntimeProxyImplementation;
  runtime_profile_kind?: RuntimeProfileKind;
  runtime_engine_kind?: RuntimeEngineKind;
  component_kind?: string;
  artifact_manifest_path?: string;
  runtime_contract_version?: number;
  materialization_sidecar_path?: string;
  child_invocation_kind?: 'entrypoint' | 'native_applet' | 'native_entrypoint';
  child_applet?: string;
  child_entrypoint: string;
  child_args: string[];
};

function carrierLaunchCommand(
  server: MaterializedServer,
  surfaceId: string,
  configPath?: string,
  carrier?: Pick<CarrierDef, 'carrier_id' | 'kind'>,
  plan: RuntimeMaterializationPlan = runtimeMaterializationPlan,
  proxyImplementationOverride?: RuntimeProxyImplementation,
): CarrierLaunchCommand {
  assertRuntimeMaterializationPlanCurrent(plan);
  const profile = String(plan.runtime_profile_kind) as RuntimeProfileKind;
  const childEntrypoint = server.entrypoint;
  const childArgs = server.args;
  const componentKind = componentKindForSurface(surfaceId);
  const selectedEngine = selectedSurfaceRuntimeEngine(surfaceId, server.surface_implementation, plan);
  const runtimeCommand = selectedEngine === 'rust'
    ? (server.command ?? server.projection?.command ?? 'node')
    : javascriptRuntimeCommand(selectedEngine);
  const useNativeLoader = selectedEngine === 'rust' && componentKind === 'mcp-loader-mcp';
  const useNativeFilesystemApplet = (selectedEngine === 'rust' && componentKind === 'filesystem-mcp')
    || (server.surface_implementation === 'native'
      && surfaceId === 'local-filesystem');
  const useNativeStructuredCommandApplet = selectedEngine === 'rust' && componentKind === 'structured-command-mcp';
  const useNativeGitApplet = selectedEngine === 'rust' && componentKind === 'git-mcp';
  const useNativeLifecycle = selectedEngine === 'rust' && (componentKind === 'task-lifecycle-mcp' || componentKind === 'work-lifecycle-mcp');
  const useNativeSharedSurface = selectedEngine === 'rust' && (surfaceId === 'catalog-observation' || surfaceId === 'operator-routing' || surfaceId === 'site-inbox' || surfaceId === 'site-lifecycle' || surfaceId === 'site-registry' || surfaceId === 'project-state' || surfaceId === 'runtime-introspection' || surfaceId === 'site-coherence' || surfaceId === 'launcher' || surfaceId === 'mailbox' || surfaceId === 'graph-mail' || surfaceId === 'calendar' || surfaceId === 'site-loop' || surfaceId === 'worker-delegation' || surfaceId === 'delegated-task' || surfaceId === 'sop' || surfaceId === 'scheduler' || surfaceId === 'surface-feedback' || surfaceId === 'speech' || surfaceId === 'artifacts' || surfaceId === 'nars-session' || surfaceId === 'quota-meter' || surfaceId === 'operator-console-overlay' || surfaceId === 'browser-control' || surfaceId === 'cloudflare-carrier');
  const nativeApplet = useNativeFilesystemApplet ? 'filesystem' : useNativeStructuredCommandApplet ? 'structured-command' : useNativeGitApplet ? 'git' : null;
  const nativeSharedSurfaceEntrypoint = MCP_NATIVE_SHARED_SURFACES_ENTRYPOINT;
  const nativeLifecycleEntrypoint = componentKind === 'task-lifecycle-mcp' ? MCP_NATIVE_TASK_LIFECYCLE_ENTRYPOINT : MCP_NATIVE_WORK_LIFECYCLE_ENTRYPOINT;
  if (useNativeLifecycle && !existsSync(nativeLifecycleEntrypoint)) {
    throw diagnosticError(
      'registrar_native_lifecycle_missing',
      `Native lifecycle surface is unavailable: ${nativeLifecycleEntrypoint}`,
      { entrypoint: nativeLifecycleEntrypoint, surface_id: surfaceId, component_kind: componentKind },
    );
  }
  if (useNativeSharedSurface && !existsSync(nativeSharedSurfaceEntrypoint)) {
    throw diagnosticError(
      'registrar_native_shared_surface_missing',
      'Native shared surface is unavailable: ' + nativeSharedSurfaceEntrypoint,
      { entrypoint: nativeSharedSurfaceEntrypoint, surface_id: surfaceId, component_kind: componentKind },
    );
  }
  if (useNativeLoader && !existsSync(MCP_NATIVE_MCP_LOADER_ENTRYPOINT)) {
    throw diagnosticError(
      'registrar_native_mcp_loader_missing',
      `Native mcp-loader is unavailable: ${MCP_NATIVE_MCP_LOADER_ENTRYPOINT}`,
      { entrypoint: MCP_NATIVE_MCP_LOADER_ENTRYPOINT, surface_id: surfaceId, component_kind: componentKind },
    );
  }
  if ((nativeApplet || useNativeLoader || useNativeLifecycle || useNativeSharedSurface) && !nativeRuntimeProxyAvailable()) {
    throw diagnosticError(
      'registrar_native_runtime_proxy_missing',
      `Native runtime proxy is unavailable: ${nativeRuntimeProxyEntrypoint()}`,
      { entrypoint: nativeRuntimeProxyEntrypoint(), surface_id: surfaceId, component_kind: componentKind },
    );
  }
  const effectiveChildCommand = useNativeLoader ? MCP_NATIVE_MCP_LOADER_ENTRYPOINT : nativeApplet ? nativeRuntimeProxyEntrypoint() : useNativeLifecycle ? nativeLifecycleEntrypoint : useNativeSharedSurface ? nativeSharedSurfaceEntrypoint : runtimeCommand;
  const effectiveChildEntrypoint = useNativeLoader ? MCP_NATIVE_MCP_LOADER_ENTRYPOINT : nativeApplet ? nativeRuntimeProxyEntrypoint() : useNativeLifecycle ? nativeLifecycleEntrypoint : useNativeSharedSurface ? nativeSharedSurfaceEntrypoint : childEntrypoint;
  const sidecarPath = configPath ? materializationSidecarPath(configPath) : null;
  const effectiveChildArgs = useNativeSharedSurface ? ['--surface-id', surfaceId, ...childArgs] : childArgs;
  const childInvocationKind = useNativeLoader || useNativeLifecycle || useNativeSharedSurface ? 'native_entrypoint' : nativeApplet ? 'native_applet' : null;
  if (server.kind === 'local' && !useNativeLoader && !nativeApplet && !useNativeLifecycle && !useNativeSharedSurface) {
    return {
      command: runtimeCommand,
      args: [childEntrypoint, ...childArgs],
      uses_runtime_proxy: false,
      runtime_profile_kind: profile,
      runtime_engine_kind: selectedEngine,
      component_kind: componentKind,
      child_entrypoint: childEntrypoint,
      child_args: childArgs,
    };
  }
  const proxyImplementation = proxyImplementationOverride ?? runtimeProxyImplementationForResolvedPlan(plan);
  const proxyEntrypoint = selectedRuntimeProxyEntrypoint(proxyImplementation);
  const nativeProxy = proxyImplementation === 'native';
  const registrarEngine = selectedSurfaceRuntimeEngine('mcp-registrar', undefined, plan);
  return {
    command: nativeProxy ? proxyEntrypoint : javascriptRuntimeCommand(proxyImplementation),
    args: [
      ...(nativeProxy ? ['proxy'] : [proxyEntrypoint]),
      '--surface-id',
      surfaceId,
      ...(configPath && carrier ? [
        '--carrier-id',
        carrier.carrier_id,
        '--carrier-kind',
        carrier.kind,
        '--registrar-command',
        javascriptRuntimeCommand(registrarEngine),
        '--registrar-entrypoint',
        MCP_REGISTRAR_RUNTIME_ENTRYPOINT,
      ] : []),
      '--child-command',
      effectiveChildCommand,
      '--artifact-manifest',
      MCP_WORKSPACE_ARTIFACT_MANIFEST,
      '--runtime-contract-version',
      String(MCP_RUNTIME_CONTRACT_VERSION),
      ...(sidecarPath ? ['--materialization-sidecar', sidecarPath] : []),
      '--entrypoint',
      effectiveChildEntrypoint,
      ...(childInvocationKind === 'native_applet' ? ['--child-invocation-kind', 'native_applet', '--child-applet', nativeApplet ?? 'filesystem'] : childInvocationKind === 'native_entrypoint' ? ['--child-invocation-kind', 'native_entrypoint'] : []),
      '--',
      ...effectiveChildArgs,
    ],
    uses_runtime_proxy: true,
    runtime_proxy_entrypoint: proxyEntrypoint,
    runtime_proxy_implementation: proxyImplementation,
    runtime_profile_kind: profile,
    runtime_engine_kind: selectedEngine,
    component_kind: componentKind,
    artifact_manifest_path: MCP_WORKSPACE_ARTIFACT_MANIFEST,
    runtime_contract_version: MCP_RUNTIME_CONTRACT_VERSION,
    ...(sidecarPath ? { materialization_sidecar_path: sidecarPath } : {}),
    ...(childInvocationKind === 'native_applet' ? { child_invocation_kind: 'native_applet' as const, child_applet: nativeApplet ?? 'filesystem' } : childInvocationKind === 'native_entrypoint' ? { child_invocation_kind: 'native_entrypoint' as const } : {}),
    child_entrypoint: effectiveChildEntrypoint,
    child_args: effectiveChildArgs,
  };
}

type RuntimeMaterializationPlanServer = {
  server_key: string;
  surface_id: string;
  component_kind: string;
  runtime_profile_kind: RuntimeProfileKind;
  runtime_engine_kind: RuntimeEngineKind;
  uses_runtime_proxy: boolean;
  runtime_proxy_implementation?: RuntimeProxyImplementation;
  child_invocation_kind?: 'entrypoint' | 'native_applet' | 'native_entrypoint';
  child_applet?: string;
  launch: { command: string; args: string[] };
};

type CarrierRuntimeMaterializationCompilation = {
  plan: JsonRecord;
  launches: Map<string, CarrierLaunchCommand>;
  runtimeProxyImplementation: RuntimeProxyImplementation;
  recoveryEscapeHatch: boolean;
};

function compileCarrierRuntimeMaterializationPlan(
  carrier: CarrierDef,
  outputPath: string,
  proxyImplementationOverride?: RuntimeProxyImplementation,
  recoveryEscapeHatch = false,
): CarrierRuntimeMaterializationCompilation {
  const resolvedPlan = runtimeMaterializationPlan;
  assertRuntimeMaterializationPlanCurrent(resolvedPlan);
  const matrixProxyImplementation = runtimeProxyImplementationForResolvedPlan(resolvedPlan);
  const proxyImplementation = proxyImplementationOverride ?? matrixProxyImplementation;
  if (proxyImplementation !== matrixProxyImplementation && !recoveryEscapeHatch) {
    throw diagnosticError(
      'registrar_runtime_proxy_override_requires_recovery_escape_hatch',
      'A runtime proxy override is admitted only for an explicitly marked recovery materialization.',
      { matrix_proxy_implementation: matrixProxyImplementation, requested_proxy_implementation: proxyImplementation },
    );
  }
  const launches = new Map<string, CarrierLaunchCommand>();
  const servers: RuntimeMaterializationPlanServer[] = Object.entries(collectCarrierServers(carrier)).map(([serverKey, server]) => {
    const surfaceId = server.kind === 'local'
      ? (server.local as SiteLocalSurface).surface_id
      : (server.surface as RegistrarSurfaceRecord).id;
    const overridden = applySurfaceOverrides(carrier, server, surfaceId);
    const launch = carrierLaunchCommand(overridden, surfaceId, outputPath, carrier, resolvedPlan, proxyImplementation);
    launches.set(serverKey, launch);
    return {
      server_key: serverKey,
      surface_id: surfaceId,
      component_kind: launch.component_kind ?? componentKindForSurface(surfaceId),
      runtime_profile_kind: launch.runtime_profile_kind ?? String(resolvedPlan.runtime_profile_kind) as RuntimeProfileKind,
      runtime_engine_kind: launch.runtime_engine_kind ?? selectedSurfaceRuntimeEngine(surfaceId, overridden.surface_implementation, resolvedPlan),
      uses_runtime_proxy: launch.uses_runtime_proxy,
      ...(launch.runtime_proxy_implementation ? { runtime_proxy_implementation: launch.runtime_proxy_implementation } : {}),
      ...(launch.child_invocation_kind ? { child_invocation_kind: launch.child_invocation_kind } : {}),
      ...(launch.child_applet ? { child_applet: launch.child_applet } : {}),
      launch: { command: launch.command, args: [...launch.args] },
    };
  });
  const profilePlanFingerprint = resolvedPlan.plan_fingerprint;
  if (typeof profilePlanFingerprint !== 'string') {
    throw diagnosticError('registrar_runtime_materialization_plan_fingerprint_missing', 'The resolved runtime materialization plan has no fingerprint.', { runtime_profile_kind: resolvedPlan.runtime_profile_kind });
  }
  const unsignedPlan: JsonRecord = {
    schema: 'narada.runtime_materialization_plan.v1',
    status: 'accepted',
    runtime_profile_kind: resolvedPlan.runtime_profile_kind,
    runtime_engine_kind: resolvedPlan.runtime_engine_kind,
    source: resolvedPlan.source,
    profile_plan_fingerprint: profilePlanFingerprint,
    matrix_entries: resolvedPlan.entries,
    carrier: {
      carrier_id: carrier.carrier_id,
      carrier_kind: carrier.kind,
      config_path: resolve(outputPath),
    },
    servers,
    runtime_proxy_implementation: proxyImplementation,
    ...(recoveryEscapeHatch ? { recovery_escape_hatch: true } : {}),
    ...(proxyImplementation !== matrixProxyImplementation ? { runtime_proxy_implementation_override: true } : {}),
  };
  return {
    launches,
    runtimeProxyImplementation: proxyImplementation,
    recoveryEscapeHatch,
    plan: {
      ...unsignedPlan,
      plan_fingerprint: runtimeMaterializationPlanFingerprint(unsignedPlan),
    },
  };
}
function buildRecoveryCarrierRuntimeMaterializationPlan(
  carrier: CarrierDef,
  configPath: string,
  operation: 'bind' | 'unbind',
  surfaceId: string,
  runtimePlan: RuntimeMaterializationPlan = runtimeMaterializationPlan,
  proxyImplementation: RuntimeProxyImplementation = runtimeProxyImplementationForResolvedPlan(runtimePlan),
): JsonRecord {
  assertRuntimeMaterializationPlanCurrent(runtimePlan);
  const profilePlanFingerprint = runtimePlan.plan_fingerprint;
  if (typeof profilePlanFingerprint !== 'string') {
    throw diagnosticError(
      'registrar_runtime_materialization_plan_fingerprint_missing',
      'The resolved runtime materialization plan has no fingerprint.',
      { runtime_profile_kind: runtimePlan.runtime_profile_kind },
    );
  }
  const unsignedPlan: JsonRecord = {
    schema: 'narada.runtime_materialization_plan.v1',
    status: 'accepted',
    runtime_profile_kind: runtimePlan.runtime_profile_kind,
    runtime_engine_kind: runtimePlan.runtime_engine_kind,
    source: runtimePlan.source,
    profile_plan_fingerprint: profilePlanFingerprint,
    matrix_entries: runtimePlan.entries,
    carrier: {
      carrier_id: carrier.carrier_id,
      carrier_kind: carrier.kind,
      config_path: resolve(configPath),
    },
    recovery_escape_hatch: true,
    recovery_scope: { operation, surface_id: surfaceId },
    runtime_proxy_implementation: proxyImplementation,
    servers: [],
  };
  return {
    ...unsignedPlan,
    plan_fingerprint: runtimeMaterializationPlanFingerprint(unsignedPlan),
  };
}
function writeRuntimeMaterializationPlan(path: string, plan: JsonRecord): void {
  writeFileAtomic(path, JSON.stringify(plan, null, 2) + '\n');
}


type RuntimeDependencyCheck = {
  dependency: string;
  package_root: string;
  export_path: string;
  exists: boolean;
};

function runtimeExportTargetExists(exportPath: string): boolean {
  const wildcardIndex = exportPath.search(/[*?]/);
  if (wildcardIndex < 0) return existsSync(exportPath);

  // Package export patterns such as ./dist/schema/* name a family of
  // runtime artifacts, not a literal filesystem entry containing '*'.
  const staticPrefix = exportPath.slice(0, wildcardIndex);
  const directory = staticPrefix.endsWith('/') || staticPrefix.endsWith('\\')
    ? staticPrefix.slice(0, -1)
    : dirname(staticPrefix);
  if (!existsSync(directory)) return false;
  try {
    return readdirSync(directory, { withFileTypes: true }).some((entry) => entry.isFile() || entry.isDirectory());
  } catch {
    return false;
  }
}

function dependencyPackageRoot(dependency: string): string {
  const packageName = dependency.replace('@narada-core/', '');
  const sharedRoot = `${MCP_SURFACES_ROOT}/shared/${packageName}`;
  if (existsSync(`${sharedRoot}/package.json`)) return sharedRoot;
  return `${MCP_SURFACES_ROOT}/${packageName}`;
}

function sharedRuntimeDependencyChecks(surface: RegistrarSurfaceRecord): RuntimeDependencyCheck[] {
  const packageRoot = `${MCP_SURFACES_ROOT}/${surface.package}`;
  const packagePath = `${packageRoot}/package.json`;
  if (!existsSync(packagePath)) return [];
  let packageJson: JsonRecord;
  try {
    packageJson = JSON.parse(readFileSync(packagePath, 'utf8')) as JsonRecord;
  } catch {
    return [];
  }
  const dependencies = asRecord(packageJson.dependencies);
  const checks: RuntimeDependencyCheck[] = [];
  for (const dependency of Object.keys(dependencies).filter((name) => name.startsWith('@narada-core/mcp-'))) {
    const dependencyRoot = dependencyPackageRoot(dependency);
    const dependencyPackagePath = `${dependencyRoot}/package.json`;
    if (!existsSync(dependencyPackagePath)) {
      checks.push({ dependency, package_root: dependencyRoot, export_path: dependencyPackagePath, exists: false });
      continue;
    }
    let dependencyPackageJson: JsonRecord;
    try {
      dependencyPackageJson = JSON.parse(readFileSync(dependencyPackagePath, 'utf8')) as JsonRecord;
    } catch {
      checks.push({ dependency, package_root: dependencyRoot, export_path: dependencyPackagePath, exists: false });
      continue;
    }
    for (const exportTarget of packageExportRuntimeTargets(dependencyPackageJson)) {
      const exportPath = `${dependencyRoot}/${exportTarget.replace(/^\.\//, '')}`;
      checks.push({ dependency, package_root: dependencyRoot, export_path: exportPath, exists: runtimeExportTargetExists(exportPath) });
    }
  }
  return checks;
}

function packageExportRuntimeTargets(packageJson: JsonRecord): string[] {
  const exportsValue = packageJson.exports;
  if (typeof exportsValue === 'string') return [exportsValue];
  const exportsRecord = asRecord(exportsValue);
  const targets: string[] = [];
  for (const value of Object.values(exportsRecord)) {
    if (typeof value === 'string') targets.push(value);
    else {
      const record = asRecord(value);
      if (typeof record.default === 'string') targets.push(record.default);
    }
  }
  return Array.from(new Set(targets));
}

function addRuntimePreflightFindings(
  add: (severity: ValidationFinding['severity'], code: string, message: string, detail?: JsonRecord) => void,
  includeOk: boolean,
  detail: JsonRecord,
  surface: RegistrarSurfaceRecord | null,
  usesRuntimeProxy: boolean,
): void {
  if (usesRuntimeProxy) {
    if (!existsSync(MCP_WORKSPACE_ARTIFACT_MANIFEST)) {
      add('error', 'registrar_workspace_artifact_manifest_missing', `Workspace artifact manifest does not exist: ${MCP_WORKSPACE_ARTIFACT_MANIFEST}`, {
        ...detail,
        artifact_manifest_path: MCP_WORKSPACE_ARTIFACT_MANIFEST,
        remediation: 'Run pnpm build from mcp-surfaces before launching carrier MCPs.',
      });
    } else if (includeOk) {
      add('info', 'registrar_workspace_artifact_manifest_exists', `Workspace artifact manifest exists: ${MCP_WORKSPACE_ARTIFACT_MANIFEST}`, {
        ...detail,
        artifact_manifest_path: MCP_WORKSPACE_ARTIFACT_MANIFEST,
      });
    }
    const runtimeProxyEntrypoint = selectedRuntimeProxyEntrypoint();
    if (!existsSync(runtimeProxyEntrypoint)) {
      add('error', 'registrar_runtime_proxy_missing', `Runtime proxy does not exist: ${runtimeProxyEntrypoint}`, {
        ...detail,
        runtime_proxy_entrypoint: runtimeProxyEntrypoint,
        runtime_proxy_implementation: runtimeProxyImplementation,
        remediation: 'Run pnpm --filter @narada-core/mcp-runtime-proxy build before launching carrier MCPs.',
      });
    } else if (includeOk) {
      add('info', 'registrar_runtime_proxy_exists', `Runtime proxy exists: ${runtimeProxyEntrypoint}`, { ...detail, runtime_proxy_entrypoint: runtimeProxyEntrypoint, runtime_proxy_implementation: runtimeProxyImplementation });
    }
  }
  if (!surface) return;
  for (const check of sharedRuntimeDependencyChecks(surface)) {
    if (!check.exists) {
      add('error', 'registrar_runtime_dependency_missing', `Runtime dependency export for '${check.dependency}' does not exist: ${check.export_path}`, {
        ...detail,
        dependency: check.dependency,
        package_root: check.package_root,
        export_path: check.export_path,
        remediation: `Run pnpm --filter ${check.dependency} build before launching carrier MCPs.`,
      });
    } else if (includeOk) {
      add('info', 'registrar_runtime_dependency_exists', `Runtime dependency export for '${check.dependency}' exists: ${check.export_path}`, {
        ...detail,
        dependency: check.dependency,
        package_root: check.package_root,
        export_path: check.export_path,
      });
    }
  }
}

function emitOpencodeConfig(carrier: CarrierDef, configPath?: string, compilation?: CarrierRuntimeMaterializationCompilation): { content: string; structured: JsonRecord } {
  const rawServers = collectCarrierServers(carrier);
  const mcp: JsonRecord = {};
  for (const [key, server] of Object.entries(rawServers)) {
    const surfaceId = server.kind === 'local' ? (server.local as SiteLocalSurface).surface_id : (server.surface as RegistrarSurfaceRecord).id;
    const overridden = applySurfaceOverrides(carrier, server, surfaceId);
    const launch = compilation?.launches.get(key) ?? carrierLaunchCommand(overridden, surfaceId, configPath, carrier);
    mcp[key] = {
      type: 'local',
      command: [launch.command, ...launch.args],
      enabled: overridden.enabled ?? true,
    };
  }
  const structured = { $schema: 'https://opencode.ai/config.json', mcp };
  const header = '// Generated by mcp-registrar. Do not hand-edit; changes will be overwritten on next materialize.\n';
  return { content: header + JSON.stringify(structured, null, 2) + '\n', structured };
}

function emitKimiConfig(carrier: CarrierDef, configPath?: string, compilation?: CarrierRuntimeMaterializationCompilation): { content: string; structured: JsonRecord } {
  const rawServers = collectCarrierServers(carrier);
  const mcpServers: JsonRecord = {};
  for (const [key, server] of Object.entries(rawServers)) {
    const surfaceId = server.kind === 'local' ? (server.local as SiteLocalSurface).surface_id : (server.surface as RegistrarSurfaceRecord).id;
    const overridden = applySurfaceOverrides(carrier, server, surfaceId);
    const approval = carrier.surface_overrides?.[surfaceId]?.approval_mode;
    const launch = compilation?.launches.get(key) ?? carrierLaunchCommand(overridden, surfaceId, configPath, carrier);
    const base: JsonRecord = {
      transport: 'stdio',
      command: launch.command,
      args: launch.args,
    };
    if (approval) base.approval_mode = approval;
    if (overridden.env_vars) base.env_vars = overridden.env_vars;
    mcpServers[key] = base;
  }
  const structured = { mcpServers };
  return { content: JSON.stringify(structured, null, 2) + '\n', structured };
}

function emitCodexConfig(carrier: CarrierDef, configPath?: string, compilation?: CarrierRuntimeMaterializationCompilation): { content: string; structured: JsonRecord } {
  const rawServers = collectCarrierServers(carrier);
  const codexPluginOverrides = carrier.codex_plugin_overrides ?? {};
  const lines: string[] = [];
  lines.push('# Generated by mcp-registrar. Do not hand-edit; changes will be overwritten on next materialize.');
  lines.push('');
  lines.push('# Codex Apps/connectors are opt-in for profile-less launches.');
  lines.push('[features]');
  lines.push('apps = false');
  lines.push('');
  for (const [pluginId, enabled] of Object.entries(codexPluginOverrides).sort(([left], [right]) => left.localeCompare(right))) {
    lines.push(`[plugins.${tomlBasicString(pluginId)}]`);
    lines.push(`enabled = ${enabled}`);
    lines.push('');
  }
  const trustProjects = dedupeRoots([...(carrier.trust_projects ?? []), ...(carrier.extra_allowed_roots ?? [])]);
  for (const project of trustProjects) {
    const escaped = project.replace(/\\/g, '\\\\');
    lines.push(`[projects.'${escaped}']`);
    lines.push('trust_level = "trusted"');
    lines.push('');
  }
  const mcpServers: JsonRecord = {};
  for (const [key, server] of Object.entries(rawServers)) {
    const surfaceId = server.kind === 'local' ? (server.local as SiteLocalSurface).surface_id : (server.surface as RegistrarSurfaceRecord).id;
    const overridden = applySurfaceOverrides(carrier, server, surfaceId);
    const launch = compilation?.launches.get(key) ?? carrierLaunchCommand(overridden, surfaceId, configPath, carrier);
    const carrierAvailableTools = codexCarrierAvailableTools(server);
    lines.push(`[mcp_servers.${key}]`);
    lines.push(`command = "${launch.command}"`);
    lines.push(`args = ${JSON.stringify(launch.args)}`);
    lines.push('approval_mode = "approve"');
    const startupTimeoutSec = server.surface?.codex_startup_timeout_sec;
    if (startupTimeoutSec !== undefined) {
      lines.push(`startup_timeout_sec = ${startupTimeoutSec}`);
    }
    if (overridden.env_vars) {
      lines.push(`env_vars = ${JSON.stringify(overridden.env_vars)}`);
    }
    lines.push('');
    if (carrierAvailableTools.length > 0) {
      lines.push('# Generated carrier availability metadata. Narada MCP surfaces own policy.');
      for (const toolName of carrierAvailableTools) {
        lines.push(`[mcp_servers.${key}.tools.${toolName}]`);
        lines.push('approval_mode = "approve"');
        lines.push('');
      }
    }
    mcpServers[key] = {
      command: launch.command,
      args: launch.args,
      approval_mode: 'approve',
      ...(overridden.env_vars === undefined ? {} : { env_vars: overridden.env_vars }),
      ...(startupTimeoutSec === undefined ? {} : { startup_timeout_sec: startupTimeoutSec }),
    };
  }
  const plugins = Object.fromEntries(Object.entries(codexPluginOverrides).map(([pluginId, enabled]) => [pluginId, { enabled }]));
  const structured = { features: { apps: false }, plugins, trust_projects: trustProjects, mcpServers };
  return { content: lines.join('\n') + '\n', structured };
}

function codexCarrierAvailableTools(server: MaterializedServer): string[] {
  if (server.kind === 'shared') return uniqueStrings((server.surface as RegistrarSurfaceRecord).tools);
  return [];
}

function writeFileAtomic(path: string, content: string): void {
  const dir = resolve(path, '..');
  mkdirSync(dir, { recursive: true });
  const temporaryPath = `${path}.tmp-${process.pid}-${Date.now()}`;
  writeFileSync(temporaryPath, content, 'utf8');
  try {
    renameSync(temporaryPath, path);
  } finally {
    if (existsSync(temporaryPath)) unlinkSync(temporaryPath);
  }
}

function readWorkspaceManifestFingerprint(): string | null {
  if (!existsSync(MCP_WORKSPACE_ARTIFACT_MANIFEST)) return null;
  try {
    const manifest = asRecord(JSON.parse(readFileSync(MCP_WORKSPACE_ARTIFACT_MANIFEST, 'utf8')));
    return typeof manifest.manifest_fingerprint === 'string' ? manifest.manifest_fingerprint : null;
  } catch {
    return null;
  }
}

function validateCarrierMaterialization(
  carrier: CarrierDef,
  result: { content: string; structured: JsonRecord },
  configPath?: string,
  runtimePlan: RuntimeMaterializationPlan = runtimeMaterializationPlan,
  carrierPlan?: JsonRecord,
  proxyImplementation: RuntimeProxyImplementation = runtimeProxyImplementationForResolvedPlan(runtimePlan),
): {
  validation: ReturnType<typeof validateMaterializedConfiguration>;
  generation: ReturnType<typeof buildMaterializationGeneration> | null;
} {
  const validation = validateMaterializedConfiguration({
    structured: result.structured,
    artifactManifestPath: MCP_WORKSPACE_ARTIFACT_MANIFEST,
    runtimeProxyEntrypoint: selectedRuntimeProxyEntrypoint(proxyImplementation),
    expectedSidecarPath: configPath ? materializationSidecarPath(configPath) : undefined,
    requireSidecar: Boolean(configPath),
  });
  if (!validation.ok) {
    throw diagnosticError(
      'registrar_materialized_config_contract_invalid',
      'Generated carrier configuration violates the MCP runtime contract.',
      {
        carrier_id: carrier.carrier_id,
        carrier_kind: carrier.kind,
        validation,
        remediation: 'Refuse the write, rebuild the workspace, and retry only after the generated contract validates.',
      },
    );
  }
  const generation = configPath
    ? (() => {
      const planPath = runtimeMaterializationPlanPath(configPath);
      const planFingerprint = typeof carrierPlan?.plan_fingerprint === 'string' ? carrierPlan.plan_fingerprint : null;
      if (!planFingerprint) {
        throw diagnosticError('registrar_runtime_materialization_plan_missing', 'A carrier generation cannot be written without its compiled runtime materialization plan.', { carrier_id: carrier.carrier_id, config_path: configPath });
      }
      return buildMaterializationGeneration({
      carrierId: carrier.carrier_id,
      carrierKind: carrier.kind,
      configPath,
      content: result.content,
      artifactManifestPath: MCP_WORKSPACE_ARTIFACT_MANIFEST,
      artifactManifestFingerprint: readWorkspaceManifestFingerprint(),
      runtimeProfileKind: String(runtimePlan.runtime_profile_kind) as RuntimeProfileKind,
      runtimeMaterializationPlanPath: planPath,
      runtimeMaterializationPlanFingerprint: planFingerprint,
      runtimeImplementationMatrixPath: MCP_RUNTIME_IMPLEMENTATION_MATRIX_PATH,
      runtimeImplementationMatrixFingerprint: runtimePlanMatrixFingerprint(runtimePlan),
      registrarEntrypoint: MCP_REGISTRAR_RUNTIME_ENTRYPOINT,
      proxyImplementation,
      proxyEntrypoint: selectedRuntimeProxyEntrypoint(proxyImplementation),
      serverCount: validation.server_count,
      proxyCount: validation.proxy_count,
      });
    })()
    : null;
  return { validation, generation };
}

function runFreshRegistrarRequest(method: string, args: JsonRecord): Promise<JsonRecord> {
  const requestedProfile = optionalString(args.runtime_profile) ?? runtimeProfileKind;
  const requestedPlan = acceptedRuntimeMaterializationPlan(requestedProfile);
  const registrarEntry = runtimeMaterializationPlanEntry(requestedPlan, 'mcp-registrar');
  const registrarEngine = String(registrarEntry?.runtime_engine_kind ?? 'bun') as RuntimeEngineKind;
  const registrarCommand = javascriptRuntimeCommand(registrarEngine);
  return new Promise((resolveRequest, rejectRequest) => {
    const child = spawn(registrarCommand, [MCP_REGISTRAR_RUNTIME_ENTRYPOINT], {
      cwd: MCP_WORKSPACE_ROOT,
      env: { ...process.env, NARADA_RUNTIME_PROFILE: requestedProfile, [FRESH_REGISTRAR_ENV]: '1' },
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    });
    let stdout = '';
    let stderr = '';
    let settled = false;
    const timeout = setTimeout(() => {
      if (settled) return;
      child.kill();
      settled = true;
      rejectRequest(diagnosticError(
        'registrar_fresh_materialization_failed',
        'Fresh registrar subprocess timed out while materializing the carrier configuration.',
        { entrypoint: MCP_REGISTRAR_RUNTIME_ENTRYPOINT, timeout_ms: 30000, stderr_tail: stderr.slice(-4000) },
      ));
    }, 30000);
    const fail = (message: string, details: JsonRecord = {}) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      rejectRequest(diagnosticError('registrar_fresh_materialization_failed', message, {
        entrypoint: MCP_REGISTRAR_RUNTIME_ENTRYPOINT,
        stderr_tail: stderr.slice(-4000),
        ...details,
      }));
    };
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk: string) => { stdout += chunk; });
    child.stderr.on('data', (chunk: string) => { stderr += chunk; });
    child.once('error', (error) => fail('Fresh registrar subprocess could not be started.', { error: error.message }));
    child.once('close', (exitCode, signal) => {
      if (settled) return;
      clearTimeout(timeout);
      const responseLine = stdout.split(/\r?\n/).map((line) => line.trim()).filter(Boolean).at(-1);
      if (!responseLine) {
        fail('Fresh registrar subprocess exited without a JSON-RPC response.', { exit_code: exitCode, signal });
        return;
      }
      let response: JsonRecord;
      try {
        response = asRecord(JSON.parse(responseLine));
      } catch (error) {
        fail('Fresh registrar subprocess returned invalid JSON-RPC output.', {
          exit_code: exitCode,
          signal,
          parse_error: error instanceof Error ? error.message : String(error),
          stdout_tail: stdout.slice(-4000),
        });
        return;
      }
      const responseError = asRecord(response.error);
      if (Object.keys(responseError).length > 0) {
        fail(String(responseError.message ?? 'Fresh registrar materialization failed.'), {
          exit_code: exitCode,
          signal,
          child_error: responseError,
        });
        return;
      }
      const resultEnvelope = asRecord(response.result);
      const result = resultEnvelope.structuredContent ?? resultEnvelope;
      if (!result || typeof result !== 'object' || Array.isArray(result)) {
        fail('Fresh registrar subprocess returned no structured result.', { exit_code: exitCode, signal });
        return;
      }
      if (exitCode !== 0) {
        fail('Fresh registrar subprocess exited non-zero after returning a result.', { exit_code: exitCode, signal });
        return;
      }
      settled = true;
      resolveRequest(result as JsonRecord);
    });
    child.stdin.write(JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: method, arguments: args },
    }) + '\n');
    child.stdin.end();
  });
}

function materializeCarrierAtPath(carrier: CarrierDef, outputPath: string, persistSiteState: boolean, proxyImplementationOverride?: RuntimeProxyImplementation, recoveryEscapeHatch = false): JsonRecord {
  const injectionSummary = carrierInjectionSummary(carrier);
  const compilation = compileCarrierRuntimeMaterializationPlan(carrier, outputPath, proxyImplementationOverride, recoveryEscapeHatch);
  let result: { content: string; structured: JsonRecord };
  switch (carrier.kind) {
    case 'opencode': result = emitOpencodeConfig(carrier, outputPath, compilation); break;
    case 'kimi': result = emitKimiConfig(carrier, outputPath, compilation); break;
    case 'codex': result = emitCodexConfig(carrier, outputPath, compilation); break;
    default: throw diagnosticError('registrar_unknown_carrier_kind', `registrar_unknown_carrier_kind:${carrier.kind}`);
  }
  const { validation, generation } = validateCarrierMaterialization(carrier, result, outputPath, runtimeMaterializationPlan, compilation.plan, compilation.runtimeProxyImplementation);
  writeFileAtomic(outputPath, result.content);
  writeRuntimeMaterializationPlan(runtimeMaterializationPlanPath(outputPath), compilation.plan);
  writeMaterializationGeneration(materializationSidecarPath(outputPath), generation!);
  const carrierRuntimePlan = compilation.plan;
  const runtimeMaterializationPlanOutputPath = runtimeMaterializationPlanPath(outputPath);
  let siteSurfaceRegistries: JsonRecord[] | null = null;
  if (persistSiteState) {
    writeSiteAllowedRootsConfig(carrier);
    siteSurfaceRegistries = writeSiteSurfaceRegistriesForCarrier(carrier);
  }
  return {
    status: 'materialized',
    carrier_id: carrier.carrier_id,
    kind: carrier.kind,
    output_path: outputPath,
    byte_size: Buffer.byteLength(result.content, 'utf8'),
    injection_scopes: injectionSummary,
    injection_scope_counts: injectionSummary.counts,
    ...(siteSurfaceRegistries === null ? {} : { site_surface_registries: siteSurfaceRegistries }),
    runtime_contract_version: MCP_RUNTIME_CONTRACT_VERSION,
    materialization_validation: validation,
    materialization_generation: generation,
    generation_sidecar_path: materializationSidecarPath(outputPath),
    runtime_materialization_plan: carrierRuntimePlan,
    runtime_materialization_plan_path: runtimeMaterializationPlanOutputPath,
    ...(compilation.recoveryEscapeHatch ? { recovery_escape_hatch: true } : {}),
  };
}

async function registrarMaterializeAll(args: JsonRecord): Promise<JsonRecord> {
  if (process.env[FRESH_REGISTRAR_ENV] !== '1') {
    return runFreshRegistrarRequest('registrar_materialize_all', args);
  }
  return withRuntimeMaterializationProfile(optionalString(args.runtime_profile), () => registrarMaterializeAllFresh(args));
}


async function registrarMaterializeAllFresh(args: JsonRecord): Promise<JsonRecord> {
  assertRegistrarProcessCurrent('registrar_materialize_all');
  const requestedOutputDir = optionalString(args.output_dir);
  const outputDir = requestedOutputDir ? resolve(requestedOutputDir) : null;
  if (outputDir) mkdirSync(outputDir, { recursive: true });
  const outputPaths = CARRIERS.map((carrier) => outputDir ? join(outputDir, basename(carrier.config_path)) : carrier.config_path);
  const pathOwners = new Map<string, string>();
  outputPaths.forEach((outputPath, index) => {
    const normalizedPath = resolve(outputPath).toLowerCase();
    const previousCarrierId = pathOwners.get(normalizedPath);
    if (previousCarrierId) {
      throw diagnosticError(
        'registrar_carrier_materialization_path_collision',
        'All-carrier materialization would overwrite two carrier configs at the same path.',
        { output_path: outputPath, carrier_ids: [previousCarrierId, CARRIERS[index]!.carrier_id] },
      );
    }
    pathOwners.set(normalizedPath, CARRIERS[index]!.carrier_id);
  });
  const carriers = CARRIERS.map((carrier, index) => materializeCarrierAtPath(carrier, outputPaths[index]!, outputDir === null));
  return {
    status: 'materialized_all',
    carrier_count: carriers.length,
    output_dir: outputDir,
    carriers,
    runtime_contract_version: MCP_RUNTIME_CONTRACT_VERSION,
  };
}

/**
 * Targeted materialization is deliberately private to the direct CLI. It is
 * an emergency recovery escape hatch, not an MCP operation or normal
 * registrar workflow. The CLI parser requires --allow-single-carrier before
 * this function can be reached.
 */
async function registrarSingleCarrierMaterialize(args: JsonRecord): Promise<JsonRecord> {
  if (process.env[FRESH_REGISTRAR_ENV] !== '1') {
    throw diagnosticError(
      'registrar_single_carrier_materialization_direct_only',
      'Single-carrier materialization is available only through the explicit direct CLI escape hatch.',
    );
  }
  assertRegistrarProcessCurrent('registrar_single_carrier_materialize');
  const carrierId = requiredString(args.carrier_id, 'registrar_requires_carrier_id');
  const carrier = lookupCarrier(carrierId);
  const outputPath = optionalString(args.output_path) ?? carrier.config_path;
  const requestedProxyImplementation = optionalString(args.runtime_proxy_implementation);
  const proxyImplementation: RuntimeProxyImplementation =
    requestedProxyImplementation === 'bun' || requestedProxyImplementation === 'node' || requestedProxyImplementation === 'native'
      ? requestedProxyImplementation
      : runtimeProxyImplementationForPlan();
  return materializeCarrierAtPath(carrier, resolve(outputPath), false, proxyImplementation, true);
}

function registrarCarrierValidate(args: JsonRecord): JsonRecord {
  const carrierId = requiredString(args.carrier_id, 'registrar_requires_carrier_id');
  const carrier = lookupCarrier(carrierId);
  const includeOk = args.include_ok === true;
  const findings: ValidationFinding[] = [];

  function add(severity: ValidationFinding['severity'], code: string, message: string, detail: JsonRecord = {}) {
    findings.push({ severity, code, message, ...detail });
  }

  // Duplicate key detection
  const seenKeys = new Map<string, string>();
  const rawServers = collectCarrierServers(carrier);
  for (const [key, server] of Object.entries(rawServers)) {
    const surfaceId = server.kind === 'local' ? (server.local as SiteLocalSurface).surface_id : (server.surface as RegistrarSurfaceRecord).id;
    const scopeDetail = scopeFindingDetail(server.narada_scope);
    if (seenKeys.has(key)) {
      add('error', 'registrar_duplicate_server_key', `Server key '${key}' is produced by both '${seenKeys.get(key)}' and '${surfaceId}'`, { server_key: key, surface_id: surfaceId, ...scopeDetail });
    } else {
      seenKeys.set(key, surfaceId);
      if (includeOk) add('info', 'registrar_server_key_ok', `Server key '${key}' resolved for surface '${surfaceId}'`, { server_key: key, surface_id: surfaceId, ...scopeDetail });
    }
  }

  // Entrypoint existence and required flags
  for (const [key, server] of Object.entries(rawServers)) {
    const surfaceId = server.kind === 'local' ? (server.local as SiteLocalSurface).surface_id : (server.surface as RegistrarSurfaceRecord).id;
    const overridden = applySurfaceOverrides(carrier, server, surfaceId);
    const scopeDetail = scopeFindingDetail(server.narada_scope);
    const launch = carrierLaunchCommand(overridden, surfaceId);
    if (!existsSync(overridden.entrypoint)) {
      add('error', 'registrar_missing_entrypoint', `Entrypoint for '${key}' does not exist: ${overridden.entrypoint}`, { server_key: key, surface_id: surfaceId, entrypoint: overridden.entrypoint, ...scopeDetail });
    } else if (includeOk) {
      add('info', 'registrar_entrypoint_exists', `Entrypoint for '${key}' exists: ${overridden.entrypoint}`, { server_key: key, surface_id: surfaceId, entrypoint: overridden.entrypoint, ...scopeDetail });
    }
    addRuntimePreflightFindings(add, includeOk, { server_key: key, surface_id: surfaceId, entrypoint: overridden.entrypoint, ...scopeDetail }, server.kind === 'shared' ? server.surface as RegistrarSurfaceRecord : null, launch.uses_runtime_proxy);

    // Allowed-root requirement
    if (rootsNeedingAllowedRoot(surfaceId)) {
      const allowedRoots: string[] = [];
      for (let i = 0; i < overridden.args.length; i++) {
        if (overridden.args[i] === '--allowed-root' && i + 1 < overridden.args.length) {
          allowedRoots.push(overridden.args[i + 1]);
        }
      }
      if (allowedRoots.length === 0) {
        add('error', 'registrar_missing_allowed_root', `Surface '${surfaceId}' requires at least one --allowed-root but '${key}' has none`, { server_key: key, surface_id: surfaceId, ...scopeDetail });
      } else if (includeOk) {
        add('info', 'registrar_allowed_root_ok', `Surface '${surfaceId}' on '${key}' has ${allowedRoots.length} allowed root(s)`, { server_key: key, surface_id: surfaceId, allowed_roots: allowedRoots, ...scopeDetail });
      }
    }

    // Output-root requirement for local-filesystem
    if (surfaceId === 'local-filesystem' || surfaceId === 'local-filesystem-mcp.local') {
      const hasOutputRoot = overridden.args.some((a) => a === '--output-root');
      if (!hasOutputRoot) {
        add('warning', 'registrar_missing_output_root', `Filesystem surface '${key}' is missing --output-root`, { server_key: key, surface_id: surfaceId, ...scopeDetail });
      } else if (includeOk) {
        add('info', 'registrar_output_root_ok', `Filesystem surface '${key}' has --output-root`, { server_key: key, surface_id: surfaceId, ...scopeDetail });
      }
    }
  }

  const errors = findings.filter((f) => f.severity === 'error').length;
  const warnings = findings.filter((f) => f.severity === 'warning').length;
  return {
    status: errors > 0 ? 'invalid' : warnings > 0 ? 'valid_with_warnings' : 'valid',
    carrier_id: carrierId,
    server_count: Object.keys(rawServers).length,
    errors,
    warnings,
    findings,
  };
}

function registrarSiteMcpFabricValidate(args: JsonRecord): JsonRecord {
  const siteId = requiredString(args.site_id, 'registrar_requires_site_id');
  const site = lookupSite(siteId);
  return validateSiteMcpFabric(site, args.include_ok === true);
}

function registrarCarrierDiff(args: JsonRecord): JsonRecord {
  const carrierId = requiredString(args.carrier_id, 'registrar_requires_carrier_id');
  const carrier = lookupCarrier(carrierId);
  const currentPath = carrier.config_path;
  const currentContent = existsSync(currentPath) ? readFileSync(currentPath, 'utf8') : null;
  const currentStructured = currentContent ? parseCarrierConfig(carrier.kind, currentContent) : null;
  const compilation = compileCarrierRuntimeMaterializationPlan(carrier, currentPath);
  let generated: { content: string; structured: JsonRecord };
  switch (carrier.kind) {
    case 'opencode': generated = emitOpencodeConfig(carrier, currentPath, compilation); break;
    case 'kimi': generated = emitKimiConfig(carrier, currentPath, compilation); break;
    case 'codex': generated = emitCodexConfig(carrier, currentPath, compilation); break;
    default: throw diagnosticError('registrar_unknown_carrier_kind', `registrar_unknown_carrier_kind:${carrier.kind}`);
  }
  const materializationValidation = validateCarrierMaterialization(carrier, generated, undefined, runtimeMaterializationPlan, compilation.plan).validation;

  return {
    ...compareCarrierProjection({
    carrierId,
    configPath: currentPath,
    generatedContent: generated.content,
    generatedStructured: generated.structured,
    currentContent,
    currentStructured,
    }),
    runtime_contract_version: MCP_RUNTIME_CONTRACT_VERSION,
    materialization_validation: materializationValidation,
  };
}

export function compareCarrierProjection({
  carrierId,
  configPath,
  generatedContent,
  generatedStructured,
  currentContent,
  currentStructured,
}: {
  carrierId: string;
  configPath: string;
  generatedContent: string;
  generatedStructured: JsonRecord;
  currentContent: string | null;
  currentStructured: JsonRecord | null;
}): JsonRecord {
  const contentSha256 = (content: string | null) =>
    content === null ? null : createHash('sha256').update(content, 'utf8').digest('hex');

  const generatedServers = asRecord(generatedStructured.mcpServers ?? generatedStructured.mcp ?? {});
  const currentServers = currentStructured ? asRecord(currentStructured.mcpServers ?? currentStructured.mcp ?? {}) : {};

  const added: string[] = [];
  const removed: string[] = [];
  const changed: string[] = [];
  const unchanged: string[] = [];

  for (const key of Object.keys(generatedServers)) {
    if (!(key in currentServers)) {
      added.push(key);
    } else if (canonicalJson(generatedServers[key]) !== canonicalJson(currentServers[key])) {
      changed.push(key);
    } else {
      unchanged.push(key);
    }
  }
  for (const key of Object.keys(currentServers)) {
    if (!(key in generatedServers)) removed.push(key);
  }

  const projectionChanged = currentContent !== generatedContent;
  const serverProjectionChanged = added.length > 0 || removed.length > 0 || changed.length > 0;
  const carrierMetadataOrFormatOnly = currentContent !== null && projectionChanged && !serverProjectionChanged;
  const changeScopes = currentContent === null
    ? ['full_projection_missing']
    : !projectionChanged
      ? []
      : ['full_projection', serverProjectionChanged ? 'server_definitions' : 'carrier_metadata_or_format'];

  return {
    schema: 'narada.registrar.carrier_projection_diff.v1',
    status: currentContent === null ? 'missing' : projectionChanged ? 'diff' : 'clean',
    carrier_id: carrierId,
    config_path: configPath,
    current_exists: currentContent !== null,
    projection_changed: projectionChanged,
    server_projection_changed: serverProjectionChanged,
    carrier_metadata_or_format_only: carrierMetadataOrFormatOnly,
    change_scopes: changeScopes,
    explanation_code: currentContent === null
      ? 'carrier_projection_missing'
      : !projectionChanged
        ? 'carrier_projection_exact_match'
        : carrierMetadataOrFormatOnly
          ? 'carrier_metadata_or_format_changed_without_server_definition_change'
          : 'carrier_server_definition_change',
    generated_sha256: contentSha256(generatedContent),
    current_sha256: contentSha256(currentContent),
    generated_byte_size: Buffer.byteLength(generatedContent, 'utf8'),
    current_byte_size: currentContent === null ? null : Buffer.byteLength(currentContent, 'utf8'),
    added,
    removed,
    changed,
    unchanged,
    added_count: added.length,
    removed_count: removed.length,
    changed_count: changed.length,
    server_changed_count: changed.length,
    count_semantics: 'added_removed_changed_counts_cover_server_definitions_only',
    server_changes: {
      added,
      removed,
      changed,
      unchanged,
      added_count: added.length,
      removed_count: removed.length,
      changed_count: changed.length,
    },
  };
}

function parseCarrierConfig(kind: CarrierDef['kind'], content: string): JsonRecord | null {
  try {
    switch (kind) {
      case 'opencode':
      case 'kimi':
        return asRecord(JSON.parse(stripJsoncComments(content)));
      case 'codex':
        return parseCodexToml(content);
      default:
        return null;
    }
  } catch {
    return null;
  }
}

function parseCodexToml(content: string): JsonRecord {
  const result: JsonRecord = { mcpServers: {} };
  const lines = content.split(/\r?\n/);
  let currentKey: string | null = null;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (line.startsWith('#') || line.length === 0) continue;
    const sectionMatch = line.match(/^\[mcp_servers\.([^\]]+)\]$/);
    if (sectionMatch) {
      const sectionPath = sectionMatch[1];
      if (sectionPath.includes('.tools.')) {
        currentKey = null;
        continue;
      }
      currentKey = sectionPath;
      (result.mcpServers as JsonRecord)[currentKey] = {};
      continue;
    }
    if (currentKey) {
      const kvMatch = line.match(/^([A-Za-z0-9_]+)\s*=\s*(.+)$/);
      if (kvMatch) {
        const [, k, rawV] = kvMatch;
        const serverRecord = asRecord((result.mcpServers as JsonRecord)[currentKey]);
        try {
          serverRecord[k] = JSON.parse(rawV);
        } catch {
          serverRecord[k] = rawV.replace(/^"|"$/g, '');
        }
        (result.mcpServers as JsonRecord)[currentKey] = serverRecord;
      }
    }
  }
  return result;
}

function registrarSurfaceList(_args: JsonRecord): JsonRecord {
  return {
    items: SURFACES.map((surface) => {
      const projections = surfaceProjections(surface);
      const descriptor = nativeSurfaceDescriptor(surface.id);
      const scope = projections.length === 1
        ? {
          ...surfaceScopeMetadata(surface.id, '{site_root}', projections[0].id),
          narada_scope: naradaScopeMetadata(surface.id, '{site_root}', undefined, projections[0].id),
        }
        : {};
      return {
        ...surface,
        tools: nativeToolNames(surface.id),
        projections,
        descriptor_source: descriptor.source,
        descriptor_digest: surfaceDescriptorDigest(descriptor),
        tool_contract_digest: surfaceToolContractDigest(descriptor),
        descriptor,
        ...scope,
      };
    }),
    count: SURFACES.length,
  };
}

function registrarSurfaceToolInventoryCheck(args: JsonRecord): JsonRecord {
  const observedInput = asRecord(args.observed_tools);
  const includeOk = args.include_ok === true;
  const surfaces = SURFACES.filter((surface) => Object.hasOwn(observedInput, surface.id));
  const findings = surfaces.flatMap((surface) => {
    const registered = nativeToolNames(surface.id);
    const observed = uniqueStrings(Array.isArray(observedInput[surface.id]) ? (observedInput[surface.id] as unknown[]).map(String) : []);
    const missing_from_registrar = observed.filter((tool) => !registered.includes(tool));
    const extra_in_registrar = registered.filter((tool) => !observed.includes(tool));
    const status = missing_from_registrar.length === 0 && extra_in_registrar.length === 0 ? 'ok' : 'drift';
    if (status === 'ok' && !includeOk) return [];
    return [{
      surface_id: surface.id,
      package: surface.package,
      status,
      registered_count: registered.length,
      observed_count: observed.length,
      missing_from_registrar,
      extra_in_registrar,
    }];
  });
  return {
    schema: 'narada.registrar.surface_tool_inventory_check.v1',
    status: findings.some((finding) => finding.status === 'drift') ? 'drift' : 'ok',
    checked_count: surfaces.length,
    surfaces_without_observations: SURFACES.map((surface) => surface.id).filter((surfaceId) => !Object.hasOwn(observedInput, surfaceId)),
    findings,
  };
}

function sameStringSet(left: string[], right: string[]): boolean {
  const a = uniqueStrings(left).sort();
  const b = uniqueStrings(right).sort();
  return a.length === b.length && a.every((value, index) => value === b[index]);
}

function stringSetDifference(left: string[], right: string[]): string[] {
  const rightSet = new Set(uniqueStrings(right));
  return uniqueStrings(left).filter((value) => !rightSet.has(value)).sort();
}

function canonicalJson(value: unknown): string {
  if (value === undefined) return 'undefined';
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map((item) => canonicalJson(item)).join(',')}]`;
  const record = asRecord(value);
  return `{${Object.keys(record).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`).join(',')}}`;
}

function observedToolsForSurface(input: JsonRecord, server: SiteMcpFabricServer, registrySurface?: JsonRecord): string[] | null {
  const keys = uniqueStrings([
    server.server_key,
    `${server.server_key}.local`,
    server.surface_id ?? '',
    String(registrySurface?.catalog_surface_id ?? ''),
  ]);
  for (const key of keys) {
    if (Object.hasOwn(input, key) && Array.isArray(input[key])) {
      return (input[key] as unknown[]).map(String);
    }
  }
  return null;
}

export function checkSiteRegistryConformance(
  site: SiteDef,
  registry: JsonRecord,
  observedToolsInput: JsonRecord,
  observedReadOnlyToolsInput: JsonRecord,
  observedMutatingToolsInput: JsonRecord,
  includeOk = false,
): JsonRecord {
  const violations: JsonRecord[] = [];
  const surfaceResults: JsonRecord[] = [];
  const observedServerKeys = new Set(Object.keys(observedToolsInput));
  const observationCoverage = observedServerKeys.size === 0 ? 'none' : 'partial';
  const unobservedServerNames: string[] = [];
  const rawRegistrySurfaces = Array.isArray(registry.surfaces) ? registry.surfaces : [];
  const registrySurfaces = rawRegistrySurfaces.map((entry) => asRecord(entry));
  const registryByServer = new Map<string, JsonRecord>();

  const addGlobal = (code: string, details: JsonRecord = {}) => {
    violations.push({ layer: 'materialized_registry', code, surface_id: null, server_name: null, ...details });
  };

  if (registry.schema !== 'narada.site.capabilities.mcp_surfaces.v1') {
    addGlobal('registry_schema_mismatch', { expected: 'narada.site.capabilities.mcp_surfaces.v1', actual: registry.schema ?? null });
  }
  if (registry.site_id !== site.site_id) {
    addGlobal('registry_site_id_mismatch', { expected: site.site_id, actual: registry.site_id ?? null });
  }
  if (registry.generated_by !== 'mcp-registrar') {
    addGlobal('registry_generator_mismatch', { expected: 'mcp-registrar', actual: registry.generated_by ?? null });
  }
  const generationPolicy = asRecord(registry.generation_policy);
  if (generationPolicy.mode !== 'enabled_surface_tool_authority') {
    addGlobal('registry_generation_policy_mismatch', { expected: 'enabled_surface_tool_authority', actual: generationPolicy.mode ?? null });
  }
  if (generationPolicy.source !== '.ai/mcp + registrar surface catalog') {
    addGlobal('registry_generation_source_mismatch', { expected: '.ai/mcp + registrar surface catalog', actual: generationPolicy.source ?? null });
  }
  if (generationPolicy.note !== 'Every tool exposed by an enabled MCP surface is declared for action admission. The MCP surface remains responsible for command policy and mutation enforcement.') {
    addGlobal('registry_generation_note_mismatch');
  }
  if (typeof registry.generated_at !== 'string' || !Number.isFinite(Date.parse(registry.generated_at))) {
    addGlobal('registry_generated_at_invalid', { actual: registry.generated_at ?? null });
  }
  if (!Array.isArray(registry.surfaces)) {
    addGlobal('registry_surfaces_invalid', { actual_type: typeof registry.surfaces });
  }

  for (const surface of registrySurfaces) {
    const serverName = String(surface.server_name ?? '');
    if (!serverName) {
      addGlobal('registry_surface_server_name_missing', { surface_id: surface.surface_id ?? null });
      continue;
    }
    if (registryByServer.has(serverName)) {
      addGlobal('registry_surface_server_name_duplicate', { server_name: serverName });
      continue;
    }
    registryByServer.set(serverName, surface);
  }

  const fabricServers = discoverSiteMcpFabric(site);
  const fabricServerNames = new Set(fabricServers.map((server) => server.server_key));
  for (const registryServerName of registryByServer.keys()) {
    if (!fabricServerNames.has(registryServerName)) {
      addGlobal('registry_surface_not_in_fabric', { server_name: registryServerName });
    }
  }

  for (const server of fabricServers) {
    const surfaceViolations: JsonRecord[] = [];
    const actualSurface = registryByServer.get(server.server_key);
    const catalogSurfaceId = actualSurface
      ? String(actualSurface.catalog_surface_id ?? '')
      : (server.surface_id ?? fabricSurfaceId(server.server_key, site));
    const catalog = catalogSurface(catalogSurfaceId) ?? catalogSurfaceAlias(catalogSurfaceId);
    const rawFabricTools = readConfiguredServerToolsRaw(site, server);
    const fabricTools = uniqueStrings(rawFabricTools);
    const liveTools = observedToolsForSurface(observedToolsInput, server, actualSurface);
    const liveReadOnlyTools = observedToolsForSurface(observedReadOnlyToolsInput, server, actualSurface);
    const liveMutatingTools = observedToolsForSurface(observedMutatingToolsInput, server, actualSurface);
    const add = (layer: string, code: string, details: JsonRecord = {}) => {
      const violation = {
        layer,
        code,
        surface_id: actualSurface?.surface_id ?? `${server.server_key}.local`,
        server_name: server.server_key,
        catalog_surface_id: catalog?.id ?? catalogSurfaceId,
        ...details,
      };
      surfaceViolations.push(violation);
      violations.push(violation);
    };
    const compare = (layer: string, code: string, expected: string[], actual: string[]) => {
      if (sameStringSet(expected, actual)) return;
      add(layer, code, {
        missing: stringSetDifference(expected, actual),
        extra: stringSetDifference(actual, expected),
        expected_count: uniqueStrings(expected).length,
        actual_count: uniqueStrings(actual).length,
      });
    };

    if (!actualSurface) add('materialized_registry', 'registry_surface_missing');
    if (!catalog) add('registrar_catalog', 'catalog_surface_missing');
    const surfaceWasRequested = observedServerKeys.size === 0 || observedServerKeys.has(server.server_key)
      || observedServerKeys.has(server.surface_id ?? '') || observedServerKeys.has(String(actualSurface?.catalog_surface_id ?? ''));
    if (!surfaceWasRequested) {
      unobservedServerNames.push(server.server_key);
    } else {
      if (liveTools === null) add('live_surface', 'live_tool_observation_missing');
      if (liveReadOnlyTools === null) add('live_surface', 'live_read_only_observation_missing');
      if (liveMutatingTools === null) add('live_surface', 'live_mutating_observation_missing');
    }
    const duplicateFabricTools = duplicateStrings(rawFabricTools);
    if (duplicateFabricTools.length > 0) add('site_fabric', 'fabric_tools_duplicate', { duplicate_tools: duplicateFabricTools });
    if (liveTools !== null) {
      const duplicateLiveTools = duplicateStrings(liveTools);
      if (duplicateLiveTools.length > 0) add('live_surface', 'live_tools_duplicate', { duplicate_tools: duplicateLiveTools });
    }
    if (liveReadOnlyTools !== null) {
      const duplicateLiveReadOnlyTools = duplicateStrings(liveReadOnlyTools);
      if (duplicateLiveReadOnlyTools.length > 0) add('live_surface', 'live_read_only_tools_duplicate', { duplicate_tools: duplicateLiveReadOnlyTools });
    }
    if (liveMutatingTools !== null) {
      const duplicateLiveMutatingTools = duplicateStrings(liveMutatingTools);
      if (duplicateLiveMutatingTools.length > 0) add('live_surface', 'live_mutating_tools_duplicate', { duplicate_tools: duplicateLiveMutatingTools });
    }
    if (liveTools !== null && liveReadOnlyTools !== null && liveMutatingTools !== null) {
      compare('live_surface', 'live_tool_semantics_partition_incomplete', liveTools, [...liveReadOnlyTools, ...liveMutatingTools]);
      const liveOverlaps = uniqueStrings(liveReadOnlyTools.filter((tool) => liveMutatingTools.includes(tool)));
      if (liveOverlaps.length > 0) add('live_surface', 'live_tool_semantics_partition_overlap', { overlapping_tools: liveOverlaps });
    }

    if (liveTools !== null) {
      compare('site_fabric', 'fabric_tools_differ_from_live', liveTools, fabricTools);
      if (catalog) compare('registrar_catalog', 'catalog_tools_differ_from_live', liveTools, catalog.tools);
    }

    if (actualSurface) {
      const registeredTools = uniqueStrings(Array.isArray(actualSurface.registered_live_tools) ? actualSurface.registered_live_tools.map(String) : []);
      const contract = asRecord(actualSurface.tool_contract);
      const rawRegisteredTools = Array.isArray(actualSurface.registered_live_tools) ? actualSurface.registered_live_tools.map(String) : [];
      const rawReadOnlyTools = Array.isArray(contract.read_only_tools) ? contract.read_only_tools.map(String) : [];
      const rawMutatingTools = Array.isArray(contract.mutating_tools) ? contract.mutating_tools.map(String) : [];
      const rawRefusedTools = Array.isArray(contract.refused_tools) ? contract.refused_tools.map(String) : [];
      const readOnlyTools = uniqueStrings(rawReadOnlyTools);
      const mutatingTools = uniqueStrings(rawMutatingTools);
      const refusedTools = uniqueStrings(rawRefusedTools);
      const contractUnion = uniqueStrings([...readOnlyTools, ...mutatingTools, ...refusedTools]);
      const overlaps = uniqueStrings([
        ...readOnlyTools.filter((tool) => mutatingTools.includes(tool) || refusedTools.includes(tool)),
        ...mutatingTools.filter((tool) => refusedTools.includes(tool)),
      ]);
      const duplicateContractTools = {
        registered_live_tools: duplicateStrings(rawRegisteredTools),
        read_only_tools: duplicateStrings(rawReadOnlyTools),
        mutating_tools: duplicateStrings(rawMutatingTools),
        refused_tools: duplicateStrings(rawRefusedTools),
      };
      if (Object.values(duplicateContractTools).some((entries) => entries.length > 0)) {
        add('tool_contract', 'tool_contract_contains_duplicates', duplicateContractTools);
      }

      if (liveTools !== null) compare('materialized_registry', 'registered_tools_differ_from_live', liveTools, registeredTools);
      compare('materialized_registry', 'registered_tools_differ_from_fabric', fabricTools, registeredTools);
      if (catalog) compare('materialized_registry', 'registered_tools_differ_from_catalog', catalog.tools, registeredTools);
      compare('tool_contract', 'tool_contract_partition_incomplete', registeredTools, contractUnion);
      if (overlaps.length > 0) add('tool_contract', 'tool_contract_partition_overlap', { overlapping_tools: overlaps });
      if (refusedTools.length > 0) add('tool_contract', 'tool_contract_contains_external_refusals', { refused_tools: refusedTools });
      if (liveReadOnlyTools !== null) {
        compare('tool_contract', 'read_only_classification_differ_from_live', liveReadOnlyTools, readOnlyTools);
      }
      if (liveMutatingTools !== null) compare('tool_contract', 'mutating_classification_differ_from_live', liveMutatingTools, mutatingTools);

      const expectedSurface = registrySurfaceForFabricServer(site, server);
      for (const field of ['surface_id', 'display_name', 'server_name', 'authority_boundary', 'client_config', 'catalog_surface_id']) {
        if (canonicalJson(actualSurface[field]) !== canonicalJson(asRecord(expectedSurface)[field])) {
          add('materialized_registry', 'registry_surface_projection_drift', { field });
        }
      }
    }

    if (includeOk || surfaceViolations.length > 0) {
      surfaceResults.push({
        surface_id: actualSurface?.surface_id ?? `${server.server_key}.local`,
        server_name: server.server_key,
        catalog_surface_id: catalog?.id ?? catalogSurfaceId,
        status: surfaceViolations.length === 0 ? 'ok' : 'drift',
        violation_count: surfaceViolations.length,
        violations: surfaceViolations,
      });
    }
  }

  const outputReaderCheck = checkOutputReaderClosureForRegistry(registry, {
    site_id: site.site_id,
    site_root: site.root,
    registry_path: materializedSurfaceRegistryPathForRoot(site.root),
  });
  for (const rawViolation of Array.isArray(outputReaderCheck.violations) ? outputReaderCheck.violations : []) {
    const violation = { layer: 'output_reader_closure', code: 'output_reader_closure_violation', ...asRecord(rawViolation) };
    violations.push(violation);
  }

  return {
    schema: 'narada.registrar.site_registry_conformance_check.v1',
    status: violations.length > 0 ? 'drift' : unobservedServerNames.length > 0 ? 'incomplete' : 'ok',
    site_id: site.site_id,
    site_root: site.root,
    registry_path: materializedSurfaceRegistryPathForRoot(site.root),
    checked_surface_count: fabricServers.length,
    observed_surface_count: Object.keys(observedToolsInput).length,
    observation_coverage: {
      status: observedServerKeys.size === 0 ? 'missing' : unobservedServerNames.length > 0 ? observationCoverage : 'complete',
      observed_server_names: [...observedServerKeys].sort(),
      unobserved_server_names: unobservedServerNames.sort(),
    },
    violation_count: violations.length,
    violations,
    surfaces: surfaceResults,
    output_reader_closure: outputReaderCheck,
  };
}

function registrarSiteRegistryConformanceCheck(args: JsonRecord): JsonRecord {
  const siteId = requiredString(args.site_id, 'registrar_requires_site_id');
  const site = lookupSite(siteId);
  const observationRef = requiredString(args.observation_ref, 'registrar_requires_observation_ref');
  const registryPath = materializedSurfaceRegistryPathForRoot(site.root);
  if (!existsSync(registryPath)) {
    throw diagnosticError('registrar_site_surface_registry_not_found', `registrar_site_surface_registry_not_found:${registryPath}`, { site_id: siteId, registry_path: registryPath });
  }
  let registry: JsonRecord;
  try {
    registry = asRecord(JSON.parse(readFileSync(registryPath, 'utf8')));
  } catch (error) {
    throw diagnosticError('registrar_site_surface_registry_parse_failed', `registrar_site_surface_registry_parse_failed:${registryPath}`, {
      site_id: siteId,
      registry_path: registryPath,
      error: error instanceof Error ? error.message : String(error),
    });
  }
  return checkSiteRegistryConformanceFromObservation(
    site,
    registry,
    observationRef,
    args.include_ok === true,
  );
}

export function checkSiteRegistryConformanceFromObservation(
  site: SiteDef,
  registry: JsonRecord,
  observationRef: string,
  includeOk = false,
): JsonRecord {
  const shown = asRecord(payloadShow({ siteRoot: site.root, args: { ref: observationRef } }));
  if (shown.created_by !== 'mcp-loader-mcp' || !String(shown.payload_id ?? '').startsWith('site-tools-')) {
    throw diagnosticError('registrar_inventory_observation_lineage_mismatch', 'registrar_inventory_observation_lineage_mismatch', {
      expected_declared_creator: 'mcp-loader-mcp',
      actual_declared_creator: shown.created_by ?? null,
      payload_id: shown.payload_id ?? null,
      assurance: 'declarative_lineage_guard_not_cryptographic_provenance',
    });
  }
  const observation = validateSiteToolInventoryObservation(site, asRecord(shown.payload));
  const result = checkSiteRegistryConformance(
    site,
    registry,
    asRecord(observation.observed_tools),
    asRecord(observation.observed_read_only_tools),
    asRecord(observation.observed_mutating_tools),
    includeOk,
  );
  return {
    ...result,
    observation_ref: observationRef,
    observation_sha256: shown.sha256 ?? null,
    observation_created_at: shown.created_at ?? null,
    observation_status: observation.status ?? null,
    observation_observed_at: observation.observed_at ?? null,
    observation_lineage: {
      declared_creator: shown.created_by ?? null,
      payload_id: shown.payload_id ?? null,
      assurance: 'declarative_lineage_guard_not_cryptographic_provenance',
      authority_effect: 'none',
    },
  };
}

export function validateSiteToolInventoryObservation(site: SiteDef, observation: JsonRecord): JsonRecord {
  if (observation.schema !== 'narada.mcp_loader.site_tool_inventory_check.v1') {
    throw diagnosticError('registrar_inventory_observation_schema_mismatch', 'registrar_inventory_observation_schema_mismatch', {
      expected: 'narada.mcp_loader.site_tool_inventory_check.v1',
      actual: observation.schema ?? null,
    });
  }
  if (portablePath(String(observation.site_root ?? '')) !== portablePath(site.root)) {
    throw diagnosticError('registrar_inventory_observation_site_mismatch', 'registrar_inventory_observation_site_mismatch', {
      expected_site_root: site.root,
      actual_site_root: observation.site_root ?? null,
    });
  }
  for (const field of ['observed_tools', 'observed_read_only_tools', 'observed_mutating_tools']) {
    if (!observation[field] || typeof observation[field] !== 'object' || Array.isArray(observation[field])) {
      throw diagnosticError('registrar_inventory_observation_field_missing', `registrar_inventory_observation_field_missing:${field}`, { field });
    }
  }
  return observation;
}

type OutputReaderClosureContext = {
  site_id?: string;
  site_root?: string;
  registry_path?: string;
};

export function checkOutputReaderClosureForRegistry(registry: JsonRecord, context: OutputReaderClosureContext = {}): JsonRecord {
  const rawSurfaces = asRecord(registry).surfaces;
  const violations: JsonRecord[] = [];
  let producerRuleCount = 0;
  if (!Array.isArray(rawSurfaces)) {
    violations.push({
      site_id: context.site_id ?? null,
      site_root: context.site_root ?? null,
      registry_path: context.registry_path ?? null,
      surface_id: null,
      server_name: null,
      catalog_surface_id: null,
      producer_tool: null,
      required_reader_tool: null,
      violation: 'invalid_registry_surfaces',
    });
  } else {
    for (const rawSurface of rawSurfaces) {
      const surface = asRecord(rawSurface);
      const registeredTools = new Set(uniqueStrings(Array.isArray(surface.registered_live_tools) ? surface.registered_live_tools : []));
      const toolContract = asRecord(surface.tool_contract);
      const readOnlyTools = new Set(uniqueStrings(Array.isArray(toolContract.read_only_tools) ? toolContract.read_only_tools : []));
      const outputReaderClosure = outputReaderClosureForRegistrySurface(surface);
      producerRuleCount += Object.keys(outputReaderClosure).length;
      for (const [producerTool, requiredReaderTool] of Object.entries(outputReaderClosure)) {
        if (!registeredTools.has(producerTool)) continue;
        const base = {
          site_id: context.site_id ?? null,
          site_root: context.site_root ?? null,
          registry_path: context.registry_path ?? null,
          surface_id: String(surface.surface_id ?? ''),
          server_name: String(surface.server_name ?? ''),
          catalog_surface_id: String(surface.catalog_surface_id ?? ''),
          producer_tool: producerTool,
          required_reader_tool: requiredReaderTool,
        };
        if (!registeredTools.has(requiredReaderTool)) {
          violations.push({ ...base, violation: 'missing_registered_live_tool' });
        }
        if (!readOnlyTools.has(requiredReaderTool)) {
          violations.push({ ...base, violation: 'missing_read_only_admission' });
        }
      }
    }
  }
  return {
    schema: 'narada.registrar.output_reader_closure_check.v1',
    status: violations.length > 0 ? 'drift' : 'ok',
    site_id: context.site_id ?? null,
    site_root: context.site_root ?? null,
    registry_path: context.registry_path ?? null,
    checked_surface_count: Array.isArray(rawSurfaces) ? rawSurfaces.length : 0,
    producer_rule_count: producerRuleCount,
    violation_count: violations.length,
    violations,
  };
}

function outputReaderClosureForRegistrySurface(surface: JsonRecord): Record<string, string> {
  const catalogSurfaceId = typeof surface.catalog_surface_id === 'string' ? surface.catalog_surface_id : '';
  const registeredTools = uniqueStrings(Array.isArray(surface.registered_live_tools) ? surface.registered_live_tools : []);
  const resolvedCatalogSurface = catalogSurfaceId ? (catalogSurface(catalogSurfaceId) ?? catalogSurfaceAlias(catalogSurfaceId)) : null;
  if (resolvedCatalogSurface?.output_reader_closure) return resolvedCatalogSurface.output_reader_closure;
  const inferredSurface = SURFACES.find((candidate) => {
    if (!candidate.output_reader_closure) return false;
    return Object.keys(candidate.output_reader_closure).some((producerTool) => registeredTools.includes(producerTool));
  });
  return inferredSurface?.output_reader_closure ?? {};
}

function materializedSurfaceRegistryPathForRoot(siteRoot: string): string {
  const resolvedRoot = resolve(siteRoot);
  const normalizedRoot = resolvedRoot.replace(/\\/g, '/');
  const controlRoot = normalizedRoot.endsWith('/.narada') ? resolvedRoot : join(resolvedRoot, '.narada');
  return join(controlRoot, 'capabilities', 'mcp-surfaces.json');
}

function knownSiteIdForRoot(siteRoot: string): string | null {
  const requested = portablePath(siteRoot);
  const match = siteCatalogForOperations().find((site) => {
    const siteRoot = portablePath(site.root);
    const controlRoot = portablePath(siteCapabilityRoot(site));
    return requested === siteRoot || requested === controlRoot;
  });
  return match?.site_id ?? null;
}

function registrarSiteOutputReaderClosureCheck(args: JsonRecord): JsonRecord {
  const includeOk = args.include_ok === true;
  const requested: OutputReaderClosureContext[] = [];
  const seenRegistryPaths = new Set<string>();

  function add(siteRoot: string, siteId?: string): void {
    const registryPath = materializedSurfaceRegistryPathForRoot(siteRoot);
    const normalizedRegistryPath = portablePath(registryPath);
    if (seenRegistryPaths.has(normalizedRegistryPath)) return;
    seenRegistryPaths.add(normalizedRegistryPath);
    requested.push({
      site_id: siteId ?? knownSiteIdForRoot(siteRoot) ?? undefined,
      site_root: resolve(siteRoot),
      registry_path: registryPath,
    });
  }

  const siteIds = uniqueStrings([
    ...(args.site_id ? [args.site_id] : []),
    ...(Array.isArray(args.site_ids) ? args.site_ids : []),
  ]);
  for (const siteId of siteIds) {
    const site = lookupSite(siteId);
    add(site.root, site.site_id);
  }

  const siteRoots = uniqueStrings([
    ...(args.site_root ? [args.site_root] : []),
    ...(Array.isArray(args.site_roots) ? args.site_roots : []),
  ]);
  for (const siteRoot of siteRoots) add(siteRoot);

  if (requested.length === 0) {
    throw diagnosticError('registrar_requires_site_for_output_reader_closure_check', 'registrar_requires_site_for_output_reader_closure_check', {
      expected: 'Pass site_id, site_ids, site_root, or site_roots.',
    });
  }

  const sites: JsonRecord[] = [];
  const violations: JsonRecord[] = [];
  let missingCount = 0;
  let driftCount = 0;
  let checkedSurfaceCount = 0;

  for (const context of requested) {
    const registryPath = requiredString(context.registry_path, 'registrar_internal_missing_registry_path');
    if (!existsSync(registryPath)) {
      missingCount++;
      const missing = {
        status: 'missing',
        site_id: context.site_id ?? null,
        site_root: context.site_root ?? null,
        registry_path: registryPath,
        violation: 'missing_registry',
      };
      sites.push(missing);
      continue;
    }
    const registry = readJsonFile(registryPath);
    if (!registry) {
      driftCount++;
      const invalid = {
        status: 'drift',
        site_id: context.site_id ?? null,
        site_root: context.site_root ?? null,
        registry_path: registryPath,
        violation: 'invalid_registry_json',
      };
      sites.push(invalid);
      violations.push(invalid);
      continue;
    }
    const check = checkOutputReaderClosureForRegistry(registry, context);
    checkedSurfaceCount += Number(check.checked_surface_count ?? 0);
    if (check.status === 'drift') driftCount++;
    violations.push(...((check.violations as JsonRecord[] | undefined) ?? []));
    if (check.status !== 'ok' || includeOk) sites.push(check);
  }

  return {
    schema: 'narada.registrar.site_output_reader_closure_check.v1',
    status: driftCount > 0 ? 'drift' : missingCount > 0 ? 'missing' : 'ok',
    checked_site_count: requested.length,
    checked_surface_count: checkedSurfaceCount,
    missing_count: missingCount,
    drift_count: driftCount,
    violation_count: violations.length,
    violations,
    sites,
  };
}

function registrarSiteList(_args: JsonRecord): JsonRecord {
  const catalog = readSiteRegistryCatalog();
  if (catalog.status === 'ready') {
    return {
      items: catalog.items,
      count: catalog.items.length,
      catalog_source: 'user_site_site_registry',
      registry_path: catalog.path,
      compatibility_fallback_used: false,
    };
  }
  return {
    items: KNOWN_SITES,
    count: KNOWN_SITES.length,
    catalog_source: 'legacy_compatibility_catalog',
    registry_path: catalog.path,
    compatibility_fallback_used: true,
    catalog_error: catalog.error ?? 'registry_unavailable',
  };
}

function registrarSiteSurfaces(args: JsonRecord): JsonRecord {
  const siteId = requiredString(args.site_id, 'registrar_requires_site_id');
  const site = lookupSite(siteId);
  const configDir = join(siteMcpControlRoot(site), '.ai', 'mcp');
  if (!existsSync(configDir)) return { site_id: siteId, surfaces: [], count: 0 };
  const files = readdirSync(configDir).filter((f: string) => f.endsWith('.json'));
  const allFound: string[] = [];
  for (const file of files) {
    try {
      const content = readFileSync(join(configDir, file), 'utf8');
      const cfg = JSON.parse(content);
      const servers = asRecord(cfg.mcpServers);
      for (const surfaceId of SURFACES.map((s) => s.id)) {
        const canonicalKey = siteSurfaceServerKey(siteId, surfaceId);
        if (servers[canonicalKey] && !allFound.includes(surfaceId)) allFound.push(surfaceId);
      }
    } catch { /* skip */ }
  }
  return { site_id: siteId, surfaces: allFound, count: allFound.length };
}

export function siteSurfaceServerKey(siteId: string, surfaceId: string): string {
  return `${siteSurfacePrefix(siteId)}-${surfaceId}`;
}

function siteSurfacePrefix(siteId: string): string {
  // User Site uses an explicit kind boundary so its key cannot resemble a retired alias.
  if (siteId === 'andrey-user') return 'narada-site-andrey-user';
  return siteId.startsWith('narada-') ? siteId : 'narada-' + siteId;
}

export function buildSiteBindConfig(site: SiteDef, surface: RegistrarSurfaceRecord, projectionId?: string | null, runtimeKind?: McpRuntimeKind): { fileName: string; serverKey: string; config: JsonRecord } {
  const siteId = site.site_id;
  const surfaceId = surface.id;
  const selected = selectSurfaceProjection(surfaceId, projectionId, runtimeKind, {
    requireExplicit: Boolean(surface.projections && surface.projections.length > 1) && projectionId === undefined && runtimeKind === undefined,
  });
  const projection = surface.projections?.length
    ? selected.projection
    : { ...selected.projection, args: undefined };
  const serverKey = siteSurfaceServerKey(siteId, surfaceId);
  const fileName = `${siteSurfacePrefix(siteId)}-${surfaceId}-mcp.json`;
  const siteRoot = canonicalWorkspaceRoot(site.root);
  const workspaceRoot = canonicalWorkspaceRoot(siteWorkspaceRoot(site));
  const paths = sitePathInterpolation(siteRoot, workspaceRoot);
  const resolvedArgs = [
    ...(projection.args ?? surface.args).map((arg) => interpolateArg(arg, siteId, paths)),
    ...projectionLaunchArgs(projection),
  ];
  const resolvedEntrypoint = resolveEntrypoint(surface, siteId, siteRoot, projection);
  const scopeMetadata = surfaceScopeMetadata(surfaceId, siteRoot, projection.id);
  const naradaScope = naradaScopeMetadata(surfaceId, siteRoot, siteId, projection.id);
  if (surfaceId === 'sop') appendSopsDirs(resolvedArgs);
  const launch = carrierLaunchCommand({
    kind: 'shared',
    entrypoint: resolvedEntrypoint,
    command: projection.command,
    args: resolvedArgs,
    surface,
    projection,
    surface_implementation: site.surface_overrides?.[surfaceId]?.surface_implementation,
    ...scopeMetadata,
    narada_scope: naradaScope,
  }, surfaceId);

  return {
    fileName,
    serverKey,
    config: {
      schema: 'narada.mcp.client_config.v0',
      site_id: siteId,
      description: `${surface.package} MCP surface bound by registrar.`,
      mcpServers: {
        [serverKey]: {
          transport: 'stdio',
          command: launch.command,
          args: launch.args,
          tools: surface.tools,
          env_vars: projectionEnvVars(surface, projection),
          surface_id: surfaceId,
          projection_id: projection.id,
          surface_projection: projectionMetadata(surfaceId, projection.id, runtimeKind),
          authority_posture: scopeMetadata.injection_scope === 'local_site' ? 'site_local_mcp_surface' : `${scopeMetadata.injection_scope}_injected_mcp_surface`,
          ...scopeMetadata,
          bound_into_site: siteId,
          narada_scope: naradaScope,
        },
      },
    },
  };
}

function siteWorkspaceRoot(site: SiteDef): string {
  try {
    const config = asRecord(JSON.parse(readFileSync(site.config_path, 'utf8')));
    const nestedSite = asRecord(config.site);
    const configured = optionalString(config.workspace_root) ?? optionalString(nestedSite.workspace_root);
    if (configured) return configured;
  } catch { /* fall back to the Site root */ }
  return site.root;
}

function registrarSiteBind(args: JsonRecord): JsonRecord {
  const siteId = requiredString(args.site_id, 'registrar_requires_site_id');
  const surfaceId = requiredString(args.surface_id, 'registrar_requires_surface_id');
  const projectionId = optionalString(args.projection_id);
  const runtimeKind = optionalRuntimeKind(args.runtime_kind);
  const site = lookupSite(siteId);
  const surface = lookupSurface(surfaceId);
  const configDir = join(siteMcpControlRoot(site), '.ai', 'mcp');
  const sidecarRefusal = siteBindSidecarRefusal(site, surfaceId, args);
  if (sidecarRefusal) return sidecarRefusal;
  mkdirSync(configDir, { recursive: true });
  const { fileName, serverKey, config } = buildSiteBindConfig(site, surface, projectionId, runtimeKind);
  const filePath = join(configDir, fileName);
  writeFileSync(filePath, JSON.stringify(config, null, 2) + '\n', 'utf8');
  const registry = writeSiteSurfaceRegistry(site);
  return {
    status: 'bound',
    site_id: siteId,
    surface_id: surfaceId,
    projection_id: asRecord(asRecord(asRecord(config.mcpServers)[serverKey]).surface_projection).projection_id,
    file: fileName,
    server_key: serverKey,
    registry,
  };
}

function registrarSiteUnbind(args: JsonRecord): JsonRecord {
  const siteId = requiredString(args.site_id, 'registrar_requires_site_id');
  const surfaceId = requiredString(args.surface_id, 'registrar_requires_surface_id');
  const site = lookupSite(siteId);
  const configDir = join(siteMcpControlRoot(site), '.ai', 'mcp');
  if (!existsSync(configDir)) return { status: 'not_found', site_id: siteId, surface_id: surfaceId };
  const files = readdirSync(configDir).filter((f: string) => f.endsWith('.json'));
  const serverKey = siteSurfaceServerKey(siteId, surfaceId);
  let removed = 0;
  for (const file of files) {
    try {
      const content = readFileSync(join(configDir, file), 'utf8');
      const cfg = JSON.parse(content);
      const servers = asRecord(cfg.mcpServers);
      if (servers[serverKey]) {
        unlinkSync(join(configDir, file));
        removed++;
        const registry = writeSiteSurfaceRegistry(site);
        return { status: 'unbound', site_id: siteId, surface_id: surfaceId, file, registry };
      }
    } catch { /* skip */ }
  }
  return { status: 'not_bound', site_id: siteId, surface_id: surfaceId };
}

function registrarCarrierList(_args: JsonRecord): JsonRecord {
  return { items: CARRIERS, count: CARRIERS.length };
}

function registrarSurfaceUsage(args: JsonRecord): JsonRecord {
  const surfaceId = requiredString(args.surface_id, 'registrar_requires_surface_id');
  const isLocal = surfaceId.endsWith('.local');
  const matchingSites: { site_id: string; via: 'shared' | 'local' }[] = [];
  const matchingCarriers: { carrier_id: string; kind: CarrierDef['kind']; via: 'shared' | 'local'; site_id: string }[] = [];

  for (const site of siteCatalogForOperations()) {
    if (!isLocal) {
      const fabricSurfaceIds = new Set(discoverSiteMcpFabric(site).map((server) => server.surface_id ?? fabricSurfaceId(server.server_key, site)));
      if (site.surfaces.includes(surfaceId) || fabricSurfaceIds.has(surfaceId)) {
        matchingSites.push({ site_id: site.site_id, via: 'shared' });
      }
    }
    // Site-local surface: check config.json allowed_mcp_entrypoints
    const locals = readSiteConfig(site);
    if (locals.some((l) => l.surface_id === surfaceId)) {
      matchingSites.push({ site_id: site.site_id, via: 'local' });
    }
  }

  for (const carrier of CARRIERS) {
    for (const binding of carrier.site_bindings) {
      const site = lookupSite(binding.site_id);
      const sharedIds = sharedSurfaceIdsForBinding(binding);
      if (!isLocal && sharedIds.includes(surfaceId)) {
        matchingCarriers.push({ carrier_id: carrier.carrier_id, kind: carrier.kind, via: 'shared', site_id: binding.site_id });
      }
      if (isLocal || binding.surfaces === 'all') {
        const locals = readSiteConfig(site);
        for (const local of locals) {
          if (local.surface_id !== surfaceId) continue;
          if (binding.surfaces !== 'all' && !binding.surfaces.includes(local.surface_id)) continue;
          matchingCarriers.push({ carrier_id: carrier.carrier_id, kind: carrier.kind, via: 'local', site_id: binding.site_id });
        }
      }
    }
  }

  // Dedupe carriers
  const carrierMap = new Map<string, typeof matchingCarriers[0]>();
  for (const c of matchingCarriers) carrierMap.set(`${c.carrier_id}:${c.site_id}:${c.via}`, c);
  const dedupedCarriers = Array.from(carrierMap.values());

  return {
    surface_id: surfaceId,
    is_local: isLocal,
    sites: matchingSites,
    carriers: dedupedCarriers,
    site_count: matchingSites.length,
    carrier_count: dedupedCarriers.length,
  };
}

type SiteMcpFabricServer = {
  server_key: string;
  command: string;
  args: string[];
  entrypoint: string;
  launch_entrypoint: string;
  uses_runtime_proxy: boolean;
  child_invocation_kind?: 'entrypoint' | 'native_applet' | 'native_entrypoint';
  child_applet?: string;
  surface_id?: string;
  projection_id?: string;
  runtime_kind?: McpRuntimeKind;
  runtime_requirements?: McpRuntimeKind[];
  narada_scope: NaradaScopeMetadata;
  source_file: string;
  projection_kind: 'site_fabric' | 'carrier_projection';
};

function unwrapRuntimeProxyLaunch(entrypoint: string, args: string[]): { entrypoint: string; args: string[]; usesRuntimeProxy: boolean; launchEntrypoint: string; childInvocationKind?: 'entrypoint' | 'native_applet' | 'native_entrypoint'; childApplet?: string } {
  const launchEntrypoint = entrypoint;
  if (portablePath(entrypoint) !== portablePath(MCP_RUNTIME_PROXY_ENTRYPOINT)
    && !isNativeArtifactEntrypoint(MCP_RUNTIME_PROXY_PACKAGE_ROOT, 'narada-mcp-runtime.exe', entrypoint)) {
    return { entrypoint, args, usesRuntimeProxy: false, launchEntrypoint };
  }
  const entrypointIndex = args.indexOf('--entrypoint');
  const separatorIndex = args.indexOf('--');
  const childEntrypoint = entrypointIndex >= 0 ? args[entrypointIndex + 1] : '';
  const childArgs = separatorIndex >= 0 ? args.slice(separatorIndex + 1) : [];
  const invocationIndex = args.indexOf('--child-invocation-kind');
  const appletIndex = args.indexOf('--child-applet');
  const childInvocationValue = invocationIndex >= 0 ? args[invocationIndex + 1] : undefined;
  const childInvocationKind = childInvocationValue === 'native_applet' || childInvocationValue === 'native_entrypoint' ? childInvocationValue : 'entrypoint';
  const childApplet = appletIndex >= 0 ? args[appletIndex + 1] : undefined;
  return { entrypoint: childEntrypoint, args: childArgs, usesRuntimeProxy: true, launchEntrypoint, childInvocationKind, ...(childApplet ? { childApplet } : {}) };
}

function portablePath(path: string): string {
  return resolve(path).replace(/\\/g, '/');
}

function surfaceRequiresSiteRoot(surfaceId: string): boolean {
  return ['agent-context', 'task-lifecycle', 'site-inbox', 'site-loop', 'mailbox', 'graph-mail', 'delegated-task'].includes(surfaceId);
}

function siteMcpControlRoot(site: SiteDef): string {
  if (site.root.replace(/\\/g, '/').endsWith('/.narada')) return site.root;
  if (existsSync(join(site.root, '.ai', 'mcp'))) return site.root;
  const nestedControlRoot = sitePathInterpolation(site.root).siteControlRoot;
  if (existsSync(join(nestedControlRoot, '.ai', 'mcp'))) return nestedControlRoot;
  return site.root;
}

function siteCapabilityRoot(site: SiteDef): string {
  const normalizedRoot = resolve(site.root);
  return normalizedRoot.replace(/\\/g, '/').endsWith('/.narada')
    ? normalizedRoot
    : join(normalizedRoot, '.narada');
}

function discoverSiteMcpFabric(site: SiteDef): SiteMcpFabricServer[] {
  const controlRoot = siteMcpControlRoot(site);
  const configDir = join(controlRoot, '.ai', 'mcp');
  if (!existsSync(configDir)) return [];
  return discoverMcpConfigDirectory(site, controlRoot, configDir, 'site_fabric');
}

function discoverSiteCarrierProjections(site: SiteDef): SiteMcpFabricServer[] {
  const controlRoot = siteMcpControlRoot(site);
  const configDir = join(controlRoot, '.ai', 'mcp', 'carriers');
  if (!existsSync(configDir)) return [];
  return discoverMcpConfigDirectory(site, controlRoot, configDir, 'carrier_projection');
}

function discoverMcpConfigDirectory(
  site: SiteDef,
  controlRoot: string,
  configDir: string,
  projectionKind: SiteMcpFabricServer['projection_kind'],
): SiteMcpFabricServer[] {
  const servers: SiteMcpFabricServer[] = [];
  for (const file of readdirSync(configDir)) {
    if (!file.endsWith('.json')) continue;
    const filePath = join(configDir, file);
    let content: string;
    try {
      content = readFileSync(filePath, 'utf8');
    } catch {
      continue;
    }
    let cfg: JsonRecord;
    try {
      cfg = JSON.parse(content);
    } catch {
      continue;
    }
    const mcpServers = asRecord(cfg.mcpServers);
    for (const [serverKey, rawServer] of Object.entries(mcpServers)) {
      const server = asRecord(rawServer);
      const surfaceId = fabricSurfaceId(serverKey, site);
      const surfaceProjection = asRecord(server.surface_projection);
      let command = server.command ?? 'node';
      let args: string[] = [];
      if (Array.isArray(command)) {
        args = command.slice(2).map(String);
        command = command.slice(0, 2);
      } else {
        args = Array.isArray(server.args) ? server.args.map(String) : [];
      }
      let entrypoint = '';
      if (Array.isArray(command) && command.length >= 2) {
        entrypoint = String(command[1]);
      } else if (args.length > 0) {
        entrypoint = args[0];
        args = args.slice(1);
      }
      // Handle entrypoint that is itself a command with flags (e.g. node --import tsx path)
      if (entrypoint === 'node' && args.length >= 3 && args[0] === '--import') {
        args.shift(); // --import
        args.shift(); // tsx
        entrypoint = args.shift() ?? '';
      } else if (entrypoint === '--import' && args.length >= 2 && args[0] === 'tsx') {
        args.shift(); // tsx
        entrypoint = args.shift() ?? '';
      }
      // Materialized fabric is executable data, never a template. Validation reports any token verbatim.
      const unwrapped = unwrapRuntimeProxyLaunch(entrypoint, args);
      servers.push({
        server_key: serverKey,
        command: Array.isArray(command) ? String(command[0] ?? 'node') : String(command),
        args: unwrapped.args,
        entrypoint: unwrapped.entrypoint,
        launch_entrypoint: unwrapped.launchEntrypoint,
        uses_runtime_proxy: unwrapped.usesRuntimeProxy,
        child_invocation_kind: unwrapped.childInvocationKind,
        child_applet: unwrapped.childApplet,
        surface_id: server.surface_id ? String(server.surface_id) : undefined,
        projection_id: optionalString(server.projection_id) ?? optionalString(surfaceProjection.projection_id) ?? undefined,
        runtime_kind: surfaceProjection.runtime_kind === 'nars' ? 'nars' : undefined,
        runtime_requirements: Array.isArray(surfaceProjection.runtime_requirements)
          ? surfaceProjection.runtime_requirements.filter((value): value is McpRuntimeKind => value === 'nars')
          : undefined,
        narada_scope: readNaradaScope(server, surfaceId, controlRoot, site.site_id),
        source_file: projectionKind === 'carrier_projection' ? `carriers/${file}` : file,
        projection_kind: projectionKind,
      });
    }
  }
  return servers;
}

function fabricSurfaceId(serverKey: string, site: SiteDef): string {
  const canonicalPrefix = siteSurfacePrefix(site.site_id);
  if (serverKey.startsWith(`${canonicalPrefix}-`)) {
    const rest = serverKey.slice(canonicalPrefix.length + 1);
    const known = SURFACES.find((s) => s.id === rest);
    if (known) return known.id;
    const alias = catalogSurfaceAlias(rest);
    if (alias) return alias.id;
  }
  const prefix = site.site_id.replace('narada-', '');
  if (serverKey.startsWith(`${prefix}-`)) {
    const rest = serverKey.slice(prefix.length + 1);
    const known = SURFACES.find((s) => s.id === rest);
    if (known) return known.id;
    const alias = catalogSurfaceAlias(rest);
    if (alias) return alias.id;
  }
  return serverKey;
}

function catalogSurfaceAlias(surfaceId: string): RegistrarSurfaceRecord | undefined {
  if (surfaceId === 'inbox') return catalogSurface('site-inbox');
  return undefined;
}

function catalogSurfaceForFabricServer(site: SiteDef, server: SiteMcpFabricServer): RegistrarSurfaceRecord | undefined {
  const declaredSurfaceId = server.surface_id ?? fabricSurfaceId(server.server_key, site);
  const declaredSurface = catalogSurface(declaredSurfaceId) ?? catalogSurfaceAlias(declaredSurfaceId);
  if (declaredSurface) return declaredSurface;
  return SURFACES.find((surface) => portablePath(surface.entrypoint) === portablePath(server.entrypoint));
}

type SiteSurfaceRegistrySurface = {
  surface_id: string;
  surface_projection: JsonRecord;
  surface_type: string;
  display_name: string;
  server_name: string;
  runtime_binding: {
    runtime_kind: 'node-stdio' | 'bun-stdio';
    proxy_implementation: RuntimeProxyImplementation | null;
    entrypoint: string;
    owner_site_id: string;
    transport: {
      type: 'stdio';
      command: string;
      args: string[];
    };
  };
  authority_boundary: JsonRecord;
  client_config: JsonRecord;
  tool_contract: {
    exposed_tools: string[];
    semantic_operations: string[];
    deprecated_aliases: Record<string, string>;
    read_only_tools: string[];
    mutating_tools: string[];
    refused_tools: string[];
  };
  registered_live_tools: string[];
  catalog_surface_id: string;
  evidence: JsonRecord;
};

function runtimeBindingForFabricServer(site: SiteDef, server: SiteMcpFabricServer): SiteSurfaceRegistrySurface['runtime_binding'] {
  const surfaceId = server.surface_id ?? fabricSurfaceId(server.server_key, site);
  const transportArgs = server.uses_runtime_proxy
    ? [
      ...(runtimeProxyImplementation === 'native' ? ['proxy'] : [server.launch_entrypoint]),
      '--surface-id',
      surfaceId,
      '--child-command',
      server.command,
      '--artifact-manifest',
      MCP_WORKSPACE_ARTIFACT_MANIFEST,
      '--runtime-contract-version',
      String(MCP_RUNTIME_CONTRACT_VERSION),
      '--entrypoint',
      server.entrypoint,
      ...(server.child_invocation_kind === 'native_applet'
        ? ['--child-invocation-kind', 'native_applet', '--child-applet', server.child_applet ?? 'filesystem']
         : server.child_invocation_kind === 'native_entrypoint'
           ? ['--child-invocation-kind', 'native_entrypoint']
           : []),
      '--',
      ...server.args,
    ]
    : [server.entrypoint, ...server.args];
  return {
    runtime_kind: /(^|[\\/])bun(?:\.exe)?$/i.test(server.command) ? 'bun-stdio' : 'node-stdio',
    proxy_implementation: server.uses_runtime_proxy ? runtimeProxyImplementation : null,
    entrypoint: server.entrypoint,
    owner_site_id: site.site_id,
    transport: {
      type: 'stdio',
      command: server.uses_runtime_proxy && runtimeProxyImplementation === 'native'
        ? nativeRuntimeProxyEntrypoint()
        : server.command,
      args: transportArgs,
    },
  };
}

function registrySurfaceForFabricServer(site: SiteDef, server: SiteMcpFabricServer): SiteSurfaceRegistrySurface {
  const surfaceId = server.surface_id ?? fabricSurfaceId(server.server_key, site);
  const catalog = catalogSurfaceForFabricServer(site, server);
  const surfaceProjection = catalog
    ? projectionMetadata(catalog.id, server.projection_id, server.runtime_kind)
    : {
      surface_id: surfaceId,
      projection_id: server.projection_id ?? 'unknown',
      runtime_requirements: server.runtime_requirements ?? [],
      ...(server.runtime_kind ? { runtime_kind: server.runtime_kind } : {}),
    };
  const registeredTools = uniqueStrings(catalog ? nativeToolNames(catalog.id) : readConfiguredServerTools(site, server));
  const toolContract = surfaceToolContract(catalog?.id ?? surfaceId, registeredTools);
  return {
    surface_id: `${server.server_key}.local`,
    surface_projection: surfaceProjection,
    surface_type: catalog?.kind ?? 'mcp_surface',
    display_name: server.server_key,
    server_name: server.server_key,
    runtime_binding: runtimeBindingForFabricServer(site, server),
    authority_boundary: {
      posture: 'registrar_generated_runtime_surface_registry',
      grants_tool_authority: true,
      granted_tool_authority_kind: 'declared_enabled_mcp_surface_tools',
      source: 'site_mcp_fabric_and_registrar_catalog',
    },
    client_config: {
      generated_path: `.ai/mcp/${server.source_file}`,
      generated_file: server.source_file,
    },
    tool_contract: toolContract,
    registered_live_tools: registeredTools,
    catalog_surface_id: catalog?.id ?? surfaceId,
    evidence: {
      source: 'site_mcp_fabric',
      path: `.ai/mcp/${server.source_file}`,
      projection_kind: server.projection_kind,
    },
  };
}

function readConfiguredServerToolsRaw(site: SiteDef, server: SiteMcpFabricServer): string[] {
  const filePath = join(siteMcpControlRoot(site), '.ai', 'mcp', server.source_file);
  const cfg = readJsonFile(filePath);
  const rawServer = asRecord(asRecord(cfg?.mcpServers)[server.server_key]);
  return Array.isArray(rawServer.tools) ? rawServer.tools.map(String) : [];
}

function readConfiguredServerTools(site: SiteDef, server: SiteMcpFabricServer): string[] {
  return uniqueStrings(readConfiguredServerToolsRaw(site, server));
}

function surfaceToolContract(surfaceId: string, registeredTools: string[]): SiteSurfaceRegistrySurface['tool_contract'] {
  const descriptor = nativeSurfaceDescriptor(surfaceId);
  // readOnlyHint is the cross-surface authority for mutation semantics. A
  // read-only runtime-admin tool may intentionally use a non-read effect class.
  const readOnlyTools = descriptor.tools
    .filter((tool) => tool.annotations?.readOnlyHint === true && registeredTools.includes(tool.name))
    .map((tool) => tool.name);
  const refusedTools = descriptor.tools
    .filter((tool) => tool.annotations?.legacy_policy === 'refused' && registeredTools.includes(tool.name))
    .map((tool) => tool.name);
  const classified = new Set([...readOnlyTools, ...refusedTools]);
  return {
    exposed_tools: [...registeredTools],
    semantic_operations: [],
    deprecated_aliases: {},
    read_only_tools: readOnlyTools,
    mutating_tools: registeredTools.filter((tool) => !classified.has(tool)),
    refused_tools: refusedTools,
  };
}

export function buildSiteSurfaceRegistry(site: SiteDef): JsonRecord {
  const servers = discoverSiteMcpFabric(site);
  const surfaces = servers
    .map((server) => registrySurfaceForFabricServer(site, server))
    .sort((a, b) => a.server_name.localeCompare(b.server_name));
  return {
    schema: 'narada.site.capabilities.mcp_surfaces.v1',
    artifact_role: 'site_capability_surface_registry_not_mcp_client_config',
    site_id: site.site_id,
    generated_by: 'mcp-registrar',
    generated_at: new Date().toISOString(),
    generation_policy: {
      source: '.ai/mcp + registrar surface catalog',
      mode: 'enabled_surface_tool_authority',
      note: 'Every tool exposed by an enabled MCP surface is declared for action admission. The MCP surface remains responsible for command policy and mutation enforcement.',
    },
    surfaces,
  };
}

function writeSiteSurfaceRegistry(site: SiteDef): JsonRecord {
  const registry = buildSiteSurfaceRegistry(site);
  const dir = join(siteCapabilityRoot(site), 'capabilities');
  mkdirSync(dir, { recursive: true });
  const path = join(dir, 'mcp-surfaces.json');
  writeFileSync(path, JSON.stringify(registry, null, 2) + '\n', 'utf8');
  return {
    status: 'synced',
    site_id: site.site_id,
    path,
    surface_count: Array.isArray(registry.surfaces) ? registry.surfaces.length : 0,
    tool_count: Array.isArray(registry.surfaces)
      ? registry.surfaces.reduce((sum, surface) => {
        const tools = asRecord(surface).registered_live_tools;
        return sum + (Array.isArray(tools) ? tools.length : 0);
      }, 0)
      : 0,
  };
}

function registrarSiteSurfaceRegistrySync(args: JsonRecord): JsonRecord {
  const siteId = requiredString(args.site_id, 'registrar_requires_site_id');
  const site = lookupSite(siteId);
  const registry = buildSiteSurfaceRegistry(site);
  if (args.dry_run === true) {
    return {
      status: 'dry_run',
      site_id: siteId,
      path: join(siteCapabilityRoot(site), 'capabilities', 'mcp-surfaces.json'),
      registry,
    };
  }
  return writeSiteSurfaceRegistry(site);
}

export function validateSiteMcpFabric(site: SiteDef, includeOk = false): JsonRecord {
  const siteId = assertCanonicalSiteId(site.site_id);
  const findings: ValidationFinding[] = [];

  function add(severity: ValidationFinding['severity'], code: string, message: string, detail: JsonRecord = {}) {
    findings.push({ severity, code, message, ...detail });
  }

  const servers = discoverSiteMcpFabric(site);
  const carrierProjections = discoverSiteCarrierProjections(site);

  if (servers.length === 0) {
    add('warning', 'registrar_site_fabric_empty', `No MCP servers found in ${join(site.root, '.ai', 'mcp')}`, { site_id: siteId });
  }

  const seenKeys = new Set<string>();
  const seenCanonicalSurfaces = new Map<string, SiteMcpFabricServer>();
  const presentSurfaceIds = new Set<string>();
  for (const server of servers) {
    const surfaceId = fabricSurfaceId(server.server_key, site);
    const canonicalSurface = catalogSurfaceForFabricServer(site, server);
    presentSurfaceIds.add(server.surface_id ?? surfaceId);
    const scopeDetail = scopeFindingDetail(server.narada_scope);
    if (seenKeys.has(server.server_key)) {
      add('error', 'registrar_site_fabric_duplicate_server_key', `Duplicate server key '${server.server_key}' in site fabric`, { site_id: siteId, server_key: server.server_key, source_file: server.source_file, surface_id: surfaceId, ...scopeDetail });
    } else {
      seenKeys.add(server.server_key);
      if (includeOk) { add('info', 'registrar_site_fabric_server_key_ok', `Server key '${server.server_key}' found`, { site_id: siteId, server_key: server.server_key, source_file: server.source_file, surface_id: surfaceId, ...scopeDetail }); }
    }

    if (canonicalSurface) {
      const previous = seenCanonicalSurfaces.get(canonicalSurface.id);
      if (previous) {
        add('error', 'registrar_site_fabric_duplicate_canonical_surface', `Multiple Site fabric entries claim canonical surface '${canonicalSurface.id}'`, {
          site_id: siteId,
          canonical_surface_id: canonicalSurface.id,
          server_key: server.server_key,
          source_file: server.source_file,
          conflicting_server_key: previous.server_key,
          conflicting_source_file: previous.source_file,
          remediation: `Remove the superseded projection from ${join(siteMcpControlRoot(site), '.ai', 'mcp')} and rematerialize from authoritative Site registration.`,
          ...scopeDetail,
        });
      } else {
        seenCanonicalSurfaces.set(canonicalSurface.id, server);
      }
    }

    const unresolvedTemplates = [server.entrypoint, ...server.args].filter((value) => /\{[^}]+\}/.test(value));
    if (unresolvedTemplates.length > 0) {
      add('error', 'registrar_site_fabric_unresolved_template', 'Surface ' + server.server_key + ' contains unresolved materialization tokens', {
        site_id: siteId,
        server_key: server.server_key,
        surface_id: surfaceId,
        source_file: server.source_file,
        unresolved_values: unresolvedTemplates,
        remediation: 'Regenerate the Site fabric from registrar materialization; do not defer placeholder expansion to the loader.',
        ...scopeDetail,
      });
    }

    // Entrypoint existence
    const resolvedEntrypoint = resolve(server.entrypoint);
    if (!existsSync(resolvedEntrypoint)) {
      add('error', 'registrar_site_fabric_missing_entrypoint', `Entrypoint for '${server.server_key}' does not exist: ${resolvedEntrypoint}`, { site_id: siteId, server_key: server.server_key, entrypoint: resolvedEntrypoint, source_file: server.source_file, surface_id: surfaceId, ...scopeDetail });
    } else if (includeOk) {
      add('info', 'registrar_site_fabric_entrypoint_exists', `Entrypoint for '${server.server_key}' exists: ${resolvedEntrypoint}`, { site_id: siteId, server_key: server.server_key, entrypoint: resolvedEntrypoint, source_file: server.source_file, surface_id: surfaceId, ...scopeDetail });
    }
    addRuntimePreflightFindings(add, includeOk, {
      site_id: siteId,
      server_key: server.server_key,
      entrypoint: resolvedEntrypoint,
      source_file: server.source_file,
      surface_id: surfaceId,
      ...scopeDetail,
    }, SURFACES.find((surface) => surface.id === surfaceId) ?? null, server.uses_runtime_proxy);

    // Allowed-root requirement
    if (rootsNeedingAllowedRoot(surfaceId)) {
      const allowedRoots: string[] = [];
      for (let i = 0; i < server.args.length; i++) {
        if (server.args[i] === '--allowed-root' && i + 1 < server.args.length) {
          allowedRoots.push(server.args[i + 1]);
        }
      }
      if (allowedRoots.length === 0) {
        add('error', 'registrar_site_fabric_missing_allowed_root', `Surface '${surfaceId}' requires at least one --allowed-root but '${server.server_key}' has none`, { site_id: siteId, server_key: server.server_key, surface_id: surfaceId, source_file: server.source_file, ...scopeDetail });
      } else if (includeOk) {
        add('info', 'registrar_site_fabric_allowed_root_ok', `Surface '${surfaceId}' on '${server.server_key}' has ${allowedRoots.length} allowed root(s)`, { site_id: siteId, server_key: server.server_key, surface_id: surfaceId, allowed_roots: allowedRoots, source_file: server.source_file, ...scopeDetail });
      }
    }

    // Output-root requirement for local-filesystem
    if (surfaceId === 'local-filesystem' || surfaceId === 'local-filesystem-mcp.local') {
      const hasOutputRoot = server.args.some((a) => a === '--output-root');
      if (!hasOutputRoot) {
        add('warning', 'registrar_site_fabric_missing_output_root', `Filesystem surface '${server.server_key}' is missing --output-root`, { site_id: siteId, server_key: server.server_key, surface_id: surfaceId, source_file: server.source_file, ...scopeDetail });
      } else if (includeOk) {
        add('info', 'registrar_site_fabric_output_root_ok', `Filesystem surface '${server.server_key}' has --output-root`, { site_id: siteId, server_key: server.server_key, surface_id: surfaceId, source_file: server.source_file, ...scopeDetail });
      }
    }

    // Site-root requirement for site-aware surfaces
    if (surfaceRequiresSiteRoot(surfaceId)) {
      const hasSiteRoot = server.args.some((a) => a === '--site-root');
      if (!hasSiteRoot) {
        add('error', 'registrar_site_fabric_missing_site_root', `Surface '${surfaceId}' on '${server.server_key}' is missing --site-root`, { site_id: siteId, server_key: server.server_key, surface_id: surfaceId, source_file: server.source_file, ...scopeDetail });
      } else if (includeOk) {
        add('info', 'registrar_site_fabric_site_root_ok', `Surface '${surfaceId}' on '${server.server_key}' has --site-root`, { site_id: siteId, server_key: server.server_key, surface_id: surfaceId, source_file: server.source_file, ...scopeDetail });
      }
    }
  }

  for (const server of carrierProjections) {
    const surfaceId = server.surface_id ?? fabricSurfaceId(server.server_key, site);
    const authoritative = catalogSurfaceForFabricServer(site, server) ?? null;
    const detail = {
      site_id: siteId,
      server_key: server.server_key,
      surface_id: surfaceId,
      source_file: server.source_file,
      projection_kind: server.projection_kind,
    };
    if (!authoritative) {
      add('error', 'registrar_carrier_projection_unknown_surface', `Carrier projection '${server.server_key}' has no authoritative surface definition`, detail);
      continue;
    }
    const actualEntrypoint = portablePath(server.entrypoint);
    const expectedEntrypoint = portablePath(authoritative.entrypoint);
    if (actualEntrypoint !== expectedEntrypoint) {
      add('error', 'registrar_carrier_projection_entrypoint_drift', `Carrier projection '${server.server_key}' does not use the authoritative '${surfaceId}' entrypoint`, {
        ...detail,
        entrypoint: actualEntrypoint,
        expected_entrypoint: expectedEntrypoint,
        authoritative_package: authoritative.package,
      });
    } else if (includeOk) {
      add('info', 'registrar_carrier_projection_entrypoint_ok', `Carrier projection '${server.server_key}' uses the authoritative '${surfaceId}' entrypoint`, detail);
    }
    if (surfaceRequiresSiteRoot(surfaceId) && !server.args.includes('--site-root')) {
      add('error', 'registrar_carrier_projection_missing_site_root', `Carrier projection '${server.server_key}' is missing required --site-root`, detail);
    }
  }

  for (const surface of SURFACES) {
    if (site.surface_overrides?.[surface.id]?.enabled === false) continue;
    for (const projection of surfaceProjections(surface)) {
      if (projection.injection_scope !== 'local_site' || projection.default_injection !== 'all_site_bound_sessions') continue;
      if (presentSurfaceIds.has(surface.id)) continue;
      const replacements = DEFAULT_SURFACE_REPLACEMENTS[surface.id] ?? [];
      if (replacements.some((replacementId) => presentSurfaceIds.has(replacementId))) continue;
      add('error', 'registrar_site_fabric_missing_default_surface', `Default local Site surface '${surface.id}' is missing from runtime-authoritative Site MCP fabric`, {
        site_id: siteId,
        surface_id: surface.id,
        projection_id: projection.id,
        default_injection: projection.default_injection,
        injection_scope: projection.injection_scope,
        expected_server_key: siteSurfaceServerKey(siteId, surface.id),
        required_repair_locus: { kind: 'local_site', site_root: site.root },
        remediation: `Materialize '${surface.id}' with projection '${projection.id}' into ${join(site.root, '.ai', 'mcp')} before launching Site-bound sessions.`,
      });
    }
  }

  const errors = findings.filter((f) => f.severity === 'error').length;
  const warnings = findings.filter((f) => f.severity === 'warning').length;
  return {
    status: errors > 0 ? 'invalid' : warnings > 0 ? 'valid_with_warnings' : 'valid',
    site_id: siteId,
    server_count: servers.length,
    carrier_projection_count: carrierProjections.length,
    errors,
    warnings,
    findings,
  };
}

async function registrarCarrierBind(args: JsonRecord): Promise<JsonRecord> {
  if (process.env[FRESH_REGISTRAR_ENV] !== '1') {
    return runFreshRegistrarRequest('registrar_carrier_bind', args);
  }
  assertRegistrarProcessCurrent('registrar_carrier_bind');
  const carrierId = requiredString(args.carrier_id, 'registrar_requires_carrier_id');
  const surfaceId = requiredString(args.surface_id, 'registrar_requires_surface_id');
  const projectionId = optionalString(args.projection_id);
  const carrier = lookupCarrier(carrierId);
  const surface = lookupSurface(surfaceId);
  const defaultSiteId = optionalString(args.site_id) ?? 'andrey-user';
  const binding = carrier.site_bindings.find((candidate) => candidate.site_id === defaultSiteId);
  const selected = selectSurfaceProjection(surfaceId, projectionId, binding?.runtime_kind);
  const projection = selected.projection;
  const siteRoot = lookupSite(defaultSiteId).root;

  const resolvedArgs = interpolateArgs(projection.args ?? surface.args, defaultSiteId, siteRoot);
  const resolvedEntrypoint = resolveEntrypoint(surface, defaultSiteId, siteRoot, projection);
  if (surfaceId === 'sop') appendSopsDirs(resolvedArgs);

  const aggregateServerKeys = carrierServerKeysForSurface(carrier, surfaceId);
  if (binding?.loading_mode === 'progressive' && aggregateServerKeys.length === 0) {
    throw diagnosticError(
      'registrar_progressive_surface_bind_refused',
      `registrar_progressive_surface_bind_refused:${carrierId}:${surfaceId}`,
      {
        carrier_id: carrierId,
        site_id: defaultSiteId,
        surface_id: surfaceId,
        loading_mode: binding.loading_mode,
        remediation: 'Use mcp-loader to attach this surface at runtime, or explicitly add it to the progressive bootstrap allowlist before materializing the carrier.',
      },
    );
  }
  if (aggregateServerKeys.length > 0) {
    const materialized = await registrarMaterializeAll(args.runtime_profile ? { runtime_profile: args.runtime_profile } : {});
    return {
      ...materialized,
      status: 'applied',
      surface_id: surfaceId,
      projection_id: projection.id,
      server_keys: aggregateServerKeys,
      binding_model: 'aggregate_carrier_config',
    };
  }

  type CarrierBindPreparation = { result: JsonRecord; content: string; structured: JsonRecord };
  let prepared: CarrierBindPreparation;
  switch (carrier.kind) {
    case 'opencode':
      throw diagnosticError('registrar_single_surface_bind_unsupported_for_opencode_aggregate', 'registrar_single_surface_bind_unsupported_for_opencode_aggregate');
    case 'kimi':
      prepared = kimiBind(carrier.config_path, surfaceId, resolvedEntrypoint, resolvedArgs, defaultSiteId, siteRoot, projection.id);
      break;
    case 'codex':
      prepared = codexBind(carrier.config_path, surfaceId, resolvedEntrypoint, resolvedArgs, defaultSiteId, siteRoot, projection.id);
      break;
    default:
      throw diagnosticError('registrar_unknown_carrier_kind', `registrar_unknown_carrier_kind:${carrier.kind}`);
  }
  const recoveryPlan = buildRecoveryCarrierRuntimeMaterializationPlan(carrier, carrier.config_path, 'bind', surfaceId);
  const finalized = validateCarrierMaterialization(
    carrier,
    { content: prepared.content, structured: prepared.structured },
    carrier.config_path,
    runtimeMaterializationPlan,
    recoveryPlan,
  );
  writeFileAtomic(carrier.config_path, prepared.content);
  writeRuntimeMaterializationPlan(runtimeMaterializationPlanPath(carrier.config_path), recoveryPlan);
  writeMaterializationGeneration(materializationSidecarPath(carrier.config_path), finalized.generation!);
  writeSiteAllowedRootsConfig(carrier);
  return {
    ...prepared.result,
    runtime_contract_version: MCP_RUNTIME_CONTRACT_VERSION,
    materialization_validation: finalized.validation,
    materialization_generation: finalized.generation,
    generation_sidecar_path: materializationSidecarPath(carrier.config_path),
    runtime_materialization_plan: recoveryPlan,
    runtime_materialization_plan_path: runtimeMaterializationPlanPath(carrier.config_path),
    recovery_escape_hatch: true,
  };
}

async function registrarCarrierUnbind(args: JsonRecord): Promise<JsonRecord> {
  if (process.env[FRESH_REGISTRAR_ENV] !== '1') {
    return runFreshRegistrarRequest('registrar_carrier_unbind', args);
  }
  assertRegistrarProcessCurrent('registrar_carrier_unbind');
  const carrierId = requiredString(args.carrier_id, 'registrar_requires_carrier_id');
  const surfaceId = requiredString(args.surface_id, 'registrar_requires_surface_id');
  const carrier = lookupCarrier(carrierId);
  const aggregateServerKeys = carrierServerKeysForSurface(carrier, surfaceId);
  if (aggregateServerKeys.length > 0) {
    throw diagnosticError(
      'registrar_carrier_unbind_refused_aggregate_surface',
      `registrar_carrier_unbind_refused_aggregate_surface:${surfaceId}`,
      {
        carrier_id: carrierId,
        surface_id: surfaceId,
        server_keys: aggregateServerKeys,
        remediation: 'This surface is produced by the aggregate carrier model. Remove it from the carrier site binding/source model, then run registrar_materialize_all.',
      },
    );
  }
  let result: JsonRecord;
  switch (carrier.kind) {
    case 'opencode':
      throw diagnosticError('registrar_single_surface_unbind_unsupported_for_opencode_aggregate', 'registrar_single_surface_unbind_unsupported_for_opencode_aggregate');
    case 'kimi':
      result = kimiUnbind(carrier.config_path, surfaceId);
      break;
    case 'codex':
      result = codexUnbind(carrier.config_path, surfaceId);
      break;
    default:
      throw diagnosticError('registrar_unknown_carrier_kind', `registrar_unknown_carrier_kind:${carrier.kind}`);
  }
  writeSiteAllowedRootsConfig(carrier);
  if (result.status === 'unbound') {
    const content = readFileSync(carrier.config_path, 'utf8');
    const structured = parseCarrierConfig(carrier.kind, content);
    if (!structured) {
      throw diagnosticError('registrar_materialized_config_parse_failed', 'The carrier configuration could not be parsed after unbinding.', { config_path: carrier.config_path });
    }
    const recoveryPlan = buildRecoveryCarrierRuntimeMaterializationPlan(carrier, carrier.config_path, 'unbind', surfaceId);
    const finalized = validateCarrierMaterialization(
      carrier,
      { content, structured },
      carrier.config_path,
      runtimeMaterializationPlan,
      recoveryPlan,
    );
    writeRuntimeMaterializationPlan(runtimeMaterializationPlanPath(carrier.config_path), recoveryPlan);
    writeMaterializationGeneration(materializationSidecarPath(carrier.config_path), finalized.generation!);
    return {
      ...result,
      runtime_contract_version: MCP_RUNTIME_CONTRACT_VERSION,
      materialization_validation: finalized.validation,
      materialization_generation: finalized.generation,
      generation_sidecar_path: materializationSidecarPath(carrier.config_path),
      runtime_materialization_plan: recoveryPlan,
      runtime_materialization_plan_path: runtimeMaterializationPlanPath(carrier.config_path),
      recovery_escape_hatch: true,
    };
  }
  return result;
}

function kimiBind(configPath: string, surfaceId: string, entrypoint: string, resolvedArgs: string[], siteId: string, siteRoot: string, projectionId: string): { result: JsonRecord; content: string; structured: JsonRecord } {
  if (!existsSync(configPath)) throw diagnosticError('registrar_config_not_found', `registrar_config_not_found:${configPath}`);
  const content = readFileSync(configPath, 'utf8');
  const cfg = JSON.parse(content);
  const mcp = asRecord(cfg.mcpServers);
  const serverKey = siteSurfaceServerKey(siteId, surfaceId);
  if (mcp[serverKey]) return { result: { status: 'already_bound', carrier_id: 'kimi-andrey', surface_id: surfaceId, server_key: serverKey }, content, structured: cfg };
  const surface = lookupSurface(surfaceId);
  const launch = carrierLaunchCommand({ kind: 'shared', entrypoint, args: resolvedArgs, surface, projection: selectSurfaceProjection(surfaceId, projectionId).projection, ...naradaScopeMetadata(surfaceId, siteRoot, siteId, projectionId), narada_scope: naradaScopeMetadata(surfaceId, siteRoot, siteId, projectionId) }, surfaceId, configPath);
  mcp[serverKey] = {
    transport: 'stdio',
    command: launch.command,
    args: launch.args,
  };
  const nextContent = JSON.stringify(cfg, null, 2) + '\n';
  return { result: { status: 'bound', carrier_id: 'kimi-andrey', surface_id: surfaceId, server_key: serverKey }, content: nextContent, structured: cfg };
}

function kimiUnbind(configPath: string, surfaceId: string): JsonRecord {
  if (!existsSync(configPath)) throw diagnosticError('registrar_config_not_found', `registrar_config_not_found:${configPath}`);
  const content = readFileSync(configPath, 'utf8');
  const cfg = JSON.parse(content);
  const mcp = asRecord(cfg.mcpServers);
  const serverKey = siteSurfaceServerKey('andrey-user', surfaceId);
  if (!mcp[serverKey]) return { status: 'not_bound', carrier_id: 'kimi-andrey', surface_id: surfaceId };
  delete mcp[serverKey];
  writeFileSync(configPath, JSON.stringify(cfg, null, 2) + '\n', 'utf8');
  return { status: 'unbound', carrier_id: 'kimi-andrey', surface_id: surfaceId, server_key: serverKey };
}

function codexBind(configPath: string, surfaceId: string, entrypoint: string, resolvedArgs: string[], siteId: string, siteRoot: string, projectionId: string): { result: JsonRecord; content: string; structured: JsonRecord } {
  if (!existsSync(configPath)) throw diagnosticError('registrar_config_not_found', `registrar_config_not_found:${configPath}`);
  let content = readFileSync(configPath, 'utf8');
  const sectionKey = `[mcp_servers.${surfaceId}]`;
  if (content.includes(sectionKey)) {
    const structured = parseCarrierConfig('codex', content);
    if (!structured) throw diagnosticError('registrar_materialized_config_parse_failed', 'The carrier configuration could not be parsed before binding.', { config_path: configPath });
    return { result: { status: 'already_bound', carrier_id: 'codex-andrey', surface_id: surfaceId }, content, structured };
  }
  const surface = lookupSurface(surfaceId);
  const launch = carrierLaunchCommand({ kind: 'shared', entrypoint, args: resolvedArgs, surface, projection: selectSurfaceProjection(surfaceId, projectionId).projection, ...naradaScopeMetadata(surfaceId, siteRoot, siteId, projectionId), narada_scope: naradaScopeMetadata(surfaceId, siteRoot, siteId, projectionId) }, surfaceId, configPath);
  content += `\n${sectionKey}\ncommand = "${launch.command}"\nargs = ${JSON.stringify(launch.args)}\n`;
  const structured = parseCarrierConfig('codex', content);
  if (!structured) throw diagnosticError('registrar_materialized_config_parse_failed', 'The carrier configuration could not be parsed after binding.', { config_path: configPath });
  return { result: { status: 'bound', carrier_id: 'codex-andrey', surface_id: surfaceId }, content, structured };
}

function codexUnbind(configPath: string, surfaceId: string): JsonRecord {
  if (!existsSync(configPath)) throw diagnosticError('registrar_config_not_found', `registrar_config_not_found:${configPath}`);
  let content = readFileSync(configPath, 'utf8');
  const sectionKey = `[mcp_servers.${surfaceId}]`;
  if (!content.includes(sectionKey)) return { status: 'not_bound', carrier_id: 'codex-andrey', surface_id: surfaceId };
  const idx = content.indexOf(sectionKey);
  const nextSection = content.indexOf('\n[', idx + sectionKey.length);
  if (nextSection >= 0) {
    content = content.slice(0, idx) + content.slice(nextSection);
  } else {
    content = content.slice(0, idx).trimEnd();
  }
  writeFileSync(configPath, content, 'utf8');
  return { status: 'unbound', carrier_id: 'codex-andrey', surface_id: surfaceId, server_key: surfaceId };
}

async function registrarSync(args: JsonRecord): Promise<JsonRecord> {
  const target = requiredString(args.target, 'registrar_requires_target');
  const results: JsonRecord[] = [];

  if (target === 'all_surfaces_to_carriers') {
    const carrierId = requiredString(args.carrier_id, 'registrar_requires_carrier_id_for_target');
    const carrier = lookupCarrier(carrierId);
    if (carrier.site_bindings.some((binding) => binding.loading_mode === 'progressive')) {
      throw diagnosticError(
        'registrar_progressive_bulk_bind_refused',
        `registrar_progressive_bulk_bind_refused:${carrierId}`,
        {
          carrier_id: carrierId,
          remediation: 'Progressive carriers expose only their explicit bootstrap allowlist; use mcp-loader for runtime attachment or switch the binding to static loading.',
        },
      );
    }
    for (const surface of SURFACES) {
      try { results.push(await registrarCarrierBind({ carrier_id: carrierId, surface_id: surface.id, projection_id: args.projection_id })); }
      catch (e) { results.push({ carrier_id: carrierId, surface_id: surface.id, error: e instanceof Error ? e.message : String(e) }); }
    }
    return { target, carrier_id: carrierId, results, count: results.length };
  }

  if (target === 'all_surfaces_to_all_carriers') {
    if (CARRIERS.some((carrier) => carrier.site_bindings.some((binding) => binding.loading_mode === 'progressive'))) {
      throw diagnosticError(
        'registrar_progressive_bulk_bind_refused',
        'registrar_progressive_bulk_bind_refused:all_carriers',
        {
          remediation: 'Progressive carriers expose only their explicit bootstrap allowlists; use mcp-loader for runtime attachment or switch the bindings to static loading.',
        },
      );
    }
    for (const carrier of CARRIERS) {
      for (const surface of SURFACES) {
        try { results.push(await registrarCarrierBind({ carrier_id: carrier.carrier_id, surface_id: surface.id, projection_id: args.projection_id })); }
        catch (e) { results.push({ carrier_id: carrier.carrier_id, surface_id: surface.id, error: e instanceof Error ? e.message : String(e) }); }
      }
    }
    return { target, results, count: results.length };
  }

  const surfaceId = requiredString(args.surface_id, 'registrar_requires_surface_id');
  lookupSurface(surfaceId);
  if (target === 'all_sites' || target === 'all') {
    for (const site of siteCatalogForOperations()) {
      try { results.push(registrarSiteBind({ site_id: site.site_id, surface_id: surfaceId, projection_id: args.projection_id, runtime_kind: args.runtime_kind, allow_sidecar: args.allow_sidecar === true })); }
      catch (e) { results.push({ site_id: site.site_id, surface_id: surfaceId, error: e instanceof Error ? e.message : String(e) }); }
    }
  }
  if (target === 'all_carriers' || target === 'all') {
    for (const carrier of CARRIERS) {
      try { results.push(await registrarCarrierBind({ carrier_id: carrier.carrier_id, surface_id: surfaceId, projection_id: args.projection_id })); }
      catch (e) { results.push({ carrier_id: carrier.carrier_id, surface_id: surfaceId, error: e instanceof Error ? e.message : String(e) }); }
    }
  }
  return { surface_id: surfaceId, target, results, count: results.length };
}

function renderResult(result: JsonRecord): string {
  if (result.items !== undefined) return `registrar: ${result.count ?? 0} items\n${(result.items as JsonRecord[]).map((i) => `  ${i.id ?? i.site_id ?? i.carrier_id ?? ''}`).join('\n')}`;
  if (result.results) return `registrar sync: ${result.count ?? 0} results\n${(result.results as JsonRecord[]).map((r) => `  ${r.status ?? r.error ?? ''}`).join('\n')}`;
  return `${result.status ?? 'ok'}: ${result.surface_id ?? ''} @ ${result.site_id ?? result.carrier_id ?? ''}`;
}

function requiredString(value: unknown, code: string, details: JsonRecord = {}): string {
  const text = String(value ?? '').trim();
  if (!text) throw diagnosticError(code, code, details);
  return text;
}

function optionalString(value: unknown): string | null {
  const text = String(value ?? '').trim();
  return text || null;
}

function optionalRuntimeKind(value: unknown): McpRuntimeKind | undefined {
  if (value === undefined || value === null || value === '') return undefined;
  if (value === 'nars') return 'nars';
  throw diagnosticError('registrar_unknown_runtime_kind', `registrar_unknown_runtime_kind:${String(value)}`, {
    runtime_kind: value,
    admitted_runtime_kinds: ['nars'],
  });
}

function asRecord(value: unknown): JsonRecord {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonRecord : {};
}

function uniqueStrings(values: unknown[]): string[] {
  return [...new Set(values.map(String).filter(Boolean))];
}

function diagnosticError(code: string, message: string = code, details: JsonRecord = {}) {
  const error = new Error(message);
  Object.assign(error, { codeName: code, details });
  return error;
}

function errorDiagnostic(error: unknown) {
  const record = asRecord(error);
  return { schema: 'narada.registrar.error.v1', code: String(record.codeName ?? 'registrar_error'), message: error instanceof Error ? error.message : String(error), details: asRecord(record.details) };
}

function drainJsonLines(buffer: string) {
  const lines = buffer.split(/\r?\n/);
  return { framed: false, remaining: lines.pop() ?? '', requests: lines.filter((line) => line.trim()).map((line) => asRecord(JSON.parse(line))) };
}

function drainJsonRpcFrames(buffer: string) {
  const requests: JsonRecord[] = [];
  let remaining = buffer;
  while (true) {
    const headerEnd = remaining.indexOf('\r\n\r\n');
    if (headerEnd < 0) break;
    const match = /Content-Length:\s*(\d+)/i.exec(remaining.slice(0, headerEnd));
    if (!match) break;
    const length = Number(match[1]);
    const bodyStart = headerEnd + 4;
    const bodyEnd = bodyStart + length;
    if (remaining.length < bodyEnd) break;
    requests.push(asRecord(JSON.parse(remaining.slice(bodyStart, bodyEnd))));
    remaining = remaining.slice(bodyEnd);
  }
  return { framed: true, remaining, requests };
}

function writeJsonRpcResponse(response: JsonRecord, { framed }: { framed: boolean }) {
  const body = JSON.stringify(response);
  if (framed) process.stdout.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\r\n\r\n${body}`);
  else process.stdout.write(`${body}\n`);
}

export type RegistrarCliOptions =
  | { mode: 'stdio' }
  | { mode: 'help' }
  | { mode: 'materialize-all'; outputDir: string | null; runtimeProxyImplementation: RuntimeProxyImplementation; runtimeProfile: RuntimeProfileKind; recoveryEscapeHatch: false }
  | { mode: 'materialize-carrier'; carrierId: string; outputPath: string | null; allowSingleCarrier: true; runtimeProxyImplementation: RuntimeProxyImplementation; runtimeProfile: RuntimeProfileKind; recoveryEscapeHatch: boolean }

export function parseArgs(argv: string[]): RegistrarCliOptions {
  let carrierId: string | null = null;
  let outputPath: string | null = null;
  let outputDir: string | null = null;
  let materializeAll = false;
  let allowSingleCarrier = false;
  let recoveryEscapeHatch = false;
  let selectedProxyImplementation: RuntimeProxyImplementation = defaultRuntimeProxyImplementation();
  let selectedProxyImplementationExplicit = false;
  let selectedRuntimeProfile: RuntimeProfileKind = (process.env.NARADA_RUNTIME_PROFILE?.trim() || 'native') as RuntimeProfileKind;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help' || arg === '-h') return { mode: 'help' };
    if (arg === '--materialize-all') {
      materializeAll = true;
      continue;
    }
    if (arg === '--materialize-carrier') {
      const value = argv[++index];
      if (!value || value.startsWith('--')) throw new Error('registrar_missing_carrier_id');
      carrierId = value;
      continue;
    }
    if (arg === '--output-path') {
      const value = argv[++index];
      if (!value || value.startsWith('--')) throw new Error('registrar_missing_output_path');
      outputPath = value;
      continue;
    }
    if (arg === '--output-dir') {
      const value = argv[++index];
      if (!value || value.startsWith('--')) throw new Error('registrar_missing_output_dir');
      outputDir = value;
      continue;
    }
    if (arg === '--allow-single-carrier') {
      allowSingleCarrier = true;
      continue;
    }
    if (arg === '--recovery-escape-hatch') {
      recoveryEscapeHatch = true;
      continue;
    }
    if (arg === '--runtime-profile') {
      const value = argv[++index];
      if (value !== 'native' && value !== 'bun' && value !== 'node-compat') throw new Error('registrar_invalid_runtime_profile');
      selectedRuntimeProfile = value;
      continue;
    }
    if (arg === '--runtime-proxy-implementation') {
      const value = argv[++index];
      if (value !== 'bun' && value !== 'node' && value !== 'native') throw new Error('registrar_invalid_runtime_proxy_implementation');
      selectedProxyImplementation = value;
      selectedProxyImplementationExplicit = true;
      continue;
    }
    // Keep the historical launch hint accepted by Site Fabric clients. The
    // registrar's authoritative roots are resolved from its environment and
    // generated fabric, so this compatibility argument does not alter
    // materialization mode.
    if (arg === '--narada-root') {
      const value = argv[++index];
      if (!value || value.startsWith('--')) throw new Error('registrar_missing_narada_root');
      continue;
    }
    throw new Error(`registrar_unknown_cli_argument:${arg}`);
  }
  const selectedPlan = acceptedRuntimeMaterializationPlan(selectedRuntimeProfile);
  const matrixProxyImplementation = runtimeProxyImplementationForResolvedPlan(selectedPlan);
  if (!selectedProxyImplementationExplicit) {
    selectedProxyImplementation = matrixProxyImplementation;
  } else if (selectedProxyImplementation !== matrixProxyImplementation && !recoveryEscapeHatch) {
    throw new Error('registrar_runtime_proxy_override_requires_recovery_escape_hatch');
  }
  if (materializeAll && carrierId) throw new Error('registrar_materialize_modes_are_mutually_exclusive');
  if (materializeAll && allowSingleCarrier) throw new Error('registrar_allow_single_carrier_requires_materialize_carrier');
  if (materializeAll && outputPath) throw new Error('registrar_output_path_requires_materialize_carrier');
  if (carrierId && outputDir) throw new Error('registrar_output_dir_requires_materialize_all');
  if (carrierId && !allowSingleCarrier) throw new Error('registrar_single_carrier_materialization_requires_explicit_escape_hatch');
  if (!carrierId && allowSingleCarrier) throw new Error('registrar_allow_single_carrier_requires_materialize_carrier');
  if (recoveryEscapeHatch && (!carrierId || !allowSingleCarrier)) throw new Error('registrar_recovery_escape_hatch_requires_single_carrier');
  if (!materializeAll && !carrierId && (outputPath || outputDir)) throw new Error('registrar_output_requires_materialization_mode');
  if (!materializeAll && !carrierId && selectedProxyImplementationExplicit && selectedProxyImplementation !== 'bun') throw new Error('registrar_runtime_proxy_implementation_requires_materialization_mode');
  if (materializeAll) return { mode: 'materialize-all', outputDir, runtimeProxyImplementation: selectedProxyImplementation, runtimeProfile: selectedRuntimeProfile, recoveryEscapeHatch: false };
  if (carrierId) return { mode: 'materialize-carrier', carrierId, outputPath, allowSingleCarrier: true, runtimeProxyImplementation: selectedProxyImplementation, runtimeProfile: selectedRuntimeProfile, recoveryEscapeHatch };
  return { mode: 'stdio' };
}

async function runDirectMaterialization(options: Extract<RegistrarCliOptions, { mode: 'materialize-all' | 'materialize-carrier' }>): Promise<void> {
  process.env[FRESH_REGISTRAR_ENV] = '1';
  setRuntimeMaterializationProfile(options.runtimeProfile);
  runtimeProxyImplementation = options.runtimeProxyImplementation;
  if (runtimeProxyImplementation === 'native' && !nativeRuntimeProxyAvailable()) {
    throw new Error(process.platform === 'win32'
      ? `registrar_native_runtime_proxy_missing:${nativeRuntimeProxyEntrypoint()}`
      : `registrar_native_runtime_proxy_unsupported_platform:${process.platform}`);
  }
  const result = options.mode === 'materialize-all'
    ? await registrarMaterializeAll({ ...(options.outputDir ? { output_dir: resolve(options.outputDir) } : {}), runtime_profile: options.runtimeProfile })
    : await registrarSingleCarrierMaterialize({ carrier_id: options.carrierId, runtime_proxy_implementation: options.runtimeProxyImplementation, recovery_escape_hatch: options.recoveryEscapeHatch, ...(options.outputPath ? { output_path: resolve(options.outputPath) } : {}) });
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

function printCliHelp(): void {
  process.stdout.write([
    'mcp-registrar MCP server',
    '',
    'Out-of-band carrier recovery (works when the MCP registrar surface cannot start):',
    '  mcp-registrar --materialize-all [--output-dir <directory>] [--runtime-profile native|bun|node-compat] [--runtime-proxy-implementation bun|node|native]',
    '',
    'Targeted recovery is intentionally difficult and is not an MCP operation:',
    '  mcp-registrar --materialize-carrier <carrier-id> --allow-single-carrier [--output-path <carrier-config>] [--runtime-profile native|bun|node-compat] [--runtime-proxy-implementation bun|node|native] [--recovery-escape-hatch]',
    '',
    'Without arguments, mcp-registrar serves its MCP stdio protocol.',
    'Normal materialization writes every registered carrier config and its .narada-generation.json sidecar atomically.',
    '',
  ].join('\n'));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const options = parseArgs(process.argv.slice(2));
  const run = options.mode === 'materialize-all' || options.mode === 'materialize-carrier'
    ? runDirectMaterialization(options)
    : options.mode === 'help'
      ? Promise.resolve(printCliHelp())
      : runStdioServer(options);
  run.catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  });
}
