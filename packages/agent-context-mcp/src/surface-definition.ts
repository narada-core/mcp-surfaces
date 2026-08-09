import { defineNativeSurface, type DefinedSurface, type McpToolDefinition } from '@narada-core/mcp-fabric-contracts';
import { listTools } from './main.js';

const READ_ONLY_TOOLS = [
  'agent_context_guidance', 'agent_context_doctor', 'agent_context_whoami',
  'agent_context_continuation_read',
  'agent_context_rehydrate', 'agent_context_hydrate_current',
  'agent_context_startup_sequence', 'agent_context_list_sessions',
  'mcp_output_show',
] as const;

const OCCUPANT_TOOLS = listTools('occupant') as McpToolDefinition[];
const ADMIN_TOOLS = listTools('admin') as McpToolDefinition[];
const FORWARDED_ENV = [
  'NARADA_AGENT_CONTEXT_DB',
  'NARADA_AGENT_ID',
  'NARADA_AGENT_START_EVENT_ID',
  'NARADA_CARRIER_SESSION_ACTIVATION_RECEIPT',
  'NARADA_CARRIER_SESSION_ADMISSION_RECEIPT',
  'NARADA_CARRIER_SESSION_ID',
  'NARADA_ORIENTATION_BRIEF',
  'NARADA_ORIENTATION_DELIVERY_RECEIPT',
  'NARADA_ORIENTATION_ENTRY_FILE',
  'NARADA_ORIENTATION_MANIFEST_ID',
  'NARADA_SITE_ID',
  'NARADA_SITE_ROOT',
];

export function surfaceDefinition(): DefinedSurface {
  return defineNativeSurface({
    surface_id: 'agent-context',
    surface_version: '0.1.0',
    package: '@narada-core/agent-context-mcp',
    entrypoint: '{mcp_surfaces_root}/agent-context-mcp/dist/src/main.js',
    tools: ADMIN_TOOLS,
    read_only_tools: READ_ONLY_TOOLS,
    default_effect: 'local_write',
    projections: [{
      id: 'default',
      exposed_tools: OCCUPANT_TOOLS.map((tool) => tool.name),
      transport: {
        kind: 'stdio',
        command: 'bun',
        args: [
          '--site-root', '{site_root}',
          '--site-id', '{site_id}',
          '--tool-projection', 'occupant',
        ],
        env: FORWARDED_ENV,
      },
      injection_scope: 'local_site',
      default_injection: 'enabled',
      runtime_requirements: [],
      authority_requirements: ['scope.local_site'],
      lifecycle: { mode: 'restart_required', restart_owner: 'local_site', reason: 'The stdio transport binding is process-scoped; durable checkpoints and immutable manifest generations remain Site-local and are read back after reconnect.' },
    }, {
      id: 'admin',
      exposed_tools: ADMIN_TOOLS.map((tool) => tool.name),
      transport: {
        kind: 'stdio',
        command: 'bun',
        args: [
          '--site-root', '{site_root}',
          '--site-id', '{site_id}',
          '--tool-projection', 'admin',
        ],
        env: FORWARDED_ENV,
      },
      injection_scope: 'local_site',
      default_injection: 'disabled',
      runtime_requirements: [],
      authority_requirements: ['scope.local_site', 'role.operator'],
      lifecycle: { mode: 'restart_required', restart_owner: 'local_site', reason: 'Administrative compatibility tools are exposed only by an explicit projection; the process-scoped transport requires restart after rebinding.' },
    }],
  });
}
