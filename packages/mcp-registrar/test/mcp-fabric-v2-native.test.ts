import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { assertLiveToolsConform } from '@narada-core/mcp-fabric-contracts';
import { nativeSurfaceDescriptor, SURFACES } from '../src/main.js';

function findRepositoryRoot(start: string): string {
  let current = resolve(start);
  while (true) {
    if (existsSync(resolve(current, 'pnpm-workspace.yaml'))) return current;
    const parent = dirname(current);
    if (parent === current) throw new Error('mcp_fabric_test_repository_root_not_found');
    current = parent;
  }
}

const REPOSITORY_ROOT = findRepositoryRoot(dirname(fileURLToPath(import.meta.url)));
const MCP_SURFACES_ROOT = resolve(REPOSITORY_ROOT, 'packages').replace(/\\/g, '/');

test('every registered surface is backed by a package-owned native descriptor', () => {
  assert.ok(SURFACES.length > 0);
  for (const surface of SURFACES) {
    const descriptor = nativeSurfaceDescriptor(surface.id);
    assert.equal(descriptor.source, 'native', surface.id);
    assert.equal(descriptor.surface_id, surface.id);
    assert.equal(descriptor.package, '@narada-core/' + surface.package);
    const defaultProjection = descriptor.projections.find((projection) => projection.id === 'default')
      ?? descriptor.projections[0];
    assert.deepEqual(
      defaultProjection?.exposed_tools ?? descriptor.tools.map((tool) => tool.name),
      surface.tools,
      'native default projection changed for ' + surface.id,
    );
    assertLiveToolsConform(descriptor, descriptor.tools.map((tool) => ({
      name: tool.name,
      description: tool.description,
      inputSchema: tool.input_schema,
      ...(tool.output_schema === undefined ? {} : { outputSchema: tool.output_schema }),
      ...(tool.annotations === undefined ? {} : { annotations: tool.annotations }),
    })));
  }
});

test('native descriptors match package versions and advertise loader lifecycle readback', () => {
  for (const surface of SURFACES) {
    const descriptor = nativeSurfaceDescriptor(surface.id);
    const packageJson = JSON.parse(readFileSync(
      resolve(REPOSITORY_ROOT, 'packages', surface.package, 'package.json'),
      'utf8',
    )) as { version?: string };
    assert.equal(descriptor.surface_version, packageJson.version, surface.id);
    assert.deepEqual(descriptor.metadata?.lifecycle_readback, {
      authority: 'mcp-loader',
      availability: 'loader-managed',
      discovery: {
        tool_name: 'mcp_loader_connection_inventory',
        arguments: {},
        select: { field: 'surface_id', equals: surface.id, result_field: 'connection_id' },
      },
      status: {
        tool_name: 'mcp_loader_surface_status',
        arguments: { connection_id: '{connection_id}' },
        connection_id_from: 'discovery.selected.connection_id',
      },
    }, surface.id);
  }
});

test('native projection transport and registrar projection transport remain equivalent', () => {
  for (const surface of SURFACES) {
    const native = nativeSurfaceDescriptor(surface.id);
    const registrarProjections = surface.projections ?? [];
    assert.equal(registrarProjections.length, native.projections.length, surface.id);
    for (const nativeProjection of native.projections) {
      const registrarProjection = registrarProjections.find((candidate) => candidate.id === nativeProjection.id);
      assert.ok(registrarProjection, `${surface.id}:${nativeProjection.id} missing registrar projection`);
      assert.equal(nativeProjection.transport.kind, 'stdio');
      if (nativeProjection.transport.kind === 'stdio') {
        assert.equal(registrarProjection!.command, nativeProjection.transport.command);
        assert.deepEqual(
          [registrarProjection!.entrypoint, ...(registrarProjection!.args ?? [])],
          nativeProjection.transport.args.map((arg, index) => index === 0 && arg.includes('{mcp_surfaces_root}')
            ? arg.replace('{mcp_surfaces_root}', MCP_SURFACES_ROOT)
            : arg),
          `${surface.id}:${nativeProjection.id} transport drift`,
        );
      }
      assert.equal(registrarProjection!.injection_scope, nativeProjection.injection_scope);
      assert.deepEqual(registrarProjection!.runtime_requirements, nativeProjection.runtime_requirements.filter((value) => value === 'nars'));
    }
  }
});

test('native descriptors preserve explicit projection selection boundaries', () => {
  for (const surface of SURFACES) {
    const descriptor = nativeSurfaceDescriptor(surface.id);
    assert.equal(new Set(descriptor.projections.map((projection) => projection.id)).size, descriptor.projections.length);
    for (const projection of descriptor.projections) {
      assert.ok(projection.transport.kind === 'stdio', `${surface.id}:${projection.id} must be stdio for registrar carriers`);
      assert.ok(projection.lifecycle.mode.length > 0, `${surface.id}:${projection.id} lifecycle is required`);
      assert.ok(projection.injection_scope.length > 0, `${surface.id}:${projection.id} authority scope is required`);
    }
  }
});
