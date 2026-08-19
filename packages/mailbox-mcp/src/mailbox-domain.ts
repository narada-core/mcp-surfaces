import { createHash, randomUUID } from 'node:crypto';
import { existsSync } from 'node:fs';
import { mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import {
  loadControlPlaneRuntime,
  buildDelegatedGraphTokenProvider,
  type ControlPlaneRuntime,
  type Fact,
  type FactStore,
  type ScopeConfig,
  type Source,
  type SourceBatch,
  type SourceRecord,
} from './control-plane-runtime.js';
import {
  MailboxDomainStore,
  sha256,
  stableId,
  type FirstObservationCandidate,
  type GenerationRecordRow,
  type JsonRecord,
  type StagedGenerationRecord,
  type SyncGenerationRow,
} from './mailbox-domain-store.js';

const DOMAIN_SCHEMA = 'narada.domain_operation.v1';
const GENERATION_ARTIFACT_SCHEMA = 'narada.mailbox.sync_generation_artifact.v1';
const DEFAULT_CONFIG_PATH = 'config/config.json';
const DOMAIN_RELATIVE_ROOT = join('.narada', 'runtime', 'mailbox-domain');
const MAX_IDEMPOTENCY_KEY = 512;
const MAX_SCOPE_ID = 256;
const MAX_EXPLICIT_FACT_PAYLOAD_BYTES = 750 * 1024;

export interface MailboxDomainServiceOptions {
  sourceFactory?: (scope: ScopeConfig) => Source;
  now?: () => string;
  faultInjector?: (point: 'after_batch_staged' | 'after_runner', generationId: string) => void | Promise<void>;
  runtime?: ControlPlaneRuntime;
  controlPlaneRoot?: string;
}

function requiredUniqueStrings(value: unknown, code: string, maxItems: number, maxLength: number): string[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > maxItems) throw new Error(code);
  const normalized = value.map((item) => requiredBoundedString(item, code, maxLength));
  const unique = [...new Set(normalized)].sort();
  if (unique.length !== normalized.length) throw new Error(`${code}_duplicate`);
  return unique;
}

export class MailboxDomainService {
  private readonly siteRoot: string;
  private readonly options: MailboxDomainServiceOptions;

  constructor(siteRoot: string, options: MailboxDomainServiceOptions = {}) {
    this.siteRoot = resolve(siteRoot);
    this.options = options;
  }

  messageFactFind(args: JsonRecord): JsonRecord {
    const scopeId = requiredBoundedString(args.scope_id, 'mailbox_fact_find_scope_id_required', MAX_SCOPE_ID);
    const messageId = requiredBoundedString(args.message_id, 'mailbox_fact_find_message_id_required', 1024);
    const store = this.openStore();
    try {
      const observation = store.observationByMessage(scopeId, messageId);
      return observation
        ? { schema: 'narada.mailbox.message_fact_lookup.v1', status: 'ok', scope_id: scopeId, message_id: messageId, observation }
        : { schema: 'narada.mailbox.message_fact_lookup.v1', status: 'not_found', scope_id: scopeId, message_id: messageId };
    } finally {
      store.close();
    }
  }

  admissionShow(args: JsonRecord): JsonRecord {
    const scopeId = requiredBoundedString(args.scope_id, 'mailbox_admission_scope_id_required', MAX_SCOPE_ID);
    const factId = requiredBoundedString(args.fact_id, 'mailbox_admission_fact_id_required', 256);
    const store = this.openStore();
    try {
      const admission = store.admissionByFact(scopeId, factId);
      return admission
        ? { schema: 'narada.mailbox.admission_show.v1', status: 'ok', scope_id: scopeId, fact_id: factId, admission }
        : { schema: 'narada.mailbox.admission_show.v1', status: 'not_found', scope_id: scopeId, fact_id: factId };
    } finally {
      store.close();
    }
  }

