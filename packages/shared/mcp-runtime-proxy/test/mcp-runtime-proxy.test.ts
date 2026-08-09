import assert from 'node:assert/strict';

import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { effectiveRequestTimeoutMs } from '../src/main.js';
import { createTestProcessScope } from '@narada-core/mcp-e2e-harness';
import {
  captureRuntimeFreshness,
  classifyRuntimeInstance,
  evaluateRuntimeFreshness,
  listRuntimeInstances,
  runtimeInstancePath,
  writeRuntimeInstance,
  type RuntimeInstanceRecord,
} from '../src/runtime-lifecycle.js';
import { fingerprintWorkspaceArtifactManifest } from '../src/workspace-artifact-manifest.js';
import { MCP_RUNTIME_CONTRACT_VERSION } from '../src/materialization-contract.js';

const root = mkdtempSync(join(tmpdir(), 'mcp-runtime-proxy-'));
const processScope = createTestProcessScope({ label: 'mcp-runtime-proxy-test' });

assert.equal(effectiveRequestTimeoutMs(100, null, 60000), 100);
assert.equal(effectiveRequestTimeoutMs(100, 300, 1000), 1300);
assert.equal(effectiveRequestTimeoutMs(100, 900000, 60000), 960000);

const freshnessPackageRoot = join(root, 'freshness-package');
const freshnessRuntimeRoot = join(freshnessPackageRoot, 'dist', 'src');
const freshnessSourceRoot = join(freshnessPackageRoot, 'src');
mkdirSync(freshnessRuntimeRoot, { recursive: true });
mkdirSync(freshnessSourceRoot, { recursive: true });
const freshnessProxyRuntime = join(freshnessRuntimeRoot, 'proxy.js');
const freshnessChildRuntime = join(freshnessRuntimeRoot, 'child.js');
writeFileSync(freshnessProxyRuntime, 'export {};\n', 'utf8');
writeFileSync(freshnessChildRuntime, 'export {};\n', 'utf8');
const freshnessTracker = captureRuntimeFreshness({
  proxyRuntimePath: freshnessProxyRuntime,
  childEntrypoint: freshnessChildRuntime,
});
assert.equal(evaluateRuntimeFreshness({
  tracker: freshnessTracker,
  surfaceId: 'freshness-test',
}).status, 'current');
utimesSync(freshnessChildRuntime, new Date(0), new Date(0));
assert.equal(evaluateRuntimeFreshness({
  tracker: freshnessTracker,
  surfaceId: 'freshness-test',
}).status, 'current');
await new Promise((resolve) => setTimeout(resolve, 20));
writeFileSync(join(freshnessSourceRoot, 'child.ts'), 'export const changed = true;\n', 'utf8');
const staleFreshness = evaluateRuntimeFreshness({
  tracker: freshnessTracker,
  surfaceId: 'freshness-test',
});
assert.equal(staleFreshness.status, 'stale');
assert.ok((staleFreshness.reasons as Array<Record<string, unknown>>).some((reason) => reason.code === 'source_newer_than_runtime_build'));
assert.equal((staleFreshness.reload_action as Record<string, unknown>).operation, 'restart');

const manifestForFreshnessPath = join(root, 'freshness-manifest.json');
const manifestForFreshness = {
  schema: 'narada.workspace_artifact_manifest.v1',
  generated_at: '2026-01-01T00:00:00.000Z',
  workspace_root: root,
  packages: [],
  artifacts: [],
};
const rebuiltAt = new Date(statSync(join(freshnessSourceRoot, 'child.ts')).mtimeMs + 2_000);
utimesSync(freshnessChildRuntime, rebuiltAt, rebuiltAt);
writeFileSync(manifestForFreshnessPath, `${JSON.stringify({
  ...manifestForFreshness,
  manifest_fingerprint: fingerprintWorkspaceArtifactManifest(manifestForFreshness),
}, null, 2)}\n`, 'utf8');
const manifestFreshnessTracker = captureRuntimeFreshness({
  proxyRuntimePath: freshnessProxyRuntime,
  childEntrypoint: freshnessChildRuntime,
  artifactManifestPath: manifestForFreshnessPath,
});
writeFileSync(manifestForFreshnessPath, `${JSON.stringify({
  ...manifestForFreshness,
  generated_at: '2026-01-01T00:00:00.000Z-extra-metadata',
  manifest_fingerprint: fingerprintWorkspaceArtifactManifest(manifestForFreshness),
}, null, 2)}\n`, 'utf8');
assert.equal(evaluateRuntimeFreshness({
  tracker: manifestFreshnessTracker,
  surfaceId: 'freshness-manifest-test',
}).status, 'current');
const unknownFreshness = evaluateRuntimeFreshness({
  tracker: {
    ...freshnessTracker,
    source_files: [],
    proxy_runtime: {
      path: join(root, 'missing-proxy-runtime.js'),
      exists: false,
      mtime_ms: null,
      size: null,
      sha256: null,
    },
  },
  surfaceId: 'freshness-test',
});
assert.equal(unknownFreshness.status, 'unknown');

