import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';
import { resolveNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';

type Json = Record<string, any>;
type Topology = { id: string; loader: string; child: string; command: string; args: string[] };
const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const serverPath = join(packageRoot, 'dist', 'src', 'main.js');
const nativeArtifactName = process.platform === 'win32' ? 'narada-mcp-loader.exe' : 'narada-mcp-loader';
const nativePath = resolveNativeArtifact(packageRoot, nativeArtifactName);
const n = positive(process.env.NARADA_LOADER_BENCHMARK_SAMPLES, 12);
const warmCount = positive(process.env.NARADA_LOADER_BENCHMARK_WARM_CALLS, 60);
const fixture = mkdtempSync(join(tmpdir(), 'narada-loader-benchmark-'));
const siteRoot = join(fixture, 'narada.benchmark');
const configPath = join(siteRoot, '.ai', 'mcp', 'config.json');
const childPath = join(fixture, 'echo-child.mjs');
writeFileSync(childPath, [
  "import { writeFileSync } from 'node:fs';",
  "const pidFile = process.argv[2] ?? process.env.NARADA_LOADER_BENCHMARK_CHILD_PID_FILE; if (pidFile) writeFileSync(pidFile, String(process.pid), 'utf8');",
  "let b = '';",
  "process.stdin.setEncoding('utf8');",
  "process.stdin.on('data', c => { b += c; const lines = b.split(String.fromCharCode(10)); b = lines.pop() ?? ''; for (const line of lines) { if (!line.trim()) continue; const q = JSON.parse(line); if (q.method === 'notifications/initialized') continue; let r = {}; if (q.method === 'initialize') r = { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'benchmark-child', version: '1' } }; else if (q.method === 'tools/list') r = { tools: [{ name: 'echo', inputSchema: { type: 'object' } }] }; else if (q.method === 'tools/call') r = { content: [{ type: 'text', text: JSON.stringify(q.params.arguments ?? {}) }], structuredContent: { status: 'ok', args: q.params.arguments ?? {} } }; process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: q.id, result: r }) + String.fromCharCode(10)); } });",
].join('\n'), 'utf8');

mkdirSync(join(siteRoot, '.ai', 'mcp'), { recursive: true });
writeFileSync(configPath, JSON.stringify({ site_id: 'benchmark', mcpServers: { 'benchmark-echo': { command: 'node', args: [childPath] } } }), 'utf8');
function positive(raw: string | undefined, fallback: number): number {
  const value = raw === undefined ? fallback : Number(raw);
  if (!Number.isSafeInteger(value) || value <= 0) throw new Error('benchmark_invalid_sample_count');
  return value;
}
type MemorySnapshot = { loader_bytes: number | null; child_bytes: number | null };
const memoryMetric = process.platform === 'win32' ? 'private_bytes' : 'rss_bytes';

function processMemoryBytes(pid: number): number | null {
  if (!Number.isSafeInteger(pid) || pid <= 0) return null;
  if (process.platform === 'win32') {
    const command = `(Get-Process -Id ${pid} -ErrorAction Stop | Select-Object -ExpandProperty PrivateMemorySize64)`;
    const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', command], {
      stdio: ['ignore', 'pipe', 'ignore'], encoding: 'utf8', windowsHide: true, timeout: 2000,
    });
    if (result.status !== 0) return null;
    const value = Number(result.stdout.trim().split(/\s+/)[0]);
    return Number.isSafeInteger(value) && value >= 0 ? value : null;
  }
  try {
    const status = readFileSync(`/proc/${pid}/status`, 'utf8');
    const match = /^VmRSS:\s+(\d+)\s+kB$/m.exec(status);
    if (match) return Number(match[1]) * 1024;
  } catch { /* fall through to ps on non-Linux Unix */ }
  const result = spawnSync('ps', ['-o', 'rss=', '-p', String(pid)], {
    stdio: ['ignore', 'pipe', 'ignore'], encoding: 'utf8', timeout: 2000,
  });
  const value = Number(result.stdout.trim().split(/\s+/)[0]) * 1024;
  return Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function memorySnapshot(loaderPid: number, childPid: number | null): MemorySnapshot {
  return { loader_bytes: processMemoryBytes(loaderPid), child_bytes: childPid === null ? null : processMemoryBytes(childPid) };
}

function peak(values: Array<number | null>): number | null {
  const present = values.filter((value): value is number => value !== null && Number.isFinite(value));
  return present.length === 0 ? null : Math.max(...present);
}

function readPid(path: string): number | null {
  try {
    const value = Number(readFileSync(path, 'utf8').trim());
    return Number.isSafeInteger(value) && value > 0 ? value : null;
  } catch { return null; }
}

