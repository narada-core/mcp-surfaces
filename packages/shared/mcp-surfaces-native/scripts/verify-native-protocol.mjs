import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
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

function runMailbox(command, args, requests, cwd) {
  const result = spawnSync(command, [...args], {
    cwd,
    input: requests.map((request) => JSON.stringify(request)).join('\n') + '\n',
    encoding: 'utf8',
    timeout: 15_000,
    maxBuffer: 2 * 1024 * 1024,
    windowsHide: true,
  });
  if (result.error) throw new Error('mailbox_parity_spawn_failed:' + command + ':' + result.error.message);
  if (result.status !== 0) throw new Error('mailbox_parity_exit:' + command + ':' + result.status + ':' + String(result.stderr).slice(0, 500));
  const lines = String(result.stdout).trim().split(/\r?\n/).filter(Boolean);
  if (lines.length !== requests.length) throw new Error('mailbox_parity_response_count:' + command + ':' + lines.length);
  return lines.map((line) => JSON.parse(line));
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

const mailboxParity = runMailboxParity();
process.stdout.write(JSON.stringify({
  schema: 'narada.mcp_surfaces_native.protocol_parity.v1',
  status: 'passed',
  surfaces: surfaces.length,
  legacy: '2024-11-05',
  modern: '2026-07-28',
  defaults_changed: false,
  mailbox_parity: mailboxParity,
}) + '\n');
