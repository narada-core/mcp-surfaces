import { defineNativeSurface, type DefinedSurface, type McpToolDefinition } from '@narada-core/mcp-fabric-contracts';
import { listTools } from './main.js';

const READ_ONLY_TOOLS = ["structured_command_guidance","structured_command_execution_policy_inspect","structured_command_execution_show","structured_command_powershell_parse_check","structured_command_output_show"] as const;

export function surfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'structured-command',
    surface_version: '0.1.0',
    package: '@narada-core/structured-command-mcp',
    entrypoint: '{mcp_surfaces_root}/structured-command-mcp/dist/src/main.js',
    tools: listTools() as McpToolDefinition[],
    read_only_tools: READ_ONLY_TOOLS,
    default_effect: 'command',
    projections: [{
      id: 'default',
      transport: { kind: 'stdio', command: 'node', args: ["--allowed-root","{workspace_root}","--allow-command","node","--allow-command","pnpm","--allow-command","npm"], env: [] },
      injection_scope: 'local_site',
      default_injection: 'disabled',
      runtime_requirements: [],
      authority_requirements: ['scope.local_site'],
      lifecycle: { mode: 'replayable', reason: "Each command call is independently admitted; the stdio process has no replay-sensitive session state." },
    }],
  });
}
