import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { isAbsolute, join, resolve } from 'node:path';

type CarrierContract = {
  schema: string;
  sites: Array<{ site_id: string; registry_path: string; surface_ids: string[] }>;
};

const profile = process.env.USERPROFILE?.trim();
if (!profile) throw new Error('site_registry_sync_userprofile_required');
const workspaceRoot = resolve(import.meta.dirname, '..');
const contractPath = resolve(process.env.NARADA_CARRIER_CONTRACT?.trim()
  || join(profile, 'Narada', '.narada', 'capabilities', 'carrier-materialization.json'));
const contract = JSON.parse(readFileSync(contractPath, 'utf8')) as CarrierContract;
if (contract.schema !== 'narada.native_carrier_contract.v2' || !Array.isArray(contract.sites) || contract.sites.length === 0) {
  throw new Error(`site_registry_sync_contract_invalid:${contractPath}`);
}
const seen = new Set<string>();
for (const site of contract.sites) {
  if (!site.site_id || seen.has(site.site_id) || !isAbsolute(site.registry_path)) {
    throw new Error(`site_registry_sync_site_invalid:${site.site_id || 'missing'}`);
  }
  seen.add(site.site_id);
}

const nativeRoot = resolve(workspaceRoot, 'packages', 'mcp-registrar', 'dist', 'native');
const pointerPath = resolve(nativeRoot, 'current.json');
if (!existsSync(pointerPath)) {
  process.stdout.write(`${JSON.stringify({
    schema: 'narada.site_registry_publication.v1',
    status: 'deferred',
    reason: 'native_registrar_not_published',
    contract_path: contractPath,
    site_count: 0,
    sites: [],
  })}\n`);
  process.exit(0);
}
const pointer = JSON.parse(readFileSync(pointerPath, 'utf8')) as { artifacts?: Record<string, string> };
const artifactName = `narada-mcp-registrar${process.platform === 'win32' ? '.exe' : ''}`;
const relativeArtifact = pointer.artifacts?.[artifactName];
if (!relativeArtifact) throw new Error(`site_registry_sync_native_artifact_undeclared:${artifactName}`);
const executable = resolve(nativeRoot, relativeArtifact);
if (!existsSync(executable)) throw new Error(`site_registry_sync_native_artifact_missing:${executable}`);

const results = contract.sites.map((site, index) => {
  const request = JSON.stringify({
    jsonrpc: '2.0',
    id: index + 1,
    method: 'tools/call',
    params: { name: 'registrar_site_surface_registry_sync', arguments: { site_id: site.site_id } },
  });
  const invocation = spawnSync(executable, [], { input: `${request}\n`, encoding: 'utf8', windowsHide: true });
  if (invocation.status !== 0) throw new Error(`site_registry_sync_native_failed:${site.site_id}:${invocation.stderr.slice(-2000)}`);
  const separator = invocation.stdout.indexOf('\r\n\r\n');
  const response = JSON.parse(separator >= 0 ? invocation.stdout.slice(separator + 4) : invocation.stdout);
  if (response.error) throw new Error(`site_registry_sync_native_refused:${site.site_id}:${response.error.message}`);
  const actual = resolve(String(response.result?.structuredContent?.path ?? ''));
  if (actual.toLowerCase() !== resolve(site.registry_path).toLowerCase()) {
    throw new Error(`site_registry_sync_path_mismatch:${site.site_id}:${actual}`);
  }
  return { site_id: site.site_id, registry_path: actual };
});

process.stdout.write(`${JSON.stringify({
  schema: 'narada.site_registry_publication.v1',
  status: 'published',
  contract_path: contractPath,
  site_count: results.length,
  sites: results,
})}\n`);
