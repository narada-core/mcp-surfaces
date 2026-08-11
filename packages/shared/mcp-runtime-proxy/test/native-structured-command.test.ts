import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { requireNativeArtifact } from '../src/native-artifact.js';

type JsonRecord = Record<string, any>;

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const workspaceRoot = resolve(packageRoot, '..', '..', '..');
const executable = resolve(process.env.NARADA_NATIVE_STRUCTURED_COMMAND_TEST_EXECUTABLE ?? requireNativeArtifact(packageRoot, 'narada-mcp-runtime.exe'));

function run(root: string, requests: JsonRecord[], auditLogDir: string): Promise<JsonRecord[]> {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(executable, ['structured-command', '--allowed-root', root, '--allow-command', 'node', '--audit-log-dir', auditLogDir], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    const timer = setTimeout(() => { child.kill(); rejectPromise(new Error(`native_structured_command_timeout:${stderr}`)); }, 15_000);
    child.on('error', (error) => { clearTimeout(timer); rejectPromise(error); });
    child.on('close', (code) => {
      clearTimeout(timer);
      if (code !== 0) { rejectPromise(new Error(`native_structured_command_exit:${code}:${stderr}`)); return; }
      try { resolvePromise(stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))); }
      catch (error) { rejectPromise(new Error(`native_structured_command_invalid_output:${String(error)}:${stdout.slice(0, 1000)}`)); }
    });
    child.stdin.end(requests.map((request) => JSON.stringify(request)).join('\n') + '\n');
  });
}

