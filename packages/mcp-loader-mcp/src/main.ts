#!/usr/bin/env node
import { randomUUID } from 'node:crypto';
import { spawn, ChildProcess } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { homedir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import {
  MCP_FABRIC_SCHEMA_VERSION,
  RuntimeObservationV2Schema,
  liveToolsContractDigest,
  parseSurfaceDescriptorV2,
  parseMcpBindingAdmissionEnvelopeV1,
  mcpBindingAdmissionEntryDigestV1,
  surfaceExecutionDeclaration,
  stableDigest,
  LifecycleRequirementSchema,
  type LifecycleRequirement,
  type McpToolDefinition,
  type SurfaceDescriptorV2,
  type SurfaceExecutionDeclaration,
  type McpBindingAdmissionEnvelopeV1,
  type McpBindingAdmissionEntryV1,
} from '@narada-core/mcp-fabric-contracts';
import { createRuntimeObservationSink, type RuntimeObservationSink } from '@narada-core/mcp-runtime-observation';
import { buildBoundedToolResult, outputShow, outputShowAsync, payloadCreate, prunePayloadWorkspaces } from '@narada-core/mcp-transport';
import { buildGuidanceResult, guidanceToolDefinition } from './guidance.js';
import { loaderRuntimeLifecycle, loaderSupervisorRestartAction } from './runtime-lifecycle.js';
import { DEFAULT_TOOL_CALL_TIMEOUT_MS, DEFAULT_TOOL_TIMEOUT_GRACE_MS, resolveToolCallTimeoutMs } from './tool-timeout.js';

const MCP_SURFACES_ROOT = normalizePath(resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..'));
const MCP_WORKSPACE_ROOT = normalizePath(resolve(MCP_SURFACES_ROOT, '..'));
function requiredNativeSurfaceEntrypoint(packageName: string, artifactName: string): string {
  const nativeRoot = resolve(MCP_SURFACES_ROOT, packageName, 'dist', 'native');
  const pointerPath = resolve(nativeRoot, 'current.json');
  if (!existsSync(pointerPath)) throw new Error(`mcp_loader_native_pointer_missing:${pointerPath}`);
  const pointer = JSON.parse(readFileSync(pointerPath, 'utf8')) as { artifacts?: Record<string, string> };
  const relativePath = pointer.artifacts?.[artifactName];
  if (!relativePath) throw new Error(`mcp_loader_native_artifact_undeclared:${packageName}:${artifactName}`);
  const entrypoint = resolve(nativeRoot, relativePath);
  if (!existsSync(entrypoint)) throw new Error(`mcp_loader_native_artifact_missing:${entrypoint}`);
  return normalizePath(entrypoint);
}
const LOADER_RUNTIME_ENTRYPOINT = normalizePath(fileURLToPath(import.meta.url));
const LOADER_SOURCE_ENTRYPOINT = normalizePath(resolve(MCP_SURFACES_ROOT, 'mcp-loader-mcp/src/main.ts'));
const LOADER_PROCESS_STARTED_AT_MS = Date.now();
const RUNTIME_PROXY_STATUS_TOOL_NAME = 'mcp_runtime_proxy_status';
const LOADER_RUNTIME_FILE_PAIRS = [
  {
    name: 'loader_entrypoint',
    source: LOADER_SOURCE_ENTRYPOINT,
    runtime: LOADER_RUNTIME_ENTRYPOINT,
  },
  {
    name: 'loader_guidance',
    source: normalizePath(resolve(MCP_SURFACES_ROOT, 'mcp-loader-mcp/src/guidance.ts')),
    runtime: normalizePath(resolve(dirname(LOADER_RUNTIME_ENTRYPOINT), 'guidance.js')),
  },
  {
    name: 'loader_runtime_lifecycle',
    source: normalizePath(resolve(MCP_SURFACES_ROOT, 'mcp-loader-mcp/src/runtime-lifecycle.ts')),
    runtime: normalizePath(resolve(dirname(LOADER_RUNTIME_ENTRYPOINT), 'runtime-lifecycle.js')),
  },
  {
    name: 'loader_tool_timeout',
    source: normalizePath(resolve(MCP_SURFACES_ROOT, 'mcp-loader-mcp/src/tool-timeout.ts')),
    runtime: normalizePath(resolve(dirname(LOADER_RUNTIME_ENTRYPOINT), 'tool-timeout.js')),
  },
  {
    name: 'mcp_transport',
    source: normalizePath(resolve(MCP_WORKSPACE_ROOT, 'packages/shared/mcp-transport/src/mcp-payload-file.ts')),
    runtime: normalizePath(resolve(MCP_WORKSPACE_ROOT, 'packages/shared/mcp-transport/dist/src/mcp-payload-file.js')),
  },
] as const;
const LOADER_CONFIG_FILES = [
  { name: 'workspace_package', path: normalizePath(resolve(MCP_WORKSPACE_ROOT, 'package.json')) },
  { name: 'workspace_lockfile', path: normalizePath(resolve(MCP_WORKSPACE_ROOT, 'pnpm-lock.yaml')) },
  { name: 'workspace_typescript_config', path: normalizePath(resolve(MCP_WORKSPACE_ROOT, 'tsconfig.base.json')) },
  { name: 'loader_package', path: normalizePath(resolve(MCP_WORKSPACE_ROOT, 'packages/mcp-loader-mcp/package.json')) },
  { name: 'loader_typescript_config', path: normalizePath(resolve(MCP_WORKSPACE_ROOT, 'packages/mcp-loader-mcp/tsconfig.json')) },
  { name: 'mcp_transport_package', path: normalizePath(resolve(MCP_WORKSPACE_ROOT, 'packages/shared/mcp-transport/package.json')) },
] as const;

const SERVER_NAME = 'mcp-loader-mcp';
const SERVER_VERSION = '0.1.0';
const PROTOCOL_VERSION = '2024-11-05';
const observationSinks = new Map<string, RuntimeObservationSink>();

const DEFAULT_MAX_CONNECTIONS = 8;
const DEFAULT_MAX_REQUEST_BYTES = 1024 * 1024;
const DEFAULT_MAX_RESPONSE_BYTES = 4 * 1024 * 1024;
const STDERR_TAIL_LIMIT = 8000;
const DEFAULT_ATTACH_TIMEOUT_MS = 30000;
const DEFAULT_RUNTIME_LEASE_MS = 30000;
const DEFAULT_LOADER_RESULT_INLINE_LIMIT = 12000;
const SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX = 'site-tools-';
const SITE_TOOL_OBSERVATION_MAX_ENTRIES = 32;
const SITE_TOOL_OBSERVATION_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000;
const REJECTED_SITE_IDS = new Set(['narada-andrey', 'narada-user-site']);
const SURFACE_HANDLE_PREFIX = 'msh_';
const LOADER_RUN_ID = `loader-${randomUUID()}`;
const LOADER_OWNER_PID = process.pid;
const LOADER_OWNERSHIP_MARKER = `narada.mcp.loader/${LOADER_RUN_ID}`;

type McpRuntimeKind = string;

function assertSupportedSiteId(siteId: string, source: string): string {
  if (REJECTED_SITE_IDS.has(siteId)) {
    throw diagnosticError(
      'site_fabric_legacy_site_id_rejected',
      `site_fabric_legacy_site_id_rejected:${siteId}:${source}`,
      { received: siteId, source, reason: 'legacy_site_id_not_supported' },
    );
  }
  return siteId;
}

function connectionOwnershipFields(connection: ChildConnection): JsonRecord {
  return {
    owner: 'mcp-loader',
    owner_run_id: connection.ownerRunId,
    owner_pid: connection.ownerPid,
    parent_pid: connection.parentPid,
    ownership_marker: connection.ownershipMarker,
    cleanup_scope: 'loader_owned_child_only',
  };
}

function runtimeObservation(args: JsonRecord, state: LoaderState): JsonRecord {
  const connection = getConnection(args, state);
  const carrierKind = requiredString(args.carrier_kind, 'missing_carrier_kind');
  const fabric = readSiteFabric(connection.siteRoot);
  const explicitManifestDigest = optionalDigest(args.manifest_digest, 'manifest_digest');
  const manifestDigest = explicitManifestDigest ?? optionalDigest(fabric.manifest_digest, 'manifest_digest');
  const live = isConnectionLive(connection);
  if (live) touchConnection(connection);
  const observedAt = new Date().toISOString();
  const activeGeneration = live ? runtimeGeneration(connection, observedAt) : null;
  return RuntimeObservationV2Schema.parse({
    schema_version: MCP_FABRIC_SCHEMA_VERSION,
    observation_id: `observation-${Date.now()}-${connection.logicalConnectionId.slice(0, 12)}`,
    observed_at: observedAt,
    site_id: typeof fabric.site_id === 'string' ? fabric.site_id : deriveSiteId(connection.siteRoot),
    carrier_kind: carrierKind,
    runtime_state_root: null,
    manifest_digest: manifestDigest,
    servers: [{
      server_name: connection.serverName,
      surface_id: connection.surfaceId,
      projection_id: connection.projectionId,
      logical_connection_id: connection.logicalConnectionId,
      lifecycle: connection.lifecycle,
      active_generation: activeGeneration,
      draining_generations: [],
      recovery_actions: loaderRecoveryActions(connection),
      detail: live
        ? 'mcp-loader owns this active generation; use the bounded loader restart action for replacement.'
        : 'The loader child is no longer live; inspect the status and use the bounded loader restart action if lifecycle permits.',
    }],
  });
}

function defaultAllowedSiteRoots(): string[] {
  const roots = [MCP_WORKSPACE_ROOT];
  const configuredRoots = normalizeStringArray(process.env.NARADA_MCP_ALLOWED_SITE_ROOTS) ?? [];
  if (configuredRoots.length > 0) roots.push(...configuredRoots);
  const configuredSourceRoot = optionalString(process.env.NARADA_SRC_ROOT);
  if (configuredSourceRoot) roots.push(resolve(configuredSourceRoot));
  const configuredSiteRoot = optionalString(process.env.NARADA_SITE_ROOT);
  if (configuredSiteRoot) roots.push(configuredSiteRoot);
  const userProfile = process.env.USERPROFILE || process.env.HOME;
  if (userProfile) roots.push(resolve(userProfile, 'Narada'));
  return [...new Set(roots)];
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

function findingStatusCounts(findings: JsonRecord[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const finding of findings) {
    const status = typeof finding.status === 'string' ? finding.status : 'unknown';
    counts[status] = (counts[status] ?? 0) + 1;
  }
  return counts;
}

async function siteToolInventoryCheck(args: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  const siteRoot = normalizePath(requiredString(args.site_root, 'missing_site_root'));
  ensureSiteRootAllowed(siteRoot, state.policy);
  const servers = asRecord(readSiteFabric(siteRoot).mcpServers);
  const requestedSurfaceIds = stringArray(args.surface_ids);
  const surfaceIds = requestedSurfaceIds ?? Object.keys(servers).sort();
  const runtimeKind = optionalRuntimeKind(args.runtime_kind);
  const includeOk = args.include_ok === true;
  const findings: JsonRecord[] = [];
  const observedToolsBySurface: Record<string, string[]> = {};
  const observedReadOnlyToolsBySurface: Record<string, string[]> = {};
  const observedMutatingToolsBySurface: Record<string, string[]> = {};
  const observedUnclassifiedToolsBySurface: Record<string, string[]> = {};

  for (const surfaceId of surfaceIds) {
    const matchedServer = findSiteServer(servers, surfaceId);
    const server = matchedServer?.server ?? null;
    if (!server) {
      findings.push({ surface_id: surfaceId, status: 'surface_not_declared', declared_tools: [], observed_tools: [] });
      continue;
    }
    const rawDeclaredTools = stringArray(server.tools) ?? [];
    const declaredTools = [...new Set(rawDeclaredTools)].sort();
    const duplicateDeclaredTools = duplicateStrings(rawDeclaredTools);
    const runtimeRequirements = surfaceRuntimeRequirements(server);
    if (!runtimeRequirementsMatch(runtimeRequirements, runtimeKind)) {
      findings.push({
        surface_id: surfaceId,
        status: 'runtime_not_selected',
        declared_count: declaredTools.length,
        observed_count: 0,
        declared_tools: declaredTools,
        observed_tools: [],
        runtime_kind: runtimeKind,
        runtime_requirements: runtimeRequirements,
      });
      continue;
    }
    let connectionId: string | null = null;
    try {
      const attached = await attachSurface({ site_root: siteRoot, surface_id: surfaceId, runtime_kind: runtimeKind }, state);
      connectionId = requiredString(attached.connection_id, 'attached_connection_id_missing');
      const connection = getConnection({ connection_id: connectionId }, state);
      const observedDefinitions = (connection.toolSnapshot ?? [])
        .filter((tool) => String(tool.name ?? '') !== RUNTIME_PROXY_STATUS_TOOL_NAME);
      const rawObservedTools = observedDefinitions.map((tool) => String(tool.name ?? '')).filter(Boolean);
      const observedTools = [...new Set(rawObservedTools)].sort();
      const duplicateObservedTools = duplicateStrings(rawObservedTools);
      const observedReadOnlyTools = [...new Set(observedDefinitions
        .filter((tool) => asRecord(tool.annotations).readOnlyHint === true)
        .map((tool) => String(tool.name ?? ''))
        .filter(Boolean))].sort();
      const observedMutatingTools = [...new Set(observedDefinitions
        .filter((tool) => asRecord(tool.annotations).readOnlyHint === false)
        .map((tool) => String(tool.name ?? ''))
        .filter(Boolean))].sort();
      const observedUnclassifiedTools = [...new Set(observedDefinitions
        .filter((tool) => typeof asRecord(tool.annotations).readOnlyHint !== 'boolean')
        .map((tool) => String(tool.name ?? ''))
        .filter(Boolean))].sort();
      observedToolsBySurface[surfaceId] = observedTools;
      observedReadOnlyToolsBySurface[surfaceId] = observedReadOnlyTools;
      observedMutatingToolsBySurface[surfaceId] = observedMutatingTools;
      observedUnclassifiedToolsBySurface[surfaceId] = observedUnclassifiedTools;
      const missingFromFabric = observedTools.filter((toolName) => !declaredTools.includes(toolName));
      const extraInFabric = declaredTools.filter((toolName) => !observedTools.includes(toolName));
      const status = missingFromFabric.length === 0
        && extraInFabric.length === 0
        && duplicateDeclaredTools.length === 0
        && duplicateObservedTools.length === 0
        && observedUnclassifiedTools.length === 0
        ? 'ok'
        : 'drift';
      if (includeOk || status !== 'ok') {
        findings.push({
          surface_id: surfaceId,
          status,
          declared_count: declaredTools.length,
          observed_count: observedTools.length,
          missing_from_fabric: missingFromFabric,
          extra_in_fabric: extraInFabric,
          duplicate_declared_tools: duplicateDeclaredTools,
          duplicate_observed_tools: duplicateObservedTools,
          unclassified_observed_tools: observedUnclassifiedTools,
        });
      }
    } catch (error) {
      const diagnostic = errorDiagnostic(error);
      findings.push({ surface_id: surfaceId, status: 'probe_failed', error: diagnostic });
    } finally {
      if (connectionId && state.connections.has(connectionId)) {
        detachConnection({ connection_id: connectionId }, state);
      }
    }
  }

  const violationCount = findings.filter((finding) => finding.status !== 'ok' && finding.status !== 'runtime_not_selected').length;
  const runtimeSkippedSurfaceIds = findings
    .filter((finding) => finding.status === 'runtime_not_selected')
    .map((finding) => finding.surface_id);
  const runtimeSkippedCount = runtimeSkippedSurfaceIds.length;
  const observation = {
    schema: 'narada.mcp_loader.site_tool_inventory_check.v1',
    status: violationCount > 0 ? 'drift' : runtimeSkippedCount > 0 ? 'partial' : 'ok',
    site_root: siteRoot,
    observed_at: new Date().toISOString(),
    requested_surface_ids: requestedSurfaceIds ?? null,
    runtime_kind: runtimeKind,
    attempted_surface_ids: surfaceIds,
    observed_surface_ids: Object.keys(observedToolsBySurface).sort(),
    unobserved_surface_ids: surfaceIds.filter((surfaceId) => !Object.hasOwn(observedToolsBySurface, surfaceId)),
    runtime_skipped_surface_ids: runtimeSkippedSurfaceIds,
    runtime_skipped_count: runtimeSkippedCount,
    observation_coverage: requestedSurfaceIds || runtimeSkippedCount > 0 ? 'partial' : 'complete',
    checked_surface_count: surfaceIds.length,
    violation_count: violationCount,
    observed_tools: observedToolsBySurface,
    observed_read_only_tools: observedReadOnlyToolsBySurface,
    observed_mutating_tools: observedMutatingToolsBySurface,
    observed_unclassified_tools: observedUnclassifiedToolsBySurface,
    finding_status_counts: findingStatusCounts(findings),
    findings,
  };
  const materialized = payloadCreate({
    siteRoot,
    args: {
      payload_id: `site-tools-${randomUUID().replace(/-/g, '').slice(0, 24)}`,
      payload: observation,
      created_by: SERVER_NAME,
    },
  });
  const retention = prunePayloadWorkspaces({
    siteRoot,
    payloadIdPrefix: SITE_TOOL_OBSERVATION_PAYLOAD_PREFIX,
    maxEntries: SITE_TOOL_OBSERVATION_MAX_ENTRIES,
    maxAgeMs: SITE_TOOL_OBSERVATION_MAX_AGE_MS,
  });
  return {
    ...observation,
    observation_ref: materialized.ref,
    observation_sha256: materialized.sha256,
    observation_byte_size: materialized.byte_size,
    observation_retention: retention,
  };
}

function defaultAllowedEntrypointPrefixes(): string[] {
  const prefixes = [
    MCP_SURFACES_ROOT,
    '{site_root}/tools/',
  ];
  const configuredPrefixes = normalizeStringArray(process.env.NARADA_MCP_ALLOWED_ENTRYPOINT_PREFIXES) ?? [];
  if (configuredPrefixes.length > 0) prefixes.push(...configuredPrefixes);
  const userProfile = process.env.USERPROFILE || process.env.HOME;
  if (userProfile) prefixes.push(resolve(userProfile, 'Narada', 'tools'));
  return [...new Set(prefixes)];
}
const DEFAULT_ALLOWED_ENV_VARS = [
  'USERPROFILE',
  'HOME',
  'NODE_OPTIONS',
  'PATH',
  'PROCESSOR_ARCHITECTURE',
  'SystemRoot',
  'NARADA_AGENT_ID',
  'NARADA_OPERATOR_ID',
  'NARADA_NARS_SESSION_SOURCE_KIND',
  'NARADA_CARRIER_SESSION_ID',
  'NARADA_SITE_ID',
  'NARADA_ROOT',
  'NARADA_SRC_ROOT',
];
type JsonRecord = Record<string, unknown>;

export type RuntimeFileObservation = {
  path: string;
  exists: boolean;
  mtime_ms: number | null;
  mtime: string | null;
};

type RuntimeSurfaceMetadata = {
  serverName: string;
  projectionId: string;
  execution: SurfaceExecutionDeclaration;
  lifecycle: LifecycleRequirement;
  descriptor: SurfaceDescriptorV2 | null;
  descriptorDigest: string | null;
  declaredToolContractDigest: string | null;
};

export type RuntimeFilePair = {
  name: string;
  source: RuntimeFileObservation;
  runtime: RuntimeFileObservation;
};

export type LoaderConfigObservation = {
  name: string;
  observation: RuntimeFileObservation;
};

export type LoaderFreshnessEvidence = {
  processStartedAtMs: number;
  filePairs: RuntimeFilePair[];
  configFiles: LoaderConfigObservation[];
};

function observeRuntimeFile(path: string): RuntimeFileObservation {
  try {
    const stat = statSync(path);
    return {
      path,
      exists: true,
      mtime_ms: stat.mtimeMs,
      mtime: new Date(stat.mtimeMs).toISOString(),
    };
  } catch {
    return { path, exists: false, mtime_ms: null, mtime: null };
  }
}

export function classifyLoaderRuntimeFreshness(evidence: LoaderFreshnessEvidence): JsonRecord {
  const reasons: string[] = [];
  for (const pair of evidence.filePairs) {
    if (pair.name === 'loader_entrypoint' && !pair.runtime.exists) {
      reasons.push('runtime_file_unavailable:loader_entrypoint');
    }
  }

  const status = reasons.some((reason) => reason.includes('unavailable'))
    ? 'unknown'
    : reasons.length > 0
      ? 'stale'
      : 'current';
  const entrypoint = evidence.filePairs.find((pair) => pair.name === 'loader_entrypoint');
  const dependencyPairs = evidence.filePairs.filter((pair) => pair.name !== 'loader_entrypoint');
  return {
    schema: 'narada.mcp_loader.runtime_freshness.v1',
    status,
    reload_required: status === 'stale' ? true : status === 'current' ? false : null,
    process_started_at: new Date(evidence.processStartedAtMs).toISOString(),
    process_started_at_ms: evidence.processStartedAtMs,
    freshness_scope: 'native_loader_artifact',
    runtime_entrypoint: entrypoint?.runtime ?? null,
    source_entrypoint: entrypoint?.source ?? null,
    source_files: evidence.filePairs.map((pair) => ({ name: pair.name, ...pair.source })),
    runtime_files: evidence.filePairs.map((pair) => ({ name: pair.name, ...pair.runtime })),
    dependency_files: dependencyPairs.map((pair) => ({ name: pair.name, source: pair.source, runtime: pair.runtime })),
    config_files: evidence.configFiles.map((config) => ({ name: config.name, ...config.observation })),
    tracked_file_count: evidence.filePairs.length * 2 + evidence.configFiles.length,
    reasons,
    reload_action: {
      ...loaderSupervisorRestartAction(),
      guidance: 'Restart the mcp-loader process through its carrier or runtime supervisor to load rebuilt loader code. mcp_loader_surface_restart replaces only an attached child and does not reload this loader process.',
    },
  };
}

function loaderRuntimeFreshness(): JsonRecord {
  const filePairs = LOADER_RUNTIME_FILE_PAIRS.map((pair) => ({
    name: pair.name,
    source: observeRuntimeFile(pair.source),
    runtime: observeRuntimeFile(pair.runtime),
  }));
  const configFiles = LOADER_CONFIG_FILES.map((config) => ({
    name: config.name,
    observation: observeRuntimeFile(config.path),
  }));
  return classifyLoaderRuntimeFreshness({
    processStartedAtMs: LOADER_PROCESS_STARTED_AT_MS,
    filePairs,
    configFiles,
  });
}

export function assertLoaderRuntimeFreshnessCurrent(freshness: JsonRecord, operation: string): JsonRecord {
  const status = freshness.status;
  if (status === 'current') return freshness;
  throw diagnosticError('loader_runtime_not_current', `loader_runtime_not_current:${String(status ?? 'unknown')}`, {
    operation,
    spawn_permitted: false,
    child_spawned: false,
    failure_class: status === 'stale' ? 'stale_runtime_evidence' : 'runtime_freshness_unknown',
    runtime_freshness: freshness,
    remediation: 'Regenerate/build the loader runtime and restart the loader process through its carrier or runtime supervisor before retrying.',
    recovery_action: freshness.reload_action ?? null,
  });
}

function assertLoaderRuntimeCurrentForSpawn(operation: string): JsonRecord {
  return assertLoaderRuntimeFreshnessCurrent(loaderRuntimeFreshness(), operation);
}

type LoaderPolicy = {
  allowedSiteRoots: string[];
  allowedEntrypointPrefixes: string[];
  allowedSurfaceIds: string[] | 'site_fabric';
  allowedEnvVars: string[];
  maxConnections: number;
  maxRequestBytes: number;
  maxResponseBytes: number;
  attachTimeoutMs: number;
  toolCallTimeoutMs: number;
  toolCallGraceMs: number;
};

type ChildConnection = {
  connectionId: string;
  ownerRunId: string;
  ownerPid: number;
  parentPid: number;
  ownershipMarker: string;
  logicalConnectionId: string;
  generationId: string;
  serverName: string;
  projectionId: string;
  execution: SurfaceExecutionDeclaration;
  lifecycle: LifecycleRequirement;
  descriptor: SurfaceDescriptorV2 | null;
  descriptorDigest: string | null;
  declaredToolContractDigest: string | null;
  toolContractDigest: string | null;
  heartbeatAt: string;
  leaseExpiresAt: string;
  siteRoot: string;
  surfaceId: string;
  bindingId: string | null;
  admissionEnvelopeId: string | null;
  admittedBindingDigest: string | null;
  authorityEpoch: number | null;
  runtimeKind: McpRuntimeKind | null;
  runtimeRequirements: McpRuntimeKind[];
  entrypoint: string;
  args: string[];
  childInvocationKind: ChildInvocationKind;
  runtimeCommand: string;
  requestedEntrypoint: string | null;
  extraArgs: string[];
  process: ChildProcess;
  pending: Map<number | string, { resolve: (value: JsonRecord) => void; reject: (error: Error) => void; timeout: ReturnType<typeof setTimeout> }>;
  nextId: number;
  buffer: string;
  initialized: boolean;
  capabilities: JsonRecord;
  serverInfo: JsonRecord;
  toolSnapshot: JsonRecord[] | null;
  detached: boolean;
  attachedAt: string;
  detachedAt: string | null;
  stderrTail: string;
};

type SurfaceHandle = {
  handle: string;
  handleScope: 'loader_process';
  logicalConnectionId: string;
  siteRoot: string;
  surfaceId: string;
  runtimeKind: McpRuntimeKind | null;
  createdAt: string;
};

type LoaderState = {
  policy: LoaderPolicy;
  connections: Map<string, ChildConnection>;
  surfaceHandles: Map<string, SurfaceHandle>;
  bindingAdmission: McpBindingAdmissionEnvelopeV1 | null;
  standaloneAmbientAttachment: boolean;
};

function loadBindingAdmission(options: JsonRecord): McpBindingAdmissionEnvelopeV1 | null {
  const required = process.env.NARADA_MCP_BINDING_ADMISSION_REQUIRED === '1';
  const path = String(options.bindingAdmissionPath ?? process.env.NARADA_MCP_BINDING_ADMISSION_PATH ?? '').trim();
  if (!path) {
    if (required) throw diagnosticError('mcp_binding_admission_required', 'mcp_binding_admission_required');
    return null;
  }
  const envelope = parseMcpBindingAdmissionEnvelopeV1(JSON.parse(readFileSync(path, 'utf8')));
  const expectedDigest = String(options.bindingAdmissionDigest ?? process.env.NARADA_MCP_BINDING_ADMISSION_DIGEST ?? '').trim();
  if (expectedDigest && expectedDigest !== envelope.envelope_digest) {
    throw diagnosticError('mcp_binding_admission_digest_mismatch', 'mcp_binding_admission_digest_mismatch');
  }
  const expectedSession = String(process.env.NARADA_NARS_SESSION_ID ?? process.env.NARADA_RUNTIME_SESSION_ID ?? process.env.NARADA_CARRIER_SESSION_ID ?? '').trim();
  const expectedPrincipal = String(process.env.NARADA_SESSION_AUTHORITY_PRINCIPAL_KEY ?? '').trim();
  const expectedEpoch = Number(process.env.NARADA_SESSION_AUTHORITY_EPOCH);
  if (expectedSession && envelope.carrier_session_id !== expectedSession) throw diagnosticError('mcp_binding_admission_session_mismatch', 'mcp_binding_admission_session_mismatch');
  if (expectedPrincipal && envelope.principal_key !== expectedPrincipal) throw diagnosticError('mcp_binding_admission_principal_mismatch', 'mcp_binding_admission_principal_mismatch');
  if (Number.isInteger(expectedEpoch) && envelope.authority_epoch !== expectedEpoch) throw diagnosticError('mcp_binding_admission_epoch_mismatch', 'mcp_binding_admission_epoch_mismatch');
  if (Date.parse(envelope.issued_at) > Date.now()) throw diagnosticError('mcp_binding_admission_not_yet_issued', 'mcp_binding_admission_not_yet_issued');
  if (envelope.valid_until !== null && Date.parse(envelope.valid_until) <= Date.now()) throw diagnosticError('mcp_binding_admission_expired', 'mcp_binding_admission_expired');
  return envelope;
}

export function createServerState(options: JsonRecord = {}): LoaderState {
  const rawAllowedSiteRoots = normalizeStringArray(options.allowedSiteRoots) ?? defaultAllowedSiteRoots();
  const rawAllowedPrefixes = normalizeStringArray(options.allowedEntrypointPrefixes) ?? defaultAllowedEntrypointPrefixes();
  const rawAllowedSurfaces = normalizeStringArray(options.allowedSurfaceIds);
  const allowedSurfaceIds: string[] | 'site_fabric' = rawAllowedSurfaces && rawAllowedSurfaces.length > 0 ? rawAllowedSurfaces : 'site_fabric';
  const policy: LoaderPolicy = {
    allowedSiteRoots: [...new Map(rawAllowedSiteRoots.map((p) => {
      const normalized = normalizePath(p);
      const key = process.platform === 'win32' ? normalized.toLocaleLowerCase('en-US') : normalized;
      return [key, normalized] as const;
    })).values()],
    allowedEntrypointPrefixes: [...new Set(rawAllowedPrefixes.map((p) => normalizePolicyPrefix(p)))].sort((a, b) => b.length - a.length),
    allowedSurfaceIds,
    allowedEnvVars: normalizeStringArray(options.allowedEnvVars) ?? DEFAULT_ALLOWED_ENV_VARS,
    maxConnections: integer(options.maxConnections, DEFAULT_MAX_CONNECTIONS, 1, 64),
    maxRequestBytes: integer(options.maxRequestBytes, DEFAULT_MAX_REQUEST_BYTES, 4096, 16 * 1024 * 1024),
    maxResponseBytes: integer(options.maxResponseBytes, DEFAULT_MAX_RESPONSE_BYTES, 4096, 64 * 1024 * 1024),
    attachTimeoutMs: integer(options.attachTimeoutMs, DEFAULT_ATTACH_TIMEOUT_MS, 1000, 300000),
    toolCallTimeoutMs: integer(options.toolCallTimeoutMs, DEFAULT_TOOL_CALL_TIMEOUT_MS, 1000, 900000),
    toolCallGraceMs: integer(options.toolCallGraceMs, DEFAULT_TOOL_TIMEOUT_GRACE_MS, 0, 60000),
  };
  return {
    policy,
    connections: new Map(),
    surfaceHandles: new Map(),
    bindingAdmission: loadBindingAdmission(options),
    standaloneAmbientAttachment: options.standaloneAmbientAttachment === true,
  };
}

export async function handleRequest(request: JsonRecord, state: LoaderState) {
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
      if (isParallelSafeRequest(request)) {
        void handleRequest(request, state).then((response) => {
          if (response) writeJsonRpcResponse(response, { framed: sawFramedInput });
        });
        continue;
      }
      const response = await handleRequest(request, state);
      if (response) writeJsonRpcResponse(response, { framed: sawFramedInput });
    }
  }
}

function isParallelSafeRequest(request: JsonRecord): boolean {
  return request.method === 'tools/call'
    && asRecord(request.params).name === 'mcp_loader_detach';
}

async function dispatchMethod(method: string, params: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  switch (method) {
    case 'initialize':
      return { protocolVersion: params.protocolVersion ?? PROTOCOL_VERSION, capabilities: { tools: {} }, serverInfo: { name: SERVER_NAME, version: SERVER_VERSION } };
    case 'tools/list':
      return { tools: listTools() };
    case 'tools/call':
      return callToolResult(await callTool(params, state));
    default:
      throw diagnosticError('unsupported_mcp_method', `unsupported_mcp_method:${method}`);
  }
}

export function listTools() {
  return [
    guidanceToolDefinition(),
    tool('mcp_loader_runtime_status', 'Inspect whether this loader process is current relative to its runtime, source, dependency, and build-configuration evidence and whether the loader process itself must be restarted.', {}, [], { readOnly: true }),
    tool('mcp_loader_policy_inspect', 'Inspect the policy governing runtime MCP surface loading.', {}, [], { readOnly: true }),
    tool('mcp_loader_connection_inventory', 'List attached loader connections, including liveness, age, explicit loader-managed restartability, capacity, and bounded recovery actions for stale children.', {}, [], { readOnly: true }),
    tool('mcp_loader_process_ownership', 'Inspect process ownership for children spawned by this loader run. This is a read-only reconciliation view: it reports loader-owned direct children and safe cleanup actions, but never enumerates or terminates unrelated host processes or conhost descendants.', {}, [], { readOnly: true }),
    tool('mcp_loader_topology_diagnostics', 'Explain the MCP tool/surface/proxy/loader/session-instance topology and aggregate bounded counts, memory evidence, duplicate detection, and shared-mode admission posture for this loader run.', {}, [], { readOnly: true }),
    tool('mcp_loader_runtime_observation', 'Return the normalized V2 runtime observation for one attached surface, including stable logical identity, generation state, lifecycle eligibility, contract digests, and bounded actuator guidance.', {
      connection_id: { type: 'string', description: 'Connection id returned by mcp_loader_attach_surface.' },
      carrier_kind: { type: 'string', description: 'Carrier kind producing the observation, such as codex, kimi, or opencode.' },
      manifest_digest: { type: 'string', description: 'Optional current V2 fabric manifest digest.' },
    }, ['connection_id', 'carrier_kind'], { readOnly: true }),
    tool('mcp_loader_list_site_surfaces', 'List resolvable MCP surfaces declared in a site\'s local fabric.', {
      site_root: { type: 'string', description: 'Site root directory.' },
    }, ['site_root'], { readOnly: true }),
    tool('mcp_loader_site_fabric_diagnostics', 'Inspect site MCP fabric provenance and classify shared-registry drift or intentional entrypoint overrides.', {
      site_root: { type: 'string', description: 'Site root directory.' },
    }, ['site_root'], { readOnly: true }),
    tool('mcp_loader_site_tool_inventory_check', 'Compare site fabric declarations with fresh child tools/list responses; compact output includes per-finding status and tool-name deltas, runtime-skipped surfaces produce partial coverage, and an immutable observation_ref is materialized for Registrar conformance checks.', {
      site_root: { type: 'string', description: 'Site root directory.' },
      surface_ids: { type: 'array', items: { type: 'string' }, description: 'Optional surface ids to check. Defaults to every surface in the site fabric.' },
      runtime_kind: { type: 'string', description: 'Explicit runtime context used to select runtime-affined projections. Omit to inspect only runtime-neutral surfaces.' },
      include_ok: { type: 'boolean', description: 'Include passing surface findings.' },
    }, ['site_root'], { readOnly: true }),
    tool('mcp_loader_attach_surface', 'Spawn and initialize a stdio MCP surface, return a connection id, and report loader-managed restartability.', {
      site_root: { type: 'string', description: 'Site root directory.' },
      binding_id: { type: 'string', description: 'Exact binding identity from the admitted session envelope.' },
      surface_id: { type: 'string', description: 'Optional consistency assertion for the admitted binding.' },
      runtime_kind: { type: 'string', description: 'Explicit runtime context required when the selected surface projection declares runtime_requirements.' },
      entrypoint: { type: 'string', description: 'Optional explicit entrypoint path; must be allowed by policy if provided.' },
      args: { type: 'array', items: { type: 'string' }, description: 'Optional additional args appended after resolved args.' },
    }, ['site_root', 'binding_id'], { readOnly: false }),
    tool('mcp_loader_open_surface', 'Open a surface and return a stable logical handle for calls across loader-managed child generations. The handle is scoped to this loader process and does not survive loader restart.', {
      site_root: { type: 'string', description: 'Site root directory.' },
      binding_id: { type: 'string', description: 'Exact binding identity from the admitted session envelope.' },
      surface_id: { type: 'string', description: 'Optional consistency assertion for the admitted binding.' },
      runtime_kind: { type: 'string', description: 'Explicit runtime context required when the selected surface projection declares runtime_requirements.' },
      entrypoint: { type: 'string', description: 'Optional explicit entrypoint path; must be allowed by policy if provided.' },
      args: { type: 'array', items: { type: 'string' }, description: 'Optional additional args appended after resolved args.' },
    }, ['site_root', 'binding_id'], { readOnly: false }),
    tool('mcp_loader_surface_handle_inventory', 'List stable logical surface handles and the current child generation, without spawning or replacing a surface.', {}, [], { readOnly: true }),
    tool('mcp_loader_list_tools', 'List tools exposed by an attached MCP surface.', {
      connection_id: { type: 'string', description: 'Connection id returned by mcp_loader_attach_surface.' },
    }, ['connection_id'], { readOnly: true }),
    tool('mcp_loader_surface_status', 'Inspect the runtime status and loader-managed restartability of an attached MCP surface child process.', {
      connection_id: { type: 'string', description: 'Connection id returned by mcp_loader_attach_surface.' },
    }, ['connection_id'], { readOnly: true }),
    tool('mcp_loader_tool_discovery_manifest', 'Return canonical semantic tool names for an attached surface and flag generated aliases as non-authoritative.', {
      connection_id: { type: 'string', description: 'Connection id returned by mcp_loader_attach_surface.' },
    }, ['connection_id'], { readOnly: true }),
    tool('mcp_loader_call_tool', 'Call a tool on an attached MCP surface. Results are bounded by default and include a typed summary; set include_runtime_metadata=true when lifecycle/freshness evidence is needed on this call. If the nested arguments include timeout_ms, the loader honors it up to its bounded maximum and waits an additional bounded grace (--tool-timeout-grace-ms, default 1000 ms) so the tool can return its own bounded timeout result.', {
      connection_id: { type: 'string', description: 'Connection id returned by mcp_loader_attach_surface.' },
      tool_name: { type: 'string', description: 'Tool name on the attached surface.' },
      arguments: { type: 'object', description: 'Arguments object for the tool call.' },
      include_runtime_metadata: { type: 'boolean', description: 'Include loader runtime_lifecycle and runtime_freshness metadata on this result. Defaults to false; attach/status/observation tools remain authoritative for lifecycle evidence.' },
    }, ['connection_id', 'tool_name'], { readOnly: false }),
    tool('mcp_loader_call_surface_tool', 'Call a tool through a stable logical surface handle. Results are bounded by default and include a typed summary; set include_runtime_metadata=true for lifecycle/freshness evidence. The handle follows loader-managed child replacement; use mcp_loader_open_surface to create a new handle after loader restart.', {
      surface_handle: { type: 'string', description: 'Stable handle returned by mcp_loader_open_surface.' },
      tool_name: { type: 'string', description: 'Tool name on the logical surface.' },
      arguments: { type: 'object', description: 'Arguments object for the tool call.' },
      include_runtime_metadata: { type: 'boolean', description: 'Include loader runtime_lifecycle and runtime_freshness metadata on this result. Defaults to false.' },
    }, ['surface_handle', 'tool_name'], { readOnly: false }),
    tool('mcp_loader_read_result', 'Read a bounded page from a materialized proxied child result. The ref is bound to the same Site authority as the connection.', {
      connection_id: { type: 'string', description: 'Connection id returned by mcp_loader_call_tool or mcp_loader_call_surface_tool.' },
      ref: { type: 'string', description: 'Materialized result ref returned in result.details_ref or result.output_ref.' },
      output_ref: { type: 'string', description: 'Alias for ref.' },
      offset: { type: 'integer', minimum: 0, description: 'Character offset into the materialized JSON output.' },
      limit: { type: 'integer', minimum: 1, description: 'Maximum output characters for this page.' },
      timeout_ms: { type: 'integer', minimum: 1, maximum: 15000, description: 'Strict upper bound for reading and validating the materialized output file.' },
    }, ['connection_id'], { readOnly: true }),
    tool('mcp_loader_detach', 'Detach and terminate an attached MCP surface.', {
      connection_id: { type: 'string', description: 'Connection id returned by mcp_loader_attach_surface.' },
    }, ['connection_id'], { readOnly: false, destructive: true }),
    tool('mcp_loader_surface_restart', 'Replace an attached MCP surface child process with a freshly initialized connection using the same site, surface, entrypoint, and args; this does not restart the agent session.', {
      connection_id: { type: 'string', description: 'Connection id returned by mcp_loader_attach_surface.' },
      reason: { type: 'string', description: 'Optional operator or caller reason for the restart.' },
    }, ['connection_id'], { readOnly: false, destructive: true }),
  ];
}

async function callTool(params: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  const name = requiredString(params.name, 'missing_tool_name');
  const args = asRecord(params.arguments);
  switch (name) {
    case 'mcp_loader_guidance':
      return { ...buildGuidanceResult(args), runtime_freshness: loaderRuntimeFreshness() };
    case 'mcp_loader_runtime_status':
      return loaderRuntimeFreshness();
    case 'mcp_loader_policy_inspect':
      return policyInspect(state);
    case 'mcp_loader_connection_inventory':
      return connectionInventory(state);
    case 'mcp_loader_process_ownership':
      return processOwnership(state);
    case 'mcp_loader_topology_diagnostics':
      return mcpLoaderTopologyDiagnostics(state);
    case 'mcp_loader_runtime_observation':
      return runtimeObservation(args, state);
    case 'mcp_loader_list_site_surfaces':
      return listSiteSurfaces(args, state);
    case 'mcp_loader_site_fabric_diagnostics':
      return siteFabricDiagnostics(args, state);
    case 'mcp_loader_site_tool_inventory_check':
      return siteToolInventoryCheck(args, state);
    case 'mcp_loader_attach_surface':
      return attachSurface(args, state);
    case 'mcp_loader_open_surface':
      return openSurfaceHandle(args, state);
    case 'mcp_loader_surface_handle_inventory':
      return surfaceHandleInventory(state);
    case 'mcp_loader_list_tools':
      return listAttachedTools(args, state);
    case 'mcp_loader_surface_status':
      return surfaceStatus(args, state);
    case 'mcp_loader_tool_discovery_manifest':
      return toolDiscoveryManifest(args, state);
    case 'mcp_loader_call_tool':
      return callAttachedTool(args, state);
    case 'mcp_loader_call_surface_tool':
      return callSurfaceHandleTool(args, state);
    case 'mcp_loader_read_result':
      return await readLoaderResult(args, state);
    case 'mcp_loader_detach':
      return detachConnection(args, state);
    case 'mcp_loader_surface_restart':
      return restartConnection(args, state);
    default:
      throw diagnosticError('unknown_tool', `unknown_tool:${name}`);
  }
}

function policyInspect(state: LoaderState): JsonRecord {
  return {
    schema: 'narada.mcp_loader.policy.v1',
    policy: state.policy,
    binding_admission: state.bindingAdmission ? {
      status: 'admitted',
      envelope_id: state.bindingAdmission.envelope_id,
      envelope_digest: state.bindingAdmission.envelope_digest,
      binding_count: state.bindingAdmission.bindings.length,
      authority_epoch: state.bindingAdmission.authority_epoch,
      carrier_session_id: state.bindingAdmission.carrier_session_id,
    } : { status: state.standaloneAmbientAttachment ? 'standalone_ambient' : 'required_missing' },
  };
}

function connectionInventory(state: LoaderState): JsonRecord {
  const now = Date.now();
  const connections = [...state.connections.values()].map((connection) => {
    const status = connectionStatusFields(connection);
    const attachedAtMs = Date.parse(connection.attachedAt);
    return {
      ...status,
      connection_id: connection.connectionId,
      liveness: status.status,
      age_ms: Number.isFinite(attachedAtMs) ? Math.max(0, now - attachedAtMs) : null,
      runtime_lifecycle: loaderRuntimeLifecycle(connection.connectionId, connection.lifecycle),
      recovery_actions: {
        inspect: { tool_name: 'mcp_loader_surface_status', arguments: { connection_id: connection.connectionId } },
        detach: { tool_name: 'mcp_loader_detach', arguments: { connection_id: connection.connectionId } },
        restart: loaderRecoveryActions(connection)[0],
        ownership: { tool_name: 'mcp_loader_process_ownership', arguments: {} },
      },
    };
  });
  const liveConnections = connections.filter((connection) => connection.liveness === 'live');
  const closedConnections = connections.filter((connection) => connection.liveness === 'closed');
  return {
    schema: 'narada.mcp_loader.connection_inventory.v1',
    status: 'ok',
    runtime_freshness: loaderRuntimeFreshness(),
    max_connections: state.policy.maxConnections,
    connection_count: connections.length,
    available_slots: Math.max(0, state.policy.maxConnections - connections.length),
    live_count: liveConnections.length,
    closed_count: closedConnections.length,
    live_connection_ids: liveConnections.map((connection) => connection.connection_id),
    closed_connection_ids: closedConnections.map((connection) => connection.connection_id),
    connections,
    recovery: {
      when_full: 'Inspect this inventory, then detach closed or no-longer-needed connections. Use surface restart only for an intentionally live replacement.',
      inspect_tool: 'mcp_loader_surface_status',
      detach_tool: 'mcp_loader_detach',
      restart_tool: 'mcp_loader_surface_restart',
      ownership_tool: 'mcp_loader_process_ownership',
      note: 'The inventory is read-only and does not reap children or free slots automatically.',
    },
  };
}

function processOwnership(state: LoaderState): JsonRecord {
  const processes = [...state.connections.values()].map((connection) => {
    const live = isConnectionLive(connection);
    const ownershipVerified = connection.ownerRunId === LOADER_RUN_ID
      && connection.ownerPid === LOADER_OWNER_PID
      && connection.parentPid === LOADER_OWNER_PID
      && connection.ownershipMarker === LOADER_OWNERSHIP_MARKER;
    return {
      ...connectionOwnershipFields(connection),
      connection_id: connection.connectionId,
      logical_connection_id: connection.logicalConnectionId,
      generation_id: connection.generationId,
      pid: connection.process.pid ?? null,
      status: live ? 'live' : 'closed',
      ownership_status: ownershipVerified ? 'loader_owned' : 'ownership_mismatch',
      descendant_scope: 'direct_child_process_only',
      cleanup: live
        ? { status: 'not_eligible', action: { tool_name: 'mcp_loader_detach', arguments: { connection_id: connection.connectionId } } }
        : { status: 'safe_to_reconcile', action: { tool_name: 'mcp_loader_detach', arguments: { connection_id: connection.connectionId } } },
    };
  });
  const safeClosed = processes.filter((process) => process.ownership_status === 'loader_owned' && process.status === 'closed');
  return {
    schema: 'narada.mcp_loader.process_ownership.v1',
    status: 'ok',
    loader: {
      run_id: LOADER_RUN_ID,
      pid: LOADER_OWNER_PID,
      ownership_marker: LOADER_OWNERSHIP_MARKER,
      started_at: new Date(LOADER_PROCESS_STARTED_AT_MS).toISOString(),
    },
    scope: 'known_direct_children_spawned_by_this_loader_run',
    processes,
    safe_reconciliation_connection_ids: safeClosed.map((process) => process.connection_id),
    external_process_policy: 'unowned_or_unobserved_processes_are_not_enumerated_or_terminated',
    host_process_reconciliation: {
      status: 'not_available',
      reason: 'mcp-loader has no authority to enumerate arbitrary host processes or conhost descendants',
      conhost_descendants: 'not_enumerated',
      remediation: 'Use the host/runtime supervisor for external process inspection; use mcp_loader_detach for a known loader-owned connection.',
    },
  };
}

function mcpLoaderTopologyDiagnostics(state: LoaderState): JsonRecord {
  const connections = [...state.connections.values()];
  const sessionId = optionalString(process.env.NARADA_CARRIER_SESSION_ID) ?? 'session:unknown';
  const countBy = (values: string[]): JsonRecord => values.reduce<JsonRecord>((counts, value) => {
    counts[value] = Number(counts[value] ?? 0) + 1;
    return counts;
  }, {});
  const liveConnections = connections.filter((connection) => isConnectionLive(connection));
  const instanceKeys = connections.map((connection) => `${normalizePath(connection.siteRoot)}|${connection.surfaceId}|${sessionId}`);
  const duplicateInstanceKeys = duplicateStrings(instanceKeys);
  const orphanedConnectionIds = connections
    .filter((connection) => !isConnectionLive(connection) && !connection.detached)
    .map((connection) => connection.connectionId);
  const memory = process.memoryUsage();
  const bySurface = countBy(connections.map((connection) => connection.surfaceId));
  const bySession = countBy(connections.map(() => sessionId));
  const byOwner = countBy(connections.map((connection) => `mcp-loader/${connection.ownerRunId}`));
  const byProxy = countBy(connections.map(() => 'mcp-loader'));
  const childMemory = connections.map((connection) => ({
    connection_id: connection.connectionId,
    surface_id: connection.surfaceId,
    owner_id: `loader-child-${connection.connectionId}`,
    memory_status: 'not_available_from_loader',
    rss_bytes: null,
    heap_used_bytes: null,
  }));
  return {
    schema: 'narada.mcp_loader.topology_diagnostics.v1',
    status: duplicateInstanceKeys.length > 0 || orphanedConnectionIds.length > 0 ? 'attention' : 'ok',
    generated_at: new Date().toISOString(),
    vocabulary: {
      tool: 'A named MCP operation exposed by one surface.',
      surface: 'A server boundary exposing one or more MCP tools.',
      runtime_proxy: 'A carrier-owned process boundary that may host or supervise a loader; it is not a tool.',
      loader: 'The policy-gated process that attaches and proxies child surfaces.',
      session_instance: 'One loader-owned surface child for a Site/session projection.',
    },
    topology: {
      transport: 'stdio',
      instance_scope: 'per_loader_session_surface',
      loader_run_id: LOADER_RUN_ID,
      loader_pid: LOADER_OWNER_PID,
      carrier_session_id: sessionId === 'session:unknown' ? null : sessionId,
      counts: {
        loader_processes: 1,
        child_processes: connections.length,
        live_child_processes: liveConnections.length,
        closed_child_processes: connections.length - liveConnections.length,
      },
      by_surface: bySurface,
      by_proxy: byProxy,
      by_session: bySession,
      by_owner: byOwner,
    },
    diagnostics: {
      duplicate_instance_keys: duplicateInstanceKeys,
      orphaned_connection_ids: orphanedConnectionIds,
      known_connection_ids: connections.map((connection) => connection.connectionId),
      host_process_reconciliation: {
        status: 'not_available',
        reason: 'Loader authority is limited to its own direct children; host-wide proxy and conhost inspection belongs to the runtime supervisor.',
        next_tool: 'runtime_introspection_memory_owners',
      },
    },
    memory: {
      measurement_status: 'loader_process_only',
      loader_process: {
        owner_id: `mcp-loader-${LOADER_OWNER_PID}`,
        rss_bytes: memory.rss,
        heap_used_bytes: memory.heapUsed,
        external_bytes: memory.external,
        array_buffers_bytes: memory.arrayBuffers,
      },
      children: childMemory,
    },
    consolidation: {
      shared_mode: {
        status: 'not_implemented',
        default: false,
        admission_requirements: ['authority and tenant isolation equivalence', 'session lifecycle ownership', 'failure containment', 'conformance tests before opt-in'],
      },
      recommendation: 'Keep per-session stdio isolation as the default until an independently tested shared mode proves equivalent authority and failure boundaries.',
    },
  };
}

function listSiteSurfaces(args: JsonRecord, state: LoaderState): JsonRecord {
  const siteRoot = normalizePath(requiredString(args.site_root, 'missing_site_root'));
  ensureSiteRootAllowed(siteRoot, state.policy);
  assertBindingAdmissionAvailable(state);
  const fabric = readSiteFabric(siteRoot);
  const servers = asRecord(fabric.mcpServers);
  const surfaces: JsonRecord[] = [];
  for (const [surfaceId, server] of Object.entries(servers)) {
    const rec = asRecord(server);
    const bindingId = typeof rec.binding_id === 'string' ? rec.binding_id : '';
    const admission = state.bindingAdmission?.bindings.find((binding) => binding.binding_id === bindingId);
    if (state.bindingAdmission && (!admission || !admission.operations.includes('discover'))) continue;
    surfaces.push({
      binding_id: bindingId || null,
      surface_id: typeof rec.surface_id === 'string' ? rec.surface_id : surfaceId,
      server_name: surfaceId,
      command: rec.command,
      args: rec.args,
      env_vars: rec.env ? Object.keys(asRecord(rec.env)) : [],
      runtime_requirements: surfaceRuntimeRequirements(rec),
      runtime_lifecycle: loaderRuntimeLifecycle(),
    });
  }
  return { schema: 'narada.mcp_loader.site_surfaces.v1', site_root: siteRoot, runtime_freshness: loaderRuntimeFreshness(), surfaces };
}

function assertBindingAdmissionAvailable(state: LoaderState): void {
  if (!state.bindingAdmission && !state.standaloneAmbientAttachment) {
    throw diagnosticError('mcp_binding_admission_required', 'mcp_binding_admission_required', {
      child_spawned: false,
      remediation: 'Launch this loader through an admitted Narada carrier session or use --standalone-ambient-attachment only for explicit development fixtures.',
    });
  }
}

function currentBindingDigest(entry: McpBindingAdmissionEntryV1): string {
  return mcpBindingAdmissionEntryDigestV1(entry);
}

function admitBindingForOperation(
  state: LoaderState,
  siteRoot: string,
  bindingId: string,
  operation: 'attach' | 'restart',
): { entry: McpBindingAdmissionEntryV1; server: JsonRecord } | null {
  assertBindingAdmissionAvailable(state);
  if (!state.bindingAdmission) return null;
  const entry = state.bindingAdmission.bindings.find((candidate) => candidate.binding_id === bindingId);
  if (!entry || !entry.operations.includes(operation)) {
    throw diagnosticError('mcp_binding_not_admitted', `mcp_binding_not_admitted:${bindingId}:${operation}`, { child_spawned: false });
  }
  const server = asRecord(entry.binding_identity);
  const digest = currentBindingDigest(entry);
  if (digest !== entry.binding_digest) {
    throw diagnosticError('mcp_binding_digest_mismatch', `mcp_binding_digest_mismatch:${bindingId}`, {
      child_spawned: false,
      expected_binding_digest: entry.binding_digest,
      actual_binding_digest: digest,
    });
  }
  return { entry, server };
}

function siteFabricDiagnostics(args: JsonRecord, state: LoaderState): JsonRecord {
  const siteRoot = normalizePath(requiredString(args.site_root, 'missing_site_root'));
  ensureSiteRootAllowed(siteRoot, state.policy);
  const bundle = readSiteFabricBundle(siteRoot);
  const fabric = bundle.fabric;
  const servers = asRecord(fabric.mcpServers);
  const diagnostics = Object.entries(servers).map(([surfaceId, server]) => {
    const fabricPath = bundle.sourceBySurface[surfaceId] ?? bundle.paths[0];
    const rec = asRecord(server);
    const command = String(rec.command ?? '');
    const rawArgs = stringArray(rec.args) ?? [];
    const declaredEntrypoint = extractRuntimeEntrypoint(command, rawArgs);
    const shared = SHARED_SURFACE_REGISTRY[surfaceId];
    const expectedEntrypoint = shared ? normalizePath(shared.entrypoint.replace(/{site_root}/g, siteRoot)) : null;
    const normalizedDeclared = declaredEntrypoint ? normalizePath(declaredEntrypoint.replace(/{site_root}/g, siteRoot)) : null;
    const entrypointExists = normalizedDeclared ? existsSync(normalizedDeclared) : false;
    const classification = classifyFabricEntrypoint({
      siteRoot,
      declaredEntrypoint: normalizedDeclared,
      expectedEntrypoint,
      entrypointExists,
    });
    return {
      surface_id: surfaceId,
      source: 'site_fabric',
      config_path: fabricPath,
      command,
      args: rawArgs,
      declared_entrypoint: normalizedDeclared,
      shared_registry_entrypoint: expectedEntrypoint,
      entrypoint_exists: entrypointExists,
      classification: classification.status,
      durability: {
        local_repair_durable: 'unknown',
        reason: 'mcp-loader reads site fabric but does not own the generator or VCS ignore rules for this config.',
      },
      provenance: {
        config_source: fabricPath,
        shared_registry_source: shared ? '@narada-core/mcp-loader-mcp embedded registry' : null,
        generator: typeof fabric.generated_by === 'string' ? fabric.generated_by : null,
        generated_at: typeof fabric.generated_at === 'string' ? fabric.generated_at : null,
        tracking_state: 'unknown',
        tracking_state_reason: 'VCS tracking and ignore state are outside mcp-loader authority.',
      },
      remediation: classification.remediation,
    };
  });
  const sharedFallbacks = Object.entries(SHARED_SURFACE_REGISTRY)
    .filter(([surfaceId]) => !servers[surfaceId])
    .map(([surfaceId, shared]) => ({
      surface_id: surfaceId,
      source: 'shared_registry_fallback',
      shared_registry_entrypoint: normalizePath(shared.entrypoint.replace(/{site_root}/g, siteRoot)),
      classification: 'registry_fallback_available',
      provenance: {
        shared_registry_source: '@narada-core/mcp-loader-mcp embedded registry',
      },
    }));
  return {
    schema: 'narada.mcp_loader.site_fabric_diagnostics.v1',
    site_root: siteRoot,
    config_path: bundle.paths.length === 1 ? bundle.paths[0] : null,
    config_paths: bundle.paths,
    config_exists: bundle.paths.every((fabricPath) => existsSync(fabricPath)),
    diagnostics,
    shared_registry_fallbacks: sharedFallbacks,
  };
}

async function attachSurface(args: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  const siteRoot = normalizePath(requiredString(args.site_root, 'missing_site_root'));
  assertLoaderRuntimeCurrentForSpawn('mcp_loader_attach_surface');
  const bindingId = state.bindingAdmission
    ? requiredString(args.binding_id, 'missing_binding_id')
    : optionalString(args.binding_id) ?? null;
  const admitted = bindingId ? admitBindingForOperation(state, siteRoot, bindingId, 'attach') : (assertBindingAdmissionAvailable(state), null);
  const surfaceId = admitted?.entry.surface_id ?? requiredString(args.surface_id, 'missing_surface_id');
  if (admitted && optionalString(args.surface_id) && optionalString(args.surface_id) !== surfaceId) {
    throw diagnosticError('mcp_binding_surface_assertion_mismatch', `mcp_binding_surface_assertion_mismatch:${bindingId}`);
  }
  const runtimeKind = optionalRuntimeKind(args.runtime_kind);
  ensureSiteRootAllowed(siteRoot, state.policy);
  ensureSurfaceAllowed(surfaceId, siteRoot, state.policy);
  const runtimeRequirements = ensureSurfaceRuntimeAllowed(surfaceId, siteRoot, runtimeKind);
  const runtimeMetadata = surfaceRuntimeMetadata(siteRoot, surfaceId);

  if (runtimeMetadata.execution.adapter !== 'stdio') {
    throw diagnosticError(
      'surface_execution_adapter_not_supported_by_loader',
      `surface_execution_adapter_not_supported_by_loader:${surfaceId}:${runtimeMetadata.execution.adapter}`,
      {
        surface_id: surfaceId,
        projection_id: runtimeMetadata.projectionId,
        execution: runtimeMetadata.execution,
        responsible_actuator: 'pc_site_surface_runtime',
        remediation: 'Route this admitted binding through the PC Site surface runtime; mcp-loader remains the stdio compatibility adapter.',
      },
    );
  }

  if (state.connections.size >= state.policy.maxConnections) {
    const inventory = connectionInventory(state);
    throw diagnosticError('max_connections_reached', `max_connections_reached:${state.connections.size}`, {
      max_connections: inventory.max_connections,
      connection_count: inventory.connection_count,
      available_slots: inventory.available_slots,
      closed_connection_ids: inventory.closed_connection_ids,
      recovery: inventory.recovery,
    });
  }

  const explicitEntrypoint = optionalString(args.entrypoint);
  if (admitted && (explicitEntrypoint || stringArray(args.args)?.length)) {
    throw diagnosticError('mcp_binding_invocation_override_not_allowed', `mcp_binding_invocation_override_not_allowed:${bindingId}`, { child_spawned: false });
  }
  const extraArgs = stringArray(args.args);
  if (!explicitEntrypoint && extraArgs && extraArgs.length > 0) {
    throw diagnosticError(
      'site_fabric_invocation_override_not_allowed',
      `site_fabric_invocation_override_not_allowed:${surfaceId}`,
      {
        surface_id: surfaceId,
        remediation: 'Change and rematerialize the authoritative Site fabric instead of supplying per-call arguments.',
      },
    );
  }
  const launch = admitted
    ? resolveFabricLaunch(
        requiredString(admitted.entry.binding_identity.command, 'mcp_binding_identity_command_required'),
        stringArray(admitted.entry.binding_identity.args) ?? [],
      )
    : await resolveSurfaceEntrypoint(siteRoot, surfaceId, explicitEntrypoint, extraArgs);
  const { entrypoint, resolvedArgs } = launch;
  if (explicitEntrypoint) ensureEntrypointAllowed(siteRoot, entrypoint, state.policy);
  assertSurfaceLaunchMetadata(launch.childInvocationKind, entrypoint, launch.runtimeCommand);

  if (!existsSync(entrypoint)) {
    throw diagnosticError('entrypoint_not_found', `entrypoint_not_found:${entrypoint}`);
  }

  const connection = await openConnection({
    state,
    siteRoot,
    surfaceId,
    runtimeKind,
    runtimeRequirements,
    runtimeCommand: launch.runtimeCommand,
    entrypoint,
    resolvedArgs,
    childInvocationKind: launch.childInvocationKind,
    requestedEntrypoint: explicitEntrypoint,
    extraArgs: extraArgs ?? [],
    metadata: runtimeMetadata,
    bindingAdmission: admitted?.entry ?? null,
  });
  return attachedResponse(connection);
}

async function openSurfaceHandle(args: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  const attached = await attachSurface(args, state);
  const connectionId = requiredString(attached.connection_id, 'surface_attach_missing_connection_id');
  const connection = getConnection({ connection_id: connectionId }, state);
  const handle = `${SURFACE_HANDLE_PREFIX}${randomUUID().replace(/-/g, '').slice(0, 24)}`;
  const record: SurfaceHandle = {
    handle,
    handleScope: 'loader_process',
    logicalConnectionId: connection.logicalConnectionId,
    siteRoot: connection.siteRoot,
    surfaceId: connection.surfaceId,
    runtimeKind: connection.runtimeKind,
    createdAt: new Date().toISOString(),
  };
  state.surfaceHandles.set(handle, record);
  return {
    schema: 'narada.mcp_loader.surface_handle_opened.v1',
    status: 'opened',
    surface_handle: handle,
    handle_scope: record.handleScope,
    handle_survives_child_restart: true,
    handle_survives_loader_restart: false,
    logical_connection_id: connection.logicalConnectionId,
    connection_id: connection.connectionId,
    ownership: connectionOwnershipFields(connection),
    generation_id: connection.generationId,
    site_root: connection.siteRoot,
    binding_id: connection.bindingId,
    admission_envelope_id: connection.admissionEnvelopeId,
    binding_digest: connection.admittedBindingDigest,
    authority_epoch: connection.authorityEpoch,
    surface_id: connection.surfaceId,
    runtime_kind: connection.runtimeKind,
    runtime_requirements: connection.runtimeRequirements,
    runtime_lifecycle: loaderRuntimeLifecycle(connection.connectionId, connection.lifecycle),
    runtime_freshness: loaderRuntimeFreshness(),
    tool_count: connection.toolSnapshot?.length ?? null,
    created_at: record.createdAt,
    call: {
      tool_name: 'mcp_loader_call_surface_tool',
      arguments: { surface_handle: handle, tool_name: '<child_tool>', arguments: {} },
    },
  };
}

function surfaceHandleInventory(state: LoaderState): JsonRecord {
  const handles = [...state.surfaceHandles.values()].map((handle) => {
    const connection = findConnectionForHandle(handle, state);
    return {
      surface_handle: handle.handle,
      handle_scope: handle.handleScope,
      logical_connection_id: handle.logicalConnectionId,
      site_root: handle.siteRoot,
      surface_id: handle.surfaceId,
      runtime_kind: handle.runtimeKind,
      created_at: handle.createdAt,
      connection_id: connection?.connectionId ?? null,
      generation_id: connection?.generationId ?? null,
      status: connection && isConnectionLive(connection) ? 'live' : 'unavailable',
      recovery: connection
        ? { tool_name: 'mcp_loader_surface_restart', arguments: { connection_id: connection.connectionId } }
        : { tool_name: 'mcp_loader_open_surface', arguments: { site_root: handle.siteRoot, surface_id: handle.surfaceId, runtime_kind: handle.runtimeKind } },
    };
  });
  return {
    schema: 'narada.mcp_loader.surface_handle_inventory.v1',
    status: 'ok',
    handle_scope: 'loader_process',
    handle_count: handles.length,
    handles,
  };
}

async function openConnection(input: {
  state: LoaderState;
  siteRoot: string;
  surfaceId: string;
  runtimeKind: McpRuntimeKind | null;
  runtimeRequirements: McpRuntimeKind[];
  runtimeCommand: string;
  entrypoint: string;
  resolvedArgs: string[];
  childInvocationKind: ChildInvocationKind;
  requestedEntrypoint: string | null;
  extraArgs: string[];
  logicalConnectionId?: string;
  metadata: RuntimeSurfaceMetadata;
  bindingAdmission?: McpBindingAdmissionEntryV1 | null;
}): Promise<ChildConnection> {
  const { state, siteRoot, surfaceId, runtimeKind, runtimeRequirements, runtimeCommand, entrypoint, resolvedArgs, childInvocationKind, requestedEntrypoint, extraArgs, logicalConnectionId, metadata, bindingAdmission = null } = input;
  const connectionId = randomUUID();
  assertLoaderRuntimeCurrentForSpawn('mcp_loader_child_spawn');
  const stableLogicalConnectionId = logicalConnectionId ?? connectionId;
  const generationId = `generation-${randomUUID()}`;
  const attachedAt = new Date().toISOString();
  const childArgs = childInvocationKind === 'entrypoint' ? [entrypoint, ...resolvedArgs] : resolvedArgs;
  const child = spawn(runtimeCommand, childArgs, {
    stdio: ['pipe', 'pipe', 'pipe'],
    env: buildChildEnv(siteRoot, state.policy, { connectionId, logicalConnectionId: stableLogicalConnectionId, generationId }),
    shell: false,
    windowsHide: true,
  });

  const connection: ChildConnection = {
    connectionId,
    ownerRunId: LOADER_RUN_ID,
    ownerPid: LOADER_OWNER_PID,
    parentPid: LOADER_OWNER_PID,
    ownershipMarker: LOADER_OWNERSHIP_MARKER,
    logicalConnectionId: stableLogicalConnectionId,
    generationId,
    serverName: metadata.serverName,
    projectionId: metadata.projectionId,
    execution: metadata.execution,
    lifecycle: metadata.lifecycle,
    descriptor: metadata.descriptor,
    descriptorDigest: metadata.descriptorDigest,
    declaredToolContractDigest: metadata.declaredToolContractDigest,
    toolContractDigest: null,
    heartbeatAt: attachedAt,
    leaseExpiresAt: new Date(Date.parse(attachedAt) + DEFAULT_RUNTIME_LEASE_MS).toISOString(),
    siteRoot,
    surfaceId,
    bindingId: bindingAdmission?.binding_id ?? null,
    admissionEnvelopeId: state.bindingAdmission?.envelope_id ?? null,
    admittedBindingDigest: bindingAdmission?.binding_digest ?? null,
    authorityEpoch: state.bindingAdmission?.authority_epoch ?? null,
    runtimeKind,
    runtimeRequirements,
    runtimeCommand,
    entrypoint,
    args: resolvedArgs,
    childInvocationKind,
    requestedEntrypoint,
    extraArgs,
    process: child,
    pending: new Map(),
    nextId: 1,
    buffer: '',
    initialized: false,
    capabilities: {},
    serverInfo: {},
    toolSnapshot: null,
    detached: false,
    attachedAt,
    detachedAt: null,
    stderrTail: '',
  };

  state.connections.set(connectionId, connection);
  emitLoaderConnectionOwner(connection);

  child.stdout?.setEncoding('utf8');
  child.stdout?.on('data', (chunk) => handleChildStdout(chunk as string, connection));
  child.stderr?.on('data', (chunk) => {
    connection.stderrTail = tail(`${connection.stderrTail}${String(chunk)}`, STDERR_TAIL_LIMIT);
  });
  child.on('error', (error) => childError(connection, error));
  child.on('exit', () => {
    emitLoaderConnectionLifecycle(connection, 'process_exited', child.exitCode === 0 ? 'ok' : 'failed');
    cleanupConnection(connection);
  });

  const initResult = await sendChildRequest(connection, 'initialize', { protocolVersion: PROTOCOL_VERSION }, state.policy.attachTimeoutMs);
  connection.initialized = true;
  connection.capabilities = asRecord(initResult.capabilities);
  connection.serverInfo = asRecord(initResult.serverInfo);
  await sendChildNotification(connection, 'notifications/initialized', {});

  const toolsResult = await sendChildRequest(connection, 'tools/list', {}, state.policy.attachTimeoutMs);
  connection.toolSnapshot = (toolsResult.tools as JsonRecord[]) ?? [];
  connection.toolContractDigest = observedToolContractDigest(connection.toolSnapshot, connection.descriptor);
  touchConnection(connection);

  return connection;
}

function attachedResponse(connection: ChildConnection): JsonRecord {
  return {
    schema: 'narada.mcp_loader.surface_attached.v1',
    connection_id: connection.connectionId,
    logical_connection_id: connection.logicalConnectionId,
    generation_id: connection.generationId,
    site_root: connection.siteRoot,
    runtime_kind: connection.runtimeKind,
    runtime_requirements: connection.runtimeRequirements,
    runtime_command: connection.runtimeCommand,
    runtime_lifecycle: loaderRuntimeLifecycle(connection.connectionId, connection.lifecycle),
    runtime_freshness: loaderRuntimeFreshness(),
    entrypoint: connection.entrypoint,
    args: connection.args,
    child_invocation_kind: connection.childInvocationKind,
    server_info: connection.serverInfo,
    tools: connection.toolSnapshot,
    descriptor_digest: connection.descriptorDigest,
    tool_contract_digest: connection.toolContractDigest,
    lifecycle: connection.lifecycle,
    ownership: connectionOwnershipFields(connection),
  };
}

async function listAttachedTools(args: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  const connection = getConnection(args, state);
  return {
    schema: 'narada.mcp_loader.tools.v1',
    connection_id: connection.connectionId,
    surface_id: connection.surfaceId,
    runtime_lifecycle: loaderRuntimeLifecycle(connection.connectionId, connection.lifecycle),
    runtime_freshness: loaderRuntimeFreshness(),
    tools: connection.toolSnapshot ?? [],
  };
}

function surfaceStatus(args: JsonRecord, state: LoaderState): JsonRecord {
  const connection = getConnection(args, state);
  return {
    schema: 'narada.mcp_loader.surface_status.v1',
    ...connectionStatusFields(connection),
  };
}

async function toolDiscoveryManifest(args: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  const connection = getConnection(args, state);
  const tools = connection.toolSnapshot ?? [];
  return {
    schema: 'narada.mcp_loader.tool_discovery_manifest.v1',
    connection_id: connection.connectionId,
    surface_id: connection.surfaceId,
    runtime_lifecycle: loaderRuntimeLifecycle(connection.connectionId, connection.lifecycle),
    runtime_freshness: loaderRuntimeFreshness(),
    alias_policy: {
      canonical_name_source: 'tools/list.name',
      generated_aliases_authoritative: false,
      guidance: 'Use canonical_name/callable_name for directives and tool calls. Client-generated aliases should be treated as compatibility UI labels only.',
    },
    tools: tools.map((tool) => {
      const record = asRecord(tool);
      const name = String(record.name ?? '');
      return {
        canonical_name: name,
        callable_name: name,
        generated_aliases: [],
        description: record.description ?? null,
        inputSchema: record.inputSchema ?? null,
      };
    }),
  };
}

async function callAttachedTool(args: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  const connection = getConnection(args, state);
  if (connection.bindingId) admitBindingForOperation(state, connection.siteRoot, connection.bindingId, 'attach');
  else assertBindingAdmissionAvailable(state);
  const toolName = requiredString(args.tool_name, 'missing_tool_name');
  const toolArgs = asRecord(args.arguments);
  enforceRequestSize(toolArgs, state.policy.maxRequestBytes);
  const timeout = resolveToolCallTimeoutMs(toolArgs.timeout_ms, state.policy.toolCallTimeoutMs, state.policy.toolCallGraceMs);
  if (timeout.status === 'refused') {
    const code = timeout.reason === 'exceeds_loader_max'
      ? 'tool_call_timeout_exceeds_loader_max'
      : 'invalid_tool_call_timeout';
    throw diagnosticError(code, `${code}:${String(timeout.requestedTimeoutMs)}`, {
      requested_timeout_ms: timeout.requestedTimeoutMs,
      max_timeout_ms: timeout.maxTimeoutMs,
    });
  }
  const childParams = {
    name: toolName,
    arguments: toolArgs,
    ...(timeout.source === 'tool_request' ? { _meta: { narada_request_timeout_ms: timeout.timeoutMs } } : {}),
  };
  const result = await sendChildRequest(connection, 'tools/call', childParams, timeout.outerTimeoutMs);
  const includeRuntimeMetadata = args.include_runtime_metadata === true;
  const enrichedResult = enrichAttachedGuidanceResult(result, toolName, connection, includeRuntimeMetadata);
  const bounded = buildBoundedToolResult({
    siteRoot: connection.siteRoot,
    toolName: `mcp_loader_call_tool:${connection.surfaceId}:${toolName}`,
    value: enrichedResult,
    isError: Boolean(enrichedResult.isError),
    limit: DEFAULT_LOADER_RESULT_INLINE_LIMIT,
    readerTool: 'mcp_loader_read_result',
  });
  const boundedResult = asRecord(bounded.structuredContent);
  const response: JsonRecord = {
    schema: 'narada.mcp_loader.tool_result.v1',
    connection_id: connection.connectionId,
    surface_id: connection.surfaceId,
    result: boundedResult,
    result_summary: typedResultSummary(enrichedResult),
    result_bounded: boundedResult.schema === 'narada.producer_output_page.v1',
    ...(typeof boundedResult.output_ref === 'string' ? {
      details_ref: boundedResult.output_ref,
      details_reader: 'mcp_loader_read_result',
    } : {}),
    ...(includeRuntimeMetadata ? {
      runtime_lifecycle: loaderRuntimeLifecycle(connection.connectionId, connection.lifecycle),
      runtime_freshness: loaderRuntimeFreshness(),
    } : {}),
  };
  enforceResponseSize(response, state.policy.maxResponseBytes);
  return response;
}

async function callSurfaceHandleTool(args: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  const handle = getSurfaceHandle(args, state);
  const connection = findConnectionForHandle(handle, state);
  if (!connection || !isConnectionLive(connection)) {
    throw diagnosticError(
      'surface_handle_connection_unavailable',
      `surface_handle_connection_unavailable:${handle.handle}`,
      {
        surface_handle: handle.handle,
        logical_connection_id: handle.logicalConnectionId,
        site_root: handle.siteRoot,
        surface_id: handle.surfaceId,
        recovery: {
          tool_name: 'mcp_loader_open_surface',
          arguments: { site_root: handle.siteRoot, surface_id: handle.surfaceId, runtime_kind: handle.runtimeKind },
        },
      },
    );
  }
  return callAttachedTool({
    connection_id: connection.connectionId,
    tool_name: requiredString(args.tool_name, 'missing_tool_name'),
    arguments: asRecord(args.arguments),
    include_runtime_metadata: args.include_runtime_metadata,
  }, state);
}

async function readLoaderResult(args: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  const connection = getConnection(args, state);
  const ref = requiredString(args.ref ?? args.output_ref, 'missing_output_ref');
  const page = await outputShowAsync({
    siteRoot: connection.siteRoot,
    args: {
      ref,
      ...(args.offset === undefined ? {} : { offset: args.offset }),
      ...(args.limit === undefined ? {} : { limit: args.limit }),
      ...(args.timeout_ms === undefined ? {} : { timeout_ms: args.timeout_ms }),
    },
    maxBytes: state.policy.maxResponseBytes,
  });
  const result = {
    schema: 'narada.mcp_loader.result_page.v1',
    connection_id: connection.connectionId,
    surface_id: connection.surfaceId,
    result: page,
  };
  enforceResponseSize(result, state.policy.maxResponseBytes);
  return result;
}

function getSurfaceHandle(args: JsonRecord, state: LoaderState): SurfaceHandle {
  const handle = requiredString(args.surface_handle, 'missing_surface_handle');
  const record = state.surfaceHandles.get(handle);
  if (!record) throw diagnosticError('surface_handle_not_found', `surface_handle_not_found:${handle}`);
  return record;
}

function findConnectionForHandle(handle: SurfaceHandle, state: LoaderState): ChildConnection | null {
  const matches = [...state.connections.values()]
    .filter((connection) => connection.logicalConnectionId === handle.logicalConnectionId)
    .sort((left, right) => Date.parse(right.attachedAt) - Date.parse(left.attachedAt));
  return matches.find((connection) => isConnectionLive(connection)) ?? matches[0] ?? null;
}

function typedResultSummary(result: JsonRecord): JsonRecord {
  const structured = asRecord(result.structuredContent);
  const summary: JsonRecord = {
    schema: typeof structured.schema === 'string' ? structured.schema : 'narada.mcp_loader.child_result.v1',
    status: typeof structured.status === 'string' ? structured.status : (result.isError ? 'error' : 'ok'),
    is_error: result.isError === true,
  };
  for (const key of ['code', 'message', 'summary', 'surface_id', 'task_id', 'task_number', 'ref', 'output_ref', 'next_offset', 'truncated']) {
    const value = structured[key];
    if (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean' || value === null) summary[key] = value;
  }
  for (const key of ['count', 'total', 'checked_surface_count', 'violation_count']) {
    const value = structured[key];
    if (typeof value === 'number') summary[key] = value;
  }
  if (structured.counts && typeof structured.counts === 'object' && !Array.isArray(structured.counts)) {
    const counts: JsonRecord = {};
    for (const [key, value] of Object.entries(asRecord(structured.counts)).slice(0, 32)) {
      if (typeof value === 'number') counts[key] = value;
    }
    if (Object.keys(counts).length > 0) summary.counts = counts;
  }
  if (Array.isArray(structured.items)) summary.item_count = structured.items.length;
  if (Array.isArray(structured.findings)) summary.finding_count = structured.findings.length;
  return summary;
}

function enrichAttachedGuidanceResult(result: JsonRecord, toolName: string, connection: ChildConnection, includeRuntimeMetadata: boolean): JsonRecord {
  if (!includeRuntimeMetadata || (!toolName.endsWith('_guidance') && toolName !== 'guidance')) return result;
  const structuredContent = asRecord(result.structuredContent);
  return {
    ...result,
    structuredContent: {
      ...structuredContent,
      loader_runtime_lifecycle: loaderRuntimeLifecycle(connection.connectionId, connection.lifecycle),
      loader_runtime_freshness: loaderRuntimeFreshness(),
    },
  };
}

async function detachConnection(args: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  const connection = getConnection(args, state);
  const termination = await terminateConnection(connection);
  state.connections.delete(connection.connectionId);
  return {
    schema: 'narada.mcp_loader.detached.v1',
    connection_id: connection.connectionId,
    surface_id: connection.surfaceId,
    status: 'detached',
    termination,
  };
}

async function restartConnection(args: JsonRecord, state: LoaderState): Promise<JsonRecord> {
  const connection = getConnection(args, state);
  const admitted = connection.bindingId
    ? admitBindingForOperation(state, connection.siteRoot, connection.bindingId, 'restart')
    : (assertBindingAdmissionAvailable(state), null);
  assertLoaderRuntimeCurrentForSpawn('mcp_loader_surface_restart');
  if (connection.lifecycle.mode !== 'replayable') {
    throw diagnosticError(
      'surface_restart_not_loader_replayable',
      `surface_restart_not_loader_replayable:${connection.surfaceId}:${connection.lifecycle.mode}`,
      {
        connection_id: connection.connectionId,
        surface_id: connection.surfaceId,
        lifecycle: connection.lifecycle,
        runtime_lifecycle: loaderRuntimeLifecycle(connection.connectionId, connection.lifecycle),
        recovery_actions: loaderRecoveryActions(connection),
      },
    );
  }
  const previous = connectionStatusFields(connection);
  const previousConnectionId = connection.connectionId;
  const termination = await terminateConnection(connection);
  state.connections.delete(previousConnectionId);
  const replacement = await openConnection({
    state,
    siteRoot: connection.siteRoot,
    surfaceId: connection.surfaceId,
    runtimeKind: connection.runtimeKind,
    runtimeRequirements: connection.runtimeRequirements,
    runtimeCommand: connection.runtimeCommand,
    entrypoint: connection.entrypoint,
    resolvedArgs: connection.args,
    childInvocationKind: connection.childInvocationKind,
    requestedEntrypoint: connection.requestedEntrypoint,
    extraArgs: connection.extraArgs,
    logicalConnectionId: connection.logicalConnectionId,
    metadata: {
      serverName: connection.serverName,
      projectionId: connection.projectionId,
      execution: connection.execution,
      lifecycle: connection.lifecycle,
      descriptor: connection.descriptor,
      descriptorDigest: connection.descriptorDigest,
      declaredToolContractDigest: connection.declaredToolContractDigest,
    },
    bindingAdmission: admitted?.entry ?? null,
  });
  return {
    schema: 'narada.mcp_loader.surface_restarted.v1',
    status: 'restarted',
    reason: optionalString(args.reason),
    previous_connection: previous,
    replacement_connection: connectionStatusFields(replacement),
    connection_id: replacement.connectionId,
    previous_connection_id: previousConnectionId,
    surface_id: replacement.surfaceId,
    runtime_lifecycle: loaderRuntimeLifecycle(replacement.connectionId, replacement.lifecycle),
    entrypoint: replacement.entrypoint,
    args: replacement.args,
    termination,
    server_info: replacement.serverInfo,
    tools: replacement.toolSnapshot,
  };
}

export type ChildInvocationKind = 'entrypoint' | 'native_entrypoint' | 'native_applet';
type ResolvedSurfaceLaunch = { runtimeCommand: string; entrypoint: string; resolvedArgs: string[]; childInvocationKind: ChildInvocationKind };

export function assertSurfaceLaunchMetadata(childInvocationKind: ChildInvocationKind, entrypoint: string, runtimeCommand: string): void {
  if (childInvocationKind !== 'entrypoint' || !/\.(?:exe|com|cmd|bat)$/i.test(entrypoint)) return;
  throw diagnosticError('surface_native_invocation_metadata_missing', 'surface_native_invocation_metadata_missing', {
    entrypoint,
    runtime_command: runtimeCommand,
    child_invocation_kind: childInvocationKind,
    expected_invocation_kinds: ['native_entrypoint', 'native_applet'],
    spawn_permitted: false,
    remediation: 'Regenerate the registrar-owned Site fragment with --child-invocation-kind native_entrypoint or native_applet. A native executable must never be passed to a JavaScript runtime as an entrypoint.',
  });
}

async function resolveSurfaceEntrypoint(siteRoot: string, surfaceId: string, explicitEntrypoint: string | null, extraArgs: string[] | null): Promise<ResolvedSurfaceLaunch> {
  if (explicitEntrypoint) {
    return { runtimeCommand: resolveJavaScriptRuntime(), entrypoint: normalizePath(explicitEntrypoint), resolvedArgs: extraArgs ?? [], childInvocationKind: 'entrypoint' };
  }
  const fabric = readSiteFabric(siteRoot);
  const servers = asRecord(fabric.mcpServers);
  const server = findSiteServer(servers, surfaceId)?.server;
  if (server) {
    const rec = asRecord(server);
    const command = String(rec.command ?? '');
    const rawArgs = stringArray(rec.args) ?? [];
    const launch = resolveFabricLaunch(command, rawArgs);
    return { ...launch, resolvedArgs: [...launch.resolvedArgs, ...extraArgs ?? []] };
  }
  const shared = SHARED_SURFACE_REGISTRY[surfaceId];
  if (shared) {
    const entrypoint = normalizePath(shared.entrypoint.replace(/{site_root}/g, siteRoot));
    const args = shared.args.map((a) => interpolateSiteArg(a, siteRoot));
    return { runtimeCommand: resolveJavaScriptRuntime(), entrypoint, resolvedArgs: [...args, ...extraArgs ?? []], childInvocationKind: 'entrypoint' };
  }
  throw diagnosticError('surface_not_found', `surface_not_found:${surfaceId}`);
}

function argValue(args: string[], name: string): string | null {
  const index = args.indexOf(name);
  return index >= 0 && args[index + 1] ? args[index + 1]! : null;
}

function isRuntimeProxyLaunch(command: string, args: string[]): boolean {
  const base = basename(normalizePath(command.trim())).toLowerCase();
  return (base === 'narada-mcp-runtime.exe' || base === 'narada-mcp-runtime') && args[0] === 'proxy'
    || (args[0] ?? '').replace(/\\/g, '/').endsWith('/mcp-runtime-proxy/dist/src/main.js');
}

function resolveFabricLaunch(command: string, args: string[]): ResolvedSurfaceLaunch {
  if (isRuntimeProxyLaunch(command, args)) {
    const childCommand = argValue(args, '--child-command');
    const childEntrypoint = argValue(args, '--entrypoint');
    const separatorIndex = args.indexOf('--');
    if (!childCommand || !childEntrypoint || separatorIndex < 0) {
      throw diagnosticError('surface_command_unsupported', `surface_command_unsupported:runtime-proxy:${command}`, { command, args, remediation: 'Regenerate the registrar-owned Site fragment so proxy child-command and entrypoint metadata are complete.' });
    }
    const childInvocationKind = argValue(args, '--child-invocation-kind');
    const invocationKind = (childInvocationKind ?? 'entrypoint') as ChildInvocationKind;
    if (!['entrypoint', 'native_entrypoint', 'native_applet'].includes(invocationKind)) {
      throw diagnosticError('surface_native_child_unsupported', `surface_native_child_unsupported:${invocationKind}`, { child_command: childCommand, child_entrypoint: childEntrypoint });
    }
    if (invocationKind === 'entrypoint') {
      return { runtimeCommand: resolveDeclaredRuntime(childCommand), entrypoint: normalizePath(childEntrypoint), resolvedArgs: args.slice(separatorIndex + 1), childInvocationKind: invocationKind };
    }
    const nativeCommand = resolveNativeChildCommand(childCommand);
    const nativeArgs = args.slice(separatorIndex + 1);
    if (invocationKind === 'native_applet') {
      const childApplet = argValue(args, '--child-applet');
      if (!childApplet) {
        throw diagnosticError('surface_native_child_unsupported', `surface_native_child_unsupported:${invocationKind}`, { child_command: childCommand, child_entrypoint: childEntrypoint });
      }
      nativeArgs.unshift(childApplet);
    }
    return { runtimeCommand: nativeCommand, entrypoint: nativeCommand, resolvedArgs: nativeArgs, childInvocationKind: invocationKind };
  }
  const entrypoint = extractRuntimeEntrypoint(command, args);
  if (!entrypoint) throw diagnosticError('surface_command_unsupported', `surface_command_unsupported:${command}`);
  return { runtimeCommand: resolveDeclaredRuntime(command), entrypoint: normalizePath(entrypoint), resolvedArgs: removeEntrypointArg(args, entrypoint), childInvocationKind: 'entrypoint' };
}

function resolveNativeChildCommand(command: string): string {
  const normalized = normalizePath(command.trim());
  if (!isAbsolute(normalized) || !existsSync(normalized)) {
    throw diagnosticError('child_runtime_executable_unresolved', `child_runtime_executable_unresolved:${command}`, {
      child_command: command,
      remediation: 'Regenerate the registrar-owned Site fragment with an existing absolute native child executable.',
    });
  }
  return resolve(normalized);
}

function extractRuntimeEntrypoint(command: string, args: string[]): string | null {
  if (isRuntimeProxyLaunch(command, args)) return argValue(args, '--entrypoint');
  const normalizedCommand = normalizePath(command.trim());
  const commandBaseName = normalizedCommand.slice(normalizedCommand.lastIndexOf('/') + 1).toLowerCase();
  if (['node', 'node.exe', 'bun', 'bun.exe'].includes(commandBaseName)) {
    return args.find((arg) => /\.m?js$/i.test(arg) || /\.cjs$/i.test(arg)) ?? null;
  }
  const commandEntrypoint = command.replace(/^node\s+--import\s+tsx\s+/i, '').replace(/^node\s+/i, '').trim();
  if (commandEntrypoint && commandEntrypoint !== 'node') return commandEntrypoint;
  return args.find((arg) => /\.m?js$/i.test(arg) || /\.cjs$/i.test(arg)) ?? null;
}
function resolveJavaScriptRuntime(): string {
  const runtime = resolve(process.execPath);
  if (existsSync(runtime)) return runtime;
  throw diagnosticError('child_runtime_executable_unresolved', `child_runtime_executable_unresolved:${process.execPath}`, {
    child_command: process.execPath,
    remediation: 'Start mcp-loader with an absolute Node or Bun executable.',
  });
}

function resolveDeclaredRuntime(command: string): string {
  const normalized = command.trim();
  const base = basename(normalized).toLowerCase();
  if (/\.(cmd|bat)$/i.test(base)) {
    throw diagnosticError('child_runtime_wrapper_unsupported', `child_runtime_wrapper_unsupported:${command}`, {
      child_command: command,
      remediation: 'Regenerate the registrar-owned Site fragment with an absolute bun.exe or node.exe executable; shell wrappers are not valid child runtimes.',
    });
  }
  const engine = base === 'bun' || base === 'bun.exe' ? 'bun' : base === 'node' || base === 'node.exe' ? 'node' : null;
  if (isAbsolute(normalized)) {
    if (engine && existsSync(normalized)) return resolve(normalized);
    throw diagnosticError('child_runtime_executable_unresolved', `child_runtime_executable_unresolved:${command}`, {
      child_command: command,
      runtime_engine: engine,
      remediation: 'Regenerate the registrar-owned Site fragment with an existing absolute bun.exe or node.exe executable.',
    });
  }
  if (!engine) return resolveJavaScriptRuntime();
  const candidates: string[] = [];
  const processBase = basename(process.execPath).toLowerCase();
  if ((engine === 'bun' && (processBase === 'bun' || processBase === 'bun.exe')) || (engine === 'node' && (processBase === 'node' || processBase === 'node.exe'))) candidates.push(process.execPath);
  if (engine === 'bun') candidates.push(resolve(homedir(), '.bun', 'bin', process.platform === 'win32' ? 'bun.exe' : 'bun'));
  if (engine === 'node' && process.platform === 'win32') {
    candidates.push(resolve(process.env.PROGRAMFILES ?? 'C:\\Program Files', 'nodejs', 'node.exe'));
    candidates.push(resolve(process.env['PROGRAMFILES(X86)'] ?? 'C:\\Program Files (x86)', 'nodejs', 'node.exe'));
  }
  const separator = process.platform === 'win32' ? ';' : ':';
  for (const rawDir of (process.env.PATH || process.env.Path || '').split(separator)) {
    if (!rawDir.trim()) continue;
    candidates.push(join(rawDir.trim(), process.platform === 'win32' ? `${engine}.exe` : engine));
  }
  const resolved = candidates.find((candidate) => isAbsolute(candidate) && existsSync(candidate));
  if (resolved) return resolve(resolved);
  if (processBase === 'bun' || processBase === 'bun.exe' || processBase === 'node' || processBase === 'node.exe') {
    return resolve(process.execPath);
  }
  throw diagnosticError('child_runtime_executable_unresolved', `child_runtime_executable_unresolved:${command}`, { child_command: command, runtime_engine: engine, remediation: 'Regenerate the registrar-owned Site fragment with an absolute runtime executable or install the declared runtime.' });
}

function removeEntrypointArg(args: string[], entrypoint: string): string[] {
  let removed = false;
  const normalizedEntrypoint = normalizePath(entrypoint);
  return args.filter((arg) => {
    if (!removed && normalizePath(arg) === normalizedEntrypoint) {
      removed = true;
      return false;
    }
    return true;
  });
}

function classifyFabricEntrypoint({ siteRoot, declaredEntrypoint, expectedEntrypoint, entrypointExists }: {
  siteRoot: string;
  declaredEntrypoint: string | null;
  expectedEntrypoint: string | null;
  entrypointExists: boolean;
}): { status: string; remediation: string[] } {
  if (!declaredEntrypoint) {
    return {
      status: 'entrypoint_unresolved',
      remediation: ['Inspect the site fabric command and args; mcp-loader could not determine the JavaScript entrypoint.'],
    };
  }
  if (!entrypointExists) {
    return {
      status: 'stale_entrypoint',
      remediation: ['Repair or regenerate the site MCP fabric so the declared entrypoint exists before attach.'],
    };
  }
  if (expectedEntrypoint && declaredEntrypoint === expectedEntrypoint) {
    return {
      status: 'matches_shared_registry',
      remediation: [],
    };
  }
  if (isUnderPath(declaredEntrypoint, siteRoot)) {
    return {
      status: expectedEntrypoint ? 'site_local_override' : 'site_local_surface',
      remediation: ['Treat this as site-local authority; compare expected tools before replacing it with the shared registry entrypoint.'],
    };
  }
  if (expectedEntrypoint) {
    return {
      status: 'external_entrypoint_override',
      remediation: ['Classify as intentional override or drift at the fabric generator/registrar layer before local repair. Compare tool counts and authority implications against the shared registry entrypoint.'],
    };
  }
  return {
    status: 'external_site_declared_surface',
    remediation: ['Verify the external entrypoint authority and allowed-entrypoint policy before attach.'],
  };
}

function isUnderPath(child: string, parent: string): boolean {
  const normalizedChild = normalizePath(child);
  const normalizedParent = normalizePath(parent);
  return normalizedChild === normalizedParent || normalizedChild.startsWith(`${normalizedParent}/`);
}

const SHARED_SURFACE_REGISTRY: Record<string, { entrypoint: string; args: string[] }> = {
  'operator-console-overlay': { entrypoint: MCP_SURFACES_ROOT + '/operator-console-overlay-mcp/dist/src/main.js', args: [] },
  'local-filesystem': { entrypoint: `${MCP_SURFACES_ROOT}/local-filesystem-mcp/dist/src/main.js`, args: ['--mode', 'write', '--allowed-root', '{site_root}', '--anchored-allowed-root', 'user_home:.codex', '--output-root', '{site_root}'] },
  'structured-command': { entrypoint: `${MCP_SURFACES_ROOT}/structured-command-mcp/dist/src/main.js`, args: ['--allowed-root', '{site_root}', '--allow-command', 'node', '--allow-command', 'pnpm', '--allow-command', 'npm'] },
  'git': { entrypoint: `${MCP_SURFACES_ROOT}/git-mcp/dist/src/main.js`, args: ['--allowed-root', '{workspace_root}', '--output-root', '{site_root}', '--mode', 'write'] },
  'site-inbox': { entrypoint: `${MCP_SURFACES_ROOT}/site-inbox-mcp/dist/src/main.js`, args: ['--site-root', '{site_root}'] },
  'mailbox': { entrypoint: `${MCP_SURFACES_ROOT}/mailbox-mcp/dist/src/main.js`, args: ['--site-root', '{site_root}'] },
  'graph-mail': { entrypoint: `${MCP_SURFACES_ROOT}/graph-mail-mcp/dist/src/main.js`, args: ['--site-root', '{site_root}'] },
  'calendar': { entrypoint: `${MCP_SURFACES_ROOT}/calendar-mcp/dist/src/main.js`, args: ['--site-root', '{site_root}'] },
  'task-lifecycle': { entrypoint: `${MCP_SURFACES_ROOT}/task-lifecycle-mcp/dist/src/task-lifecycle/task-mcp-server.js`, args: ['--site-root', '{site_root}'] },
  'agent-context': { entrypoint: `${MCP_SURFACES_ROOT}/agent-context-mcp/dist/src/main.js`, args: ['--site-root', '{site_root}'] },
  'catalog-observation': { entrypoint: `${MCP_SURFACES_ROOT}/catalog-observation-mcp/dist/src/main.js`, args: [] },
  'runtime-introspection': { entrypoint: `${MCP_SURFACES_ROOT}/runtime-introspection-mcp/dist/src/main.js`, args: [] },
  'worker-delegation': { entrypoint: requiredNativeSurfaceEntrypoint('shared/mcp-surfaces-native', 'narada-mcp-surfaces.exe'), args: ['--surface-id', 'worker-delegation', '--site-root', '{site_root}', '--allowed-root', '{site_root}', '--run-root', '{site_runtime_root}/worker-delegation'] },
  'delegated-task': { entrypoint: requiredNativeSurfaceEntrypoint('shared/mcp-surfaces-native', 'narada-mcp-surfaces.exe'), args: ['--surface-id', 'delegated-task', '--task-root', '{site_root}', '--site-root', '{site_root}', '--allowed-root', '{site_root}'] },
  'sop': { entrypoint: `${MCP_SURFACES_ROOT}/sop-mcp/dist/src/main.js`, args: ['--sop-root', '{site_root}', '--server-name', '{site_id}-sop'] },
  'scheduler': { entrypoint: `${MCP_SURFACES_ROOT}/scheduler-mcp/dist/src/main.js`, args: ['--allowed-root', '{site_root}'] },
  'mcp-registrar': { entrypoint: requiredNativeSurfaceEntrypoint('mcp-registrar', `narada-mcp-registrar${process.platform === 'win32' ? '.exe' : ''}`), args: [] },
  'surface-feedback': {
    entrypoint: `${MCP_SURFACES_ROOT}/surface-feedback-mcp/dist/src/main.js`,
    args: ['--feedback-root', '{site_control_root}/feedback', '--canonical-feedback-root', '{site_control_root}/feedback', '--task-lifecycle-root', '{site_root}', '--site-id', '{site_id}'],
  },
  'speech': { entrypoint: `${MCP_SURFACES_ROOT}/speech-mcp/dist/src/main.js`, args: [] },
  'cloudflare-carrier': { entrypoint: `${MCP_SURFACES_ROOT}/cloudflare-carrier-mcp/dist/src/main.js`, args: ['--site-root', '{site_root}'] },
  'site-coherence': { entrypoint: `${MCP_SURFACES_ROOT}/site-coherence-mcp/dist/src/main.js`, args: ['--site-root', '{site_root}'] },
  'site-lifecycle': { entrypoint: `${MCP_SURFACES_ROOT}/site-lifecycle-mcp/dist/src/main.js`, args: ['--narada-root', '{site_root}'] },
  'artifacts': { entrypoint: `${MCP_SURFACES_ROOT}/artifacts-mcp/dist/src/main.js`, args: [] },
  'nars-session': { entrypoint: `${MCP_SURFACES_ROOT}/nars-session-mcp/dist/src/main.js`, args: [] },
  'quota-meter': { entrypoint: `${MCP_SURFACES_ROOT}/quota-meter-mcp/dist/src/main.js`, args: [] },
};

function resolveSiteFabricPaths(siteRoot: string): string[] {
  const mcpDir = resolve(siteRoot, '.ai', 'mcp');
  const canonicalPath = resolve(mcpDir, 'config.json');
  const canonicalExists = existsSync(canonicalPath);
  const canonicalHasServers = canonicalExists ? siteFabricHasDeclaredServers(canonicalPath) : null;
  if (canonicalExists && canonicalHasServers !== false) return [canonicalPath];
  const siteBase = basename(siteRoot).replace(/\./g, '-');
  const siteAggregatePath = resolve(mcpDir, `${siteBase}-mcp.json`);
  if (existsSync(siteAggregatePath)) return [siteAggregatePath];
  if (!existsSync(mcpDir)) {
    throw diagnosticError('site_fabric_not_found', `site_fabric_not_found:${canonicalPath}`);
  }
  const candidates = readdirSync(mcpDir)
    .filter((name) => name.endsWith('-mcp.json'))
    .sort()
    .map((name) => resolve(mcpDir, name))
    .filter((candidatePath) => {
      try {
        return Boolean(asRecord(JSON.parse(readFileSync(candidatePath, 'utf8'))).mcpServers);
      } catch {
        return false;
      }
    });
  if (candidates.length > 0) return candidates;
  if (canonicalExists) return [canonicalPath];
  throw diagnosticError('site_fabric_not_found', `site_fabric_not_found:${canonicalPath}`);
}

function siteFabricHasDeclaredServers(fabricPath: string): boolean | null {
  try {
    const fragment = asRecord(JSON.parse(readFileSync(fabricPath, 'utf8')));
    return Object.keys(asRecord(fragment.mcpServers)).length > 0;
  } catch {
    return null;
  }
}

function readSiteFabricBundle(siteRoot: string): { fabric: JsonRecord; paths: string[]; sourceBySurface: Record<string, string> } {
  const paths = resolveSiteFabricPaths(siteRoot);
  const mcpServers: JsonRecord = {};
  const sourceBySurface: Record<string, string> = {};
  let siteId: string | null = null;
  for (const fabricPath of paths) {
    let fragment: JsonRecord;
    try {
      fragment = asRecord(JSON.parse(readFileSync(fabricPath, 'utf8')));
    } catch (error) {
      throw diagnosticError('site_fabric_parse_error', `site_fabric_parse_error:${fabricPath}:${error instanceof Error ? error.message : String(error)}`);
    }
    const fragmentSiteId = typeof fragment.site_id === 'string'
      ? assertSupportedSiteId(fragment.site_id, fabricPath)
      : null;
    if (fragmentSiteId && siteId && fragmentSiteId !== siteId) {
      throw diagnosticError('site_fabric_site_id_mismatch', `site_fabric_site_id_mismatch:${siteId}:${fragmentSiteId}:${fabricPath}`);
    }
    if (fragmentSiteId) siteId = fragmentSiteId;
    for (const [surfaceId, server] of Object.entries(asRecord(fragment.mcpServers))) {
      if (Object.hasOwn(mcpServers, surfaceId)) {
        throw diagnosticError('site_fabric_duplicate_surface', `site_fabric_duplicate_surface:${surfaceId}:${sourceBySurface[surfaceId]}:${fabricPath}`);
      }
      mcpServers[surfaceId] = server;
      sourceBySurface[surfaceId] = fabricPath;
    }
  }
  return {
    fabric: {
      schema: paths.length === 1 ? 'narada.mcp_loader.site_fabric.v1' : 'narada.mcp_loader.fragmented_site_fabric.v1',
      site_id: siteId,
      mcpServers,
    },
    paths,
    sourceBySurface,
  };
}

function readSiteFabric(siteRoot: string): JsonRecord {
  return readSiteFabricBundle(siteRoot).fabric;
}

function findSiteServer(servers: JsonRecord, requestedSurfaceId: string): { serverKey: string; server: JsonRecord } | null {
  const direct = servers[requestedSurfaceId];
  if (direct) return { serverKey: requestedSurfaceId, server: asRecord(direct) };
  const matches: Array<{ serverKey: string; server: JsonRecord }> = [];
  for (const [serverKey, rawServer] of Object.entries(servers)) {
    const server = asRecord(rawServer);
    if (server.surface_id === requestedSurfaceId) matches.push({ serverKey, server });
  }
  if (matches.length > 1) {
    throw diagnosticError(
      'surface_id_ambiguous',
      `surface_id_ambiguous:${requestedSurfaceId}`,
      { surface_id: requestedSurfaceId, server_names: matches.map((match) => match.serverKey).sort() },
    );
  }
  if (matches.length === 1) return matches[0];
  return null;
}

function buildChildEnv(siteRoot: string, policy: LoaderPolicy, ownership: { connectionId: string; logicalConnectionId: string; generationId: string }): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {};
  for (const key of policy.allowedEnvVars) {
    if (process.env[key] !== undefined) env[key] = process.env[key];
  }
  env.NARADA_SITE_ROOT = siteRoot;
  env.NARADA_MCP_LOADER_RUN_ID = LOADER_RUN_ID;
  env.NARADA_MCP_LOADER_CONNECTION_ID = ownership.connectionId;
  env.NARADA_MCP_LOADER_LOGICAL_CONNECTION_ID = ownership.logicalConnectionId;
  env.NARADA_MCP_LOADER_GENERATION_ID = ownership.generationId;
  env.NARADA_MCP_LOADER_OWNER_PID = String(LOADER_OWNER_PID);
  env.NARADA_MCP_LOADER_PARENT_PID = String(LOADER_OWNER_PID);
  env.NARADA_MCP_LOADER_OWNERSHIP_MARKER = LOADER_OWNERSHIP_MARKER;
  return env;
}

function ensureSiteRootAllowed(siteRoot: string, policy: LoaderPolicy) {
  const realSiteRoot = normalizePath(siteRoot);
  for (const allowed of policy.allowedSiteRoots) {
    const candidate = process.platform === 'win32' ? realSiteRoot.toLocaleLowerCase('en-US') : realSiteRoot;
    const boundary = process.platform === 'win32' ? allowed.toLocaleLowerCase('en-US') : allowed;
    if (candidate === boundary || candidate.startsWith(boundary + '/')) return;
  }
  throw diagnosticError('site_root_not_allowed', `site_root_not_allowed:${siteRoot}`);
}

function ensureSurfaceAllowed(surfaceId: string, siteRoot: string, policy: LoaderPolicy) {
  if (policy.allowedSurfaceIds === 'site_fabric') {
    const fabric = readSiteFabric(siteRoot);
    const servers = asRecord(fabric.mcpServers);
    if (findSiteServer(servers, surfaceId)) return;
    if (SHARED_SURFACE_REGISTRY[surfaceId]) return;
    throw diagnosticError('surface_not_allowed', `surface_not_allowed:${surfaceId}`);
  }
  if (!policy.allowedSurfaceIds.includes(surfaceId)) {
    const matched = findSiteServer(asRecord(readSiteFabric(siteRoot).mcpServers), surfaceId);
    if (matched && policy.allowedSurfaceIds.includes(matched.serverKey)) return;
    throw diagnosticError('surface_not_allowed', `surface_not_allowed:${surfaceId}`);
  }
}

function ensureEntrypointAllowed(siteRoot: string, entrypoint: string, policy: LoaderPolicy) {
  const realEntrypoint = normalizePath(entrypoint);
  const resolvedSiteRoot = normalizePath(siteRoot);
  for (const prefix of policy.allowedEntrypointPrefixes) {
    const expanded = prefix.replace(/{site_root}/g, resolvedSiteRoot);
    if (realEntrypoint === expanded || realEntrypoint.startsWith(`${expanded}/`)) return;
  }
  throw diagnosticError('entrypoint_not_allowed', `entrypoint_not_allowed:${entrypoint}`);
}

function getConnection(args: JsonRecord, state: LoaderState): ChildConnection {
  const connectionId = requiredString(args.connection_id, 'missing_connection_id');
  const connection = state.connections.get(connectionId);
  if (!connection) throw diagnosticError('connection_not_found', `connection_not_found:${connectionId}`);
  return connection;
}

async function sendChildRequest(connection: ChildConnection, method: string, params: JsonRecord, timeoutMs: number): Promise<JsonRecord> {
  if (connection.detached) throw diagnosticError('connection_detached', `connection_detached:${connection.connectionId}`);
  const id = connection.nextId++;
  const message = { jsonrpc: '2.0', id, method, params };
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      connection.pending.delete(id);
      reject(diagnosticError('child_timeout', `child_timeout:${method}:${timeoutMs}ms`, childRuntimeDiagnostic(connection, { method, timeout_ms: timeoutMs })));
    }, timeoutMs);
    connection.pending.set(id, { resolve, reject, timeout });
    writeToChild(connection, message);
  });
}

