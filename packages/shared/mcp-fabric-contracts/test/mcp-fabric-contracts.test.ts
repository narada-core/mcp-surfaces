import assert from 'node:assert/strict';
import { access, readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import {
  MCP_FABRIC_SCHEMA_VERSION,
  fabricManifestDigest,
  parseFabricManifestV2,
  parseSurfaceDescriptorV2,
  surfaceDescriptorDigest,
  assertLiveToolsConform,
  CarrierRestartOutcomeV1Schema,
  CarrierRestartRequestV1Schema,
  RuntimeLifecycleEventV1Schema,
  RuntimeMemoryIncidentV1Schema,
  RuntimeResourceOwnerV1Schema,
  RuntimeResourceSampleV1Schema,
  defineSurface,
  defineNativeSurface,
  surfaceExecutionDeclaration,
  type SurfaceDescriptorV2,
  bindingAdmissionEnvelopeDigest,
  bindingAdmissionEntryDigest,
  canonicalJson,
  parseMcpBindingAdmissionEnvelopeV1,
} from '../src/index.js';
import { startHttpFixture } from '../src/http-fixture.js';

test('binding identity golden vectors fix canonical bytes and digest', async () => {
  const vectors = JSON.parse(await readFile(
    new URL('../../contracts/binding-identity-v1.vectors.json', import.meta.url),
    'utf8',
  ));
  assert.equal(vectors.schema, 'narada.mcp.binding_identity_vectors.v1');
  for (const vector of vectors.vectors) {
    assert.equal(canonicalJson(vector.unsigned), vector.canonical_json, vector.name);
    assert.equal(bindingAdmissionEntryDigest(vector.unsigned), vector.sha256, vector.name);
  }
});

test('binding admission envelope is exact, order-stable, and digest-fenced', () => {
  const unsigned: any = {
    schema: 'narada.mcp.binding_admission_envelope.v1', envelope_id: 'envelope-1', decision: 'admitted',
    issued_at: '2026-08-12T00:00:00.000Z', valid_until: null, principal_key: 'local:site:agent',
    site_id: 'site-1', carrier_session_id: 'session-1', carrier_kind: 'codex', runtime_kind: 'narada-runtime',
    authority_epoch: 1, carrier_session_admission_receipt_ref: 'receipt:1', authority_readback_ref: 'authority:1',
    fabric_digest: 'a'.repeat(64),
    bindings: [
      { binding_id: 'binding-b', surface_id: 'surface-b', projection_id: 'default', authority_locus: { kind: 'local_site' }, injection_scope: 'local_site', operations: ['restart', 'attach', 'discover'], binding_identity: { schema: 'narada.mcp.binding_identity.v1', binding_id: 'binding-b', surface_id: 'surface-b', projection_id: 'default', injection_scope: 'local_site', authority_locus: { kind: 'local_site' }, transport: 'stdio', command: 'b', args: [], env: {}, env_vars: [], target_site_root: null, surface_projection: null }, binding_digest: 'b'.repeat(64) },
      { binding_id: 'binding-a', surface_id: 'surface-a', projection_id: 'default', authority_locus: { kind: 'local_site' }, injection_scope: 'local_site', operations: ['discover', 'attach'], binding_identity: { schema: 'narada.mcp.binding_identity.v1', binding_id: 'binding-a', surface_id: 'surface-a', projection_id: 'default', injection_scope: 'local_site', authority_locus: { kind: 'local_site' }, transport: 'stdio', command: 'a', args: [], env: {}, env_vars: [], target_site_root: null, surface_projection: null }, binding_digest: 'c'.repeat(64) },
    ],
  };
  const envelope = { ...unsigned, envelope_digest: bindingAdmissionEnvelopeDigest(unsigned) };
  assert.deepEqual(parseMcpBindingAdmissionEnvelopeV1(envelope).bindings.map((binding) => binding.binding_id), ['binding-a', 'binding-b']);
  assert.throws(() => parseMcpBindingAdmissionEnvelopeV1({ ...envelope, authority_epoch: 2 }), /digest_mismatch/);
  assert.throws(() => parseMcpBindingAdmissionEnvelopeV1({ ...envelope, bindings: [envelope.bindings[0], envelope.bindings[0]], envelope_digest: bindingAdmissionEnvelopeDigest({ ...unsigned, bindings: [envelope.bindings[0], envelope.bindings[0]] }) }));
  const competing = {
    ...envelope.bindings[0],
    binding_id: 'binding-c',
    binding_identity: { ...envelope.bindings[0].binding_identity, binding_id: 'binding-c' },
  };
  competing.binding_digest = bindingAdmissionEntryDigest(competing);
  const competingUnsigned = { ...unsigned, bindings: [envelope.bindings[0], competing] };
  assert.throws(
    () => parseMcpBindingAdmissionEnvelopeV1({ ...competingUnsigned, envelope_digest: bindingAdmissionEnvelopeDigest(competingUnsigned) }),
    /admitted surface/,
  );
});

test('runtime memory observation contracts are strict, versioned, and sanitized', () => {
  const now = new Date().toISOString();
  assert.equal(RuntimeResourceOwnerV1Schema.parse({
    schema: 'narada.mcp_runtime.resource_owner.v1', owner_id: 'owner-1', site_id: 'site-1', authority_ref: 'site:site-1',
    owner_kind: 'surface_worker', pid: 10, process_started_at: null, parent_owner_id: null, surface_id: 'fixture',
    instance_id: 'instance-1', generation_id: 'generation-1', carrier_session_id: null, executable_name: 'node', observed_at: now,
  }).pid, 10);
  assert.equal(RuntimeLifecycleEventV1Schema.parse({
    schema: 'narada.mcp_runtime.lifecycle_event.v1', event_id: 'event-1', occurred_at: now, site_id: 'site-1',
    authority_ref: 'site:site-1', owner_id: 'owner-1', event_type: 'invocation_terminal', surface_id: 'fixture',
    instance_id: 'instance-1', generation_id: 'generation-1', request_id: 'request-1', status: 'ok', inflight: 0,
  }).status, 'ok');
  assert.equal(RuntimeResourceSampleV1Schema.parse({
    schema: 'narada.mcp_runtime.resource_sample.v1', sample_id: 'sample-1', sampled_at: now, owner_id: 'owner-1', pid: 10,
    process_started_at: null, process: null, worker: { heap_total_bytes: 10, heap_used_bytes: 5, external_bytes: 2,
      array_buffers_bytes: 1, heap_limit_bytes: 100, active_resource_counts: { Timeout: 1 }, invocation_count: 2, inflight: 0 },
    sample_status: 'partial', unavailable_reason: 'process_probe_unavailable',
  }).worker?.heap_used_bytes, 5);
  assert.equal(RuntimeMemoryIncidentV1Schema.parse({
    schema: 'narada.mcp_runtime.memory_incident.v1', incident_id: 'incident-1', site_id: 'site-1', authority_ref: 'site:site-1',
    owner_id: 'owner-1', opened_at: now, updated_at: now, status: 'open', detector: 'worker_heap_growth', attribution: 'direct',
    confidence: 0.9, baseline_bytes: 10, observed_bytes: 20, slope_bytes_per_minute: 2, evidence_refs: [], review_note: null,
  }).detector, 'worker_heap_growth');
  assert.throws(() => RuntimeLifecycleEventV1Schema.parse({ schema: 'narada.mcp_runtime.lifecycle_event.v1', secret: 'not-allowed' }));
});

function descriptor(): SurfaceDescriptorV2 {
  return {
    schema_version: MCP_FABRIC_SCHEMA_VERSION,
    source: 'native',
    surface_id: 'example',
    surface_version: '1.0.0',
    package: '@example/mcp',
    guidance_tool: 'example_guidance',
    tools: [
      {
        name: 'example_read',
        description: 'Read one example.',
        input_schema: { type: 'object', properties: { b: {}, a: {} } },
        effect: { class: 'read', idempotency: 'replayable', confirmation: 'never' },
      },
      {
        name: 'example_guidance',
        description: 'Show guidance.',
        input_schema: { type: 'object' },
        effect: { class: 'read', idempotency: 'replayable', confirmation: 'never' },
      },
    ],
    projections: [
      {
        id: 'default',
        transport: {
          kind: 'stdio',
          command: 'node',
          args: ['dist/main.js', '--mode', 'read'],
          env: ['SITE_ROOT', 'OUTPUT_ROOT'],
        },
        injection_scope: 'local_site',
        default_injection: 'enabled',
        runtime_requirements: ['nars'],
        authority_requirements: ['site.local', 'site.read'],
        lifecycle: {
          mode: 'restart_required',
          restart_owner: 'mcp-loader',
        },
      },
    ],
  };
}

test('descriptor digest is stable across declaration and object-key order', () => {
  const left = descriptor();
  const right = descriptor();
  right.tools.reverse();
  right.projections[0]!.runtime_requirements.reverse();
  right.projections[0]!.authority_requirements.reverse();
  const transport = right.projections[0]!.transport;
  assert.equal(transport.kind, 'stdio');
  if (transport.kind === 'stdio') {
    right.projections[0]!.transport = {
      ...transport,
      env: ['OUTPUT_ROOT', 'SITE_ROOT'],
    };
  }
  right.tools[1]!.input_schema = {
    properties: { a: {}, b: {} },
    type: 'object',
  };
  assert.equal(surfaceDescriptorDigest(left), surfaceDescriptorDigest(right));
});

test('common descriptor contract distinguishes Site-local ownership from the global native catalog', () => {
  const siteLocal = { ...descriptor(), source: 'site_local' as const };
  assert.equal(parseSurfaceDescriptorV2(siteLocal).source, 'site_local');
});

test('defineSurface uses one registry for tools/list and descriptor emission', () => {
  const definition = {
    name: 'example_guidance',
    description: 'Show guidance.',
    inputSchema: { type: 'object', properties: {}, additionalProperties: false },
  };
  const surface = defineSurface({
    surface_id: 'single-source',
    surface_version: '1.0.0',
    package: '@example/single-source',
    tools: [{
      definition,
      effect: { class: 'read', idempotency: 'replayable', confirmation: 'never' },
    }],
    projections: [descriptor().projections[0]!],
  });
  assert.deepEqual(surface.tools, [definition]);
  assertLiveToolsConform(surface.descriptor, surface.tools);
  assert.equal(surface.descriptor.guidance_tool, 'example_guidance');
});

test('defineNativeSurface validates read-only inventory and exposes lifecycle readback', () => {
  const definition = {
    name: 'native_guidance',
    description: 'Show native guidance.',
    inputSchema: { type: 'object', additionalProperties: false },
  };
  const base = {
    surface_id: 'native-helper',
    surface_version: '1.0.0',
    package: '@example/native-helper',
    entrypoint: 'dist/main.js',
    tools: [definition],
    read_only_tools: ['native_guidance'] as const,
    default_effect: 'read' as const,
    projections: [descriptor().projections[0]!],
  };
  const surface = defineNativeSurface(base);
  assert.deepEqual(surface.descriptor.metadata?.lifecycle_readback, {
    authority: 'mcp-loader',
    availability: 'loader-managed',
    discovery: {
      tool_name: 'mcp_loader_connection_inventory',
      arguments: {},
      select: { field: 'surface_id', equals: 'native-helper', result_field: 'connection_id' },
    },
    status: {
      tool_name: 'mcp_loader_surface_status',
      arguments: { connection_id: '{connection_id}' },
      connection_id_from: 'discovery.selected.connection_id',
    },
  });
  assert.throws(
    () => defineNativeSurface({ ...base, read_only_tools: ['stale_tool'] as const }),
    /mcp_fabric_read_only_tool_undeclared/,
  );
});

test('Streamable HTTP fixture is session-pinned and conforms to fresh tools/list', async () => {
  const fixture = await startHttpFixture();
  try {
    const response = await fetch(fixture.url, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list', params: {} }),
    });
    assert.equal(response.status, 200);
    const message = await response.json() as {
      result: { tools: Array<{ name: string; description: string; inputSchema: Record<string, unknown> }> };
    };
    assertLiveToolsConform(fixture.surface.descriptor, message.result.tools);
    assert.equal(fixture.surface.descriptor.projections[0]!.lifecycle.mode, 'session_pinned');
  } finally {
    await fixture.close();
  }
});

