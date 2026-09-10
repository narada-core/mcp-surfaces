import assert from 'node:assert/strict';
import test from 'node:test';
import type { JsonRecord } from '@narada-core/mcp-runtime-client';
import {
  runTicketDraftDispositionReconciler,
  type TicketDraftDispositionFabricCaller,
} from '../src/ticket-draft-disposition-reconciler.js';

type Call = { surface: string; tool: string; args: JsonRecord };

const receipt: JsonRecord = {
  schema: 'narada.graph_mail.ticket_draft_disposition_receipt.v1',
  observation_id: 'observation-1',
  ticket_id: 'ticket-1',
  draft_id: 'draft-1',
  disposition: 'sent',
  receipt_sha256: 'a'.repeat(64),
};

class FixtureFabric implements TicketDraftDispositionFabricCaller {
  calls: Call[] = [];
  failReconcile = false;

  async call(surface: string, tool: string, args: JsonRecord = {}): Promise<JsonRecord> {
    this.calls.push({ surface, tool, args });
    if (tool === 'graph_mail_ticket_draft_disposition_scan') {
      return { observations_recorded: 1 };
    }
    if (tool === 'graph_mail_ticket_draft_disposition_list') {
      return { items: [receipt], count: 1 };
    }
    if (tool === 'ticket_draft_disposition_reconcile') {
      if (this.failReconcile) throw new Error('work_lifecycle_unavailable');
      return {
        schema: 'narada.domain_operation.v1',
        operation_ref: 'work-lifecycle:observation-1',
        outcome: 'completed',
        result: { ticket_id: 'ticket-1', status: 'resolved', event_id: 'work-event-1' },
      };
    }
    if (tool === 'graph_mail_ticket_draft_disposition_ack') return { status: 'acknowledged' };
    if (tool === 'mailbox_sync_generation') return completedOperation('mailbox-sync:cycle-1');
    if (tool === 'mailbox_thread_attention_rebuild') return completedOperation('mailbox-attention:cycle-1');
    throw new Error(`unexpected_tool:${surface}:${tool}`);
  }
}

const options = {
  siteRoot: 'D:/fixture',
  consumerId: 'work-reconciler',
  scopeId: 'support',
  configPath: 'config/config.json',
  cycleKey: 'cycle-1',
};

test('reconciles, acknowledges, and refreshes mailbox state after a sent draft', async () => {
  const fabric = new FixtureFabric();
  const report = await runTicketDraftDispositionReconciler(options, fabric);
  assert.equal(report.status, 'completed');
  assert.equal(report.dispositions_reconciled, 1);
  assert.equal(report.mailbox_refreshed, true);

  const reconcile = fabric.calls.find((call) => call.tool === 'ticket_draft_disposition_reconcile')!;
  assert.equal(reconcile.args.idempotency_key, 'ticket-draft-disposition:observation-1');
  assert.deepEqual(reconcile.args.evidence, receipt);
  const ack = fabric.calls.find((call) => call.tool === 'graph_mail_ticket_draft_disposition_ack')!;
  assert.equal(ack.args.reconciliation_ref, 'work-lifecycle:observation-1');
  assert.equal(fabric.calls.at(-1)?.tool, 'mailbox_thread_attention_rebuild');
});

test('keeps the disposition unacknowledged and skips mailbox refresh when reconciliation fails', async () => {
  const fabric = new FixtureFabric();
  fabric.failReconcile = true;
  const report = await runTicketDraftDispositionReconciler(options, fabric);
  assert.equal(report.status, 'completed_with_errors');
  assert.equal(report.dispositions_reconciled, 0);
  assert.equal(fabric.calls.some((call) => call.tool === 'graph_mail_ticket_draft_disposition_ack'), false);
  assert.equal(fabric.calls.some((call) => call.tool === 'mailbox_sync_generation'), false);
});

test('performs no mailbox work when there are no dispositions', async () => {
  const fabric = new FixtureFabric();
  fabric.call = async (surface: string, tool: string, args: JsonRecord = {}) => {
    fabric.calls.push({ surface, tool, args });
    if (tool === 'graph_mail_ticket_draft_disposition_scan') return { observations_recorded: 0 };
    if (tool === 'graph_mail_ticket_draft_disposition_list') return { items: [], count: 0 };
    throw new Error(`unexpected_tool:${surface}:${tool}`);
  };
  const report = await runTicketDraftDispositionReconciler(options, fabric);
  assert.equal(report.status, 'completed');
  assert.equal(report.dispositions_seen, 0);
  assert.equal(report.mailbox_refreshed, false);
});

function completedOperation(operationRef: string): JsonRecord {
  return {
    schema: 'narada.domain_operation.v1',
    operation_ref: operationRef,
    outcome: 'completed',
    result: { status: 'completed' },
  };
}