async function waitForPid(path: string): Promise<number | null> {
  const deadline = Date.now() + 2000;
  while (Date.now() < deadline) {
    const pid = readPid(path);
    if (pid !== null) return pid;
    await new Promise(resolve => setTimeout(resolve, 5));
  }
  return readPid(path);
}
function available(command: string): boolean {
  return spawnSync(command, ['--version'], { stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8', windowsHide: true, timeout: 10000 }).status === 0;
}
function p95(values: number[], fraction = 0.95): number {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * fraction) - 1)] ?? 0;
}
function topologies(): Topology[] {
  const node = basename(process.execPath).toLowerCase().includes('node') ? process.execPath : 'node';
  const bun = basename(process.execPath).toLowerCase().includes('bun') ? process.execPath : 'bun';
  if (!existsSync(serverPath)) throw new Error('benchmark_missing_typescript_build');
  if (!nativePath || !existsSync(nativePath)) throw new Error('benchmark_missing_native_build');
  if (!available(node)) throw new Error('benchmark_node_unavailable');
  if (!available(bun)) throw new Error('benchmark_bun_unavailable');
  return [
    { id: 'Node/Node', loader: 'node', child: 'node', command: node, args: [serverPath] },
    { id: 'Bun/Bun', loader: 'bun', child: 'bun', command: bun, args: [serverPath] },
    { id: 'Rust/Node', loader: 'rust', child: 'node', command: nativePath, args: ['--child-command', node] },
  ];
}

