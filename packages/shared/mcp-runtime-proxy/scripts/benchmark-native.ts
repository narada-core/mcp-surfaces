import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';
import { MCP_RUNTIME_CONTRACT_VERSION } from '../src/materialization-contract.js';
import { fingerprintWorkspaceArtifactManifest } from '../src/workspace-artifact-manifest.js';
import { requireNativeArtifact } from '../src/native-artifact.js';

type JsonRecord = Record<string, any>;
type RuntimeName = 'bun' | 'node' | 'deno' | 'boa';
type RuntimeCommand = { executable: string; runtime_args: string[] };
type ProxyImplementation = 'javascript' | 'native';
type TopologyId = 'bun-bun' | 'node-node' | 'deno-deno' | 'native-bun' | 'native-node' | 'native-deno' | 'native-boa';
type TopologyStatus = 'measured' | 'skipped' | 'failed';
type ProcessMemory = { pid: number; name: string; private_bytes: number; working_set_bytes: number };
type TraceEvent = { at?: string; elapsed_ms?: number; event: string; detail?: JsonRecord };
type Sample = {
  ordinal: number;
  phases: {
    process_spawn_ms: number;
    initialize_roundtrip_ms: number;
    tools_list_roundtrip_ms: number;
    status_roundtrip_ms: number;
    warm_call_ms: number;
    cold_start_to_initialize_ms: number;
    trace_preflight_elapsed_ms: number | null;
    trace_preflight_to_child_spawn_ms: number | null;
    trace_initialize_forwarded_to_response_ms: number | null;
    trace_tools_list_forwarded_to_response_ms: number | null;
  };
  memory: { private_bytes: number | null; working_set_bytes: number | null; processes: ProcessMemory[] };
  lifecycle: { exit_code: number | null; protocol_ok: boolean; trace_schema: string | null; trace_events: TraceEvent[] };
};
type TopologyReport = {
  id: TopologyId;
  proxy_implementation: ProxyImplementation;
  proxy_runtime: RuntimeName | 'native';
  child_runtime: RuntimeName;
  status: TopologyStatus;
  reason?: string;
  samples: Sample[];
  summary?: JsonRecord;
  error?: string;
};

const args = parseArgs(process.argv.slice(2));
const sampleCount = args.samples ?? Number(process.env['NARADA_MCP_BENCHMARK_SAMPLES'] ?? 12);
const warmCalls = args.warmCalls ?? Number(process.env['NARADA_MCP_BENCHMARK_WARM_CALLS'] ?? 200);
const root = mkdtempSync(join(tmpdir(), 'mcp-runtime-benchmark-'));
const fixtureHandlerPath = join(root, 'fixture-handler.js');
const fixtureHostPath = join(root, 'fixture-host.mjs');
const manifestPath = join(root, 'workspace-artifact-manifest.json');
const bunProxyPath = fileURLToPath(new URL('../dist/src/main.js', import.meta.url));
const packageRoot = resolve(fileURLToPath(new URL('..', import.meta.url)));
const nativeProxyPath = requireNativeArtifact(packageRoot, 'narada-mcp-runtime.exe');
const nativeBoaPath = requireNativeArtifact(packageRoot, 'narada-mcp-boa-fixture.exe');
const reportId = `mcp-runtime-${new Date().toISOString().replace(/[-:.TZ]/g, '').slice(0, 14)}`;

function parseArgs(argv: string[]): { outputDir?: string; samples?: number; warmCalls?: number } {
  const result: { outputDir?: string; samples?: number; warmCalls?: number } = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--output-dir') {
      result.outputDir = argv[++index];
    } else if (arg === '--samples') {
      result.samples = positiveInteger(argv[++index], '--samples');
    } else if (arg === '--warm-calls') {
      result.warmCalls = positiveInteger(argv[++index], '--warm-calls');
    } else {
      throw new Error(`benchmark_unknown_argument:${arg}`);
    }
  }
  return result;
}

function positiveInteger(value: string | undefined, name: string): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new Error(`benchmark_invalid_value:${name}`);
  return parsed;
}

function artifact(path: string): JsonRecord {
  const bytes = readFileSync(path);
  const stat = statSync(path);
  return { path, sha256: createHash('sha256').update(bytes).digest('hex'), size: stat.size, mtime_ms: stat.mtimeMs };
}

