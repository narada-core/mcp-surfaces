import { defineNativeSurface, type DefinedSurface, type McpToolDefinition } from '@narada-core/mcp-fabric-contracts';
import { nativeStructuredCommandTools } from './native-tool-catalog.js';

const READ_ONLY_TOOLS = ["structured_command_guidance","structured_command_execution_policy_inspect","structured_command_execution_show","structured_command_powershell_parse_check","structured_command_output_show"] as const;
const nativeExecutable = `narada-structured-command-mcp${process.platform === 'win32' ? '.exe' : ''}`;

export function surfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'structured-command',
    surface_version: '0.1.0',
    package: '@narada-core/structured-command-mcp',
    entrypoint: `{mcp_surfaces_root}/structured-command-mcp/dist/native/${nativeExecutable}`,
    tools: nativeStructuredCommandTools() as McpToolDefinition[],
    read_only_tools: READ_ONLY_TOOLS,
    default_effect: 'command',
    projections: [{
      id: 'default',
      transport: { kind: 'stdio', command: 'narada-structured-command-mcp', args: ["--allowed-root","{workspace_root}","--allow-command","node","--allow-command","pnpm","--allow-command","npm","--allow-command","python","--allow-prefix","uv run --with sympy python"], env: [] },
      injection_scope: 'local_site',
      default_injection: 'disabled',
      runtime_requirements: [],
      authority_requirements: ['scope.local_site'],
      lifecycle: { mode: 'replayable', reason: "Each command call is independently admitted; the stdio process has no replay-sensitive session state." },
    }],
  });
}
