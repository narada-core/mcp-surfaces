#!/usr/bin/env node
import { pathToFileURL } from 'node:url';
import {
  SiteFabricClient,
  isRecord,
  type JsonRecord,
  type SiteFabricToolCallOptions,
} from '@narada-core/mcp-runtime-client';

const SOP_TERMINAL_TOPIC = 'sop.run.terminal.v1';

export interface SchedulerFabricCaller {
  call(
    surfaceId: string,
    toolName: string,
    args?: JsonRecord,
    options?: SiteFabricToolCallOptions,
  ): Promise<JsonRecord>;
}

async function reconcileAdmittedActivations(
  fabric: SchedulerFabricCaller,
  options: NormalizedOptions,
  implementationId: string,
): Promise<{ reconciled: number; errors: JsonRecord[] }> {
  const listed = await fabric.call(options.schedulerSurfaceId, 'scheduler_activation_list', {
    status: 'admitted',
    limit: options.maxActivations,
  });
  const activations = recordArray(listed.activations, 'scheduler_admitted_activation_list_invalid');
  let reconciled = 0;
  const errors: JsonRecord[] = [];
  for (const activation of activations) {
    const runId = optionalString(activation.sop_run_id);
    const itemId = optionalString(activation.activation_id) ?? runId ?? undefined;
    if (!runId) {
      errors.push(errorRecord('scheduler_activation_reconcile', new Error('scheduler_admitted_activation_run_id_missing'), itemId));
      continue;
    }
    try {
      const runResult = await fabric.call(options.sopSurfaceId, 'sop_run_status', { run_id: runId });
      const run = requireRecord(runResult, 'scheduler_run_status_invalid');
      const status = requiredString(run.status, 'scheduler_run_status_missing');
      if (status !== 'completed' && status !== 'failed' && status !== 'cancelled') continue;
      await fabric.call(options.schedulerSurfaceId, 'scheduler_activation_resolve', {
        sop_run_id: runId,
        outcome: status,
        receipt_id: `sop-terminal:recovery:${runId}`,
        receipt: {
          schema: 'narada.scheduler.sop_terminal_recovery_receipt.v1',
          run_id: runId,
          outcome: status,
          completed_at: run.completed_at ?? null,
          recovery: 'admitted_activation_terminal_run',
        },
        implementation_id: implementationId,
      });
      reconciled += 1;
    } catch (error) {
      errors.push(errorRecord('scheduler_activation_reconcile', error, itemId));
    }
  }
  return { reconciled, errors };
}

export interface SchedulerSopDispatcherOptions {
  siteRoot: string;
  outboxStartAt: string;
  consumerId?: string;
  schedulerSurfaceId?: string;
  sopSurfaceId?: string;
  maxEvents?: number;
  maxActivations?: number;
  leaseMs?: number;
  requestTimeoutMs?: number;
  loaderEntrypoint?: string;
  bindingAdmissionPath?: string;
}

export interface SchedulerSopDispatcherReport extends JsonRecord {
  schema: 'narada.scheduler.sop_dispatch_pass.v1';
  status: 'completed' | 'completed_with_errors';
  events_seen: number;
  events_acknowledged: number;
  predecessor_activations_resolved: number;
  admitted_activations_reconciled: number;
  activations_claimed: number;
  sop_runs_admitted: number;
  activation_failures_recorded: number;
  errors: JsonRecord[];
}

