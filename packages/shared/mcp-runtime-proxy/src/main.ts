#!/usr/bin/env node
import { createHash, randomUUID } from 'node:crypto';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';
import { processSupervisorEntrypoint } from '@narada-core/process-launch-posture';
import { createRuntimeObservationSink, type RuntimeObservationSink } from '@narada-core/mcp-runtime-observation';
import {
  RUNTIME_STATUS_TOOL_NAME,
  captureRuntimeFreshness,
  classifyRuntimeInstance,
  evaluateRuntimeFreshness,
  listRuntimeInstances,
  processIsAlive,
  runtimeInstancePath,
  runtimeStatusToolDefinition,
  writeRuntimeInstance,
  type RuntimeInstanceRecord,
} from './runtime-lifecycle.js';
import {
  MCP_RUNTIME_CONTRACT_VERSION,
  preflightMaterializationGeneration,
  type MaterializationPreflight,
} from './materialization-contract.js';
import { preflightWorkspaceArtifacts, type WorkspaceArtifactPreflight } from './workspace-artifact-manifest.js';
import { describeUnknownError } from './error-description.js';
import { admitOrientationRequest } from './orientation-entry-admission.js';

type JsonRecord = Record<string, unknown>;
type RequestLifecycleEvent = {
  at: string;
  event: string;
  detail?: JsonRecord;
};
type StartupTrace = {
  path: string | null;
  startedAt: string;
  completed: boolean;
  runtimeContractVersion: number | null;
  artifactManifestFingerprint: string | null;
  materializationGenerationFingerprint: string | null;
  events: RequestLifecycleEvent[];
};
type PendingRequest = {
  id: string | number;
  method: string;
  framed: boolean;
  timeoutTimer: NodeJS.Timeout;
  requestedTransportTimeoutMs: number | null;
  effectiveTimeoutMs: number;
  toolName: string | null;
  argsHash: string | null;
  argsSummary: JsonRecord;
  startedAt: string;
  progressToken: string | number | null;
  lastProgress: JsonRecord | null;
  lifecycle: RequestLifecycleEvent[];
};
type ProxyOptions = {
  childCommand: string;
  entrypoint: string;
  childInvocationKind: 'entrypoint' | 'native_applet' | 'native_entrypoint';
  childApplet: string | null;
  childPrefixArgs: string[];
  childArgs: string[];
  carrierId: string | null;
  carrierKind: string | null;
  registrarEntrypoint: string | null;
  registrarCommand: string | null;
  artifactManifestPath: string | null;
  artifactManifestFingerprint: string | null;
  runtimeContractVersion: number | null;
  materializationSidecarPath: string | null;
  materializationGenerationFingerprint: string | null;
  surfaceId: string | null;
  requestTimeoutMs: number;
  toolTimeoutGraceMs: number;
  diagnosticsDir: string | null;
  livenessCheckMs: number;
  orphanGraceMs: number;
};
type ChildLaunch = {
  child: ChildProcessWithoutNullStreams;
  supervisorPath: string | null;
  supervisorIdentityPath: string | null;
};

const STDERR_TAIL_LIMIT = 8000;
const STDOUT_TAIL_LIMIT = 8000;
const DEFAULT_REQUEST_TIMEOUT_MS = 240_000;
const DEFAULT_REQUEST_TIMEOUT_KILL_GRACE_MS = 5_000;
const DEFAULT_TOOL_TIMEOUT_GRACE_MS = 15_000;
const MAX_TRANSPORT_TIMEOUT_MS = 900_000;
const MAX_TOOL_TIMEOUT_GRACE_MS = 60_000;
const DEFAULT_LIVENESS_CHECK_MS = 5_000;
const DEFAULT_ORPHAN_GRACE_MS = 15_000;
const MAX_LIVENESS_CHECK_MS = 60_000;
const MAX_ORPHAN_GRACE_MS = 120_000;
const SUPPRESSED_RESPONSE_TTL_MS = 60_000;
const FORENSIC_ARTIFACT_SCHEMA = 'narada.mcp_runtime_proxy.forensic_artifact.v1';
const STARTUP_TRACE_SCHEMA = 'narada.mcp_runtime_proxy.startup_trace.v1';

function parseArgs(argv: string[]): ProxyOptions {
  let childCommand = '';
  let entrypoint = '';
  let childInvocationKind: 'entrypoint' | 'native_applet' | 'native_entrypoint' = 'entrypoint';
  let childApplet: string | null = null;
  let childPrefixArgs: string[] = [];
  let carrierId: string | null = null;
  let carrierKind: string | null = null;
  let registrarEntrypoint: string | null = null;
  let registrarCommand: string | null = null;
  let artifactManifestPath: string | null = null;
  let runtimeContractVersion: number | null = null;
  let materializationSidecarPath: string | null = null;
  let surfaceId: string | null = null;
  let requestTimeoutMs = DEFAULT_REQUEST_TIMEOUT_MS;
  let toolTimeoutGraceMs = DEFAULT_TOOL_TIMEOUT_GRACE_MS;
  let diagnosticsDir = process.env.NARADA_MCP_RUNTIME_PROXY_DIAGNOSTICS_DIR ?? '';
  let livenessCheckMs = DEFAULT_LIVENESS_CHECK_MS;
  let orphanGraceMs = DEFAULT_ORPHAN_GRACE_MS;
  let passthroughIndex = argv.indexOf('--');
  if (passthroughIndex < 0) passthroughIndex = argv.length;
  const prelude = argv.slice(0, passthroughIndex);
  for (let index = 0; index < prelude.length; index += 1) {
    const arg = prelude[index];
    if (arg === '--child-command' && prelude[index + 1]) childCommand = prelude[++index];
    else if (arg === '--entrypoint' && prelude[index + 1]) entrypoint = prelude[++index];
    else if (arg === '--child-invocation-kind' && prelude[index + 1]) {
      const value = prelude[++index];
      if (value !== 'entrypoint' && value !== 'native_applet' && value !== 'native_entrypoint') throw new Error('mcp_runtime_proxy_invalid_child_invocation_kind');
      childInvocationKind = value;
    }
    else if (arg === '--child-applet' && prelude[index + 1]) childApplet = prelude[++index];
    else if (arg === '--child-prefix-args' && prelude[index + 1]) {
      const raw = prelude[++index];
      let parsed: unknown;
      try { parsed = JSON.parse(raw); } catch { throw new Error('mcp_runtime_proxy_invalid_child_prefix_args'); }
      if (!Array.isArray(parsed) || parsed.some((value) => typeof value !== 'string')) throw new Error('mcp_runtime_proxy_invalid_child_prefix_args');
      childPrefixArgs = parsed;
    }
    else if (arg === '--carrier-id' && prelude[index + 1]) carrierId = prelude[++index];
    else if (arg === '--carrier-kind' && prelude[index + 1]) carrierKind = prelude[++index];
    else if (arg === '--registrar-entrypoint' && prelude[index + 1]) registrarEntrypoint = prelude[++index];
    else if (arg === '--registrar-command' && prelude[index + 1]) registrarCommand = prelude[++index];
    else if (arg === '--artifact-manifest' && prelude[index + 1]) artifactManifestPath = prelude[++index];
    else if (arg === '--runtime-contract-version' && prelude[index + 1]) runtimeContractVersion = parsePositiveInteger(prelude[++index], 'runtime_contract_version');
    else if (arg === '--materialization-sidecar' && prelude[index + 1]) materializationSidecarPath = prelude[++index];
    else if (arg === '--surface-id' && prelude[index + 1]) surfaceId = prelude[++index];
    else if (arg === '--request-timeout-ms' && prelude[index + 1]) requestTimeoutMs = parsePositiveInteger(prelude[++index], 'request_timeout_ms');
    else if (arg === '--tool-timeout-grace-ms' && prelude[index + 1]) toolTimeoutGraceMs = parsePositiveInteger(prelude[++index], 'tool_timeout_grace_ms', MAX_TOOL_TIMEOUT_GRACE_MS);
    else if (arg === '--diagnostics-dir' && prelude[index + 1]) diagnosticsDir = prelude[++index];
    else if (arg === '--liveness-check-ms' && prelude[index + 1]) livenessCheckMs = parsePositiveInteger(prelude[++index], 'liveness_check_ms', MAX_LIVENESS_CHECK_MS);
    else if (arg === '--orphan-grace-ms' && prelude[index + 1]) orphanGraceMs = parsePositiveInteger(prelude[++index], 'orphan_grace_ms', MAX_ORPHAN_GRACE_MS);
  }
  if (!entrypoint) throw new Error('mcp_runtime_proxy_missing_entrypoint');
  if (childInvocationKind === 'native_applet' && !childApplet) throw new Error('mcp_runtime_proxy_missing_child_applet');
  if (runtimeContractVersion !== null && runtimeContractVersion >= 3 && !childCommand) {
    throw new Error('mcp_runtime_proxy_missing_child_command');
  }
  if (runtimeContractVersion !== null && runtimeContractVersion >= 3 && registrarEntrypoint && !registrarCommand) {
    throw new Error('mcp_runtime_proxy_missing_registrar_command');
  }
  return {
    childCommand: childCommand || process.execPath,
    entrypoint: resolve(entrypoint),
    childInvocationKind,
    childApplet,
    childPrefixArgs,
    childArgs: argv.slice(Math.min(passthroughIndex + 1, argv.length)),
    carrierId,
    carrierKind,
    registrarEntrypoint: registrarEntrypoint ? resolve(registrarEntrypoint) : null,
    registrarCommand: registrarCommand || (registrarEntrypoint ? process.execPath : null),
    artifactManifestPath: artifactManifestPath ? resolve(artifactManifestPath) : null,
    artifactManifestFingerprint: null,
    runtimeContractVersion,
    materializationSidecarPath: materializationSidecarPath ? resolve(materializationSidecarPath) : null,
    materializationGenerationFingerprint: null,
    surfaceId,
    requestTimeoutMs,
    toolTimeoutGraceMs,
    diagnosticsDir: diagnosticsDir ? resolve(diagnosticsDir) : defaultDiagnosticsDir(),
    livenessCheckMs,
    orphanGraceMs,
  };
}