function percentile(values: number[], fraction: number): number | null {
  if (values.length === 0) return null;
  const ordered = [...values].sort((left, right) => left - right);
  return ordered[Math.min(ordered.length - 1, Math.ceil(ordered.length * fraction) - 1)]!;
}

function commandVersion(command: RuntimeCommand): string | null {
  const result = spawnSync(command.executable, [...command.runtime_args, '--version'], { encoding: 'utf8', windowsHide: true, timeout: 10_000, stdio: ['ignore', 'pipe', 'pipe'] });
  if (result.status !== 0) return null;
  return `${String(result.stdout ?? '').trim()} ${String(result.stderr ?? '').trim()}`.trim().slice(0, 160) || null;
}

function availableCommand(runtime: RuntimeName): RuntimeCommand | null {
  if (runtime === 'boa') {
    return existsSync(nativeBoaPath) ? { executable: nativeBoaPath, runtime_args: [] } : null;
  }
  const own = basename(process.execPath).toLowerCase();
  const candidates = runtime === 'bun'
    ? [own.includes('bun') ? process.execPath : 'bun']
    : runtime === 'node'
      ? [own.includes('node') ? process.execPath : 'node']
      : [process.env['NARADA_MCP_BENCHMARK_DENO']?.trim() || '', 'deno'];
  const runtimeArgs = runtime === 'deno' ? ['--allow-all', '--no-config', '--node-modules-dir=manual'] : [];
  for (const candidate of candidates) {
    if (!candidate) continue;
    const command = { executable: candidate, runtime_args: runtimeArgs };
    if (commandVersion(command) !== null) return command;
  }
  return null;
}

function commandSpec(command: RuntimeCommand | null): JsonRecord | null {
  return command ? { executable: command.executable, runtime_args: command.runtime_args, child_invocation: [command.executable, '<entrypoint>'] } : null;
}

function rpcReader(child: ChildProcessWithoutNullStreams): (id: string | number) => Promise<JsonRecord> {
  let buffer = '';
  let childError: Error | null = null;
  const waiting = new Map<string | number, (value: JsonRecord) => void>();
  const ready = new Map<string | number, JsonRecord>();
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (chunk: string) => {
    buffer += chunk;
    let end: number;
    while ((end = buffer.indexOf('\n')) >= 0) {
      const line = buffer.slice(0, end).trim();
      buffer = buffer.slice(end + 1);
      if (!line) continue;
      let response: JsonRecord;
      try {
        response = JSON.parse(line) as JsonRecord;
      } catch (error) {
        childError = new Error(`benchmark_invalid_json:${String(error)}`);
        continue;
      }
      const resolver = waiting.get(response.id);
      if (resolver) {
        waiting.delete(response.id);
        resolver(response);
      } else {
        ready.set(response.id, response);
      }
    }
  });
  child.on('error', (error) => { childError = error; });
  return (id) => new Promise((resolveResponse, reject) => {
    const existing = ready.get(id);
    if (existing) {
      ready.delete(id);
      resolveResponse(existing);
      return;
    }
    const timeout = setTimeout(() => {
      waiting.delete(id);
      reject(childError ?? new Error(`benchmark_response_timeout:${id}`));
    }, 15_000);
    waiting.set(id, (value) => {
      clearTimeout(timeout);
      resolveResponse(value);
    });
  });
}

async function processMemory(pids: number[]): Promise<ProcessMemory[]> {
  if (process.platform !== 'win32') return [];
  const ids = [...new Set(pids.filter((pid) => Number.isSafeInteger(pid) && pid > 0))];
  if (ids.length === 0) return [];
  const script = `@(Get-Process -Id ${ids.join(',')} -ErrorAction SilentlyContinue | ForEach-Object { [pscustomobject]@{ pid=$_.Id; name=$_.ProcessName; private_bytes=$_.PrivateMemorySize64; working_set_bytes=$_.WorkingSet64 } }) | ConvertTo-Json -Compress`;
  const child = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
  let stdout = '';
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (chunk: string) => { stdout += chunk; });
  const code = await new Promise<number | null>((resolveCode) => child.on('close', resolveCode));
  if (code !== 0 || !stdout.trim()) return [];
  const parsed = JSON.parse(stdout.trim()) as ProcessMemory | ProcessMemory[];
  return (Array.isArray(parsed) ? parsed : [parsed]).map((value) => ({
    pid: Number(value.pid),
    name: String(value.name),
    private_bytes: Number(value.private_bytes),
    working_set_bytes: Number(value.working_set_bytes),
  }));
}

