import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

type JsonRecord = Record<string, any>;

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const workspaceRoot = resolve(packageRoot, '..', '..');
const executable = join(packageRoot, 'native-dotnet', 'publish', 'narada-filesystem-dotnet.exe');
const fixturePath = join(packageRoot, 'package.json');

function run(requests: JsonRecord[]): Promise<JsonRecord[]> {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(executable, ['--mode', 'read', '--allowed-root', workspaceRoot], {
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    const timer = setTimeout(() => {
      child.kill();
      rejectPromise(new Error(`native_dotnet_filesystem_timeout:${stderr.slice(-1000)}`));
    }, 10_000);
    child.on('error', (error) => {
      clearTimeout(timer);
      rejectPromise(error);
    });
    child.on('close', (code) => {
      clearTimeout(timer);
      if (code !== 0) {
        rejectPromise(new Error(`native_dotnet_filesystem_exit:${code}:${stderr.slice(-1000)}`));
        return;
      }
      try {
        resolvePromise(stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line)));
      } catch (error) {
        rejectPromise(new Error(`native_dotnet_filesystem_invalid_output:${String(error)}:${stdout.slice(0, 1000)}`));
      }
    });
    child.stdin.end(requests.map((request) => JSON.stringify(request)).join('\n') + '\n');
  });
}

const responses = await run([
  { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } },
  { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'fs_read_file', arguments: { path: 'packages/local-filesystem-mcp/package.json', offset: 1, limit: 1 } } },
  { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'fs_read_file', arguments: { path: 'packages/local-filesystem-mcp/package.json', offset: 1, limit: 1001 } } },
]);

const byId = new Map(responses.map((response) => [response.id, response]));
assert.equal(byId.get(1)?.result?.serverInfo?.name, 'local-filesystem-dotnet-native');
const boundedRead = byId.get(2)?.result?.structuredContent;
assert.equal(boundedRead?.schema, 'local.filesystem.read.v1');
assert.equal(boundedRead?.content_sha256, createHash('sha256').update(readFileSync(fixturePath)).digest('hex'));
assert.equal(boundedRead?.content_hash_scope, 'full_file');
assert.equal(boundedRead?.hash_source, 'live_file_bytes');
assert.equal(boundedRead?.cache_used, false);
assert.equal(boundedRead?.limit_adjusted, false);
assert.equal(boundedRead?.pagination_required, true);
assert.match(JSON.stringify(byId.get(3)), /fs_read_file_limit_exceeds_max/);

console.log('native .NET filesystem parity ok');
