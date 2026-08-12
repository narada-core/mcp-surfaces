import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import {
  MCP_RUNTIME_CONTRACT_VERSION,
  buildMaterializationGeneration,
  describeMaterializedConfiguration,
  materializationSidecarPath,
  preflightMaterializationGeneration,
  validateMaterializedConfiguration,
  writeMaterializationGeneration,
} from '../src/materialization-contract.js';

type FingerprintVector = {
  id: string;
  carrier_kind: string;
  content: string;
  selectors: string[];
  expected_bytes_sha256: string;
  expected_managed_sha256: string;
};

function sha256(value: string | Buffer): string {
  return createHash('sha256').update(value).digest('hex');
}

function writePlan(root: string, configPath: string, matrixPath: string): { path: string; fingerprint: string } {
  const matrixFingerprint = sha256(readFileSync(matrixPath));
  const path = `${configPath}.narada-runtime-plan.json`;
  const unsigned = {
    schema: 'narada.runtime_materialization_plan.v1',
    status: 'accepted',
    runtime_profile_kind: 'bun',
    runtime_engine_kind: 'bun',
    source: { matrix_fingerprint: matrixFingerprint },
    servers: [],
  };
  const fingerprint = sha256(JSON.stringify(unsigned));
  writeFileSync(path, JSON.stringify({ ...unsigned, plan_fingerprint: fingerprint }) + '\n', 'utf8');
  return { path, fingerprint };
}

function fixture() {
  const root = mkdtempSync(join(tmpdir(), 'mcp-materialization-contract-'));
  const proxyPath = join(root, 'mcp-runtime-proxy.js');
  const childPath = join(root, 'child.js');
  const manifestPath = join(root, 'workspace-artifact-manifest.json');
  const registrarPath = join(root, 'registrar.js');
  const contractEntrypoint = resolve(
    process.cwd(),
    '../mcp-materializer-native/native/target/debug/',
    `narada-mcp-materializer${process.platform === 'win32' ? '.exe' : ''}`,
  );
  const matrixPath = join(root, 'runtime-implementation-matrix.json');
  const configPath = join(root, 'carrier.json');
  const sidecarPath = materializationSidecarPath(configPath);
  mkdirSync(root, { recursive: true });
  for (const path of [proxyPath, childPath, manifestPath, registrarPath]) writeFileSync(path, 'fixture\n', 'utf8');
  writeFileSync(matrixPath, '{"schema":"fixture.matrix"}\n', 'utf8');
  const plan = writePlan(root, configPath, matrixPath);
  const args = [
    proxyPath,
    '--surface-id', 'fixture',
    '--child-command', process.execPath,
    '--registrar-command', process.execPath,
    '--registrar-entrypoint', registrarPath,
    '--artifact-manifest', manifestPath,
    '--runtime-contract-version', String(MCP_RUNTIME_CONTRACT_VERSION),
    '--materialization-sidecar', sidecarPath,
    '--entrypoint', childPath,
    '--',
  ];
  const structured = { mcpServers: { fixture: { command: process.execPath, args } } };
  return { root, proxyPath, childPath, manifestPath, registrarPath, contractEntrypoint, matrixPath, configPath, sidecarPath, planPath: plan.path, planFingerprint: plan.fingerprint, args, structured };
}