function traceEvent(trace: JsonRecord | null, event: string): TraceEvent | null {
  return (trace?.events as TraceEvent[] | undefined)?.find((candidate) => candidate.event === event) ?? null;
}

function traceDelta(trace: JsonRecord | null, first: string, second: string): number | null {
  const firstEvent = traceEvent(trace, first);
  const secondEvent = traceEvent(trace, second);
  if (typeof firstEvent?.elapsed_ms === 'number' && typeof secondEvent?.elapsed_ms === 'number') {
    return Math.max(0, secondEvent.elapsed_ms - firstEvent.elapsed_ms);
  }
  if (firstEvent?.at && secondEvent?.at) {
    const delta = Date.parse(secondEvent.at) - Date.parse(firstEvent.at);
    return Number.isFinite(delta) ? Math.max(0, delta) : null;
  }
  return null;
}

function traceDeltaForMethod(trace: JsonRecord | null, method: string): number | null {
  const events = (trace?.events as TraceEvent[] | undefined) ?? [];
  const forwarded = events.find((event) => event.event === 'request_forwarded' && event.detail?.method === method);
  const response = events.find((event) => event.event === 'child_response' && event.detail?.method === method);
  if (!forwarded || !response) return null;
  if (typeof forwarded.elapsed_ms === 'number' && typeof response.elapsed_ms === 'number') return Math.max(0, response.elapsed_ms - forwarded.elapsed_ms);
  if (forwarded.at && response.at) {
    const delta = Date.parse(response.at) - Date.parse(forwarded.at);
    return Number.isFinite(delta) ? Math.max(0, delta) : null;
  }
  return null;
}

function readTrace(diagnostics: string, surfaceId: string): JsonRecord | null {
  try {
    return JSON.parse(readFileSync(join(diagnostics, `startup-${surfaceId}.json`), 'utf8')) as JsonRecord;
  } catch {
    return null;
  }
}

function readPhaseTrace(diagnostics: string, surfaceId: string): JsonRecord | null {
  try {
    return JSON.parse(readFileSync(join(diagnostics, `startup-phases-${surfaceId}.json`), 'utf8')) as JsonRecord;
  } catch {
    return null;
  }
}

async function waitForSpawn(child: ChildProcessWithoutNullStreams): Promise<number> {
  return new Promise((resolveSpawn, reject) => {
    child.once('spawn', () => resolveSpawn(performance.now()));
    child.once('error', reject);
  });
}

function proxyLaunch(
  topology: { proxy: ProxyImplementation; proxyRuntime: RuntimeName | 'native'; childRuntime: RuntimeName },
  childCommand: string,
  diagnostics: string,
  runtimeCommands: Record<RuntimeName, RuntimeCommand | null>,
): { command: string; args: string[] } {
  const child = runtimeCommands[topology.childRuntime];
  assert.ok(child);
  const childEntrypoint = topology.childRuntime === 'boa' ? fixtureHandlerPath : fixtureHostPath;
  const childArgs = topology.childRuntime === 'boa' ? [] : [fixtureHandlerPath];
  const common = [
    '--surface-id', `benchmark-${topology.proxyRuntime}-${topology.childRuntime}`,
    '--artifact-manifest', manifestPath,
    '--runtime-contract-version', String(MCP_RUNTIME_CONTRACT_VERSION),
    '--child-command', childCommand,
    '--child-prefix-args', JSON.stringify(child.runtime_args),
    '--entrypoint', childEntrypoint,
    '--diagnostics-dir', diagnostics,
    '--orphan-grace-ms', '100',
    '--',
    ...childArgs,
  ];
  if (topology.proxy === 'native') return { command: nativeProxyPath, args: ['proxy', ...common] };
  const proxyCommand = runtimeCommands[topology.proxyRuntime];
  assert.ok(proxyCommand);
  return { command: proxyCommand.executable, args: [...proxyCommand.runtime_args, bunProxyPath, ...common] };
}