function createProxyObservationSink(options: ProxyOptions): RuntimeObservationSink | null {
  const siteRoot = process.env.NARADA_SITE_ROOT?.trim();
  if (!siteRoot) return null;
  try {
    return createRuntimeObservationSink({
      site_root: siteRoot,
      source_id: `carrier-proxy-${options.surfaceId ?? process.pid}`,
    });
  } catch {
    return null;
  }
}

function emitProxyOwners(sink: RuntimeObservationSink | null, options: ProxyOptions, childPid: number | null): void {
  if (!sink) return;
  const siteId = process.env.NARADA_SITE_ID?.trim() || 'unknown-site';
  const authorityRef = process.env.NARADA_AUTHORITY_REF?.trim() || `site:${siteId}:mcp-surfaces`;
  const observedAt = new Date().toISOString();
  const proxyOwner = `carrier-proxy-${process.pid}`;
  void sink.emit({
    schema: 'narada.mcp_runtime.resource_owner.v1', owner_id: proxyOwner, site_id: siteId,
    authority_ref: authorityRef, owner_kind: 'carrier_proxy', pid: process.pid, process_started_at: null,
    parent_owner_id: null, surface_id: options.surfaceId, instance_id: null, generation_id: null,
    carrier_session_id: process.env.NARADA_CARRIER_SESSION_ID?.trim() || null,
    executable_name: process.execPath, observed_at: observedAt,
  });
  if (childPid) void sink.emit({
    schema: 'narada.mcp_runtime.resource_owner.v1', owner_id: `proxy-child-${childPid}`, site_id: siteId,
    authority_ref: authorityRef, owner_kind: 'nars_stdio_child', pid: childPid, process_started_at: null,
    parent_owner_id: proxyOwner, surface_id: options.surfaceId, instance_id: null,
    generation_id: null, carrier_session_id: process.env.NARADA_CARRIER_SESSION_ID?.trim() || null,
    executable_name: options.entrypoint, observed_at: observedAt,
  });
  emitProxyLifecycle(sink, options, 'process_started', 'ok', childPid);
}

function emitProxyLifecycle(
  sink: RuntimeObservationSink | null,
  options: ProxyOptions,
  eventType: 'process_started' | 'process_exited',
  status: 'ok' | 'failed',
  childPid: number | null,
): void {
  if (!sink) return;
  const siteId = process.env.NARADA_SITE_ID?.trim() || 'unknown-site';
  void sink.emit({
    schema: 'narada.mcp_runtime.lifecycle_event.v1', event_id: `event-${randomUUID()}`,
    occurred_at: new Date().toISOString(), site_id: siteId,
    authority_ref: process.env.NARADA_AUTHORITY_REF?.trim() || `site:${siteId}:mcp-surfaces`,
    owner_id: childPid ? `proxy-child-${childPid}` : `carrier-proxy-${process.pid}`,
    event_type: eventType, surface_id: options.surfaceId, instance_id: null, generation_id: null,
    request_id: null, status, inflight: null,
  });
}

function stringDetail(details: JsonRecord, key: string): string | null {
  return typeof details[key] === 'string' && details[key] ? details[key] as string : null;
}

function pairedConfigPath(sidecarPath: string | null): string | null {
  const suffix = '.narada-generation.json';
  if (!sidecarPath || !sidecarPath.endsWith(suffix)) return null;
  return sidecarPath.slice(0, -suffix.length);
}

function commandLineArg(value: string): string {
  return `"${value.replace(/"/g, '\\"')}"`;
}

function carrierRestartInstruction(carrierKind: string | null): string {
  switch (carrierKind?.toLowerCase()) {
    case 'codex': return 'Restart Codex or start a new Codex session after materialization.';
    case 'kimi': return 'Restart Kimi or start a new Kimi session after materialization.';
    case 'opencode': return 'Restart OpenCode or start a new OpenCode session after materialization.';
    default: return 'Restart the carrier or start a new carrier session after materialization.';
  }
}

function buildMaterializationRecovery(options: ProxyOptions, preflight: MaterializationPreflight): JsonRecord {
  const details = preflight.details ?? {};
  const carrierId = stringDetail(details, 'carrier_id') ?? options.carrierId;
  const carrierKind = stringDetail(details, 'carrier_kind') ?? options.carrierKind;
  const configPath = stringDetail(details, 'config_path') ?? pairedConfigPath(options.materializationSidecarPath);
  const registrarEntrypoint = stringDetail(details, 'registrar_entrypoint') ?? options.registrarEntrypoint;
  const groupKey = JSON.stringify({
    carrier_id: carrierId,
    carrier_kind: carrierKind,
    config_path: configPath,
    code: preflight.code ?? 'materialization_generation_stale',
    generation_fingerprint: preflight.generation_fingerprint,
    expected_manifest_fingerprint: stringDetail(details, 'expected_manifest_fingerprint'),
    actual_manifest_fingerprint: stringDetail(details, 'actual_manifest_fingerprint'),
  });
  const recoveryGroupId = `materialization-${createHash('sha256').update(groupKey, 'utf8').digest('hex').slice(0, 20)}`;
  const commandArgs = registrarEntrypoint && options.registrarCommand && options.materializationSidecarPath
    && resolve(registrarEntrypoint).toLowerCase() === resolve(options.registrarCommand).toLowerCase()
    ? ['recover-generation', '--generation', options.materializationSidecarPath]
    : null;
  const command = commandArgs && options.registrarCommand
    ? {
      executable: options.registrarCommand,
      args: commandArgs,
      display: [options.registrarCommand, ...commandArgs].map(commandLineArg).join(' '),
    }
    : null;
  return {
    schema: 'narada.mcp_runtime_proxy.materialization_recovery.v1',
    recovery_group_id: recoveryGroupId,
    deduplication: {
      scope: 'carrier_materialization',
      key: recoveryGroupId,
      guidance: 'Report one recovery action for this group; bootstrap surfaces sharing this id describe the same carrier failure.',
    },
    carrier: {
      carrier_id: carrierId,
      carrier_kind: carrierKind,
      config_path: configPath,
    },
    regeneration: {
      required: true,
      available: command !== null,
      owner: 'narada-mcp-materializer',
      command,
      unavailable_reason: command ? null : 'The materialization record does not identify the registrar entrypoint.',
    },
    restart_required: true,
    restart: {
      owner: carrierKind ?? 'carrier',
      automatic: false,
      instruction: carrierRestartInstruction(carrierKind),
    },
  };
}

function workspaceRootFromManifest(manifestPath: string | null): string | null {
  if (!manifestPath) return null;
  return resolve(manifestPath, '..', '..', '..');
}

