import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { taskLifecycleTools } from '../../../task-lifecycle-mcp/src/task-lifecycle/task-mcp-server.ts';
import { listTools } from '../../../work-lifecycle-mcp/src/main.ts';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const read = (name) => JSON.parse(readFileSync(new URL(`../catalog/${name}`, import.meta.url), 'utf8'));
assert.deepEqual(taskLifecycleTools(), read('task-tools.json'), 'task tools/list catalog drifted from TypeScript authority');
assert.deepEqual(listTools(), read('work-tools.json'), 'work tools/list catalog drifted from TypeScript authority');
process.stdout.write(JSON.stringify({ schema: 'narada.mcp_lifecycle_native.parity.v1', status: 'passed', task_tools: read('task-tools.json').length, work_tools: read('work-tools.json').length, package_root: packageRoot }) + '\n');
