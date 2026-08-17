#!/usr/bin/env node
import { buildGuidanceResult } from './guidance.js';
import { guidanceToolDefinition } from './guidance.js';
import { createTaskLifecycleProcessClient, type TaskLifecycleProcessClient } from './task-lifecycle-client.js';
import { randomUUID } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { DatabaseSync } from '@narada-core/sqlite';

const SERVER_NAME = 'surface-feedback-mcp';
const SERVER_VERSION = '0.2.0';
const PROTOCOL_VERSION = '2024-11-05';

const FEEDBACK_KINDS = ['bug', 'improvement', 'gap', 'observation'] as const;
const FEEDBACK_STATUSES = ['submitted', 'acknowledged', 'routed', 'converted_to_task', 'closed'] as const;
const UNPROCESSED_FEEDBACK_STATUSES = ['submitted', 'acknowledged'] as const;
const FEEDBACK_READ_SCOPES = ['all_authorized', 'store_reconciliation', 'authority_visible', 'owned_surfaces', 'authority_site_submissions'] as const;
const HANDOFF_LEASE_MS = 120_000;
const HANDOFF_LEASE_RENEW_MS = 30_000;

type JsonRecord = Record<string, unknown>;
type FeedbackReadScope = typeof FEEDBACK_READ_SCOPES[number];
type TaskLifecycleRequest = (request: JsonRecord) => Promise<JsonRecord>;
type AuthoritySource = 'server_config' | 'unconfigured';
type TaskLifecycleRootSource = 'option' | 'task_lifecycle_env' | 'site_root_env' | 'feedback_root_fallback';
type FeedbackDiscoveryKind = 'configured_root' | 'registrar_allowed_root';
type FeedbackSourceSyncStatus = 'synced' | 'missing' | 'invalid' | 'conflict';

type FeedbackDiscoveryRoot = {
  root: string;
  kind: FeedbackDiscoveryKind;
};

type FeedbackSourceSync = {
  sourceDbPath: string;
  discoveryRoot: string;
  discoveryKind: FeedbackDiscoveryKind;
  status: FeedbackSourceSyncStatus;
  rowsSeen: number;
  importedCount: number;
  updatedCount: number;
  skippedCount: number;
  conflictCount: number;
  error: string | null;
  lastScannedAt: string;
};

type FeedbackState = {
  feedbackRoot: string;
  dbPath: string;
  canonicalFeedbackRoot: string;
  taskLifecycleRoot: string;
  taskLifecycleRootSource: TaskLifecycleRootSource;
  authoritySiteId: string | null;
  authorityPrincipal: string | null;
  authorityOwnedSurfaceIds: string[];
  authoritySource: AuthoritySource;
  feedbackDiscoveryRoots: FeedbackDiscoveryRoot[];
  feedbackDiscoveryDiagnostics: string[];
  feedbackSourceSync: FeedbackSourceSync[];
  taskLifecycleRequest: TaskLifecycleRequest;
  taskLifecycleClient: TaskLifecycleProcessClient | null;
  taskLifecycleHealth: 'unverified' | 'healthy' | 'unhealthy';
  taskLifecycleHealthError: string | null;
  handoffLeaseMs: number;
  handoffLeaseRenewMs: number;
  db: DatabaseSync;
};

const CREATE_TABLES = [
  `CREATE TABLE IF NOT EXISTS feedback_entries (
    feedback_id TEXT PRIMARY KEY,
    surface_id TEXT NOT NULL,
    submitter_site_id TEXT NOT NULL,
    submitter_principal TEXT NOT NULL,
    kind TEXT NOT NULL,
    summary TEXT NOT NULL,
    details TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'submitted',
    resolution_note TEXT,
    resolved_by TEXT,
    task_ref TEXT,
    task_status TEXT,
    source_db_path TEXT,
    source_updated_at TEXT,
    source_sync_mode TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
  ) STRICT`,
  `CREATE TABLE IF NOT EXISTS feedback_events (
    event_id TEXT PRIMARY KEY,
    feedback_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    actor_principal TEXT NOT NULL,
    status TEXT,
    task_ref TEXT,
    task_status TEXT,
    note TEXT,
    details_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
  ) STRICT`,
  `CREATE TABLE IF NOT EXISTS feedback_task_handoffs (
    feedback_id TEXT PRIMARY KEY,
    idempotency_key TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL,
    payload_ref TEXT,
    task_ref TEXT,
    task_number INTEGER,
    task_id TEXT,
    task_status TEXT,
    requested_note TEXT,
    requested_title TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_expires_at TEXT,
    last_error_code TEXT,
    last_error_message TEXT,
    last_error_details TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
  ) STRICT`,
  `CREATE TABLE IF NOT EXISTS feedback_source_sync (
    source_db_path TEXT PRIMARY KEY,
    discovery_root TEXT NOT NULL,
    discovery_kind TEXT NOT NULL,
    status TEXT NOT NULL,
    rows_seen INTEGER NOT NULL DEFAULT 0,
    imported_count INTEGER NOT NULL DEFAULT 0,
    updated_count INTEGER NOT NULL DEFAULT 0,
    skipped_count INTEGER NOT NULL DEFAULT 0,
    conflict_count INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    last_scanned_at TEXT NOT NULL
  ) STRICT`,
];

export function createServerState(options: JsonRecord = {}): FeedbackState {
  const feedbackRoot = resolve(String(options.feedbackRoot ?? options.outputRoot ?? process.cwd()));
  const configuredCanonicalFeedbackRoot = options.canonicalFeedbackRoot
    ?? process.env.NARADA_SURFACE_FEEDBACK_ROOT;
  if (typeof configuredCanonicalFeedbackRoot !== 'string' || configuredCanonicalFeedbackRoot.trim().length === 0) {
    throw diagnosticError(
      'canonical_feedback_root_required',
      'canonical_feedback_root_required',
      {
        remediation: 'Pass --canonical-feedback-root <path> or configure NARADA_SURFACE_FEEDBACK_ROOT before starting surface-feedback-mcp.',
        feedback_root: feedbackRoot,
      },
    );
  }
  const canonicalFeedbackRoot = resolve(configuredCanonicalFeedbackRoot);
  const taskLifecycleRootOption = optionalString(options.taskLifecycleRoot);
  const taskLifecycleRootEnv = optionalString(process.env.NARADA_TASK_LIFECYCLE_ROOT);
  const siteRootEnv = optionalString(process.env.NARADA_SITE_ROOT);
  const taskLifecycleRoot = resolve(taskLifecycleRootOption ?? taskLifecycleRootEnv ?? siteRootEnv ?? feedbackRoot);
  const taskLifecycleRootSource: TaskLifecycleRootSource = taskLifecycleRootOption
    ? 'option'
    : taskLifecycleRootEnv
      ? 'task_lifecycle_env'
      : siteRootEnv
        ? 'site_root_env'
        : 'feedback_root_fallback';
  const dbPath = resolve(feedbackRoot, '.feedback', 'surface-feedback.db');
  const dbDir = resolve(dbPath, '..');
  mkdirSync(dbDir, { recursive: true });
  const db = new DatabaseSync(dbPath);
  db.exec('PRAGMA journal_mode=WAL');
  for (const sql of CREATE_TABLES) db.exec(sql);
  ensureColumn(db, 'feedback_entries', 'resolution_note', 'TEXT');
  ensureColumn(db, 'feedback_entries', 'resolved_by', 'TEXT');
  ensureColumn(db, 'feedback_entries', 'task_ref', 'TEXT');
  ensureColumn(db, 'feedback_entries', 'task_status', 'TEXT');
  ensureColumn(db, 'feedback_entries', 'source_db_path', 'TEXT');
  ensureColumn(db, 'feedback_entries', 'source_updated_at', 'TEXT');
  ensureColumn(db, 'feedback_entries', 'source_sync_mode', 'TEXT');
  const authority = resolveAuthority(options);
  const taskLifecycleClient = typeof options.taskLifecycleRequest === 'function'
    ? null
    : createTaskLifecycleProcessClient({ siteRoot: taskLifecycleRoot });
  const taskLifecycleRequest = typeof options.taskLifecycleRequest === 'function'
    ? options.taskLifecycleRequest as TaskLifecycleRequest
    : taskLifecycleClient!.request.bind(taskLifecycleClient!);
  const state: FeedbackState = {
    feedbackRoot,
    dbPath,
    canonicalFeedbackRoot,
    taskLifecycleRoot,
    taskLifecycleRootSource,
    authoritySiteId: authority.siteId,
    authorityPrincipal: authority.principal,
    authorityOwnedSurfaceIds: authority.ownedSurfaceIds,
    authoritySource: authority.source,
    feedbackDiscoveryRoots: [],
    feedbackDiscoveryDiagnostics: [],
    feedbackSourceSync: [],
    taskLifecycleRequest,
    taskLifecycleClient,
    taskLifecycleHealth: 'unverified',
    taskLifecycleHealthError: null,
    handoffLeaseMs: integer(options.handoffLeaseMs, HANDOFF_LEASE_MS, 50, 600_000),
    handoffLeaseRenewMs: integer(options.handoffLeaseRenewMs, HANDOFF_LEASE_RENEW_MS, 10, 300_000),
    db,
  };
  const discovery = samePath(feedbackRoot, canonicalFeedbackRoot)
    ? resolveFeedbackDiscoveryRoots(options, taskLifecycleRoot)
    : { roots: [], diagnostics: [] };
  state.feedbackDiscoveryRoots = discovery.roots;
  state.feedbackDiscoveryDiagnostics = discovery.diagnostics;
  syncDiscoveredFeedbackStores(state);
  return state;
}

function resolveFeedbackDiscoveryRoots(options: JsonRecord, taskLifecycleRoot: string): { roots: FeedbackDiscoveryRoot[]; diagnostics: string[] } {
  const roots: FeedbackDiscoveryRoot[] = [];
  const diagnostics: string[] = [];
  const addRoot = (value: unknown, kind: FeedbackDiscoveryKind): void => {
    const root = optionalString(value);
    if (!root) return;
    const resolvedRoot = resolve(root);
    const existing = roots.find((entry) => samePath(entry.root, resolvedRoot));
    if (existing) {
      if (kind === 'configured_root') existing.kind = kind;
      return;
    }
    roots.push({ root: resolvedRoot, kind });
  };

  for (const root of stringList(options.feedbackDiscoveryRoots ?? process.env.NARADA_FEEDBACK_DISCOVERY_ROOTS)) {
    addRoot(root, 'configured_root');
  }

  const allowedRootsPath = resolve(taskLifecycleRoot, '.narada', 'allowed-roots.json');
  if (!existsSync(allowedRootsPath)) return { roots, diagnostics };
  try {
    const document = asRecord(JSON.parse(readFileSync(allowedRootsPath, 'utf8')));
    if (document.schema !== 'narada.site.allowed_roots.v1' || document.generated_by !== 'mcp-registrar') {
      diagnostics.push(`registrar_allowed_roots_untrusted:${allowedRootsPath}`);
      return { roots, diagnostics };
    }
    addRoot(taskLifecycleRoot, 'registrar_allowed_root');
    for (const root of stringList(document.extra_allowed_roots)) addRoot(root, 'registrar_allowed_root');
  } catch (error) {
    diagnostics.push(`registrar_allowed_roots_unreadable:${allowedRootsPath}:${error instanceof Error ? error.message : String(error)}`);
  }
  return { roots, diagnostics };
}

function discoverFeedbackStoreCandidates(state: FeedbackState): { candidates: Array<FeedbackDiscoveryRoot & { sourceDbPath: string }>; diagnostics: string[] } {
  const candidates = new Map<string, FeedbackDiscoveryRoot & { sourceDbPath: string }>();
  const diagnostics = [...state.feedbackDiscoveryDiagnostics];
  const relativeStorePaths = [
    ['.feedback', 'surface-feedback.db'],
    ['.narada', 'feedback', '.feedback', 'surface-feedback.db'],
  ];
  const candidateRoots = (root: string): string[] => {
    const result = [root];
    if (!existsSync(root)) {
      diagnostics.push(`feedback_discovery_root_missing:${root}`);
      return result;
    }
    try {
      for (const entry of readdirSync(root, { withFileTypes: true }).sort((left, right) => left.name.localeCompare(right.name))) {
        if (entry.isDirectory()) result.push(resolve(root, entry.name));
      }
    } catch (error) {
      diagnostics.push(`feedback_discovery_root_unreadable:${root}:${error instanceof Error ? error.message : String(error)}`);
    }
    return result;
  };
  for (const discoveryRoot of state.feedbackDiscoveryRoots) {
    for (const candidateRoot of candidateRoots(discoveryRoot.root)) {
      for (const relativePath of relativeStorePaths) {
        const sourceDbPath = resolve(candidateRoot, ...relativePath);
        if (samePath(sourceDbPath, state.dbPath) || !existsSync(sourceDbPath)) continue;
        const key = process.platform === 'win32' ? sourceDbPath.toLowerCase() : sourceDbPath;
        if (!candidates.has(key)) candidates.set(key, { ...discoveryRoot, sourceDbPath });
      }
    }
  }
  return { candidates: [...candidates.values()], diagnostics };
}

function sourceFeedbackRows(sourceDb: DatabaseSync): JsonRecord[] {
  const columns = new Set((sourceDb.prepare('PRAGMA table_info(feedback_entries)').all() as JsonRecord[]).map((row) => String(row.name)));
  const requiredColumns = ['feedback_id', 'surface_id', 'submitter_site_id', 'submitter_principal', 'kind', 'summary', 'created_at', 'updated_at'];
  if (requiredColumns.some((column) => !columns.has(column))) {
    throw new Error('feedback_entries_schema_missing_required_columns');
  }
  const supportedColumns = [
    'feedback_id', 'surface_id', 'submitter_site_id', 'submitter_principal', 'kind', 'summary', 'details',
    'status', 'resolution_note', 'resolved_by', 'task_ref', 'task_status', 'created_at', 'updated_at',
  ].filter((column) => columns.has(column));
  const rows = sourceDb.prepare(`SELECT ${supportedColumns.join(', ')} FROM feedback_entries`).all() as JsonRecord[];
  for (const row of rows) {
    for (const column of requiredColumns) {
      if (!optionalString(row[column])) throw new Error(`feedback_entries_row_missing_${column}`);
    }
  }
  return rows;
}

function sourceUpdatedAt(row: JsonRecord): string {
  return optionalString(row.updated_at) ?? optionalString(row.created_at) ?? nowIso();
}

function sourceSyncRecord(row: JsonRecord): FeedbackSourceSync {
  return {
    sourceDbPath: String(row.source_db_path),
    discoveryRoot: String(row.discovery_root),
    discoveryKind: String(row.discovery_kind) as FeedbackDiscoveryKind,
    status: String(row.status) as FeedbackSourceSyncStatus,
    rowsSeen: Number(row.rows_seen ?? 0),
    importedCount: Number(row.imported_count ?? 0),
    updatedCount: Number(row.updated_count ?? 0),
    skippedCount: Number(row.skipped_count ?? 0),
    conflictCount: Number(row.conflict_count ?? 0),
    error: optionalString(row.error),
    lastScannedAt: String(row.last_scanned_at),
  };
}

