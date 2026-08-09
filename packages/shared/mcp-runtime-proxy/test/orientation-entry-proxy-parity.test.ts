import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  createTestProcessScope,
  spawnJsonlMcpServer,
} from '@narada-core/mcp-e2e-harness';
import { MCP_RUNTIME_CONTRACT_VERSION } from '../src/materialization-contract.js';
import { resolveNativeArtifact } from '../src/native-artifact.js';
import { fingerprintWorkspaceArtifactManifest } from '../src/workspace-artifact-manifest.js';
import {
  expectedOrientationCallAdmission,
  expectedOrientationEntryState,
  loadOrientationEntryConformanceCorpus,
  materializeOrientationEntryCase,
  type OrientationEntryConformanceCase,
} from './orientation-entry-conformance.js';

type ProxyKind = 'typescript' | 'rust';
type SurfaceKind = 'agent-context' | 'performative-work';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const typescriptProxy = fileURLToPath(new URL('../src/main.js', import.meta.url));
const nativeProxy = resolveNativeArtifact(packageRoot, 'narada-mcp-runtime.exe');
const corpus = loadOrientationEntryConformanceCorpus();
const forwardLogs: Record<ProxyKind, Record<SurfaceKind, string>> = {
  typescript: {} as Record<SurfaceKind, string>,
  rust: {} as Record<SurfaceKind, string>,
};

function artifact(path: string) {
  const stat = statSync(path);
  return {
    path,
    sha256: createHash('sha256').update(readFileSync(path)).digest('hex'),
    size: stat.size,
    mtime_ms: stat.mtimeMs,
  };
}

function writeManifest(path: string, root: string, childPath: string): void {
  const unsigned = {
    schema: 'narada.workspace_artifact_manifest.v1',
    generated_at: '2026-08-09T00:00:00.000Z',
    workspace_root: root,
    packages: [],
    artifacts: [artifact(childPath)],
  };
  writeFileSync(path, JSON.stringify({
    ...unsigned,
    manifest_fingerprint: fingerprintWorkspaceArtifactManifest(unsigned),
  }), 'utf8');
}

function fixtureChildSource(): string {
  return [
    "import { appendFileSync } from 'node:fs';",
    "let buffer = '';",
    "process.stdin.setEncoding('utf8');",
    "process.stdin.on('data', chunk => {",
    '  buffer += chunk;',
    "  const lines = buffer.split(/\\r?\\n/); buffer = lines.pop() ?? '';",
    '  for (const line of lines) {',
    '    if (!line.trim()) continue;',
    '    const request = JSON.parse(line);',
    "    if (process.env.NARADA_ORIENTATION_FORWARD_LOG) appendFileSync(process.env.NARADA_ORIENTATION_FORWARD_LOG, String(request.method ?? '') + '\\n', 'utf8');",
    '    if (request.id === undefined) continue;',
    "    const result = request.method === 'initialize'",
    "      ? { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'orientation-parity-fixture', version: '1' } }",
    "      : request.method === 'tools/list'",
    "        ? { tools: [{ name: 'work_perform', inputSchema: { type: 'object' } }, { name: 'agent_orientation_read', inputSchema: { type: 'object' } }] }",
    "        : { content: [{ type: 'text', text: 'forwarded:' + (request.params?.name ?? request.method) }], structuredContent: { schema: 'fixture.forwarded.v1', method: request.method, tool: request.params?.name } };",
    "    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\\n');",
    '  }',
    '});',
  ].join('\n');
}

function proxyLaunch(
  kind: ProxyKind,
  surface: SurfaceKind,
  commonArgs: string[],
  environment: NodeJS.ProcessEnv,
  scope: ReturnType<typeof createTestProcessScope>,
) {
  assert.ok(nativeProxy, 'native orientation parity requires the built Rust proxy');
  const command = kind === 'rust' ? nativeProxy : process.execPath;
  const args = kind === 'rust'
    ? ['proxy', ...commonArgs]
    : [typescriptProxy, ...commonArgs];
  return spawnJsonlMcpServer(command, args, {
    cwd: process.cwd(),
    env: environment,
    scope,
    label: `orientation-${kind}-${surface}`,
    timeoutMs: 20_000,
  });
}