function buildWorkspaceArtifactRecovery(options: ProxyOptions, preflight: WorkspaceArtifactPreflight): JsonRecord {
  const details = preflight.details ?? {};
  const carrierId = options.carrierId;
  const carrierKind = options.carrierKind;
  const configPath = pairedConfigPath(options.materializationSidecarPath);
  const registrarEntrypoint = options.registrarEntrypoint;
  const manifestPath = options.artifactManifestPath;
  const workspaceRoot = workspaceRootFromManifest(manifestPath);
  const groupKey = JSON.stringify({
    carrier_id: carrierId,
    carrier_kind: carrierKind,
    config_path: configPath,
    manifest_path: manifestPath,
    code: preflight.code ?? 'workspace_manifest_stale',
  });
  const recoveryGroupId = `workspace-materialization-${createHash('sha256').update(groupKey, 'utf8').digest('hex').slice(0, 20)}`;
  const materializeArgs = registrarEntrypoint && options.registrarCommand && options.materializationSidecarPath
    && resolve(registrarEntrypoint).toLowerCase() === resolve(options.registrarCommand).toLowerCase()
    ? ['recover-generation', '--generation', options.materializationSidecarPath]
    : null;
  const materializeCommand = materializeArgs && options.registrarCommand
    ? {
      executable: options.registrarCommand,
      args: materializeArgs,
      display: [options.registrarCommand, ...materializeArgs].map(commandLineArg).join(' '),
    }
    : null;
  const buildCommand = {
    executable: 'pnpm',
    args: ['build'],
    ...(workspaceRoot ? { cwd: workspaceRoot } : {}),
    display: 'pnpm build',
  };
  return {
    schema: 'narada.mcp_runtime_proxy.workspace_recovery.v1',
    recovery_group_id: recoveryGroupId,
    deduplication: {
      scope: 'carrier_materialization',
      key: recoveryGroupId,
      guidance: 'Report one build/materialization action for this group; bootstrap surfaces sharing this id describe the same carrier failure.',
    },
    cause: {
      code: preflight.code,
      reason: preflight.reason,
      details,
    },
    steps: [
      { order: 1, action: 'build_workspace', command: buildCommand },
      {
        order: 2,
        action: 'materialize_all_carriers',
        required: true,
        owner: 'narada-mcp-materializer',
        available: materializeCommand !== null,
        command: materializeCommand,
        unavailable_reason: materializeCommand ? null : 'The carrier launch does not identify the registrar entrypoint.',
      },
      {
        order: 3,
        action: 'restart_carrier',
        required: true,
        automatic: false,
        instruction: carrierRestartInstruction(carrierKind),
      },
    ],
    restart_required: true,
  };
}

// The watchdog guards against a hung child. Callers that own a surface timeout
// must carry it in the transport-level _meta field below; arbitrary tool
// arguments remain domain data and are never interpreted here.
export function effectiveRequestTimeoutMs(proxyTimeoutMs: number, requestedTransportTimeoutMs: number | null, toolTimeoutGraceMs: number): number {
  if (requestedTransportTimeoutMs === null) return proxyTimeoutMs;
  const boundedRequestedTimeoutMs = Math.min(MAX_TRANSPORT_TIMEOUT_MS, requestedTransportTimeoutMs);
  // The 15-minute bound applies to the admitted transport timeout. Grace is
  // additive, so a timeout at the bound still receives the configured grace.
  return Math.max(proxyTimeoutMs, boundedRequestedTimeoutMs + toolTimeoutGraceMs);
}

function createStartupTrace(
  options: ProxyOptions,
  child: ReturnType<typeof spawn>,
  childIdentity: JsonRecord,
): StartupTrace {
  const trace: StartupTrace = {
    path: options.diagnosticsDir
      ? join(options.diagnosticsDir, `startup-${safeSegment(options.surfaceId ?? basename(options.entrypoint))}.json`)
      : null,
    startedAt: new Date().toISOString(),
    completed: false,
    runtimeContractVersion: null,
    artifactManifestFingerprint: null,
    materializationGenerationFingerprint: null,
    events: [],
  };
  recordStartupTrace(trace, options, child, childIdentity, 'proxy_started', {
    proxy_pid: process.pid,
    child_pid: child.pid ?? null,
  });
  return trace;
}

function recordStartupTrace(
  trace: StartupTrace,
  options: ProxyOptions,
  child: ReturnType<typeof spawn>,
  childIdentity: JsonRecord,
  event: string,
  detail?: JsonRecord,
  completed = false,
): void {
  trace.events.push({ at: new Date().toISOString(), event, ...(detail ? { detail } : {}) });
  if (completed) trace.completed = true;
  if (!trace.path) return;
  try {
    mkdirSync(dirname(trace.path), { recursive: true });
    writeFileSync(trace.path, JSON.stringify({
      schema: STARTUP_TRACE_SCHEMA,
      surface_id: options.surfaceId,
      entrypoint: options.entrypoint,
      child_invocation_kind: options.childInvocationKind,
      child_applet: options.childApplet,
      child_prefix_args: options.childPrefixArgs,
      child_args: options.childArgs,
      started_at: trace.startedAt,
      updated_at: new Date().toISOString(),
      completed: trace.completed,
      runtime_contract_version: trace.runtimeContractVersion,
      artifact_manifest_fingerprint: trace.artifactManifestFingerprint,
      materialization_generation_fingerprint: trace.materializationGenerationFingerprint,
      proxy_pid: process.pid,
      child_pid: child.pid ?? null,
      child_identity: childIdentity,
      events: trace.events,
    }, null, 2) + '\n', 'utf8');
  } catch {
    // Startup tracing must never prevent the proxy from serving MCP traffic.
  }
}

function writeStartupPhaseTrace(options: ProxyOptions, details: JsonRecord): void {
  if (!options.diagnosticsDir) return;
  try {
    mkdirSync(options.diagnosticsDir, { recursive: true });
    writeFileSync(join(options.diagnosticsDir, `startup-phases-${safeSegment(options.surfaceId ?? basename(options.entrypoint))}.json`), JSON.stringify({
      schema: 'narada.mcp_runtime_proxy.startup_phases.v1',
      surface_id: options.surfaceId,
      observed_at: new Date().toISOString(),
      ...details,
    }, null, 2) + '\n', 'utf8');
  } catch {
    // Startup diagnostics must never prevent the proxy from serving MCP traffic.
  }
}

function startsWithJsonRpcFrame(buffer: string): boolean {
  return /^\s*Content-Length:\s*\d+\r?\n/i.test(buffer);
}

