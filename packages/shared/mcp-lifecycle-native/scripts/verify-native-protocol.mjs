import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const extension = process.platform === 'win32' ? '.exe' : '';
const workspaceBinDir = new URL('../../../../target/debug/', import.meta.url).pathname.replace(/^\/(?:([A-Za-z]:))/, '$1');
const binDir = resolve(process.env.NARADA_LIFECYCLE_NATIVE_BIN_DIR ?? (existsSync(workspaceBinDir) ? workspaceBinDir : 'target/debug'));
const executable = (surface) => join(binDir, `narada-${surface}-lifecycle-mcp${extension}`);
const rpc = (id, method, params = {}) => ({ jsonrpc: '2.0', id, method, params });
const tool = (id, name, args = {}) => rpc(id, 'tools/call', { name, arguments: args });
const successfulTools = { task: new Set(), work: new Set() };
const canonical = (value) => Array.isArray(value) ? value.map(canonical) : value && typeof value === 'object' ? Object.fromEntries(Object.keys(value).sort().map((key) => [key, canonical(value[key])])) : value;
const sha256 = (value) => createHash('sha256').update(JSON.stringify(canonical(value))).digest('hex');

function run(surface, root, requests = [], extraArgs = []) {
  const input = requests.length ? `${requests.map((request) => JSON.stringify(request)).join('\n')}\n` : '';
  const result = spawnSync(executable(surface), [...extraArgs, '--site-root', root], {
    input, encoding: 'utf8', windowsHide: true, timeout: 120000, maxBuffer: 4 * 1024 * 1024,
  });
  assert.equal(result.status, 0, `${surface}: ${String(result.stderr).slice(-2000)}`);
  const responses = String(result.stdout).split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
  for (const request of requests) {
    if (request.method !== 'tools/call') continue;
    const response = responses.find((candidate) => candidate.id === request.id);
    if (response && response.error === undefined) successfulTools[surface].add(request.params.name);
  }
  return responses;
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

  const taskReadResponses = run('task', taskRoot, [
    tool(40, 'task_lifecycle_doctor', { detail: 'full' }),
    tool(41, 'task_lifecycle_restart', { mode: 'status' }),
    tool(42, 'task_lifecycle_list', { status: 'closed', limit: 1, offset: 0 }),
    tool(43, 'task_lifecycle_diagnose_task_ref', { task_number: 1 }),
    tool(44, 'task_lifecycle_roster'),
    tool(45, 'task_lifecycle_next', { agent_id: 'native.proof', limit: 1 }),
    tool(46, 'task_lifecycle_workboard_snapshot', { agent_id: 'native.proof', limit: 1 }),
    tool(47, 'task_lifecycle_obligations', { agent_id: 'native.proof', limit: 1 }),
    tool(48, 'task_lifecycle_inspect', { task_number: 1 }),
    tool(49, 'task_lifecycle_evidence_preflight', { task_number: 1 }),
    tool(50, 'task_lifecycle_audit'),
    tool(51, 'task_lifecycle_related', { task_number: 1, limit: 1 }),
    tool(52, 'task_lifecycle_guidance', { detail: 'compact' }),
    tool(53, 'task_lifecycle_payload_schema', { tool: 'task_lifecycle_create' }),
    tool(54, 'task_lifecycle_inspect_range', { start_task_number: 1, end_task_number: 1, limit: 1 }),
    tool(55, 'task_lifecycle_bridge_poll', { dry_run: true, limit: 1 }),
  ]);
  for (let id = 40; id <= 55; id += 1) structured(taskReadResponses, id);
  const restartSameProcess = run('task', taskRoot, [tool(56, 'task_lifecycle_restart', { mode: 'request', reason: 'Native recovery protocol proof' }), tool(57, 'task_lifecycle_restart', { mode: 'acknowledge' })]);
  assert.equal(structured(restartSameProcess, 56).status, 'restart_requested');
  assert.equal(structured(restartSameProcess, 57).status, 'restart_acknowledgement_rejected');
  const restartFreshProcess = run('task', taskRoot, [tool(58, 'task_lifecycle_restart', { mode: 'acknowledge' })]);
  assert.equal(structured(restartFreshProcess, 58).status, 'restart_acknowledged');

  const taskTransitionResponses = run('task', taskRoot, [
    tool(60, 'task_lifecycle_roster_admit', { agent_id: 'native.proof', role: 'resident', actor_agent_id: 'native.proof', capabilities: ['native-proof'], authority_basis: { kind: 'operator_request', summary: 'Native lifecycle protocol proof' }, reason: 'Exercise native roster authority' }),
    tool(61, 'mcp_payload_create', { payload_id: 'native-proof-transitions', payload: { title: 'Native transition proof', goal: 'Exercise lifecycle transitions', required_work: ['Exercise transition commands'], acceptance_criteria: ['Every transition succeeds'], idempotency_key: 'native-proof-transitions' }, created_by: 'native.proof' }),
    tool(62, 'task_lifecycle_create', { payload_ref: 'mcp_payload:native-proof-transitions@v1' }),
    tool(63, 'task_lifecycle_tags_update', { task_number: 2, agent_id: 'native.proof', tags: ['native-proof'], reason: 'Exercise tag replacement' }),
    tool(64, 'task_lifecycle_claim', { task_number: 2, agent_id: 'native.proof' }),
    tool(65, 'task_lifecycle_continue', { task_number: 2, agent_id: 'native.proof', reason: 'handoff' }),
    tool(66, 'task_lifecycle_unclaim', { task_number: 2, agent_id: 'native.proof', reason: 'Exercise release' }),
    tool(67, 'task_lifecycle_defer', { task_number: 2, agent_id: 'native.proof', reason: 'Exercise deferral' }),
    tool(68, 'task_lifecycle_un_defer', { task_number: 2, agent_id: 'native.proof', reason: 'Exercise resumption' }),
    tool(69, 'task_lifecycle_set_routing', { task_number: 2, actor_agent_id: 'native.proof', target_role: 'resident', preferred_agent_id: 'native.proof', relative_priority: 1, reason: 'Exercise routing' }),
    tool(70, 'task_lifecycle_report_blocked', { task_number: 2, agent_id: 'native.proof', reason: 'Synthetic recoverable blocker', blockers: [{ kind: 'synthetic', summary: 'Protocol proof' }], next_action: 'Resume immediately', defer: true }),
    tool(71, 'task_lifecycle_reopen', { task_number: 2, agent_id: 'native.proof', reason: 'Exercise reopen' }),
  ]);
  for (let id = 60; id <= 71; id += 1) structured(taskTransitionResponses, id);

  const authorityBasis = { kind: 'operator_request', summary: 'Native lifecycle protocol proof' };
  const recurringCreateResponses = run('task', taskRoot, [
    tool(80, 'task_lifecycle_recurring_create', { title: 'Native recurring proof', actor_agent_id: 'native.proof', authority_basis: authorityBasis, goal: 'Exercise recurring task authority', required_work: 'Create one governed occurrence', acceptance_criteria: ['Occurrence is durable'], trigger_description: 'Daily native protocol proof', trigger_mode: 'schedule', schedule_kind: 'daily', schedule_timezone: 'UTC', initial_status: 'active' }),
  ]);
  const recurrenceId = structured(recurringCreateResponses, 80).recurrence_id;
  assert.ok(recurrenceId);
  const recurringResponses = run('task', taskRoot, [
    tool(81, 'task_lifecycle_recurring_show', { recurrence_id: recurrenceId, include_runs: true }),
    tool(82, 'task_lifecycle_recurring_list', { status: 'active', limit: 1, offset: 0, compact: true }),
    tool(83, 'task_lifecycle_recurring_trigger', { recurrence_id: recurrenceId, actor_agent_id: 'native.proof', authority_basis: authorityBasis, run_reason: 'Native protocol proof' }),
    tool(84, 'task_lifecycle_recurring_runs', { recurrence_id: recurrenceId, limit: 10 }),
    tool(85, 'task_lifecycle_recurring_suspend', { recurrence_id: recurrenceId, actor_agent_id: 'native.proof', authority_basis: authorityBasis, reason: 'Exercise suspension' }),
    tool(86, 'task_lifecycle_recurring_run_due', { actor_agent_id: 'native.proof', authority_basis: authorityBasis, current_time: '2030-01-01T00:00:00Z', limit: 10 }),
    tool(87, 'task_lifecycle_recurring_retire', { recurrence_id: recurrenceId, actor_agent_id: 'native.proof', authority_basis: authorityBasis, reason: 'Protocol proof complete' }),
  ]);
  for (let id = 81; id <= 87; id += 1) structured(recurringResponses, id);

  const executabilityRequestResponses = run('task', taskRoot, [
    tool(90, 'task_lifecycle_executability_request', { task_number: 2, agent_id: 'native.proof' }),
    tool(91, 'task_lifecycle_executability_status', { task_number: 2, include_assessment: true }),
    tool(92, 'task_lifecycle_executability_requests_next', { consumer_id: 'native.proof.evaluator', lease_duration_minutes: 10, limit: 1 }),
  ]);
  const executabilityRequest = structured(executabilityRequestResponses, 90);
  structured(executabilityRequestResponses, 91);
  const leased = structured(executabilityRequestResponses, 92).leased[0];
  assert.equal(leased.request_id, executabilityRequest.request_id);
  const executabilityResponses = run('task', taskRoot, [
    tool(93, 'task_lifecycle_executability_complete', { request_id: leased.request_id, assessment: { task_id: leased.task_id, task_number: leased.task_number, task_spec_digest: leased.task_spec_digest, environment_digest: leased.environment_digest, verdict: 'executable', findings: [], evaluator: { profile: leased.evaluator_profile, profile_version: leased.evaluator_profile_version, cognition: 'low' }, created_at: '2030-01-01T00:00:00Z' } }),
    tool(94, 'task_lifecycle_executability_override', { task_number: 2, agent_id: 'native.proof', reason: 'Exercise explicit dispatch override', authority_basis: authorityBasis }),
    tool(95, 'task_lifecycle_executability_dispatch_check', { task_number: 2 }),
  ]);
  assert.equal(structured(executabilityResponses, 93).status, 'completed');
  assert.equal(structured(executabilityResponses, 94).status, 'admitted');
  structured(executabilityResponses, 95);

  const taskAuxiliaryResponses = run('task', taskRoot, [
    tool(100, 'task_lifecycle_self_certification_preflight', { self_certification: { target_category: 'ordinary_task', subject_principal: 'native.proof', actor_principal: 'native.proof', requires_independent_review: false } }),
    tool(101, 'task_lifecycle_record_observation', { task_number: 2, artifact_uri: 'urn:narada:native-proof:observation:one', content: { result: 'observed' }, source_operator: 'native.proof', agent_id: 'native.proof' }),
    tool(102, 'task_lifecycle_submit_observation', { task_number: 2, artifact_uri: 'urn:narada:native-proof:observation:two', content: { result: 'submitted' }, source_operator: 'native.proof', agent_id: 'native.proof' }),
    tool(103, 'task_lifecycle_chapter_add_task', { chapter_id: 'native-proof', task_number: 2, append: true, note: 'Native protocol proof', actor_agent_id: 'native.proof' }),
    tool(104, 'task_lifecycle_chapter_show', { chapter_id: 'native-proof' }),
    tool(105, 'task_lifecycle_compatibility_reconcile', { agent_id: 'native.proof', task_numbers: [1, 2], limit: 2, dry_run: true }),
  ]);
  for (let id = 100; id <= 105; id += 1) structured(taskAuxiliaryResponses, id);

  const dependencySetupResponses = run('task', taskRoot, [
    tool(110, 'mcp_payload_create', { payload_id: 'native-proof-parent', payload: { title: 'Native dependency parent', goal: 'Exercise dependency authority', required_work: ['Track required outcome'], acceptance_criteria: ['Dependency is recorded'], idempotency_key: 'native-proof-parent' }, created_by: 'native.proof' }),
    tool(111, 'task_lifecycle_create', { payload_ref: 'mcp_payload:native-proof-parent@v1' }),
    tool(112, 'mcp_payload_create', { payload_id: 'native-proof-required', payload: { title: 'Native dependency requirement', goal: 'Supply required outcome', required_work: ['Produce outcome'], acceptance_criteria: ['Outcome is durable'], idempotency_key: 'native-proof-required' }, created_by: 'native.proof' }),
    tool(113, 'task_lifecycle_create', { payload_ref: 'mcp_payload:native-proof-required@v1' }),
    tool(114, 'task_lifecycle_dependency_declare', { parent_task_number: 4, required_task_number: 5, agent_id: 'native.proof', kind: 'downstream_work', satisfying_outcomes: ['completed'] }),
  ]);
  const dependency = structured(dependencySetupResponses, 114);
  const dependencyId = dependency.dependency_id ?? dependency.dependency?.dependency_id;
  assert.ok(dependencyId);
  const dependencyResponses = run('task', taskRoot, [
    tool(115, 'task_lifecycle_claim', { task_number: 5, agent_id: 'native.proof' }),
    tool(116, 'task_lifecycle_finish', { task_number: 5, agent_id: 'native.proof', summary: 'Required outcome supplied', outcome: 'completed', no_files_changed: true }),
    tool(117, 'task_lifecycle_dependency_dispose', { dependency_id: dependencyId, agent_id: 'native.proof', kind: 'operator_deferred', summary: 'Exercise governed dependency disposition', status: 'deferred', authority_basis: authorityBasis }),
  ]);
  for (let id = 115; id <= 117; id += 1) structured(dependencyResponses, id);

  const submitSetupResponses = run('task', taskRoot, [
    tool(120, 'mcp_payload_create', { payload_id: 'native-proof-submit-work', payload: { title: 'Native submit-work proof', goal: 'Exercise the compound native workflow', required_work: ['Submit evidence through one public call'], acceptance_criteria: ['Compound workflow reaches review'], idempotency_key: 'native-proof-submit-work' }, created_by: 'native.proof' }),
    tool(121, 'task_lifecycle_create', { payload_ref: 'mcp_payload:native-proof-submit-work@v1' }),
    tool(122, 'task_lifecycle_submit_work', { task_number: 6, agent_id: 'native.proof', summary: 'Completed native compound workflow proof', execution_notes: 'Implemented and exercised the native compound submit-work orchestration.', verification: 'Verified claim, criteria proof, evidence admission, and finish through public MCP.', no_files_changed: true }),
  ]);
  const submitWork = structured(submitSetupResponses, 122);
  assert.equal(submitWork.status, 'submitted');
  const finishPrimitive = submitWork.primitive_results.find(({ tool: name }) => name === 'task_lifecycle_finish');
  const submitReportId = finishPrimitive?.result?.report_id;
  assert.ok(submitReportId);
  const submitFollowupResponses = run('task', taskRoot, [
    tool(123, 'task_lifecycle_prove_criteria', { task_number: 6, agent_id: 'native.proof' }),
    tool(124, 'task_lifecycle_evidence_supersede', { task_number: 6, agent_id: 'native.proof', supersedes_report_id: submitReportId, artifact_uri: 'urn:narada:native-proof:evidence:replacement', summary: 'Replacement native evidence', verification_summary: 'Replacement evidence was verified through public MCP.', no_files_changed: true }),
    tool(125, 'task_lifecycle_review', { task_number: 6, agent_id: 'native.proof', verdict: 'accepted', findings: [], single_operator_review: true }),
    tool(126, 'task_lifecycle_closeout', { task_number: 2, agent_id: 'native.proof', dry_run: true, summary: 'Plan native closeout' }),
    tool(127, 'task_lifecycle_disposition_closeout', { task_number: 2, agent_id: 'native.proof', dry_run: true, disposition: 'operator_deferred', summary: 'Plan native disposition closeout' }),
    tool(128, 'task_lifecycle_inbox_target', { envelope_id: 'native-proof-envelope', dry_run: true, disposition: 'targeted', agent_id: 'native.proof' }),
    tool(129, 'task_lifecycle_test_mcp_tool', { server_path: 'native-proof-server', tool_name: 'native-proof-tool', arguments: {}, timeout_seconds: 1 }),
    tool(130, 'task_lifecycle_run_tests', { selector: 'native-proof', task_number: 6, agent_id: 'native.proof', timeout_seconds: 1 }),
  ]);
  for (let id = 123; id <= 130; id += 1) structured(submitFollowupResponses, id);

  const reportSetupResponses = run('task', taskRoot, [
    tool(131, 'mcp_payload_create', { payload_id: 'native-proof-submit-report', payload: { title: 'Native submit-report proof', goal: 'Exercise the report alias', required_work: ['Submit a report'], acceptance_criteria: ['Report is durable'], idempotency_key: 'native-proof-submit-report' }, created_by: 'native.proof' }),
    tool(132, 'task_lifecycle_create', { payload_ref: 'mcp_payload:native-proof-submit-report@v1' }),
    tool(133, 'task_lifecycle_claim', { task_number: 7, agent_id: 'native.proof' }),
    tool(134, 'task_lifecycle_submit_report', { task_number: 7, agent_id: 'native.proof', summary: 'Native submit-report proof complete', no_files_changed: true }),
  ]);
  for (let id = 131; id <= 134; id += 1) structured(reportSetupResponses, id);

  const submitRecoveryResponses = run('task', taskRoot, [
    tool(138, 'task_lifecycle_submit_work', { task_number: 6, agent_id: 'native.proof', resume_existing_work: true, claim: false, prove_criteria: false, admit_evidence: false, finish: false }),
    tool(139, 'mcp_payload_create', { payload_id: 'native-proof-auto-submit', payload: { title: 'Native auto-payload submit proof', goal: 'Exercise one-call payload materialization', required_work: ['Submit work with an immutable companion payload'], acceptance_criteria: ['Payload source is reported'], idempotency_key: 'native-proof-auto-submit' }, created_by: 'native.proof' }),
    tool(140, 'task_lifecycle_create', { payload_ref: 'mcp_payload:native-proof-auto-submit@v1' }),
    tool(141, 'task_lifecycle_submit_work', { task_number: 8, agent_id: 'native.proof', summary: 'Completed auto-payload native proof', execution_notes: 'Exercised automatic immutable payload materialization in native submit-work.', verification: 'Verified the returned payload source and compound lifecycle result.', no_files_changed: true, auto_materialize_payload: true }),
  ]);
  assert.equal(structured(submitRecoveryResponses, 138).status, 'submitted');
  assert.equal(structured(submitRecoveryResponses, 141).payload_source.kind, 'auto_materialized_payload');

  const largePayload = 'native-output-proof-'.repeat(2000);
  const outputSetupResponses = run('task', taskRoot, [
    tool(135, 'mcp_payload_create', { payload_id: 'native-proof-large-output', payload: { content: largePayload }, created_by: 'native.proof' }),
    tool(136, 'mcp_payload_show', { ref: 'mcp_payload:native-proof-large-output@v1' }),
  ]);
  const materialized = structured(outputSetupResponses, 136);
  assert.equal(materialized.result_materialized, true);
  const outputReadResponses = run('task', taskRoot, [tool(137, 'mcp_output_show', { ref: materialized.output_ref, offset: 0, limit: 100 })]);
  structured(outputReadResponses, 137);

  run('work', workRoot, [], ['--prepare']);
  const workCatalogResponse = run('work', workRoot, [rpc(20, 'tools/list')]);
  const workTools = workCatalogResponse[0]?.result?.tools ?? [];
  assertCatalog('work', workTools, 80);
  assertEveryToolRejectsUnknown('work', workRoot, workTools);
  let workCallId = 2000;
  const workToolSchema = new Map(workTools.map((entry) => [entry.name, entry.inputSchema]));
  const workTaskRevision = (taskNumber) => {
    const id = workCallId++;
    const response = structured(run('work', workRoot, [tool(id, 'task_lifecycle_show', { task_number: taskNumber })]), id);
    if (Number.isInteger(response.lifecycle?.revision)) return response.lifecycle.revision;
    const revision = String(response.output_text ?? '').match(/"revision"\s*:\s*(\d+)/)?.[1];
    assert.ok(revision, `materialized task ${taskNumber} preview omitted lifecycle revision`);
    return Number(revision);
  };
  const callWork = (name, suppliedArgs = {}) => {
    const args = { ...suppliedArgs };
    const properties = workToolSchema.get(name)?.properties ?? {};
    for (const [revisionField, taskField] of [['expected_revision', 'task_number'], ['expected_parent_revision', 'parent_task_number'], ['expected_required_revision', 'required_task_number']]) {
      if (properties[revisionField] && Number.isInteger(args[taskField])) args[revisionField] = workTaskRevision(args[taskField]);
    }
    const id = workCallId++;
    return structured(run('work', workRoot, [tool(id, name, args)]), id);
  };
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

  callWork('task_lifecycle_roster_admit', { agent_id: 'native.proof', role: 'resident', actor_agent_id: 'native.proof', capabilities: ['native-proof'], authority_basis: authorityBasis, reason: 'Exercise work-hosted task authority' });
  const workPayload = callWork('mcp_payload_create', { payload_id: 'native-proof-work-task', payload: { title: 'Work-hosted task proof', goal: 'Exercise task tools through work lifecycle', required_work: ['Exercise shared native authority'], acceptance_criteria: ['Shared authority succeeds'], idempotency_key: 'native-proof-work-task' }, created_by: 'native.proof' });
  const workTask = callWork('task_lifecycle_create', { payload_ref: workPayload.ref });
  const workTaskNumber = workTask.task_number;
  callWork('mcp_payload_show', { ref: workPayload.ref });
  const derivedWorkPayload = callWork('mcp_payload_derive', { source_ref: workPayload.ref, overlay: { summary: 'work-derived' }, created_by: 'native.proof' });
  callWork('mcp_payload_validate', { ref: derivedWorkPayload.ref });
  callWork('task_lifecycle_list', { limit: 1, offset: 0 });
  callWork('task_lifecycle_tags_update', { task_number: workTaskNumber, agent_id: 'native.proof', tags: ['native-proof'], reason: 'Exercise work-hosted tags' });
  callWork('task_lifecycle_diagnose_task_ref', { task_number: workTaskNumber });
  callWork('task_lifecycle_roster');
  callWork('task_lifecycle_claim', { task_number: workTaskNumber, agent_id: 'native.proof' });
  callWork('task_lifecycle_continue', { task_number: workTaskNumber, agent_id: 'native.proof', reason: 'handoff' });
  callWork('task_lifecycle_unclaim', { task_number: workTaskNumber, agent_id: 'native.proof', reason: 'Exercise work-hosted release' });
  callWork('task_lifecycle_next', { agent_id: 'native.proof', limit: 1 });
  callWork('task_lifecycle_workboard_snapshot', { agent_id: 'native.proof', limit: 1 });
  callWork('task_lifecycle_obligations', { agent_id: 'native.proof', limit: 1 });
  callWork('task_lifecycle_inspect', { task_number: workTaskNumber });
  callWork('task_lifecycle_evidence_preflight', { task_number: workTaskNumber });
  callWork('task_lifecycle_self_certification_preflight', { self_certification: { target_category: 'ordinary_task', actor_principal: 'native.proof' } });
  callWork('task_lifecycle_audit');
  callWork('task_lifecycle_search', { query: 'Work-hosted', limit: 1 });
  callWork('task_lifecycle_related', { task_number: workTaskNumber, limit: 1 });
  callWork('task_lifecycle_defer', { task_number: workTaskNumber, agent_id: 'native.proof', reason: 'Exercise work-hosted defer' });
  callWork('task_lifecycle_un_defer', { task_number: workTaskNumber, agent_id: 'native.proof', reason: 'Exercise work-hosted resume' });
  callWork('task_lifecycle_set_routing', { task_number: workTaskNumber, actor_agent_id: 'native.proof', target_role: 'resident', preferred_agent_id: 'native.proof', reason: 'Exercise work-hosted routing' });
  callWork('task_lifecycle_report_blocked', { task_number: workTaskNumber, agent_id: 'native.proof', reason: 'Synthetic work-hosted blocker', defer: true });
  callWork('task_lifecycle_reopen', { task_number: workTaskNumber, agent_id: 'native.proof', reason: 'Exercise work-hosted reopen' });
  callWork('task_lifecycle_record_observation', { task_number: workTaskNumber, artifact_uri: 'urn:narada:native-proof:work-observation:one', content: { result: 'observed' }, agent_id: 'native.proof' });
  callWork('task_lifecycle_submit_observation', { task_number: workTaskNumber, artifact_uri: 'urn:narada:native-proof:work-observation:two', content: { result: 'submitted' }, agent_id: 'native.proof' });
  callWork('task_lifecycle_bridge_poll', { dry_run: true, limit: 1 });
  callWork('task_lifecycle_inbox_target', { envelope_id: 'native-proof-work-envelope', dry_run: true, agent_id: 'native.proof' });
  callWork('task_lifecycle_guidance', { detail: 'compact' });
  callWork('task_lifecycle_payload_schema', { tool: 'task_lifecycle_create' });
  callWork('task_lifecycle_inspect_range', { start_task_number: workTaskNumber, end_task_number: workTaskNumber, limit: 1 });
  callWork('task_lifecycle_chapter_add_task', { chapter_id: 'native-proof-work', task_number: workTaskNumber, append: true, actor_agent_id: 'native.proof' });
  callWork('task_lifecycle_chapter_show', { chapter_id: 'native-proof-work' });
  callWork('task_lifecycle_test_mcp_tool', { server_path: 'native-proof-server', tool_name: 'native-proof-tool' });
  callWork('task_lifecycle_run_tests', { selector: 'native-proof', task_number: workTaskNumber, agent_id: 'native.proof', timeout_seconds: 1 });

  callWork('task_lifecycle_claim', { task_number: workTaskNumber, agent_id: 'native.proof' });
  callWork('task_lifecycle_prove_criteria', { task_number: workTaskNumber, agent_id: 'native.proof' });
  callWork('task_lifecycle_admit_evidence', { task_number: workTaskNumber, agent_id: 'native.proof' });
  callWork('task_lifecycle_finish', { task_number: workTaskNumber, agent_id: 'native.proof', summary: 'Work-hosted finish proof complete', no_files_changed: true });
  callWork('task_lifecycle_review', { task_number: workTaskNumber, agent_id: 'native.proof', verdict: 'accepted', findings: [], single_operator_review: true });
  callWork('task_lifecycle_close', { task_number: workTaskNumber, agent_id: 'native.proof', mode: 'operator_direct' });
  callWork('task_lifecycle_closeout', { task_number: workTaskNumber, agent_id: 'native.proof', dry_run: true, summary: 'Plan work-hosted closeout' });
  callWork('task_lifecycle_disposition_closeout', { task_number: workTaskNumber, agent_id: 'native.proof', dry_run: true, disposition: 'operator_deferred', summary: 'Plan work-hosted disposition' });

  const workRecurrence = callWork('task_lifecycle_recurring_create', { title: 'Work-hosted recurring proof', actor_agent_id: 'native.proof', authority_basis: authorityBasis, goal: 'Exercise shared recurrence authority', required_work: 'Create one occurrence', acceptance_criteria: ['Occurrence is durable'], trigger_mode: 'schedule', schedule_kind: 'daily', schedule_timezone: 'UTC', initial_status: 'active' });
  const workRecurrenceId = workRecurrence.recurrence_id;
  callWork('task_lifecycle_recurring_show', { recurrence_id: workRecurrenceId, include_runs: true });
  callWork('task_lifecycle_recurring_list', { status: 'active', limit: 1, offset: 0, compact: true });
  callWork('task_lifecycle_recurring_trigger', { recurrence_id: workRecurrenceId, actor_agent_id: 'native.proof', authority_basis: authorityBasis, run_reason: 'Work-hosted protocol proof' });
  callWork('task_lifecycle_recurring_runs', { recurrence_id: workRecurrenceId, limit: 10 });
  callWork('task_lifecycle_recurring_suspend', { recurrence_id: workRecurrenceId, actor_agent_id: 'native.proof', authority_basis: authorityBasis, reason: 'Exercise suspension' });
  callWork('task_lifecycle_recurring_run_due', { actor_agent_id: 'native.proof', authority_basis: authorityBasis, current_time: '2030-01-01T00:00:00Z', limit: 10 });
  callWork('task_lifecycle_recurring_retire', { recurrence_id: workRecurrenceId, actor_agent_id: 'native.proof', authority_basis: authorityBasis, reason: 'Protocol proof complete' });

  const workExecutablePayload = callWork('mcp_payload_create', { payload_id: 'native-proof-work-executable', payload: { title: 'Work-hosted executability proof', goal: 'Exercise executability authority', required_work: ['Evaluate executability'], acceptance_criteria: ['Assessment is durable'], idempotency_key: 'native-proof-work-executable' }, created_by: 'native.proof' });
  const workExecutableTask = callWork('task_lifecycle_create', { payload_ref: workExecutablePayload.ref });
  const workExecutableNumber = workExecutableTask.task_number;
  const workExecutionRequest = callWork('task_lifecycle_executability_request', { task_number: workExecutableNumber, agent_id: 'native.proof' });
  callWork('task_lifecycle_executability_status', { task_number: workExecutableNumber, include_assessment: true });
  const workLease = callWork('task_lifecycle_executability_requests_next', { consumer_id: 'native.proof.evaluator', lease_duration_minutes: 10, limit: 1 }).leased[0];
  callWork('task_lifecycle_executability_complete', { request_id: workExecutionRequest.request_id, assessment: { task_id: workLease.task_id, task_number: workLease.task_number, task_spec_digest: workLease.task_spec_digest, environment_digest: workLease.environment_digest, verdict: 'executable', findings: [], evaluator: { profile: workLease.evaluator_profile, profile_version: workLease.evaluator_profile_version, cognition: 'low' }, created_at: '2030-01-01T00:00:00Z' } });
  callWork('task_lifecycle_executability_override', { task_number: workExecutableNumber, agent_id: 'native.proof', reason: 'Exercise work-hosted override', authority_basis: authorityBasis });
  callWork('task_lifecycle_executability_dispatch_check', { task_number: workExecutableNumber });

  const workSubmitPayload = callWork('mcp_payload_create', { payload_id: 'native-proof-work-submit', payload: { title: 'Work-hosted submit-work proof', goal: 'Exercise compound authority through work lifecycle', required_work: ['Submit evidence in one call'], acceptance_criteria: ['Compound workflow reaches review'], idempotency_key: 'native-proof-work-submit' }, created_by: 'native.proof' });
  const workSubmitTask = callWork('task_lifecycle_create', { payload_ref: workSubmitPayload.ref });
  const workSubmitNumber = workSubmitTask.task_number;
  const workSubmit = callWork('task_lifecycle_submit_work', { task_number: workSubmitNumber, agent_id: 'native.proof', summary: 'Completed work-hosted compound proof', execution_notes: 'Implemented the work-hosted native compound submit-work protocol proof.', verification: 'Verified every compound primitive through the public work MCP surface.', no_files_changed: true });
  const workSubmitReport = workSubmit.primitive_results.find(({ tool: name }) => name === 'task_lifecycle_finish')?.result?.report_id;
  assert.ok(workSubmitReport);
  callWork('task_lifecycle_evidence_supersede', { task_number: workSubmitNumber, agent_id: 'native.proof', supersedes_report_id: workSubmitReport, artifact_uri: 'urn:narada:native-proof:work-evidence:replacement', summary: 'Replacement work-hosted evidence', verification_summary: 'Replacement evidence was verified through public MCP.', no_files_changed: true });

  const workReportPayload = callWork('mcp_payload_create', { payload_id: 'native-proof-work-report', payload: { title: 'Work-hosted submit-report proof', goal: 'Exercise report authority', required_work: ['Submit a report'], acceptance_criteria: ['Report is durable'], idempotency_key: 'native-proof-work-report' }, created_by: 'native.proof' });
  const workReportTask = callWork('task_lifecycle_create', { payload_ref: workReportPayload.ref });
  callWork('task_lifecycle_claim', { task_number: workReportTask.task_number, agent_id: 'native.proof' });
  callWork('task_lifecycle_submit_report', { task_number: workReportTask.task_number, agent_id: 'native.proof', summary: 'Work-hosted submit-report proof complete', no_files_changed: true });

  const workParentPayload = callWork('mcp_payload_create', { payload_id: 'native-proof-work-parent', payload: { title: 'Work-hosted dependency parent', goal: 'Exercise dependency authority', required_work: ['Track required outcome'], acceptance_criteria: ['Dependency is recorded'], idempotency_key: 'native-proof-work-parent' }, created_by: 'native.proof' });
  const workParentTask = callWork('task_lifecycle_create', { payload_ref: workParentPayload.ref });
  const workRequiredPayload = callWork('mcp_payload_create', { payload_id: 'native-proof-work-required', payload: { title: 'Work-hosted dependency requirement', goal: 'Supply required outcome', required_work: ['Produce required outcome'], acceptance_criteria: ['Outcome is durable'], idempotency_key: 'native-proof-work-required' }, created_by: 'native.proof' });
  const workRequiredTask = callWork('task_lifecycle_create', { payload_ref: workRequiredPayload.ref });
  const workDependency = callWork('task_lifecycle_dependency_declare', { parent_task_number: workParentTask.task_number, required_task_number: workRequiredTask.task_number, agent_id: 'native.proof', kind: 'downstream_work', satisfying_outcomes: ['completed'] });
  const workDependencyId = workDependency.dependency_id ?? workDependency.dependency?.dependency_id;
  assert.ok(workDependencyId);
  callWork('task_lifecycle_claim', { task_number: workRequiredTask.task_number, agent_id: 'native.proof' });
  callWork('task_lifecycle_finish', { task_number: workRequiredTask.task_number, agent_id: 'native.proof', summary: 'Work-hosted required outcome supplied', outcome: 'completed', no_files_changed: true });
  callWork('task_lifecycle_dependency_disposition_record', { dependency_id: workDependencyId, agent_id: 'native.proof', kind: 'operator_deferred', summary: 'Exercise work-hosted dependency disposition', status: 'deferred', authority_basis: authorityBasis });

  const workLargePayload = callWork('mcp_payload_create', { payload_id: 'native-proof-work-large-output', payload: { content: 'native-work-output-proof-'.repeat(2000) }, created_by: 'native.proof' });
  const workMaterialized = callWork('mcp_payload_show', { ref: workLargePayload.ref });
  assert.equal(workMaterialized.result_materialized, true);
  callWork('mcp_output_show', { ref: workMaterialized.output_ref, offset: 0, limit: 100 });

  const mailboxAdmission = callWork('ticket_admit_source', { source_kind: 'mailbox_message', source_scope: 'native-proof-mailbox', immutable_source_id: 'native-proof-message', idempotency_key: 'native-proof-mailbox-ticket', causation_id: 'native-proof-mailbox-event', policy_version: 'v1', summary: 'Native mailbox draft proof', source_ref: { scope_id: 'native-proof-mailbox', mailbox_id: 'native-proof-mailbox', message_id: 'native-proof-message' }, correlation_keys: [] }).result;
  const mailboxTicket = callWork('ticket_show', { ticket_id: mailboxAdmission.ticket_id }).ticket;
  const draftProposal = callWork('ticket_admit_proposal', { ticket_id: mailboxAdmission.ticket_id, expected_revision: mailboxTicket.revision, route: 'response_draft', idempotency_key: 'native-proof-draft-proposal', causation_id: 'native-proof-draft-causation', actor_id: 'native.proof', summary: 'Admit native unsent response draft', draft: { source_id: mailboxAdmission.source_id, reply_mode: 'reply', body_text: 'This is an unsent native protocol proof response.' } }).result;
  const draftId = 'native-proof-draft';
  const draftReceiptArgs = { ticket_id: mailboxAdmission.ticket_id, effect_claim_id: draftProposal.effect_claim_id, draft_operation_key: draftProposal.draft_operation_key, draft_request_digest: draftProposal.draft_request_digest, receipt_id: 'native-proof-draft-receipt', draft_id: draftId, draft_ref: { draft_operation_key: draftProposal.draft_operation_key, mailbox_id: draftProposal.mailbox_id, draft_id: draftId, web_url: 'https://example.invalid/native-proof-draft' }, idempotency_key: 'native-proof-draft-receipt-operation', causation_id: 'native-proof-draft-receipt-causation' };
  const draftReceipt = callWork('ticket_draft_receipt_record', draftReceiptArgs);
  assert.equal(draftReceipt.result.status, 'recorded');
  assert.equal(callWork('ticket_draft_receipt_record', draftReceiptArgs).result.status, 'already_recorded');
  const dispositionEvidence = { schema: 'narada.graph_mail.ticket_draft_disposition_receipt.v1', disposition: 'sent', evidence_kind: 'synchronized_graph_observation', is_draft: false, observation_id: 'native-proof-draft-observation', evidence_id: 'native-proof-draft-observation', ticket_id: mailboxAdmission.ticket_id, effect_claim_id: draftProposal.effect_claim_id, draft_operation_key: draftProposal.draft_operation_key, mailbox_id: draftProposal.mailbox_id, draft_id: draftId, observed_message_id: 'native-proof-sent-message', observed_at: '2030-01-01T00:00:00Z' };
  dispositionEvidence.receipt_sha256 = sha256(dispositionEvidence);
  const draftDispositionArgs = { ticket_id: mailboxAdmission.ticket_id, draft_id: draftId, evidence: dispositionEvidence, idempotency_key: 'native-proof-draft-disposition', causation_id: 'native-proof-draft-disposition-causation' };
  const draftDisposition = callWork('ticket_draft_disposition_reconcile', draftDispositionArgs);
  assert.equal(draftDisposition.result.status, 'reconciled');
  assert.equal(callWork('ticket_draft_disposition_reconcile', draftDispositionArgs).result.status, 'already_reconciled');

  const taskPageOne = structured(run('task', taskRoot, [tool(3000, 'task_lifecycle_list', { limit: 1, offset: 0 })]), 3000);
  const taskPageTwo = structured(run('task', taskRoot, [tool(3001, 'task_lifecycle_list', { limit: 1, offset: 1 })]), 3001);
  assert.notEqual(taskPageOne.tasks[0]?.task_number, taskPageTwo.tasks[0]?.task_number);
  const workPageOne = callWork('task_lifecycle_list', { limit: 1, offset: 0 });
  const workPageTwo = callWork('task_lifecycle_list', { limit: 1, offset: 1 });
  assert.notEqual(workPageOne.tasks[0]?.task_number, workPageTwo.tasks[0]?.task_number);

  const taskNormalMissing = taskTools.map(({ name }) => name).filter((name) => !successfulTools.task.has(name));
  const workNormalMissing = workTools.map(({ name }) => name).filter((name) => !successfulTools.work.has(name));
  assert.deepEqual(taskNormalMissing, []);
  assert.deepEqual(workNormalMissing, []);
  process.stdout.write(`${JSON.stringify({ schema: 'narada.mcp_lifecycle_native.protocol_proof.v1', status: 'passed', task_tools: taskTools.length, work_tools: workTools.length, normal_coverage: { task: successfulTools.task.size, work: successfulTools.work.size, task_missing: taskNormalMissing, work_missing: workNormalMissing }, verified: ['all_tools_normal_path', 'all_schemas_named_closed_bounded', 'all_tools_invalid_input', 'task_create_retry_claim_finish_evidence_close_read_page_search', 'payload_create_show_derive_validate_and_materialized_output_read', 'restart_request_same_process_refusal_fresh_process_acknowledgement', 'work_ticket_create_retry_show_sources_context_proposal', 'work_draft_claim_receipt_retry_disposition_retry', 'work_outbox_register_list_ack_compact', 'task_and_work_pagination', 'cross_process_persistence'] })}\n`);
} finally {
  rmSync(taskRoot, { recursive: true, force: true });
  rmSync(workRoot, { recursive: true, force: true });
}