async function sendChildNotification(connection: ChildConnection, method: string, params: JsonRecord): Promise<void> {
  if (connection.detached) return;
  writeToChild(connection, { jsonrpc: '2.0', method, params });
}

function writeToChild(connection: ChildConnection, message: JsonRecord) {
  const body = JSON.stringify(message);
  const stdin = connection.process.stdin;
  if (!stdin || stdin.destroyed) {
    throw diagnosticError('child_stdin_closed', `child_stdin_closed:${connection.connectionId}`);
  }
  stdin.write(`${body}\n`);
}

function handleChildStdout(chunk: string, connection: ChildConnection) {
  connection.buffer += chunk;
  const drained = connection.buffer.includes('Content-Length:') ? drainJsonRpcFrames(connection.buffer) : drainJsonLines(connection.buffer);
  connection.buffer = drained.remaining;
  for (const message of drained.requests) {
    const id = Number(message.id);
    if (Number.isFinite(id)) {
      const pending = connection.pending.get(id);
      if (pending) {
        connection.pending.delete(id);
        clearTimeout(pending.timeout);
        if (message.error) pending.reject(new Error(String((message.error as JsonRecord).message ?? 'child_error')));
        else pending.resolve(asRecord(message.result));
      }
    }
  }
}

function childError(connection: ChildConnection, error: Error) {
  for (const [_, pending] of connection.pending) {
    clearTimeout(pending.timeout);
    pending.reject(diagnosticError('child_error', error.message, childRuntimeDiagnostic(connection)));
  }
  connection.pending.clear();
}

