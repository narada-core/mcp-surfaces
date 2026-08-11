import { readFileSync } from 'node:fs';
import { isAbsolute, join, resolve } from 'node:path';
import { syncSiteSurfaceRegistriesById } from '../packages/mcp-registrar/dist/src/main.js';

type CarrierContract = {
  schema: string;
  sites: Array<{ site_id: string; registry_path: string; surface_ids: string[] }>;
};

const profile = process.env.USERPROFILE?.trim();
if (!profile) throw new Error('site_registry_sync_userprofile_required');
const contractPath = resolve(
  process.env.NARADA_CARRIER_CONTRACT?.trim()
    || join(profile, 'Narada', '.narada', 'capabilities', 'carrier-materialization.json'),
);
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
const publication = syncSiteSurfaceRegistriesById(contract.sites.map((site) => site.site_id));
const results = (publication.sites as Array<Record<string, unknown>>).map((result, index) => {
  const site = contract.sites[index];
  const actual = resolve(String(result.path ?? ''));
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
