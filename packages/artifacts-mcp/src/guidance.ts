import { compactGuidanceResult } from '@narada-core/mcp-fabric-contracts';

export type GuidanceRecord = Record<string, unknown>;
export type GuidanceToolDefinition = GuidanceRecord & { name: string; description: string; inputSchema: GuidanceRecord; annotations: GuidanceRecord; outputSchema: GuidanceRecord };

const SURFACE_ID = 'artifacts';
const GUIDANCE_TOOL = 'artifacts_guidance';
const PURPOSE = 'Register NARS session artifacts and present renderable artifact_ref messages in operator projections.';

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
      'Call artifacts_doctor first when unsure whether this session has a NARS artifact endpoint.',
      'Use artifact_register_file for a local file that should become visible in agent-web-ui or another NARS projection.',
      'Use artifact_present after registration when the operator should see the artifact inline; NARS emits the structured assistant_message event.',
      'Use artifact_list or artifact_read after registration when verification requires current NARS state.'
    ],
    tool_preference: [
      { step: 'orient', guidance: 'artifacts_doctor reports NARS endpoint, session id, transport, and whether registration is configured.' },
      { step: 'register', guidance: 'artifact_register_file posts a source path to NARS and returns artifact metadata plus a structured artifact_ref message part.' },
      { step: 'present', guidance: 'artifact_present asks NARS to append and broadcast an assistant_message event with structured artifact_ref content.' },
      { step: 'verify', guidance: 'artifact_read confirms one artifact; artifact_list confirms the session artifact index.' }
    ],
    examples: [
      { intent: 'First use', call: 'artifacts_guidance({})' },
      { intent: 'Check endpoint', call: 'artifacts_doctor({})' },
      { intent: 'Register local HTML report', call: "artifact_register_file({ path: \"C:/workspace/site/.ai/report.html\", kind: \"html\", title: \"Report\", render_hint: \"inline\" })" },
      { intent: 'Show registered artifact inline', call: 'artifact_present({ artifact_id: result.artifact.artifact_id, text: \"Here is the report.\" })' }
    ],
    anti_patterns: [
      'Do not paste raw HTML into chat when a file artifact can be registered.',
      'Do not invent artifact ids or content URLs; use NARS returned artifact metadata and message_part.',
      'Do not use filesystem or shell as an artifact registry when NARS is available.',
      'Do not register secrets, credentials, private cookies, or broad workspace dumps as operator-visible artifacts.'
    ],
    recovery: [
      'If artifact_register_file returns nars_endpoint_missing, inspect launch/session configuration and restart with a NARS artifact endpoint exposed to MCP.',
      'If NARS refuses the source path, move or generate the artifact under an admitted site/session root and retry.',
      'If agent-web-ui cannot render the presented artifact, verify the UI is attached to the same NARS session and read artifact metadata with artifact_read.',
      'If only an artifact id is known, prefer artifact_read before artifact_message_part_create so the emitted artifact_ref is verified against NARS.',
      'For unclear or stale behavior, submit surface_feedback_submit with surface_id artifacts and exact tool/result details.'
    ],
    boundaries: [
      'This surface is a NARS artifact client, not a filesystem reader, writer, or second artifact store.',
      'NARS remains authoritative for source path admission, artifact ids, content-type policy, metadata, and serving.',
      'This surface can ask NARS to present an artifact, but NARS remains the event emitter and projection authority.'
    ]
  });
}

export function guidanceToolDefinition(name: string = GUIDANCE_TOOL, description: string = 'Show model-facing operating guidance for artifacts MCP workflows.'): GuidanceToolDefinition {
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
