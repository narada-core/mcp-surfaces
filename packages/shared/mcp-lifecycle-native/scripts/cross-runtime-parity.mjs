import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const surfacesRoot = resolve(packageRoot, '..', '..', '..');
const extension = process.platform === 'win32' ? '.exe' : '';
const entrypoints = {
  task: {
    node: join(surfacesRoot, 'packages', 'task-lifecycle-mcp', 'dist', 'src', 'task-lifecycle', 'task-mcp-server.js'),
    rust: join(packageRoot, 'dist', 'native', `narada-task-lifecycle-mcp${extension}`),
    protocol: '2026-04-18',
    server: 'narada-task-lifecycle-mcp',
  },
  work: {
    node: join(surfacesRoot, 'packages', 'work-lifecycle-mcp', 'dist', 'src', 'main.js'),
    rust: join(packageRoot, 'dist', 'native', `narada-work-lifecycle-mcp${extension}`),
    protocol: '2024-11-05',
    server: 'work-lifecycle-mcp',
  },
};

function parse(stdout, label) {
  const lines = String(stdout).trim().split(/\r?\n/).filter(Boolean);
  assert.ok(lines.length > 0, `${label}: no JSON-RPC output`);
  return lines.map((line) => JSON.parse(line));
}

function run(runtime, surface, root, input, prepare = false) {
  const spec = entrypoints[surface];
  const command = runtime === 'node' ? process.execPath : spec.rust;
  const args = runtime === 'node' ? [spec.node] : [];
  if (prepare) args.push('--prepare');
  args.push('--site-root', root);
  const result = spawnSync(command, args, { cwd: surfacesRoot, input, encoding: 'utf8', windowsHide: true });
  assert.equal(result.status, 0, `${runtime}/${surface}: ${result.stderr}`);
  return parse(result.stdout, `${runtime}/${surface}`);
}

const rpc = (id, method, params = {}) => JSON.stringify({ jsonrpc: '2.0', id, method, params });
const call = (id, name, args = {}) => rpc(id, 'tools/call', { name, arguments: args });
const structured = (line) => line?.result?.structuredContent ?? line?.result ?? {};
const responseCode = (line) => line?.error?.data?.code ?? line?.error?.message?.split(':', 1)[0] ?? null;
const byId = (lines) => new Map(lines.map((line) => [String(line.id), line]));

function taskInput() {
  return [
    rpc(1, 'initialize'),
    rpc(2, 'tools/list'),
    call(3, 'mcp_payload_create', { payload_id: 'paritytask', payload: { title: 'Parity task', goal: 'Exercise Rust authority', required_work: ['persist'], acceptance_criteria: ['durable'], idempotency_key: 'parity-task' }, created_by: 'cross-runtime' }),
    call(4, 'task_lifecycle_create', { payload_ref: 'mcp_payload:paritytask@v1' }),
    call(5, 'task_lifecycle_claim', { task_number: 1, agent_id: 'cross-runtime' }),
    call(6, 'task_lifecycle_roster_admit', { agent_id: 'cross-runtime', role: 'builder', actor_agent_id: 'cross-runtime', capabilities: [], authority_basis: { kind: 'operator_direct_instruction', summary: 'Admit parity actor' }, reason: 'Admit parity actor' }),
    call(7, 'task_lifecycle_disposition_closeout', { task_number: 1, agent_id: 'cross-runtime', disposition: 'acknowledged', summary: 'Parity closeout', no_files_changed: true }),
    call(8, 'task_lifecycle_prove_criteria', { task_number: 1, agent_id: 'cross-runtime' }),
    call(9, 'task_lifecycle_roster_admit', { agent_id: 'cross-runtime.reviewer', role: 'architect', actor_agent_id: 'cross-runtime', capabilities: ['architect_as_reviewer'], authority_basis: { kind: 'task_owner_handoff', summary: 'Cross-runtime parity reviewer' }, reason: 'Admit parity reviewer' }),
    call(10, 'task_lifecycle_finish', { task_number: 1, agent_id: 'cross-runtime', summary: 'Parity finish', no_files_changed: true, reviewer: 'cross-runtime.reviewer' }),
    call(11, 'task_lifecycle_admit_evidence', { task_number: 1, agent_id: 'cross-runtime' }),
    call(12, 'task_lifecycle_close', { task_number: 1, agent_id: 'cross-runtime', mode: 'operator_direct' }),
    call(13, 'task_lifecycle_show', { task_number: 1 }),
    rpc(14, 'resources/list'),
    rpc(15, 'prompts/list'),
    rpc(16, 'prompts/get', { name: 'task_lifecycle_workflow' }),
    rpc(17, 'completion/complete', { argument: { name: 'name' } }),
    rpc(18, 'completion/complete', { argument: { name: 'description' } }),
    call(19, '__missing_task_tool__'),
    rpc(20, 'resources/list'),
    rpc(21, 'resources/read', { uri: 'mcp-output:mcp_output%3Amissing' }),
    rpc(22, 'experimental/method'),
    call(23, 'task_lifecycle_doctor', { detail: 'summary' }),
    call(24, 'task_mcp_list'),
  ].join('\n') + '\n';
}