export async function runProxy(argv = process.argv.slice(2)): Promise<void> {
  if (argv.includes('--list-runtime-instances')) {
    const diagnosticsIndex = argv.indexOf('--diagnostics-dir');
    const diagnosticsDir = diagnosticsIndex >= 0 && argv[diagnosticsIndex + 1]
      ? resolve(argv[diagnosticsIndex + 1])
      : defaultDiagnosticsDir();
    process.stdout.write(`${JSON.stringify(listRuntimeInstances(diagnosticsDir), null, 2)}\n`);
    return;
  }
  const options = parseArgs(argv);
  const preflightStartedAt = performance.now();
  const artifactPreflight = preflightWorkspaceArtifacts({
    surfaceId: options.surfaceId,
    entrypoint: options.entrypoint,
    artifactManifestPath: options.artifactManifestPath,
  });
  if (!artifactPreflight.ok) {
    await writePreflightRefusal({
      ...artifactPreflight,
      details: {
        ...(artifactPreflight.details ?? {}),
        remediation: 'Run pnpm build, then materialize the carrier with the supplied registrar recovery command, and restart the carrier session.',
        recovery: buildWorkspaceArtifactRecovery(options, artifactPreflight),
      },
    });
    process.exitCode = 1;
    return;
  }
  options.artifactManifestFingerprint = artifactPreflight.manifest_fingerprint;
  if (options.runtimeContractVersion === null) {
    await writePreflightRefusal({
      schema: 'narada.workspace_artifact_preflight.v1',
      status: 'refused',
      ok: false,
      surface_id: options.surfaceId,
      entrypoint: options.entrypoint,
      artifact_manifest_path: options.artifactManifestPath,
      manifest_fingerprint: artifactPreflight.manifest_fingerprint,
      code: 'runtime_contract_version_missing',
      reason: 'The launch did not declare the MCP runtime contract version.',
      details: {
        expected_runtime_contract_version: MCP_RUNTIME_CONTRACT_VERSION,
        remediation: 'Regenerate the carrier configuration with the current registrar before launching this surface.',
      },
    });
    process.exitCode = 1;
    return;
  }
  if (options.runtimeContractVersion !== MCP_RUNTIME_CONTRACT_VERSION) {
    await writePreflightRefusal({
      schema: 'narada.workspace_artifact_preflight.v1',
      status: 'refused',
      ok: false,
      surface_id: options.surfaceId,
      entrypoint: options.entrypoint,
      artifact_manifest_path: options.artifactManifestPath,
      manifest_fingerprint: artifactPreflight.manifest_fingerprint,
      code: 'runtime_contract_version_mismatch',
      reason: 'The launch declares an obsolete MCP runtime contract version.',
      details: {
        actual_runtime_contract_version: options.runtimeContractVersion,
        expected_runtime_contract_version: MCP_RUNTIME_CONTRACT_VERSION,
        remediation: 'Regenerate the carrier configuration with the current registrar before launching this surface.',
      },
    });
    process.exitCode = 1;
    return;
  }
  const materializationPreflight: MaterializationPreflight = options.materializationSidecarPath
    ? preflightMaterializationGeneration({
      sidecarPath: options.materializationSidecarPath,
      manifestPath: options.artifactManifestPath!,
      manifestFingerprint: artifactPreflight.manifest_fingerprint,
      materializationContractEntrypoint: options.registrarEntrypoint,
      runtimeProxyEntrypoint: fileURLToPath(import.meta.url),
    })
    : { ok: true, generation_fingerprint: null };
  for (const observation of materializationPreflight.observations ?? []) {
    process.stderr.write(JSON.stringify({
      schema: 'narada.mcp_runtime_proxy.observation.v1',
      code: observation.code,
      ...observation.detail,
    }) + '\n');
  }
  if (!materializationPreflight.ok) {
    const recovery = buildMaterializationRecovery(options, materializationPreflight);
    await writePreflightRefusal({
      schema: 'narada.workspace_artifact_preflight.v1',
      status: 'refused',
      ok: false,
      surface_id: options.surfaceId,
      entrypoint: options.entrypoint,
      artifact_manifest_path: options.artifactManifestPath,
      manifest_fingerprint: artifactPreflight.manifest_fingerprint,
      code: materializationPreflight.code,
      reason: materializationPreflight.reason,
      details: {
        ...(materializationPreflight.details ?? {}),
        materialization_sidecar_path: options.materializationSidecarPath,
        materialization_generation_fingerprint: materializationPreflight.generation_fingerprint,
        remediation: 'Regenerate the carrier configuration with the current registrar; the proxy will not rebuild or retry it.',
        recovery,
      },
    });
    process.exitCode = 1;
    return;
  }
  options.materializationGenerationFingerprint = materializationPreflight.generation_fingerprint;
  const supervisorPath = processSupervisorEntrypoint();
  if (process.platform === 'win32' && (!supervisorPath || !existsSync(supervisorPath))) {
    await writePreflightRefusal({
      schema: 'narada.workspace_artifact_preflight.v1',
      status: 'refused',
      ok: false,
      surface_id: options.surfaceId,
      entrypoint: options.entrypoint,
      artifact_manifest_path: options.artifactManifestPath,
      manifest_fingerprint: artifactPreflight.manifest_fingerprint,
      code: 'workspace_artifact_missing',
      reason: 'The Windows process supervisor executable is missing.',
      details: { supervisor_path: supervisorPath },
    });
    process.exitCode = 1;
    return;
  }
  if (!existsSync(options.entrypoint)) {
    process.stderr.write(`mcp_runtime_proxy_entrypoint_not_found:${options.entrypoint}\n`);
  }
  writeStartupPhaseTrace(options, {
    preflight_ms: performance.now() - preflightStartedAt,
    completed_at: new Date().toISOString(),
  });

  const pending = new Map<string | number, PendingRequest>();
  const timedOutRequests = new Map<string | number, NodeJS.Timeout>();
  const childTerminationTimers = new Set<NodeJS.Timeout>();
  let parentBuffer = '';
  let childBuffer = '';
  let stderrTail = '';
  let stdoutTail = '';
  let childClosed = false;
  let childCloseDiagnostic: ProxyDiagnostic | null = null;
  let parentRequestObserved = false;
  let parentFramed = false;
  const childLaunch = spawnProxyChild(options, supervisorPath);
  const child = childLaunch.child;
  const observation = createProxyObservationSink(options);
  emitProxyOwners(observation, options, child.pid ?? null);
  const childIdentity = buildChildIdentity(options, childLaunch);
  const startupTrace = createStartupTrace(options, child, childIdentity);
  startupTrace.runtimeContractVersion = options.runtimeContractVersion;
  startupTrace.artifactManifestFingerprint = artifactPreflight.manifest_fingerprint;
  startupTrace.materializationGenerationFingerprint = materializationPreflight.generation_fingerprint;
  recordStartupTrace(startupTrace, options, child, childIdentity, 'preflight_ok', {
    runtime_contract_version: options.runtimeContractVersion,
    artifact_manifest_fingerprint: artifactPreflight.manifest_fingerprint,
    materialization_generation_fingerprint: materializationPreflight.generation_fingerprint,
  });
  const parentPid = process.ppid;
  const freshnessTracker = captureRuntimeFreshness({
    proxyRuntimePath: fileURLToPath(import.meta.url),
    childEntrypoint: options.entrypoint,
    artifactManifestPath: options.artifactManifestPath,
  });
  const instancePath = runtimeInstancePath(options.diagnosticsDir ?? defaultDiagnosticsDir());
  let reclamationReason: string | null = null;
  let orphanTerminationTimer: NodeJS.Timeout | null = null;
  let orphanForceKillTimer: NodeJS.Timeout | null = null;
  const writeInstance = (
    state: RuntimeInstanceRecord['state'],
    evidence: JsonRecord,
    closedAt: string | null = null,
  ) => {
    const now = new Date();
    const runtimeFreshness = evaluateRuntimeFreshness({
      tracker: freshnessTracker,
      surfaceId: options.surfaceId,
      proxyPid: process.pid,
      childPid: child.pid ?? null,
    });
    const supervisorIdentity = childLaunch.supervisorIdentityPath
      ? readSupervisorIdentity(childLaunch.supervisorIdentityPath)
      : null;
    const managedChildPid = typeof supervisorIdentity?.managed_child_pid === 'number'
      ? supervisorIdentity.managed_child_pid
      : null;
    const record: RuntimeInstanceRecord = {
      schema: 'narada.mcp_runtime_proxy.instance.v2',
      surface_id: options.surfaceId,
      proxy_pid: process.pid,
      parent_pid: parentPid,
      child_pid: child.pid ?? null,
      supervisor_pid: childLaunch.supervisorPath ? child.pid ?? null : null,
      managed_child_pid: managedChildPid,
      server_pid: managedChildPid,
      entrypoint: options.entrypoint,
      started_at: freshnessTracker.started_at,
      heartbeat_at: now.toISOString(),
      lease_expires_at: new Date(now.getTime() + options.livenessCheckMs * 3).toISOString(),
      state,
      liveness_evidence: evidence,
      runtime_freshness: runtimeFreshness,
      artifact_manifest_path: options.artifactManifestPath,
      artifact_manifest_fingerprint: artifactPreflight.manifest_fingerprint,
      generation_id: `${options.surfaceId ?? 'surface'}:${freshnessTracker.started_at}`,
      supervisor_identity_path: childLaunch.supervisorIdentityPath,
      closed_at: closedAt,
    };
    writeRuntimeInstance(instancePath, record);
    return record;
  };
  let runtimeInstance = writeInstance('live', {
    parent_pid_alive: processIsAlive(parentPid),
    carrier_stdin_open: true,
  });
  const scheduleOrphanReclamation = (reason: string) => {
    if (childClosed || reclamationReason) return;
    reclamationReason = reason;
    runtimeInstance = writeInstance('stale', {
      reason,
      parent_pid_alive: processIsAlive(parentPid),
      carrier_stdin_open: reason !== 'carrier_stdin_closed',
      grace_ms: options.orphanGraceMs,
    });
    if (!child.stdin.destroyed) child.stdin.end();
    orphanTerminationTimer = setTimeout(() => {
      if (childClosed) return;
      runtimeInstance = writeInstance('reclaiming', {
        reason,
        termination_mode: childLaunch.supervisorPath ? 'owned_supervisor_tree' : 'SIGTERM',
        grace_ms: options.orphanGraceMs,
      });
      terminateProxyChild(child, false);
      orphanForceKillTimer = setTimeout(() => {
        if (!childClosed) terminateProxyChild(child, true);
      }, Math.min(options.orphanGraceMs, 5_000));
      orphanForceKillTimer.unref();
    }, options.orphanGraceMs);
    orphanTerminationTimer.unref();
  };
  const livenessTimer = setInterval(() => {
    const parentAlive = processIsAlive(parentPid);
    if (!parentAlive) {
      scheduleOrphanReclamation('parent_carrier_pid_not_alive');
      return;
    }
    if (!reclamationReason && !childClosed) {
      runtimeInstance = writeInstance('live', {
        parent_pid_alive: true,
        carrier_stdin_open: !process.stdin.readableEnded,
      });
    }
  }, options.livenessCheckMs);
  livenessTimer.unref();

  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');

  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk) => {
    parentBuffer += chunk;
    const drained = startsWithJsonRpcFrame(parentBuffer) ? drainJsonRpcFrames(parentBuffer) : drainJsonLines(parentBuffer);
    parentBuffer = drained.remaining;
    if (drained.requests.length > 0) parentFramed = drained.framed;
    if (drained.requests.length > 0) parentRequestObserved = true;
    for (const request of drained.requests) {
      const params = isJsonRecord(request.params) ? request.params : {};
      const id = request.id;
      const messageKind = Object.prototype.hasOwnProperty.call(request, 'id')
        ? 'request'
        : 'notification';
      if (
        request.method === 'tools/call'
        && params.name === RUNTIME_STATUS_TOOL_NAME
        && (typeof id === 'string' || typeof id === 'number')
      ) {
        const supervisorIdentity = childLaunch.supervisorIdentityPath
          ? readSupervisorIdentity(childLaunch.supervisorIdentityPath)
          : null;
        const serverPid = typeof supervisorIdentity?.managed_child_pid === 'number'
          ? supervisorIdentity.managed_child_pid
          : (childLaunch.supervisorPath ? null : child.pid ?? null);
        const childPidRole = childLaunch.supervisorPath ? 'supervisor' : 'server';
        const runtimeFreshness = evaluateRuntimeFreshness({
          tracker: freshnessTracker,
          surfaceId: options.surfaceId,
          proxyPid: process.pid,
          childPid: child.pid ?? null,
        });
        const liveness = classifyRuntimeInstance({
          ...runtimeInstance,
          managed_child_pid: serverPid,
          server_pid: serverPid,
        });
        const payload = {
          schema: 'narada.mcp_runtime_proxy.status.v1',
          status: 'ok',
          surface_id: options.surfaceId,
          liveness,
          runtime_freshness: runtimeFreshness,
        };
        writeJsonRpcMessage({
          jsonrpc: '2.0',
          id,
          result: {
            content: [{ type: 'text', text: `mcp_runtime_proxy_status: ${runtimeFreshness.status}\nproxy_pid: ${process.pid}\nchild_pid: ${child.pid ?? 'unknown'}\nchild_pid_role: ${childPidRole}\nserver_pid: ${serverPid ?? 'unknown'}\nrestart_owner: carrier_or_runtime_supervisor` }],
            structuredContent: payload,
          },
        }, drained.framed);
        continue;
      }
      const admission = admitOrientationRequest({
        surfaceId: options.surfaceId,
        messageKind,
        method: typeof request.method === 'string' ? request.method : null,
        params,
      });
      if (!admission.admitted) {
        if (typeof id === 'string' || typeof id === 'number') {
          writeJsonRpcMessage({
            jsonrpc: '2.0',
            id,
            error: {
              code: -32000,
              message: `orientation_required:${admission.state.reason}`,
              data: admission.state,
            },
          }, drained.framed);
        }
        recordStartupTrace(startupTrace, options, child, childIdentity, 'request_refused', {
          method: request.method,
          request_id: request.id ?? null,
          reason: admission.state.reason,
          ordinary_work_gate: admission.state.ordinary_work_gate,
          delivery_receipt_ref: admission.state.delivery_receipt_ref,
        });
        continue;
      }
      if ((typeof id === 'string' || typeof id === 'number') && typeof request.method === 'string') {
        const requestedTransportTimeoutMs = extractRequestedTransportTimeoutMs(request);
        const effectiveTimeoutMs = effectiveRequestTimeoutMs(options.requestTimeoutMs, requestedTransportTimeoutMs, options.toolTimeoutGraceMs);
        const timeoutTimer = setTimeout(() => {
          const pendingRequest = pending.get(id);
          if (!pendingRequest) return;
          pending.delete(id);
          rememberTimedOutRequest(timedOutRequests, id);
          recordLifecycle(pendingRequest, 'proxy_timeout', {
            proxy_request_timeout_ms: options.requestTimeoutMs,
            effective_request_timeout_ms: pendingRequest.effectiveTimeoutMs,
            requested_transport_timeout_ms: pendingRequest.requestedTransportTimeoutMs,
          });
          recordLifecycle(pendingRequest, 'child_termination_requested', {
            termination_mode: childLaunch.supervisorPath ? 'owned_supervisor_tree' : 'SIGTERM',
          });
          const artifactPath = writeForensicArtifact({
            event: 'proxy_child_request_timeout',
            request: pendingRequest,
            pending,
            options,
            child,
            childIdentity,
            stderrTail,
            stdoutTail,
            childBuffer,
            diagnostic: {
              code: 'child_request_timeout',
              message: `child_request_timeout:${request.method}:${pendingRequest.effectiveTimeoutMs}ms`,
              exitCode: null,
              signal: null,
            },
          });
          writePendingError(pendingRequest, options, {
            code: 'child_request_timeout',
            message: `child_request_timeout:${request.method}:${pendingRequest.effectiveTimeoutMs}ms`,
            stderrTail,
            stdoutTail,
            exitCode: null,
            signal: null,
            forensicArtifactPath: artifactPath,
          });
          sendCancellationToChild(child, pendingRequest, 'request timed out in mcp runtime proxy');
          recordLifecycle(pendingRequest, 'cancellation_sent');
          terminateChildAfterRequestTimeout(child, childTerminationTimers, () => childClosed);
        }, effectiveTimeoutMs);
        const requestMetadata = requestMetadataFor(request);
        pending.set(id, {
          id,
          method: request.method,
          framed: drained.framed,
          timeoutTimer,
          requestedTransportTimeoutMs,
          effectiveTimeoutMs,
          ...requestMetadata,
          startedAt: new Date().toISOString(),
          lastProgress: null,
          lifecycle: [{ at: new Date().toISOString(), event: 'request_forwarded' }],
        });
      }
      if (childClosed || child.stdin.destroyed) {
        if (pending.size > 0) {
          flushPendingErrors(pending, options, childCloseDiagnostic ?? {
            code: 'child_exited_before_response',
            message: `child_exited_before_response:${child.exitCode ?? child.signalCode ?? 'unknown'}`,
            stderrTail,
            stdoutTail,
            exitCode: child.exitCode,
            signal: child.signalCode,
          }, child, childIdentity, childBuffer);
        }
        continue;
      }
      writeJsonRpcMessageToStream(child.stdin, request, false);
      if (request.method === 'initialize' || request.method === 'tools/list') {
        recordStartupTrace(startupTrace, options, child, childIdentity, 'request_forwarded', {
          method: request.method,
          request_id: request.id ?? null,
        });
      }
    }
  });

  process.stdin.on('end', () => {
    scheduleOrphanReclamation('carrier_stdin_closed');
  });

  child.stdout.on('data', (chunk) => {
    stdoutTail = tail(`${stdoutTail}${chunk}`, STDOUT_TAIL_LIMIT);
    childBuffer += chunk;
    const drained = startsWithJsonRpcFrame(childBuffer) ? drainJsonRpcFrames(childBuffer) : drainJsonLines(childBuffer);
    childBuffer = drained.remaining;
    for (const response of drained.requests) {
      observeChildMessage(response, pending);
      const id = response.id;
      let responseFramed = parentFramed;
      if (typeof id === 'string' || typeof id === 'number') {
        const request = pending.get(id);
        if (request) {
          responseFramed = request.framed;
          recordLifecycle(request, 'child_response');
          if (request.method === 'initialize' || request.method === 'tools/list') {
            recordStartupTrace(startupTrace, options, child, childIdentity, 'child_response', {
              method: request.method,
              request_id: request.id,
            }, request.method === 'tools/list');
          }
          clearTimeout(request.timeoutTimer);
          if (request.method === 'tools/list' && isJsonRecord(response.result) && Array.isArray(response.result.tools)) {
            if (!response.result.tools.some((tool) => isJsonRecord(tool) && tool.name === RUNTIME_STATUS_TOOL_NAME)) {
              response.result.tools.push(runtimeStatusToolDefinition());
            }
          }
        }
        pending.delete(id);
        if (timedOutRequests.has(id)) continue;
      }
      writeJsonRpcMessage(response, responseFramed);
    }
  });

  child.stderr.on('data', (chunk) => {
    stderrTail = tail(`${stderrTail}${chunk}`, STDERR_TAIL_LIMIT);
    process.stderr.write(chunk);
  });

  child.on('error', (error) => {
    recordStartupTrace(startupTrace, options, child, childIdentity, 'child_error', { message: error.message });
    stderrTail = tail(`${stderrTail}${error.message}\n`, STDERR_TAIL_LIMIT);
    flushPendingErrors(pending, options, {
      code: 'child_spawn_error',
      message: error.message,
      stderrTail,
      stdoutTail,
      exitCode: null,
      signal: null,
    }, child, childIdentity, childBuffer);
  });

  child.on('close', (code, signal) => {
    if (!startupTrace.completed) {
      recordStartupTrace(startupTrace, options, child, childIdentity, 'child_closed_before_tools_list', {
        exit_code: code,
        signal,
      });
    }
    childClosed = true;
    emitProxyLifecycle(observation, options, 'process_exited', code === 0 ? 'ok' : 'failed', child.pid ?? null);
    clearInterval(livenessTimer);
    if (orphanTerminationTimer) clearTimeout(orphanTerminationTimer);
    if (orphanForceKillTimer) clearTimeout(orphanForceKillTimer);
    runtimeInstance = writeInstance(reclamationReason ? 'reclaimed' : 'closed', {
      reason: reclamationReason ?? 'child_closed',
      exit_code: code,
      signal,
      parent_pid_alive: processIsAlive(parentPid),
    }, new Date().toISOString());
    childCloseDiagnostic = {
      code: 'child_exited_before_response',
      message: `child_exited_before_response:${code ?? signal ?? 'unknown'}`,
      stderrTail,
      stdoutTail,
      exitCode: code,
      signal,
    };
    if (pending.size > 0) {
      flushPendingErrors(pending, options, childCloseDiagnostic, child, childIdentity, childBuffer);
    }
    clearTimedOutRequests(timedOutRequests);
    clearTimers(childTerminationTimers);
    process.exitCode = typeof code === 'number' ? code : 1;
  });

  await new Promise<void>((resolveDone) => {
    if (childClosed) {
      resolveDone();
      return;
    }
    child.on('close', () => resolveDone());
    process.stdin.on('end', () => {
      if (childClosed) resolveDone();
    });
  });
  if (childClosed && !parentRequestObserved && !process.stdin.readableEnded) {
    const inputDrainDeadline = Date.now() + 1_000;
    while (!parentRequestObserved && !process.stdin.readableEnded && Date.now() < inputDrainDeadline) {
      await new Promise<void>((resolveDelay) => setTimeout(resolveDelay, 5));
    }
  }
  process.stdin.pause();
  // A terminal child failure can be observed in the same tick as the proxy's
  // exit. Close stdout only after the diagnostic write has drained so callers
  // never lose the structured child_exited_before_response error.
  await flushProxyStdout();
}

