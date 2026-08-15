import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { appendAdmissionEvent } from '../src/inbox/admission-log.js';
import { prepareTaskLifecycleStore } from '@narada-core/task-governance-core/task-lifecycle-store';
import {
  buildTaskSpecFromEnvelope,
  checkDuplicateTask,
  deriveRoutingFromEnvelopePayload,
  pollInboxBridge,
  readUnprocessedEnvelopes,
} from '../src/task-lifecycle/inbox-bridge.js';

const siteRoot = mkdtempSync(join(tmpdir(), 'task-lifecycle-inbox-bridge-'));
mkdirSync(join(siteRoot, '.ai', 'inbox-envelopes'), { recursive: true });

const envelope = {
  schema: 'narada.inbox.envelope.v1',
  envelope_id: 'env-bridge-materialized-latest',
  received_at: '2026-07-08T00:00:00.000Z',
  kind: 'incident',
  authority: {
    level: 'system_generated',
    principal: 'test',
  },
  source: {
    kind: 'synthetic_fixture',
    ref: 'bridge-materialized-latest',
  },
  payload: {
    title: 'Already materialized envelope',
    summary: 'Regression fixture.',
    target_role: 'resident',
  },
  status: 'promoted',
};

writeFileSync(
  join(siteRoot, '.ai', 'inbox-envelopes', 'fixture-env-bridge-materialized-latest.json'),
  JSON.stringify(envelope, null, 2),
  'utf8',
);

appendAdmissionEvent(siteRoot, {
  envelope_id: envelope.envelope_id,
  event_kind: 'envelope_promoted',
  principal: 'inbox-bridge',
  authority_level: 'system_generated',
  payload_hash: null,
});

appendAdmissionEvent(siteRoot, {
  envelope_id: envelope.envelope_id,
  event_kind: 'bridge_materialized',
  principal: 'inbox-bridge',
  authority_level: 'system_generated',
  payload_hash: null,
});

const unprocessed = readUnprocessedEnvelopes(siteRoot);
assert.equal(
  unprocessed.some((item: { envelope_id?: string }) => item.envelope_id === envelope.envelope_id),
  false,
  'bridge_materialized as latest admission event must keep an envelope out of unprocessed polling',
);

const reusedStore = prepareTaskLifecycleStore(siteRoot);
try {
  const reusedStoreResult = await pollInboxBridge(siteRoot, { store: reusedStore, limit: 1 });
  assert.equal(reusedStoreResult.status, 'ok');
  assert.equal(
    (reusedStore.db.prepare('SELECT 1 AS alive').get() as { alive?: number }).alive,
    1,
    'polling with a caller-owned store must leave that connection open',
  );
} finally {
  reusedStore.db.close();
}

const canonicalScheduledEnvelope = {
  schema: 'narada.inbox_envelope.v1',
  envelope_id: 'env_scheduled_sop_zenoti-sync-full-health-check_20260721_canonical',
  received_at: '2026-07-21T14:30:00.000Z',
  kind: 'command_request',
  title: 'Check full Zenoti synchronization health',
  summary: 'Start SOP zenoti-sync-full-health-check through SOP MCP and retain the complete evidence.',
  target_role: 'operator',
  authority: { level: 'site_policy', principal: 'andrey-user.maintenance.scheduled_sops' },
  source: { kind: 'scheduler_schedule', ref: 'zenoti-sync-full-health-check' },
  payload: {
    recommendation: 'escalate',
    preferred_agent_id: 'operator',
    sop_id: 'zenoti-sync-full-health-check',
    trigger_kind: 'schedule',
    trigger_source_ref: 'zenoti-sync-full-health-check:2026-07-21T06:00:00.000Z',
  },
  status: 'received',
};

const routing = deriveRoutingFromEnvelopePayload(canonicalScheduledEnvelope, {
  severity: 45,
  targetRole: 'architect',
});
assert.equal(routing.targetRole, 'operator', 'outer target_role is authoritative for canonical envelopes');
assert.equal(routing.source.targetRoleField, 'envelope.target_role');

const spec = buildTaskSpecFromEnvelope(canonicalScheduledEnvelope, {
  severity: 45,
  action: 'materialize',
  targetRole: 'architect',
  reason: 'command_request_architect_triage',
});
assert.equal(spec.title, '[From Inbox] Check full Zenoti synchronization health');
assert.equal(spec.goal, canonicalScheduledEnvelope.summary);
assert.equal(spec.preferredRole, 'operator');
assert.equal(spec.preferredAgentId, 'operator');
const requiredWork = Array.isArray(spec.requiredWork) ? spec.requiredWork.map(String) : [];
const acceptanceCriteria = Array.isArray(spec.acceptanceCriteria) ? spec.acceptanceCriteria.map(String) : [];
assert.match(requiredWork[0] ?? '', /Start SOP zenoti-sync-full-health-check through SOP MCP/);
assert.match(requiredWork[0] ?? '', /trigger_source_ref="env_scheduled_sop_zenoti-sync-full-health-check_20260721_canonical"/);
assert.ok(acceptanceCriteria.some((item) => item.includes('Record the SOP run evidence')));

const duplicateRows = [{
  task_id: 'legacy-task',
  task_number: 2317,
  title: '[From Inbox] Check full Zenoti synchronization health',
  context_markdown: '**Envelope ID:** env_scheduled_sop_zenoti-sync-full-health-check_20260721',
  goal_markdown: '',
  required_work_markdown: '',
  non_goals_markdown: '',
  status: 'opened',
}];
const duplicateStore = {
  getTaskByEnvelopeId: () => null,
  db: {
    prepare: () => ({ all: () => duplicateRows }),
  },
};
const canonicalReplacementDecision = checkDuplicateTask(duplicateStore, {
  ...canonicalScheduledEnvelope,
  supersedes_envelope_id: 'env_scheduled_sop_zenoti-sync-full-health-check_20260721',
});
assert.equal(
  canonicalReplacementDecision.isDuplicate,
  false,
  'canonical scheduled-SOP replacements must not fuzzy-deduplicate into the superseded task',
);

const legacyDecision = checkDuplicateTask(duplicateStore, {
  ...canonicalScheduledEnvelope,
  kind: 'request',
  supersedes_envelope_id: undefined,
});
assert.equal(legacyDecision.isDuplicate, true);
assert.equal(legacyDecision.matchType, 'title_similarity');
