import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import {
  admitOrientationRequest,
  inspectOrientationEntryAdmission,
} from '../src/orientation-entry-admission.js';
import {
  expectedOrientationCallAdmission,
  expectedOrientationEntryState,
  loadOrientationEntryConformanceCorpus,
  materializeOrientationEntryCase,
} from './orientation-entry-conformance.js';

const corpus = loadOrientationEntryConformanceCorpus();

for (const testCase of corpus.cases) {
  test(`TypeScript orientation admission satisfies shared case: ${testCase.id}`, () => {
    const root = mkdtempSync(join(tmpdir(), `orientation-admission-${testCase.id}-`));
    try {
      const materialized = materializeOrientationEntryCase({ root, corpus, testCase });
      const state = inspectOrientationEntryAdmission(materialized.environment);
      assert.deepEqual(
        state,
        expectedOrientationEntryState(testCase, materialized.entryFile),
        testCase.id,
      );
      const calls = [
        {
          kind: 'ordinary' as const,
          surfaceId: 'performative-work',
          toolName: 'work_perform',
        },
        {
          kind: 'orientation_read' as const,
          surfaceId: 'agent-context',
          toolName: 'agent_orientation_read',
        },
        {
          kind: 'orientation_acknowledge' as const,
          surfaceId: 'agent-context',
          toolName: 'agent_orientation_acknowledge',
        },
        {
          kind: 'transport' as const,
          surfaceId: 'agent-context',
          toolName: 'mcp_output_show',
        },
        {
          kind: 'hidden' as const,
          surfaceId: 'agent-context',
          toolName: 'agent_context_checkpoint_create',
        },
      ];
      for (const call of calls) {
        const admission = admitOrientationRequest({
          surfaceId: call.surfaceId,
          messageKind: 'request',
          method: 'tools/call',
          params: { name: call.toolName, arguments: {} },
          environment: materialized.environment,
        });
        assert.equal(
          admission.admitted,
          expectedOrientationCallAdmission(testCase, call.kind),
          `${testCase.id}:${call.kind}`,
        );
        assert.deepEqual(admission.state, state, `${testCase.id}:${call.kind}:state`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test('blocked orientation admission distinguishes every request and notification category', () => {
  const root = mkdtempSync(join(tmpdir(), 'orientation-admission-method-matrix-'));
  try {
    const testCase = corpus.cases.find((candidate) => candidate.id === 'acknowledgement_absent');
    assert.ok(testCase);
    const materialized = materializeOrientationEntryCase({ root, corpus, testCase });
    const request = (method: string, params: unknown = {}) => admitOrientationRequest({
      surfaceId: 'agent-context',
      messageKind: 'request',
      method,
      params,
      environment: materialized.environment,
    }).admitted;
    const notification = (method: string, params: unknown = {}) => admitOrientationRequest({
      surfaceId: 'agent-context',
      messageKind: 'notification',
      method,
      params,
      environment: materialized.environment,
    }).admitted;

    for (const method of [
      'initialize',
      'ping',
      'tools/list',
      'resources/list',
      'resources/templates/list',
      'prompts/list',
      'logging/setLevel',
    ]) assert.equal(request(method), true, `request:${method}`);
    for (const method of [
      'notifications/initialized',
      'notifications/cancelled',
      'notifications/progress',
      'notifications/roots/list_changed',
    ]) assert.equal(notification(method), true, `notification:${method}`);

    for (const method of ['resources/read', 'prompts/get', 'completion/complete', 'roots/list']) {
      assert.equal(request(method), false, `blocked_request:${method}`);
    }
    assert.equal(request('notifications/progress'), false, 'notification method cannot masquerade as a request');
    assert.equal(notification('resources/list'), false, 'request method cannot masquerade as a notification');
    assert.equal(notification('tools/call', { name: 'agent_orientation_read' }), false, 'tool calls require request identity');
    assert.equal(request('tools/call', { name: 'agent_orientation_acknowledge' }), false, 'administrative acknowledgement is never a pre-entry occupant call');
    assert.equal(request('tools/call', { name: 'mcp_runtime_proxy_status' }), true, 'proxy status remains available as a request');
    assert.equal(notification('tools/call', { name: 'mcp_runtime_proxy_status' }), false, 'proxy status is not a notification');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
