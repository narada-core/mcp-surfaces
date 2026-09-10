import assert from 'node:assert/strict';
import test from 'node:test';
import {
  runSchedulerSopDispatcher,
  type SchedulerFabricCaller,
} from '../src/sop-activation-dispatcher.js';
import type { JsonRecord } from '@narada-core/mcp-runtime-client';

type Call = { surface: string; tool: string; args: JsonRecord };

const terminalEvent: JsonRecord = {
  schema: 'narada.sop.outbox_event.v1',
  event_id: 'terminal-event-1',
  topic: 'sop.run.terminal.v1',
  partition_key: 'mailbox-sync',
  run_id: 'predecessor-run',
  sop_id: 'mailbox-sync',
  sop_version: 1,
  occurrence_key: 'mailbox-sync:cycle-1',
  outcome: 'completed',
  payload: {
    schema: 'narada.sop.run_terminal.v2',
    run_id: 'predecessor-run',
    sop_id: 'mailbox-sync',
    run_outcome: 'completed',
    outcome: 'no_change',
    output: { outcome: 'no_change' },
  },
  created_at: '2026-01-01T00:00:00.000Z',
  available_at: '2026-01-01T00:00:00.000Z',
  compacted_at: null,
};

class HappyFabric implements SchedulerFabricCaller {
  calls: Call[] = [];
  claimCount = 0;

  async call(surface: string, tool: string, args: JsonRecord = {}): Promise<JsonRecord> {
    this.calls.push({ surface, tool, args });
    switch (tool) {
      case 'scheduler_runtime_status': return { status: 'fresh', implementation_id: 'implementation-1' };
      case 'sop_outbox_consumer_register': return { registration_replayed: false };
      case 'sop_outbox_list': return { items: [terminalEvent], count: 1 };
      case 'scheduler_event_admit': return { status: 'admitted' };
      case 'scheduler_activation_list': return args.status === 'admitted'
        ? { activations: [] }
        : { activations: [{ activation_id: 'predecessor-activation' }] };
      case 'scheduler_activation_resolve': return { activation: { status: 'terminal' } };
      case 'sop_outbox_ack': return { acknowledgement_replayed: false };
      case 'scheduler_activation_claim':
        this.claimCount += 1;
        return this.claimCount === 1 ? { activation: activation('lease-1') } : { activation: null };
      case 'scheduler_event_show': return { event: sourceEvent };
      case 'sop_run_start': return { run_id: 'target-run', admission: 'created' };
      case 'scheduler_activation_admit_sop': return { activation: { status: 'admitted' } };
      default: throw new Error(`unexpected_tool:${tool}`);
    }
  }
}

const sourceEvent: JsonRecord = {
  event_id: 'terminal-event-1',
  topic: 'sop.run.terminal.v1',
  partition_key: 'mailbox-sync',
  aggregate_id: 'predecessor-run',
  aggregate_revision: 1,
  schema_version: 1,
  causation_id: 'mailbox-sync:cycle-1',
  idempotency_key: 'terminal-event-1',
  payload: terminalEvent.payload,
  occurred_at: '2026-01-01T00:00:00.000Z',
};

function activation(leaseToken: string): JsonRecord {
  return {
    activation_id: 'activation-1',
    binding_id: 'binding-1',
    source_event_id: 'terminal-event-1',
    occurrence_key: 'binding-1:terminal-event-1',
    target_sop_id: 'mailbox-sync',
    target_template_version: 'v2',
    lease_token: leaseToken,
  };
}

const options = {
  siteRoot: 'D:\\site',
  outboxStartAt: '2026-01-01T00:00:00.000Z',
  consumerId: 'dispatcher-1',
  maxEvents: 10,
  maxActivations: 10,
};

test('terminal outbox is acknowledged only after Scheduler admission/resolution and activation admission is lease-token guarded', async () => {
  const fabric = new HappyFabric();
  const report = await runSchedulerSopDispatcher(options, fabric);
  assert.equal(report.status, 'completed');
  assert.equal(report.events_acknowledged, 1);
  assert.equal(report.predecessor_activations_resolved, 1);
  assert.equal(report.admitted_activations_reconciled, 0);
  assert.equal(report.sop_runs_admitted, 1);

  const order = fabric.calls.map((call) => call.tool);
  assert.ok(order.indexOf('scheduler_event_admit') < order.indexOf('scheduler_activation_resolve'));
  assert.ok(order.indexOf('scheduler_activation_resolve') < order.indexOf('sop_outbox_ack'));
  const start = fabric.calls.find((call) => call.tool === 'sop_run_start')!;
  assert.equal(start.args.version, 2);
  assert.equal(start.args.occurrence_key, 'binding-1:terminal-event-1');
  const admitted = fabric.calls.find((call) => call.tool === 'scheduler_activation_admit_sop')!;
  assert.equal(admitted.args.lease_token, 'lease-1');
});

test('failed Scheduler event admission leaves the source event unacknowledged', async () => {
  const fabric = new HappyFabric();
  const original = fabric.call.bind(fabric);
  fabric.call = async (surface, tool, args = {}) => {
    if (tool === 'scheduler_event_admit') {
      fabric.calls.push({ surface, tool, args });
      throw new Error('scheduler_unavailable');
    }
    if (tool === 'scheduler_activation_claim') {
      fabric.calls.push({ surface, tool, args });
      return { activation: null };
    }
    return original(surface, tool, args);
  };
  const report = await runSchedulerSopDispatcher(options, fabric);
  assert.equal(report.status, 'completed_with_errors');
  assert.equal(report.events_acknowledged, 0);
  assert.equal(fabric.calls.some((call) => call.tool === 'sop_outbox_ack'), false);
});

