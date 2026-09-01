import { defineNativeSurface, type DefinedSurface, type McpToolDefinition } from '@narada-core/mcp-fabric-contracts';
import { nativeFilesystemTools } from './native-tool-catalog.js';

const READ_ONLY_TOOLS = ["fs_guidance","fs_read_file","fs_read_file_range","fs_stat","fs_glob_search","fs_search","fs_search_results_read","fs_grep_search","fs_repository_inventory","fs_file_metrics","fs_doctor"] as const;
const nativeExecutable = `narada-local-filesystem-mcp${process.platform === 'win32' ? '.exe' : ''}`;

export function surfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'local-filesystem',
    surface_version: '0.1.0',
    package: '@narada-core/local-filesystem-mcp',
    entrypoint: `{mcp_surfaces_root}/local-filesystem-mcp/dist/native/${nativeExecutable}`,
    tools: nativeFilesystemTools('write') as McpToolDefinition[],
    read_only_tools: READ_ONLY_TOOLS,
    default_effect: 'local_write',
    projections: [{
      id: 'default',
      transport: { kind: 'stdio', command: 'narada-local-filesystem-mcp', args: ["--mode","write","--allowed-root","{workspace_root}","--anchored-allowed-root","user_home:.codex","--output-root","{site_root}"], env: [] },
      injection_scope: 'local_site',
      default_injection: 'disabled',
      runtime_requirements: [],
      authority_requirements: ['scope.local_site'],
      lifecycle: { mode: 'replayable', reason: "The governed filesystem process has no session-pinned protocol state; replacement replays initialization safely." },
    }],
  });
}
