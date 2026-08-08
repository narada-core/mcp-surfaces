import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const serverPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
const proc = spawn(process.execPath, [serverPath, '--project-root', 'D:/definitely/missing/narada.space'], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });

let stdout = '';
let stderr = '';
proc.stdout.setEncoding('utf8');
proc.stderr.setEncoding('utf8');
proc.stdout.on('data', (chunk) => { stdout += chunk; });
proc.stderr.on('data', (chunk) => { stderr += chunk; });

function writeMessage(message: Record<string, unknown>) {
  const body = JSON.stringify(message);
  proc.stdin.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\n\n${body}`);
}

function readOne() {
  const crlfHeaderEnd = stdout.indexOf('\r\n\r\n');
  const lfHeaderEnd = stdout.indexOf('\n\n');
  const headerEnd = crlfHeaderEnd >= 0 ? crlfHeaderEnd : lfHeaderEnd;
  if (headerEnd < 0) return null;
  const separatorLength = crlfHeaderEnd >= 0 ? 4 : 2;
  const header = stdout.slice(0, headerEnd);
  const match = /Content-Length:\s*(\d+)/i.exec(header);
  if (!match) throw new Error(`bad_header:${header}`);
  const bodyStart = headerEnd + separatorLength;
  const bodyEnd = bodyStart + Number(match[1]);
  if (stdout.length < bodyEnd) return null;
  const body = stdout.slice(bodyStart, bodyEnd);
  stdout = stdout.slice(bodyEnd);
  return JSON.parse(body) as any;
}

async function waitFor(id: number) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const message = readOne();
    if (message?.id === id) return message;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error(`timeout:${id}; stderr=${stderr}`);
}

try {
  writeMessage({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } });
  const init = await waitFor(1);
  assert.equal(init.error, undefined);
  assert.equal(init.result.serverInfo.name, 'project-state-mcp');
  writeMessage({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} });
  const tools = await waitFor(2);
  assert.equal(tools.error, undefined);
  const names = tools.result.tools.map((tool: { name: string }) => tool.name);
  assert.equal(names.includes('project_state_doctor'), true);
  assert.equal(names.includes('project_state_validate'), true);
  assert.equal(names.includes('project_state_standard_trace'), true);
  assert.equal(names.includes('project_state_standard_gaps'), true);
  assert.equal(names.includes('project_state_handoff'), true);
  assert.equal(names.includes('project_state_guidance'), true);
  assert.equal(stderr.trim(), '');
} finally {
  proc.stdin?.destroy();
  proc.stdout?.destroy();
  proc.stderr?.destroy();
  proc.kill();
}

console.log('project-state-mcp protocol smoke ok');
