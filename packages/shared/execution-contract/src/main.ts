import { isAbsolute, resolve } from 'node:path';
import { createHash } from 'node:crypto';

export const EXECUTOR_KINDS = Object.freeze([
  'manual',
  'operator',
  'worker_delegation',
  'delegated_task',
] as const);

const EXECUTION_BINDING_KEYS = new Set([
  'workspace_root',
  'executor_kind',
  'executor_profile',
  'executor_id',
  'repository_root',
  'site_root',
  'correlation_key',
]);

export type ExecutorKind = (typeof EXECUTOR_KINDS)[number];

export type ExecutionBinding = {
  workspace_root: string;
  executor_kind: ExecutorKind;
  executor_profile: string | null;
  executor_id: string | null;
  repository_root: string | null;
  site_root: string | null;
  correlation_key: string;
};

export type ExecutionBindingDefaults = {
  workspace_root?: string;
  executor_kind?: ExecutorKind;
  executor_profile?: string | null;
  executor_id?: string | null;
  repository_root?: string | null;
  site_root?: string | null;
  correlation_key?: string;
};

export function executionBindingSchema() {
  return {
    type: 'object',
    properties: {
      workspace_root: { type: 'string', description: 'Absolute workspace root in which execution is authorized.' },
      executor_kind: { type: 'string', enum: [...EXECUTOR_KINDS] },
      executor_profile: { type: 'string' },
      executor_id: { type: 'string' },
      repository_root: { type: 'string' },
      site_root: { type: 'string' },
      correlation_key: { type: 'string', description: 'Stable operation correlation key used for recovery and result binding.' },
    },
    required: ['workspace_root', 'executor_kind', 'correlation_key'],
    additionalProperties: false,
  };
}

export function normalizeExecutionBinding(value: unknown, defaults: ExecutionBindingDefaults = {}): ExecutionBinding {
  const input = asRecord(value);
  const unknownKeys = Object.keys(input).filter((key) => !EXECUTION_BINDING_KEYS.has(key));
  if (unknownKeys.length > 0) throw new Error(`execution_binding_unknown_fields: ${unknownKeys.join(',')}`);
  const workspaceRoot = inputString(input, 'workspace_root', true) ?? stringValue(defaults.workspace_root);
  if (!workspaceRoot) throw new Error('execution_binding_workspace_root_required');
  const resolvedWorkspaceRoot = resolveAbsolutePath(workspaceRoot, 'execution_binding_workspace_root');
  const rawKind = inputString(input, 'executor_kind', true) ?? defaults.executor_kind ?? 'manual';
  if (!EXECUTOR_KINDS.includes(rawKind as ExecutorKind)) throw new Error(`execution_binding_executor_kind_invalid: ${rawKind}`);
  const correlationKey = inputString(input, 'correlation_key', true) ?? stringValue(defaults.correlation_key);
  if (!correlationKey) throw new Error('execution_binding_correlation_key_required');
  return {
    workspace_root: resolvedWorkspaceRoot,
    executor_kind: rawKind as ExecutorKind,
    executor_profile: inputString(input, 'executor_profile') ?? defaults.executor_profile ?? null,
    executor_id: inputString(input, 'executor_id') ?? defaults.executor_id ?? null,
    repository_root: optionalAbsolutePath(input.repository_root ?? defaults.repository_root, 'execution_binding_repository_root'),
    site_root: optionalAbsolutePath(input.site_root ?? defaults.site_root, 'execution_binding_site_root'),
    correlation_key: correlationKey,
  };
}

export function executionRequestFingerprint(value: unknown): string {
  return createHash('sha256').update(canonicalJson(value)).digest('hex');
}

export function canonicalJson(value: unknown): string {
  return JSON.stringify(canonicalize(value));
}

function canonicalize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (!value || typeof value !== 'object') return value;
  return Object.fromEntries(Object.entries(value as Record<string, unknown>).sort(([left], [right]) => left.localeCompare(right)).map(([key, entry]) => [key, canonicalize(entry)]));
}

function resolveAbsolutePath(value: string, field: string): string {
  if (!isAbsolute(value)) throw new Error(`${field}_must_be_absolute`);
  return resolve(value);
}

function optionalAbsolutePath(value: unknown, field: string): string | null {
  const text = optionalInputString(value, field);
  return text ? resolveAbsolutePath(text, field) : null;
}

function inputString(input: Record<string, unknown>, key: string, required = false): string | undefined {
  if (!(key in input)) return undefined;
  return optionalInputString(input[key], `execution_binding_${key}`, required);
}

function optionalInputString(value: unknown, field: string, required = false): string | undefined {
  if (value === undefined) return undefined;
  if (value === null && !required) return undefined;
  if (typeof value !== 'string') throw new Error(`${field}_must_be_string`);
  const text = value.trim();
  if (!text) {
    if (required) throw new Error(`${field}_required`);
    return undefined;
  }
  return text;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}
