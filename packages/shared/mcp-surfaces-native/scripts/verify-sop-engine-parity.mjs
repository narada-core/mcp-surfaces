import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

function runMailbox(command, args, requests, cwd) {
  const result = spawnSync(command, args, {
    cwd,
    input: requests.map((request) => JSON.stringify(request)).join('\n') + '\n',
    encoding: 'utf8',
    timeout: 30_000,
    maxBuffer: 4 * 1024 * 1024,
    windowsHide: true,
  });
  if (result.error) throw new Error(`sop_engine_parity_spawn_failed:${command}:${result.error.message}`);
  if (result.status !== 0) throw new Error(`sop_engine_parity_exit:${command}:${result.status}:${String(result.stderr).slice(-1000)}`);
  const responses = String(result.stdout).trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
  if (responses.length !== requests.length) throw new Error(`sop_engine_parity_response_count:${command}:${responses.length}:${requests.length}`);
  return responses;
}

function structured(responses, id, runtime) {
  const response = responses.find((candidate) => candidate.id === id);
  const value = response?.result?.structuredContent;
  if (!value || typeof value !== 'object') throw new Error(`sop_engine_parity_structured_missing:${runtime}:${id}:${JSON.stringify(response).slice(0, 800)}`);
  return value;
}

function diagnosticCode(responses, id, runtime) {
  const response = responses.find((candidate) => candidate.id === id);
  const code = response?.error?.data?.code;
  if (typeof code !== 'string') throw new Error(`sop_engine_parity_diagnostic_missing:${runtime}:${id}:${JSON.stringify(response).slice(0, 800)}`);
  return code;
}

const OMIT = new Set([
  'run_id', 'parent_run_id', 'child_run_id', 'action_id', 'handoff_id', 'event_id',
  'retry_of_run_id', 'retry_of_handoff_id', 'retry_run_id', 'retry_handoff_id', 'original_outbox_event_id',
  'reopened_outbox_event_id', 'occurrence_key', 'handoff_occurrence_key',
  'retry_occurrence_key',
  'request_fingerprint', 'lease_token', 'lease_expires_at', 'lease_ms', 'lease_remaining_ms', 'next', 'trigger_source_ref', 'triggered_by',
  'created_at', 'updated_at', 'completed_at', 'started_at', 'recorded_at', 'available_at',
  'registered_at', 'processed_at', 'compacted_at', 'idempotency_key',
]);

function normalize(value) {
  if (Array.isArray(value)) return value.map(normalize);
  if (!value || typeof value !== 'object') return value;
  return Object.fromEntries(Object.entries(value)
    .filter(([key]) => !OMIT.has(key))
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, child]) => [key, normalize(child)]));
}

function assertSame(label, left, right) {
  const normalizedLeft = normalize(left);
  const normalizedRight = normalize(right);
  const lhs = JSON.stringify(normalizedLeft);
  const rhs = JSON.stringify(normalizedRight);
  if (lhs !== rhs) {
    const difference = firstDifference(normalizedLeft, normalizedRight);
    const render = (value) => (JSON.stringify(value) ?? String(value)).slice(0, 800);
    throw new Error(`${label}:path=${difference.path}:node=${render(difference.left)}:rust=${render(difference.right)}`);
  }
}

function firstDifference(left, right, path = '$') {
  if (Object.is(left, right)) return null;
  if (Array.isArray(left) && Array.isArray(right)) {
    if (left.length !== right.length) return { path: `${path}.length`, left: left.length, right: right.length };
    for (let index = 0; index < left.length; index += 1) {
      const difference = firstDifference(left[index], right[index], `${path}[${index}]`);
      if (difference) return difference;
    }
    return null;
  } else if (left && right && typeof left === 'object' && typeof right === 'object') {
    const keys = [...new Set([...Object.keys(left), ...Object.keys(right)])].sort();
    for (const key of keys) {
      if (!Object.hasOwn(left, key) || !Object.hasOwn(right, key)) return { path: `${path}.${key}`, left: left[key], right: right[key] };
      const difference = firstDifference(left[key], right[key], `${path}.${key}`);
      if (difference) return difference;
    }
    return null;
  }
  return { path, left, right };
}

