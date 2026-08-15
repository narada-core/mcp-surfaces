import { defineNativeSurface, type DefinedSurface, type McpToolDefinition } from '@narada-core/mcp-fabric-contracts';
import { listTools } from './git-tool-list.js';

const READ_ONLY_TOOLS = ["git_guidance","git_policy_inspect","git_status","git_branch_list","git_worktree_list","git_output_show","git_changed_summary","git_repositories_summary","git_diff","git_log","git_show"] as const;

export function surfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'git',
    surface_version: '0.1.0',
    package: '@narada-core/git-mcp',
    entrypoint: '{mcp_surfaces_root}/git-mcp/dist/src/main.js',
    tools: listTools('write') as McpToolDefinition[],
    read_only_tools: READ_ONLY_TOOLS,
    default_effect: 'local_write',
    projections: [{
      id: 'default',
      transport: { kind: 'stdio', command: 'node', args: ["--allowed-root","{workspace_root}","--output-root","{site_root}","--mode","write"], env: [] },
      injection_scope: 'local_site',
      default_injection: 'enabled',
      runtime_requirements: [],
      authority_requirements: ['scope.local_site'],
      lifecycle: { mode: 'replayable', reason: "Git state is external to the process and every operation carries its own repository context." },
    }],
  });
}
