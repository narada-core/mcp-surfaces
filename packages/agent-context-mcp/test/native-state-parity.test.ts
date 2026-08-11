import assert from 'node:assert/strict';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';

const tsEntrypoint = fileURLToPath(new URL('../src/main.js', import.meta.url));
const rustEntrypoint = fileURLToPath(new URL(`../../native/target/release/narada-agent-context-mcp${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const roots: string[] = [];

try {
  const ts = fixture('ts');
  const rust = fixture('rust');
  const tsClient = client(process.execPath, [tsEntrypoint, '--site-root', ts.root, '--site-id', 'parity', '--tool-projection', 'admin'], ts);
  const rustClient = client(rustEntrypoint, ['--site-root', rust.root, '--site-id', 'parity', '--tool-projection', 'admin'], rust);
  try {
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

function client(executable: string, args: string[], fixtureValue: ReturnType<typeof fixture>) {
  const child = spawn(executable, args, { env: { ...process.env, NARADA_AGENT_CONTEXT_DB: fixtureValue.db }, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true }) as ChildProcessWithoutNullStreams;
  let output = Buffer.alloc(0); let stderr = ''; let id = 0;
  child.stdout.on('data', (chunk) => { output = Buffer.concat([output, chunk]); });
  child.stderr.setEncoding('utf8'); child.stderr.on('data', (chunk) => { stderr += chunk; });
  return {
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
      .replace(/20\d\d-\d\d-\d\dT\d\d:\d\d:\d\d\.\d\d\dZ/g, '<timestamp>')
    : value;
  const result: Record<string, unknown> = {};
  for (const [key, entry] of Object.entries(value)) {
    if (['checkpoint_id', 'archived_prior', 'content_hash'].includes(key)) result[key] = entry == null ? null : `<${key}>`;
    else if (['checkpoint_at', 'created_at'].includes(key)) result[key] = '<timestamp>';
    else if (key === 'source_checkpoint_ref') result[key] = '<checkpoint-ref>';
    else result[key] = normalize(entry, fixtureValue);
  }
  return result;
}

function dbCounts(path: string) {
  const db = new DatabaseSync(path, { readOnly: true });
  try { return { current: db.prepare('SELECT COUNT(*) AS count FROM agent_checkpoints').get()?.count, history: db.prepare('SELECT COUNT(*) AS count FROM agent_checkpoint_history').get()?.count }; }
  finally { db.close(); }
}