function sourceSyncView(source: FeedbackSourceSync): JsonRecord {
  return {
    source_db_path: source.sourceDbPath,
    discovery_root: source.discoveryRoot,
    discovery_kind: source.discoveryKind,
    status: source.status,
    rows_seen: source.rowsSeen,
    imported_count: source.importedCount,
    updated_count: source.updatedCount,
    skipped_count: source.skippedCount,
    conflict_count: source.conflictCount,
    error: source.error,
    last_scanned_at: source.lastScannedAt,
  };
}

function persistFeedbackSourceSync(state: FeedbackState, sync: FeedbackSourceSync): void {
  state.db.prepare(
    `INSERT INTO feedback_source_sync
      (source_db_path, discovery_root, discovery_kind, status, rows_seen, imported_count, updated_count, skipped_count, conflict_count, error, last_scanned_at)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
     ON CONFLICT(source_db_path) DO UPDATE SET
       discovery_root = excluded.discovery_root,
       discovery_kind = excluded.discovery_kind,
       status = excluded.status,
       rows_seen = excluded.rows_seen,
       imported_count = excluded.imported_count,
       updated_count = excluded.updated_count,
       skipped_count = excluded.skipped_count,
       conflict_count = excluded.conflict_count,
       error = excluded.error,
       last_scanned_at = excluded.last_scanned_at`
  ).run(
    sync.sourceDbPath,
    sync.discoveryRoot,
    sync.discoveryKind,
    sync.status,
    sync.rowsSeen,
    sync.importedCount,
    sync.updatedCount,
    sync.skippedCount,
    sync.conflictCount,
    sync.error,
    sync.lastScannedAt,
  );
}

function syncOneFeedbackStore(state: FeedbackState, candidate: FeedbackDiscoveryRoot & { sourceDbPath: string }): FeedbackSourceSync {
  const result: FeedbackSourceSync = {
    sourceDbPath: candidate.sourceDbPath,
    discoveryRoot: candidate.root,
    discoveryKind: candidate.kind,
    status: 'synced',
    rowsSeen: 0,
    importedCount: 0,
    updatedCount: 0,
    skippedCount: 0,
    conflictCount: 0,
    error: null,
    lastScannedAt: nowIso(),
  };
  let sourceDb: DatabaseSync | null = null;
  try {
    sourceDb = new DatabaseSync(candidate.sourceDbPath, { readOnly: true });
    const rows = sourceFeedbackRows(sourceDb);
    result.rowsSeen = rows.length;
    const actor = state.authorityPrincipal ?? 'surface-feedback@federation';
    withImmediateTransaction(state.db, () => {
      const targetSelect = state.db.prepare('SELECT * FROM feedback_entries WHERE feedback_id = ?');
      const insert = state.db.prepare(
        `INSERT INTO feedback_entries
          (feedback_id, surface_id, submitter_site_id, submitter_principal, kind, summary, details, status, resolution_note, resolved_by, task_ref, task_status, source_db_path, source_updated_at, source_sync_mode, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`
      );
      const update = state.db.prepare(
        `UPDATE feedback_entries SET
          surface_id = ?, submitter_site_id = ?, submitter_principal = ?, kind = ?, summary = ?, details = ?, status = ?,
          resolution_note = ?, resolved_by = ?, task_ref = ?, task_status = ?, source_updated_at = ?, source_sync_mode = ?,
          created_at = ?, updated_at = ?
         WHERE feedback_id = ?`
      );
      for (const row of rows) {
        const feedbackId = String(row.feedback_id);
        const updatedAt = sourceUpdatedAt(row);
        const existing = targetSelect.get(feedbackId) as JsonRecord | undefined;
        if (!existing) {
          insert.run(
            feedbackId,
            String(row.surface_id),
            String(row.submitter_site_id),
            String(row.submitter_principal),
            String(row.kind),
            String(row.summary),
            String(row.details ?? ''),
            String(row.status ?? 'submitted'),
            optionalString(row.resolution_note),
            optionalString(row.resolved_by),
            optionalString(row.task_ref) ?? legacyTaskRef(row.resolution_note),
            optionalString(row.task_status),
            candidate.sourceDbPath,
            updatedAt,
            'startup_materialized',
            String(row.created_at),
            updatedAt,
          );
          recordFeedbackEvent(state, {
            feedback_id: feedbackId,
            event_type: 'federated_imported',
            actor_principal: actor,
            status: String(row.status ?? 'submitted'),
            task_ref: optionalString(row.task_ref) ?? legacyTaskRef(row.resolution_note),
            task_status: optionalString(row.task_status),
            details: {
              source_db_path: candidate.sourceDbPath,
              discovery_root: candidate.root,
              discovery_kind: candidate.kind,
              source_updated_at: updatedAt,
            },
          });
          result.importedCount += 1;
          continue;
        }

        const existingSourcePath = optionalString(existing.source_db_path);
        const existingSourceUpdatedAt = optionalString(existing.source_updated_at);
        if (!existingSourcePath || !samePath(existingSourcePath, candidate.sourceDbPath)) {
          result.conflictCount += 1;
          recordFeedbackEvent(state, {
            feedback_id: feedbackId,
            event_type: 'federated_conflict',
            actor_principal: actor,
            status: optionalString(existing.status),
            details: {
              source_db_path: candidate.sourceDbPath,
              existing_source_db_path: existingSourcePath,
              discovery_root: candidate.root,
              reason: 'feedback_id_already_owned_by_another_store',
            },
          });
          continue;
        }
        if (existingSourceUpdatedAt === updatedAt) {
          result.skippedCount += 1;
          continue;
        }
        const existingUpdatedAt = optionalString(existing.updated_at);
        if (existingSourceUpdatedAt && existingUpdatedAt === existingSourceUpdatedAt && updatedAt > existingSourceUpdatedAt) {
          update.run(
            String(row.surface_id),
            String(row.submitter_site_id),
            String(row.submitter_principal),
            String(row.kind),
            String(row.summary),
            String(row.details ?? ''),
            String(row.status ?? 'submitted'),
            optionalString(row.resolution_note),
            optionalString(row.resolved_by),
            optionalString(row.task_ref) ?? legacyTaskRef(row.resolution_note),
            optionalString(row.task_status),
            updatedAt,
            'startup_materialized',
            String(row.created_at),
            updatedAt,
            feedbackId,
          );
          recordFeedbackEvent(state, {
            feedback_id: feedbackId,
            event_type: 'federated_refreshed',
            actor_principal: actor,
            status: String(row.status ?? 'submitted'),
            task_ref: optionalString(row.task_ref) ?? legacyTaskRef(row.resolution_note),
            task_status: optionalString(row.task_status),
            details: {
              source_db_path: candidate.sourceDbPath,
              discovery_root: candidate.root,
              discovery_kind: candidate.kind,
              source_updated_at: updatedAt,
              previous_source_updated_at: existingSourceUpdatedAt,
            },
          });
          result.updatedCount += 1;
          continue;
        }
        if (existingSourceUpdatedAt && updatedAt > existingSourceUpdatedAt) {
          result.conflictCount += 1;
          recordFeedbackEvent(state, {
            feedback_id: feedbackId,
            event_type: 'federated_conflict',
            actor_principal: actor,
            status: optionalString(existing.status),
            details: {
              source_db_path: candidate.sourceDbPath,
              discovery_root: candidate.root,
              reason: 'canonical_entry_changed_after_source_materialization',
              source_updated_at: updatedAt,
              canonical_updated_at: existingUpdatedAt,
              previous_source_updated_at: existingSourceUpdatedAt,
            },
          });
          continue;
        }
        result.skippedCount += 1;
      }
    });
  } catch (error) {
    result.status = 'invalid';
    result.error = error instanceof Error ? error.message : String(error);
  } finally {
    sourceDb?.close();
  }
  if (result.conflictCount > 0 && result.status === 'synced') result.status = 'conflict';
  return result;
}

function syncDiscoveredFeedbackStores(state: FeedbackState): void {
  const discovered = discoverFeedbackStoreCandidates(state);
  state.feedbackDiscoveryDiagnostics = discovered.diagnostics;
  const previous = new Map(
    (state.db.prepare('SELECT * FROM feedback_source_sync').all() as JsonRecord[]).map((row) => [String(row.source_db_path), sourceSyncRecord(row)])
  );
  const seen = new Set<string>();
  for (const candidate of discovered.candidates) {
    const key = process.platform === 'win32' ? candidate.sourceDbPath.toLowerCase() : candidate.sourceDbPath;
    seen.add(key);
    persistFeedbackSourceSync(state, syncOneFeedbackStore(state, candidate));
  }
  const scanTime = nowIso();
  for (const prior of previous.values()) {
    const key = process.platform === 'win32' ? prior.sourceDbPath.toLowerCase() : prior.sourceDbPath;
    if (seen.has(key)) continue;
    persistFeedbackSourceSync(state, {
      ...prior,
      status: 'missing',
      error: 'source_store_not_found_under_discovery_roots',
      lastScannedAt: scanTime,
    });
  }
  state.feedbackSourceSync = (state.db.prepare('SELECT * FROM feedback_source_sync ORDER BY source_db_path').all() as JsonRecord[]).map(sourceSyncRecord);
}

