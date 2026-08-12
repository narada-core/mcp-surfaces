import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import {
  defineSurface,
  fabricManifestDigest,
  surfaceDescriptorDigest,
  surfaceToolContractDigest,
  type FabricBindingV2,
  type CarrierProjectionV2,
  type RuntimeObservationV2,
} from '@narada-core/mcp-fabric-contracts';
import {
  CarrierSchemaCompatibilityError,
  compileAllCarrierArtifacts,
  compileFabricManifest,
  transformMoonshotToolSchema,
  assertReconciliationApplyAllowed,
  reconcileFabricState,
} from '../src/index.js';

function fixture() {
  const surface = defineSurface({
    surface_id: 'compiler-fixture',
    surface_version: '1.0.0',
    package: '@example/compiler-fixture',
    tools: [
      {
        definition: {
          name: 'compiler_fixture_guidance',
          description: 'Show fixture guidance.',
          inputSchema: { type: 'object', properties: {}, additionalProperties: false },
        },
        effect: { class: 'read', idempotency: 'replayable', confirmation: 'never' },
      },
      {
        definition: {
          name: 'compiler_fixture_update',
          description: 'Update fixture state.',
          inputSchema: {
            type: 'object',
            properties: { value: { type: 'string' } },
            required: ['value'],
            additionalProperties: false,
          },
        },
        effect: { class: 'local_write', idempotency: 'idempotent', confirmation: 'policy' },
      },
    ],
    projections: [{
      id: 'stdio',
      transport: {
        kind: 'stdio',
        command: 'node',
        args: ['fixture.js'],
        env: ['TOKEN'],
      },
      injection_scope: 'local_site',
      default_injection: 'enabled',
      runtime_requirements: [],
      authority_requirements: [],
      lifecycle: { mode: 'replayable' },
    }],
  });
  const binding: FabricBindingV2 = {
    binding_id: 'compiler-fixture-binding',
    surface_id: surface.descriptor.surface_id,
    projection_id: 'stdio',
    server_name: 'compiler-fixture',
    enabled: true,
    config: { env: { TOKEN: 'secret', UNDECLARED: 'excluded' } },
  };
  return { surface, binding };
}

test('one immutable manifest deterministically compiles all carrier projections', () => {
  const { surface, binding } = fixture();
  const manifest = compileFabricManifest({
    manifest_id: 'compiler-test',
    site_id: 'test-site',
    generated_at: '2026-07-19T00:00:00.000Z',
    descriptors: [surface.descriptor],
    bindings: [binding],
  });
  const first = compileAllCarrierArtifacts(manifest);
  const second = compileAllCarrierArtifacts({
    ...manifest,
    descriptors: [...manifest.descriptors].reverse(),
    bindings: [...manifest.bindings].reverse(),
  });
  assert.equal(first.manifest_digest, second.manifest_digest);
  assert.equal(first.artifacts.codex.content, second.artifacts.codex.content);
  assert.equal(first.artifacts.kimi.content, second.artifacts.kimi.content);
  assert.equal(first.artifacts.opencode.content, second.artifacts.opencode.content);
  assert.equal(Object.isFrozen(first), true);
  assert.equal(Object.isFrozen(first.artifacts.kimi.document), true);

  assert.equal(first.artifacts.codex.content, [
    '[mcp_servers.compiler-fixture]',
    'command = "node"',
    'args = ["fixture.js"]',
    'env = { TOKEN = "secret" }',
    '',
  ].join('\n'));
  assert.deepEqual(first.artifacts.kimi.document, {
    mcpServers: {
      'compiler-fixture': {
        command: 'node',
        args: ['fixture.js'],
        env: { TOKEN: 'secret' },
      },
    },
    approvals: {
      allow: ['compiler-fixture-binding/compiler_fixture_guidance'],
      prompt: ['compiler-fixture-binding/compiler_fixture_update'],
    },
  });
  assert.deepEqual(first.artifacts.opencode.document, {
    mcp: {
      'compiler-fixture': {
        type: 'local',
        command: ['node', 'fixture.js'],
        environment: { TOKEN: 'secret' },
      },
    },
    approvals: [
      {
        binding_id: 'compiler-fixture-binding',
        server_name: 'compiler-fixture',
        tool_name: 'compiler_fixture_guidance',
        decision: 'allow',
        reasons: [],
      },
      {
        binding_id: 'compiler-fixture-binding',
        server_name: 'compiler-fixture',
        tool_name: 'compiler_fixture_update',
        decision: 'prompt',
        reasons: ['effect.confirmation=policy', 'effect.class=local_write'],
      },
    ],
  });
});

