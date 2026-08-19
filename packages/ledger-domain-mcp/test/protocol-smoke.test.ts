import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runMcpProtocolSmoke, spawnJsonlMcpServer } from '@narada-core/mcp-e2e-harness';
import { requireNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const repoRoot = resolve(packageRoot, '..', '..');
const domainPath = join(repoRoot, 'packages', 'shared', 'ledger-domain-epistemic', 'domain.json');
const executableName = process.platform === 'win32' ? 'narada-ledger-domain.exe' : 'narada-ledger-domain';
const executable = requireNativeArtifact(packageRoot, executableName);
const siteRoot = mkdtempSync(join(tmpdir(), 'ledger-domain-mcp-protocol-'));
const server = spawnJsonlMcpServer(executable, ['--domain', domainPath, '--site-root', siteRoot], { label: 'ledger-domain-mcp protocol smoke' });

try {
  const protocol = await runMcpProtocolSmoke(server.client, { expectedServerName: 'epistemic-graph-mcp' });
  assert.equal(protocol.toolNames.length, 21);
  assert.deepEqual([...protocol.toolNames].sort(), [
    'epistemic_graph_capture_sources',
    'epistemic_graph_export',
    'epistemic_graph_guidance',
    'epistemic_graph_neighborhood',
    'epistemic_graph_proposal_admit',
    'epistemic_graph_proposal_read',
    'epistemic_graph_proposal_reject',
    'epistemic_graph_proposal_resubmit',
    'epistemic_graph_proposal_review',
    'epistemic_graph_proposal_submit',
    'epistemic_graph_query',
    'epistemic_graph_query_batch',
    'epistemic_graph_sequence_claim_next',
    'epistemic_graph_sequence_claims',
    'epistemic_graph_sequence_create',
    'epistemic_graph_sequence_list',
    'epistemic_graph_sequence_status',
    'epistemic_graph_snapshot',
    'epistemic_graph_source_inspect',
    'epistemic_graph_status',
    'epistemic_graph_submit_review_admit',
  ]);

  const statusResponse = await server.client.request(3, 'tools/call', { name: 'epistemic_graph_status', arguments: {} });
  assert.equal(statusResponse.error, undefined, JSON.stringify(statusResponse.error));
  const status = statusResponse.result?.structuredContent as Record<string, unknown> | undefined;
  assert.equal(status?.status, 'ok', JSON.stringify(statusResponse.result));
  assert.equal(status?.schema, 'narada.epistemic.status.v1');

  console.log('ledger-domain-mcp protocol smoke ok');
} finally {
  await server.close();
  rmSync(siteRoot, { recursive: true, force: true });
}