test('materialized configuration validates the runtime contract and generation preflight', () => {
  const f = fixture();
  try {
    const validation = validateMaterializedConfiguration({
      structured: f.structured,
      artifactManifestPath: f.manifestPath,
      runtimeProxyEntrypoint: f.proxyPath,
      expectedSidecarPath: f.sidecarPath,
      requireSidecar: true,
    });
    assert.equal(validation.ok, true, JSON.stringify(validation));
    assert.equal(validation.server_count, 1);
    assert.equal(validation.proxy_count, 1);

    const content = JSON.stringify(f.structured) + '\n';
    writeFileSync(f.configPath, content, 'utf8');
    const generation = buildMaterializationGeneration({
      carrierId: 'fixture-carrier',
      carrierKind: 'kimi',
      configPath: f.configPath,
      content,
      materializationContractEntrypoint: f.contractEntrypoint,
      artifactManifestPath: f.manifestPath,
      artifactManifestFingerprint: 'fixture-manifest-fingerprint',
      runtimeProfileKind: 'bun',
      runtimeMaterializationPlanPath: f.planPath,
      runtimeMaterializationPlanFingerprint: f.planFingerprint,
      runtimeImplementationMatrixPath: f.matrixPath,
      runtimeImplementationMatrixFingerprint: sha256(readFileSync(f.matrixPath)),
      registrarEntrypoint: f.registrarPath,
      proxyImplementation: 'bun',
      proxyEntrypoint: f.proxyPath,
      serverCount: validation.server_count,
      proxyCount: validation.proxy_count,
    });
    writeMaterializationGeneration(f.sidecarPath, generation);
    assert.deepEqual(JSON.parse(readFileSync(f.sidecarPath, 'utf8')), generation);
    assert.deepEqual(
      preflightMaterializationGeneration({
        sidecarPath: f.sidecarPath,
        manifestPath: f.manifestPath,
        manifestFingerprint: 'fixture-manifest-fingerprint',
        materializationContractEntrypoint: f.contractEntrypoint,
        runtimeProxyEntrypoint: f.proxyPath,
      }),
      { ok: true, generation_fingerprint: generation.generation_fingerprint },
    );

    writeFileSync(f.sidecarPath, JSON.stringify({ ...generation, materialization_contract_entrypoint: f.proxyPath }) + '\n', 'utf8');
    const untrustedAuthority = preflightMaterializationGeneration({
      sidecarPath: f.sidecarPath,
      manifestPath: f.manifestPath,
      manifestFingerprint: 'fixture-manifest-fingerprint',
      materializationContractEntrypoint: f.contractEntrypoint,
      runtimeProxyEntrypoint: f.proxyPath,
    });
    assert.equal(untrustedAuthority.code, 'materialization_generation_stale');
    assert.match(untrustedAuthority.reason ?? '', /different contract authority/);
    writeMaterializationGeneration(f.sidecarPath, generation);

    const untrustedProxy = preflightMaterializationGeneration({
      sidecarPath: f.sidecarPath,
      manifestPath: f.manifestPath,
      manifestFingerprint: 'fixture-manifest-fingerprint',
      materializationContractEntrypoint: f.contractEntrypoint,
      runtimeProxyEntrypoint: f.childPath,
    });
    assert.equal(untrustedProxy.code, 'materialization_generation_stale');
    assert.match(untrustedProxy.reason ?? '', /different runtime proxy/);

    writeFileSync(f.planPath, JSON.stringify({ schema: 'narada.runtime_materialization_plan.v1', status: 'accepted', runtime_profile_kind: 'bun', plan_fingerprint: f.planFingerprint }) + '\n', 'utf8');
    assert.equal(preflightMaterializationGeneration({
      sidecarPath: f.sidecarPath,
      manifestPath: f.manifestPath,
      manifestFingerprint: 'fixture-manifest-fingerprint',
      materializationContractEntrypoint: f.contractEntrypoint,
      runtimeProxyEntrypoint: f.proxyPath,
    }).code, 'materialization_generation_stale');
    writePlan(f.root, f.configPath, f.matrixPath);
    writeFileSync(f.matrixPath, '{"schema":"fixture.matrix.changed"}\n', 'utf8');
    assert.equal(preflightMaterializationGeneration({
      sidecarPath: f.sidecarPath,
      manifestPath: f.manifestPath,
      manifestFingerprint: 'fixture-manifest-fingerprint',
      materializationContractEntrypoint: f.contractEntrypoint,
      runtimeProxyEntrypoint: f.proxyPath,
    }).code, 'materialization_generation_stale');
    writeFileSync(f.matrixPath, '{"schema":"fixture.matrix"}\n', 'utf8');
    writePlan(f.root, f.configPath, f.matrixPath);

    writeFileSync(f.sidecarPath, JSON.stringify({ ...generation, managed_projection: { ...generation.managed_projection, sha256: 'tampered' } }) + '\n', 'utf8');
    assert.equal(preflightMaterializationGeneration({
      sidecarPath: f.sidecarPath,
      manifestPath: f.manifestPath,
      manifestFingerprint: 'fixture-manifest-fingerprint',
      materializationContractEntrypoint: f.contractEntrypoint,
      runtimeProxyEntrypoint: f.proxyPath,
    }).code, 'materialization_generation_stale');
    writeMaterializationGeneration(f.sidecarPath, generation);

    writeFileSync(f.configPath, content + 'changed\n', 'utf8');
    assert.equal(preflightMaterializationGeneration({
      sidecarPath: f.sidecarPath,
      manifestPath: f.manifestPath,
      manifestFingerprint: 'fixture-manifest-fingerprint',
      materializationContractEntrypoint: f.contractEntrypoint,
      runtimeProxyEntrypoint: f.proxyPath,
    }).code, 'materialization_managed_projection_stale');

    writeFileSync(f.configPath, Buffer.from([0xff, 0x0a]));
    assert.equal(preflightMaterializationGeneration({
      sidecarPath: f.sidecarPath,
      manifestPath: f.manifestPath,
      manifestFingerprint: 'fixture-manifest-fingerprint',
      materializationContractEntrypoint: f.contractEntrypoint,
      runtimeProxyEntrypoint: f.proxyPath,
    }).code, 'materialization_managed_projection_stale');
  } finally {
    rmSync(f.root, { recursive: true, force: true });
  }
});

