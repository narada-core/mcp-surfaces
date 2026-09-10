#!/usr/bin/env node
import { pathToFileURL } from 'node:url';
import {
  SiteFabricClient,
  isRecord,
  type JsonRecord,
  type SiteFabricToolCallOptions,
} from '@narada-core/mcp-runtime-client';

export type DomainOutboxProfile = 'mailbox' | 'work-lifecycle';

export interface SchedulerDomainFabricCaller {
  call(
    surfaceId: string,
    toolName: string,
    args?: JsonRecord,
    options?: SiteFabricToolCallOptions,
  ): Promise<JsonRecord>;
}

export interface SchedulerDomainOutboxOptions {
  siteRoot: string;
  profile: DomainOutboxProfile;
  consumerId: string;
  scopeId?: string;
  outboxStartAt?: string;
  topics?: string[];
  sourceSurfaceId?: string;
  schedulerSurfaceId?: string;
  workLifecycleSurfaceId?: string;
  maxEvents?: number;
  requestTimeoutMs?: number;
  loaderEntrypoint?: string;
  bindingAdmissionPath?: string;
}

export interface SchedulerDomainOutboxReport extends JsonRecord {
  schema: 'narada.scheduler.domain_outbox_dispatch_pass.v1';
  status: 'completed' | 'completed_with_errors';
  profile: DomainOutboxProfile;
  events_seen: number;
  events_admitted: number;
  events_acknowledged: number;
  events_ticketed: number;
  errors: JsonRecord[];
}

export async function runSchedulerDomainOutboxDispatcher(
  input: SchedulerDomainOutboxOptions,
  providedFabric?: SchedulerDomainFabricCaller,
): Promise<SchedulerDomainOutboxReport> {
  const options = normalizeOptions(input);
  const ownedFabric = providedFabric ? null : await SiteFabricClient.open({
    siteRoot: options.siteRoot,
    loaderEntrypoint: options.loaderEntrypoint,
    bindingAdmissionPath: options.bindingAdmissionPath,
    allowedSurfaceIds: [...new Set([options.schedulerSurfaceId, options.sourceSurfaceId, options.workLifecycleSurfaceId])],
    requestTimeoutMs: options.requestTimeoutMs,
  });
  const fabric = providedFabric ?? ownedFabric!;
  const report: SchedulerDomainOutboxReport = {
    schema: 'narada.scheduler.domain_outbox_dispatch_pass.v1',
    status: 'completed',
    profile: options.profile,
    events_seen: 0,
    events_admitted: 0,
    events_acknowledged: 0,
    events_ticketed: 0,
    errors: [],
  };
  try {
    const runtime = await fabric.call(options.schedulerSurfaceId, 'scheduler_runtime_status', {});
    const implementationId = requiredString(runtime.implementation_id, 'scheduler_implementation_id_missing');
    if (runtime.status !== 'fresh') throw new Error(`scheduler_runtime_not_fresh:${String(runtime.status)}`);

    await registerConsumer(fabric, options);
    let remaining = options.maxEvents;
    while (remaining > 0) {
      const pageLimit = Math.min(5, remaining);
      const page = await listEvents(fabric, options, pageLimit);
      const listed = page.events;
      if (listed.length === 0) break;
      let pageFailed = false;
      for (const raw of listed) {
        report.events_seen += 1;
        remaining -= 1;
        try {
          const event = parseDomainEvent(raw);
          const ticketReceipt = options.profile === 'mailbox' && event.topic === 'mailbox.thread.attention_required'
            ? await admitMailboxAttention(fabric, options, event)
            : null;
          if (ticketReceipt) report.events_ticketed += 1;
          await fabric.call(options.schedulerSurfaceId, 'scheduler_event_admit', {
            event_id: event.event_id,
            topic: event.topic,
            partition_key: event.partition_key,
            aggregate_id: event.aggregate_id,
            aggregate_revision: event.aggregate_revision,
            schema_version: event.schema_version,
            causation_id: event.causation_id,
            idempotency_key: event.idempotency_key,
            payload: ticketReceipt ? { ...event.payload, ticket_admission: ticketReceipt } : event.payload,
            occurred_at: event.occurred_at,
            implementation_id: implementationId,
          });
          report.events_admitted += 1;
          await acknowledgeEvent(fabric, options, event, ticketReceipt);
          report.events_acknowledged += 1;
        } catch (error) {
          pageFailed = true;
          report.errors.push({
            stage: 'domain_outbox_event',
            event_id: optionalString(raw.event_id),
            error: boundedError(error),
          });
        }
      }
      if (pageFailed || !page.hasMore) break;
    }
  } finally {
    if (ownedFabric) {
      try {
        await ownedFabric.close();
      } catch (error) {
        report.errors.push({ stage: 'site_fabric_close', error: boundedError(error) });
      }
    }
  }
  report.status = report.errors.length === 0 ? 'completed' : 'completed_with_errors';
  return report;
}

