
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { surfaceDefinition } from '../src/surface-definition.js';

const siteRoot = mkdtempSync(join(tmpdir(), 'agent-context-mcp-protocol-'));
mkdirSync(join(siteRoot, '.ai', 'agents'), { recursive: true });
writeFileSync(join(siteRoot, 'AGENTS.md'), '# Fixture Site\n', 'utf8');
writeFileSync(join(siteRoot, '.ai', 'agents', 'roster.json'), JSON.stringify({
  agents: [{ agent_id: 'sonar.architect', role: 'architect', capabilities: [] }],
}, null, 2), 'utf8');

const serverPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
const proc = spawn(process.execPath, [
  serverPath,
  '--site-root', siteRoot,
  '--site-id', 'narada-test',
  '--tool-projection', 'occupant',
], {
  cwd: siteRoot,
  env: {
    ...process.env,
    NARADA_AGENT_ID: 'sonar.architect',
    NARADA_SITE_ROOT: siteRoot,
    NARADA_AGENT_CONTEXT_DB: join(siteRoot, '.ai', 'state', 'agent-context.sqlite'),
  },
  stdio: ['pipe', 'pipe', 'pipe'],
  windowsHide: true,
});

let stdout = '';
let stderr = '';
proc.stdout.setEncoding('utf8');
proc.stderr.setEncoding('utf8');
proc.stdout.on('data', (chunk: any) => { stdout += chunk; });
proc.stderr.on('data', (chunk: any) => { stderr += chunk; });

function writeMessage(message: any) {
  const body = JSON.stringify(message);
  proc.stdin.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\r\n\r\n${body}`);
}

function readOne() {
  const headerEnd = stdout.indexOf('\r\n\r\n');
  if (headerEnd < 0) return null;
  const header = stdout.slice(0, headerEnd);
  const match = header.match(/Content-Length:\s*(\d+)/i);
  if (!match) throw new Error(`bad_header:${header}`);
  const length = Number(match[1]);
  const bodyStart = headerEnd + 4;
  if (stdout.length < bodyStart + length) return null;
  const body = stdout.slice(bodyStart, bodyStart + length);
  stdout = stdout.slice(bodyStart + length);
  return JSON.parse(body);
}

async function waitFor(id: any, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const message = readOne();
    if (message?.id === id) return message;
    await new Promise((resolve: any) => setTimeout(resolve, 20));
  }
  throw new Error(`timeout:${id}; stderr=${stderr}`);
}

try {
  writeMessage({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } });
  const init = await waitFor(1, 30_000);
  assert.equal(init.error, undefined);
  assert.equal(init.result.serverInfo.name, 'narada.test-agent-context-mcp');

  writeMessage({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} });
  const tools = await waitFor(2);
  assert.equal(tools.error, undefined);
  const names = tools.result.tools.map((tool: any) => tool.name);
  assert.deepEqual(names, [
    'agent_orientation_read',
    'mcp_output_show',
  ]);
  assert.equal(names.includes('agent_context_startup_sequence'), false);
  assert.equal(names.includes('agent_context_checkpoint'), false);
  const definition = surfaceDefinition();
  const defaultProjection = definition.descriptor.projections.find(
    (projection) => projection.id === 'default',
  );
  assert.deepEqual(defaultProjection?.exposed_tools, names);
  const descriptorTools = new Map(definition.descriptor.tools.map((tool) => [tool.name, tool]));
  for (const tool of tools.result.tools) {
    const descriptorTool = descriptorTools.get(tool.name);
    assert.ok(descriptorTool, `surface descriptor is missing ${tool.name}`);
    assert.equal(
      tool.annotations?.readOnlyHint,
      descriptorTool.effect.class === 'read',
      `${tool.name} runtime and registrar effect metadata disagree`,
    );
  }
  assert.equal(descriptorTools.get('agent_orientation_acknowledge')?.effect.class, 'local_write');
  assert.equal(names.includes('agent_orientation_acknowledge'), false);
  assert.equal(descriptorTools.get('agent_orientation_read')?.effect.class, 'local_write');

  console.log('agent-context-mcp protocol smoke ok');
} finally {
  proc.stdin?.destroy();
  proc.stdout?.destroy();
  proc.stderr?.destroy();
  await stopChildProcess(proc);
  rmSync(siteRoot, { recursive: true, force: true });
}

function stopChildProcess(child: any) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolveStop: any) => {
    const timeout = setTimeout(resolveStop, 1000);
    child.once('exit', () => {
      clearTimeout(timeout);
      resolveStop();
    });
    child.kill();
  });
}
