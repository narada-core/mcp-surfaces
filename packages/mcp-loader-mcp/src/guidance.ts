import { loaderRuntimeLifecycle } from './runtime-lifecycle.js';
import {
  DEFAULT_TOOL_CALL_TIMEOUT_MS,
  DEFAULT_TOOL_TIMEOUT_GRACE_MS,
  MAX_TOOL_CALL_TIMEOUT_MS,
  MAX_TOOL_TIMEOUT_GRACE_MS,
} from './tool-timeout.js';

export type GuidanceRecord = Record<string, unknown>;
export type GuidanceToolDefinition = GuidanceRecord & {
  name: string;
  description: string;
  inputSchema: GuidanceRecord;
  annotations: GuidanceRecord;
  outputSchema: GuidanceRecord;
};

const SURFACE_ID = 'mcp-loader';
const GUIDANCE_TOOL = 'mcp_loader_guidance';
const PURPOSE = 'Policy-gated runtime attachment and proxying for MCP surfaces admitted by a Site fabric.';

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
    runtime_lifecycle: loaderRuntimeLifecycle(),
    tool_call_timeout: {
      tool: 'mcp_loader_call_tool',
      nested_argument: 'arguments.timeout_ms',
      policy_default_ms: DEFAULT_TOOL_CALL_TIMEOUT_MS,
      request_max_ms: MAX_TOOL_CALL_TIMEOUT_MS,
      grace_flag: '--tool-timeout-grace-ms',
      default_grace_ms: DEFAULT_TOOL_TIMEOUT_GRACE_MS,
      grace_max_ms: MAX_TOOL_TIMEOUT_GRACE_MS,
      semantics: 'When nested timeout_ms is present, it is forwarded to the child and the loader waits timeout_ms plus bounded grace for the child timeout result. When absent, the loader policy default is the outer deadline and no grace is added.',
    },
    first_use: [
      'Call mcp_loader_policy_inspect before relying on loader capabilities or allowed roots.',
      'Call mcp_loader_connection_inventory before attachment when recovering from capacity errors or an earlier interrupted session.',
      'Call mcp_loader_process_ownership when reconciling child processes after an interrupted attach; it reports only this loader run\'s direct children and safe known-connection cleanup actions.',
      'Call mcp_loader_list_site_surfaces and mcp_loader_site_fabric_diagnostics for the explicit Site root.',
      'Use mcp_loader_attach_surface with an explicit surface_id and runtime_kind when the projection requires one.',
      'Inspect surface_projection.execution before attachment. mcp-loader accepts stdio projections only; surface_factory projections belong to the PC Site surface runtime.',
      'Use mcp_loader_list_tools or mcp_loader_tool_discovery_manifest after attachment; the child tools/list response owns exact tool schemas.',
      'Call mcp_loader_runtime_observation with connection_id and carrier_kind to obtain the V2 normalized observation; it reports the stable logical connection id, generation, lifecycle, digests, and bounded recovery actuator.',
      'For mcp_loader_call_tool, place timeout_ms inside the nested arguments object. The loader forwards it to the child and adds bounded outer grace so the child can return its own timeout result.',
      'Call mcp_loader_runtime_status when the loader process may have out-of-date source, dependency, or build-configuration evidence; inspect runtime_freshness.reload_action for the descriptive carrier/runtime-supervisor capability. Its agent_callable field is false because mcp-loader does not expose that actuator itself.',
      'Preserve structuredContent as authoritative evidence; text content is for assistant readability.',
    ],
    tool_preference: [
      { step: 'orient', guidance: 'Use mcp_loader_guidance, mcp_loader_runtime_status, and mcp_loader_policy_inspect before attachment or proxy calls.' },
      { step: 'recover', guidance: 'For a stale or transport-closed child, inspect mcp_loader_connection_inventory or mcp_loader_surface_status, then call mcp_loader_surface_restart with the connection_id; the agent session does not need to restart.' },
      { step: 'reconcile_processes', guidance: 'Use mcp_loader_process_ownership to distinguish loader-owned direct children from unobserved host processes. Only known loader-owned connections may be detached; conhost or unrelated host processes require the external host/runtime supervisor.' },
      { step: 'resolve_site', guidance: 'Use mcp_loader_list_site_surfaces and mcp_loader_site_fabric_diagnostics against the same explicit Site root.' },
      { step: 'attach', guidance: 'Use mcp_loader_open_surface when repeated calls should survive a loader-managed child restart; it returns a stable surface_handle scoped to this loader process. Use mcp_loader_attach_surface when a one-generation connection id is sufficient.' },
      { step: 'discover', guidance: 'Use mcp_loader_list_tools or mcp_loader_tool_discovery_manifest; use the child tools/list definitions for exact input and output shape.' },
      { step: 'observe_live', guidance: 'Use mcp_loader_site_tool_inventory_check to compare declared tools with fresh child tools/list responses. Read the compact per-finding status and missing/extra/unclassified tool lists first; retain the immutable observation_ref for full evidence. A skipped runtime-affined surface makes the aggregate status partial, so rerun with the required runtime_kind for complete coverage.' },
      { step: 'observe_runtime', guidance: 'Call mcp_loader_runtime_observation({ connection_id, carrier_kind }) after attachment. Treat its structuredContent as the V2 runtime observation; runtime_state_root is null because the loader reports but does not own durable observation persistence.' },
      { step: 'operate', guidance: 'Call a child tool only after selecting the intended connection and honoring the child surface policy. For bounded calls, pass arguments.timeout_ms; the loader outer deadline is that child timeout plus --tool-timeout-grace-ms.' },
      { step: 'finish', guidance: 'Use mcp_loader_detach or mcp_loader_surface_restart deliberately and inspect the returned termination or replacement evidence; when mcp_loader_runtime_status reports stale, invoke the separately exposed carrier/runtime-supervisor capability named by runtime_freshness.reload_action.capability rather than treating its next_call descriptor as a loader tool.' },
    ],
    examples: [
      { intent: 'First use', call: 'mcp_loader_guidance({})' },
      { intent: 'Inspect a workflow', call: 'mcp_loader_guidance({ workflow: "discover", tool: "mcp_loader_list_tools" })' },
      { intent: 'Recover capacity', call: 'mcp_loader_connection_inventory({})' },
      { intent: 'Reconcile loader children', call: 'mcp_loader_process_ownership({})' },
      { intent: 'Inspect a Site', call: 'mcp_loader_list_site_surfaces({ site_root: "<site_root>" })' },
      { intent: 'Inspect loader freshness', call: 'mcp_loader_runtime_status({})' },
      { intent: 'Observe live tools', call: 'mcp_loader_site_tool_inventory_check({ site_root: "<site_root>", runtime_kind: "<runtime_kind>" })' },
      { intent: 'Observe a generation', call: 'mcp_loader_runtime_observation({ connection_id: "<connection_id>", carrier_kind: "codex" })' },
      { intent: 'Call with a bounded child timeout', call: 'mcp_loader_call_tool({ connection_id: "<connection_id>", tool_name: "<tool_name>", arguments: { timeout_ms: 120000 } })' },
    ],
    anti_patterns: [
      'Do not infer a Site or runtime from the current directory, process name, server name, or entrypoint path.',
      'Do not attach an undeclared surface or use an entrypoint outside the allowed policy prefixes.',
      'Do not reinterpret a surface_factory projection as stdio or treat loader approval as admission for nested mutating tools.',
      'Do not copy child inputSchema or outputSchema into loader guidance; read the current child tools/list response instead.',
      'Do not treat loader attachment as authorization for the child surface domain; the attached surface remains authoritative.',
      'Do not copy or hand-build observation maps; pass the immutable observation_ref returned by mcp_loader_site_tool_inventory_check.',
      'Do not bypass the owning surface with shell scripts when a governed MCP tool exists.',
      'Do not add a progressive surface by editing a carrier config paired with .narada-generation.json. Use mcp_loader_open_surface for runtime attachment; direct edits deliberately invalidate every bootstrap projection sharing that generation.',
      'Do not treat mcp_loader_surface_restart as a loader hot-reload; it replaces only the selected attached child process.',
      'Do not enumerate or terminate arbitrary host processes, conhost descendants, or processes lacking this loader run\'s ownership marker.',
      'Do not apply a reconciliation plan from an old observation; use its expected observation digest and required authority before invoking the named actuator.',
    ],
    recovery: [
      'For unknown_tool, call tools/list and mcp_loader_guidance again after restart.',
      'For surface_runtime_required or surface_runtime_not_supported, inspect the declared projection and retry only with an explicit compatible runtime_kind.',
      'For surface_execution_adapter_not_supported_by_loader, route the admitted binding through the PC Site surface runtime; changing the adapter locally would violate the descriptor.',
      'For fabric drift, repair the owning Site fabric or shared registry declaration before retrying attachment.',
      'For missing or stale live observations, rerun mcp_loader_site_tool_inventory_check and pass its new observation_ref to the Registrar.',
      'For child failures, inspect mcp_loader_surface_status and stderr evidence, then use mcp_loader_surface_restart({ connection_id, reason }) when the attached child should be replaced.',
      'For child_exited caused by a PATH-relative node or bun command in registrar-owned Site fabric, rebuild and run all-carrier materialization so the Registrar refreshes generated Site projections with absolute executable paths. Do not copy the surface into the carrier config.',
      'For a replayable generation, invoke mcp_loader_surface_restart with the connection_id. For session_pinned or restart_required lifecycle, the observation names carrier-supervisor as the actuator; mcp-loader marks that capability agent_callable=false, so invoke restart_mcp_loader_process only if the carrier supervisor separately exposes it before reconnecting.',
      'For max_connections_reached, call mcp_loader_connection_inventory, detach stale or closed connection ids, and retry only after capacity is available.',
      'For interrupted child cleanup, call mcp_loader_process_ownership and reconcile only the listed loader-owned connection ids; external or unobserved process cleanup belongs to the host/runtime supervisor.',
      'For stale loader runtime, call mcp_loader_runtime_status and use reload_action.capability as a request to the carrier or runtime supervisor; mcp-loader itself cannot invoke it and child restart cannot reload the loader.',
      'For unclear behavior, submit surface_feedback_submit with reproduction steps, expected behavior, and impact.',
    ],
    feedback: {
      surface_id: SURFACE_ID,
      tool: 'surface_feedback_submit',
      when: [
        'guidance is missing, stale, or contradicted by live loader behavior',
        'schema shape makes correct usage hard',
        'errors hide the actionable refusal or recovery path',
      ],
    },
    boundaries: [
      'MCP Loader owns child attachment, initialization, tool discovery, call proxying, and detachment.',
      'MCP Loader does not own attached-surface domain policy, action admission, or child tool semantics.',
      'MCP Loader is the stdio compatibility adapter. It does not host surface factories or own authority-shared instances.',
      'The loader binds children to the requested Site root and does not let an ambient caller Site root override it.',
      'Process ownership is limited to direct children spawned by this loader run; the loader does not own arbitrary host process or conhost enumeration.',
      'Guidance is read-only model-facing operating advice and does not replace tool schemas or policy checks.',
    ],
  };
}

export function guidanceToolDefinition(
  name: string = GUIDANCE_TOOL,
  description: string = 'Show model-facing operating guidance for ' + SURFACE_ID + ' MCP workflows.',
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