test('private materialization alias changes do not alter the carrier projection', () => {
  const { surface, binding } = fixture();
  const compile = (server_name: string) => compileAllCarrierArtifacts(compileFabricManifest({
    manifest_id: `alias-${server_name}`,
    site_id: 'test-site',
    generated_at: '2026-07-19T00:00:00.000Z',
    descriptors: [surface.descriptor],
    bindings: [{ ...binding, server_name }],
  })).artifacts.kimi.document;

  assert.deepEqual(compile('first-alias'), compile('second-alias'));
});

test('reconciliation deterministically assigns one bounded actuator and guards apply', () => {
  const { surface, binding } = fixture();
  const manifest = compileFabricManifest({
    manifest_id: 'reconcile-test',
    site_id: 'test-site',
    generated_at: '2026-07-19T00:00:00.000Z',
    descriptors: [surface.descriptor],
    bindings: [binding],
  });
  const manifestDigest = fabricManifestDigest(manifest);
  const carrier: CarrierProjectionV2 = {
    schema_version: '2.0',
    carrier_kind: 'codex',
    site_id: 'test-site',
    manifest_digest: manifestDigest,
    servers: [{
      server_name: binding.surface_id,
      surface_id: surface.descriptor.surface_id,
      projection_id: binding.projection_id,
      transport: surface.descriptor.projections[0]!.transport,
    }],
  };
  const observation: RuntimeObservationV2 = {
    schema_version: '2.0',
    observation_id: 'observation-test',
    observed_at: '2026-07-19T00:01:00.000Z',
    site_id: 'test-site',
    carrier_kind: 'codex',
    runtime_state_root: 'C:/runtime',
    manifest_digest: manifestDigest,
    servers: [{
      server_name: binding.surface_id,
      surface_id: surface.descriptor.surface_id,
      projection_id: binding.projection_id,
      logical_connection_id: 'connection-test',
      lifecycle: { mode: 'replayable' },
      active_generation: {
        generation_id: 'generation-one',
        state: 'active',
        started_at: '2026-07-19T00:00:00.000Z',
        activated_at: '2026-07-19T00:00:01.000Z',
        heartbeat_at: '2026-07-19T00:00:59.000Z',
        lease_expires_at: '2026-07-19T00:02:00.000Z',
        freshness: 'current',
        health: 'healthy',
        descriptor_digest: surfaceDescriptorDigest(surface.descriptor),
        tool_contract_digest: surfaceToolContractDigest(surface.descriptor),
        inflight: 0,
      },
      draining_generations: [],
      recovery_actions: [],
    }],
  };

  const noOp = reconcileFabricState({ manifest, carrier_projection: carrier, observation });
  assert.equal(noOp.actions.length, 1);
  assert.equal(noOp.actions[0]!.action, 'no_op');
  assert.equal(noOp.actions[0]!.actuator, 'fabric-observer');
  assert.deepEqual(
    reconcileFabricState({ manifest, carrier_projection: carrier, observation }),
    noOp,
  );
  assert.equal(
    assertReconciliationApplyAllowed({
      action: noOp.actions[0]!,
      current_observation: observation,
      granted_authorities: ['fabric.read'],
      operation_id: noOp.actions[0]!.operation_id,
    }).action,
    'no_op',
  );
  assert.throws(
    () => assertReconciliationApplyAllowed({
      action: noOp.actions[0]!,
      current_observation: { ...observation, observed_at: '2026-07-19T00:01:01.000Z' },
      granted_authorities: ['fabric.read'],
      operation_id: noOp.actions[0]!.operation_id,
    }),
    /mcp_fabric_stale_precondition/,
  );
  assert.throws(
    () => assertReconciliationApplyAllowed({
      action: noOp.actions[0]!,
      current_observation: observation,
      granted_authorities: [],
      operation_id: noOp.actions[0]!.operation_id,
    }),
    /mcp_fabric_authority_required/,
  );

  const contractDrift = structuredClone(observation);
  contractDrift.servers[0]!.active_generation!.tool_contract_digest = '0'.repeat(64);
  const replacement = reconcileFabricState({ manifest, carrier_projection: carrier, observation: contractDrift });
  assert.deepEqual(
    [replacement.actions[0]!.action, replacement.actions[0]!.actuator],
    ['replace_generation', 'mcp-loader'],
  );

  const restartManifest = structuredClone(manifest);
  restartManifest.descriptors[0]!.projections[0]!.lifecycle = {
    mode: 'restart_required',
    restart_owner: 'carrier-supervisor',
  };
  const restartDigest = fabricManifestDigest(restartManifest);
  const reconnect = reconcileFabricState({
    manifest: restartManifest,
    carrier_projection: { ...carrier, manifest_digest: restartDigest },
    observation: { ...contractDrift, manifest_digest: restartDigest },
  });
  assert.deepEqual(
    [reconnect.actions[0]!.action, reconnect.actions[0]!.actuator],
    ['reconnect_required', 'carrier-supervisor'],
  );

  const rematerialize = reconcileFabricState({
    manifest,
    carrier_projection: carrier,
    observation: { ...observation, manifest_digest: '0'.repeat(64) },
  });
  assert.deepEqual(
    [rematerialize.actions[0]!.action, rematerialize.actions[0]!.actuator],
    ['rematerialize_all_carrier_configs', 'narada-mcp-materializer'],
  );

  const unsupported = reconcileFabricState({
    manifest,
    carrier_projection: carrier,
    observation: { ...observation, site_id: 'other-site' },
  });
  assert.equal(unsupported.actions[0]!.action, 'unsupported');
});