function call(runtime, name, args, id = 1) {
  return runtime.run([{ jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: args } }]);
}

function callStructured(runtime, name, args, id = 1) {
  return structured(call(runtime, name, args, id), id, runtime.name);
}

function createRuntime(name, root, command, args, cwd) {
  mkdirSync(root, { recursive: true });
  return { name, root, run: (requests) => runMailbox(command, args, requests, cwd) };
}

function compareCall(label, node, rust, name, args) {
  const nodeValue = callStructured(node, name, args);
  const rustValue = callStructured(rust, name, args);
  assertSame(label, nodeValue, rustValue);
  return { node: nodeValue, rust: rustValue };
}

function compareDiagnostic(label, node, rust, name, args) {
  const nodeResponses = call(node, name, args);
  const rustResponses = call(rust, name, args);
  const nodeCode = diagnosticCode(nodeResponses, 1, 'node');
  const rustCode = diagnosticCode(rustResponses, 1, 'rust');
  if (nodeCode !== rustCode) throw new Error(`${label}:node=${nodeCode}:rust=${rustCode}`);
}

function assertDiagnosticPair(label, nodeResponses, rustResponses) {
  const nodeCode = diagnosticCode(nodeResponses, 1, 'node');
  const rustCode = diagnosticCode(rustResponses, 1, 'rust');
  if (nodeCode !== rustCode) throw new Error(`${label}:node=${nodeCode}:rust=${rustCode}`);
}

function bootstrap(node, rust) {
  const templates = [
    {
      sop_id: 'engine-flow', title: 'Engine flow',
      input_schema: { type: 'object', properties: { ticket: { type: 'string' }, include_optional: { type: 'boolean' } }, required: ['ticket'] },
      output: { ticket: { $ref: 'input.ticket' } },
      output_schema: { type: 'object', properties: { ticket: { type: 'string' } }, required: ['ticket'] },
      steps: [
        { id: 'first', executor: 'engine', title: 'First', instructions: 'Record {{input.ticket}}' },
        { id: 'optional', executor: 'engine', title: 'Optional', instructions: 'Optional', depends_on: ['first'], when: { ref: 'input.include_optional', op: 'equals', value: true } },
      ],
    },
    {
      sop_id: 'manual-flow', title: 'Manual flow',
      output: { approved: { $ref: 'steps.approve.result.approved' } },
      steps: [{ id: 'approve', executor: 'agent', title: 'Approve', instructions: 'Approve {{input.ticket}}', result_schema: { type: 'object', properties: { approved: { type: 'boolean' } }, required: ['approved'] } }],
    },
    {
      sop_id: 'action-flow', title: 'Action flow',
      output: { written: { $ref: 'steps.write.result.written' } },
      steps: [{
        id: 'write', executor: 'action', title: 'Write', instructions: 'Write {{input.ticket}}',
        action: { surface_id: 'fixture-writer', tool_name: 'fixture_write', idempotency_key_argument: 'idempotency_key', arguments: { ticket: { $ref: 'input.ticket' } } },
        result_schema: { type: 'object', properties: { written: { type: 'boolean' } }, required: ['written'] },
      }],
    },
    {
      sop_id: 'child-flow', title: 'Child flow',
      output: { child_ticket: { $ref: 'input.ticket' } },
      steps: [{ id: 'child-engine', executor: 'engine', title: 'Child engine', instructions: 'Child' }],
    },
    {
      sop_id: 'parent-flow', title: 'Parent flow',
      output: { child_ticket: { $ref: 'steps.child.result.output.child_ticket' } },
      steps: [{ id: 'child', executor: 'sop', title: 'Child', instructions: 'Run child', sop_id: 'child-flow', sop_version: 1, input: { ticket: { $ref: 'input.ticket' } } }],
    },
  ];
  for (const template of templates) compareCall(`sop.engine.template.${template.sop_id}`, node, rust, 'sop_template_create', template);
}

