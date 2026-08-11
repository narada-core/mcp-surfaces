import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';
import { buildWorkspaceArtifactManifest, fingerprintWorkspaceArtifactManifest, type ArtifactFingerprint } from '../src/workspace-artifact-manifest.js';
import { MCP_RUNTIME_CONTRACT_VERSION } from '../src/materialization-contract.js';
import { requireNativeArtifact } from '../src/native-artifact.js';

type JsonRecord = Record<string, any>;
type RuntimeName = 'bun' | 'node' | 'deno';
type RuntimeCommand = { executable: string; runtime_args: string[] };
type ProxyImplementation = 'javascript' | 'native';
type ChildRuntime = RuntimeName | 'native_applet';
type Topology = { id: string; proxy: ProxyImplementation; proxyRuntime: RuntimeName | 'native'; childRuntime: ChildRuntime; childApplet?: string; nativeVariant?: string };
type ProcessMemory = { pid: number; name: string; private_bytes: number; working_set_bytes: number };
type Sample = {
  ordinal: number;
  phases: {
    process_spawn_ms: number;
    initialize_roundtrip_ms: number;
    tools_list_roundtrip_ms: number;
    status_roundtrip_ms: number;
    cold_start_to_initialize_ms: number;
  };
  memory: { private_bytes: number | null; working_set_bytes: number | null; processes: ProcessMemory[] };
  lifecycle: { exit_code: number | null; protocol_ok: boolean; leaked_processes: number };
  metrics: JsonRecord;
};
type Surface = {
  id: string;
  entrypoint: string;
  manifestPath: string;
  nativeVariants?: Record<string, { entrypoint: string; manifestPath: string }>;
  workingDirectory: string;
  childArgs: string[];
};
type FilesystemSurface = Surface & {
  filesystem: {
    root: string;
    primaryNeedle: string;
    secondaryNeedle: string;
    primaryFile: string;
    fileCount: number;
    linesPerFile: number;
    totalBytes: number;
    primaryFileCount: number;
    secondaryFileCount: number;
  };
};
type GitSurface = Surface & {
  git: {
    root: string;
    head: string;
    changedFile: string;
  };
};
type WorkloadTopology = {
  id: string;
  status: 'measured' | 'skipped' | 'failed';
  reason?: string;
  error?: string;
  samples: Sample[];
  summary?: JsonRecord;
};
type WorkloadReport = {
  id: string;
  description: string;
  configuration: JsonRecord;
  topologies: WorkloadTopology[];
  gates: JsonRecord[];
  verdict: JsonRecord;
};

const workspaceRoot = resolve(fileURLToPath(new URL('../../../../', import.meta.url)));
const root = mkdtempSync(join(tmpdir(), 'mcp-runtime-strong-'));
const keepArtifacts = process.env['NARADA_MCP_STRONG_KEEP_ARTIFACTS'] === '1';
const fixtureRoot = join(root, 'representative-fixture');
const diagnosticsRoot = join(root, 'diagnostics');
mkdirSync(fixtureRoot, { recursive: true });
mkdirSync(diagnosticsRoot, { recursive: true });
const reportId = `mcp-runtime-strong-${new Date().toISOString().replace(/[-:.TZ]/g, '').slice(0, 14)}`;
const bunProxyPath = fileURLToPath(new URL('../dist/src/main.js', import.meta.url));
const runtimeProxyPackageRoot = join(workspaceRoot, 'packages', 'shared', 'mcp-runtime-proxy');
const nativeProxyPath = process.env['NARADA_MCP_NATIVE_PROXY_PATH']?.trim() || requireNativeArtifact(runtimeProxyPackageRoot, 'narada-mcp-runtime.exe');
const nativeRhaiFilesystemPath = process.env['NARADA_MCP_NATIVE_RHAI_FILESYSTEM_PATH']?.trim() || requireNativeArtifact(runtimeProxyPackageRoot, 'narada-mcp-rhai-filesystem.exe');
const dotnetFilesystemPath = join(workspaceRoot, 'packages', 'local-filesystem-mcp', 'native-dotnet', 'publish', 'narada-filesystem-dotnet.exe');

function selectionValues(value: string | undefined): string[] | undefined {
  return value?.split(/[\s,]+/).map((item) => item.trim()).filter(Boolean);
}

function parseArgs(argv: string[]) {
  const result = {
    outputDir: undefined as string | undefined,
    samples: positiveInteger(process.env['NARADA_MCP_STRONG_SAMPLES'] ?? '8', 'samples'),
    loadRepetitions: positiveInteger(process.env['NARADA_MCP_STRONG_LOAD_REPETITIONS'] ?? '8', 'load_repetitions'),
    soakCycles: positiveInteger(process.env['NARADA_MCP_STRONG_SOAK_CYCLES'] ?? '200', 'soak_cycles'),
    soakWarmCalls: positiveInteger(process.env['NARADA_MCP_STRONG_SOAK_WARM_CALLS'] ?? '2000', 'soak_warm_calls'),
    filesystemFiles: positiveInteger(process.env['NARADA_MCP_STRONG_FILESYSTEM_FILES'] ?? '2048', 'filesystem_files'),
    filesystemLines: positiveInteger(process.env['NARADA_MCP_STRONG_FILESYSTEM_LINES'] ?? '64', 'filesystem_lines'),
    filesystemConcurrent: positiveInteger(process.env['NARADA_MCP_STRONG_FILESYSTEM_CONCURRENT'] ?? '8', 'filesystem_concurrent'),
    workloads: selectionValues(process.env['NARADA_MCP_STRONG_WORKLOADS']),
    topologies: selectionValues(process.env['NARADA_MCP_STRONG_TOPOLOGIES']),
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--output-dir') result.outputDir = argv[++index];
    else if (arg === '--samples') result.samples = positiveInteger(argv[++index], 'samples');
    else if (arg === '--load-repetitions') result.loadRepetitions = positiveInteger(argv[++index], 'load_repetitions');
    else if (arg === '--soak-cycles') result.soakCycles = positiveInteger(argv[++index], 'soak_cycles');
    else if (arg === '--soak-warm-calls') result.soakWarmCalls = positiveInteger(argv[++index], 'soak_warm_calls');
    else if (arg === '--filesystem-files') result.filesystemFiles = positiveInteger(argv[++index], 'filesystem_files');
    else if (arg === '--filesystem-lines') result.filesystemLines = positiveInteger(argv[++index], 'filesystem_lines');
    else if (arg === '--filesystem-concurrent') result.filesystemConcurrent = positiveInteger(argv[++index], 'filesystem_concurrent');
    else if (arg === '--workloads') result.workloads = selectionValues(argv[++index]);
    else if (arg === '--topologies') result.topologies = selectionValues(argv[++index]);
    else throw new Error(`strong_benchmark_unknown_argument:${arg}`);
  }
  return result;
}

function positiveInteger(value: string | undefined, name: string): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new Error(`strong_benchmark_invalid_value:${name}`);
  return parsed;
}

const args = parseArgs(process.argv.slice(2));

function commandVersion(command: RuntimeCommand): string | null {
  const versionArgs = command.runtime_args[0] === 'run' ? ['--version'] : [...command.runtime_args, '--version'];
  const result = spawnSync(command.executable, versionArgs, { encoding: 'utf8', windowsHide: true, timeout: 10_000, stdio: ['ignore', 'pipe', 'pipe'] });
  if (result.status !== 0) return null;
  return `${String(result.stdout ?? '').trim()} ${String(result.stderr ?? '').trim()}`.trim().slice(0, 200) || null;
}

function availableCommand(runtime: RuntimeName): RuntimeCommand | null {
  const own = basename(process.execPath).toLowerCase();
  const candidates = runtime === 'bun'
    ? [own.includes('bun') ? process.execPath : 'bun']
    : runtime === 'node'
      ? [own.includes('node') ? process.execPath : 'node']
      : [process.env['NARADA_MCP_BENCHMARK_DENO']?.trim() || '', 'deno'];
  const runtimeArgs = runtime === 'deno' ? ['run', '--allow-all', '--no-config', '--node-modules-dir=manual'] : [];
  for (const executable of candidates) {
    if (!executable) continue;
    const command = { executable, runtime_args: runtimeArgs };
    if (commandVersion(command) !== null) return command;
  }
  return null;
}

function writeFilesystemSearchFixture(): FilesystemSurface {
  const hayRoot = join(root, 'filesystem-hay');
  const fileCount = args.filesystemFiles;
  const linesPerFile = Math.max(32, args.filesystemLines);
  const primaryNeedle = 'NARADA_FILESYSTEM_BENCHMARK_PRIMARY_NEEDLE';
  const secondaryNeedle = 'NARADA_FILESYSTEM_BENCHMARK_SECONDARY_NEEDLE';
  const shardCount = Math.min(32, Math.max(1, Math.ceil(fileCount / 64)));
  const filler = 'haystack-' + '0123456789abcdef'.repeat(24);
  const primaryFiles: string[] = [];
  const secondaryFiles: string[] = [];
  let primaryFile = '';
  let totalBytes = 0;

  for (let index = 0; index < fileCount; index += 1) {
    const shard = join(hayRoot, 'shard-' + String(index % shardCount).padStart(2, '0'));
    mkdirSync(shard, { recursive: true });
    const filePath = join(shard, 'hay-' + String(index).padStart(5, '0') + '.txt');
    const hasPrimaryNeedle = index % 64 === 0;
    const hasSecondaryNeedle = index % 128 === 0;
    if (hasPrimaryNeedle) {
      primaryFiles.push(filePath);
      if (!primaryFile) primaryFile = filePath;
    }
    if (hasSecondaryNeedle) secondaryFiles.push(filePath);
    const content = Array.from({ length: linesPerFile }, (_unused, line) => {
      if (line === 17 && hasPrimaryNeedle) return primaryNeedle + ' file=' + index + ' ' + filler + '\n';
      if (line === 29 && hasSecondaryNeedle) return secondaryNeedle + ' file=' + index + ' ' + filler + '\n';
      return 'file=' + index + ' line=' + line + ' ' + filler + '\n';
    }).join('');
    writeFileSync(filePath, content, 'utf8');
    totalBytes += Buffer.byteLength(content);
  }

  const manifestPath = join(root, 'filesystem-artifact-manifest.json');
  const nativeManifestPath = join(root, 'native-filesystem-artifact-manifest.json');
  const rhaiManifestPath = join(root, 'rhai-filesystem-artifact-manifest.json');
  const dotnetManifestPath = join(root, 'dotnet-filesystem-artifact-manifest.json');
  buildWorkspaceArtifactManifest({
    workspaceRoot,
    packageRoots: [
      join(workspaceRoot, 'packages', 'local-filesystem-mcp'),
      join(workspaceRoot, 'packages', 'shared', 'mcp-fabric-contracts'),
      join(workspaceRoot, 'packages', 'shared', 'mcp-transport'),
    ],
    outputPath: manifestPath,
  });
  const nativeVariants: NonNullable<Surface['nativeVariants']> = {
    rust: {
      entrypoint: nativeProxyPath,
      manifestPath: writeSyntheticManifest([nativeProxyPath], nativeManifestPath),
    },
    rhai: {
      entrypoint: nativeRhaiFilesystemPath,
      manifestPath: writeSyntheticManifest([nativeRhaiFilesystemPath], rhaiManifestPath),
    },
  };
  if (existsSync(dotnetFilesystemPath)) {
    nativeVariants.dotnet = {
      entrypoint: dotnetFilesystemPath,
      manifestPath: writeSyntheticManifest([dotnetFilesystemPath], dotnetManifestPath),
    };
  }
  return {
    id: 'local-filesystem-search',
    entrypoint: join(workspaceRoot, 'packages', 'local-filesystem-mcp', 'dist', 'src', 'main.js'),
    manifestPath,
    nativeVariants,
    workingDirectory: hayRoot,
    childArgs: ['--mode', 'read', '--allowed-root', hayRoot],
    filesystem: {
      root: hayRoot,
      primaryNeedle,
      secondaryNeedle,
      primaryFile,
      fileCount,
      linesPerFile,
      totalBytes,
      primaryFileCount: primaryFiles.length,
      secondaryFileCount: secondaryFiles.length,
    },
  };
}

