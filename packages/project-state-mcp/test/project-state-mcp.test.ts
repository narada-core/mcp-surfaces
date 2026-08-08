import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { createServerState, handleRequest, listTools } from '../src/main.js';
import { surfaceDefinition } from '../src/surface-definition.js';

const names = listTools().map((tool) => tool.name);
assert.deepEqual(names.sort(), [
  'project_state_command_map',
  'project_state_doctor',
  'project_state_gaps',
  'project_state_handoff',
  'project_state_guidance',
  'project_state_matrix',
  'project_state_program_list',
  'project_state_program_show',
  'project_state_project_list',
  'project_state_project_show',
  'project_state_standards_list',
  'project_state_standard_show',
  'project_state_applicability',
  'project_state_standard_trace',
  'project_state_standard_gaps',
  'project_state_validate',
].sort());

const definition = surfaceDefinition();
assert.equal(definition.descriptor.surface_id, 'project-state');
assert.equal(definition.descriptor.projections[0]?.default_injection, 'disabled');
assert.equal(definition.descriptor.projections[0]?.injection_scope, 'local_site');
assert.equal(definition.descriptor.tools.every((tool) => tool.effect.class === 'read'), true);

const fixtureDir = await mkdtemp(join(tmpdir(), 'project-state-mcp-'));
const fixtureCli = join(fixtureDir, 'scripts', 'project-state-cli.mjs');
await mkdir(join(fixtureDir, 'scripts'), { recursive: true });
await writeFile(fixtureCli, `
import process from 'node:process';
const args = process.argv.slice(2);
process.stdout.write(JSON.stringify({ schema: 'narada.project_state.cli_result.v1', status: 'ok', virtual_only: true, result: { args } }));
` , 'utf8');

try {
  const state = createServerState({ projectRoot: fixtureDir });
  const doctorResponse = await handleRequest({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'project_state_doctor', arguments: {} } }, state) as JsonRpcResponse;
  const doctor = JSON.parse(String(doctorResponse.result?.content?.[0]?.text));
  assert.equal(doctor.status, 'ok');
  const listResponse = await handleRequest({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'project_state_program_list', arguments: {} } }, state) as JsonRpcResponse;
  const list = JSON.parse(String(listResponse.result?.content?.[0]?.text));
  assert.equal(list.read_only, true);
  assert.equal(list.virtual_only, true);
  assert.equal(list.result.args.includes('program'), true);
  assert.equal(list.result.args.includes('list'), true);
  const showResponse = await handleRequest({ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'project_state_program_show', arguments: { program_id: 'demo' } } }, state) as JsonRpcResponse;
  const show = JSON.parse(String(showResponse.result?.content?.[0]?.text));
  assert.equal(show.result.args.includes('program'), true);
  assert.equal(show.result.args.includes('show'), true);
  assert.equal(show.result.args.includes('demo'), true);
  const bad = await handleRequest({ jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'project_state_program_show', arguments: {} } }, state) as JsonRpcResponse;
  assert.equal(bad.error?.data?.code, 'required_argument_missing');
  const traceResponse = await handleRequest({ jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'project_state_standard_trace', arguments: { standard_id: 'iso-15288-2023', status: 'open_gap' } } }, state) as JsonRpcResponse;
  const trace = JSON.parse(String(traceResponse.result?.content?.[0]?.text));
  assert.equal(trace.result.args.includes('trace'), true);
  assert.equal(trace.result.args.includes('iso-15288-2023'), true);
  assert.equal(trace.result.args.includes('open_gap'), true);
  const handoffResponse = await handleRequest({ jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'project_state_handoff', arguments: { project_id: 'NRC600' } } }, state) as JsonRpcResponse;
  const handoff = JSON.parse(String(handoffResponse.result?.content?.[0]?.text));
  assert.equal(handoff.result.args.includes('handoff'), true);
  assert.equal(handoff.result.args.includes('NRC600'), true);
} finally {
  await rm(fixtureDir, { recursive: true, force: true });
}

type JsonRpcResponse = { result?: { content?: Array<{ text?: string }>; [key: string]: unknown }; error?: { data?: { code?: string } } };
console.log('project-state-mcp behavior ok');
