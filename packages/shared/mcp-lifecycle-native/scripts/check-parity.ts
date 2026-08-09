import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { taskLifecycleTools } from '../../../task-lifecycle-mcp/src/task-lifecycle/task-mcp-server.js';
import { listTools } from '../../../work-lifecycle-mcp/src/main.js';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));

function readCatalog(name: string): unknown[] {
  const value = JSON.parse(readFileSync(new URL(`../catalog/${name}`, import.meta.url), 'utf8')) as unknown;
  assert.ok(Array.isArray(value), `${name}: catalog must be an array`);
  return value;
}

const taskCatalog = readCatalog('task-tools.json');
const workCatalog = readCatalog('work-tools.json');
assert.deepEqual(taskLifecycleTools(), taskCatalog, 'task tools/list catalog drifted from TypeScript authority');
assert.deepEqual(listTools(), workCatalog, 'work tools/list catalog drifted from TypeScript authority');
process.stdout.write(`${JSON.stringify({
  schema: 'narada.mcp_lifecycle_native.parity.v1',
  status: 'passed',
  task_tools: taskCatalog.length,
  work_tools: workCatalog.length,
  package_root: packageRoot,
})}\n`);