function observationSink(siteRoot: string): RuntimeObservationSink | null {
  const existing = observationSinks.get(siteRoot);
  if (existing) return existing;
  try {
    const created = createRuntimeObservationSink({ site_root: siteRoot, source_id: `mcp-loader-${process.pid}` });
    observationSinks.set(siteRoot, created);
    return created;
  } catch {
    return null;
  }
}

function emitLoaderConnectionOwner(connection: ChildConnection): void {
  const sink = observationSink(connection.siteRoot);
  if (!sink) return;
  const siteId = deriveSiteId(connection.siteRoot);
  const authorityRef = `site:${siteId}:mcp-surfaces`;
  const observedAt = new Date().toISOString();
  const loaderOwner = `mcp-loader-${process.pid}`;
  void sink.emit({
    schema: 'narada.mcp_runtime.resource_owner.v1', owner_id: loaderOwner, site_id: siteId,
    authority_ref: authorityRef, owner_kind: 'carrier_proxy', pid: process.pid, process_started_at: null,
    parent_owner_id: null, surface_id: 'mcp-loader', instance_id: null, generation_id: null,
    carrier_session_id: process.env.NARADA_CARRIER_SESSION_ID?.trim() || null,
    executable_name: process.execPath, observed_at: observedAt,
  });
  void sink.emit({
    schema: 'narada.mcp_runtime.resource_owner.v1', owner_id: `loader-child-${connection.connectionId}`,
    site_id: siteId, authority_ref: authorityRef, owner_kind: 'loader_child', pid: connection.process.pid ?? null,
    process_started_at: null, parent_owner_id: loaderOwner, surface_id: connection.surfaceId,
    instance_id: connection.logicalConnectionId, generation_id: connection.generationId,
    carrier_session_id: process.env.NARADA_CARRIER_SESSION_ID?.trim() || null,
    executable_name: connection.entrypoint, observed_at: observedAt,
  });
  emitLoaderConnectionLifecycle(connection, 'process_started', 'ok');
}

