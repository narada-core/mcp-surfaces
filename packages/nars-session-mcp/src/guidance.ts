import { compactGuidanceResult } from '@narada-core/mcp-fabric-contracts';

export const NARS_SESSION_GUIDANCE_SCHEMA = 'narada.nars_session_mcp.guidance.v1';

export function guidanceToolDefinition() {
  return {
    name: 'nars_session_guidance',
    description: 'Explain governed NARS session discovery, input delivery, authority fencing, and status readback.',
    inputSchema: {
      type: 'object',
      properties: {},
      additionalProperties: false,
    },
    annotations: {
      title: 'NARS session guidance',
      readOnlyHint: true,
      destructiveHint: false,
      idempotentHint: true,
      openWorldHint: false,
    },
    outputSchema: { type: 'object', additionalProperties: true },
  };
}

export function buildGuidanceResult() {
  return compactGuidanceResult({
    schema: NARS_SESSION_GUIDANCE_SCHEMA,
    status: 'ok',
    purpose: 'Deliver bounded, authorized input to an already-existing NARS session and read back authoritative admission, liveness, request-state, and terminal evidence.',
    first_use: [
      'Call nars_session_list to discover sessions in the bound Site scope.',
      'Call nars_session_show with an explicit session_id to verify liveness and authority posture.',
      'Call nars_session_input_deliver with explicit delivery: send, enqueue, or steer and an idempotency_key.',
      'Use nars_session_input_status to distinguish the backwards-compatible admission-oriented status field (status_semantics: admission), admission_status, request_state, terminal_state, and outcome.',
    ],
    delivery_modes: {
      send: 'Current or next eligible turn according to NARS queue semantics; never assume provider completion.',
      enqueue: 'NARS-owned durable queue entry after the active turn; this surface owns no second queue.',
      steer: 'Explicit interruptive delivery; disabled by default and never inferred from timeout or activity.',
    },
    authority: [
      'The bound caller supplies intent; it does not supply an arbitrary source principal or authority locus.',
      'The NARS authority runtime owns session state, queue state, turn admission, and provider dispatch.',
      'A stale, closed, superseded, or non-writable session is refused.',
    ],
    boundaries: [
      'This is live session coordination, not a task, inbox, delegated-work, or hidden message-bus surface.',
      'Do not write control.jsonl or operator-input-queue.json directly.',
      'Transport acknowledgement is not provider completion.',
      'The sessions-list authority_root/scope_root identifies the bounded discovery authority; session.site_root identifies the admitted Site that owns that session.',
      'last_seen_at is a persisted discovery projection. Use heartbeat_at, heartbeat_age_ms, heartbeat_fresh, and health_observed_at for current read-only liveness evidence.',
      'A retained queue item is recovery evidence, not proof that the corresponding request is still running; correlate request_id, input_event_id, turn_id, and runtime_request_id.',
    ],
    recovery: [
      'On timeout, call nars_session_input_status; do not retry blindly with a new idempotency key.',
      'On stale authority refusal, rediscover the session and require an explicit current session_id.',
      'On missing health or event endpoint, repair the carrier launch/session registration rather than bypassing the authority runtime.',
    ],
  });
}