interface NormalizedOptions {
  siteRoot: string;
  profile: DomainOutboxProfile;
  consumerId: string;
  scopeId: string | null;
  outboxStartAt: string | null;
  topics: string[];
  sourceSurfaceId: string;
  schedulerSurfaceId: string;
  workLifecycleSurfaceId: string;
  maxEvents: number;
  requestTimeoutMs: number;
  loaderEntrypoint?: string;
  bindingAdmissionPath?: string;
}

function normalizeOptions(input: SchedulerDomainOutboxOptions): NormalizedOptions {
  const profile = input.profile;
  if (profile !== 'mailbox' && profile !== 'work-lifecycle') throw new Error(`domain_outbox_profile_invalid:${String(profile)}`);
  const startAt = input.outboxStartAt ? normalizeTimestamp(input.outboxStartAt, 'outboxStartAt') : null;
  const topics = [...new Set((input.topics ?? []).map((topic) => requiredString(topic, 'domain_outbox_topic_invalid')))];
  if (profile === 'mailbox' && startAt === null) throw new Error('mailbox_outbox_start_at_required');
  const scopeId = input.scopeId ? requiredString(input.scopeId, 'scopeId_required') : null;
  if (profile === 'mailbox' && scopeId === null) throw new Error('mailbox_outbox_scope_id_required');
  if (profile === 'mailbox' && topics.length === 0) throw new Error('mailbox_outbox_topics_required');
  if (profile === 'work-lifecycle' && topics.length === 0) throw new Error('work_lifecycle_outbox_topics_required');
  return {
    siteRoot: requiredString(input.siteRoot, 'siteRoot_required'),
    profile,
    consumerId: requiredString(input.consumerId, 'consumerId_required'),
    scopeId,
    outboxStartAt: startAt,
    topics,
    sourceSurfaceId: optionalString(input.sourceSurfaceId) ?? profile,
    schedulerSurfaceId: optionalString(input.schedulerSurfaceId) ?? 'scheduler',
    workLifecycleSurfaceId: optionalString(input.workLifecycleSurfaceId) ?? 'work-lifecycle',
    maxEvents: boundedInteger(input.maxEvents, 100, 1, 100, 'maxEvents'),
    requestTimeoutMs: boundedInteger(input.requestTimeoutMs, 30_000, 1_000, 300_000, 'requestTimeoutMs'),
    ...(input.loaderEntrypoint ? { loaderEntrypoint: input.loaderEntrypoint } : {}),
    ...(input.bindingAdmissionPath ? { bindingAdmissionPath: input.bindingAdmissionPath } : {}),
  };
}

async function registerConsumer(fabric: SchedulerDomainFabricCaller, options: NormalizedOptions): Promise<void> {
  if (options.profile === 'mailbox') {
    await fabric.call(options.sourceSurfaceId, 'mailbox_outbox_consumer_register', {
      consumer_id: options.consumerId,
      scope_id: options.scopeId,
      topics: options.topics,
      start_at: options.outboxStartAt,
    });
    return;
  }
  for (const topic of options.topics) {
    await fabric.call(options.sourceSurfaceId, 'work_outbox_consumer_register', {
      consumer_id: options.consumerId,
      topic,
    });
  }
}

async function listEvents(
  fabric: SchedulerDomainFabricCaller,
  options: NormalizedOptions,
  limit: number,
): Promise<{ events: JsonRecord[]; hasMore: boolean }> {
  if (options.profile === 'mailbox') {
    const result = await fabric.call(options.sourceSurfaceId, 'mailbox_outbox_list', {
      consumer_id: options.consumerId,
      limit,
    });
    if (!Array.isArray(result.items)) {
      throw new Error(`mailbox_outbox_items_invalid:${JSON.stringify(result).slice(0, 2_000)}`);
    }
    return {
      events: recordArray(result.items, 'mailbox_outbox_items_invalid'),
      hasMore: result.has_more === true,
    };
  }
  const result = await fabric.call(options.sourceSurfaceId, 'work_outbox_list', {
    consumer_id: options.consumerId,
    topics: options.topics,
    limit,
  });
  const events = recordArray(result.events, 'work_outbox_events_invalid');
  return { events, hasMore: events.length >= limit };
}