function filesystemStructuredContent(response: JsonRecord, operation: string): JsonRecord {
  const value = response.result?.structuredContent as JsonRecord | undefined;
  assert.ok(value && typeof value.schema === 'string', operation + ':missing_structured_content');
  return value;
}

async function runFilesystemSearchLoad(surface: FilesystemSurface): Promise<WorkloadReport> {
  const requiredTools = ['fs_doctor', 'fs_grep_search', 'fs_glob_search', 'fs_file_metrics', 'fs_read_file_range', 'fs_stat'];
  const reports: WorkloadTopology[] = [];
  for (const topology of orderedSelectedTopologies(filesystemTopologies, 'NARADA_MCP_STRONG_REVERSE_SEARCH_TOPOLOGIES')) {
    const unavailable = topologyAvailable(topology);
    if (unavailable) { reports.push({ id: topology.id, status: 'skipped', reason: unavailable, samples: [] }); continue; }
    const samples: Sample[] = [];
    try {
      for (let ordinal = 0; ordinal < args.samples; ordinal += 1) {
        let session: Session | null = null;
        try {
          session = await openSession(topology, surface, 'filesystem-search-load', ordinal);
          const toolNames = session.tools.map((tool) => String(tool.name));
          for (const requiredTool of requiredTools) assert.ok(toolNames.includes(requiredTool), topology.id + ':missing_tool:' + requiredTool);
          const sequentialLatencies: number[] = [];
          const timedCall = async (name: string, tool: string, input: JsonRecord): Promise<{ value: JsonRecord; elapsed_ms: number }> => {
            const started = performance.now();
            const response = await call(session!, name + '-' + ordinal, tool, input);
            const elapsedMs = performance.now() - started;
            sequentialLatencies.push(elapsedMs);
            return { value: filesystemStructuredContent(response, tool), elapsed_ms: elapsedMs };
          };
          const doctor = await timedCall('doctor', 'fs_doctor', {});
          assert.equal(doctor.value.status, 'ok', topology.id + ':doctor_not_ok');
          const primary = await timedCall('primary', 'fs_grep_search', {
            path: surface.filesystem.root,
            pattern: surface.filesystem.primaryNeedle,
            output_mode: 'files_with_matches',
            limit: 100,
            cache_policy: 'bypass',
            timeout_ms: 60_000,
          });
          const primaryReturned = Number(primary.value.returned ?? 0);
          assert.ok(primaryReturned >= surface.filesystem.primaryFileCount, topology.id + ':primary_match_count:' + primaryReturned);
          const secondary = await timedCall('secondary', 'fs_grep_search', {
            path: surface.filesystem.root,
            pattern: surface.filesystem.secondaryNeedle,
            output_mode: 'count_matches',
            limit: 100,
            cache_policy: 'bypass',
            timeout_ms: 60_000,
          });
          const secondaryReturned = Number(secondary.value.returned ?? 0);
          assert.ok(secondaryReturned >= surface.filesystem.secondaryFileCount, topology.id + ':secondary_match_count:' + secondaryReturned);
          const content = await timedCall('content', 'fs_grep_search', {
            path: surface.filesystem.root,
            pattern: surface.filesystem.primaryNeedle,
            output_mode: 'content',
            limit: 20,
            cache_policy: 'bypass',
            timeout_ms: 60_000,
          });
          assert.ok(Number(content.value.returned ?? 0) > 0, topology.id + ':content_search_empty');
          const glob = await timedCall('glob', 'fs_glob_search', {
            directory: surface.filesystem.root,
            pattern: '**/*.txt',
            limit: 100,
            cache_policy: 'bypass',
            timeout_ms: 60_000,
          });
          assert.ok(Number(glob.value.returned ?? 0) > 0, topology.id + ':glob_empty');
          const metrics = await timedCall('metrics', 'fs_file_metrics', {
            directory: surface.filesystem.root,
            pattern: '**/*.txt',
            limit: 100,
            max_total_scan_bytes: 8_000_000,
            cache_policy: 'bypass',
            timeout_ms: 60_000,
          });
          assert.ok(Number(metrics.value.returned ?? 0) > 0, topology.id + ':metrics_empty');
          const readRange = await timedCall('read-range', 'fs_read_file_range', {
            path: surface.filesystem.primaryFile,
            start_line: 17,
            end_line: 18,
            timeout_ms: 60_000,
          });
          assert.ok(Number(readRange.value.returned_lines ?? 0) > 0, topology.id + ':read_range_empty');
          const stat = await timedCall('stat', 'fs_stat', { path: surface.filesystem.root });
          assert.equal(stat.value.type, 'directory', topology.id + ':stat_not_directory');

          const concurrentStarted = performance.now();
          const concurrent = await Promise.all(Array.from({ length: args.filesystemConcurrent }, async (_unused, index) => {
            const started = performance.now();
            const response = await call(session!, 'concurrent-' + ordinal + '-' + index, 'fs_grep_search', {
              path: surface.filesystem.root,
              pattern: index % 2 === 0 ? surface.filesystem.primaryNeedle : surface.filesystem.secondaryNeedle,
              output_mode: 'files_with_matches',
              limit: 100,
              cache_policy: 'bypass',
              timeout_ms: 60_000,
            });
            const value = filesystemStructuredContent(response, 'fs_grep_search');
            assert.ok(Number(value.returned ?? 0) > 0, topology.id + ':concurrent_search_empty:' + index);
            return performance.now() - started;
          }));
          const concurrentBatchMs = performance.now() - concurrentStarted;
          const close = await closeSession(session);
          const concurrentP95 = percentile(concurrent);
          samples.push(makeSample(session, ordinal, close, {
            fixture_file_count: surface.filesystem.fileCount,
            fixture_lines_per_file: surface.filesystem.linesPerFile,
            fixture_total_bytes: surface.filesystem.totalBytes,
            sequential_command_count: sequentialLatencies.length,
            sequential_command_latencies_ms: sequentialLatencies,
            sequential_command_p95_ms: percentile(sequentialLatencies),
            concurrent_command_count: concurrent.length,
            concurrent_command_latencies_ms: concurrent,
            concurrent_batch_ms: concurrentBatchMs,
            concurrent_command_p95_ms: concurrentP95,
            primary_files_returned: primaryReturned,
            secondary_files_returned: secondaryReturned,
            content_matches_returned: Number(content.value.returned ?? 0),
            glob_files_returned: Number(glob.value.returned ?? 0),
            metrics_files_returned: Number(metrics.value.returned ?? 0),
            read_lines_returned: Number(readRange.value.returned_lines ?? 0),
            filesystem_commands_ok: true,
          }));
          session = null;
        } finally {
          if (session) await closeSession(session).catch(() => undefined);
        }
      }
      const sequentialLatencies = samples.flatMap((sample) => sample.metrics.sequential_command_latencies_ms as number[]);
      const concurrentLatencies = samples.flatMap((sample) => sample.metrics.concurrent_command_latencies_ms as number[]);
      reports.push({
        id: topology.id,
        status: 'measured',
        samples,
        summary: {
          ...summary(samples),
          fixture_file_count: surface.filesystem.fileCount,
          fixture_lines_per_file: surface.filesystem.linesPerFile,
          fixture_total_bytes: surface.filesystem.totalBytes,
          sequential_command_count: samples[0]?.metrics.sequential_command_count ?? 0,
          concurrent_command_count: samples[0]?.metrics.concurrent_command_count ?? 0,
          sequential_command_p95_ms: percentile(sequentialLatencies),
          concurrent_batch_p95_ms: percentile(samples.map((sample) => Number(sample.metrics.concurrent_batch_ms))),
          concurrent_command_p95_ms: percentile(concurrentLatencies),
          filesystem_commands_ok: samples.every((sample) => sample.metrics.filesystem_commands_ok === true),
        },
      });
    } catch (error) { reports.push({ id: topology.id, status: 'failed', samples, error: 'filesystem_benchmark_error:' + String(error).slice(0, 2_000) }); }
  }
  const gates = reports.map((report) => {
    const name = report.id + '.filesystem_search_protocol_and_lifecycle';
    if (report.status === 'skipped') return { name, status: 'not_run', reason: report.reason ?? 'unavailable' };
    return booleanGate(name, report.status === 'measured' && report.summary?.filesystem_commands_ok === true && report.summary?.lifecycle_passed === true);
  });
  return {
    id: 'filesystem-search-load',
    description: 'Real local-filesystem MCP search workload over a deterministic large haystack: multiple grep modes, glob, file metrics, range read, stat, and concurrent repeated searches.',
    configuration: {
      samples: args.samples,
      file_count: surface.filesystem.fileCount,
      lines_per_file: surface.filesystem.linesPerFile,
      total_bytes: surface.filesystem.totalBytes,
      primary_needle_files: surface.filesystem.primaryFileCount,
      secondary_needle_files: surface.filesystem.secondaryFileCount,
      sequential_commands_per_sample: 8,
      concurrent_searches_per_sample: args.filesystemConcurrent,
      topologies: filesystemTopologies.map((topology) => topology.id),
      topology_definitions: filesystemTopologies.map((topology) => ({
        id: topology.id,
        proxy: topology.proxy,
        proxy_runtime: topology.proxyRuntime,
        child_runtime: topology.childRuntime,
        ...(topology.childApplet ? { child_applet: topology.childApplet } : {}),
        ...(topology.nativeVariant ? { native_variant: topology.nativeVariant } : {}),
      })),
    },
    topologies: reports,
    gates,
    verdict: workloadVerdict(reports, gates),
  };
}

