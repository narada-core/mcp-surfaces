import assert from 'node:assert/strict';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { issueCarrierSessionOrientationDeliveryReceipt } from '@narada-core/orientation-manifest';
import { materializeAgentSessionStart, recordOrientationDeliveryReceipt } from '../src/session-start.js';

const tsEntrypoint = fileURLToPath(new URL('../src/main.js', import.meta.url));
const rustEntrypoint = fileURLToPath(new URL(`../../native/target/release/narada-agent-context-mcp${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const roots: string[] = [];

try {
  const ts = fixture('ts');
  const rust = fixture('rust');
  const tsClient = client(process.execPath, [tsEntrypoint, '--site-root', ts.root, '--site-id', 'parity', '--tool-projection', 'admin'], ts);
  const rustClient = client(rustEntrypoint, ['--site-root', rust.root, '--site-id', 'parity', '--tool-projection', 'admin'], rust);
  try {
    for (const [tool, argumentsValue] of [
      ['agent_context_guidance', { workflow: 'checkpoint', tool: 'agent_context_checkpoint' }],
      ['agent_context_doctor', {}],
      ['agent_context_whoami', {}],
      ['agent_context_hydrate_current', {}],
      ['agent_context_hydrate_current', { checkpoint_startup: true }],
      ['agent_context_start_session', { identity: 'parity.builder', runtime: 'codex', dry_run: true }],
    ] as const) {
      assert.deepEqual(
        normalize(await rustClient.call(tool, argumentsValue), rust),
        normalize(await tsClient.call(tool, argumentsValue), ts),
        `${tool} refusal/read-only projection`,
      );
    }
    seedSessions(ts.db);
    seedSessions(rust.db);
    assert.deepEqual(
      normalize(await rustClient.call('agent_context_list_sessions', { identity: 'parity.builder', limit: 10 }), rust),
      normalize(await tsClient.call('agent_context_list_sessions', { identity: 'parity.builder', limit: 10 }), ts),
      'agent_context_list_sessions parity',
    );
    for (const [method, params] of [
      ['prompts/list', {}],
      ['prompts/get', { name: 'agent_context_startup' }],
      ['completion/complete', { argument: { name: 'name', value: '' }, ref: { type: 'ref/prompt', name: 'agent_context_startup' } }],
      ['logging/setLevel', { level: 'info' }],
    ] as const) {
      assert.deepEqual(normalize(await rustClient.request(method, params), rust), normalize(await tsClient.request(method, params), ts), `${method} protocol parity`);
    }
    const tsResources = await tsClient.request('resources/list', {});
    const rustResources = await rustClient.request('resources/list', {});
    assert.deepEqual(normalize(rustResources, rust), normalize(tsResources, ts), 'resources/list protocol parity');
    const tsOutputUri = tsResources.resources.find((resource: any) => resource.uri.startsWith('mcp-output:'))?.uri;
    const rustOutputUri = rustResources.resources.find((resource: any) => resource.uri.startsWith('mcp-output:'))?.uri;
    assert.ok(tsOutputUri && rustOutputUri);
    assert.deepEqual(normalize(await rustClient.request('resources/read', { uri: rustOutputUri }), rust), normalize(await tsClient.request('resources/read', { uri: tsOutputUri }), ts), 'resources/read protocol parity');
    const admission = parityAdmission();
    const materializationArgs = {
      identity: 'parity.builder', runtime: 'codex',
      generated_at: '2026-08-11T00:00:00.000Z', admission_receipt: admission,
    };
    const tsMaterialization = fullResult(ts, await tsClient.call('agent_context_start_session', materializationArgs));
    const rustMaterialization = fullResult(rust, await rustClient.call('agent_context_start_session', materializationArgs));
    assert.deepEqual(
      normalize(rustMaterialization, rust), normalize(tsMaterialization, ts),
      'agent_context_start_session materialization parity',
    );
    assert.deepEqual(
      materializationCounts(rust.db), materializationCounts(ts.db),
      'session materialization persistence parity',
    );
    const activation = parityActivation(admission);
    const activatedArgs = { ...materializationArgs, activation_receipt: activation };
    assert.deepEqual(
      normalize(fullResult(rust, await rustClient.call('agent_context_start_session', activatedArgs)), rust),
      normalize(fullResult(ts, await tsClient.call('agent_context_start_session', activatedArgs)), ts),
      'activated session materialization parity',
    );
    const countsBeforeHydration = materializationCounts(rust.db);
    const hydrationArgs = { admission_receipt: admission, generated_at: '2026-08-11T00:01:00.000Z' };
    const tsHydration = fullResult(ts, await tsClient.call('agent_context_hydrate_current', hydrationArgs));
    const rustHydration = fullResult(rust, await rustClient.call('agent_context_hydrate_current', hydrationArgs));
    assert.deepEqual(normalize(rustHydration, rust), normalize(tsHydration, ts), 'receipt-based hydration parity');
    assert.deepEqual(materializationCounts(rust.db), countsBeforeHydration, 'diagnostic hydration remains read-only');
    const checkpointArgs = {
      agent_id: 'parity.builder', session_id: 'session-1', active_task: { task: 42 },
      files_touched: ['alpha.ts'], key_decisions: ['native parity'], open_questions: ['orientation'], git_head: 'abc123',
      next_intended_action: 'continue', continuation: {
        schema: 'narada.continuation.v1', continuation_id: 'cont_parity', objective: 'Port Agent Context',
        current_state: 'Checkpoint parity phase', completed_work: ['catalog'], decisions: ['Rust'],
        evidence_refs: ['commit:c66b0ab'], open_blockers: [], next_action: 'Port orientation',
        canonical_sources: ['src/main.ts'], constraints: ['preserve behavior'], resume_mode: 'fresh_session',
        created_at: '2026-08-11T00:00:00.000Z',
      },
    };
    const firstTs = await tsClient.call('agent_context_checkpoint', checkpointArgs);
    const firstRust = await rustClient.call('agent_context_checkpoint', checkpointArgs);
    assert.deepEqual(normalize(firstRust, rust), normalize(firstTs, ts), 'first checkpoint result');

    const currentTs = await tsClient.call('agent_context_rehydrate', { agent_id: 'parity.builder' });
    const currentRust = await rustClient.call('agent_context_rehydrate', { agent_id: 'parity.builder' });
    assert.deepEqual(normalize(currentRust, rust), normalize(currentTs, ts), 'current checkpoint projection');
    const exactTsHydration = fullResult(ts, await tsClient.call('agent_context_hydrate_current', { admission_receipt: admission, checkpoint_id: firstTs.checkpoint_id, generated_at: '2026-08-11T00:02:00.000Z' }));
    const exactRustHydration = fullResult(rust, await rustClient.call('agent_context_hydrate_current', { admission_receipt: admission, checkpoint_id: firstRust.checkpoint_id, generated_at: '2026-08-11T00:02:00.000Z' }));
    assert.deepEqual(normalize(exactRustHydration, rust), normalize(exactTsHydration, ts), 'exact-checkpoint hydration parity');

    const exportArgs = { agent_id: 'parity.builder', path: '.ai/continuations/parity.md' };
    const exportTs = await tsClient.call('agent_context_continuation_export', exportArgs);
    const exportRust = await rustClient.call('agent_context_continuation_export', exportArgs);
    assert.deepEqual(normalize(exportRust, rust), normalize(exportTs, ts), 'continuation export projection');
    const readTs = await tsClient.call('agent_context_continuation_read', { agent_id: 'parity.builder' });
    const readRust = await rustClient.call('agent_context_continuation_read', { agent_id: 'parity.builder' });
    assert.deepEqual(normalize(readRust, rust), normalize(readTs, ts), 'continuation artifact readback');

    const secondArgs = { ...checkpointArgs, active_task: { task: 43 }, continuation: null };
    await tsClient.call('agent_context_checkpoint', secondArgs);
    await rustClient.call('agent_context_checkpoint', secondArgs);
    const historyTs = await tsClient.call('agent_context_rehydrate', { agent_id: 'parity.builder', history: true, limit: 10 });
    const historyRust = await rustClient.call('agent_context_rehydrate', { agent_id: 'parity.builder', history: true, limit: 10 });
    assert.deepEqual(normalize(historyRust, rust), normalize(historyTs, ts), 'checkpoint history projection');
    assert.deepEqual(dbCounts(rust.db), dbCounts(ts.db), 'checkpoint persistence counts');

    const tsOrientation = prepareOrientation(ts);
    const rustOrientation = prepareOrientation(rust);
    const tsOccupant = client(process.execPath, [tsEntrypoint, '--site-root', ts.root, '--site-id', 'parity', '--tool-projection', 'occupant'], ts, tsOrientation);
    const rustOccupant = client(rustEntrypoint, ['--site-root', rust.root, '--site-id', 'parity', '--tool-projection', 'occupant'], rust, rustOrientation);
    try {
      const rustEntry = await rustOccupant.call('agent_orientation_read', {});
      const tsEntry = await tsOccupant.call('agent_orientation_read', {});
      assert.deepEqual(
        normalize(rustEntry, rust), normalize(tsEntry, ts),
        'initial occupant orientation projection and continuation binding',
      );
      let rustMaterial: any = rustEntry;
      let tsMaterial: any = tsEntry;
      let pages = 0;
      while (rustMaterial.next_call && pages < 20) {
        rustMaterial = await rustOccupant.call('agent_orientation_read', rustMaterial.next_call.arguments);
        tsMaterial = await tsOccupant.call('agent_orientation_read', tsMaterial.next_call.arguments);
        assert.deepEqual(normalize(rustMaterial, rust), normalize(tsMaterial, ts), `required-read material page ${pages + 1}`);
        pages += 1;
        if (rustMaterial.material?.page?.eof === true) break;
      }
      assert.ok(pages > 1, 'fixture must exercise multi-page orientation reads');
      const rustReady = await rustOccupant.call('agent_orientation_read', rustMaterial.next_call.arguments);
      const tsReady = await tsOccupant.call('agent_orientation_read', tsMaterial.next_call.arguments);
      assert.deepEqual(normalize(rustReady, rust), normalize(tsReady, ts), 'acknowledgement and ready gate projection');
    } finally { await Promise.all([tsOccupant.stop(), rustOccupant.stop()]); }
    const tsAdminEvidence = client(process.execPath, [tsEntrypoint, '--site-root', ts.root, '--site-id', 'parity', '--tool-projection', 'admin'], ts, tsOrientation);
    const rustAdminEvidence = client(rustEntrypoint, ['--site-root', rust.root, '--site-id', 'parity', '--tool-projection', 'admin'], rust, rustOrientation);
    try {
      for (const [tool, argumentsValue] of [
        ['agent_context_whoami', { hint: 'parity.builder' }],
        ['agent_context_startup_sequence', {}],
        ['agent_orientation_read', { step_id: 'read:site-law', offset: 0 }],
        ['agent_orientation_read', { selection: 'continuity' }],
        ['agent_orientation_read', { selection: 'work' }],
        ['agent_orientation_acknowledge', {}],
      ] as const) {
        assert.deepEqual(normalize(await rustAdminEvidence.call(tool, argumentsValue), rust), normalize(await tsAdminEvidence.call(tool, argumentsValue), ts), `${tool} native administrative evidence parity`);
      }
    } finally { await Promise.all([tsAdminEvidence.stop(), rustAdminEvidence.stop()]); }
  } finally {
    await Promise.all([tsClient.stop(), rustClient.stop()]);
  }
  console.log('agent-context native checkpoint state parity ok');
} finally {
  for (const root of roots) rmSync(root, { recursive: true, force: true });
}

function fixture(label: string) {
  const root = mkdtempSync(join(tmpdir(), `agent-context-${label}-parity-`));
  roots.push(root);
  mkdirSync(join(root, '.ai', 'agents'), { recursive: true });
  writeFileSync(join(root, 'AGENTS.md'), '# Parity fixture\n');
  writeFileSync(join(root, '.ai', 'agents', 'roster.json'), JSON.stringify({ enforce_session_roster: true, agents: [{ agent_id: 'parity.builder', role: 'builder', capabilities: [] }] }));
  return { root, db: join(root, '.ai', 'state', 'agent-context.sqlite') };
}

function client(executable: string, args: string[], fixtureValue: ReturnType<typeof fixture>, extraEnv: Record<string, string> = {}) {
  const child = spawn(executable, args, { env: { ...process.env, NARADA_AGENT_CONTEXT_DB: fixtureValue.db, ...extraEnv }, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true }) as ChildProcessWithoutNullStreams;
  let output = Buffer.alloc(0); let stderr = ''; let id = 0;
  child.stdout.on('data', (chunk) => { output = Buffer.concat([output, chunk]); });
  child.stderr.setEncoding('utf8'); child.stderr.on('data', (chunk) => { stderr += chunk; });
  return {
    async request(method: string, params: unknown) {
      const requestId = ++id;
      const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: requestId, method, params }));
      child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`); child.stdin.write(body);
      const response = await waitFor(requestId);
      assert.equal(response.error, undefined, response.error?.message ?? stderr);
      return response.result;
    },
    async call(name: string, argumentsValue: unknown) {
      const requestId = ++id;
      const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: requestId, method: 'tools/call', params: { name, arguments: argumentsValue } }));
      child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`); child.stdin.write(body);
      const response = await waitFor(requestId);
      assert.equal(response.error, undefined, response.error?.message ?? stderr);
      return response.result.structuredContent;
    },
    stop() { child.stdin.end(); return new Promise<void>((resolve) => { if (child.exitCode !== null) return resolve(); child.once('exit', () => resolve()); setTimeout(() => { child.kill(); resolve(); }, 1000).unref(); }); },
  };
  async function waitFor(requestId: number) {
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline) {
      const split = output.indexOf('\r\n\r\n');
      if (split >= 0) {
        const length = Number(output.subarray(0, split).toString('ascii').match(/Content-Length:\s*(\d+)/i)?.[1]);
        if (output.length >= split + 4 + length) {
          const body = output.subarray(split + 4, split + 4 + length); output = output.subarray(split + 4 + length);
          const response = JSON.parse(body.toString('utf8')); if (response.id === requestId) return response;
        }
      }
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    throw new Error(`timeout:${requestId}:${stderr}`);
  }
}

function normalize(value: unknown, fixtureValue: ReturnType<typeof fixture>): unknown {
  if (Array.isArray(value)) return value.map((entry) => normalize(entry, fixtureValue));
  if (!value || typeof value !== 'object') return typeof value === 'string'
    ? value.replaceAll(fixtureValue.root, '<site>').replaceAll(fixtureValue.db, '<db>')
      .replace(/chk_[a-f0-9]{32}/g, '<checkpoint_id>').replace(/[a-f0-9]{64}/g, '<sha256>')
      .replace(/mcp_output:o_[a-f0-9]{24}/g, '<output_ref>').replace(/o_[a-f0-9]{24}\.json/g, '<output_id>.json')
      .replace(/mcp_output%3Ao_[a-f0-9]{24}/g, '<encoded_output_ref>')
      .replace(/orientation-ack:carrier-parity:1:[a-f0-9]{16}/g, '<orientation_ack>')
      .replace(/orientation:carrier-materialization-parity:1:[a-f0-9]{16}/g, '<orientation_manifest_id>')
      .replace(/read:continuity:[a-f0-9]{16}/g, '<continuity_read_step>')
      .replace(/20\d\d-\d\d-\d\dT\d\d:\d\d:\d\d\.\d\d\dZ/g, '<timestamp>')
    : value;
  const result: Record<string, unknown> = {};
  for (const [key, entry] of Object.entries(value)) {
    if (['checkpoint_id', 'archived_prior', 'content_hash', 'agent_start_event'].includes(key)) result[key] = entry == null ? null : `<${key}>`;
    else if (key === 'event_id' && typeof entry === 'string' && /^evt-\d{4}-/.test(entry)) result[key] = '<agent_start_event>';
    else if (key === 'manifest_bytes') result[key] = '<manifest_bytes>';
    else if (['checkpoint_at', 'created_at'].includes(key)) result[key] = '<timestamp>';
    else if (key === 'source_checkpoint_ref') result[key] = '<checkpoint-ref>';
    else result[key] = normalize(entry, fixtureValue);
  }
  return result;
}

function parityAdmission() {
  return {
    schema: 'narada.carrier_session.admission_receipt.v0', receipt_id: 'receipt:materialization-parity:1', decision: 'admitted', state: 'starting',
    coordinate: { authority_scope: 'test', site_ref: 'site:parity', carrier_session_id: 'carrier-materialization-parity', authority_epoch: 1 },
    agent_identity: { source_authority_ref: 'agent-identity:parity', artifact_ref: 'agent:parity.builder@1', revision: '1', local_agent_id: 'parity.builder', canonical_agent_id: 'parity.builder' },
    carrier_kind: 'codex', admission_policy: { source_authority_ref: 'site-law:parity', artifact_ref: 'carrier-policy:parity', revision: '1' },
    issued_at: '2026-08-11T00:00:00.000Z', valid_until: null, authority_readback_ref: 'carrier-session-authority:materialization-parity', evidence_refs: [], reason_codes: [],
  };
}

function parityActivation(admission: ReturnType<typeof parityAdmission>) {
  return {
    schema: 'narada.carrier_session.activation_receipt.v0', receipt_id: 'activation:materialization-parity:1',
    decision: 'activated', state: 'active', coordinate: admission.coordinate,
    admission_receipt_ref: admission.receipt_id,
    runtime_binding: { source_authority_ref: 'runtime-host:windows', artifact_ref: 'runtime:codex:parity', revision: '1', owning_site_ref: 'site:parity', observed_at: '2026-08-11T00:00:00.000Z' },
    issued_at: '2026-08-11T00:00:00.000Z', authority_readback_ref: 'carrier-session-authority:materialization-parity', evidence_refs: ['runtime-observation:parity'], reason_codes: [],
  };
}

function materializationCounts(path: string) {
  const db = new DatabaseSync(path, { readOnly: true });
  try {
    return {
      manifests: db.prepare('SELECT COUNT(*) AS count FROM orientation_manifest_generations').get()?.count,
      briefs: db.prepare('SELECT COUNT(*) AS count FROM orientation_brief_generations').get()?.count,
      starts: db.prepare('SELECT COUNT(*) AS count FROM agent_start_events').get()?.count,
    };
  } finally { db.close(); }
}

function fullResult(fixtureValue: ReturnType<typeof fixture>, value: any) {
  if (!value?.output_ref) return value;
  const outputId = String(value.output_ref).replace(/^mcp_output:/, '');
  const record = JSON.parse(readFileSync(join(fixtureValue.root, '.ai', 'tmp', 'mcp-outputs', 'workspace', `${outputId}.json`), 'utf8'));
  return record.full_output;
}

function dbCounts(path: string) {
  const db = new DatabaseSync(path, { readOnly: true });
  try { return { current: db.prepare('SELECT COUNT(*) AS count FROM agent_checkpoints').get()?.count, history: db.prepare('SELECT COUNT(*) AS count FROM agent_checkpoint_history').get()?.count }; }
  finally { db.close(); }
}

function seedSessions(path: string) {
  const db = new DatabaseSync(path);
  try {
    const insert = db.prepare('INSERT INTO agent_start_events (event_id, identity_id, runtime, created_at, status, resume_command, bootstrap_artifact_uri) VALUES (?, ?, ?, ?, ?, ?, ?)');
    insert.run('evt-older', 'parity.builder', 'codex', '2026-01-01T00:00:00.000Z', 'materialized', 'resume older', null);
    insert.run('evt-newer', 'parity.builder', 'codex', '2026-02-01T00:00:00.000Z', 'materialized', 'resume newer', 'file:///bootstrap');
    insert.run('evt-other', 'other.builder', 'codex', '2026-03-01T00:00:00.000Z', 'materialized', null, null);
  } finally { db.close(); }
}

function prepareOrientation(fixtureValue: ReturnType<typeof fixture>) {
  const carrierSessionId = 'carrier-parity';
  const admission: any = {
    schema: 'narada.carrier_session.admission_receipt.v0', receipt_id: 'receipt:carrier-parity:1', decision: 'admitted', state: 'starting',
    coordinate: { authority_scope: 'test', site_ref: 'site:parity', carrier_session_id: carrierSessionId, authority_epoch: 1 },
    agent_identity: { source_authority_ref: 'agent-identity:parity', artifact_ref: 'agent:parity.builder@1', revision: '1', local_agent_id: 'parity.builder', canonical_agent_id: 'parity.builder' },
    carrier_kind: 'codex', admission_policy: { source_authority_ref: 'site-law:parity', artifact_ref: 'carrier-policy:parity', revision: '1' },
    issued_at: '2026-08-11T00:00:00.000Z', valid_until: null, authority_readback_ref: 'carrier-session-authority:carrier-parity', evidence_refs: [], reason_codes: [],
  };
  writeFileSync(join(fixtureValue.root, 'AGENTS.md'), ['# Parity fixture', '', ...Array.from({ length: 180 }, (_, index) => `Rule ${index + 1}: preserve exact authority and evidence boundaries.`), ''].join('\n'));
  const started: any = materializeAgentSessionStart({ siteRoot: fixtureValue.root, siteId: 'parity', identity: 'parity.builder', runtime: 'codex', dbPath: fixtureValue.db, carrierSessionId, admissionReceipt: admission, generatedAt: '2026-08-11T00:00:00.000Z' });
  const delivery: any = issueCarrierSessionOrientationDeliveryReceipt({ admissionReceipt: admission, brief: started.orientation_brief, deliveredAt: '2026-08-11T00:00:00.000Z' });
  recordOrientationDeliveryReceipt({ siteRoot: fixtureValue.root, dbPath: fixtureValue.db, admissionReceipt: admission, brief: started.orientation_brief, deliveryReceipt: delivery });
  const entryRoot = join(fixtureValue.root, '.ai', 'runtime', 'orientation-entry', carrierSessionId);
  const entryFile = join(entryRoot, 'entry.json');
  mkdirSync(entryRoot, { recursive: true });
  writeFileSync(entryFile, JSON.stringify({
    schema: 'narada.carrier_entry.orientation_packet.v1', ordinary_work_gate: 'acknowledgement_required',
    acknowledgement_projection: { schema: 'narada.carrier_entry.orientation_acknowledgement_projection_ref.v1', relative_path: 'acknowledgement.json', posture: 'derived_readback_of_canonical_acknowledgement' },
    orientation_brief: started.orientation_brief, delivery_receipt: delivery,
  }, null, 2));
  return {
    NARADA_AGENT_ID: 'parity.builder', NARADA_CARRIER_SESSION_ID: carrierSessionId,
    NARADA_CARRIER_SESSION_ADMISSION_RECEIPT: JSON.stringify(admission),
    NARADA_ORIENTATION_MANIFEST_ID: started.orientation_manifest.manifest_id,
    NARADA_ORIENTATION_DELIVERY_RECEIPT: JSON.stringify(delivery),
    NARADA_ORIENTATION_ENTRY_FILE: entryFile,
  };
}
