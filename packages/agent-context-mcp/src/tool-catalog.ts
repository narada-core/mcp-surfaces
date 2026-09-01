import { guidanceToolDefinition } from './guidance.js';
import {
  CARRIER_SESSION_ACTIVATION_RECEIPT_JSON_SCHEMA,
  CARRIER_SESSION_ADMISSION_RECEIPT_JSON_SCHEMA,
  ORIENTATION_ACKNOWLEDGEMENT_JSON_SCHEMA,
  ORIENTATION_BRIEF_JSON_SCHEMA,
  ORIENTATION_MANIFEST_REFERENCE_JSON_SCHEMA,
} from '@narada-core/orientation-manifest';

const LOCAL_WRITE_TOOLS = new Set([
  'agent_orientation_read',
  'agent_orientation_acknowledge',
  'agent_context_start_session',
  'agent_context_checkpoint',
  'agent_context_continuation_export',
]);

export const ORIENTATION_REQUIRED_READ_PROGRESS_JSON_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    total: { type: 'integer', minimum: 0 },
    completed: { type: 'integer', minimum: 0 },
    pending: { type: 'integer', minimum: 0 },
    completed_step_ids: {
      type: 'array',
      items: { type: 'string', minLength: 1 },
    },
    pending_step_ids: {
      type: 'array',
      items: { type: 'string', minLength: 1 },
    },
    completion_refs: {
      type: 'array',
      items: { type: 'string', minLength: 1 },
    },
    active_step_id: {
      anyOf: [
        { type: 'string', minLength: 1 },
        { type: 'null' },
      ],
    },
    next_byte_offset: {
      anyOf: [
        { type: 'integer', minimum: 0 },
        { type: 'null' },
      ],
    },
  },
  required: [
    'total', 'completed', 'pending', 'completed_step_ids',
    'pending_step_ids', 'completion_refs', 'active_step_id',
    'next_byte_offset',
  ],
} as const;

export const ORIENTATION_NEXT_CALL_JSON_SCHEMA = {
  anyOf: [{
    type: 'object',
    additionalProperties: false,
    properties: {
      tool: { type: 'string', minLength: 1 },
      arguments: { type: 'object', additionalProperties: true },
    },
    required: ['tool', 'arguments'],
  }, {
    type: 'null',
  }],
} as const;