  async syncGeneration(args: JsonRecord): Promise<JsonRecord> {
    const kernel = await this.runtime();
    const idempotencyKey = requiredBoundedString(args.idempotency_key, 'mailbox_sync_idempotency_key_required', MAX_IDEMPOTENCY_KEY);
    const loaded = await this.loadScope(args, kernel);
    const configFingerprint = syncConfigFingerprint(loaded.scope);
    const requestFingerprint = fingerprint({
      schema: 'narada.mailbox.sync_generation_request.v1',
      scope_id: loaded.scope.scope_id,
      config_fingerprint: configFingerprint,
    });
    const generationId = stableId('mbg_', idempotencyKey);
    const store = this.openStore();
    const now = this.now();
    const claim = store.claimGeneration({
      generation_id: generationId,
      idempotency_key: idempotencyKey,
      request_fingerprint: requestFingerprint,
      scope_id: loaded.scope.scope_id,
      config_fingerprint: configFingerprint,
      now,
    });
    if (claim.generation.status === 'completed') {
      store.close();
      return generationOperation(claim.generation, true);
    }
    if (claim.generation.status === 'failed') {
      store.close();
      return blockedGenerationOperation(claim.generation, true);
    }
    const leaseToken = requiredBoundedString(claim.lease_token, 'mailbox_sync_lease_missing', 128);
    let leaseReleased = false;
    let heartbeatError: Error | null = null;
    const heartbeat = setInterval(() => {
      try {
        store.renewLease(loaded.scope.scope_id, generationId, leaseToken, this.now());
      } catch (error) {
        heartbeatError = asError(error);
      }
    }, 10_000);
    heartbeat.unref();

    const assertLease = (): void => {
      if (heartbeatError) throw heartbeatError;
      store.assertLease(loaded.scope.scope_id, generationId, leaseToken, this.now());
    };

    let factStore: FactStore | null = null;
    try {
      const cursorStore = new kernel.FileCursorStore({ rootDir: loaded.scope.root_dir, scopeId: loaded.scope.scope_id });
      const currentCursor = await cursorStore.read();
      let generation = store.requireGeneration(generationId);
      if (generation.status === 'staged') {
        await readGenerationArtifact(generation, this.generationArtifactPath(generationId));
        if (generation.next_cursor !== null && currentCursor === generation.next_cursor) {
          assertLease();
          store.reconcileApplicationAfterCursorCommit(generationId);
          generation = store.finalizeGeneration(generationId, leaseToken, this.now());
          leaseReleased = true;
          return generationOperation(generation, true);
        }
        if (generation.next_cursor === null && generationReady(store.generationRecords(generationId))) {
          generation = store.finalizeGeneration(generationId, leaseToken, this.now());
          leaseReleased = true;
          return generationOperation(generation, true);
        }
        if (currentCursor !== generation.parent_cursor) {
          throw new Error(`mailbox_sync_cursor_conflict:${generationId}`);
        }
      }

      const factDbDir = join(loaded.scope.root_dir, '.narada');
      await mkdir(factDbDir, { recursive: true });
      const factDb = new kernel.Database(join(factDbDir, 'facts.db'));
      factDb.pragma('journal_mode = WAL');
      factStore = new kernel.SqliteFactStore({ db: factDb });
      factStore.initSchema();

      const source = new GenerationSource({
        generationId,
        leaseToken,
        store,
        source: this.options.sourceFactory?.(loaded.scope) ?? createGraphSource(kernel, loaded.scope, this.siteRoot),
        sourceRecordToFact: kernel.sourceRecordToFact,
        artifactPath: this.generationArtifactPath(generationId),
        assertLease,
        now: () => this.now(),
        afterStaged: async () => {
          await this.options.faultInjector?.('after_batch_staged', generationId);
        },
      });
      const applyLog = new kernel.FileApplyLogStore({ rootDir: loaded.scope.root_dir });
      const projector = new kernel.DefaultProjector({
        rootDir: loaded.scope.root_dir,
        tombstonesEnabled: loaded.scope.normalize.tombstones_enabled,
      });
      const trackedApplyLog = {
        hasApplied: async (recordId: string): Promise<boolean> => {
          assertLease();
          const applied = await applyLog.hasApplied(recordId);
          if (applied) store.markRecordApplication(generationId, recordId, 'already_applied');
          return applied;
        },
        markApplied: async (recordId: string, payload?: unknown): Promise<void> => {
          assertLease();
          await applyLog.markApplied(recordId, payload);
          assertLease();
        },
      };
      const trackedProjector = {
        applyRecord: async (record: SourceRecord) => {
          assertLease();
          const result = await projector.applyRecord(record);
          assertLease();
          store.markRecordApplication(
            generationId,
            record.recordId,
            result.applied ? 'projected' : 'not_applied',
          );
          return result;
        },
      };
      const trackedCursorStore = {
        read: async () => await cursorStore.read(),
        commit: async (nextCursor: string): Promise<void> => {
          assertLease();
          await cursorStore.commit(nextCursor);
          assertLease();
        },
      };
      const trackedFactStore = {
        db: factStore.db,
        initSchema: () => factStore!.initSchema(),
        ingest: (fact: Omit<Fact, 'created_at'>) => {
          assertLease();
          const result = factStore!.ingest(fact);
          assertLease();
          return result;
        },
        getById: (factId: string) => factStore!.getById(factId),
        getBySourceRecord: (sourceId: string, sourceRecordId: string) => factStore!.getBySourceRecord(sourceId, sourceRecordId),
        getFactsForCursor: (sourceId: string, sourceCursor: string) => factStore!.getFactsForCursor(sourceId, sourceCursor),
        getUnadmittedFacts: (sourceId?: string, limit?: number) => factStore!.getUnadmittedFacts(sourceId, limit),
        markAdmitted: (factIds: string[]) => factStore!.markAdmitted(factIds),
        getFactsByScope: (scopeId: string, selector?: unknown) => factStore!.getFactsByScope(scopeId, selector),
        close: () => undefined,
      };
      const lock = new kernel.FileLock({
        rootDir: loaded.scope.root_dir,
        acquireTimeoutMs: loaded.scope.runtime.acquire_lock_timeout_ms,
        staleAfterMs: 60 * 60_000,
      });
      const runner = new kernel.DefaultSyncRunner({
        rootDir: loaded.scope.root_dir,
        source,
        cursorStore: trackedCursorStore,
        applyLogStore: trackedApplyLog,
        projector: trackedProjector,
        factStore: trackedFactStore,
        requireFactPersistence: true,
        cleanupTmp: loaded.scope.runtime.cleanup_tmp_on_startup
          ? async () => await kernel.cleanupTmp({ rootDir: loaded.scope.root_dir })
          : undefined,
        acquireLock: async () => await lock.acquire(),
        continueOnError: false,
      });
      const result = await runner.syncOnce();
      await this.options.faultInjector?.('after_runner', generationId);
      assertLease();
      if (result.status === 'success') {
        const staged = store.requireGeneration(generationId);
        if (staged.status !== 'staged') throw new Error(`mailbox_sync_batch_not_staged:${generationId}`);
        const committedCursor = await cursorStore.read();
        if (staged.next_cursor !== null && committedCursor !== staged.next_cursor) {
          throw new Error(`mailbox_sync_cursor_not_committed:${generationId}`);
        }
        generation = store.finalizeGeneration(generationId, leaseToken, this.now());
        leaseReleased = true;
        return generationOperation(generation, false);
      }

      if (result.status === 'retryable_failure' || isLockContention(result.error)) {
        store.releaseLease(loaded.scope.scope_id, generationId, leaseToken, this.now());
        leaseReleased = true;
        throw new Error(`mailbox_sync_retryable:${boundedError(result.error ?? 'sync_retryable_failure')}`);
      }
      generation = store.failGeneration(
        generationId,
        leaseToken,
        boundedError(result.error ?? 'sync_fatal_failure'),
        this.now(),
      );
      leaseReleased = true;
      return blockedGenerationOperation(generation, false);
    } finally {
      clearInterval(heartbeat);
      factStore?.close();
      if (!leaseReleased) {
        try {
          store.releaseLease(loaded.scope.scope_id, generationId, leaseToken, this.now());
        } catch {
          // A lost lease is already a fail-closed outcome. Its current owner is
          // the only process allowed to release it.
        }
      }
      store.close();
    }
  }

