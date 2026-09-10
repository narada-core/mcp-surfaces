#!/usr/bin/env node
import { pathToFileURL } from 'node:url';
import {
  SiteFabricClient,
  isRecord,
  type JsonRecord,
  type SiteFabricToolCallOptions,
} from '@narada-core/mcp-runtime-client';

export interface TicketDraftDispositionFabricCaller {
  call(
    surfaceId: string,
    toolName: string,
    args?: JsonRecord,
    options?: SiteFabricToolCallOptions,
  ): Promise<JsonRecord>;
}

export interface TicketDraftDispositionReconcilerOptions {
  siteRoot: string;
  consumerId?: string;
  scopeId?: string;
  configPath?: string;
  maxDispositions?: number;
  requestTimeoutMs?: number;
  cycleKey?: string;
  loaderEntrypoint?: string;
  bindingAdmissionPath?: string;
}

export interface TicketDraftDispositionReconcilerReport extends JsonRecord {
  schema: 'narada.scheduler.ticket_draft_disposition_reconciliation_pass.v1';
  status: 'completed' | 'completed_with_errors';
  observations_recorded: number;
  dispositions_seen: number;
  dispositions_reconciled: number;
  mailbox_refreshed: boolean;
  errors: JsonRecord[];
}

export async function runTicketDraftDispositionReconciler(
  input: TicketDraftDispositionReconcilerOptions,
  providedFabric?: TicketDraftDispositionFabricCaller,
): Promise<TicketDraftDispositionReconcilerReport> {
  const options = normalizeOptions(input);
  const ownedFabric = providedFabric ? null : await SiteFabricClient.open({
    siteRoot: options.siteRoot,
    loaderEntrypoint: options.loaderEntrypoint,
    bindingAdmissionPath: options.bindingAdmissionPath,
    allowedSurfaceIds: ['graph-mail', 'work-lifecycle', 'mailbox'],
    requestTimeoutMs: options.requestTimeoutMs,
  });
  const fabric = providedFabric ?? ownedFabric!;
  const report: TicketDraftDispositionReconcilerReport = {
    schema: 'narada.scheduler.ticket_draft_disposition_reconciliation_pass.v1',
    status: 'completed',
    observations_recorded: 0,
    dispositions_seen: 0,
    dispositions_reconciled: 0,
    mailbox_refreshed: false,
    errors: [],
  };

  try {
    const scan = await fabric.call('graph-mail', 'graph_mail_ticket_draft_disposition_scan', {
      limit: Math.min(options.maxDispositions, 5),
    });
    report.observations_recorded = optionalInteger(scan.observations_recorded) ?? 0;
    const listed = await fabric.call('graph-mail', 'graph_mail_ticket_draft_disposition_list', {
      consumer_id: options.consumerId,
      limit: Math.min(options.maxDispositions, 5),
    });
    const receipts = recordArray(listed.items, 'disposition_list_items_invalid');
    report.dispositions_seen = receipts.length;

    for (const receipt of receipts) {
      const observationId = optionalString(receipt.observation_id);
      try {
        validateDispositionReceipt(receipt);
        const reconciled = parseDomainOperation(await fabric.call(
          'work-lifecycle',
          'ticket_draft_disposition_reconcile',
          {
            ticket_id: receipt.ticket_id,
            draft_id: receipt.draft_id,
            evidence: receipt,
            idempotency_key: `ticket-draft-disposition:${receipt.observation_id}`,
            causation_id: receipt.observation_id,
          },
          { timeoutMs: options.requestTimeoutMs },
        ));
        await fabric.call('graph-mail', 'graph_mail_ticket_draft_disposition_ack', {
          observation_id: receipt.observation_id,
          consumer_id: options.consumerId,
          reconciliation_ref: reconciled.operation_ref,
          reconciliation_receipt: reconciled.result,
        });
        report.dispositions_reconciled += 1;
      } catch (error) {
        report.errors.push({
          stage: 'disposition_reconcile',
          ...(observationId ? { observation_id: observationId } : {}),
          error: boundedError(error),
        });
      }
    }

    if (report.dispositions_reconciled > 0) {
      try {
        parseDomainOperation(await fabric.call('mailbox', 'mailbox_sync_generation', {
          idempotency_key: `post-send-sync:${options.cycleKey}`,
          scope_id: options.scopeId,
          config_path: options.configPath,
          timeout_ms: Math.min(options.requestTimeoutMs, 60_000),
        }));
        parseDomainOperation(await fabric.call('mailbox', 'mailbox_thread_attention_rebuild', {
          idempotency_key: `post-send-attention:${options.cycleKey}`,
          scope_id: options.scopeId,
          config_path: options.configPath,
        }));
        report.mailbox_refreshed = true;
      } catch (error) {
        report.errors.push({ stage: 'mailbox_refresh', error: boundedError(error) });
      }
    }
  } catch (error) {
    report.errors.push({ stage: 'disposition_scan_or_list', error: boundedError(error) });
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

interface DomainOperation {
  operation_ref: string;
  result: JsonRecord;
}

function parseDomainOperation(value: JsonRecord): DomainOperation {
  if (value.schema !== 'narada.domain_operation.v1') throw new Error('domain_operation_schema_invalid');
  const operationRef = requiredString(value.operation_ref, 'domain_operation_ref_missing');
  if (value.outcome !== 'completed') {
    throw new Error(`domain_operation_not_completed:${String(value.error_message ?? value.outcome)}`);
  }
  if (!isRecord(value.result)) throw new Error('domain_operation_result_invalid');
  return { operation_ref: operationRef, result: value.result };
}

function validateDispositionReceipt(receipt: JsonRecord): void {
  if (receipt.schema !== 'narada.graph_mail.ticket_draft_disposition_receipt.v1') {
    throw new Error('disposition_receipt_schema_invalid');
  }
  requiredString(receipt.observation_id, 'disposition_observation_id_missing');
  requiredString(receipt.ticket_id, 'disposition_ticket_id_missing');
  requiredString(receipt.draft_id, 'disposition_draft_id_missing');
  if (receipt.disposition !== 'sent' && receipt.disposition !== 'discarded') {
    throw new Error(`disposition_invalid:${String(receipt.disposition)}`);
  }
  requiredString(receipt.receipt_sha256, 'disposition_receipt_sha256_missing');
}

interface NormalizedOptions {
  siteRoot: string;
  consumerId: string;
  scopeId: string;
  configPath: string;
  maxDispositions: number;
  requestTimeoutMs: number;
  cycleKey: string;
  loaderEntrypoint?: string;
  bindingAdmissionPath?: string;
}

function normalizeOptions(input: TicketDraftDispositionReconcilerOptions): NormalizedOptions {
  return {
    siteRoot: requiredString(input.siteRoot, 'siteRoot_required'),
    consumerId: optionalString(input.consumerId) ?? 'work-lifecycle-ticket-draft-reconciler',
    scopeId: optionalString(input.scopeId) ?? 'smart-scheduling-help-global-maxima',
    configPath: optionalString(input.configPath) ?? 'config/config.json',
    maxDispositions: boundedInteger(input.maxDispositions, 5, 1, 5, 'maxDispositions'),
    requestTimeoutMs: boundedInteger(input.requestTimeoutMs, 60_000, 1_000, 300_000, 'requestTimeoutMs'),
    cycleKey: optionalString(input.cycleKey) ?? new Date().toISOString(),
    ...(input.loaderEntrypoint ? { loaderEntrypoint: input.loaderEntrypoint } : {}),
    ...(input.bindingAdmissionPath ? { bindingAdmissionPath: input.bindingAdmissionPath } : {}),
  };
}

function recordArray(value: unknown, code: string): JsonRecord[] {
  if (!Array.isArray(value) || value.some((entry) => !isRecord(entry))) throw new Error(code);
  return value as JsonRecord[];
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

function optionalInteger(value: unknown): number | null {
  return typeof value === 'number' && Number.isSafeInteger(value) ? value : null;
}

function boundedInteger(value: number | undefined, fallback: number, min: number, max: number, name: string): number {
  const resolved = value ?? fallback;
  if (!Number.isSafeInteger(resolved) || resolved < min || resolved > max) throw new Error(`${name}_invalid`);
  return resolved;
}

function boundedError(error: unknown): string {
  const value = error instanceof Error ? error.message : String(error);
  return value.length <= 2_048 ? value : value.slice(0, 2_048);
}

function parseCliArgs(argv: string[]): TicketDraftDispositionReconcilerOptions {
  const values = parseFlagValues(argv, new Set([
    '--site-root', '--consumer-id', '--scope-id', '--config-path', '--max-dispositions',
    '--request-timeout-ms', '--cycle-key', '--loader-entrypoint', '--binding-admission-path',
  ]));
  return {
    siteRoot: requiredString(values.get('--site-root'), 'site_root_required'),
    ...(values.has('--consumer-id') ? { consumerId: values.get('--consumer-id') } : {}),
    ...(values.has('--scope-id') ? { scopeId: values.get('--scope-id') } : {}),
    ...(values.has('--config-path') ? { configPath: values.get('--config-path') } : {}),
    ...(values.has('--max-dispositions') ? { maxDispositions: Number(values.get('--max-dispositions')) } : {}),
    ...(values.has('--request-timeout-ms') ? { requestTimeoutMs: Number(values.get('--request-timeout-ms')) } : {}),
    ...(values.has('--cycle-key') ? { cycleKey: values.get('--cycle-key') } : {}),
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

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const report = await runTicketDraftDispositionReconciler(parseCliArgs(process.argv.slice(2)));
    process.stdout.write(`${JSON.stringify(report)}\n`);
    if (report.status !== 'completed') process.exitCode = 1;
  } catch (error) {
    process.stderr.write(`${JSON.stringify({
      schema: 'narada.scheduler.ticket_draft_disposition_reconciliation_pass.v1',
      status: 'error',
      error: boundedError(error),
    })}\n`);
    process.exitCode = 1;
  }
}
