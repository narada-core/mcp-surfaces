import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { describeUnknownError } from './error-description.js';

export const MCP_RUNTIME_CONTRACT_VERSION = 8 as const;
export const MATERIALIZATION_GENERATION_SCHEMA = 'narada.mcp_materialization_generation.v2' as const;
const LEGACY_MATERIALIZATION_GENERATION_SCHEMA = 'narada.mcp_materialization_generation.v1' as const;

type JsonRecord = Record<string, unknown>;

export type MaterializationGeneration = {
  schema: typeof MATERIALIZATION_GENERATION_SCHEMA;
  contract_version: number;
  carrier_id: string;
  carrier_kind: string;
  config_path: string;
  config_artifact: ConfigArtifact;
  managed_projection: ManagedProjection;
  materialization_contract_entrypoint: string;
  materialization_contract_fingerprint: string | null;
  artifact_manifest_path: string;
  artifact_manifest_fingerprint: string | null;
  runtime_profile_kind: 'native' | 'bun' | 'node-compat';
  runtime_materialization_plan_path: string;
  runtime_materialization_plan_fingerprint: string;
  runtime_implementation_matrix_path: string;
  runtime_implementation_matrix_fingerprint: string;
  registrar_entrypoint: string;
  registrar_fingerprint: string | null;
  proxy_implementation: 'bun' | 'node' | 'native';
  proxy_entrypoint: string;
  proxy_fingerprint: string | null;
  server_count: number;
  proxy_count: number;
  generation_fingerprint: string;
  generated_at: string;
};

export type ConfigArtifact = {
  bytes_sha256: string;
  encoding: 'utf-8';
  bom: boolean;
  line_endings: 'lf' | 'crlf' | 'cr' | 'mixed' | 'none';
  final_newline: boolean;
};

export type ManagedProjection = {
  sha256: string;
  scope: 'codex_managed_selectors' | 'whole_document';
  canonicalization: 'narada.codex_managed_projection.v1' | 'narada.whole_document_bytes.v1';
  selectors: string[];
};

export type MaterializationValidation = {
  schema: 'narada.mcp_materialization_validation.v1';
  ok: boolean;
  contract_version: number;
  server_count: number;
  proxy_count: number;
  errors: Array<{ code: string; server_key: string; detail?: JsonRecord }>;
};

export type MaterializationPreflight = {
  ok: boolean;
  code?: 'materialization_generation_missing' | 'materialization_generation_obsolete' | 'materialization_generation_stale' | 'materialization_managed_projection_stale';
  reason?: string;
  generation_fingerprint: string | null;
  details?: JsonRecord;
  observations?: Array<{ code: 'materialization_artifact_bytes_drift'; detail: JsonRecord }>;
};

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function sha256(value: string | Buffer): string {
  return createHash('sha256').update(value).digest('hex');
}

