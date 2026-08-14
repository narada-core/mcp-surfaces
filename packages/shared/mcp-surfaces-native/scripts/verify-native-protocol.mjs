import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';
import { runSopEngineParity } from './verify-sop-engine-parity.mjs';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const executableName = process.platform === 'win32' ? 'narada-mcp-surfaces.exe' : 'narada-mcp-surfaces';
const executable = process.env.NARADA_NATIVE_SURFACE_EXECUTABLE
  ?? requireLocalNativeArtifact(packageRoot, executableName);

function requireLocalNativeArtifact(root, artifactName) {
  const pointerPath = join(root, 'dist', 'native', 'current.json');
  if (!existsSync(pointerPath)) throw new Error(`native_surface_artifact_pointer_missing:${pointerPath}`);
  const pointer = JSON.parse(readFileSync(pointerPath, 'utf8'));
  const relative = pointer?.artifacts?.[artifactName];
  if (typeof relative !== 'string' || !relative) throw new Error(`native_surface_artifact_missing:${artifactName}`);
  const artifact = resolve(dirname(pointerPath), relative);
  if (!existsSync(artifact)) throw new Error(`native_surface_artifact_path_missing:${artifact}`);
  return artifact;
}
const paritySlice = process.argv[2] ?? process.env.NARADA_PARITY_SLICE ?? 'all';
let controlPlaneReady = false;
function resolveNaradaRoot(workspaceRoot) {
  return resolve(process.env.NARADA_ROOT ?? resolve(workspaceRoot, '..', 'narada'));
}
function ensureControlPlaneBuild(workspaceRoot) {
  if (controlPlaneReady) return resolveNaradaRoot(workspaceRoot);
  const naradaRoot = resolveNaradaRoot(workspaceRoot);
  const packagePath = join(naradaRoot, 'packages', 'layers', 'control-plane', 'package.json');
  const entrypoint = join(naradaRoot, 'packages', 'layers', 'control-plane', 'dist', 'index.js');
  if (!existsSync(packagePath)) throw new Error(`native_parity_narada_root_missing:${naradaRoot}:set_NARADA_ROOT`);
  const buildCommand = process.platform === 'win32' ? (process.env.ComSpec ?? 'cmd.exe') : 'pnpm';
  const buildArgs = process.platform === 'win32'
    ? ['/d', '/s', '/c', 'pnpm --filter @narada-core/control-plane build']
    : ['--filter', '@narada-core/control-plane', 'build'];
  const build = spawnSync(buildCommand, buildArgs, {
    cwd: naradaRoot,
    env: process.env,
    encoding: 'utf8',
    timeout: 120_000,
    maxBuffer: 2 * 1024 * 1024,
    windowsHide: true,
  });
  if (build.error || build.status !== 0 || !existsSync(entrypoint)) {
    throw new Error(`native_parity_control_plane_build_failed:${build.error?.message ?? build.status}:${String(build.stderr).slice(0, 1000)}`);
  }
  controlPlaneReady = true;
  return naradaRoot;
}
const allSurfaces = [
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
const surfaces = paritySlice === 'all' ? allSurfaces : allSurfaces.includes(paritySlice) ? [paritySlice] : [];
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

function verifyCatalogObservationContract() {
  const observedAt = '2026-08-14T00:00:00Z';
  const requests = [
    { jsonrpc: '2.0', id: 101, method: 'tools/call', params: { name: 'catalog_observation_guidance', arguments: {} } },
    { jsonrpc: '2.0', id: 102, method: 'tools/call', params: { name: 'catalog_observation_observe', arguments: { provider_id: 'inference-provider:test', observed_at: observedAt, access_mode: 'credentialed' } } },
    { jsonrpc: '2.0', id: 103, method: 'tools/call', params: { name: 'catalog_observation_observe', arguments: { provider_id: 'inference-provider:test', observed_at: 'not-an-instant' } } },
    { jsonrpc: '2.0', id: 104, method: 'tools/call', params: { name: 'catalog_observation_observe', arguments: { provider_id: 'inference-provider:test', observed_at: observedAt, access_mode: 'ambient' } } },
  ];
  const result = spawnSync(executable, ['--surface-id', 'catalog-observation', '--site-root', packageRoot], {
    input: requests.map((request) => JSON.stringify(request)).join('\n') + '\n',
    encoding: 'utf8', timeout: 10_000, maxBuffer: 512 * 1024, windowsHide: true,
  });
  if (result.error || result.status !== 0) throw new Error(`catalog-observation:contract_spawn_failed:${result.error?.message ?? result.status}:${String(result.stderr).slice(0, 500)}`);
  const responses = String(result.stdout).trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
  if (responses.length !== requests.length) throw new Error(`catalog-observation:contract_response_count:${responses.length}`);
  const byId = new Map(responses.map((response) => [response.id, response]));
  if (byId.get(101)?.result?.structuredContent?.capability_status !== 'contract_only_until_observation_port_installed') throw new Error('catalog-observation:guidance_not_truthful');
  const unavailable = byId.get(102)?.result;
  if (unavailable?.isError !== true || unavailable?.structuredContent?.status !== 'unavailable' || unavailable?.structuredContent?.models?.length !== 0) throw new Error('catalog-observation:unavailable_contract_invalid');
  if (unavailable?.structuredContent?.requested_access_mode !== 'credentialed') throw new Error('catalog-observation:requested_access_mode_missing');
  if (JSON.stringify(unavailable).includes('credential_value')) throw new Error('catalog-observation:credential_leak');
  if (byId.get(103)?.error?.data?.code !== 'catalog_observation_observed_at_invalid') throw new Error('catalog-observation:invalid_instant_diagnostic_missing');
  if (byId.get(104)?.error?.data?.code !== 'catalog_observation_access_mode_invalid') throw new Error('catalog-observation:invalid_access_diagnostic_missing');
}

if (surfaces.includes('catalog-observation')) verifyCatalogObservationContract();

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

function seedMailboxOutbox(root) {
  const directory = join(root, '.narada', 'runtime', 'mailbox-domain');
  mkdirSync(directory, { recursive: true });
  const db = new DatabaseSync(join(directory, 'mailbox-domain.db'));
  try {
    db.exec(`
      create table mailbox_outbox(
        event_id text primary key,
        scope_id text not null,
        topic text not null,
        aggregate_id text not null,
        aggregate_revision integer not null,
        schema_version integer not null,
        causation_id text not null,
        idempotency_key text not null unique,
        partition_key text not null,
        occurred_at text not null,
        payload_json text not null
      );
      create table mailbox_outbox_consumers(
        consumer_id text primary key,
        scope_id text,
        topics_json text,
        start_at text not null,
        created_at text not null
      );
      create table mailbox_outbox_receipts(
        consumer_id text not null references mailbox_outbox_consumers(consumer_id),
        event_id text not null references mailbox_outbox(event_id),
        receipt_fingerprint text not null,
        receipt_json text not null,
        acknowledged_at text not null,
        primary key(consumer_id, event_id)
      );
    `);
    db.prepare(`
      insert into mailbox_outbox(
        event_id,scope_id,topic,aggregate_id,aggregate_revision,schema_version,
        causation_id,idempotency_key,partition_key,occurred_at,payload_json
      ) values (?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      'event-1', 'support', 'topic.alpha', 'aggregate-1', 1, 1,
      'cause-1', 'event-key-1', 'partition-1', '2026-08-01T00:00:00.000Z',
      JSON.stringify({ schema: 'fixture.event.v1', value: 1 }),
    );
  } finally {
    db.close();
  }
}

function runMailboxOutboxMutationParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'mailbox-mcp', 'src', 'main.ts');
  const bunRoot = mkdtempSync(join(tmpdir(), 'narada-mailbox-outbox-bun-'));
  const rustRoot = mkdtempSync(join(tmpdir(), 'narada-mailbox-outbox-rust-'));
  try {
    seedMailboxOutbox(bunRoot);
    seedMailboxOutbox(rustRoot);
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'mailbox_outbox_consumer_register', arguments: { consumer_id: 'consumer-1', scope_id: 'support', topics: ['topic.beta', 'topic.alpha'], start_at: '2026-08-01T00:00:00.000Z' } } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'mailbox_outbox_consumer_show', arguments: { consumer_id: 'consumer-1' } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'mailbox_outbox_list', arguments: { consumer_id: 'consumer-1', limit: 10 } } },
      { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'mailbox_outbox_ack', arguments: { consumer_id: 'consumer-1', event_id: 'event-1', receipt: { schema: 'fixture.receipt.v1', outcome: 'completed', effect_ref: 'effect:1' } } } },
      { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'mailbox_outbox_ack', arguments: { consumer_id: 'consumer-1', event_id: 'event-1', receipt: { schema: 'fixture.receipt.v1', outcome: 'completed', effect_ref: 'effect:1' } } } },
      { jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'mailbox_outbox_list', arguments: { consumer_id: 'consumer-1', limit: 10 } } },
    ];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--site-root', bunRoot], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'mailbox', '--site-root', rustRoot], requests, workspaceRoot);
    const bunRegistered = mailboxStructured(bun, 1, 'bun');
    const rustRegistered = mailboxStructured(rust, 1, 'rust');
    for (const field of ['consumer_id', 'scope_id', 'topics_json', 'start_at']) {
      assertSame('mailbox.outbox.register.' + field, bunRegistered.consumer?.[field], rustRegistered.consumer?.[field]);
    }
    if (!/^\d{4}-\d{2}-\d{2}T/.test(String(rustRegistered.consumer?.created_at))) throw new Error('mailbox_outbox_rust_created_at_invalid');
    const bunShown = mailboxStructured(bun, 2, 'bun');
    const rustShown = mailboxStructured(rust, 2, 'rust');
    for (const field of ['status']) assertSame('mailbox.outbox.show.' + field, bunShown[field], rustShown[field]);
    for (const field of ['consumer_id', 'scope_id', 'topics', 'start_at']) {
      assertSame('mailbox.outbox.show.consumer.' + field, bunShown.consumer?.[field], rustShown.consumer?.[field]);
    }
    assertSame('mailbox.outbox.list', mailboxStructured(bun, 3, 'bun'), mailboxStructured(rust, 3, 'rust'));
    assertSame('mailbox.outbox.ack', mailboxStructured(bun, 4, 'bun'), mailboxStructured(rust, 4, 'rust'));
    assertSame('mailbox.outbox.ack_replay', mailboxStructured(bun, 5, 'bun'), mailboxStructured(rust, 5, 'rust'));
    assertSame('mailbox.outbox.drained', mailboxStructured(bun, 6, 'bun'), mailboxStructured(rust, 6, 'rust'));
    return { status: 'passed', fixture: 'durable_scoped_outbox', compared: ['consumer_register', 'consumer_show', 'list', 'ack', 'ack_replay', 'drained'] };
  } finally {
    rmSync(bunRoot, { recursive: true, force: true });
    rmSync(rustRoot, { recursive: true, force: true });
  }
}

function seedMailboxReconciliation(root) {
  seedMailboxOutbox(root);
  const scopeRoot = join(root, '.narada', 'runtime', 'mailboxes', 'support');
  mkdirSync(join(root, 'config'), { recursive: true });
  mkdirSync(join(scopeRoot, '.narada'), { recursive: true });
  writeFileSync(join(root, 'config', 'config.json'), JSON.stringify({
    root_dir: '.narada/runtime/mailboxes/support',
    scopes: [{
      scope_id: 'support',
      root_dir: '.narada/runtime/mailboxes/support',
      sources: [{ type: 'graph' }],
      graph: { user_id: 'support@example.test', prefer_immutable_ids: true },
      scope: { included_container_refs: ['inbox'], included_item_kinds: ['message'] },
      normalize: { attachment_policy: 'metadata_only', body_policy: 'text_only', include_headers: false, tombstones_enabled: true },
      runtime: { polling_interval_ms: 60000, acquire_lock_timeout_ms: 1000, cleanup_tmp_on_startup: true, rebuild_views_after_sync: false, rebuild_search_after_sync: false },
      admission: { mail: { included_folder_refs: ['inbox'], allowed_sender_domains: ['allowed.test'], unknown_sender_behavior: 'ignore' } },
    }],
  }), 'utf8');
  const domainPath = join(root, '.narada', 'runtime', 'mailbox-domain', 'mailbox-domain.db');
  const domain = new DatabaseSync(domainPath);
  try {
    domain.exec(`
      create table mailbox_sync_generations(
        generation_id text primary key,idempotency_key text not null unique,request_fingerprint text not null,
        scope_id text not null,config_fingerprint text not null,status text not null,parent_cursor text,next_cursor text,
        batch_path text,batch_sha256 text,batch_record_count integer not null default 0,staged_at text,receipt_json text,
        error_message text,lease_token text,lease_expires_at text,created_at text not null,updated_at text not null,completed_at text
      );
      create table mailbox_sync_generation_records(
        generation_id text not null references mailbox_sync_generations(generation_id),record_id text not null,ordinal text,
        fact_id text not null,event_kind text not null,message_id text,mailbox_id text,conversation_id text,source_version text,
        application_status text not null,primary key(generation_id,record_id)
      );
      create table mailbox_sync_scope_leases(scope_id text primary key,generation_id text not null,lease_token text not null,expires_at text not null,updated_at text not null);
      create table mailbox_message_observations(
        observation_id text primary key,mailbox_id text not null,message_id text not null,first_generation_id text not null,
        first_fact_id text not null,observed_at text not null,unique(mailbox_id,message_id)
      );
      create table mailbox_admission_receipts(
        admission_id text primary key,idempotency_key text not null unique,request_fingerprint text not null,scope_id text not null,
        fact_id text not null,policy_version text not null,decision_json text not null,created_at text not null
      );
      create table mailbox_reconciliation_operations(
        operation_id text primary key,idempotency_key text not null unique,request_fingerprint text not null,scope_id text not null,
        generation_id text not null,result_json text not null,created_at text not null
      );
    `);
    domain.prepare(`
      insert into mailbox_sync_generations(
        generation_id,idempotency_key,request_fingerprint,scope_id,config_fingerprint,status,batch_record_count,created_at,updated_at,completed_at
      ) values (?,?,?,?,?,'completed',1,?,?,?)
    `).run('generation-1', 'sync-key-1', 'request-1', 'support', 'config-1', '2026-08-01T00:00:00.000Z', '2026-08-01T00:00:00.000Z', '2026-08-01T00:00:00.000Z');
    domain.prepare(`
      insert into mailbox_sync_generation_records(
        generation_id,record_id,fact_id,event_kind,message_id,mailbox_id,conversation_id,source_version,application_status
      ) values (?,?,?,?,?,?,?,?,?)
    `).run('generation-1', 'record-1', 'fact-1', 'upsert', 'message-1', 'support', 'conversation-1', 'v1', 'projected');
  } finally {
    domain.close();
  }
  const facts = new DatabaseSync(join(scopeRoot, '.narada', 'facts.db'));
  try {
    facts.exec(`
      create table facts(
        fact_id text primary key,fact_type text not null,source_id text not null,source_record_id text not null,
        source_version text,source_cursor text,provenance_json text not null,payload_json text not null,
        created_at text not null,admitted_at text
      );
    `);
    const provenance = { source_id: 'support', source_record_id: 'record-1', source_version: 'v1', source_cursor: 'cursor-1', observed_at: '2026-08-01T00:00:00.000Z' };
    const payload = {
      record_id: 'record-1', ordinal: '2026-08-01T00:00:00.000Z',
      event: {
        mailbox_id: 'support', message_id: 'message-1', event_kind: 'upsert',
        payload: {
          mailbox_id: 'support', message_id: 'message-1', conversation_id: 'conversation-1',
          internet_message_id: '<message-1@example.test>', subject: 'Fixture subject',
          from: { email: 'sender@allowed.test' }, folder_refs: ['inbox'],
          body: { text: 'secret body must not cross the admission receipt' },
        },
      },
    };
    facts.prepare(`
      insert into facts(fact_id,fact_type,source_id,source_record_id,source_version,source_cursor,provenance_json,payload_json,created_at)
      values (?,?,?,?,?,?,?,?,?)
    `).run('fact-1', 'mail.message.discovered', 'support', 'record-1', 'v1', 'cursor-1', JSON.stringify(provenance), JSON.stringify(payload), '2026-08-01T00:00:00.000Z');
  } finally {
    facts.close();
  }
}

function runMailboxReconciliationParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const naradaRoot = ensureControlPlaneBuild(workspaceRoot);
  const bunEntrypoint = join(workspaceRoot, 'packages', 'mailbox-mcp', 'src', 'main.ts');
  const bunRoot = mkdtempSync(join(tmpdir(), 'narada-mailbox-reconcile-bun-'));
  const rustRoot = mkdtempSync(join(tmpdir(), 'narada-mailbox-reconcile-rust-'));
  try {
    seedMailboxReconciliation(bunRoot);
    seedMailboxReconciliation(rustRoot);
    const firstEventId = 'mbe_' + createHash('sha256').update('first-observed\0support\0message-1').digest('hex').slice(0, 40);
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'mailbox_reconcile_first_observations', arguments: { idempotency_key: 'reconcile-1', generation_id: 'generation-1', scope_id: 'support', limit: 10 } } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'mailbox_reconcile_first_observations', arguments: { idempotency_key: 'reconcile-1', generation_id: 'generation-1', scope_id: 'support', limit: 10 } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'mailbox_outbox_consumer_register', arguments: { consumer_id: 'consumer-1', scope_id: 'support', topics: ['mailbox.message.first_observed'], start_at: '2026-01-01T00:00:00.000Z' } } },
      { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'mailbox_outbox_list', arguments: { consumer_id: 'consumer-1', limit: 10 } } },
      { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'mailbox_message_admit', arguments: { idempotency_key: 'admit-1', fact_id: 'fact-1', source_event_id: firstEventId, scope_id: 'support' } } },
      { jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'mailbox_message_admit', arguments: { idempotency_key: 'admit-2', fact_id: 'fact-1', source_event_id: firstEventId, scope_id: 'support' } } },
      { jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name: 'mailbox_admission_show', arguments: { fact_id: 'fact-1', scope_id: 'support' } } },
      { jsonrpc: '2.0', id: 8, method: 'tools/call', params: { name: 'mailbox_outbox_consumer_register', arguments: { consumer_id: 'decision-consumer', scope_id: 'support', topics: ['mailbox.message.admitted', 'mailbox.message.rejected'], start_at: '2026-01-01T00:00:00.000Z' } } },
      { jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name: 'mailbox_outbox_list', arguments: { consumer_id: 'decision-consumer', limit: 10 } } },
      { jsonrpc: '2.0', id: 10, method: 'tools/call', params: { name: 'mailbox_message_fact_find', arguments: { scope_id: 'support', message_id: 'message-1' } } },
      { jsonrpc: '2.0', id: 11, method: 'tools/call', params: { name: 'mailbox_fact_show', arguments: { scope_id: 'support', fact_id: 'fact-1', include_content: false } } },
      { jsonrpc: '2.0', id: 12, method: 'tools/call', params: { name: 'mailbox_fact_show', arguments: { scope_id: 'support', fact_id: 'fact-1', include_content: true } } },
      { jsonrpc: '2.0', id: 13, method: 'tools/call', params: { name: 'mailbox_generation_show', arguments: { generation_id: 'generation-1' } } },
    ];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--site-root', bunRoot, '--control-plane-root', naradaRoot], requests, workspaceRoot);
    const rust = runMailbox(executable, ['--surface-id', 'mailbox', '--site-root', rustRoot], requests, workspaceRoot);
    assertSame('mailbox.reconcile.first', mailboxStructured(bun, 1, 'bun'), mailboxStructured(rust, 1, 'rust'));
    assertSame('mailbox.reconcile.replay', mailboxStructured(bun, 2, 'bun'), mailboxStructured(rust, 2, 'rust'));
    const bunPage = mailboxStructured(bun, 4, 'bun');
    const rustPage = mailboxStructured(rust, 4, 'rust');
    assertSame('mailbox.reconcile.outbox.count', bunPage.count, rustPage.count);
    for (const field of ['schema', 'event_id', 'scope_id', 'topic', 'aggregate_id', 'aggregate_revision', 'schema_version', 'causation_id', 'idempotency_key', 'partition_key', 'payload']) {
      assertSame('mailbox.reconcile.outbox.' + field, bunPage.items?.[0]?.[field], rustPage.items?.[0]?.[field]);
    }
    assertSame('mailbox.admission.first', mailboxStructured(bun, 5, 'bun'), mailboxStructured(rust, 5, 'rust'));
    assertSame('mailbox.admission.canonical_replay', mailboxStructured(bun, 6, 'bun'), mailboxStructured(rust, 6, 'rust'));
    assertSame('mailbox.admission.show', mailboxStructured(bun, 7, 'bun'), mailboxStructured(rust, 7, 'rust'));
    const bunDecisionPage = mailboxStructured(bun, 9, 'bun');
    const rustDecisionPage = mailboxStructured(rust, 9, 'rust');
    assertSame('mailbox.admission.outbox.count', bunDecisionPage.count, rustDecisionPage.count);
    for (const field of ['schema', 'event_id', 'scope_id', 'topic', 'aggregate_id', 'aggregate_revision', 'schema_version', 'causation_id', 'idempotency_key', 'partition_key', 'payload']) {
      assertSame('mailbox.admission.outbox.' + field, bunDecisionPage.items?.[0]?.[field], rustDecisionPage.items?.[0]?.[field]);
    }
    if (JSON.stringify(rustDecisionPage).includes('secret body')) throw new Error('mailbox_admission_body_leaked');
    const bunFactFind = mailboxStructured(bun, 10, 'bun');
    const rustFactFind = mailboxStructured(rust, 10, 'rust');
    for (const field of ['schema', 'status', 'scope_id', 'message_id']) assertSame('mailbox.fact_find.' + field, bunFactFind[field], rustFactFind[field]);
    for (const field of ['observation_id', 'mailbox_id', 'message_id', 'first_generation_id', 'first_fact_id', 'event_id']) {
      assertSame('mailbox.fact_find.observation.' + field, bunFactFind.observation?.[field], rustFactFind.observation?.[field]);
    }
    assertSame('mailbox.fact_show.safe', mailboxStructured(bun, 11, 'bun'), mailboxStructured(rust, 11, 'rust'));
    assertSame('mailbox.fact_show.full', mailboxStructured(bun, 12, 'bun'), mailboxStructured(rust, 12, 'rust'));
    assertSame('mailbox.generation_show', mailboxStructured(bun, 13, 'bun'), mailboxStructured(rust, 13, 'rust'));
    return { status: 'passed', fixture: 'immutable_fact_reconciliation_and_admission', compared: ['first_observation', 'reconciliation_replay', 'first_observation_event', 'admission', 'canonical_admission_replay', 'admission_show', 'admission_event', 'fact_find', 'fact_show_safe', 'fact_show_full', 'generation_show'] };
  } finally {
    rmSync(bunRoot, { recursive: true, force: true });
    rmSync(rustRoot, { recursive: true, force: true });
  }
}

function runMailboxSyncParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const naradaRoot = ensureControlPlaneBuild(workspaceRoot);
  const bunEntrypoint = join(workspaceRoot, 'packages', 'mailbox-mcp', 'src', 'main.ts');
  const root = mkdtempSync(join(tmpdir(), 'narada-mailbox-sync-native-'));
  const siteRoot = join(root, 'site');
  const fixtureScript = join(root, 'mailbox-sync-fixture.mjs');
  const fixture = String.raw`import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';

const [nativeExecutable, bunEntrypoint, bunCommand, siteRoot, naradaRoot, workspaceRoot] = process.argv.slice(2);
const received = [];
let origin = '';
const server = createServer((request, response) => {
  received.push({ method: request.method, url: request.url, authorization: request.headers.authorization ?? null, prefer: request.headers.prefer ?? null });
  const url = request.url ?? '';
  let payload;
  if (url.includes('/messages/message-1/attachments')) {
    payload = { value: [{ '@odata.type': '#microsoft.graph.fileAttachment', id: 'attachment-1', name: 'note.txt', contentType: 'text/plain', size: 5, contentBytes: 'SGVsbG8=', isInline: false }] };
  } else if (url.includes('/messages/delta')) {
    payload = { value: [{
      id: 'message-1', changeKey: 'version-1', conversationId: 'conversation-1', internetMessageId: '<message-1@example.test>',
      receivedDateTime: '2026-08-01T10:00:00.000Z', sentDateTime: '2026-08-01T09:59:00.000Z', subject: 'Native sync fixture',
      body: { contentType: 'text', content: 'Needle body\r\nsecond line' }, bodyPreview: 'Needle body',
      from: { emailAddress: { name: 'Fixture Sender', address: 'Sender@Example.Test' } },
      toRecipients: [{ emailAddress: { name: 'Support', address: 'support@example.test' } }], ccRecipients: [], bccRecipients: [], replyTo: [],
      parentFolderId: 'folder-id', categories: ['beta', 'alpha'], isRead: false, isDraft: false, hasAttachments: true,
      importance: 'normal', flag: { flagStatus: 'flagged' }, webLink: 'https://example.test/message-1',
    }], '@odata.deltaLink': origin + '/v1.0/delta/1' };
  } else if (url.includes('/delta/')) {
    payload = { value: [], '@odata.deltaLink': origin + '/v1.0/delta/2' };
  } else {
    response.statusCode = 404;
    payload = { error: { code: 'fixture_not_found', message: url } };
  }
  response.setHeader('content-type', 'application/json');
  response.end(JSON.stringify(payload));
});

function writeConfig() {
  const projectionRoot = join(siteRoot, '.narada', 'runtime', 'mailboxes', 'support');
  mkdirSync(projectionRoot, { recursive: true });
  mkdirSync(join(siteRoot, 'config'), { recursive: true });
  writeFileSync(join(siteRoot, 'package.json'), JSON.stringify({ name: 'mailbox-sync-parity', private: true }));
  writeFileSync(join(siteRoot, 'config', 'config.json'), JSON.stringify({
    root_dir: '.narada/runtime/mailboxes/support',
    scopes: [{
      scope_id: 'support', root_dir: '.narada/runtime/mailboxes/support', sources: [{ type: 'graph' }],
      graph: { user_id: 'fixture@example.test', base_url: origin + '/v1.0', prefer_immutable_ids: true },
      scope: { included_container_refs: ['inbox'], included_item_kinds: ['message'] },
      normalize: { attachment_policy: 'metadata_only', body_policy: 'text_only', include_headers: false, tombstones_enabled: true },
      runtime: { polling_interval_ms: 60000, acquire_lock_timeout_ms: 1000, cleanup_tmp_on_startup: true, rebuild_views_after_sync: false, rebuild_search_after_sync: false },
      policy: { primary_charter: 'fixture', allowed_actions: ['no_action'] },
    }],
  }));
}

function requests() {
  const generationId = 'mbg_' + createHash('sha256').update('sync-parity-1').digest('hex').slice(0, 40);
  return [
    { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'mailbox_sync_generation', arguments: { idempotency_key: 'sync-parity-1', scope_id: 'support' } } },
    { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'mailbox_sync_generation', arguments: { idempotency_key: 'sync-parity-1', scope_id: 'support' } } },
    { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'mailbox_generation_show', arguments: { generation_id: generationId } } },
    { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'mailbox_message_fact_find', arguments: { scope_id: 'support', message_id: 'message-1' } } },
  ];
}

