import {
  MCP_FABRIC_SCHEMA_VERSION,
  canonicalizeJson,
  fabricManifestDigest,
  parseFabricManifestV2,
  parseSurfaceDescriptorV2,
  stableDigest,
  surfaceDescriptorDigest,
  type FabricBindingV2,
  type FabricManifestV2,
  type SurfaceDescriptorV2,
  type SurfaceProjectionV2,
  type ToolContractV2,
} from '@narada-core/mcp-fabric-contracts';
import {
  MOONSHOT_SCHEMA_DIALECT,
  validateMoonshotToolInputSchema,
} from './moonshot-schema.js';

export * from './reconciliation.js';

export type CarrierKind = 'codex' | 'kimi' | 'opencode';

export type ApprovalDecision = {
  binding_id: string;
  server_name: string;
  tool_name: string;
  decision: 'allow' | 'prompt';
  reasons: string[];
};

export type CarrierArtifact = {
  carrier_kind: CarrierKind;
  format: 'toml' | 'json' | 'jsonc';
  manifest_digest: string;
  document: Record<string, unknown>;
  content: string;
  approvals: ApprovalDecision[];
};

export type MoonshotCompilerDiagnostic = {
  tool_name: string;
  schema_path: string;
  dialect: string;
  code: string;
  message: string;
  remediation: string;
};

export class CarrierSchemaCompatibilityError extends Error {
  readonly diagnostics: MoonshotCompilerDiagnostic[];

  constructor(diagnostics: MoonshotCompilerDiagnostic[]) {
    super(`mcp_fabric_carrier_schema_incompatible: ${diagnostics.length} finding(s)`);
    this.name = 'CarrierSchemaCompatibilityError';
    this.diagnostics = diagnostics;
  }
}

export function compileFabricManifest(input: {
  manifest_id: string;
  site_id: string;
  generated_at: string;
  descriptors: unknown[];
  bindings: FabricBindingV2[];
}): FabricManifestV2 {
  const descriptors = input.descriptors.map(parseSurfaceDescriptorV2);
  const descriptorById = new Map(descriptors.map((descriptor) => [descriptor.surface_id, descriptor]));
  for (const binding of input.bindings) {
    const descriptor = descriptorById.get(binding.surface_id);
    if (descriptor === undefined) {
      throw new Error(
        `mcp_fabric_binding_surface_missing: ${binding.binding_id} -> ${binding.surface_id}`,
      );
    }
    if (!descriptor.projections.some((projection) => projection.id === binding.projection_id)) {
      throw new Error(
        `mcp_fabric_binding_projection_missing: ${binding.binding_id} -> ${binding.projection_id}`,
      );
    }
  }
  const sourceDigest = stableDigest({
    descriptors: descriptors
      .map((descriptor) => ({
        surface_id: descriptor.surface_id,
        digest: surfaceDescriptorDigest(descriptor),
      }))
      .sort((left, right) => left.surface_id.localeCompare(right.surface_id)),
    bindings: [...input.bindings].sort((left, right) => left.binding_id.localeCompare(right.binding_id)),
  });
  return deepFreeze(parseFabricManifestV2({
    schema_version: MCP_FABRIC_SCHEMA_VERSION,
    manifest_id: input.manifest_id,
    site_id: input.site_id,
    generated_at: input.generated_at,
    descriptors,
    bindings: input.bindings,
    source_digest: sourceDigest,
  }));
}

export function compileAllCarrierArtifacts(manifestValue: unknown): {
  manifest: FabricManifestV2;
  manifest_digest: string;
  artifacts: Record<CarrierKind, CarrierArtifact>;
} {
  const manifest = parseFabricManifestV2(manifestValue);
  const digest = fabricManifestDigest(manifest);
  const artifacts = {
    codex: compileCarrierArtifact(manifest, 'codex'),
    kimi: compileCarrierArtifact(manifest, 'kimi'),
    opencode: compileCarrierArtifact(manifest, 'opencode'),
  };
  return deepFreeze({ manifest, manifest_digest: digest, artifacts });
}