async function measure(
  topology: { id: TopologyId; proxy: ProxyImplementation; proxyRuntime: RuntimeName | 'native'; childRuntime: RuntimeName },
  runtimeCommands: Record<RuntimeName, RuntimeCommand | null>,
): Promise<TopologyReport> {
  const childRuntime = runtimeCommands[topology.childRuntime];
  if (!childRuntime) return { ...topology, status: 'skipped', samples: [], reason: topology.childRuntime === 'boa' ? 'boa_artifact_unavailable' : `${topology.childRuntime}_runtime_unavailable` };
  if (topology.proxy === 'native' && (process.platform !== 'win32' || !existsSync(nativeProxyPath))) {
    return { ...topology, status: 'skipped', samples: [], reason: 'native_windows_artifact_unavailable' };
  }
  const samples: Sample[] = [];
  let lastStderr = '';
  try {
    for (let ordinal = 0; ordinal < sampleCount; ordinal += 1) {
      const diagnostics = join(root, `${topology.id}-${ordinal}`);
      mkdirSync(diagnostics, { recursive: true });
      const launch = proxyLaunch(topology, childRuntime.executable, diagnostics, runtimeCommands);
      const started = performance.now();
      const child = spawn(launch.command, launch.args, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
      let stderr = '';
      child.stderr.setEncoding('utf8');
      child.stderr.on('data', (chunk: string) => { stderr = `${stderr}${chunk}`.slice(-4_000); lastStderr = stderr; });
      const response = rpcReader(child);
      const spawnedAt = await waitForSpawn(child);
      const initializeSent = performance.now();
      child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: 'initialize', method: 'initialize', params: { protocolVersion: '2024-11-05' } }) + '\n');
      const initialized = await response('initialize');
      const initializedAt = performance.now();
      assert.equal(initialized.error, undefined, `${topology.id}:${stderr}`);
      const toolsStarted = performance.now();
      child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: 'tools', method: 'tools/list', params: {} }) + '\n');
      const tools = await response('tools');
      const toolsFinished = performance.now();
      assert.equal(tools.error, undefined, `${topology.id}:tools:${stderr}`);
      assert.equal(Array.isArray(tools.result?.tools), true);
      const statusStarted = performance.now();
      child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: 'status', method: 'tools/call', params: { name: 'mcp_runtime_proxy_status', arguments: {} } }) + '\n');
      const status = await response('status');
      const statusFinished = performance.now();
      assert.equal(status.error, undefined, `${topology.id}:status:${stderr}`);
      const liveness = status.result?.structuredContent?.liveness as JsonRecord;
      const processes = await processMemory([child.pid!, liveness?.supervisor_pid, liveness?.server_pid]);
      const warmStarted = performance.now();
      for (let index = 0; index < warmCalls; index += 1) {
        const id = `warm-${index}`;
        child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method: 'tools/call', params: { name: 'fixture_echo', arguments: { value: index } } }) + '\n');
        const value = await response(id);
        assert.equal(value.result?.content?.[0]?.text, String(index));
      }
      const warmCallMs = (performance.now() - warmStarted) / warmCalls;
      child.stdin.end();
      const exitCode = await new Promise<number | null>((resolveExit) => child.on('close', resolveExit));
      const surfaceId = `benchmark-${topology.proxyRuntime}-${topology.childRuntime}`;
      const trace = readTrace(diagnostics, surfaceId);
      const phaseTrace = readPhaseTrace(diagnostics, surfaceId);
      samples.push({
        ordinal,
        phases: {
          process_spawn_ms: spawnedAt - started,
          initialize_roundtrip_ms: initializedAt - initializeSent,
          tools_list_roundtrip_ms: toolsFinished - toolsStarted,
          status_roundtrip_ms: statusFinished - statusStarted,
          warm_call_ms: warmCallMs,
          cold_start_to_initialize_ms: initializedAt - started,
          trace_preflight_elapsed_ms: typeof phaseTrace?.preflight_ms === 'number' ? phaseTrace.preflight_ms : null,
          trace_preflight_to_child_spawn_ms: traceDelta(trace, 'preflight_ok', 'child_spawned'),
          trace_initialize_forwarded_to_response_ms: traceDeltaForMethod(trace, 'initialize'),
          trace_tools_list_forwarded_to_response_ms: traceDeltaForMethod(trace, 'tools/list'),
        },
        memory: {
          private_bytes: processes.length ? processes.reduce((total, value) => total + value.private_bytes, 0) : null,
          working_set_bytes: processes.length ? processes.reduce((total, value) => total + value.working_set_bytes, 0) : null,
          processes,
        },
        lifecycle: {
          exit_code: exitCode,
          protocol_ok: exitCode === 0,
          trace_schema: typeof trace?.schema === 'string' ? trace.schema : null,
          trace_events: Array.isArray(trace?.events) ? trace.events : [],
        },
      });
    }
    return { ...topology, status: 'measured', samples, summary: summarize(samples) };
  } catch (error) {
    return { ...topology, status: 'failed', samples, error: `${String(error)}${lastStderr ? `; stderr=${lastStderr}` : ''}`.slice(0, 2_000) };
  }
}