  async admitMessage(args: JsonRecord): Promise<JsonRecord> {
    const kernel = await this.runtime();
    const idempotencyKey = requiredBoundedString(args.idempotency_key, 'mailbox_admission_idempotency_key_required', MAX_IDEMPOTENCY_KEY);
    const factId = requiredBoundedString(args.fact_id, 'mailbox_admission_fact_id_required', 256);
    const sourceEventId = requiredBoundedString(args.source_event_id, 'mailbox_admission_source_event_id_required', 256);
    const loaded = await this.loadScope(args, kernel);
    const policyVersion = admissionPolicyVersion(loaded.scope);
    const expectedPolicyVersion = optionalBoundedString(args.policy_version, 128);
    if (expectedPolicyVersion && expectedPolicyVersion !== policyVersion) {
      throw new Error(`mailbox_admission_policy_version_mismatch:${expectedPolicyVersion}:${policyVersion}`);
    }
    const factDb = new kernel.Database(join(loaded.scope.root_dir, '.narada', 'facts.db'));
    const facts = new kernel.SqliteFactStore({ db: factDb });
    facts.initSchema();
    try {
      const fact = facts.getById(factId);
      if (!fact) throw new Error(`mailbox_admission_fact_not_found:${factId}`);
      if (fact.fact_type !== 'mail.message.discovered') {
        throw new Error(`mailbox_admission_fact_type_invalid:${fact.fact_type}`);
      }
      const metadata = mailMetadata(fact);
      const graphMailboxId = configuredGraphMailboxId(loaded.scope);
      if (metadata.mailbox_id !== loaded.scope.scope_id) {
        throw new Error(`mailbox_admission_scope_mismatch:${metadata.mailbox_id}:${loaded.scope.scope_id}`);
      }
      const store = this.openStore();
      try {
        store.requireFirstObservedEvent(sourceEventId, loaded.scope.scope_id, factId);
      const evaluation = kernel.evaluateMailFactAdmission(fact, loaded.scope.admission?.mail);
      const requestFingerprint = fingerprint({
        schema: 'narada.mailbox.message_admission_request.v2',
        scope_id: loaded.scope.scope_id,
        fact_id: factId,
        source_event_id: sourceEventId,
        policy_version: policyVersion,
      });
      const admissionId = stableId('mba_', `${loaded.scope.scope_id}\u0000${factId}`);
      const correlationKeys = trustedCorrelationKeys(metadata);
      const source: JsonRecord = {
        source_kind: 'mailbox_message',
        source_scope: metadata.mailbox_id,
        immutable_source_id: metadata.message_id,
        summary: metadata.subject ? `Mailbox message: ${metadata.subject}`.slice(0, 500) : 'Mailbox message',
        source_ref: {
          schema: 'narada.mailbox.source_ref.v1',
          scope_id: loaded.scope.scope_id,
          mailbox_id: graphMailboxId,
          message_id: metadata.message_id,
          fact_id: factId,
          source_record_id: fact.provenance.source_record_id,
          source_version: fact.provenance.source_version,
          ...(metadata.conversation_id ? { conversation_id: metadata.conversation_id } : {}),
          ...(metadata.internet_message_id ? { internet_message_id: metadata.internet_message_id } : {}),
        },
        correlation_keys: correlationKeys,
      };
      const decision: JsonRecord = {
        schema: 'narada.mailbox.message_admission_receipt.v2',
        admission_id: admissionId,
        decision: evaluation.admitted ? 'admitted' : 'rejected',
        reason: evaluation.reason,
        policy_version: policyVersion,
        source_event_id: sourceEventId,
        scope_id: loaded.scope.scope_id,
        fact_id: factId,
        source,
        evaluated_metadata: {
          folder_refs: evaluation.folder_refs,
          sender_email: evaluation.sender_email,
        },
      };
      const eventPayload: JsonRecord = {
        schema: evaluation.admitted
          ? 'narada.mailbox.message_admitted.v1'
          : 'narada.mailbox.message_rejected.v1',
        admission_id: admissionId,
        source_event_id: sourceEventId,
        scope_id: loaded.scope.scope_id,
        fact_id: factId,
        decision: decision.decision,
        reason: evaluation.reason,
        policy_version: policyVersion,
        source,
      };
        const recorded = store.recordAdmission({
          admission_id: admissionId,
          idempotency_key: idempotencyKey,
          request_fingerprint: requestFingerprint,
          scope_id: loaded.scope.scope_id,
          fact_id: factId,
          policy_version: policyVersion,
          decision,
          event_topic: evaluation.admitted ? 'mailbox.message.admitted' : 'mailbox.message.rejected',
          event_payload: eventPayload,
          source_event_id: sourceEventId,
          now: this.now(),
        });
        return {
          schema: DOMAIN_SCHEMA,
          operation_ref: `mailbox-admission:${admissionId}`,
          outcome: 'completed',
          result: { ...recorded.decision, idempotency_replayed: recorded.replayed },
        };
      } finally {
        store.close();
      }
    } finally {
      facts.close();
    }
  }

