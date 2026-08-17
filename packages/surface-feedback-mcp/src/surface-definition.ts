import { defineNativeSurface, type DefinedSurface, type McpToolDefinition } from '@narada-core/mcp-fabric-contracts';
import { listTools } from './main.js';

const READ_ONLY_TOOLS = ["surface_feedback_guidance","surface_feedback_doctor","surface_feedback_list","surface_feedback_actionable_queue","surface_feedback_show","surface_feedback_stats","surface_feedback_live_proof_template"] as const;

export function surfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'surface-feedback',
    surface_version: '0.2.0',
    package: '@narada-core/surface-feedback-mcp',
    entrypoint: '{mcp_surfaces_root}/surface-feedback-mcp/dist/src/main.js',
    tools: listTools() as McpToolDefinition[],
    read_only_tools: READ_ONLY_TOOLS,
    default_effect: 'local_write',
    projections: [{
      id: 'default',
      transport: {
        kind: 'stdio',
        command: 'node',
        args: [
          '--feedback-root', '{site_control_root}/feedback',
          '--canonical-feedback-root', '{site_control_root}/feedback',
          '--task-lifecycle-root', '{site_root}',
          '--site-id', '{site_id}',
          '--owned-surface-id', '*',
        ],
        env: ['NARADA_SURFACE_FEEDBACK_ROOT'],
      },
      injection_scope: 'user_site',
      default_injection: 'disabled',
      runtime_requirements: [],
      authority_requirements: ['scope.user_site'],
      lifecycle: { mode: 'replayable', reason: "Feedback records are durable and each request is independently scoped by User Site authority." },
    }],
  });
}