async function runFilesystemWriteLoad(surface: FilesystemSurface): Promise<WorkloadReport> {
  const requiredTools = ['fs_doctor', 'fs_write_file', 'fs_str_replace_file', 'fs_replace_range', 'fs_read_file', 'fs_stat', 'fs_move_path', 'fs_create_directory', 'fs_rename_directory', 'fs_delete_directory'];
  const reports: WorkloadTopology[] = [];
  for (const topology of orderedSelectedTopologies(filesystemWriteTopologies, 'NARADA_MCP_STRONG_REVERSE_WRITE_TOPOLOGIES')) {
    const unavailable = topologyAvailable(topology);
    if (unavailable) { reports.push({ id: topology.id, status: 'skipped', reason: unavailable, samples: [] }); continue; }
    const samples: Sample[] = [];
    try {
      for (let ordinal = 0; ordinal < args.samples; ordinal += 1) {
        let session: Session | null = null;
        try {
          session = await openSession(topology, surface, 'filesystem-write-load', ordinal);
          const toolNames = session.tools.map((tool) => String(tool.name));
          for (const requiredTool of requiredTools) assert.ok(toolNames.includes(requiredTool), topology.id + ':missing_tool:' + requiredTool);
          const sequentialLatencies: number[] = [];
          const timedCall = async (name: string, tool: string, input: JsonRecord): Promise<JsonRecord> => {
            const started = performance.now();
            const response = await call(session!, name + '-' + ordinal, tool, input);
            sequentialLatencies.push(performance.now() - started);
            return filesystemStructuredContent(response, tool);
          };
          const doctor = await timedCall('doctor', 'fs_doctor', {});
          assert.equal(doctor.status, 'ok', topology.id + ':doctor_not_ok');
          const pathTag = topology.id.replace(/[^a-z0-9-]+/gi, '-');
          const relativePath = `benchmark-write-${pathTag}-${ordinal}.txt`;
          const content = `filesystem-write-benchmark-${ordinal}\n`;
          const write = await timedCall('write', 'fs_write_file', { path: relativePath, content, create_parent_directories: true });
          assert.equal(write.status, 'written', topology.id + ':write_not_written');
          const stringReplace = await timedCall('str-replace', 'fs_str_replace_file', { path: relativePath, old: content.trimEnd(), new: `string-replace-${ordinal}` });
          assert.equal(stringReplace.status, 'replaced', topology.id + ':str_replace_not_replaced');
          const rangeReplace = await timedCall('replace-range', 'fs_replace_range', { path: relativePath, start_line: 1, end_line: 1, replacement: `range-replace-${ordinal}` });
          assert.equal(rangeReplace.status, 'replaced_range', topology.id + ':replace_range_not_replaced');
          const read = await timedCall('read', 'fs_read_file', { path: relativePath, offset: 0, limit: 100 });
          assert.equal(read.content, `range-replace-${ordinal}`, topology.id + ':readback_mismatch');
          const stat = await timedCall('stat', 'fs_stat', { path: relativePath });
          assert.equal(stat.type, 'file', topology.id + ':stat_not_file');
          const directoryPath = `benchmark-directory-${pathTag}-${ordinal}`;
          const renamedDirectoryPath = `benchmark-renamed-directory-${pathTag}-${ordinal}`;
          const createDirectory = await timedCall('create-directory', 'fs_create_directory', { path: `${directoryPath}/nested`, recursive: true });
          assert.equal(createDirectory.status, 'created', topology.id + ':create_directory_not_created');
          const renameDirectory = await timedCall('rename-directory', 'fs_rename_directory', { from: directoryPath, to: renamedDirectoryPath });
          assert.equal(renameDirectory.status, 'moved', topology.id + ':rename_directory_not_moved');
          const nestedPath = `${renamedDirectoryPath}/moved-source.txt`;
          const nestedWrite = await timedCall('nested-write', 'fs_write_file', { path: nestedPath, content: `nested-${ordinal}\n`, create_parent_directories: false });
          assert.equal(nestedWrite.status, 'written', topology.id + ':nested_write_not_written');
          const movedPath = `benchmark-moved-${pathTag}-${ordinal}.txt`;
          const move = await timedCall('move', 'fs_move_path', { from: nestedPath, to: movedPath });
          assert.equal(move.status, 'moved', topology.id + ':move_not_moved');
          const deleteNestedDirectory = await timedCall('delete-nested-directory', 'fs_delete_directory', { path: `${renamedDirectoryPath}/nested`, recursive: true });
          assert.equal(deleteNestedDirectory.status, 'deleted', topology.id + ':delete_nested_directory_not_deleted');
          const deleteDirectory = await timedCall('delete-directory', 'fs_delete_directory', { path: renamedDirectoryPath, recursive: true });
          assert.equal(deleteDirectory.status, 'deleted', topology.id + ':delete_directory_not_deleted');
          const nonemptyDirectoryPath = `benchmark-nonempty-directory-${pathTag}-${ordinal}`;
          const createNonemptyDirectory = await timedCall('create-nonempty-directory', 'fs_create_directory', { path: nonemptyDirectoryPath, recursive: true });
          assert.equal(createNonemptyDirectory.status, 'created', topology.id + ':create_nonempty_directory_not_created');
          const nonemptyChildPath = `${nonemptyDirectoryPath}/child.txt`;
          const nonemptyWrite = await timedCall('nonempty-write', 'fs_write_file', { path: nonemptyChildPath, content: `nonempty-${ordinal}\n`, create_parent_directories: false });
          assert.equal(nonemptyWrite.status, 'written', topology.id + ':nonempty_write_not_written');
          const nonemptyRefusalStarted = performance.now();
          const nonemptyRefusal = await sendRequest(session.child, session.read, 'nonempty-delete-refusal-' + ordinal, 'tools/call', { name: 'fs_delete_directory', arguments: { path: nonemptyDirectoryPath } });
          const nonemptyRefusalCallMs = performance.now() - nonemptyRefusalStarted;
          assert.equal(nonemptyRefusal.error?.data?.code, 'delete_directory_not_empty', topology.id + ':nonempty_delete_refusal');
          const recursiveDelete = await timedCall('recursive-delete', 'fs_delete_directory', { path: nonemptyDirectoryPath, recursive: true });
          assert.equal(recursiveDelete.status, 'deleted', topology.id + ':recursive_delete_not_deleted');
          const refusalStarted = performance.now();
          const refusal = await sendRequest(session.child, session.read, 'write-refusal-' + ordinal, 'tools/call', { name: 'fs_write_file', arguments: { path: relativePath, content: 'unexpected\n', expected_sha256: 'deadbeef' } });
          const refusalCallMs = performance.now() - refusalStarted;
          assert.equal(refusal.error?.data?.code, 'fs_write_file_expected_sha256_mismatch', topology.id + ':expected_sha_refusal');
          const close = await closeSession(session);
          samples.push(makeSample(session, ordinal, close, {
            actual_entrypoint: surface.entrypoint,
            advertised_tool_count: session.tools.length,
            proxy_status_tool_present: session.tools.some((tool) => String(tool.name) === 'mcp_runtime_proxy_status'),
            write_ok: true,
            str_replace_ok: true,
            replace_range_ok: true,
            readback_ok: true,
            stat_ok: true,
            create_directory_ok: true,
            rename_directory_ok: true,
            nested_write_ok: true,
            move_ok: true,
            delete_nested_directory_ok: true,
            delete_directory_ok: true,
            nonempty_delete_refusal_ok: nonemptyRefusal.error?.data?.code === 'delete_directory_not_empty',
            recursive_delete_ok: true,
            expected_sha_refusal_ok: refusal.error?.data?.code === 'fs_write_file_expected_sha256_mismatch',
            sequential_command_count: sequentialLatencies.length,
            sequential_command_latencies_ms: sequentialLatencies,
            sequential_command_p95_ms: percentile(sequentialLatencies),
            expected_sha_refusal_call_ms: refusalCallMs,
            nonempty_delete_refusal_call_ms: nonemptyRefusalCallMs,
          }));
          session = null;
        } finally {
          if (session) await closeSession(session).catch(() => undefined);
        }
      }
      const sequentialLatencies = samples.flatMap((sample) => sample.metrics.sequential_command_latencies_ms as number[]);
      reports.push({
        id: topology.id,
        status: 'measured',
        samples,
        summary: {
          ...summary(samples),
          sequential_command_count: samples[0]?.metrics.sequential_command_count ?? 0,
          sequential_command_p95_ms: percentile(sequentialLatencies),
          expected_sha_refusal_p95_ms: percentile(samples.map((sample) => Number(sample.metrics.expected_sha_refusal_call_ms))),
          nonempty_delete_refusal_p95_ms: percentile(samples.map((sample) => Number(sample.metrics.nonempty_delete_refusal_call_ms))),
          filesystem_write_commands_ok: samples.every((sample) => sample.metrics.write_ok && sample.metrics.str_replace_ok && sample.metrics.replace_range_ok && sample.metrics.readback_ok && sample.metrics.stat_ok && sample.metrics.create_directory_ok && sample.metrics.rename_directory_ok && sample.metrics.nested_write_ok && sample.metrics.move_ok && sample.metrics.delete_nested_directory_ok && sample.metrics.delete_directory_ok && sample.metrics.nonempty_delete_refusal_ok && sample.metrics.recursive_delete_ok && sample.metrics.expected_sha_refusal_ok),
        },
      });
    } catch (error) { reports.push({ id: topology.id, status: 'failed', samples, error: 'filesystem_write_benchmark_error:' + String(error).slice(0, 2_000) }); }
  }
  const gates = reports.map((report) => {
    const name = report.id + '.filesystem_write_protocol_and_lifecycle';
    if (report.status === 'skipped') return { name, status: 'not_run', reason: report.reason ?? 'unavailable' };
    return booleanGate(name, report.status === 'measured' && report.summary?.filesystem_write_commands_ok === true && report.summary?.lifecycle_passed === true);
  });
  return {
    id: 'filesystem-write-load',
    description: 'Filesystem mutation workload over the governed write contract: write, exact/range edits, readback/stat, directory lifecycle, move, recursive delete, and stale-hash/nonempty-delete refusals.',
    configuration: { samples: args.samples, fixture_root: surface.filesystem.root, operations: ['fs_doctor', 'fs_write_file', 'fs_str_replace_file', 'fs_replace_range', 'fs_read_file', 'fs_stat', 'fs_create_directory', 'fs_rename_directory', 'fs_move_path', 'fs_delete_directory', 'fs_delete_directory(nonempty refusal)', 'fs_write_file(expected_sha256 refusal)'], topologies: filesystemWriteTopologies.map((topology) => topology.id) },
    topologies: reports,
    gates,
    verdict: workloadVerdict(reports, gates),
  };
}

function commandSpec(command: RuntimeCommand | null): JsonRecord | null {
  return command ? { executable: command.executable, runtime_args: command.runtime_args, child_invocation: [command.executable, ...command.runtime_args, '<entrypoint>'] } : null;
}

const runtimeCommands: Record<RuntimeName, RuntimeCommand | null> = {
  bun: availableCommand('bun'),
  node: availableCommand('node'),
  deno: availableCommand('deno'),
};

function fingerprint(path: string): ArtifactFingerprint {
  const bytes = readFileSync(path);
  const stat = statSync(path);
  return { path: resolve(path), sha256: createHash('sha256').update(bytes).digest('hex'), size: stat.size, mtime_ms: stat.mtimeMs };
}