function emitLoaderConnectionLifecycle(
  connection: ChildConnection,
  eventType: 'process_started' | 'process_exited',
  status: 'ok' | 'failed',
): void {
  const sink = observationSink(connection.siteRoot);
  if (!sink) return;
  const siteId = deriveSiteId(connection.siteRoot);
  void sink.emit({
    schema: 'narada.mcp_runtime.lifecycle_event.v1', event_id: `event-${randomUUID()}`,
    occurred_at: new Date().toISOString(), site_id: siteId, authority_ref: `site:${siteId}:mcp-surfaces`,
    owner_id: `loader-child-${connection.connectionId}`, event_type: eventType, surface_id: connection.surfaceId,
    instance_id: connection.logicalConnectionId, generation_id: connection.generationId,
    request_id: null, status, inflight: connection.pending.size,
  });
}

function cleanupConnection(connection: ChildConnection) {
  connection.detached = true;
  for (const [_, pending] of connection.pending) {
    clearTimeout(pending.timeout);
    pending.reject(diagnosticError('child_exited', 'child_exited', childRuntimeDiagnostic(connection)));
  }
  connection.pending.clear();
}

async function terminateConnection(connection: ChildConnection): Promise<JsonRecord> {
  connection.detached = true;
  connection.detachedAt = new Date().toISOString();
  const proc = connection.process;
  if (proc.exitCode !== null || proc.signalCode !== null) return { status: 'already_exited', exit_code: proc.exitCode, signal: proc.signalCode, forced: false };
  const graceful = await signalAndWaitForChild(proc, 'SIGTERM', 5000);
  if (graceful) return { status: 'terminated', exit_code: proc.exitCode, signal: proc.signalCode, forced: false };
  const forced = await signalAndWaitForChild(proc, 'SIGKILL', 1000);
  return { status: forced ? 'terminated' : 'termination_timeout', exit_code: proc.exitCode, signal: proc.signalCode, forced: true };
}