function summarize(samples: Sample[]): JsonRecord {
  const values = (selector: (sample: Sample) => number) => samples.map(selector).filter((value) => Number.isFinite(value));
  return {
    sample_count: samples.length,
    initialize_p95_ms: percentile(values((sample) => sample.phases.cold_start_to_initialize_ms), 0.95),
    initialize_roundtrip_p95_ms: percentile(values((sample) => sample.phases.initialize_roundtrip_ms), 0.95),
    tools_list_p95_ms: percentile(values((sample) => sample.phases.tools_list_roundtrip_ms), 0.95),
    process_spawn_p95_ms: percentile(values((sample) => sample.phases.process_spawn_ms), 0.95),
    warm_call_p95_ms: percentile(values((sample) => sample.phases.warm_call_ms), 0.95),
    private_bytes_p95: percentile(samples.flatMap((sample) => sample.memory.private_bytes === null ? [] : [sample.memory.private_bytes]), 0.95),
    working_set_bytes_p95: percentile(samples.flatMap((sample) => sample.memory.working_set_bytes === null ? [] : [sample.memory.working_set_bytes]), 0.95),
    lifecycle_passed: samples.every((sample) => sample.lifecycle.protocol_ok),
  };
}

function topologyMatrix(): Array<{ id: TopologyId; proxy: ProxyImplementation; proxyRuntime: RuntimeName | 'native'; childRuntime: RuntimeName }> {
  return [
    { id: 'bun-bun', proxy: 'javascript', proxyRuntime: 'bun', childRuntime: 'bun' },
    { id: 'node-node', proxy: 'javascript', proxyRuntime: 'node', childRuntime: 'node' },
    { id: 'deno-deno', proxy: 'javascript', proxyRuntime: 'deno', childRuntime: 'deno' },
    { id: 'native-bun', proxy: 'native', proxyRuntime: 'native', childRuntime: 'bun' },
    { id: 'native-node', proxy: 'native', proxyRuntime: 'native', childRuntime: 'node' },
    { id: 'native-deno', proxy: 'native', proxyRuntime: 'native', childRuntime: 'deno' },
    { id: 'native-boa', proxy: 'native', proxyRuntime: 'native', childRuntime: 'boa' },
  ];
}

function ratio(nativeValue: number | null, baselineValue: number | null): number | null {
  return nativeValue === null || baselineValue === null || baselineValue === 0 ? null : nativeValue / baselineValue;
}

