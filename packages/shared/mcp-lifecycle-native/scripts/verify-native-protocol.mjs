import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const extension = process.platform === 'win32' ? '.exe' : '';
const binDir = resolve(process.env.NARADA_LIFECYCLE_NATIVE_BIN_DIR ?? 'target/debug');
const executable = (surface) => join(binDir, `narada-${surface}-lifecycle-mcp${extension}`);
const rpc = (id, method, params = {}) => ({ jsonrpc: '2.0', id, method, params });
const tool = (id, name, args = {}) => rpc(id, 'tools/call', { name, arguments: args });

function run(surface, root, requests = [], extraArgs = []) {
  const input = requests.length ? `${requests.map((request) => JSON.stringify(request)).join('\n')}\n` : '';
  const result = spawnSync(executable(surface), [...extraArgs, '--site-root', root], {
    input, encoding: 'utf8', windowsHide: true, timeout: 120000, maxBuffer: 4 * 1024 * 1024,
  });
  assert.equal(result.status, 0, `${surface}: ${String(result.stderr).slice(-2000)}`);
  return String(result.stdout).split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
}

function structured(responses, id) {
  const response = responses.find((candidate) => candidate.id === id);
  assert.ok(response, `missing response ${id}`);
  assert.equal(response.error, undefined, `request ${id}: ${JSON.stringify(response.error)}`);
  return response.result?.structuredContent;
}

function assertCatalog(surface, tools, expected) {
  assert.equal(tools.length, expected, `${surface} tool count`);
  const walk = (schema, path) => {
    if (schema?.type === 'string' && !Array.isArray(schema.enum)) assert.ok(Number.isInteger(schema.maxLength), `unbounded string ${path}`);
    if (schema?.type === 'array') assert.ok(Number.isInteger(schema.maxItems), `unbounded array ${path}`);
    for (const [name, child] of Object.entries(schema?.properties ?? {})) walk(child, `${path}/${name}`);
    if (schema?.items) walk(schema.items, `${path}/*`);
    for (const keyword of ['allOf', 'anyOf', 'oneOf']) for (const [index, child] of (schema?.[keyword] ?? []).entries()) walk(child, `${path}/${keyword}/${index}`);
  };
  for (const entry of tools) {
    assert.equal(entry.inputSchema?.title, `${entry.name}.input`, `${entry.name} title`);
    assert.equal(entry.inputSchema?.additionalProperties, false, `${entry.name} closure`);
    walk(entry.inputSchema, entry.name);
  }
}

function assertEveryToolRejectsUnknown(surface, root, tools) {
  const requests = tools.map((entry, index) => tool(1000 + index, entry.name, { __unexpected_contract_probe__: true }));
  const responses = run(surface, root, requests);
  for (const [index, entry] of tools.entries()) assert.ok(responses.find((response) => response.id === 1000 + index)?.error, `${entry.name} accepted unknown input`);
}