async function signalAndWaitForChild(proc: ChildProcess, signal: NodeJS.Signals, timeoutMs: number): Promise<boolean> {
  if (proc.exitCode !== null || proc.signalCode !== null) return true;
  const close = waitForChildClose(proc, timeoutMs);
  try {
    proc.kill(signal);
  } catch {
    // The close wait still observes a concurrent natural exit.
  }
  return close;
}

function waitForChildClose(proc: ChildProcess, timeoutMs: number): Promise<boolean> {
  if (proc.exitCode !== null || proc.signalCode !== null) return Promise.resolve(true);
  return new Promise((resolvePromise) => {
    let settled = false;
    const finish = (closed: boolean) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      proc.off('exit', onExit);
      proc.off('close', onClose);
      resolvePromise(closed);
    };
    const onExit = () => finish(true);
    const onClose = () => finish(true);
    const timer = setTimeout(() => finish(false), timeoutMs);
    proc.once('exit', onExit);
    proc.once('close', onClose);
  });
}

function connectionStatusFields(connection: ChildConnection): JsonRecord {
  const proc = connection.process;
  const live = isConnectionLive(connection);
  const observedAt = new Date().toISOString();
  return {
    connection_id: connection.connectionId,
    ownership: connectionOwnershipFields(connection),
    logical_connection_id: connection.logicalConnectionId,
    generation_id: connection.generationId,
    site_root: connection.siteRoot,
    surface_id: connection.surfaceId,
    server_name: connection.serverName,
    projection_id: connection.projectionId,
    execution: connection.execution,
    runtime_kind: connection.runtimeKind,
    runtime_requirements: connection.runtimeRequirements,
    runtime_command: connection.runtimeCommand,
    lifecycle: connection.lifecycle,
    runtime_lifecycle: loaderRuntimeLifecycle(connection.connectionId, connection.lifecycle),
    runtime_freshness: loaderRuntimeFreshness(),
    entrypoint: connection.entrypoint,
    args: connection.args,
    child_invocation_kind: connection.childInvocationKind,
    status: live ? 'live' : 'closed',
    detached: connection.detached,
    initialized: connection.initialized,
    pid: proc.pid ?? null,
    exit_code: proc.exitCode,
    signal_code: proc.signalCode,
    killed: proc.killed,
    pending_count: connection.pending.size,
    attached_at: connection.attachedAt,
    detached_at: connection.detachedAt,
    stderr_tail: connection.stderrTail,
    server_info: connection.serverInfo,
    tool_count: connection.toolSnapshot?.length ?? null,
    descriptor_digest: connection.descriptorDigest,
    declared_tool_contract_digest: connection.declaredToolContractDigest,
    tool_contract_digest: connection.toolContractDigest,
    heartbeat_at: connection.heartbeatAt,
    lease_expires_at: connection.leaseExpiresAt,
    active_generation: live ? runtimeGeneration(connection, observedAt) : null,
    draining_generations: [],
    recovery_actions: loaderRecoveryActions(connection),
  };
}