function buildReport(reports: TopologyReport[], environment: JsonRecord): JsonRecord {
  const baseline = reports.find((report) => report.id === 'bun-bun' && report.status === 'measured');
  const nodeBaseline = reports.find((report) => report.id === 'node-node' && report.status === 'measured');
  const denoBaseline = reports.find((report) => report.id === 'deno-deno' && report.status === 'measured');
  const nativeBun = reports.find((report) => report.id === 'native-bun' && report.status === 'measured');
  const nativeNode = reports.find((report) => report.id === 'native-node' && report.status === 'measured');
  const nativeDeno = reports.find((report) => report.id === 'native-deno' && report.status === 'measured');
  const nativeCandidates = [nativeBun, nativeNode, nativeDeno].filter((report): report is TopologyReport => Boolean(report));
  const pairs = [
    { label: 'native_bun_vs_bun_bun', native: nativeBun, baseline },
    { label: 'native_node_vs_node_node', native: nativeNode, baseline: nodeBaseline },
    { label: 'native_deno_vs_deno_deno', native: nativeDeno, baseline: denoBaseline },
  ];
  const comparisons = pairs.flatMap(({ label, native, baseline: pairBaseline }) => [
    { name: `${label}.private_bytes_p95`, actual: native?.summary?.private_bytes_p95 ?? null, baseline: pairBaseline?.summary?.private_bytes_p95 ?? null, ratio: ratio(native?.summary?.private_bytes_p95 ?? null, pairBaseline?.summary?.private_bytes_p95 ?? null) },
    { name: `${label}.initialize_p95_ms`, actual: native?.summary?.initialize_p95_ms ?? null, baseline: pairBaseline?.summary?.initialize_p95_ms ?? null, ratio: ratio(native?.summary?.initialize_p95_ms ?? null, pairBaseline?.summary?.initialize_p95_ms ?? null) },
    { name: `${label}.warm_call_p95_ms`, actual: native?.summary?.warm_call_p95_ms ?? null, baseline: pairBaseline?.summary?.warm_call_p95_ms ?? null, ratio: ratio(native?.summary?.warm_call_p95_ms ?? null, pairBaseline?.summary?.warm_call_p95_ms ?? null) },
  ]);
  const lifecycleFailures = reports.filter((report) => report.status === 'failed' || report.summary?.lifecycle_passed === false);
  return {
    schema: 'narada.mcp_runtime_proxy.benchmark_report.v1',
    report_id: reportId,
    generated_at: new Date().toISOString(),
    objective: 'Measure attributable Bun/Node/Deno/native MCP runtime topology differences; native is the supported-Windows default, Deno remains experimental, and Native/Boa is diagnostic-only.',
    scope: { runtimes: ['bun', 'node', 'deno', 'boa'], native_windows_only: true, deno_included: true, boa_diagnostic_only: true, network_required: false },
    environment,
    configuration: { sample_count: sampleCount, warm_calls_per_sample: warmCalls, runtime_contract_version: MCP_RUNTIME_CONTRACT_VERSION, matrix: ['bun-bun', 'node-node', 'deno-deno', 'native-bun', 'native-node', 'native-deno', 'native-boa'], diagnostic_only: ['native-boa'] },
    baseline: baseline?.id ?? null,
    topologies: reports,
    comparisons,
    gates: [],
    verdict: {
      performance: 'measurements_only',
      correctness: lifecycleFailures.length === 0 ? 'passed' : 'failed',
      native_default: process.platform === 'win32' && existsSync(nativeProxyPath) ? 'default_when_available' : 'bun_fallback',
      native_availability: nativeCandidates.length > 0 ? 'available_as_default_on_supported_host' : 'unavailable_on_this_host',
    },
  };
}