function writeSyntheticManifest(paths: string[], outputPath: string): string {
  const unsigned = {
    schema: 'narada.workspace_artifact_manifest.v1',
    generated_at: new Date().toISOString(),
    workspace_root: root,
    packages: [],
    artifacts: paths.map(fingerprint).sort((left, right) => left.path.localeCompare(right.path)),
  };
  const manifest = { ...unsigned, manifest_fingerprint: fingerprintWorkspaceArtifactManifest(unsigned) };
  writeFileSync(outputPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
  return outputPath;
}

function writeRepresentativeFixture(): Surface {
  const modulePath = join(fixtureRoot, 'representative-tools.mjs');
  const dataRoot = join(fixtureRoot, 'data');
  const entrypoint = join(fixtureRoot, 'representative-server.mjs');
  const manifestPath = join(root, 'representative-artifact-manifest.json');
  mkdirSync(dataRoot, { recursive: true });
  writeFileSync(modulePath, [
    "const commonSchema = { type: 'object', additionalProperties: false, properties: { value: { type: 'string' }, metadata: { type: 'object', additionalProperties: true }, options: { type: 'array', items: { type: 'string' } } } };",
    'export function buildToolDefinitions() {',
    '  const tools = [{ name: \'fixture_catalog_summary\', description: \'Return deterministic catalog and dataset metadata.\', inputSchema: commonSchema }, { name: \'fixture_payload_echo\', description: \'Echo a payload and report its encoded size.\', inputSchema: commonSchema }];',
    '  for (let index = 0; index < 30; index++) {',
    '    tools.push({',
    '      name: `fixture_domain_operation_${String(index).padStart(2, \'0\')}`,',
    '      description: `Representative domain operation ${index} with realistic nested input.`,',
    '      inputSchema: {',
    '        ...commonSchema,',
    '        properties: {',
    '          ...commonSchema.properties,',
    '          entity: { type: \'object\', required: [\'id\', \'kind\'], properties: { id: { type: \'string\' }, kind: { type: \'string\' }, revision: { type: \'integer\' } } },',
    '          filter: { type: \'object\', properties: { status: { type: \'string\', enum: [\'active\', \'paused\', \'archived\'] }, tags: { type: \'array\', items: { type: \'string\' } } } },',
    '        },',
    '      },',
    '    });',
    '  }',
    '  return tools;',
    '}',
  ].join('\n'), 'utf8');
  for (let index = 0; index < 24; index += 1) {
    const record = { id: `fixture-${String(index).padStart(2, '0')}`, revision: 7, status: index % 3 === 0 ? 'paused' : 'active', tags: ['benchmark', `group-${index % 4}`], values: Array.from({ length: 64 }, (_unused, value) => value + index) };
    writeFileSync(join(dataRoot, `record-${String(index).padStart(2, '0')}.json`), `${JSON.stringify(record)}\n`, 'utf8');
  }
  writeFileSync(entrypoint, [
    "import { createHash } from 'node:crypto';",
    "import { readFileSync, readdirSync } from 'node:fs';",
    "import { dirname, join } from 'node:path';",
    "import { fileURLToPath } from 'node:url';",
    "import { buildToolDefinitions } from './representative-tools.mjs';",
    'const fixtureRoot = dirname(fileURLToPath(import.meta.url));',
    'const dataRoot = join(fixtureRoot, \'data\');',
    'const dataFiles = readdirSync(dataRoot).filter((name) => name.endsWith(\'.json\')).sort();',
    'const data = dataFiles.map((name) => JSON.parse(readFileSync(join(dataRoot, name), \'utf8\')));',
    'const dataBytes = dataFiles.reduce((total, name) => total + readFileSync(join(dataRoot, name)).byteLength, 0);',
    'const datasetDigest = createHash(\'sha256\').update(JSON.stringify(data)).digest(\'hex\');',
    'const tools = buildToolDefinitions();',
    'let buffer = \"\";',
    'process.stdin.setEncoding(\'utf8\');',
    'process.stdin.on(\'data\', (chunk) => { buffer += chunk; let end; while ((end = buffer.indexOf(\'\\n\')) >= 0) { const line = buffer.slice(0, end).trim(); buffer = buffer.slice(end + 1); if (!line) continue; const request = JSON.parse(line); let result; if (request.method === \'initialize\') result = { protocolVersion: \'2024-11-05\', capabilities: { tools: {}, resources: {} }, serverInfo: { name: \'representative-fixture\', version: \'1\' } }; else if (request.method === \'tools/list\') result = { tools }; else if (request.method === \'tools/call\') { const name = request.params?.name; const input = request.params?.arguments ?? {}; if (name === \'fixture_catalog_summary\') result = { content: [{ type: \'text\', text: JSON.stringify({ tool_count: tools.length, dataset_files: dataFiles.length, data_bytes: dataBytes, dataset_digest: datasetDigest }) }], structuredContent: { tool_count: tools.length, dataset_files: dataFiles.length, data_bytes: dataBytes, dataset_digest: datasetDigest } }; else result = { content: [{ type: \'text\', text: JSON.stringify({ tool: name, payload_bytes: JSON.stringify(input).length, value: input.value ?? null }) }] }; } else result = {}; process.stdout.write(JSON.stringify({ jsonrpc: \'2.0\', id: request.id, result }) + \'\\n\'); } });',
  ].join('\n'), 'utf8');
  return { id: 'representative-fixture', entrypoint, manifestPath: writeSyntheticManifest([entrypoint, modulePath, ...readdirSync(dataRoot).map((name) => join(dataRoot, name))], manifestPath), workingDirectory: fixtureRoot, childArgs: [] };
}

function writeRealSurface(): Surface {
  const realRoot = join(root, 'real-surface-root');
  mkdirSync(realRoot, { recursive: true });
  const packageRoots = [
    join(workspaceRoot, 'packages', 'structured-command-mcp'),
    join(workspaceRoot, 'packages', 'shared', 'mcp-fabric-contracts'),
    join(workspaceRoot, 'packages', 'shared', 'mcp-transport'),
    join(workspaceRoot, 'packages', 'shared', 'mcp-telemetry'),
  ];
  const manifestPath = join(root, 'structured-command-artifact-manifest.json');
  const nativeManifestPath = join(root, 'native-structured-command-artifact-manifest.json');
  buildWorkspaceArtifactManifest({ workspaceRoot, packageRoots, outputPath: manifestPath });
  return {
    id: 'structured-command',
    entrypoint: join(workspaceRoot, 'packages', 'structured-command-mcp', 'dist', 'src', 'main.js'),
    manifestPath,
    nativeVariants: {
      rust: {
        entrypoint: nativeProxyPath,
        manifestPath: writeSyntheticManifest([nativeProxyPath], nativeManifestPath),
      },
    },
    workingDirectory: realRoot,
    childArgs: ['--allowed-root', realRoot, '--allow-command', 'node'],
  };
}

function runFixtureGit(rootPath: string, gitArgs: string[]): string {
  const result = spawnSync('git', gitArgs, { cwd: rootPath, encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  if (result.status !== 0) throw new Error(`strong_benchmark_git_fixture_failed:${gitArgs.join(' ')}:${String(result.stderr ?? '').slice(0, 500)}`);
  return String(result.stdout ?? '').trim();
}

function writeGitSurface(): GitSurface {
  const repoRoot = join(root, 'git-surface-root');
  mkdirSync(repoRoot, { recursive: true });
  runFixtureGit(repoRoot, ['init', '-q']);
  runFixtureGit(repoRoot, ['config', 'user.email', 'mcp-benchmark@example.invalid']);
  runFixtureGit(repoRoot, ['config', 'user.name', 'MCP Benchmark']);
  const changedFile = join(repoRoot, 'src', 'shard-00', 'file-000.txt');
  for (let index = 0; index < 96; index += 1) {
    const shard = join(repoRoot, 'src', `shard-${String(index % 8).padStart(2, '0')}`);
    mkdirSync(shard, { recursive: true });
    const filePath = join(shard, `file-${String(index).padStart(3, '0')}.txt`);
    writeFileSync(filePath, Array.from({ length: 12 }, (_unused, line) => `file=${index} line=${line} benchmark-haystack\n`).join(''), 'utf8');
  }
  runFixtureGit(repoRoot, ['add', '.']);
  runFixtureGit(repoRoot, ['commit', '-qm', 'benchmark baseline']);
  writeFileSync(join(repoRoot, 'history.txt'), 'second commit\n', 'utf8');
  runFixtureGit(repoRoot, ['add', 'history.txt']);
  runFixtureGit(repoRoot, ['commit', '-qm', 'benchmark history']);
  writeFileSync(changedFile, `${readFileSync(changedFile, 'utf8')}changed-line\n`, 'utf8');
  writeFileSync(join(repoRoot, 'untracked.txt'), 'untracked benchmark file\n', 'utf8');
  const manifestPath = join(root, 'git-artifact-manifest.json');
  const nativeManifestPath = join(root, 'native-git-artifact-manifest.json');
  buildWorkspaceArtifactManifest({
    workspaceRoot,
    packageRoots: [
      join(workspaceRoot, 'packages', 'git-mcp'),
      join(workspaceRoot, 'packages', 'shared', 'mcp-fabric-contracts'),
      join(workspaceRoot, 'packages', 'shared', 'mcp-transport'),
    ],
    outputPath: manifestPath,
  });
  return {
    id: 'git',
    entrypoint: join(workspaceRoot, 'packages', 'git-mcp', 'dist', 'src', 'main.js'),
    manifestPath,
    nativeVariants: { rust: { entrypoint: nativeProxyPath, manifestPath: writeSyntheticManifest([nativeProxyPath], nativeManifestPath) } },
    workingDirectory: repoRoot,
    childArgs: ['--mode', 'read', '--allowed-root', repoRoot, '--output-root', repoRoot],
    git: { root: repoRoot, head: runFixtureGit(repoRoot, ['rev-parse', 'HEAD']), changedFile },
  };
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
      try { response = JSON.parse(line) as JsonRecord; } catch (error) { childError = new Error(`strong_benchmark_invalid_json:${String(error)}`); continue; }
      const resolver = waiting.get(response.id);
      if (resolver) { waiting.delete(response.id); resolver(response); } else ready.set(response.id, response);
    }
  });
  child.on('error', (error) => { childError = error; });
  return (id) => new Promise((resolveResponse, reject) => {
    const existing = ready.get(id);
    if (existing) { ready.delete(id); resolveResponse(existing); return; }
    const timeout = setTimeout(() => { waiting.delete(id); reject(childError ?? new Error(`strong_benchmark_response_timeout:${id}`)); }, 30_000);
    waiting.set(id, (value) => { clearTimeout(timeout); resolveResponse(value); });
  });
}

async function processMemory(pids: number[]): Promise<ProcessMemory[]> {
  if (process.platform !== 'win32') return [];
  const ids = [...new Set(pids.filter((pid) => Number.isSafeInteger(pid) && pid > 0))];
  if (!ids.length) return [];
  const script = `@(Get-Process -Id ${ids.join(',')} -ErrorAction SilentlyContinue | ForEach-Object { [pscustomobject]@{ pid=$_.Id; name=$_.ProcessName; private_bytes=$_.PrivateMemorySize64; working_set_bytes=$_.WorkingSet64 } }) | ConvertTo-Json -Compress`;
  const child = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
  let stdout = '';
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (chunk: string) => { stdout += chunk; });
  const code = await new Promise<number | null>((resolveCode) => child.on('close', resolveCode));
  if (code !== 0 || !stdout.trim()) return [];
  try {
    const parsed = JSON.parse(stdout.trim()) as ProcessMemory | ProcessMemory[];
    return (Array.isArray(parsed) ? parsed : [parsed]).map((value) => ({ pid: Number(value.pid), name: String(value.name), private_bytes: Number(value.private_bytes), working_set_bytes: Number(value.working_set_bytes) }));
  } catch { return []; }
}

async function waitForSpawn(child: ChildProcessWithoutNullStreams): Promise<number> {
  return new Promise((resolveSpawn, reject) => { child.once('spawn', () => resolveSpawn(performance.now())); child.once('error', reject); });
}

function sendRequest(child: ChildProcessWithoutNullStreams, read: (id: string | number) => Promise<JsonRecord>, id: string, method: string, params: JsonRecord): Promise<JsonRecord> {
  child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
  return read(id);
}

function proxyLaunch(topology: Topology, surface: Surface, child: RuntimeCommand | null, diagnostics: string, ordinal: number, scenario: string): { command: string; args: string[] } {
  const nativeApplet = topology.childRuntime === 'native_applet';
  if (nativeApplet) assert.equal(topology.proxy, 'native', topology.id + ':native_applet_requires_native_proxy');
  const nativeVariant = topology.nativeVariant ?? 'rust';
  const nativeChild = nativeApplet ? surface.nativeVariants?.[nativeVariant] : undefined;
  const childCommand = nativeApplet ? nativeChild?.entrypoint : child?.executable;
  const childPrefixArgs = nativeApplet ? [] : child?.runtime_args ?? [];
  const childEntrypoint = nativeApplet ? nativeChild?.entrypoint : surface.entrypoint;
  const artifactManifest = nativeApplet ? nativeChild?.manifestPath : surface.manifestPath;
  if (nativeApplet) assert.ok(nativeChild, topology.id + ':native_variant_unavailable:' + nativeVariant);
  assert.ok(childCommand, topology.id + ':missing_child_command');
  assert.ok(artifactManifest, topology.id + ':missing_artifact_manifest');
  const common = [
    '--surface-id', 'strong-' + scenario + '-' + topology.id + '-' + ordinal,
    '--artifact-manifest', artifactManifest,
    '--runtime-contract-version', String(MCP_RUNTIME_CONTRACT_VERSION),
    '--child-command', childCommand,
    ...(childPrefixArgs.length ? ['--child-prefix-args', JSON.stringify(childPrefixArgs)] : []),
    '--entrypoint', childEntrypoint,
    '--diagnostics-dir', diagnostics,
    '--orphan-grace-ms', '100',
    ...(nativeApplet ? ['--child-invocation-kind', 'native_applet', '--child-applet', topology.childApplet ?? 'filesystem'] : []),
    '--',
    ...surface.childArgs,
  ];
  if (topology.proxy === 'native') return { command: nativeProxyPath, args: ['proxy', ...common] };
  const proxy = runtimeCommands[topology.proxyRuntime];
  assert.ok(proxy);
  return { command: proxy.executable, args: [...proxy.runtime_args, bunProxyPath, ...common] };
}