  async reconcileFirstObservations(args: JsonRecord): Promise<JsonRecord> {
    const kernel = await this.runtime();
    const idempotencyKey = requiredBoundedString(
      args.idempotency_key,
      'mailbox_reconciliation_idempotency_key_required',
      MAX_IDEMPOTENCY_KEY,
    );
    const generationId = requiredBoundedString(args.generation_id, 'mailbox_reconciliation_generation_id_required', 128);
    const loaded = await this.loadScope(args, kernel);
    const limit = boundedInteger(args.limit, 100, 1, 100);
    const store = this.openStore();
    let factStore: FactStore | null = null;
    try {
      const observed = store.observedMessageKeys(loaded.scope.scope_id);
      const candidatesByIdentity = new Map<string, FirstObservationCandidate>();
      for (const record of store.generationRecords(generationId)) {
        if (record.application_status === 'not_applied') continue;
        if (record.event_kind === 'delete' || record.event_kind === 'deleted') continue;
        if (!record.mailbox_id || !record.message_id) continue;
        if (record.mailbox_id !== loaded.scope.scope_id) {
          throw new Error(`mailbox_reconciliation_scope_mismatch:${record.mailbox_id}:${loaded.scope.scope_id}`);
        }
        const identity = `${record.mailbox_id}\u0000${record.message_id}`;
        if (!observed.has(identity) && !candidatesByIdentity.has(identity)) {
          candidatesByIdentity.set(identity, {
            mailbox_id: record.mailbox_id,
            message_id: record.message_id,
            fact_id: record.fact_id,
            conversation_id: record.conversation_id,
          });
        }
      }
      const unobserved = [...candidatesByIdentity.values()];
      const candidates = unobserved.slice(0, limit);
      if (candidates.length > 0) {
        const databasePath = join(loaded.scope.root_dir, '.narada', 'facts.db');
        if (!existsSync(databasePath)) {
          throw new Error(`mailbox_reconciliation_fact_db_missing:${databasePath}`);
        }
        try {
          const factDb = new kernel.Database(databasePath);
          factStore = new kernel.SqliteFactStore({ db: factDb });
          for (const candidate of candidates) {
            const fact = factStore.getById(candidate.fact_id);
            if (!fact) throw new Error(`mailbox_reconciliation_fact_not_found:${candidate.fact_id}`);
            if (fact.fact_type !== 'mail.message.discovered') {
              throw new Error(`mailbox_reconciliation_fact_type_invalid:${fact.fact_type}`);
            }
            const metadata = mailMetadata(fact);
            if (metadata.mailbox_id !== candidate.mailbox_id || metadata.message_id !== candidate.message_id) {
              throw new Error(`mailbox_reconciliation_fact_identity_mismatch:${candidate.fact_id}`);
            }
            if (metadata.conversation_id) candidate.conversation_id = metadata.conversation_id;
          }
        } catch (error) {
          throw new Error(`mailbox_reconciliation_fact_validation_failed:${asError(error).message}`);
        } finally {
          factStore?.close();
          factStore = null;
        }
      }
      const requestFingerprint = fingerprint({
        schema: 'narada.mailbox.reconcile_first_observations_request.v1',
        scope_id: loaded.scope.scope_id,
        generation_id: generationId,
        limit,
      });
      const operationId = stableId('mbr_', idempotencyKey);
      const recorded = store.reconcileFirstObservations({
        operation_id: operationId,
        idempotency_key: idempotencyKey,
        request_fingerprint: requestFingerprint,
        scope_id: loaded.scope.scope_id,
        generation_id: generationId,
        candidates,
        remaining_unobserved: Math.max(0, unobserved.length - candidates.length),
        has_more: unobserved.length > candidates.length,
        now: this.now(),
      });
      return {
        schema: DOMAIN_SCHEMA,
        operation_ref: `mailbox-reconcile:${operationId}`,
        outcome: 'completed',
        result: { ...recorded.result, idempotency_replayed: recorded.replayed },
      };
    } finally {
      store.close();
    }
  }

  async factShow(args: JsonRecord): Promise<JsonRecord> {
    const kernel = await this.runtime();
    const factId = requiredBoundedString(args.fact_id, 'mailbox_fact_id_required', 256);
    const loaded = await this.loadScope(args, kernel);
    const databasePath = join(loaded.scope.root_dir, '.narada', 'facts.db');
    if (!existsSync(databasePath)) {
      return {
        schema: 'narada.mailbox.immutable_fact.v1',
        status: 'not_found',
        fact_id: factId,
        scope_id: loaded.scope.scope_id,
      };
    }
    const factDb = new kernel.Database(databasePath);
    const facts = new kernel.SqliteFactStore({ db: factDb });
    try {
      const fact = facts.getById(factId);
      if (!fact) {
        return {
          schema: 'narada.mailbox.immutable_fact.v1',
          status: 'not_found',
          fact_id: factId,
          scope_id: loaded.scope.scope_id,
        };
      }
      if (fact.fact_type !== 'mail.message.discovered') {
        throw new Error(`mailbox_fact_type_invalid:${fact.fact_type}`);
      }
      const payload = requireRecord(JSON.parse(fact.payload_json) as unknown, 'mailbox_fact_payload_invalid');
      const metadata = mailMetadata(fact);
      if (metadata.mailbox_id !== loaded.scope.scope_id) {
        throw new Error(`mailbox_fact_scope_mismatch:${metadata.mailbox_id}:${loaded.scope.scope_id}`);
      }
      const includeContent = args.include_content === true;
      if (includeContent && Buffer.byteLength(fact.payload_json, 'utf8') > MAX_EXPLICIT_FACT_PAYLOAD_BYTES) {
        throw new Error(`mailbox_fact_content_too_large:${Buffer.byteLength(fact.payload_json, 'utf8')}`);
      }
      return {
        schema: 'narada.mailbox.immutable_fact.v1',
        status: 'ok',
        scope_id: loaded.scope.scope_id,
        projection: includeContent ? 'full' : 'safe',
        fact: {
          fact_id: fact.fact_id,
          fact_type: fact.fact_type,
          provenance: fact.provenance,
          payload_sha256: createHash('sha256').update(fact.payload_json).digest('hex'),
          payload: includeContent ? payload : safeFactPayload(payload),
          payload_content_included: includeContent,
          created_at: fact.created_at,
        },
      };
    } finally {
      facts.close();
    }
  }