async function writePreflightRefusal(preflight: WorkspaceArtifactPreflight): Promise<void> {
  process.stderr.write(`mcp_runtime_proxy_preflight_refused:${preflight.code ?? 'workspace_manifest_stale'}:${preflight.reason ?? 'workspace artifact preflight failed'}\n`);
  await new Promise<void>((resolveDone) => {
    let buffer = '';
    let finished = false;
    const finish = () => {
      if (finished) return;
      finished = true;
      clearTimeout(timeout);
      process.stdin.off('data', onData);
      process.stdin.off('end', finish);
      process.stdin.pause();
      resolveDone();
    };
    const respond = (request: JsonRecord, framed: boolean) => {
      if (typeof request.id !== 'string' && typeof request.id !== 'number') return;
      writeJsonRpcMessage({
        jsonrpc: '2.0',
        id: request.id,
        error: {
          code: -32000,
          message: `mcp_runtime_proxy_preflight_refused:${preflight.code}`,
          data: {
            schema: 'narada.mcp_runtime_proxy.error.v1',
            code: preflight.code,
            method: request.method ?? null,
            surface_id: preflight.surface_id,
            entrypoint: preflight.entrypoint,
            artifact_manifest_path: preflight.artifact_manifest_path,
            details: preflight.details ?? {},
          },
        },
      }, framed);
      finish();
    };
    const onData = (chunk: string) => {
      buffer += chunk;
      const drained = startsWithJsonRpcFrame(buffer) ? drainJsonRpcFrames(buffer) : drainJsonLines(buffer);
      buffer = drained.remaining;
      for (const request of drained.requests) respond(request, drained.framed);
    };
    const timeout = setTimeout(finish, 5_000);
    process.stdin.setEncoding('utf8');
    process.stdin.on('data', onData);
    process.stdin.on('end', finish);
    process.stdin.resume();
  });
  await flushProxyStdout();
}