type Session = {
  child: ChildProcessWithoutNullStreams;
  read: (id: string | number) => Promise<JsonRecord>;
  topology: Topology;
  surface: Surface;
  diagnostics: string;
  surfaceId: string;
  pids: number[];
  tools: JsonRecord[];
  memory: ProcessMemory[];
  phases: Sample['phases'];
  stderr: string;
};

const activeChildren = new Set<ChildProcessWithoutNullStreams>();

function selectedTopology(id: string): boolean { return !args.topologies?.length || args.topologies.includes(id); }

function orderedSelectedTopologies(topologies: Topology[], reverseEnvironmentVariable?: string): Topology[] {
  const selected = topologies.filter((candidate) => selectedTopology(candidate.id));
  return reverseEnvironmentVariable && process.env[reverseEnvironmentVariable] === '1'
    ? [...selected].reverse()
    : selected;
}

async function closeRawChild(child: ChildProcessWithoutNullStreams): Promise<void> {
  if (child.exitCode === null && child.signalCode === null) {
    try { child.stdin.destroy(); } catch {}
    try { child.kill(); } catch {}
  }
  await new Promise<void>((resolveClose) => {
    if (child.exitCode !== null || child.signalCode !== null) {
      resolveClose();
      return;
    }
    const timer = setTimeout(resolveClose, 2_000);
    child.once('close', () => { clearTimeout(timer); resolveClose(); });
  });
  activeChildren.delete(child);
}

async function openSession(topology: Topology, surface: Surface, scenario: string, ordinal: number): Promise<Session> {
  const childRuntime = topology.childRuntime === 'native_applet' ? null : runtimeCommands[topology.childRuntime];
  if (topology.childRuntime !== 'native_applet' && !childRuntime) throw new Error(topology.childRuntime + '_runtime_unavailable');
  if (topology.proxy === 'native' && (process.platform !== 'win32' || !existsSync(nativeProxyPath))) throw new Error('native_windows_artifact_unavailable');
  const diagnostics = join(diagnosticsRoot, scenario, `${topology.id}-${ordinal}-${Date.now()}`);
  mkdirSync(diagnostics, { recursive: true });
  const launch = proxyLaunch(topology, surface, childRuntime, diagnostics, ordinal, scenario);
  const started = performance.now();
  const child = spawn(launch.command, launch.args, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true, env: { ...process.env, DENO_NO_PROMPT: '1' } });
  activeChildren.add(child);
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk: string) => { stderr = `${stderr}${chunk}`.slice(-8_000); });
  try {
    const read = rpcReader(child);
    const spawnedAt = await waitForSpawn(child);
    const initializeSent = performance.now();
    const initialized = await sendRequest(child, read, 'initialize', 'initialize', { protocolVersion: '2024-11-05' });
    const initializedAt = performance.now();
    assert.equal(initialized.error, undefined, `${topology.id}:initialize:${stderr}`);
    const toolsStarted = performance.now();
    const toolsResponse = await sendRequest(child, read, 'tools', 'tools/list', {});
    const toolsFinished = performance.now();
    assert.equal(toolsResponse.error, undefined, `${topology.id}:tools:${stderr}`);
    const tools = Array.isArray(toolsResponse.result?.tools) ? toolsResponse.result.tools as JsonRecord[] : [];
    const statusStarted = performance.now();
    const statusResponse = await sendRequest(child, read, 'status', 'tools/call', { name: 'mcp_runtime_proxy_status', arguments: {} });
    const statusFinished = performance.now();
    assert.equal(statusResponse.error, undefined, `${topology.id}:status:${stderr}`);
    const liveness = statusResponse.result?.structuredContent?.liveness ?? {};
    const pids = [child.pid, Number(liveness.proxy_pid), Number(liveness.supervisor_pid), Number(liveness.server_pid)].filter((pid): pid is number => Number.isSafeInteger(pid) && pid > 0);
    const memory = await processMemory(pids);
    return {
      child, read, topology, surface, diagnostics, surfaceId: `strong-${scenario}-${topology.id}-${ordinal}`, pids, tools, memory, stderr,
      phases: { process_spawn_ms: spawnedAt - started, initialize_roundtrip_ms: initializedAt - initializeSent, tools_list_roundtrip_ms: toolsFinished - toolsStarted, status_roundtrip_ms: statusFinished - statusStarted, cold_start_to_initialize_ms: initializedAt - started },
    };
  } catch (error) {
    await closeRawChild(child);
    throw error;
  }
}

async function closeSession(session: Session): Promise<{ exitCode: number | null; leakedProcesses: ProcessMemory[] }> {
  try {
    session.child.stdin.end();
    const exitCode = await new Promise<number | null>((resolveExit) => {
      const timer = setTimeout(() => { try { session.child.kill(); } catch {} resolveExit(null); }, 15_000);
      session.child.once('close', (code) => { clearTimeout(timer); resolveExit(code); });
    });
    await new Promise((resolveWait) => setTimeout(resolveWait, 40));
    return { exitCode, leakedProcesses: await processMemory(session.pids) };
  } finally {
    activeChildren.delete(session.child);
  }
}

async function call(session: Session, id: string, name: string, input: JsonRecord): Promise<JsonRecord> {
  const response = await sendRequest(session.child, session.read, id, 'tools/call', { name, arguments: input });
  assert.equal(response.error, undefined, `${session.topology.id}:${name}:${session.stderr}`);
  return response;
}

function percentile(values: number[], fraction = 0.95): number | null {
  if (!values.length) return null;
  const ordered = [...values].sort((left, right) => left - right);
  return ordered[Math.min(ordered.length - 1, Math.ceil(ordered.length * fraction) - 1)]!;
}

function mean(values: number[]): number | null { return values.length ? values.reduce((total, value) => total + value, 0) / values.length : null; }

function summary(samples: Sample[]): JsonRecord {
  return {
    sample_count: samples.length,
    initialize_p95_ms: percentile(samples.map((sample) => sample.phases.cold_start_to_initialize_ms)),
    initialize_roundtrip_p95_ms: percentile(samples.map((sample) => sample.phases.initialize_roundtrip_ms)),
    tools_list_p95_ms: percentile(samples.map((sample) => sample.phases.tools_list_roundtrip_ms)),
    private_bytes_p95: percentile(samples.flatMap((sample) => sample.memory.private_bytes === null ? [] : [sample.memory.private_bytes])),
    leaked_processes: samples.reduce((total, sample) => total + sample.lifecycle.leaked_processes, 0),
    lifecycle_passed: samples.every((sample) => sample.lifecycle.protocol_ok && sample.lifecycle.leaked_processes === 0),
  };
}

function makeSample(session: Session, ordinal: number, close: { exitCode: number | null; leakedProcesses: ProcessMemory[] }, metrics: JsonRecord): Sample {
  return {
    ordinal,
    phases: session.phases,
    memory: { private_bytes: session.memory.length ? session.memory.reduce((total, value) => total + value.private_bytes, 0) : null, working_set_bytes: session.memory.length ? session.memory.reduce((total, value) => total + value.working_set_bytes, 0) : null, processes: session.memory },
    lifecycle: { exit_code: close.exitCode, protocol_ok: close.exitCode === 0, leaked_processes: close.leakedProcesses.length },
    metrics,
  };
}

const proxyTopologies: Topology[] = [
  { id: 'bun-bun', proxy: 'javascript', proxyRuntime: 'bun', childRuntime: 'bun' },
  { id: 'node-node', proxy: 'javascript', proxyRuntime: 'node', childRuntime: 'node' },
  { id: 'deno-deno', proxy: 'javascript', proxyRuntime: 'deno', childRuntime: 'deno' },
  { id: 'native-bun', proxy: 'native', proxyRuntime: 'native', childRuntime: 'bun' },
  { id: 'native-node', proxy: 'native', proxyRuntime: 'native', childRuntime: 'node' },
  { id: 'native-deno', proxy: 'native', proxyRuntime: 'native', childRuntime: 'deno' },
];

const filesystemTopologies: Topology[] = [
  ...proxyTopologies,
  { id: 'native-filesystem', proxy: 'native', proxyRuntime: 'native', childRuntime: 'native_applet', childApplet: 'filesystem', nativeVariant: 'rust' },
  { id: 'native-rhai-filesystem', proxy: 'native', proxyRuntime: 'native', childRuntime: 'native_applet', childApplet: 'rhai-filesystem', nativeVariant: 'rhai' },
  { id: 'native-dotnet-filesystem', proxy: 'native', proxyRuntime: 'native', childRuntime: 'native_applet', childApplet: 'filesystem', nativeVariant: 'dotnet' },
];

const filesystemWriteTopologies: Topology[] = [
  ...proxyTopologies,
  { id: 'native-filesystem-write', proxy: 'native', proxyRuntime: 'native', childRuntime: 'native_applet', childApplet: 'filesystem', nativeVariant: 'rust' },
  { id: 'native-rhai-filesystem-write', proxy: 'native', proxyRuntime: 'native', childRuntime: 'native_applet', childApplet: 'rhai-filesystem', nativeVariant: 'rhai' },
];

const gitTopologies: Topology[] = [
  { id: 'bun-git-node', proxy: 'javascript', proxyRuntime: 'bun', childRuntime: 'node' },
  { id: 'node-git-node', proxy: 'javascript', proxyRuntime: 'node', childRuntime: 'node' },
  { id: 'deno-git-node', proxy: 'javascript', proxyRuntime: 'deno', childRuntime: 'node' },
  { id: 'native-git-node', proxy: 'native', proxyRuntime: 'native', childRuntime: 'node' },
  { id: 'native-git-native', proxy: 'native', proxyRuntime: 'native', childRuntime: 'native_applet', childApplet: 'git', nativeVariant: 'rust' },
];

function topologyAvailable(topology: Topology): string | null {
  if (topology.childRuntime !== 'native_applet' && !runtimeCommands[topology.childRuntime]) return topology.childRuntime + '_runtime_unavailable';
  if (topology.childRuntime === 'native_applet' && topology.proxy !== 'native') return 'native_applet_requires_native_proxy';
  if (topology.proxy === 'native' && (process.platform !== 'win32' || !existsSync(nativeProxyPath))) return 'native_windows_artifact_unavailable';
  if (topology.childRuntime === 'native_applet' && topology.nativeVariant === 'dotnet' && !existsSync(dotnetFilesystemPath)) return 'dotnet_native_applet_unavailable';
  if (topology.childRuntime === 'native_applet' && topology.nativeVariant === 'rhai' && !existsSync(nativeRhaiFilesystemPath)) return 'rhai_native_applet_unavailable';
  if (topology.proxy !== 'native' && !runtimeCommands[topology.proxyRuntime]) return `${topology.proxyRuntime}_runtime_unavailable`;
  return null;
}

