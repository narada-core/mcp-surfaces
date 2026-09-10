import assert from 'node:assert/strict';
import test from 'node:test';
import {
  runSchedulerDomainOutboxDispatcher,
  type SchedulerDomainFabricCaller,
} from '../src/domain-outbox-dispatcher.js';
import type { JsonRecord } from '@narada-core/mcp-runtime-client';

const mailboxEvent: JsonRecord = {
  schema: 'narada.mailbox.outbox_event.v1',
  event_id: 'mailbox-event-1',
  topic: 'mailbox.message.first_observed',
  partition_key: 'observation-1',
  aggregate_id: 'observation-1',
  aggregate_revision: 1,
  schema_version: 1,
  causation_id: 'generation-1',
  idempotency_key: 'mailbox-event-1',
  occurred_at: '2026-07-31T12:00:00.000Z',
  payload: { mailbox_id: 'support', message_id: 'message-1', fact_id: 'fact-1' },
};

test('domain outbox replay admits one scheduler event across a lost source acknowledgement', async () => {
  const fabric = new FixtureFabric(mailboxEvent);
  fabric.failNextMailboxAck = true;

  const first = await runSchedulerDomainOutboxDispatcher({
    siteRoot: 'D:/fixture',
    profile: 'mailbox',
    consumerId: 'scheduler-mailbox',
    scopeId: 'support',
    topics: ['mailbox.message.first_observed'],
    outboxStartAt: '2026-07-31T00:00:00.000Z',
  }, fabric);

  assert.equal(first.status, 'completed_with_errors');
  assert.equal(first.events_admitted, 1);
  assert.equal(first.events_acknowledged, 0);
  assert.equal(fabric.schedulerEvents.size, 1);

  const recovered = await runSchedulerDomainOutboxDispatcher({
    siteRoot: 'D:/fixture',
    profile: 'mailbox',
    consumerId: 'scheduler-mailbox',
    scopeId: 'support',
    topics: ['mailbox.message.first_observed'],
    outboxStartAt: '2026-07-31T00:00:00.000Z',
  }, fabric);

  assert.equal(recovered.status, 'completed');
  assert.equal(recovered.events_admitted, 1);
  assert.equal(recovered.events_acknowledged, 1);
  assert.equal(fabric.schedulerEvents.size, 1);
  assert.equal(fabric.mailboxAcknowledged, true);
});

test('work-lifecycle profile registers every topic and normalizes created_at', async () => {
  const event: JsonRecord = {
    event_id: 'work-event-1',
    topic: 'work.ticket-work-due.v1',
    partition_key: 'ticket-1',
    aggregate_id: 'ticket-1',
    aggregate_revision: 2,
    schema_version: 1,
    causation_id: 'source-1',
    idempotency_key: 'work-event-1',
    created_at: '2026-07-31T13:00:00.000Z',
    payload: { ticket_id: 'ticket-1' },
  };
  const fabric = new FixtureFabric(null, event);

  const report = await runSchedulerDomainOutboxDispatcher({
    siteRoot: 'D:/fixture',
    profile: 'work-lifecycle',
    consumerId: 'scheduler-work',
    topics: ['work.ticket-work-due.v1', 'work.task-terminal.v1'],
  }, fabric);

  assert.equal(report.status, 'completed');
  assert.deepEqual([...fabric.workTopics].sort(), ['work.task-terminal.v1', 'work.ticket-work-due.v1']);
  assert.equal(fabric.schedulerEvents.get('work-event-1')?.occurred_at, '2026-07-31T13:00:00.000Z');
  assert.equal(fabric.workAcknowledged, true);
});

test('mailbox outbox drains in bounded pages without requiring oversized MCP output', async () => {
  const events = Array.from({ length: 12 }, (_, index) => ({
    ...mailboxEvent,
    event_id: `mailbox-event-${index + 1}`,
    partition_key: `observation-${index + 1}`,
    aggregate_id: `observation-${index + 1}`,
    idempotency_key: `mailbox-event-${index + 1}`,
    payload: { mailbox_id: 'support', message_id: `message-${index + 1}`, fact_id: `fact-${index + 1}` },
  }));
  const fabric = new FixtureFabric(events);
  const report = await runSchedulerDomainOutboxDispatcher({
    siteRoot: 'D:/fixture',
    profile: 'mailbox',
    consumerId: 'scheduler-mailbox-paged',
    scopeId: 'support',
    topics: ['mailbox.message.first_observed'],
    outboxStartAt: '2026-07-31T00:00:00.000Z',
    maxEvents: 12,
  }, fabric);

  assert.equal(report.status, 'completed');
  assert.equal(report.events_acknowledged, 12);
  assert.deepEqual(fabric.mailboxListLimits, [5, 5, 2]);
  assert.equal(fabric.schedulerEvents.size, 12);
});

test('client-last attention is not acknowledged until mailbox admission and durable ticket admission succeed', async () => {
  const attentionEvent: JsonRecord = {
    ...mailboxEvent,
    event_id: 'attention-1',
    topic: 'mailbox.thread.attention_required',
    partition_key: 'thread-1',
    aggregate_id: 'thread-1',
    payload: {
      scope_id: 'support',
      message_id: 'message-1',
      fact_id: 'fact-1',
      attention_state: 'required',
    },
  };
  const fabric = new FixtureFabric(attentionEvent);
  fabric.failNextTicketAdmission = true;
  const options = {
    siteRoot: 'D:/fixture',
    profile: 'mailbox' as const,
    consumerId: 'scheduler-mailbox-attention',
    scopeId: 'support',
    topics: ['mailbox.thread.attention_required'],
    outboxStartAt: '2026-07-31T00:00:00.000Z',
  };

  const failed = await runSchedulerDomainOutboxDispatcher(options, fabric);
  assert.equal(failed.status, 'completed_with_errors');
  assert.equal(failed.events_ticketed, 0);
  assert.equal(failed.events_acknowledged, 0);

  const recovered = await runSchedulerDomainOutboxDispatcher(options, fabric);
  assert.equal(recovered.status, 'completed');
  assert.equal(recovered.events_ticketed, 1);
  assert.equal(recovered.events_acknowledged, 1);
  assert.equal(fabric.ticketAdmissions.size, 1);
  assert.equal(fabric.mailboxAcknowledged, true);
});