export async function runSchedulerSopDispatcher(
  input: SchedulerSopDispatcherOptions,
  providedFabric?: SchedulerFabricCaller,
): Promise<SchedulerSopDispatcherReport> {
  const options = normalizeOptions(input);
  const ownedFabric = providedFabric ? null : await SiteFabricClient.open({
    siteRoot: options.siteRoot,
    loaderEntrypoint: options.loaderEntrypoint,
    bindingAdmissionPath: options.bindingAdmissionPath,
    allowedSurfaceIds: [options.schedulerSurfaceId, options.sopSurfaceId],
    requestTimeoutMs: options.requestTimeoutMs,
  });
  const fabric = providedFabric ?? ownedFabric!;
  const report: SchedulerSopDispatcherReport = {
    schema: 'narada.scheduler.sop_dispatch_pass.v1',
    status: 'completed',
    events_seen: 0,
    events_acknowledged: 0,
    predecessor_activations_resolved: 0,
    admitted_activations_reconciled: 0,
    activations_claimed: 0,
    sop_runs_admitted: 0,
    activation_failures_recorded: 0,
    errors: [],
  };

  try {
    const runtime = await fabric.call(options.schedulerSurfaceId, 'scheduler_runtime_status', {});
    const implementationId = requiredString(runtime.implementation_id, 'scheduler_implementation_id_missing');
    if (runtime.status !== 'fresh') throw new Error(`scheduler_runtime_not_fresh:${String(runtime.status)}`);

    await fabric.call(options.sopSurfaceId, 'sop_outbox_consumer_register', {
      topic: SOP_TERMINAL_TOPIC,
      consumer_id: options.consumerId,
      start_at: options.outboxStartAt,
    });
    const outbox = await fabric.call(options.sopSurfaceId, 'sop_outbox_list', {
      topic: SOP_TERMINAL_TOPIC,
      consumer_id: options.consumerId,
      limit: options.maxEvents,
    });
    for (const raw of recordArray(outbox.items, 'sop_outbox_items_invalid')) {
      report.events_seen += 1;
      try {
        const event = parseSopTerminalEvent(raw);
        await fabric.call(options.schedulerSurfaceId, 'scheduler_event_admit', {
          event_id: event.event_id,
          topic: event.topic,
          partition_key: event.partition_key,
          aggregate_id: event.run_id,
          aggregate_revision: 1,
          schema_version: 1,
          causation_id: event.occurrence_key,
          idempotency_key: event.event_id,
          payload: event.payload,
          occurred_at: event.created_at,
          implementation_id: implementationId,
        });

        const linked = await fabric.call(options.schedulerSurfaceId, 'scheduler_activation_list', {
          sop_run_id: event.run_id,
          limit: 2,
        });
        const activations = recordArray(linked.activations, 'scheduler_activation_list_invalid');
        if (activations.length > 1) throw new Error(`scheduler_sop_run_link_not_unique:${event.run_id}`);
        if (activations.length === 1) {
          const linkedActivation = activations[0]!;
          if (linkedActivation.status === 'terminal') {
            const terminalOutcome = optionalString(linkedActivation.terminal_outcome);
            const payloadOutcome = optionalString(event.payload.outcome);
            if (terminalOutcome && terminalOutcome !== event.outcome && terminalOutcome !== payloadOutcome) {
              throw new Error(`scheduler_terminal_outcome_conflict:${event.run_id}`);
            }
          } else {
            await fabric.call(options.schedulerSurfaceId, 'scheduler_activation_resolve', {
              sop_run_id: event.run_id,
              outcome: event.outcome,
              receipt_id: `sop-terminal:${event.event_id}`,
              receipt: {
                schema: 'narada.scheduler.sop_terminal_receipt.v1',
                event_id: event.event_id,
                run_id: event.run_id,
                outcome: event.outcome,
              },
              implementation_id: implementationId,
            });
          }
          report.predecessor_activations_resolved += 1;
        }

        await fabric.call(options.sopSurfaceId, 'sop_outbox_ack', {
          event_id: event.event_id,
          consumer_id: options.consumerId,
          receipt: {
            schema: 'narada.scheduler.sop_outbox_receipt.v1',
            scheduler_event_id: event.event_id,
            predecessor_activation_resolved: activations.length === 1,
          },
        });
        report.events_acknowledged += 1;
      } catch (error) {
        report.errors.push(errorRecord('sop_terminal_event', error, raw.event_id));
      }
    }

    try {
      const reconciliation = await reconcileAdmittedActivations(
        fabric,
        options,
        implementationId,
      );
      report.admitted_activations_reconciled += reconciliation.reconciled;
      report.errors.push(...reconciliation.errors);
    } catch (error) {
      report.errors.push(errorRecord('scheduler_activation_reconcile_list', error));
    }

    for (let index = 0; index < options.maxActivations; index += 1) {
      const claimResult = await fabric.call(options.schedulerSurfaceId, 'scheduler_activation_claim', {
        consumer_id: options.consumerId,
        lease_ms: options.leaseMs,
        implementation_id: implementationId,
      });
      if (claimResult.activation === null || claimResult.activation === undefined) break;
      const activation = requireRecord(claimResult.activation, 'scheduler_activation_claim_invalid');
      report.activations_claimed += 1;
      const activationId = requiredString(activation.activation_id, 'scheduler_activation_id_missing');
      const leaseToken = requiredString(activation.lease_token, 'scheduler_activation_lease_token_missing');
      try {
        const eventResult = await fabric.call(options.schedulerSurfaceId, 'scheduler_event_show', {
          event_id: requiredString(activation.source_event_id, 'scheduler_source_event_id_missing'),
        });
        const sourceEvent = requireRecord(eventResult.event, 'scheduler_source_event_missing');
        const sopVersion = parseSopVersion(activation.target_template_version);
        const run = await fabric.call(options.sopSurfaceId, 'sop_run_start', {
          sop_id: requiredString(activation.target_sop_id, 'scheduler_target_sop_id_missing'),
          version: sopVersion,
          occurrence_key: requiredString(activation.occurrence_key, 'scheduler_occurrence_key_missing'),
          input: {
            schema: 'narada.scheduler.sop_activation_input.v1',
            activation_id: activationId,
            binding_id: requiredString(activation.binding_id, 'scheduler_binding_id_missing'),
            source_event: sourceEvent,
          },
          trigger_source_kind: 'scheduler_activation',
          trigger_source_ref: activationId,
          triggered_by: options.consumerId,
        });
        const runId = requiredString(run.run_id, 'sop_run_id_missing');
        await fabric.call(options.schedulerSurfaceId, 'scheduler_activation_admit_sop', {
          activation_id: activationId,
          consumer_id: options.consumerId,
          lease_token: leaseToken,
          sop_run_id: runId,
          receipt_id: `sop-admission:${activationId}`,
          receipt: {
            schema: 'narada.scheduler.sop_admission_receipt.v1',
            activation_id: activationId,
            run_id: runId,
            admission: run.admission ?? null,
          },
          implementation_id: implementationId,
        });
        report.sop_runs_admitted += 1;
      } catch (error) {
        try {
          await fabric.call(options.schedulerSurfaceId, 'scheduler_activation_fail', {
            activation_id: activationId,
            consumer_id: options.consumerId,
            lease_token: leaseToken,
            retryable: true,
            error: boundedError(error),
            implementation_id: implementationId,
          });
          report.activation_failures_recorded += 1;
        } catch (failureError) {
          report.errors.push(errorRecord('scheduler_activation_failure_record', failureError, activationId));
        }
        report.errors.push(errorRecord('scheduler_activation_dispatch', error, activationId));
      }
    }
  } finally {
    if (ownedFabric) {
      try {
        await ownedFabric.close();
      } catch (error) {
        report.errors.push(errorRecord('site_fabric_close', error));
      }
    }
  }

  report.status = report.errors.length === 0 ? 'completed' : 'completed_with_errors';
  return report;
}

