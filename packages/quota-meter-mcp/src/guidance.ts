import { compactGuidanceResult } from '@narada-core/mcp-fabric-contracts';

export type GuidanceRecord = Record<string, unknown>;
export type GuidanceToolDefinition = GuidanceRecord & {
  name: string;
  description: string;
  inputSchema: GuidanceRecord;
  annotations: GuidanceRecord;
  outputSchema: GuidanceRecord;
};

const SURFACE_ID = 'quota-meter';
const GUIDANCE_TOOL = 'quota_meter_guidance';
const PURPOSE = 'Host-level quota-meter glide status and transparent overlay lifecycle management for Codex and Kimi Code.';

export function buildGuidanceResult(args: GuidanceRecord = {}, state?: { quotaMeterRoot: string; stateRoot: string }): GuidanceRecord {
  const workflow = typeof args.workflow === 'string' && args.workflow.trim() ? args.workflow.trim() : null;
  const tool = typeof args.tool === 'string' && args.tool.trim() ? args.tool.trim() : null;
  return compactGuidanceResult({
    schema: 'narada.mcp_surface.guidance.v0',
    status: 'ok',
    surface_id: SURFACE_ID,
    guidance_tool: GUIDANCE_TOOL,
    purpose: PURPOSE,
    requested: { workflow, tool },
    configuration: state ? { quota_meter_root: state.quotaMeterRoot, state_root: state.stateRoot } : null,
    first_use: [
      'Call quota_meter_guidance when the surface is unfamiliar or an error needs recovery guidance.',
      'Call quota_meter_glide_status for current provider windows and glide factors; it never launches login.',
      'Call quota_meter_overlay_start with an explicit provider selection and refresh interval to show the desktop monitor.',
      'Call quota_meter_overlay_status to verify the PID, saved position, and running state after starting.',
      'Call quota_meter_overlay_stop or click the overlay’s faint × button to close it.',
      'Run codex login or kimi login through the native CLI when credentials are expired; this surface never handles tokens.',
    ],
    tool_preference: [
      { step: 'orient', guidance: 'Use quota_meter_guidance first when uncertain.' },
      { step: 'observe', guidance: 'Use quota_meter_glide_status for usage and quota evidence.' },
      { step: 'manage', guidance: 'Use quota_meter_overlay_start or quota_meter_overlay_stop for lifecycle changes.' },
      { step: 'verify', guidance: 'Read quota_meter_overlay_status after lifecycle changes.' },
    ],
    examples: [
      { intent: 'Current factors', call: 'quota_meter_glide_status({ providers: "all" })' },
      { intent: 'Start monitor', call: 'quota_meter_overlay_start({ providers: "codex,kimi", refresh_seconds: 60 })' },
      { intent: 'Stop monitor', call: 'quota_meter_overlay_stop({})' },
    ],
    anti_patterns: [
      'Do not pass shell command strings; use the explicit tool arguments.',
      'Do not infer authentication from overlay text when quota_meter_glide_status can provide provider status.',
      'Do not bypass the surface with arbitrary process termination; stop only the quota-meter-owned overlay.',
      'Do not treat provider access tokens or credential files as surface output.',
    ],
    recovery: [
      'If quota_meter_cli_not_found is returned, set QUOTA_METER_ROOT to the quota-meter checkout and restart the surface.',
      'If a provider is auth_required, run that provider’s native login command and wait for the next overlay refresh.',
      'If the overlay is stale, call quota_meter_overlay_stop, then quota_meter_overlay_start.',
      'If behavior or guidance is unclear, submit feedback through surface_feedback_submit.',
    ],
    boundaries: [
      'This surface manages only quota-meter and its own overlay PID/state files.',
      'Provider authentication, token refresh, and quota interpretation remain owned by quota-meter and native provider CLIs.',
      'The surface does not provide arbitrary shell, filesystem, or process-control access.',
    ],
  });
}

export function guidanceToolDefinition(name: string = GUIDANCE_TOOL, description = 'Show model-facing operating guidance for quota-meter MCP workflows.'): GuidanceToolDefinition {
  return {
    name,
    description,
    inputSchema: {
      type: 'object',
      properties: {
        workflow: { type: 'string', description: 'Optional workflow name or area to focus guidance on.' },
        tool: { type: 'string', description: 'Optional tool name for tool-specific guidance.' },
      },
      additionalProperties: false,
    },
    annotations: { title: name, readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    outputSchema: { type: 'object', additionalProperties: true },
  };
}
