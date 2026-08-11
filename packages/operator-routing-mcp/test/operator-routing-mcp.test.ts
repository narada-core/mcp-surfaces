import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { createServerState, handleRequest, listTools, operatorRouteRequest } from '../src/main.js';

const siteRoot = mkdtempSync(resolve(tmpdir(), 'operator-routing-mcp-'));
const logPath = resolve(siteRoot, '.narada', 'runtime', 'operator-routing', 'operator-routing-log.jsonl');

try {
  const state = createServerState({ siteRoot });
  const init = (handleRequest({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } }, state)) as any;
  assert.equal(init?.result.serverInfo.name, 'operator-routing-mcp');
  const listed = (handleRequest({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} }, state)) as any;
  const names = (listed?.result.tools as Array<Record<string, unknown>>).map((tool) => tool.name);
  assert.ok(names.includes('operator_route_doctor'));
  assert.ok(names.includes('operator_route_request'));
  assert.deepEqual(listTools().map((tool) => tool.name), ['operator_routing_guidance', 'operator_route_doctor', 'operator_route_request']);

  const route = operatorRouteRequest({
    transcript: 'route this to codex',
    target_runtime: 'codex',
    target_identity: 'narada-codex.resident',
    allow_inbox_fallback: true,
    speaker_agent_id: 'andrey-user.resident',
  }, state);
  assert.equal(route.status, 'drafted_for_site_inbox');
  assert.equal((route.spoken_acknowledgement as Record<string, unknown>).model, 'tts-1');
  assert.equal((route.spoken_acknowledgement as Record<string, unknown>).voice, 'nova');
  assert.match(String((route.spoken_acknowledgement as Record<string, unknown>).text), /not available from this surface/);
  assert.equal((route.inbox_envelope as Record<string, unknown>).kind, 'command_request');
  assert.equal((route.inbox_envelope as Record<string, unknown>).target_role, null);
  assert.equal((route.routing as Record<string, unknown>).next_step, 'handoff_to_site-inbox');

  const typedRoute = operatorRouteRequest({
    transcript: 'admit the architect for fixture-site',
    target_runtime: 'codex_cli',
    target_site_id: 'fixture-site',
    target_site_root: siteRoot,
    operation_kind: 'role_admission',
    role: 'architect',
    agent_kind: 'codex_cli',
    principal: 'operator',
    target_identity: 'fixture-site.architect',
    allow_inbox_fallback: true,
  }, state);
  const typedRouting = typedRoute.routing as Record<string, unknown>;
  const typedHandoff = typedRouting.handoff as Record<string, unknown>;
  assert.equal(typedHandoff.schema, 'narada.mcp_handoff.v1');
  assert.equal(typedHandoff.target_surface, 'site-lifecycle');
  assert.equal(typedHandoff.tool, 'site_admit_role');
  assert.equal(typedHandoff.status, 'ready');
  assert.equal(typedHandoff.executable, true);
  assert.deepEqual(typedHandoff.required_inputs, []);
  const typedEnvelope = typedRoute.inbox_envelope as Record<string, unknown>;
  assert.equal((typedEnvelope.payload as Record<string, unknown>).typed_handoff, typedHandoff);
  assert.equal(typedRoute.log_path, logPath);

  const logLines = readFileSync(logPath, 'utf8').trim().split(/\r?\n/);
  assert.equal(logLines.length, 2);
  const logged = JSON.parse(logLines[1]) as Record<string, unknown>;
  assert.equal(logged.request_id, typedRoute.request_id);
  assert.equal(logged.target_runtime, 'codex_cli');
  assert.equal((logged.routing as Record<string, unknown>).next_step, 'handoff_to_site-lifecycle');
} finally {
  rmSync(siteRoot, { recursive: true, force: true });
}

console.log('operator-routing-mcp tests passed');