class FixtureFabric implements SchedulerDomainFabricCaller {
  readonly schedulerEvents = new Map<string, JsonRecord>();
  readonly workTopics = new Set<string>();
  readonly mailboxAcknowledgedIds = new Set<string>();
  readonly mailboxListLimits: number[] = [];
  workAcknowledged = false;
  failNextMailboxAck = false;
  failNextTicketAdmission = false;
  readonly ticketAdmissions = new Map<string, JsonRecord>();

  constructor(
    mailboxEvent: JsonRecord | JsonRecord[] | null,
    private readonly workEvent: JsonRecord | null = null,
  ) {
    this.mailboxEvents = mailboxEvent === null
      ? []
      : Array.isArray(mailboxEvent) ? mailboxEvent : [mailboxEvent];
  }

  private readonly mailboxEvents: JsonRecord[];

  get mailboxAcknowledged(): boolean {
    return this.mailboxEvents.length > 0
      && this.mailboxEvents.every((event) => this.mailboxAcknowledgedIds.has(String(event.event_id)));
  }

  async call(surfaceId: string, toolName: string, args: JsonRecord = {}): Promise<JsonRecord> {
    if (surfaceId === 'scheduler' && toolName === 'scheduler_runtime_status') {
      return { status: 'fresh', implementation_id: 'scheduler-runtime-fixture' };
    }
    if (surfaceId === 'scheduler' && toolName === 'scheduler_event_admit') {
      const eventId = String(args.event_id);
      const existing = this.schedulerEvents.get(eventId);
      if (existing && JSON.stringify(existing) !== JSON.stringify(args)) {
        throw new Error(`scheduler_event_idempotency_conflict:${eventId}`);
      }
      this.schedulerEvents.set(eventId, args);
      return { admission: existing ? 'existing' : 'created' };
    }
    if (surfaceId === 'mailbox' && toolName === 'mailbox_outbox_consumer_register') {
      return { status: 'registered' };
    }
    if (surfaceId === 'mailbox' && toolName === 'mailbox_outbox_list') {
      const limit = Number(args.limit);
      this.mailboxListLimits.push(limit);
      return {
        items: this.mailboxEvents
          .filter((event) => !this.mailboxAcknowledgedIds.has(String(event.event_id)))
          .slice(0, limit),
        has_more: this.mailboxEvents
          .filter((event) => !this.mailboxAcknowledgedIds.has(String(event.event_id))).length > limit,
      };
    }
    if (surfaceId === 'mailbox' && toolName === 'mailbox_outbox_ack') {
      if (this.failNextMailboxAck) {
        this.failNextMailboxAck = false;
        throw new Error('fixture_response_lost_before_mailbox_ack');
      }
      this.mailboxAcknowledgedIds.add(String(args.event_id));
      return { status: 'acknowledged' };
    }
    if (surfaceId === 'mailbox' && toolName === 'mailbox_message_admit') {
      return {
        operation_ref: `mailbox-admission:${String(args.fact_id)}`,
        result: {
          admission_id: `admission-${String(args.fact_id)}`,
          decision: 'admitted',
          policy_version: 'fixture-policy-v1',
          source: {
            source_kind: 'mailbox_message',
            source_scope: 'support',
            immutable_source_id: 'message-1',
            summary: 'Mailbox message: fixture',
            source_ref: { fact_id: args.fact_id },
            correlation_keys: [],
          },
        },
      };
    }
    if (surfaceId === 'work-lifecycle' && toolName === 'ticket_admit_source') {
      if (this.failNextTicketAdmission) {
        this.failNextTicketAdmission = false;
        throw new Error('fixture_ticket_admission_failed');
      }
      const key = String(args.idempotency_key);
      assert.equal(args.source_kind, 'mailbox_message');
      assert.equal(args.source_scope, 'support');
      assert.equal(args.immutable_source_id, 'message-1');
      assert.equal(args.policy_version, 'fixture-policy-v1');
      assert.equal(args.summary, 'Mailbox message: fixture');
      assert.deepEqual(args.source_ref, { fact_id: 'fact-1' });
      assert.deepEqual(args.correlation_keys, []);
      assert.equal(args.work_due_policy, 'deferred');
      assert.equal('source' in args, false);
      const ticket = this.ticketAdmissions.get(key) ?? { ticket_id: 'ticket-1', ticket_revision: 1 };
      this.ticketAdmissions.set(key, ticket);
      return { operation_ref: 'work-ticket:ticket-1', result: ticket };
    }
    if (surfaceId === 'work-lifecycle' && toolName === 'work_outbox_consumer_register') {
      this.workTopics.add(String(args.topic));
      return { status: 'registered' };
    }
    if (surfaceId === 'work-lifecycle' && toolName === 'work_outbox_list') {
      return { events: this.workEvent && !this.workAcknowledged ? [this.workEvent] : [] };
    }
    if (surfaceId === 'work-lifecycle' && toolName === 'work_outbox_ack') {
      this.workAcknowledged = true;
      return { status: 'acknowledged' };
    }
    throw new Error(`unexpected_call:${surfaceId}:${toolName}`);
  }
}
