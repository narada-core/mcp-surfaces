import { writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { listTools } from '../src/main.js';
import { buildGuidanceResult } from '../src/guidance.js';

writeFileSync(resolve(import.meta.dirname, '..', 'native', 'tool-catalog.json'), JSON.stringify({
  schema: 'narada.mcp_registrar.native_tool_catalog.v1',
  tools: listTools(),
  guidance: buildGuidanceResult({}),
}, null, 2) + '\n', 'utf8');