function runEngineScenario(node, rust) {
  const start = {
    sop_id: 'engine-flow', occurrence_key: 'engine-occurrence', input: { ticket: 'T-1', include_optional: false }, triggered_by: 'native-parity',
  };
  compareCall('sop.engine.start', node, rust, 'sop_run_start', start);
  compareCall('sop.engine.start_replay', node, rust, 'sop_run_start', start);
  compareDiagnostic('sop.engine.start_conflict', node, rust, 'sop_run_start', { ...start, input: { ticket: 'T-2', include_optional: false } });
}

function runManualScenario(node, rust, occurrenceKey, outcome = 'completed') {
  const startArgs = { sop_id: 'manual-flow', occurrence_key: occurrenceKey, input: { ticket: occurrenceKey }, triggered_by: 'native-parity' };
  const started = compareCall(`sop.manual.${occurrenceKey}.start`, node, rust, 'sop_run_start', startArgs);
  const nodeRunId = started.node.run_id;
  const rustRunId = started.rust.run_id;
  const nodeHandoffId = started.node.next_step?.result?.handoff_id;
  const rustHandoffId = started.rust.next_step?.result?.handoff_id;
  const nodeClaim = callStructured(node, 'sop_handoff_claim', { consumer_id: 'worker-1', handoff_id: nodeHandoffId, executor: 'agent', lease_ms: 120_000 });
  const rustClaim = callStructured(rust, 'sop_handoff_claim', { consumer_id: 'worker-1', handoff_id: rustHandoffId, executor: 'agent', lease_ms: 120_000 });
  assertSame(`sop.manual.${occurrenceKey}.claim`, nodeClaim, rustClaim);
  const completion = (runId, handoffId, leaseToken) => ({
    handoff_id: handoffId, run_id: runId, step_id: 'approve', consumer_id: 'worker-1', lease_token: leaseToken,
    completion_key: `completion-${occurrenceKey}`, principal: 'fixture-agent', outcome,
    result: outcome === 'completed' ? { approved: true } : {},
    error_message: outcome === 'failed' ? 'fixture failure' : undefined,
  });
  const nodeCompletion = completion(nodeRunId, nodeHandoffId, nodeClaim.handoff?.lease_token);
  const rustCompletion = completion(rustRunId, rustHandoffId, rustClaim.handoff?.lease_token);
  const nodeAdvanced = callStructured(node, 'sop_run_advance', nodeCompletion);
  const rustAdvanced = callStructured(rust, 'sop_run_advance', rustCompletion);
  assertSame(`sop.manual.${occurrenceKey}.advance`, nodeAdvanced, rustAdvanced);
  const nodeReplay = callStructured(node, 'sop_run_advance', nodeCompletion);
  const rustReplay = callStructured(rust, 'sop_run_advance', rustCompletion);
  assertSame(`sop.manual.${occurrenceKey}.advance_replay`, nodeReplay, rustReplay);
  assertDiagnosticPair(
    `sop.manual.${occurrenceKey}.advance_conflict`,
    call(node, 'sop_run_advance', { ...nodeCompletion, completion_key: `conflict-${occurrenceKey}` }),
    call(rust, 'sop_run_advance', { ...rustCompletion, completion_key: `conflict-${occurrenceKey}` }),
  );
  return { nodeRunId, rustRunId, nodeHandoffId, rustHandoffId, nodeAdvanced, rustAdvanced };
}

function runActionScenario(node, rust) {
  const startArgs = { sop_id: 'action-flow', occurrence_key: 'action-occurrence', input: { ticket: 'A-1' }, triggered_by: 'native-parity' };
  const started = compareCall('sop.action.start', node, rust, 'sop_run_start', startArgs);
  const nodeActionId = started.node.next_step?.action_id;
  const rustActionId = started.rust.next_step?.action_id;
  const resolution = (actionId) => ({
    action_id: actionId, completion_key: 'action-completion', outcome: 'completed',
    operation_ref: 'fixture://operation/1', result: { written: true },
  });
  const nodeResolution = resolution(nodeActionId);
  const rustResolution = resolution(rustActionId);
  const nodeResolved = callStructured(node, 'sop_action_resolve', nodeResolution);
  const rustResolved = callStructured(rust, 'sop_action_resolve', rustResolution);
  assertSame('sop.action.resolve', nodeResolved, rustResolved);
  assertSame(
    'sop.action.resolve_replay',
    callStructured(node, 'sop_action_resolve', nodeResolution),
    callStructured(rust, 'sop_action_resolve', rustResolution),
  );
  assertDiagnosticPair(
    'sop.action.resolve_conflict',
    call(node, 'sop_action_resolve', { ...nodeResolution, completion_key: 'action-conflict' }),
    call(rust, 'sop_action_resolve', { ...rustResolution, completion_key: 'action-conflict' }),
  );
}

