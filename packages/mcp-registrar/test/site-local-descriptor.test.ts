import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  buildSiteSurfaceRegistry,
  checkSiteRegistryConformance,
} from '../src/main.js';

type JsonRecord = Record<string, unknown>;

const root = mkdtempSync(join(tmpdir(), 'mcp-registrar-site-local-descriptor-'));

function descriptor(surfaceId = 'fixture-domain', entrypoint = join(root, 'site', 'server.mjs')): JsonRecord {
  return {
    schema_version: '2.0',
    source: 'site_local',
    surface_id: surfaceId,
    surface_version: '1.0.0',
    package: '@fixture/site-local-domain',
    guidance_tool: null,
    tools: [
      {
        name: `${surfaceId.replace(/-/g, '_')}_read`,
        description: 'Read bounded Site-local domain state.',
        input_schema: { type: 'object', properties: {}, additionalProperties: false },
        output_schema: { type: 'object', additionalProperties: true },
        annotations: { readOnlyHint: true },
        effect: { class: 'read', idempotency: 'replayable', confirmation: 'never' },
      },
      {
        name: `${surfaceId.replace(/-/g, '_')}_write`,
        description: 'Apply a governed Site-local domain mutation.',
        input_schema: { type: 'object', properties: {}, additionalProperties: false },
        output_schema: { type: 'object', additionalProperties: true },
        annotations: { readOnlyHint: false },
        effect: { class: 'local_write', idempotency: 'idempotent', confirmation: 'policy' },
      },
    ],
    projections: [{
      id: 'site-local',
      transport: {
        kind: 'stdio',
        command: process.execPath,
        args: [entrypoint, '--site-root', '{site_root}'],
        env: [],
      },
      exposed_tools: [
        `${surfaceId.replace(/-/g, '_')}_read`,
        `${surfaceId.replace(/-/g, '_')}_write`,
      ],
      injection_scope: 'local_site',
      default_injection: 'disabled',
      runtime_requirements: [],
      authority_requirements: [],
      lifecycle: { mode: 'replayable' },
    }],
  };
}

function writeFixture(
  siteRoot: string,
  options: {
    surfaceId?: string;
    descriptorValue?: JsonRecord;
    descriptorPath?: string | null;
    serverArgs?: string[];
    tools?: string[];
  } = {},
) {
  const surfaceId = options.surfaceId ?? 'fixture-domain';
  const descriptorPath = options.descriptorPath === undefined ? 'governed/fixture-domain.surface.json' : options.descriptorPath;
  const entrypoint = join(siteRoot, 'server.mjs');
  mkdirSync(join(siteRoot, '.ai', 'mcp'), { recursive: true });
  mkdirSync(join(siteRoot, 'governed'), { recursive: true });
  writeFileSync(entrypoint, 'process.stdin.resume();\n', 'utf8');
  writeFileSync(join(siteRoot, 'config.json'), JSON.stringify({ workspace_root: siteRoot }), 'utf8');
  if (descriptorPath && !descriptorPath.startsWith('..')) {
    const descriptorFile = join(siteRoot, descriptorPath);
    mkdirSync(join(descriptorFile, '..'), { recursive: true });
    writeFileSync(descriptorFile, JSON.stringify(options.descriptorValue ?? descriptor(surfaceId, entrypoint), null, 2) + '\n', 'utf8');
  }
  const toolPrefix = surfaceId.replace(/-/g, '_');
  const server: JsonRecord = {
    command: process.execPath,
    args: options.serverArgs ?? [entrypoint, '--site-root', siteRoot],
    surface_id: surfaceId,
    ...(descriptorPath === null ? {} : { surface_descriptor_path: descriptorPath }),
    tools: options.tools ?? [`${toolPrefix}_read`, `${toolPrefix}_write`],
  };
  writeFileSync(join(siteRoot, '.ai', 'mcp', `${surfaceId}.json`), JSON.stringify({ mcpServers: { [surfaceId]: server } }, null, 2) + '\n', 'utf8');
  return {
    site: { site_id: 'fixture-site', root: siteRoot, config_path: join(siteRoot, 'config.json'), surfaces: [] },
    descriptorPath,
    entrypoint,
  };
}

