import assert from 'node:assert/strict';
import test from 'node:test';
import {
  CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA,
  type CarrierSessionAdmissionReceipt,
} from '@narada-core/orientation-manifest';
import {
  assertAdmissionMatchesAgentContext,
  buildAgentContextOrientationProjections,
} from '../src/orientation-manifest.js';

function admission(overrides: Partial<CarrierSessionAdmissionReceipt> = {}): CarrierSessionAdmissionReceipt {
  return {
    schema: CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA,
    receipt_id: 'receipt:adapter-test',
    decision: 'admitted',
    state: 'starting',
    coordinate: {
      authority_scope: 'test',
      site_ref: 'site:narada.test',
      carrier_session_id: 'carrier_test',
      authority_epoch: 1,
    },
    agent_identity: {
      source_authority_ref: 'agent-identity:narada.test',
      artifact_ref: 'agent:narada.test:resident@1',
      revision: '1',
      local_agent_id: 'resident',
      canonical_agent_id: 'narada.test.resident',
    },
    carrier_kind: 'codex',
    admission_policy: {
      source_authority_ref: 'site-law:narada.test',
      artifact_ref: 'carrier-policy:default',
      revision: '1',
    },
    issued_at: '2026-08-08T10:00:00.000Z',
    valid_until: '2026-08-08T12:00:00.000Z',
    authority_readback_ref: 'carrier-session-authority:carrier_test',
    evidence_refs: [],
    reason_codes: [],
    ...overrides,
  };
}

test('Agent Context refuses an expired exact admission receipt', () => {
  assert.throws(
    () => assertAdmissionMatchesAgentContext(admission(), {
      siteId: 'narada.test',
      identity: 'resident',
      carrierSessionId: 'carrier_test',
      observedAt: '2026-08-08T12:00:00.000Z',
    }),
    /agent_context_admission_receipt_expired/,
  );
});

test('Agent Context adapter revisions are independent of object insertion order', () => {
  const input = {
    siteRoot: process.cwd(),
    siteId: 'narada.test',
    admissionReceipt: admission({ valid_until: null }),
    observedAt: '2026-08-08T11:00:00.000Z',
  };
  const first = buildAgentContextOrientationProjections({
    ...input,
    roleBinding: {
      binding_authority: 'agent_roster',
      role_name: 'resident',
      nested: { beta: 2, alpha: 1 },
    },
  });
  const second = buildAgentContextOrientationProjections({
    ...input,
    roleBinding: {
      nested: { alpha: 1, beta: 2 },
      role_name: 'resident',
      binding_authority: 'agent_roster',
    },
  });
  const firstRole = first.find((entry) => entry.entry_kind === 'role_binding');
  const secondRole = second.find((entry) => entry.entry_kind === 'role_binding');
  assert.equal(firstRole?.revision, secondRole?.revision);
});