async function acknowledgeEvent(
  fabric: SchedulerDomainFabricCaller,
  options: NormalizedOptions,
  event: DomainEvent,
  ticketReceipt: JsonRecord | null,
): Promise<void> {
  const args = {
    consumer_id: options.consumerId,
    event_id: event.event_id,
    receipt: {
      schema: 'narada.scheduler.domain_outbox_receipt.v2',
      outcome: 'admitted',
      effect_ref: ticketReceipt
        ? requiredString(ticketReceipt.ticket_ref, 'ticket_admission_ticket_ref_missing')
        : `scheduler-event:${event.event_id}`,
    },
  };
  await fabric.call(
    options.sourceSurfaceId,
    options.profile === 'mailbox' ? 'mailbox_outbox_ack' : 'work_outbox_ack',
    args,
  );
}

async function admitMailboxAttention(
  fabric: SchedulerDomainFabricCaller,
  options: NormalizedOptions,
  event: DomainEvent,
): Promise<JsonRecord> {
  const factId = requiredString(event.payload.fact_id, 'mailbox_attention_fact_id_missing');
  const admissionOperation = await fabric.call(options.sourceSurfaceId, 'mailbox_message_admit', {
    idempotency_key: `scheduler-mailbox-admission:${event.event_id}`,
    scope_id: options.scopeId,
    fact_id: factId,
    source_event_id: event.event_id,
  });
  const admission = requireRecord(admissionOperation.result, 'mailbox_attention_admission_result_missing');
  const decision = requiredString(admission.decision, 'mailbox_attention_admission_decision_missing');
  if (decision !== 'admitted') {
    return {
      schema: 'narada.scheduler.mailbox_attention_ticket_admission.v1',
      decision,
      mailbox_admission_ref: requiredString(admissionOperation.operation_ref, 'mailbox_admission_ref_missing'),
      ticket_ref: `mailbox-admission:${requiredString(admission.admission_id, 'mailbox_admission_id_missing')}`,
    };
  }
  const source = requireRecord(admission.source, 'mailbox_attention_source_missing');
  const sourceRef = requireRecord(source.source_ref, 'mailbox_attention_source_ref_missing');
  const correlationKeys = recordArray(source.correlation_keys, 'mailbox_attention_correlation_keys_invalid');
  const ticketOperation = await fabric.call(options.workLifecycleSurfaceId, 'ticket_admit_source', {
    idempotency_key: `scheduler-mailbox-ticket:${event.event_id}`,
    source_kind: requiredString(source.source_kind, 'mailbox_attention_source_kind_missing'),
    source_scope: requiredString(source.source_scope, 'mailbox_attention_source_scope_missing'),
    immutable_source_id: requiredString(source.immutable_source_id, 'mailbox_attention_immutable_source_id_missing'),
    causation_id: event.event_id,
    policy_version: requiredString(admission.policy_version, 'mailbox_attention_policy_version_missing'),
    summary: requiredString(source.summary, 'mailbox_attention_source_summary_missing'),
    source_ref: sourceRef,
    correlation_keys: correlationKeys,
    work_due_policy: 'deferred',
  });
  const ticket = requireRecord(ticketOperation.result ?? ticketOperation.ticket, 'ticket_admission_result_missing');
  const ticketId = requiredString(ticket.ticket_id, 'ticket_admission_ticket_id_missing');
  return {
    schema: 'narada.scheduler.mailbox_attention_ticket_admission.v1',
    decision: 'admitted',
    mailbox_admission_ref: requiredString(admissionOperation.operation_ref, 'mailbox_admission_ref_missing'),
    ticket_ref: `work-ticket:${ticketId}`,
    ticket_id: ticketId,
    ticket_revision: ticket.ticket_revision,
  };
}

interface DomainEvent {
  event_id: string;
  topic: string;
  partition_key: string;
  aggregate_id: string;
  aggregate_revision: number;
  schema_version: number;
  causation_id: string;
  idempotency_key: string;
  payload: JsonRecord;
  occurred_at: string;
}

function parseDomainEvent(value: JsonRecord): DomainEvent {
  return {
    event_id: requiredString(value.event_id, 'domain_event_id_missing'),
    topic: requiredString(value.topic, 'domain_event_topic_missing'),
    partition_key: requiredString(value.partition_key, 'domain_event_partition_key_missing'),
    aggregate_id: requiredString(value.aggregate_id, 'domain_event_aggregate_id_missing'),
    aggregate_revision: requiredInteger(value.aggregate_revision, 'domain_event_aggregate_revision_invalid'),
    schema_version: requiredInteger(value.schema_version, 'domain_event_schema_version_invalid'),
    causation_id: requiredString(value.causation_id, 'domain_event_causation_id_missing'),
    idempotency_key: requiredString(value.idempotency_key, 'domain_event_idempotency_key_missing'),
    payload: requireRecord(value.payload, 'domain_event_payload_invalid'),
    occurred_at: normalizeTimestamp(value.occurred_at ?? value.created_at, 'domain_event_occurred_at'),
  };
}

