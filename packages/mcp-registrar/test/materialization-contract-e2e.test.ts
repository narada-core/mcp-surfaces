import assert from 'node:assert/strict';
import test from 'node:test';
import { createServerState, handleRequest, parseArgs } from '../src/main.js';

test('Registrar exposes no carrier-materialization compatibility authority', async () => {
  for (const args of [
    ['--materialize-all'],
    ['--materialize-carrier', 'codex-test'],
    ['--runtime-profile', 'bun'],
    ['--runtime-proxy-implementation', 'node'],
  ]) {
    assert.throws(() => parseArgs(args), /registrar_unknown_cli_argument/);
  }

  const listed = await handleRequest({
    jsonrpc: '2.0', id: 1, method: 'tools/list', params: {},
  }, createServerState({}));
  const names = ((listed as Record<string, any>).result?.tools as Array<{ name: string }>).map((tool) => tool.name);
  assert.equal(names.includes('registrar_materialize_all'), false);

  const called = await handleRequest({
    jsonrpc: '2.0', id: 2, method: 'tools/call',
    params: { name: 'registrar_materialize_all', arguments: {} },
  }, createServerState({}));
  assert.equal((called as Record<string, any>).error?.data?.code, 'unknown_tool');
});
