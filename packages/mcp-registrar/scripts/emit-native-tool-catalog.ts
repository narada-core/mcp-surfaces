import { writeFileSync } from 'node:fs';
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

const contract = JSON.stringify({
  schema: 'narada.mcp_registrar.native_tool_catalog.v1',
  tools: listTools(),
  guidance: buildGuidanceResult({}),
  read_models: {
    registrar_surface_list: await tool('registrar_surface_list'),
    registrar_carrier_list: await tool('registrar_carrier_list'),
    registrar_carrier_validation_plans: nativeCarrierValidationPlans(),
    registrar_carrier_projection_plans: nativeCarrierProjectionPlans(),
    registrar_site_list_fallback: await fallbackSiteList(),
  },
});
writeFileSync(resolve(import.meta.dirname, '..', 'native', 'tool-catalog.json.gz'), gzipSync(Buffer.from(contract), { level: 9 }));
