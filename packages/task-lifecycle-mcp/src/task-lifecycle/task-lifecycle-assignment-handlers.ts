type TaskLifecyclePayload = Record<string, unknown>;
import { inspectSupersededTaskGuard } from './task-lineage-guards.js';
import { buildTaskLifecycleIdentityState } from './task-lifecycle-routing-roster.js';

type GenericEngineerClaimAuthorityArgs = {
  args: TaskLifecyclePayload;
  eligibility: TaskLifecyclePayload;
  lifecycle: TaskLifecyclePayload;
  taskNumber: number;
  agentId: string;
};

export const TASK_LIFECYCLE_ASSIGNMENT_TOOL_NAMES = Object.freeze([
  'task_lifecycle_claim',
  'task_lifecycle_continue',
  'task_lifecycle_unclaim',
]);

export function createTaskLifecycleAssignmentHandlers({
  store,
  siteRoot,
  jsonToolResult,
  stringField,
  numberField,
  enforceSessionIdentity,
  verifySessionIdentity,
  checkTaskRoleEligibilityLocal,
  validatePreferredAgentMismatchAuthority,
  recordClaimIntent,
  claimLifecycleTask,
  continueTaskService,
  unclaimLifecycleTask,
  withAuthoredRosterJsonPreserved,
}: any) {
  return {
    task_lifecycle_claim: async (args: any) => {
      const taskNumber = numberField(args, 'task_number');
      const agentId = stringField(args, 'agent_id');
      if (!taskNumber) throw new Error('task_number_required');
      if (!agentId) throw new Error('agent_id_required');
      enforceSessionIdentity(agentId);
      const identityWarning = verifySessionIdentity(agentId);
      const lifecycle = store.getLifecycleByNumber(taskNumber);
      if (!lifecycle) throw new Error(`task_not_found: ${taskNumber}`);

      const lineageGuard = inspectSupersededTaskGuard({ store, lifecycle, authorityBasis: args.authority_basis });
      if (lineageGuard.status === 'blocked') {
        return jsonToolResult({
          ...lineageGuard,
          status: 'blocked',
          operation: 'claim',
        }, true);
      }

      const eligibility = checkTaskRoleEligibilityLocal({ store, siteRoot, taskId: lifecycle.task_id, taskNumber, agentId });
      const genericEngineerAuthority = validateGenericEngineerClaimAuthority({ args, eligibility, lifecycle, taskNumber, agentId });
      if (!eligibility.eligible && genericEngineerAuthority.status !== 'ok') {
        return jsonToolResult({
          status: 'role_mismatch',
          schema: 'narada.task.claim.role_mismatch.v1',
          task_number: taskNumber,
          target_role: eligibility.targetRole,
          agent_role: eligibility.agentRole,
          role_resolution: eligibility.roleResolution,
          message: eligibility.warning,
          remediation: {
            summary: 'Use MCP-native roster admission or routing repair before claiming role-targeted work.',
            claim_with_authority: {
              applies_when: 'target_role is engineer and the rostered agent role is a site-specific engineer role ending with -engineer',
              required_authority_basis: { kind: 'operator_direct_instruction', summary: '<operator authorized this repo-specific engineer to claim generic engineer work>' },
              example_args: { task_number: taskNumber, agent_id: agentId, authority_basis: { kind: 'operator_direct_instruction', summary: '<why this repo-specific engineer claim is authorized>' } },
            },
            roster_admit: {
              tool: 'task_lifecycle_roster_admit',
              required_authority_basis: { kind: 'operator_direct_instruction', summary: '<operator authorized this agent/role binding>' },
              example_args: { agent_id: agentId, role: eligibility.targetRole ?? '<target_role>', actor_agent_id: '<admitted_operator_agent>', authority_basis: { kind: 'operator_direct_instruction', summary: '<why this roster admission is authorized>' } },
            },
            reroute: {
              tool: 'task_lifecycle_set_routing',
              required_authority_basis: { kind: 'operator_direct_instruction', summary: '<operator authorized routing change>' },
              example_args: { task_number: taskNumber, actor_agent_id: '<admitted_operator_agent>', target_role: eligibility.agentRole ?? '<agent_role>', authority_basis: { kind: 'operator_direct_instruction', summary: '<why reroute is authorized>' } },
            },
          },
        }, true);
      }
      const mismatchAuthority = validatePreferredAgentMismatchAuthority({ args, eligibility, lifecycle, taskNumber, agentId });
      if (mismatchAuthority.status === 'blocked') {
        const intentWarning = safeRecordClaimIntent(recordClaimIntent, {
          store,
          lifecycle,
          taskNumber,
          agentId,
          status: 'rejected',
          rejectionReason: 'preferred_agent_mismatch_requires_authority',
          authorityBasis: mismatchAuthority.authority_basis,
          preferredAgentWarning: mismatchAuthority.preferred_agent_warning,
        });
        return jsonToolResult({
          status: 'preferred_agent_mismatch_requires_authority',
          task_number: taskNumber,
          preferred_agent_id: eligibility.preferredAgentId,
          claiming_agent: agentId,
          pre_claim_warnings: [mismatchAuthority.preferred_agent_warning],
          remediation: 'Retry the claim with authority_basis: { kind: "operator_direct_instruction" | "directed_obligation" | "task_owner_handoff", summary: "..." }.',
          preferred_agent_warning: mismatchAuthority.preferred_agent_warning,
          identity_state: buildTaskLifecycleIdentityState({
            agentId,
            operation: 'task_lifecycle.claim',
            status: 'denied',
            authorityBasis: mismatchAuthority.authority_basis,
          }),
          schema: 'narada.task.claim.preferred_agent_authority.v0',
          ...(intentWarning ? { intent_recording_warning: intentWarning } : {}),
        }, true);
      }

      const serviceResult = await claimLifecycleTask({ siteRoot, store, taskNumber, agentId });
      if (serviceResult.status === 'closure_authority_blocks_claim') return jsonToolResult(serviceResult, true);
      if (serviceResult.status === 'already_claimed') {
        return jsonToolResult({
          status: 'already_claimed',
          assignment: serviceResult.assignment,
          pre_claim_warnings: [{
            kind: 'active_assignment',
            severity: 'blocker',
            assigned_agent: serviceResult.assignment?.agent_id ?? null,
            claimed_at: serviceResult.assignment?.claimed_at ?? null,
            message: `Task already has an active assignment by ${serviceResult.assignment?.agent_id ?? 'unknown'}.`,
          }],
        }, true);
      }
      const result: TaskLifecyclePayload = {
        status: 'claimed',
        assignment_id: serviceResult.assignment_id,
        task_number: taskNumber,
        identity_state: buildTaskLifecycleIdentityState({
          agentId,
          operation: 'task_lifecycle.claim',
          status: 'authorized',
          authorityBasis: genericEngineerAuthority.authority_basis ?? mismatchAuthority.authority_basis,
        }),
      };
      if (genericEngineerAuthority.status === 'ok') {
        result.role_mismatch_authority = genericEngineerAuthority.authority_basis;
        result.role_claim_warning = genericEngineerAuthority.role_claim_warning;
        result.pre_claim_warnings = [genericEngineerAuthority.role_claim_warning];
      }
      if (eligibility.preferredAgentId && eligibility.preferredAgentId !== agentId && eligibility.warning) {
        result.preferred_agent_warning = {
          kind: 'preferred_agent_mismatch',
          severity: 'requires_authority',
          warning: 'preferred_agent_mismatch',
          preferred_agent_id: eligibility.preferredAgentId,
          claiming_agent: agentId,
          message: eligibility.warning,
        };
        result.pre_claim_warnings = [...(Array.isArray(result.pre_claim_warnings) ? result.pre_claim_warnings : []), result.preferred_agent_warning];
        result.preferred_agent_mismatch_authority = mismatchAuthority.authority_basis;
      }
      const intentWarning = safeRecordClaimIntent(recordClaimIntent, {
        store,
        lifecycle,
        taskNumber,
        agentId,
        status: 'claimed',
        assignmentId: serviceResult.assignment_id,
        authorityBasis: genericEngineerAuthority.authority_basis ?? mismatchAuthority.authority_basis,
        preferredAgentWarning: result.preferred_agent_warning ?? null,
      });
      if (intentWarning) result.intent_recording_warning = intentWarning;
      if (identityWarning) result.identity_warning = identityWarning;
      return jsonToolResult(result);
    },

    task_lifecycle_continue: async (args: any) => {
      const taskNumber = numberField(args, 'task_number');
      const agentId = stringField(args, 'agent_id');
      const reason = stringField(args, 'reason');
      if (!taskNumber) throw new Error('task_number_required');
      if (!agentId) throw new Error('agent_id_required');
      if (!reason) throw new Error('reason_required');
      enforceSessionIdentity(agentId);
      const lifecycle = store.getLifecycleByNumber(taskNumber);
      if (!lifecycle) throw new Error(`task_not_found: ${taskNumber}`);

      const lineageGuard = inspectSupersededTaskGuard({ store, lifecycle, authorityBasis: args.authority_basis });
      if (lineageGuard.status === 'blocked') {
        return jsonToolResult({
          ...lineageGuard,
          status: 'blocked',
          operation: 'continue',
        }, true);
      }

      const eligibility = checkTaskRoleEligibilityLocal({ store, siteRoot, taskId: lifecycle.task_id, taskNumber, agentId });
      if (!eligibility.eligible) {
        return jsonToolResult({
          status: 'role_mismatch',
          schema: 'narada.task.claim.role_mismatch.v1',
          task_number: taskNumber,
          target_role: eligibility.targetRole,
          agent_role: eligibility.agentRole,
          role_resolution: eligibility.roleResolution,
          message: eligibility.warning,
          remediation: {
            summary: 'Use task_lifecycle_roster_admit with operator_direct_instruction authority or task_lifecycle_set_routing with explicit authority before continuing this task as the requested agent.',
            required_authority_basis: { kind: 'operator_direct_instruction', summary: '<operator authorized roster or routing repair>' },
          },
        }, true);
      }

      const result = await withAuthoredRosterJsonPreserved(siteRoot, () => continueTaskService({ cwd: siteRoot, taskNumber, agent: agentId, reason }), store);
      return jsonToolResult(result.result || result, result.exitCode !== 0);
    },

    task_lifecycle_unclaim: async (args: any) => {
      const taskNumber = numberField(args, 'task_number');
      const agentId = stringField(args, 'agent_id');
      const reason = stringField(args, 'reason') ?? 'mcp_unclaim';
      if (!taskNumber) throw new Error('task_number_required');
      if (agentId) enforceSessionIdentity(agentId);
      const serviceResult = await unclaimLifecycleTask({ siteRoot, store, taskNumber, agentId, reason });
      return jsonToolResult(serviceResult, ['not_claimed', 'claimed_by_other', 'closure_authority_blocks_unclaim'].includes(serviceResult.status));
    },
  };
}