test('crash gap after idempotent SOP admission retries the same occurrence under a new lease', async () => {
  let pass = 0;
  let claimIssued = false;
  const starts: JsonRecord[] = [];
  const fabric: SchedulerFabricCaller = {
    async call(_surface, tool, args = {}) {
      switch (tool) {
        case 'scheduler_runtime_status': return { status: 'fresh', implementation_id: 'implementation-1' };
        case 'sop_outbox_consumer_register': return {};
        case 'sop_outbox_list': return { items: [] };
        case 'scheduler_activation_claim':
          if (claimIssued) return { activation: null };
          claimIssued = true;
          return { activation: activation(`lease-${pass}`) };
        case 'scheduler_event_show': return { event: sourceEvent };
        case 'sop_run_start': starts.push(args); return { run_id: 'same-target-run', admission: pass === 1 ? 'created' : 'replayed' };
        case 'scheduler_activation_admit_sop':
          if (pass === 1) throw new Error('lost_after_sop_commit');
          return { activation: { status: 'admitted' } };
        case 'scheduler_activation_fail': return { activation: { status: 'pending' } };
        default: throw new Error(`unexpected_tool:${tool}`);
      }
    },
  };

  pass = 1;
  claimIssued = false;
  const first = await runSchedulerSopDispatcher(options, fabric);
  assert.equal(first.sop_runs_admitted, 0);
  assert.equal(first.activation_failures_recorded, 1);

  pass = 2;
  claimIssued = false;
  const second = await runSchedulerSopDispatcher(options, fabric);
  assert.equal(second.sop_runs_admitted, 1);
  assert.equal(starts.length, 2);
  assert.equal(starts[0]!.occurrence_key, starts[1]!.occurrence_key);
});

test('reconciles an admitted activation whose SOP already reached a terminal state', async () => {
  const calls: Call[] = [];
  const fabric: SchedulerFabricCaller = {
    async call(surface, tool, args = {}) {
      calls.push({ surface, tool, args });
      switch (tool) {
        case 'scheduler_runtime_status': return { status: 'fresh', implementation_id: 'implementation-1' };
        case 'sop_outbox_consumer_register': return {};
        case 'sop_outbox_list': return { items: [] };
        case 'scheduler_activation_list':
          return args.status === 'admitted'
            ? { activations: [{ activation_id: 'orphaned-activation', sop_run_id: 'completed-run', status: 'admitted' }] }
            : { activations: [] };
        case 'sop_run_status': return { run_id: 'completed-run', status: 'completed', completed_at: '2026-01-01T00:00:02.000Z' };
        case 'scheduler_activation_resolve': return { activation: { status: 'terminal' } };
        case 'scheduler_activation_claim': return { activation: null };
        default: throw new Error(`unexpected_tool:${tool}`);
      }
    },
  };

  const report = await runSchedulerSopDispatcher(options, fabric);
  assert.equal(report.status, 'completed');
  assert.equal(report.admitted_activations_reconciled, 1);
  const resolution = calls.find((call) => call.tool === 'scheduler_activation_resolve')!;
  assert.equal(resolution.args.sop_run_id, 'completed-run');
  assert.equal(resolution.args.outcome, 'completed');
  assert.equal(resolution.args.receipt_id, 'sop-terminal:recovery:completed-run');
});

test('continues admitted-activation reconciliation after one run-status failure', async () => {
  const calls: Call[] = [];
  const fabric: SchedulerFabricCaller = {
    async call(surface, tool, args = {}) {
      calls.push({ surface, tool, args });
      switch (tool) {
        case 'scheduler_runtime_status': return { status: 'fresh', implementation_id: 'implementation-1' };
        case 'sop_outbox_consumer_register': return {};
        case 'sop_outbox_list': return { items: [] };
        case 'scheduler_activation_list':
          return args.status === 'admitted'
            ? { activations: [
              { activation_id: 'broken-activation', sop_run_id: 'broken-run', status: 'admitted' },
              { activation_id: 'healthy-activation', sop_run_id: 'healthy-run', status: 'admitted' },
            ] }
            : { activations: [] };
        case 'sop_run_status':
          if (args.run_id === 'broken-run') throw new Error('sop_status_unavailable');
          return { run_id: 'healthy-run', status: 'completed', completed_at: '2026-01-01T00:00:02.000Z' };
        case 'scheduler_activation_resolve': return { activation: { status: 'terminal' } };
        case 'scheduler_activation_claim': return { activation: null };
        default: throw new Error(`unexpected_tool:${tool}`);
      }
    },
  };

  const report = await runSchedulerSopDispatcher(options, fabric);
  assert.equal(report.status, 'completed_with_errors');
  assert.equal(report.admitted_activations_reconciled, 1);
  assert.equal(report.errors.length, 1);
  assert.equal(calls.filter((call) => call.tool === 'scheduler_activation_resolve').length, 1);
  assert.equal(calls.find((call) => call.tool === 'scheduler_activation_resolve')?.args.sop_run_id, 'healthy-run');
});