function workInput() {
  return [
    rpc(1, 'initialize'),
    rpc(2, 'tools/list'),
    call(3, 'ticket_admit_source', { source_kind: 'smoke', source_scope: 'native', immutable_source_id: 'parity-ticket', idempotency_key: 'parity-ticket', causation_id: 'cross-runtime', policy_version: 'v1', summary: 'Parity ticket', source_ref: {}, correlation_keys: [] }),
    call(4, 'ticket_admit_proposal', { ticket_id: 'ticket-1', expected_revision: 1, route: 'followup_task', idempotency_key: 'parity-proposal', causation_id: 'cross-runtime', actor_id: 'cross-runtime', summary: 'Create follow-up', task: { title: 'Parity follow-up', goal: 'Exercise shared task semantics', required_work: 'Persist follow-up', acceptance_criteria: ['exists'] } }),
    call(5, 'ticket_admit_proposal', { ticket_id: 'ticket-1', expected_revision: 1, route: 'blocked_operator', idempotency_key: 'parity-stale', causation_id: 'cross-runtime', actor_id: 'cross-runtime', summary: 'Stale revision must refuse' }),
    call(6, 'work_outbox_consumer_register', { topic: 'work.ticket-work-due.v1', consumer_id: 'cross-runtime' }),
    call(7, 'work_outbox_list', { consumer_id: 'cross-runtime', limit: 10 }),
    call(8, 'work_lifecycle_storage_inspect'),
    call(9, '__missing_work_tool__'),
    rpc(10, 'resources/list'),
    rpc(11, 'prompts/list'),
    rpc(12, 'completion/complete', { argument: { name: 'name' } }),
    rpc(13, 'experimental/method'),
    call(14, 'work_lifecycle_doctor'),
  ].join('\n') + '\n';
}

function protocol(lines, surface) {
  const responses = byId(lines);
  const init = responses.get('1')?.result;
  const list = responses.get('2')?.result;
  assert.equal(init?.protocolVersion, entrypoints[surface].protocol, `${surface}: protocol version drift`);
  assert.equal(init?.serverInfo?.name, entrypoints[surface].server, `${surface}: server name drift`);
  assert.equal(init?.serverInfo?.version, '0.1.0', `${surface}: server version drift`);
  assert.ok(list && Array.isArray(list.tools), `${surface}: tools/list missing`);
  return { init: { protocolVersion: init.protocolVersion, capabilities: init.capabilities, serverInfo: init.serverInfo }, tools: list.tools };
}

function errorSignature(line) {
  return {
    code: line?.error?.data?.code ?? null,
    message: line?.error?.message ?? null,
  };
}