test('unsupported schema majors fail closed', () => {
  assert.throws(
    () => parseSurfaceDescriptorV2({ ...descriptor(), schema_version: '3.0' }),
    /mcp_fabric_schema_major_unsupported/,
  );
});

test('duplicate tool and projection identities are rejected', () => {
  const duplicateTool = descriptor();
  duplicateTool.tools.push({ ...duplicateTool.tools[0]! });
  assert.throws(() => parseSurfaceDescriptorV2(duplicateTool), /duplicate tool/);

  const duplicateProjection = descriptor();
  duplicateProjection.projections.push({ ...duplicateProjection.projections[0]! });
  assert.throws(() => parseSurfaceDescriptorV2(duplicateProjection), /duplicate projection/);
});

test('projection tool inventories are declared subsets and reject drift at definition time', () => {
  const projected = descriptor();
  projected.projections[0]!.exposed_tools = ['example_read'];
  assert.deepEqual(
    parseSurfaceDescriptorV2(projected).projections[0]!.exposed_tools,
    ['example_read'],
  );

  const duplicate = descriptor();
  duplicate.projections[0]!.exposed_tools = ['example_read', 'example_read'];
  assert.throws(
    () => parseSurfaceDescriptorV2(duplicate),
    /duplicate projection tool/,
  );

  const undeclared = descriptor();
  undeclared.projections[0]!.exposed_tools = ['not_declared'];
  assert.throws(
    () => parseSurfaceDescriptorV2(undeclared),
    /projection exposed_tools must name declared tools/,
  );
});