function nativeContractRequest<T extends JsonRecord>(input: {
  entrypoint: string;
  command: 'contract-describe' | 'contract-fingerprint-generation' | 'contract-merge-codex' | 'contract-format-json';
  payload: JsonRecord;
  configContent?: string;
  configPath?: string;
}): T {
  const root = mkdtempSync(join(tmpdir(), 'narada-materialization-contract-'));
  try {
    const payload = { ...input.payload };
    if (input.configContent !== undefined && input.configPath !== undefined) {
      throw new Error('materialization_contract_config_source_ambiguous');
    }
    if (input.configContent !== undefined) {
      const configPath = join(root, 'config');
      writeFileSync(configPath, input.configContent, 'utf8');
      payload.config_path = configPath;
    } else if (input.configPath !== undefined) {
      payload.config_path = resolve(input.configPath);
    }
    const inputPath = join(root, 'input.json');
    writeFileSync(inputPath, JSON.stringify(payload) + '\n', 'utf8');
    const result = spawnSync(input.entrypoint, [input.command, '--input', inputPath], {
      encoding: 'utf8',
      windowsHide: true,
      maxBuffer: 1024 * 1024,
    });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`materialization_contract_native_failed:${result.status}:${result.stderr.trim()}`);
    const parsed: unknown = JSON.parse(result.stdout.trim());
    if (!isRecord(parsed)) throw new Error('materialization_contract_native_result_invalid');
    return parsed as T;
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

export function describeMaterializedConfiguration(input: {
  entrypoint: string;
  carrierKind: string;
  content: string;
  selectors?: string[];
  pluginIds?: string[];
  projectPaths?: string[];
}): { config_artifact: ConfigArtifact; managed_projection: ManagedProjection } {
  return nativeContractRequest({
    entrypoint: input.entrypoint,
    command: 'contract-describe',
    configContent: input.content,
    payload: {
      carrier_kind: input.carrierKind,
      selectors: input.selectors ?? [],
      plugin_ids: input.pluginIds ?? [],
      project_paths: input.projectPaths ?? [],
    },
  }) as { config_artifact: ConfigArtifact; managed_projection: ManagedProjection };
}

function describeMaterializedConfigurationFile(input: {
  entrypoint: string;
  carrierKind: string;
  configPath: string;
  selectors?: string[];
}): { config_artifact: ConfigArtifact; managed_projection: ManagedProjection } {
  return nativeContractRequest({
    entrypoint: input.entrypoint,
    command: 'contract-describe',
    configPath: input.configPath,
    payload: {
      carrier_kind: input.carrierKind,
      selectors: input.selectors ?? [],
    },
  }) as { config_artifact: ConfigArtifact; managed_projection: ManagedProjection };
}

function nativeGenerationFingerprint(entrypoint: string, generation: JsonRecord): string {
  const result = nativeContractRequest<{ generation_fingerprint: string }>({
    entrypoint,
    command: 'contract-fingerprint-generation',
    payload: generation,
  });
  if (typeof result.generation_fingerprint !== 'string') throw new Error('materialization_contract_generation_fingerprint_missing');
  return result.generation_fingerprint;
}

export function mergeCodexMaterializedConfiguration(input: {
  materializationContractEntrypoint: string;
  configPath: string;
  desiredContent: string;
  pluginIds: string[];
  projectPaths: string[];
}): string {
  const root = mkdtempSync(join(tmpdir(), 'narada-codex-merge-'));
  try {
    const desiredPath = join(root, 'desired.toml');
    const outputPath = join(root, 'merged.toml');
    writeFileSync(desiredPath, input.desiredContent, 'utf8');
    const sidecarPath = materializationSidecarPath(input.configPath);
    let previousSelectors: string[] = [];
    if (existsSync(sidecarPath)) {
      const generation: unknown = JSON.parse(readFileSync(sidecarPath, 'utf8'));
      if (isRecord(generation) && generation.schema === MATERIALIZATION_GENERATION_SCHEMA) {
        const projection = isRecord(generation.managed_projection) ? generation.managed_projection : null;
        if (!projection || !Array.isArray(projection.selectors) || !projection.selectors.every((value) => typeof value === 'string')) {
          throw new Error('materialization_generation_managed_selectors_invalid');
        }
        previousSelectors = projection.selectors as string[];
      } else if (isRecord(generation) && generation.schema === LEGACY_MATERIALIZATION_GENERATION_SCHEMA) {
        previousSelectors = ['/mcp_servers'];
      } else {
        throw new Error('materialization_generation_schema_unsupported');
      }
    }
    nativeContractRequest({
      entrypoint: input.materializationContractEntrypoint,
      command: 'contract-merge-codex',
      payload: {
        existing_path: existsSync(input.configPath) ? resolve(input.configPath) : null,
        desired_path: desiredPath,
        output_path: outputPath,
        previous_selectors: previousSelectors,
        plugin_ids: input.pluginIds,
        project_paths: input.projectPaths,
      },
    });
    return readFileSync(outputPath, 'utf8');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

export function formatMaterializedJson(input: {
  materializationContractEntrypoint: string;
  value: unknown;
  header?: string;
}): string {
  const root = mkdtempSync(join(tmpdir(), 'narada-json-format-'));
  try {
    const sourcePath = join(root, 'source.json');
    const outputPath = join(root, 'output.json');
    writeFileSync(sourcePath, JSON.stringify(input.value), 'utf8');
    nativeContractRequest({
      entrypoint: input.materializationContractEntrypoint,
      command: 'contract-format-json',
      payload: {
        source_path: sourcePath,
        output_path: outputPath,
        header: input.header ?? null,
      },
    });
    return readFileSync(outputPath, 'utf8');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function sha256File(path: string): string | null {
  if (!existsSync(path)) return null;
  try {
    return createHash('sha256').update(readFileSync(path)).digest('hex');
  } catch {
    return null;
  }
}

function runtimeMaterializationPlanUnsigned(plan: JsonRecord): JsonRecord {
  const unsigned = { ...plan };
  delete unsigned.plan_fingerprint;
  return unsigned;
}

function runtimeMaterializationPlanFileFingerprint(path: string): string | null {
  if (!existsSync(path)) return null;
  try {
    const parsed: unknown = JSON.parse(readFileSync(path, 'utf8'));
    if (!isRecord(parsed) || typeof parsed.plan_fingerprint !== 'string') return null;
    return sha256(JSON.stringify(runtimeMaterializationPlanUnsigned(parsed)));
  } catch {
    return null;
  }
}

function argValue(args: string[], name: string): string | null {
  const index = args.indexOf(name);
  return index >= 0 && typeof args[index + 1] === 'string' ? args[index + 1]! : null;
}

function pathEquals(left: string, right: string): boolean {
  return resolve(left).toLowerCase() === resolve(right).toLowerCase();
}

function launchRecords(structured: JsonRecord): Array<[string, JsonRecord]> {
  const root = isRecord(structured.mcpServers)
    ? structured.mcpServers
    : isRecord(structured.mcp)
      ? structured.mcp
      : {};
  return Object.entries(root).flatMap(([key, value]): Array<[string, JsonRecord]> => isRecord(value) ? [[key, value]] : []);
}

function launchCommand(record: JsonRecord): { command: string; args: string[] } | null {
  if (Array.isArray(record.command)) {
    const values = record.command.filter((value): value is string => typeof value === 'string');
    if (values.length === 0) return null;
    return { command: values[0]!, args: values.slice(1) };
  }
  if (typeof record.command !== 'string') return null;
  return {
    command: record.command,
    args: Array.isArray(record.args) ? record.args.filter((value): value is string => typeof value === 'string') : [],
  };
}

export function materializationSidecarPath(configPath: string): string {
  return `${resolve(configPath)}.narada-generation.json`;
}

export function validateMaterializedConfiguration(input: {
  structured: JsonRecord;
  artifactManifestPath: string;
  runtimeProxyEntrypoint: string;
  expectedSidecarPath?: string;
  requireSidecar: boolean;
}): MaterializationValidation {
  const errors: MaterializationValidation['errors'] = [];
  const records = launchRecords(input.structured);
  let proxyCount = 0;
  for (const [serverKey, record] of records) {
    const launch = launchCommand(record);
    if (!launch) continue;
    const isProxy = launch.args.includes('--surface-id') || launch.args.some((arg) => arg.includes('mcp-runtime-proxy'));
    if (!isProxy) continue;
    proxyCount += 1;
    if (!isAbsolute(launch.command)) {
      errors.push({ code: 'materialized_config_proxy_command_not_absolute', server_key: serverKey, detail: { command: launch.command } });
    }
    const version = argValue(launch.args, '--runtime-contract-version');
    if (version !== String(MCP_RUNTIME_CONTRACT_VERSION)) {
      errors.push({ code: 'materialized_config_contract_version_mismatch', server_key: serverKey, detail: { actual: version, expected: MCP_RUNTIME_CONTRACT_VERSION } });
    }
    const manifestPath = argValue(launch.args, '--artifact-manifest');
    if (!manifestPath) {
      errors.push({ code: 'materialized_config_missing_artifact_manifest', server_key: serverKey });
    } else if (!pathEquals(manifestPath, input.artifactManifestPath)) {
      errors.push({ code: 'materialized_config_artifact_manifest_mismatch', server_key: serverKey, detail: { actual: manifestPath, expected: input.artifactManifestPath } });
    } else if (!existsSync(manifestPath)) {
      errors.push({ code: 'materialized_config_artifact_manifest_missing', server_key: serverKey, detail: { path: manifestPath } });
    }
    const proxyPath = launch.args.find((arg) => arg.includes('mcp-runtime-proxy')) ?? launch.command;
    if (!pathEquals(proxyPath, input.runtimeProxyEntrypoint)) {
      errors.push({ code: 'materialized_config_runtime_proxy_mismatch', server_key: serverKey, detail: { actual: proxyPath, expected: input.runtimeProxyEntrypoint } });
    }
    if (!existsSync(proxyPath)) {
      errors.push({ code: 'materialized_config_runtime_proxy_missing', server_key: serverKey, detail: { path: proxyPath } });
    }
    const childEntrypoint = argValue(launch.args, '--entrypoint');
    const childCommand = argValue(launch.args, '--child-command');
    const childInvocationKind = argValue(launch.args, '--child-invocation-kind') ?? 'entrypoint';
    const childApplet = argValue(launch.args, '--child-applet');
    if (!childCommand) {
      errors.push({ code: 'materialized_config_child_command_missing', server_key: serverKey });
    } else if (!isAbsolute(childCommand)) {
      errors.push({ code: 'materialized_config_child_command_not_absolute', server_key: serverKey, detail: { command: childCommand } });
    }
    if (!childEntrypoint) {
      errors.push({ code: 'materialized_config_child_entrypoint_missing', server_key: serverKey });
    } else if (!existsSync(childEntrypoint)) {
      errors.push({ code: 'materialized_config_child_entrypoint_missing', server_key: serverKey, detail: { path: childEntrypoint } });
    }
    if (childInvocationKind !== 'entrypoint' && childInvocationKind !== 'native_applet' && childInvocationKind !== 'native_entrypoint') {
      errors.push({ code: 'materialized_config_child_invocation_kind_invalid', server_key: serverKey, detail: { child_invocation_kind: childInvocationKind } });
    }
    if ((childInvocationKind === 'native_entrypoint' || childInvocationKind === 'native_applet')
      && childCommand
      && childEntrypoint
      && !pathEquals(childCommand, childEntrypoint)) {
      errors.push({
        code: 'materialized_config_native_child_entrypoint_mismatch',
        server_key: serverKey,
        detail: { child_command: childCommand, child_entrypoint: childEntrypoint, child_invocation_kind: childInvocationKind },
      });
    }
    if (childInvocationKind === 'native_applet' && !childApplet) {
      errors.push({ code: 'materialized_config_child_applet_missing', server_key: serverKey });
    }
    const registrarEntrypoint = argValue(launch.args, '--registrar-entrypoint');
    const registrarCommand = argValue(launch.args, '--registrar-command');
    if (registrarEntrypoint && !registrarCommand) {
      errors.push({ code: 'materialized_config_registrar_command_missing', server_key: serverKey });
    } else if (registrarCommand && !isAbsolute(registrarCommand)) {
      errors.push({ code: 'materialized_config_registrar_command_not_absolute', server_key: serverKey, detail: { command: registrarCommand } });
    }
    const sidecarPath = argValue(launch.args, '--materialization-sidecar');
    if (input.requireSidecar && !sidecarPath) {
      errors.push({ code: 'materialized_config_missing_generation_sidecar', server_key: serverKey });
    } else if (sidecarPath && input.expectedSidecarPath && !pathEquals(sidecarPath, input.expectedSidecarPath)) {
      errors.push({ code: 'materialized_config_generation_sidecar_mismatch', server_key: serverKey, detail: { actual: sidecarPath, expected: input.expectedSidecarPath } });
    }
  }
  return {
    schema: 'narada.mcp_materialization_validation.v1',
    ok: errors.length === 0,
    contract_version: MCP_RUNTIME_CONTRACT_VERSION,
    server_count: records.length,
    proxy_count: proxyCount,
    errors,
  };
}

export function buildMaterializationGeneration(input: {
  carrierId: string;
  carrierKind: string;
  configPath: string;
  content: string;
  managedSelectors?: string[];
  managedPluginIds?: string[];
  managedProjectPaths?: string[];
  materializationContractEntrypoint: string;
  artifactManifestPath: string;
  artifactManifestFingerprint: string | null;
  runtimeProfileKind: 'native' | 'bun' | 'node-compat';
  runtimeMaterializationPlanPath: string;
  runtimeMaterializationPlanFingerprint: string;
  runtimeImplementationMatrixPath: string;
  runtimeImplementationMatrixFingerprint: string;
  registrarEntrypoint: string;
  proxyImplementation: 'bun' | 'node' | 'native';
  proxyEntrypoint: string;
  serverCount: number;
  proxyCount: number;
}): MaterializationGeneration {
  const description = describeMaterializedConfiguration({
    entrypoint: input.materializationContractEntrypoint,
    carrierKind: input.carrierKind,
    content: input.content,
    selectors: input.managedSelectors,
    pluginIds: input.managedPluginIds,
    projectPaths: input.managedProjectPaths,
  });
  const unsigned = {
    schema: MATERIALIZATION_GENERATION_SCHEMA,
    contract_version: MCP_RUNTIME_CONTRACT_VERSION,
    carrier_id: input.carrierId,
    carrier_kind: input.carrierKind,
    config_path: resolve(input.configPath),
    config_artifact: description.config_artifact,
    managed_projection: description.managed_projection,
    materialization_contract_entrypoint: resolve(input.materializationContractEntrypoint),
    materialization_contract_fingerprint: sha256File(input.materializationContractEntrypoint),
    artifact_manifest_path: resolve(input.artifactManifestPath),
    artifact_manifest_fingerprint: input.artifactManifestFingerprint,
    runtime_profile_kind: input.runtimeProfileKind,
    runtime_materialization_plan_path: resolve(input.runtimeMaterializationPlanPath),
    runtime_materialization_plan_fingerprint: input.runtimeMaterializationPlanFingerprint,
    runtime_implementation_matrix_path: resolve(input.runtimeImplementationMatrixPath),
    runtime_implementation_matrix_fingerprint: input.runtimeImplementationMatrixFingerprint,
    registrar_entrypoint: resolve(input.registrarEntrypoint),
    registrar_fingerprint: sha256File(input.registrarEntrypoint),
    proxy_implementation: input.proxyImplementation,
    proxy_entrypoint: resolve(input.proxyEntrypoint),
    proxy_fingerprint: sha256File(input.proxyEntrypoint),
    server_count: input.serverCount,
    proxy_count: input.proxyCount,
    generated_at: new Date().toISOString(),
  };
  return {
    ...unsigned,
    generation_fingerprint: nativeGenerationFingerprint(input.materializationContractEntrypoint, unsigned),
  };
}

export function writeMaterializationGeneration(path: string, generation: MaterializationGeneration): void {
  const resolved = resolve(path);
  mkdirSync(dirname(resolved), { recursive: true });
  const temporary = `${resolved}.tmp-${process.pid}-${Date.now()}`;
  writeFileSync(temporary, JSON.stringify(generation, null, 2) + '\n', 'utf8');
  try {
    renameSync(temporary, resolved);
  } finally {
    if (existsSync(temporary)) unlinkSync(temporary);
  }
}

export function preflightMaterializationGeneration(input: {
  sidecarPath: string | null;
  manifestPath: string;
  manifestFingerprint: string | null;
  materializationContractEntrypoint: string | null;
  runtimeProxyEntrypoint: string | null;
}): MaterializationPreflight {
  if (!input.sidecarPath || !existsSync(input.sidecarPath)) {
    return { ok: false, code: 'materialization_generation_missing', reason: 'The materialization generation sidecar is missing.', generation_fingerprint: null, details: { sidecar_path: input.sidecarPath } };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(readFileSync(input.sidecarPath, 'utf8'));
  } catch (error) {
    return { ok: false, code: 'materialization_generation_stale', reason: 'The materialization generation sidecar is unreadable.', generation_fingerprint: null, details: { error: describeUnknownError(error, 'materialization_generation_read_error') } };
  }
  if (isRecord(parsed) && parsed.schema === LEGACY_MATERIALIZATION_GENERATION_SCHEMA) {
    return {
      ok: false,
      code: 'materialization_generation_obsolete',
      reason: 'The materialization generation uses the ambiguous v1 configuration fingerprint contract.',
      generation_fingerprint: typeof parsed.generation_fingerprint === 'string' ? parsed.generation_fingerprint : null,
      details: { remediation: 'Regenerate this carrier with the current materializer; v1 remains readable only as recovery input.' },
    };
  }
  if (!isRecord(parsed) || parsed.schema !== MATERIALIZATION_GENERATION_SCHEMA || typeof parsed.generation_fingerprint !== 'string') {
    return { ok: false, code: 'materialization_generation_stale', reason: 'The materialization generation sidecar has an unsupported schema.', generation_fingerprint: null };
  }
  const generation = parsed as unknown as MaterializationGeneration;
  if (
    typeof generation.contract_version !== 'number' ||
    typeof generation.carrier_id !== 'string' ||
    typeof generation.carrier_kind !== 'string' ||
    typeof generation.config_path !== 'string' ||
    !isRecord(generation.config_artifact) ||
    typeof generation.config_artifact.bytes_sha256 !== 'string' ||
    generation.config_artifact.encoding !== 'utf-8' ||
    typeof generation.config_artifact.bom !== 'boolean' ||
    typeof generation.config_artifact.line_endings !== 'string' ||
    typeof generation.config_artifact.final_newline !== 'boolean' ||
    !isRecord(generation.managed_projection) ||
    typeof generation.managed_projection.sha256 !== 'string' ||
    (generation.managed_projection.scope !== 'codex_managed_selectors' && generation.managed_projection.scope !== 'whole_document') ||
    typeof generation.managed_projection.canonicalization !== 'string' ||
    !Array.isArray(generation.managed_projection.selectors) ||
    !generation.managed_projection.selectors.every((selector) => typeof selector === 'string') ||
    typeof generation.materialization_contract_entrypoint !== 'string' ||
    (generation.materialization_contract_fingerprint !== null && typeof generation.materialization_contract_fingerprint !== 'string') ||
    typeof generation.artifact_manifest_path !== 'string' ||
    (generation.artifact_manifest_fingerprint !== null && typeof generation.artifact_manifest_fingerprint !== 'string') ||
    (generation.runtime_profile_kind !== 'native' && generation.runtime_profile_kind !== 'bun' && generation.runtime_profile_kind !== 'node-compat') ||
    typeof generation.runtime_materialization_plan_path !== 'string' ||
    typeof generation.runtime_materialization_plan_fingerprint !== 'string' ||
    typeof generation.runtime_implementation_matrix_path !== 'string' ||
    typeof generation.runtime_implementation_matrix_fingerprint !== 'string' ||
    typeof generation.registrar_entrypoint !== 'string' ||
    (generation.registrar_fingerprint !== null && typeof generation.registrar_fingerprint !== 'string') ||
    (generation.proxy_implementation !== 'bun' && generation.proxy_implementation !== 'node' && generation.proxy_implementation !== 'native') ||
    typeof generation.proxy_entrypoint !== 'string' ||
    (generation.proxy_fingerprint !== null && typeof generation.proxy_fingerprint !== 'string') ||
    typeof generation.server_count !== 'number' ||
    typeof generation.proxy_count !== 'number' ||
    typeof generation.generated_at !== 'string'
  ) {
    return { ok: false, code: 'materialization_generation_stale', reason: 'The materialization generation sidecar is structurally incomplete.', generation_fingerprint: generation.generation_fingerprint };
  }
  const generationContext: JsonRecord = {
    carrier_id: generation.carrier_id,
    carrier_kind: generation.carrier_kind,
    config_path: resolve(generation.config_path),
    materialization_contract_entrypoint: resolve(generation.materialization_contract_entrypoint),
    materialization_contract_fingerprint: generation.materialization_contract_fingerprint,
    registrar_entrypoint: resolve(generation.registrar_entrypoint),
    registrar_fingerprint: generation.registrar_fingerprint,
    proxy_implementation: generation.proxy_implementation,
    proxy_entrypoint: resolve(generation.proxy_entrypoint),
    proxy_fingerprint: generation.proxy_fingerprint,
    runtime_profile_kind: generation.runtime_profile_kind,
    runtime_materialization_plan_path: resolve(generation.runtime_materialization_plan_path),
    runtime_materialization_plan_fingerprint: generation.runtime_materialization_plan_fingerprint,
    runtime_implementation_matrix_path: resolve(generation.runtime_implementation_matrix_path),
    runtime_implementation_matrix_fingerprint: generation.runtime_implementation_matrix_fingerprint,
    materialization_generated_at: generation.generated_at,
  };
  const stale = (reason: string, details: JsonRecord = {}): MaterializationPreflight => ({
    ok: false,
    code: 'materialization_generation_stale',
    reason,
    generation_fingerprint: generation.generation_fingerprint,
    details: { ...generationContext, ...details },
  });
  if (!input.materializationContractEntrypoint) {
    return stale('The trusted materialization contract authority is absent from the carrier launch.');
  }
  const trustedContractEntrypoint = resolve(input.materializationContractEntrypoint);
  if (!pathEquals(generation.materialization_contract_entrypoint, trustedContractEntrypoint)) {
    return stale('The materialization generation references a different contract authority than the carrier launch.', {
      expected_materialization_contract_entrypoint: trustedContractEntrypoint,
      actual_materialization_contract_entrypoint: generation.materialization_contract_entrypoint,
    });
  }
  const currentContractFingerprint = sha256File(trustedContractEntrypoint);
  if (!currentContractFingerprint || currentContractFingerprint !== generation.materialization_contract_fingerprint) {
    return stale('The materialization contract authority changed after generation.', { materialization_contract_entrypoint: trustedContractEntrypoint });
  }
  if (nativeGenerationFingerprint(trustedContractEntrypoint, parsed) !== generation.generation_fingerprint) {
    return stale('The materialization generation fingerprint does not match its contents.');
  }
  const sidecarSuffix = '.narada-generation.json';
  const expectedConfigPath = input.sidecarPath.endsWith(sidecarSuffix)
    ? input.sidecarPath.slice(0, -sidecarSuffix.length)
    : null;
  if (!expectedConfigPath || !pathEquals(generation.config_path, expectedConfigPath)) {
    return stale('The materialization generation sidecar is not paired with its carrier configuration.', { expected_config_path: expectedConfigPath, actual_config_path: generation.config_path });
  }
  const expectedPlanPath = `${expectedConfigPath}.narada-runtime-plan.json`;
  if (!pathEquals(generation.runtime_materialization_plan_path, expectedPlanPath)) {
    return stale('The materialization generation sidecar is not paired with its runtime materialization plan.', { expected_plan_path: expectedPlanPath, actual_plan_path: generation.runtime_materialization_plan_path });
  }
  const runtimePlanPath = resolve(generation.runtime_materialization_plan_path);
  let runtimePlan: unknown;
  try {
    runtimePlan = JSON.parse(readFileSync(runtimePlanPath, 'utf8'));
  } catch (error) {
    return stale('The runtime materialization plan is missing or unreadable.', { runtime_materialization_plan_path: runtimePlanPath, error: describeUnknownError(error, 'runtime_materialization_plan_read_error') });
  }
  if (!isRecord(runtimePlan)
    || runtimePlan.schema !== 'narada.runtime_materialization_plan.v1'
    || runtimePlan.status !== 'accepted'
    || typeof runtimePlan.plan_fingerprint !== 'string'
    || runtimePlan.runtime_profile_kind !== generation.runtime_profile_kind) {
    return stale('The runtime materialization plan is structurally incomplete or uses a different runtime profile.', { runtime_materialization_plan_path: runtimePlanPath });
  }
  const runtimePlanRecord = runtimePlan as JsonRecord;
  const runtimePlanFingerprint = runtimeMaterializationPlanFileFingerprint(runtimePlanPath);
  if (!runtimePlanFingerprint || runtimePlanFingerprint !== generation.runtime_materialization_plan_fingerprint || runtimePlanFingerprint !== runtimePlan.plan_fingerprint) {
    return stale('The runtime materialization plan changed after generation.', { runtime_materialization_plan_path: runtimePlanPath, expected_plan_fingerprint: generation.runtime_materialization_plan_fingerprint, actual_plan_fingerprint: runtimePlanFingerprint });
  }
  const runtimePlanSource = isRecord(runtimePlanRecord.source) ? runtimePlanRecord.source : null;
  if (!runtimePlanSource || runtimePlanSource.matrix_fingerprint !== generation.runtime_implementation_matrix_fingerprint) {
    return stale('The runtime materialization plan references a different implementation matrix.', { runtime_implementation_matrix_path: generation.runtime_implementation_matrix_path, expected_matrix_fingerprint: generation.runtime_implementation_matrix_fingerprint, actual_matrix_fingerprint: runtimePlanSource?.matrix_fingerprint ?? null });
  }
  const currentMatrixFingerprint = sha256File(generation.runtime_implementation_matrix_path);
  if (!currentMatrixFingerprint || currentMatrixFingerprint !== generation.runtime_implementation_matrix_fingerprint) {
    return stale('The runtime implementation matrix changed after generation.', { runtime_implementation_matrix_path: generation.runtime_implementation_matrix_path, expected_matrix_fingerprint: generation.runtime_implementation_matrix_fingerprint, actual_matrix_fingerprint: currentMatrixFingerprint });
  }
  const configPath = resolve(generation.config_path);
  let currentDescription: { config_artifact: ConfigArtifact; managed_projection: ManagedProjection };
  try {
    currentDescription = describeMaterializedConfigurationFile({
      entrypoint: trustedContractEntrypoint,
      carrierKind: generation.carrier_kind,
      configPath,
      selectors: generation.managed_projection.selectors,
    });
  } catch (error) {
    return {
      ...stale('The Narada-managed configuration projection cannot be read.', { config_path: configPath, error: describeUnknownError(error, 'materialization_managed_projection_read_error') }),
      code: 'materialization_managed_projection_stale',
    };
  }
  if (currentDescription.managed_projection.sha256 !== generation.managed_projection.sha256) {
    return {
      ...stale('The Narada-managed configuration projection changed after generation.', {
        config_path: configPath,
        managed_scope: generation.managed_projection.scope,
        managed_selectors: generation.managed_projection.selectors,
        expected_managed_projection_sha256: generation.managed_projection.sha256,
        actual_managed_projection_sha256: currentDescription.managed_projection.sha256,
      }),
      code: 'materialization_managed_projection_stale',
    };
  }
  if (!pathEquals(generation.artifact_manifest_path, input.manifestPath) || generation.artifact_manifest_fingerprint !== input.manifestFingerprint) {
    return stale('The materialization generation references a different workspace artifact manifest.', { expected_manifest_path: input.manifestPath, actual_manifest_path: generation.artifact_manifest_path, expected_manifest_fingerprint: input.manifestFingerprint, actual_manifest_fingerprint: generation.artifact_manifest_fingerprint });
  }
  const currentRegistrarFingerprint = sha256File(generation.registrar_entrypoint);
  if (!currentRegistrarFingerprint || currentRegistrarFingerprint !== generation.registrar_fingerprint) {
    return stale('The registrar build changed after configuration generation.', { registrar_entrypoint: generation.registrar_entrypoint });
  }
  const currentProxyFingerprint = sha256File(generation.proxy_entrypoint);
  if (!input.runtimeProxyEntrypoint || !pathEquals(generation.proxy_entrypoint, input.runtimeProxyEntrypoint)) {
    return stale('The materialization generation references a different runtime proxy than the carrier launch.', {
      expected_proxy_entrypoint: input.runtimeProxyEntrypoint,
      actual_proxy_entrypoint: generation.proxy_entrypoint,
    });
  }
  if (!currentProxyFingerprint || currentProxyFingerprint !== generation.proxy_fingerprint) {
    return stale('The selected runtime proxy changed after configuration generation.', { proxy_implementation: generation.proxy_implementation, proxy_entrypoint: generation.proxy_entrypoint });
  }
  if (generation.contract_version !== MCP_RUNTIME_CONTRACT_VERSION) {
    return stale('The materialization contract version is obsolete.', { actual: generation.contract_version, expected: MCP_RUNTIME_CONTRACT_VERSION });
  }
  const currentArtifact = currentDescription.config_artifact;
  if (currentArtifact.bytes_sha256 !== generation.config_artifact.bytes_sha256) {
    return {
      ok: true,
      generation_fingerprint: generation.generation_fingerprint,
      observations: [{
        code: 'materialization_artifact_bytes_drift',
        detail: {
          config_path: configPath,
          expected_bytes_sha256: generation.config_artifact.bytes_sha256,
          actual_bytes_sha256: currentArtifact.bytes_sha256,
          managed_projection_unchanged: true,
        },
      }],
    };
  }
  return { ok: true, generation_fingerprint: generation.generation_fingerprint };
}