async function runRepresentative(surface: Surface): Promise<WorkloadReport> {
  const reports: WorkloadTopology[] = [];
  for (const topology of proxyTopologies.filter((candidate) => selectedTopology(candidate.id))) {
    const unavailable = topologyAvailable(topology);
    if (unavailable) { reports.push({ id: topology.id, status: 'skipped', reason: unavailable, samples: [] }); continue; }
    const samples: Sample[] = [];
    try {
      for (let ordinal = 0; ordinal < args.samples; ordinal += 1) {
        let session: Session | null = null;
        try {
          session = await openSession(topology, surface, 'representative', ordinal);
          const domainToolCount = session.tools.filter((tool) => String(tool.name) !== 'mcp_runtime_proxy_status').length;
          assert.ok(domainToolCount >= 20 && domainToolCount <= 50, `${topology.id}:representative_domain_tool_count:${domainToolCount}`);
          const catalog = await call(session, `catalog-${ordinal}`, 'fixture_catalog_summary', {});
          assert.equal(catalog.result?.structuredContent?.tool_count, domainToolCount, `${topology.id}:representative_catalog_tool_count`);
          const warmLatencies: number[] = [];
          for (let index = 0; index < 20; index += 1) {
            const started = performance.now();
            await call(session, `representative-${ordinal}-${index}`, 'fixture_domain_operation_00', { value: `value-${index}`, entity: { id: `entity-${index}`, kind: 'benchmark', revision: 7 }, filter: { status: 'active', tags: ['benchmark'] } });
            warmLatencies.push(performance.now() - started);
          }
          const close = await closeSession(session);
          samples.push(makeSample(session, ordinal, close, { advertised_tool_count: session.tools.length, domain_tool_count: domainToolCount, proxy_status_tool_present: session.tools.some((tool) => String(tool.name) === 'mcp_runtime_proxy_status'), catalog_ok: Boolean(catalog.result), warm_call_p95_ms: percentile(warmLatencies), dataset: catalog.result?.structuredContent ?? null }));
          session = null;
        } finally {
          if (session) await closeSession(session).catch(() => undefined);
        }
      }
      reports.push({ id: topology.id, status: 'measured', samples, summary: summary(samples) });
    } catch (error) { reports.push({ id: topology.id, status: 'failed', samples, error: `${String(error)}`.slice(0, 2_000) }); }
  }
  const gates = comparisonGates(reports, [
    ['native_bun_vs_bun_bun', 'native-bun', 'bun-bun'],
    ['native_node_vs_node_node', 'native-node', 'node-node'],
    ['native_deno_vs_deno_deno', 'native-deno', 'deno-deno'],
  ], { private_bytes_p95: 0.6, initialize_p95_ms: 1.0, warm_call_p95_ms: 1.05 });
  return { id: 'representative', description: '32-domain-tool surface plus one proxy-owned status tool, with imported schema definitions, 24 deterministic JSON records read during module initialization, and normal domain-shaped calls.', configuration: { samples: args.samples, domain_tool_count_expected: 32, proxy_owned_tool_count: 1, dataset_files: 24, gates: { private_bytes_ratio_at_most: 0.6, initialize_ratio_at_most: 1.0, warm_call_ratio_at_most: 1.05 } }, topologies: reports, gates, verdict: workloadVerdict(reports, gates) };
}

function payloads(): Array<{ id: string; bytes: number; value: string }> {
  return [{ id: 'small', bytes: 32, value: 'x'.repeat(32) }, { id: 'medium', bytes: 4_096, value: 'm'.repeat(4_096) }, { id: 'large', bytes: 65_536, value: 'l'.repeat(65_536) }];
}

async function loadProfile(session: Session, profile: { id: string; bytes: number; value: string }, ordinal: number): Promise<JsonRecord> {
  const sequential: number[] = [];
  for (let index = 0; index < args.loadRepetitions; index += 1) {
    const started = performance.now();
    await call(session, `seq-${ordinal}-${profile.id}-${index}`, 'fixture_payload_echo', { value: profile.value, metadata: { profile: profile.id, sequence: index }, options: ['deterministic', 'sequential'] });
    sequential.push(performance.now() - started);
  }
  const concurrent: number[] = [];
  const concurrentBatchDurations: number[] = [];
  for (let batch = 0; batch < 2; batch += 1) {
    const batchStarted = performance.now();
    const starts = new Map<string, number>();
    const pending: Array<Promise<JsonRecord>> = [];
    for (let index = 0; index < 8; index += 1) {
      const id = `con-${ordinal}-${profile.id}-${batch}-${index}`;
      starts.set(id, performance.now());
      pending.push(call(session, id, 'fixture_payload_echo', { value: profile.value, metadata: { profile: profile.id, batch, index }, options: ['deterministic', 'concurrent'] }).then((response) => { concurrent.push(performance.now() - starts.get(id)!); return response; }));
    }
    await Promise.all(pending);
    concurrentBatchDurations.push(performance.now() - batchStarted);
  }
  const concurrentBatchP95 = percentile(concurrentBatchDurations);
  return {
    payload_bytes: profile.bytes,
    sequential_p95_ms: percentile(sequential),
    concurrent_p95_ms: percentile(concurrent),
    concurrent_batch_p95_ms: concurrentBatchP95,
    concurrent_calls: concurrent.length,
    concurrent_throughput_calls_per_second: concurrentBatchP95 && concurrentBatchP95 > 0
      ? (concurrent.length / 2) / (concurrentBatchP95 / 1_000)
      : null,
  };
}

async function runPayloadLoad(surface: Surface): Promise<WorkloadReport> {
  const reports: WorkloadTopology[] = [];
  for (const topology of proxyTopologies.filter((candidate) => selectedTopology(candidate.id))) {
    const unavailable = topologyAvailable(topology);
    if (unavailable) { reports.push({ id: topology.id, status: 'skipped', reason: unavailable, samples: [] }); continue; }
    const samples: Sample[] = [];
    try {
      for (let ordinal = 0; ordinal < args.samples; ordinal += 1) {
        let session: Session | null = null;
        try {
          session = await openSession(topology, surface, 'payload-load', ordinal);
          const profileResults: JsonRecord = {};
          for (const profile of payloads()) profileResults[profile.id] = await loadProfile(session, profile, ordinal);
          const close = await closeSession(session);
          samples.push(makeSample(session, ordinal, close, { profiles: profileResults }));
          session = null;
        } finally {
          if (session) await closeSession(session).catch(() => undefined);
        }
      }
      reports.push({ id: topology.id, status: 'measured', samples, summary: { ...summary(samples), profiles: Object.fromEntries(payloads().map((profile) => [profile.id, { sequential_p95_ms: percentile(samples.map((sample) => sample.metrics.profiles[profile.id].sequential_p95_ms).filter(Number.isFinite)), concurrent_p95_ms: percentile(samples.map((sample) => sample.metrics.profiles[profile.id].concurrent_p95_ms).filter(Number.isFinite)) }])) } });
    } catch (error) { reports.push({ id: topology.id, status: 'failed', samples, error: `${String(error)}`.slice(0, 2_000) }); }
  }
  const gates: JsonRecord[] = [];
  for (const profile of payloads()) {
    for (const mode of ['sequential_p95_ms', 'concurrent_p95_ms']) {
      const baseline = reports.find((report) => report.id === 'node-node' && report.status === 'measured')?.summary?.profiles?.[profile.id]?.[mode] ?? null;
      const native = reports.find((report) => report.id === 'native-node' && report.status === 'measured')?.summary?.profiles?.[profile.id]?.[mode] ?? null;
      gates.push(gate(`native_node_vs_node_node.${profile.id}.${mode}`, native, baseline, 1.05, 1));
    }
  }
  return { id: 'payload-load', description: 'Small, medium, and large payloads measured sequentially and in eight-request concurrent batches over the representative surface. Native-vs-Node latency uses the greater of 1.05x baseline or baseline plus 1ms to account explicitly for fixed IPC/proxy overhead on tiny payloads.', configuration: { samples: args.samples, repetitions: args.loadRepetitions, payloads: payloads().map(({ id, bytes }) => ({ id, bytes })), concurrency: 8, gates: { native_node_latency_ratio_at_most: 1.05, fixed_latency_slack_ms: 1 } }, topologies: reports, gates, verdict: workloadVerdict(reports, gates) };
}

function linearSlope(values: number[]): number | null {
  if (values.length < 2) return null;
  const meanX = (values.length - 1) / 2;
  const meanY = mean(values)!;
  let numerator = 0;
  let denominator = 0;
  values.forEach((value, index) => { numerator += (index - meanX) * (value - meanY); denominator += (index - meanX) ** 2; });
  return denominator === 0 ? null : numerator / denominator;
}

async function runSoak(surface: Surface): Promise<WorkloadReport> {
  const soakTopologies = proxyTopologies.filter((topology) => (topology.id === 'node-node' || topology.id === 'native-node') && selectedTopology(topology.id));
  const reports: WorkloadTopology[] = [];
  const perCycle = Math.floor(args.soakWarmCalls / args.soakCycles);
  const remainder = args.soakWarmCalls % args.soakCycles;
  for (const topology of soakTopologies) {
    const unavailable = topologyAvailable(topology);
    if (unavailable) { reports.push({ id: topology.id, status: 'skipped', reason: unavailable, samples: [] }); continue; }
    const samples: Sample[] = [];
    let warmCallsCompleted = 0;
    try {
      for (let cycle = 0; cycle < args.soakCycles; cycle += 1) {
        let session: Session | null = null;
        try {
          session = await openSession(topology, surface, 'restart-soak', cycle);
          const callsThisCycle = perCycle + (cycle < remainder ? 1 : 0);
          const latencies: number[] = [];
          for (let index = 0; index < callsThisCycle; index += 1) {
            const started = performance.now();
            await call(session, `soak-${topology.id}-${cycle}-${index}`, 'fixture_domain_operation_00', { value: `soak-${cycle}-${index}`, entity: { id: 'soak', kind: 'benchmark', revision: cycle } });
            latencies.push(performance.now() - started);
            warmCallsCompleted += 1;
          }
          const close = await closeSession(session);
          samples.push(makeSample(session, cycle, close, { warm_calls: callsThisCycle, warm_call_p95_ms: percentile(latencies) }));
          session = null;
        } finally {
          if (session) await closeSession(session).catch(() => undefined);
        }
      }
      const privateValues = samples.map((sample) => sample.memory.private_bytes).filter((value): value is number => value !== null);
      const leaked = samples.reduce((total, sample) => total + sample.lifecycle.leaked_processes, 0);
      reports.push({ id: topology.id, status: 'measured', samples, summary: { cycles_completed: samples.length, cycles_expected: args.soakCycles, warm_calls_completed: warmCallsCompleted, warm_calls_expected: args.soakWarmCalls, private_bytes_first: privateValues[0] ?? null, private_bytes_last: privateValues.at(-1) ?? null, private_bytes_slope_per_cycle: linearSlope(privateValues), leaked_processes: leaked, initialize_p95_ms: percentile(samples.map((sample) => sample.phases.cold_start_to_initialize_ms)), lifecycle_passed: samples.every((sample) => sample.lifecycle.protocol_ok && sample.lifecycle.leaked_processes === 0) } });
    } catch (error) { reports.push({ id: topology.id, status: 'failed', samples, error: `${String(error)}`.slice(0, 2_000) }); }
  }
  const gates: JsonRecord[] = [];
  for (const report of reports) {
    gates.push(booleanGate(`${report.id}.all_cycles_completed`, report.summary?.cycles_completed === args.soakCycles));
    gates.push(booleanGate(`${report.id}.all_warm_calls_completed`, report.summary?.warm_calls_completed === args.soakWarmCalls));
    gates.push(booleanGate(`${report.id}.no_leaked_processes`, report.summary?.leaked_processes === 0));
    gates.push(booleanGate(`${report.id}.lifecycle_passed`, report.summary?.lifecycle_passed === true));
  }
  return { id: 'restart-soak', description: '200 cold restart cycles and 2,000 warm calls across the Node baseline and native Node proxy, with per-cycle memory and process-leak evidence.', configuration: { cycles: args.soakCycles, warm_calls: args.soakWarmCalls, topologies: soakTopologies.map((topology) => topology.id), gates: { cycles_complete: true, warm_calls_complete: true, leaked_processes_zero: true } }, topologies: reports, gates, verdict: workloadVerdict(reports, gates) };
}

