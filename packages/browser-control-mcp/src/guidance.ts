import { compactGuidanceResult } from '@narada-core/mcp-fabric-contracts';

export type GuidanceRecord = Record<string, unknown>;

export type GuidanceToolDefinition = GuidanceRecord & {
  name: string;
  description: string;
  inputSchema: GuidanceRecord;
  annotations: GuidanceRecord;
  outputSchema: GuidanceRecord;
};

const SURFACE_ID = 'browser-control';
const GUIDANCE_TOOL = 'browser_control_guidance';

export function buildGuidanceResult(args: GuidanceRecord = {}): GuidanceRecord {
  const workflow = typeof args.workflow === 'string' && args.workflow.trim() ? args.workflow.trim() : null;
  const tool = typeof args.tool === 'string' && args.tool.trim() ? args.tool.trim() : null;
  return compactGuidanceResult({
    schema: 'narada.mcp_surface.guidance.v0',
    status: 'ok',
    surface_id: SURFACE_ID,
    guidance_tool: GUIDANCE_TOOL,
    purpose: 'Bounded host-level browser verification after an operator performs authentication.',
    requested: { workflow, tool },
    first_use: [
      'Perform or re-authenticate to the site interactively in the selected browser profile first.',
      'Call browser_control_attach with explicit profile_id, session_id, loopback cdp_endpoint, and exact allowed_origins.',
      'Call browser_control_status and browser_control_accessibility_snapshot before interacting.',
      'Use browser_control_navigate only within the exact origin allowlist.',
      'Use click, fill, and wait for bounded DOM actions; use confirmed:true for login, submit, or destructive intent.',
      'Use browser_control_screenshot or browser_control_assert for evidence; large results provide mcp_output_show refs.',
      'Call browser_control_detach when verification is complete.',
    ],
    tool_preference: [
      { step: 'orient', guidance: 'Use browser_control_guidance and browser_control_session_inventory first.' },
      { step: 'attach', guidance: 'Attach only an explicitly named profile and session; the surface never launches a browser.' },
      { step: 'observe', guidance: 'Prefer status, accessibility snapshots, screenshots, and assertions before mutations.' },
      { step: 'interact', guidance: 'Use selector-bounded click/fill/wait actions; never request arbitrary CDP or JavaScript.' },
      { step: 'verify', guidance: 'Record the returned action receipt and output ref for repeatable review.' },
    ],
    examples: [
      {
        intent: 'Attach',
        call: 'browser_control_attach({ profile_id: "operator", session_id: "tab-id", cdp_endpoint: "http://127.0.0.1:9222", allowed_origins: ["https://console.example.com"] })',
      },
      { intent: 'Snapshot', call: 'browser_control_accessibility_snapshot({ profile_id: "operator", session_id: "tab-id" })' },
      { intent: 'Assert', call: 'browser_control_assert({ profile_id: "operator", session_id: "tab-id", selector: "[data-testid=operator-console]", contains_text: "Ready" })' },
    ],
    anti_patterns: [
      'Do not provide cookies, headers, passwords, tokens, API keys, or authentication values.',
      'Do not use wildcard origins, arbitrary JavaScript, Runtime.evaluate, unrestricted CDP, or browser launch commands.',
      'Do not use this surface as a substitute for repeatable HTTP/API checks with server-side credentials.',
      'Do not perform login, submission, or destructive actions without explicit confirmed:true.',
    ],
    recovery: [
      'If the endpoint is unavailable, start or inspect the browser manually and retry the explicit attachment.',
      'If a target is missing, refresh session_inventory and use the exact target id; the surface never guesses.',
      'If an origin is refused, update the explicit allowlist rather than bypassing the check.',
      'If a sensitive field is refused, complete that step interactively as the operator; the surface never handles secrets.',
      'If a result is truncated, follow its mcp_output_show ref and next_offset.',
    ],
    boundaries: [
      'The browser, profile, authentication, and browser lifecycle remain host/operator-owned.',
      'This surface exposes only bounded DOM, accessibility, screenshot, and input primitives.',
      'The CDP endpoint must be loopback and is used only to connect to the selected target.',
    ],
  });
}

export function guidanceToolDefinition(
  name: string = GUIDANCE_TOOL,
  description = 'Show model-facing operating guidance for bounded browser-control MCP workflows.',
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
