import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { gzipSync, gunzipSync } from 'node:zlib';
import { canonicalJson } from '../packages/shared/mcp-fabric-contracts/src/index.ts';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import { surfaceDefinition as filesystemSurface } from '../packages/local-filesystem-mcp/src/surface-definition.ts';
import { surfaceDefinition as structuredCommandSurface } from '../packages/structured-command-mcp/src/surface-definition.ts';

type JsonRecord = Record<string, any>;
const workspaceRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const catalogPath = join(workspaceRoot, 'packages', 'mcp-registrar', 'native', 'tool-catalog.json.gz');

function catalogSurface(id: string, definition: JsonRecord, existing: JsonRecord): JsonRecord {
  const projection = definition.descriptor.projections[0];
  const transport = projection.transport;
  const args = transport.args.slice(1);
  const nativeProjection = {
    id: projection.id,
    injection_scope: projection.injection_scope,
    execution: {
      adapter: 'stdio',
      tenancy: 'session_isolated',
      replacement: 'manual',
    },
    restart_owner: 'local_site',
    runtime_requirements: projection.runtime_requirements,
    env_vars: transport.env,
    command: transport.command,
    entrypoint: transport.args[0],
    args,
    ...(projection.default_injection === undefined ? {} : { default_injection: projection.default_injection }),
  };
  return {
    ...existing,
    id,
    package: existing.package,
    entrypoint: transport.args[0],
    args,
    tools: definition.descriptor.tools.map((tool: JsonRecord) => tool.name),
    projections: [nativeProjection],
    injection_scope: projection.injection_scope,
    restart_owner: 'local_site',
    env_vars: transport.env,
    descriptor_source: 'native',
    // Registrar recomputes the raw descriptor digest after native admission
    // repairs; keep this checked-in catalog equal to that runtime authority.
    descriptor_digest: createHash('sha256').update(canonicalJson(definition.descriptor)).digest('hex'),
    tool_contract_digest: definition.tool_contract_digest,
    descriptor: definition.descriptor,
    authority_locus: existing.authority_locus ?? { kind: 'local_site' },
    mutation_locus: existing.mutation_locus ?? { kind: 'local_site' },
    narada_scope: existing.narada_scope ?? {
      injection_scope: projection.injection_scope,
      authority_locus: { kind: 'local_site' },
      mutation_locus: { kind: 'local_site' },
      restart_owner: 'local_site',
      scope_source: 'registrar_surface_catalog',
    },
  };
}

if (!existsSync(catalogPath)) throw new Error(`native_registrar_catalog_missing:${catalogPath}`);
const catalog = JSON.parse(gunzipSync(readFileSync(catalogPath)).toString('utf8')) as JsonRecord;
const items = catalog.read_models?.registrar_surface_list?.items as JsonRecord[] | undefined;
if (!items) throw new Error('native_registrar_catalog_surface_list_missing');
const definitions = new Map([
  ['local-filesystem', filesystemSurface()],
  ['structured-command', structuredCommandSurface()],
]);
for (const [id, definition] of definitions) {
  const index = items.findIndex((item) => item.id === id);
  if (index < 0) throw new Error(`native_registrar_catalog_surface_missing:${id}`);
  items[index] = catalogSurface(id, definition, items[index]);
}
catalog.read_models.registrar_surface_list.count = items.length;
writeFileSync(catalogPath, gzipSync(JSON.stringify(catalog, null, 2) + '\n', { level: 9 }));
process.stdout.write(JSON.stringify({
  schema: 'narada.native_registrar_catalog_generation.v1',
  status: 'generated',
  catalog_path: catalogPath,
  surfaces: Object.fromEntries([...definitions.keys()].map((id) => {
    const item = items.find((candidate) => candidate.id === id)!;
    return [id, { tools: item.tools.length, descriptor_digest: item.descriptor_digest, tool_contract_digest: item.tool_contract_digest }];
  })),
}) + '\n');