export function compileCarrierArtifact(
  manifestValue: unknown,
  carrierKind: CarrierKind,
): CarrierArtifact {
  const manifest = parseFabricManifestV2(manifestValue);
  const digest = fabricManifestDigest(manifest);
  const resolved = resolveEnabledBindings(manifest);
  if (carrierKind === 'kimi') {
    for (const item of resolved) {
      for (const tool of item.descriptor.tools) {
        transformMoonshotToolSchema(tool.name, tool.input_schema);
      }
    }
  }
  const approvals = deriveApprovalDecisions(resolved);
  const document = carrierDocument(carrierKind, resolved, approvals);
  return deepFreeze({
    carrier_kind: carrierKind,
    format: carrierKind === 'codex' ? 'toml' : carrierKind === 'kimi' ? 'json' : 'jsonc',
    manifest_digest: digest,
    document,
    content: carrierKind === 'codex'
      ? renderCodexToml(document)
      : `${JSON.stringify(canonicalizeJson(document), null, 2)}\n`,
    approvals,
  });
}

export function transformMoonshotToolSchema(
  toolName: string,
  schemaValue: Record<string, unknown>,
): Record<string, unknown> {
  const transformed = transformAnyOfParentTypes(
    structuredClone(schemaValue),
    toolName,
    'root',
  );
  const findings = validateMoonshotToolInputSchema(transformed);
  if (findings.length > 0) {
    throw new CarrierSchemaCompatibilityError(findings.map((finding) => ({
      tool_name: toolName,
      schema_path: finding.path,
      dialect: MOONSHOT_SCHEMA_DIALECT,
      code: finding.code,
      message: finding.message,
      remediation: 'Change the package-owned tool schema or add an explicit semantics-preserving compiler transform; do not weaken runtime policy.',
    })));
  }
  return transformed;
}

type ResolvedBinding = {
  binding: FabricBindingV2;
  descriptor: SurfaceDescriptorV2;
  projection: SurfaceProjectionV2;
};

function resolveEnabledBindings(manifest: FabricManifestV2): ResolvedBinding[] {
  const descriptorById = new Map(
    manifest.descriptors.map((descriptor) => [descriptor.surface_id, descriptor]),
  );
  return manifest.bindings
    .filter((binding) => binding.enabled)
    .map((binding) => {
      const descriptor = descriptorById.get(binding.surface_id);
      const projection = descriptor?.projections.find((candidate) => candidate.id === binding.projection_id);
      if (descriptor === undefined || projection === undefined) {
        throw new Error(`mcp_fabric_manifest_binding_unresolved: ${binding.binding_id}`);
      }
      return { binding, descriptor, projection };
    })
    .sort((left, right) => left.binding.surface_id.localeCompare(right.binding.surface_id));
}

function deriveApprovalDecisions(resolved: ResolvedBinding[]): ApprovalDecision[] {
  return resolved.flatMap(({ binding, descriptor, projection }) =>
    descriptor.tools.map((tool) => {
      const reasons: string[] = [];
      if (tool.effect.confirmation !== 'never') {
        reasons.push(`effect.confirmation=${tool.effect.confirmation}`);
      }
      if (tool.effect.class !== 'read') reasons.push(`effect.class=${tool.effect.class}`);
      if (projection.authority_requirements.length > 0) {
        reasons.push(...projection.authority_requirements.map((requirement) => `authority=${requirement}`));
      }
      return {
        binding_id: binding.binding_id,
        server_name: binding.surface_id,
        tool_name: tool.name,
        decision: reasons.length === 0 ? 'allow' as const : 'prompt' as const,
        reasons,
      };
    }),
  ).sort((left, right) =>
    left.binding_id.localeCompare(right.binding_id)
    || left.tool_name.localeCompare(right.tool_name));
}

function carrierDocument(
  carrierKind: CarrierKind,
  resolved: ResolvedBinding[],
  approvals: ApprovalDecision[],
): Record<string, unknown> {
  const entries = Object.fromEntries(resolved.map((item) => [
    item.binding.surface_id,
    transportDocument(carrierKind, item),
  ]));
  if (carrierKind === 'codex') {
    return { mcp_servers: entries, approvals };
  }
  if (carrierKind === 'kimi') {
    return {
      mcpServers: entries,
      approvals: {
        allow: approvals.filter((entry) => entry.decision === 'allow').map(approvalKey),
        prompt: approvals.filter((entry) => entry.decision === 'prompt').map(approvalKey),
      },
    };
  }
  return { mcp: entries, approvals };
}

