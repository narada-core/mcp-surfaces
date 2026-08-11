import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { listAgentContextTools, type AgentContextToolProjection } from '../src/tool-catalog.js';

const executable = fileURLToPath(new URL(
  `../../native/target/release/narada-agent-context-mcp${process.platform === 'win32' ? '.exe' : ''}`,
  import.meta.url,
));

for (const projection of ['occupant', 'admin'] as const) {
  const actual = await requestNativeTools(projection);
  assert.deepEqual(actual, listAgentContextTools(projection), `${projection} native tool contract drifted from TypeScript`);
}

console.log('agent-context native tool catalog parity ok');

async function requestNativeTools(projection: AgentContextToolProjection) {
  const child = spawn(executable, ['--tool-projection', projection], {
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
  });
  const request = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list', params: {} }));
  child.stdin.end(`Content-Length: ${request.length}\r\n\r\n${request}`);
  const stdout: Buffer[] = [];
  const stderr: Buffer[] = [];
  child.stdout.on('data', (chunk) => stdout.push(chunk));
  child.stderr.on('data', (chunk) => stderr.push(chunk));
  const exitCode = await new Promise<number | null>((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', resolve);
  });
  assert.equal(exitCode, 0, Buffer.concat(stderr).toString('utf8'));
  const response = parseMessage(Buffer.concat(stdout));
  assert.equal(response.error, undefined);
  return response.result.tools;
}

function parseMessage(message: Buffer) {
  const separator = message.indexOf('\r\n\r\n');
  assert.notEqual(separator, -1, 'native response must use Content-Length framing');
  const header = message.subarray(0, separator).toString('ascii');
  const length = Number(header.match(/Content-Length:\s*(\d+)/i)?.[1]);
  assert.equal(Number.isFinite(length), true, 'native response must declare Content-Length');
  const body = message.subarray(separator + 4);
  assert.equal(body.length, length, 'native response length must match its header');
  return JSON.parse(body.toString('utf8'));
}
