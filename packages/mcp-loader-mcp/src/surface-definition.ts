import { defineNativeSurface, type DefinedSurface, type McpToolDefinition } from '@narada-core/mcp-fabric-contracts';
import { listTools } from './main.js';

const READ_ONLY_TOOLS = [
  'mcp_loader_guidance', 'mcp_loader_runtime_status', 'mcp_loader_policy_inspect',
  'mcp_loader_connection_inventory', 'mcp_loader_list_site_surfaces',
  'mcp_loader_site_fabric_diagnostics', 'mcp_loader_site_tool_inventory_check',
  'mcp_loader_list_tools', 'mcp_loader_surface_status',
  'mcp_loader_tool_discovery_manifest', 'mcp_loader_runtime_observation',
] as const;

export function surfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'mcp-loader',
    surface_version: '0.1.0',
    package: '@narada-core/mcp-loader-mcp',
    entrypoint: '{mcp_surfaces_root}/mcp-loader-mcp/dist/src/main.js',
    tools: listTools() as McpToolDefinition[],
    read_only_tools: READ_ONLY_TOOLS,
    default_effect: 'runtime_admin',
    projections: [{
      id: 'default',
      transport: {
        kind: 'stdio',
        command: 'node',
        args: [],
        env: [
          'NARADA_SRC_ROOT',
          'NARADA_AGENT_ID',
          'NARADA_SITE_ID',
          'NARADA_NARS_SESSION_ID',
          'NARADA_RUNTIME_SESSION_ID',
          'NARADA_CARRIER_SESSION_ID',
          'NARADA_SESSION_AUTHORITY_PRINCIPAL_KEY',
          'NARADA_SESSION_AUTHORITY_EPOCH',
          'NARADA_MCP_BINDING_ADMISSION_REQUIRED',
          'NARADA_MCP_BINDING_ADMISSION_PATH',
          'NARADA_MCP_BINDING_ADMISSION_DIGEST',
        ],
      },
      injection_scope: 'user_site',
      default_injection: 'enabled',
      runtime_requirements: [],
      authority_requirements: ['scope.user_site'],
      lifecycle: { mode: 'restart_required', restart_owner: 'user_site', reason: 'The loader owns attached child generations and must be restarted by its User Site supervisor.' },
    }],
  });
}
