import { writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { gzipSync } from 'node:zlib';
import { createServerState, handleRequest, listTools } from '../src/main.js';
import { buildGuidanceResult } from '../src/guidance.js';

async function tool(name: string) {
  const response: any = await handleRequest({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: {} } }, createServerState());
  if (response.error) throw new Error(response.error.message);
  return response.result.structuredContent;
}

const contract = JSON.stringify({
  schema: 'narada.mcp_registrar.native_tool_catalog.v1',
  tools: listTools(),
  guidance: buildGuidanceResult({}),
  read_models: {
    registrar_surface_list: await tool('registrar_surface_list'),
    registrar_carrier_list: await tool('registrar_carrier_list'),
  },
});
writeFileSync(resolve(import.meta.dirname, '..', 'native', 'tool-catalog.json.gz'), gzipSync(Buffer.from(contract), { level: 9 }));
