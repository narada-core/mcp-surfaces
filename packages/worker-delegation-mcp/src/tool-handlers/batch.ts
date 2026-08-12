import { diagnosticError } from '../errors.js';

export function normalizeBatchRequests(value: unknown): unknown[] {
  if (!Array.isArray(value) || value.length === 0) throw diagnosticError('worker_run_batch_requests_required', 'worker_run_batch_requests_required');
  if (value.length > 50) throw diagnosticError('worker_run_batch_too_large', 'worker_run_batch_too_large', { max_requests: 50 });
  return [...value];
}

export function requireBatchRequestRecord(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw diagnosticError('worker_run_batch_item_invalid', 'worker_run_batch_item_invalid');
  return value as Record<string, unknown>;
}

export function normalizeRunIds(value: unknown): string[] {
  if (!Array.isArray(value) || value.length === 0) throw diagnosticError('worker_run_ids_required', 'worker_run_ids_required');
  return uniqueStrings(value.map((item) => requiredNonEmptyString(item, 'worker_run_id_required')));
}

export function normalizeOptionalRunIds(value: unknown): string[] {
  if (value === undefined || value === null) return [];
  return normalizeRunIds(value);
}

function uniqueStrings(values: string[]): string[] {
  return [...new Set(values.filter(Boolean))];
}

function requiredNonEmptyString(value: unknown, code: string): string {
  const text = String(value ?? '').trim();
  if (!text) throw diagnosticError(code);
  return text;
}