  generationShow(args: JsonRecord): JsonRecord {
    const generationId = requiredBoundedString(args.generation_id, 'mailbox_generation_id_required', 128);
    const store = this.openStore();
    try {
      const generation = store.requireGeneration(generationId);
      const offset = boundedInteger(args.offset, 0, 0, Number.MAX_SAFE_INTEGER);
      const limit = boundedInteger(args.limit, 100, 1, 100);
      const records = store.generationRecords(generationId).slice(offset, offset + limit).map(publicGenerationRecord);
      return {
        schema: 'narada.mailbox.sync_generation.v1',
        generation: publicGeneration(generation),
        offset,
        limit,
        records,
        next_offset: offset + records.length < generation.batch_record_count ? offset + records.length : null,
        records_truncated: offset + records.length < generation.batch_record_count,
      };
    } finally {
      store.close();
    }
  }

  outboxConsumerRegister(args: JsonRecord): JsonRecord {
    const consumerId = requiredBoundedString(args.consumer_id, 'mailbox_outbox_consumer_id_required', 256);
    const scopeId = requiredBoundedString(args.scope_id, 'mailbox_outbox_scope_id_required', MAX_SCOPE_ID);
    const topics = requiredUniqueStrings(args.topics, 'mailbox_outbox_topics_required', 16, 256);
    const startAt = timestamp(args.start_at, 'mailbox_outbox_start_at_required');
    const store = this.openStore();
    try {
      return {
        schema: 'narada.mailbox.outbox_consumer.v2',
        consumer: store.registerOutboxConsumer(consumerId, scopeId, topics, startAt, this.now()),
      };
    } finally {
      store.close();
    }
  }

  outboxConsumerShow(args: JsonRecord): JsonRecord {
    const consumerId = requiredBoundedString(args.consumer_id, 'mailbox_outbox_consumer_id_required', 256);
    const store = this.openStore();
    try {
      const consumer = store.outboxConsumer(consumerId);
      return consumer
        ? { schema: 'narada.mailbox.outbox_consumer_lookup.v1', status: 'ok', consumer }
        : { schema: 'narada.mailbox.outbox_consumer_lookup.v1', status: 'not_found', consumer_id: consumerId };
    } finally {
      store.close();
    }
  }

  outboxList(args: JsonRecord): JsonRecord {
    const consumerId = requiredBoundedString(args.consumer_id, 'mailbox_outbox_consumer_id_required', 256);
    const limit = boundedInteger(args.limit, 100, 1, 100);
    const store = this.openStore();
    try {
      const page = store.listOutbox(consumerId, limit);
      return { schema: 'narada.mailbox.outbox_list.v2', consumer_id: consumerId, count: page.items.length, ...page };
    } finally {
      store.close();
    }
  }

  outboxAck(args: JsonRecord): JsonRecord {
    const consumerId = requiredBoundedString(args.consumer_id, 'mailbox_outbox_consumer_id_required', 256);
    const eventId = requiredBoundedString(args.event_id, 'mailbox_outbox_event_id_required', 256);
    const rawReceipt = requireRecord(args.receipt, 'mailbox_outbox_receipt_required');
    if (Object.keys(rawReceipt).some((key) => !['schema', 'outcome', 'effect_ref'].includes(key))) {
      throw new Error('mailbox_outbox_receipt_fields_invalid');
    }
    const receipt = {
      schema: requiredBoundedString(rawReceipt.schema, 'mailbox_outbox_receipt_schema_required', 128),
      outcome: requiredBoundedString(rawReceipt.outcome, 'mailbox_outbox_receipt_outcome_required', 64),
      effect_ref: requiredBoundedString(rawReceipt.effect_ref, 'mailbox_outbox_receipt_effect_ref_required', 512),
    };
    const store = this.openStore();
    try {
      return {
        schema: 'narada.mailbox.outbox_ack.v1',
        ...store.ackOutbox(consumerId, eventId, receipt, this.now()),
      };
    } finally {
      store.close();
    }
  }

  private async loadScope(args: JsonRecord, kernel: ControlPlaneRuntime): Promise<{ scope: ScopeConfig; configPath: string }> {
    const configArgument = optionalBoundedString(args.config_path, 1024) ?? DEFAULT_CONFIG_PATH;
    const configPath = resolve(this.siteRoot, configArgument);
    assertInside(this.siteRoot, configPath, 'mailbox_config_path_outside_site');
    const config = await kernel.loadConfig({ path: configPath });
    const requestedScope = optionalBoundedString(args.scope_id, MAX_SCOPE_ID);
    const scope = requestedScope
      ? config.scopes.find((candidate) => candidate.scope_id === requestedScope)
      : config.scopes.length === 1 ? config.scopes[0] : undefined;
    if (!scope) throw new Error(requestedScope ? `mailbox_scope_not_found:${requestedScope}` : 'mailbox_scope_id_required');
    const rootDir = resolve(this.siteRoot, scope.root_dir);
    assertInside(this.siteRoot, rootDir, 'mailbox_scope_root_outside_site');
    return { scope: { ...scope, root_dir: rootDir }, configPath };
  }

  private openStore(): MailboxDomainStore {
    return new MailboxDomainStore(join(this.siteRoot, DOMAIN_RELATIVE_ROOT, 'mailbox-domain.db'));
  }

  private generationArtifactPath(generationId: string): string {
    return join(this.siteRoot, DOMAIN_RELATIVE_ROOT, 'generations', `${generationId}.json`);
  }

