import assert from 'node:assert/strict';
import { validateGovernedTestEvidenceRefs } from '../src/task-lifecycle/governed-test-evidence.js';

const admitted = validateGovernedTestEvidenceRefs([
  'structured_command_execution:e_123',
  'test_mcp_artifact:artifact_abc',
  'mcp_output:output_456',
]);
assert.equal(admitted.status, 'unverified');
assert.equal(admitted.verification_eligible, false);
assert.deepEqual(admitted.execution_refs, ['structured_command_execution:e_123', 'test_mcp_artifact:artifact_abc']);
assert.deepEqual(admitted.diagnostic_refs, ['mcp_output:output_456']);
assert.deepEqual(admitted.verified_refs, []);
assert.equal(admitted.unverified_refs.length, 2);

const verified = validateGovernedTestEvidenceRefs([
  'structured_command_execution:e_123',
  'test_mcp_artifact:artifact_abc',
], {
  resolve: (ref) => ({ status: 'verified', reason: `verified:${ref}` }),
});
assert.equal(verified.status, 'admissible');
assert.equal(verified.verification_eligible, true);
assert.deepEqual(verified.verified_refs, ['structured_command_execution:e_123', 'test_mcp_artifact:artifact_abc']);
assert.deepEqual(verified.unverified_refs, []);

const mixedRejected = validateGovernedTestEvidenceRefs([
  'structured_command_execution:e_123',
  'test:copied-log',
], {
  resolve: (ref) => ({ status: 'verified', reason: `verified:${ref}` }),
});
assert.equal(mixedRejected.status, 'rejected');
assert.equal(mixedRejected.verification_eligible, false);
assert.equal(mixedRejected.rejected_refs.length, 1);

const diagnosticOnly = validateGovernedTestEvidenceRefs(['mcp_output:output_456']);
assert.equal(diagnosticOnly.status, 'diagnostic_only');
assert.equal(diagnosticOnly.verification_eligible, false);

const rejected = validateGovernedTestEvidenceRefs([
  'C:\\\\Users\\\\Andrey\\\\.ai\\\\tmp\\\\native-focused-tests.cmd',
  'C:\\\\Users\\\\Andrey\\\\.ai\\\\tmp\\\\native-focused-tests.log',
  'C:\\\\Users\\\\Andrey\\\\.ai\\\\tmp\\\\native-focused-tests.exit',
  'test:untyped-narrative',
]);
assert.equal(rejected.status, 'rejected');
assert.equal(rejected.verification_eligible, false);
assert.equal(rejected.rejected_refs.length, 4);
assert.match(rejected.remediation, /structured_command_execution/);

const transientPath = 'C:\\Users\\Andrey\\.ai\\tmp\\native-focused-tests.ps1';
const transient = validateGovernedTestEvidenceRefs([transientPath]);
assert.equal(transient.status, 'rejected');
assert.deepEqual(transient.rejected_refs, [{ ref: transientPath, reason: 'transient_path_not_admissible' }]);

const empty = validateGovernedTestEvidenceRefs([]);
assert.equal(empty.status, 'not_provided');
assert.equal(empty.verification_eligible, false);

console.log('governed test evidence validation ok');
