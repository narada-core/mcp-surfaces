import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import {
  CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA,
  issueCarrierSessionOrientationDeliveryReceipt,
} from '@narada-core/orientation-manifest';
import {
  materializeAgentSessionStart,
  ORIENTATION_REQUIRED_READ_PAGE_BYTES,
  projectOrientationAcknowledgement,
  readOrientationEntryPacket,
  recordOrientationAcknowledgement,
  recordOrientationDeliveryReceipt,
  recordOrientationRequiredRead,
} from '../src/session-start.js';

const GENERATED_AT = '2026-08-08T12:00:00.000Z';

function sha256(value: string): string {
  return createHash('sha256').update(value).digest('hex');
}

test('Agent Context persists exact delivery and opens the gate only after evidenced required reads', {
  timeout: 30_000,
}, () => {
  const siteRoot = mkdtempSync(join(tmpdir(), 'agent-context-orientation-ceremony-'));
  const dbPath = join(siteRoot, '.ai', 'state', 'agent-context.sqlite');
  const agentId = 'fixture.resident';
  const sessionId = 'carrier_fixture_orientation';
  const law = [
    '# Fixture Site',
    '',
    ...Array.from(
      { length: 800 },
      (_, index) => `Rule ${index + 1}: preserve exact authority and evidence boundaries.`,
    ),
    '',
  ].join('\n');
  mkdirSync(join(siteRoot, '.ai', 'agents'), { recursive: true });
  writeFileSync(join(siteRoot, 'AGENTS.md'), law, 'utf8');
  writeFileSync(join(siteRoot, '.ai', 'agents', 'roster.json'), JSON.stringify({
    agents: [{ agent_id: agentId, role: 'resident', capabilities: [] }],
  }), 'utf8');
  const admission: any = {
    schema: CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA,
    receipt_id: `receipt:${sessionId}:1`,
    decision: 'admitted',
    state: 'starting',
    coordinate: {
      authority_scope: 'test',
      site_ref: 'site:fixture',
      carrier_session_id: sessionId,
      authority_epoch: 1,
    },
    agent_identity: {
      source_authority_ref: 'agent-identity:fixture',
      artifact_ref: `agent:${agentId}@1`,
      revision: '1',
      local_agent_id: agentId,
      canonical_agent_id: agentId,
    },
    carrier_kind: 'codex',
    admission_policy: {
      source_authority_ref: 'site-law:fixture',
      artifact_ref: 'carrier-policy:fixture',
      revision: '1',
    },
    issued_at: GENERATED_AT,
    valid_until: null,
    authority_readback_ref: `carrier-session-authority:${sessionId}`,
    evidence_refs: [],
    reason_codes: [],
  };

  try {
    const started: any = materializeAgentSessionStart({
      siteRoot,
      siteId: 'fixture',
      identity: agentId,
      runtime: 'codex',
      dbPath,
      carrierSessionId: sessionId,
      admissionReceipt: admission,
      generatedAt: GENERATED_AT,
    });
    assert.equal(started.status, 'materialized');
    assert.equal(
      started.orientation_brief.inline_bytes
        <= started.orientation_brief.max_inline_bytes,
      true,
    );
    assert.equal(started.orientation_brief.required_reads.length, 1);
    assert.equal(started.orientation_brief.required_reads[0].tool.name, 'agent_orientation_read');
    assert.equal(started.orientation_manifest_ref.manifest_id, started.orientation_manifest.manifest_id);

    const delivery: any = issueCarrierSessionOrientationDeliveryReceipt({
      admissionReceipt: admission,
      brief: started.orientation_brief,
      deliveredAt: GENERATED_AT,
    });
    const deliveryRecord: any = recordOrientationDeliveryReceipt({
      siteRoot,
      dbPath,
      admissionReceipt: admission,
      brief: started.orientation_brief,
      deliveryReceipt: delivery,
    });
    assert.equal(deliveryRecord.status, 'recorded');
    const entryRoot = join(siteRoot, '.ai', 'runtime', 'orientation-entry', sessionId);
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
    }, null, 2), 'utf8');

    const before: any = readOrientationEntryPacket({
      siteRoot,
      dbPath,
      manifestId: started.orientation_manifest.manifest_id,
      admissionReceipt: admission,
      deliveryReceipt: delivery,
    });
    assert.equal(before.status, 'orientation_required');
    assert.equal(before.ordinary_work_gate, 'acknowledgement_required');
    assert.equal(before.schema, 'narada.agent_context.orientation_entry_packet.v2');
    assert.equal('orientation_card' in before, false);
    assert.equal(before.orientation_brief.agent_identity.local_agent_id, agentId);
    assert.equal(before.delivery_receipt_ref, delivery.receipt_id);
    assert.equal(before.acknowledgement_ref, null);
    assert.equal('delivery_receipt' in before, false);
    assert.equal('acknowledgement' in before, false);
    assert.match(
      before.manifest_ref.artifact_ref,
      /^narada-agent-context:\/\/orientation-manifest\//,
    );
    assert.equal(Buffer.byteLength(JSON.stringify(before), 'utf8') < 6_000, true);

    const fileText = readFileSync(join(siteRoot, 'AGENTS.md'), 'utf8');
    assert.equal(sha256(fileText), started.orientation_brief.required_reads[0].source.revision);
    const stepId = started.orientation_brief.required_reads[0].step_id;
    assert.throws(() => recordOrientationRequiredRead({
      siteRoot,
      dbPath,
      admissionReceipt: admission,
      deliveryReceipt: delivery,
      brief: started.orientation_brief,
      stepId,
      byteOffset: 0,
      completedAt: '2026-08-08T12:00:00.500Z',
      resultValidator: () => {
        throw new Error('fixture_orientation_delivery_refused');
      },
    }), /fixture_orientation_delivery_refused/);
    let offset = 0;
    let emittedContent = '';
    let finalPage: any = null;
    let finalPageOffset = 0;
    let testedPrematureAcknowledgement = false;
    for (let pageNumber = 0; pageNumber < 100; pageNumber += 1) {
      const requestedOffset = offset;
      const pageResult: any = recordOrientationRequiredRead({
        siteRoot,
        dbPath,
        admissionReceipt: admission,
        deliveryReceipt: delivery,
        brief: started.orientation_brief,
        stepId,
        byteOffset: offset,
        completedAt: `2026-08-08T12:00:${String(pageNumber + 1).padStart(2, '0')}.000Z`,
      });
      assert.equal(pageResult.status, pageResult.page.eof ? 'completed' : 'page_emitted');
      assert.equal(pageResult.page.byte_offset, requestedOffset);
      assert.equal(pageResult.page.returned_bytes <= ORIENTATION_REQUIRED_READ_PAGE_BYTES, true);
      assert.equal(Buffer.byteLength(JSON.stringify(pageResult), 'utf8') < 6_000, true);
      if (!pageResult.page.eof) assert.equal(pageResult.content.endsWith('\n'), true);
      emittedContent += pageResult.content;
      finalPage = pageResult;
      if (pageResult.page.eof) {
        finalPageOffset = requestedOffset;
        assert.match(
          pageResult.completion_ref,
          /^agent-context:orientation_required_read_completions:/,
        );
        assert.equal(pageResult.required_read_progress.completed, 1);
        assert.equal(pageResult.next_call.tool, 'agent_orientation_acknowledge');
        break;
      }
      assert.equal(pageResult.completion_ref, null);
      assert.equal(pageResult.required_read_progress.completed, 0);
      offset = pageResult.next_call.arguments.offset;
      if (!testedPrematureAcknowledgement) {
        assert.throws(() => recordOrientationAcknowledgement({
          siteRoot,
          dbPath,
          admissionReceipt: admission,
          deliveryReceipt: delivery,
          brief: started.orientation_brief,
          acknowledgedAt: '2026-08-08T12:00:58.000Z',
        }), /agent_context_orientation_required_reads_incomplete/);
        testedPrematureAcknowledgement = true;
      }
    }
    assert.ok(finalPage?.page.eof, 'expected a bounded final page');
    assert.equal(testedPrematureAcknowledgement, true);
    assert.equal(emittedContent, fileText);
    const replayedFinalPage: any = recordOrientationRequiredRead({
      siteRoot,
      dbPath,
      admissionReceipt: admission,
      deliveryReceipt: delivery,
      brief: started.orientation_brief,
      stepId,
      byteOffset: finalPageOffset,
      completedAt: '2026-08-08T12:00:59.000Z',
    });
    assert.equal(replayedFinalPage.status, 'already_completed');
    assert.equal(replayedFinalPage.content, finalPage.content);
    assert.equal(replayedFinalPage.page.eof, true);
    assert.equal(replayedFinalPage.required_read_progress.pending, 0);
    assert.equal(replayedFinalPage.next_call.tool, 'agent_orientation_acknowledge');
    const acknowledgementRecord: any = recordOrientationAcknowledgement({
      siteRoot,
      dbPath,
      admissionReceipt: admission,
      deliveryReceipt: delivery,
      brief: started.orientation_brief,
      acknowledgedAt: '2026-08-08T12:01:00.000Z',
    });
    assert.equal(acknowledgementRecord.status, 'acknowledged');
    assert.equal(acknowledgementRecord.ordinary_work_gate, 'open');
    const projected: any = projectOrientationAcknowledgement({
      siteRoot,
      entryFile,
      acknowledgement: acknowledgementRecord.acknowledgement,
    });
    assert.equal(projected.status, 'projected');
    const acknowledgementProjection: any = JSON.parse(readFileSync(
      join(entryRoot, 'acknowledgement.json'),
      'utf8',
    ));
    assert.equal(acknowledgementProjection.status, 'open');
    assert.equal(
      acknowledgementProjection.delivery_receipt_ref,
      delivery.receipt_id,
    );
    assert.equal(
      acknowledgementProjection.projection_posture,
      'derived_readback_not_independent_authority',
    );

    const after: any = readOrientationEntryPacket({
      siteRoot,
      dbPath,
      manifestId: started.orientation_manifest.manifest_id,
      admissionReceipt: admission,
      deliveryReceipt: delivery,
    });
    assert.equal(after.status, 'acknowledged');
    assert.equal(after.ordinary_work_gate, 'open');
    assert.match(
      after.acknowledgement_ref,
      /^agent-context:orientation_acknowledgements:/,
    );
    assert.equal(after.next_call, null);
  } finally {
    rmSync(siteRoot, { recursive: true, force: true });
  }
});
