import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from '@narada-core/sqlite';
import { normalizeExecutionBinding } from '@narada-core/execution-contract';
import {
  buildLifecycleTargetLocusStatus,
  guardLifecycleTargetLocus,
} from '../src/kernel/tool-call-pipeline.js';
import { assertExecutionBindingScope } from '../src/task-lifecycle/task-lifecycle-create-recurring-handlers.js';
import { bindTaskExecution, readTaskExecutionBinding } from '../src/task-lifecycle/task-execution-state.js';

const userSiteRoot = 'C:\\Users\\Andrey\\Narada';
const repositorySiteRoot = 'C:\\workspace\\narada';
const guardedTools = new Set(['task_lifecycle_create']);

const mismatch = buildLifecycleTargetLocusStatus({
  siteRoot: userSiteRoot,
  env: { NARADA_REQUESTED_WORK_ROOT: repositorySiteRoot },
});
assert.equal(mismatch.status, 'operator_stated_locus_mismatch');
assert.equal(mismatch.default_target_site_root, userSiteRoot);
assert.equal(mismatch.operator_stated_locus_root, repositorySiteRoot);

const refusal = guardLifecycleTargetLocus({
  canonicalName: 'task_lifecycle_create',
  args: { payload_ref: 'mcp_payload:external-repository-task@v1' },
  siteRoot: userSiteRoot,
  env: { NARADA_REQUESTED_WORK_ROOT: repositorySiteRoot },
  locusGuardedMutationTools: guardedTools,
});
assert.equal(refusal.status, 'refused');
assert.ok('refusal_code' in refusal);
assert.ok('remediation' in refusal);
assert.equal(refusal.refusal_code, 'target_locus_preflight_required');
assert.match(refusal.remediation, /intended Site/i);

const destinationStatus = buildLifecycleTargetLocusStatus({
  siteRoot: repositorySiteRoot,
  env: { NARADA_REQUESTED_WORK_ROOT: repositorySiteRoot },
});
assert.equal(destinationStatus.status, 'clear');

const binding = normalizeExecutionBinding({
  workspace_root: repositorySiteRoot,
  repository_root: repositorySiteRoot,
  site_root: repositorySiteRoot,
  executor_kind: 'operator',
  correlation_key: 'user-site-task-2211-narada-proper-v2',
});
assert.equal(binding.workspace_root, repositorySiteRoot);
assert.equal(binding.repository_root, repositorySiteRoot);
assert.equal(binding.site_root, repositorySiteRoot);

const database = new DatabaseSync(':memory:');
try {
  const store = { db: database };
  bindTaskExecution(store, { task_id: 'task-bound-readback', task_number: 1 }, binding);
  const readback = readTaskExecutionBinding(store, 'task-bound-readback');
  assert.equal(readback.status, 'bound');
  assert.deepEqual(readback.binding, binding);
  assert.ok(readback.created_at);
  assert.ok(readback.updated_at);
  assert.deepEqual(readTaskExecutionBinding(store, 'task-unbound-readback'), {
    status: 'unbound',
    binding: null,
  });
} finally {
  database.close();
}

const projectRoot = mkdtempSync(join(tmpdir(), 'task-lifecycle-project-site-'));
try {
  const projectSiteRoot = join(projectRoot, '.narada');
  mkdirSync(join(projectRoot, '.git'), { recursive: true });
  mkdirSync(projectSiteRoot, { recursive: true });
  writeFileSync(join(projectSiteRoot, 'config.json'), JSON.stringify({ site_id: 'project', site_root: projectSiteRoot }));
  const projectBinding = normalizeExecutionBinding({
    workspace_root: projectRoot,
    repository_root: projectRoot,
    site_root: projectSiteRoot,
    executor_kind: 'operator',
    correlation_key: 'project-site-parent-repository',
  });
  assert.doesNotThrow(() => assertExecutionBindingScope(projectBinding, projectSiteRoot));
  assert.doesNotThrow(() => assertExecutionBindingScope(projectBinding, projectRoot));
  assert.throws(
    () => assertExecutionBindingScope({ ...projectBinding, repository_root: tmpdir() }, projectSiteRoot),
    /repository_outside_site_root/,
  );
} finally {
  rmSync(projectRoot, { recursive: true, force: true });
}

console.log('target locus and execution binding tests passed');
