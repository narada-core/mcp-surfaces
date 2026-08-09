import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import {
  CARRIER_SESSION_ADMISSION_RECEIPT_SCHEMA,
  buildOrientationBrief,
  type CarrierSessionAdmissionReceipt,
} from '@narada-core/orientation-manifest';
import {
  assertAdmissionMatchesAgentContext,
  buildAgentContextOrientationProjections,
  compileAgentContextOrientation,
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

test('exact continuity and work selections are bounded and executable at Carrier entry', () => {
  const siteRoot = mkdtempSync(join(tmpdir(), 'orientation-actionable-selection-'));
  writeFileSync(join(siteRoot, 'AGENTS.md'), '# Exact site law\n', 'utf8');
  try {
    const compilation: any = compileAgentContextOrientation({
      siteRoot,
      siteId: 'narada.test',
      admissionReceipt: admission({ valid_until: null }),
      observedAt: '2026-08-08T11:00:00.000Z',
      exactCheckpoint: {
        status: 'ok',
        checkpoint_id: 'checkpoint-exact-1',
        checkpoint_at: '2026-08-08T10:59:00.000Z',
        continuation: {
          objective: 'Preserve exact continuity across occupant turnover.',
          current_state: 'The evidence boundary is implemented.',
          next_action: 'Run the performative carrier checks.',
        },
        continuation_blockers: [],
      },
      exactWork: {
        status: 'ok',
        task_id: 'task-exact-42',
        task_number: 42,
        lifecycle: {
          status: 'in_progress',
          continuation_packet: { next_action: 'Exercise the live gate.' },
          updated_at: '2026-08-08T10:58:00.000Z',
        },
        specification: {
          title: 'Prove Carrier-entry orientation',
          goal_markdown: 'Prove ordinary work is impossible before exact orientation.',
        },
      },
    });
    assert.equal(compilation.manifest.delivery, 'deliverable');
    const brief: any = buildOrientationBrief({
      manifest: compilation.manifest,
      manifestArtifactRef: 'narada-agent-context://orientation-manifest/exact',
    });
    assert.equal(brief.continuity_selection.mode, 'exact');
    assert.equal(
      brief.continuity_selection.summary.checkpoint_id,
      'checkpoint-exact-1',
    );
    assert.equal(brief.continuity_selection.inspection_call, null);
    assert.equal(brief.work_selection.summary.task_number, 42);
    assert.equal(
      brief.work_selection.summary.next_action,
      'Exercise the live gate.',
    );
    assert.deepEqual(brief.work_selection.inspection_call, {
      surface_id: 'task-lifecycle',
      tool: 'task_lifecycle_inspect_range',
      arguments: {
        start_task_number: 42,
        end_task_number: 42,
        include_body: true,
        limit: 1,
      },
    });
    assert.equal(brief.required_reads.length, 2);
    const continuityRead: any = brief.required_reads.find(
      (step: any) => step.source.artifact_ref.startsWith('orientation-manifest-entry:'),
    );
    assert.ok(continuityRead);
    assert.equal(continuityRead.ordinal, 2);
    assert.equal(continuityRead.tool.name, 'agent_orientation_read');
    const continuityEntry: any = compilation.manifest.entries.find(
      (entry: any) => entry.entry_kind === 'exact_continuity',
    );
    assert.equal(continuityRead.source.revision, continuityEntry.revision);
    assert.equal(brief.inline_bytes <= brief.max_inline_bytes, true);
  } finally {
    rmSync(siteRoot, { recursive: true, force: true });
  }
});

test('required-read call count is admitted before a Carrier-entry generation exists', () => {
  const siteRoot = mkdtempSync(join(tmpdir(), 'orientation-page-bound-'));
  writeFileSync(join(siteRoot, 'AGENTS.md'), '\0'.repeat(70_000), 'utf8');
  try {
    assert.throws(() => compileAgentContextOrientation({
      siteRoot,
      siteId: 'narada.test',
      admissionReceipt: admission({ valid_until: null }),
      observedAt: '2026-08-08T11:00:00.000Z',
    }), /agent_context_orientation_required_read_page_bound_exceeded/);
  } finally {
    rmSync(siteRoot, { recursive: true, force: true });
  }
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
