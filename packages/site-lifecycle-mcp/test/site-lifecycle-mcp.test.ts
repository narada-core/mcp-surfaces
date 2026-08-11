import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { createServerState, handleRequest, listTools } from '../src/main.js';

const state = createServerState({ naradaRoot: 'D:/definitely/missing/narada' });
assert.equal(createServerState({}, { NARADA_SRC_ROOT: 'C:/portable-src' }).naradaRoot, resolve('C:/portable-src/narada').replace(/\\/g, '/'));
const tools = listTools();
const names = tools.map((tool) => tool.name);

assert.equal(names.includes('site_lifecycle_doctor'), true);
assert.equal(names.includes('site_create_plan'), true);
assert.equal(names.includes('site_init'), true);
assert.equal(names.includes('site_deps_sync'), true);
assert.equal(names.includes('site_lifecycle_preflight'), true);
assert.equal(names.includes('site_admit_role'), true);
assert.equal(names.includes('site_verify_role'), true);
assert.equal(names.includes('site_observe_runtime'), true);
assert.equal(names.includes('site_bind_runtime'), true);
assert.equal(names.includes('site_registry_list'), false);
assert.equal((tools.find((tool) => tool.name === 'site_discover') as any).annotations.readOnlyHint, false);
assert.equal((tools.find((tool) => tool.name === 'site_init') as any).annotations.readOnlyHint, false);
assert.equal((tools.find((tool) => tool.name === 'site_deps_sync') as any).annotations.readOnlyHint, false);
assert.equal((tools.find((tool) => tool.name === 'site_admit_role') as any).annotations.readOnlyHint, false);
assert.equal((tools.find((tool) => tool.name === 'site_bind_runtime') as any).annotations.readOnlyHint, false);
assert.equal((tools.find((tool) => tool.name === 'site_verify_role') as any).annotations.readOnlyHint, true);
assert.equal((tools.find((tool) => tool.name === 'site_observe_runtime') as any).annotations.readOnlyHint, true);

const doctorResponse = await ((handleRequest({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'site_lifecycle_doctor', arguments: {} } }, state)) as any) as any;
assert.equal(doctorResponse?.error, undefined);
const doctorPayload = JSON.parse((doctorResponse?.result as { content: { text: string }[] }).content[0].text);
assert.equal(doctorPayload.status, 'cli_module_missing');
assert.equal(doctorPayload.cli_module_exists, false);

const mapResponse = await ((handleRequest({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'site_lifecycle_command_map', arguments: {} } }, state)) as any) as any;
const mapPayload = JSON.parse((mapResponse?.result as { content: { text: string }[] }).content[0].text);
assert.equal(mapPayload.commands.some((item: { tool: string; cli_command: string }) => item.tool === 'site_create_plan' && item.cli_command === 'narada sites create --dry-run'), true);
assert.equal(mapPayload.commands.some((item: { tool: string; cli_command: string }) => item.tool === 'site_admit_role' && item.cli_command === 'narada operator-surface agent instantiate'), true);
assert.equal(mapPayload.commands.some((item: { tool: string; cli_command: string }) => item.tool === 'site_bind_runtime' && item.cli_command === 'narada operator-surface bind-focused'), true);

const plannedMutation = await ((handleRequest({
  jsonrpc: '2.0',
  id: 3,
  method: 'tools/call',
  params: {
    name: 'site_admit_role',
    arguments: {
      site_id: 'demo-site',
      site_root: 'D:/fixture-site',
      role: 'architect',
      agent_kind: 'codex_cli',
      by: 'operator',
      authority_basis: { kind: 'test', summary: 'fixture' },
    },
  },
}, state)) as any) as any;
const plannedPayload = JSON.parse((plannedMutation?.result as { content: { text: string }[] }).content[0].text);
assert.equal(plannedPayload.status, 'planned');
assert.equal(plannedPayload.mutation_performed, false);
assert.equal(plannedPayload.next_action.schema, 'narada.site_lifecycle.next_action.v1');
assert.equal(plannedPayload.next_action.tool, 'site_admit_role');
assert.equal(plannedPayload.next_action.status, 'planned');
assert.equal(plannedPayload.next_action.arguments.site_id, 'demo-site');

console.log('site-lifecycle-mcp behavior ok');