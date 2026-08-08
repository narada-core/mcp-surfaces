import { defineNativeSurface, type DefinedSurface, type McpToolDefinition } from '@narada-core/mcp-fabric-contracts';
import { listTools } from './main.js';

const READ_ONLY_TOOLS = [
  'project_state_guidance',
  'project_state_doctor',
  'project_state_command_map',
  'project_state_program_list',
  'project_state_program_show',
  'project_state_project_list',
  'project_state_project_show',
  'project_state_matrix',
  'project_state_gaps',
  'project_state_handoff',
  'project_state_standards_list',
  'project_state_standard_show',
  'project_state_applicability',
  'project_state_standard_trace',
  'project_state_standard_gaps',
  'project_state_validate',
] as const;

export function surfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'project-state',
    surface_version: '0.2.0',
    package: '@narada-core/project-state-mcp',
    entrypoint: '{mcp_surfaces_root}/project-state-mcp/dist/src/main.js',
    tools: listTools() as McpToolDefinition[],
    read_only_tools: READ_ONLY_TOOLS,
    default_effect: 'read',
    projections: [{
      id: 'default',
      transport: { kind: 'stdio', command: 'node', args: ['--project-root', '{site_root}'], env: [] },
      injection_scope: 'local_site',
      default_injection: 'disabled',
      runtime_requirements: [],
      authority_requirements: ['scope.local_site'],
      lifecycle: {
        mode: 'replayable',
        reason: 'Project-state queries rebuild and validate from the canonical SQL snapshot and hold no session state.',
      },
    }],
  });
}