function isConnectionLive(connection: ChildConnection): boolean {
  const proc = connection.process;
  return !connection.detached && proc.exitCode === null && proc.signalCode === null && !proc.killed;
}

function touchConnection(connection: ChildConnection): void {
  const heartbeatAt = new Date().toISOString();
  connection.heartbeatAt = heartbeatAt;
  connection.leaseExpiresAt = new Date(Date.parse(heartbeatAt) + DEFAULT_RUNTIME_LEASE_MS).toISOString();
}

function runtimeGeneration(connection: ChildConnection, observedAt: string): JsonRecord {
  const leaseExpiresAtMs = Date.parse(connection.leaseExpiresAt);
  const observedAtMs = Date.parse(observedAt);
  const fresh = Number.isFinite(leaseExpiresAtMs) && leaseExpiresAtMs > observedAtMs;
  return {
    generation_id: connection.generationId,
    state: 'active',
    started_at: connection.attachedAt,
    activated_at: connection.attachedAt,
    heartbeat_at: connection.heartbeatAt,
    lease_expires_at: connection.leaseExpiresAt,
    freshness: fresh ? 'current' : 'stale',
    health: isConnectionLive(connection) ? 'healthy' : 'unreachable',
    descriptor_digest: connection.descriptorDigest,
    tool_contract_digest: connection.toolContractDigest,
    inflight: connection.pending.size,
  };
}