async function runRealSurface(surface: Surface): Promise<WorkloadReport> {
  const topologies: Topology[] = [
    { id: 'bun-structured-node', proxy: 'javascript', proxyRuntime: 'bun', childRuntime: 'node' },
    { id: 'node-structured-node', proxy: 'javascript', proxyRuntime: 'node', childRuntime: 'node' },
    { id: 'deno-structured-node', proxy: 'javascript', proxyRuntime: 'deno', childRuntime: 'node' },
    { id: 'native-structured-node', proxy: 'native', proxyRuntime: 'native', childRuntime: 'node' },
    { id: 'native-structured-native', proxy: 'native', proxyRuntime: 'native', childRuntime: 'native_applet', childApplet: 'structured-command', nativeVariant: 'rust' },
  ];
  const reports: WorkloadTopology[] = [];
  for (const topology of orderedSelectedTopologies(topologies, 'NARADA_MCP_STRONG_REVERSE_STRUCTURED_TOPOLOGIES')) {
    const unavailable = topologyAvailable(topology);
    if (unavailable) { reports.push({ id: topology.id, status: 'skipped', reason: unavailable, samples: [] }); continue; }
    const samples: Sample[] = [];
    try {
      for (let ordinal = 0; ordinal < args.samples; ordinal += 1) {
        let session: Session | null = null;
        try {
          session = await openSession(topology, surface, 'real-structured-command', ordinal);
          const toolNames = session.tools.map((tool) => String(tool.name));
          assert.ok(toolNames.includes('structured_command_execution_policy_inspect'), topology.id + ':real_surface_tool_missing');
          const policyStarted = performance.now();
          const policy = await call(session, 'policy-' + ordinal, 'structured_command_execution_policy_inspect', {});
          const policyCallMs = performance.now() - policyStarted;
          const executionStarted = performance.now();
          const execution = await call(session, 'execute-' + ordinal, 'structured_command_execute', { command: 'node', args: ['-e', 'process.stdout.write("strong-real-surface")'], working_directory: surface.workingDirectory, timeout_ms: 5_000, wait_for_completion: true, test_scope: 'focused', expected_cost: 'low' });
          const executionCallMs = performance.now() - executionStarted;
          const close = await closeSession(session);
          samples.push(makeSample(session, ordinal, close, { actual_entrypoint: surface.entrypoint, advertised_tool_count: session.tools.length, proxy_status_tool_present: session.tools.some((tool) => String(tool.name) === 'mcp_runtime_proxy_status'), policy_inspect_ok: Boolean(policy.result), policy_call_ms: policyCallMs, safe_command_ok: Boolean(execution.result), execution_call_ms: executionCallMs, safe_command_response: execution.result ?? null }));
          session = null;
        } finally {
          if (session) await closeSession(session).catch(() => undefined);
        }
      }
      reports.push({ id: topology.id, status: 'measured', samples, summary: { ...summary(samples), actual_entrypoint: surface.entrypoint, policy_call_p95_ms: percentile(samples.map((sample) => sample.metrics.policy_call_ms)), execution_call_p95_ms: percentile(samples.map((sample) => sample.metrics.execution_call_ms)), safe_command_successes: samples.filter((sample) => sample.metrics.safe_command_ok).length } });
    } catch (error) { reports.push({ id: topology.id, status: 'failed', samples, error: `${String(error)}`.slice(0, 2_000) }); }
  }
  const gates: JsonRecord[] = reports.map((report) => {
    const name = `${report.id}.real_surface_protocol_and_tool_call`;
    if (report.status === 'skipped') return { name, status: 'not_run', reason: report.reason ?? 'unavailable' };
    return booleanGate(name, report.status === 'measured' && report.samples.every((sample) => sample.lifecycle.protocol_ok && sample.metrics.policy_inspect_ok && sample.metrics.safe_command_ok && sample.lifecycle.leaked_processes === 0));
  });
  return { id: 'real-structured-command', description: 'Structured-command policy and safe argv execution across JavaScript and Rust-native applets.', configuration: { samples: args.samples, surface: surface.id, validation_tool: 'structured_command_execution_policy_inspect', safe_tool: 'structured_command_execute', command: ['node', '-e', 'process.stdout.write("strong-real-surface")'] }, topologies: reports, gates, verdict: workloadVerdict(reports, gates) };
}

async function runRealGitSurface(surface: GitSurface): Promise<WorkloadReport> {
  const reports: WorkloadTopology[] = [];
  for (const topology of orderedSelectedTopologies(gitTopologies, 'NARADA_MCP_STRONG_REVERSE_GIT_TOPOLOGIES')) {
    const unavailable = topologyAvailable(topology);
    if (unavailable) { reports.push({ id: topology.id, status: 'skipped', reason: unavailable, samples: [] }); continue; }
    const samples: Sample[] = [];
    try {
      for (let ordinal = 0; ordinal < args.samples; ordinal += 1) {
        let session: Session | null = null;
        try {
          session = await openSession(topology, surface, 'real-git', ordinal);
          const toolNames = session.tools.map((tool) => String(tool.name));
          assert.ok(toolNames.includes('git_policy_inspect'), topology.id + ':policy_tool_missing');
          assert.ok(toolNames.includes('git_status'), topology.id + ':status_tool_missing');
          const policyStarted = performance.now();
          const policy = await call(session, 'policy-' + ordinal, 'git_policy_inspect', {});
          const policyCallMs = performance.now() - policyStarted;
          const statusStarted = performance.now();
          const status = await call(session, 'status-' + ordinal, 'git_status', { working_directory: surface.git.root });
          const statusCallMs = performance.now() - statusStarted;
          const changedStarted = performance.now();
          const changed = await call(session, 'changed-' + ordinal, 'git_changed_summary', { working_directory: surface.git.root });
          const changedCallMs = performance.now() - changedStarted;
          const diffStarted = performance.now();
          const diff = await call(session, 'diff-' + ordinal, 'git_diff', { working_directory: surface.git.root, scope: 'working', limit: 4_000 });
          const diffCallMs = performance.now() - diffStarted;
          const logStarted = performance.now();
          const log = await call(session, 'log-' + ordinal, 'git_log', { working_directory: surface.git.root, limit: 10 });
          const logCallMs = performance.now() - logStarted;
          const showStarted = performance.now();
          const show = await call(session, 'show-' + ordinal, 'git_show', { working_directory: surface.git.root, commit: surface.git.head, include_patch: false });
          const showCallMs = performance.now() - showStarted;
          const refusalStarted = performance.now();
          const refusal = await sendRequest(session.child, session.read, 'refusal-' + ordinal, 'tools/call', { name: 'git_show', arguments: { working_directory: surface.git.root, commit: 'bad!commit', include_patch: false } });
          const refusalCallMs = performance.now() - refusalStarted;
          assert.equal(refusal.error?.data?.code, 'git_invalid_commitish', topology.id + ':invalid_commit_refusal');
          const close = await closeSession(session);
          samples.push(makeSample(session, ordinal, close, {
            actual_entrypoint: surface.entrypoint,
            advertised_tool_count: session.tools.length,
            proxy_status_tool_present: session.tools.some((tool) => String(tool.name) === 'mcp_runtime_proxy_status'),
            policy_inspect_ok: Boolean(policy.result?.structuredContent?.schema ?? policy.result),
            status_ok: status.result?.structuredContent?.schema === 'narada.git.status.v1',
            changed_summary_ok: changed.result?.structuredContent?.schema === 'narada.git.changed_summary.v1',
            diff_ok: diff.result?.structuredContent?.schema === 'narada.git.diff.v1',
            log_ok: log.result?.structuredContent?.schema === 'narada.git.log.v1',
            show_ok: show.result?.structuredContent?.schema === 'narada.git.show.v1',
            policy_call_ms: policyCallMs,
            status_call_ms: statusCallMs,
            changed_summary_call_ms: changedCallMs,
            diff_call_ms: diffCallMs,
            log_call_ms: logCallMs,
            show_call_ms: showCallMs,
            invalid_commit_refusal_ok: refusal.error?.data?.code === 'git_invalid_commitish',
            invalid_commit_refusal_call_ms: refusalCallMs,
            status_response: status.result?.structuredContent ?? null,
          }));
          session = null;
        } finally {
          if (session) await closeSession(session).catch(() => undefined);
        }
      }
      reports.push({ id: topology.id, status: 'measured', samples, summary: {
        ...summary(samples),
        actual_entrypoint: surface.entrypoint,
        policy_call_p95_ms: percentile(samples.map((sample) => sample.metrics.policy_call_ms)),
        status_call_p95_ms: percentile(samples.map((sample) => sample.metrics.status_call_ms)),
        changed_summary_call_p95_ms: percentile(samples.map((sample) => sample.metrics.changed_summary_call_ms)),
        diff_call_p95_ms: percentile(samples.map((sample) => sample.metrics.diff_call_ms)),
        log_call_p95_ms: percentile(samples.map((sample) => sample.metrics.log_call_ms)),
        show_call_p95_ms: percentile(samples.map((sample) => sample.metrics.show_call_ms)),
        invalid_commit_refusal_p95_ms: percentile(samples.map((sample) => sample.metrics.invalid_commit_refusal_call_ms)),
        protocol_successes: samples.filter((sample) => sample.metrics.policy_inspect_ok && sample.metrics.status_ok && sample.metrics.changed_summary_ok && sample.metrics.diff_ok && sample.metrics.log_ok && sample.metrics.show_ok && sample.metrics.invalid_commit_refusal_ok).length,
      } });
    } catch (error) { reports.push({ id: topology.id, status: 'failed', samples, error: `${String(error)}`.slice(0, 2_000) }); }
  }
  const gates: JsonRecord[] = reports.map((report) => {
    const name = `${report.id}.real_git_protocol_and_read_calls`;
    if (report.status === 'skipped') return { name, status: 'not_run', reason: report.reason ?? 'unavailable' };
    return booleanGate(name, report.status === 'measured' && report.samples.every((sample) => sample.lifecycle.protocol_ok && sample.metrics.policy_inspect_ok && sample.metrics.status_ok && sample.metrics.changed_summary_ok && sample.metrics.diff_ok && sample.metrics.log_ok && sample.metrics.show_ok && sample.metrics.invalid_commit_refusal_ok && sample.lifecycle.leaked_processes === 0));
  });
  return { id: 'real-git', description: 'Git read canary across JavaScript runtimes and the Rust-native applet: policy, status, dirty summary, diff, log, and commit metadata.', configuration: { samples: args.samples, surface: surface.id, tools: ['git_policy_inspect', 'git_status', 'git_changed_summary', 'git_diff', 'git_log', 'git_show'], repository_files: 96, mutated_file: surface.git.changedFile }, topologies: reports, gates, verdict: workloadVerdict(reports, gates) };
}

function gate(name: string, actual: number | null, baseline: number | null, limit: number, absoluteSlack = 0): JsonRecord {
  const comparable = actual !== null && baseline !== null && Number.isFinite(actual) && Number.isFinite(baseline) && baseline !== 0;
  const threshold = comparable ? Math.max(baseline! * limit, baseline! + absoluteSlack) : null;
  return { name, status: comparable ? actual! <= threshold! ? 'passed' : 'failed' : 'not_comparable', actual, baseline, limit_ratio: limit, absolute_slack_ms: absoluteSlack, threshold, ratio: comparable ? actual! / baseline! : null };
}