function canonical(value) {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']';
  return '{' + Object.keys(value).sort().filter((key) => value[key] !== undefined).map((key) => JSON.stringify(key) + ':' + canonical(value[key])).join(',') + '}';
}

function configFingerprintMaterial() {
  return canonical({
    schema: 'narada.mailbox.sync_config.v1', scope_id: 'support', root_dir: join(siteRoot, '.narada', 'runtime', 'mailboxes', 'support'),
    source: { type: 'graph', mailbox_id: undefined, user_id: 'fixture@example.test', base_url: origin + '/v1.0', prefer_immutable_ids: true },
    scope: { included_container_refs: ['inbox'], included_item_kinds: ['message'] },
    normalize: { attachment_policy: 'metadata_only', body_policy: 'text_only', include_headers: false, tombstones_enabled: true },
  });
}

function run(command, args, env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd: workspaceRoot, env, windowsHide: true });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    const timer = setTimeout(() => { child.kill(); reject(new Error('mailbox_sync_fixture_timeout:' + command)); }, 30000);
    child.on('error', (error) => { clearTimeout(timer); reject(error); });
    child.on('close', (code) => {
      clearTimeout(timer);
      if (code !== 0) { reject(new Error('mailbox_sync_fixture_exit:' + command + ':' + code + ':' + stderr.slice(-1500))); return; }
      try { resolve(stdout.trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))); }
      catch (error) { reject(new Error('mailbox_sync_fixture_output_invalid:' + error.message + ':' + stdout.slice(-1000))); }
    });
    child.stdin.end(requests().map((request) => JSON.stringify(request)).join('\n') + '\n');
  });
}