function transportDocument(
  carrierKind: CarrierKind,
  { binding, projection }: ResolvedBinding,
): Record<string, unknown> {
  const config = binding.config;
  if (projection.transport.kind === 'streamable_http') {
    if (carrierKind === 'opencode') {
      return { type: 'remote', url: projection.transport.url, headers: config.headers ?? {} };
    }
    return { url: projection.transport.url, headers: config.headers ?? {} };
  }
  const env = filterDeclaredRecord(config.env, projection.transport.env);
  if (carrierKind === 'opencode') {
    return {
      type: 'local',
      command: [projection.transport.command, ...projection.transport.args],
      environment: env,
    };
  }
  return {
    command: projection.transport.command,
    args: projection.transport.args,
    env,
  };
}

function filterDeclaredRecord(value: unknown, declared: string[]): Record<string, string> {
  const source = isRecord(value) ? value : {};
  return Object.fromEntries(
    declared
      .filter((name) => typeof source[name] === 'string')
      .sort()
      .map((name) => [name, String(source[name])]),
  );
}

function approvalKey(entry: ApprovalDecision): string {
  return `${entry.binding_id}/${entry.tool_name}`;
}

function transformAnyOfParentTypes(
  value: unknown,
  toolName: string,
  path: string,
): Record<string, unknown> {
  if (!isRecord(value)) return {};
  const node: Record<string, unknown> = {};
  for (const [key, child] of Object.entries(value)) {
    if (key === 'anyOf' && Array.isArray(child)) {
      node[key] = child.map((branch, index) =>
        transformAnyOfParentTypes(branch, toolName, `${path}.anyOf[${index}]`));
    } else if (key === 'properties' && isRecord(child)) {
      node[key] = Object.fromEntries(Object.entries(child).map(([name, property]) => [
        name,
        transformAnyOfParentTypes(property, toolName, `${path}.properties.${name}`),
      ]));
    } else if (key === 'items' && isRecord(child)) {
      node[key] = transformAnyOfParentTypes(child, toolName, `${path}.items`);
    } else {
      node[key] = child;
    }
  }
  if (Array.isArray(node.anyOf) && node.type !== undefined) {
    const parentType = node.type;
    node.anyOf = node.anyOf.map((branch, index) => {
      if (!isRecord(branch)) return branch;
      if (branch.type !== undefined && JSON.stringify(branch.type) !== JSON.stringify(parentType)) {
        throw new CarrierSchemaCompatibilityError([{
          tool_name: toolName,
          schema_path: `${path}.anyOf[${index}].type`,
          dialect: MOONSHOT_SCHEMA_DIALECT,
          code: 'any_of_parent_type_conflict',
          message: 'anyOf branch type conflicts with its parent type',
          remediation: 'Express each union branch with its exact type in the package-owned schema.',
        }]);
      }
      return { ...branch, type: parentType };
    });
    delete node.type;
  }
  return node;
}

function renderCodexToml(document: Record<string, unknown>): string {
  const servers = isRecord(document.mcp_servers) ? document.mcp_servers : {};
  const lines: string[] = [];
  for (const name of Object.keys(servers).sort()) {
    const server = isRecord(servers[name]) ? servers[name] : {};
    lines.push(`[mcp_servers.${tomlKey(name)}]`);
    if (typeof server.url === 'string') {
      lines.push(`url = ${tomlString(server.url)}`);
    } else {
      lines.push(`command = ${tomlString(String(server.command ?? ''))}`);
      lines.push(`args = ${tomlArray(Array.isArray(server.args) ? server.args.map(String) : [])}`);
    }
    const env = isRecord(server.env) ? server.env : {};
    if (Object.keys(env).length > 0) {
      lines.push(`env = ${tomlInlineTable(env)}`);
    }
    lines.push('');
  }
  return `${lines.join('\n').trimEnd()}\n`;
}

function tomlKey(value: string): string {
  return /^[A-Za-z0-9_-]+$/.test(value) ? value : tomlString(value);
}

function tomlString(value: string): string {
  return JSON.stringify(value);
}

function tomlArray(values: string[]): string {
  return `[${values.map(tomlString).join(', ')}]`;
}

function tomlInlineTable(record: Record<string, unknown>): string {
  return `{ ${Object.keys(record).sort().map((key) =>
    `${tomlKey(key)} = ${tomlString(String(record[key]))}`).join(', ')} }`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function deepFreeze<T>(value: T): T {
  if (value !== null && typeof value === 'object' && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const child of Object.values(value as Record<string, unknown>)) deepFreeze(child);
  }
  return value;
}
