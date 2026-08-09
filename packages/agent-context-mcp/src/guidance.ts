export type GuidanceRecord = Record<string, unknown>;
export type GuidanceToolDefinition = GuidanceRecord & { name: string; description: string; inputSchema: GuidanceRecord; annotations: GuidanceRecord; outputSchema: GuidanceRecord };

const SURFACE_ID = "agent-context";
const GUIDANCE_TOOL = "agent_context_guidance";
const PURPOSE = "Enforced Carrier-entry orientation through one exact inline brief and canonical manifest reference, with administrative checkpoint compatibility.";

export function buildGuidanceResult(args: GuidanceRecord = {}): GuidanceRecord {
  const workflow = typeof args.workflow === 'string' && args.workflow.trim() ? args.workflow.trim() : null;
  const tool = typeof args.tool === 'string' && args.tool.trim() ? args.tool.trim() : null;
  return {
    schema: 'narada.mcp_surface.guidance.v0',
    status: 'ok',
    surface_id: SURFACE_ID,
    guidance_tool: GUIDANCE_TOOL,
    purpose: PURPOSE,
    requested: { workflow, tool },
    first_use: [
      'Normal occupants do not choose or assemble orientation: Carrier entry supplies exact evidence and ordinary tools remain gated until the ceremony completes.',
      'Call agent_orientation_read({}) first and execute only its exact next_call. Treat every continuation as opaque and never inspect or alter it.',
      'Use this administrative guidance only when diagnosing a refused launch, compatibility operation, or checkpoint workflow.',
      'Preserve structuredContent as authoritative evidence; text content is for assistant readability.'
    ],
    tool_preference: [
      { step: 'carrier_entry', guidance: 'Call agent_orientation_read({}) and follow next_call exactly. The thin inline brief contains exact entry-time continuity/work selections or explicit omissions plus one canonical manifest_ref; evidence-plane mechanics remain Carrier-owned.' },
      { step: 'required_reads', guidance: 'Each agent_orientation_read({ continuation }) returns one bounded page and records server-owned evidence. Reusing the same continuation safely replays the same page.' },
      { step: 'acknowledge', guidance: 'The final continuation records canonical acknowledgement and returns status=ready with a compact ready projection. It proves delivery and completed reads, not comprehension or authority for a later action.' },
      { step: 'inspect_selection', guidance: 'Use the brief summary for the exact entry snapshot and its inspection_call, when present, to query the owning live surface for current state.' },
      { step: 'checkpoint', guidance: 'Keep operational checkpoint state authoritative; when fresh-session handoff is needed, add one bounded narada.continuation.v1 object and optionally link its Markdown projection with continuation_ref.' },
      { step: 'export', guidance: 'After checkpointing canonical continuation state, use agent_context_continuation_export to create a Site-local Markdown projection under .ai/continuations and attach its verified reference.' },
      { step: 'administrative_readback', guidance: 'Use the exact manifest MCP resource or compatibility startup sequence only for diagnostics. Neither is the normal occupant entry workflow.' },
      { step: 'diagnose', guidance: 'Use agent_context_hydrate_current only to compile a read-only diagnostic candidate. Omit checkpoint_id to omit continuity, or supply one exact id; the candidate never replaces the admitted generation.' },
      { step: 'consume_continuity', guidance: 'For agent_context_rehydrate and agent_context_continuation_read, omission may inspect the latest checkpoint; an exact checkpoint_id searches current and archived state without fallback. This checkpoint convenience never establishes Agent identity or startup authority.' },
      { step: 'mutate', guidance: 'Only call mutation tools after policy allows it and intent, target, and expected result are explicit.' },
      { step: 'verify', guidance: 'Read back state with the owning surface after any mutation.' }
    ],
    examples: [
      { intent: 'Enforced Carrier entry', call: 'agent_orientation_read({}) // then execute the returned next_call exactly until acknowledgement opens the gate' },
      { intent: 'Tool-specific help', call: "agent_context_guidance({ tool: \"<tool_name>\" })" },
      { intent: 'Workflow-specific help', call: "agent_context_guidance({ workflow: \"<workflow_name>\" })" },
      { intent: 'Portable continuation', call: "agent_context_checkpoint({ continuation: { schema: 'narada.continuation.v1', objective: '<objective>', current_state: '<bounded state summary>', next_action: '<next action>' } }); agent_context_continuation_export({ agent_id: '<agent_id>' }); agent_context_continuation_read({ agent_id: '<agent_id>' })" }
    ],
    anti_patterns: [
      'Do not guess hidden state from a tool name; use doctor/status/list/show tools for evidence.',
      'Do not treat assistant text as the durable record when structuredContent is present.',
      'Do not bypass the owning surface with shell scripts when a governed MCP tool exists.',
      'Do not ask the occupant to select a manifest id, receipt, checkpoint, hash, timestamp, step id, or page offset; use the exact Carrier-bound evidence and opaque returned continuation.',
      'Do not use latest checkpoints, latest start events, names, or hints as Agent/Carrier Session identity evidence.',
      'Do not treat the immutable entry-time work snapshot as current task state.',
      'Do not use agent_context_hydrate_current as admitted startup; it produces a separately identified diagnostic candidate.',
      'Do not treat manifest readback as an owner-issued delivery receipt or as admission for a later action.',
      'Do not store raw transcripts, unbounded history, or diff-only state in continuation.',
      'Do not treat a Markdown projection as authoritative when its reference or canonical content hash is stale.',
      'Do not continue after malformed payloads, empty refs, or ambiguous target identifiers; stop and repair the input.'
    ],
    recovery: [
      'For unknown_tool, call tools/list and this guidance command again after restart.',
      'For policy refusal, inspect the surface policy/doctor output and report the exact refusal reason.',
      'For oversized inputs, use the surface payload_ref or output_ref convention when it exists; otherwise reduce scope.',
      'For unclear behavior, submit surface_feedback_submit with surface_id, kind, summary, reproduction steps, expected behavior, and impact.'
    ],
    feedback: {
      surface_id: SURFACE_ID,
      tool: 'surface_feedback_submit',
      when: [
        'guidance is missing, stale, or contradicted by live behavior',
        'schema shape makes correct usage hard',
        'errors hide the actionable refusal or recovery path'
      ]
    },
    boundaries: [
      'Guidance is read-only model-facing operating advice.',
      'Guidance does not weaken policy, authorize mutation, or replace tool schemas.',
      'Carrier Session Authority owns admission; the Orientation Manifest package owns shared contracts and pure compilation.',
      'This surface owns only its bounded adapter, persistence/readback, diagnostics, and checkpoint operations.'
    ]
  };
}

export function guidanceToolDefinition(name: string = GUIDANCE_TOOL, description: string = 'Show model-facing operating guidance for ' + SURFACE_ID + ' MCP workflows.'): GuidanceToolDefinition {
  return {
    name,
    description,
    inputSchema: {
      type: 'object',
      properties: {
        workflow: { type: 'string', description: 'Optional workflow name or area to focus guidance on.' },
        tool: { type: 'string', description: 'Optional tool name for tool-specific guidance.' }
      },
      additionalProperties: false
    },
    annotations: { title: name, readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    outputSchema: { type: 'object', additionalProperties: true }
  };
}
