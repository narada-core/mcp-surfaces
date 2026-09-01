import { compactGuidanceResult } from '@narada-core/mcp-fabric-contracts';

export type GuidanceRecord = Record<string, unknown>;

export type GuidanceToolDefinition = GuidanceRecord & {
  name: string;
  description: string;
  inputSchema: GuidanceRecord;
  annotations: GuidanceRecord;
  outputSchema: GuidanceRecord;
};

const SURFACE_ID = 'operator-console-overlay';
const GUIDANCE_TOOL = 'operator_console_overlay_guidance';
const PURPOSE = 'Host-level, governed lifecycle control for the Narada Operator Console Windows overlay.';

export function buildGuidanceResult(
  args: GuidanceRecord = {},
  state?: { naradaRoot: string; overlayEntrypoint: string },
): GuidanceRecord {
  const workflow = typeof args.workflow === 'string' && args.workflow.trim() ? args.workflow.trim() : null;
  const tool = typeof args.tool === 'string' && args.tool.trim() ? args.tool.trim() : null;
  return compactGuidanceResult({
    schema: 'narada.mcp_surface.guidance.v0',
    status: 'ok',
    surface_id: SURFACE_ID,
    guidance_tool: GUIDANCE_TOOL,
    purpose: PURPOSE,
    requested: { workflow, tool },
    configuration: state ? {
      narada_root: state.naradaRoot,
      canonical_overlay_entrypoint: state.overlayEntrypoint,
    } : null,
    first_use: [
      'Call operator_console_overlay_status to inspect the current overlay before changing it.',
      'Call operator_console_overlay_open to create or refresh the Narada Operator Console overlay.',
      'Follow the returned state and document paths when diagnosing a local overlay.',
      'Call operator_console_overlay_refresh after an external console URL or document change.',
      'Call operator_console_overlay_close to stop only the Operator Console overlay owned by this surface.',
      'Opening a local overlay establishes or verifies the Operator Console runtime before creating the window; use the returned diagnostics if readiness fails.',
      'For a bounded startup through mcp-loader, pass timeout_ms inside the nested tool arguments; the loader adds its bounded grace window.',
      'Use Narada console status/stop/restart commands when an explicit runtime lifecycle operation is needed; this MCP surface delegates rather than owning that authority.',
    ],
    tool_preference: [
      { step: 'orient', guidance: 'Use operator_console_overlay_guidance when the surface or recovery path is unfamiliar.' },
      { step: 'observe', guidance: 'Use operator_console_overlay_status before and after lifecycle operations.' },
      { step: 'manage', guidance: 'Use operator_console_overlay_open, operator_console_overlay_refresh, or operator_console_overlay_close.' },
      { step: 'verify', guidance: 'Confirm the returned overlay state and canonical document path.' },
    ],
    examples: [
      { intent: 'Inspect overlay', call: 'operator_console_overlay_status({})' },
      { intent: 'Open overlay', call: 'operator_console_overlay_open({})' },
      { intent: 'Open a known local console', call: 'operator_console_overlay_open({ url: "http://127.0.0.1:61729" })' },
      { intent: 'Close overlay', call: 'operator_console_overlay_close({})' },
    ],
    anti_patterns: [
      'Do not pass shell command strings or executable paths to this surface.',
      'Do not pass arbitrary process commands to this surface or treat it as a general process manager.',
      'Do not terminate arbitrary processes; close only the overlay identified by its canonical overlay id.',
      'Do not treat a stopped overlay as evidence that the Operator Router is stopped.',
    ],
    recovery: [
      'If operator_console_overlay_entrypoint_not_found is returned, verify NARADA_ROOT points to the Narada checkout and restart the MCP surface.',
      'If the overlay is stale, call operator_console_overlay_close and then operator_console_overlay_open.',
      'If local readiness fails, inspect the returned runtime log_path and state_path before retrying; do not create a second console process by hand.',
      'If the diagnostic reports corrupt router state, the canonical runtime quarantines the dead-owner state and retries once; preserve the recovery directory when submitting feedback.',
      'If the lifecycle result is unclear, inspect the returned state_directory and document_path rather than guessing.',
    ],
    boundaries: [
      'This surface owns only the Operator Console overlay projection.',
      'The canonical overlay implementation remains in Narada proper at packages/operator-console-overlay.',
      'The surface launches only that fixed entrypoint with a bounded argument set.',
      'The Operator Console runtime owns local Router/Console readiness and lifecycle; the Router, console backend, browser, and agent sessions retain their respective domain boundaries.',
    ],
  });
}

export function guidanceToolDefinition(
  name: string = GUIDANCE_TOOL,
  description = 'Show model-facing operating guidance for the Narada Operator Console overlay MCP surface.',
): GuidanceToolDefinition {
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