function flushPendingErrors(
  pending: Map<string | number, PendingRequest>,
  options: ProxyOptions,
  diagnostic: ProxyDiagnostic,
  child: ReturnType<typeof spawn>,
  childIdentity: JsonRecord,
  childBuffer: string,
): void {
  for (const request of pending.values()) {
    const forensicArtifactPath = writeForensicArtifact({
      event: diagnostic.code,
      request,
      pending,
      options,
      child,
      childIdentity,
      stderrTail: diagnostic.stderrTail,
      stdoutTail: diagnostic.stdoutTail,
      childBuffer,
      diagnostic,
    });
    writePendingError(request, options, { ...diagnostic, forensicArtifactPath });
  }
  pending.clear();
}

type ProxyDiagnostic = {
  code: string;
  message: string;
  stderrTail: string;
  stdoutTail: string;
  exitCode: number | null;
  signal: NodeJS.Signals | null;
  forensicArtifactPath?: string | null;
};

function writePendingError(
  request: PendingRequest,
  options: { entrypoint: string; surfaceId: string | null; requestTimeoutMs?: number; toolTimeoutGraceMs?: number },
  diagnostic: ProxyDiagnostic,
): void {
  clearTimeout(request.timeoutTimer);
  const proxyRequestTimeoutMs = typeof options.requestTimeoutMs === 'number' ? options.requestTimeoutMs : null;
  const toolTimeoutGraceMs = typeof options.toolTimeoutGraceMs === 'number' ? options.toolTimeoutGraceMs : DEFAULT_TOOL_TIMEOUT_GRACE_MS;
  const proxyWatchdogData = diagnostic.code === 'child_request_timeout'
    ? {
      timeout_layer: 'mcp_runtime_proxy_watchdog',
      proxy_request_timeout_ms: proxyRequestTimeoutMs,
      effective_request_timeout_ms: request.effectiveTimeoutMs,
      requested_transport_timeout_ms: request.requestedTransportTimeoutMs,
      tool_timeout_grace_ms: toolTimeoutGraceMs,
      surface_timeout_expected_before_proxy:
        request.requestedTransportTimeoutMs !== null &&
        request.requestedTransportTimeoutMs + toolTimeoutGraceMs <= request.effectiveTimeoutMs,
      kill_grace_ms: DEFAULT_REQUEST_TIMEOUT_KILL_GRACE_MS,
    }
    : {};
  writeJsonRpcMessage({
    jsonrpc: '2.0',
    id: request.id,
    error: {
      code: -32000,
      message: diagnostic.message,
      data: {
        schema: 'narada.mcp_runtime_proxy.error.v1',
        code: diagnostic.code,
        method: request.method,
        surface_id: options.surfaceId,
        entrypoint: options.entrypoint,
        exit_code: diagnostic.exitCode,
        signal: diagnostic.signal,
        stderr_tail: diagnostic.stderrTail,
        stdout_tail: diagnostic.stdoutTail,
        forensic_artifact_path: diagnostic.forensicArtifactPath ?? null,
        ...proxyWatchdogData,
      },
    },
  }, false);
}

function extractRequestedTransportTimeoutMs(request: JsonRecord): number | null {
  const params = request.params;
  if (!isJsonRecord(params)) return null;
  const meta = params._meta;
  if (!isJsonRecord(meta)) return null;
  return normalizedPositiveInteger(meta.narada_request_timeout_ms);
}

function requestMetadataFor(request: JsonRecord): Pick<PendingRequest, 'toolName' | 'argsHash' | 'argsSummary' | 'progressToken'> {
  const params = isJsonRecord(request.params) ? request.params : {};
  const toolName = typeof params.name === 'string' ? params.name : null;
  const toolArguments = isJsonRecord(params.arguments) ? params.arguments : {};
  const meta = isJsonRecord(params._meta) ? params._meta : {};
  const progressToken = typeof meta.progressToken === 'string' || typeof meta.progressToken === 'number' ? meta.progressToken : null;
  return {
    toolName,
    argsHash: Object.keys(toolArguments).length > 0 ? sha256Json(toolArguments) : null,
    argsSummary: summarizeJson(toolArguments),
    progressToken,
  };
}

function observeChildMessage(message: JsonRecord, pending: Map<string | number, PendingRequest>): void {
  if (message.method === 'notifications/progress') {
    const params = isJsonRecord(message.params) ? message.params : {};
    const progressToken = params.progressToken;
    for (const request of pending.values()) {
      if (request.progressToken !== null && request.progressToken === progressToken) {
        request.lastProgress = summarizeJson(params);
        recordLifecycle(request, 'child_progress', request.lastProgress);
      }
    }
  }
}

function recordLifecycle(request: PendingRequest, event: string, detail?: JsonRecord): void {
  request.lifecycle.push({ at: new Date().toISOString(), event, ...(detail ? { detail } : {}) });
}

function writeForensicArtifact(input: {
  event: string;
  request: PendingRequest;
  pending: Map<string | number, PendingRequest>;
  options: ProxyOptions;
  child: ReturnType<typeof spawn>;
  childIdentity: JsonRecord;
  stderrTail: string;
  stdoutTail: string;
  childBuffer: string;
  diagnostic: { code: string; message: string; exitCode: number | null; signal: NodeJS.Signals | null };
}): string | null {
  if (!input.options.diagnosticsDir) return null;
  try {
    mkdirSync(input.options.diagnosticsDir, { recursive: true });
    const now = new Date();
    const artifact = {
      schema: FORENSIC_ARTIFACT_SCHEMA,
      event: input.event,
      captured_at: now.toISOString(),
      proxy: {
        pid: process.pid,
        ppid: process.ppid,
        argv: process.argv,
        cwd: process.cwd(),
        request_timeout_ms: input.options.requestTimeoutMs,
        tool_timeout_grace_ms: input.options.toolTimeoutGraceMs,
        kill_grace_ms: DEFAULT_REQUEST_TIMEOUT_KILL_GRACE_MS,
        runtime_contract_version: input.options.runtimeContractVersion,
        artifact_manifest_path: input.options.artifactManifestPath,
        artifact_manifest_fingerprint: input.options.artifactManifestFingerprint,
        materialization_sidecar_path: input.options.materializationSidecarPath,
        materialization_generation_fingerprint: input.options.materializationGenerationFingerprint,
      },
      surface: {
        surface_id: input.options.surfaceId,
        entrypoint: input.options.entrypoint,
        child_prefix_args: input.options.childPrefixArgs,
        child_args: input.options.childArgs,
      },
      child_process: {
        pid: input.child.pid ?? null,
        killed: input.child.killed,
        exit_code: input.diagnostic.exitCode,
        signal: input.diagnostic.signal,
        ...input.childIdentity,
      },
      diagnostic: input.diagnostic,
      request: serializeRequest(input.request),
      pending_requests: [...input.pending.values()].map(serializeRequest),
      stream_tails: {
        stderr_tail: input.stderrTail,
        stdout_tail: input.stdoutTail,
        child_stdout_partial_buffer_tail: tail(input.childBuffer, STDOUT_TAIL_LIMIT),
      },
    };
    const fileName = `${toArtifactTimestamp(now)}-${safeSegment(input.options.surfaceId ?? 'surface')}-${safeSegment(String(input.request.id))}-${safeSegment(input.event)}.json`;
    const artifactPath = join(input.options.diagnosticsDir, fileName);
    writeFileSync(artifactPath, JSON.stringify(artifact, null, 2) + '\n', 'utf8');
    return artifactPath;
  } catch {
    return null;
  }
}

