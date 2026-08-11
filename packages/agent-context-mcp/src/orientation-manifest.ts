import {
  ORIENTATION_ASSEMBLY_POLICY_SCHEMA,
  compileOrientationManifest,
  parseCarrierSessionActivationReceipt,
  parseCarrierSessionAdmissionReceipt,
  type CarrierSessionActivationReceipt,
  type CarrierSessionAdmissionReceipt,
  type JsonObject,
  type OrientationAssemblyPolicy,
  type OrientationCompilationResult,
  type OrientationProjectionEntry,
} from '@narada-core/orientation-manifest';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  assertOrientationRequiredReadSourceBound,
  ORIENTATION_REQUIRED_READ_MAX_TOTAL_PAGES,
  orientationManifestEntryArtifactRef,
  renderExactContinuityReadMaterial,
} from './orientation-read-material.js';

export const ORIENTATION_ADMISSION_ENV = 'NARADA_CARRIER_SESSION_ADMISSION_RECEIPT';
export const ORIENTATION_ACTIVATION_ENV = 'NARADA_CARRIER_SESSION_ACTIVATION_RECEIPT';
export const AGENT_CONTEXT_ORIENTATION_POLICY_REF = 'orientation-policy:agent-context-compatibility';
export const AGENT_CONTEXT_ORIENTATION_POLICY_REVISION = '1';

type UnknownRecord = Record<string, any>;

function sha256(value: string): string {
  return createHash('sha256').update(value).digest('hex');
}

function boundedOrientationText(value: unknown, maxLength = 320): string | null {
  if (value === null || value === undefined) return null;
  const text = typeof value === 'string'
    ? value.trim()
    : JSON.stringify(value);
  if (!text) return null;
  return text.length <= maxLength
    ? text
    : text.slice(0, Math.max(1, maxLength - 1)).trimEnd() + '…';
}

function continuityOccupantSummary(checkpoint: UnknownRecord): JsonObject {
  const continuation = jsonObject(checkpoint.continuation ?? {});
  const activeTask = jsonObject(checkpoint.active_task ?? {});
  const blockers = Array.isArray(checkpoint.continuation_blockers)
    ? checkpoint.continuation_blockers
    : [];
  return {
    checkpoint_id: String(checkpoint.checkpoint_id),
    checkpoint_at: boundedOrientationText(checkpoint.checkpoint_at, 80),
    objective: boundedOrientationText(
      continuation.objective ?? activeTask.objective ?? activeTask.title,
    ),
    current_state: boundedOrientationText(
      continuation.current_state ?? checkpoint.tactical_resume_notes,
    ),
    next_action: boundedOrientationText(
      continuation.next_action ?? checkpoint.next_intended_action,
    ),
    blocker_count: blockers.length,
    historical_advisory_only: true,
  };
}

function workOccupantSummary(work: UnknownRecord): JsonObject {
  const lifecycle = jsonObject(work.lifecycle ?? {});
  const specification = jsonObject(work.specification ?? {});
  const continuationPacket = jsonObject(lifecycle.continuation_packet ?? {});
  return {
    task_number: Number(work.task_number),
    title: boundedOrientationText(specification.title)
      ?? `Task #${String(work.task_number)}`,
    status: boundedOrientationText(lifecycle.status, 80),
    objective: boundedOrientationText(
      specification.goal_markdown ?? specification.required_work_markdown,
    ),
    next_action: boundedOrientationText(
      continuationPacket.next_action ?? continuationPacket.next_intended_action,
    ),
    selection_semantics: 'orientation_only_not_action_authority',
  };
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value === 'boolean' || typeof value === 'number') {
    return JSON.stringify(value);
  }
  if (typeof value === 'string') return JSON.stringify(value);
  if (Array.isArray(value)) return '[' + value.map(canonicalJson).join(',') + ']';
  if (typeof value === 'object') {
    const record = value as Record<string, unknown>;
    return '{' + Object.keys(record).sort().map((key) => (
      JSON.stringify(key) + ':' + canonicalJson(record[key])
    )).join(',') + '}';
  }
  throw new TypeError('agent_context_orientation_non_json_value');
}

function sha256Json(value: unknown): string {
  return sha256(canonicalJson(value));
}