function runCancellationScenario(node, rust) {
  const startArgs = { sop_id: 'action-flow', occurrence_key: 'cancel-occurrence', input: { ticket: 'C-1' }, triggered_by: 'native-parity' };
  const started = compareCall('sop.cancel.start', node, rust, 'sop_run_start', startArgs);
  const nodeActionId = started.node.next_step?.action_id;
  const rustActionId = started.rust.next_step?.action_id;
  const nodeCancelled = callStructured(node, 'sop_run_cancel', { run_id: started.node.run_id, reason: 'fixture cancellation' });
  const rustCancelled = callStructured(rust, 'sop_run_cancel', { run_id: started.rust.run_id, reason: 'fixture cancellation' });
  assertSame('sop.cancel.run', nodeCancelled, rustCancelled);
  assertSame(
    'sop.cancel.replay',
    callStructured(node, 'sop_run_cancel', { run_id: started.node.run_id, reason: 'fixture cancellation' }),
    callStructured(rust, 'sop_run_cancel', { run_id: started.rust.run_id, reason: 'fixture cancellation' }),
  );
  const resolution = (actionId) => ({
    action_id: actionId, completion_key: 'late-action-completion', outcome: 'completed',
    operation_ref: 'fixture://operation/late', result: { written: true },
  });
  assertSame(
    'sop.cancel.late_action_acknowledgement',
    callStructured(node, 'sop_action_resolve', resolution(nodeActionId)),
    callStructured(rust, 'sop_action_resolve', resolution(rustActionId)),
  );
}

function runChildScenario(node, rust) {
  compareCall('sop.child.start', node, rust, 'sop_run_start', {
    sop_id: 'parent-flow', occurrence_key: 'parent-occurrence', input: { ticket: 'P-1' }, triggered_by: 'native-parity',
  });
}

function runRetryScenarios(node, rust) {
  const inline = runManualScenario(node, rust, 'retry-inline', 'failed');
  const retryArgs = (handoffId) => ({ handoff_id: handoffId, principal: 'retry-operator', reason: 'transient worker failure' });
  assertSame(
    'sop.retry.inline',
    callStructured(node, 'sop_handoff_retry', retryArgs(inline.nodeHandoffId)),
    callStructured(rust, 'sop_handoff_retry', retryArgs(inline.rustHandoffId)),
  );
  assertSame(
    'sop.retry.inline_replay',
    callStructured(node, 'sop_handoff_retry', retryArgs(inline.nodeHandoffId)),
    callStructured(rust, 'sop_handoff_retry', retryArgs(inline.rustHandoffId)),
  );

  const newRun = runManualScenario(node, rust, 'retry-new-run', 'failed');
  compareCall('sop.retry.consumer_register', node, rust, 'sop_outbox_consumer_register', {
    consumer_id: 'retry-consumer', start_at: '2020-01-01T00:00:00Z',
  });
  const nodeOutbox = callStructured(node, 'sop_outbox_list', { consumer_id: 'retry-consumer' });
  const rustOutbox = callStructured(rust, 'sop_outbox_list', { consumer_id: 'retry-consumer' });
  const orderedOutbox = (value) => ({
    ...value,
    items: [...(value.items ?? [])].sort((left, right) =>
      `${left.payload?.sop_id ?? ''}:${left.payload?.occurrence_key ?? ''}`
        .localeCompare(`${right.payload?.sop_id ?? ''}:${right.payload?.occurrence_key ?? ''}`)),
  });
  assertSame('sop.retry.outbox_list', orderedOutbox(nodeOutbox), orderedOutbox(rustOutbox));
  const nodeEvent = nodeOutbox.items?.find((event) => event.payload?.occurrence_key === 'retry-new-run');
  const rustEvent = rustOutbox.items?.find((event) => event.payload?.occurrence_key === 'retry-new-run');
  if (!nodeEvent?.event_id || !rustEvent?.event_id) throw new Error('sop_retry_new_run_outbox_event_missing');
  const receipt = { disposition: 'processed', fixture: 'new-run-retry' };
  const nodeAck = callStructured(node, 'sop_outbox_ack', { event_id: nodeEvent.event_id, consumer_id: 'retry-consumer', receipt });
  const rustAck = callStructured(rust, 'sop_outbox_ack', { event_id: rustEvent.event_id, consumer_id: 'retry-consumer', receipt });
  assertSame('sop.retry.outbox_ack', nodeAck, rustAck);
  const nodeRetry = callStructured(node, 'sop_handoff_retry', retryArgs(newRun.nodeHandoffId));
  const rustRetry = callStructured(rust, 'sop_handoff_retry', retryArgs(newRun.rustHandoffId));
  assertSame('sop.retry.new_run', nodeRetry, rustRetry);
}

