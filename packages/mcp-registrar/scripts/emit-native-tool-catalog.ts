import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { gzipSync } from 'node:zlib';
import { createServerState, handleRequest, listTools, nativeCarrierProjectionPlans, nativeCarrierValidationPlans } from '../src/main.js';
import { buildGuidanceResult } from '../src/guidance.js';

async function tool(name: string) {
  const response: any = await handleRequest({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: {} } }, createServerState());
  if (response.error) throw new Error(response.error.message);
  return response.result.structuredContent;
}

async function fallbackSiteList() {
  const previous = process.env.NARADA_SITE_REGISTRY_DB;
  process.env.NARADA_SITE_REGISTRY_DB = resolve(import.meta.dirname, '..', 'native', '.missing-site-registry.db');
  try {
    return await tool('registrar_site_list');
  } finally {
    if (previous === undefined) delete process.env.NARADA_SITE_REGISTRY_DB;
    else process.env.NARADA_SITE_REGISTRY_DB = previous;
  }
}

const registrarEntrypointSentinel = 'narada-mcp-registrar';
const projectionPlans = nativeCarrierProjectionPlans();
for (const carrier of Object.values(projectionPlans)) {
  if (!carrier || typeof carrier !== 'object' || Array.isArray(carrier)) continue;
  const recovery = (carrier as Record<string, unknown>).recovery_unbind;
  if (!recovery || typeof recovery !== 'object' || Array.isArray(recovery)) continue;
  for (const template of Object.values(recovery as Record<string, unknown>)) {
    if (!template || typeof template !== 'object' || Array.isArray(template)) continue;
    const generation = (template as Record<string, unknown>).generation_unsigned;
    if (!generation || typeof generation !== 'object' || Array.isArray(generation)) continue;
    const record = generation as Record<string, unknown>;
    record.artifact_manifest_fingerprint = 'runtime-artifact-manifest';
    for (const field of ['config_artifact', 'managed_projection']) {
      const descriptor = record[field];
      if (descriptor && typeof descriptor === 'object' && !Array.isArray(descriptor)) {
        const digestKey = field === 'config_artifact' ? 'bytes_sha256' : 'sha256';
        (descriptor as Record<string, unknown>)[digestKey] = `runtime-${field}`;
      }
    }
  }
}
const contract = JSON.stringify({
  schema: 'narada.mcp_registrar.native_tool_catalog.v1',
  tools: listTools(),
  guidance: buildGuidanceResult({}),
  runtime_bindings: {
    // Native startup replaces this stable logical identity with the current
    // immutable artifact path. Embedding that path here creates a hash loop:
    // catalog -> binary fingerprint -> version path -> catalog.
    registrar_entrypoint: registrarEntrypointSentinel,
  },
  read_models: {
    registrar_surface_list: await tool('registrar_surface_list'),
    registrar_carrier_list: await tool('registrar_carrier_list'),
    registrar_carrier_validation_plans: nativeCarrierValidationPlans(),
    registrar_carrier_projection_plans: projectionPlans,
    registrar_site_list_fallback: await fallbackSiteList(),
  },
}, (_key, value) => typeof value === 'string'
  ? value.replace(
      /(?:[A-Za-z]:)?[^"\r\n]*?[\\/]mcp-registrar[\\/]dist[\\/]native[\\/]versions[\\/][^\\/"\r\n]+[\\/]narada-mcp-registrar(?:\.exe)?/gi,
      registrarEntrypointSentinel,
    )
  : value);
const output = resolve(import.meta.dirname, '..', 'native', 'tool-catalog.json.gz');
const content = gzipSync(Buffer.from(contract), { level: 9 });
if (!existsSync(output) || !readFileSync(output).equals(content)) {
  writeFileSync(output, content);
}