export const TOOLS = [
  {
    name: 'agent_orientation_read',
    description: 'Receive the exact Carrier-entry Orientation Brief and its next_call. Call with no arguments first, then follow next_call exactly; required-read pages persist server-owned completion evidence and selections inspect exact entry snapshots.',
    inputSchema: {
      type: 'object',
      properties: {
        step_id: {
          type: 'string',
          minLength: 1,
          description: 'Optional exact pending step_id returned by required_read_progress or next_call.',
        },
        offset: {
          type: 'integer',
          minimum: 0,
          description: 'Exact UTF-8 byte offset returned by the preceding next_call. Use 0 for the first page.',
        },
        selection: {
          type: 'string',
          enum: ['continuity', 'work'],
          description: 'Inspect the exact continuity or work artifact selected by this admitted manifest.',
        },
      },
      additionalProperties: false,
    },
    outputSchema: {
      anyOf: [{
        type: 'object',
        additionalProperties: false,
        properties: {
          schema: { type: 'string', const: 'narada.agent_context.orientation_entry_packet.v2' },
          status: { type: 'string', enum: ['orientation_required', 'acknowledged'] },
          source_mutation: { type: 'boolean', const: false },
          ordinary_work_gate: {
            type: 'string',
            enum: ['acknowledgement_required', 'open'],
          },
          orientation_brief: ORIENTATION_BRIEF_JSON_SCHEMA,
          manifest_ref: ORIENTATION_MANIFEST_REFERENCE_JSON_SCHEMA,
          delivery_receipt_ref: { type: 'string', minLength: 1 },
          acknowledgement_ref: {
            anyOf: [
              { type: 'string', minLength: 1 },
              { type: 'null' },
            ],
          },
          required_read_progress: ORIENTATION_REQUIRED_READ_PROGRESS_JSON_SCHEMA,
          next_call: ORIENTATION_NEXT_CALL_JSON_SCHEMA,
        },
        required: [
          'schema', 'status', 'source_mutation', 'ordinary_work_gate',
          'orientation_brief', 'manifest_ref',
          'delivery_receipt_ref', 'acknowledgement_ref',
          'required_read_progress', 'next_call',
        ],
      }, {
        type: 'object',
        additionalProperties: false,
        properties: {
          schema: { type: 'string', const: 'narada.agent_context.orientation_required_read.v1' },
          status: {
            type: 'string',
            enum: [
              'page_emitted', 'page_already_emitted',
              'completed', 'already_completed',
            ],
          },
          source_mutation: { type: 'boolean', const: false },
          local_persistence: { type: 'boolean', const: true },
          ordinary_work_gate: { type: 'string', const: 'acknowledgement_required' },
          step_id: { type: 'string', minLength: 1 },
          source: { type: 'object', additionalProperties: true },
          content: {
            anyOf: [{ type: 'string' }, { type: 'null' }],
          },
          page: {
            anyOf: [
              { type: 'object', additionalProperties: true },
              { type: 'null' },
            ],
          },
          result_evidence: {
            anyOf: [
              { type: 'object', additionalProperties: true },
              { type: 'null' },
            ],
          },
          completion_ref: {
            anyOf: [
              { type: 'string', minLength: 1 },
              { type: 'null' },
            ],
          },
          required_read_progress: ORIENTATION_REQUIRED_READ_PROGRESS_JSON_SCHEMA,
          next_call: ORIENTATION_NEXT_CALL_JSON_SCHEMA,
        },
        required: [
          'schema', 'status', 'source_mutation', 'local_persistence',
          'ordinary_work_gate', 'step_id', 'source', 'content', 'page',
          'result_evidence', 'completion_ref', 'required_read_progress',
          'next_call',
        ],
      }, {
        type: 'object',
        additionalProperties: true,
        properties: {
          schema: {
            type: 'string',
            const: 'narada.agent_context.orientation_selection_read.v1',
          },
          status: { type: 'string', enum: ['exact', 'omitted'] },
          source_mutation: { type: 'boolean', const: false },
          ordinary_work_gate: {
            type: 'string',
            enum: ['acknowledgement_required', 'open'],
          },
          selection_kind: { type: 'string', enum: ['continuity', 'work'] },
          manifest_ref: ORIENTATION_MANIFEST_REFERENCE_JSON_SCHEMA,
        },
        required: [
          'schema', 'status', 'source_mutation', 'ordinary_work_gate',
          'selection_kind', 'manifest_ref', 'selection', 'projection',
        ],
      }],
    },
  },
  {
    name: 'agent_orientation_acknowledge',
    description: 'Open the orientation gate after Agent Context has recorded every exact required read. No hashes, timestamps, or evidence payloads are accepted from the occupant.',
    inputSchema: {
      type: 'object',
      properties: {},
      required: [],
      additionalProperties: false,
    },
    outputSchema: {
      type: 'object',
      additionalProperties: false,
      properties: {
        schema: {
          type: 'string',
          const: 'narada.agent_context.orientation_acknowledgement_record.v1',
        },
        status: {
          type: 'string',
          enum: ['acknowledged', 'already_acknowledged'],
        },
        source_mutation: { type: 'boolean', const: false },
        local_persistence: { type: 'boolean', const: true },
        ordinary_work_gate: { type: 'string', const: 'open' },
        acknowledgement: ORIENTATION_ACKNOWLEDGEMENT_JSON_SCHEMA,
      },
      required: [
        'schema', 'status', 'source_mutation', 'local_persistence',
        'ordinary_work_gate', 'acknowledgement',
      ],
    },
  },
  guidanceToolDefinition(),
  {
    name: 'agent_context_doctor',
    description: 'Check site-local agent-context DB readiness and schema presence.',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'agent_context_whoami',
    description: 'Resolve the exact admitted Agent/Carrier Session binding from an owner-issued admission receipt. Historical recency is never identity evidence.',
    inputSchema: {
      type: 'object',
      properties: {
        hint: { type: 'string' },
        claimed_identity: {
          anyOf: [
            { type: 'string', minLength: 1 },
            {
              type: 'object',
              additionalProperties: false,
              properties: {
                identity: { type: 'string', minLength: 1 },
                value: { type: 'string', minLength: 1 },
                source: { type: 'string', minLength: 1 },
                evidence_refs: { type: 'array', items: { type: 'string', minLength: 1 } },
                asserted_at: { type: 'string' },
              },
              required: ['identity'],
            },
          ],
          description: 'A caller/runtime identity claim. This is recorded independently and never grants authority.',
        },
        admission_receipt: CARRIER_SESSION_ADMISSION_RECEIPT_JSON_SCHEMA,
      },
    },
  },
  {
    name: 'agent_context_start_session',
    description: 'Compile and retain an Orientation Manifest against an exact owner-issued Carrier Session admission receipt; the start event is only a downstream trace.',
    inputSchema: {
      type: 'object',
      properties: {
        identity: { type: 'string' },
        claimed_identity: {
          anyOf: [{ type: 'string', minLength: 1 }, { type: 'object', additionalProperties: true }],
          description: 'Identity claim recorded separately from authentication and authority.',
        },
        runtime: { type: 'string' },
        cwd: { type: 'string' },
        dry_run: { type: 'boolean' },
        admission_receipt: CARRIER_SESSION_ADMISSION_RECEIPT_JSON_SCHEMA,
        activation_receipt: CARRIER_SESSION_ACTIVATION_RECEIPT_JSON_SCHEMA,
        generated_at: { type: 'string' },
      },
      required: ['identity'],
    },
  },
  {
    name: 'agent_context_checkpoint',
    description: 'Write a durable site-local agent checkpoint, optionally carrying canonical continuation state and linking an exact portable continuation artifact.',
    inputSchema: {
      type: 'object',
      properties: {
        agent_id: { type: 'string' },
        claimed_identity: {
          anyOf: [{ type: 'string', minLength: 1 }, { type: 'object', additionalProperties: true }],
          description: 'Identity claim recorded in the checkpoint; it does not authenticate or authorize the operation.',
        },
        session_id: { type: 'string' },
        active_task: { type: 'object' },
        files_touched: { type: 'array', items: { type: 'string' } },
        key_decisions: { type: 'array', items: { type: 'string' } },
        open_questions: { type: 'array', items: { type: 'string' } },
        git_head: { type: 'string' },
        last_workboard_check_at: { type: 'string' },
        next_intended_action: { type: 'object' },
        authority_basis: { type: 'object' },
        continuation_blockers: { type: 'array', items: { type: 'string' } },
        evidence_refs: { type: 'array', items: { type: 'string' } },
        worktree_state: { type: 'object' },
        tactical_resume_notes: { type: 'array', items: { type: 'string' } },
        continuation_ref: {
          type: 'object',
          additionalProperties: false,
          properties: {
            schema: { type: 'string', const: 'narada.continuation.handoff.v1' },
            path: { type: 'string' },
            sha256: { type: 'string' },
            created_at: { type: 'string' },
          },
          required: ['schema', 'path', 'sha256', 'created_at'],
        },
        continuation: {
          type: 'object',
          additionalProperties: false,
          properties: {
            schema: { type: 'string', const: 'narada.continuation.v1' },
            continuation_id: { type: 'string' },
            objective: { type: 'string' },
            current_state: { type: 'string' },
            completed_work: { type: 'array', items: { type: 'string' } },
            decisions: { type: 'array', items: { type: 'string' } },
            evidence_refs: { type: 'array', items: { type: 'string' } },
            open_blockers: { type: 'array', items: { type: 'string' } },
            next_action: { type: 'string' },
            canonical_sources: { type: 'array', items: { type: 'string' } },
            constraints: { type: 'array', items: { type: 'string' } },
            resume_mode: { type: 'string', enum: ['fresh_session', 'same_session'] },
            created_at: { type: 'string' },
          },
          required: ['schema', 'objective', 'current_state'],
        },
      },
    },
  },
  {
    name: 'agent_context_rehydrate',
    description: 'Retrieve the latest site-local checkpoint, an exact current or archived checkpoint, or bounded checkpoint history for an agent.',
    inputSchema: {
      type: 'object',
      properties: {
        agent_id: { type: 'string' },
        checkpoint_id: { type: 'string', description: 'Optional exact checkpoint ID. Searches current and archived checkpoints scoped to this agent.' },
        history: { type: 'boolean' },
        limit: { type: 'integer', minimum: 1, maximum: 50 },
        offset: { type: 'integer', minimum: 0, default: 0 },
      },
      required: ['agent_id'],
    },
  },
  {
    name: 'agent_context_continuation_export',
    description: 'Render the latest canonical continuation as a bounded Site-local Markdown projection and attach its verified reference.',
    inputSchema: {
      type: 'object',
      properties: {
        agent_id: { type: 'string' },
        path: { type: 'string', description: 'Optional Site-relative path under .ai/continuations ending in .md.' },
        overwrite: { type: 'boolean', description: 'Allow replacing an existing projection at the explicit path.' },
      },
      required: ['agent_id'],
    },
  },
  {
    name: 'agent_context_continuation_read',
    description: 'Read the latest or explicitly selected continuation and verify its portable Markdown projection against the checkpoint reference and canonical content hash.',
    inputSchema: {
      type: 'object',
      properties: {
        agent_id: { type: 'string' },
        checkpoint_id: { type: 'string', description: 'Optional exact checkpoint ID. Searches current and archived checkpoints scoped to this agent.' },
      },
      required: ['agent_id'],
    },
  },
  {
    name: 'agent_context_hydrate_current',
    description: 'Compile a read-only diagnostic Orientation Manifest candidate for the exact admitted Carrier Session, optionally including one explicitly selected checkpoint. It does not replace the admitted generation.',
    inputSchema: {
      type: 'object',
      properties: {
        checkpoint_id: { type: 'string', description: 'Optional exact checkpoint ID. Omission means continuity is omitted, never latest.' },
        admission_receipt: CARRIER_SESSION_ADMISSION_RECEIPT_JSON_SCHEMA,
        activation_receipt: CARRIER_SESSION_ACTIVATION_RECEIPT_JSON_SCHEMA,
        generated_at: { type: 'string' },
        output: { type: 'string' },
      },
    },
  },
  {
    name: 'agent_context_startup_sequence',
    description: 'Read and deliver the exact immutable Orientation Manifest generation selected by the Carrier entry procedure. It never recompiles, selects latest, or mutates a checkpoint.',
    inputSchema: {
      type: 'object',
      properties: {
        manifest_id: { type: 'string', description: 'Optional exact manifest ID; must match NARADA_ORIENTATION_MANIFEST_ID when both are present.' },
        admission_receipt: CARRIER_SESSION_ADMISSION_RECEIPT_JSON_SCHEMA,
        activation_receipt: CARRIER_SESSION_ACTIVATION_RECEIPT_JSON_SCHEMA,
        output: { type: 'string' },
      },
      additionalProperties: false,
    },
  },
  {
    name: 'agent_context_list_sessions',
    description: 'List site-local agent start sessions.',
    inputSchema: {
      type: 'object',
      properties: {
        identity: { type: 'string' },
        limit: { type: 'integer', minimum: 1, maximum: 500 },
        offset: { type: 'integer', minimum: 0, default: 0 },
      },
    },
  },
  {
    name: 'mcp_output_show',
    description: 'Read a materialized Agent Context MCP output ref with offset/limit paging.',
    inputSchema: {
      type: 'object',
      properties: {
        ref: { type: 'string' },
        output_ref: { type: 'string' },
        offset: { type: 'integer' },
        limit: { type: 'integer' },
      },
    },
  },
].map((tool: any) => ({
  ...tool,
  annotations: toolAnnotations(tool.name),
  outputSchema: tool.outputSchema ?? genericToolOutputSchema(),
}));

