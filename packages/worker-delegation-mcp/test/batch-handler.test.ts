import assert from 'node:assert/strict';
import { normalizeBatchRequests, normalizeOptionalRunIds, normalizeRunIds, requireBatchRequestRecord } from '../src/tool-handlers/batch.js';

assert.deepEqual(normalizeBatchRequests([{ cwd: 'C:/workspace/example' }, null, 'ignored-shape']), [
  { cwd: 'C:/workspace/example' },
  null,
  'ignored-shape',
]);
assert.deepEqual(requireBatchRequestRecord({ cwd: 'C:/workspace/example' }), { cwd: 'C:/workspace/example' });
assert.throws(() => requireBatchRequestRecord(null), /worker_run_batch_item_invalid/);
assert.throws(() => requireBatchRequestRecord('ignored-shape'), /worker_run_batch_item_invalid/);

assert.throws(() => normalizeBatchRequests([]), /worker_run_batch_requests_required/);
assert.throws(() => normalizeBatchRequests(Array.from({ length: 51 }, () => ({}))), /worker_run_batch_too_large/);

assert.deepEqual(normalizeRunIds(['run-a', 'run-a', ' run-b ']), ['run-a', 'run-b']);
assert.throws(() => normalizeRunIds([]), /worker_run_ids_required/);
assert.throws(() => normalizeRunIds(['']), /worker_run_id_required/);

assert.deepEqual(normalizeOptionalRunIds(undefined), []);
assert.deepEqual(normalizeOptionalRunIds(null), []);
assert.deepEqual(normalizeOptionalRunIds(['run-c']), ['run-c']);