function serializeRequest(request: PendingRequest): JsonRecord {
  return {
    id: request.id,
    method: request.method,
    tool_name: request.toolName,
    started_at: request.startedAt,
    age_ms: Date.now() - Date.parse(request.startedAt),
    requested_transport_timeout_ms: request.requestedTransportTimeoutMs,
    effective_request_timeout_ms: request.effectiveTimeoutMs,
    progress_token: request.progressToken,
    last_progress: request.lastProgress,
    args_hash: request.argsHash,
    args_summary: request.argsSummary,
    lifecycle: request.lifecycle,
  };
}

function spawnProxyChild(options: ProxyOptions, supervisorPath: string | null): ChildLaunch {
  const resolvedChildCommand = resolveChildCommand(options.childCommand);
  const spawnOptions: import('node:child_process').SpawnOptions = {
    stdio: ['pipe', 'pipe', 'pipe'],
    env: {
      ...process.env,
      ...(options.carrierId ? { NARADA_MATERIALIZED_CARRIER_ID: options.carrierId } : {}),
    },
    shell: false,
    windowsHide: true,
  };
  if (process.platform === 'win32' && supervisorPath) {
    const supervisorIdentityPath = join(
      options.diagnosticsDir ?? defaultDiagnosticsDir(),
      `supervisor-${process.pid}-${randomUUID()}.json`,
    );
    return {
      child: spawn(supervisorPath, [
        '--identity-path',
        supervisorIdentityPath,
        '--parent-pid',
        String(process.pid),
        '--',
        resolvedChildCommand,
        ...options.childPrefixArgs,
        ...(options.childInvocationKind === 'native_entrypoint' ? [] : [options.childInvocationKind === 'native_applet' ? options.childApplet! : options.entrypoint]),
        ...options.childArgs,
      ], spawnOptions) as ChildProcessWithoutNullStreams,
      supervisorPath,
      supervisorIdentityPath,
    };
  }
  return {
    child: spawn(resolvedChildCommand, [
      ...options.childPrefixArgs,
      ...(options.childInvocationKind === 'native_entrypoint' ? [] : [options.childInvocationKind === 'native_applet' ? options.childApplet! : options.entrypoint]),
      ...options.childArgs,
    ], spawnOptions) as ChildProcessWithoutNullStreams,
    supervisorPath: null,
    supervisorIdentityPath: null,
  };
}

function resolveChildCommand(childCommand: string): string {
  if (isAbsolute(childCommand)) return childCommand;
  if (existsSync(childCommand)) return resolve(childCommand);

  const base = basename(childCommand).toLowerCase();
  const isBun = base === 'bun' || base === 'bun.exe';
  const isNode = base === 'node' || base === 'node.exe';

  if (isBun) {
    const candidates = knownBunPaths();
    for (const candidate of candidates) {
      if (existsSync(candidate)) return candidate;
    }
  }

  if (isNode) {
    const candidates = knownNodePaths();
    for (const candidate of candidates) {
      if (existsSync(candidate)) return candidate;
    }
  }

  const pathCandidates = executableOnPath(base);
  if (pathCandidates.length > 0) return pathCandidates[0];

  // Fall back to the original command and let spawn report the failure.
  return childCommand;
}

function knownBunPaths(): string[] {
  const home = process.env.USERPROFILE || process.env.HOME || '';
  const bunInstall = process.env.BUN_INSTALL || '';
  const candidates: string[] = [];
  if (home) {
    candidates.push(join(home, '.bun', 'bin', 'bun.exe'));
    candidates.push(join(home, '.bun', 'bin', 'bun'));
  }
  if (bunInstall) {
    candidates.push(join(bunInstall, 'bun.exe'));
    candidates.push(join(bunInstall, 'bun'));
    candidates.push(join(bunInstall, 'bin', 'bun.exe'));
    candidates.push(join(bunInstall, 'bin', 'bun'));
  }
  return candidates;
}

function knownNodePaths(): string[] {
  const candidates: string[] = [];
  // If the proxy itself is running under Node, use that interpreter.
  const execBase = basename(process.execPath).toLowerCase();
  if (execBase === 'node.exe' || execBase === 'node') {
    candidates.push(process.execPath);
  }
  const programFiles = process.env.PROGRAMFILES || 'C:\\Program Files';
  const programFilesX86 = process.env['PROGRAMFILES(X86)'] || 'C:\\Program Files (x86)';
  if (process.platform === 'win32') {
    candidates.push(join(programFiles, 'nodejs', 'node.exe'));
    candidates.push(join(programFilesX86, 'nodejs', 'node.exe'));
  }
  const home = process.env.USERPROFILE || process.env.HOME || '';
  if (home && process.platform !== 'win32') {
    candidates.push(join(home, '.nvm', 'versions', 'node', 'current', 'bin', 'node'));
  }
  return candidates;
}

function executableOnPath(command: string): string[] {
  const pathVar = process.env.PATH || process.env.Path || process.env.path || '';
  if (!pathVar) return [];
  const separator = process.platform === 'win32' ? ';' : ':';
  const extensions = process.platform === 'win32' ? ['.exe', '.cmd', '.bat', ''] : [''];
  const names = process.platform === 'win32'
    ? [command, command.replace(/\.exe$/i, ''), `${command}.exe`]
    : [command];
  const found: string[] = [];
  for (const dir of pathVar.split(separator)) {
    if (!dir) continue;
    for (const name of names) {
      for (const ext of extensions) {
        const candidate = join(dir, `${name}${ext}`);
        if (existsSync(candidate)) {
          found.push(candidate);
        }
      }
    }
  }
  return found;
}

function readSupervisorIdentity(path: string): JsonRecord | null {
  try {
    const value = JSON.parse(readFileSync(path, 'utf8')) as unknown;
    return isJsonRecord(value) ? value : null;
  } catch {
    return null;
  }
}

function buildChildIdentity(options: ProxyOptions, launch: ChildLaunch): JsonRecord {
  const { entrypoint, childArgs } = options;
  const entrypointStat = safeStat(entrypoint);
  const sourcePath = sourcePathForEntrypoint(entrypoint);
  const sourceStat = sourcePath ? safeStat(sourcePath) : null;
  return {
    parent_pid: process.pid,
    command: options.childCommand,
    entrypoint,
    child_prefix_args: options.childPrefixArgs,
    child_args: childArgs,
    entrypoint_basename: basename(entrypoint),
    entrypoint_sha256: sha256File(entrypoint),
    entrypoint_mtime: entrypointStat?.mtime.toISOString() ?? null,
    entrypoint_size: entrypointStat?.size ?? null,
    source_path: sourcePath,
    source_sha256: sourcePath ? sha256File(sourcePath) : null,
    source_mtime: sourceStat?.mtime.toISOString() ?? null,
    source_size: sourceStat?.size ?? null,
    build_freshness: sourceStat && entrypointStat
      ? sourceStat.mtimeMs > entrypointStat.mtimeMs ? 'source_newer_than_entrypoint' : 'entrypoint_not_older_than_source'
      : 'unknown',
    package: packageMetadataFor(entrypoint),
    supervisor_path: launch.supervisorPath,
    supervisor_identity_path: launch.supervisorIdentityPath,
    child_pid_role: launch.supervisorPath ? 'supervisor' : 'server',
  };
}

function sourcePathForEntrypoint(entrypoint: string): string | null {
  const normalized = entrypoint.replace(/\\/g, '/');
  const marker = '/dist/src/';
  const index = normalized.indexOf(marker);
  if (index < 0) return null;
  const candidate = `${normalized.slice(0, index)}/src/${normalized.slice(index + marker.length).replace(/\.js$/, '.ts')}`;
  return existsSync(candidate) ? candidate : null;
}