const root = mkdtempSync(join(tmpdir(), 'narada-native-structured-command-'));
const auditLogDir = join(root, 'audit');
try {
  const modernMeta = {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    "io.modelcontextprotocol/clientInfo": { name: "native-test", version: "1" },
    "io.modelcontextprotocol/clientCapabilities": {}
  };
  const responses = await run(root, [
    { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } },
    { jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} },
    { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'structured_command_execution_policy_inspect', arguments: {} } },
    { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'structured_command_execute', arguments: { command: 'node', args: ['-e', 'process.stdout.write("native-structured")'], working_directory: root, timeout_ms: 5000 } } },
    { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'structured_command_execute', arguments: { command: 'cmd.exe', args: ['/c', 'echo refused'], working_directory: root } } },
    { jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'structured_command_execute', arguments: { command: 'node', args: ['-e', 'setTimeout(() => {}, 1000)'], working_directory: root, timeout_ms: 100 } } },
    { jsonrpc: "2.0", id: 20, method: "server/discover", params: { _meta: modernMeta } },
    { jsonrpc: "2.0", id: 21, method: "tools/list", params: { _meta: modernMeta } },
    { jsonrpc: "2.0", id: 22, method: "tools/call", params: { _meta: modernMeta, name: "structured_command_execution_policy_inspect", arguments: {} } },
    { jsonrpc: "2.0", id: 23, method: "initialize", params: { _meta: modernMeta } },
  ], auditLogDir);
  const byId = new Map(responses.map((response) => [response.id, response]));
  assert.equal(byId.get(1)?.result?.serverInfo?.name, 'structured-command-native');
  assert.deepEqual(byId.get(2)?.result?.tools?.map((tool: JsonRecord) => tool.name), [
    'structured_command_guidance',
    'structured_command_execution_policy_inspect',
    'structured_command_output_show',
    'structured_command_execute',
    'structured_command_start',
    'structured_command_execution_show',
    'structured_command_powershell_parse_check',
    'structured_command_input_create',
    'structured_command_elevated_window_execute',
  ]);
  assert.equal(byId.get(3)?.result?.structuredContent?.schema, 'narada.structured_command.execution_policy.v0');
  assert.equal(byId.get(3)?.result?.structuredContent?.shell_interpolation, false);
  assert.equal(byId.get(4)?.result?.structuredContent?.schema, 'narada.structured_command.execution_result.v0');
  assert.equal(byId.get(4)?.result?.structuredContent?.status, 'ok');
  assert.equal(byId.get(4)?.result?.structuredContent?.stdout, 'native-structured');
  assert.equal(byId.get(5)?.result?.structuredContent?.status, 'refused');
  assert.equal(byId.get(5)?.result?.structuredContent?.decision?.reasons?.some((reason: string) => reason.startsWith('blocked_command:')), true);
  assert.equal(byId.get(6)?.result?.structuredContent?.status, 'timed_out');
  assert.equal(byId.get(20)?.result?.resultType, 'complete');
  assert.equal(byId.get(20)?.result?.supportedVersions?.includes('2026-07-28'), true);
  assert.equal(byId.get(21)?.result?.resultType, 'complete');
  assert.equal(byId.get(21)?.result?.cacheScope, 'public');
  assert.equal(byId.get(22)?.result?.resultType, 'complete');
  assert.equal(byId.get(22)?.result?._meta?.['io.modelcontextprotocol/serverInfo']?.name, 'structured-command-native');
  assert.equal(byId.get(23)?.error?.data?.code, 'initialize_removed');
  assert.match(readFileSync(join(auditLogDir, 'structured-command.jsonl'), 'utf8'), /narada\.structured_command\.execution_result\.v0/);

  const inputResponses = await run(root, [
    { jsonrpc: '2.0', id: 10, method: 'tools/call', params: { name: 'structured_command_input_create', arguments: { input_id: 'inputtest1', command: 'node', args: ['-e', 'process.stdout.write("input-ref-ok")'], working_directory: root } } },
  ], auditLogDir);
  const inputRef = inputResponses[0]?.result?.structuredContent?.input_ref;
  assert.match(String(inputRef), /^structured_command_input:/);
  const persistedResponses = await run(root, [
    { jsonrpc: '2.0', id: 11, method: 'tools/call', params: { name: 'structured_command_execute', arguments: { input_ref: inputRef } } },
  ], auditLogDir);
  const persisted = persistedResponses[0]?.result?.structuredContent;
  assert.equal(persisted?.status, 'ok');
  assert.equal(persisted?.stdout, 'input-ref-ok');
  assert.match(String(persisted?.execution_ref), /^structured_command_execution:/);
  const shownResponses = await run(root, [
    { jsonrpc: '2.0', id: 12, method: 'tools/call', params: { name: 'structured_command_execution_show', arguments: { execution_ref: persisted.execution_ref } } },
  ], auditLogDir);
  assert.equal(shownResponses[0]?.result?.structuredContent?.page_source, 'persisted_execution');
  assert.equal(shownResponses[0]?.result?.structuredContent?.stdout, 'input-ref-ok');

  const largeResponses = await run(root, [
    { jsonrpc: '2.0', id: 13, method: 'tools/call', params: { name: 'structured_command_execute', arguments: { command: 'node', args: ['-e', 'process.stdout.write("x".repeat(6500))'], working_directory: root, stdout_limit: 4000 } } },
  ], auditLogDir);
  const large = largeResponses[0]?.result?.structuredContent;
  assert.equal(large?.schema, 'narada.producer_output_page.v1');
  assert.match(String(large?.output_ref), /^mcp_output:/);
  const outputResponses = await run(root, [
    { jsonrpc: '2.0', id: 14, method: 'tools/call', params: { name: 'structured_command_output_show', arguments: { ref: large.output_ref, offset: 0, limit: 120 } } },
  ], auditLogDir);
  assert.equal(outputResponses[0]?.result?.structuredContent?.schema, 'narada.mcp_output_page.v1');
  assert.equal(outputResponses[0]?.result?.structuredContent?.ref, large.output_ref);

  const ps1Path = join(root, 'parse-check.ps1');
  writeFileSync(ps1Path, 'Write-Output "ok"\n', 'utf8');
  const parseResponses = await run(root, [
    { jsonrpc: '2.0', id: 15, method: 'tools/call', params: { name: 'structured_command_powershell_parse_check', arguments: { path: ps1Path, working_directory: root } } },
  ], auditLogDir);
  assert.equal(parseResponses[0]?.result?.structuredContent?.arbitrary_command_execution_admitted, false);

  const elevatedResponses = await run(root, [
    { jsonrpc: '2.0', id: 16, method: 'tools/call', params: { name: 'structured_command_elevated_window_execute', arguments: { command: 'pwsh.exe', args: ['-NoProfile', '-File', ps1Path], working_directory: root, dry_run: true } } },
  ], auditLogDir);
  assert.equal(elevatedResponses[0]?.result?.structuredContent?.status, 'planned');
  assert.equal(elevatedResponses[0]?.result?.structuredContent?.executed, false);

  const cancelChild = spawn(executable, ['structured-command', '--allowed-root', root, '--allow-command', 'node'], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let cancelOutput = '';
  let cancelError = '';
  cancelChild.stdout.setEncoding('utf8');
  cancelChild.stderr.setEncoding('utf8');
  cancelChild.stdout.on('data', (chunk) => { cancelOutput += chunk; });
  cancelChild.stderr.on('data', (chunk) => { cancelError += chunk; });
  cancelChild.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: 'cancel-me', method: 'tools/call', params: { name: 'structured_command_execute', arguments: { command: 'node', args: ['-e', 'setTimeout(() => {}, 10000)'], working_directory: root, timeout_ms: 10_000 } } }) + '\n');
  await new Promise((resolvePromise) => setTimeout(resolvePromise, 100));
  cancelChild.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 'cancel-me' } }) + '\n');
  cancelChild.stdin.end();
  await new Promise<void>((resolvePromise, rejectPromise) => {
    const timer = setTimeout(() => { cancelChild.kill(); rejectPromise(new Error('native_structured_command_cancel_timeout:' + cancelError)); }, 15_000);
    cancelChild.on('close', (code) => { clearTimeout(timer); if (code !== 0) rejectPromise(new Error('native_structured_command_cancel_exit:' + code + ':' + cancelError)); else resolvePromise(); });
  });
  const cancelResponse = cancelOutput.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line)).find((response) => response.id === 'cancel-me');
  assert.equal(cancelResponse?.result?.structuredContent?.status, 'cancelled');
  assert.equal(cancelResponse?.result?.structuredContent?.cancelled, true);
} finally {
  rmSync(root, { recursive: true, force: true });
}