function compareTaskProtocolExtras(node, rust) {
  const nodeById = byId(node);
  const rustById = byId(rust);
  assert.deepEqual(rustById.get('15')?.result?.prompts, nodeById.get('15')?.result?.prompts, 'task prompts/list drifted');
  assert.deepEqual(rustById.get('16')?.result, nodeById.get('16')?.result, 'task prompts/get drifted');
  assert.deepEqual(rustById.get('17')?.result, nodeById.get('17')?.result, 'task completion/name drifted');
  assert.deepEqual(rustById.get('18')?.result, nodeById.get('18')?.result, 'task completion/other drifted');
  assert.deepEqual(errorSignature(rustById.get('19')), errorSignature(nodeById.get('19')), 'task unknown-tool error drifted');
  const normalizeResources = (line) => {
    const result = line?.result ?? {};
    return { ...result, resources: (result.resources ?? []).map(({ description, mimeType }) => ({ description, mimeType })) };
  };
  assert.deepEqual(normalizeResources(rustById.get('20')), normalizeResources(nodeById.get('20')), 'task resources/list drifted');
  assert.deepEqual(errorSignature(rustById.get('21')), errorSignature(nodeById.get('21')), 'task resources/read error drifted');
  assert.deepEqual(errorSignature(rustById.get('22')), errorSignature(nodeById.get('22')), 'task unsupported-method error drifted');
  const taskDoctor = structured(rustById.get('23'));
  const nodeTaskDoctor = structured(nodeById.get('23'));
  const stableDoctor = (value) => ({ schema: value.schema, status: value.status, detail: value.detail, site_root_source: value.site_root_source, authority_posture: value.authority_posture, surface_type: value.surface_type, tool_posture: value.tool_posture, site_policy: value.site_policy, target_locus_guard: value.target_locus_guard });
  assert.deepEqual(stableDoctor(taskDoctor), stableDoctor(nodeTaskDoctor), 'task doctor parity drifted');
}
function compareTask(node, rust) {
  assert.deepEqual(protocol(rust, 'task'), protocol(node, 'task'), 'task initialize/tools parity drifted');
  const nodeById = byId(node);
  const rustById = byId(rust);
  const select = (lines) => [
    structured(lines.get('3')).status,
    structured(lines.get('4')).status,
    structured(lines.get('5')).status,
    structured(lines.get('6')).status,
    structured(lines.get('7')).status,
    structured(lines.get('8')).status,
    structured(lines.get('9')).status,
    responseCode(lines.get('10')) ?? structured(lines.get('10')).status,
    structured(lines.get('11')).status,
    structured(lines.get('12')).new_status ?? structured(lines.get('12')).status,
    structured(lines.get('13')).lifecycle?.status ?? structured(lines.get('13')).status,
    lines.get('14')?.result?.resources?.length,
    structured(lines.get('24')).status,
  ];
  assert.deepEqual(select(rustById), select(nodeById), 'task lifecycle status parity drifted');
  compareTaskProtocolExtras(node, rust);
  return { tool_count: protocol(rust, 'task').tools.length, statuses: select(rustById) };
}

function compareWorkProtocolExtras(node, rust) {
  const nodeById = byId(node);
  const rustById = byId(rust);
  assert.deepEqual(errorSignature(rustById.get('9')), errorSignature(nodeById.get('9')), 'work unknown-tool error drifted');
  assert.deepEqual(errorSignature(rustById.get('10')), errorSignature(nodeById.get('10')), 'work resources/list error drifted');
  assert.deepEqual(errorSignature(rustById.get('11')), errorSignature(nodeById.get('11')), 'work prompts/list error drifted');
  assert.deepEqual(errorSignature(rustById.get('12')), errorSignature(nodeById.get('12')), 'work completion/complete error drifted');
  assert.deepEqual(errorSignature(rustById.get('13')), errorSignature(nodeById.get('13')), 'work unsupported-method error drifted');
  const stableWorkDoctor = (value) => ({
    schema: value.schema,
    status: value.status,
    site_root: '<site-root>',
    concurrency: {
      database_path: '<site-root>/.ai/work-lifecycle.db',
      posture: value.concurrency?.posture,
      conflict_guards: value.concurrency?.conflict_guards,
    },
  });
  assert.deepEqual(stableWorkDoctor(structured(rustById.get('14'))), stableWorkDoctor(structured(nodeById.get('14'))), 'work doctor parity drifted');
}
function compareWork(node, rust) {
  assert.deepEqual(protocol(rust, 'work'), protocol(node, 'work'), 'work initialize/tools parity drifted');
  const nodeById = byId(node);
  const rustById = byId(rust);
  const select = (lines) => [
    structured(lines.get('3')).result?.status,
    structured(lines.get('4')).result?.status,
    responseCode(lines.get('5')),
    structured(lines.get('6')).status,
    structured(lines.get('7')).count,
    structured(lines.get('8')).status,
  ];
  assert.deepEqual(select(rustById), select(nodeById), 'work lifecycle status parity drifted');
  compareWorkProtocolExtras(node, rust);
  return { tool_count: protocol(rust, 'work').tools.length, statuses: select(rustById) };
}

function scenario(runtime, surface, input) {
  const root = mkdtempSync(join(tmpdir(), `narada-cross-${runtime}-${surface}-`));
  try {
    run(runtime, surface, root, '', true);
    const responses = [];
    for (const request of input.trim().split(/\r?\n/).filter(Boolean)) {
      responses.push(...run(runtime, surface, root, `${request}\n`));
    }
    return responses;
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

const task = compareTask(scenario('node', 'task', taskInput()), scenario('rust', 'task', taskInput()));
const work = compareWork(scenario('node', 'work', workInput()), scenario('rust', 'work', workInput()));
process.stdout.write(JSON.stringify({ schema: 'narada.mcp_lifecycle_native.cross_runtime_parity.v1', status: 'passed', runtimes: ['node', 'rust'], task, work }) + '\n');