const instanceRoot = join(root, 'instance-registry');
const now = new Date();
const liveInstance: RuntimeInstanceRecord = {
  schema: 'narada.mcp_runtime_proxy.instance.v1',
  surface_id: 'live-surface',
  proxy_pid: process.pid,
  parent_pid: process.ppid,
  child_pid: null,
  entrypoint: freshnessChildRuntime,
  started_at: now.toISOString(),
  heartbeat_at: now.toISOString(),
  lease_expires_at: new Date(now.getTime() + 10_000).toISOString(),
  state: 'live',
  liveness_evidence: { parent_pid_alive: true },
};
writeRuntimeInstance(runtimeInstancePath(instanceRoot, process.pid), liveInstance);
assert.equal(classifyRuntimeInstance(liveInstance, { isPidAlive: () => true }).observed_state, 'live');
assert.equal(classifyRuntimeInstance(liveInstance, { isPidAlive: (pid) => pid === process.pid }).observed_state, 'stale');
const deadChildInstance = { ...liveInstance, child_pid: 424242 };
assert.ok(classifyRuntimeInstance(deadChildInstance, { isPidAlive: (pid) => pid !== 424242 }).stale_reasons.includes('child_pid_not_alive'));
const instanceListing = listRuntimeInstances(instanceRoot, { isPidAlive: () => true });
assert.equal((instanceListing.counts as Record<string, unknown>).live, 1);