try {
  const siteRoot = join(root, 'site');
  const fixture = writeFixture(siteRoot);
  const registry = buildSiteSurfaceRegistry(fixture.site) as any;
  assert.equal(registry.surfaces.length, 1);
  const surface = registry.surfaces[0];
  assert.equal(surface.catalog_surface_id, 'fixture-domain');
  assert.equal(surface.surface_type, 'site_local_mcp_surface');
  assert.equal(surface.authority_boundary.owner_site_id, 'fixture-site');
  assert.equal(surface.authority_boundary.injection_scope, 'local_site');
  assert.deepEqual(surface.tool_contract.read_only_tools, ['fixture_domain_read']);
  assert.deepEqual(surface.tool_contract.mutating_tools, ['fixture_domain_write']);
  assert.deepEqual(surface.registered_live_tools, ['fixture_domain_read', 'fixture_domain_write']);
  assert.equal(surface.descriptor_provenance.source, 'site_local');
  assert.equal(surface.descriptor_provenance.owner_site_id, 'fixture-site');
  assert.equal(surface.descriptor_provenance.path, 'governed/fixture-domain.surface.json');
  assert.match(surface.descriptor_provenance.content_sha256, /^[a-f0-9]{64}$/);
  assert.equal(surface.descriptor_provenance.descriptor_digest, surface.evidence.descriptor_digest);
  assert.equal(surface.surface_descriptor.schema_version, '2.0');
  assert.equal(surface.surface_descriptor.surface_id, 'fixture-domain');
  assert.equal(
    surface.descriptor_provenance.content_sha256,
    createHash('sha256').update(JSON.stringify(descriptor('fixture-domain', fixture.entrypoint), null, 2) + '\n').digest('hex'),
  );

  const conformance = checkSiteRegistryConformance(
    fixture.site,
    registry,
    { 'fixture-domain': ['fixture_domain_read', 'fixture_domain_write'] },
    { 'fixture-domain': ['fixture_domain_read'] },
    { 'fixture-domain': ['fixture_domain_write'] },
    true,
  ) as any;
  assert.equal(conformance.status, 'ok');
  assert.equal(conformance.violation_count, 0);

  const missingRoot = join(root, 'missing');
  const missing = writeFixture(missingRoot, { descriptorPath: null });
  assert.throws(
    () => buildSiteSurfaceRegistry(missing.site),
    (error: any) => error?.codeName === 'registrar_site_local_descriptor_missing',
  );

  const escapeRoot = join(root, 'escape');
  const outsideDescriptor = join(root, 'outside.surface.json');
  writeFileSync(outsideDescriptor, JSON.stringify(descriptor('escape-domain', join(escapeRoot, 'server.mjs'))), 'utf8');
  const escaped = writeFixture(escapeRoot, { surfaceId: 'escape-domain', descriptorPath: '../outside.surface.json' });
  assert.throws(
    () => buildSiteSurfaceRegistry(escaped.site),
    (error: any) => error?.codeName === 'registrar_site_local_descriptor_path_escape',
  );

  const collisionRoot = join(root, 'collision');
  const collision = writeFixture(collisionRoot, { surfaceId: 'mailbox' });
  assert.throws(
    () => buildSiteSurfaceRegistry(collision.site),
    (error: any) => error?.codeName === 'registrar_site_local_descriptor_global_collision',
  );

  const mismatchRoot = join(root, 'identity-mismatch');
  const mismatch = writeFixture(mismatchRoot, {
    descriptorValue: descriptor('different-domain', join(mismatchRoot, 'server.mjs')),
  });
  assert.throws(
    () => buildSiteSurfaceRegistry(mismatch.site),
    (error: any) => error?.codeName === 'registrar_site_local_descriptor_identity_mismatch',
  );

  const transportRoot = join(root, 'transport-mismatch');
  const transport = writeFixture(transportRoot, { serverArgs: [join(transportRoot, 'server.mjs'), '--wrong'] });
  assert.throws(
    () => buildSiteSurfaceRegistry(transport.site),
    (error: any) => error?.codeName === 'registrar_site_local_descriptor_transport_mismatch',
  );

  const toolRoot = join(root, 'tool-mismatch');
  const toolMismatch = writeFixture(toolRoot, { tools: ['fixture_domain_read'] });
  assert.throws(
    () => buildSiteSurfaceRegistry(toolMismatch.site),
    (error: any) => error?.codeName === 'registrar_site_local_descriptor_tools_mismatch',
  );
} finally {
  rmSync(root, { recursive: true, force: true });
}