function callCoordinates(call: 'ordinary' | 'orientation_read' | 'orientation_acknowledge' | 'transport' | 'hidden') {
  switch (call) {
    case 'ordinary':
      return { surface: 'performative-work' as const, tool: 'work_perform' };
    case 'orientation_read':
      return { surface: 'agent-context' as const, tool: 'agent_orientation_read' };
    case 'orientation_acknowledge':
      return { surface: 'agent-context' as const, tool: 'agent_orientation_acknowledge' };
    case 'transport':
      return { surface: 'agent-context' as const, tool: 'mcp_output_show' };
    case 'hidden':
      return { surface: 'agent-context' as const, tool: 'agent_context_checkpoint_create' };
  }
}

async function exerciseGroup({
  root,
  manifestPath,
  childPath,
  cases,
}: {
  root: string;
  manifestPath: string;
  childPath: string;
  cases: OrientationEntryConformanceCase[];
}): Promise<void> {
  const first = materializeOrientationEntryCase({ root, corpus, testCase: cases[0] });
  const environment: NodeJS.ProcessEnv = { ...process.env };
  delete environment.NARADA_ORIENTATION_ENTRY_FILE;
  delete environment.NARADA_ORIENTATION_REQUIRED;
  Object.assign(environment, first.environment);
  const scope = createTestProcessScope({ label: `orientation-parity-${cases[0].entry.environment ?? cases[0].entry.mode}` });
  const servers: Record<ProxyKind, Record<SurfaceKind, any>> = {
    typescript: {} as Record<SurfaceKind, any>,
    rust: {} as Record<SurfaceKind, any>,
  };
  try {
    for (const kind of ['typescript', 'rust'] as const) {
      for (const surface of ['agent-context', 'performative-work'] as const) {
        const diagnostics = join(root, 'diagnostics', `${kind}-${surface}-${cases[0].id}`);
        mkdirSync(diagnostics, { recursive: true });
        const forwardLog = join(root, `forwarded-${kind}-${surface}-${cases[0].id}.log`);
        forwardLogs[kind][surface] = forwardLog;
        const args = [
          '--surface-id', surface,
          '--artifact-manifest', manifestPath,
          '--runtime-contract-version', String(MCP_RUNTIME_CONTRACT_VERSION),
          '--child-command', process.execPath,
          '--entrypoint', childPath,
          '--diagnostics-dir', diagnostics,
          '--orphan-grace-ms', '1000',
          '--',
        ];
        servers[kind][surface] = proxyLaunch(kind, surface, args, {
          ...environment,
          NARADA_ORIENTATION_FORWARD_LOG: forwardLog,
        }, scope);
        const initialized = await servers[kind][surface].client.request(
          `${kind}-${surface}-initialize`,
          'initialize',
          { protocolVersion: '2024-11-05' },
        );
        assert.equal(initialized.error, undefined, JSON.stringify(initialized));
      }
    }

    let requestSequence = 0;
    let notificationAdmissionChecked = false;
    for (const testCase of cases) {
      const materialized = materializeOrientationEntryCase({ root, corpus, testCase });
      assert.deepEqual(materialized.environment, first.environment, `${testCase.id}:environment_group`);
      const expectedState = expectedOrientationEntryState(testCase, materialized.entryFile);
      requestSequence += 1;
      const methodRequestId = `${testCase.id}:resources-read:${requestSequence}`;
      const [typescriptMethod, rustMethod] = await Promise.all([
        servers.typescript['agent-context'].client.request(
          methodRequestId,
          'resources/read',
          { uri: 'fixture://orientation' },
        ),
        servers.rust['agent-context'].client.request(
          methodRequestId,
          'resources/read',
          { uri: 'fixture://orientation' },
        ),
      ]);
      assert.deepEqual(rustMethod, typescriptMethod, `${testCase.id}:resources_read:runtime_parity`);
      if (testCase.expected.call_posture === 'open') {
        assert.equal(typescriptMethod.error, undefined, `${testCase.id}:resources_read`);
        assert.equal(typescriptMethod.result?.structuredContent?.method, 'resources/read');
      } else {
        assert.deepEqual(typescriptMethod.error, {
          code: -32000,
          message: `orientation_required:${expectedState.reason}`,
          data: expectedState,
        }, `${testCase.id}:resources_read`);
      }

      if (!notificationAdmissionChecked && testCase.expected.call_posture === 'orientation_only') {
        notificationAdmissionChecked = true;
        for (const kind of ['typescript', 'rust'] as const) {
          const server = servers[kind]['agent-context'];
          const logPath = forwardLogs[kind]['agent-context'];
          const before = readFileSync(logPath, 'utf8').trim().split(/\r?\n/).filter(Boolean).length;
          server.child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/progress', params: { progress: 1 } })}\n`);
          server.child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'resources/list', params: {} })}\n`);
          server.child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'tools/call', params: { name: 'agent_orientation_read', arguments: {} } })}\n`);
          requestSequence += 1;
          const barrier = await server.client.request(
            `${testCase.id}:${kind}:notification-barrier:${requestSequence}`,
            'ping',
            {},
          );
          assert.equal(barrier.error, undefined, `${testCase.id}:${kind}:notification_barrier`);
          const forwarded = readFileSync(logPath, 'utf8').trim().split(/\r?\n/).filter(Boolean).slice(before);
          assert.deepEqual(forwarded, ['notifications/progress', 'ping'], `${testCase.id}:${kind}:notification_admission`);
        }
      }
      for (const call of [
        'ordinary',
        'orientation_read',
        'orientation_acknowledge',
        'transport',
        'hidden',
      ] as const) {
        const coordinates = callCoordinates(call);
        requestSequence += 1;
        const requestId = `${testCase.id}:${call}:${requestSequence}`;
        const [typescript, rust] = await Promise.all([
          servers.typescript[coordinates.surface].client.request(
            requestId,
            'tools/call',
            { name: coordinates.tool, arguments: {} },
          ),
          servers.rust[coordinates.surface].client.request(
            requestId,
            'tools/call',
            { name: coordinates.tool, arguments: {} },
          ),
        ]);
        assert.deepEqual(rust, typescript, `${testCase.id}:${call}:runtime_parity`);
        if (expectedOrientationCallAdmission(testCase, call)) {
          assert.equal(typescript.error, undefined, `${testCase.id}:${call}:${JSON.stringify(typescript)}`);
          assert.equal(typescript.result?.structuredContent?.tool, coordinates.tool);
        } else {
          assert.deepEqual(typescript.error, {
            code: -32000,
            message: `orientation_required:${expectedState.reason}`,
            data: expectedState,
          }, `${testCase.id}:${call}`);
        }
      }
    }
  } finally {
    await Promise.allSettled([
      ...Object.values(servers.typescript).map((server) => server?.close?.()),
      ...Object.values(servers.rust).map((server) => server?.close?.()),
    ]);
    await scope.close();
  }
}

