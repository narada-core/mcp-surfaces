import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { gunzipSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';

const executable = fileURLToPath(new URL(
  `../../native/target/release/narada-mcp-registrar${process.platform === 'win32' ? '.exe' : ''}`,
  import.meta.url,
));
const contractPath = fileURLToPath(new URL('../../native/tool-catalog.json.gz', import.meta.url));
const contract = JSON.parse(gunzipSync(readFileSync(contractPath)).toString('utf8'));

assert.equal(contract.schema, 'narada.mcp_registrar.native_tool_catalog.v1');
assert.ok(Array.isArray(contract.tools) && contract.tools.length > 0);
assert.ok(Array.isArray(contract.read_models?.registrar_surface_list?.items));
assert.ok(Array.isArray(contract.read_models?.registrar_carrier_list?.items));
assert.equal(new Set(contract.tools.map((tool: any) => tool.name)).size, contract.tools.length);
assert.equal(
  new Set(contract.read_models.registrar_surface_list.items.map((surface: any) => surface.id)).size,
  contract.read_models.registrar_surface_list.items.length,
);

const client = nativeClient(executable);
try {
  const initialized = await client.request('initialize', { protocolVersion: '2024-11-05' });
  assert.equal(initialized.result.serverInfo.name, 'mcp-registrar');
  const listed = await client.request('tools/list', {});
  assert.deepEqual(listed.result.tools, contract.tools);
  for (const tool of contract.tools) {
    const response = await client.request('tools/call', { name: tool.name, arguments: {} });
    assert.ok(response.result || response.error, `tool did not produce a result or refusal: ${tool.name}`);
    if (response.error) {
      assert.notEqual(response.error.message, `unknown_tool:${tool.name}`, `native dispatcher missing ${tool.name}`);
    } else {
      assert.ok(response.result.structuredContent, `tool omitted structured content: ${tool.name}`);
    }
  }
  for (const name of ['registrar_surface_list', 'registrar_carrier_list', 'registrar_site_list'] as const) {
    const response = await client.request('tools/call', { name, arguments: {} });
    assert.equal(response.error, undefined, response.error?.message);
    assert.ok(Array.isArray(response.result.structuredContent.items));
    assert.equal(response.result.structuredContent.count, response.result.structuredContent.items.length);
  }
  const unknown = await client.request('tools/call', { name: 'registrar_not_real', arguments: {} });
  assert.equal(unknown.error.code, -32000);
  assert.match(unknown.error.message, /unknown|not_real/i);
} finally {
  await client.stop();
}
console.log('mcp-registrar native contract conformance ok');

function nativeClient(command: string) {
  const child = spawn(command, [], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let output = Buffer.alloc(0);
  let stderr = '';
  let nextId = 0;
  child.stdout.on('data', (chunk) => { output = Buffer.concat([output, chunk]); });
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  return {
    async request(method: string, params: unknown) {
      const id = ++nextId;
      const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id, method, params }));
      child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
      child.stdin.write(body);
      const deadline = Date.now() + 15_000;
      while (Date.now() < deadline) {
        const separator = output.indexOf('\r\n\r\n');
        if (separator >= 0) {
          const length = Number(output.subarray(0, separator).toString('ascii').match(/Content-Length:\s*(\d+)/i)?.[1]);
          if (output.length >= separator + 4 + length) {
            const response = JSON.parse(output.subarray(separator + 4, separator + 4 + length).toString('utf8'));
            output = output.subarray(separator + 4 + length);
            if (response.id === id) return response;
          }
        }
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      throw new Error(`native_registrar_timeout:${id}:${stderr}`);
    },
    stop() {
      child.stdin.end();
      return new Promise<void>((resolve) => {
        if (child.exitCode !== null) return resolve();
        child.once('exit', () => resolve());
        setTimeout(() => { child.kill(); resolve(); }, 1_000).unref();
      });
    },
  };
}