export type AgentContextToolProjection = 'occupant' | 'admin';

const OCCUPANT_TOOL_NAMES = new Set([
  'agent_orientation_read',
  'mcp_output_show',
]);

const OCCUPANT_ORIENTATION_READ_TOOL = {
  name: 'agent_orientation_read',
  description: 'Receive the bounded Carrier-entry orientation. Call with {} first, then replay only the exact opaque continuation returned in next_call until status=ready. The Carrier retains receipts, hashes, page coordinates, and completion evidence.',
  inputSchema: {
    type: 'object',
    properties: {
      continuation: {
        type: 'string',
        minLength: 1,
        description: 'Opaque continuation returned by the preceding next_call. Do not inspect or alter it.',
      },
    },
    additionalProperties: false,
  },
  outputSchema: { type: 'object', additionalProperties: true },
  annotations: toolAnnotations('agent_orientation_read'),
};


function toolAnnotations(name: string) {
  const writes = LOCAL_WRITE_TOOLS.has(name);
  return {
    title: name,
    readOnlyHint: !writes,
    destructiveHint: false,
    idempotentHint: name.startsWith('agent_orientation_')
      || /doctor|whoami|rehydrate|hydrate|startup|list/.test(name),
    openWorldHint: false,
  };
}

function genericToolOutputSchema() {
  return { type: 'object', additionalProperties: true };
}

export function listAgentContextTools(
  projection: AgentContextToolProjection,
) {
  if (projection === 'admin') return TOOLS;
  return TOOLS
    .filter((tool: any) => OCCUPANT_TOOL_NAMES.has(tool.name))
    .map((tool: any) => tool.name === 'agent_orientation_read'
      ? OCCUPANT_ORIENTATION_READ_TOOL
      : tool);
}