test('materialized configuration refuses missing or obsolete launch invariants', () => {
  const f = fixture();
  try {
    const missingVersion = { mcpServers: { fixture: { command: process.execPath, args: f.args.filter((arg) => arg !== '--runtime-contract-version' && arg !== String(MCP_RUNTIME_CONTRACT_VERSION)) } } };
    const missingVersionResult = validateMaterializedConfiguration({
      structured: missingVersion,
      artifactManifestPath: f.manifestPath,
      runtimeProxyEntrypoint: f.proxyPath,
      expectedSidecarPath: f.sidecarPath,
      requireSidecar: true,
    });
    assert.equal(missingVersionResult.ok, false);
    assert.equal(missingVersionResult.errors[0]?.code, 'materialized_config_contract_version_mismatch');

    const missingManifest = { mcpServers: { fixture: { command: process.execPath, args: f.args.filter((arg) => arg !== '--artifact-manifest' && arg !== f.manifestPath) } } };
    const missingManifestResult = validateMaterializedConfiguration({
      structured: missingManifest,
      artifactManifestPath: f.manifestPath,
      runtimeProxyEntrypoint: f.proxyPath,
      expectedSidecarPath: f.sidecarPath,
      requireSidecar: true,
    });
    assert.equal(missingManifestResult.ok, false);
    assert.equal(missingManifestResult.errors.some((error) => error.code === 'materialized_config_missing_artifact_manifest'), true);

    const missingSidecar = { mcpServers: { fixture: { command: process.execPath, args: f.args.filter((arg) => arg !== '--materialization-sidecar' && arg !== f.sidecarPath) } } };
    const missingSidecarResult = validateMaterializedConfiguration({
      structured: missingSidecar,
      artifactManifestPath: f.manifestPath,
      runtimeProxyEntrypoint: f.proxyPath,
      expectedSidecarPath: f.sidecarPath,
      requireSidecar: true,
    });
    assert.equal(missingSidecarResult.ok, false);
    assert.equal(missingSidecarResult.errors.some((error) => error.code === 'materialized_config_missing_generation_sidecar'), true);

    const relativeExecutables = {
      mcpServers: {
        fixture: {
          command: 'node',
          args: f.args.map((arg) => arg === process.execPath ? 'node' : arg),
        },
      },
    };
    const relativeExecutableResult = validateMaterializedConfiguration({
      structured: relativeExecutables,
      artifactManifestPath: f.manifestPath,
      runtimeProxyEntrypoint: f.proxyPath,
      expectedSidecarPath: f.sidecarPath,
      requireSidecar: true,
    });
    assert.equal(relativeExecutableResult.ok, false);
    assert.deepEqual(
      relativeExecutableResult.errors.map((error) => error.code).filter((code) => code.endsWith('_not_absolute')).sort(),
      [
        'materialized_config_child_command_not_absolute',
        'materialized_config_proxy_command_not_absolute',
        'materialized_config_registrar_command_not_absolute',
      ],
    );

    const nativeArgs = [...f.args];
    nativeArgs.splice(nativeArgs.indexOf('--'), 0, '--child-invocation-kind', 'native_entrypoint');
    const nativeEntrypointMismatch = {
      mcpServers: {
        fixture: {
          command: process.execPath,
          args: nativeArgs,
        },
      },
    };
    const nativeMismatchResult = validateMaterializedConfiguration({
      structured: nativeEntrypointMismatch,
      artifactManifestPath: f.manifestPath,
      runtimeProxyEntrypoint: f.proxyPath,
      expectedSidecarPath: f.sidecarPath,
      requireSidecar: true,
    });
    assert.equal(nativeMismatchResult.ok, false);
    assert.equal(nativeMismatchResult.errors.some((error) => error.code === 'materialized_config_native_child_entrypoint_mismatch'), true);

    assert.equal(preflightMaterializationGeneration({
      sidecarPath: join(f.root, 'missing-generation.json'),
      manifestPath: f.manifestPath,
      manifestFingerprint: null,
      materializationContractEntrypoint: f.contractEntrypoint,
      runtimeProxyEntrypoint: f.proxyPath,
    }).code, 'materialization_generation_missing');
  } finally {
    rmSync(f.root, { recursive: true, force: true });
  }
});