test('invalid effect and lifecycle combinations are rejected', () => {
  const invalidEffect = descriptor();
  invalidEffect.tools[0]!.effect = {
    class: 'read',
    idempotency: 'non_idempotent',
    confirmation: 'always',
  };
  assert.throws(() => parseSurfaceDescriptorV2(invalidEffect), /read effects/);

  const invalidLifecycle = descriptor();
  invalidLifecycle.projections[0]!.lifecycle = { mode: 'restart_required' } as never;
  assert.throws(() => parseSurfaceDescriptorV2(invalidLifecycle), /restart_owner/);
});

test('manifest digest is stable and duplicate bindings fail closed', () => {
  const manifest = {
    schema_version: MCP_FABRIC_SCHEMA_VERSION,
    manifest_id: 'example-manifest',
    site_id: 'example-site',
    generated_at: '2026-07-19T00:00:00.000Z',
    descriptors: [descriptor()],
    bindings: [
      {
        binding_id: 'example-binding',
        surface_id: 'example',
        projection_id: 'default',
        server_name: 'example',
        enabled: true,
        config: { z: 1, a: 2 },
      },
    ],
    source_digest: 'a'.repeat(64),
  };
  const parsed = parseFabricManifestV2(manifest);
  assert.equal(fabricManifestDigest(parsed), fabricManifestDigest({
    ...manifest,
    bindings: [{ ...manifest.bindings[0]!, config: { a: 2, z: 1 } }],
  }));
  assert.throws(
    () => parseFabricManifestV2({
      ...manifest,
      bindings: [...manifest.bindings, { ...manifest.bindings[0]! }],
    }),
    /duplicate binding/,
  );
});

