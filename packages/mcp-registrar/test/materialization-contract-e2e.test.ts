import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { test } from 'node:test';
import {
  MCP_RUNTIME_CONTRACT_VERSION,
  preflightMaterializationGeneration,
} from '@narada-core/mcp-runtime-proxy/materialization-contract';
import { resolveNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';

type JsonRecord = Record<string, any>;

const packageRoot = resolve(fileURLToPath(new URL('../..', import.meta.url)));
const workspaceRoot = resolve(packageRoot, '..', '..');
const registrarEntrypoint = join(packageRoot, 'dist', 'src', 'main.js');
const artifactManifestPath = join(workspaceRoot, '.ai', 'runtime', 'workspace-artifact-manifest.json');
const surfacesRoot = join(workspaceRoot, 'packages');
const nativeRuntimeArtifactAvailable = process.platform === 'win32'
  && resolveNativeArtifact(join(surfacesRoot, 'shared', 'mcp-runtime-proxy'), 'narada-mcp-runtime.exe') !== null;

type RpcRun = {
  exitCode: number | null;
  responses: JsonRecord[];
  stdout: string;
  stderr: string;
};

function testEnvironment(overrides: NodeJS.ProcessEnv = {}): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, NARADA_MCP_WORKSPACE_ROOT: workspaceRoot, NARADA_MCP_SURFACES_ROOT: surfacesRoot, ...overrides };
  delete env['NARADA_MCP_REGISTRAR_FRESH_CHILD'];
  return env;
}

function runCli(args: string[], environment: NodeJS.ProcessEnv = {}): Promise<RpcRun> {
  return new Promise((resolveRun, rejectRun) => {
    const child = spawn(process.execPath, args, {
      cwd: workspaceRoot,
      env: testEnvironment(environment),
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    });
    let stdout = '';
    let stderr = '';
    let settled = false;
    const timeout = setTimeout(() => {
      if (settled) return;
      settled = true;
      child.kill();
      rejectRun(new Error(`materialization_cli_e2e_timeout:${args.join(' ')}`));
    }, 45_000);
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk: string) => { stdout += chunk; });
    child.stderr.on('data', (chunk: string) => { stderr += chunk; });
    child.once('error', (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      rejectRun(error);
    });
    child.once('close', (exitCode) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      resolveRun({ exitCode, responses: [], stdout, stderr });
    });
  });
}

function runRpc(command: string, args: string[], request: JsonRecord): Promise<RpcRun> {
  return new Promise((resolveRun, rejectRun) => {
    const child = spawn(command, args, {
      cwd: workspaceRoot,
      env: testEnvironment(),
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    });
    let stdout = '';
    let stderr = '';
    let settled = false;
    const timeout = setTimeout(() => {
      if (settled) return;
      settled = true;
      child.kill();
      rejectRun(new Error(`materialization_contract_e2e_timeout:${command}`));
    }, 45_000);
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk: string) => { stdout += chunk; });
    child.stderr.on('data', (chunk: string) => { stderr += chunk; });
    child.once('error', (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      rejectRun(error);
    });
    child.once('close', (exitCode) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      const responses = stdout.split(/\r?\n/).map((line) => line.trim()).filter(Boolean).map((line) => JSON.parse(line) as JsonRecord);
      resolveRun({ exitCode, responses, stdout, stderr });
    });
    child.stdin.write(JSON.stringify(request) + '\n');
    child.stdin.end();
  });
}

function structuredResult(response: JsonRecord): JsonRecord {
  assert.equal(response.error, undefined, JSON.stringify(response));
  return response.result?.structuredContent as JsonRecord;
}

function codexLaunch(config: string, serverKey: string): { command: string; args: string[] } {
  const marker = `[mcp_servers.${serverKey}]`;
  const start = config.indexOf(marker);
  assert.notEqual(start, -1, `missing generated carrier section: ${serverKey}`);
  const next = config.indexOf('\n[', start + marker.length);
  const section = config.slice(start, next < 0 ? config.length : next);
  const commandMatch = /^command = "([^"]+)"$/m.exec(section);
  const argsMatch = /^args = (.+)$/m.exec(section);
  assert.ok(commandMatch, section);
  assert.ok(argsMatch, section);
  return { command: commandMatch[1]!, args: JSON.parse(argsMatch[1]!) as string[] };
}