const artifactManifestPath = join(root, 'artifact-manifest.json');
const proxyEntrypoint = fileURLToPath(new URL('../src/main.js', import.meta.url));
const testEntrypoints = new Set<string>();
function registerArtifact(entrypoint: string): void {
  testEntrypoints.add(entrypoint);
  const unsigned = {
    schema: 'narada.workspace_artifact_manifest.v1',
    generated_at: '2026-01-01T00:00:00.000Z',
    workspace_root: root,
    packages: [],
    artifacts: [...testEntrypoints].sort().map((path) => {
      const stat = statSync(path);
      return {
        path,
        sha256: createHash('sha256').update(readFileSync(path)).digest('hex'),
        size: stat.size,
        mtime_ms: stat.mtimeMs,
      };
    }),
  };
  const manifest = {
    ...unsigned,
    manifest_fingerprint: fingerprintWorkspaceArtifactManifest(unsigned),
  };
  writeFileSync(artifactManifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
}

function spawnProxy(args: string[], options: Parameters<typeof processScope.spawn>[2]): ReturnType<typeof processScope.spawn> {
  const entrypointIndex = args.indexOf('--entrypoint');
  const entrypoint = entrypointIndex >= 0 ? args[entrypointIndex + 1] : null;
  if (!entrypoint) throw new Error('test_proxy_entrypoint_required');
  registerArtifact(entrypoint);
  return processScope.spawn(process.execPath, [
    proxyEntrypoint,
    '--artifact-manifest',
    artifactManifestPath,
    '--runtime-contract-version', String(MCP_RUNTIME_CONTRACT_VERSION),
    '--child-command', process.execPath,
    ...args,
  ], options);
}

async function waitForOutput(condition: () => boolean, timeoutMs: number): Promise<void> {
  const started = Date.now();
  while (!condition()) {
    if (Date.now() - started > timeoutMs) throw new Error('wait_for_output_timeout');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

try {
  const refusedEntrypoint = join(root, 'refused-child.mjs');
  const refusedMarker = join(root, 'refused-child.started');
  writeFileSync(refusedEntrypoint, `import { writeFileSync } from 'node:fs'; writeFileSync(${JSON.stringify(refusedMarker)}, 'started'); process.stdin.resume();\n`, 'utf8');
  const refusedProxy = spawn(process.execPath, [
    proxyEntrypoint,
    '--surface-id',
    'manifest-required-surface',
    '--entrypoint',
    refusedEntrypoint,
    '--',
  ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let refusedStdout = '';
  refusedProxy.stdout.setEncoding('utf8');
  refusedProxy.stdout.on('data', (chunk) => { refusedStdout += chunk; });
  refusedProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 'manifest-required', method: 'initialize', params: {} })}\n`);
  refusedProxy.stdin.end();
  const refusedExitCode = await new Promise<number | null>((resolve) => refusedProxy.on('close', resolve));
  assert.notEqual(refusedExitCode, 0);
  assert.equal(existsSync(refusedMarker), false);
  const refusedResponse = JSON.parse(refusedStdout.trim());
  assert.equal(refusedResponse.error.data.code, 'workspace_manifest_missing');

  const childEntrypoint = join(root, 'failing-child.mjs');
  writeFileSync(childEntrypoint, "process.stderr.write('import failed: missing shared dist\\n'); process.exit(42);\n", 'utf8');
  for (let attempt = 0; attempt < 5; attempt += 1) {
    const child = spawnProxy([
      '--surface-id',
      'test-surface',
      '--entrypoint',
      childEntrypoint,
      '--',
      '--site-root',
      root,
    ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });

    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });

    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: attempt + 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } })}\n`);
    child.stdin.end();

    const exitCode = await new Promise<number | null>((resolve) => child.on('close', resolve));
    assert.equal(exitCode, 42, `proxy stderr:\n${stderr}`);
    assert.match(stderr, /missing shared dist/);

    const response = JSON.parse(stdout.trim());
    assert.equal(response.id, attempt + 1);
    assert.equal(response.error.data.schema, 'narada.mcp_runtime_proxy.error.v1');
    assert.equal(response.error.data.code, 'child_exited_before_response');
    assert.equal(response.error.data.surface_id, 'test-surface');
    assert.equal(response.error.data.exit_code, 42);
    assert.match(response.error.data.stderr_tail, /missing shared dist/);
  }

  const silentEntrypoint = join(root, 'silent-child.mjs');
  writeFileSync(silentEntrypoint, "process.stdin.resume(); setInterval(() => {}, 1000);\n", 'utf8');
  const diagnosticsDir = join(root, 'diagnostics');
  const silentProxy = spawnProxy([
    '--surface-id',
    'silent-surface',
    '--entrypoint',
    silentEntrypoint,
    '--request-timeout-ms',
    '100',
    '--tool-timeout-grace-ms',
    '1000',
    '--diagnostics-dir',
    diagnosticsDir,
    '--',
  ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });

  let silentStdout = '';
  silentProxy.stdout.setEncoding('utf8');
  silentProxy.stdout.on('data', (chunk) => { silentStdout += chunk; });
  silentProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 'slow-1', method: 'tools/call', params: { name: 'slow_read', arguments: { timeout_ms: 5 } } })}\n`);

  const silentExitCode = await new Promise<number | null>((resolve) => silentProxy.on('close', resolve));
  assert.notEqual(silentExitCode, 0);
  const timeoutResponse = JSON.parse(silentStdout.trim());
  assert.equal(timeoutResponse.id, 'slow-1');
  assert.equal(timeoutResponse.error.data.schema, 'narada.mcp_runtime_proxy.error.v1');
  assert.equal(timeoutResponse.error.data.code, 'child_request_timeout');
  assert.equal(timeoutResponse.error.data.method, 'tools/call');
  assert.equal(timeoutResponse.error.data.surface_id, 'silent-surface');
  assert.equal(timeoutResponse.error.data.timeout_layer, 'mcp_runtime_proxy_watchdog');
  assert.equal(timeoutResponse.error.data.proxy_request_timeout_ms, 100);
  assert.equal(timeoutResponse.error.data.effective_request_timeout_ms, 100);
  assert.equal(timeoutResponse.error.data.requested_transport_timeout_ms, null);
  assert.equal(timeoutResponse.error.data.surface_timeout_expected_before_proxy, false);
  assert.equal(timeoutResponse.error.data.kill_grace_ms, 5000);
  assert.equal(typeof timeoutResponse.error.data.forensic_artifact_path, 'string');
  assert.match(timeoutResponse.error.data.forensic_artifact_path, /silent-surface/);
  const artifacts = readdirSync(diagnosticsDir).filter((file) => file.endsWith('.json') && !file.startsWith('startup-') && !file.startsWith('instance-') && !file.startsWith('supervisor-'));
  assert.equal(artifacts.length, 1);
  const startupTrace = JSON.parse(readFileSync(join(diagnosticsDir, 'startup-silent-surface.json'), 'utf8'));
  assert.equal(startupTrace.schema, 'narada.mcp_runtime_proxy.startup_trace.v1');
  assert.equal(startupTrace.surface_id, 'silent-surface');
  assert.equal(startupTrace.completed, false);
  assert.ok(startupTrace.events.some((event: { event: string }) => event.event === 'proxy_started'));
  assert.ok(startupTrace.events.some((event: { event: string }) => event.event === 'child_closed_before_tools_list'));
  const artifact = JSON.parse(readFileSync(join(diagnosticsDir, artifacts[0]), 'utf8'));
  assert.equal(artifact.schema, 'narada.mcp_runtime_proxy.forensic_artifact.v1');
  assert.equal(artifact.event, 'proxy_child_request_timeout');
  assert.equal(artifact.surface.surface_id, 'silent-surface');
  assert.equal(artifact.request.id, 'slow-1');
  assert.equal(artifact.request.method, 'tools/call');
  assert.equal(artifact.request.tool_name, 'slow_read');
  assert.equal(artifact.request.requested_transport_timeout_ms, null);
  assert.equal(typeof artifact.request.args_hash, 'string');
  assert.equal(artifact.request.args_summary.timeout_ms, 5);
  assert.equal(artifact.pending_requests.length, 0);
  assert.equal(artifact.proxy.request_timeout_ms, 100);
  assert.equal(artifact.proxy.tool_timeout_grace_ms, 1000);
  assert.equal(artifact.child_process.entrypoint, silentEntrypoint);
  assert.equal(typeof artifact.child_process.entrypoint_sha256, 'string');
  assert.equal(artifact.diagnostic.code, 'child_request_timeout');
  assert.ok(artifact.request.lifecycle.some((event: Record<string, unknown>) => event.event === 'proxy_timeout'));
  assert.ok(artifact.request.lifecycle.some((event: Record<string, unknown>) => event.event === 'child_termination_requested'));

  const statusChildEntrypoint = join(root, 'status-child.mjs');
  writeFileSync(statusChildEntrypoint, [
    "let buffer = '';",
    "process.stdin.setEncoding('utf8');",
    "process.stdin.on('data', (chunk) => {",
    "  buffer += chunk;",
    "  let index;",
    "  while ((index = buffer.indexOf('\\n')) >= 0) {",
    "    const line = buffer.slice(0, index);",
    "    buffer = buffer.slice(index + 1);",
    "    if (!line.trim()) continue;",
    "    const request = JSON.parse(line);",
    "    if (request.method === 'tools/list') process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { tools: [] } }) + '\\n');",
    "  }",
    "});",
  ].join('\n'), 'utf8');
  const statusDiagnosticsDir = join(root, 'status-diagnostics');
  const statusProxy = spawnProxy([
    '--surface-id', 'status-surface',
    '--entrypoint', statusChildEntrypoint,
    '--diagnostics-dir', statusDiagnosticsDir,
    '--liveness-check-ms', '20',
    '--orphan-grace-ms', '25',
    '--',
  ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let statusStdout = '';
  statusProxy.stdout.setEncoding('utf8');
  statusProxy.stdout.on('data', (chunk) => { statusStdout += chunk; });
  statusProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 'tools', method: 'tools/list', params: {} })}\n`);
  await waitForOutput(() => statusStdout.includes('mcp_runtime_proxy_status'), 2_000);
  const toolsResponse = JSON.parse(statusStdout.trim().split(/\r?\n/)[0]);
  assert.ok(toolsResponse.result.tools.some((tool: Record<string, unknown>) => tool.name === 'mcp_runtime_proxy_status'));
  statusStdout = '';
  statusProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 'status', method: 'tools/call', params: { name: 'mcp_runtime_proxy_status', arguments: {} } })}\n`);
  await waitForOutput(() => statusStdout.includes('"id":"status"'), 2_000);
  const statusResponse = JSON.parse(statusStdout.trim());
  assert.equal(statusResponse.result.structuredContent.runtime_freshness.schema, 'narada.mcp_runtime_proxy.runtime_freshness.v2');
  assert.equal(statusResponse.result.structuredContent.runtime_freshness.reload_action.kind, 'restart_carrier_bound_surface');
  assert.equal(statusResponse.result.structuredContent.liveness.observed_state, 'live');
  assert.equal(statusResponse.result.structuredContent.liveness.supervisor_pid > 0, true);
  assert.equal(statusResponse.result.structuredContent.liveness.server_pid > 0, true);
  statusProxy.stdin.end();
  await new Promise<number | null>((resolve) => statusProxy.on('close', resolve));
  const reclaimedListing = listRuntimeInstances(statusDiagnosticsDir, { isPidAlive: () => false });
  assert.equal((reclaimedListing.counts as Record<string, unknown>).reclaimed, 1);

  // Regression for sfb_36762540-087: a tool that declares timeout_ms beyond
  // the proxy watchdog must not get the shared child SIGTERMed. The watchdog
  // honors the declared timeout plus a process-startup/transport grace margin,
  // the slow response arrives, and the transport stays usable for the next call.
  const honoredChildEntrypoint = join(root, 'honored-child.mjs');
  writeFileSync(honoredChildEntrypoint, [
    "let buffer = '';",
    "process.stdin.setEncoding('utf8');",
    "process.stdin.on('data', (chunk) => {",
    '  buffer += chunk;',
    '  let index;',
    "  while ((index = buffer.indexOf('\\n')) >= 0) {",
    '    const line = buffer.slice(0, index);',
    '    buffer = buffer.slice(index + 1);',
    '    if (!line.trim()) continue;',
    '    const request = JSON.parse(line);',
    '    setTimeout(() => {',
    "      process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { content: [{ type: 'text', text: 'slow-ok-' + request.id }] } }) + '\\n');",
    '    }, 150);',
    '  }',
    '});',
  ].join('\n'), 'utf8');
  const honoredProxy = spawnProxy([
    '--surface-id',
    'honored-surface',
    '--entrypoint',
    honoredChildEntrypoint,
    '--request-timeout-ms',
    '100',
    '--tool-timeout-grace-ms',
    '50',
    '--',
  ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let honoredStdout = '';
  honoredProxy.stdout.setEncoding('utf8');
  honoredProxy.stdout.on('data', (chunk) => { honoredStdout += chunk; });
  // Warm the child process independently. Otherwise Windows process startup
  // latency is charged to the first tool call and can falsify this watchdog
  // contract even though the tool's own 150ms latency is within its budget.
  honoredProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 'honored-warmup', method: 'ping', params: { _meta: { narada_request_timeout_ms: 2_000 } } })}\n`);
  await waitForOutput(() => honoredStdout.includes('"honored-warmup"'), 5_000);
  assert.equal(JSON.parse(honoredStdout.trim()).result?.content?.[0]?.text, 'slow-ok-honored-warmup');
  honoredStdout = '';
  honoredProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 'honored-1', method: 'tools/call', params: { name: 'slow_tool', arguments: {}, _meta: { narada_request_timeout_ms: 300 } } })}\n`);
  await waitForOutput(() => honoredStdout.includes('"honored-1"'), 2000);
  const honoredResponse = JSON.parse(honoredStdout.trim());
  assert.equal(honoredResponse.id, 'honored-1');
  assert.equal(honoredResponse.result?.content?.[0]?.text, 'slow-ok-honored-1', JSON.stringify(honoredResponse));
  honoredStdout = '';
  honoredProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 'honored-2', method: 'tools/call', params: { name: 'slow_tool', arguments: {}, _meta: { narada_request_timeout_ms: 300 } } })}\n`);
  await waitForOutput(() => honoredStdout.includes('"honored-2"'), 2000);
  assert.equal(JSON.parse(honoredStdout.trim()).result.content[0].text, 'slow-ok-honored-2');
  honoredProxy.kill();
  await new Promise<number | null>((resolve) => honoredProxy.on('close', resolve));

  // A child that never responds is still terminated after the honored tool
  // timeout plus grace: the watchdog remains the hung-child guard.
  const honoredSilentProxy = spawnProxy([
    '--surface-id',
    'honored-silent-surface',
    '--entrypoint',
    silentEntrypoint,
    '--request-timeout-ms',
    '100',
    '--tool-timeout-grace-ms',
    '50',
    '--',
  ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let honoredSilentStdout = '';
  honoredSilentProxy.stdout.setEncoding('utf8');
  honoredSilentProxy.stdout.on('data', (chunk) => { honoredSilentStdout += chunk; });
  honoredSilentProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 'honored-slow-1', method: 'tools/call', params: { name: 'slow_read', arguments: {}, _meta: { narada_request_timeout_ms: 200 } } })}\n`);
  const honoredSilentExitCode = await new Promise<number | null>((resolve) => honoredSilentProxy.on('close', resolve));
  assert.notEqual(honoredSilentExitCode, 0);
  const honoredSilentResponse = JSON.parse(honoredSilentStdout.trim());
  assert.equal(honoredSilentResponse.id, 'honored-slow-1');
  assert.equal(honoredSilentResponse.error.data.code, 'child_request_timeout');
  assert.equal(honoredSilentResponse.error.data.proxy_request_timeout_ms, 100);
  assert.equal(honoredSilentResponse.error.data.requested_transport_timeout_ms, 200);
  assert.equal(honoredSilentResponse.error.data.effective_request_timeout_ms, 250);
  assert.equal(honoredSilentResponse.error.data.surface_timeout_expected_before_proxy, true);

  const contentLengthChildEntrypoint = join(root, 'json-line-content-length-child.mjs');
  writeFileSync(contentLengthChildEntrypoint, "process.stdin.setEncoding('utf8'); process.stdin.on('data', (chunk) => { const request = JSON.parse(chunk.trim()); process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { content: [{ type: 'text', text: 'Content-Length: is data, not framing' }] } }) + String.fromCharCode(10)); });", 'utf8');
  const contentLengthProxy = spawnProxy([
    '--surface-id',
    'json-line-content-length-surface',
    '--entrypoint',
    contentLengthChildEntrypoint,
    '--',
  ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let contentLengthStdout = '';
  contentLengthProxy.stdout.setEncoding('utf8');
  const contentLengthResponse = new Promise<{ result: { content: Array<{ text: string }> } }>((resolve, reject) => {
    contentLengthProxy.stdout.on('data', (chunk) => {
      contentLengthStdout += chunk;
      if (contentLengthStdout.includes('Content-Length: is data')) {
        resolve(JSON.parse(contentLengthStdout.trim()) as { result: { content: Array<{ text: string }> } });
        contentLengthProxy.kill();
      }
    });
    contentLengthProxy.once('error', reject);
  });
  contentLengthProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'content_length_probe', arguments: {} } })}\n`);
  const contentLengthResponseValue = await contentLengthResponse;
  assert.equal(contentLengthResponseValue.result.content[0].text, 'Content-Length: is data, not framing');

  const framedChildEntrypoint = join(root, 'framed-child.mjs');
  writeFileSync(framedChildEntrypoint, "process.stdin.setEncoding('utf8'); process.stdin.once('data', (chunk) => { const request = JSON.parse(chunk.trim()); const body = JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { protocolVersion: '2024-11-05', capabilities: {}, serverInfo: { name: 'framed-child', version: '1' } } }); process.stdout.write('Content-Length: ' + Buffer.byteLength(body, 'utf8') + '\\r\\n\\r\\n' + body); setTimeout(() => process.exit(0), 20); });\n", 'utf8');
  const framedProxy = spawnProxy([
    '--surface-id',
    'framed-surface',
    '--entrypoint',
    framedChildEntrypoint,
    '--',
  ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let framedStdout = '';
  framedProxy.stdout.setEncoding('utf8');
  framedProxy.stdout.on('data', (chunk) => { framedStdout += chunk; });
  framedProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 'init-1', method: 'initialize', params: { protocolVersion: '2024-11-05' } })}\n`);
  framedProxy.stdin.end();
  await new Promise<number | null>((resolve) => framedProxy.on('close', resolve));
  assert.doesNotMatch(framedStdout, /Content-Length:/i);
  assert.equal(JSON.parse(framedStdout.trim()).id, 'init-1');

  const normalizedChildEntrypoint = join(root, 'normalized-child.mjs');
  writeFileSync(normalizedChildEntrypoint, "process.stdin.setEncoding('utf8'); process.stdin.once('data', (chunk) => { if (/Content-Length:/i.test(chunk)) process.exit(41); const request = JSON.parse(chunk.trim()); process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: {} }) + '\\n'); setTimeout(() => process.exit(0), 20); });\n", 'utf8');
  const normalizedProxy = spawnProxy(['--surface-id', 'normalized-surface', '--entrypoint', normalizedChildEntrypoint, '--'], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let normalizedStdout = '';
  normalizedProxy.stdout.setEncoding('utf8');
  normalizedProxy.stdout.on('data', (chunk) => { normalizedStdout += chunk; });
  const framedRequest = JSON.stringify({ jsonrpc: '2.0', id: 'framed-init', method: 'initialize', params: {} });
  normalizedProxy.stdin.write(`Content-Length: ${Buffer.byteLength(framedRequest, 'utf8')}\r\n\r\n${framedRequest}`);
  normalizedProxy.stdin.end();
  const normalizedExitCode = await new Promise<number | null>((resolve) => normalizedProxy.on('close', resolve));
  assert.equal(normalizedExitCode, 0);
  assert.match(normalizedStdout, /^Content-Length:/i);
  assert.match(normalizedStdout, /"id":"framed-init"/);

  // A large refused proof result may be followed by a carrier presentation
  // continuation marker on the same physical line. The proxy must forward
  // the complete JSON-RPC response, discard only the marker, and remain live
  // for the next call.
  const oversizedChildEntrypoint = join(root, 'oversized-refusal-child.mjs');
  writeFileSync(oversizedChildEntrypoint, [
    "let buffer = '';",
    "process.stdin.setEncoding('utf8');",
    "process.stdin.on('data', (chunk) => {",
    '  buffer += chunk;',
    '  let index;',
    "  while ((index = buffer.indexOf('\\n')) >= 0) {",
    '    const line = buffer.slice(0, index);',
    '    buffer = buffer.slice(index + 1);',
    '    if (!line.trim()) continue;',
    '    const request = JSON.parse(line);',
    '    const body = JSON.stringify({',
    "      jsonrpc: '2.0',",
    '      id: request.id,',
    '      result: {',
    '        isError: true,',
    "        content: [{ type: 'text', text: 'production_carrier_not_available' }],",
    '        structuredContent: {',
    "          schema: 'narada.producer_output_page.v1',",
    "          status: 'error',",
    '          truncated: true,',
    "          output_ref: 'mcp_output:o_task39_proxy_boundary',",
    "          reason: 'production_carrier_not_available',",
    "          output_text: 'x'.repeat(14000),",
    '        },',
    '      },',
    '    });',
    "    process.stdout.write(body + 'Cont\\r\\n');",
    '  }',
    '});',
  ].join('\n'), 'utf8');
  const oversizedProxy = spawnProxy([
    '--surface-id',
    'site-loop',
    '--entrypoint',
    oversizedChildEntrypoint,
    '--',
  ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  let oversizedStdout = '';
  oversizedProxy.stdout.setEncoding('utf8');
  oversizedProxy.stdout.on('data', (chunk) => { oversizedStdout += chunk; });
  oversizedProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 'oversized-1', method: 'tools/call', params: { name: 'site_loop_proof_run', arguments: {} } })}\n`);
  await waitForOutput(() => oversizedStdout.includes('"oversized-1"'), 2_000);
  const oversizedFirst = JSON.parse(oversizedStdout.trim()) as {
    id: string;
    result: { structuredContent: { schema: string; reason: string; output_text: string } };
  };
  assert.equal(oversizedFirst.id, 'oversized-1');
  assert.equal(oversizedFirst.result.structuredContent.schema, 'narada.producer_output_page.v1');
  assert.equal(oversizedFirst.result.structuredContent.reason, 'production_carrier_not_available');
  assert.equal(oversizedFirst.result.structuredContent.output_text.length, 14000);
  assert.equal(oversizedProxy.exitCode, null);
  oversizedStdout = '';
  oversizedProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 'oversized-2', method: 'tools/call', params: { name: 'site_loop_proof_run', arguments: {} } })}\n`);
  await waitForOutput(() => oversizedStdout.includes('"oversized-2"'), 2_000);
  const oversizedLines = oversizedStdout.trim().split(/\r?\n/).filter(Boolean);
  assert.equal(oversizedLines.length, 1);
  assert.equal((JSON.parse(oversizedLines[0]) as { id: string }).id, 'oversized-2');
  assert.equal(oversizedLines.some((line) => line.trim() === 'Cont'), false);
  assert.equal(oversizedProxy.exitCode, null);
  oversizedProxy.kill();
  await new Promise<number | null>((resolve) => oversizedProxy.on('close', resolve));

  const cycleEntrypoint = join(root, 'cycle-child.mjs');
  writeFileSync(cycleEntrypoint, [
    "process.stdin.setEncoding('utf8');",
    "process.stdin.on('data', (chunk) => {",
    "  const request = JSON.parse(chunk.trim());",
    "  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: { ok: true } }) + String.fromCharCode(10));",
    "});",
  ].join('\n'), 'utf8');
  const cycleDiagnostics = join(root, 'cycle-diagnostics');
  const identityTuples = new Set<string>();
  for (let cycle = 0; cycle < 100; cycle += 1) {
    const cycleProxy = spawnProxy([
      '--surface-id',
      `cycle-${cycle}`,
      '--entrypoint',
      cycleEntrypoint,
      '--diagnostics-dir',
      cycleDiagnostics,
      '--liveness-check-ms',
      '20',
      '--orphan-grace-ms',
      '50',
      '--',
    ], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    let cycleStdout = '';
    cycleProxy.stdout.setEncoding('utf8');
    cycleProxy.stderr.resume();
    cycleProxy.stdout.on('data', (chunk) => { cycleStdout += chunk; });
    cycleProxy.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: `cycle-${cycle}`, method: 'initialize', params: {} })}\n`);
    await waitForOutput(() => cycleStdout.includes(`"id":"cycle-${cycle}"`), 2_000);
    cycleProxy.stdin.end();
    const cycleExitCode = await new Promise<number | null>((resolve) => cycleProxy.on('close', resolve));
    assert.equal(cycleExitCode, 0);
    const instanceFile = readdirSync(cycleDiagnostics)
      .filter((file) => /^instance-\d+\.json$/.test(file))
      .find((file) => JSON.parse(readFileSync(join(cycleDiagnostics, file), 'utf8')).surface_id === `cycle-${cycle}`);
    assert.ok(instanceFile);
    const cycleRecord = JSON.parse(readFileSync(join(cycleDiagnostics, instanceFile), 'utf8')) as Record<string, any>;
    assert.equal(cycleRecord.surface_id, `cycle-${cycle}`);
    assert.equal(typeof cycleRecord.proxy_pid, 'number');
    assert.equal(typeof cycleRecord.supervisor_pid, 'number');
    assert.equal(typeof cycleRecord.managed_child_pid, 'number');
    assert.equal(typeof cycleRecord.generation_id, 'string');
    assert.equal(typeof cycleRecord.supervisor_identity_path, 'string');
    const identity = JSON.parse(readFileSync(String(cycleRecord.supervisor_identity_path), 'utf8')) as Record<string, any>;
    assert.equal(identity.schema, 'narada.process_supervisor.identity.v1');
    assert.equal(identity.supervisor_pid, cycleRecord.supervisor_pid);
    assert.equal(identity.managed_child_pid, cycleRecord.managed_child_pid);
    const tuple = [cycleRecord.surface_id, cycleRecord.generation_id, cycleRecord.proxy_pid, cycleRecord.supervisor_pid, cycleRecord.managed_child_pid].join(':');
    assert.equal(identityTuples.has(tuple), false);
    identityTuples.add(tuple);
  }
  assert.equal(identityTuples.size, 100);

  console.log('mcp-runtime-proxy behavior ok');
} finally {
  rmSync(root, { recursive: true, force: true });
  await processScope.close();
  processScope.assertClean();
}