  private now(): string {
    return this.options.now?.() ?? new Date().toISOString();
  }

  private async runtime(): Promise<ControlPlaneRuntime> {
    return this.options.runtime ?? await loadControlPlaneRuntime(this.siteRoot, this.options.controlPlaneRoot);
  }
}

class GenerationSource implements Source {
  readonly sourceId: string;
  private readonly input: {
    generationId: string;
    leaseToken: string;
    store: MailboxDomainStore;
    source: Source;
    sourceRecordToFact: ControlPlaneRuntime['sourceRecordToFact'];
    artifactPath: string;
    assertLease: () => void;
    now: () => string;
    afterStaged: () => Promise<void>;
  };

  constructor(input: GenerationSource['input']) {
    this.input = input;
    this.sourceId = input.source.sourceId;
  }

  async pull(checkpoint?: string | null): Promise<SourceBatch> {
    this.input.assertLease();
    const generation = this.input.store.requireGeneration(this.input.generationId);
    if (generation.status === 'staged') {
      const batch = await readGenerationArtifact(generation, this.input.artifactPath);
      if ((checkpoint ?? null) !== (batch.priorCheckpoint ?? null)) {
        throw new Error(`mailbox_sync_staged_parent_cursor_mismatch:${this.input.generationId}`);
      }
      return batch;
    }

    const existingArtifact = await readGenerationArtifactAt(this.input.artifactPath, this.input.generationId);
    const batch = existingArtifact ?? await this.input.source.pull(checkpoint ?? null);
    this.input.assertLease();
    if ((batch.priorCheckpoint ?? checkpoint ?? null) !== (checkpoint ?? null)) {
      throw new Error(`mailbox_sync_source_parent_cursor_mismatch:${this.input.generationId}`);
    }
    const artifact = existingArtifact
      ? await describeExistingArtifact(this.input.artifactPath)
      : await writeGenerationArtifact(this.input.artifactPath, this.input.generationId, batch);
    const records = batch.records.map((record) => stagedRecord(
      record,
      batch.nextCheckpoint ?? null,
      this.input.sourceRecordToFact,
    ));
    this.input.store.stageGeneration({
      generation_id: this.input.generationId,
      lease_token: this.input.leaseToken,
      parent_cursor: batch.priorCheckpoint ?? checkpoint ?? null,
      next_cursor: batch.nextCheckpoint ?? null,
      batch_path: this.input.artifactPath,
      batch_sha256: artifact.sha256,
      records,
      now: this.input.now(),
    });
    await this.input.afterStaged();
    this.input.assertLease();
    return batch;
  }
}

function createGraphSource(kernel: ControlPlaneRuntime, scope: ScopeConfig, siteRoot: string): Source {
  const configured = scope.graph ?? scope.sources.find((source) => source.type === 'graph');
  if (!configured?.user_id) throw new Error(`mailbox_scope_graph_source_required:${scope.scope_id}`);
  const graph = {
    tenant_id: configured.tenant_id,
    client_id: configured.client_id,
    client_secret: configured.client_secret,
    user_id: configured.user_id,
    base_url: configured.base_url,
    prefer_immutable_ids: configured.prefer_immutable_ids ?? true,
  };
  const tokenProvider = configured.auth_mode === 'delegated_token_store'
    ? buildDelegatedGraphTokenProvider(siteRoot)
    : kernel.buildGraphTokenProvider({ graph });
  const client = new kernel.GraphHttpClient({
    tokenProvider,
    baseUrl: graph.base_url,
    preferImmutableIds: graph.prefer_immutable_ids,
  });
  const adapter = new kernel.DefaultGraphAdapter({
    mailbox_id: scope.scope_id,
    user_id: graph.user_id,
    client,
    adapter_scope: {
      mailbox_id: scope.scope_id,
      included_container_refs: scope.scope.included_container_refs,
      included_item_kinds: scope.scope.included_item_kinds,
    },
    body_policy: scope.normalize.body_policy,
    attachment_policy: scope.normalize.attachment_policy,
    include_headers: scope.normalize.include_headers,
    normalize_folder_ref: kernel.normalizeFolderRef,
    normalize_flagged: kernel.normalizeFlagged,
  });
  return new kernel.ExchangeSource({ adapter, sourceId: scope.scope_id });
}

function configuredGraphMailboxId(scope: ScopeConfig): string {
  const configured = scope.graph ?? scope.sources.find((source) => source.type === 'graph');
  return requiredBoundedString(
    configured?.mailbox_id ?? configured?.user_id,
    `mailbox_scope_graph_user_id_required:${scope.scope_id}`,
    512,
  );
}

async function writeGenerationArtifact(path: string, generationId: string, batch: SourceBatch): Promise<{ sha256: string }> {
  await mkdir(dirname(path), { recursive: true });
  const document = { schema: GENERATION_ARTIFACT_SCHEMA, generation_id: generationId, batch };
  const bytes = `${JSON.stringify(document)}\n`;
  const digest = sha256(bytes);
  const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
  try {
    await writeFile(temporary, bytes, { encoding: 'utf8', flag: 'wx' });
    try {
      await rename(temporary, path);
    } catch (error) {
      const existing = await readFile(path, 'utf8').catch(() => null);
      if (existing === null || sha256(existing) !== digest) throw error;
      await rm(temporary, { force: true });
    }
    return { sha256: digest };
  } catch (error) {
    await rm(temporary, { force: true }).catch(() => undefined);
    throw error;
  }
}

async function describeExistingArtifact(path: string): Promise<{ sha256: string }> {
  return { sha256: sha256(await readFile(path, 'utf8')) };
}