function recordArray(value: unknown, code: string): JsonRecord[] {
  if (!Array.isArray(value) || value.some((entry) => !isRecord(entry))) throw new Error(code);
  return value as JsonRecord[];
}

function requireRecord(value: unknown, code: string): JsonRecord {
  if (!isRecord(value)) throw new Error(code);
  return value;
}

function requiredString(value: unknown, code: string): string {
  const normalized = typeof value === 'string' ? value.trim() : '';
  if (!normalized) throw new Error(code);
  return normalized;
}

function optionalString(value: unknown): string | null {
  const normalized = typeof value === 'string' ? value.trim() : '';
  return normalized || null;
}

function requiredInteger(value: unknown, code: string): number {
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < 0) throw new Error(code);
  return number;
}

function boundedInteger(value: number | undefined, fallback: number, min: number, max: number, name: string): number {
  const resolved = value ?? fallback;
  if (!Number.isSafeInteger(resolved) || resolved < min || resolved > max) throw new Error(`${name}_invalid`);
  return resolved;
}

function normalizeTimestamp(value: unknown, name: string): string {
  const text = requiredString(value, `${name}_required`);
  const timestamp = new Date(text);
  if (Number.isNaN(timestamp.getTime())) throw new Error(`${name}_invalid`);
  return timestamp.toISOString();
}

function boundedError(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error);
  return text.length <= 2_048 ? text : text.slice(0, 2_048);
}

function parseCliArgs(argv: string[]): SchedulerDomainOutboxOptions {
  const values = parseFlagValues(argv, new Set([
    '--site-root', '--profile', '--consumer-id', '--scope-id', '--outbox-start-at', '--topics',
    '--source-surface-id', '--scheduler-surface-id', '--max-events',
    '--work-lifecycle-surface-id',
    '--request-timeout-ms', '--loader-entrypoint', '--binding-admission-path',
  ]));
  return {
    siteRoot: requiredString(values.get('--site-root'), 'site_root_required'),
    profile: requiredString(values.get('--profile'), 'profile_required') as DomainOutboxProfile,
    consumerId: requiredString(values.get('--consumer-id'), 'consumer_id_required'),
    ...(values.has('--scope-id') ? { scopeId: values.get('--scope-id') } : {}),
    ...(values.has('--outbox-start-at') ? { outboxStartAt: values.get('--outbox-start-at') } : {}),
    ...(values.has('--topics') ? { topics: values.get('--topics')!.split(',').map((topic) => topic.trim()).filter(Boolean) } : {}),
    ...(values.has('--source-surface-id') ? { sourceSurfaceId: values.get('--source-surface-id') } : {}),
    ...(values.has('--scheduler-surface-id') ? { schedulerSurfaceId: values.get('--scheduler-surface-id') } : {}),
    ...(values.has('--work-lifecycle-surface-id') ? { workLifecycleSurfaceId: values.get('--work-lifecycle-surface-id') } : {}),
    ...(values.has('--max-events') ? { maxEvents: Number(values.get('--max-events')) } : {}),
    ...(values.has('--request-timeout-ms') ? { requestTimeoutMs: Number(values.get('--request-timeout-ms')) } : {}),
    ...(values.has('--loader-entrypoint') ? { loaderEntrypoint: values.get('--loader-entrypoint') } : {}),
    ...(values.has('--binding-admission-path') ? { bindingAdmissionPath: values.get('--binding-admission-path') } : {}),
  };
}

function parseFlagValues(argv: string[], known: ReadonlySet<string>): Map<string, string> {
  const values = new Map<string, string>();
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index]!;
    if (!known.has(flag)) throw new Error(`unknown_argument:${flag}`);
    const value = argv[index + 1];
    if (!value || value.startsWith('--')) throw new Error(`missing_argument_value:${flag}`);
    if (values.has(flag)) throw new Error(`duplicate_argument:${flag}`);
    values.set(flag, value);
    index += 1;
  }
  return values;
}

function isEntrypoint(): boolean {
  const invoked = process.argv[1];
  return Boolean(invoked) && import.meta.url === pathToFileURL(invoked).href;
}

if (isEntrypoint()) {
  runSchedulerDomainOutboxDispatcher(parseCliArgs(process.argv.slice(2)))
    .then((report) => {
      process.stdout.write(`${JSON.stringify(report)}\n`);
      if (report.status !== 'completed') process.exitCode = 1;
    })
    .catch((error) => {
      process.stderr.write(`${boundedError(error)}\n`);
      process.exitCode = 1;
    });
}