function legacyTaskRef(value: unknown): string | null {
  const note = String(value ?? '');
  const match = note.match(/(?:^|\s)Task:\s*(task\s+#\d+|\d{8}-\d+-[A-Za-z0-9._-]+)/i);
  return match?.[1] ?? null;
}

export async function handleRequest(request: JsonRecord, state: FeedbackState) {
  if (!request.id && typeof request.method === 'string' && request.method.startsWith('notifications/')) return null;
  try {
    const result = await dispatchMethod(String(request.method), asRecord(request.params), state);
    return { jsonrpc: '2.0', id: request.id ?? null, result };
  } catch (error) {
    const diagnostic = errorDiagnostic(error);
    return { jsonrpc: '2.0', id: request.id ?? null, error: { code: -32000, message: diagnostic.message, data: diagnostic } };
  }
}

export async function runStdioServer(options: JsonRecord = {}): Promise<void> {
  const state = createServerState(options);
  let buffer = '';
  let sawFramedInput = false;
  process.stdin.setEncoding('utf8');
  let closed = false;
  const close = async () => {
    if (closed) return;
    closed = true;
    await closeServerState(state);
  };
  const handleSignal = () => {
    void close().finally(() => process.exit(0));
  };
  process.once('SIGTERM', handleSignal);
  process.once('SIGINT', handleSignal);
  try {
    for await (const chunk of process.stdin) {
      buffer += chunk;
      const drained = buffer.includes('Content-Length:') ? drainJsonRpcFrames(buffer) : drainJsonLines(buffer);
      sawFramedInput ||= drained.framed;
      buffer = drained.remaining;
      for (const request of drained.requests) {
        const response = await handleRequest(request, state);
        if (response) writeJsonRpcResponse(response, { framed: sawFramedInput });
      }
    }
  } finally {
    process.removeListener('SIGTERM', handleSignal);
    process.removeListener('SIGINT', handleSignal);
    await close();
  }
}

export async function closeServerState(state: FeedbackState): Promise<void> {
  await state.taskLifecycleClient?.close();
  state.db.close();
}

async function dispatchMethod(method: string, params: JsonRecord, state: FeedbackState) {
  switch (method) {
    case 'initialize':
      return { protocolVersion: params.protocolVersion ?? PROTOCOL_VERSION, capabilities: { tools: {} }, serverInfo: { name: SERVER_NAME, version: SERVER_VERSION } };
    case 'tools/list':
      return { tools: listTools() };
    case 'tools/call':
      return await callTool(params, state);
    default:
      throw diagnosticError('unsupported_mcp_method', `unsupported_mcp_method:${method}`);
  }
}

export function listTools() {
  return [
    guidanceToolDefinition(),
    {
      name: 'surface_feedback_doctor',
      description: 'Inspect surface feedback MCP storage posture and backing store path.',
      inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      annotations: { title: 'surface_feedback_doctor', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'surface_feedback_submit',
      description: 'Submit feedback about an MCP surface. Cross-site: any site may submit feedback about any surface.',
      inputSchema: {
        type: 'object',
        properties: {
          surface_id: { type: 'string', description: 'Surface identifier, e.g. sop or scheduler.' },
          submitter_site_id: { type: 'string', description: 'Optional assertion that must equal server-bound NARADA_SITE_ID.' },
          submitter_principal: { type: 'string', description: 'Optional assertion that must equal server-bound NARADA_AGENT_ID.' },
          kind: { type: 'string', enum: FEEDBACK_KINDS, description: 'Feedback kind.' },
          summary: { type: 'string', description: 'One-line summary.' },
          details: { type: 'string', description: 'Extended description or context.' },
          idempotency_key: { type: 'string', description: 'Stable retry key; reuse with different content is refused.' },
        },
        required: ['surface_id', 'kind', 'summary'],
        additionalProperties: false,
      },
      annotations: { title: 'surface_feedback_submit', readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'surface_feedback_live_proof_template',
      description: 'Return a reusable structured template for live no-mock proof feedback and task handoff packets.',
      inputSchema: {
        type: 'object',
        properties: {
          workflow: { type: 'string', description: 'Optional workflow label to echo into the template.' },
          surface_id: { type: 'string', description: 'Optional surface identifier the proof contract will concern.' },
        },
        additionalProperties: false,
      },
      annotations: { title: 'surface_feedback_live_proof_template', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'surface_feedback_update_status',
      description: 'Update one feedback entry status with a concise resolution or routing note.',
      inputSchema: {
        type: 'object',
        properties: {
          feedback_id: { type: 'string' },
          status: { type: 'string', enum: FEEDBACK_STATUSES },
          resolved_by: { type: 'string', description: 'Deprecated compatibility field. Ignored; the server-bound authority principal is recorded.' },
          resolution_note: { type: 'string', description: 'Reason, fix summary, route destination, or acknowledgement note.' },
          task_ref: { type: 'string', description: 'Optional first-class linked task reference.' },
          task_status: { type: 'string', description: 'Optional projected lifecycle state for the linked task.' },
        },
        required: ['feedback_id', 'status', 'resolution_note'],
        additionalProperties: false,
      },
      annotations: { title: 'surface_feedback_update_status', readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'surface_feedback_convert_to_task',
      description: 'Create or recover one authoritative task-lifecycle handoff from the canonical feedback store using server-bound User Site authority. This explicit maintainer handoff can route any canonical entry; ordinary status mutations remain owner-scoped. The durable handoff is idempotent per feedback entry and this tool never executes the task.',
      inputSchema: {
        type: 'object',
        properties: {
          feedback_id: { type: 'string' },
          resolved_by: { type: 'string', description: 'Deprecated compatibility field. Ignored; the server-bound authority principal is recorded.' },
          resolution_note: { type: 'string', description: 'Optional conversion note recorded in the immutable feedback event log and latest projection.' },
          task_title: { type: 'string', description: 'Optional task title. Defaults to a title derived from the feedback summary.' },
        },
        required: ['feedback_id'],
        additionalProperties: false,
      },
      annotations: { title: 'surface_feedback_convert_to_task', readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'surface_feedback_update_status_batch',
      description: 'Update multiple feedback entries with per-item status, task refs, and resolution notes. Reports partial failures without rolling back successful updates.',
      inputSchema: {
        type: 'object',
        properties: {
          resolved_by: { type: 'string', description: 'Deprecated compatibility field. Ignored; the server-bound authority principal is recorded.' },
          updates: {
            type: 'array',
            minItems: 1,
            items: {
              type: 'object',
                properties: {
                  feedback_id: { type: 'string' },
                  status: { type: 'string', enum: FEEDBACK_STATUSES },
                  resolved_by: { type: 'string', description: 'Deprecated compatibility field. Ignored.' },
                resolution_note: { type: 'string', description: 'Per-item resolution or route note.' },
                task_ref: { type: 'string', description: 'Optional task reference, e.g. task #1276 or 20260623-1276-...' },
                task_status: { type: 'string', description: 'Optional projected lifecycle state for the linked task.' },
              },
              required: ['feedback_id', 'status', 'resolution_note'],
              additionalProperties: false,
            },
          },
        },
        required: ['updates'],
        additionalProperties: false,
      },
      annotations: { title: 'surface_feedback_update_status_batch', readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'surface_feedback_import',
      description: 'Import explicit feedback entries from another local surface-feedback store into this store. Intended for repairing split-brain site-local stores.',
      inputSchema: {
        type: 'object',
        properties: {
          source_feedback_root: { type: 'string', description: 'Root containing .feedback/surface-feedback.db for the source store.' },
          source_db_path: { type: 'string', description: 'Direct path to a source surface-feedback.db. Use only when source_feedback_root is unavailable.' },
          feedback_ids: { type: 'array', items: { type: 'string' }, minItems: 1, description: 'Feedback IDs to import from the source store.' },
        },
        required: ['feedback_ids'],
        additionalProperties: false,
      },
      annotations: { title: 'surface_feedback_import', readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'surface_feedback_list',
      description: 'List feedback entries using the required server-bound read scope. The scope enum is protocol-stable; inspect surface_feedback_guidance or surface_feedback_doctor capabilities.read_scopes before calling because availability is server-bound. Submitter-site visibility uses declared metadata, not authenticated provenance.',
      inputSchema: {
        type: 'object',
        properties: {
          surface_id: { type: 'string', description: 'Filter by surface identifier.' },
          submitter_site_id_filter: { type: 'string', description: 'Optional metadata filter by declared submitter site ID; this never grants or changes authorization.' },
          kind: { type: 'string', enum: FEEDBACK_KINDS, description: 'Filter by feedback kind.' },
          status: { type: 'string', enum: FEEDBACK_STATUSES, description: 'Filter by status.' },
          scope: { type: 'string', enum: FEEDBACK_READ_SCOPES, description: 'Required read scope. The enum is protocol-stable; inspect surface_feedback_guidance or surface_feedback_doctor capabilities.read_scopes before calling because availability is server-bound.' },
          since: { type: 'string', description: 'ISO 8601 start date.' },
          until: { type: 'string', description: 'ISO 8601 end date.' },
          limit: { type: 'number', default: 50 },
          offset: { type: 'number', default: 0 },
        },
        required: ['scope'],
        additionalProperties: false,
      },
      annotations: { title: 'surface_feedback_list', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'surface_feedback_actionable_queue',
      description: 'Return one bounded actionable feedback queue using the required server-bound read scope. The scope enum is protocol-stable; inspect surface_feedback_guidance or surface_feedback_doctor capabilities.read_scopes before calling because availability is server-bound. Submitter-site visibility uses declared metadata, not authenticated provenance.',
      inputSchema: {
        type: 'object',
        properties: {
          surface_id: { type: 'string', description: 'Optional filter by surface identifier.' },
          submitter_site_id_filter: { type: 'string', description: 'Optional metadata filter by declared submitter site ID; this never grants or changes authorization.' },
          kind: { type: 'string', enum: FEEDBACK_KINDS, description: 'Optional filter by feedback kind.' },
          scope: { type: 'string', enum: FEEDBACK_READ_SCOPES, description: 'Required read scope. The enum is protocol-stable; inspect surface_feedback_guidance or surface_feedback_doctor capabilities.read_scopes before calling because availability is server-bound.' },
          since: { type: 'string', description: 'ISO 8601 start date.' },
          until: { type: 'string', description: 'ISO 8601 end date.' },
          limit: { type: 'number', default: 50 },
          offset: { type: 'number', default: 0 },
        },
        required: ['scope'],
        additionalProperties: false,
      },
      annotations: { title: 'surface_feedback_actionable_queue', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'surface_feedback_show',
      description: 'Show one feedback entry by feedback_id using the required server-bound read scope. Inspect surface_feedback_guidance or surface_feedback_doctor capabilities.read_scopes before calling; an entry outside that scope is reported as not found.',
      inputSchema: {
        type: 'object',
        properties: {
          feedback_id: { type: 'string' },
          scope: { type: 'string', enum: FEEDBACK_READ_SCOPES, description: 'Required read scope. The enum is protocol-stable; inspect surface_feedback_guidance or surface_feedback_doctor capabilities.read_scopes before calling because availability is server-bound.' },
        },
        required: ['feedback_id', 'scope'],
        additionalProperties: false,
      },
      annotations: { title: 'surface_feedback_show', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
    {
      name: 'surface_feedback_stats',
      description: 'Return aggregated feedback counts by surface, kind, and status using an explicit server-bound read scope. Inspect surface_feedback_guidance or surface_feedback_doctor capabilities.read_scopes before calling.',
      inputSchema: {
        type: 'object',
        properties: {
          surface_id: { type: 'string', description: 'Optional surface ID filter.' },
          scope: { type: 'string', enum: FEEDBACK_READ_SCOPES, description: 'Required read scope. The enum is protocol-stable; inspect surface_feedback_guidance or surface_feedback_doctor capabilities.read_scopes before calling because availability is server-bound.' },
        },
        required: ['scope'],
        additionalProperties: false,
      },
      annotations: { title: 'surface_feedback_stats', readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      outputSchema: { type: 'object', additionalProperties: true },
    },
  ];
}

async function callTool(params: JsonRecord, state: FeedbackState) {
  const name = String(params.name ?? '');
  const args = asRecord(params.arguments);
  let result: JsonRecord;
  switch (name) {
    case 'surface_feedback_guidance':
      result = buildGuidanceResult(args, { capabilities: feedbackCapabilities(state) });
      break;
    case 'surface_feedback_doctor': result = feedbackDoctor(state); break;
    case 'surface_feedback_submit': result = feedbackSubmit(args, state); break;
    case 'surface_feedback_live_proof_template': result = feedbackLiveProofTemplate(args); break;
    case 'surface_feedback_update_status': result = feedbackUpdateStatus(args, state); break;
    case 'surface_feedback_convert_to_task': result = await feedbackConvertToTask(args, state); break;
    case 'surface_feedback_update_status_batch': result = feedbackUpdateStatusBatch(args, state); break;
    case 'surface_feedback_import': result = feedbackImport(args, state); break;
    case 'surface_feedback_list': result = feedbackList(args, state); break;
    case 'surface_feedback_actionable_queue': result = feedbackActionableQueue(args, state); break;
    case 'surface_feedback_show': result = feedbackShow(args, state); break;
    case 'surface_feedback_stats': result = feedbackStats(args, state); break;
    default: throw diagnosticError('unknown_tool', `unknown_tool:${name}`, { tool_name: name });
  }
  return { content: [{ type: 'text', text: renderResult(result) }], structuredContent: result };
}

function feedbackLiveProofTemplate(args: JsonRecord): JsonRecord {
  const workflow = optionalString(args.workflow);
  const surfaceId = optionalString(args.surface_id);
  return {
    schema: 'narada.surface_feedback.live_proof_template.v1',
    status: 'ok',
    workflow: workflow ?? null,
    surface_id: surfaceId ?? null,
    purpose: 'Capture evidence expectations for live, no-mock, no-fallback E2E authority/projection behavior.',
    recommended_feedback: {
      kind: 'observation',
      details_format: 'json_or_markdown_with_live_proof_contract',
    },
    live_proof_contract: {
      authority_location: {
        deployed: '<where the deployed authority or projection state lives>',
        local: '<where local source/test authority lives>',
      },
      transport: {
        live_transport_assumption: '<named live transport path and why it is expected>',
        replay_vs_live_delivery: '<how replay evidence is distinguished from live delivery>',
      },
      success: {
        semantic_success_point: '<observable state/event that proves live success>',
        saved_evidence_file: '<required artifact path or null when not applicable>',
      },
      exclusions: {
        no_mock: '<evidence that mocks were not used>',
        no_fallback: '<evidence that fallback path was not used>',
        no_shim: '<evidence that compatibility shim did not carry the behavior>',
      },
      negative_controls: {
        revocation_or_refusal_proof: '<how revoked/unauthorized paths fail>',
      },
      test_alignment: {
        unit_tests_specify_deployed_transport: '<yes/no/unknown plus file references>',
      },
    },
    usage: [
      'Use this template in feedback details when reporting live-proof gaps or observations.',
      'Use it in task context when converting feedback into implementation work.',
      'Do not treat a completed template as proof by itself; proof requires cited artifacts and live readback.',
    ],
  };
}

function feedbackDoctor(state: FeedbackState): JsonRecord {
  const row = state.db.prepare('SELECT COUNT(*) AS count FROM feedback_entries').get() as JsonRecord;
  const materializedRow = state.db.prepare('SELECT COUNT(*) AS count FROM feedback_entries WHERE source_db_path IS NOT NULL').get() as JsonRecord;
  const usesCanonicalStore = samePath(state.feedbackRoot, state.canonicalFeedbackRoot);
  const taskRoot = taskLifecycleRootPosture(state);
  const authorityConfigured = Boolean(state.authoritySiteId && state.authorityPrincipal);
  const federationProblems = [
    ...state.feedbackDiscoveryDiagnostics,
    ...state.feedbackSourceSync.filter((source) => source.status !== 'synced').map((source) => `${source.status}:${source.sourceDbPath}${source.error ? `:${source.error}` : ''}`),
  ];
  const status = usesCanonicalStore && taskRoot.configuration_valid && authorityConfigured && state.taskLifecycleHealth !== 'unhealthy' && federationProblems.length === 0 ? 'ok' : 'warning';
  return {
    schema: 'narada.surface_feedback.doctor.v1',
    status,
    server_name: SERVER_NAME,
    server_version: SERVER_VERSION,
    protocol_version: PROTOCOL_VERSION,
    storage_posture: usesCanonicalStore ? 'canonical_feedback_root' : 'noncanonical_feedback_root',
    feedback_root: state.feedbackRoot,
    canonical_feedback_root: state.canonicalFeedbackRoot,
    uses_canonical_store: usesCanonicalStore,
    db_path: state.dbPath,
    task_lifecycle_root: state.taskLifecycleRoot,
    task_lifecycle_root_source: state.taskLifecycleRootSource,
    task_lifecycle_root_configured: taskRoot.configuration_valid,
    task_lifecycle_root_diagnostics: taskRoot,
    task_lifecycle_integration: state.taskLifecycleClient ? 'isolated_stdio_process' : 'injected_request_adapter',
    task_lifecycle_health: state.taskLifecycleHealth,
    task_lifecycle_health_error: state.taskLifecycleHealthError,
    authority: {
      configured: authorityConfigured,
      source: state.authoritySource,
      site_id: state.authoritySiteId,
      principal: state.authorityPrincipal,
      owned_surface_ids: state.authorityOwnedSurfaceIds,
    },
    task_handoff_capability: taskHandoffCapability(state),
    capabilities: feedbackCapabilities(state),
    total_feedback_entries: Number(row.count ?? 0),
    federation: {
      enabled: state.feedbackDiscoveryRoots.length > 0,
      discovery_roots: state.feedbackDiscoveryRoots,
      discovery_diagnostics: state.feedbackDiscoveryDiagnostics,
      source_db_count: state.feedbackSourceSync.length,
      active_source_db_count: state.feedbackSourceSync.filter((source) => source.status !== 'missing').length,
      materialized_entry_count: Number(materializedRow.count ?? 0),
      conflict_count: state.feedbackSourceSync.reduce((total, source) => total + source.conflictCount, 0),
      sources: state.feedbackSourceSync.map(sourceSyncView),
    },
    diagnostics: [
      usesCanonicalStore ? null : 'The feedback store is noncanonical; cross-site feedback may be invisible.',
      taskRoot.configuration_valid ? null : 'The task-lifecycle root configuration is invalid because the root or .ai directory is missing.',
      state.taskLifecycleHealth === 'unhealthy' ? `The task-lifecycle child is unhealthy: ${state.taskLifecycleHealthError ?? 'unknown error'}` : null,
      authorityConfigured ? null : 'Server-bound mutation authority is not configured.',
      ...state.feedbackDiscoveryDiagnostics,
      ...state.feedbackSourceSync.filter((source) => source.status !== 'synced').map((source) => `Federated source ${source.sourceDbPath} is ${source.status}${source.error ? `: ${source.error}` : '.'}`),
    ].filter(Boolean),
    remediation: [
      usesCanonicalStore ? null : `Configure --canonical-feedback-root ${state.canonicalFeedbackRoot}.`,
      taskRoot.configuration_valid ? null : 'Configure --task-lifecycle-root or NARADA_TASK_LIFECYCLE_ROOT to a Site root containing .ai.',
      state.taskLifecycleHealth === 'unhealthy' ? 'Repair task-lifecycle startup/runtime configuration, then retry a lifecycle operation to refresh observed health.' : null,
      authorityConfigured ? null : 'Configure --site-id/NARADA_SITE_ID and optional --owned-surface-id values.',
      ...state.feedbackDiscoveryDiagnostics.map(() => 'Repair the registrar-generated allowed-roots.json or configure an explicit --feedback-discovery-root.'),
      ...state.feedbackSourceSync.filter((source) => source.status !== 'synced').map((source) => `Inspect or repair the federated source store ${source.sourceDbPath}; canonical entries were not overwritten.`),
    ].filter(Boolean),
  };
}

function feedbackCapabilities(state: FeedbackState): JsonRecord {
  const usesCanonicalStore = samePath(state.feedbackRoot, state.canonicalFeedbackRoot);
  const readAuthorityConfigured = Boolean(state.authoritySiteId);
  const authorityConfigured = Boolean(state.authoritySiteId && state.authorityPrincipal);
  const authorityRemediation = 'Configure --site-id/NARADA_SITE_ID on the serving User Site projection.';
  const unavailableScope = (available: boolean, reasonCode: string, reason: string, remediation: string, extra: JsonRecord = {}): JsonRecord => ({
    available,
    reason_code: available ? null : reasonCode,
    reason: available ? null : reason,
    remediation: available ? null : remediation,
    ...extra,
  });
  const allAuthorizedAvailable = usesCanonicalStore && readAuthorityConfigured;
  const allAuthorizedReasonCode = usesCanonicalStore
    ? 'feedback_global_read_requires_server_authority'
    : 'feedback_global_read_requires_canonical_store';
  const allAuthorizedReason = usesCanonicalStore
    ? 'Server-bound Site authority is not configured for the canonical cross-site read.'
    : 'The configured feedback root is not the canonical feedback store.';
  const allAuthorizedRemediation = usesCanonicalStore
    ? authorityRemediation
    : `Configure --canonical-feedback-root ${state.canonicalFeedbackRoot} for the canonical cross-site feedback store.`;
  return {
    schema: 'narada.surface_feedback.capabilities.v1',
    status: allAuthorizedAvailable ? 'ready' : 'degraded',
    read_scopes: {
      all_authorized: unavailableScope(
        allAuthorizedAvailable,
        allAuthorizedReasonCode,
        allAuthorizedReason,
        allAuthorizedRemediation,
        { canonical_store_required: true, server_authority_required: true },
      ),
      store_reconciliation: unavailableScope(
        allAuthorizedAvailable,
        allAuthorizedReasonCode,
        allAuthorizedReason,
        allAuthorizedRemediation,
        {
          canonical_store_required: true,
          server_authority_required: true,
          purpose: 'read_every_row_physically_present_for_store_reconciliation',
          mutation_authority_unchanged: true,
        },
      ),
      authority_visible: unavailableScope(
        readAuthorityConfigured,
        'feedback_read_scope_requires_server_authority',
        'Server-bound Site authority is not configured for authority-visible reads.',
        authorityRemediation,
        { server_authority_required: true },
      ),
      owned_surfaces: unavailableScope(
        readAuthorityConfigured,
        'feedback_read_scope_requires_server_authority',
        'Server-bound Site authority is not configured for owned-surface reads.',
        authorityRemediation,
        {
          server_authority_required: true,
          owned_surface_ids_configured: state.authorityOwnedSurfaceIds.length > 0,
          data_available: readAuthorityConfigured && state.authorityOwnedSurfaceIds.length > 0,
        },
      ),
      authority_site_submissions: unavailableScope(
        readAuthorityConfigured,
        'feedback_read_scope_requires_server_authority',
        'Server-bound Site authority is not configured for submitter-site reads.',
        authorityRemediation,
        { server_authority_required: true },
      ),
    },
    task_handoff: taskHandoffCapability(state),
    authority: {
      read_configured: readAuthorityConfigured,
      configured: authorityConfigured,
      source: state.authoritySource,
      site_id: state.authoritySiteId,
      principal: state.authorityPrincipal,
      owned_surface_ids: state.authorityOwnedSurfaceIds,
    },
    storage: {
      uses_canonical_store: usesCanonicalStore,
      feedback_root: state.feedbackRoot,
      canonical_feedback_root: state.canonicalFeedbackRoot,
      federation_enabled: state.feedbackDiscoveryRoots.length > 0,
      discovery_roots: state.feedbackDiscoveryRoots,
      source_db_count: state.feedbackSourceSync.length,
      source_sync_statuses: state.feedbackSourceSync.map((source) => ({ source_db_path: source.sourceDbPath, status: source.status })),
    },
  };
}

function feedbackSubmit(args: JsonRecord, state: FeedbackState): JsonRecord {
  const surfaceId = requiredString(args.surface_id, 'feedback_requires_surface_id');
  const siteId = requiredString(args.submitter_site_id, 'feedback_requires_submitter_site_id');
  const principal = requiredString(args.submitter_principal, 'feedback_requires_submitter_principal');
  const kind = requiredString(args.kind, 'feedback_requires_kind');
  if (!FEEDBACK_KINDS.includes(kind as typeof FEEDBACK_KINDS[number])) throw diagnosticError('feedback_invalid_kind', `feedback_invalid_kind:${kind}`, { allowed: FEEDBACK_KINDS });
  const summary = requiredString(args.summary, 'feedback_requires_summary');
  const details = optionalString(args.details) ?? '';
  const feedbackId = `sfb_${randomUUID().slice(0, 12)}`;
  const now = nowIso();
  withImmediateTransaction(state.db, () => {
    state.db.prepare(
      'INSERT INTO feedback_entries (feedback_id, surface_id, submitter_site_id, submitter_principal, kind, summary, details, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)'
    ).run(feedbackId, surfaceId, siteId, principal, kind, summary, details, 'submitted', now, now);
    recordFeedbackEvent(state, {
      feedback_id: feedbackId,
      event_type: 'submitted',
      actor_principal: principal,
      status: 'submitted',
      note: summary,
      details: { submitter_site_id: siteId, surface_id: surfaceId, kind },
      created_at: now,
    });
  });
  return { status: 'submitted', feedback_id: feedbackId, surface_id: surfaceId, submitter_site_id: siteId, kind, summary, created_at: now };
}

function feedbackUpdateStatus(args: JsonRecord, state: FeedbackState): JsonRecord {
  const feedbackId = requiredString(args.feedback_id, 'feedback_requires_feedback_id');
  const status = requiredString(args.status, 'feedback_requires_status');
  if (!FEEDBACK_STATUSES.includes(status as typeof FEEDBACK_STATUSES[number])) throw diagnosticError('feedback_invalid_status', `feedback_invalid_status:${status}`, { allowed: FEEDBACK_STATUSES });
  const resolvedBy = mutationPrincipal(state);
  const resolutionNote = requiredString(args.resolution_note, 'feedback_requires_resolution_note');
  const taskRef = optionalString(args.task_ref);
  const taskStatus = optionalString(args.task_status);
  const existing = state.db.prepare('SELECT * FROM feedback_entries WHERE feedback_id = ?').get(feedbackId) as JsonRecord | undefined;
  if (!existing) throw feedbackNotFound(feedbackId, state);
  assertMutationAuthority(existing, state);
  const now = nowIso();
  withImmediateTransaction(state.db, () => {
    if (taskRef || taskStatus) {
      state.db.prepare('UPDATE feedback_entries SET status = ?, resolved_by = ?, resolution_note = ?, task_ref = COALESCE(?, task_ref), task_status = COALESCE(?, task_status), updated_at = ? WHERE feedback_id = ?').run(status, resolvedBy, resolutionNote, taskRef, taskStatus, now, feedbackId);
    } else {
      state.db.prepare('UPDATE feedback_entries SET status = ?, resolved_by = ?, resolution_note = ?, updated_at = ? WHERE feedback_id = ?').run(status, resolvedBy, resolutionNote, now, feedbackId);
    }
    recordFeedbackEvent(state, {
      feedback_id: feedbackId,
      event_type: 'status_updated',
      actor_principal: resolvedBy,
      status,
      task_ref: taskRef,
      task_status: taskStatus,
      note: resolutionNote,
      details: { previous_status: existing.status, previous_resolution_note: existing.resolution_note ?? null },
      created_at: now,
    });
  });
  const updated = state.db.prepare('SELECT * FROM feedback_entries WHERE feedback_id = ?').get(feedbackId) as JsonRecord;
  return { status: 'updated', feedback: hydrateFeedback(updated) };
}

async function feedbackConvertToTask(args: JsonRecord, state: FeedbackState): Promise<JsonRecord> {
  const feedbackId = requiredString(args.feedback_id, 'feedback_requires_feedback_id');
  rejectClientAuthorityOverrides(args);
  assertTaskHandoffAuthority(state);
  const resolvedBy = mutationPrincipal(state);
  const row = state.db.prepare('SELECT * FROM feedback_entries WHERE feedback_id = ?').get(feedbackId) as JsonRecord | undefined;
  if (!row) throw feedbackNotFound(feedbackId, state);
  requireTaskLifecycleRootReady(state);

  const existingTaskRef = optionalString(row.task_ref) ?? legacyTaskRef(row.resolution_note);
  if (existingTaskRef) {
    if (String(row.status) !== 'converted_to_task') {
      throw diagnosticError('feedback_task_link_conflict', `feedback_task_link_conflict:${feedbackId}`, {
        feedback_id: feedbackId,
        status: row.status,
        task_ref: existingTaskRef,
      });
    }
    const linkedHandoff = ensureLinkedHandoffForExisting(state, row, existingTaskRef);
    return buildTaskConversionResult(
      'already_linked',
      row,
      existingTaskRef,
      numberValue(linkedHandoff.task_number),
      optionalString(row.task_status),
      optionalString(linkedHandoff.task_id),
      optionalString(linkedHandoff.payload_ref),
      linkedHandoff,
      state.authoritySiteId,
    );
  }

  const idempotencyKey = `surface-feedback:${feedbackId}`;
  let handoff = claimFeedbackTaskHandoff(state, row, args, resolvedBy, idempotencyKey);
  if (String(handoff.status) === 'linked' && optionalString(handoff.task_ref)) {
    const current = state.db.prepare('SELECT * FROM feedback_entries WHERE feedback_id = ?').get(feedbackId) as JsonRecord;
    return buildTaskConversionResult(
      'already_linked',
      current,
      String(handoff.task_ref),
      numberValue(handoff.task_number),
      optionalString(handoff.task_status),
      optionalString(handoff.task_id),
      optionalString(handoff.payload_ref),
      handoff,
      state.authoritySiteId,
    );
  }

  const leaseOwner = requiredString(handoff.lease_owner, 'feedback_handoff_lease_missing', { feedback_id: feedbackId });
  let payloadRef = optionalString(handoff.payload_ref);
  let taskRef = optionalString(handoff.task_ref);
  let taskNumber = numberValue(handoff.task_number);
  let taskId = optionalString(handoff.task_id);
  let taskStatus = optionalString(handoff.task_status);
  let failureStage = payloadRef ? 'task_lifecycle_create' : 'mcp_payload_create';
  try {
    if (!payloadRef) {
      const taskDefinition = buildTaskDefinition(row, {
        ...args,
        task_title: optionalString(handoff.requested_title) ?? args.task_title,
      }, idempotencyKey);
      const payloadResult = await withHandoffLeaseHeartbeat(state, feedbackId, leaseOwner, () =>
        callTaskLifecycle(state, 'mcp_payload_create', {
          payload_id: `surface-feedback-${feedbackId}-task`,
          payload: taskDefinition,
          created_by: resolvedBy,
        })
      );
      payloadRef = requiredString(payloadResult.ref ?? payloadResult.payload_ref, 'feedback_task_payload_ref_missing', { feedback_id: feedbackId });
      failureStage = 'payload_state_persist';
      markHandoffPayloadCreated(state, feedbackId, leaseOwner, payloadRef);
      failureStage = 'payload_audit_persist';
      recordFeedbackEvent(state, {
        feedback_id: feedbackId,
        event_type: 'task_payload_created',
        actor_principal: resolvedBy,
        status: String(row.status),
        note: 'Persisted task creation payload for feedback handoff.',
        details: { payload_ref: payloadRef, idempotency_key: idempotencyKey },
      });
      failureStage = 'task_lifecycle_create';
    }
    if (!taskRef) {
      const taskResult = await withHandoffLeaseHeartbeat(state, feedbackId, leaseOwner, () =>
        callTaskLifecycle(state, 'task_lifecycle_create', { payload_ref: payloadRef })
      );
      taskNumber = numberValue(taskResult.task_number);
      taskId = optionalString(taskResult.task_id);
      taskRef = taskNumber !== null ? `task #${taskNumber}` : taskId;
      if (!taskRef) {
        throw diagnosticError('feedback_task_create_result_invalid', `feedback_task_create_result_invalid:${feedbackId}`, {
          feedback_id: feedbackId,
          payload_ref: payloadRef,
          task_result: taskResult,
        });
      }
      taskStatus = optionalString(taskResult.task_status) ?? 'opened';
      failureStage = 'task_state_persist';
      markHandoffTaskCreated(state, feedbackId, leaseOwner, {
        payload_ref: payloadRef,
        task_ref: taskRef,
        task_number: taskNumber,
        task_id: taskId,
        task_status: taskStatus,
      });
      failureStage = 'task_audit_persist';
      recordFeedbackEvent(state, {
        feedback_id: feedbackId,
        event_type: 'task_created',
        actor_principal: resolvedBy,
        status: String(row.status),
        task_ref: taskRef,
        task_status: taskStatus,
        note: `Task created for feedback; durable link pending.`,
        details: { payload_ref: payloadRef, idempotency_key: idempotencyKey },
      });
    }
  } catch (error) {
    const diagnostic = errorDiagnostic(error);
    let recoveryPersistenceError: JsonRecord | null = null;
    try {
      if (taskRef) {
        markHandoffPostCreateFailure(state, feedbackId, leaseOwner, {
          payload_ref: payloadRef,
          task_ref: taskRef,
          task_number: taskNumber,
          task_id: taskId,
          task_status: taskStatus,
        }, diagnostic);
      } else {
        markHandoffFailure(state, feedbackId, leaseOwner, payloadRef, diagnostic);
      }
    } catch (persistenceError) {
      recoveryPersistenceError = errorDiagnostic(persistenceError);
    }
    try {
      recordFeedbackEvent(state, {
        feedback_id: feedbackId,
        event_type: 'task_handoff_failed',
        actor_principal: resolvedBy,
        status: String(row.status),
        task_ref: taskRef,
        task_status: taskStatus,
        note: diagnostic.message,
        details: { stage: failureStage, code: diagnostic.code, recovery_persistence_error: recoveryPersistenceError },
      });
    } catch (auditError) {
      recoveryPersistenceError ??= errorDiagnostic(auditError);
    }
    throw diagnosticError(taskRef ? 'feedback_task_post_create_persist_failed' : 'feedback_task_handoff_failed', diagnostic.message, {
      feedback_id: feedbackId,
      stage: failureStage,
      payload_ref: payloadRef,
      task_ref: taskRef,
      task_number: taskNumber,
      task_id: taskId,
      task_status: taskStatus,
      handoff_status: taskRef ? 'task_created' : 'failed',
      retryable: true,
      task_lifecycle_code: diagnostic.code,
      task_lifecycle_details: diagnostic.details,
      recovery_persistence_error: recoveryPersistenceError,
      next_action: { tool: 'surface_feedback_convert_to_task', arguments: { feedback_id: feedbackId } },
    });
  }

  if (!taskRef) throw diagnosticError('feedback_handoff_task_ref_missing', `feedback_handoff_task_ref_missing:${feedbackId}`);
  const suppliedNote = optionalString(handoff.requested_note) ?? optionalString(args.resolution_note) ?? `Created ${taskRef} from feedback via surface_feedback_convert_to_task.`;
  try {
    linkFeedbackTaskHandoff(state, {
      feedback_id: feedbackId,
      lease_owner: leaseOwner,
      resolved_by: resolvedBy,
      resolution_note: suppliedNote,
      task_ref: taskRef,
      task_number: taskNumber,
      task_id: taskId,
      task_status: taskStatus,
      payload_ref: payloadRef,
      prior_resolution_note: row.resolution_note ?? null,
    });
  } catch (error) {
    const diagnostic = errorDiagnostic(error);
    let recoveryPersistenceError: JsonRecord | null = null;
    try {
      markHandoffLinkFailure(state, feedbackId, leaseOwner, diagnostic);
    } catch (persistenceError) {
      recoveryPersistenceError = errorDiagnostic(persistenceError);
    }
    try {
      recordFeedbackEvent(state, {
        feedback_id: feedbackId,
        event_type: 'task_link_failed',
        actor_principal: resolvedBy,
        status: String(row.status),
        task_ref: taskRef,
        task_status: taskStatus,
        note: optionalString(diagnostic.message) ?? 'Feedback task link failed.',
        details: { code: diagnostic.code, payload_ref: payloadRef, recovery_persistence_error: recoveryPersistenceError },
      });
    } catch (auditError) {
      recoveryPersistenceError ??= errorDiagnostic(auditError);
    }
    throw diagnosticError('feedback_task_link_failed', diagnostic.message, {
      feedback_id: feedbackId,
      task_ref: taskRef,
      task_number: taskNumber,
      task_id: taskId,
      task_status: taskStatus,
      payload_ref: payloadRef,
      handoff_status: 'task_created',
      retryable: true,
      recovery_persistence_error: recoveryPersistenceError,
      next_action: {
        tool: 'surface_feedback_convert_to_task',
        arguments: { feedback_id: feedbackId },
      },
    });
  }

  const updated = state.db.prepare('SELECT * FROM feedback_entries WHERE feedback_id = ?').get(feedbackId) as JsonRecord;
  handoff = readFeedbackTaskHandoff(state, feedbackId) ?? handoff;
  const resultStatus = Number(handoff.attempt_count ?? 1) > 1 ? 'recovered' : 'converted';
  return buildTaskConversionResult(resultStatus, updated, taskRef, taskNumber, taskStatus, taskId, payloadRef, handoff, state.authoritySiteId);
}

function claimFeedbackTaskHandoff(
  state: FeedbackState,
  row: JsonRecord,
  args: JsonRecord,
  resolvedBy: string,
  idempotencyKey: string,
): JsonRecord {
  const feedbackId = String(row.feedback_id);
  const now = nowIso();
  const leaseOwner = randomUUID();
  const leaseExpiresAt = new Date(Date.now() + state.handoffLeaseMs).toISOString();
  let existing = readFeedbackTaskHandoff(state, feedbackId);
  if (existing && String(existing.status) === 'linked') return existing;
  if (existing && String(existing.status) !== 'linked') {
    const leaseExpiry = Date.parse(String(existing.lease_expires_at ?? ''));
    if (Number.isFinite(leaseExpiry) && leaseExpiry > Date.now()) {
      throw diagnosticError('feedback_task_handoff_in_progress', `feedback_task_handoff_in_progress:${feedbackId}`, {
        feedback_id: feedbackId,
        lease_expires_at: existing.lease_expires_at,
        retryable: true,
      });
    }
  }
  if (!existing) {
    try {
      state.db.prepare(
        'INSERT INTO feedback_task_handoffs (feedback_id, idempotency_key, status, requested_note, requested_title, attempt_count, lease_owner, lease_expires_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)'
      ).run(
        feedbackId,
        idempotencyKey,
        'pending',
        optionalString(args.resolution_note),
        optionalString(args.task_title),
        1,
        leaseOwner,
        leaseExpiresAt,
        now,
        now,
      );
    } catch {
      existing = readFeedbackTaskHandoff(state, feedbackId);
      if (!existing) throw diagnosticError('feedback_task_handoff_reservation_failed', `feedback_task_handoff_reservation_failed:${feedbackId}`);
      return claimFeedbackTaskHandoff(state, row, args, resolvedBy, idempotencyKey);
    }
  } else {
    const resumeStatus = optionalString(existing.task_ref)
      ? 'task_created'
      : optionalString(existing.payload_ref)
        ? 'payload_created'
        : 'pending';
    const reclaim = state.db.prepare(
      'UPDATE feedback_task_handoffs SET status = ?, requested_note = COALESCE(requested_note, ?), requested_title = COALESCE(requested_title, ?), attempt_count = attempt_count + 1, lease_owner = ?, lease_expires_at = ?, last_error_code = NULL, last_error_message = NULL, last_error_details = NULL, updated_at = ? WHERE feedback_id = ? AND status <> ? AND (lease_expires_at IS NULL OR lease_expires_at <= ?)'
    ).run(
      resumeStatus,
      optionalString(args.resolution_note),
      optionalString(args.task_title),
      leaseOwner,
      leaseExpiresAt,
      now,
      feedbackId,
      'linked',
      now,
    ) as unknown as { changes?: number };
    if (Number(reclaim.changes ?? 0) !== 1) {
      const current = readFeedbackTaskHandoff(state, feedbackId);
      if (current && String(current.status) === 'linked') return current;
      throw diagnosticError('feedback_task_handoff_in_progress', `feedback_task_handoff_in_progress:${feedbackId}`, {
        feedback_id: feedbackId,
        lease_expires_at: current?.lease_expires_at ?? null,
        retryable: true,
      });
    }
  }
  recordFeedbackEvent(state, {
    feedback_id: feedbackId,
    event_type: existing ? 'task_handoff_resumed' : 'task_handoff_reserved',
    actor_principal: resolvedBy,
    status: String(row.status),
    note: existing ? 'Resumed durable feedback-to-task handoff.' : 'Reserved durable feedback-to-task handoff.',
    details: { idempotency_key: idempotencyKey, lease_expires_at: leaseExpiresAt },
    created_at: now,
  });
  return readFeedbackTaskHandoff(state, feedbackId) as JsonRecord;
}

function readFeedbackTaskHandoff(state: FeedbackState, feedbackId: string): JsonRecord | null {
  return state.db.prepare('SELECT * FROM feedback_task_handoffs WHERE feedback_id = ?').get(feedbackId) as JsonRecord | undefined ?? null;
}

function markHandoffPayloadCreated(state: FeedbackState, feedbackId: string, leaseOwner: string, payloadRef: string): void {
  const result = state.db.prepare(
    'UPDATE feedback_task_handoffs SET status = ?, payload_ref = ?, updated_at = ? WHERE feedback_id = ? AND lease_owner = ?'
  ).run('payload_created', payloadRef, nowIso(), feedbackId, leaseOwner) as unknown as { changes?: number };
  if (Number(result.changes ?? 0) !== 1) throw diagnosticError('feedback_handoff_lease_lost', `feedback_handoff_lease_lost:${feedbackId}`);
}

function markHandoffTaskCreated(state: FeedbackState, feedbackId: string, leaseOwner: string, task: JsonRecord): void {
  const result = state.db.prepare(
    'UPDATE feedback_task_handoffs SET status = ?, payload_ref = ?, task_ref = ?, task_number = ?, task_id = ?, task_status = ?, updated_at = ? WHERE feedback_id = ? AND lease_owner = ?'
  ).run(
    'task_created',
    optionalString(task.payload_ref),
    requiredString(task.task_ref, 'feedback_handoff_task_ref_missing'),
    numberValue(task.task_number),
    optionalString(task.task_id),
    optionalString(task.task_status),
    nowIso(),
    feedbackId,
    leaseOwner,
  ) as unknown as { changes?: number };
  if (Number(result.changes ?? 0) !== 1) throw diagnosticError('feedback_handoff_lease_lost', `feedback_handoff_lease_lost:${feedbackId}`);
}

function markHandoffPostCreateFailure(state: FeedbackState, feedbackId: string, leaseOwner: string, task: JsonRecord, diagnostic: JsonRecord): void {
  const result = state.db.prepare(
    'UPDATE feedback_task_handoffs SET status = ?, payload_ref = COALESCE(?, payload_ref), task_ref = ?, task_number = ?, task_id = ?, task_status = ?, lease_owner = NULL, lease_expires_at = NULL, last_error_code = ?, last_error_message = ?, last_error_details = ?, updated_at = ? WHERE feedback_id = ? AND lease_owner = ?'
  ).run(
    'task_created',
    optionalString(task.payload_ref),
    requiredString(task.task_ref, 'feedback_handoff_task_ref_missing'),
    numberValue(task.task_number),
    optionalString(task.task_id),
    optionalString(task.task_status),
    optionalString(diagnostic.code),
    optionalString(diagnostic.message),
    JSON.stringify(diagnostic.details ?? {}),
    nowIso(),
    feedbackId,
    leaseOwner,
  ) as unknown as { changes?: number };
  if (Number(result.changes ?? 0) !== 1) throw diagnosticError('feedback_handoff_lease_lost', `feedback_handoff_lease_lost:${feedbackId}`);
}

function renewHandoffLease(state: FeedbackState, feedbackId: string, leaseOwner: string): void {
  const leaseExpiresAt = new Date(Date.now() + state.handoffLeaseMs).toISOString();
  const result = state.db.prepare(
    'UPDATE feedback_task_handoffs SET lease_expires_at = ?, updated_at = ? WHERE feedback_id = ? AND lease_owner = ? AND status <> ?'
  ).run(leaseExpiresAt, nowIso(), feedbackId, leaseOwner, 'linked') as unknown as { changes?: number };
  if (Number(result.changes ?? 0) !== 1) throw diagnosticError('feedback_handoff_lease_lost', `feedback_handoff_lease_lost:${feedbackId}`);
}

async function withHandoffLeaseHeartbeat<T>(state: FeedbackState, feedbackId: string, leaseOwner: string, operation: () => Promise<T>): Promise<T> {
  renewHandoffLease(state, feedbackId, leaseOwner);
  let heartbeatError: unknown = null;
  const timer = setInterval(() => {
    try {
      renewHandoffLease(state, feedbackId, leaseOwner);
    } catch (error) {
      heartbeatError = error;
    }
  }, state.handoffLeaseRenewMs);
  timer.unref?.();
  try {
    const result = await operation();
    if (heartbeatError) throw heartbeatError;
    renewHandoffLease(state, feedbackId, leaseOwner);
    return result;
  } finally {
    clearInterval(timer);
  }
}

function markHandoffFailure(state: FeedbackState, feedbackId: string, leaseOwner: string, payloadRef: string | null, diagnostic: JsonRecord): void {
  const result = state.db.prepare(
    'UPDATE feedback_task_handoffs SET status = ?, payload_ref = COALESCE(?, payload_ref), lease_owner = NULL, lease_expires_at = NULL, last_error_code = ?, last_error_message = ?, last_error_details = ?, updated_at = ? WHERE feedback_id = ? AND lease_owner = ?'
  ).run('failed', payloadRef, optionalString(diagnostic.code), optionalString(diagnostic.message), JSON.stringify(diagnostic.details ?? {}), nowIso(), feedbackId, leaseOwner);
  if (Number((result as unknown as { changes?: number }).changes ?? 0) !== 1) {
    throw diagnosticError('feedback_handoff_lease_lost', `feedback_handoff_lease_lost:${feedbackId}`);
  }
}

function markHandoffLinkFailure(state: FeedbackState, feedbackId: string, leaseOwner: string, diagnostic: JsonRecord): void {
  const result = state.db.prepare(
    'UPDATE feedback_task_handoffs SET status = ?, lease_owner = NULL, lease_expires_at = NULL, last_error_code = ?, last_error_message = ?, last_error_details = ?, updated_at = ? WHERE feedback_id = ? AND lease_owner = ?'
  ).run('task_created', optionalString(diagnostic.code), optionalString(diagnostic.message), JSON.stringify(diagnostic.details ?? {}), nowIso(), feedbackId, leaseOwner);
  if (Number((result as unknown as { changes?: number }).changes ?? 0) !== 1) {
    throw diagnosticError('feedback_handoff_lease_lost', `feedback_handoff_lease_lost:${feedbackId}`);
  }
}

function linkFeedbackTaskHandoff(state: FeedbackState, link: JsonRecord): void {
  const feedbackId = requiredString(link.feedback_id, 'feedback_requires_feedback_id');
  const taskRef = requiredString(link.task_ref, 'feedback_handoff_task_ref_missing');
  state.db.exec('BEGIN IMMEDIATE');
  try {
    const current = state.db.prepare('SELECT * FROM feedback_entries WHERE feedback_id = ?').get(feedbackId) as JsonRecord | undefined;
    if (!current) throw feedbackNotFound(feedbackId, state);
    const currentTaskRef = optionalString(current.task_ref) ?? legacyTaskRef(current.resolution_note);
    if (currentTaskRef && currentTaskRef !== taskRef) {
      throw diagnosticError('feedback_task_link_conflict', `feedback_task_link_conflict:${feedbackId}`, {
        feedback_id: feedbackId,
        existing_task_ref: currentTaskRef,
        requested_task_ref: taskRef,
      });
    }
    state.db.prepare(
      'UPDATE feedback_entries SET status = ?, resolved_by = ?, resolution_note = ?, task_ref = ?, task_status = ?, updated_at = ? WHERE feedback_id = ?'
    ).run('converted_to_task', String(link.resolved_by), String(link.resolution_note), taskRef, optionalString(link.task_status), nowIso(), feedbackId);
    const handoffUpdate = state.db.prepare(
      'UPDATE feedback_task_handoffs SET status = ?, payload_ref = COALESCE(?, payload_ref), task_ref = ?, task_number = ?, task_id = ?, task_status = ?, lease_owner = NULL, lease_expires_at = NULL, last_error_code = NULL, last_error_message = NULL, last_error_details = NULL, updated_at = ? WHERE feedback_id = ? AND lease_owner = ?'
    ).run(
      'linked',
      optionalString(link.payload_ref),
      taskRef,
      numberValue(link.task_number),
      optionalString(link.task_id),
      optionalString(link.task_status),
      nowIso(),
      feedbackId,
      String(link.lease_owner),
    ) as unknown as { changes?: number };
    if (Number(handoffUpdate.changes ?? 0) !== 1) throw diagnosticError('feedback_handoff_lease_lost', `feedback_handoff_lease_lost:${feedbackId}`);
    recordFeedbackEvent(state, {
      feedback_id: feedbackId,
      event_type: 'task_linked',
      actor_principal: String(link.resolved_by),
      status: 'converted_to_task',
      task_ref: taskRef,
      task_status: optionalString(link.task_status),
      note: String(link.resolution_note),
      details: {
        payload_ref: link.payload_ref ?? null,
        prior_resolution_note: link.prior_resolution_note ?? null,
      },
    });
    state.db.exec('COMMIT');
  } catch (error) {
    state.db.exec('ROLLBACK');
    throw error;
  }
}

function ensureLinkedHandoffForExisting(state: FeedbackState, row: JsonRecord, taskRef: string): JsonRecord {
  const feedbackId = String(row.feedback_id);
  const existing = readFeedbackTaskHandoff(state, feedbackId);
  if (existing) return existing;
  const now = nowIso();
  state.db.prepare(
    'INSERT INTO feedback_task_handoffs (feedback_id, idempotency_key, status, task_ref, task_number, task_status, attempt_count, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)'
  ).run(feedbackId, `surface-feedback:${feedbackId}`, 'linked', taskRef, taskNumberFromRef(taskRef), optionalString(row.task_status), 0, now, now);
  return readFeedbackTaskHandoff(state, feedbackId) as JsonRecord;
}

async function callTaskLifecycle(state: FeedbackState, name: string, args: JsonRecord): Promise<JsonRecord> {
  let response: JsonRecord;
  try {
    response = await state.taskLifecycleRequest({
      jsonrpc: '2.0',
      id: randomUUID(),
      method: 'tools/call',
      params: { name, arguments: args },
    });
  } catch (error) {
    state.taskLifecycleHealth = 'unhealthy';
    state.taskLifecycleHealthError = error instanceof Error ? error.message : String(error);
    throw error;
  }
  state.taskLifecycleHealth = 'healthy';
  state.taskLifecycleHealthError = null;
  const error = asRecord(response.error);
  if (Object.keys(error).length > 0) {
    throw diagnosticError('task_lifecycle_request_failed', String(error.message ?? `task_lifecycle_request_failed:${name}`), {
      tool: name,
      response_error: error,
    });
  }
  const result = asRecord(response.result);
  if (result.isError === true) {
    throw diagnosticError('task_lifecycle_tool_refused', `task_lifecycle_tool_refused:${name}`, {
      tool: name,
      structured_content: result.structuredContent ?? null,
    });
  }
  const structured = asRecord(result.structuredContent);
  if (Object.keys(structured).length === 0) {
    throw diagnosticError('task_lifecycle_result_missing', `task_lifecycle_result_missing:${name}`, { tool: name, result });
  }
  return structured;
}

function buildTaskDefinition(row: JsonRecord, args: JsonRecord, idempotencyKey: string): JsonRecord {
  const feedbackId = String(row.feedback_id);
  const summary = String(row.summary);
  const title = (optionalString(args.task_title) ?? `Address feedback: ${summary}`).slice(0, 160);
  const details = optionalString(row.details);
  const priorNote = optionalString(row.resolution_note);
  return {
    title,
    goal: `Address feedback ${feedbackId} for ${String(row.surface_id)}: ${summary}`,
    context: [
      `Source feedback: ${feedbackId}`,
      `Surface: ${String(row.surface_id)}`,
      `Submitter site: ${String(row.submitter_site_id)}`,
      details ? `Details: ${details}` : null,
      priorNote ? `Prior resolution note: ${priorNote}` : null,
    ].filter(Boolean).join('\n'),
    required_work: [
      `Inspect the reported behavior for feedback ${feedbackId}.`,
      'Implement the smallest coherent fix within the owning MCP surface boundary.',
      'Add focused tests for the success and relevant failure paths.',
      'Record verification evidence through task-lifecycle before closeout.',
    ].join('\n'),
    non_goals: 'Do not execute the task from surface-feedback; task execution and lifecycle state remain owned by task-lifecycle and worker surfaces.',
    acceptance_criteria: [
      `The concern described by feedback ${feedbackId} is addressed or an exact blocker is recorded.`,
      'Focused tests cover the changed behavior and relevant error paths.',
      'The task lifecycle record contains truthful changed-file and verification evidence.',
    ],
    idempotency_key: idempotencyKey,
  };
}

function buildTaskConversionResult(
  status: 'converted' | 'recovered' | 'already_linked',
  row: JsonRecord,
  taskRef: string,
  taskNumber: number | null,
  taskStatus: string | null,
  taskId: string | null,
  payloadRef: string | null,
  handoff: JsonRecord | null,
  authoritySiteId: string | null,
): JsonRecord {
  const resolvedTaskNumber = taskNumber ?? taskNumberFromRef(taskRef);
  const nextAction = resolvedTaskNumber !== null
    ? {
      surface_id: 'task-lifecycle',
      tool: 'task_lifecycle_show',
      arguments: { task_number: resolvedTaskNumber },
      reason: 'Inspect the authoritative task lifecycle state; task execution remains outside surface-feedback.',
    }
    : {
      surface_id: 'task-lifecycle',
      tool: 'task_lifecycle_search',
      arguments: { query: taskId ?? taskRef, limit: 5 },
      reason: 'Resolve the task number from the authoritative task registry before using task_lifecycle_show.',
    };
  return {
    schema: 'narada.surface_feedback.convert_to_task.v1',
    status,
    feedback_id: String(row.feedback_id),
    task_ref: taskRef,
    task_number: resolvedTaskNumber,
    task_id: taskId,
    task_status: taskStatus,
    feedback: hydrateFeedback(row),
    task_creation: {
      status: status === 'already_linked' ? 'already_linked' : 'created_or_recovered',
      payload_ref: payloadRef,
      idempotency_key: handoff?.idempotency_key ?? `surface-feedback:${String(row.feedback_id)}`,
    },
    handoff_authorization: {
      scope: 'canonical_user_site_handoff',
      authorization_basis: 'canonical_feedback_store_and_server_binding',
      authority_site_id: authoritySiteId,
    },
    handoff: handoff ? {
      status: handoff.status,
      attempt_count: Number(handoff.attempt_count ?? 0),
      last_error_code: optionalString(handoff.last_error_code),
      updated_at: optionalString(handoff.updated_at),
    } : null,
    next_action: nextAction,
  };
}

function buildTaskHandoffReadback(row: JsonRecord | null): JsonRecord | null {
  if (!row) return null;
  return {
    status: String(row.status),
    idempotency_key: String(row.idempotency_key),
    payload_ref: optionalString(row.payload_ref),
    task_ref: optionalString(row.task_ref),
    task_number: numberValue(row.task_number),
    task_id: optionalString(row.task_id),
    task_status: optionalString(row.task_status),
    attempt_count: Number(row.attempt_count ?? 0),
    lease_expires_at: optionalString(row.lease_expires_at),
    last_error_code: optionalString(row.last_error_code),
    last_error_message: optionalString(row.last_error_message),
    updated_at: optionalString(row.updated_at),
  };
}

function recordFeedbackEvent(state: FeedbackState, event: JsonRecord): void {
  const createdAt = optionalString(event.created_at) ?? nowIso();
  state.db.prepare(
    'INSERT INTO feedback_events (event_id, feedback_id, event_type, actor_principal, status, task_ref, task_status, note, details_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)'
  ).run(
    `sfe_${randomUUID().slice(0, 16)}`,
    requiredString(event.feedback_id, 'feedback_event_requires_feedback_id'),
    requiredString(event.event_type, 'feedback_event_requires_type'),
    requiredString(event.actor_principal, 'feedback_event_requires_actor'),
    optionalString(event.status),
    optionalString(event.task_ref),
    optionalString(event.task_status),
    optionalString(event.note),
    JSON.stringify(asRecord(event.details)),
    createdAt,
  );
}

function withImmediateTransaction<T>(db: DatabaseSync, operation: () => T): T {
  db.exec('BEGIN IMMEDIATE');
  try {
    const result = operation();
    db.exec('COMMIT');
    return result;
  } catch (error) {
    db.exec('ROLLBACK');
    throw error;
  }
}

function feedbackEventHistory(state: FeedbackState, feedbackId: string): JsonRecord[] {
  const rows = state.db.prepare(
    'SELECT rowid AS event_order, * FROM feedback_events WHERE feedback_id = ? ORDER BY rowid ASC'
  ).all(feedbackId) as JsonRecord[];
  return rows.map((row) => ({
    event_id: String(row.event_id),
    event_order: Number(row.event_order),
    event_type: String(row.event_type),
    actor_principal: String(row.actor_principal),
    status: optionalString(row.status),
    task_ref: optionalString(row.task_ref),
    task_status: optionalString(row.task_status),
    note: optionalString(row.note),
    details: parseJsonRecord(row.details_json),
    created_at: String(row.created_at),
  }));
}

function parseJsonRecord(value: unknown): JsonRecord {
  try {
    return asRecord(JSON.parse(String(value ?? '{}')));
  } catch {
    return {};
  }
}

function resolveAuthority(options: JsonRecord): { siteId: string | null; principal: string | null; ownedSurfaceIds: string[]; source: AuthoritySource } {
  const configured = asRecord(options.authority);
  const siteId = optionalString(
    configured.site_id
      ?? options.authoritySiteId
      ?? options.siteId
      ?? process.env.NARADA_SITE_ID
  );
  const ownedSurfaceIds = stringList(
    configured.owned_surface_ids
      ?? options.authorityOwnedSurfaceIds
      ?? options.ownedSurfaceIds
      ?? process.env.NARADA_OWNED_SURFACE_IDS
  );
  const principal = optionalString(configured.principal ?? options.authorityPrincipal ?? process.env.NARADA_AGENT_ID)
    ?? (siteId ? `surface-feedback@${siteId}` : null);
  return {
    siteId,
    principal,
    ownedSurfaceIds,
    source: siteId ? 'server_config' : 'unconfigured',
  };
}

function stringList(value: unknown): string[] {
  if (Array.isArray(value)) return [...new Set(value.map((item) => String(item).trim()).filter(Boolean))];
  const text = optionalString(value);
  if (!text) return [];
  if (text.startsWith('[')) {
    try {
      const parsed = JSON.parse(text);
      if (Array.isArray(parsed)) return [...new Set(parsed.map((item) => String(item).trim()).filter(Boolean))];
    } catch {
      return [];
    }
  }
  return [...new Set(text.split(',').map((item) => item.trim()).filter(Boolean))];
}

function rejectClientAuthorityOverrides(args: JsonRecord): void {
  if (args.caller_site_id !== undefined || args.owned_surface_ids !== undefined || args.idempotency_key !== undefined) {
    throw diagnosticError('feedback_authority_must_be_server_bound', 'feedback_authority_must_be_server_bound', {
      forbidden_fields: ['caller_site_id', 'owned_surface_ids', 'idempotency_key'],
    });
  }
}

function taskHandoffCapability(state: FeedbackState): JsonRecord {
  const base = {
    operation: 'surface_feedback_convert_to_task',
    scope: 'canonical_user_site_handoff',
    authorization_basis: 'canonical_feedback_store_and_server_binding',
    authority_site_id: state.authoritySiteId,
  };
  if (!state.authoritySiteId || !state.authorityPrincipal) {
    return {
      ...base,
      available: false,
      reason_code: 'feedback_handoff_requires_server_authority',
      reason: 'Canonical User Site authority is not configured on this server.',
    };
  }
  if (!samePath(state.feedbackRoot, state.canonicalFeedbackRoot)) {
    return {
      ...base,
      available: false,
      reason_code: 'feedback_handoff_requires_canonical_store',
      reason: 'Task handoff is available only from the canonical feedback store.',
      feedback_root: state.feedbackRoot,
      canonical_feedback_root: state.canonicalFeedbackRoot,
    };
  }
  if (state.taskLifecycleClient) {
    const posture = taskLifecycleRootPosture(state);
    if (posture.configuration_valid !== true) {
      return {
        ...base,
        available: false,
        reason_code: 'feedback_task_lifecycle_root_invalid',
        reason: 'The configured task-lifecycle root is not a Site root containing .ai.',
        task_lifecycle_root: state.taskLifecycleRoot,
        task_lifecycle_root_source: state.taskLifecycleRootSource,
        diagnostics: posture,
      };
    }
  }
  return { ...base, available: true, reason_code: null, reason: null };
}

function assertTaskHandoffAuthority(state: FeedbackState): void {
  const capability = taskHandoffCapability(state);
  if (capability.available === true) return;
  const code = String(capability.reason_code ?? 'feedback_handoff_unavailable');
  throw diagnosticError(code, String(capability.reason ?? code), {
    ...capability,
    remediation: code === 'feedback_handoff_requires_canonical_store'
      ? `Configure --canonical-feedback-root ${state.canonicalFeedbackRoot} for the canonical User Site handoff store.`
      : code === 'feedback_handoff_requires_server_authority'
        ? 'Configure --site-id or NARADA_SITE_ID on the serving User Site projection.'
        : code === 'feedback_task_lifecycle_root_invalid'
          ? 'Configure --task-lifecycle-root or NARADA_TASK_LIFECYCLE_ROOT to a Site root containing .ai.'
          : undefined,
  });
}

function assertMutationAuthority(row: JsonRecord, state: FeedbackState): void {
  if (!state.authoritySiteId || !state.authorityPrincipal) {
    throw diagnosticError('feedback_authority_unconfigured', 'feedback_authority_unconfigured', {
      remediation: 'Configure NARADA_SITE_ID or --site-id; configure owned surfaces at server startup.',
    });
  }
  if (!isVisible(row, state.authoritySiteId, state.authorityOwnedSurfaceIds)) {
    throw diagnosticError('feedback_not_visible', `feedback_not_visible:${String(row.feedback_id)}`, {
      feedback_id: row.feedback_id,
      authority_site_id: state.authoritySiteId,
    });
  }
}

function mutationPrincipal(state: FeedbackState): string {
  if (!state.authorityPrincipal) {
    throw diagnosticError('feedback_authority_unconfigured', 'feedback_authority_unconfigured', {
      remediation: 'Configure NARADA_SITE_ID or --site-id and an optional NARADA_AGENT_ID authority principal.',
    });
  }
  return state.authorityPrincipal;
}

function taskLifecycleRootPosture(state: FeedbackState): JsonRecord {
  const rootExists = existsSync(state.taskLifecycleRoot);
  const aiDirectoryExists = existsSync(resolve(state.taskLifecycleRoot, '.ai'));
  return {
    configuration_valid: rootExists && aiDirectoryExists,
    root_exists: rootExists,
    ai_directory_exists: aiDirectoryExists,
  };
}

function requireTaskLifecycleRootReady(state: FeedbackState): void {
  if (!state.taskLifecycleClient) return;
  const posture = taskLifecycleRootPosture(state);
  if (posture.configuration_valid === true) return;
  throw diagnosticError('feedback_task_lifecycle_root_invalid', 'feedback_task_lifecycle_root_invalid', {
    task_lifecycle_root: state.taskLifecycleRoot,
    task_lifecycle_root_source: state.taskLifecycleRootSource,
    diagnostics: posture,
    remediation: 'Configure --task-lifecycle-root or NARADA_TASK_LIFECYCLE_ROOT to a Site root containing .ai.',
  });
}

function taskNumberFromRef(taskRef: string): number | null {
  const text = String(taskRef).trim();
  const taskMatch = /^task\s*#(\d+)$/i.exec(text);
  if (taskMatch) return Number(taskMatch[1]);
  return /^\d+$/.test(text) ? Number(text) : null;
}

function feedbackUpdateStatusBatch(args: JsonRecord, state: FeedbackState): JsonRecord {
  const resolvedBy = mutationPrincipal(state);
  const updates = arrayOfRecords(args.updates, 'feedback_batch_requires_updates');
  const succeeded: JsonRecord[] = [];
  const failed: JsonRecord[] = [];
  for (const [index, update] of updates.entries()) {
    try {
      const feedbackId = requiredString(update.feedback_id, 'feedback_requires_feedback_id', { index });
      const status = requiredString(update.status, 'feedback_requires_status', { feedback_id: feedbackId, index });
      if (!FEEDBACK_STATUSES.includes(status as typeof FEEDBACK_STATUSES[number])) throw diagnosticError('feedback_invalid_status', `feedback_invalid_status:${status}`, { feedback_id: feedbackId, index, allowed: FEEDBACK_STATUSES });
      const baseNote = requiredString(update.resolution_note, 'feedback_requires_resolution_note', { feedback_id: feedbackId, index });
      const taskRef = optionalString(update.task_ref);
      const taskStatus = optionalString(update.task_status);
      const resolutionNote = taskRef ? `${baseNote} Task: ${taskRef}` : baseNote;
      const existing = state.db.prepare('SELECT * FROM feedback_entries WHERE feedback_id = ?').get(feedbackId) as JsonRecord | undefined;
      if (!existing) throw feedbackNotFound(feedbackId, state);
      assertMutationAuthority(existing, state);
      const now = nowIso();
      withImmediateTransaction(state.db, () => {
        if (taskRef || taskStatus) {
          state.db.prepare('UPDATE feedback_entries SET status = ?, resolved_by = ?, resolution_note = ?, task_ref = COALESCE(?, task_ref), task_status = COALESCE(?, task_status), updated_at = ? WHERE feedback_id = ?').run(status, resolvedBy, resolutionNote, taskRef, taskStatus, now, feedbackId);
        } else {
          state.db.prepare('UPDATE feedback_entries SET status = ?, resolved_by = ?, resolution_note = ?, updated_at = ? WHERE feedback_id = ?').run(status, resolvedBy, resolutionNote, now, feedbackId);
        }
        recordFeedbackEvent(state, {
          feedback_id: feedbackId,
          event_type: 'status_updated',
          actor_principal: resolvedBy,
          status,
          task_ref: taskRef,
          task_status: taskStatus,
          note: resolutionNote,
          details: { previous_status: existing.status, batch_index: index },
          created_at: now,
        });
      });
      const updated = state.db.prepare('SELECT * FROM feedback_entries WHERE feedback_id = ?').get(feedbackId) as JsonRecord;
      succeeded.push({ feedback_id: feedbackId, status, task_ref: optionalString(updated.task_ref) ?? taskRef, task_status: optionalString(updated.task_status) ?? taskStatus, feedback: hydrateFeedback(updated) });
    } catch (error) {
      const diagnostic = errorDiagnostic(error);
      failed.push({ index, feedback_id: optionalString(update.feedback_id), code: diagnostic.code, message: diagnostic.message, details: diagnostic.details });
    }
  }
  return {
    schema: 'narada.surface_feedback.status_batch.v1',
    status: failed.length ? (succeeded.length ? 'partial' : 'failed') : 'updated',
    requested_count: updates.length,
    updated_count: succeeded.length,
    failed_count: failed.length,
    updates: succeeded,
    failures: failed,
  };
}

function feedbackImport(args: JsonRecord, state: FeedbackState): JsonRecord {
  const sourceDbPath = sourceFeedbackDbPath(args);
  if (samePath(sourceDbPath, state.dbPath)) {
    throw diagnosticError('feedback_import_same_store', 'feedback_import_same_store', { source_db_path: sourceDbPath, target_db_path: state.dbPath });
  }
  if (!existsSync(sourceDbPath)) {
    throw diagnosticError('feedback_import_source_missing', `feedback_import_source_missing:${sourceDbPath}`, { source_db_path: sourceDbPath });
  }
  const feedbackIds = feedbackIdList(args.feedback_ids);
  const imported: JsonRecord[] = [];
  const skipped: JsonRecord[] = [];
  const missing: string[] = [];
  const sourceDb = new DatabaseSync(sourceDbPath, { readOnly: true });
  try {
    const sourceRows = new Map<string, JsonRecord>();
    const selectSource = sourceDb.prepare('SELECT * FROM feedback_entries WHERE feedback_id = ?');
    for (const feedbackId of feedbackIds) {
      const row = selectSource.get(feedbackId) as JsonRecord | undefined;
      if (row) sourceRows.set(feedbackId, row);
      else missing.push(feedbackId);
    }
    const targetSelect = state.db.prepare('SELECT feedback_id FROM feedback_entries WHERE feedback_id = ?');
    const insert = state.db.prepare(
      'INSERT INTO feedback_entries (feedback_id, surface_id, submitter_site_id, submitter_principal, kind, summary, details, status, resolution_note, resolved_by, task_ref, task_status, source_db_path, source_updated_at, source_sync_mode, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)'
    );
    state.db.exec('BEGIN');
    try {
      for (const [feedbackId, row] of sourceRows.entries()) {
        const existing = targetSelect.get(feedbackId) as JsonRecord | undefined;
        if (existing) {
          skipped.push({ feedback_id: feedbackId, reason: 'already_exists' });
          continue;
        }
        insert.run(
          String(row.feedback_id),
          String(row.surface_id),
          String(row.submitter_site_id),
          String(row.submitter_principal),
          String(row.kind),
          String(row.summary),
          String(row.details ?? ''),
          String(row.status ?? 'submitted'),
          optionalString(row.resolution_note),
          optionalString(row.resolved_by),
          optionalString(row.task_ref) ?? legacyTaskRef(row.resolution_note),
          optionalString(row.task_status),
          sourceDbPath,
          String(row.updated_at),
          'explicit_import',
          String(row.created_at),
          String(row.updated_at)
        );
        imported.push(hydrateFeedback({ ...row, source_db_path: sourceDbPath, source_updated_at: String(row.updated_at), source_sync_mode: 'explicit_import' }));
      }
      state.db.exec('COMMIT');
    } catch (error) {
      state.db.exec('ROLLBACK');
      throw error;
    }
  } finally {
    sourceDb.close();
  }
  return {
    schema: 'narada.surface_feedback.import.v1',
    status: missing.length || skipped.length ? 'partial' : 'imported',
    store: storeIdentity(state),
    source_db_path: sourceDbPath,
    target_db_path: state.dbPath,
    requested_count: feedbackIds.length,
    imported_count: imported.length,
    skipped_count: skipped.length,
    missing_count: missing.length,
    imported,
    skipped,
    missing_feedback_ids: missing,
  };
}

function feedbackReadScope(args: JsonRecord): FeedbackReadScope {
  const legacyFields = ['caller_site_id', 'owned_surface_ids'].filter((field) => args[field] !== undefined);
  if (legacyFields.length > 0) {
    throw diagnosticError('feedback_read_scope_server_bound', 'feedback_read_scope_server_bound', {
      forbidden_fields: legacyFields,
      required_field: 'scope',
      allowed_scopes: [...FEEDBACK_READ_SCOPES],
      remediation: 'Use the required explicit scope: all_authorized, store_reconciliation, authority_visible, owned_surfaces, or authority_site_submissions. Site identity and owned surfaces are bound by the server.',
    });
  }
  if (args.submitter_site_id !== undefined) {
    throw diagnosticError('feedback_read_filter_renamed', 'feedback_read_filter_renamed', {
      forbidden_field: 'submitter_site_id',
      replacement_field: 'submitter_site_id_filter',
      remediation: 'Use submitter_site_id_filter for an explicit declared-metadata filter; it never changes authorization.',
    });
  }
  if (args.scope === undefined || args.scope === null || (typeof args.scope === 'string' && !args.scope.trim())) {
    throw diagnosticError('feedback_read_scope_required', 'feedback_read_scope_required', {
      allowed_scopes: [...FEEDBACK_READ_SCOPES],
      remediation: 'Provide one explicit read scope; do not rely on an implicit default.',
    });
  }
  if (typeof args.scope !== 'string') {
    throw diagnosticError('feedback_invalid_read_scope', 'feedback_invalid_read_scope', {
      scope: args.scope,
      allowed_scopes: [...FEEDBACK_READ_SCOPES],
    });
  }
  const raw = args.scope.trim();
  if (!FEEDBACK_READ_SCOPES.includes(raw as FeedbackReadScope)) {
    throw diagnosticError('feedback_invalid_read_scope', `feedback_invalid_read_scope:${raw}`, {
      scope: raw,
      allowed_scopes: [...FEEDBACK_READ_SCOPES],
    });
  }
  return raw as FeedbackReadScope;
}

function feedbackReadScopeQuery(args: JsonRecord, state: FeedbackState): { sql: string; params: string[]; read_scope: JsonRecord } {
  const scope = feedbackReadScope(args);
  if (scope === 'all_authorized' || scope === 'store_reconciliation') {
    if (!samePath(state.feedbackRoot, state.canonicalFeedbackRoot)) {
      throw diagnosticError('feedback_global_read_requires_canonical_store', 'feedback_global_read_requires_canonical_store', {
        scope,
        feedback_root: state.feedbackRoot,
        canonical_feedback_root: state.canonicalFeedbackRoot,
        storage_posture: 'noncanonical_feedback_root',
        remediation: `Configure --canonical-feedback-root ${state.canonicalFeedbackRoot} for the canonical cross-site feedback store.`,
      });
    }
    if (!state.authoritySiteId) {
      throw diagnosticError('feedback_global_read_requires_server_authority', 'feedback_global_read_requires_server_authority', {
        scope,
        remediation: 'Configure --site-id or NARADA_SITE_ID on the serving User Site projection.',
      });
    }
    return {
      sql: '',
      params: [],
      read_scope: {
        mode: scope,
        scope_limited: false,
        authorization_basis: scope === 'store_reconciliation'
          ? 'canonical_feedback_store_reconciliation_and_server_binding'
          : 'canonical_feedback_store_and_server_binding',
        authority_site_id: state.authoritySiteId,
        reconciliation_read: scope === 'store_reconciliation',
        submitter_site_id_semantics: 'declared_metadata_not_authenticated_provenance',
        federated_entries_included: true,
        federation_materialization: 'bounded_startup_sync',
        mutation_authority_unchanged: true,
      },
    };
  }
  if (!state.authoritySiteId) {
    throw diagnosticError('feedback_read_scope_requires_server_authority', 'feedback_read_scope_requires_server_authority', {
      scope,
      remediation: 'Configure NARADA_SITE_ID or --site-id before using an authority-bound read scope.',
    });
  }
  if (scope === 'authority_site_submissions') {
    return {
      sql: ' AND submitter_site_id = ?',
      params: [state.authoritySiteId],
      read_scope: {
        mode: scope,
        scope_limited: true,
        authorization_basis: 'server_bound_site_authority_metadata_filter',
        authority_site_id: state.authoritySiteId,
        declared_submitter_site_id: state.authoritySiteId,
        metadata_only: true,
        provenance_authenticated: false,
      },
    };
  }
  const ownedSurfaceIds = state.authorityOwnedSurfaceIds;
  if (scope === 'owned_surfaces') {
    const readScope = {
      mode: scope,
      scope_limited: true,
      authorization_basis: 'server_bound_surface_ownership',
      authority_site_id: state.authoritySiteId,
      surface_ids: [...ownedSurfaceIds],
    };
    if (ownedSurfaceIds.length === 0) return { sql: ' AND 0 = 1', params: [], read_scope: readScope };
    const placeholders = ownedSurfaceIds.map(() => '?').join(', ');
    return { sql: ` AND surface_id IN (${placeholders})`, params: ownedSurfaceIds, read_scope: readScope };
  }
  const readScope = {
    mode: scope,
    scope_limited: true,
    authorization_basis: 'server_bound_authority_visibility',
    authority_site_id: state.authoritySiteId,
    surface_ids: [...ownedSurfaceIds],
    declared_submitter_metadata: true,
    provenance_authenticated: false,
  };
  if (ownedSurfaceIds.length === 0) {
    return { sql: ' AND submitter_site_id = ?', params: [state.authoritySiteId], read_scope: readScope };
  }
  const placeholders = ownedSurfaceIds.map(() => '?').join(', ');
  return {
    sql: ` AND (submitter_site_id = ? OR surface_id IN (${placeholders}))`,
    params: [state.authoritySiteId, ...ownedSurfaceIds],
    read_scope: readScope,
  };
}

function feedbackList(args: JsonRecord, state: FeedbackState): JsonRecord {
  const limit = clamp(integer(args.limit, 50, 1, 200), 1, 200);
  const offset = Math.max(0, integer(args.offset, 0, 0, 10000));
  const surfaceId = optionalString(args.surface_id);
  const siteId = optionalString(args.submitter_site_id_filter);
  const kind = optionalString(args.kind);
  const status = optionalString(args.status);
  const since = optionalString(args.since);
  const until = optionalString(args.until);
  const readScope = feedbackReadScopeQuery(args, state);
  let sql = 'SELECT * FROM feedback_entries WHERE 1=1';
  const params: (string | number)[] = [];
  if (surfaceId) { sql += ' AND surface_id = ?'; params.push(surfaceId); }
  if (siteId) { sql += ' AND submitter_site_id = ?'; params.push(siteId); }
  if (kind) { sql += ' AND kind = ?'; params.push(kind); }
  if (status) { sql += ' AND status = ?'; params.push(status); }
  if (since) { sql += ' AND created_at >= ?'; params.push(since); }
  if (until) { sql += ' AND created_at <= ?'; params.push(until); }
  sql += readScope.sql;
  params.push(...readScope.params);
  sql += ' ORDER BY created_at DESC LIMIT ? OFFSET ?';
  params.push(limit, offset);
  const rows = state.db.prepare(sql).all(...params) as JsonRecord[];
  return {
    store: storeIdentity(state),
    read_scope: readScope.read_scope,
    items: rows.map((row) => ({ ...hydrateFeedback(row), task_handoff_capability: taskHandoffCapability(state) })),
    count: rows.length,
    limit,
    offset,
  };
}

function feedbackActionableQueue(args: JsonRecord, state: FeedbackState): JsonRecord {
  const limit = clamp(integer(args.limit, 50, 1, 200), 1, 200);
  const offset = Math.max(0, integer(args.offset, 0, 0, 10000));
  const surfaceId = optionalString(args.surface_id);
  const siteId = optionalString(args.submitter_site_id_filter);
  const kind = optionalString(args.kind);
  const since = optionalString(args.since);
  const until = optionalString(args.until);
  const readScope = feedbackReadScopeQuery(args, state);
  const statusPlaceholders = UNPROCESSED_FEEDBACK_STATUSES.map(() => '?').join(', ');
  let baseWhere = 'FROM feedback_entries WHERE 1=1';
  const filterParams: (string | number)[] = [];
  if (surfaceId) { baseWhere += ' AND surface_id = ?'; filterParams.push(surfaceId); }
  if (siteId) { baseWhere += ' AND submitter_site_id = ?'; filterParams.push(siteId); }
  if (kind) { baseWhere += ' AND kind = ?'; filterParams.push(kind); }
  if (since) { baseWhere += ' AND created_at >= ?'; filterParams.push(since); }
  if (until) { baseWhere += ' AND created_at <= ?'; filterParams.push(until); }
  baseWhere += readScope.sql;
  filterParams.push(...readScope.params);
  const includedWhere = `${baseWhere} AND status IN (${statusPlaceholders})`;
  const includedParams = [...filterParams, ...UNPROCESSED_FEEDBACK_STATUSES];
  const countRow = state.db.prepare(`SELECT COUNT(*) AS total ${includedWhere}`).get(...includedParams) as JsonRecord;
  const rows = state.db.prepare(`SELECT * ${includedWhere} ORDER BY updated_at DESC, created_at DESC, feedback_id ASC LIMIT ? OFFSET ?`).all(...includedParams, limit, offset) as JsonRecord[];
  const excludedRows = state.db.prepare(`SELECT status, COUNT(*) AS total ${baseWhere} AND status NOT IN (${statusPlaceholders}) GROUP BY status ORDER BY status`).all(...filterParams, ...UNPROCESSED_FEEDBACK_STATUSES) as JsonRecord[];
  const excludedStatusCounts = Object.fromEntries(excludedRows.map((row) => [String(row.status), Number(row.total ?? 0)]));
  const totalCount = Number(countRow.total ?? 0);
  const items = rows.map((row) => {
    const feedback = hydrateFeedback(row);
    const taskRef = optionalString(feedback.task_ref);
    const taskStatus = optionalString(feedback.task_status);
    return {
      ...feedback,
      task_handoff_capability: taskHandoffCapability(state),
      actionability: 'feedback_action_required',
      task_link: taskRef ? {
        task_ref: taskRef,
        lifecycle_state: taskStatus,
        lifecycle_state_source: taskStatus ? 'feedback_projection' : 'unavailable',
      } : {
        task_ref: null,
        lifecycle_state: null,
        lifecycle_state_source: 'unlinked',
      },
    };
  });
  const nextOffset = offset + items.length < totalCount ? offset + items.length : null;
  return {
    schema: 'narada.surface_feedback.actionable_queue.v1',
    status: 'ok',
    store: storeIdentity(state),
    read_scope: readScope.read_scope,
    actionable_statuses: [...UNPROCESSED_FEEDBACK_STATUSES],
    queue_selection: {
      mode: 'unprocessed',
      included_statuses: [...UNPROCESSED_FEEDBACK_STATUSES],
      filtering_stage: 'before_pagination',
    },
    excluded_status_counts: excludedStatusCounts,
    items,
    count: items.length,
    total_count: totalCount,
    limit,
    offset,
    has_more: nextOffset !== null,
    next_offset: nextOffset,
  };
}

function feedbackShow(args: JsonRecord, state: FeedbackState): JsonRecord {
  const feedbackId = requiredString(args.feedback_id, 'feedback_requires_feedback_id');
  const readScope = feedbackReadScopeQuery(args, state);
  const row = state.db.prepare(`SELECT * FROM feedback_entries WHERE feedback_id = ?${readScope.sql}`).get(feedbackId, ...readScope.params) as JsonRecord | undefined;
  if (!row) throw feedbackNotFound(feedbackId, state);
  return {
    ...hydrateFeedback(row),
    read_scope: readScope.read_scope,
    audit_events: feedbackEventHistory(state, feedbackId),
    task_handoff: buildTaskHandoffReadback(readFeedbackTaskHandoff(state, feedbackId)),
    task_handoff_capability: taskHandoffCapability(state),
    store: storeIdentity(state),
  };
}

function feedbackStats(args: JsonRecord, state: FeedbackState): JsonRecord {
  const surfaceId = optionalString(args.surface_id);
  const readScope = feedbackReadScopeQuery(args, state);
  const bySurface: Record<string, number> = {};
  const byKind: Record<string, number> = {};
  const byStatus: Record<string, number> = {};
  let sql = 'SELECT surface_id, kind, status FROM feedback_entries WHERE 1=1';
  const params: string[] = [];
  if (surfaceId) { sql += ' AND surface_id = ?'; params.push(surfaceId); }
  sql += readScope.sql;
  params.push(...readScope.params);
  const rows = state.db.prepare(sql).all(...params) as JsonRecord[];
  for (const row of rows) {
    const s = String(row.surface_id);
    const k = String(row.kind);
    const st = String(row.status);
    bySurface[s] = (bySurface[s] ?? 0) + 1;
    byKind[k] = (byKind[k] ?? 0) + 1;
    byStatus[st] = (byStatus[st] ?? 0) + 1;
  }
  return { store: storeIdentity(state), read_scope: readScope.read_scope, by_surface: bySurface, by_kind: byKind, by_status: byStatus, total: rows.length };
}

function isVisible(row: JsonRecord, callerSiteId: string | null, ownedSurfaceIds: string[]): boolean {
  if (!callerSiteId) return true;
  if (String(row.submitter_site_id) === callerSiteId) return true;
  if (ownedSurfaceIds.includes(String(row.surface_id))) return true;
  return false;
}

function feedbackNotFound(feedbackId: string, state: FeedbackState) {
  return diagnosticError('feedback_not_found', `feedback_not_found:${feedbackId}`, {
    feedback_id: feedbackId,
    feedback_root: state.feedbackRoot,
    db_path: state.dbPath,
    store_hint: 'Feedback IDs are local to the configured feedback_root. If this ID was created from another site/session, compare surface_feedback_doctor.db_path for both sessions and migrate/rebind stale site-local feedback roots to the shared feedback root.',
  });
}

function storeIdentity(state: FeedbackState): JsonRecord {
  const usesCanonicalStore = samePath(state.feedbackRoot, state.canonicalFeedbackRoot);
  return {
    feedback_root: state.feedbackRoot,
    canonical_feedback_root: state.canonicalFeedbackRoot,
    uses_canonical_store: usesCanonicalStore,
    storage_posture: usesCanonicalStore ? 'canonical_feedback_root' : 'noncanonical_feedback_root',
    db_path: state.dbPath,
    federation_enabled: state.feedbackDiscoveryRoots.length > 0,
    federated_source_db_count: state.feedbackSourceSync.filter((source) => source.status !== 'missing').length,
  };
}

function sourceFeedbackDbPath(args: JsonRecord): string {
  const sourceRoot = optionalString(args.source_feedback_root);
  const sourceDbPath = optionalString(args.source_db_path);
  if (sourceRoot && sourceDbPath) throw diagnosticError('feedback_import_source_ambiguous', 'feedback_import_source_ambiguous');
  if (sourceRoot) return resolve(sourceRoot, '.feedback', 'surface-feedback.db');
  if (sourceDbPath) return resolve(sourceDbPath);
  throw diagnosticError('feedback_import_requires_source', 'feedback_import_requires_source');
}

function feedbackIdList(value: unknown): string[] {
  if (!Array.isArray(value)) throw diagnosticError('feedback_import_requires_feedback_ids', 'feedback_import_requires_feedback_ids');
  const ids = [...new Set(value.map((v) => String(v ?? '').trim()).filter(Boolean))];
  if (!ids.length) throw diagnosticError('feedback_import_requires_feedback_ids', 'feedback_import_requires_feedback_ids');
  return ids;
}

function ensureColumn(db: DatabaseSync, table: string, column: string, definition: string): void {
  const rows = db.prepare(`PRAGMA table_info(${table})`).all() as JsonRecord[];
  if (rows.some((row) => String(row.name) === column)) return;
  db.exec(`ALTER TABLE ${table} ADD COLUMN ${column} ${definition}`);
}

function hydrateFeedback(row: JsonRecord): JsonRecord {
  return {
    feedback_id: String(row.feedback_id),
    surface_id: String(row.surface_id),
    submitter_site_id: String(row.submitter_site_id),
    submitter_site_metadata: {
      declared: true,
      authenticated_provenance: false,
    },
    submitter_principal: String(row.submitter_principal),
    kind: String(row.kind),
    summary: String(row.summary),
    details: String(row.details),
    status: String(row.status),
    resolution_note: optionalString(row.resolution_note),
    resolved_by: optionalString(row.resolved_by),
    task_ref: optionalString(row.task_ref) ?? legacyTaskRef(row.resolution_note),
    task_status: optionalString(row.task_status),
    source: {
      kind: optionalString(row.source_db_path) ? 'federated_site_store' : 'canonical_store',
      db_path: optionalString(row.source_db_path),
      source_updated_at: optionalString(row.source_updated_at),
      sync_mode: optionalString(row.source_sync_mode) ?? (optionalString(row.source_db_path) ? 'startup_materialized' : 'canonical'),
    },
    created_at: String(row.created_at),
    updated_at: String(row.updated_at),
  };
}

function renderResult(result: JsonRecord): string {
  if (result.by_surface !== undefined) {
    const parts: string[] = [];
    const bySurface = result.by_surface as Record<string, number>;
    const byKind = result.by_kind as Record<string, number>;
    const byStatus = result.by_status as Record<string, number>;
    parts.push(`feedback stats: ${result.total} total`);
    for (const [s, c] of Object.entries(bySurface)) parts.push(`  surface ${s}: ${c}`);
    for (const [k, c] of Object.entries(byKind)) parts.push(`  kind ${k}: ${c}`);
    for (const [s, c] of Object.entries(byStatus)) parts.push(`  status ${s}: ${c}`);
    return parts.join('\n');
  }
  if (result.schema === 'narada.surface_feedback.doctor.v1') return compactLines([
    `surface_feedback_doctor: ${result.status}`,
    `storage_posture: ${result.storage_posture}`,
    `feedback_root: ${result.feedback_root}`,
    `canonical_feedback_root: ${result.canonical_feedback_root}`,
    `db_path: ${result.db_path}`,
    `task_lifecycle_root: ${result.task_lifecycle_root}`,
    `task_lifecycle_root_source: ${result.task_lifecycle_root_source}`,
    `task_lifecycle_root_configured: ${result.task_lifecycle_root_configured}`,
    `task_lifecycle_health: ${result.task_lifecycle_health}`,
    `task_lifecycle_integration: ${result.task_lifecycle_integration}`,
    `authority_configured: ${asRecord(result.authority).configured ?? false}`,
    `authority_site_id: ${asRecord(result.authority).site_id ?? 'unconfigured'}`,
    `authority_principal: ${asRecord(result.authority).principal ?? 'unconfigured'}`,
    `task_handoff_available: ${asRecord(result.task_handoff_capability).available ?? false}`,
    ...(asRecord(result.task_handoff_capability).reason_code ? [`task_handoff_reason: ${asRecord(result.task_handoff_capability).reason_code}`] : []),
    `total_feedback_entries: ${result.total_feedback_entries}`,
    `federation_enabled: ${asRecord(result.federation).enabled ?? false}`,
    `federated_source_db_count: ${asRecord(result.federation).source_db_count ?? 0}`,
    `federated_conflict_count: ${asRecord(result.federation).conflict_count ?? 0}`,
    ...(Array.isArray(result.diagnostics) ? result.diagnostics.map((item) => `diagnostic: ${item}`) : []),
    ...(Array.isArray(result.remediation) ? result.remediation.map((item) => `remediation: ${item}`) : []),
  ]);
  if (result.schema === 'narada.surface_feedback.actionable_queue.v1') return compactLines([
    `actionable feedback: ${result.count ?? 0} of ${result.total_count ?? 0}`,
    `has_more: ${result.has_more ?? false}`,
    ...(result.next_offset !== null && result.next_offset !== undefined ? [`next_offset: ${result.next_offset}`] : []),
    ...((result.items as JsonRecord[]).map((item) => {
      const link = item.task_link as JsonRecord;
      return `  ${item.feedback_id} [${item.status}] ${String(item.summary ?? '').slice(0, 80)}${link?.task_ref ? ` -> ${link.task_ref} (${link.lifecycle_state ?? 'state unavailable'})` : ''}`;
    })),
  ]);
  if (result.schema === 'narada.surface_feedback.status_batch.v1') {
    const updates = Array.isArray(result.updates) ? result.updates.map(asRecord) : [];
    const failures = Array.isArray(result.failures) ? result.failures.map(asRecord) : [];
    return compactLines([
      `feedback status batch: ${result.status}`,
      `updated: ${result.updated_count ?? updates.length} of ${result.requested_count ?? updates.length + failures.length}`,
      `failed: ${result.failed_count ?? failures.length}`,
      ...updates.map((update) => `  ${update.feedback_id} [${update.status}]${update.task_ref ? ` -> ${update.task_ref} (${update.task_status ?? 'state unavailable'})` : ''}`),
      ...failures.map((failure) => `  ${failure.feedback_id ?? 'unknown feedback'} failed: ${failure.code ?? 'unknown_error'}`),
    ]);
  }
  if (result.schema === 'narada.surface_feedback.convert_to_task.v1') {
    const feedback = asRecord(result.feedback);
    return compactLines([
      `feedback task handoff: ${result.status}`,
      `feedback_id: ${result.feedback_id}`,
      ...(feedback.summary ? [`feedback_summary: ${feedback.summary}`] : []),
      `task_ref: ${result.task_ref}`,
      `task_number: ${result.task_number}`,
      `task_status: ${result.task_status}`,
      `next_action: ${asRecord(result.next_action).tool ?? 'task_lifecycle_show'}`,
    ]);
  }
  if (result.items !== undefined) {
    const items = result.items as JsonRecord[];
    return [`feedback: ${result.count ?? 0} entries`, ...items.map((i) => `  ${i.feedback_id} [${i.kind}] ${String(i.summary ?? '').slice(0, 80)} (${i.surface_id} <- ${i.submitter_site_id})`)].join('\n');
  }
  if (result.feedback_id) return `feedback: ${result.feedback_id} [${result.kind}] ${result.summary} (${result.surface_id} <- ${result.submitter_site_id})`;
  return `${result.status ?? 'ok'}: ${result.feedback_id ?? ''}`;
}

function requiredString(value: unknown, code: string, details: JsonRecord = {}): string {
  const text = String(value ?? '').trim();
  if (!text) throw diagnosticError(code, code, details);
  return text;
}

function optionalString(value: unknown): string | null {
  const text = String(value ?? '').trim();
  return text || null;
}

function numberValue(value: unknown): number | null {
  const parsed = typeof value === 'number' ? value : Number(String(value ?? '').trim());
  return Number.isInteger(parsed) && parsed > 0 ? parsed : null;
}

function integer(value: unknown, fallback: number, min: number, max: number): number {
  const parsed = Number(value ?? fallback);
  return Number.isFinite(parsed) ? Math.max(min, Math.min(max, Math.trunc(parsed))) : fallback;
}

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Math.trunc(value)));
}

function asRecord(value: unknown): JsonRecord {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonRecord : {};
}

function arrayOfRecords(value: unknown, code: string): JsonRecord[] {
  if (!Array.isArray(value) || value.length === 0) throw diagnosticError(code, code);
  return value.map(asRecord);
}

function samePath(a: string, b: string): boolean {
  const left = resolve(a);
  const right = resolve(b);
  return process.platform === 'win32' ? left.toLowerCase() === right.toLowerCase() : left === right;
}

function compactLines(lines: Array<string | null | undefined>): string {
  return lines.filter((line): line is string => Boolean(line)).join('\n');
}

function nowIso(): string {
  return new Date().toISOString();
}

function diagnosticError(code: string, message: string = code, details: JsonRecord = {}) {
  const error = new Error(message);
  Object.assign(error, { codeName: code, details });
  return error;
}

function errorDiagnostic(error: unknown) {
  const record = asRecord(error);
  const details = asRecord(record.details);
  return { schema: 'narada.surface_feedback.error.v1', code: String(record.codeName ?? 'surface_feedback_error'), message: error instanceof Error ? error.message : String(error), ...details, details };
}

function drainJsonLines(buffer: string) {
  const lines = buffer.split(/\r?\n/);
  const remaining = lines.pop() ?? '';
  return { framed: false, remaining, requests: lines.filter((line) => line.trim()).map((line) => asRecord(JSON.parse(line))) };
}

function drainJsonRpcFrames(buffer: string) {
  const requests: JsonRecord[] = [];
  let remaining = buffer;
  while (true) {
    const headerEnd = remaining.indexOf('\r\n\r\n');
    if (headerEnd < 0) break;
    const header = remaining.slice(0, headerEnd);
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) break;
    const length = Number(match[1]);
    const bodyStart = headerEnd + 4;
    const bodyEnd = bodyStart + length;
    if (remaining.length < bodyEnd) break;
    requests.push(asRecord(JSON.parse(remaining.slice(bodyStart, bodyEnd))));
    remaining = remaining.slice(bodyEnd);
  }
  return { framed: true, remaining, requests };
}

function writeJsonRpcResponse(response: JsonRecord, { framed }: { framed: boolean }) {
  const body = JSON.stringify(response);
  if (framed) process.stdout.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\r\n\r\n${body}`);
  else process.stdout.write(`${body}\n`);
}

function parseArgs(argv: string[]) {
  const options: JsonRecord = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--feedback-root') options.feedbackRoot = argv[++i];
    else if (arg === '--output-root') options.outputRoot = argv[++i];
    else if (arg === '--canonical-feedback-root') options.canonicalFeedbackRoot = argv[++i];
    else if (arg === '--task-lifecycle-root') options.taskLifecycleRoot = argv[++i];
    else if (arg === '--feedback-discovery-root') {
      const roots = Array.isArray(options.feedbackDiscoveryRoots) ? options.feedbackDiscoveryRoots as unknown[] : [];
      options.feedbackDiscoveryRoots = [...roots, argv[++i]];
    }
    else if (arg === '--site-id') options.authoritySiteId = argv[++i];
    else if (arg === '--owned-surface-id') {
      const owned = Array.isArray(options.authorityOwnedSurfaceIds) ? options.authorityOwnedSurfaceIds as unknown[] : [];
      options.authorityOwnedSurfaceIds = [...owned, argv[++i]];
    }
  }
  return options;
}

export { parseArgs };

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runStdioServer(parseArgs(process.argv.slice(2))).catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  });
}
