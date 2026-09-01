import assert from 'node:assert/strict';
import test from 'node:test';
import {
  assertClaimMatchesAuthenticatedIdentity,
  buildIdentityState,
} from '../src/identity-state.js';

test('anonymous identity has no claim, authentication, or authority', () => {
  const state = buildIdentityState();
  assert.equal(state.claimed_identity.status, 'unclaimed');
  assert.equal(state.claimed_identity.identity, null);
  assert.equal(state.authentication.status, 'missing');
  assert.equal(state.authority.status, 'not_evaluated');
  assert.equal(state.authority.granted, false);
});

test('claimed-only identity survives missing authentication without authority', () => {
  const state = buildIdentityState({
    claimed_identity: 'worker.builder',
    claimed_identity_source: 'caller_assertion',
  });
  assert.equal(state.claimed_identity.status, 'claimed');
  assert.equal(state.claimed_identity.identity, 'worker.builder');
  assert.equal(state.authentication.status, 'missing');
  assert.equal(state.authentication.authenticated_identity, null);
  assert.equal(state.claimed_identity.authority_granted, false);
  assert.equal(state.authority.granted, false);
});

test('authentication is independent from a claim and does not grant operation authority', () => {
  const state = buildIdentityState({
    claimed_identity: 'worker.builder',
    authenticated_identity: 'worker.builder',
    authentication_evidence_refs: ['receipt:worker'],
  });
  assert.equal(state.claimed_identity.identity, 'worker.builder');
  assert.equal(state.authentication.status, 'authenticated');
  assert.deepEqual(state.authentication.evidence_refs, ['receipt:worker']);
  assert.equal(state.authority.status, 'not_evaluated');
  assert.equal(state.authority.granted, false);
  assert.doesNotThrow(() => assertClaimMatchesAuthenticatedIdentity(state));
});

test('explicit operation authority is still a separate decision', () => {
  const state = buildIdentityState({
    claimed_identity: 'operator.architect',
    authenticated_identity: 'operator.architect',
    authority: {
      status: 'authorized',
      operation: 'task_lifecycle.claim',
      granted: true,
      evidence_refs: ['authority:claim-1'],
    },
  });
  assert.equal(state.authority.status, 'authorized');
  assert.equal(state.authority.operation, 'task_lifecycle.claim');
  assert.equal(state.authority.granted, true);
  assert.equal(state.claimed_identity.authority_granted, false);
});

test('a mismatched claim is not silently promoted to the authenticated identity', () => {
  const state = buildIdentityState({
    claimed_identity: 'other.agent',
    authenticated_identity: 'worker.builder',
  });
  assert.throws(
    () => assertClaimMatchesAuthenticatedIdentity(state),
    /agent_context_claimed_identity_mismatch/,
  );
});