function htmlEscape(value: unknown): string {
  return String(value).replace(/[&<>"']/g, (character) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[character]!));
}

function htmlArtifact(report: JsonRecord): string {
  const embedded = JSON.stringify(report).replace(/</g, '\\u003c');
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>MCP runtime benchmark ${htmlEscape(report.report_id)}</title>
<style>
:root{color-scheme:dark;font:14px/1.45 system-ui,sans-serif;background:#111827;color:#e5e7eb}body{margin:0;padding:24px;max-width:1280px;margin-inline:auto}h1,h2{margin:0 0 12px}p{color:#aab4c3}.toolbar,.cards,.grid{display:grid;gap:12px}.toolbar{grid-template-columns:1fr auto auto;align-items:center;margin:16px 0}.cards{grid-template-columns:repeat(auto-fit,minmax(180px,1fr))}.card,section{background:#1f2937;border:1px solid #374151;border-radius:10px;padding:14px}.value{font-size:24px;font-weight:700}.muted{color:#9ca3af}.pass{color:#86efac}.fail{color:#fca5a5}.skip{color:#fde68a}table{width:100%;border-collapse:collapse;margin-top:10px}th,td{text-align:left;border-bottom:1px solid #374151;padding:7px}code{color:#c4b5fd}.bar{height:8px;background:#374151;border-radius:8px;overflow:hidden}.bar>i{display:block;height:100%;background:#60a5fa}.hidden{display:none}.wide{overflow:auto}
</style></head><body>
<h1>MCP runtime benchmark</h1><p>Offline artifact: <code>${htmlEscape(report.report_id)}</code>. JSON is embedded and can be downloaded.</p>
<div class="toolbar"><select id="topology"></select><button id="download">Download JSON</button><span id="verdict"></span></div>
<div id="summary" class="cards"></div><section><h2>Baseline comparisons</h2><div id="comparisons" class="wide"></div></section>
<section><h2>Selected topology</h2><div id="detail" class="wide"></div></section>
<script id="benchmark-data" type="application/json">${embedded}</script>
<script>
const report=JSON.parse(document.getElementById('benchmark-data').textContent);const select=document.getElementById('topology');
const esc=v=>String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const fmt=v=>v===null||v===undefined?'—':typeof v==='number'?(Math.abs(v)>100000?Math.round(v).toLocaleString():v.toFixed(2)):esc(v);
const cls=s=>s==='passed'||s==='measured'?'pass':s==='failed'?'fail':s==='skipped'||s==='not_comparable'?'skip':'';
report.topologies.forEach(t=>{const o=document.createElement('option');o.value=t.id;o.textContent=t.id+' ('+t.status+')';select.appendChild(o)});
document.getElementById('verdict').innerHTML='<span class="'+cls(report.verdict.performance)+'">performance: '+esc(report.verdict.performance)+'</span>';
document.getElementById('comparisons').innerHTML='<table><tr><th>Comparison</th><th>Actual</th><th>Baseline</th><th>Ratio</th></tr>'+report.comparisons.map(g=>'<tr><td>'+esc(g.name)+'</td><td>'+fmt(g.actual)+'</td><td>'+fmt(g.baseline)+'</td><td>'+fmt(g.ratio)+'</td></tr>').join('')+'</table>';
function render(){const t=report.topologies.find(v=>v.id===select.value)||report.topologies[0];if(!t)return;const s=t.summary||{};document.getElementById('summary').innerHTML=[['status',t.status],['initialize p95',s.initialize_p95_ms==null?'—':fmt(s.initialize_p95_ms)+' ms'],['warm p95',s.warm_call_p95_ms==null?'—':fmt(s.warm_call_p95_ms)+' ms'],['private p95',s.private_bytes_p95==null?'—':fmt(s.private_bytes_p95)+' B'],['working set p95',s.working_set_bytes_p95==null?'—':fmt(s.working_set_bytes_p95)+' B']].map(([k,v])=>'<div class="card"><div class="muted">'+esc(k)+'</div><div class="value '+cls(k==='status'?v:'')+'">'+esc(v)+'</div></div>').join('');if(t.status!=='measured'){document.getElementById('detail').innerHTML='<p class="'+cls(t.status)+'">'+esc(t.reason||t.error||'No measurement')+'</p>';return}const rows=t.samples.flatMap(x=>x.memory.processes.map(p=>'<tr><td>'+x.ordinal+'</td><td>'+esc(p.name)+'</td><td>'+p.pid+'</td><td>'+fmt(p.private_bytes)+'</td><td>'+fmt(p.working_set_bytes)+'</td></tr>')).join('');document.getElementById('detail').innerHTML='<p>proxy: <code>'+esc(t.proxy_implementation+'/'+t.proxy_runtime)+'</code>; child: <code>'+esc(t.child_runtime)+'</code></p><table><tr><th>Sample</th><th>Process</th><th>PID</th><th>Private bytes</th><th>Working set</th></tr>'+rows+'</table><p class="muted">Raw samples are embedded in this artifact. The benchmark did not upload them.</p>'}
select.addEventListener('change',render);document.getElementById('download').addEventListener('click',()=>{const a=document.createElement('a');a.href=URL.createObjectURL(new Blob([JSON.stringify(report,null,2)],{type:'application/json'}));a.download=report.report_id+'.json';a.click();URL.revokeObjectURL(a.href)});select.value=report.baseline||report.topologies[0]?.id;render();
</script></body></html>`;
}

function writeArtifacts(report: JsonRecord): { jsonPath: string; htmlPath: string } {
  const outputDir = resolve(args.outputDir ?? join(process.cwd(), '.ai', 'runtime', 'mcp-runtime-benchmarks', report.report_id));
  mkdirSync(outputDir, { recursive: true });
  const jsonPath = join(outputDir, `${report.report_id}.json`);
  const htmlPath = join(outputDir, `${report.report_id}.html`);
  writeFileSync(jsonPath, `${JSON.stringify(report, null, 2)}\n`, 'utf8');
  writeFileSync(htmlPath, htmlArtifact(report), 'utf8');
  return { jsonPath, htmlPath };
}

writeFileSync(fixtureHandlerPath, [
  "globalThis.naradaFixtureHandle = function(request) {",
  "  if (request.method === 'initialize') return { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'benchmark', version: '1' } };",
  "  if (request.method === 'tools/list') return { tools: [{ name: 'fixture_echo', inputSchema: { type: 'object' } }] };",
  "  return { content: [{ type: 'text', text: String(request.params.arguments.value) }] };",
  "};",
].join('\n'));
writeFileSync(fixtureHostPath, [
  "import { readFileSync } from 'node:fs';",
  "const handlerPath = process.argv[2];",
  "if (!handlerPath) throw new Error('benchmark_fixture_handler_path_required');",
  "eval(readFileSync(handlerPath, 'utf8'));",
  "let buffer = ''; process.stdin.setEncoding('utf8');",
  "process.stdin.on('data', chunk => { buffer += chunk; let end; while ((end = buffer.indexOf('\\n')) >= 0) {",
  "  const line = buffer.slice(0, end).trim(); buffer = buffer.slice(end + 1); if (!line) continue; const request = JSON.parse(line);",
  "  const result = globalThis.naradaFixtureHandle(request);",
  "  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\\n');",
  "}});",
].join('\n'));
const manifestArtifacts = [artifact(fixtureHandlerPath), artifact(fixtureHostPath)];
if (existsSync(nativeBoaPath)) manifestArtifacts.push(artifact(nativeBoaPath));
const unsigned = { schema: 'narada.workspace_artifact_manifest.v1', generated_at: new Date().toISOString(), workspace_root: root, packages: [], artifacts: manifestArtifacts };
writeFileSync(manifestPath, JSON.stringify({ ...unsigned, manifest_fingerprint: fingerprintWorkspaceArtifactManifest(unsigned) }, null, 2) + '\n');

const bunCommand = availableCommand('bun');
const nodeCommand = availableCommand('node');
const denoCommand = availableCommand('deno');
const boaCommand = availableCommand('boa');
if (!Number.isSafeInteger(sampleCount) || sampleCount <= 0) throw new Error('benchmark_invalid_sample_count');
if (!Number.isSafeInteger(warmCalls) || warmCalls <= 0) throw new Error('benchmark_invalid_warm_call_count');
const matrix = topologyMatrix();
const environment = {
  platform: process.platform,
  architecture: process.arch,
  runner: process.execPath,
  runtimes: { bun: bunCommand ? commandVersion(bunCommand) : null, node: nodeCommand ? commandVersion(nodeCommand) : null, deno: denoCommand ? commandVersion(denoCommand) : null, boa: boaCommand ? commandVersion(boaCommand) : null },
  runtime_commands: { bun: commandSpec(bunCommand), node: commandSpec(nodeCommand), deno: commandSpec(denoCommand), boa: commandSpec(boaCommand) },
  native_artifact: process.platform === 'win32' && existsSync(nativeProxyPath),
  boa_artifact: process.platform === 'win32' && existsSync(nativeBoaPath),
};

try {
  const reports: TopologyReport[] = [];
  for (const topology of matrix) reports.push(await measure(topology, { bun: bunCommand, node: nodeCommand, deno: denoCommand, boa: boaCommand }));
  const report = buildReport(reports, environment);
  const artifacts = writeArtifacts(report);
  const output = { ...report, artifacts: { json_path: artifacts.jsonPath, html_path: artifacts.htmlPath } };
  writeFileSync(artifacts.jsonPath, `${JSON.stringify(output, null, 2)}\n`, 'utf8');
  writeFileSync(artifacts.htmlPath, htmlArtifact(output), 'utf8');
  console.log(JSON.stringify({ schema: 'narada.mcp_runtime_proxy.benchmark_complete.v1', report_id: report.report_id, json_path: artifacts.jsonPath, html_path: artifacts.htmlPath, verdict: report.verdict, comparisons: report.comparisons }));
  const harnessFailed = report.verdict.correctness === 'failed';
  if (harnessFailed) process.exitCode = 1;
} finally {
  rmSync(root, { recursive: true, force: true });
}