function booleanGate(name: string, passed: boolean): JsonRecord { return { name, status: passed ? 'passed' : 'failed', actual: passed, expected: true }; }

function comparisonGates(reports: WorkloadTopology[], pairs: Array<[string, string, string]>, limits: { private_bytes_p95: number; initialize_p95_ms: number; warm_call_p95_ms: number }): JsonRecord[] {
  const gates: JsonRecord[] = [];
  for (const [label, candidateId, baselineId] of pairs) {
    const candidate = reports.find((report) => report.id === candidateId && report.status === 'measured');
    const baseline = reports.find((report) => report.id === baselineId && report.status === 'measured');
    gates.push(gate(`${label}.private_bytes_p95`, candidate?.summary?.private_bytes_p95 ?? null, baseline?.summary?.private_bytes_p95 ?? null, limits.private_bytes_p95));
    gates.push(gate(`${label}.initialize_p95`, candidate?.summary?.initialize_p95_ms ?? null, baseline?.summary?.initialize_p95_ms ?? null, limits.initialize_p95_ms));
    const candidateWarm = candidate?.samples.map((sample) => sample.metrics.warm_call_p95_ms).filter(Number.isFinite) ?? [];
    const baselineWarm = baseline?.samples.map((sample) => sample.metrics.warm_call_p95_ms).filter(Number.isFinite) ?? [];
    gates.push(gate(`${label}.warm_call_p95`, percentile(candidateWarm), percentile(baselineWarm), limits.warm_call_p95_ms));
  }
  return gates;
}

function workloadVerdict(reports: WorkloadTopology[], gates: JsonRecord[]): JsonRecord {
  const failedReport = reports.some((report) => report.status === 'failed' || report.summary?.lifecycle_passed === false);
  const failedGate = gates.some((item) => item.status === 'failed');
  const comparableGate = gates.some((item) => item.status === 'passed' || item.status === 'failed');
  return { correctness: failedReport || gates.some((item) => item.name.includes('protocol') && item.status === 'failed') ? 'failed' : 'passed', performance: failedReport ? 'not_comparable' : failedGate ? 'performance_target_not_met' : comparableGate ? 'passed' : 'not_comparable' };
}

function htmlEscape(value: unknown): string { return String(value).replace(/[&<>"']/g, (character) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[character]!)); }

function htmlArtifact(report: JsonRecord): string {
  const embedded = JSON.stringify(report).replace(/</g, '\\u003c');
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Strong MCP runtime benchmark</title><style>:root{color-scheme:dark;font:14px/1.45 system-ui,sans-serif;background:#111827;color:#e5e7eb}body{margin:0;padding:24px;max-width:1400px;margin-inline:auto}h1,h2{margin:0 0 12px}.toolbar,.cards{display:grid;gap:12px}.toolbar{grid-template-columns:1fr 1fr auto;align-items:center;margin:16px 0}.cards{grid-template-columns:repeat(auto-fit,minmax(180px,1fr))}.card,section{background:#1f2937;border:1px solid #374151;border-radius:10px;padding:14px}.value{font-size:22px;font-weight:700}.muted{color:#9ca3af}.pass{color:#86efac}.fail{color:#fca5a5}.skip{color:#fde68a}table{width:100%;border-collapse:collapse;margin-top:10px}th,td{text-align:left;border-bottom:1px solid #374151;padding:7px}select,button{padding:8px;background:#111827;color:#e5e7eb;border:1px solid #4b5563;border-radius:6px}pre{white-space:pre-wrap;overflow:auto;max-height:600px}.wide{overflow:auto}</style></head><body><h1>Strong MCP runtime benchmark</h1><p>Offline artifact <code>${htmlEscape(report.report_id)}</code>. Workloads retain separate gates.</p><div class="toolbar"><select id="workload"></select><select id="topology"></select><button id="download">Download JSON</button></div><div id="summary" class="cards"></div><section><h2>Workload gates</h2><div id="gates" class="wide"></div></section><section><h2>Selected topology evidence</h2><pre id="detail"></pre></section><script id="benchmark-data" type="application/json">${embedded}</script><script>const report=JSON.parse(document.getElementById('benchmark-data').textContent),wsel=document.getElementById('workload'),tsel=document.getElementById('topology');const esc=v=>String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));const cls=v=>v==='passed'||v==='measured'?'pass':v==='failed'||v==='performance_target_not_met'?'fail':v==='skipped'||v==='not_comparable'?'skip':'';const fmt=v=>v==null?'—':typeof v==='number'?(Math.abs(v)>100000?Math.round(v).toLocaleString():v.toFixed(3)):esc(v);report.workloads.forEach(w=>{const o=document.createElement('option');o.value=w.id;o.textContent=w.id+' ('+w.verdict.performance+')';wsel.appendChild(o)});function render(){const w=report.workloads.find(x=>x.id===wsel.value)||report.workloads[0];const previousTopology=tsel.value;tsel.innerHTML='';w.topologies.forEach(t=>{const o=document.createElement('option');o.value=t.id;o.textContent=t.id+' ('+t.status+')';tsel.appendChild(o)});tsel.value=w.topologies.some(t=>t.id===previousTopology)?previousTopology:(w.topologies[0]?.id||'');const t=w.topologies.find(x=>x.id===tsel.value)||w.topologies[0];document.getElementById('gates').innerHTML='<table><tr><th>Gate</th><th>Status</th><th>Actual</th><th>Baseline/Expected</th><th>Ratio</th></tr>'+w.gates.map(g=>'<tr><td>'+esc(g.name)+'</td><td class="'+cls(g.status)+'">'+esc(g.status)+'</td><td>'+fmt(g.actual)+'</td><td>'+fmt(g.baseline??g.expected)+'</td><td>'+fmt(g.ratio)+'</td></tr>').join('')+'</table>';const s=t?.summary||{};document.getElementById('summary').innerHTML=[['workload',w.id],['topology',t?.id],['status',t?.status],['initialize p95 ms',s.initialize_p95_ms],['private p95 bytes',s.private_bytes_p95],['leaked processes',s.leaked_processes]].map(([k,v])=>'<div class="card"><div class="muted">'+esc(k)+'</div><div class="value '+cls(v)+'">'+fmt(v)+'</div></div>').join('');document.getElementById('detail').textContent=JSON.stringify({workload:w.configuration,topology:t},null,2)}wsel.addEventListener('change',render);tsel.addEventListener('change',render);document.getElementById('download').addEventListener('click',()=>{const a=document.createElement('a');a.href=URL.createObjectURL(new Blob([JSON.stringify(report,null,2)],{type:'application/json'}));a.download=report.report_id+'.json';a.click();URL.revokeObjectURL(a.href)});wsel.value=report.workloads[0]?.id;render();</script></body></html>`;
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

const representativeSurface = writeRepresentativeFixture();
const realSurface = writeRealSurface();
const gitSurface = writeGitSurface();

try {
  const workloadRequested = (id: string) => !args.workloads?.length || args.workloads.includes(id);
  const filesystemFixture = workloadRequested('filesystem-search-load') || workloadRequested('filesystem-write-load') ? writeFilesystemSearchFixture() : null;
  const filesystemSurface = workloadRequested('filesystem-search-load') ? filesystemFixture : null;
  const filesystemWriteSurface = workloadRequested('filesystem-write-load') && filesystemFixture
    ? { ...filesystemFixture, id: 'local-filesystem-write', childArgs: ['--mode', 'write', '--allowed-root', filesystemFixture.filesystem.root] }
    : null;
  const workloads = [
    workloadRequested('representative') ? await runRepresentative(representativeSurface) : null,
    workloadRequested('payload-load') ? await runPayloadLoad(representativeSurface) : null,
    workloadRequested('restart-soak') ? await runSoak(representativeSurface) : null,
    filesystemSurface ? await runFilesystemSearchLoad(filesystemSurface) : null,
    filesystemWriteSurface ? await runFilesystemWriteLoad(filesystemWriteSurface) : null,
    workloadRequested('real-structured-command') ? await runRealSurface(realSurface) : null,
    workloadRequested('real-git') ? await runRealGitSurface(gitSurface) : null,
  ].filter((workload): workload is WorkloadReport => workload !== null);
  const correctnessFailed = workloads.some((workload) => workload.verdict.correctness === 'failed');
  const performanceTargetNotMet = workloads.some((workload) => workload.verdict.performance === 'performance_target_not_met');
  const performanceNotComparable = workloads.some((workload) => workload.verdict.performance === 'not_comparable');
  const report: JsonRecord = {
    schema: 'narada.mcp_runtime_proxy.strong_benchmark_report.v1',
    report_id: reportId,
    generated_at: new Date().toISOString(),
    objective: 'Measure runtime behavior under representative, payload/load, restart/soak, filesystem read/write, structured-command, and Git read workloads without replacing the minimal attribution benchmark.',
    environment: { platform: process.platform, architecture: process.arch, runner: process.execPath, workspace_root: workspaceRoot, runtimes: Object.fromEntries(Object.entries(runtimeCommands).map(([name, command]) => [name, command ? commandVersion(command) : null])), runtime_commands: Object.fromEntries(Object.entries(runtimeCommands).map(([name, command]) => [name, commandSpec(command)])), native_artifact: process.platform === 'win32' && existsSync(nativeProxyPath), real_surfaces: [{ id: realSurface.id, entrypoint: realSurface.entrypoint }, { id: gitSurface.id, entrypoint: gitSurface.entrypoint }], diagnostics_root: keepArtifacts ? root : null },
    configuration: { samples: args.samples, load_repetitions: args.loadRepetitions, soak_cycles: args.soakCycles, soak_warm_calls: args.soakWarmCalls, filesystem_files: args.filesystemFiles, filesystem_lines: Math.max(32, args.filesystemLines), filesystem_concurrent: args.filesystemConcurrent, workloads: args.workloads ?? ['representative', 'payload-load', 'restart-soak', 'filesystem-search-load', 'filesystem-write-load', 'real-structured-command', 'real-git'], topologies: args.topologies ?? 'all', runtime_contract_version: MCP_RUNTIME_CONTRACT_VERSION },
    workloads,
    verdict: { correctness: correctnessFailed ? 'failed' : 'passed', performance: performanceTargetNotMet ? 'performance_target_not_met' : performanceNotComparable ? 'not_comparable' : 'passed', native_default: process.platform === 'win32' && existsSync(nativeProxyPath) ? 'default_when_available' : 'bun_fallback', deno_support: 'experimental_lane_only' },
  };
  const artifacts = writeArtifacts(report);
  const output = { ...report, artifacts: { json_path: artifacts.jsonPath, html_path: artifacts.htmlPath } };
  writeFileSync(artifacts.jsonPath, `${JSON.stringify(output, null, 2)}\n`, 'utf8');
  writeFileSync(artifacts.htmlPath, htmlArtifact(output), 'utf8');
  console.log(JSON.stringify({ schema: 'narada.mcp_runtime_proxy.strong_benchmark_complete.v1', report_id: report.report_id, json_path: artifacts.jsonPath, html_path: artifacts.htmlPath, verdict: report.verdict, workloads: workloads.map((workload) => ({ id: workload.id, verdict: workload.verdict, gates: workload.gates })) }));
  if (report.verdict.correctness === 'failed') process.exitCode = 1;
} finally {
  for (const child of activeChildren) { try { child.kill(); } catch {} }
  activeChildren.clear();
  if (!keepArtifacts) rmSync(root, { recursive: true, force: true });
}
