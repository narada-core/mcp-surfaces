import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const executable = join(packageRoot, 'dist', 'native', process.platform === 'win32' ? 'narada-mcp-surfaces.exe' : 'narada-mcp-surfaces');
const surfaces = [
  'catalog-observation',
  'operator-routing',
  'site-inbox',
  'site-lifecycle',
  'site-registry',
  'project-state',
  'runtime-introspection',
  'site-coherence',
  'launcher',
  'mailbox',
  'graph-mail',
  'calendar',
  'site-loop',
  'worker-delegation',
  'delegated-task',
  'sop',
  'scheduler',
  'surface-feedback',
  'speech',
  'artifacts',
  'nars-session',
  'quota-meter',
  'operator-console-overlay',
  'browser-control',
  'cloudflare-carrier',
];
const modernMeta = {
  _meta: {
    'io.modelcontextprotocol/protocolVersion': '2026-07-28',
    'io.modelcontextprotocol/clientInfo': { name: 'native-protocol-parity', version: '0.1.0' },
    'io.modelcontextprotocol/clientCapabilities': {},
  },
};

if (!existsSync(executable)) {
  throw new Error(`native_protocol_executable_missing:${executable}`);
}

function run(surface) {
  const requests = [
    { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } },
    { jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} },
    { jsonrpc: '2.0', id: 3, method: 'server/discover', params: modernMeta },
    { jsonrpc: '2.0', id: 4, method: 'tools/list', params: modernMeta },
    { jsonrpc: '2.0', id: 5, method: 'initialize', params: modernMeta },
  ];
  const result = spawnSync(executable, ['--surface-id', surface, '--site-root', packageRoot], {
    input: requests.map((request) => JSON.stringify(request)).join('\n') + '\n',
    encoding: 'utf8',
    timeout: 10_000,
    maxBuffer: 2 * 1024 * 1024,
    windowsHide: true,
  });
  if (result.error) throw new Error(`${surface}:native_protocol_spawn_failed:${result.error.message}`);
  if (result.status !== 0) throw new Error(`${surface}:native_protocol_exit:${result.status}:${String(result.stderr).slice(0, 500)}`);
  const lines = String(result.stdout).trim().split(/\r?\n/).filter(Boolean);
  if (lines.length !== requests.length) throw new Error(`${surface}:native_protocol_response_count:${lines.length}`);
  return lines.map((line) => JSON.parse(line));
}

for (const surface of surfaces) {
  const responses = run(surface);
  const byId = new Map(responses.map((response) => [response.id, response]));
  if (byId.get(1)?.result?.protocolVersion !== '2024-11-05') throw new Error(`${surface}:legacy_initialize_version_mismatch`);
  const legacyTools = byId.get(2)?.result?.tools;
  if (!Array.isArray(legacyTools) || legacyTools.length === 0) throw new Error(`${surface}:legacy_tools_list_missing`);
  const discover = byId.get(3)?.result;
  if (discover?.resultType !== 'complete' || !discover.supportedVersions?.includes('2026-07-28')) throw new Error(`${surface}:modern_discovery_incomplete`);
  const modernTools = byId.get(4)?.result;
  if (modernTools?.resultType !== 'complete' || !Array.isArray(modernTools.tools) || modernTools.tools.length === 0) throw new Error(`${surface}:modern_tools_list_incomplete`);
  if (modernTools.cacheScope !== 'public' || !Number.isFinite(modernTools.ttlMs)) throw new Error(`${surface}:modern_tools_cache_metadata_missing`);
  const modernInitialize = byId.get(5)?.error;
  if (modernInitialize?.data?.code !== 'initialize_removed') throw new Error(`${surface}:modern_initialize_not_removed`);
}

function runMailbox(command, args, requests, cwd, env = process.env) {
  const result = spawnSync(command, [...args], {
    cwd,
    env,
    input: requests.map((request) => JSON.stringify(request)).join('\n') + '\n',
    encoding: 'utf8',
    timeout: 15_000,
    maxBuffer: 2 * 1024 * 1024,
    windowsHide: true,
  });
  if (result.error) throw new Error('mailbox_parity_spawn_failed:' + command + ':' + result.error.message);
  if (result.status !== 0) throw new Error('mailbox_parity_exit:' + command + ':' + result.status + ':' + String(result.stderr).slice(0, 500));
  const output = String(result.stdout);
  const responses = /Content-Length:/i.test(output) ? parseFramedResponses(output) : output.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
  if (responses.length !== requests.length) throw new Error('mailbox_parity_response_count:' + command + ':' + responses.length);
  return responses;
}

function parseFramedResponses(output) {
  const responses = [];
  let remaining = output;
  while (remaining.trim()) {
    const crlfHeaderEnd = remaining.indexOf('\r\n\r\n');
    const lfHeaderEnd = remaining.indexOf('\n\n');
    const headerEnd = crlfHeaderEnd >= 0 && (lfHeaderEnd < 0 || crlfHeaderEnd <= lfHeaderEnd) ? crlfHeaderEnd : lfHeaderEnd;
    if (headerEnd < 0) throw new Error('framed_response_header_incomplete');
    const separatorLength = headerEnd === crlfHeaderEnd ? 4 : 2;
    const header = remaining.slice(0, headerEnd);
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) throw new Error('framed_response_content_length_missing');
    const length = Number(match[1]);
    const bodyStart = headerEnd + separatorLength;
    const bodyEnd = bodyStart + length;
    if (remaining.length < bodyEnd) throw new Error('framed_response_body_incomplete');
    responses.push(JSON.parse(remaining.slice(bodyStart, bodyEnd)));
    remaining = remaining.slice(bodyEnd);
  }
  return responses;
}

function mailboxStructured(responses, id, command) {
  const response = responses.find((candidate) => candidate.id === id);
  const value = response?.result?.structuredContent;
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('mailbox_parity_structured_content_missing:' + command + ':' + id + ':' + JSON.stringify(response).slice(0, 500));
  }
  return value;
}

function stable(value) {
  if (Array.isArray(value)) return value.map(stable);
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map((key) => [key, stable(value[key])]));
  return value;
}

function assertSame(label, left, right) {
  if (JSON.stringify(stable(left)) !== JSON.stringify(stable(right))) {
    throw new Error('mailbox_parity_mismatch:' + label + ':bun=' + JSON.stringify(left) + ':rust=' + JSON.stringify(right));
  }
}

function runMailboxParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'mailbox-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('mailbox_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-mailbox-native-parity-'));
  try {
    const mailboxRoot = join(root, '.ai', 'mailboxes', 'support@example.test');
    const viewsRoot = join(mailboxRoot, 'views', 'by-thread');
    mkdirSync(viewsRoot, { recursive: true });
    writeFileSync(join(mailboxRoot, 'messages.json'), JSON.stringify([{
      id: 'm1',
      mailbox_id: 'support@example.test',
      folder: 'Inbox',
      subject: 'Fixture subject',
      conversationId: 'thread-1',
      from: { address: 'sender@example.test' },
      to: [{ address: 'support@example.test' }],
      receivedDateTime: '2026-01-01T00:00:00Z',
      isRead: false,
      body: { contentType: 'text', content: 'Fixture body' },
      attachments: [{ name: 'fixture.txt', contentBytes: 'must-not-cross-summary-boundary' }],
    }]), 'utf8');
    writeFileSync(join(mailboxRoot, 'settings.json'), JSON.stringify({ id: 'settings', enabled: true }), 'utf8');
    writeFileSync(join(viewsRoot, 'm1.json'), JSON.stringify({
      id: 'm1',
      mailbox_id: 'support@example.test',
      folder: 'Inbox',
      subject: 'Derived view must lose',
      conversationId: 'thread-1',
      text: 'derived body',
    }), 'utf8');
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'mailbox_doctor', arguments: {} } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'mailbox_accounts_list', arguments: {} } },
      { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'mailbox_messages_list', arguments: { query: 'fixture', include_body: false, limit: 10 } } },
      { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'mailbox_message_show', arguments: { message_id: 'm1' } } },
      { jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'mailbox_thread_show', arguments: { thread_id: 'thread-1' } } },
    ];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--site-root', root], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'mailbox', '--site-root', root], requests, workspaceRoot);
    const bunDoctor = mailboxStructured(bun, 2, 'bun');
    const rustDoctor = mailboxStructured(rust, 2, 'rust');
    assertSame('doctor.message_count', bunDoctor.message_count, rustDoctor.message_count);
    assertSame('doctor.skipped_non_message_records', bunDoctor.skipped_non_message_records, rustDoctor.skipped_non_message_records);
    const bunAccounts = mailboxStructured(bun, 3, 'bun').accounts?.[0];
    const rustAccounts = mailboxStructured(rust, 3, 'rust').accounts?.[0];
    assertSame('accounts', {
      mailbox_id: bunAccounts?.mailbox_id,
      message_count: bunAccounts?.message_count,
      unread_count: bunAccounts?.unread_count,
      folders: bunAccounts?.folders,
      latest_message_at: bunAccounts?.latest_message_at,
    }, {
      mailbox_id: rustAccounts?.mailbox_id,
      message_count: rustAccounts?.message_count,
      unread_count: rustAccounts?.unread_count,
      folders: rustAccounts?.folders,
      latest_message_at: rustAccounts?.latest_message_at,
    });
    const bunList = mailboxStructured(bun, 4, 'bun');
    const rustList = mailboxStructured(rust, 4, 'rust');
    assertSame('messages.count', bunList.count, rustList.count);
    assertSame('messages.row', bunList.messages?.[0], rustList.messages?.[0]);
    const bunShow = mailboxStructured(bun, 5, 'bun').message;
    const rustShow = mailboxStructured(rust, 5, 'rust').message;
    assertSame('message_show', bunShow, rustShow);
    const bunThread = mailboxStructured(bun, 6, 'bun');
    const rustThread = mailboxStructured(rust, 6, 'rust');
    assertSame('thread.count', bunThread.count, rustThread.count);
    assertSame('thread.message_ids', bunThread.messages?.map((value) => value.message_id), rustThread.messages?.map((value) => value.message_id));
    return { status: 'passed', fixture: 'local_projection_reads', compared: ['doctor', 'accounts', 'messages', 'message_show', 'thread_show'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runDelegatedTaskParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'delegated-task-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('delegated_task_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-delegated-task-native-parity-'));
  try {
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'delegated_task_template_catalog', arguments: {} } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'delegated_task_template_catalog', arguments: { template_id: 'commit_push_guarded' } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'delegated_task_template_catalog', arguments: { template_id: 'unknown' } } },
    ];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--task-root', root, '--site-root', root, '--allowed-root', root], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'delegated-task', '--site-root', root], requests, workspaceRoot);
    for (const request of requests) {
      assertSame(`delegated_task.template_catalog.${request.id}`, mailboxStructured(bun, request.id, 'bun'), mailboxStructured(rust, request.id, 'rust'));
    }
    return { status: 'passed', fixture: 'full_template_catalog', compared: ['all', 'commit_push_guarded', 'unknown'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runWorkerDelegationParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'worker-delegation-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('worker_delegation_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-worker-delegation-native-parity-'));
  const runRoot = join(root, '.narada', 'runtime', 'worker-delegation');
  const completedId = 'run-fixture-20260101';
  const runningId = 'run-fixture-20260102';
  try {
    mkdirSync(join(runRoot, completedId), { recursive: true });
    mkdirSync(join(runRoot, runningId), { recursive: true });
    writeFileSync(join(runRoot, completedId, 'result.json'), JSON.stringify({
      run_id: completedId,
      status: 'completed',
      completion_state: 'complete',
      requested_mode: 'audit_only',
      resolved_worker_config: { authority: 'read', runtime: 'codex' },
      summary: 'fixture complete',
      error: null,
      warning_count: 0,
      timing: { started_at: '2026-01-01T00:00:00.000Z', finished_at: '2026-01-01T00:01:00.000Z', duration_ms: 60_000 },
      progress: {},
      run_dir: join(runRoot, completedId),
    }), 'utf8');
    writeFileSync(join(runRoot, runningId, 'result.json'), JSON.stringify({
      run_id: runningId,
      status: 'running',
      requested_mode: 'audit_only',
      resolved_worker_config: { authority: 'read', runtime: 'codex' },
      summary: 'fixture running',
      timing: { started_at: new Date().toISOString() },
      run_dir: join(runRoot, runningId),
    }), 'utf8');
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'worker_dashboard_describe', arguments: { mode: 'single_run', run_id: completedId, include_terminal: true } } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'worker_dashboard_describe', arguments: { mode: 'all_active', include_terminal: false, limit: 10 } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'worker_runs_list', arguments: { limit: 10 } } },
      { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'worker_run_wait', arguments: { run_id: completedId, timeout_ms: 0 } } },
    ];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--site-root', root, '--allowed-root', root, '--run-root', runRoot], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'worker-delegation', '--site-root', root], requests, workspaceRoot);
    const projectDashboard = (value) => ({
      mode: value?.mode,
      include_terminal: value?.include_terminal,
      counts: Object.fromEntries(['total', 'active', 'terminal', 'failed'].map((key) => [key, value?.counts?.[key]])),
      run_ids: (value?.runs ?? []).map((run) => run?.run_id).sort(),
      statuses: (value?.runs ?? []).map((run) => [run?.run_id, run?.status]).sort((left, right) => String(left[0]).localeCompare(String(right[0]))),
      pending_run_ids: (value?.pending_join_gates ?? []).map((gate) => gate?.run_id).sort(),
    });
    const projectList = (value) => ({
      status: value?.status,
      count: value?.count,
      limit: value?.limit,
      run_ids: (value?.runs ?? []).map((run) => run?.run_id).sort(),
      statuses: (value?.runs ?? []).map((run) => [run?.run_id, run?.status]).sort((left, right) => String(left[0]).localeCompare(String(right[0]))),
    });
    const projectWait = (value) => ({
      status: value?.status,
      wait_status: value?.wait?.status,
      timeout_ms: value?.wait?.timeout_ms,
      run_id: value?.run?.run_id,
      run_status: value?.run?.status,
    });
    assertSame('worker_delegation.dashboard.single_run', projectDashboard(mailboxStructured(bun, 1, 'bun')), projectDashboard(mailboxStructured(rust, 1, 'rust')));
    assertSame('worker_delegation.dashboard.active_filter', projectDashboard(mailboxStructured(bun, 2, 'bun')), projectDashboard(mailboxStructured(rust, 2, 'rust')));
    assertSame('worker_delegation.runs_list', projectList(mailboxStructured(bun, 3, 'bun')), projectList(mailboxStructured(rust, 3, 'rust')));
    assertSame('worker_delegation.run_wait', projectWait(mailboxStructured(bun, 4, 'bun')), projectWait(mailboxStructured(rust, 4, 'rust')));
    return { status: 'passed', fixture: 'durable_run_dashboard_projection', compared: ['dashboard_single_run', 'dashboard_active_filter', 'runs_list', 'run_wait'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runArtifactsParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'artifacts-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('artifacts_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const requests = [
    { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } },
    { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'artifact_message_part_create', arguments: { artifact_id: 'artifact-1', kind: 'markdown', title: 'Fixture artifact', render_hint: 'inline' } } },
  ];
  const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--site-root', workspaceRoot], requests, workspaceRoot);
  const rust = runMailbox(executable, ['--surface-id', 'artifacts', '--site-root', workspaceRoot], requests, workspaceRoot);
  const bunResult = mailboxStructured(bun, 2, 'bun');
  const rustResult = mailboxStructured(rust, 2, 'rust');
  const comparable = (value) => Object.fromEntries(['schema', 'status', 'verification_status', 'message_part', 'assistant_content_parts', 'operator_message', 'recommended_verification'].map((key) => [key, value?.[key]]));
  assertSame('artifacts.message_part_create', comparable(bunResult), comparable(rustResult));
  return { status: 'passed', fixture: 'pure_message_part', compared: ['message_part', 'operator_message', 'recommendation'] };
}

function runSopParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'sop-mcp', 'dist', 'src', 'main.js');
  if (!existsSync(bunEntrypoint)) throw new Error('sop_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-sop-native-parity-'));
  try {
    const setupAndReads = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'sop_template_create', arguments: { sop_id: 'fixture', title: 'Fixture SOP', description: 'Parity fixture', steps: [{ id: 'step-1', executor: 'agent', title: 'Inspect', instructions: 'Inspect fixture' }] } } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'sop_template_list', arguments: {} } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'sop_template_show', arguments: { sop_id: 'fixture' } } },
      { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'sop_template_search', arguments: { query: 'Fixture' } } },
    ];
    const reads = setupAndReads.slice(1);
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--sop-root', root], setupAndReads, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'sop', '--site-root', root], reads.map((request, index) => ({ ...request, id: index + 2 })), workspaceRoot);
    const bunList = mailboxStructured(bun, 2, 'bun');
    const rustList = mailboxStructured(rust, 2, 'rust');
    const listComparable = (value) => Object.fromEntries(['schema', 'items', 'count'].map((key) => [key, value?.[key]]));
    assertSame('sop.template_list', listComparable(bunList), listComparable(rustList));
    const bunShow = mailboxStructured(bun, 3, 'bun');
    const rustShow = mailboxStructured(rust, 3, 'rust');
    const showComparable = (value) => Object.fromEntries(Object.keys(value ?? {}).filter((key) => key !== 'native_hydration').map((key) => [key, value[key]]));
    assertSame('sop.template_show', showComparable(bunShow), showComparable(rustShow));
    const bunSearch = mailboxStructured(bun, 4, 'bun');
    const rustSearch = mailboxStructured(rust, 4, 'rust');
    const searchComparable = (value) => Object.fromEntries(['schema', 'query', 'items', 'count'].map((key) => [key, value?.[key]]));
    assertSame('sop.template_search', searchComparable(bunSearch), searchComparable(rustSearch));
    return { status: 'passed', fixture: 'local_template_registry', compared: ['template_list', 'template_show', 'template_search'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runSopActionParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'sop-mcp', 'dist', 'src', 'main.js');
  if (!existsSync(bunEntrypoint)) throw new Error('sop_action_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-sop-action-native-parity-'));
  const dbPath = join(root, '.sop', 'sop.db');
  mkdirSync(dirname(dbPath), { recursive: true });
  const db = new DatabaseSync(dbPath);
  try {
    db.exec(`CREATE TABLE sop_actions (
      action_id TEXT PRIMARY KEY,
      run_id TEXT NOT NULL,
      step_id TEXT NOT NULL,
      occurrence_key TEXT NOT NULL,
      surface_id TEXT NOT NULL,
      tool_name TEXT NOT NULL,
      arguments_json TEXT NOT NULL,
      request_fingerprint TEXT NOT NULL,
      status TEXT NOT NULL,
      completion_key TEXT,
      completion_fingerprint TEXT,
      operation_ref TEXT,
      result_json TEXT NOT NULL,
      result_ref_json TEXT,
      error_message TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      completed_at TEXT
    )`);
    const insert = db.prepare('INSERT INTO sop_actions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)');
    insert.run('action-1', 'run-1', 'step-1', 'occ-1', 'surface-a', 'tool-a', '{}', 'fp-a', 'pending', null, null, null, '{}', null, null, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', null);
    insert.run('action-2', 'run-2', 'step-2', 'occ-2', 'surface-b', 'tool-b', '{"x":1}', 'fp-b', 'completed', 'key', 'cfp', 'op://2', '{"ok":true}', null, null, '2026-01-02T00:00:00Z', '2026-01-02T00:00:00Z', '2026-01-02T00:00:01Z');
  } finally {
    db.close();
  }
  try {
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'sop_action_list', arguments: {} } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'sop_action_list', arguments: { run_id: 'run-1' } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'sop_action_list', arguments: { status: 'completed' } } },
    ];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'node', [bunEntrypoint, '--sop-root', root], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'sop', '--site-root', root], requests, workspaceRoot);
    const comparable = (value) => ({ schema: value?.schema, count: value?.count, items: value?.items });
    for (const request of requests) assertSame(`sop.action_list.${request.id}`, comparable(mailboxStructured(bun, request.id, 'bun')), comparable(mailboxStructured(rust, request.id, 'rust')));
    return { status: 'passed', fixture: 'durable_action_list_projection', compared: ['all', 'run_filter', 'status_filter'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runSopRunListParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'sop-mcp', 'dist', 'src', 'main.js');
  if (!existsSync(bunEntrypoint)) throw new Error('sop_run_list_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-sop-run-list-native-parity-'));
  const dbPath = join(root, '.sop', 'sop.db');
  mkdirSync(dirname(dbPath), { recursive: true });
  const db = new DatabaseSync(dbPath);
  try {
    db.exec(`CREATE TABLE sop_runs (
      run_id TEXT PRIMARY KEY,
      sop_id TEXT NOT NULL,
      sop_version INTEGER NOT NULL,
      sop_title TEXT NOT NULL,
      status TEXT NOT NULL DEFAULT 'pending',
      occurrence_key TEXT NOT NULL DEFAULT '',
      request_fingerprint TEXT NOT NULL DEFAULT '',
      definition_fingerprint TEXT NOT NULL DEFAULT '',
      definition_json TEXT NOT NULL DEFAULT '{}',
      input_json TEXT NOT NULL DEFAULT '{}',
      input_ref_json TEXT,
      output_json TEXT NOT NULL DEFAULT '{}',
      output_ref_json TEXT,
      step_states_json TEXT NOT NULL DEFAULT '[]',
      trigger_source_kind TEXT NOT NULL DEFAULT 'manual',
      trigger_source_ref TEXT NOT NULL DEFAULT '',
      triggered_by TEXT NOT NULL DEFAULT '',
      parent_run_id TEXT,
      parent_step_id TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      completed_at TEXT
    )`);
    const insert = db.prepare('INSERT INTO sop_runs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)');
    const row = (runId, status, createdAt, completedAt = null) => [runId, 'fixture', 1, 'Fixture', status, `${runId}-occ`, '', '', '{}', '{}', null, '{}', null, '[]', 'manual', 'fixture', 'tester', null, null, createdAt, createdAt, completedAt];
    insert.run(...row('run-pending', 'pending', '2026-01-01T00:00:00Z'));
    insert.run(...row('run-completed', 'completed', '2026-01-02T00:00:00Z', '2026-01-02T00:01:00Z'));
    insert.run(...row('run-failed', 'failed', '2026-01-03T00:00:00Z', '2026-01-03T00:01:00Z'));
  } finally {
    db.close();
  }
  try {
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'sop_run_list', arguments: { include_terminal: true } } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'sop_run_list', arguments: { status: 'pending' } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'sop_run_list', arguments: { include_terminal: false } } },
    ];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'node', [bunEntrypoint, '--sop-root', root], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'sop', '--site-root', root], requests, workspaceRoot);
    const comparable = (value) => ({ schema: value?.schema, count: value?.count, items: value?.items });
    for (const request of requests) assertSame(`sop.run_list.${request.id}`, comparable(mailboxStructured(bun, request.id, 'bun')), comparable(mailboxStructured(rust, request.id, 'rust')));
    return { status: 'passed', fixture: 'durable_run_list_projection', compared: ['all', 'status_filter', 'terminal_filter'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runSopRunEventsParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'sop-mcp', 'dist', 'src', 'main.js');
  if (!existsSync(bunEntrypoint)) throw new Error('sop_run_events_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-sop-run-events-native-parity-'));
  const dbPath = join(root, '.sop', 'sop.db');
  mkdirSync(dirname(dbPath), { recursive: true });
  const db = new DatabaseSync(dbPath);
  try {
    db.exec(`CREATE TABLE sop_events (
      event_id TEXT PRIMARY KEY,
      run_id TEXT NOT NULL,
      step_id TEXT NOT NULL,
      event_kind TEXT NOT NULL,
      details_json TEXT NOT NULL DEFAULT '{}',
      recorded_at TEXT NOT NULL
    )`);
    const insert = db.prepare('INSERT INTO sop_events VALUES (?,?,?,?,?,?)');
    insert.run('event-1', 'run-1', 'step-1', 'step_started', '{"a":1}', '2026-01-01T00:00:00Z');
    insert.run('event-2', 'run-1', 'step-1', 'step_completed', '{"b":true}', '2026-01-01T00:01:00Z');
    insert.run('event-3', 'run-2', '', 'run_started', '{}', '2026-01-02T00:00:00Z');
  } finally {
    db.close();
  }
  try {
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'sop_run_events', arguments: { run_id: 'run-1' } } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'sop_run_events', arguments: { run_id: 'run-1', limit: 1 } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'sop_run_events', arguments: { run_id: 'run-1', offset: 1 } } },
    ];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'node', [bunEntrypoint, '--sop-root', root], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'sop', '--site-root', root], requests, workspaceRoot);
    const comparable = (value) => ({ run_id: value?.run_id, count: value?.count, items: value?.items });
    for (const request of requests) assertSame(`sop.run_events.${request.id}`, comparable(mailboxStructured(bun, request.id, 'bun')), comparable(mailboxStructured(rust, request.id, 'rust')));
    return { status: 'passed', fixture: 'durable_run_events_projection', compared: ['all', 'limit', 'offset'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runSopRunStatusParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'sop-mcp', 'dist', 'src', 'main.js');
  if (!existsSync(bunEntrypoint)) throw new Error('sop_run_status_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-sop-run-status-native-parity-'));
  const dbPath = join(root, '.sop', 'sop.db');
  mkdirSync(dirname(dbPath), { recursive: true });
  const db = new DatabaseSync(dbPath);
  try {
    db.exec(`CREATE TABLE sop_runs (
      run_id TEXT PRIMARY KEY,
      sop_id TEXT NOT NULL,
      sop_version INTEGER NOT NULL,
      sop_title TEXT NOT NULL,
      status TEXT NOT NULL DEFAULT 'pending',
      occurrence_key TEXT NOT NULL DEFAULT '',
      request_fingerprint TEXT NOT NULL DEFAULT '',
      definition_fingerprint TEXT NOT NULL DEFAULT '',
      definition_json TEXT NOT NULL DEFAULT '{}',
      input_json TEXT NOT NULL DEFAULT '{}',
      input_ref_json TEXT,
      output_json TEXT NOT NULL DEFAULT '{}',
      output_ref_json TEXT,
      step_states_json TEXT NOT NULL DEFAULT '[]',
      trigger_source_kind TEXT NOT NULL DEFAULT 'manual',
      trigger_source_ref TEXT NOT NULL DEFAULT '',
      triggered_by TEXT NOT NULL DEFAULT '',
      parent_run_id TEXT,
      parent_step_id TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      completed_at TEXT
    )`);
    const steps = JSON.stringify([{
      step_id: 'step-1', executor: 'operator', blocking: true, title: 'Approve', status: 'running', depends_on: [],
      instructions: 'approve', when: null, input: {}, input_ref: null, result_schema: null, action: null,
      sop_id: null, sop_version: null, wait_policy: null, pinned_child_definition_fingerprint: null,
      child_run_id: null, action_id: null, started_at: '2026-01-01', completed_at: null,
      result: { instructions: 'approve now' }, result_ref: null, completion_key: null,
      completion_fingerprint: null, error_message: null,
    }]);
    const insert = db.prepare('INSERT INTO sop_runs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)');
    insert.run('run-1', 'demo', 1, 'Demo', 'awaiting_confirmation', 'occ-1', '', '', '{}', '{"input":1}', null, '{}', null, steps, 'manual', '', 'operator', null, null, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', null);
  } finally {
    db.close();
  }
  try {
    const request = { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'sop_run_status', arguments: { run_id: 'run-1' } } };
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'node', [bunEntrypoint, '--sop-root', root], [request], workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'sop', '--site-root', root], [request], workspaceRoot);
    const comparable = (value) => {
      if (!value || typeof value !== 'object') return value;
      const { native_hydration: _nativeHydration, ...publicValue } = value;
      return publicValue;
    };
    assertSame('sop.run_status', comparable(mailboxStructured(bun, 1, 'bun')), comparable(mailboxStructured(rust, 1, 'rust')));
    return { status: 'passed', fixture: 'durable_run_status_projection', compared: ['full_projection'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runSopHandoffParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'sop-mcp', 'dist', 'src', 'main.js');
  if (!existsSync(bunEntrypoint)) throw new Error('sop_handoff_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-sop-handoff-native-parity-'));
  const dbPath = join(root, '.sop', 'sop.db');
  mkdirSync(dirname(dbPath), { recursive: true });
  const db = new DatabaseSync(dbPath);
  const canonical = (value) => {
    if (Array.isArray(value)) return value.map(canonical);
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).sort(([left], [right]) => left.localeCompare(right)).map(([key, entry]) => [key, canonical(entry)]));
    return value;
  };
  const fingerprint = (value) => createHash('sha256').update(JSON.stringify(canonical(value)), 'utf8').digest('hex');
  const deterministicId = (prefix, value) => `${prefix}${createHash('sha256').update(value, 'utf8').digest('hex').slice(0, 24)}`;
  try {
    db.exec(`CREATE TABLE sop_handoffs (
      handoff_id TEXT PRIMARY KEY,
      run_id TEXT NOT NULL,
      step_id TEXT NOT NULL,
      occurrence_key TEXT NOT NULL,
      sop_id TEXT NOT NULL,
      sop_version INTEGER NOT NULL,
      executor TEXT NOT NULL,
      title TEXT NOT NULL,
      instructions TEXT NOT NULL,
      input_json TEXT NOT NULL,
      input_ref_json TEXT,
      result_schema_json TEXT,
      request_fingerprint TEXT NOT NULL,
      status TEXT NOT NULL DEFAULT 'pending',
      lease_owner TEXT,
      lease_token TEXT,
      lease_expires_at TEXT,
      attempt_count INTEGER NOT NULL DEFAULT 0,
      last_error TEXT,
      completion_key TEXT,
      completion_fingerprint TEXT,
      principal TEXT,
      result_json TEXT NOT NULL DEFAULT '{}',
      result_ref_json TEXT,
      error_message TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      completed_at TEXT
    )`);
    const insert = db.prepare('INSERT INTO sop_handoffs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)');
    const row = ({ runId, stepId, executor, status, createdAt, leaseOwner = null, leaseToken = null, leaseExpiresAt = null, attemptCount = 0 }) => {
      const identity = `${runId}\0${stepId}`;
      const input = {};
      const resultSchema = null;
      return [
        deterministicId('soh_', identity), runId, stepId, deterministicId('sop_handoff_', identity), 'demo', 1,
        executor, 'Approve', 'approve now', JSON.stringify(input), null, resultSchema, fingerprint({
          run_id: runId, step_id: stepId, sop_id: 'demo', sop_version: 1, executor, title: 'Approve', instructions: 'approve now', input, input_ref: null, result_schema: resultSchema,
        }), status, leaseOwner, leaseToken, leaseExpiresAt, attemptCount, null, null, null, null, '{}', null, null, createdAt, createdAt, null,
      ];
    };
    insert.run(...row({ runId: 'run-1', stepId: 'step-1', executor: 'operator', status: 'pending', createdAt: '2026-01-01T00:00:00Z' }));
    insert.run(...row({ runId: 'run-2', stepId: 'step-2', executor: 'agent', status: 'leased', leaseOwner: 'consumer-1', leaseToken: 'secret-token', leaseExpiresAt: '2099-01-01T00:00:00Z', attemptCount: 2, createdAt: '2026-01-02T00:00:00Z' }));
  } finally {
    db.close();
  }
  try {
    const showId = deterministicId('soh_', 'run-2\0step-2');
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'sop_handoff_list', arguments: { limit: 10 } } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'sop_handoff_list', arguments: { executor: 'agent' } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'sop_handoff_list', arguments: { run_id: 'run-1' } } },
      { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'sop_handoff_list', arguments: { status: 'pending' } } },
      { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'sop_handoff_show', arguments: { handoff_id: showId } } },
    ];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'node', [bunEntrypoint, '--sop-root', root], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'sop', '--site-root', root], requests, workspaceRoot);
    for (const request of requests) assertSame(`sop.handoff.${request.id}`, mailboxStructured(bun, request.id, 'bun'), mailboxStructured(rust, request.id, 'rust'));
    return { status: 'passed', fixture: 'durable_handoff_read_projection', compared: ['list', 'executor_filter', 'run_filter', 'status_filter', 'show'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runSopRunCoverageParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'sop-mcp', 'dist', 'src', 'main.js');
  if (!existsSync(bunEntrypoint)) throw new Error('sop_run_coverage_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-sop-run-coverage-native-parity-'));
  const dbPath = join(root, '.sop', 'sop.db');
  mkdirSync(dirname(dbPath), { recursive: true });
  const db = new DatabaseSync(dbPath);
  try {
    db.exec(`CREATE TABLE sop_templates (sop_id TEXT, version INTEGER, title TEXT, status TEXT, updated_at TEXT);
      CREATE TABLE sop_runs (
        run_id TEXT PRIMARY KEY, sop_id TEXT NOT NULL, sop_version INTEGER NOT NULL, sop_title TEXT NOT NULL,
        status TEXT NOT NULL, occurrence_key TEXT NOT NULL, request_fingerprint TEXT NOT NULL DEFAULT '',
        definition_fingerprint TEXT NOT NULL DEFAULT '', definition_json TEXT NOT NULL DEFAULT '{}',
        input_json TEXT NOT NULL DEFAULT '{}', input_ref_json TEXT, output_json TEXT NOT NULL DEFAULT '{}',
        output_ref_json TEXT, step_states_json TEXT NOT NULL DEFAULT '[]', trigger_source_kind TEXT NOT NULL DEFAULT 'manual',
        trigger_source_ref TEXT NOT NULL DEFAULT '', triggered_by TEXT NOT NULL DEFAULT '', parent_run_id TEXT,
        parent_step_id TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, completed_at TEXT
      )`);
    const insertTemplate = db.prepare('INSERT INTO sop_templates VALUES (?,?,?,?,?)');
    insertTemplate.run('old-run', 1, 'Old run', 'active', '2026-01-03T00:00:00Z');
    insertTemplate.run('fresh-run', 1, 'Fresh run', 'active', '2026-01-04T00:00:00Z');
    insertTemplate.run('never-run', 1, 'Never run', 'active', '2026-01-05T00:00:00Z');
    insertTemplate.run('deprecated-run', 1, 'Deprecated run', 'deprecated', '2026-01-02T00:00:00Z');
    const insertRun = db.prepare('INSERT INTO sop_runs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)');
    const runRow = (runId, sopId, status, createdAt, completedAt = null) => [runId, sopId, 1, `${sopId} title`, status, `${runId}-occ`, '', '', '{}', '{}', null, '{}', null, '[]', 'manual', '', 'operator', null, null, createdAt, createdAt, completedAt];
    insertRun.run(...runRow('run-old', 'old-run', 'completed', '2026-01-01T00:00:00Z', '2026-01-01T00:01:00Z'));
    insertRun.run(...runRow('run-fresh', 'fresh-run', 'running', '2026-01-03T00:00:00Z'));
  } finally {
    db.close();
  }
  try {
    const since = '2026-01-02T00:00:00Z';
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'sop_run_coverage_since', arguments: { since, template_status: 'active' } } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'sop_run_coverage_since', arguments: { since, template_status: 'active', include_terminal: false } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'sop_run_coverage_since', arguments: { since, template_status: 'active', status: 'completed' } } },
      { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'sop_run_coverage_since', arguments: { since, template_status: 'deprecated' } } },
    ];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'node', [bunEntrypoint, '--sop-root', root], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'sop', '--site-root', root], requests, workspaceRoot);
    for (const request of requests) assertSame(`sop.run_coverage.${request.id}`, mailboxStructured(bun, request.id, 'bun'), mailboxStructured(rust, request.id, 'rust'));
    return { status: 'passed', fixture: 'durable_run_coverage_projection', compared: ['active', 'non_terminal', 'status_filter', 'template_status'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runSopOutboxParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'sop-mcp', 'dist', 'src', 'main.js');
  if (!existsSync(bunEntrypoint)) throw new Error('sop_outbox_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-sop-outbox-native-parity-'));
  const dbPath = join(root, '.sop', 'sop.db');
  mkdirSync(dirname(dbPath), { recursive: true });
  const db = new DatabaseSync(dbPath);
  try {
    db.exec(`CREATE TABLE sop_outbox (
      event_id TEXT PRIMARY KEY, topic TEXT NOT NULL, partition_key TEXT NOT NULL, run_id TEXT NOT NULL,
      sop_id TEXT NOT NULL, sop_version INTEGER NOT NULL, occurrence_key TEXT NOT NULL, outcome TEXT NOT NULL,
      payload_json TEXT NOT NULL, created_at TEXT NOT NULL, available_at TEXT NOT NULL, compacted_at TEXT
    );
    CREATE TABLE sop_outbox_consumer_requirements (topic TEXT NOT NULL, consumer_id TEXT NOT NULL, start_at TEXT NOT NULL, registered_at TEXT NOT NULL);
    CREATE TABLE sop_outbox_receipts (event_id TEXT NOT NULL, consumer_id TEXT NOT NULL, processed_at TEXT NOT NULL, receipt_json TEXT NOT NULL);`);
    const topic = 'sop.run.terminal.v1';
    db.prepare('INSERT INTO sop_outbox_consumer_requirements VALUES (?,?,?,?)').run(topic, 'consumer-1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
    const insert = db.prepare('INSERT INTO sop_outbox VALUES (?,?,?,?,?,?,?,?,?,?,?,?)');
    const event = (id, createdAt, availableAt, payload = '{}') => [id, topic, `partition-${id}`, `run-${id}`, 'demo', 1, `occ-${id}`, 'completed', payload, createdAt, availableAt, null];
    insert.run(...event('event-1', '2026-01-02T00:00:00Z', '2026-01-02T00:00:00Z', '{"status":"completed"}'));
    insert.run(...event('event-2', '2026-01-03T00:00:00Z', '2026-01-03T00:00:00Z'));
    insert.run(...event('event-before-start', '2025-12-31T00:00:00Z', '2025-12-31T00:00:00Z'));
    insert.run(...event('event-future', '2026-01-04T00:00:00Z', '2099-01-01T00:00:00Z'));
    db.prepare('INSERT INTO sop_outbox_receipts VALUES (?,?,?,?)').run('event-2', 'consumer-1', '2026-01-03T01:00:00Z', '{}');
  } finally {
    db.close();
  }
  try {
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'sop_outbox_list', arguments: { consumer_id: 'consumer-1' } } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'sop_outbox_list', arguments: { consumer_id: 'consumer-1', topic: 'sop.run.terminal.v1' } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'sop_outbox_list', arguments: { consumer_id: 'consumer-1', limit: 1 } } },
    ];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'node', [bunEntrypoint, '--sop-root', root], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'sop', '--site-root', root], requests, workspaceRoot);
    for (const request of requests) assertSame(`sop.outbox.${request.id}`, mailboxStructured(bun, request.id, 'bun'), mailboxStructured(rust, request.id, 'rust'));
    return { status: 'passed', fixture: 'durable_outbox_read_projection', compared: ['all_topics', 'topic_filter', 'limit'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runSurfaceFeedbackParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'surface-feedback-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('surface_feedback_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-surface-feedback-native-parity-'));
  try {
    const requests = [{
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: 'surface_feedback_live_proof_template', arguments: { workflow: 'fixture', surface_id: 'calendar' } },
    }];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--feedback-root', root, '--canonical-feedback-root', root], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'surface-feedback', '--site-root', root], requests, workspaceRoot);
    assertSame('surface_feedback.live_proof_template', mailboxStructured(bun, 1, 'bun'), mailboxStructured(rust, 1, 'rust'));
    return { status: 'passed', fixture: 'live_proof_template', compared: ['full_structured_content'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runSiteLoopParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'site-loop-mcp', 'src', 'site-loop-mcp-server.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('site_loop_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-site-loop-native-parity-'));
  try {
    const configDir = join(root, '.narada', 'capabilities');
    mkdirSync(configDir, { recursive: true });
    writeFileSync(join(configDir, 'site-loop-config.json'), JSON.stringify({
      schema: 'narada.site_loop.config.v2',
      loop_id: 'fixture.loop',
      site_id: 'fixture-site',
      display_name: 'Fixture Loop',
      resident: { agent_id: 'fixture-agent', role: 'resident' },
      scheduler: { default_task_name: 'Fixture-Loop' },
      docs: [{ path: 'README.md', description: 'Fixture documentation' }],
      tests: { smoke_echo: { command: 'node', args: ['-e', 'process.stdout.write("ok")'] } },
      policy: {},
      persistence: {
        schema: 'narada.site_loop.persistence.v2',
        evidence_root: '.ai/evidence',
        raw_retention_days: 1,
        summary_retention_days: 1,
        inline_summary_bytes: 1024,
        compression: 'gzip',
      },
    }), 'utf8');
    writeFileSync(join(root, 'README.md'), 'Fixture Site Loop documentation.\n', 'utf8');
    const bunCommand = process.env.NARADA_BUN_EXECUTABLE ?? 'bun';
    const prepare = spawnSync(bunCommand, ['-e', "import { openSiteLoopStore } from './packages/site-loop-mcp/src/site-loop/site-loop-store.ts'; const store = openSiteLoopStore(process.env.NARADA_PARITY_ROOT, { storeMode: 'prepare' }); store.close();"], {
      cwd: workspaceRoot,
      env: { ...process.env, NARADA_PARITY_ROOT: root },
      encoding: 'utf8',
      timeout: 15_000,
      maxBuffer: 2 * 1024 * 1024,
      windowsHide: true,
    });
    if (prepare.error) throw new Error('site_loop_parity_store_prepare_failed:' + prepare.error.message);
    if (prepare.status !== 0) throw new Error('site_loop_parity_store_prepare_exit:' + prepare.status + ':' + String(prepare.stderr).slice(0, 500));
    const requests = [{ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'site_loop_config_validate', arguments: {} } }, {
      jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'site_docs_list', arguments: {} },
    }, {
      jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'site_docs_show', arguments: { path: 'README.md' } },
    }, {
      jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'site_test_list', arguments: {} },
    }, {
      jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'site_loop_status', arguments: {} },
    }];
    const bun = runMailbox(bunCommand, [bunEntrypoint, '--site-root', root], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'site-loop', '--site-root', root], requests, workspaceRoot);
    const comparable = (value) => Object.fromEntries(['schema', 'status', 'site_root', 'path', 'schema_id', 'config_schema', 'loop_id', 'site_id', 'display_name', 'errors', 'active_tools_refuse'].map((key) => [key, value?.[key]]));
    assertSame('site_loop.config_validate', comparable(mailboxStructured(bun, 1, 'bun')), comparable(mailboxStructured(rust, 1, 'rust')));
    assertSame('site_loop.docs_list', mailboxStructured(bun, 2, 'bun'), mailboxStructured(rust, 2, 'rust'));
    assertSame('site_loop.docs_show', mailboxStructured(bun, 3, 'bun'), mailboxStructured(rust, 3, 'rust'));
    assertSame('site_loop.tests_list', mailboxStructured(bun, 4, 'bun'), mailboxStructured(rust, 4, 'rust'));
    assertSame('site_loop.status', mailboxStructured(bun, 5, 'bun'), mailboxStructured(rust, 5, 'rust'));
    return { status: 'passed', fixture: 'config_docs_tests_and_status_read', compared: ['config_validate', 'docs_list', 'docs_show', 'tests_list', 'status'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runCalendarParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'calendar-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('calendar_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-calendar-native-parity-'));
  try {
    const configDir = join(root, '.ai');
    mkdirSync(configDir, { recursive: true });
    writeFileSync(join(configDir, 'calendar-mcp.json'), JSON.stringify({
      graph_base_url: 'https://graph.microsoft.com/v1.0///',
      allowed_mailboxes: ['fixture@example.test', '', 42],
      allow_event_writes: true,
      write_approval_token: '',
    }), 'utf8');
    const outputId = 'fixture123';
    const outputRef = `mcp_output:${outputId}`;
    const fullOutput = { answer: 'fixture', items: [1, 2, 3] };
    const outputRecord = {
      schema: 'narada.mcp_output_ref.v1',
      ref: outputRef,
      output_id: outputId,
      tool_name: 'calendar_doctor',
      full_output_char_length: JSON.stringify(fullOutput, null, 2).length,
      truncated: false,
      sha256: createHash('sha256').update(JSON.stringify(fullOutput), 'utf8').digest('hex'),
      full_output: fullOutput,
    };
    const outputDir = join(root, '.ai', 'tmp', 'mcp-outputs', 'workspace');
    mkdirSync(outputDir, { recursive: true });
    writeFileSync(join(outputDir, `${outputId}.json`), `${JSON.stringify(outputRecord)}\n`, 'utf8');
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'calendar_doctor', arguments: {} } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'calendar_guidance', arguments: { workflow: '  weekly  ', tool: ' calendar_event_query ' } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'calendar_output_show', arguments: { ref: outputRef, offset: 0, limit: 100 } } },
    ];
    const env = { ...process.env };
    for (const key of ['MS_GRAPH_ACCESS_TOKEN', 'GRAPH_ACCESS_TOKEN', 'GRAPH_TENANT_ID', 'GRAPH_CLIENT_ID', 'GRAPH_CLIENT_SECRET', 'GRAPH_TOKEN_ENDPOINT']) delete env[key];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--site-root', root], requests, workspaceRoot, env);
    const rust = runMailbox(executable, ['--surface-id', 'calendar', '--site-root', root], requests, workspaceRoot, env);
    assertSame('calendar.doctor', mailboxStructured(bun, 1, 'bun'), mailboxStructured(rust, 1, 'rust'));
    assertSame('calendar.guidance', mailboxStructured(bun, 2, 'bun'), mailboxStructured(rust, 2, 'rust'));
    assertSame('calendar.output_show', mailboxStructured(bun, 3, 'bun'), mailboxStructured(rust, 3, 'rust'));
    return { status: 'passed', fixture: 'local_policy_posture', compared: ['doctor', 'guidance', 'output_show'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runCalendarAuthorityBridge() {
  const cleanEnv = { ...process.env };
  delete cleanEnv.NARADA_CALENDAR_AUTHORITY_ENTRYPOINT;
  delete cleanEnv.NARADA_CALENDAR_AUTHORITY_ARGS;
  cleanEnv.NARADA_NATIVE_GRAPH_AUTHORITY = '0';
  const request = { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'calendar_event_query', arguments: { start_datetime: '2026-01-01T00:00:00Z', end_datetime: '2026-01-02T00:00:00Z', limit: 1 } } };
  const refusal = runMailbox(executable, ['--surface-id', 'calendar', '--site-root', packageRoot], [request], packageRoot, cleanEnv)[0];
  assertSame('calendar.authority.refusal', {
    schema: refusal.error?.data?.schema,
    status: refusal.error?.data?.status,
    reason: refusal.error?.data?.reason,
    tool_name: refusal.error?.data?.tool_name,
  }, {
    schema: 'narada.calendar_mcp.authority_boundary.v1',
    status: 'unavailable',
    reason: 'native_calendar_external_authority_not_enabled',
    tool_name: 'calendar_event_query',
  });
  const authorityScript = "let body='';process.stdin.on('data',chunk=>body+=chunk).on('end',()=>{const request=JSON.parse(body);process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:request.id,result:{schema:'narada.calendar_mcp.authority_fixture.v1',status:'ok',method:request.method,arguments:request.params}}));});";
  const authorityEnv = {
    ...cleanEnv,
    NARADA_CALENDAR_AUTHORITY_ENTRYPOINT: process.execPath,
    NARADA_CALENDAR_AUTHORITY_ARGS: ['-e', authorityScript].join(String.fromCharCode(31)),
  };
  const forwarded = runMailbox(executable, ['--surface-id', 'calendar', '--site-root', packageRoot], [request], packageRoot, authorityEnv);
  assertSame('calendar.authority.forwarded', mailboxStructured(forwarded, 1, 'native-calendar-authority'), {
    schema: 'narada.calendar_mcp.authority_fixture.v1',
    status: 'ok',
    method: 'calendar_event_query',
    arguments: request.params.arguments,
  });
  return { status: 'passed', fixture: 'opt_in_stdio_authority_bridge', compared: ['unconfigured_refusal', 'configured_forwarding'] };
}

function runCalendarNativeGraphParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'calendar-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('calendar_graph_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-calendar-native-graph-'));
  const bunRoot = join(root, 'bun-site');
  const rustRoot = join(root, 'rust-site');
  const fixtureScript = join(root, 'calendar-graph-fixture.mjs');
  const requests = [
    { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'calendar_list', arguments: { limit: 3 } } },
    { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'calendar_event_query', arguments: { start_datetime: '2026-06-25T10:00:00Z', end_datetime: '2026-06-25T11:00:00Z', select: 'id,subject,start,end', limit: 5 } } },
    { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'calendar_event_show', arguments: { event_id: 'event-1' } } },
    { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'calendar_event_create', arguments: { subject: 'Allowed write', start_datetime: '2026-06-25T10:00:00', end_datetime: '2026-06-25T11:00:00', time_zone: 'UTC', attendees: ['person@example.test'], location: 'Conference Room', confirm_write: true, approval_token: 'approve-1' } } },
    { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'calendar_event_update', arguments: { event_id: 'event-1', subject: 'Updated write', confirm_write: true, approval_token: 'approve-1' } } },
    { jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'calendar_event_delete', arguments: { event_id: 'event-1', confirm_write: true, approval_token: 'approve-1' } } },
    { jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name: 'calendar_event_create', arguments: { subject: 'Refused write', start_datetime: '2026-06-25T10:00:00', end_datetime: '2026-06-25T11:00:00', time_zone: 'UTC' } } },
  ];
  const cleanEnv = { ...process.env, NARADA_GRAPH_FIXTURE_REQUESTS: JSON.stringify(requests) };
  for (const key of ['MS_GRAPH_ACCESS_TOKEN', 'GRAPH_ACCESS_TOKEN', 'GRAPH_TENANT_ID', 'GRAPH_CLIENT_ID', 'GRAPH_CLIENT_SECRET', 'GRAPH_TOKEN_ENDPOINT', 'NARADA_NATIVE_GRAPH_AUTHORITY', 'NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST']) delete cleanEnv[key];
  const fixture = String.raw`import { createServer } from 'node:http';
import { mkdirSync, writeFileSync } from 'node:fs';
import { spawn } from 'node:child_process';

const [nativeExecutable, bunEntrypoint, bunCommand, bunRoot, rustRoot] = process.argv.slice(2);
const requests = JSON.parse(process.env.NARADA_GRAPH_FIXTURE_REQUESTS ?? '[]');
const received = [];
const server = createServer((request, response) => {
  const chunks = [];
  request.on('data', (chunk) => chunks.push(chunk));
  request.on('end', () => {
    const raw = Buffer.concat(chunks).toString('utf8');
    let body = null;
    try { body = raw ? JSON.parse(raw) : null; } catch { body = raw; }
    received.push({ method: request.method, url: request.url, authorization: request.headers.authorization ?? null, body });
    const url = request.url ?? '';
    let status = 200;
    let payload = { value: [] };
    if (request.method === 'GET' && url.includes('/calendarView')) payload = { value: [{ id: 'event-1', subject: 'Planning' }] };
    else if (request.method === 'GET' && url.includes('/events/')) payload = { id: 'event-1', subject: 'Planning' };
    else if (request.method === 'GET' && url.includes('/calendars')) payload = { value: [{ id: 'calendar-1', name: 'Calendar' }] };
    else if (request.method === 'POST') { status = 201; payload = { id: 'created-1', subject: 'Allowed write' }; }
    else if (request.method === 'PATCH') payload = { id: 'event-1', subject: 'Updated write' };
    else if (request.method === 'DELETE') { status = 204; payload = null; }
    response.statusCode = status;
    if (payload === null) { response.end(); return; }
    const encoded = JSON.stringify(payload);
    response.setHeader('content-type', 'application/json');
    response.end(encoded);
  });
});

function run(command, args, env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { env, windowsHide: true });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    const timer = setTimeout(() => { child.kill(); reject(new Error('fixture_child_timeout:' + command)); }, 30000);
    child.on('error', (error) => { clearTimeout(timer); reject(error); });
    child.on('close', (code) => {
      clearTimeout(timer);
      if (code !== 0) { reject(new Error('fixture_child_exit:' + command + ':' + code + ':' + stderr.slice(-1000))); return; }
      try { resolve(stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))); }
      catch (error) { reject(new Error('fixture_child_output_invalid:' + command + ':' + error.message + ':' + stdout.slice(-1000))); }
    });
    child.stdin.end(requests.map((request) => JSON.stringify(request)).join('\n') + '\n');
  });
}

server.listen(0, '127.0.0.1', async () => {
  try {
    const port = server.address().port;
    const config = JSON.stringify({ graph_base_url: 'http://127.0.0.1:' + port + '/v1.0', allowed_mailboxes: ['fixture@example.test'], allow_event_writes: true, write_approval_token: 'approve-1' });
    for (const root of [bunRoot, rustRoot]) { mkdirSync(root + '/.ai', { recursive: true }); writeFileSync(root + '/.ai/calendar-mcp.json', config); }
    const env = { ...process.env, GRAPH_ACCESS_TOKEN: 'fixture-token', NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST: '1' };
    for (const key of ['MS_GRAPH_ACCESS_TOKEN', 'GRAPH_TENANT_ID', 'GRAPH_CLIENT_ID', 'GRAPH_CLIENT_SECRET', 'GRAPH_TOKEN_ENDPOINT', 'NARADA_CALENDAR_AUTHORITY_ENTRYPOINT', 'NARADA_CALENDAR_AUTHORITY_ARGS']) delete env[key];
    const bun = await run(bunCommand, [bunEntrypoint, '--site-root', bunRoot], env);
    const rust = await run(nativeExecutable, ['--surface-id', 'calendar', '--native-authority', '--site-root', rustRoot], env);
    server.close(() => process.stdout.write(JSON.stringify({ bun, rust, received }) + '\n'));
  } catch (error) {
    server.close(() => { process.stderr.write(String(error.stack ?? error) + '\n'); process.exit(1); });
  }
});
`;
  writeFileSync(fixtureScript, fixture, 'utf8');
  try {
    const result = spawnSync(process.execPath, [fixtureScript, executable, bunEntrypoint, process.env.NARADA_BUN_EXECUTABLE ?? 'bun', bunRoot, rustRoot], {
      cwd: workspaceRoot,
      env: cleanEnv,
      encoding: 'utf8',
      timeout: 90_000,
      maxBuffer: 4 * 1024 * 1024,
      windowsHide: true,
    });
    if (result.error) throw new Error('calendar_graph_fixture_spawn_failed:' + result.error.message);
    if (result.status !== 0) throw new Error('calendar_graph_fixture_exit:' + result.status + ':' + String(result.stderr).slice(-1500));
    const payload = JSON.parse(String(result.stdout).trim());
    for (const id of [1, 2, 3, 4, 5, 6, 7]) {
      assertSame('calendar.native_graph.' + id, mailboxStructured(payload.bun, id, 'bun'), mailboxStructured(payload.rust, id, 'rust'));
    }
    const expectedMethods = ['GET', 'GET', 'GET', 'POST', 'PATCH', 'DELETE'];
    assertSame('calendar.native_graph.bun_methods', payload.received.slice(0, 6).map((value) => value.method), expectedMethods);
    assertSame('calendar.native_graph.rust_methods', payload.received.slice(6, 12).map((value) => value.method), expectedMethods);
    if (payload.received.some((value) => value.authorization !== 'Bearer fixture-token')) throw new Error('calendar_graph_fixture_authorization_mismatch');
    const auditPath = join(rustRoot, '.ai', 'audit', 'calendar-mcp.jsonl');
    const auditKinds = readFileSync(auditPath, 'utf8').trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line).event_kind);
    assertSame('calendar.native_graph.audit', auditKinds, ['event_create_requested', 'event_create_completed', 'event_update_requested', 'event_update_completed', 'event_delete_requested', 'event_delete_completed', 'event_create_refused']);
    return { status: 'passed', fixture: 'loopback_graph_authority', compared: ['calendar_list', 'calendar_event_query', 'calendar_event_show', 'calendar_event_create', 'calendar_event_update', 'calendar_event_delete', 'write_refusal', 'audit'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runCloudflareParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'cloudflare-carrier-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('cloudflare_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-cloudflare-native-parity-'));
  try {
    const healthFile = join(root, 'cloudflare-health.json');
    writeFileSync(healthFile, JSON.stringify({
      generated_at: '2026-01-01T00:00:00Z',
      continuity_health: {
        local_sync_status: 'healthy',
        local_sync_artifact_count: 3,
        local_inbound_status: 'idle',
        local_inbound_artifact_count: 2,
        reconciliation_execution_status: 'ready',
        reconciliation_execution_plan_status: 'planned',
      },
      scheduler_task_readback: {
        scheduled_task_state: 'Ready',
        last_run_time: '2026-01-01T00:01:00Z',
        last_result: 'ok',
        next_run_time: '2026-01-01T01:00:00Z',
        cadence_status: 'hourly',
      },
      cloudflare_product_posture: {
        state: 'healthy',
        status: 'ok',
        site_product_overview: {
          site_count: 4,
          health_counts: { healthy: 4 },
          next_action: 'none',
          next_reason: null,
        },
      },
      cloudflare_product_binding_alignment: {
        state: 'aligned',
        status: 'ok',
        reason: null,
        local_site_count: 4,
        cloudflare_product_next_action: 'none',
      },
    }), 'utf8');
    const requests = [{
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: 'cloudflare_health', arguments: { health_file: healthFile } },
    }, {
      jsonrpc: '2.0',
      id: 2,
      method: 'tools/call',
      params: { name: 'cloudflare_session_status', arguments: { session_file: join(root, 'missing-session.json') } },
    }, {
      jsonrpc: '2.0',
      id: 3,
      method: 'tools/call',
      params: { name: 'cloudflare_doctor', arguments: {} },
    }];
    const env = { ...process.env, NARADA_ROOT: root };
    for (const key of ['CLOUDFLARE_CARRIER_URL', 'CLOUDFLARE_SESSION_FILE', 'CLOUDFLARE_HEALTH_FILE', 'NARADA_CLOUDFLARE_PROJECTION_REGISTRY_ROOT']) delete env[key];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--site-root', root], requests, workspaceRoot, env);
    const rust = runMailbox(executable, ['--surface-id', 'cloudflare-carrier', '--site-root', root], requests, workspaceRoot, env);
    assertSame('cloudflare.health', mailboxStructured(bun, 1, 'bun'), mailboxStructured(rust, 1, 'rust'));
    assertSame('cloudflare.session_status', mailboxStructured(bun, 2, 'bun'), mailboxStructured(rust, 2, 'rust'));
    assertSame('cloudflare.doctor', mailboxStructured(bun, 3, 'bun'), mailboxStructured(rust, 3, 'rust'));
    return { status: 'passed', fixture: 'local_health_projection', compared: ['health', 'session_status', 'doctor'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runGraphMailParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'graph-mail-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('graph_mail_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-graph-mail-native-parity-'));
  try {
    const configDir = join(root, '.ai');
    mkdirSync(configDir, { recursive: true });
    writeFileSync(join(configDir, 'graph-mail-mcp.json'), JSON.stringify({
      graph_base_url: 'https://graph.microsoft.com/v1.0///',
      allowed_mailboxes: ['fixture@example.test'],
      allowed_attachment_roots: ['attachments'],
      allow_device_code_auth: true,
      device_code_tenant_id: 'tenant-fixture',
      device_code_client_id: 'client-fixture',
      device_code_allowed_scopes: ['Mail.Read'],
      allow_send_draft: true,
      send_approval_token: '',
      allow_folder_create: true,
      allow_message_move: false,
      allow_message_mark_read: true,
      mailbox_organization_approval_token: 'org-fixture',
    }), 'utf8');
    const requests = [{
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: 'graph_mail_doctor', arguments: {} },
    }, {
      jsonrpc: '2.0',
      id: 2,
      method: 'tools/call',
      params: { name: 'graph_mail_auth_status', arguments: {} },
    }];
    const env = { ...process.env };
    for (const key of ['MS_GRAPH_ACCESS_TOKEN', 'GRAPH_ACCESS_TOKEN', 'GRAPH_TENANT_ID', 'GRAPH_CLIENT_ID', 'GRAPH_CLIENT_SECRET', 'GRAPH_TOKEN_ENDPOINT']) delete env[key];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--site-root', root], requests, workspaceRoot, env);
    const rust = runMailbox(executable, ['--surface-id', 'graph-mail', '--site-root', root], requests, workspaceRoot, env);
    assertSame('graph_mail.doctor', mailboxStructured(bun, 1, 'bun'), mailboxStructured(rust, 1, 'rust'));
    assertSame('graph_mail.auth_status', mailboxStructured(bun, 2, 'bun'), mailboxStructured(rust, 2, 'rust'));
    return { status: 'passed', fixture: 'local_policy_posture', compared: ['doctor', 'auth_status'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runGraphMailNativeGraphParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'graph-mail-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('graph_mail_native_graph_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-graph-mail-native-graph-'));
  const fixtureScript = join(root, 'graph-mail-fixture.mjs');
  const requests = [
    { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'graph_mail_query', arguments: { mailbox_id: 'fixture@example.test', limit: 2, query: 'needle' } } },
    { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'graph_mail_message_show', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1' } } },
    { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'graph_mail_folder_list', arguments: { mailbox_id: 'fixture@example.test', limit: 2 } } },
    { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'graph_mail_folder_create', arguments: { mailbox_id: 'fixture@example.test', display_name: 'Customers', confirm_write: true, approval_token: 'org-fixture' } } },
    { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'graph_mail_message_move', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', destination_folder_id: 'folder-2', confirm_write: true, approval_token: 'org-fixture' } } },
    { jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'graph_mail_message_mark_read', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', idempotency_key: 'fixture-mark-read', confirm_write: true, approval_token: 'org-fixture' } } },
    { jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name: 'graph_mail_attachment_list', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', limit: 2 } } },
    { jsonrpc: '2.0', id: 8, method: 'tools/call', params: { name: 'graph_mail_attachment_get', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', attachment_id: 'attachment-1' } } },
    { jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name: 'graph_mail_attachment_get', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', attachment_id: 'attachment-1', include_content: true } } },
    { jsonrpc: '2.0', id: 10, method: 'tools/call', params: { name: 'graph_mail_attachment_add', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', name: 'note.txt', content_type: 'text/plain', content_base64: 'SGVsbG8=' } } },
    { jsonrpc: '2.0', id: 11, method: 'tools/call', params: { name: 'graph_mail_attachment_delete', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', attachment_id: 'attachment-1' } } },
    { jsonrpc: '2.0', id: 12, method: 'tools/call', params: { name: 'graph_mail_draft_create', arguments: { mailbox_id: 'fixture@example.test', subject: 'Draft', body_text: 'Hello', to_recipients: ['person@example.test'] } } },
    { jsonrpc: '2.0', id: 13, method: 'tools/call', params: { name: 'graph_mail_reply_draft_create', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', comment: 'Thanks' } } },
    { jsonrpc: '2.0', id: 14, method: 'tools/call', params: { name: 'graph_mail_forward_draft_create', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', comment: 'FYI', to_recipients: ['forward@example.test'] } } },
    { jsonrpc: '2.0', id: 15, method: 'tools/call', params: { name: 'graph_mail_draft_update', arguments: { mailbox_id: 'fixture@example.test', draft_id: 'draft-1', subject: 'Updated' } } },
    { jsonrpc: '2.0', id: 16, method: 'tools/call', params: { name: 'graph_mail_draft_discard', arguments: { mailbox_id: 'fixture@example.test', draft_id: 'draft-1' } } },
    { jsonrpc: '2.0', id: 17, method: 'tools/call', params: { name: 'graph_mail_draft_send', arguments: { mailbox_id: 'fixture@example.test', draft_id: 'draft-1', confirm_send: true, approval_token: 'send-fixture' } } },
  ];
  const cleanEnv = { ...process.env, NARADA_GRAPH_MAIL_FIXTURE_REQUESTS: JSON.stringify(requests) };
  for (const key of ['MS_GRAPH_ACCESS_TOKEN', 'GRAPH_ACCESS_TOKEN', 'GRAPH_TENANT_ID', 'GRAPH_CLIENT_ID', 'GRAPH_CLIENT_SECRET', 'GRAPH_TOKEN_ENDPOINT', 'NARADA_NATIVE_GRAPH_MAIL_AUTHORITY', 'NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST']) delete cleanEnv[key];
  const fixture = String.raw`import { createServer } from 'node:http';
import { mkdirSync, writeFileSync } from 'node:fs';
import { spawn } from 'node:child_process';

const [nativeExecutable, bunEntrypoint, bunCommand, bunRoot, rustRoot] = process.argv.slice(2);
const requests = JSON.parse(process.env.NARADA_GRAPH_MAIL_FIXTURE_REQUESTS ?? '[]');
const received = [];
const server = createServer((request, response) => {
  const chunks = [];
  request.on('data', (chunk) => chunks.push(chunk));
  request.on('end', () => {
    const raw = Buffer.concat(chunks).toString('utf8');
    let body = null;
    try { body = raw ? JSON.parse(raw) : null; } catch { body = raw; }
    received.push({ method: request.method, url: request.url, authorization: request.headers.authorization ?? null, body });
    const url = request.url ?? '';
    let status = 200;
    let payload = { value: [] };
    if (request.method === 'GET' && url.includes('/messages/message-1/attachments/attachment-1')) payload = { id: 'attachment-1', name: 'note.txt', contentType: 'text/plain', contentBytes: 'SGVsbG8=', size: 5 };
    else if (request.method === 'GET' && url.includes('/messages/message-1/attachments')) payload = { value: [{ id: 'attachment-1', name: 'note.txt', contentType: 'text/plain', contentBytes: 'SGVsbG8=', size: 5 }] };
    else if (request.method === 'GET' && url.includes('/messages/draft-1')) payload = { id: 'draft-1', isDraft: true, changeKey: 'ck-1', subject: 'Draft' };
    else if (request.method === 'GET' && url.includes('/messages/message-1')) payload = { id: 'message-1', subject: 'Needle' };
    else if (request.method === 'GET' && url.includes('/messages')) payload = { value: [{ id: 'message-1', subject: 'Needle' }] };
    else if (request.method === 'GET' && url.includes('/mailFolders')) payload = { value: [{ id: 'folder-1', displayName: 'Inbox' }] };
    else if (request.method === 'POST' && url.endsWith('/mailFolders')) { status = 201; payload = { id: 'folder-2', displayName: 'Customers' }; }
    else if (request.method === 'POST' && url.includes('/move')) { status = 200; payload = { id: 'message-1', parentFolderId: 'folder-2' }; }
    else if (request.method === 'POST' && url.includes('/attachments')) { status = 201; payload = { id: 'attachment-2', name: 'note.txt', contentType: 'text/plain' }; }
    else if (request.method === 'POST' && url.includes('/createReply')) { status = 201; payload = { id: 'draft-reply', isDraft: true, subject: 'Re: Needle' }; }
    else if (request.method === 'POST' && url.includes('/createForward')) { status = 201; payload = { id: 'draft-forward', isDraft: true, subject: 'Fwd: Needle' }; }
    else if (request.method === 'POST' && url.endsWith('/messages')) { status = 201; payload = { id: 'draft-1', isDraft: true, subject: 'Draft' }; }
    else if (request.method === 'POST' && url.endsWith('/send')) { status = 202; payload = null; }
    else if (request.method === 'DELETE' && url.includes('/attachments/')) { status = 204; payload = null; }
    else if (request.method === 'DELETE' && url.includes('/messages/draft-1')) { status = 204; payload = null; }
    else if (request.method === 'PATCH' && url.includes('/messages/draft-1')) { status = 200; payload = { id: 'draft-1', isDraft: true, subject: 'Updated' }; }
    else if (request.method === 'PATCH') { status = 204; payload = null; }
    response.statusCode = status;
    if (payload === null) { response.end(); return; }
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify(payload));
  });
});

function run(command, args, env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { env, windowsHide: true });
    let stdout = '';
    let stderr = '';
    const timer = setTimeout(() => { child.kill(); reject(new Error('graph_mail_fixture_child_timeout:' + command)); }, 30000);
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.on('error', (error) => { clearTimeout(timer); reject(error); });
    child.on('close', (code) => {
      clearTimeout(timer);
      if (code !== 0) { reject(new Error('graph_mail_fixture_child_exit:' + command + ':' + code + ':' + stderr.slice(-1000))); return; }
      try { resolve(stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))); }
      catch (error) { reject(new Error('graph_mail_fixture_child_output_invalid:' + error.message + ':' + stdout.slice(-1000))); }
    });
    child.stdin.end(requests.map((request) => JSON.stringify(request)).join('\n') + '\n');
  });
}

server.listen(0, '127.0.0.1', async () => {
  try {
    const port = server.address().port;
    const config = JSON.stringify({ graph_base_url: 'http://127.0.0.1:' + port + '/v1.0', allowed_mailboxes: ['fixture@example.test'], allow_folder_create: true, allow_message_move: true, allow_message_mark_read: true, mailbox_organization_approval_token: 'org-fixture', allow_send_draft: true, send_approval_token: 'send-fixture' });
    for (const root of [bunRoot, rustRoot]) { mkdirSync(root + '/.ai', { recursive: true }); writeFileSync(root + '/.ai/graph-mail-mcp.json', config); }
    const env = { ...process.env, GRAPH_ACCESS_TOKEN: 'fixture-token', NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST: '1' };
    for (const key of ['MS_GRAPH_ACCESS_TOKEN', 'GRAPH_TENANT_ID', 'GRAPH_CLIENT_ID', 'GRAPH_CLIENT_SECRET', 'GRAPH_TOKEN_ENDPOINT', 'NARADA_GRAPH_MAIL_AUTHORITY_ENTRYPOINT', 'NARADA_GRAPH_MAIL_AUTHORITY_ARGS']) delete env[key];
    const bun = await run(bunCommand, [bunEntrypoint, '--site-root', bunRoot], env);
    const rust = await run(nativeExecutable, ['--surface-id', 'graph-mail', '--native-authority', '--site-root', rustRoot], env);
    server.close(() => process.stdout.write(JSON.stringify({ bun, rust, received }) + '\n'));
  } catch (error) {
    server.close(() => { process.stderr.write(String(error.stack ?? error) + '\n'); process.exit(1); });
  }
});
`;
  writeFileSync(fixtureScript, fixture, 'utf8');
  try {
    const result = spawnSync(process.execPath, [fixtureScript, executable, bunEntrypoint, process.env.NARADA_BUN_EXECUTABLE ?? 'bun', join(root, 'bun'), join(root, 'rust')], {
      cwd: workspaceRoot,
      env: cleanEnv,
      encoding: 'utf8',
      timeout: 90_000,
      maxBuffer: 4 * 1024 * 1024,
      windowsHide: true,
    });
    if (result.error) throw new Error('graph_mail_native_graph_fixture_spawn_failed:' + result.error.message);
    if (result.status !== 0) throw new Error('graph_mail_native_graph_fixture_exit:' + result.status + ':' + String(result.stderr).slice(-1500));
    const payload = JSON.parse(String(result.stdout).trim());
    for (const id of [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17]) assertSame('graph_mail.native_graph.' + id, mailboxStructured(payload.bun, id, 'bun'), mailboxStructured(payload.rust, id, 'rust'));
    const expectedMethods = ['GET', 'GET', 'GET', 'POST', 'POST', 'PATCH', 'GET', 'GET', 'GET', 'POST', 'DELETE', 'POST', 'POST', 'POST', 'PATCH', 'GET', 'DELETE', 'POST'];
    assertSame('graph_mail.native_graph.bun_methods', payload.received.slice(0, expectedMethods.length).map((value) => value.method), expectedMethods);
    assertSame('graph_mail.native_graph.rust_methods', payload.received.slice(expectedMethods.length, expectedMethods.length * 2).map((value) => value.method), expectedMethods);
    if (payload.received.some((value) => value.authorization !== 'Bearer fixture-token')) throw new Error('graph_mail_native_graph_fixture_authorization_mismatch');
    const auditPath = join(root, 'rust', '.ai', 'audit', 'graph-mail-mcp.jsonl');
    const auditKinds = readFileSync(auditPath, 'utf8').trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line).event_kind);
    assertSame('graph_mail.native_graph.audit', auditKinds, ['folder_create_requested', 'folder_create_completed', 'message_move_requested', 'message_move_completed', 'message_mark_read_requested', 'message_mark_read_completed', 'draft_create_requested', 'draft_create_completed', 'createReply_requested', 'createReply_completed', 'createForward_requested', 'createForward_completed', 'draft_update_requested', 'draft_update_completed', 'draft_discard_requested', 'draft_discard_completed', 'draft_send_requested', 'draft_send_completed']);
    return { status: 'passed', fixture: 'loopback_graph_mail_authority', compared: ['query', 'message_show', 'folder_list', 'folder_create', 'message_move', 'message_mark_read', 'attachment_list', 'attachment_get_metadata', 'attachment_get_content', 'attachment_add', 'attachment_delete', 'draft_create', 'reply_draft_create', 'forward_draft_create', 'draft_update', 'draft_discard', 'draft_send', 'audit'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runOperatorOverlayParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'operator-console-overlay-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('operator_overlay_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const naradaRoot = resolve(workspaceRoot, '..', 'narada');
  const root = mkdtempSync(join(tmpdir(), 'narada-operator-overlay-native-parity-'));
  try {
    const stateRoot = join(root, 'overlay-state');
    const stateDirectory = join(stateRoot, 'operator-console');
    mkdirSync(stateDirectory, { recursive: true });
    writeFileSync(join(stateDirectory, 'document.json'), JSON.stringify({
      schema: 'narada.window_surface_overlay.document.v1',
      id: 'operator-console',
      title: 'Fixture overlay',
      title_tone: 'default',
      subtitle: null,
      rows: [{ label: 'Status', value: 'ready', tone: 'success' }],
      actions: [],
      updated_at: '2026-01-01T00:00:00.000Z',
    }), 'utf8');
    writeFileSync(join(stateDirectory, 'action-state.json'), JSON.stringify({
      schema: 'narada.window_surface_overlay.action_state.v1',
      action_id: 'refresh',
      request_id: 'fixture-request',
      status: 'succeeded',
      started_at: '2026-01-01T00:00:00.000Z',
      finished_at: '2026-01-01T00:00:01.000Z',
    }), 'utf8');
    writeFileSync(join(stateDirectory, 'visibility.state.json'), JSON.stringify({
      schema: 'narada.window_surface_overlay.visibility_state.v1',
      state: 'visible',
    }), 'utf8');
    writeFileSync(join(stateRoot, 'surface.snapshot.json'), JSON.stringify({
      schema: 'narada.window_surface_overlay.surface_snapshot.v1',
      status: 'ready',
    }), 'utf8');
    writeFileSync(join(stateRoot, 'focus.owner.json'), JSON.stringify({
      schema: 'narada.window_surface_overlay.focus_owner.v1',
      owner: 'fixture',
    }), 'utf8');
    const requests = [{
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: 'operator_console_overlay_status', arguments: {} },
    }];
    const env = { ...process.env, NARADA_WINDOW_SURFACE_OVERLAY_STATE_ROOT: stateRoot };
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--narada-root', naradaRoot], requests, workspaceRoot, env);
    const rust = runMailbox(executable, ['--surface-id', 'operator-console-overlay', '--site-root', naradaRoot], requests, workspaceRoot, env);
    assertSame('operator_overlay.status', mailboxStructured(bun, 1, 'bun'), mailboxStructured(rust, 1, 'rust'));
    return { status: 'passed', fixture: 'persisted_overlay_state', compared: ['status'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runBrowserControlParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'browser-control-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('browser_control_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-browser-control-native-parity-'));
  try {
    const requests = [{
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: 'browser_control_session_inventory', arguments: {} },
    }];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--site-root', root], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'browser-control', '--site-root', root], requests, workspaceRoot);
    assertSame('browser_control.session_inventory', mailboxStructured(bun, 1, 'bun'), mailboxStructured(rust, 1, 'rust'));
    return { status: 'passed', fixture: 'no_attached_sessions', compared: ['session_inventory'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runQuotaMeterParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'quota-meter-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('quota_meter_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-quota-meter-native-parity-'));
  try {
    const stateRoot = join(root, 'quota-state');
    mkdirSync(stateRoot, { recursive: true });
    writeFileSync(join(stateRoot, 'overlay-position.json'), JSON.stringify({
      left: 42,
      top: 24,
      updatedAt: '2026-01-01T00:00:00.000Z',
    }), 'utf8');
    const requests = [{
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: 'quota_meter_overlay_status', arguments: {} },
    }];
    const env = { ...process.env, QUOTA_METER_STATE_ROOT: stateRoot };
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--state-root', stateRoot], requests, workspaceRoot, env);
    const rust = runMailbox(executable, ['--surface-id', 'quota-meter', '--site-root', root], requests, workspaceRoot, env);
    assertSame('quota_meter.overlay_status', mailboxStructured(bun, 1, 'bun'), mailboxStructured(rust, 1, 'rust'));
    return { status: 'passed', fixture: 'persisted_position_without_pid', compared: ['overlay_status'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runNarsSessionParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'nars-session-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('nars_session_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-nars-session-native-parity-'));
  try {
    const sessionDirectory = join(root, '.narada', 'crew', 'nars-sessions', 'session_test');
    mkdirSync(sessionDirectory, { recursive: true });
    writeFileSync(join(sessionDirectory, 'session-index-record.json'), JSON.stringify({
      schema: 'narada.nars.session_index_record.v1',
      session_id: 'session_test',
      carrier_session_id: 'carrier_test',
      nars_session_id: 'nars_test',
      site_id: 'test-site',
      site_root: root,
      agent_id: 'fixture-agent',
      runtime_kind: 'fixture-runtime',
      launch_operator_surface_kind: 'codex',
      status_hint: 'closed',
      started_at: '2026-01-01T00:00:00.000Z',
      last_seen_at: '2026-01-01T00:01:00.000Z',
      terminal_state: 'closed',
      event_endpoint: null,
      health_endpoint: null,
      authority_runtime_id: 'runtime_test',
      authority_epoch: 2,
      source_write_admission: 'inactive',
    }), 'utf8');
    const requests = [{
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: 'nars_session_list', arguments: { include_health: false, limit: 10 } },
    }, {
      jsonrpc: '2.0',
      id: 2,
      method: 'tools/call',
      params: { name: 'nars_session_show', arguments: { session_id: 'session_test', include_health: false } },
    }];
    const env = { ...process.env, NARADA_SITE_ROOT: root, NARADA_SITE_ID: 'test-site', NARADA_AGENT_ID: 'fixture.agent' };
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint], requests, workspaceRoot, env);
    const rust = runMailbox(executable, ['--surface-id', 'nars-session', '--site-root', root], requests, workspaceRoot, env);
    assertSame('nars_session.list', mailboxStructured(bun, 1, 'bun'), mailboxStructured(rust, 1, 'rust'));
    assertSame('nars_session.show', mailboxStructured(bun, 2, 'bun'), mailboxStructured(rust, 2, 'rust'));
    return { status: 'passed', fixture: 'closed_local_session_projection', compared: ['list', 'show'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

const mailboxParity = runMailboxParity();
const delegatedTaskParity = runDelegatedTaskParity();
const workerDelegationParity = runWorkerDelegationParity();
const artifactsParity = runArtifactsParity();
const sopParity = runSopParity();
const sopActionParity = runSopActionParity();
const sopRunListParity = runSopRunListParity();
const sopRunEventsParity = runSopRunEventsParity();
const sopRunStatusParity = runSopRunStatusParity();
const sopHandoffParity = runSopHandoffParity();
const sopRunCoverageParity = runSopRunCoverageParity();
const sopOutboxParity = runSopOutboxParity();
const surfaceFeedbackParity = runSurfaceFeedbackParity();
const siteLoopParity = runSiteLoopParity();
const calendarParity = runCalendarParity();
const calendarAuthorityBridge = runCalendarAuthorityBridge();
const calendarNativeGraphParity = runCalendarNativeGraphParity();
const graphMailNativeGraphParity = runGraphMailNativeGraphParity();
const cloudflareParity = runCloudflareParity();
const graphMailParity = runGraphMailParity();
const operatorOverlayParity = runOperatorOverlayParity();
const browserControlParity = runBrowserControlParity();
const quotaMeterParity = runQuotaMeterParity();
const narsSessionParity = runNarsSessionParity();
process.stdout.write(JSON.stringify({
  schema: 'narada.mcp_surfaces_native.protocol_parity.v1',
  status: 'passed',
  surfaces: surfaces.length,
  legacy: '2024-11-05',
  modern: '2026-07-28',
  defaults_changed: false,
  mailbox_parity: mailboxParity,
  delegated_task_parity: delegatedTaskParity,
  worker_delegation_parity: workerDelegationParity,
  artifacts_parity: artifactsParity,
  sop_parity: sopParity,
  sop_action_parity: sopActionParity,
  sop_run_list_parity: sopRunListParity,
  sop_run_events_parity: sopRunEventsParity,
  sop_run_status_parity: sopRunStatusParity,
  sop_handoff_parity: sopHandoffParity,
  sop_run_coverage_parity: sopRunCoverageParity,
  sop_outbox_parity: sopOutboxParity,
  surface_feedback_parity: surfaceFeedbackParity,
  site_loop_parity: siteLoopParity,
  calendar_parity: calendarParity,
  calendar_authority_bridge: calendarAuthorityBridge,
  calendar_native_graph_parity: calendarNativeGraphParity,
  graph_mail_native_graph_parity: graphMailNativeGraphParity,
  cloudflare_parity: cloudflareParity,
  graph_mail_parity: graphMailParity,
  operator_overlay_parity: operatorOverlayParity,
  browser_control_parity: browserControlParity,
  quota_meter_parity: quotaMeterParity,
  nars_session_parity: narsSessionParity,
}) + '\n');