test('fresh registrar materializes, validates, and launches a carrier generation', async () => {
  assert.equal(existsSync(registrarEntrypoint), true, registrarEntrypoint);
  assert.equal(existsSync(artifactManifestPath), true, artifactManifestPath);
  const root = mkdtempSync(join(tmpdir(), 'mcp-registrar-materialization-e2e-'));
   const materializationProfile = nativeRuntimeArtifactAvailable ? 'native' : 'bun';
  const configPath = join(root, 'config.toml');
  const sidecarPath = `${resolve(configPath)}.narada-generation.json`;
  try {
    const rejectedSingle = await runCli([
      registrarEntrypoint,
      '--materialize-carrier',
      'codex-andrey',
      '--output-path',
      configPath,
    ]);
    assert.notEqual(rejectedSingle.exitCode, 0);
    assert.match(rejectedSingle.stderr, /registrar_single_carrier_materialization_requires_explicit_escape_hatch/);

    const emergencySingle = await runCli([
      registrarEntrypoint,
      '--materialize-carrier',
      'codex-andrey',
      '--allow-single-carrier',
      '--output-path',
      configPath,
    ]);
    assert.equal(emergencySingle.exitCode, 0, emergencySingle.stderr);
    const emergencyResult = JSON.parse(emergencySingle.stdout) as JsonRecord;
    assert.equal(emergencyResult.status, 'materialized');
    assert.equal(emergencyResult.carrier_id, 'codex-andrey');
    assert.equal(emergencyResult.materialization_validation.ok, true, JSON.stringify(emergencyResult));
    assert.equal(emergencyResult.recovery_escape_hatch, true);
    assert.equal((emergencyResult.runtime_materialization_plan as JsonRecord).recovery_escape_hatch, true);

    const directMaterialize = await runCli([
      registrarEntrypoint,
      '--materialize-all',
      '--output-dir',
      root,
      '--runtime-profile',
      materializationProfile,
    ]);
    assert.equal(directMaterialize.exitCode, 0, directMaterialize.stderr);
    const directResult = JSON.parse(directMaterialize.stdout) as JsonRecord;
    assert.equal(directResult.status, 'materialized_all');
    assert.equal(directResult.carrier_count, 3);
    const directCodex = (directResult.carriers as JsonRecord[]).find((carrier) => carrier.carrier_id === 'codex-andrey')!;
    assert.equal(directCodex.materialization_validation.ok, true, JSON.stringify(directCodex));
    assert.equal(directCodex.materialization_generation.proxy_implementation, materializationProfile === 'native' ? 'native' : 'bun');
    assert.equal(typeof directCodex.materialization_generation.runtime_materialization_plan_fingerprint, 'string');
    assert.equal(typeof directCodex.materialization_generation.runtime_implementation_matrix_fingerprint, 'string');
    assert.equal(existsSync(configPath), true);
    assert.equal(existsSync(sidecarPath), true);
    const planPath = `${resolve(configPath)}.narada-runtime-plan.json`;
    assert.equal(existsSync(planPath), true);
    const directPlan = JSON.parse(readFileSync(planPath, 'utf8')) as JsonRecord;
    assert.equal(directPlan.schema, 'narada.runtime_materialization_plan.v1');
    assert.equal(directPlan.runtime_profile_kind, materializationProfile);
    assert.equal(existsSync(join(root, 'mcp.json')), true);
    assert.equal(existsSync(join(root, 'opencode.jsonc')), true);
    const directConfig = readFileSync(join(root, 'config.toml'), 'utf8');
    const directRegistrarLaunch = codexLaunch(directConfig, 'narada-site-andrey-user-mcp-registrar');
    if (nativeRuntimeArtifactAvailable) {
      assert.match(directRegistrarLaunch.command, /narada-mcp-runtime\.exe$/i);
      assert.equal(directRegistrarLaunch.args[0], 'proxy');
    } else {
      assert.match(directRegistrarLaunch.command, /node(?:\.exe)?$/i);
    }

    const nativeRoot = join(root, 'native');
    const nativeMaterialize = await runCli([
      registrarEntrypoint,
      '--materialize-all',
      '--output-dir',
      nativeRoot,
      '--runtime-proxy-implementation',
      'native',
      '--runtime-profile',
      'native',
    ]);
    assert.equal(nativeMaterialize.exitCode, 0, nativeMaterialize.stderr);
    const nativeResult = JSON.parse(nativeMaterialize.stdout) as JsonRecord;
    const nativeCodex = (nativeResult.carriers as JsonRecord[]).find((carrier) => carrier.carrier_id === 'codex-andrey')!;
    assert.equal(nativeCodex.materialization_generation.proxy_implementation, 'native');
    assert.match(nativeCodex.materialization_generation.proxy_entrypoint, /narada-mcp-runtime\.exe$/i);
    if (nativeRuntimeArtifactAvailable) {
      const recoveryRoot = join(root, 'recovery');
      const recoveryMaterialize = await runCli([
        registrarEntrypoint,
        '--materialize-carrier',
        'codex-andrey',
        '--allow-single-carrier',
        '--output-path',
        join(recoveryRoot, 'config.toml'),
        '--runtime-profile',
        'native',
        '--runtime-proxy-implementation',
        'bun',
        '--recovery-escape-hatch',
      ]);
      assert.equal(recoveryMaterialize.exitCode, 0, recoveryMaterialize.stderr);
      const recoveryResult = JSON.parse(recoveryMaterialize.stdout) as JsonRecord;
      assert.equal(recoveryResult.recovery_escape_hatch, true);
      assert.equal(recoveryResult.materialization_generation.proxy_implementation, 'bun');
      const recoveryPlan = JSON.parse(readFileSync(join(recoveryRoot, 'config.toml.narada-runtime-plan.json'), 'utf8')) as JsonRecord;
      assert.equal(recoveryPlan.recovery_escape_hatch, true);
      assert.equal(recoveryPlan.runtime_proxy_implementation, 'bun');
      assert.equal(recoveryPlan.runtime_proxy_implementation_override, true);
    }
    const nativeConfig = readFileSync(join(nativeRoot, 'config.toml'), 'utf8');
    const nativeLaunch = codexLaunch(nativeConfig, 'narada-site-andrey-user-mcp-registrar');
    assert.match(nativeLaunch.command, /narada-mcp-runtime\.exe$/i);
    assert.equal(nativeLaunch.args[0], 'proxy');
    const nativeChildCommand = nativeLaunch.args[nativeLaunch.args.indexOf('--child-command') + 1];
    const nativeRegistrarCommand = nativeLaunch.args[nativeLaunch.args.indexOf('--registrar-command') + 1];
    assert.equal(typeof nativeChildCommand, 'string');
    assert.equal(nativeChildCommand!.length > 0, true);
    assert.equal(nativeRegistrarCommand, nativeChildCommand);
    const nativeProxyRun = await runRpc(nativeLaunch.command, nativeLaunch.args, {
      jsonrpc: '2.0',
      id: 'native-initialize',
      method: 'initialize',
      params: { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'native-materialization-e2e', version: '1.0.0' } },
    });
    assert.equal(nativeProxyRun.exitCode, 0, nativeProxyRun.stderr);
    assert.equal(nativeProxyRun.responses[0]?.error, undefined, JSON.stringify(nativeProxyRun.responses[0]));
    assert.equal(nativeProxyRun.responses[0]?.result?.serverInfo?.name, 'mcp-registrar');

     const nativePlan = JSON.parse(readFileSync(join(nativeRoot, 'config.toml.narada-runtime-plan.json'), 'utf8')) as JsonRecord;
     const nativeLoaderRow = (nativePlan.servers as JsonRecord[]).find((row) => row.surface_id === 'mcp-loader')!;
     const nativeLoaderRun = await runRpc(nativeLoaderRow.launch.command, nativeLoaderRow.launch.args, {
       jsonrpc: '2.0',
       id: 'native-loader-initialize',
       method: 'initialize',
       params: { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'native-loader-materialization-e2e', version: '1.0.0' } },
     });
     assert.equal(nativeLoaderRun.exitCode, 0, nativeLoaderRun.stderr);
     assert.equal(nativeLoaderRun.responses[0]?.error, undefined, JSON.stringify(nativeLoaderRun.responses[0]));
     assert.equal(nativeLoaderRun.responses[0]?.result?.serverInfo?.name, 'mcp-loader-mcp');
    const nativePlanPath = join(nativeRoot, 'config.toml.narada-runtime-plan.json');
    const nativePlanText = readFileSync(nativePlanPath, 'utf8');
    writeFileSync(nativePlanPath, nativePlanText.replace('"status": "accepted"', '"status": "refused"'), 'utf8');
    const nativePlanStaleRun = await runRpc(nativeLaunch.command, nativeLaunch.args, {
      jsonrpc: '2.0',
      id: 'native-plan-stale',
      method: 'initialize',
      params: { protocolVersion: '2024-11-05' },
    });
    assert.notEqual(nativePlanStaleRun.exitCode, 0);
    assert.equal(nativePlanStaleRun.responses[0]?.error?.data?.code, 'materialization_generation_stale');
    writeFileSync(nativePlanPath, nativePlanText, 'utf8');
    writeFileSync(join(nativeRoot, 'config.toml'), nativeConfig + '# native stale generation test\n', 'utf8');
    const nativeStaleRun = await runRpc(nativeLaunch.command, nativeLaunch.args, {
      jsonrpc: '2.0',
      id: 'native-stale',
      method: 'initialize',
      params: { protocolVersion: '2024-11-05' },
    });
    assert.notEqual(nativeStaleRun.exitCode, 0);
    assert.equal(nativeStaleRun.responses[0]?.error?.data?.code, 'materialization_generation_stale');
    assert.equal(nativeStaleRun.responses[0]?.error?.data?.details?.recovery?.schema, 'narada.mcp_runtime_proxy.materialization_recovery.v1');
    assert.deepEqual(nativeStaleRun.responses[0]?.error?.data?.details?.recovery?.regeneration?.command?.args.slice(1), ['--materialize-all']);

    const materialize = await runRpc(process.execPath, [registrarEntrypoint], {
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: 'registrar_materialize_all', arguments: { output_dir: root } },
    });
    assert.equal(materialize.exitCode, 0, materialize.stderr);
    const result = structuredResult(materialize.responses[0]!);
    assert.equal(result.status, 'materialized_all');
    assert.equal(result.carrier_count, 3);
    const codexResult = (result.carriers as JsonRecord[]).find((carrier) => carrier.carrier_id === 'codex-andrey')!;
    assert.equal(result.runtime_contract_version, MCP_RUNTIME_CONTRACT_VERSION);
    assert.equal(codexResult.materialization_validation.ok, true, JSON.stringify(codexResult.materialization_validation));
    assert.equal(codexResult.materialization_generation.config_path, resolve(configPath));
    assert.equal(existsSync(configPath), true);
    assert.equal(existsSync(sidecarPath), true);

    const config = readFileSync(configPath, 'utf8');
    assert.match(config, /\[features\]\r?\napps = false\r?\n/);
    assert.match(config, /\[plugins\."github@openai-curated-remote"\]\r?\nenabled = false\r?\n/);

    const pluginPolicyRoot = mkdtempSync(join(tmpdir(), 'mcp-registrar-plugin-policy-e2e-'));
    try {
      const pluginPolicyMaterialize = await runCli([
        registrarEntrypoint,
        '--materialize-all',
        '--output-dir',
        pluginPolicyRoot,
      ], {
        NARADA_CODEX_ENABLED_PLUGINS: 'github@openai-curated-remote;sample-enabled@personal',
        NARADA_CODEX_DISABLED_PLUGINS: 'sample-disabled@personal',
      });
      assert.equal(pluginPolicyMaterialize.exitCode, 0, pluginPolicyMaterialize.stderr);
      const pluginPolicyConfig = readFileSync(join(pluginPolicyRoot, 'config.toml'), 'utf8');
      assert.match(pluginPolicyConfig, /\[plugins\."sample-disabled@personal"\]\r?\nenabled = false\r?\n/);
      assert.match(pluginPolicyConfig, /\[plugins\."sample-enabled@personal"\]\r?\nenabled = true\r?\n/);
      assert.match(pluginPolicyConfig, /\[plugins\."github@openai-curated-remote"\]\r?\nenabled = true\r?\n/);
      assert.equal(readFileSync(join(pluginPolicyRoot, 'mcp.json'), 'utf8').includes('sample-enabled@personal'), false);
      assert.equal(readFileSync(join(pluginPolicyRoot, 'opencode.jsonc'), 'utf8').includes('sample-enabled@personal'), false);
    } finally {
      rmSync(pluginPolicyRoot, { recursive: true, force: true });
    }

    const proxyCount = codexResult.materialization_validation.proxy_count as number;
    assert.equal((config.match(/--artifact-manifest/g) ?? []).length, proxyCount);
    assert.equal((config.match(/--runtime-contract-version/g) ?? []).length, proxyCount);
    assert.equal((config.match(/--materialization-sidecar/g) ?? []).length, proxyCount);
    assert.deepEqual(
      preflightMaterializationGeneration({
        sidecarPath,
        manifestPath: artifactManifestPath,
        manifestFingerprint: codexResult.materialization_generation.artifact_manifest_fingerprint,
      }),
      { ok: true, generation_fingerprint: codexResult.materialization_generation.generation_fingerprint },
    );

    const launch = codexLaunch(config, 'narada-site-andrey-user-mcp-registrar');
    assert.equal(launch.args[launch.args.indexOf('--carrier-id') + 1], 'codex-andrey');
    assert.equal(launch.args[launch.args.indexOf('--carrier-kind') + 1], 'codex');
    assert.equal(launch.args.includes('--registrar-entrypoint'), true);
    const proxyRun = await runRpc(launch.command === 'node' ? process.execPath : launch.command, launch.args, {
      jsonrpc: '2.0',
      id: 2,
      method: 'initialize',
      params: { protocolVersion: '2024-11-05' },
    });
    assert.equal(proxyRun.exitCode, 0, proxyRun.stderr);
    assert.equal(proxyRun.responses[0]?.result?.serverInfo?.name, 'mcp-registrar');

    const toolsRun = await runRpc(launch.command === 'node' ? process.execPath : launch.command, launch.args, {
      jsonrpc: '2.0',
      id: 3,
      method: 'tools/list',
      params: {},
    });
    assert.equal(toolsRun.exitCode, 0, toolsRun.stderr);
    assert.equal(toolsRun.responses[0]?.result?.tools?.some((tool: JsonRecord) => tool.name === 'mcp_runtime_proxy_status'), true);

    writeFileSync(configPath, config + '# stale generation test\n', 'utf8');
    const staleRun = await runRpc(launch.command === 'node' ? process.execPath : launch.command, launch.args, {
      jsonrpc: '2.0',
      id: 4,
      method: 'initialize',
      params: { protocolVersion: '2024-11-05' },
    });
    assert.notEqual(staleRun.exitCode, 0);
    assert.equal(staleRun.responses[0]?.error?.data?.code, 'materialization_generation_stale');
    const staleRecovery = staleRun.responses[0]?.error?.data?.details?.recovery;
    assert.match(staleRecovery?.recovery_group_id, /^materialization-[0-9a-f]{20}$/);
    assert.equal(staleRecovery?.regeneration?.available, true);
    assert.deepEqual(staleRecovery?.regeneration?.command?.args.slice(1), ['--materialize-all']);
    assert.equal(staleRecovery?.restart_required, true);
    assert.match(staleRecovery?.restart?.instruction, /Restart Codex/);

    const staleBootstrapRuns = await Promise.all([
      'narada-site-andrey-user-agent-context',
      'narada-site-andrey-user-local-filesystem',
      'narada-site-andrey-user-mcp-loader',
    ].map(async (serverKey) => {
      const bootstrapLaunch = codexLaunch(config, serverKey);
      return runRpc(bootstrapLaunch.command === 'node' ? process.execPath : bootstrapLaunch.command, bootstrapLaunch.args, {
        jsonrpc: '2.0',
        id: 40,
        method: 'initialize',
        params: { protocolVersion: '2024-11-05' },
      });
    }));
    const recoveryGroupIds = [
      staleRecovery?.recovery_group_id,
      ...staleBootstrapRuns.map((run) => run.responses[0]?.error?.data?.details?.recovery?.recovery_group_id),
    ];
    assert.equal(new Set(recoveryGroupIds).size, 1, JSON.stringify(recoveryGroupIds));

    const missingVersionArgs = [...launch.args];
    const versionIndex = missingVersionArgs.indexOf('--runtime-contract-version');
    missingVersionArgs.splice(versionIndex, 2);
    const missingVersionRun = await runRpc(launch.command === 'node' ? process.execPath : launch.command, missingVersionArgs, {
      jsonrpc: '2.0',
      id: 5,
      method: 'initialize',
      params: { protocolVersion: '2024-11-05' },
    });
    assert.notEqual(missingVersionRun.exitCode, 0);
    assert.equal(missingVersionRun.responses[0]?.error?.data?.code, 'runtime_contract_version_missing');

    const missingManifestArgs = [...launch.args];
    const manifestIndex = missingManifestArgs.indexOf('--artifact-manifest');
    missingManifestArgs.splice(manifestIndex, 2);
    const missingManifestRun = await runRpc(launch.command === 'node' ? process.execPath : launch.command, missingManifestArgs, {
      jsonrpc: '2.0',
      id: 6,
      method: 'initialize',
      params: { protocolVersion: '2024-11-05' },
    });
    assert.notEqual(missingManifestRun.exitCode, 0);
    assert.equal(missingManifestRun.responses[0]?.error?.data?.code, 'workspace_manifest_missing');
    const workspaceRecovery = missingManifestRun.responses[0]?.error?.data?.details?.recovery;
    assert.match(workspaceRecovery?.recovery_group_id, /^workspace-materialization-[0-9a-f]{20}$/);
    assert.equal(workspaceRecovery?.steps?.[0]?.command?.display, 'pnpm build');
    assert.equal(workspaceRecovery?.steps?.[1]?.available, true);
    assert.equal(workspaceRecovery?.restart_required, true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('runtime profiles compile carrier plans with matrix-selected engines', async () => {
  const root = mkdtempSync(join(tmpdir(), 'mcp-registrar-runtime-profiles-e2e-'));
  const nativeLoaderAvailable = process.platform === 'win32'
    && existsSync(join(surfacesRoot, 'mcp-loader-mcp', 'dist', 'native', 'narada-mcp-loader.exe'));
  const profiles = nativeRuntimeArtifactAvailable && nativeLoaderAvailable
    ? ['native', 'bun', 'node-compat']
    : ['bun', 'node-compat'];
  const expected: Record<string, Record<string, string>> = {
    native: { 'mcp-loader': 'rust', 'local-filesystem': 'rust', 'agent-context': 'bun', 'mcp-registrar': 'bun' },
    bun: { 'mcp-loader': 'bun', 'local-filesystem': 'bun', 'agent-context': 'bun', 'mcp-registrar': 'bun' },
    'node-compat': { 'mcp-loader': 'node', 'local-filesystem': 'node', 'agent-context': 'node', 'mcp-registrar': 'node' },
  };
  try {
    for (const profile of profiles) {
      const outputDir = join(root, profile);
      const result = await runCli([registrarEntrypoint, '--materialize-all', '--output-dir', outputDir, '--runtime-profile', profile]);
      assert.equal(result.exitCode, 0, `${profile}:${result.stderr}`);
      const planPath = join(outputDir, 'config.toml.narada-runtime-plan.json');
      const plan = JSON.parse(readFileSync(planPath, 'utf8')) as JsonRecord;
      assert.equal(plan.schema, 'narada.runtime_materialization_plan.v1');
      assert.equal(plan.runtime_profile_kind, profile);
      for (const [surfaceId, runtimeEngineKind] of Object.entries(expected[profile]!)) {
        const row = (plan.servers as JsonRecord[]).find((candidate) => candidate.surface_id === surfaceId);
        assert.ok(row, `${profile}:${surfaceId}:missing`);
        assert.equal(row.runtime_engine_kind, runtimeEngineKind, `${profile}:${surfaceId}:engine`);
      }
      if (profile === 'native') {
        const nativeFilesystem = (plan.servers as JsonRecord[]).find((candidate) => candidate.surface_id === 'local-filesystem');
        const nativeLoader = (plan.servers as JsonRecord[]).find((candidate) => candidate.surface_id === 'mcp-loader');
        assert.equal(nativeFilesystem?.child_invocation_kind, 'native_applet');
        assert.equal(nativeFilesystem?.child_applet, 'filesystem');
        assert.equal(nativeLoader?.child_invocation_kind, 'native_entrypoint');
        assert.equal(nativeLoader?.child_applet, undefined);
      }
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
test('carrier recovery evidence retention bounds current and legacy artifacts', async () => {
  const root = mkdtempSync(join(tmpdir(), 'narada-recovery-evidence-contract-'));
  const evidenceRoot = join(root, '.ai', 'runtime', 'carrier-materialization-recovery');
  try {
    mkdirSync(evidenceRoot, { recursive: true });
    const legacyPath = join(evidenceRoot, '20260811120000-aaaaaaaaaaaaaaaa.json');
    writeFileSync(legacyPath, '{}\n', 'utf8');
    const evidenceModule = await import(pathToFileURL(join(workspaceRoot, 'scripts', 'carrier-recovery-evidence.mjs')).href + '?contract=' + Date.now()) as {
      writeRecoveryEvidence(args: { workspaceRoot: string; evidenceRoot: string; value: unknown; maxFiles: number; now?: () => Date }): {
        path: string;
        retention: { max_files: number; retained_count: number; pruned_count: number };
      };
    };
    const refs = Array.from({ length: 4 }, (_, sequence) => evidenceModule.writeRecoveryEvidence({
      workspaceRoot: root,
      evidenceRoot,
      value: { sequence },
      maxFiles: 2,
    }));
    assert.equal(readdirSync(evidenceRoot).filter((name) => name.endsWith('.json')).length, 2);
    assert.equal(existsSync(legacyPath), false);
    assert.equal(existsSync(refs[0].path), false);
    assert.equal(existsSync(refs[3].path), true);
    assert.deepEqual(refs[3].retention, { policy: 'current_then_newest_files', max_files: 2, retained_count: 2, pruned_count: 1 });
    const fixedNow = () => new Date('2026-08-11T12:34:56.789Z');
    const duplicateA = evidenceModule.writeRecoveryEvidence({ workspaceRoot: root, evidenceRoot, value: { duplicate: true }, maxFiles: 2, now: fixedNow });
    const duplicateB = evidenceModule.writeRecoveryEvidence({ workspaceRoot: root, evidenceRoot, value: { duplicate: true }, maxFiles: 2, now: fixedNow });
    assert.equal(duplicateB.path, duplicateA.path);
    assert.equal(existsSync(duplicateA.path), true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
