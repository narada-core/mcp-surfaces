import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import {
  asRecord,
  createTestProcessScope,
  spawnJsonlMcpServer,
} from '@narada-core/mcp-e2e-harness';
import {
  MOONSHOT_SCHEMA_DIALECT,
  validateMoonshotToolInputSchema,
} from '@narada-core/mcp-fabric-compiler/moonshot-schema';

type ServerDefinition = { command: string; args?: string[]; env?: Record<string, string> };

const repositoryRoot = resolve(fileURLToPath(new URL('../../../', import.meta.url)));
const configPath = resolve(process.env.NARADA_KIMI_MCP_CONFIG ?? join(homedir(), '.kimi-code', 'mcp.json'));

test('materialized Kimi tools satisfy the Moonshot schema dialect', { timeout: 120_000 }, async () => {
  const config = JSON.parse(readFileSync(configPath, 'utf8')) as { mcpServers?: Record<string, ServerDefinition> };
  const servers = Object.entries(config.mcpServers ?? {}).sort(([left], [right]) => left.localeCompare(right));
  assert.ok(servers.length > 0, `Kimi config has no MCP servers: ${configPath}`);
  const scope = createTestProcessScope({ label: 'kimi-materialized-schema-contract' });
  const failures: string[] = [];
  let toolCount = 0;
  try {
    for (const [serverName, definition] of servers) {
      const server = spawnJsonlMcpServer(definition.command, definition.args ?? [], {
        cwd: repositoryRoot,
        env: { ...process.env, ...definition.env },
        timeoutMs: 15_000,
        closeTimeoutMs: 1_000,
        scope,
        label: `Kimi schema probe ${serverName}`,
      });
      try {
        const initialized = await server.client.request(1, 'initialize', {
          protocolVersion: '2024-11-05',
          capabilities: {},
          clientInfo: { name: 'kimi-schema-contract', version: '1' },
        });
        if (initialized.error) throw new Error(JSON.stringify(initialized.error));
        const listed = await server.client.request(2, 'tools/list', {});
        if (listed.error) throw new Error(JSON.stringify(listed.error));
        const tools = asRecord(listed.result).tools;
        if (!Array.isArray(tools)) throw new Error('tools/list returned no tools array');
        toolCount += tools.length;
        for (const rawTool of tools) {
          const tool = asRecord(rawTool);
          const toolName = String(tool.name ?? '<unnamed>');
          for (const finding of validateMoonshotToolInputSchema(tool.inputSchema)) {
            failures.push(`${serverName}/${toolName}/${finding.path}: [${finding.code}] ${finding.message}`);
          }
        }
      } catch (error) {
        failures.push(`${serverName}: ${error instanceof Error ? error.message : String(error)}`);
      } finally {
        await server.close();
      }
    }
  } finally {
    await scope.close();
    scope.assertClean();
  }
  assert.ok(toolCount > 0, 'materialized Kimi carrier exposed no tools');
  assert.deepEqual(failures, [], `Kimi schema contract failures (${MOONSHOT_SCHEMA_DIALECT}):\n${failures.slice(0, 100).join('\n')}`);
});