function snapshot() {
  const projectionRoot = join(siteRoot, '.narada', 'runtime', 'mailboxes', 'support');
  const domain = new DatabaseSync(join(siteRoot, '.narada', 'runtime', 'mailbox-domain', 'mailbox-domain.db'));
  const facts = new DatabaseSync(join(projectionRoot, '.narada', 'facts.db'));
  try {
    const generation = domain.prepare('select generation_id,scope_id,config_fingerprint,status,parent_cursor,next_cursor,batch_record_count from mailbox_sync_generations').get();
    const generationRecord = domain.prepare('select record_id,event_kind,message_id,mailbox_id,conversation_id,source_version,application_status from mailbox_sync_generation_records').get();
    const fact = facts.prepare('select fact_type,source_id,source_record_id,source_version,source_cursor,provenance_json,payload_json from facts').get();
    const provenance = JSON.parse(String(fact.provenance_json)); delete provenance.observed_at;
    const factPayload = JSON.parse(String(fact.payload_json)); delete factPayload.ordinal; delete factPayload.event.observed_at;
    const message = JSON.parse(readFileSync(join(projectionRoot, 'messages', 'message-1', 'record.json'), 'utf8')); delete message._checksum;
    const cursor = JSON.parse(readFileSync(join(projectionRoot, 'state', 'cursor.json'), 'utf8')); delete cursor.committed_at;
    return { generation, generationRecord, fact: { ...fact, provenance_json: provenance, payload_json: factPayload }, message, cursor };
  } finally { domain.close(); facts.close(); }
}