function compareStrings(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function siteRef(siteId: string): string {
  const normalized = String(siteId).trim();
  return normalized.startsWith('site:') ? normalized : 'site:' + normalized;
}

function jsonObject(value: unknown): JsonObject {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return {};
  return JSON.parse(JSON.stringify(value)) as JsonObject;
}

function parseEnvironmentJson(value: string | undefined, variable: string): unknown {
  if (!value) return null;
  try {
    return JSON.parse(value);
  } catch (error) {
    throw new Error(
      'orientation_environment_json_invalid:' + variable + ':'
      + (error instanceof Error ? error.message : String(error)),
    );
  }
}

export function orientationEvidenceFromEnvironment(
  env: NodeJS.ProcessEnv = process.env,
): {
  admission_receipt: CarrierSessionAdmissionReceipt | null;
  activation_receipt: CarrierSessionActivationReceipt | null;
} {
  const admissionValue = parseEnvironmentJson(env[ORIENTATION_ADMISSION_ENV], ORIENTATION_ADMISSION_ENV);
  const activationValue = parseEnvironmentJson(env[ORIENTATION_ACTIVATION_ENV], ORIENTATION_ACTIVATION_ENV);
  return {
    admission_receipt: admissionValue === null
      ? null
      : parseCarrierSessionAdmissionReceipt(admissionValue),
    activation_receipt: activationValue === null
      ? null
      : parseCarrierSessionActivationReceipt(activationValue),
  };
}

export function assertAdmissionMatchesAgentContext(
  admissionValue: unknown,
  options: {
    siteId: string;
    identity: string;
    carrierSessionId?: string | null;
    observedAt?: string | null;
  },
): CarrierSessionAdmissionReceipt {
  const admission = parseCarrierSessionAdmissionReceipt(admissionValue);
  if (admission.decision !== 'admitted') {
    throw new Error('agent_context_exact_admission_receipt_required');
  }
  if (admission.coordinate.site_ref !== siteRef(options.siteId)) {
    throw new Error('agent_context_admission_site_mismatch');
  }
  if (admission.agent_identity.local_agent_id !== options.identity) {
    throw new Error('agent_context_admission_identity_mismatch');
  }
  if (
    options.carrierSessionId
    && admission.coordinate.carrier_session_id !== options.carrierSessionId
  ) {
    throw new Error('agent_context_admission_session_mismatch');
  }
  if (
    options.observedAt
    && admission.valid_until !== null
    && Date.parse(admission.valid_until) <= Date.parse(options.observedAt)
  ) {
    throw new Error('agent_context_admission_receipt_expired');
  }
  return admission;
}

export function defaultAgentContextOrientationPolicy(
  overrides: Partial<OrientationAssemblyPolicy> = {},
): OrientationAssemblyPolicy {
  return {
    schema: ORIENTATION_ASSEMBLY_POLICY_SCHEMA,
    policy_ref: AGENT_CONTEXT_ORIENTATION_POLICY_REF,
    revision: AGENT_CONTEXT_ORIENTATION_POLICY_REVISION,
    required_entry_kinds: ['agent_identity', 'site_law', 'entry_procedure'],
    max_entries: 24,
    max_rendered_bytes: 64 * 1024,
    max_manifest_bytes: 256 * 1024,
    continuity_selection: 'exact_or_omitted',
    optional_entry_behavior: 'degrade',
    negative_claims: [
      {
        claim_id: 'orientation_is_not_authorization',
        statement: 'This Orientation Manifest does not authorize any later action.',
      },
      {
        claim_id: 'capability_is_not_authority',
        statement: 'Projected tools and capabilities are availability evidence, not authority grants.',
      },
      {
        claim_id: 'work_reference_is_not_claim',
        statement: 'A work reference does not claim, activate, defer, review, or close work.',
      },
      {
        claim_id: 'checkpoint_is_not_live_truth',
        statement: 'Continuity material is historical evidence and must not replace live authority readback.',
      },
      {
        claim_id: 'acknowledgement_is_not_comprehension',
        statement: 'Delivery or acknowledgement does not prove comprehension, competence, or compliance.',
      },
    ],
    ...overrides,
  };
}

function projection(
  admission: CarrierSessionAdmissionReceipt,
  options: Omit<OrientationProjectionEntry, 'subject'>,
): OrientationProjectionEntry {
  return {
    ...options,
    subject: {
      site_ref: admission.coordinate.site_ref,
      agent_ref: admission.agent_identity.artifact_ref,
      carrier_session_id: admission.coordinate.carrier_session_id,
    },
  };
}

export function resolveAgentContextLawPath(siteRoot: string): {
  path: string;
  siteRelativePath: string;
  source: 'site_root' | 'contained_governance_root';
} {
  const direct = join(siteRoot, 'AGENTS.md');
  if (existsSync(direct)) {
    return { path: direct, siteRelativePath: 'AGENTS.md', source: 'site_root' };
  }
  const contained = join(siteRoot, '.narada', 'AGENTS.md');
  const containedConfig = join(siteRoot, '.narada', 'config.json');
  if (existsSync(contained) && existsSync(containedConfig)) {
    return {
      path: contained,
      siteRelativePath: '.narada/AGENTS.md',
      source: 'contained_governance_root',
    };
  }
  return { path: direct, siteRelativePath: 'AGENTS.md', source: 'site_root' };
}

export interface AgentContextOrientationProjectionInput {
  siteRoot: string;
  siteId: string;
  admissionReceipt: CarrierSessionAdmissionReceipt;
  observedAt: string;
  roleBinding?: unknown;
  exactCheckpoint?: UnknownRecord | null;
  portableContinuation?: UnknownRecord | null;
  exactWork?: UnknownRecord | null;
  mcpServers?: readonly UnknownRecord[];
}

export function buildAgentContextOrientationProjections(
  input: AgentContextOrientationProjectionInput,
): readonly OrientationProjectionEntry[] {
  const admission = assertAdmissionMatchesAgentContext(input.admissionReceipt, {
    siteId: input.siteId,
    identity: input.admissionReceipt.agent_identity.local_agent_id,
    carrierSessionId: input.admissionReceipt.coordinate.carrier_session_id,
    observedAt: input.observedAt,
  });
  const observedAt = new Date(input.observedAt).toISOString();
  const entries: OrientationProjectionEntry[] = [
    projection(admission, {
      entry_id: 'orientation:agent-identity',
      compartment: 'office_and_role',
      entry_kind: 'agent_identity',
      source_authority_ref: admission.agent_identity.source_authority_ref,
      artifact_ref: admission.agent_identity.artifact_ref,
      revision: admission.agent_identity.revision,
      observed_at: observedAt,
      valid_until: admission.valid_until,
      criticality: 'required',
      projection_status: 'available',
      revalidation_rule: 'on_agent_identity_revision_or_status_change',
      evidence_refs: [admission.receipt_id, ...admission.evidence_refs],
      payload: {
        local_agent_id: admission.agent_identity.local_agent_id,
        canonical_agent_id: admission.agent_identity.canonical_agent_id,
      },
      rendered_text: 'Admitted Agent: ' + admission.agent_identity.canonical_agent_id,
    }),
  ];

  const roleBinding = jsonObject(input.roleBinding ?? {});
  if (Object.keys(roleBinding).length > 0) {
    const bindingAuthority = String(
      roleBinding.binding_authority ?? roleBinding.binding_source ?? 'unavailable',
    );
    const authoritative = bindingAuthority !== 'unavailable'
      && !bindingAuthority.includes('non_authoritative');
    const revision = sha256Json(roleBinding);
    entries.push(projection(admission, {
      entry_id: 'orientation:role-binding',
      compartment: 'office_and_role',
      entry_kind: 'role_binding',
      source_authority_ref: 'agent-role-binding:' + bindingAuthority,
      artifact_ref: 'agent-role-binding:' + admission.agent_identity.canonical_agent_id,
      revision,
      observed_at: observedAt,
      valid_until: null,
      criticality: 'optional',
      projection_status: authoritative ? 'available' : 'rejected',
      revalidation_rule: authoritative
        ? 'on_role_binding_revision_or_status_change'
        : 'replace_with_owner_issued_role_binding',
      evidence_refs: authoritative ? ['sha256:' + revision] : [],
      payload: {
        role_binding: roleBinding,
        authoritative,
        grants_capability: false,
      },
      rendered_text: authoritative
        ? 'Role binding projected from ' + bindingAuthority + '.'
        : null,
    }));
  }

  const lawLocation = resolveAgentContextLawPath(input.siteRoot);
  const agentsPath = lawLocation.path;
  const lawArtifactRef = `site-file:${lawLocation.siteRelativePath}`;
  let requiredReads: UnknownRecord[] = [];
  let requiredReadPageCount: any = 0;
  if (existsSync(agentsPath)) {
    const law = readFileSync(agentsPath, 'utf8');
    requiredReadPageCount += assertOrientationRequiredReadSourceBound(
      law,
      lawArtifactRef,
    ).page_count;
    const revision = sha256(law);
    const lineCount = Math.max(1, law.split(/\r?\n/).length);
    requiredReads = [{
      step_id: 'read:site-law',
      ordinal: 1,
      required: true,
      source: {
        source_authority_ref: 'site-law:' + admission.coordinate.site_ref,
        artifact_ref: lawArtifactRef,
        revision,
      },
      tool: {
        name: 'agent_orientation_read',
        arguments: {
          step_id: 'read:site-law',
        },
      },
      completion: {
        kind: 'tool_result_fields',
        expected_result: {
          content_sha256: revision,
          offset: 1,
          returned_lines: lineCount,
        },
        evidence_fields: [
          'content_sha256',
          'content_window_sha256',
          'offset',
          'returned_lines',
        ],
      },
    }];
    entries.push(projection(admission, {
      entry_id: 'orientation:site-law',
      compartment: 'law_and_constraints',
      entry_kind: 'site_law',
      source_authority_ref: 'site-law:' + admission.coordinate.site_ref,
      artifact_ref: lawArtifactRef,
      revision,
      observed_at: observedAt,
      valid_until: null,
      criticality: 'required',
      projection_status: 'available',
      revalidation_rule: 'on_sha256_change',
      evidence_refs: ['sha256:' + revision],
      payload: {
        site_relative_path: lawLocation.siteRelativePath,
        sha256: revision,
        content_included: false,
        read_required: true,
        required_read_step_ids: requiredReads.map((step) => String(step.step_id)),
      },
      rendered_text: 'Applicable Site instructions: AGENTS.md (sha256 ' + revision + ').',
    }));
  } else {
    entries.push(projection(admission, {
      entry_id: 'orientation:site-law',
      compartment: 'law_and_constraints',
      entry_kind: 'site_law',
      source_authority_ref: 'site-law:' + admission.coordinate.site_ref,
      artifact_ref: lawArtifactRef,
      revision: 'unavailable',
      observed_at: observedAt,
      valid_until: null,
      criticality: 'required',
      projection_status: 'unavailable',
      revalidation_rule: 'before_orientation_delivery',
      evidence_refs: [],
      payload: {
        site_relative_path: lawLocation.siteRelativePath,
        missing: true,
      },
      rendered_text: null,
    }));
  }

  entries.push(projection(admission, {
    entry_id: 'orientation:entry-procedure',
    compartment: 'entry_procedure',
    entry_kind: 'entry_procedure',
    source_authority_ref: 'carrier-entry-procedure:agent-context',
    artifact_ref: 'agent-context:orientation-entry-procedure',
    revision: AGENT_CONTEXT_ORIENTATION_POLICY_REVISION,
    observed_at: observedAt,
    valid_until: null,
    criticality: 'required',
    projection_status: 'available',
    revalidation_rule: 'on_entry_procedure_revision',
    evidence_refs: [admission.receipt_id],
    payload: {
      required_reads: [],
      ordered_steps: [
        {
          step: 'complete_required_reads',
          effect: 'read',
          required: true,
          completion_evidence: 'orientation_required_read_completion',
        },
        {
          step: 'inspect_named_live_authorities_before_work_mutation',
          effect: 'read',
          required: true,
        },
        {
          step: 'obtain_owner_specific_action_admission_before_consequence',
          effect: 'separate_governed_crossing',
          required: true,
        },
      ],
      self_referential_tool_call: false,
    },
    rendered_text: 'Review this manifest, inspect live owners, and obtain separate admission before consequential action.',
  }));

  if (input.exactCheckpoint?.status === 'ok' && input.exactCheckpoint.checkpoint_id) {
    const checkpoint = jsonObject(input.exactCheckpoint);
    const portableContinuation = jsonObject(input.portableContinuation ?? {});
    const entryId = 'orientation:continuity:'
      + String(input.exactCheckpoint.checkpoint_id);
    const readMaterial = renderExactContinuityReadMaterial({
      checkpoint,
      portableContinuation,
    });
    requiredReadPageCount += assertOrientationRequiredReadSourceBound(
      readMaterial,
      orientationManifestEntryArtifactRef(entryId),
    ).page_count;
    const revision = sha256(readMaterial);
    const readStepId = 'read:continuity:' + sha256(entryId).slice(0, 16);
    const lineCount = Math.max(1, readMaterial.split(/\r?\n/).length);
    requiredReads.push({
      step_id: readStepId,
      ordinal: requiredReads.length + 1,
      required: true,
      source: {
        source_authority_ref: 'agent-continuity:' + admission.coordinate.site_ref,
        artifact_ref: orientationManifestEntryArtifactRef(entryId),
        revision,
      },
      tool: {
        name: 'agent_orientation_read',
        arguments: {
          step_id: readStepId,
        },
      },
      completion: {
        kind: 'tool_result_fields',
        expected_result: {
          content_sha256: revision,
          offset: 1,
          returned_lines: lineCount,
        },
        evidence_fields: [
          'content_sha256',
          'content_window_sha256',
          'offset',
          'returned_lines',
        ],
      },
    });
    entries.push(projection(admission, {
      entry_id: entryId,
      compartment: 'continuity',
      entry_kind: 'exact_continuity',
      source_authority_ref: 'agent-continuity:' + admission.coordinate.site_ref,
      artifact_ref: 'checkpoint:' + String(input.exactCheckpoint.checkpoint_id),
      revision,
      observed_at: observedAt,
      valid_until: null,
      criticality: 'required',
      projection_status: 'available',
      revalidation_rule: 'never_as_live_truth;verify_exact_hash_on_read',
      evidence_refs: [
        'checkpoint:' + String(input.exactCheckpoint.checkpoint_id),
        'sha256:' + revision,
        ...(input.portableContinuation?.artifact?.sha256
          ? ['sha256:' + String(input.portableContinuation.artifact.sha256)]
          : []),
      ],
      payload: {
        checkpoint,
        portable_continuation: portableContinuation,
        historical_advisory_only: true,
        occupant_summary: continuityOccupantSummary(checkpoint),
        inspection_call: null,
        required_read_step_ids: [readStepId],
      },
      rendered_text: 'Exact continuity checkpoint: ' + String(input.exactCheckpoint.checkpoint_id) + '.',
    }));
  } else if (input.exactCheckpoint?.checkpoint_id) {
    const checkpointId = String(input.exactCheckpoint.checkpoint_id);
    const unavailable = input.exactCheckpoint.status === 'checkpoint_not_found';
    entries.push(projection(admission, {
      entry_id: 'orientation:continuity:' + checkpointId,
      compartment: 'continuity',
      entry_kind: 'exact_continuity',
      source_authority_ref: 'agent-continuity:' + admission.coordinate.site_ref,
      artifact_ref: 'checkpoint:' + checkpointId,
      revision: 'unavailable',
      observed_at: observedAt,
      valid_until: null,
      criticality: 'required',
      projection_status: unavailable ? 'unavailable' : 'incompatible',
      revalidation_rule: 'resolve_exact_checkpoint_before_reassembly',
      evidence_refs: [],
      payload: {
        requested_checkpoint_id: checkpointId,
        source_status: String(input.exactCheckpoint.status ?? 'unknown'),
        source_message: String(input.exactCheckpoint.message ?? ''),
        historical_advisory_only: true,
      },
      rendered_text: null,
    }));
  }

  if (input.exactWork?.status === 'ok' && input.exactWork.task_number) {
    const work = jsonObject(input.exactWork);
    const revision = sha256Json(work);
    entries.push(projection(admission, {
      entry_id: 'orientation:work:task-' + String(input.exactWork.task_number),
      compartment: 'work_orientation',
      entry_kind: 'exact_work',
      source_authority_ref: 'task-lifecycle:' + admission.coordinate.site_ref,
      artifact_ref: 'task:' + String(input.exactWork.task_number),
      revision,
      observed_at: observedAt,
      valid_until: null,
      criticality: 'required',
      projection_status: 'available',
      revalidation_rule: 'inspect_exact_task_live_authority_before_mutation',
      evidence_refs: [
        'task:' + String(input.exactWork.task_number),
        'sha256:' + revision,
      ],
      payload: {
        work,
        orientation_only: true,
        action_authority_granted: false,
        occupant_summary: workOccupantSummary(work),
        inspection_call: {
          surface_id: 'task-lifecycle',
          tool: 'task_lifecycle_inspect_range',
          arguments: {
            start_task_number: Number(input.exactWork.task_number),
            end_task_number: Number(input.exactWork.task_number),
            include_body: true,
            limit: 1,
          },
        },
      },
      rendered_text: 'Exact work selection: task #' + String(input.exactWork.task_number) + '.',
    }));
  } else if (input.exactWork?.task_number) {
    const taskNumber = String(input.exactWork.task_number);
    entries.push(projection(admission, {
      entry_id: 'orientation:work:task-' + taskNumber,
      compartment: 'work_orientation',
      entry_kind: 'exact_work',
      source_authority_ref: 'task-lifecycle:' + admission.coordinate.site_ref,
      artifact_ref: 'task:' + taskNumber,
      revision: 'unavailable',
      observed_at: observedAt,
      valid_until: null,
      criticality: 'required',
      projection_status: input.exactWork.status === 'task_not_found'
        ? 'unavailable'
        : 'incompatible',
      revalidation_rule: 'resolve_exact_task_before_reassembly',
      evidence_refs: [],
      payload: {
        requested_task_number: Number(input.exactWork.task_number),
        source_status: String(input.exactWork.status ?? 'unknown'),
        source_message: String(input.exactWork.message ?? ''),
        orientation_only: true,
        action_authority_granted: false,
      },
      rendered_text: null,
    }));
  }

  if ((input.mcpServers?.length ?? 0) > 0) {
    const servers = [...(input.mcpServers ?? [])]
      .map((server) => ({
        name: String(server.name ?? ''),
        transport: String(server.transport ?? 'stdio'),
      }))
      .filter((server) => server.name)
      .sort((left, right) => compareStrings(left.name, right.name));
    const revision = sha256Json(servers);
    entries.push(projection(admission, {
      entry_id: 'orientation:capability-projection',
      compartment: 'capability_projection',
      entry_kind: 'mcp_capability_projection',
      source_authority_ref: 'mcp-fabric:' + admission.coordinate.site_ref,
      artifact_ref: 'mcp-fabric:carrier-session:' + admission.coordinate.carrier_session_id,
      revision,
      observed_at: observedAt,
      valid_until: null,
      criticality: 'optional',
      projection_status: 'available',
      revalidation_rule: 'on_mcp_fabric_generation_or_runtime_posture_change',
      evidence_refs: ['sha256:' + revision],
      payload: {
        servers,
        availability_only: true,
        authority_granted: false,
      },
      rendered_text: 'Projected MCP servers: ' + servers.map((server) => server.name).join(', ') + '.',
    }));
  }

  const entryProcedureIndex: any = entries.findIndex(
    (entry: any) => entry.entry_id === 'orientation:entry-procedure',
  );
  if (entryProcedureIndex < 0) {
    throw new Error('agent_context_orientation_entry_procedure_missing');
  }
  const entryProcedure: any = entries[entryProcedureIndex];
  entries[entryProcedureIndex] = {
    ...entryProcedure,
    payload: {
      ...entryProcedure.payload,
      required_reads: requiredReads.map((step: any) => jsonObject(step)),
    },
  };

  if (requiredReadPageCount > ORIENTATION_REQUIRED_READ_MAX_TOTAL_PAGES) {
    throw new Error(
      'agent_context_orientation_required_read_aggregate_page_bound_exceeded:'
      + `actual=${requiredReadPageCount}:max=${ORIENTATION_REQUIRED_READ_MAX_TOTAL_PAGES}`,
    );
  }

  return entries;
}

export interface CompileAgentContextOrientationInput extends AgentContextOrientationProjectionInput {
  activationReceipt?: CarrierSessionActivationReceipt | null;
  assemblyPolicy?: OrientationAssemblyPolicy;
}

export function compileAgentContextOrientation(
  input: CompileAgentContextOrientationInput,
): OrientationCompilationResult {
  const policy = input.assemblyPolicy ?? defaultAgentContextOrientationPolicy();
  return compileOrientationManifest({
    admission_receipt: input.admissionReceipt,
    activation_receipt: input.activationReceipt ?? null,
    assembly_policy: policy,
    projections: buildAgentContextOrientationProjections(input),
    generated_at: input.observedAt,
  });
}
