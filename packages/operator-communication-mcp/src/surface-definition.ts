import { defineNativeSurface, type DefinedSurface, type McpToolDefinition } from '@narada-core/mcp-fabric-contracts';
import { listTools } from './main.js';

export function surfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'operator-communication',
    surface_version: '0.1.0',
    package: '@narada-core/operator-communication-mcp',
    entrypoint: '{mcp_surfaces_root}/operator-communication-mcp/dist/src/main.js',
    tools: listTools() as McpToolDefinition[],
    read_only_tools: ['operator_communication_guidance'],
    default_effect: 'local_write',
    projections: [{
      id: 'default',
      transport: { kind: 'stdio', command: 'node', args: ['--site-root', '{site_root}'], env: [] },
      injection_scope: 'local_site',
      default_injection: 'enabled',
      runtime_requirements: [],
      authority_requirements: ['scope.local_site'],
      lifecycle: { mode: 'replayable', reason: 'Each inline call appends one immutable Site-local response record by default; reference replay is read-only at call level.' },
    }],
  });
}