function safeRecordClaimIntent(recordClaimIntent: any, input: any) {
  try {
    recordClaimIntent(input);
    return null;
  } catch (error) {
    return {
      status: 'failed',
      warning: 'claim_intent_recording_failed_after_primary_decision',
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

function validateGenericEngineerClaimAuthority({ args, eligibility, lifecycle, taskNumber, agentId }: GenericEngineerClaimAuthorityArgs) {
  if (eligibility.eligible) return { status: 'not_required', authority_basis: null, role_claim_warning: null };
  if (eligibility.targetRole !== 'engineer') return { status: 'not_applicable', authority_basis: null, role_claim_warning: null };
  if (typeof eligibility.agentRole !== 'string' || !eligibility.agentRole.endsWith('-engineer')) {
    return { status: 'not_applicable', authority_basis: null, role_claim_warning: null };
  }
  const authorityBasis = normalizeGenericEngineerAuthorityBasis(args.authority_basis);
  if (!authorityBasis) return { status: 'authority_required', authority_basis: null, role_claim_warning: null };
  const roleClaimWarning = {
    kind: 'generic_engineer_role_claim',
    severity: 'authority_recorded',
    task_number: taskNumber,
    target_role: eligibility.targetRole,
    agent_role: eligibility.agentRole,
    claiming_agent: agentId,
    message: eligibility.warning,
  };
  return {
    status: 'ok',
    authority_basis: {
      ...authorityBasis,
      task_id: lifecycle.task_id,
      task_number: taskNumber,
      target_role: eligibility.targetRole,
      agent_role: eligibility.agentRole,
      claiming_agent: agentId,
    },
    role_claim_warning: roleClaimWarning,
  };
}

function normalizeGenericEngineerAuthorityBasis(value: any) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const kind = typeof value.kind === 'string' ? value.kind.trim() : '';
  const summary = typeof value.summary === 'string' ? value.summary.trim() : '';
  if (kind !== 'operator_direct_instruction' || !summary) return null;
  return { kind, summary };
}