test('TypeScript compatibility wrapper consumes the shared Rust fingerprint vectors', () => {
  const f = fixture();
  try {
    const corpus = JSON.parse(readFileSync(resolve(
      process.cwd(),
      '../mcp-materialization-contract-native/contracts/fingerprint-vectors.json',
    ), 'utf8')) as { vectors: FingerprintVector[] };
    for (const vector of corpus.vectors) {
      const description = describeMaterializedConfiguration({
        entrypoint: f.contractEntrypoint,
        carrierKind: vector.carrier_kind,
        content: vector.content,
        selectors: vector.selectors,
      });
      assert.equal(description.config_artifact.bytes_sha256, vector.expected_bytes_sha256, `${vector.id} bytes`);
      assert.equal(description.managed_projection.sha256, vector.expected_managed_sha256, `${vector.id} managed`);
    }
  } finally {
    rmSync(f.root, { recursive: true, force: true });
  }
});

test('Codex project trust updates do not invalidate the managed MCP projection', () => {
  const f = fixture();
  const configPath = join(f.root, 'carrier.toml');
  const sidecarPath = materializationSidecarPath(configPath);
  const args = f.args.map((arg) => arg === f.sidecarPath ? sidecarPath : arg);
  const plan = writePlan(f.root, configPath, f.matrixPath);
  const content = [
    "[projects.'C:/workspace']",
    'trust_level = "trusted"',
    '',
    '[mcp_servers.fixture]',
    `command = ${JSON.stringify(process.execPath)}`,
    `args = ${JSON.stringify(args)}`,
    '',
  ].join('\n');
  try {
    const validation = validateMaterializedConfiguration({
      structured: { mcpServers: { fixture: { command: process.execPath, args } } },
      artifactManifestPath: f.manifestPath,
      runtimeProxyEntrypoint: f.proxyPath,
      expectedSidecarPath: sidecarPath,
      requireSidecar: true,
    });
    assert.equal(validation.ok, true, JSON.stringify(validation));
    writeFileSync(configPath, content, 'utf8');
    const generation = buildMaterializationGeneration({
      carrierId: 'fixture-carrier',
      carrierKind: 'codex',
      configPath,
      content,
      managedSelectors: ['/mcp_servers'],
      materializationContractEntrypoint: f.contractEntrypoint,
      artifactManifestPath: f.manifestPath,
      artifactManifestFingerprint: 'fixture-manifest-fingerprint',
      runtimeProfileKind: 'bun',
      runtimeMaterializationPlanPath: plan.path,
      runtimeMaterializationPlanFingerprint: plan.fingerprint,
      runtimeImplementationMatrixPath: f.matrixPath,
      runtimeImplementationMatrixFingerprint: sha256(readFileSync(f.matrixPath)),
      registrarEntrypoint: f.registrarPath,
      proxyImplementation: 'bun',
      proxyEntrypoint: f.proxyPath,
      serverCount: validation.server_count,
      proxyCount: validation.proxy_count,
    });
    writeMaterializationGeneration(sidecarPath, generation);

    const codexUserSettings = [
      'approvals_reviewer = "auto_review"',
      '',
      content,
      '[tui]',
      'resume_cwd = "session"',
      '',
      '[windows]',
      'sandbox = "elevated"',
      '',
    ].join('\n');
    writeFileSync(configPath, codexUserSettings, 'utf8');
    const userSettingsPreflight = preflightMaterializationGeneration({
      sidecarPath,
      manifestPath: f.manifestPath,
      manifestFingerprint: 'fixture-manifest-fingerprint',
      materializationContractEntrypoint: f.contractEntrypoint,
      runtimeProxyEntrypoint: f.proxyPath,
    });
    assert.equal(userSettingsPreflight.ok, true);
    assert.equal(userSettingsPreflight.observations?.[0]?.code, 'materialization_artifact_bytes_drift');

    writeFileSync(configPath, `${codexUserSettings}[projects.'C:\\Users\\Andrey']\ntrust_level = "trusted"\n\n`, 'utf8');
    const projectTrustPreflight = preflightMaterializationGeneration({
      sidecarPath,
      manifestPath: f.manifestPath,
      manifestFingerprint: 'fixture-manifest-fingerprint',
      materializationContractEntrypoint: f.contractEntrypoint,
      runtimeProxyEntrypoint: f.proxyPath,
    });
    assert.equal(projectTrustPreflight.ok, true);
    assert.equal(projectTrustPreflight.observations?.[0]?.code, 'materialization_artifact_bytes_drift');

    writeFileSync(configPath, codexUserSettings.replace(`command = ${JSON.stringify(process.execPath)}`, 'command = "pnpm"'), 'utf8');
    assert.equal(preflightMaterializationGeneration({
      sidecarPath,
      manifestPath: f.manifestPath,
      manifestFingerprint: 'fixture-manifest-fingerprint',
      materializationContractEntrypoint: f.contractEntrypoint,
      runtimeProxyEntrypoint: f.proxyPath,
    }).code, 'materialization_managed_projection_stale');
  } finally {
    rmSync(f.root, { recursive: true, force: true });
  }
});