class Client {
  private buffer = '';
  private id = 1;
  private closed = false;
  private pending = new Map<number, { resolve: (v: Json) => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  constructor(private readonly proc: any) {
    proc.stdout.setEncoding('utf8');
    proc.stderr.resume();
    proc.stdout.on('data', (chunk: string) => {
      this.buffer += chunk;
      const lines = this.buffer.split(/\r?\n/);
      this.buffer = lines.pop() ?? '';
      for (const line of lines) {
        if (!line.trim()) continue;
        try {
          const msg = JSON.parse(line) as Json;
          const waiter = this.pending.get(Number(msg.id));
          if (!waiter) continue;
          this.pending.delete(Number(msg.id));
          clearTimeout(waiter.timer);
          waiter.resolve(msg);
        } catch (error) {
          this.fail(new Error('benchmark_invalid_json:' + String(error).slice(0, 256)));
        }
      }
    });
    proc.on('error', (error: Error) => this.fail(error));
    proc.on('close', () => { this.closed = true; this.fail(new Error('loader_closed')); });
  }
  private fail(error: Error): void {
    for (const [id, waiter] of this.pending) {
      this.pending.delete(id);
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
  }
  call(method: string, params: Json = {}): Promise<Json> {
    if (this.closed) return Promise.reject(new Error('loader_closed'));
    const id = this.id++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error('benchmark_timeout:' + method)); }, 15000);
      this.pending.set(id, { resolve, reject, timer });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }
  notify(method: string): void {
    this.proc.stdin.write(JSON.stringify({ jsonrpc: '2.0', method, params: {} }) + '\n');
  }
}
function value(message: Json, phase: string): Json {
  if (message.error) throw new Error('benchmark_' + phase + '_error:' + JSON.stringify(message.error).slice(0, 512));
  return (message.result ?? {}) as Json;
}
async function spawnReady(proc: any): Promise<void> {
  await new Promise<void>((resolve, reject) => { proc.once('spawn', resolve); proc.once('error', reject); });
}
async function stop(proc: any): Promise<void> {
  if (proc.exitCode !== null || proc.signalCode !== null) return;
  proc.kill();
  await new Promise<void>(resolve => { const timer = setTimeout(resolve, 2000); proc.once('close', () => { clearTimeout(timer); resolve(); }); });
}
async function sample(topology: Topology, sampleIndex: number): Promise<Json> {
  const started = performance.now();
  const childPidPath = join(fixture, `${topology.id.replace(/[^a-z0-9]+/gi, '-')}-${sampleIndex}.pid`);
  writeFileSync(configPath, JSON.stringify({ site_id: 'benchmark', mcpServers: { 'benchmark-echo': { command: 'node', args: [childPath, childPidPath] } } }), 'utf8');
  const proc = spawn(topology.command, [...topology.args, '--allowed-site-root', siteRoot, '--allowed-entrypoint-prefix', fixture, '--attach-timeout-ms', '3000', '--tool-call-timeout-ms', '3000', '--max-response-bytes', '1048576'], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  const client = new Client(proc);
  let memoryOverheadMs = 0;
  const readMemory = (childPid: number | null): MemorySnapshot => {
    const memoryStarted = performance.now();
    const snapshot = memorySnapshot(proc.pid, childPid);
    memoryOverheadMs += performance.now() - memoryStarted;
    return snapshot;
  };
  try {
    await spawnReady(proc);
    let t = performance.now();
    value(await client.call('initialize', { protocolVersion: '2024-11-05' }), 'initialize');
    const init = performance.now() - t;
    const initMemory = readMemory(null);
    client.notify('notifications/initialized');
    t = performance.now();
    value(await client.call('tools/list'), 'tools_list');
    const list = performance.now() - t;
    t = performance.now();
    const attachResult = value(await client.call('tools/call', { name: 'mcp_loader_attach_surface', arguments: { site_root: siteRoot, surface_id: 'benchmark-echo', entrypoint: childPath, args: [childPidPath] } }), 'attach');
    const attached = (attachResult.structuredContent ?? attachResult) as Json;
    const connectionId = String(attached.connection_id ?? '');
    if (!connectionId) throw new Error('benchmark_attach_missing_connection_id');
    const attach = performance.now() - t;
    const childPid = await waitForPid(childPidPath);
    if (childPid === null) throw new Error('benchmark_child_pid_missing');
    const attachMemory = readMemory(childPid);
    const warm: number[] = [];
    for (let i = 0; i < warmCount; i += 1) {
      t = performance.now();
      const callResult = value(await client.call('tools/call', { name: 'mcp_loader_call_tool', arguments: { connection_id: connectionId, tool_name: 'echo', arguments: { ordinal: i } } }), 'warm_call');
      if (!(callResult.structuredContent ?? callResult)) throw new Error('benchmark_warm_missing_result');
      warm.push(performance.now() - t);
    }
    const warmMemory = readMemory(childPid);
    await client.call('tools/call', { name: 'mcp_loader_detach', arguments: { connection_id: connectionId } }).catch(() => undefined);
    const snapshots = [initMemory, attachMemory, warmMemory];
    const loaderMemory = peak(snapshots.map(snapshot => snapshot.loader_bytes));
    const childMemory = peak(snapshots.map(snapshot => snapshot.child_bytes));
    const totalMemory = peak(snapshots.map(snapshot => {
      const values = [snapshot.loader_bytes, snapshot.child_bytes].filter((value): value is number => value !== null);
      return values.length === 0 ? null : values.reduce((sum, value) => sum + value, 0);
    }));
    return { init_ms: init, tools_list_ms: list, attach_ms: attach, warm_p50_ms: p95(warm, 0.5), warm_p95_ms: p95(warm), total_to_attach_ms: performance.now() - started - memoryOverheadMs, memory_metric: memoryMetric, loader_memory_peak_bytes: loaderMemory, child_memory_peak_bytes: childMemory, total_memory_peak_bytes: totalMemory };
  } finally {
    await stop(proc);
  }
}
function summary(rows: Json[]): Json {
  const phases = ['init_ms', 'tools_list_ms', 'attach_ms', 'warm_p50_ms', 'warm_p95_ms', 'total_to_attach_ms', 'loader_memory_peak_bytes', 'child_memory_peak_bytes', 'total_memory_peak_bytes'];
  const result: Json = { n: rows.length };
  for (const phase of phases) {
    const values = rows.map(row => row[phase]).filter((value): value is number => typeof value === 'number' && Number.isFinite(value));
    if (values.length === 0) { result[phase] = null; continue; }
    result[phase] = { p50: Number(p95(values, 0.5).toFixed(3)), p95: Number(p95(values).toFixed(3)) };
  }
  return result;
}
async function main(): Promise<void> {
  const reports: Json[] = [];
  try {
    for (const topology of topologies()) {
      const rows: Json[] = [];
      for (let i = 0; i < n; i += 1) rows.push(await sample(topology, i));
      reports.push({ topology: topology.id, loader: topology.loader, child: topology.child, samples: n, warm_calls_per_sample: warmCount, memory_metric: memoryMetric, summary: summary(rows) });
    }
    console.log(JSON.stringify({ schema: 'narada.mcp_loader.benchmark.v1', workload: 'initialize -> tools/list -> attach explicit stdio echo child -> repeated tools/call -> detach', bounded: true, reports }, null, 2));
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}
main().catch(error => { rmSync(fixture, { recursive: true, force: true }); console.error(JSON.stringify({ schema: 'narada.mcp_loader.benchmark.v1', status: 'failed', error: String(error).slice(0, 512) })); process.exitCode = 1; });