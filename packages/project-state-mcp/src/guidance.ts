import { compactGuidanceResult } from '@narada-core/mcp-fabric-contracts';

export type GuidanceRecord = Record<string, unknown>;
export type GuidanceToolDefinition = GuidanceRecord & {
  name: string;
  description: string;
  inputSchema: GuidanceRecord;
  annotations: GuidanceRecord;
  outputSchema: GuidanceRecord;
};

const SURFACE_ID = 'project-state';
const GUIDANCE_TOOL = 'project_state_guidance';
const PURPOSE = 'Read-only inspection of a site-owned virtual project-state registry backed by authored SQL and Node node:sqlite.';

export function buildGuidanceResult(args: GuidanceRecord = {}): GuidanceRecord {
  const workflow = typeof args.workflow === 'string' && args.workflow.trim() ? args.workflow.trim() : null;
  const tool = typeof args.tool === 'string' && args.tool.trim() ? args.tool.trim() : null;
  return compactGuidanceResult({
    schema: 'narada.mcp_surface.guidance.v0',
    status: 'ok',
    surface_id: SURFACE_ID,
    guidance_tool: GUIDANCE_TOOL,
    purpose: PURPOSE,
    requested: { workflow, tool },
    first_use: [
      'Call project_state_guidance when this read-only surface is unfamiliar or a diagnostic needs interpretation.',
      'Call project_state_doctor to check the site root, canonical SQL, viewer binding, and generated-output freshness.',
      'Use bounded program/project/matrix/gaps and standards/applicability/trace queries for orientation; project_state_validate is the complete virtual registry read.',
      'Treat structuredContent as authoritative; text content is a readable serialization of the same result.',
    ],
    tool_preference: [
      { step: 'orient', guidance: 'Use guidance, then doctor and command_map.' },
      { step: 'discover', guidance: 'Use program_list or project_list with an explicit program filter when useful.' },
      { step: 'inspect', guidance: 'Use program_show, project_show, matrix, gaps, standards_list, applicability, or standard_trace for a bounded target.' },
      { step: 'handoff', guidance: 'Use project_state_handoff for the auditable virtual-only release summary, evidence replay commands, deferred gates, and re-entry triggers.' },
      { step: 'trace', guidance: 'Use standard_show, standard_trace, and standard_gaps to connect an internal control to its object, lifecycle cell, evidence, review gate, and open gap.' },
      { step: 'verify', guidance: 'Use project_state_validate only when the complete virtual registry payload is needed.' },
    ],
    examples: [
      { intent: 'First use', call: 'project_state_guidance({})' },
      { intent: 'Check local posture', call: 'project_state_doctor({})' },
      { intent: 'Inspect lifecycle gaps', call: 'project_state_gaps({ program_id: "orbital_compute_infrastructure" })' },
      { intent: 'Read the virtual handoff', call: 'project_state_handoff({ project_id: "NRC600" })' },
      { intent: 'Inspect selected standards', call: 'project_state_standards_list({ selection: "core" })' },
      { intent: 'Trace a standard', call: 'project_state_standard_trace({ standard_id: "iso-15288-2023" })' },
    ],
    anti_patterns: [
      'Do not interpret virtual maturity as fabrication, metrology, external evidence, qualification, or flight credit.',
      'Do not pass a different project root or executable through tool arguments; the projection owns its site root and CLI path.',
      'Do not use this read-only surface to mutate SQL, generated files, suppliers, or physical systems.',
      'Do not infer missing lifecycle states from a filtered result; use project_state_validate when complete coverage matters.',
      'Do not interpret a virtually_supported standards mapping as ISO conformity, certification, qualification, or external approval.',
      'Do not treat a standard identifier or source link as permission to reproduce copyrighted standard text; the site stores bounded internal paraphrases only.',
    ],
    recovery: [
      'For project_state_cli_missing, build or restore the narada.space project at the configured site root.',
      'For stale generated outputs, run the project-owned project-state build command outside this read-only surface.',
      'For oversized output, narrow the query with a project, object, lifecycle, or program filter.',
      'For unknown ids, call the corresponding list tool and retry with an exact canonical id.',
    ],
    boundaries: [
      'Every tool is read-only and replayable.',
      'The site-owned SQL snapshot remains the authored authority.',
      'The surface is virtual-only and grants no physical, supplier, external-evidence, qualification, or flight credit.',
    ],
  });
}

export function guidanceToolDefinition(): GuidanceToolDefinition {
  return {
    name: GUIDANCE_TOOL,
    description: 'Show model-facing operating guidance for the read-only project-state MCP surface.',
    inputSchema: {
      type: 'object',
      properties: {
        workflow: { type: 'string', description: 'Optional workflow or area to focus guidance on.' },
        tool: { type: 'string', description: 'Optional tool name for tool-specific guidance.' },
      },
      additionalProperties: false,
    },
    annotations: { title: GUIDANCE_TOOL, readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    outputSchema: { type: 'object', additionalProperties: true },
  };
}