async function readGenerationArtifact(generation: SyncGenerationRow, expectedPath: string): Promise<SourceBatch> {
  if (!generation.batch_path || !generation.batch_sha256) throw new Error(`mailbox_sync_generation_artifact_missing:${generation.generation_id}`);
  if (resolve(generation.batch_path) !== resolve(expectedPath)) {
    throw new Error(`mailbox_sync_generation_artifact_path_mismatch:${generation.generation_id}`);
  }
  const bytes = await readFile(generation.batch_path, 'utf8');
  if (sha256(bytes) !== generation.batch_sha256) throw new Error(`mailbox_sync_generation_artifact_hash_mismatch:${generation.generation_id}`);
  return parseGenerationArtifact(bytes, generation.generation_id);
}

async function readGenerationArtifactAt(path: string, generationId: string): Promise<SourceBatch | null> {
  const bytes = await readFile(path, 'utf8').catch((error: NodeJS.ErrnoException) => {
    if (error.code === 'ENOENT') return null;
    throw error;
  });
  return bytes === null ? null : parseGenerationArtifact(bytes, generationId);
}

function parseGenerationArtifact(bytes: string, generationId: string): SourceBatch {
  const document = requireRecord(JSON.parse(bytes) as unknown, 'mailbox_sync_generation_artifact_invalid');
  if (document.schema !== GENERATION_ARTIFACT_SCHEMA || document.generation_id !== generationId) {
    throw new Error(`mailbox_sync_generation_artifact_identity_mismatch:${generationId}`);
  }
  const batch = requireRecord(document.batch, 'mailbox_sync_generation_batch_invalid');
  if (!Array.isArray(batch.records)) throw new Error('mailbox_sync_generation_records_invalid');
  return batch as unknown as SourceBatch;
}

function stagedRecord(
  record: SourceRecord,
  sourceCursor: string | null,
  sourceRecordToFact: ControlPlaneRuntime['sourceRecordToFact'],
): StagedGenerationRecord {
  const event = requireRecord(record.payload, `mailbox_sync_record_payload_invalid:${record.recordId}`);
  const fact = sourceRecordToFact(record, sourceCursor);
  return {
    record_id: record.recordId,
    ordinal: typeof record.ordinal === 'string' ? record.ordinal : null,
    fact_id: fact.fact_id,
    event_kind: optionalBoundedString(event.event_kind, 64) ?? 'unknown',
    message_id: optionalBoundedString(event.message_id, 512),
    mailbox_id: optionalBoundedString(event.mailbox_id, 512),
    conversation_id: optionalBoundedString(event.conversation_id, 1024),
    source_version: optionalBoundedString(event.source_version, 1024),
  };
}

function generationReady(records: GenerationRecordRow[]): boolean {
  return records.every((record) => record.application_status !== 'staged');
}

function generationOperation(generation: SyncGenerationRow, replayed: boolean): JsonRecord {
  const receipt = generation.receipt;
  if (!receipt) throw new Error(`mailbox_sync_receipt_missing:${generation.generation_id}`);
  const serializedReceipt = canonicalJson(receipt);
  return {
    schema: DOMAIN_SCHEMA,
    operation_ref: `mailbox-sync:${generation.generation_id}`,
    outcome: 'completed',
    result: generationReceiptSummary(receipt, replayed),
    result_ref: {
      ref: `mailbox-generation-receipt:${generation.generation_id}`,
      sha256: sha256(serializedReceipt),
      byte_length: Buffer.byteLength(serializedReceipt, 'utf8'),
      media_type: 'application/json',
    },
  };
}

function generationReceiptSummary(receipt: JsonRecord, replayed: boolean): JsonRecord {
  const observedRefs = Array.isArray(receipt.observed_message_refs)
    ? receipt.observed_message_refs
    : [];
  return {
    schema: typeof receipt.schema === 'string'
      ? receipt.schema
      : 'narada.mailbox.sync_generation_receipt.v1',
    generation_id: receipt.generation_id,
    scope_id: receipt.scope_id,
    status: receipt.status,
    config_fingerprint: receipt.config_fingerprint,
    parent_cursor_sha256: receipt.parent_cursor_sha256 ?? null,
    next_cursor_sha256: receipt.next_cursor_sha256 ?? null,
    record_count: receipt.record_count,
    observed_message_count: receipt.observed_message_count,
    first_observation_count: receipt.first_observation_count,
    tombstone_count: receipt.tombstone_count,
    observed_message_refs_available_count: observedRefs.length,
    observed_message_refs_omitted: true,
    observed_message_refs_truncated: receipt.observed_message_refs_truncated === true,
    completed_at: receipt.completed_at,
    idempotency_replayed: replayed,
  };
}

function blockedGenerationOperation(generation: SyncGenerationRow, replayed: boolean): JsonRecord {
  const message = generation.error_message ?? 'mailbox_sync_failed';
  return {
    schema: DOMAIN_SCHEMA,
    operation_ref: `mailbox-sync:${generation.generation_id}`,
    outcome: 'completed',
    result: {
      schema: 'narada.mailbox.sync_generation_failure.v1',
      generation_id: generation.generation_id,
      scope_id: generation.scope_id,
      status: 'blocked',
      error_message: message,
      idempotency_replayed: replayed,
    },
  };
}

function publicGeneration(generation: SyncGenerationRow): JsonRecord {
  return {
    generation_id: generation.generation_id,
    scope_id: generation.scope_id,
    config_fingerprint: generation.config_fingerprint,
    status: generation.status,
    parent_cursor_sha256: nullableHash(generation.parent_cursor),
    next_cursor_sha256: nullableHash(generation.next_cursor),
    batch_sha256: generation.batch_sha256,
    batch_record_count: generation.batch_record_count,
    receipt: generation.receipt,
    error_message: generation.error_message,
    created_at: generation.created_at,
    updated_at: generation.updated_at,
    completed_at: generation.completed_at,
  };
}

function publicGenerationRecord(record: GenerationRecordRow): JsonRecord {
  return {
    record_id: record.record_id,
    fact_id: record.fact_id,
    event_kind: record.event_kind,
    message_id: record.message_id,
    mailbox_id: record.mailbox_id,
    conversation_id: record.conversation_id,
    source_version: record.source_version,
    application_status: record.application_status,
  };
}

