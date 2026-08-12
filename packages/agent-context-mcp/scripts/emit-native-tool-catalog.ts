import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { listAgentContextTools } from '../src/tool-catalog.js';
import { buildGuidanceResult } from '../src/guidance.js';

const output = resolve(import.meta.dirname, '..', 'native', 'tool-catalog.json');
const content = JSON.stringify({
  schema: 'narada.agent_context.native_tool_catalog.v1',
  projections: {
    occupant: listAgentContextTools('occupant'),
    admin: listAgentContextTools('admin'),
  },
  guidance: buildGuidanceResult({}),
}, null, 2) + '\n';
if (!existsSync(output) || readFileSync(output, 'utf8') !== content) {
  writeFileSync(output, content, 'utf8');
}
