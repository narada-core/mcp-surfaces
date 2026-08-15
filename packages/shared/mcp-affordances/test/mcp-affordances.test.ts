import assert from 'node:assert/strict';
import {
  affordanceDocumentJsonSchema,
  affordanceResourceRef,
  affordanceToolAction,
  createAffordanceDocument,
  NARADA_MCP_AFFORDANCES_SCHEMA,
  validateAffordanceDocument,
  type NaradaMcpAffordanceDocument,
} from '../src/main.js';

const document: NaradaMcpAffordanceDocument = createAffordanceDocument({
  surface_id: 'sop',
  generated_at: '2026-07-06T12:00:00.000Z',
  title: 'SOP Operator',
  audience: ['operator'],
  summary: 'Inspect and operate SOP runs.',
  panels: [
    {
      id: 'status',
      title: 'Status',
      kind: 'status',
      actions: ['refresh_status', 'run_once'],
      refs: ['latest_run'],
      metrics: [{ id: 'open_attention', label: 'Open attention', value: 2, severity: 'warning' }],
    },
  ],
  actions: [
    affordanceToolAction({
      id: 'refresh_status',
      label: 'Refresh',
      intent: 'refresh',
      tool: 'sop_status',
      read_only: true,
      idempotent: true,
      danger_level: 'none',
    }),
    affordanceToolAction({
      id: 'run_once',
      label: 'Run Once',
      intent: 'run',
      tool: 'sop_run_start',
      arguments: { dry_run: false, limit: 25 },
      read_only: false,
      idempotent: false,
      danger_level: 'medium',
      confirmation: { required: true, message: 'Start one bounded SOP run.' },
    }),
  ],
  refs: [
    affordanceResourceRef({ id: 'latest_run', label: 'Latest run', uri: 'sop-run:latest' }),
  ],
  refresh: { mode: 'poll', interval_ms: 30000, actions: ['refresh_status'] },
  source: { tool: 'sop_operator_affordances', site_id: 'narada-test', site_root: 'C:/workspace/narada.test' },
});

assert.equal(document.schema, NARADA_MCP_AFFORDANCES_SCHEMA);
assert.equal(validateAffordanceDocument(document).status, 'ok');
assert.equal(affordanceDocumentJsonSchema.properties.schema.const, NARADA_MCP_AFFORDANCES_SCHEMA);

const missingAction = structuredClone(document);
missingAction.panels[0].actions = ['missing'];
const missingActionValidation = validateAffordanceDocument(missingAction);
assert.equal(missingActionValidation.status, 'invalid');
assert.equal(missingActionValidation.errors.includes('panels[0].actions_unknown:missing'), true);

const dangerousReadOnlyConflict = structuredClone(document);
dangerousReadOnlyConflict.actions[0].target = { kind: 'tool', tool: '' };
const targetValidation = validateAffordanceDocument(dangerousReadOnlyConflict);
assert.equal(targetValidation.status, 'invalid');
assert.equal(targetValidation.errors.includes('actions[0].target.tool_non_empty_string_required'), true);

const invalidAudience = structuredClone(document);
invalidAudience.audience = ['operator', 'customer' as 'operator'];
const audienceValidation = validateAffordanceDocument(invalidAudience);
assert.equal(audienceValidation.status, 'invalid');
assert.equal(audienceValidation.errors.includes('audience[]_invalid:customer'), true);

console.log('mcp affordances contract tests passed');