function syncConfigFingerprint(scope: ScopeConfig): string {
  const graph = scope.graph ?? scope.sources.find((source) => source.type === 'graph');
  return fingerprint({
    schema: 'narada.mailbox.sync_config.v1',
    scope_id: scope.scope_id,
    root_dir: scope.root_dir,
    source: graph ? {
      type: 'graph',
      mailbox_id: graph.mailbox_id,
      user_id: graph.user_id,
      base_url: graph.base_url,
      prefer_immutable_ids: graph.prefer_immutable_ids ?? true,
    } : scope.sources[0] ?? null,
    scope: scope.scope,
    normalize: scope.normalize,
  });
}

function admissionPolicyVersion(scope: ScopeConfig): string {
  return `sha256:${fingerprint({
    schema: 'narada.mailbox.admission_policy.v1',
    scope_id: scope.scope_id,
    policy: scope.admission?.mail ?? {},
  })}`;
}

function mailMetadata(fact: Fact): {
  mailbox_id: string;
  message_id: string;
  conversation_id: string | null;
  internet_message_id: string | null;
  subject: string | null;
} {
  const envelope = requireRecord(JSON.parse(fact.payload_json) as unknown, 'mailbox_fact_payload_invalid');
  const event = requireRecord(envelope.event, 'mailbox_fact_event_invalid');
  const payload = event.payload && typeof event.payload === 'object' && !Array.isArray(event.payload)
    ? event.payload as JsonRecord
    : {};
  return {
    mailbox_id: requiredBoundedString(event.mailbox_id ?? payload.mailbox_id, 'mailbox_fact_mailbox_id_missing', 512),
    message_id: requiredBoundedString(event.message_id ?? payload.message_id, 'mailbox_fact_message_id_missing', 512),
    conversation_id: optionalBoundedString(event.conversation_id ?? payload.conversation_id, 1024),
    internet_message_id: optionalBoundedString(payload.internet_message_id, 1024),
    subject: optionalBoundedString(payload.subject, 500),
  };
}

function safeFactPayload(value: unknown): unknown {
  if (Array.isArray(value)) return value.map((entry) => safeFactPayload(entry));
  if (!value || typeof value !== 'object') return value;
  const record = value as JsonRecord;
  const result: JsonRecord = {};
  for (const [key, nested] of Object.entries(record)) {
    if (key.toLowerCase() === 'attachments') {
      result[key] = safeAttachmentMetadata(nested);
    } else {
      result[key] = safeFactPayload(nested);
    }
  }
  return result;
}

function safeAttachmentMetadata(value: unknown): unknown {
  if (Array.isArray(value)) return value.map((entry) => safeAttachmentMetadata(entry));
  if (!value || typeof value !== 'object') return value;
  const result: JsonRecord = {};
  for (const [key, nested] of Object.entries(value as JsonRecord)) {
    if (/^(?:contentbytes|content_bytes|content_base64|contentref|content_ref|content|data|bytes|raw)$/i.test(key)) continue;
    result[key] = safeAttachmentMetadata(nested);
  }
  return result;
}

function trustedCorrelationKeys(metadata: ReturnType<typeof mailMetadata>): JsonRecord[] {
  const keys: JsonRecord[] = [];
  if (metadata.conversation_id) {
    keys.push({ kind: 'mailbox_conversation', scope: metadata.mailbox_id, value: metadata.conversation_id });
  }
  if (metadata.internet_message_id) {
    keys.push({ kind: 'internet_message_id', scope: 'rfc5322', value: metadata.internet_message_id });
  }
  return keys;
}

function fingerprint(value: unknown): string {
  return createHash('sha256').update(canonicalJson(value)).digest('hex');
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  const record = value as JsonRecord;
  return `{${Object.keys(record).sort().filter((key) => record[key] !== undefined).map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`).join(',')}}`;
}

function assertInside(root: string, path: string, code: string): void {
  const rel = relative(root, path);
  if (rel.startsWith('..') || rel.includes(`..${sep}`) || isAbsolute(rel)) throw new Error(`${code}:${path}`);
}

function requireRecord(value: unknown, code: string): JsonRecord {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(code);
  return value as JsonRecord;
}

function requiredBoundedString(value: unknown, code: string, max: number): string {
  const normalized = typeof value === 'string' ? value.trim() : '';
  if (!normalized) throw new Error(code);
  if (normalized.length > max) throw new Error(`${code}_too_long`);
  return normalized;
}

function optionalBoundedString(value: unknown, max: number): string | null {
  if (value === undefined || value === null || value === '') return null;
  if (typeof value !== 'string') throw new Error('mailbox_string_argument_invalid');
  const normalized = value.trim();
  if (!normalized) return null;
  if (normalized.length > max) throw new Error('mailbox_string_argument_too_long');
  return normalized;
}

function timestamp(value: unknown, code: string): string {
  const normalized = requiredBoundedString(value, code, 64);
  if (Number.isNaN(Date.parse(normalized))) throw new Error(`${code}_invalid`);
  return new Date(normalized).toISOString();
}

function boundedInteger(value: unknown, fallback: number, min: number, max: number): number {
  const resolved = value === undefined ? fallback : Number(value);
  if (!Number.isSafeInteger(resolved) || resolved < min || resolved > max) throw new Error('mailbox_integer_argument_invalid');
  return resolved;
}

function nullableHash(value: string | null): string | null {
  return value === null ? null : sha256(value);
}

function boundedError(value: string): string {
  return value.length <= 2048 ? value : value.slice(0, 2048);
}

function isLockContention(value: string | undefined): boolean {
  return typeof value === 'string' && value.includes('Failed to acquire lock');
}

function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}
