import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { existsSync } from 'node:fs';
import { createServerState, handleRequest, persistResponse, projectOperator, resolveDisplayPreferences } from '../src/main.js';

const root = mkdtempSync(join(tmpdir(), 'operator-communication-'));
const state = createServerState({ siteRoot: root });
const response = {
  schema: 'marici.typed-response.v1',
  response_id: 'r1',
  created_at: '2026-08-28T00:00:00Z',
  agent_id: 'marici.Nima',
  operator: { items: [{ id: 'done', kind: 'result', statement: 'Done.', impact: 'Surface exists.', epistemic_status: 'verified', evidence: ['source:test'] }] },
  agent: {
    state: 'completed', objective: 'Test.', stop_condition: 'Pass.', constraints: ['No leak.'], items: [],
    communication: { opening_sequence: 1, closing_sequence: 1, actionable_messages: [], reply_events: [] },
  },
};

try {
  const projected = projectOperator({ response }, state);
  assert.deepEqual(projected, { items: [{ kind: 'result', statement: 'Done.', impact: 'Surface exists.' }] });
  assert.equal('agent' in projected, false);
  assert.deepEqual(projectOperator({ response, display_policy: 'minimal' }, state), { items: [{ statement: 'Done.' }] });
  assert.deepEqual(projectOperator({ response, format: ['id', 'evidence'] }, state), { items: [{ id: 'done', evidence: ['source:test'] }] });
  assert.equal(resolveDisplayPreferences({}, state).source, 'default');

  mkdirSync(dirname(state.siteDisplayPath), { recursive: true });
  writeFileSync(state.siteDisplayPath, '[defaults]\npolicy = "minimal"\nformat = "code"\n[policies.minimal]\nfields = ["impact"]\nmax_chars = 10\nmax_array_items = 1\n');
  assert.deepEqual(projectOperator({ response }, state), { items: [{ impact: 'Surface ex…' }] });
  assert.equal(resolveDisplayPreferences({}, state).source, 'site');
  assert.equal(resolveDisplayPreferences({ display_policy: 'medium' }, state).source, 'input');
  rmSync(state.siteDisplayPath);

  const longResponse = { ...response, operator: { items: [{ ...response.operator.items[0], statement: 'x'.repeat(600), evidence: Array.from({ length: 25 }, (_, index) => `source:${index}`) }] } };
  assert.equal((projectOperator({ response: longResponse }, state).items as any[])[0].statement.length, 501);
  assert.equal((projectOperator({ response: longResponse, display_policy: 'all-limited' }, state).items as any[])[0].evidence.length, 21);
  assert.equal((projectOperator({ response: longResponse, display_policy: 'all-unlimited' }, state).items as any[])[0].statement.length, 600);

  const proseCall = handleRequest({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'operator_communication_project', arguments: { response, persist: false } } }, state) as Record<string, any>;
  assert.match(proseCall.result.content[0].text, /^Done\.\n\nImpact: Surface exists\.$/);
  assert.doesNotMatch(proseCall.result.content[0].text, /Kind: result|Statement:/);
  assert.doesNotMatch(proseCall.result.content[0].text, /\[\[items\]\]/);
  const codeCall = handleRequest({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'operator_communication_project', arguments: { response, persist: false, format: 'code' } } }, state) as Record<string, any>;
  assert.match(codeCall.result.content[0].text, /\[\[items\]\]/);
  assert.equal(codeCall.result._meta.display.policy, 'short');

  const inferredResponse = { ...response, operator: { items: [{ ...response.operator.items[0], epistemic_status: 'inferred', uncertainty: 'Source-specific instantiation remains open.' }] } };
  const inferredCall = handleRequest({ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'operator_communication_project', arguments: { response: inferredResponse, persist: false } } }, state) as Record<string, any>;
  assert.match(inferredCall.result.content[0].text, /Epistemic status: inferred/);
  assert.match(inferredCall.result.content[0].text, /Uncertainty: Source-specific instantiation remains open\./);

  const permissive = `schema = "test"\nunknown_fields = "reject"\n[root]\nrequired = ["operator"]\n[root.fields.operator]\ntype = "table"\nschema_ref = "operator"\n[tables.operator]\nrequired = ["items"]\n[tables.operator.fields.items]\ntype = "array"\nmin_items = 0\n`;
  assert.deepEqual(projectOperator({ response: { operator: { items: [] } }, schema: permissive }, state), { items: [] });

  mkdirSync(dirname(state.siteSchemaPath), { recursive: true });
  writeFileSync(state.siteSchemaPath, permissive);
  assert.deepEqual(projectOperator({ response: { operator: { items: [] } } }, state), { items: [] });
  rmSync(state.siteSchemaPath);
  assert.throws(() => projectOperator({ response, schema: 'not = [' }, state));

  const stored = persistResponse({ response, created_by: 'marici.Nima' }, state);
  assert.match(String(stored?.ref), /^operator_response:/);
  assert.equal(stored?.storage_kind, 'sqlite');
  assert.deepEqual(projectOperator({ response_ref: stored?.ref, display_policy: 'all-unlimited' }, state), response.operator);
  assert.equal(persistResponse({ response, persist: false }, state), null);
  assert.throws(() => projectOperator({ response_ref: stored?.ref, persist: false }, state), /companion_arguments_forbidden/);
  assert.throws(() => projectOperator({ response, persist: false, created_by: 'marici.Nima' }, state), /created_by_requires_persistence/);

  const large = { ...response, response_id: 'large', agent: { ...response.agent, constraints: ['x'.repeat(21_000)] } };
  const largeStored = persistResponse({ response: large }, state);
  assert.equal(largeStored?.storage_kind, 'file');
  assert.equal(existsSync(join(root, '.narada', 'runtime', 'operator-communication', String(largeStored?.body_path))), true);
  assert.deepEqual(projectOperator({ response_ref: largeStored?.ref, display_policy: 'all-unlimited' }, state), response.operator);

  const db = new DatabaseSync(join(root, '.narada', 'runtime', 'operator-communication', 'operator-communication.sqlite'));
  assert.throws(() => db.prepare('DELETE FROM response_log').run(), /immutable/);
  assert.throws(() => db.prepare('UPDATE response_log SET created_by = ?').run('tamper'), /immutable/);
  db.close();
} finally {
  rmSync(root, { recursive: true, force: true });
}

console.log('operator-communication-mcp tests ok');