function loaderRecoveryActions(connection: ChildConnection): JsonRecord[] {
  if (connection.lifecycle.mode !== 'replayable') {
    return [{
      actuator: 'carrier-supervisor',
      tool_name: null,
      arguments: {
        connection_id: connection.connectionId,
        logical_connection_id: connection.logicalConnectionId,
        capability: 'restart_mcp_loader_process',
      },
      guidance: 'This projection is not loader-replayable. Ask the carrier supervisor to invoke restart_mcp_loader_process for the attached MCP loader before reconnecting the session.',
    }];
  }
  return [{
    actuator: 'mcp-loader',
    tool_name: 'mcp_loader_surface_restart',
    arguments: { connection_id: connection.connectionId },
    guidance: 'Invoke mcp_loader_surface_restart with the connection_id to replace this child generation; this does not restart the agent session or loader process.',
  }];
}

function observedToolContractDigest(tools: JsonRecord[], descriptor: SurfaceDescriptorV2 | null): string | null {
  if (tools.length === 0) return null;
  const surfaceTools = tools.filter((tool) => String(tool.name ?? '') !== RUNTIME_PROXY_STATUS_TOOL_NAME);
  if (surfaceTools.length === 0) return null;
  if (descriptor !== null) {
    const liveTools: McpToolDefinition[] = surfaceTools.map((tool) => ({
      name: String(tool.name ?? ''),
      description: String(tool.description ?? ''),
      inputSchema: asRecord(tool.inputSchema ?? tool.input_schema),
      ...(tool.outputSchema === undefined && tool.output_schema === undefined
        ? {}
        : { outputSchema: asRecord(tool.outputSchema ?? tool.output_schema) }),
      ...(tool.annotations === undefined ? {} : { annotations: asRecord(tool.annotations) }),
    }));
    return liveToolsContractDigest(descriptor, liveTools);
  }
  return stableDigest(surfaceTools
    .map((tool) => ({
      name: String(tool.name ?? ''),
      description: tool.description ?? null,
      input_schema: tool.inputSchema ?? tool.input_schema ?? {},
      output_schema: tool.outputSchema ?? tool.output_schema,
      annotations: tool.annotations,
    }))
    .sort((left, right) => left.name.localeCompare(right.name)));
}

