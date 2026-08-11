import { defineNativeSurface, type DefinedSurface, type McpToolDefinition } from '@narada-core/mcp-fabric-contracts';
import { listTools } from './main.js';

const READ_ONLY_TOOLS = ["site_lifecycle_guidance","site_lifecycle_doctor","site_lifecycle_command_map","site_create_presets_list","site_create_plan","site_list","site_discover","site_show","site_doctor","site_verify_role","site_observe_runtime","site_lifecycle_kinds","site_lifecycle_preflight","site_relation_list","site_relation_validate","site_authority_preflight"] as const;

export function surfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'site-lifecycle',
    surface_version: '0.1.0',
    package: '@narada-core/site-lifecycle-mcp',
    entrypoint: '{mcp_surfaces_root}/site-lifecycle-mcp/dist/src/main.js',
    tools: listTools() as McpToolDefinition[],
    read_only_tools: READ_ONLY_TOOLS,
    default_effect: 'local_write',
    projections: [{
      id: 'default',
      transport: { kind: 'stdio', command: 'node', args: ["--narada-root","{site_root}"], env: [] },
      injection_scope: 'user_site',
      default_injection: 'enabled',
      runtime_requirements: [],
      authority_requirements: ['scope.user_site'],
      lifecycle: { mode: 'replayable', reason: "Site lifecycle mutations are persisted by Narada and the server has no session-pinned protocol." },
    }],
  });
}