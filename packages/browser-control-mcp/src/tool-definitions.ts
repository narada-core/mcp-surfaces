import { listOutputTools } from '@narada-core/mcp-transport';
import { guidanceToolDefinition } from './guidance.js';

export type JsonRecord = Record<string, unknown>;
export type ToolDefinition = {
  name: string;
  description: string;
  inputSchema: JsonRecord;
  annotations?: JsonRecord;
  outputSchema?: JsonRecord;
};

const READ_ONLY_ANNOTATIONS = {
  readOnlyHint: true,
  destructiveHint: false,
  idempotentHint: true,
  openWorldHint: false,
};

const ACTION_ANNOTATIONS = {
  readOnlyHint: false,
  destructiveHint: false,
  idempotentHint: true,
  openWorldHint: true,
};

const SESSION_PROPERTIES = {
  profile_id: { type: 'string', description: 'Explicit browser profile identifier.' },
  session_id: { type: 'string', description: 'Exact browser target/session identifier from the host.' },
};

function actionTool(name: string, description: string, properties: JsonRecord, required: string[], annotations: JsonRecord = ACTION_ANNOTATIONS): ToolDefinition {
  return {
    name,
    description,
    inputSchema: { type: 'object', properties, required, additionalProperties: false },
    annotations,
    outputSchema: { type: 'object', additionalProperties: true },
  };
}

export function listTools(): ToolDefinition[] {
  const tools: ToolDefinition[] = [
    guidanceToolDefinition() as ToolDefinition,
    actionTool('browser_control_session_inventory', 'List explicitly attached browser profile/session handles without opening or discovering browser targets.', {}, [], READ_ONLY_ANNOTATIONS),
    actionTool('browser_control_attach', 'Attach to an explicitly selected browser profile and target through a loopback CDP endpoint.', {
      profile_id: { type: 'string', description: 'Explicit operator/browser profile identifier.' },
      session_id: { type: 'string', description: 'Exact target id from the browser CDP target list.' },
      cdp_endpoint: { type: 'string', description: 'Loopback HTTP(S) CDP endpoint, for example http://127.0.0.1:9222.' },
      allowed_origins: { type: 'array', minItems: 1, maxItems: 32, items: { type: 'string' }, description: 'Exact non-wildcard HTTP(S) origins allowed for navigation.' },
    }, ['profile_id', 'session_id', 'cdp_endpoint', 'allowed_origins']),
    actionTool('browser_control_status', 'Refresh and return the status of one explicitly attached browser session.', SESSION_PROPERTIES, ['profile_id', 'session_id'], READ_ONLY_ANNOTATIONS),
    actionTool('browser_control_navigate', 'Navigate the selected browser session to a URL within its exact origin allowlist.', { ...SESSION_PROPERTIES, url: { type: 'string' } }, ['profile_id', 'session_id', 'url']),
    actionTool('browser_control_accessibility_snapshot', 'Return a bounded, offset-paged accessibility tree for the selected browser session.', { ...SESSION_PROPERTIES, max_nodes: { type: 'integer', minimum: 1, maximum: 500, default: 200 }, offset: { type: 'integer', minimum: 0, default: 0 } }, ['profile_id', 'session_id'], READ_ONLY_ANNOTATIONS),
    actionTool('browser_control_screenshot', 'Capture a bounded screenshot from the selected browser session; large images use mcp_output_show.', { ...SESSION_PROPERTIES, format: { type: 'string', enum: ['png', 'jpeg'], default: 'png' }, quality: { type: 'integer', minimum: 0, maximum: 100 } }, ['profile_id', 'session_id'], READ_ONLY_ANNOTATIONS),
    actionTool('browser_control_click', 'Click one CSS-selected element using bounded CDP DOM primitives.', { ...SESSION_PROPERTIES, selector: { type: 'string', maxLength: 512 }, intent: { type: 'string', enum: ['verify', 'login', 'submit', 'destructive'], default: 'verify' }, confirmed: { type: 'boolean', default: false } }, ['profile_id', 'session_id', 'selector']),
    actionTool('browser_control_fill', 'Fill a non-sensitive input or textarea; authentication and secret fields are always refused.', { ...SESSION_PROPERTIES, selector: { type: 'string', maxLength: 512 }, value: { type: 'string', maxLength: 4000 }, intent: { type: 'string', enum: ['verify', 'login', 'submit', 'destructive'], default: 'verify' }, confirmed: { type: 'boolean', default: false } }, ['profile_id', 'session_id', 'selector', 'value']),
    actionTool('browser_control_wait', 'Wait for a bounded duration or for a CSS-selected element to appear.', { ...SESSION_PROPERTIES, selector: { type: 'string', maxLength: 512 }, sleep_ms: { type: 'integer', minimum: 0, maximum: 15000, default: 250 }, timeout_ms: { type: 'integer', minimum: 1, maximum: 15000, default: 5000 } }, ['profile_id', 'session_id']),
    actionTool('browser_control_assert', 'Assert that a CSS-selected element exists and optionally contains bounded text.', { ...SESSION_PROPERTIES, selector: { type: 'string', maxLength: 512 }, contains_text: { type: 'string', maxLength: 4000 } }, ['profile_id', 'session_id', 'selector'], READ_ONLY_ANNOTATIONS),
    actionTool('browser_control_detach', 'Detach from one explicitly selected browser session without changing browser lifecycle.', SESSION_PROPERTIES, ['profile_id', 'session_id']),
  ];
  return [...tools, ...listOutputTools().map((tool) => ({ ...tool, annotations: READ_ONLY_ANNOTATIONS })) as ToolDefinition[]];
}
