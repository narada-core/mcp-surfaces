import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const extension = process.platform === 'win32' ? '.exe' : '';
const samples = positiveInteger(process.env.NARADA_LIFECYCLE_BENCHMARK_SAMPLES ?? '12', 'samples');
const warmCalls = positiveInteger(process.env.NARADA_LIFECYCLE_BENCHMARK_WARM_CALLS ?? '24', 'warm_calls');
const executable = (name) => join(packageRoot, 'dist', 'native', `${name}${extension}`);
const percentile = (values, fraction) => {
  const ordered = [...values].sort((a, b) => a - b);
  return ordered[Math.min(ordered.length - 1, Math.ceil(ordered.length * fraction) - 1)];
};
const rpc = (id, method, params = {}) => JSON.stringify({ jsonrpc: '2.0', id, method, params });
const call = (id, name, args = {}) => rpc(id, 'tools/call', { name, arguments: args });
function positiveInteger(value, name) {
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < 1) throw new Error(`benchmark_invalid_${name}`);
  return number;
}
function run(exe, args, input) {
  const started = performance.now();
  const result = spawnSync(exe, args, { input, encoding: 'utf8', windowsHide: true });
  const elapsed = performance.now() - started;
  assert.equal(result.status, 0, `${exe}: ${result.stderr}`);
  const lines = String(result.stdout).trim().split(/\r?\n/).filter(Boolean).map(JSON.parse);
  return { elapsed, lines };
}
function artifact(path) {
  const stat = statSync(path);
  return { path, bytes: stat.size, sha256: createHash('sha256').update(readFileSync(path)).digest('hex') };
}
function benchmarkSurface(id, executableName, warmTool) {
  const siteRoot = mkdtempSync(join(tmpdir(), `narada-native-benchmark-${id}-`));
  const exe = executable(executableName);
  try {
    run(exe, ['--prepare', '--site-root', siteRoot], '');
    const cold = [];
    const warm = [];
    for (let sample = 0; sample < samples; sample += 1) {
      const input = [
        rpc(1, 'initialize', {}),
        rpc(2, 'tools/list', {}),
        call(3, warmTool),
      ].join('\n') + '\n';
      const result = run(exe, ['--site-root', siteRoot], input);
      assert.equal(result.lines[0].result.serverInfo.name.length > 0, true);
      assert.ok(Array.isArray(result.lines[1].result.tools));
      cold.push(result.elapsed);
      const warmInput = Array.from({ length: warmCalls }, (_, index) => call(index + 10, warmTool)).join('\n') + '\n';
      const warmResult = run(exe, ['--site-root', siteRoot], warmInput);
      assert.equal(warmResult.lines.length, warmCalls);
      warm.push(warmResult.elapsed / warmCalls);
    }
    return {
      id,
      executable: artifact(exe),
      samples,
      warm_calls: warmCalls,
      init_p95_ms: percentile(cold, 0.95),
      warm_call_p95_ms: percentile(warm, 0.95),
      init_samples_ms: cold,
      warm_call_samples_ms: warm,
      protocol: 'initialize/tools-list/tools-call',
    };
  } finally {
    rmSync(siteRoot, { recursive: true, force: true });
  }
}
const reports = [
  benchmarkSurface('task-rust', 'narada-task-lifecycle-mcp', 'task_lifecycle_doctor'),
  benchmarkSurface('work-rust', 'narada-work-lifecycle-mcp', 'work_lifecycle_doctor'),
];
process.stdout.write(JSON.stringify({ schema: 'narada.mcp_lifecycle_native.benchmark.v1', status: 'passed', samples, warm_calls: warmCalls, reports }) + '\n');