server.listen(0, '127.0.0.1', async () => {
  try {
    origin = 'http://127.0.0.1:' + server.address().port;
    const env = { ...process.env, GRAPH_ACCESS_TOKEN: 'fixture-token', NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST: '1' };
    for (const key of ['GRAPH_TENANT_ID', 'GRAPH_CLIENT_ID', 'GRAPH_CLIENT_SECRET']) delete env[key];
    writeConfig();
    const bunFirst = await run(bunCommand, [bunEntrypoint, '--site-root', siteRoot, '--control-plane-root', naradaRoot], env);
    const rustReplay = await run(nativeExecutable, ['--surface-id', 'mailbox', '--site-root', siteRoot], env);
    let bunSnapshot;
    try { bunSnapshot = snapshot(); } catch (error) { throw new Error('mailbox_sync_bun_snapshot_failed:' + error.message + ':bun=' + JSON.stringify(bunFirst) + ':rust_replay=' + JSON.stringify(rustReplay)); }
    rmSync(siteRoot, { recursive: true, force: true });
    writeConfig();
    const rustFirst = await run(nativeExecutable, ['--surface-id', 'mailbox', '--site-root', siteRoot], env);
    const bunReplay = await run(bunCommand, [bunEntrypoint, '--site-root', siteRoot, '--control-plane-root', naradaRoot], env);
    let rustSnapshot;
    try { rustSnapshot = snapshot(); } catch (error) { throw new Error('mailbox_sync_rust_snapshot_failed:' + error.message + ':rust=' + JSON.stringify(rustFirst) + ':bun_replay=' + JSON.stringify(bunReplay)); }
    server.close(() => process.stdout.write(JSON.stringify({ bunFirst, rustReplay, rustFirst, bunReplay, bunSnapshot, rustSnapshot, received, configFingerprintMaterial: configFingerprintMaterial() }) + '\n'));
  } catch (error) {
    server.close(() => { process.stderr.write(String(error.stack ?? error) + '\n'); process.exit(1); });
  }
});
`;
  writeFileSync(fixtureScript, fixture, 'utf8');
  try {
    const cleanEnv = { ...process.env };
    for (const key of ['GRAPH_ACCESS_TOKEN', 'GRAPH_TENANT_ID', 'GRAPH_CLIENT_ID', 'GRAPH_CLIENT_SECRET', 'NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST']) delete cleanEnv[key];
    const result = spawnSync(process.execPath, [fixtureScript, executable, bunEntrypoint, process.env.NARADA_BUN_EXECUTABLE ?? 'bun', siteRoot, naradaRoot, workspaceRoot], {
      cwd: workspaceRoot, env: cleanEnv, encoding: 'utf8', timeout: 90_000, maxBuffer: 4 * 1024 * 1024, windowsHide: true,
    });
    if (result.error) throw new Error('mailbox_sync_fixture_spawn_failed:' + result.error.message);
    if (result.status !== 0) throw new Error('mailbox_sync_fixture_exit:' + result.status + ':' + String(result.stderr).slice(-2000));
    const payload = JSON.parse(String(result.stdout).trim());
    const stableResult = (responses, command) => {
      const value = JSON.parse(JSON.stringify(mailboxStructured(responses, 1, command)));
      delete value.result_ref;
      delete value.result.completed_at;
      delete value.result.idempotency_replayed;
      return value;
    };
    const bunFingerprint = stableResult(payload.bunFirst, 'bun').result?.config_fingerprint;
    const rustFingerprint = stableResult(payload.rustFirst, 'rust').result?.config_fingerprint;
    if (bunFingerprint !== rustFingerprint) throw new Error('mailbox_sync_config_fingerprint_mismatch:bun=' + bunFingerprint + ':rust=' + rustFingerprint + ':material=' + payload.configFingerprintMaterial + ':material_sha256=' + createHash('sha256').update(payload.configFingerprintMaterial).digest('hex'));
    assertSame('mailbox.sync.result', stableResult(payload.bunFirst, 'bun'), stableResult(payload.rustFirst, 'rust'));
    assertSame('mailbox.sync.snapshot', payload.bunSnapshot, payload.rustSnapshot);
    if (mailboxStructured(payload.bunFirst, 2, 'bun').result?.idempotency_replayed !== true) throw new Error('mailbox_sync_bun_local_replay_missing');
    if (mailboxStructured(payload.rustFirst, 2, 'rust').result?.idempotency_replayed !== true) throw new Error('mailbox_sync_rust_local_replay_missing');
    if (mailboxStructured(payload.rustReplay, 1, 'rust').result?.idempotency_replayed !== true) throw new Error('mailbox_sync_rust_cross_runtime_replay_missing');
    if (mailboxStructured(payload.bunReplay, 1, 'bun').result?.idempotency_replayed !== true) throw new Error('mailbox_sync_bun_cross_runtime_replay_missing');
    assertSame('mailbox.sync.rust_reads_node_generation', mailboxStructured(payload.bunFirst, 3, 'bun'), mailboxStructured(payload.rustReplay, 3, 'rust'));
    assertSame('mailbox.sync.node_reads_rust_generation', mailboxStructured(payload.rustFirst, 3, 'rust'), mailboxStructured(payload.bunReplay, 3, 'bun'));
    assertSame('mailbox.sync.graph_methods', payload.received.map((request) => request.method), ['GET', 'GET', 'GET', 'GET']);
    if (payload.received.some((request) => request.authorization !== 'Bearer fixture-token')) throw new Error('mailbox_sync_graph_authorization_mismatch');
    if (payload.received.some((request) => request.prefer !== 'IdType="ImmutableId"')) throw new Error('mailbox_sync_graph_prefer_header_mismatch');
    return { status: 'passed', fixture: 'loopback_graph_generation_authority', compared: ['sync_receipt', 'generation_state', 'normalized_fact', 'message_projection', 'cursor', 'same_runtime_replay', 'cross_runtime_replay'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runDelegatedTaskParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'delegated-task-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('delegated_task_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-delegated-task-native-parity-'));
  const bunRoot = join(root, 'bun');
  const rustRoot = join(root, 'rust');
  try {
    mkdirSync(bunRoot, { recursive: true });
    mkdirSync(rustRoot, { recursive: true });
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
    const idempotencyKey = 'native-delegated-task-lifecycle-parity-v1';
    const taskId = `task_${createHash('sha256').update(idempotencyKey).digest('hex').slice(0, 16)}`;
    const lifecycleRequests = [
      { jsonrpc: '2.0', id: 10, method: 'tools/call', params: { name: 'delegated_task_validate', arguments: { objective: 'Verify native delegated task lifecycle parity.', workflow: { template_id: 'implement' }, execution: { start: false } } } },
      { jsonrpc: '2.0', id: 11, method: 'tools/call', params: { name: 'delegated_task_run', arguments: { objective: 'Verify native delegated task lifecycle parity.', workflow: { template_id: 'implement' }, execution: { start: false }, idempotency_key: idempotencyKey } } },
      { jsonrpc: '2.0', id: 12, method: 'tools/call', params: { name: 'delegated_task_status', arguments: { task_id: taskId } } },
      { jsonrpc: '2.0', id: 13, method: 'tools/call', params: { name: 'delegated_task_cancel', arguments: { task_id: taskId, reason: 'parity fixture' } } },
      { jsonrpc: '2.0', id: 14, method: 'tools/call', params: { name: 'delegated_task_acknowledge', arguments: { task_id: taskId, acknowledged_by: 'native-parity' } } },
      { jsonrpc: '2.0', id: 15, method: 'tools/call', params: { name: 'delegated_task_events', arguments: { task_id: taskId, limit: 50 } } },
      { jsonrpc: '2.0', id: 16, method: 'tools/call', params: { name: 'delegated_tasks_list', arguments: { limit: 10, include_terminal: true, include_acknowledged: true } } },
    ];
    const runLifecycle = (command, args, runtimeRoot) => runMailbox(command, args, lifecycleRequests, workspaceRoot);
    const bunLifecycle = runLifecycle(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--task-root', bunRoot, '--site-root', bunRoot, '--allowed-root', bunRoot], bunRoot);
    const rustLifecycle = runLifecycle(executable, ['--surface-id', 'delegated-task', '--site-root', rustRoot], rustRoot);
    const expected = new Map([[10, ['narada.delegated_task.validate.v1', 'ok']], [11, ['narada.delegated_task.run.v1', 'accepted_for_execution']], [12, ['narada.delegated_task.status.v1', 'ok']], [13, ['narada.delegated_task.cancel.v1', 'cancelled']], [14, ['narada.delegated_task.acknowledge.v1', 'acknowledged']], [15, ['narada.delegated_task.events.v1', 'ok']], [16, ['narada.delegated_task.list.v1', 'ok']]]);
    for (const [id, [schema, status]] of expected) {
      for (const [runtime, responses] of [['bun', bunLifecycle], ['rust', rustLifecycle]]) {
        const result = mailboxStructured(responses, id, runtime);
        if (result?.schema !== schema || result?.status !== status) throw new Error(`delegated_task.lifecycle.${runtime}.${id}:expected=${schema}/${status}:actual=${JSON.stringify(result).slice(0, 1000)}`);
      }
    }
    for (const [runtime, responses] of [['bun', bunLifecycle], ['rust', rustLifecycle]]) {
      if (mailboxStructured(responses, 11, runtime)?.task_id !== taskId) throw new Error(`delegated_task.lifecycle.${runtime}:idempotent_task_id_mismatch`);
      if (mailboxStructured(responses, 12, runtime)?.task_status !== 'accepted_for_execution') throw new Error(`delegated_task.lifecycle.${runtime}:pre_cancel_status_mismatch`);
      if (mailboxStructured(responses, 13, runtime)?.task_status !== 'cancelled') throw new Error(`delegated_task.lifecycle.${runtime}:cancel_status_mismatch`);
      const events = mailboxStructured(responses, 15, runtime)?.events ?? [];
      for (const kind of ['task_created', 'task_cancelled', 'task_acknowledged']) if (!events.some((event) => event.event_kind === kind)) throw new Error(`delegated_task.lifecycle.${runtime}:event_missing:${kind}`);
      if (!(mailboxStructured(responses, 16, runtime)?.tasks ?? []).some((task) => task.task_id === taskId)) throw new Error(`delegated_task.lifecycle.${runtime}:list_projection_missing`);
    }
    const crossRead = (command, args, runtimeRoot) => mailboxStructured(runMailbox(command, args, [{ jsonrpc: '2.0', id: 20, method: 'tools/call', params: { name: 'delegated_task_status', arguments: { task_id: taskId } } }], workspaceRoot), 20, 'cross');
    const rustReadsBun = crossRead(executable, ['--surface-id', 'delegated-task', '--site-root', bunRoot], bunRoot);
    const bunReadsRust = crossRead(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--task-root', rustRoot, '--site-root', rustRoot, '--allowed-root', rustRoot], rustRoot);
    if (rustReadsBun?.task_status !== 'cancelled' || bunReadsRust?.task_status !== 'cancelled') throw new Error('delegated_task.lifecycle:cross_runtime_read_failed');

    const localWorkflow = {
      steps: [
        { id: 'record', kind: 'note' },
        { id: 'collect', kind: 'join', depends_on: ['record'] },
        { id: 'authorize', kind: 'gate', depends_on: ['collect'], if: 'all(step:collect:completed,no_residual_risks)' },
      ],
    };
    const semanticRequests = [
      { jsonrpc: '2.0', id: 30, method: 'tools/call', params: { name: 'delegated_task_validate', arguments: { objective: 'Reject unsupported workflow kinds.', workflow: { steps: [{ id: 'bad', kind: 'unsupported' }] } } } },
      { jsonrpc: '2.0', id: 33, method: 'tools/call', params: { name: 'delegated_task_validate', arguments: { objective: 'Reject unsupported workflow conditions.', workflow: { steps: [{ id: 'bad', kind: 'worker', if: 'unknown_condition' }] } } } },
      { jsonrpc: '2.0', id: 31, method: 'tools/call', params: { name: 'delegated_task_run', arguments: { objective: 'Complete a local delegated workflow.', workflow: localWorkflow, execution: { start: true }, idempotency_key: 'native-local-workflow-parity-v1' } } },
      { jsonrpc: '2.0', id: 32, method: 'tools/call', params: { name: 'delegated_task_wait', arguments: { task_id: `task_${createHash('sha256').update('native-local-workflow-parity-v1').digest('hex').slice(0, 16)}`, timeout_ms: 100, poll_ms: 50 } } },
    ];
    const semanticBunRoot = join(root, 'semantic-bun');
    const semanticRustRoot = join(root, 'semantic-rust');
    mkdirSync(semanticBunRoot, { recursive: true });
    mkdirSync(semanticRustRoot, { recursive: true });
    const semanticBun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--task-root', semanticBunRoot, '--site-root', semanticBunRoot, '--allowed-root', semanticBunRoot], semanticRequests, workspaceRoot);
    const semanticRust = runMailbox(executable, ['--surface-id', 'delegated-task', '--site-root', semanticRustRoot], semanticRequests, workspaceRoot);
    const validationProjection = (value) => ({ status: value?.status, codes: (value?.diagnostics ?? []).map((item) => item.code).sort() });
    assertSame('delegated_task.invalid_workflow', validationProjection(mailboxStructured(semanticBun, 30, 'bun')), validationProjection(mailboxStructured(semanticRust, 30, 'rust')));
    assertSame('delegated_task.invalid_condition', validationProjection(mailboxStructured(semanticBun, 33, 'bun')), validationProjection(mailboxStructured(semanticRust, 33, 'rust')));
    for (const [runtime, responses] of [['bun', semanticBun], ['rust', semanticRust]]) {
      if (mailboxStructured(responses, 31, runtime)?.task_status !== 'completed') throw new Error(`delegated_task.local_workflow.${runtime}:not_completed`);
      if (mailboxStructured(responses, 32, runtime)?.task_status !== 'completed') throw new Error(`delegated_task.wait.${runtime}:not_completed`);
    }

    const sharedRoot = join(root, 'shared-replay');
    mkdirSync(sharedRoot, { recursive: true });
    const sharedArguments = { objective: 'Prove cross-runtime idempotent replay.', constraints: { authority: 'read' }, workflow: { template_id: 'implement' }, execution: { start: false }, idempotency_key: 'native-cross-runtime-replay-v1' };
    const replayRequest = [{ jsonrpc: '2.0', id: 40, method: 'tools/call', params: { name: 'delegated_task_run', arguments: sharedArguments } }];
    const createdByBun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--task-root', sharedRoot, '--site-root', sharedRoot, '--allowed-root', sharedRoot], replayRequest, workspaceRoot);
    const replayedByRust = runMailbox(executable, ['--surface-id', 'delegated-task', '--site-root', sharedRoot], replayRequest, workspaceRoot);
    const replayProjection = (value) => ({ task_id: value?.task_id, created: value?.created, task_status: value?.task_status });
    if (replayProjection(mailboxStructured(createdByBun, 40, 'bun')).task_id !== replayProjection(mailboxStructured(replayedByRust, 40, 'rust')).task_id) throw new Error('delegated_task.cross_runtime_replay:task_id_mismatch');
    if (mailboxStructured(replayedByRust, 40, 'rust')?.created !== false) throw new Error('delegated_task.cross_runtime_replay:rust_did_not_replay');

    const lockRoot = join(root, 'shared-lock');
    const lockKey = 'native-shared-lock-parity-v1';
    const lockTaskId = `task_${createHash('sha256').update(lockKey).digest('hex').slice(0, 16)}`;
    mkdirSync(join(lockRoot, 'tasks', lockTaskId, 'mutation.lockdir'), { recursive: true });
    const lockRequest = [{ jsonrpc: '2.0', id: 50, method: 'tools/call', params: { name: 'delegated_task_run', arguments: { objective: 'Observe the shared lock.', execution: { start: false }, idempotency_key: lockKey } } }];
    const lockEnv = { ...process.env, NARADA_DELEGATED_TASK_LOCK_TIMEOUT_MS: '100', NARADA_DELEGATED_TASK_LOCK_STALE_MS: '1000' };
    const lockedBun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--task-root', lockRoot, '--site-root', lockRoot, '--allowed-root', lockRoot], lockRequest, workspaceRoot, lockEnv);
    const lockedRust = runMailbox(executable, ['--surface-id', 'delegated-task', '--site-root', lockRoot], lockRequest, workspaceRoot, lockEnv);
    const diagnosticCode = (responses) => responses.find((response) => response.id === 50)?.error?.data?.code;
    assertSame('delegated_task.shared_mutation_lock', diagnosticCode(lockedBun), diagnosticCode(lockedRust));
    if (diagnosticCode(lockedRust) !== 'delegated_task_lock_failed') throw new Error('delegated_task.shared_mutation_lock:expected_lock_failure');

    const staleRoot = join(root, 'stale-lock');
    const staleKey = 'native-stale-lock-parity-v1';
    const staleTaskId = `task_${createHash('sha256').update(staleKey).digest('hex').slice(0, 16)}`;
    const staleLockPath = join(staleRoot, 'tasks', staleTaskId, 'mutation.lockdir');
    mkdirSync(staleLockPath, { recursive: true });
    const old = new Date(Date.now() - 10_000);
    utimesSync(staleLockPath, old, old);
    const staleRequest = [{ jsonrpc: '2.0', id: 51, method: 'tools/call', params: { name: 'delegated_task_run', arguments: { objective: 'Recover an abandoned shared lock.', execution: { start: false }, idempotency_key: staleKey } } }];
    const recoveredByBun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--task-root', staleRoot, '--site-root', staleRoot, '--allowed-root', staleRoot], staleRequest, workspaceRoot, lockEnv);
    if (mailboxStructured(recoveredByBun, 51, 'bun')?.task_id !== staleTaskId) throw new Error('delegated_task.stale_lock.bun:recovery_failed');
    mkdirSync(staleLockPath, { recursive: true });
    utimesSync(staleLockPath, old, old);
    const recoveredByRust = runMailbox(executable, ['--surface-id', 'delegated-task', '--site-root', staleRoot], staleRequest, workspaceRoot, lockEnv);
    if (mailboxStructured(recoveredByRust, 51, 'rust')?.task_id !== staleTaskId) throw new Error('delegated_task.stale_lock.rust:recovery_failed');

    return { status: 'passed', fixture: 'template_and_durable_lifecycle', compared: ['template_catalog', 'validate', 'run_without_start', 'status', 'cancel', 'acknowledge', 'events', 'list', 'cross_runtime_read', 'invalid_workflow', 'local_dag_fixed_point', 'wait', 'cross_runtime_idempotency', 'shared_mutation_lock', 'stale_lock_recovery'] };
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
    const fixtureEnv = {
      ...process.env,
      NARADA_WORKER_RUN_ROOT: runRoot,
      NARADA_SITE_ROOT: root,
      USERPROFILE: root,
      HOME: root,
      CODEX_HOME: join(root, '.codex'),
    };
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--site-root', root, '--allowed-root', root, '--run-root', runRoot], requests, workspaceRoot, fixtureEnv);
    const rust = runMailbox(executable, ['--surface-id', 'worker-delegation', '--site-root', root], requests, workspaceRoot, fixtureEnv);
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
    const bunRoot = join(root, 'bun');
    const rustRoot = join(root, 'rust');
    const bunSops = join(bunRoot, 'sops');
    const rustSops = join(rustRoot, 'sops');
    mkdirSync(bunSops, { recursive: true });
    mkdirSync(rustSops, { recursive: true });
    const runBun = (requests) => runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--sop-root', bunRoot, '--sops-dir', bunSops], requests, workspaceRoot);
    const runRust = (requests) => runMailbox(executable, ['--surface-id', 'sop', '--sop-root', rustRoot, '--sops-dir', rustSops], requests, workspaceRoot);
    const normalize = (value) => {
      if (Array.isArray(value)) return value.map(normalize);
      if (!value || typeof value !== 'object') return value;
      const normalized = Object.fromEntries(Object.entries(value)
        .filter(([key]) => !['created_at', 'updated_at', 'recorded_at', 'event_id', 'native_hydration', 'yaml_path', 'db_path'].includes(key))
        .map(([key, child]) => [key, normalize(child)]));
      if (['narada.sop.template_list.v2', 'narada.sop.template_search.v2'].includes(normalized.schema)) delete normalized.status;
      return normalized;
    };
    const diagnosticCode = (responses, id, runtime) => {
      const response = responses.find((candidate) => candidate.id === id);
      const code = response?.error?.data?.code;
      if (typeof code !== 'string') throw new Error('sop_parity_diagnostic_missing:' + runtime + ':' + id + ':' + JSON.stringify(response).slice(0, 500));
      return code;
    };
    const assertStructured = (label, bun, rust, id) => {
      assertSame(label, normalize(mailboxStructured(bun, id, 'bun')), normalize(mailboxStructured(rust, id, 'rust')));
    };
    const requests = [
      { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'sop_template_create', arguments: { sop_id: 'fixture', title: 'Fixture SOP', description: 'Parity fixture', trigger_kind: 'manual', input_schema: { type: 'object', properties: { ticket_id: { type: 'string' } }, required: ['ticket_id'] }, output: { inspected: { $ref: 'steps.inspect.result.inspected' } }, output_schema: { type: 'object', properties: { inspected: { type: 'boolean' } }, required: ['inspected'] }, acceptance_criteria: ['The fixture was inspected.'], evidence_requirements: ['inspection record'], steps: [{ id: 'inspect', executor: 'agent', title: 'Inspect', instructions: 'Inspect fixture', result_schema: { type: 'object', properties: { inspected: { type: 'boolean' } }, required: ['inspected'] } }] } } },
      { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'sop_template_update', arguments: { sop_id: 'fixture', title: 'Fixture SOP v2', status: 'active', steps: [{ id: 'inspect', executor: 'agent', title: 'Inspect', instructions: 'Inspect fixture', result_schema: { type: 'object', properties: { inspected: { type: 'boolean' } }, required: ['inspected'] } }, { id: 'record', executor: 'engine', title: 'Record', instructions: 'Record result', depends_on: ['inspect'], input: { inspected: { $ref: 'steps.inspect.result.inspected' } } }] } } },
      { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'sop_template_deprecate', arguments: { sop_id: 'fixture', reason: 'parity retirement' } } },
      { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'sop_template_show', arguments: { sop_id: 'fixture', version: 2 } } },
      { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'sop_template_unimport', arguments: { sop_id: 'fixture', version: 1, reason: 'parity cleanup', principal: 'native-parity' } } },
      { jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'sop_template_list', arguments: {} } },
      { jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name: 'sop_template_create', arguments: { sop_id: 'bad-dag', title: 'Bad DAG', steps: [{ id: 'broken', executor: 'engine', title: 'Broken', instructions: 'Broken', depends_on: ['missing'] }] } } },
      { jsonrpc: '2.0', id: 8, method: 'tools/call', params: { name: 'sop_template_create', arguments: { sop_id: 'bad-schema', title: 'Bad schema', input_schema: { type: 'not-a-json-schema-type' }, steps: [{ id: 'step', executor: 'engine', title: 'Step', instructions: 'Step' }] } } },
    ];
    const bun = runBun(requests);
    const rust = runRust(requests);
    for (const [id, label] of [[1, 'create'], [2, 'update'], [3, 'deprecate'], [4, 'show'], [5, 'unimport'], [6, 'list']]) {
      assertStructured('sop.template_' + label, bun, rust, id);
    }
    assertSame('sop.template_invalid_dag', diagnosticCode(bun, 7, 'bun'), diagnosticCode(rust, 7, 'rust'));
    assertSame('sop.template_invalid_schema', diagnosticCode(bun, 8, 'bun'), diagnosticCode(rust, 8, 'rust'));

    const insertRunReference = (runtimeRoot) => {
      const db = new DatabaseSync(join(runtimeRoot, '.sop', 'sop.db'));
      try {
        db.prepare(`INSERT INTO sop_runs (
          run_id,sop_id,sop_version,sop_title,occurrence_key,request_fingerprint,definition_fingerprint,
          definition_json,input_json,output_json,step_states_json,trigger_source_kind,trigger_source_ref,
          triggered_by,created_at,updated_at
        ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`).run(
          'run-template-refusal', 'fixture', 2, 'Fixture SOP v2', 'occurrence-template-refusal',
          'request-fingerprint', 'definition-fingerprint', '{}', '{}', '{}', '[]', 'manual', '',
          'native-parity', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z',
        );
      } finally {
        db.close();
      }
    };
    insertRunReference(bunRoot);
    insertRunReference(rustRoot);
    const refusalRequest = [{ jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name: 'sop_template_unimport', arguments: { sop_id: 'fixture', version: 2, reason: 'must refuse', principal: 'native-parity' } } }];
    const bunRefusal = runBun(refusalRequest);
    const rustRefusal = runRust(refusalRequest);
    assertSame('sop.template_unimport_refusal', diagnosticCode(bunRefusal, 9, 'bun'), diagnosticCode(rustRefusal, 9, 'rust'));

    const yaml = `sop_id: yaml-fixture\ntitle: YAML Fixture\ndescription: Imported parity fixture\nstatus: active\ntrigger_kind: manual\ninput_schema:\n  type: object\n  properties:\n    ticket_id:\n      type: string\nsteps:\n  - id: inspect\n    executor: agent\n    title: Inspect\n    instructions: Inspect the YAML fixture\nacceptance_criteria:\n  - The YAML fixture was inspected.\nevidence_requirements:\n  - inspection record\n`;
    const mismatchYaml = `sop_id: another-fixture\ntitle: Mismatch\nsteps:\n  - id: inspect\n    executor: agent\n    title: Inspect\n    instructions: Inspect\n`;
    writeFileSync(join(bunSops, 'yaml-fixture.sop.yaml'), yaml, 'utf8');
    writeFileSync(join(rustSops, 'yaml-fixture.sop.yaml'), yaml, 'utf8');
    writeFileSync(join(bunSops, 'mismatch.sop.yaml'), mismatchYaml, 'utf8');
    writeFileSync(join(rustSops, 'mismatch.sop.yaml'), mismatchYaml, 'utf8');
    const yamlRequests = [
      { jsonrpc: '2.0', id: 10, method: 'tools/call', params: { name: 'sop_template_import_yaml', arguments: { sop_id: 'yaml-fixture' } } },
      { jsonrpc: '2.0', id: 11, method: 'tools/call', params: { name: 'sop_template_import_yaml', arguments: { sop_id: 'yaml-fixture' } } },
      { jsonrpc: '2.0', id: 12, method: 'tools/call', params: { name: 'sop_template_import_yaml', arguments: { sop_id: 'mismatch' } } },
    ];
    const bunYaml = runBun(yamlRequests);
    const rustYaml = runRust(yamlRequests);
    assertStructured('sop.template_import_yaml', bunYaml, rustYaml, 10);
    assertStructured('sop.template_import_yaml_replay', bunYaml, rustYaml, 11);
    assertSame('sop.template_import_yaml_id_mismatch', diagnosticCode(bunYaml, 12, 'bun'), diagnosticCode(rustYaml, 12, 'rust'));

    const snapshot = (runtimeRoot) => {
      const db = new DatabaseSync(join(runtimeRoot, '.sop', 'sop.db'), { readOnly: true });
      try {
        const templates = db.prepare(`SELECT sop_id,version,title,status,description,steps_json,trigger_kind,
          input_schema_json,output_mapping_json,output_ref_mapping_json,output_schema_json,
          acceptance_criteria_json,evidence_requirements_json FROM sop_templates ORDER BY sop_id,version`).all()
          .map((row) => Object.fromEntries(Object.entries(row).map(([key, value]) => [key.endsWith('_json') && value !== null ? key.slice(0, -5) : key, key.endsWith('_json') && value !== null ? JSON.parse(String(value)) : value])));
        const events = db.prepare('SELECT event_kind,details_json FROM sop_events ORDER BY rowid').all()
          .map((row) => ({ event_kind: row.event_kind, details: JSON.parse(String(row.details_json)) }));
        return normalize({ templates, events });
      } finally {
        db.close();
      }
    };
    assertSame('sop.template_registry_snapshot', snapshot(bunRoot), snapshot(rustRoot));
    return {
      status: 'passed',
      fixture: 'independent_template_registry_authorities',
      compared: ['create', 'update', 'deprecate', 'show', 'unimport', 'list', 'invalid_dag', 'invalid_schema', 'referenced_unimport_refusal', 'yaml_import', 'yaml_replay', 'yaml_id_mismatch', 'sqlite_snapshot'],
    };
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

function runSopDurabilityMutationParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const nodeEntrypoint = join(workspaceRoot, 'packages', 'sop-mcp', 'dist', 'src', 'main.js');
  if (!existsSync(nodeEntrypoint)) throw new Error('sop_durability_parity_node_entrypoint_missing:' + nodeEntrypoint);
  const root = mkdtempSync(join(tmpdir(), 'narada-sop-durability-native-parity-'));
  const nodeRoot = join(root, 'node');
  const rustRoot = join(root, 'rust');
  const runNode = (runtimeRoot, requests) => runMailbox(process.env.NARADA_NODE_EXECUTABLE ?? 'node', [nodeEntrypoint, '--sop-root', runtimeRoot], requests, workspaceRoot);
  const runRust = (runtimeRoot, requests) => runMailbox(executable, ['--surface-id', 'sop', '--sop-root', runtimeRoot], requests, workspaceRoot);
  const canonical = (value) => {
    if (Array.isArray(value)) return value.map(canonical);
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).sort(([left], [right]) => left.localeCompare(right)).map(([key, entry]) => [key, canonical(entry)]));
    return value;
  };
  const fingerprint = (value) => createHash('sha256').update(JSON.stringify(canonical(value)), 'utf8').digest('hex');
  const deterministicId = (prefix, value) => `${prefix}${createHash('sha256').update(value, 'utf8').digest('hex').slice(0, 24)}`;
  const normalize = (value) => {
    if (Array.isArray(value)) return value.map(normalize);
    if (!value || typeof value !== 'object') return value;
    return Object.fromEntries(Object.entries(value)
      .filter(([key]) => !['lease_token', 'lease_expires_at', 'created_at', 'updated_at', 'registered_at', 'processed_at', 'compacted_at'].includes(key))
      .map(([key, child]) => [key, normalize(child)]));
  };
  const structured = (responses, id, runtime) => mailboxStructured(responses, id, runtime);
  const code = (responses, id, runtime) => {
    const response = responses.find((candidate) => candidate.id === id);
    const diagnosticCode = response?.error?.data?.code;
    if (typeof diagnosticCode !== 'string') throw new Error('sop_durability_diagnostic_missing:' + runtime + ':' + id + ':' + JSON.stringify(response).slice(0, 500));
    return diagnosticCode;
  };
  const assertStructured = (label, node, rust, id) => assertSame(label, normalize(structured(node, id, 'node')), normalize(structured(rust, id, 'rust')));
  const bootstrap = (runtimeRoot, runner, sopId) => {
    const response = runner(runtimeRoot, [{
      jsonrpc: '2.0', id: 1, method: 'tools/call', params: {
        name: 'sop_template_create',
        arguments: { sop_id: sopId, title: 'Durability fixture', steps: [{ id: 'step', executor: 'engine', title: 'Step', instructions: 'Step' }] },
      },
    }]);
    structured(response, 1, sopId);
  };
  const insertRun = (db, runId, status, createdAt) => {
    db.prepare(`INSERT INTO sop_runs (
      run_id,sop_id,sop_version,sop_title,status,occurrence_key,request_fingerprint,
      definition_fingerprint,definition_json,input_json,output_json,step_states_json,
      trigger_source_kind,trigger_source_ref,triggered_by,created_at,updated_at,completed_at
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`).run(
      runId, 'durability-template', 1, 'Durability fixture', status, 'occ-' + runId, '', '', '{}', '{}', '{}', '[]',
      'manual', '', 'native-parity', createdAt, createdAt,
      ['completed', 'failed', 'cancelled'].includes(status) ? createdAt : null,
    );
  };
  const seedHandoff = (runtimeRoot) => {
    const db = new DatabaseSync(join(runtimeRoot, '.sop', 'sop.db'));
    try {
      insertRun(db, 'run-handoff', 'running', '2026-01-01T00:00:00.000Z');
      const runId = 'run-handoff';
      const stepId = 'approve';
      const identity = `${runId}\0${stepId}`;
      const input = { ticket_id: 'T-1' };
      const resultSchema = { type: 'object', properties: { approved: { type: 'boolean' } } };
      db.prepare(`INSERT INTO sop_handoffs (
        handoff_id,run_id,step_id,occurrence_key,sop_id,sop_version,executor,title,instructions,
        input_json,input_ref_json,result_schema_json,request_fingerprint,status,created_at,updated_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,'pending',?,?)`).run(
        deterministicId('soh_', identity), runId, stepId, deterministicId('sop_handoff_', identity),
        'durability-template', 1, 'operator', 'Approve', 'Approve ticket', JSON.stringify(input), null,
        JSON.stringify(resultSchema), fingerprint({
          run_id: runId, step_id: stepId, sop_id: 'durability-template', sop_version: 1,
          executor: 'operator', title: 'Approve', instructions: 'Approve ticket', input,
          input_ref: null, result_schema: resultSchema,
        }), '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z',
      );
    } finally {
      db.close();
    }
  };
  const seedOutbox = (runtimeRoot) => {
    const db = new DatabaseSync(join(runtimeRoot, '.sop', 'sop.db'));
    try {
      const events = [
        ['event-before-start', '2025-12-31T00:00:00.000Z', { outcome: 'before' }],
        ['event-1', '2026-01-02T00:00:00.000Z', { outcome: 'one' }],
        ['event-2', '2026-01-03T00:00:00.000Z', { outcome: 'two' }],
      ];
      const insertEvent = db.prepare(`INSERT INTO sop_outbox (
        event_id,topic,partition_key,run_id,sop_id,sop_version,occurrence_key,outcome,
        payload_json,created_at,available_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?)`);
      for (const [eventId, createdAt, payload] of events) {
        const runId = 'run-' + eventId;
        insertRun(db, runId, 'completed', createdAt);
        insertEvent.run(eventId, 'sop.run.terminal.v1', 'durability-template', runId, 'durability-template', 1,
          'occ-' + eventId, 'completed', JSON.stringify(payload), createdAt, createdAt);
      }
    } finally {
      db.close();
    }
  };
  try {
    mkdirSync(nodeRoot, { recursive: true });
    mkdirSync(rustRoot, { recursive: true });
    bootstrap(nodeRoot, runNode, 'durability-template');
    bootstrap(rustRoot, runRust, 'durability-template');
    seedHandoff(nodeRoot);
    seedHandoff(rustRoot);
    seedOutbox(nodeRoot);
    seedOutbox(rustRoot);

    const handoffId = deterministicId('soh_', 'run-handoff\0approve');
    const claimRequest = [{ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'sop_handoff_claim', arguments: { consumer_id: 'consumer-1', handoff_id: handoffId, executor: 'operator', lease_ms: 60_000 } } }];
    const nodeClaim = runNode(nodeRoot, claimRequest);
    const rustClaim = runRust(rustRoot, claimRequest);
    assertStructured('sop.handoff_claim', nodeClaim, rustClaim, 1);
    const nodeToken = structured(nodeClaim, 1, 'node').handoff?.lease_token;
    const rustToken = structured(rustClaim, 1, 'rust').handoff?.lease_token;
    if (typeof nodeToken !== 'string' || typeof rustToken !== 'string') throw new Error('sop_handoff_parity_lease_token_missing');

    const nodeRenew = runNode(nodeRoot, [{ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'sop_handoff_renew', arguments: { handoff_id: handoffId, consumer_id: 'consumer-1', lease_token: nodeToken, lease_ms: 120_000 } } }]);
    const rustRenew = runRust(rustRoot, [{ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'sop_handoff_renew', arguments: { handoff_id: handoffId, consumer_id: 'consumer-1', lease_token: rustToken, lease_ms: 120_000 } } }]);
    assertStructured('sop.handoff_renew', nodeRenew, rustRenew, 2);
    const badRenew = (runtimeRoot, runner) => runner(runtimeRoot, [{ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'sop_handoff_renew', arguments: { handoff_id: handoffId, consumer_id: 'consumer-1', lease_token: 'wrong-token' } } }]);
    assertSame('sop.handoff_lease_mismatch', code(badRenew(nodeRoot, runNode), 3, 'node'), code(badRenew(rustRoot, runRust), 3, 'rust'));

    const nodeRelease = runNode(nodeRoot, [{ jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'sop_handoff_release', arguments: { handoff_id: handoffId, consumer_id: 'consumer-1', lease_token: nodeToken, error_message: 'worker unavailable' } } }]);
    const rustRelease = runRust(rustRoot, [{ jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'sop_handoff_release', arguments: { handoff_id: handoffId, consumer_id: 'consumer-1', lease_token: rustToken, error_message: 'worker unavailable' } } }]);
    assertStructured('sop.handoff_release', nodeRelease, rustRelease, 4);
    const emptyRequest = [{ jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'sop_handoff_claim', arguments: { consumer_id: 'consumer-2', handoff_id: 'soh_missing', executor: 'operator' } } }];
    assertStructured('sop.handoff_claim_empty', runNode(nodeRoot, emptyRequest), runRust(rustRoot, emptyRequest), 5);

    const outboxRequests = [
      { jsonrpc: '2.0', id: 10, method: 'tools/call', params: { name: 'sop_outbox_consumer_register', arguments: { consumer_id: 'consumer-main', start_at: '2026-01-01T00:00:00Z' } } },
      { jsonrpc: '2.0', id: 11, method: 'tools/call', params: { name: 'sop_outbox_consumer_register', arguments: { consumer_id: 'consumer-main', start_at: '2026-01-01T00:00:00Z' } } },
      { jsonrpc: '2.0', id: 12, method: 'tools/call', params: { name: 'sop_outbox_consumer_register', arguments: { consumer_id: 'consumer-main', start_at: '2026-01-02T00:00:00Z' } } },
      { jsonrpc: '2.0', id: 13, method: 'tools/call', params: { name: 'sop_outbox_list', arguments: { consumer_id: 'consumer-main' } } },
      { jsonrpc: '2.0', id: 14, method: 'tools/call', params: { name: 'sop_outbox_ack', arguments: { event_id: 'event-1', consumer_id: 'consumer-main', receipt: { disposition: 'processed', attempt: 1 } } } },
      { jsonrpc: '2.0', id: 15, method: 'tools/call', params: { name: 'sop_outbox_ack', arguments: { event_id: 'event-1', consumer_id: 'consumer-main', receipt: { disposition: 'processed', attempt: 1 } } } },
      { jsonrpc: '2.0', id: 16, method: 'tools/call', params: { name: 'sop_outbox_ack', arguments: { event_id: 'event-1', consumer_id: 'consumer-main', receipt: { disposition: 'different' } } } },
      { jsonrpc: '2.0', id: 17, method: 'tools/call', params: { name: 'sop_outbox_compact', arguments: { before: '2026-02-01T00:00:00Z' } } },
      { jsonrpc: '2.0', id: 18, method: 'tools/call', params: { name: 'sop_outbox_list', arguments: { consumer_id: 'consumer-main' } } },
      { jsonrpc: '2.0', id: 19, method: 'tools/call', params: { name: 'sop_outbox_consumer_register', arguments: { consumer_id: 'consumer-late', start_at: '2026-01-01T00:00:00Z' } } },
      { jsonrpc: '2.0', id: 20, method: 'tools/call', params: { name: 'sop_outbox_ack', arguments: { event_id: 'event-before-start', consumer_id: 'consumer-main', receipt: {} } } },
      { jsonrpc: '2.0', id: 21, method: 'tools/call', params: { name: 'sop_outbox_list', arguments: { consumer_id: 'consumer-missing' } } },
    ];
    const nodeOutbox = runNode(nodeRoot, outboxRequests);
    const rustOutbox = runRust(rustRoot, outboxRequests);
    for (const id of [10, 11, 13, 14, 15, 17, 18]) assertStructured('sop.outbox_mutation.' + id, nodeOutbox, rustOutbox, id);
    for (const id of [12, 16, 19, 20, 21]) assertSame('sop.outbox_diagnostic.' + id, code(nodeOutbox, id, 'node'), code(rustOutbox, id, 'rust'));

    const snapshot = (runtimeRoot) => {
      const db = new DatabaseSync(join(runtimeRoot, '.sop', 'sop.db'), { readOnly: true });
      try {
        return normalize({
          handoffs: db.prepare('SELECT * FROM sop_handoffs ORDER BY handoff_id').all().map((row) => Object.fromEntries(Object.entries(row).map(([key, value]) => [key, key.endsWith('_json') && value !== null ? JSON.parse(String(value)) : value]))),
          consumers: db.prepare('SELECT * FROM sop_outbox_consumer_requirements ORDER BY topic,consumer_id').all(),
          receipts: db.prepare('SELECT * FROM sop_outbox_receipts ORDER BY event_id,consumer_id').all().map((row) => ({ ...row, receipt_json: JSON.parse(String(row.receipt_json)) })),
          outbox: db.prepare('SELECT * FROM sop_outbox ORDER BY event_id').all().map((row) => ({ ...row, payload_json: JSON.parse(String(row.payload_json)) })),
        });
      } finally {
        db.close();
      }
    };
    assertSame('sop.durability_snapshot', snapshot(nodeRoot), snapshot(rustRoot));
    return {
      status: 'passed', fixture: 'independent_handoff_and_outbox_authorities',
      compared: ['claim', 'renew', 'lease_mismatch', 'release', 'empty_claim', 'consumer_register', 'registration_replay', 'registration_conflict', 'list', 'ack', 'ack_replay', 'ack_conflict', 'compaction', 'history_compacted_refusal', 'before_start_refusal', 'unregistered_refusal', 'sqlite_snapshot'],
    };
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
    const outputValue = { fixture: 'graph-mail', items: [1, 2] };
    const outputText = JSON.stringify(outputValue, null, 2);
    const outputRef = 'mcp_output:graph-mail-fixture';
    const outputRecord = {
      schema: 'narada.mcp_output_ref.v1',
      ref: outputRef,
      output_id: 'graph-mail-fixture',
      tool_name: 'graph_mail_query',
      full_output_char_length: outputText.length,
      truncated: true,
      sha256: createHash('sha256').update(JSON.stringify(stable(outputValue))).digest('hex'),
      full_output: outputValue,
    };
    mkdirSync(join(root, '.ai', 'tmp', 'mcp-outputs', 'workspace'), { recursive: true });
    writeFileSync(join(root, '.ai', 'tmp', 'mcp-outputs', 'workspace', 'graph-mail-fixture.json'), JSON.stringify(outputRecord), 'utf8');
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
      params: { name: 'graph_mail_guidance', arguments: { workflow: 'fixture', tool: 'graph_mail_query' } },
    }, {
      jsonrpc: '2.0',
      id: 2,
      method: 'tools/call',
      params: { name: 'graph_mail_output_show', arguments: { ref: outputRef, offset: 0, limit: 1000 } },
    }, {
      jsonrpc: '2.0',
      id: 3,
      method: 'tools/call',
      params: { name: 'graph_mail_doctor', arguments: {} },
    }, {
      jsonrpc: '2.0',
      id: 4,
      method: 'tools/call',
      params: { name: 'graph_mail_auth_status', arguments: {} },
    }];
    const env = { ...process.env };
    for (const key of ['MS_GRAPH_ACCESS_TOKEN', 'GRAPH_ACCESS_TOKEN', 'GRAPH_TENANT_ID', 'GRAPH_CLIENT_ID', 'GRAPH_CLIENT_SECRET', 'GRAPH_TOKEN_ENDPOINT']) delete env[key];
    const bun = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', [bunEntrypoint, '--site-root', root], requests, workspaceRoot, env);
    const rust = runMailbox(executable, ['--surface-id', 'graph-mail', '--site-root', root], requests, workspaceRoot, env);
    assertSame('graph_mail.guidance', mailboxStructured(bun, 1, 'bun'), mailboxStructured(rust, 1, 'rust'));
    assertSame('graph_mail.output_show', mailboxStructured(bun, 2, 'bun'), mailboxStructured(rust, 2, 'rust'));
    assertSame('graph_mail.doctor', mailboxStructured(bun, 3, 'bun'), mailboxStructured(rust, 3, 'rust'));
    assertSame('graph_mail.auth_status', mailboxStructured(bun, 4, 'bun'), mailboxStructured(rust, 4, 'rust'));
    return { status: 'passed', fixture: 'local_policy_posture', compared: ['guidance', 'output_show', 'doctor', 'auth_status'] };
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
    { jsonrpc: '2.0', id: 18, method: 'tools/call', params: { name: 'graph_mail_reply_draft_create', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', comment_html: '<p>Thanks</p>' } } },
    { jsonrpc: '2.0', id: 19, method: 'tools/call', params: { name: 'graph_mail_reply_all_to_last_in_thread_draft_create', arguments: { mailbox_id: 'fixture@example.test', conversation_id: 'conversation-1', comment: 'Latest' } } },
    { jsonrpc: '2.0', id: 20, method: 'tools/call', params: { name: 'graph_mail_attachment_upload_session_create', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', name: 'large.bin', size: 655360, content_type: 'application/octet-stream' } } },
    { jsonrpc: '2.0', id: 21, method: 'tools/call', params: { name: 'graph_mail_attachment_download_file', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', attachment_id: 'attachment-1', file_path: 'downloads/note.txt' } } },
    { jsonrpc: '2.0', id: 22, method: 'tools/call', params: { name: 'graph_mail_auth_status', arguments: {} } },
    { jsonrpc: '2.0', id: 23, method: 'tools/call', params: { name: 'graph_mail_auth_clear', arguments: { confirm_clear: true } } },
    { jsonrpc: '2.0', id: 24, method: 'tools/call', params: { name: 'graph_mail_ticket_draft_upsert', arguments: { ticket_id: 'ticket-1', effect_claim_id: 'claim-1', draft_operation_key: 'ticket-op-1', draft_request_digest: createHash('sha256').update(JSON.stringify(stable({ source_id: 'source-1', mailbox_id: 'fixture@example.test', source_message_id: 'message-1', reply_mode: 'reply', body_text: 'Ticket body' }))).digest('hex'), draft_source_id: 'source-1', mailbox_id: 'fixture@example.test', source_message_id: 'message-1', reply_mode: 'reply', body_text: 'Ticket body', idempotency_key: 'ticket-idempotency-1' } } },
    { jsonrpc: '2.0', id: 25, method: 'tools/call', params: { name: 'graph_mail_ticket_draft_disposition_list', arguments: { consumer_id: 'consumer-1' } } },
    { jsonrpc: '2.0', id: 26, method: 'tools/call', params: { name: 'graph_mail_ticket_draft_discard', arguments: { ticket_id: 'ticket-1', effect_claim_id: 'claim-1', draft_operation_key: 'ticket-op-1', mailbox_id: 'fixture@example.test', draft_id: 'draft-html', idempotency_key: 'discard-idempotency-1', confirm_discard: true } } },
    { jsonrpc: '2.0', id: 27, method: 'tools/call', params: { name: 'graph_mail_ticket_draft_disposition_list', arguments: { consumer_id: 'consumer-1' } } },
    { jsonrpc: '2.0', id: 28, method: 'tools/call', params: { name: 'graph_mail_ticket_draft_disposition_ack', arguments: { observation_id: 'graph_draft_disposition_' + createHash('sha256').update('ticket-op-1\u0000discarded\u0000draft-html').digest('hex').slice(0, 32), consumer_id: 'consumer-1', reconciliation_ref: 'reconcile-1', reconciliation_receipt: { status: 'ok' } } } },
    { jsonrpc: '2.0', id: 29, method: 'tools/call', params: { name: 'graph_mail_ticket_draft_disposition_list', arguments: { consumer_id: 'consumer-1' } } },
  ];
  const cleanEnv = { ...process.env, NARADA_GRAPH_MAIL_FIXTURE_REQUESTS: JSON.stringify(requests) };
  for (const key of ['MS_GRAPH_ACCESS_TOKEN', 'GRAPH_ACCESS_TOKEN', 'GRAPH_TENANT_ID', 'GRAPH_CLIENT_ID', 'GRAPH_CLIENT_SECRET', 'GRAPH_TOKEN_ENDPOINT', 'NARADA_NATIVE_GRAPH_MAIL_AUTHORITY', 'NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST']) delete cleanEnv[key];
  const fixture = String.raw`import { createServer } from 'node:http';
import { mkdirSync, writeFileSync } from 'node:fs';
import { spawn } from 'node:child_process';

const [nativeExecutable, bunEntrypoint, bunCommand, bunRoot, rustRoot] = process.argv.slice(2);
const requests = JSON.parse(process.env.NARADA_GRAPH_MAIL_FIXTURE_REQUESTS ?? '[]');
const received = [];
let fixturePort = 0;
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
    else if (request.method === 'GET' && url.includes('/messages/draft-html')) payload = { id: 'draft-html', isDraft: true, subject: 'Re: Needle', body: { contentType: 'HTML', content: '<p>Quoted history</p>' } };
    else if (request.method === 'GET' && url.includes('singleValueExtendedProperties') && (url.includes('isDraft%20eq%20true') || url.includes('isDraft+eq+true'))) payload = { value: [] };
    else if (request.method === 'GET' && url.includes('singleValueExtendedProperties')) payload = { value: [{ id: 'draft-html', isDraft: true, changeKey: 'ck-ticket' }] };
    else if (request.method === 'GET' && url.includes('/messages/message-1')) payload = { id: 'message-1', subject: 'Needle' };
    else if (request.method === 'GET' && url.includes('/messages')) payload = { value: [{ id: 'message-1', subject: 'Needle' }] };
    else if (request.method === 'GET' && url.includes('/mailFolders')) payload = { value: [{ id: 'folder-1', displayName: 'Inbox' }] };
    else if (request.method === 'POST' && url.endsWith('/mailFolders')) { status = 201; payload = { id: 'folder-2', displayName: 'Customers' }; }
    else if (request.method === 'POST' && url.includes('/move')) { status = 200; payload = { id: 'message-1', parentFolderId: 'folder-2' }; }
    else if (request.method === 'POST' && url.includes('/createUploadSession')) { status = 201; payload = { uploadUrl: 'http://127.0.0.1:' + fixturePort + '/upload/fixture' }; }
    else if (request.method === 'POST' && url.endsWith('/devicecode')) { status = 200; payload = { device_code: 'device-code-fixture', user_code: 'USER-FIXTURE', verification_uri: 'http://127.0.0.1:' + fixturePort + '/verify', expires_in: 900, interval: 5, message: 'Use the fixture verification page.' }; }
    else if (request.method === 'POST' && url.endsWith('/token')) { status = 200; payload = { access_token: 'delegated-fixture-token', expires_in: 3600, token_type: 'Bearer' }; }
    else if (request.method === 'POST' && url.includes('/attachments')) { status = 201; payload = { id: 'attachment-2', name: 'note.txt', contentType: 'text/plain' }; }
    else if (request.method === 'POST' && url.includes('/createReplyAll')) { status = 201; payload = { id: 'draft-thread', isDraft: true, subject: 'Re: Needle' }; }
    else if (request.method === 'POST' && url.includes('/createReply') && request.body?.comment === 'Thanks') { status = 201; payload = { id: 'draft-reply', isDraft: true, subject: 'Re: Needle' }; }
    else if (request.method === 'POST' && url.includes('/createReply')) { status = 201; payload = { id: 'draft-html', isDraft: true, subject: 'Re: Needle' }; }
    else if (request.method === 'POST' && url.includes('/createForward')) { status = 201; payload = { id: 'draft-forward', isDraft: true, subject: 'Fwd: Needle' }; }
    else if (request.method === 'POST' && url.endsWith('/messages')) { status = 201; payload = { id: 'draft-1', isDraft: true, subject: 'Draft' }; }
    else if (request.method === 'POST' && url.endsWith('/send')) { status = 202; payload = null; }
    else if (request.method === 'DELETE' && url.includes('/attachments/')) { status = 204; payload = null; }
    else if (request.method === 'DELETE' && url.includes('/messages/draft-1')) { status = 204; payload = null; }
    else if (request.method === 'DELETE' && url.includes('/messages/draft-html')) { status = 204; payload = null; }
    else if (request.method === 'PATCH' && url.includes('/messages/draft-html')) { status = 200; payload = { id: 'draft-html', isDraft: true, subject: 'Re: Needle', body: request.body?.body ?? null }; }
    else if (request.method === 'PATCH' && url.includes('/messages/draft-1')) { status = 200; payload = { id: 'draft-1', isDraft: true, subject: 'Updated' }; }
    else if (request.method === 'PATCH') { status = 204; payload = null; }
    else if (request.method === 'PUT' && url.includes('/upload/fixture')) { status = 201; payload = { id: 'attachment-uploaded', name: 'upload.txt', contentType: 'text/plain' }; }
    response.statusCode = status;
    if (payload === null) { response.end(); return; }
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify(payload));
  });
});