test('TypeScript and Rust proxies satisfy one shared orientation admission corpus and live gate transition', { timeout: 30_000 }, async () => {
  const root = mkdtempSync(join(tmpdir(), 'orientation-proxy-parity-'));
  const childPath = join(root, 'fixture-child.mjs');
  const manifestPath = join(root, 'workspace-artifact-manifest.json');
  try {
    writeFileSync(childPath, fixtureChildSource(), 'utf8');
    writeManifest(manifestPath, root, childPath);
    const groups = new Map<string, OrientationEntryConformanceCase[]>();
    for (const testCase of corpus.cases) {
      const entryKey = testCase.entry.mode === 'environment_absent'
        ? 'environment_absent'
        : testCase.entry.environment ?? 'absolute';
      const requiredKey = testCase.required_signal
        ?? (testCase.entry.mode === 'environment_absent' ? 'absent' : 'required');
      const key = `${requiredKey}:${entryKey}`;
      groups.set(key, [...(groups.get(key) ?? []), testCase]);
    }
    for (const cases of groups.values()) {
      await exerciseGroup({ root, manifestPath, childPath, cases });
    }
    const absentIndex = corpus.cases.findIndex((testCase) => testCase.id === 'acknowledgement_absent');
    const openIndex = corpus.cases.findIndex((testCase) => testCase.id === 'valid_acknowledgement');
    assert.equal(openIndex, absentIndex + 1, 'the shared corpus must exercise blocked-to-open in one live proxy generation');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