function parseJsonColumns(row) {
  return Object.fromEntries(Object.entries(row).map(([key, value]) => [
    key,
    key.endsWith('_json') && value !== null ? JSON.parse(String(value)) : value,
  ]));
}

function snapshot(runtimeRoot) {
  const db = new DatabaseSync(join(runtimeRoot, '.sop', 'sop.db'), { readOnly: true });
  try {
    return normalize({
      runs: db.prepare('SELECT * FROM sop_runs ORDER BY rowid').all().map(parseJsonColumns),
      actions: db.prepare('SELECT * FROM sop_actions ORDER BY rowid').all().map(parseJsonColumns),
      handoffs: db.prepare('SELECT * FROM sop_handoffs ORDER BY rowid').all().map(parseJsonColumns),
      outbox: db.prepare('SELECT * FROM sop_outbox ORDER BY rowid').all().map(parseJsonColumns),
      receipts: db.prepare('SELECT * FROM sop_outbox_receipts ORDER BY rowid').all().map(parseJsonColumns),
      events: db.prepare('SELECT event_kind,step_id,details_json FROM sop_events ORDER BY rowid').all().map(parseJsonColumns),
    });
  } finally {
    db.close();
  }
}

export function runSopEngineParity({ executable, workspaceRoot }) {
  const nodeEntrypoint = join(workspaceRoot, 'packages', 'sop-mcp', 'dist', 'src', 'main.js');
  if (!existsSync(nodeEntrypoint)) throw new Error(`sop_engine_parity_node_entrypoint_missing:${nodeEntrypoint}`);
  const root = mkdtempSync(join(tmpdir(), 'narada-sop-engine-native-parity-'));
  const nodeRoot = join(root, 'node');
  const rustRoot = join(root, 'rust');
  const node = createRuntime(
    'node', nodeRoot, process.env.NARADA_NODE_EXECUTABLE ?? process.execPath,
    [nodeEntrypoint, '--sop-root', nodeRoot], workspaceRoot,
  );
  const rust = createRuntime(
    'rust', rustRoot, executable,
    ['--surface-id', 'sop', '--sop-root', rustRoot], workspaceRoot,
  );
  try {
    bootstrap(node, rust);
    runEngineScenario(node, rust);
    runManualScenario(node, rust, 'manual-occurrence');
    runActionScenario(node, rust);
    runCancellationScenario(node, rust);
    runChildScenario(node, rust);
    runRetryScenarios(node, rust);
    assertSame('sop.engine.sqlite_snapshot', snapshot(nodeRoot), snapshot(rustRoot));
    return {
      status: 'passed',
      fixture: 'independent_run_engine_and_completion_authorities',
      compared: [
        'engine_dag', 'condition_skip', 'admission_replay', 'admission_conflict',
        'handoff_advance', 'handoff_replay', 'handoff_conflict', 'action_resolve',
        'action_replay', 'action_conflict', 'cancellation', 'late_action_acknowledgement',
        'child_sop', 'retry_in_place', 'retry_as_new_run', 'sqlite_snapshot',
      ],
    };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}