function run(command, args, env, requestList = requests) {
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
    child.stdin.end(requestList.map((request) => JSON.stringify(request)).join('\n') + '\n');
  });
}

server.listen(0, '127.0.0.1', async () => {
  try {
    const port = server.address().port;
    fixturePort = port;
    const config = JSON.stringify({ graph_base_url: 'http://127.0.0.1:' + port + '/v1.0', allowed_mailboxes: ['fixture@example.test'], allow_folder_create: true, allow_message_move: true, allow_message_mark_read: true, mailbox_organization_approval_token: 'org-fixture', allow_send_draft: true, send_approval_token: 'send-fixture', allow_device_code_auth: true, device_code_tenant_id: 'tenant-fixture', device_code_client_id: 'client-fixture', device_code_allowed_scopes: ['Mail.ReadWrite'] });
    for (const root of [bunRoot, rustRoot]) { mkdirSync(root + '/.ai', { recursive: true }); writeFileSync(root + '/.ai/graph-mail-mcp.json', config); }
    const env = { ...process.env, GRAPH_ACCESS_TOKEN: 'fixture-token', NARADA_NATIVE_GRAPH_ALLOW_INSECURE_TEST: '1', NARADA_GRAPH_MAIL_ALLOW_INSECURE_TEST: '1', NARADA_GRAPH_MAIL_DEVICE_CODE_ENDPOINT: 'http://127.0.0.1:' + port + '/oauth2/v2.0' };
    for (const key of ['MS_GRAPH_ACCESS_TOKEN', 'GRAPH_TENANT_ID', 'GRAPH_CLIENT_ID', 'GRAPH_CLIENT_SECRET', 'GRAPH_TOKEN_ENDPOINT', 'NARADA_GRAPH_MAIL_AUTHORITY_ENTRYPOINT', 'NARADA_GRAPH_MAIL_AUTHORITY_ARGS']) delete env[key];
    const bun = await run(bunCommand, [bunEntrypoint, '--site-root', bunRoot], env);
    const rust = await run(nativeExecutable, ['--surface-id', 'graph-mail', '--native-authority', '--site-root', rustRoot], env);
    const uploadUrl = 'http://127.0.0.1:' + port + '/upload/fixture';
    for (const uploadRoot of [bunRoot, rustRoot]) { mkdirSync(uploadRoot + '/uploads', { recursive: true }); writeFileSync(uploadRoot + '/uploads/input.txt', 'Hello', 'utf8'); }
    const uploadRequests = [
      { jsonrpc: '2.0', id: 30, method: 'tools/call', params: { name: 'graph_mail_attachment_upload_chunk', arguments: { mailbox_id: 'fixture@example.test', upload_url: uploadUrl, content_base64: 'SGVsbG8=', range_start: 0, range_end: 4, total_size: 5 } } },
      { jsonrpc: '2.0', id: 31, method: 'tools/call', params: { name: 'graph_mail_attachment_upload_file', arguments: { mailbox_id: 'fixture@example.test', message_id: 'message-1', file_path: 'uploads/input.txt', name: 'upload.txt', content_type: 'text/plain', chunk_size: 327680 } } },
    ];
    const bunUpload = await run(bunCommand, [bunEntrypoint, '--site-root', bunRoot], env, uploadRequests);
    const rustUpload = await run(nativeExecutable, ['--surface-id', 'graph-mail', '--native-authority', '--site-root', rustRoot], env, uploadRequests);
    const authStartRequests = [{ jsonrpc: '2.0', id: 32, method: 'tools/call', params: { name: 'graph_mail_auth_device_code_start', arguments: { scope: 'Mail.ReadWrite' } } }];
    const bunAuthStart = await run(bunCommand, [bunEntrypoint, '--site-root', bunRoot], env, authStartRequests);
    const rustAuthStart = await run(nativeExecutable, ['--surface-id', 'graph-mail', '--native-authority', '--site-root', rustRoot], env, authStartRequests);
    const structured = (responses, id) => responses.find((response) => response.id === id)?.result?.structuredContent;
    const bunStartStructured = structured(bunAuthStart, 32);
    const rustStartStructured = structured(rustAuthStart, 32);
    if (!bunStartStructured || !rustStartStructured) throw new Error('auth_start_result_missing:' + JSON.stringify({ bun: bunAuthStart, rust: rustAuthStart }));
    const bunFlowId = bunStartStructured.flow_id;
    const rustFlowId = rustStartStructured.flow_id;
    const authPollRequests = (flowId) => [{ jsonrpc: '2.0', id: 33, method: 'tools/call', params: { name: 'graph_mail_auth_device_code_poll', arguments: { flow_id: flowId } } }];
    const bunAuthPoll = await run(bunCommand, [bunEntrypoint, '--site-root', bunRoot], env, authPollRequests(bunFlowId));
    const rustAuthPoll = await run(nativeExecutable, ['--surface-id', 'graph-mail', '--native-authority', '--site-root', rustRoot], env, authPollRequests(rustFlowId));
    server.close(() => process.stdout.write(JSON.stringify({ bun, rust, bunUpload, rustUpload, bunAuthStart, rustAuthStart, bunAuthPoll, rustAuthPoll, received }) + '\n'));
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
    for (const id of [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 22, 23, 25, 28]) assertSame('graph_mail.native_graph.' + id, mailboxStructured(payload.bun, id, 'bun'), mailboxStructured(payload.rust, id, 'rust'));
    const normalizeTicket = (value) => { const copy = JSON.parse(JSON.stringify(value)); if (copy?.result) delete copy.result.completed_at; return copy; };
    assertSame('graph_mail.native_graph.24', normalizeTicket(mailboxStructured(payload.bun, 24, 'bun')), normalizeTicket(mailboxStructured(payload.rust, 24, 'rust')));
    const normalizeDiscard = (value) => { const copy = JSON.parse(JSON.stringify(value)); if (copy?.disposition_receipt) { delete copy.disposition_receipt.observed_at; delete copy.disposition_receipt.receipt_sha256; } return copy; };
    assertSame('graph_mail.native_graph.26', normalizeDiscard(mailboxStructured(payload.bun, 26, 'bun')), normalizeDiscard(mailboxStructured(payload.rust, 26, 'rust')));
    const bunDisposition = mailboxStructured(payload.bun, 27, 'bun');
    const rustDisposition = mailboxStructured(payload.rust, 27, 'rust');
    if (bunDisposition.count !== 1 || rustDisposition.count !== 1 || bunDisposition.items?.[0]?.disposition !== 'discarded' || rustDisposition.items?.[0]?.disposition !== 'discarded') throw new Error('graph_mail.native_graph.disposition_list_mismatch');
    if (mailboxStructured(payload.bun, 29, 'bun').count !== 0 || mailboxStructured(payload.rust, 29, 'rust').count !== 0) throw new Error('graph_mail.native_graph.disposition_ack_mismatch');
    assertSame('graph_mail.native_graph.upload_chunk', mailboxStructured(payload.bunUpload, 30, 'bun'), mailboxStructured(payload.rustUpload, 30, 'rust'));
    assertSame('graph_mail.native_graph.upload_file', mailboxStructured(payload.bunUpload, 31, 'bun'), mailboxStructured(payload.rustUpload, 31, 'rust'));
    const normalizeAuthStart = (value) => { const copy = { ...value }; delete copy.flow_id; return copy; };
    assertSame('graph_mail.native_graph.auth_device_code_start', normalizeAuthStart(mailboxStructured(payload.bunAuthStart, 32, 'bun')), normalizeAuthStart(mailboxStructured(payload.rustAuthStart, 32, 'rust')));
    const normalizeAuthPoll = (value) => { const copy = { ...value }; delete copy.flow_id; delete copy.expires_at_ms; return copy; };
    assertSame('graph_mail.native_graph.auth_device_code_poll', normalizeAuthPoll(mailboxStructured(payload.bunAuthPoll, 33, 'bun')), normalizeAuthPoll(mailboxStructured(payload.rustAuthPoll, 33, 'rust')));
    const normalizeDownload = (value, root) => ({ ...value, file_path: String(value.file_path).replace(root, '<fixture-root>') });
    assertSame('graph_mail.native_graph.21', normalizeDownload(mailboxStructured(payload.bun, 21, 'bun'), join(root, 'bun')), normalizeDownload(mailboxStructured(payload.rust, 21, 'rust'), join(root, 'rust')));
    const expectedMethods = ['GET', 'GET', 'GET', 'POST', 'POST', 'PATCH', 'GET', 'GET', 'GET', 'POST', 'DELETE', 'POST', 'POST', 'POST', 'PATCH', 'GET', 'DELETE', 'POST', 'POST', 'GET', 'PATCH', 'GET', 'POST', 'POST', 'GET', 'GET', 'POST', 'GET', 'DELETE'];
    assertSame('graph_mail.native_graph.bun_methods', payload.received.slice(0, expectedMethods.length).map((value) => value.method), expectedMethods);
    assertSame('graph_mail.native_graph.rust_methods', payload.received.slice(expectedMethods.length, expectedMethods.length * 2).map((value) => value.method), expectedMethods);
    if (payload.received.slice(0, expectedMethods.length * 2).some((value) => value.authorization !== 'Bearer fixture-token')) throw new Error('graph_mail_native_graph_fixture_authorization_mismatch');
    const auditPath = join(root, 'rust', '.ai', 'audit', 'graph-mail-mcp.jsonl');
    const auditKinds = readFileSync(auditPath, 'utf8').trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line).event_kind);
    assertSame('graph_mail.native_graph.audit', auditKinds, ['folder_create_requested', 'folder_create_completed', 'message_move_requested', 'message_move_completed', 'message_mark_read_requested', 'message_mark_read_completed', 'draft_create_requested', 'draft_create_completed', 'createReply_requested', 'createReply_completed', 'createForward_requested', 'createForward_completed', 'draft_update_requested', 'draft_update_completed', 'draft_discard_requested', 'draft_discard_completed', 'draft_send_requested', 'draft_send_completed', 'createReply_html_requested', 'createReply_html_completed', 'createReplyAll_to_last_in_thread_requested', 'createReplyAll_to_last_in_thread_completed', 'attachment_download_file_completed', 'device_code_auth_cleared', 'ticket_draft_create_requested', 'ticket_draft_create_completed', 'ticket_draft_discard_requested', 'ticket_draft_discard_completed', 'attachment_upload_file_completed', 'device_code_start_completed', 'device_code_poll_completed']);
    return { status: 'passed', fixture: 'loopback_graph_mail_authority', compared: ['query', 'message_show', 'folder_list', 'folder_create', 'message_move', 'message_mark_read', 'attachment_list', 'attachment_get_metadata', 'attachment_get_content', 'attachment_add', 'attachment_delete', 'attachment_upload_session_create', 'attachment_download_file', 'attachment_upload_chunk', 'attachment_upload_file', 'draft_create', 'reply_draft_create', 'forward_draft_create', 'draft_update', 'draft_discard', 'draft_send', 'html_reply_draft_create', 'reply_all_to_last_in_thread_draft_create', 'auth_status', 'auth_clear', 'auth_device_code_start', 'auth_device_code_poll', 'ticket_draft_upsert', 'ticket_draft_disposition_list', 'ticket_draft_discard', 'ticket_draft_disposition_ack', 'audit'] };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function runOperatorOverlayParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'operator-console-overlay-mcp', 'src', 'main.ts');
  if (!existsSync(bunEntrypoint)) throw new Error('operator_overlay_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const naradaRoot = resolveNaradaRoot(workspaceRoot);
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

function runSchedulerParity() {
  const workspaceRoot = resolve(packageRoot, '..', '..', '..');
  const bunEntrypoint = join(workspaceRoot, 'packages', 'scheduler-mcp', 'dist', 'src', 'main.js');
  if (!existsSync(bunEntrypoint)) throw new Error('scheduler_parity_bun_entrypoint_missing:' + bunEntrypoint);
  const bunRoot = mkdtempSync(join(tmpdir(), 'narada-scheduler-bun-parity-'));
  const rustRoot = mkdtempSync(join(tmpdir(), 'narada-scheduler-rust-parity-'));
  try {
    const bunArgs = [bunEntrypoint, '--allowed-root', bunRoot];
    const rustArgs = ['--surface-id', 'scheduler', '--site-root', rustRoot];
    const listRequest = [{ jsonrpc: '2.0', id: 1, method: 'tools/list', params: {} }];
    const bunTools = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', bunArgs, listRequest, workspaceRoot)[0]?.result?.tools ?? [];
    const rustTools = runMailbox(executable, rustArgs, listRequest, workspaceRoot)[0]?.result?.tools ?? [];
    assertSame('scheduler.tool_names', bunTools.map((tool) => tool.name).sort(), rustTools.map((tool) => tool.name).sort());
    for (const toolName of ['scheduler_task_create', 'scheduler_task_update_action', 'scheduler_binding_upsert', 'scheduler_event_admit', 'scheduler_activation_claim']) {
      const bunTool = bunTools.find((tool) => tool.name === toolName);
      const rustTool = rustTools.find((tool) => tool.name === toolName);
      assertSame('scheduler.required.' + toolName, [...(bunTool?.inputSchema?.required ?? [])].sort(), [...(rustTool?.inputSchema?.required ?? [])].sort());
    }

    const statusRequest = [{ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'scheduler_runtime_status', arguments: {} } }];
    const bunStatus = mailboxStructured(runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', bunArgs, statusRequest, workspaceRoot), 1, 'bun');
    const rustStatus = mailboxStructured(runMailbox(executable, rustArgs, statusRequest, workspaceRoot), 1, 'rust');
    if (bunStatus.status !== 'fresh') throw new Error('scheduler_parity_bun_runtime_not_fresh:' + bunStatus.status);
    if (rustStatus.status !== 'fresh' || rustStatus.implementation !== 'rust-native') throw new Error('scheduler_parity_rust_runtime_not_fresh:' + JSON.stringify(rustStatus).slice(0, 500));

    const taskListRequest = [{ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'scheduler_task_list', arguments: { limit: 5 } } }];
    const bunTaskList = mailboxStructured(runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', bunArgs, taskListRequest, workspaceRoot), 1, 'bun');
    const rustTaskList = mailboxStructured(runMailbox(executable, rustArgs, taskListRequest, workspaceRoot), 1, 'rust');
    assertSame('scheduler.task_list.names', bunTaskList.items?.map((task) => task.task_name), rustTaskList.items?.map((task) => task.task_name));

    const dryRunArguments = {
      task_name: '\\Narada\\NativeParityDryRun',
      command: 'pwsh.exe',
      arguments: '-NoProfile',
      execution_time_limit_seconds: 180,
      multiple_instances: 'ignore_new',
      dry_run: true,
    };
    const dryRun = (implementationId) => [{ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'scheduler_task_update_action', arguments: { ...dryRunArguments, implementation_id: implementationId } } }];
    const bunPlan = mailboxStructured(runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', bunArgs, dryRun(bunStatus.implementation_id), workspaceRoot), 1, 'bun');
    const rustPlan = mailboxStructured(runMailbox(executable, rustArgs, dryRun(rustStatus.implementation_id), workspaceRoot), 1, 'rust');
    for (const field of ['status', 'execute', 'arguments', 'mutation_method', 'console_window_policy', 'preserves_triggers', 'preserves_enabled_state', 'working_dir_would_apply', 'execution_time_limit_seconds', 'multiple_instances']) {
      assertSame('scheduler.dry_run.' + field, bunPlan[field], rustPlan[field]);
    }

    const activationRequests = (implementationId) => [{
      jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'scheduler_activation_prepare', arguments: { implementation_id: implementationId } },
    }, {
      jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'scheduler_binding_upsert', arguments: {
        binding_id: 'native-parity-binding', trigger_kind: 'completion', source_topic: 'sop.run.terminal.v1', source_sop_id: 'fixture-sop', terminal_outcomes: ['ok'],
        target_sop_id: 'fixture-sop', target_template_version: 'v1', concurrency: 'singleton', default_delay_ms: 0, implementation_id: implementationId,
      } },
    }, {
      jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'scheduler_event_admit', arguments: {
        event_id: 'native-parity-event', topic: 'sop.run.terminal.v1', partition_key: 'fixture', aggregate_id: 'fixture-run', aggregate_revision: 1, schema_version: 1,
        causation_id: 'native-parity-cause', idempotency_key: 'native-parity-key', payload: { sop_id: 'fixture-sop', outcome: 'ok' }, occurred_at: '2026-01-01T00:00:00.000Z', implementation_id: implementationId,
      } },
    }, {
      jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'scheduler_activation_claim', arguments: { consumer_id: 'native-parity-dispatcher', lease_ms: 30000, implementation_id: implementationId } },
    }];
    const bunActivation = runMailbox(process.env.NARADA_BUN_EXECUTABLE ?? 'bun', bunArgs, activationRequests(bunStatus.implementation_id), workspaceRoot);
    const rustActivation = runMailbox(executable, rustArgs, activationRequests(rustStatus.implementation_id), workspaceRoot);
    assertSame('scheduler.activation.prepare', mailboxStructured(bunActivation, 1, 'bun').status, mailboxStructured(rustActivation, 1, 'rust').status);
    const bunBinding = mailboxStructured(bunActivation, 2, 'bun').binding;
    const rustBinding = mailboxStructured(rustActivation, 2, 'rust').binding;
    for (const field of ['binding_id', 'trigger_kind', 'source_topic', 'target_sop_id', 'target_template_version', 'concurrency', 'status', 'revision']) assertSame('scheduler.binding.' + field, bunBinding?.[field], rustBinding?.[field]);
    const bunAdmission = mailboxStructured(bunActivation, 3, 'bun');
    const rustAdmission = mailboxStructured(rustActivation, 3, 'rust');
    assertSame('scheduler.event.status', bunAdmission.status, rustAdmission.status);
    assertSame('scheduler.event.activation_count', bunAdmission.activation_count, rustAdmission.activation_count);
    const bunClaim = mailboxStructured(bunActivation, 4, 'bun').activation;
    const rustClaim = mailboxStructured(rustActivation, 4, 'rust').activation;
    for (const field of ['activation_id', 'binding_id', 'source_event_id', 'occurrence_key', 'target_sop_id', 'target_template_version', 'partition_key', 'status', 'attempt_count']) assertSame('scheduler.claim.' + field, bunClaim?.[field], rustClaim?.[field]);
    return { status: 'passed', compared: ['tool_contract', 'runtime_status', 'task_list', 'task_update_action_dry_run', 'activation_prepare', 'binding_upsert', 'event_admit', 'activation_claim'] };
  } finally {
    rmSync(bunRoot, { recursive: true, force: true });
    rmSync(rustRoot, { recursive: true, force: true });
  }
}