function packageMetadataFor(entrypoint: string): JsonRecord | null {
  let current = dirname(entrypoint);
  for (let i = 0; i < 8; i += 1) {
    const packagePath = join(current, 'package.json');
    if (existsSync(packagePath)) {
      try {
        const parsed = JSON.parse(readFileSync(packagePath, 'utf8')) as JsonRecord;
        return {
          package_json_path: packagePath,
          name: typeof parsed.name === 'string' ? parsed.name : null,
          version: typeof parsed.version === 'string' ? parsed.version : null,
          package_json_sha256: sha256File(packagePath),
        };
      } catch {
        return { package_json_path: packagePath, status: 'unreadable' };
      }
    }
    const next = dirname(current);
    if (next === current) break;
    current = next;
  }
  return null;
}

function defaultDiagnosticsDir(): string {
  const siteRoot = process.env.NARADA_SITE_ROOT || process.env.NARADA_WORKSPACE_ROOT || '';
  if (siteRoot) return join(resolve(siteRoot), '.ai', 'runtime', 'mcp-runtime-proxy');
  return join(process.cwd(), '.ai', 'runtime', 'mcp-runtime-proxy');
}

function safeStat(path: string): ReturnType<typeof statSync> | null {
  try {
    return statSync(path);
  } catch {
    return null;
  }
}

function sha256File(path: string): string | null {
  try {
    return createHash('sha256').update(readFileSync(path)).digest('hex');
  } catch {
    return null;
  }
}

function sha256Json(value: unknown): string {
  return createHash('sha256').update(JSON.stringify(value)).digest('hex');
}

function summarizeJson(value: JsonRecord): JsonRecord {
  const summary: JsonRecord = {};
  for (const [key, raw] of Object.entries(value).slice(0, 25)) {
    if (typeof raw === 'string') summary[key] = raw.length > 120 ? { type: 'string', length: raw.length, prefix: raw.slice(0, 120) } : raw;
    else if (typeof raw === 'number' || typeof raw === 'boolean' || raw === null) summary[key] = raw;
    else if (Array.isArray(raw)) summary[key] = { type: 'array', length: raw.length };
    else if (typeof raw === 'object') summary[key] = { type: 'object', keys: Object.keys(raw as JsonRecord).slice(0, 20) };
    else summary[key] = { type: typeof raw };
  }
  if (Object.keys(value).length > 25) summary.__truncated_keys = Object.keys(value).length - 25;
  return summary;
}

function toArtifactTimestamp(date: Date): string {
  return date.toISOString().replace(/[-:.]/g, '');
}

function safeSegment(value: string): string {
  return value.replace(/[^a-zA-Z0-9_.-]+/g, '_').slice(0, 80) || 'unknown';
}

function normalizedPositiveInteger(value: unknown): number | null {
  return typeof value === 'number' && Number.isSafeInteger(value) && value > 0 ? value : null;
}

function isJsonRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function sendCancellationToChild(child: ReturnType<typeof spawn>, request: PendingRequest, reason: string): void {
  const stdin = child.stdin;
  if (!stdin || stdin.destroyed || !stdin.writable) return;
  writeJsonRpcMessageToStream(stdin, {
    jsonrpc: '2.0',
    method: 'notifications/cancelled',
    params: {
      requestId: request.id,
      reason,
    },
  }, request.framed);
}
function rememberTimedOutRequest(timedOutRequests: Map<string | number, NodeJS.Timeout>, id: string | number): void {
  const existingTimer = timedOutRequests.get(id);
  if (existingTimer) clearTimeout(existingTimer);
  const cleanupTimer = setTimeout(() => {
    timedOutRequests.delete(id);
  }, SUPPRESSED_RESPONSE_TTL_MS);
  timedOutRequests.set(id, cleanupTimer);
}

function clearTimedOutRequests(timedOutRequests: Map<string | number, NodeJS.Timeout>): void {
  for (const timer of timedOutRequests.values()) clearTimeout(timer);
  timedOutRequests.clear();
}

function terminateChildAfterRequestTimeout(
  child: ReturnType<typeof spawn>,
  timers: Set<NodeJS.Timeout>,
  isChildClosed: () => boolean,
): void {
  const stdin = child.stdin;
  if (stdin && !stdin.destroyed) stdin.end();
  terminateProxyChild(child, false);
  const sigkillTimer = setTimeout(() => {
    timers.delete(sigkillTimer);
    if (!isChildClosed()) terminateProxyChild(child, true);
  }, DEFAULT_REQUEST_TIMEOUT_KILL_GRACE_MS);
  timers.add(sigkillTimer);
}

function terminateProxyChild(child: ReturnType<typeof spawn>, force: boolean): void {
  if (process.platform === 'win32' && child.pid) {
    const killer = spawn('taskkill', ['/PID', String(child.pid), '/T', '/F'], {
      stdio: 'ignore',
      windowsHide: true,
    });
    killer.unref();
    return;
  }
  child.kill(force ? 'SIGKILL' : 'SIGTERM');
}

function clearTimers(timers: Set<NodeJS.Timeout>): void {
  for (const timer of timers) clearTimeout(timer);
  timers.clear();
}

function parsePositiveInteger(value: string, name: string, maximum = Number.MAX_SAFE_INTEGER): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0 || parsed > maximum) throw new Error(`mcp_runtime_proxy_invalid_${name}:${value}`);
  return parsed;
}

function drainJsonLines(buffer: string): { framed: boolean; remaining: string; requests: JsonRecord[] } {
  const lines = buffer.split(/\r?\n/);
  const remaining = lines.pop() ?? '';
  return {
    framed: false,
    remaining,
    // A carrier may append a presentation continuation marker after an
    // otherwise complete JSON-RPC line. Never let that marker crash the
    // proxy; recover the complete JSON object and discard only the trailing
    // non-protocol text. Standalone malformed lines are ignored as well so
    // child stdout cannot become an uncaught parser exception.
    requests: lines
      .map(parseJsonLine)
      .filter((line): line is JsonRecord => line !== null),
  };
}

function parseJsonLine(line: string): JsonRecord | null {
  const trimmed = line.trim();
  if (!trimmed) return null;
  try {
    const parsed: unknown = JSON.parse(trimmed);
    return isJsonRecord(parsed) ? parsed : null;
  } catch {
    const prefixEnd = firstJsonValueEnd(trimmed);
    if (prefixEnd === null || prefixEnd >= trimmed.length) return null;
    try {
      const parsed: unknown = JSON.parse(trimmed.slice(0, prefixEnd));
      return isJsonRecord(parsed) ? parsed : null;
    } catch {
      return null;
    }
  }
}

function firstJsonValueEnd(text: string): number | null {
  const first = text[0];
  if (first !== '{' && first !== '[') return null;
  const stack: string[] = [];
  let inString = false;
  let escaped = false;
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (inString) {
      if (escaped) escaped = false;
      else if (character === '\\') escaped = true;
      else if (character === '"') inString = false;
      continue;
    }
    if (character === '"') {
      inString = true;
      continue;
    }
    if (character === '{' || character === '[') {
      stack.push(character);
      continue;
    }
    if (character !== '}' && character !== ']') continue;
    const expected = character === '}' ? '{' : '[';
    if (stack.pop() !== expected) return null;
    if (stack.length === 0) return index + 1;
  }
  return null;
}

function drainJsonRpcFrames(buffer: string): { framed: boolean; remaining: string; requests: JsonRecord[] } {
  const requests: JsonRecord[] = [];
  let remaining = buffer;
  while (true) {
    const headerEnd = remaining.indexOf('\r\n\r\n');
    const alternateHeaderEnd = remaining.indexOf('\n\n');
    const end = headerEnd >= 0 ? headerEnd : alternateHeaderEnd;
    const separatorLength = headerEnd >= 0 ? 4 : 2;
    if (end < 0) break;
    const header = remaining.slice(0, end);
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) break;
    const length = Number(match[1]);
    const start = end + separatorLength;
    const finish = start + length;
    if (remaining.length < finish) break;
    requests.push(JSON.parse(remaining.slice(start, finish)) as JsonRecord);
    remaining = remaining.slice(finish);
  }
  return { framed: true, remaining, requests };
}

function writeJsonRpcMessage(message: JsonRecord, framed: boolean): void {
  writeJsonRpcMessageToStream(process.stdout, message, framed);
}

function writeJsonRpcMessageToStream(stream: NodeJS.WritableStream, message: JsonRecord, framed: boolean): void {
  const json = JSON.stringify(message);
  if (framed) stream.write(`Content-Length: ${Buffer.byteLength(json, 'utf8')}\r\n\r\n${json}`);
  else stream.write(`${json}\n`);
}

async function flushProxyStdout(): Promise<void> {
  if (process.stdout.writableEnded || process.stdout.destroyed) return;
  await new Promise<void>((resolve) => {
    process.stdout.end(() => resolve());
  });
}

function tail(text: string, limit: number): string {
  return text.length <= limit ? text : text.slice(text.length - limit);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  runProxy().catch((error) => {
    process.stderr.write(`${describeUnknownError(error, 'mcp_runtime_proxy_error')}\n`);
    process.exit(1);
  });
}