interface NormalizedOptions {
  siteRoot: string;
  outboxStartAt: string;
  consumerId: string;
  schedulerSurfaceId: string;
  sopSurfaceId: string;
  maxEvents: number;
  maxActivations: number;
  leaseMs: number;
  requestTimeoutMs: number;
  loaderEntrypoint?: string;
  bindingAdmissionPath?: string;
}

function normalizeOptions(input: SchedulerSopDispatcherOptions): NormalizedOptions {
  const outboxStartAt = normalizeTimestamp(input.outboxStartAt, 'outboxStartAt');
  return {
    siteRoot: requiredString(input.siteRoot, 'siteRoot'),
    outboxStartAt,
    consumerId: optionalString(input.consumerId) ?? 'scheduler-sop-dispatcher-v1',
    schedulerSurfaceId: optionalString(input.schedulerSurfaceId) ?? 'scheduler',
    sopSurfaceId: optionalString(input.sopSurfaceId) ?? 'sop',
    maxEvents: boundedInteger(input.maxEvents, 100, 1, 500, 'maxEvents'),
    maxActivations: boundedInteger(input.maxActivations, 100, 1, 500, 'maxActivations'),
    leaseMs: boundedInteger(input.leaseMs, 60_000, 1_000, 300_000, 'leaseMs'),
    requestTimeoutMs: boundedInteger(input.requestTimeoutMs, 30_000, 1_000, 300_000, 'requestTimeoutMs'),
    ...(input.loaderEntrypoint ? { loaderEntrypoint: input.loaderEntrypoint } : {}),
    ...(input.bindingAdmissionPath ? { bindingAdmissionPath: input.bindingAdmissionPath } : {}),
  };
}

function parseSopTerminalEvent(value: JsonRecord): {
  event_id: string;
  topic: string;
  partition_key: string;
  run_id: string;
  occurrence_key: string;
  outcome: 'completed' | 'failed' | 'cancelled';
  payload: JsonRecord;
  created_at: string;
} {
  if (value.schema !== 'narada.sop.outbox_event.v1') throw new Error('sop_outbox_event_schema_invalid');
  const topic = requiredString(value.topic, 'sop_outbox_topic_missing');
  if (topic !== SOP_TERMINAL_TOPIC) throw new Error(`sop_outbox_topic_invalid:${topic}`);
  const outcome = requiredString(value.outcome, 'sop_outbox_outcome_missing');
  if (outcome !== 'completed' && outcome !== 'failed' && outcome !== 'cancelled') {
    throw new Error(`sop_outbox_outcome_invalid:${outcome}`);
  }
  const createdAt = normalizeTimestamp(value.created_at, 'sop_outbox_created_at');
  return {
    event_id: requiredString(value.event_id, 'sop_outbox_event_id_missing'),
    topic,
    partition_key: requiredString(value.partition_key, 'sop_outbox_partition_key_missing'),
    run_id: requiredString(value.run_id, 'sop_outbox_run_id_missing'),
    occurrence_key: requiredString(value.occurrence_key, 'sop_outbox_occurrence_key_missing'),
    outcome,
    payload: requireRecord(value.payload, 'sop_outbox_payload_invalid'),
    created_at: createdAt,
  };
}