const runSlice = (slice, callback) => paritySlice === 'all' || paritySlice === slice ? callback() : { status: 'skipped' };
const mailboxParity = runSlice('mailbox', runMailboxParity);
const mailboxOutboxMutationParity = runSlice('mailbox', runMailboxOutboxMutationParity);
const mailboxReconciliationParity = runSlice('mailbox', runMailboxReconciliationParity);
const mailboxSyncParity = runSlice('mailbox', runMailboxSyncParity);
const delegatedTaskParity = runSlice('delegated-task', runDelegatedTaskParity);
const workerDelegationParity = runSlice('worker-delegation', runWorkerDelegationParity);
const artifactsParity = runSlice('artifacts', runArtifactsParity);
const sopParity = runSlice('sop', runSopParity);
const sopActionParity = runSlice('sop', runSopActionParity);
const sopRunListParity = runSlice('sop', runSopRunListParity);
const sopRunEventsParity = runSlice('sop', runSopRunEventsParity);
const sopRunStatusParity = runSlice('sop', runSopRunStatusParity);
const sopHandoffParity = runSlice('sop', runSopHandoffParity);
const sopRunCoverageParity = runSlice('sop', runSopRunCoverageParity);
const sopOutboxParity = runSlice('sop', runSopOutboxParity);
const sopDurabilityMutationParity = runSlice('sop', runSopDurabilityMutationParity);
const sopEngineParity = runSlice('sop', () => runSopEngineParity({ executable, workspaceRoot: resolve(packageRoot, '..', '..', '..') }));
const surfaceFeedbackParity = runSlice('surface-feedback', runSurfaceFeedbackParity);
const siteLoopParity = runSlice('site-loop', runSiteLoopParity);
const calendarParity = runSlice('calendar', runCalendarParity);
const calendarAuthorityBridge = runSlice('calendar', runCalendarAuthorityBridge);
const calendarNativeGraphParity = runSlice('calendar', runCalendarNativeGraphParity);
const graphMailNativeGraphParity = runSlice('graph-mail', runGraphMailNativeGraphParity);
const cloudflareParity = runSlice('cloudflare-carrier', runCloudflareParity);
const graphMailParity = runSlice('graph-mail', runGraphMailParity);
const operatorOverlayParity = runSlice('operator-console-overlay', runOperatorOverlayParity);
const browserControlParity = runSlice('browser-control', runBrowserControlParity);
const quotaMeterParity = runSlice('quota-meter', runQuotaMeterParity);
const narsSessionParity = runSlice('nars-session', runNarsSessionParity);
const schedulerParity = runSlice('scheduler', runSchedulerParity);
process.stdout.write(JSON.stringify({
  schema: 'narada.mcp_surfaces_native.protocol_parity.v1',
  status: 'passed',
  surfaces: surfaces.length,
  legacy: '2024-11-05',
  modern: '2026-07-28',
  defaults_changed: false,
  mailbox_parity: mailboxParity,
  mailbox_outbox_mutation_parity: mailboxOutboxMutationParity,
  mailbox_reconciliation_parity: mailboxReconciliationParity,
  mailbox_sync_parity: mailboxSyncParity,
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
  sop_durability_mutation_parity: sopDurabilityMutationParity,
  sop_engine_parity: sopEngineParity,
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
  scheduler_parity: schedulerParity,
}) + '\n');