test('surface execution posture is conservative by default and participates in descriptor identity', () => {
  assert.deepEqual(surfaceExecutionDeclaration(undefined), {
    adapter: 'stdio',
    tenancy: 'session_isolated',
    replacement: 'manual',
  });

  const isolated = descriptor();
  const normalized = parseSurfaceDescriptorV2(isolated);
  assert.equal(normalized.projections[0]!.execution, undefined);

  const shared = descriptor();
  shared.projections[0]!.execution = {
    adapter: 'surface_factory',
    tenancy: 'authority_shared',
    replacement: 'generation_swap',
  };
  assert.notEqual(surfaceDescriptorDigest(isolated), surfaceDescriptorDigest(shared));
  assert.deepEqual(parseSurfaceDescriptorV2(shared).projections[0]!.execution, shared.projections[0]!.execution);
});

test('carrier restart request and outcome contracts preserve explicit authority evidence', () => {
  const request = CarrierRestartRequestV1Schema.parse({
    schema: 'narada.pc_runtime.carrier_restart_request.v1',
    operation_id: 'restart-op-1',
    requested_at: '2026-07-30T00:00:00.000Z',
    requested_by: 'principal-andrey',
    site_id: 'site-local',
    carrier_session_id: 'carrier_20260730000000_abc123',
    expected_state: {
      manifest_digest: null,
      observation_digest: 'a'.repeat(64),
      descriptor_digest: null,
    },
    reason: 'runtime health degraded',
    timeout_ms: 60_000,
    dry_run: false,
  });
  assert.equal(request.schema, 'narada.pc_runtime.carrier_restart_request.v1');

  const outcome = CarrierRestartOutcomeV1Schema.parse({
    schema: 'narada.pc_runtime.carrier_restart_outcome.v1',
    operation_id: request.operation_id,
    requested_at: request.requested_at,
    completed_at: null,
    requested_by: request.requested_by,
    site_id: request.site_id,
    source_session_id: request.carrier_session_id,
    target_session_id: null,
    status: 'running',
    transition_state: 'source_draining',
    source_retired: false,
    reason: request.reason,
    error_code: null,
    evidence: { source_write_admission: 'draining' },
  });
  assert.equal(outcome.transition_state, 'source_draining');
});

test('postcompile emits JSON Schema artifacts', async () => {
  const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
  await access(path.join(packageRoot, 'dist', 'schema', 'surface-descriptor.schema.json'));
  await access(path.join(packageRoot, 'dist', 'schema', 'fabric-manifest.schema.json'));
});