function parseSopVersion(value: unknown): number {
  const match = /^v?([1-9]\d*)$/.exec(requiredString(value, 'scheduler_target_template_version_missing'));
  if (!match) throw new Error(`scheduler_target_template_version_invalid:${String(value)}`);
  const version = Number(match[1]);
  if (!Number.isSafeInteger(version)) throw new Error(`scheduler_target_template_version_invalid:${String(value)}`);
  return version;
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

function boundedInteger(value: number | undefined, fallback: number, min: number, max: number, name: string): number {
  const resolved = value ?? fallback;
  if (!Number.isSafeInteger(resolved) || resolved < min || resolved > max) throw new Error(`${name}_invalid`);
  return resolved;
}

function normalizeTimestamp(value: unknown, name: string): string {
  const parsed = new Date(requiredString(value, `${name}_required`));
  if (Number.isNaN(parsed.getTime())) throw new Error(`${name}_invalid`);
  return parsed.toISOString();
}

function boundedError(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error);
  return text.length <= 2_048 ? text : text.slice(0, 2_048);
}

function errorRecord(stage: string, error: unknown, itemId?: unknown): JsonRecord {
  return {
    stage,
    ...(typeof itemId === 'string' && itemId ? { item_id: itemId } : {}),
    error: boundedError(error),
  };
}

function parseCliArgs(argv: string[]): SchedulerSopDispatcherOptions {
  const values = new Map<string, string>();
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index]!;
    if (!flag.startsWith('--')) throw new Error(`unexpected_argument:${flag}`);
    const value = argv[index + 1];
    if (!value || value.startsWith('--')) throw new Error(`missing_argument_value:${flag}`);
    if (values.has(flag)) throw new Error(`duplicate_argument:${flag}`);
    values.set(flag, value);
    index += 1;
  }
  const known = new Set([
    '--site-root', '--outbox-start-at', '--consumer-id', '--scheduler-surface-id',
    '--sop-surface-id', '--max-events', '--max-activations', '--lease-ms',
    '--request-timeout-ms', '--loader-entrypoint', '--binding-admission-path',
  ]);
  for (const flag of values.keys()) if (!known.has(flag)) throw new Error(`unknown_argument:${flag}`);
  return {
    siteRoot: requiredString(values.get('--site-root'), 'site_root_required'),
    outboxStartAt: requiredString(values.get('--outbox-start-at'), 'outbox_start_at_required'),
    ...(values.has('--consumer-id') ? { consumerId: values.get('--consumer-id') } : {}),
    ...(values.has('--scheduler-surface-id') ? { schedulerSurfaceId: values.get('--scheduler-surface-id') } : {}),
    ...(values.has('--sop-surface-id') ? { sopSurfaceId: values.get('--sop-surface-id') } : {}),
    ...(values.has('--max-events') ? { maxEvents: Number(values.get('--max-events')) } : {}),
    ...(values.has('--max-activations') ? { maxActivations: Number(values.get('--max-activations')) } : {}),
    ...(values.has('--lease-ms') ? { leaseMs: Number(values.get('--lease-ms')) } : {}),
    ...(values.has('--request-timeout-ms') ? { requestTimeoutMs: Number(values.get('--request-timeout-ms')) } : {}),
    ...(values.has('--loader-entrypoint') ? { loaderEntrypoint: values.get('--loader-entrypoint') } : {}),
    ...(values.has('--binding-admission-path') ? { bindingAdmissionPath: values.get('--binding-admission-path') } : {}),
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const report = await runSchedulerSopDispatcher(parseCliArgs(process.argv.slice(2)));
    process.stdout.write(`${JSON.stringify(report)}\n`);
    if (report.status !== 'completed') process.exitCode = 1;
  } catch (error) {
    process.stderr.write(`${JSON.stringify({ schema: 'narada.scheduler.sop_dispatch_pass.v1', status: 'error', error: boundedError(error) })}\n`);
    process.exitCode = 1;
  }
}
