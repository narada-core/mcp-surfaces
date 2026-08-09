import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import {
  CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA,
  issueCarrierSessionOrientationDeliveryReceipt,
} from '@narada-core/orientation-manifest';
import {
  createTestProcessScope,
  spawnJsonlMcpServer,
} from '@narada-core/mcp-e2e-harness';
import { MCP_RUNTIME_CONTRACT_VERSION } from '@narada-core/mcp-runtime-proxy/materialization-contract';
import { resolveNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';
import { fingerprintWorkspaceArtifactManifest } from '@narada-core/mcp-runtime-proxy/workspace-artifact-manifest';
import {
  materializeAgentSessionStart,
  recordOrientationDeliveryReceipt,
} from '../src/session-start.js';

const require = createRequire(import.meta.url);
const proxyEntrypoint = require.resolve('@narada-core/mcp-runtime-proxy');
const proxyPackageRoot = resolve(dirname(proxyEntrypoint), '..', '..');
const nativeProxyEntrypoint = resolveNativeArtifact(proxyPackageRoot, 'narada-mcp-runtime.exe');
if (!nativeProxyEntrypoint) throw new Error('carrier_orientation_native_proxy_required');
const proxyImplementations = [
  { id: 'typescript', command: process.execPath, prefixArgs: [proxyEntrypoint] },
  { id: 'rust', command: nativeProxyEntrypoint, prefixArgs: ['proxy'] },
] as const;
const agentContextEntrypoint = require.resolve('@narada-core/agent-context-mcp');
const GENERATED_AT = '2026-08-08T12:00:00.000Z';

function writeArtifactManifest(path: string, workspaceRoot: string, entrypoints: string[]) {
  const unsigned: any = {
    schema: 'narada.workspace_artifact_manifest.v1',
    generated_at: GENERATED_AT,
    workspace_root: workspaceRoot,
    packages: [],
    artifacts: [...new Set(entrypoints)].sort().map((entrypoint) => {
      const stat = statSync(entrypoint);
      return {
        path: entrypoint,
        sha256: createHash('sha256').update(readFileSync(entrypoint)).digest('hex'),
        size: stat.size,
        mtime_ms: stat.mtimeMs,
      };
    }),
  };
  writeFileSync(path, JSON.stringify({
    ...unsigned,
    manifest_fingerprint: fingerprintWorkspaceArtifactManifest(unsigned),
  }), 'utf8');
}

function structured(response: any): any {
  assert.equal(response.error, undefined, JSON.stringify(response));
  return response.result?.structuredContent ?? response.result;
}

function fakeWorkSurfaceSource(markerPath: string): string {
  return `
import { writeFileSync } from 'node:fs';
let buffer = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  buffer += chunk;
  const lines = buffer.split(/\\r?\\n/);
  buffer = lines.pop() ?? '';
  for (const line of lines) {
    if (!line.trim()) continue;
    const request = JSON.parse(line);
    if (request.id === undefined) continue;
    let result;
    if (request.method === 'initialize') {
      result = { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'performative-work', version: '1' } };
    } else if (request.method === 'tools/list') {
      result = { tools: [{ name: 'work_perform', description: 'Perform one observable fixture effect.', inputSchema: { type: 'object', properties: {}, additionalProperties: false } }] };
    } else if (request.method === 'tools/call' && request.params?.name === 'work_perform') {
      writeFileSync(${JSON.stringify(markerPath)}, 'performed', 'utf8');
      result = { content: [{ type: 'text', text: 'work_perform: performed' }], structuredContent: { schema: 'fixture.work.v1', status: 'performed' } };
    } else {
      process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, error: { code: -32601, message: 'method_not_found' } }) + '\\n');
      continue;
    }
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\\n');
  }
});
`;
}

const carrierProxyTopologies = (['codex', 'kimi'] as const).flatMap((carrierKind) => (
  proxyImplementations.map((proxyImplementation) => ({ carrierKind, proxyImplementation }))
));

for (const { carrierKind, proxyImplementation } of carrierProxyTopologies) {
  test(`materialized ${carrierKind} Carrier projection refuses ordinary MCP work through the ${proxyImplementation.id} proxy until its one-tool orientation completes`, async () => {
    const siteRoot = mkdtempSync(join(tmpdir(), `orientation-${carrierKind}-${proxyImplementation.id}-e2e-`));
    const processScope = createTestProcessScope({ label: `orientation-${carrierKind}-${proxyImplementation.id}-e2e` });
    const dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite');
    const markerPath = join(siteRoot, `${carrierKind}-ordinary-work.marker`);
    const workEntrypoint = join(siteRoot, 'performative-work.mjs');
    const artifactManifestPath = join(siteRoot, 'workspace-artifact-manifest.json');
    const carrierSessionId = `carrier_${carrierKind}_${proxyImplementation.id}_orientation_e2e`;
    const agentId = `fixture.${carrierKind}`;
    mkdirSync(join(siteRoot, '.ai', 'agents'), { recursive: true });
    writeFileSync(join(siteRoot, 'AGENTS.md'), [
      '# Performative orientation fixture',
      ...Array.from(
        { length: 240 },
        (_, index) => (
          `Rule ${index + 1}: preserve "Carrier-entry" evidence at `
          + `C:\\authority\\${carrierKind}\\rule-${index + 1}; `
          + `boundary marker ✓ α 中 🚀.`
        ),
      ),
      '',
    ].join('\n'), 'utf8');
    writeFileSync(join(siteRoot, '.ai', 'agents', 'roster.json'), JSON.stringify({
      agents: [{ agent_id: agentId, role: 'resident', status: 'active', capabilities: [] }],
    }), 'utf8');
    writeFileSync(workEntrypoint, fakeWorkSurfaceSource(markerPath), 'utf8');

    const admission: any = {
      schema: CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA,
      receipt_id: `receipt:${carrierSessionId}:1`,
      decision: 'admitted',
      state: 'starting',
      coordinate: {
        authority_scope: 'test',
        site_ref: `site:${carrierKind}-fixture`,
        carrier_session_id: carrierSessionId,
        authority_epoch: 1,
      },
      agent_identity: {
        source_authority_ref: `agent-identity:${carrierKind}-fixture`,
        artifact_ref: `agent:${agentId}@1`,
        revision: '1',
        local_agent_id: agentId,
        canonical_agent_id: agentId,
      },
      carrier_kind: carrierKind,
      admission_policy: {
        source_authority_ref: `site-law:${carrierKind}-fixture`,
        artifact_ref: `carrier-policy:${carrierKind}-fixture`,
        revision: '1',
      },
      issued_at: GENERATED_AT,
      valid_until: null,
      authority_readback_ref: `carrier-session-authority:${carrierSessionId}`,
      evidence_refs: [],
      reason_codes: [],
    };
    const exactCheckpoint: any = {
      status: 'ok',
      checkpoint_id: `checkpoint-${carrierKind}-exact`,
      checkpoint_at: '2026-08-08T11:59:00.000Z',
      continuation: {
        objective: 'Preserve exact continuity across Carrier turnover.',
        current_state: `${carrierKind} orientation is awaiting delivery.`,
        next_action: 'Complete the enforced Carrier-entry ceremony.',
        constraints: ['Do not substitute a latest checkpoint.'],
      },
      continuation_blockers: [],
      key_decisions: ['Carrier-entry continuity is historical context, not action authority.'],
    };
    const exactWork: any = {
      status: 'ok',
      task_id: `task-${carrierKind}-42`,
      task_number: 42,
      lifecycle: {
        status: 'in_progress',
        continuation_packet: { next_action: 'Inspect live task authority after orientation.' },
      },
      specification: {
        title: `Prove ${carrierKind} Carrier orientation`,
        goal_markdown: 'Prove ordinary work remains gated until orientation is acknowledged.',
      },
    };

    let agentContext: any = null;
    let workSurface: any = null;
    try {
      const started: any = materializeAgentSessionStart({
        siteRoot,
        siteId: `${carrierKind}-fixture`,
        identity: agentId,
        runtime: carrierKind,
        dbPath,
        carrierSessionId,
        admissionReceipt: admission,
        generatedAt: GENERATED_AT,
        exactCheckpoint,
        exactWork,
      });
      const delivery: any = issueCarrierSessionOrientationDeliveryReceipt({
        admissionReceipt: admission,
        brief: started.orientation_brief,
        deliveredAt: GENERATED_AT,
      });
      recordOrientationDeliveryReceipt({
        siteRoot,
        dbPath,
        admissionReceipt: admission,
        brief: started.orientation_brief,
        deliveryReceipt: delivery,
      });
      const entryRoot = join(siteRoot, '.ai', 'runtime', 'orientation-entry', carrierSessionId);
      const entryFile = join(entryRoot, 'entry.json');
      mkdirSync(entryRoot, { recursive: true });
      writeFileSync(entryFile, JSON.stringify({
        schema: 'narada.carrier_entry.orientation_packet.v1',
        ordinary_work_gate: 'acknowledgement_required',
        acknowledgement_projection: {
          schema: 'narada.carrier_entry.orientation_acknowledgement_projection_ref.v1',
          relative_path: 'acknowledgement.json',
          posture: 'derived_readback_of_canonical_acknowledgement',
        },
        orientation_brief: started.orientation_brief,
        delivery_receipt: delivery,
      }), 'utf8');
      writeArtifactManifest(
        artifactManifestPath,
        siteRoot,
        [agentContextEntrypoint, workEntrypoint],
      );
      const environment: any = {
        ...process.env,
        NARADA_AGENT_ID: agentId,
        NARADA_CARRIER_SESSION_ID: carrierSessionId,
        NARADA_CARRIER_SESSION_ADMISSION_RECEIPT: JSON.stringify(admission),
        NARADA_ORIENTATION_MANIFEST_ID: started.orientation_manifest.manifest_id,
        NARADA_ORIENTATION_BRIEF: JSON.stringify(started.orientation_brief),
        NARADA_ORIENTATION_DELIVERY_RECEIPT: JSON.stringify(delivery),
        NARADA_ORIENTATION_ENTRY_FILE: entryFile,
        NARADA_ORIENTATION_REQUIRED: '1',
        NARADA_AGENT_CONTEXT_DB: dbPath,
        NARADA_SITE_ROOT: siteRoot,
        NARADA_SITE_ID: `${carrierKind}-fixture`,
      };
      const proxyArgs = (surfaceId: string, entrypoint: string, childArgs: string[] = []) => [
        '--artifact-manifest', artifactManifestPath,
        '--runtime-contract-version', String(MCP_RUNTIME_CONTRACT_VERSION),
        '--child-command', process.execPath,
        '--surface-id', surfaceId,
        '--entrypoint', entrypoint,
        '--',
        ...childArgs,
      ];
      agentContext = spawnJsonlMcpServer(proxyImplementation.command, [
        ...proxyImplementation.prefixArgs,
        ...proxyArgs(
        'agent-context',
        agentContextEntrypoint,
        ['--site-root', siteRoot, '--site-id', `${carrierKind}-fixture`, '--tool-projection', 'occupant'],
        ),
      ], {
        cwd: siteRoot,
        env: environment,
        scope: processScope,
        label: `${carrierKind} ${proxyImplementation.id} agent-context proxy`,
        timeoutMs: 20_000,
      });
      workSurface = spawnJsonlMcpServer(proxyImplementation.command, [
        ...proxyImplementation.prefixArgs,
        ...proxyArgs('performative-work', workEntrypoint),
      ], {
        cwd: siteRoot,
        env: environment,
        scope: processScope,
        label: `${carrierKind} ${proxyImplementation.id} ordinary-work proxy`,
        timeoutMs: 20_000,
      });

      structured(await agentContext.client.request(1, 'initialize', { protocolVersion: '2024-11-05' }));
      structured(await workSurface.client.request(2, 'initialize', { protocolVersion: '2024-11-05' }));
      const catalog: any = structured(await agentContext.client.request(3, 'tools/list', {}));
      const catalogNames: string[] = catalog.tools.map((tool: any) => tool.name).sort();
      assert.equal(catalogNames.includes('mcp_runtime_proxy_status'), true);
      assert.deepEqual(catalogNames.filter((name) => name !== 'mcp_runtime_proxy_status'), [
        'agent_orientation_read',
        'mcp_output_show',
      ]);
      const resources: any = structured(await agentContext.client.request(
        32,
        'resources/list',
        {},
      ));
      assert.deepEqual(resources.resources, []);
      const prompts: any = structured(await agentContext.client.request(
        33,
        'prompts/list',
        {},
      ));
      assert.deepEqual(prompts.prompts, []);
      const hiddenAcknowledgement: any = await agentContext.client.request(31, 'tools/call', {
        name: 'agent_orientation_acknowledge',
        arguments: {},
      });
      assert.match(
        hiddenAcknowledgement.error?.message ?? '',
        /^orientation_required:orientation_acknowledgement_required/,
      );
      assert.deepEqual(hiddenAcknowledgement.error?.data?.next_call, {
        surface_id: 'agent-context',
        tool: 'agent_orientation_read',
        arguments: {},
      });

      const refused: any = await workSurface.client.request(4, 'tools/call', {
        name: 'work_perform',
        arguments: {},
      });
      assert.match(refused.error?.message ?? '', /^orientation_required:/);
      assert.deepEqual(refused.error?.data?.next_call, {
        surface_id: 'agent-context',
        tool: 'agent_orientation_read',
        arguments: {},
      });
      assert.equal(existsSync(markerPath), false);

      let nextCall: any = {
        tool: 'agent_orientation_read',
        arguments: {},
      };
      const deliveredMaterials: any = new Map<string, string>();
      let replayedMaterial = false;
      let firstOpaqueContinuation: any = null;
      for (let callIndex = 0; callIndex < 64; callIndex += 1) {
        const requestedCall: any = nextCall;
        const response: any = structured(await agentContext.client.request(
          100 + callIndex,
          'tools/call',
          { name: requestedCall.tool, arguments: requestedCall.arguments },
        ));
        if (callIndex === 0) {
          assert.equal(response.schema, 'narada.agent_context.orientation_entry.v3');
          assert.equal(response.orientation_brief.schema, 'narada.orientation_occupant_brief.v1');
          assert.equal(response.orientation_brief.manifest_readiness, 'ready');
          assert.equal('readiness' in response.orientation_brief, false);
          assert.equal('brief_id' in response.orientation_brief, false);
          assert.equal('brief_digest' in response.orientation_brief, false);
          assert.equal('manifest_ref' in response.orientation_brief, false);
          assert.equal('negative_claims' in response.orientation_brief, false);
          assert.equal(response.orientation_brief.continuity.mode, 'exact');
          assert.equal(response.orientation_brief.continuity.summary.checkpoint_id, `checkpoint-${carrierKind}-exact`);
          assert.equal(response.orientation_brief.work.mode, 'exact');
          assert.equal(response.orientation_brief.work.summary.task_number, 42);
          assert.equal(response.orientation_brief.work.inspection_call.tool, 'task_lifecycle_inspect_range');
          assert.ok(response.manifest_ref.manifest_id);
          assert.deepEqual(Object.keys(response.next_call.arguments), ['continuation']);
          firstOpaqueContinuation = response.next_call.arguments.continuation;
          const recovery: any = structured(await agentContext.client.request(
            90,
            'tools/call',
            { name: 'agent_orientation_read', arguments: { continuation: 'not-a-valid-continuation' } },
          ));
          assert.equal(recovery.schema, 'narada.agent_context.orientation_recovery.v1');
          assert.deepEqual(recovery.next_call, response.next_call);
        }
        if (response.schema === 'narada.agent_context.orientation_material.v1') {
          assert.equal(response.status, 'orientation_required');
          assert.equal(response.ordinary_work_gate, 'acknowledgement_required');
          assert.equal(typeof response.material?.delivery_status, 'string');
          assert.equal('output_ref' in response, false);
          assert.ok(
            Buffer.byteLength(JSON.stringify(response), 'utf8') <= 6_000,
            'orientation material must remain on the inline protocol path',
          );
          const sourceRef: any = String(response.material?.source_ref ?? '');
          deliveredMaterials.set(
            sourceRef,
            (deliveredMaterials.get(sourceRef) ?? '')
              + String(response.material?.content ?? ''),
          );
          if (!replayedMaterial) {
            const replay: any = structured(await agentContext.client.request(
              900 + callIndex,
              'tools/call',
              { name: requestedCall.tool, arguments: requestedCall.arguments },
            ));
            assert.equal(replay.schema, 'narada.agent_context.orientation_material.v1');
            assert.equal(replay.material?.content, response.material?.content);
            assert.deepEqual(replay.next_call, response.next_call);
            replayedMaterial = true;
          }
        }
        if (response.status === 'ready' && response.ordinary_work_gate === 'open') {
          assert.equal(response.schema, 'narada.agent_context.orientation_ready.v1');
          assert.equal(response.orientation.orientation_status, 'acknowledged');
          assert.equal(response.next_call, null);
          assert.equal(response.suggested_next_call.tool, 'task_lifecycle_inspect_range');
          nextCall = null;
          break;
        }
        nextCall = response.next_call;
        assert.ok(nextCall, JSON.stringify(response));
        assert.equal(nextCall.surface_id, 'agent-context');
        assert.equal(nextCall.tool, 'agent_orientation_read');
        assert.deepEqual(Object.keys(nextCall.arguments), ['continuation']);
      }
      assert.equal(nextCall, null, 'orientation ceremony did not acknowledge');
      assert.equal(replayedMaterial, true);
      assert.equal(
        createHash('sha256')
          .update(deliveredMaterials.get('site-file:AGENTS.md') ?? '')
          .digest('hex'),
        createHash('sha256')
          .update(readFileSync(join(siteRoot, 'AGENTS.md'), 'utf8'))
          .digest('hex'),
      );
      const continuitySource: any = [...deliveredMaterials.keys()].find(
        (sourceRef: any) => sourceRef.startsWith('orientation-manifest-entry:'),
      );
      assert.ok(continuitySource, 'exact continuity material was not delivered');
      const continuityMaterial: any = JSON.parse(
        deliveredMaterials.get(continuitySource),
      );
      assert.equal(
        continuityMaterial.schema,
        'narada.agent_context.orientation_continuity_material.v1',
      );
      assert.deepEqual(continuityMaterial.checkpoint, exactCheckpoint);
      assert.equal(continuityMaterial.historical_advisory_only, true);
      assert.equal(existsSync(join(entryRoot, 'acknowledgement.json')), true);

      const recoveredReady: any = structured(await agentContext.client.request(
        450,
        'tools/call',
        { name: 'agent_orientation_read', arguments: { continuation: 'stale-continuation' } },
      ));
      assert.equal(recoveredReady.schema, 'narada.agent_context.orientation_ready.v1');
      assert.equal(recoveredReady.status, 'ready');
      assert.equal(recoveredReady.ordinary_work_gate, 'open');
      const replayedReady: any = structured(await agentContext.client.request(
        451,
        'tools/call',
        { name: 'agent_orientation_read', arguments: { continuation: firstOpaqueContinuation } },
      ));
      assert.equal(replayedReady.schema, 'narada.agent_context.orientation_ready.v1');
      assert.equal(replayedReady.status, 'ready');

      const performed: any = structured(await workSurface.client.request(500, 'tools/call', {
        name: 'work_perform',
        arguments: {},
      }));
      assert.equal(performed.status, 'performed');
      assert.equal(readFileSync(markerPath, 'utf8'), 'performed');
    } finally {
      await Promise.allSettled([
        agentContext?.close?.(),
        workSurface?.close?.(),
      ]);
      await processScope.close();
      rmSync(siteRoot, { recursive: true, force: true });
    }
  });
}