function optionalDigest(value: unknown, name: string): string | null {
  const digest = optionalString(value);
  if (digest === null) return null;
  if (!/^[a-f0-9]{64}$/.test(digest)) {
    throw diagnosticError('invalid_runtime_digest', `invalid_runtime_digest:${name}`, { name, value: digest });
  }
  return digest;
}

function surfaceRuntimeMetadata(siteRoot: string, surfaceId: string): RuntimeSurfaceMetadata {
  const fabric = readSiteFabric(siteRoot);
  const matched = findSiteServer(asRecord(fabric.mcpServers), surfaceId);
  const server = matched?.server ?? {};
  const projection = asRecord(server.surface_projection);
  const descriptorCandidate = projection.descriptor ?? projection.surface_descriptor
    ?? server.descriptor ?? server.surface_descriptor;
  let descriptor: SurfaceDescriptorV2 | null = null;
  try {
    descriptor = parseSurfaceDescriptorV2(descriptorCandidate);
  } catch {
    descriptor = null;
  }
  const lifecycleCandidate = projection.lifecycle ?? server.lifecycle;
  const parsedLifecycle = LifecycleRequirementSchema.safeParse(lifecycleCandidate);
  const lifecycle: LifecycleRequirement = parsedLifecycle.success
    ? parsedLifecycle.data
    : { mode: 'replayable' };
  return {
    serverName: matched?.serverKey ?? surfaceId,
    projectionId: optionalString(projection.id)
      ?? optionalString(projection.projection_id)
      ?? optionalString(server.projection_id)
      ?? 'default',
    execution: surfaceExecutionDeclaration(projection.execution),
    lifecycle,
    descriptor,
    descriptorDigest: optionalDigest(
      projection.descriptor_digest ?? projection.surface_descriptor_digest ?? server.descriptor_digest ?? server.surface_descriptor_digest,
      'descriptor_digest',
    ),
    declaredToolContractDigest: optionalDigest(
      projection.tool_contract_digest ?? projection.surface_tool_contract_digest ?? server.tool_contract_digest ?? server.surface_tool_contract_digest,
      'tool_contract_digest',
    ),
  };
}

function enforceRequestSize(args: JsonRecord, maxBytes: number) {
  const size = Buffer.byteLength(JSON.stringify(args), 'utf8');
  if (size > maxBytes) throw diagnosticError('request_too_large', `request_too_large:${size}:${maxBytes}`);
}

function enforceResponseSize(result: JsonRecord, maxBytes: number) {
  const size = Buffer.byteLength(JSON.stringify(result), 'utf8');
  if (size > maxBytes) throw diagnosticError('response_too_large', `response_too_large:${size}:${maxBytes}`);
}
function childRuntimeDiagnostic(connection: ChildConnection, extra: JsonRecord = {}): JsonRecord {
  return {
    connection_id: connection.connectionId,
    surface_id: connection.surfaceId,
    runtime_command: connection.runtimeCommand,
    entrypoint: connection.entrypoint,
    args: connection.args,
    exit_code: connection.process.exitCode,
    signal_code: connection.process.signalCode,
    stderr_tail: connection.stderrTail,
    runtime_lifecycle: loaderRuntimeLifecycle(connection.connectionId, connection.lifecycle),
    ...extra,
  };
}

function callToolResult(result: JsonRecord): JsonRecord {
  return {
    content: [{
      type: 'text',
      text: renderResult(result),
      annotations: { audience: ['assistant'] },
    }],
    structuredContent: result,
  };
}

function renderResult(result: JsonRecord): string {
  const schema = typeof result.schema === 'string' ? result.schema : 'mcp_loader.result';
  const status = typeof result.status === 'string' ? result.status : 'ok';
  if (schema === 'narada.mcp_loader.site_tool_inventory_check.v1') {
    return renderSiteToolInventoryResult(result, schema, status);
  }
  const connectionId = typeof result.connection_id === 'string' ? `\nconnection_id: ${result.connection_id}` : '';
  const surfaceId = typeof result.surface_id === 'string' ? `\nsurface_id: ${result.surface_id}` : '';
  return `${schema}: ${status}${connectionId}${surfaceId}`;
}

function renderSiteToolInventoryResult(result: JsonRecord, schema: string, status: string): string {
  const findings = Array.isArray(result.findings) ? result.findings.map((finding) => asRecord(finding)) : [];
  const lines = [
    `${schema}: ${status}`,
    `checked_surface_count: ${Number(result.checked_surface_count ?? 0)}`,
    `violation_count: ${Number(result.violation_count ?? 0)}`,
    `finding_status_counts: ${JSON.stringify(asRecord(result.finding_status_counts))}`,
  ];
  if (findings.length > 0) lines.push('findings:');
  for (const finding of findings.slice(0, 50)) {
    const surfaceId = typeof finding.surface_id === 'string' ? finding.surface_id : 'unknown-surface';
    const findingStatus = typeof finding.status === 'string' ? finding.status : 'unknown';
    lines.push(`- ${surfaceId} [${findingStatus}]`);
    for (const key of ['missing_from_fabric', 'extra_in_fabric', 'duplicate_declared_tools', 'duplicate_observed_tools', 'unclassified_observed_tools']) {
      const values = Array.isArray(finding[key]) ? finding[key].filter((value): value is string => typeof value === 'string') : [];
      if (values.length > 0) lines.push(`  ${key}: ${renderCompactStringList(values)}`);
    }
    const error = asRecord(finding.error);
    const errorCode = typeof error.code === 'string' ? error.code : null;
    const errorMessage = typeof error.message === 'string' ? error.message : null;
    if (errorCode || errorMessage) lines.push(`  error: ${[errorCode, errorMessage].filter(Boolean).join(' - ')}`);
  }
  if (findings.length > 50) lines.push(`findings_omitted: ${findings.length - 50}`);
  if (typeof result.observation_ref === 'string') lines.push(`observation_ref: ${result.observation_ref}`);
  return lines.join('\n');
}

function renderCompactStringList(values: string[]): string {
  const visible = values.slice(0, 20);
  const omitted = values.length - visible.length;
  return `${visible.join(', ')}${omitted > 0 ? ` (+${omitted} more)` : ''}`;
}

function tail(text: string, limit: number): string {
  return text.length <= limit ? text : text.slice(text.length - limit);
}

function tool(name: string, description: string, properties: JsonRecord, required: string[], semantics: { readOnly: boolean; destructive?: boolean }) {
  return {
    name,
    description,
    annotations: {
      title: name,
      readOnlyHint: semantics.readOnly,
      destructiveHint: semantics.destructive === true,
      idempotentHint: false,
      openWorldHint: true,
    },
    inputSchema: { type: 'object', properties, additionalProperties: false, required },
    outputSchema: { type: 'object', additionalProperties: true },
  };
}

function normalizePath(p: string): string {
  return resolve(p).replace(/\\/g, '/');
}

function normalizePolicyPrefix(prefix: string): string {
  const normalized = prefix.replace(/\\/g, '/').replace(/\/+$/g, '');
  if (normalized === '{site_root}' || normalized.startsWith('{site_root}/')) return normalized;
  return normalizePath(prefix);
}

function deriveSiteId(siteRoot: string): string {
  const parts = siteRoot.replace(/\\/g, '/').split('/').filter(Boolean);
  const last = parts[parts.length - 1] ?? 'site';
  return assertSupportedSiteId(last.replace(/^narada\./, '').replace(/^narada-/, ''), `site_root:${siteRoot}`);
}

function interpolateSiteArg(value: string, siteRoot: string): string {
  const normalizedRoot = siteRoot.replace(/\\/g, '/');
  const siteControlRoot = normalizedRoot.endsWith('/.narada') ? siteRoot : resolve(siteRoot, '.narada');
  return value
    .replace(/\{site_root\}/g, siteRoot)
    .replace(/\{site_control_root\}/g, siteControlRoot)
    .replace(/\{site_runtime_root\}/g, resolve(siteControlRoot, 'runtime'))
    .replace(/\{site_id\}/g, deriveSiteId(siteRoot));
}

function normalizeStringArray(value: unknown): string[] | null {
  if (!value) return null;
  if (Array.isArray(value)) return value.map((v) => String(v));
  if (typeof value === 'string') return value.split(',').map((s) => s.trim()).filter(Boolean);
  return null;
}

function stringArray(value: unknown): string[] | null {
  if (!value) return null;
  if (Array.isArray(value)) return value.map((v) => String(v));
  return null;
}

function optionalRuntimeKind(value: unknown): McpRuntimeKind | null {
  return optionalString(value);
}

function surfaceRuntimeRequirements(server: unknown): McpRuntimeKind[] {
  const record = asRecord(server);
  const projection = asRecord(record.surface_projection);
  const rawRequirements = Array.isArray(projection.runtime_requirements)
    ? projection.runtime_requirements
    : record.runtime_requirements;
  return (Array.isArray(rawRequirements) ? rawRequirements : [])
    .map((value) => String(value).trim())
    .filter(Boolean);
}

function runtimeRequirementsMatch(requirements: McpRuntimeKind[], runtimeKind: McpRuntimeKind | null): boolean {
  return requirements.length === 0 || (runtimeKind !== null && requirements.includes(runtimeKind));
}

function ensureSurfaceRuntimeAllowed(surfaceId: string, siteRoot: string, runtimeKind: McpRuntimeKind | null): McpRuntimeKind[] {
  const servers = asRecord(readSiteFabric(siteRoot).mcpServers);
  const requirements = surfaceRuntimeRequirements(findSiteServer(servers, surfaceId)?.server);
  if (runtimeRequirementsMatch(requirements, runtimeKind)) return requirements;
  if (runtimeKind === null) {
    throw diagnosticError(
      'surface_runtime_required',
      `surface_runtime_required:${surfaceId}`,
      { surface_id: surfaceId, runtime_requirements: requirements },
    );
  }
  throw diagnosticError(
    'surface_runtime_not_supported',
    `surface_runtime_not_supported:${surfaceId}:${runtimeKind}`,
    { surface_id: surfaceId, runtime_kind: runtimeKind, runtime_requirements: requirements },
  );
}

function optionalString(value: unknown): string | null {
  const text = String(value ?? '').trim();
  return text || null;
}

function requiredString(value: unknown, code: string): string {
  const text = String(value ?? '').trim();
  if (!text) throw diagnosticError(code, code);
  return text;
}

function integer(value: unknown, fallback: number, min: number, max: number): number {
  const parsed = Number(value ?? fallback);
  return Number.isFinite(parsed) ? clamp(Math.trunc(parsed), min, max) : fallback;
}

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Math.trunc(value)));
}

function asRecord(value: unknown): JsonRecord {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonRecord : {};
}

function diagnosticError(code: string, message: string = code, details: JsonRecord = {}) {
  const error = new Error(message);
  Object.assign(error, { codeName: code, details });
  return error;
}

function errorDiagnostic(error: unknown) {
  const record = asRecord(error);
  return { schema: 'narada.mcp_loader.error.v1', code: String(record.codeName ?? 'mcp_loader_error'), message: error instanceof Error ? error.message : String(error), details: asRecord(record.details) };
}

function drainJsonLines(buffer: string) {
  const lines = buffer.split(/\r?\n/);
  const remaining = lines.pop() ?? '';
  return { framed: false, remaining, requests: lines.filter((line) => line.trim()).map((line) => asRecord(JSON.parse(line))) };
}

function drainJsonRpcFrames(buffer: string) {
  const requests: JsonRecord[] = [];
  let remaining = buffer;
  while (true) {
    const crlfHeaderEnd = remaining.indexOf('\r\n\r\n');
    const lfHeaderEnd = remaining.indexOf('\n\n');
    const headerEnd = crlfHeaderEnd !== -1 ? crlfHeaderEnd : lfHeaderEnd;
    if (headerEnd < 0) break;
    const header = remaining.slice(0, headerEnd);
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) break;
    const length = Number(match[1]);
    const bodyStart = headerEnd + (crlfHeaderEnd !== -1 ? 4 : 2);
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

export function parseArgs(argv: string[]) {
  const options: JsonRecord = {};
  let i = 0;
  const collect = (key: string) => {
    if (!options[key]) options[key] = [] as string[];
    (options[key] as string[]).push(argv[++i]);
  };
  for (i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--allowed-site-root') collect('allowedSiteRoots');
    else if (arg === '--allowed-entrypoint-prefix') collect('allowedEntrypointPrefixes');
    else if (arg === '--allowed-surface-id') collect('allowedSurfaceIds');
    else if (arg === '--allowed-env-var') collect('allowedEnvVars');
    else if (arg === '--max-connections') options.maxConnections = argv[++i];
    else if (arg === '--max-request-bytes') options.maxRequestBytes = argv[++i];
    else if (arg === '--max-response-bytes') options.maxResponseBytes = argv[++i];
    else if (arg === '--attach-timeout-ms') options.attachTimeoutMs = argv[++i];
    else if (arg === '--tool-call-timeout-ms') options.toolCallTimeoutMs = argv[++i];
    else if (arg === '--tool-timeout-grace-ms') options.toolCallGraceMs = argv[++i];
    else if (arg === '--binding-admission-path') options.bindingAdmissionPath = argv[++i];
    else if (arg === '--binding-admission-digest') options.bindingAdmissionDigest = argv[++i];
    else if (arg === '--standalone-ambient-attachment') options.standaloneAmbientAttachment = true;
  }
  return options;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runStdioServer(parseArgs(process.argv.slice(2))).catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  });
}