test('approval posture changes only with canonical effects and authority requirements', () => {
  const { surface, binding } = fixture();
  const manifest = compileFabricManifest({
    manifest_id: 'authority-test',
    site_id: 'test-site',
    generated_at: '2026-07-19T00:00:00.000Z',
    descriptors: [{
      ...surface.descriptor,
      projections: [{
        ...surface.descriptor.projections[0]!,
        authority_requirements: ['operator.confirmed'],
      }],
    }],
    bindings: [binding],
  });
  const approvals = compileAllCarrierArtifacts(manifest).artifacts.codex.approvals;
  assert.equal(approvals.every((approval) => approval.decision === 'prompt'), true);
  assert.equal(
    approvals.find((approval) => approval.tool_name.endsWith('_guidance'))?.reasons.includes('authority=operator.confirmed'),
    true,
  );
});

test('Moonshot anyOf parent type is transformed losslessly and conflicts are precise', () => {
  assert.deepEqual(transformMoonshotToolSchema('union_tool', {
    type: 'object',
    anyOf: [
      { properties: { a: { type: 'string' } }, required: ['a'] },
      { properties: { b: { type: 'number' } }, required: ['b'] },
    ],
  }), {
    anyOf: [
      { properties: { a: { type: 'string' } }, required: ['a'], type: 'object' },
      { properties: { b: { type: 'number' } }, required: ['b'], type: 'object' },
    ],
  });

  assert.throws(
    () => transformMoonshotToolSchema('conflict_tool', {
      type: 'object',
      anyOf: [{ type: 'string' }],
    }),
    (error) => {
      assert.equal(error instanceof CarrierSchemaCompatibilityError, true);
      const diagnostic = (error as CarrierSchemaCompatibilityError).diagnostics[0]!;
      assert.equal(diagnostic.tool_name, 'conflict_tool');
      assert.equal(diagnostic.schema_path, 'root.anyOf[0].type');
      assert.match(diagnostic.dialect, /MoonshotAI/);
      assert.match(diagnostic.remediation, /package-owned schema/);
      return true;
    },
  );
});

test('compiler production source has no filesystem mutation dependency', async () => {
  const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
  const source = await readFile(path.join(packageRoot, 'src', 'index.ts'), 'utf8');
  assert.doesNotMatch(source, /node:fs|writeFile|rename|unlink|mkdir/);
});