const taskRoot = mkdtempSync(join(tmpdir(), 'narada-task-native-proof-'));
const workRoot = mkdtempSync(join(tmpdir(), 'narada-work-native-proof-'));
try {
  run('task', taskRoot, [], ['--prepare']);
  const taskCatalogResponse = run('task', taskRoot, [rpc(1, 'tools/list')]);
  const taskTools = taskCatalogResponse[0]?.result?.tools ?? [];
  assertCatalog('task', taskTools, 69);
  assertEveryToolRejectsUnknown('task', taskRoot, taskTools);

  const taskResponses = run('task', taskRoot, [
    tool(2, 'task_lifecycle_doctor'),
    tool(3, 'mcp_payload_create', { payload_id: 'native-proof-task', payload: { title: 'Native proof task', goal: 'Prove durable Rust task authority', required_work: ['Persist lifecycle state'], acceptance_criteria: ['Task closes'], idempotency_key: 'native-proof-task' }, created_by: 'native.proof' }),
    tool(4, 'task_lifecycle_create', { payload_ref: 'mcp_payload:native-proof-task@v1' }),
    tool(5, 'task_lifecycle_create', { payload_ref: 'mcp_payload:native-proof-task@v1' }),
    tool(6, 'task_lifecycle_claim', { task_number: 1, agent_id: 'native.proof' }),
    tool(7, 'task_lifecycle_finish', { task_number: 1, agent_id: 'native.proof', summary: 'Native proof finished', no_files_changed: true }),
    tool(8, 'task_lifecycle_admit_evidence', { task_number: 1, agent_id: 'native.proof' }),
    tool(9, 'task_lifecycle_close', { task_number: 1, agent_id: 'native.proof', mode: 'operator_direct' }),
    tool(10, 'task_lifecycle_show', { task_number: 1 }),
    tool(11, 'task_lifecycle_list', { limit: 1, offset: 0 }),
    tool(12, 'task_lifecycle_search', { query: 'Native proof', limit: 1 }),
    tool(13, 'mcp_payload_show', { ref: 'mcp_payload:native-proof-task@v1' }),
    tool(14, 'mcp_payload_derive', { source_ref: 'mcp_payload:native-proof-task@v1', overlay: { summary: 'derived' }, created_by: 'native.proof' }),
    tool(15, 'mcp_payload_validate', { ref: 'mcp_payload:native-proof-task@v2' }),
  ]);
  assert.equal(structured(taskResponses, 2).schema, 'narada.task_lifecycle.doctor.v1');
  assert.equal(structured(taskResponses, 3).status, 'created');
  assert.equal(structured(taskResponses, 4).status, 'created');
  assert.ok(['created', 'reused'].includes(structured(taskResponses, 5).status));
  assert.equal(structured(taskResponses, 6).status, 'claimed');
  assert.equal(structured(taskResponses, 8).status, 'admitted');
  assert.equal(structured(taskResponses, 9).new_status, 'closed');
  assert.equal(structured(taskResponses, 10).lifecycle.status, 'closed');
  assert.equal(structured(taskResponses, 11).returned, 1);
  assert.equal(structured(taskResponses, 15).status, 'valid');

  run('work', workRoot, [], ['--prepare']);
  const workCatalogResponse = run('work', workRoot, [rpc(20, 'tools/list')]);
  const workTools = workCatalogResponse[0]?.result?.tools ?? [];
  assertCatalog('work', workTools, 80);
  assertEveryToolRejectsUnknown('work', workRoot, workTools);
  const workAdmissionResponses = run('work', workRoot, [
    tool(21, 'work_lifecycle_doctor'),
    tool(22, 'ticket_list', { limit: 1, offset: 0 }),
    tool(23, 'ticket_admit_source', { source_kind: 'native-proof', source_scope: 'isolated', immutable_source_id: 'one', idempotency_key: 'native-proof-ticket', causation_id: 'native-proof', policy_version: 'v1', summary: 'Native proof ticket', source_ref: {}, correlation_keys: [] }),
    tool(24, 'ticket_admit_source', { source_kind: 'native-proof', source_scope: 'isolated', immutable_source_id: 'one', idempotency_key: 'native-proof-ticket', causation_id: 'native-proof', policy_version: 'v1', summary: 'Native proof ticket', source_ref: {}, correlation_keys: [] }),
    tool(25, 'ticket_show', { ticket_id: 'ticket-1' }),
    tool(26, 'ticket_sources_list', { ticket_id: 'ticket-1' }),
  ]);
  assert.equal(structured(workAdmissionResponses, 21).schema, 'narada.work_lifecycle.doctor.v1');
  assert.equal(structured(workAdmissionResponses, 22).count, 0);
  const admission = structured(workAdmissionResponses, 23).result;
  assert.equal(admission.status, 'created');
  assert.ok(['created', 'reused'].includes(structured(workAdmissionResponses, 24).result.status));
  assert.equal(structured(workAdmissionResponses, 25).schema, 'narada.work_lifecycle.ticket.v1');
  assert.ok(structured(workAdmissionResponses, 26).sources.length >= 1);

  const workResponses = run('work', workRoot, [
    tool(27, 'ticket_processing_context_load', { ticket_id: admission.ticket_id, triggering_event_id: admission.event_id, idempotency_key: 'native-proof-context' }),
    tool(28, 'ticket_admit_proposal', { ticket_id: admission.ticket_id, expected_revision: admission.ticket_revision, route: 'followup_task', idempotency_key: 'native-proof-proposal', causation_id: 'native-proof', actor_id: 'native.proof', summary: 'Create follow-up task', task: { title: 'Native work follow-up', goal: 'Prove shared task authority', required_work: 'Persist follow-up', acceptance_criteria: ['Task exists'] } }),
    tool(29, 'work_outbox_consumer_register', { topic: 'work.ticket-work-due.v1', consumer_id: 'native.proof' }),
    tool(30, 'work_outbox_list', { consumer_id: 'native.proof', limit: 10 }),
    tool(31, 'work_lifecycle_storage_inspect'),
  ]);
  assert.equal(structured(workResponses, 27).result.schema, 'narada.work_lifecycle.ticket_processing_context.v1');
  assert.equal(structured(workResponses, 28).result.schema, 'narada.work_lifecycle.ticket_proposal.v1');
  assert.equal(structured(workResponses, 29).status, 'registered');
  const events = structured(workResponses, 30).events;
  assert.ok(events.length >= 1);
  assert.equal(structured(workResponses, 31).status, 'ok');
  const finalWork = run('work', workRoot, [
    tool(32, 'work_outbox_ack', { event_id: events[0].event_id, consumer_id: 'native.proof', receipt: { status: 'processed' } }),
    tool(33, 'work_outbox_compact', { before: '2999-01-01T00:00:00Z' }),
  ]);
  assert.equal(structured(finalWork, 32).status, 'acknowledged');
  assert.ok(Number.isInteger(structured(finalWork, 33).compacted));

  process.stdout.write(`${JSON.stringify({ schema: 'narada.mcp_lifecycle_native.protocol_proof.v1', status: 'passed', task_tools: taskTools.length, work_tools: workTools.length, verified: ['all_schemas_named_closed_bounded', 'all_tools_invalid_input', 'task_create_retry_claim_finish_evidence_close_read_page_search', 'payload_create_show_derive_validate', 'work_ticket_create_retry_show_sources_context_proposal', 'work_outbox_register_list_ack_compact', 'cross_process_persistence'] })}\n`);
} finally {
  rmSync(taskRoot, { recursive: true, force: true });
  rmSync(workRoot, { recursive: true, force: true });
}
