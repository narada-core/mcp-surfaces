import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runMcpProtocolSmoke, spawnJsonlMcpServer } from '@narada-core/mcp-e2e-harness';

const root = mkdtempSync(join(tmpdir(), 'operator-communication-protocol-'));
const serverPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
const server = spawnJsonlMcpServer(process.execPath, [serverPath, '--site-root', root], { label: 'operator-communication protocol smoke' });
try {
  const protocol = await runMcpProtocolSmoke(server.client, { expectedServerName: 'operator-communication-mcp' });
  assert.deepEqual(protocol.toolNames, ['operator_communication_guidance', 'operator_communication_project']);
} finally {
  await server.close();
  rmSync(root, { recursive: true, force: true });
}
console.log('operator-communication-mcp protocol smoke ok');
