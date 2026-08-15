import { DEFAULT_INLINE_PAYLOAD_CHAR_LIMIT } from '../mcp-payload-file.js';
import { normalizeTaskTags, parseStoredTaskTags } from '@narada-core/task-governance-core/task-tags';
import { withSqliteBusyRetry, withStoreSavepoint } from './sqlite-contention.js';
import { buildCompactExecutabilityPosture } from './task-lifecycle-executability-handlers.js';

export const TASK_LIFECYCLE_READ_TOOL_NAMES: any = Object.freeze([
  'task_lifecycle_list',
  'task_lifecycle_roster',
  'task_lifecycle_guidance',
  'task_lifecycle_payload_schema',
]);

export function createTaskLifecycleReadHandlers({
  store,
  siteRoot,
  jsonToolResult,
  stringField,
  numberField,
  getSitePolicy,
}: any) {
  return {
    task_lifecycle_list: async (args: any) => {
      const statusFilter: any = stringField(args, 'status');
      const agentFilter: any = stringField(args, 'agent_id');
      const limit: any = Math.max(1, Math.min(numberField(args, 'limit') ?? 50, 200));
      const tagFilter: any = normalizeTaskTags(args?.tags);
      const tagMatch: any = stringField(args, 'tag_match') ?? 'all';
      if (!['any', 'all'].includes(tagMatch)) throw new Error('tag_match_must_be_any_or_all');
      const snapshot: any = await withSqliteBusyRetry(() => withStoreSavepoint(store, () => {
        const needsFullScan: any = Boolean(statusFilter || agentFilter || tagFilter.length > 0);
        const rows: any = needsFullScan
          ? store.db.prepare('SELECT * FROM task_lifecycle ORDER BY task_number DESC').all()
          : store.db.prepare('SELECT * FROM task_lifecycle ORDER BY task_number DESC LIMIT ?').all(limit);
        const tasks: any = rows.map((row: any) => {
          const spec: any = store.getTaskSpec(row.task_id);
          const assignment: any = store.db.prepare('SELECT * FROM task_assignments WHERE task_id = ? AND released_at IS NULL ORDER BY claimed_at DESC LIMIT 1').get(row.task_id);
          const tags: any = parseStoredTaskTags(spec?.tags_json);
          const projectionConsistency: any = classifyTaskListProjectionConsistency({
            lifecycle: row,
            latestOutcome: store.getLatestTaskOutcome?.(row.task_id) ?? null,
            activeAssignment: assignment ?? null,
          });
          return {
            task_number: row.task_number,
            task_id: row.task_id,
            task_ref: buildStableTaskReference({ siteRoot, lifecycle: row, spec }).task_ref,
            task_reference: buildStableTaskReference({ siteRoot, lifecycle: row, spec }),
            status: row.status,
            title: spec?.title ?? null,
            assigned_to: assignment?.agent_id ?? null,
            claimed_at: assignment?.claimed_at ?? null,
            tags,
            updated_at: row.updated_at,
            projection_consistency: projectionConsistency,
            executability_posture: buildCompactExecutabilityPosture({ store, taskId: row.task_id, siteRoot }),
          };
        });
        const filtered: any = tasks.filter((task: any) => {
          if (statusFilter && task.status !== statusFilter) return false;
          if (agentFilter && task.assigned_to !== agentFilter) return false;
          if (tagFilter.length > 0) {
            const matches: any = tagFilter.filter((tag: any) => task.tags.includes(tag)).length;
            if (tagMatch === 'all' && matches !== tagFilter.length) return false;
            if (tagMatch === 'any' && matches === 0) return false;
          }
          return true;
        }).slice(0, limit);
        const staleTasks: any = filtered.filter((task: any) => task.projection_consistency.status === 'stale');
        return { tasks, filtered, staleTasks };
      }));
      const consistencyStatus: any = snapshot.value.staleTasks.length > 0
        ? 'stale'
        : snapshot.retries > 0
          ? 'contention_observed'
          : 'snapshot_coherent';
      return jsonToolResult({
        status: 'ok',
        count: snapshot.value.filtered.length,
        filters: { status: statusFilter ?? null, agent_id: agentFilter ?? null, tags: tagFilter, tag_match: tagMatch },
        projection_consistency: {
          status: consistencyStatus,
          stale: snapshot.value.staleTasks.length > 0,
          snapshot_isolation: 'sqlite_savepoint',
          scanned_count: snapshot.value.tasks.length,
          returned_count: snapshot.value.filtered.length,
          stale_count: snapshot.value.staleTasks.length,
          contention: { attempts: snapshot.attempts, retries: snapshot.retries },
          stale_tasks: snapshot.value.staleTasks.map((task: any) => ({
            task_number: task.task_number,
            task_id: task.task_id,
            reasons: task.projection_consistency.reasons,
            expected_status: task.projection_consistency.expected_status,
          })),
          authoritative_detail_tool: 'task_lifecycle_show',
          repair_tool: snapshot.value.staleTasks.length > 0 ? 'task_lifecycle_compatibility_reconcile' : null,
        },
        tasks: snapshot.value.filtered,
      });
    },
    review_contract_defaults: {
      ordinary_finish: 'Every ordinary task finish creates or reuses one review-contract dependency. An explicit reviewer is optional; routing defaults to the first reviewer-capable roster agent and then the reviewer role.',
      review_execution: 'Claim the generated review task and finish it with task_lifecycle_finish outcome plus findings. Do not call task_lifecycle_review for tasks that already have a review dependency.',
      compatibility_migration: 'task_lifecycle_review remains only for pre-existing tasks with no review-contract dependency.',
      singleton_reviewer: 'When the site has exactly one reviewer-capable agent, same-operator review is admitted and annotated automatically. No boolean flag is required; multi-reviewer conflict policy still requires explicit authorization.',
    },
    runtime_freshness_discovery: {
      tool: 'mcp_runtime_proxy_status',
      statuses: ['current', 'stale', 'unknown'],
      restart_action_path: 'runtime_freshness.reload_action',
      note: 'This proxy-owned tool is advertised alongside task-lifecycle tools on carrier-bound sessions. It is distinct from task_lifecycle_restart request markers and does not restart automatically.',
    },
    fresh_server_recovery: {
      tool: 'task_lifecycle_test_mcp_tool',
      use_when: 'The session-bound task-lifecycle server is wedged or demonstrably stale and a one-shot fresh-process verification is needed without restarting the session.',
      path_policy: 'server_path may be site-root-relative, or absolute only under the site root, the running task-lifecycle package root, or NARADA_TASK_LIFECYCLE_FRESH_SERVER_ALLOWED_ROOTS.',
      payload_route: 'Pass payload_ref on task_lifecycle_test_mcp_tool and put required child identity/routing fields in arguments. The parent resolves the immutable payload and passes merged arguments to the fresh child; arguments wins for fields such as task_number and agent_id.',
      warning: 'A fresh one-shot result is recovery evidence, not proof that the carrier-bound server has reloaded. Use mcp_runtime_proxy_status for that process.',
    },
    restart_lifecycle: {
      request_tool: 'task_lifecycle_restart',
      modes: ['request', 'status', 'acknowledge', 'clear'],
      completion: 'A real replacement task-lifecycle child automatically acknowledges and clears a pending marker from post-request self-observed boot evidence. acknowledge and clear are idempotent after automatic reconciliation.',
      build_pinning: 'A session-bound MCP server runs the build loaded when its child process started. Source or build fixes do not appear in that process until the carrier/runtime supervisor performs a real child restart; task_lifecycle_test_mcp_tool is only a one-shot recovery route.',
    },
    task_lifecycle_roster: () => {
      const roster: any = store.getRoster();
      return jsonToolResult({ status: 'ok', roster: roster ?? [] });
    },
    task_lifecycle_guidance: (args: any) => {
      const workflow: any = stringField(args, 'workflow') ?? 'all';
      const tool: any = stringField(args, 'tool');
      return jsonToolResult(taskLifecycleGuidance({ workflow, tool, sitePolicy: getSitePolicy() }));
    },
    task_lifecycle_payload_schema: (args: any) => {
      const tool: any = stringField(args, 'tool');
      const schemas: any = taskLifecyclePayloadSchemas();
      return jsonToolResult({
        status: 'ok',
        schema: 'narada.task_lifecycle.payload_schema.v0',
        tool: tool ?? null,
        schemas: tool ? { [tool]: schemas[tool] ?? null } : schemas,
        remediation: 'Use mcp_payload_create with one of these payload shapes, then retry the lifecycle tool with payload_ref plus required top-level identity/routing fields.',
      });
    },
  };
}

function buildStableTaskReference({ siteRoot, lifecycle, spec, taskFile = null }: {
  siteRoot: string;
  lifecycle: Record<string, unknown>;
  spec?: Record<string, unknown> | null;
  taskFile?: string | null;
}) {
  const taskNumber: any = Number(lifecycle.task_number);
  const taskId: any = String(lifecycle.task_id ?? '');
  const goalRef: any = firstReference(spec, ['goal_ref', 'goal_id', 'goal_number', 'source_goal_ref']);
  return {
    schema: 'narada.task.reference.v1',
    task_ref: `task #${taskNumber}`,
    task_id: taskId,
    task_number: taskNumber,
    number_authority: 'task_lifecycle',
    task_file_name: taskFile ? taskFile.split(/[\\/]/).pop() : `${taskId}.md`,
    goal_ref: goalRef,
    cross_reference_key: `${taskId}@${taskNumber}`,
  };
}

function firstReference(spec: Record<string, unknown> | null | undefined, keys: string[]) {
  for (const key of keys) {
    const value: any = spec?.[key];
    if (typeof value === 'string' && value.trim()) return value.trim();
    if (typeof value === 'number' && Number.isFinite(value)) return value;
  }
  return null;
}

export function classifyTaskListProjectionConsistency({ lifecycle, latestOutcome, activeAssignment }: any) {
  const compatibilityReview: any = lifecycle?.governed_by === 'review'
    && String(lifecycle?.task_id ?? '').includes('-legacy-review-');
  const reasons: string[] = [];
  let expectedStatus: string | null = null;
  if (compatibilityReview && latestOutcome && lifecycle?.status !== 'closed') {
    reasons.push('admitted_review_outcome_not_projected_to_lifecycle');
    expectedStatus = 'closed';
  }
  if (compatibilityReview && latestOutcome && lifecycle?.status === 'closed'
    && !(lifecycle.closed_at && lifecycle.closed_by && lifecycle.closure_mode)) {
    reasons.push('closed_lifecycle_missing_closure_evidence');
    expectedStatus = 'closed';
  }
  if (compatibilityReview && lifecycle?.status === 'closed' && activeAssignment) {
    reasons.push('closed_lifecycle_retains_active_assignment');
    expectedStatus = 'closed';
  }
  return {
    status: reasons.length > 0 ? 'stale' : 'coherent',
    checked: compatibilityReview,
    reasons,
    expected_status: expectedStatus,
    observed_status: lifecycle?.status ?? null,
    latest_outcome: latestOutcome?.outcome ?? null,
    remediation: reasons.length > 0
      ? 'Inspect with task_lifecycle_show, then run task_lifecycle_compatibility_reconcile for the affected task.'
      : null,
  };
}

function taskLifecycleGuidance({ workflow, tool, sitePolicy }: any) {
  const sections: any = taskLifecycleGuidanceSections();
  const normalizedWorkflow: any = sections[workflow] ? workflow : 'all';
  const selectedSections: any = normalizedWorkflow === 'all'
    ? sections
    : { [normalizedWorkflow]: sections[normalizedWorkflow] };
  return {
    status: 'ok',
    schema: 'narada.task_lifecycle.guidance.v0',
    common_guidance_contract_schema: 'narada.mcp_surface.guidance.v0',
    surface_id: 'task-lifecycle',
    guidance_tool: 'task_lifecycle_guidance',
    purpose: 'Task lifecycle MCP surface for claiming, executing, evidencing, reviewing, blocking, and truthfully finishing task work.',
    requested: { workflow: workflow ?? null, tool: tool ?? null },
    workflow: normalizedWorkflow,
    tool: tool ?? null,
    first_use: [
      'Call task_lifecycle_guidance when you first see a lifecycle task, when a refusal is unclear, or before recovering from incomplete task evidence.',
      'Inspect the task before mutation; claim only when authorized; submit execution notes, verification, changed-file evidence, and closeout through lifecycle tools.',
      'Use payload_ref for long companion fields and preserve structuredContent as authoritative lifecycle evidence.',
      'Use task_lifecycle_tags_update for audited site-local labels; tags are descriptive and never replace routing, authorization, priority, dependency, review, or closure state.',
      'The carrier-facing runtime never prepares SQLite: run task-lifecycle-mcp --prepare --site-root <site-root> once before stateful calls. If a stateful call returns task_lifecycle_store_not_prepared, inspect task_lifecycle_doctor.preparation and perform explicit preparation instead of retrying.',
      'Call mcp_runtime_proxy_status when this carrier-bound surface may be running an old build; inspect runtime_freshness.status and execute only the machine-readable carrier/supervisor reload_action.',
    ],
    sections: selectedSections,
    first_use_decision_tree: taskLifecycleFirstUseDecisionTree(),
    state_truth_table: taskLifecycleStateTruthTable(),
    tool_preference_table: taskLifecycleToolPreferenceTable(),
    tool_preference: taskLifecycleToolPreferenceTable(),
    happy_path_examples: taskLifecycleHappyPathExamples(),
    examples: taskLifecycleHappyPathExamples(),
    anti_patterns: taskLifecycleAntiPatterns(),
    recovery_guidance: taskLifecycleRecoveryGuidance(),
    recovery: taskLifecycleRecoveryGuidance(),
    site_policy: sitePolicy,
    role_obligation_targeting: {
      default: false,
      config_file: '<site-root>/.narada/task-lifecycle.toml',
      config_key: 'roster.roles_are_obligation_targets',
      guidance: 'Roster roles are descriptive unless roles_are_obligation_targets is true; diagnostics may mention affected roles without creating role-targeted obligations.',
    },
    feedback: {
      surface_id: 'task-lifecycle',
      tool: 'surface_feedback_submit',
      when: [
        'guidance is missing, stale, or contradicted by live lifecycle behavior',
        'payload_ref shape or evidence gates are unclear',
        'task state, review state, or closeout requirements are hard to recover from',
      ],
    },
    boundaries: [
      'Guidance is read-only model-facing operating advice.',
      'Guidance does not weaken task lifecycle policy, authorize mutation, or replace tool schemas.',
      'Lifecycle state and completion gates remain authoritative in task lifecycle records.',
    ],
    recommended_first_call: tool ? null : 'task_lifecycle_guidance({ workflow: "ordinary_task" })',
    tool_specific_note: tool ? taskLifecycleToolGuidance(tool) : null,
  };
}

function taskLifecycleFirstUseDecisionTree() {
  return [
    {
      condition: 'You have a task number.',
      sequence: ['task_lifecycle_show', 'task_lifecycle_reopen or task_lifecycle_continue if terminal', 'task_lifecycle_claim if unclaimed and claimable', 'do the work', 'task_lifecycle_submit_work'],
    },
    {
      condition: 'You do not have a task number.',
      sequence: ['task_lifecycle_next', 'claim the recommended task when appropriate', 'do the work', 'task_lifecycle_submit_work'],
    },
    {
      condition: 'You are blocked by missing input, authority, credentials, failing infrastructure, or unclear external state.',
      sequence: ['task_lifecycle_report_blocked with exact blocker facts and next_action'],
    },
    {
      condition: 'You are completing review/dependency work.',
      sequence: ['task_lifecycle_show to read outcome contract and terminal state', 'task_lifecycle_reopen or task_lifecycle_continue if terminal', 'task_lifecycle_finish with outcome and findings'],
    },
    {
      condition: 'The carrier-bound server is stale or wedged.',
      sequence: ['mcp_runtime_proxy_status', 'task_lifecycle_test_mcp_tool with an admitted server_path for one-shot recovery evidence', 'route runtime_freshness.reload_action to the carrier/runtime supervisor'],
    },
    {
      condition: 'Your summary, verification, findings, or blocker details are too long for inline fields.',
      sequence: ['mcp_payload_create with companion fields under payload', 'retry lifecycle tool with payload_ref plus top-level task_number and agent_id'],
    },
  ];
}

function taskLifecycleStateTruthTable() {
  return {
    opened: 'Task exists and is available or waiting; no current agent responsibility is implied.',
    claimed: 'An agent has accepted responsibility; no completion is implied. A closed or confirmed task must not enter this state without an explicit reopen/continue trace.',
    submitted: 'A report or evidence packet was recorded by a tool call; closure is not implied.',
    completed_outcome: 'A task_outcome was admitted as completed; closure may still be gated by review or dependencies.',
    in_review: 'Submitted work awaits review or dependency satisfaction; do not report this as closed.',
    awaiting_dependencies: 'Parent work is not closed because one or more dependencies must satisfy outcomes first.',
    closed: 'Closure authority has been recorded; this is the normal terminal claim for task completion. Claiming or submitting a new outcome requires task_lifecycle_reopen or task_lifecycle_continue first.',
    confirmed: 'A stronger finalization state when supported by local policy; treat as at least closed and require an explicit reopen/continue trace before new work or outcomes.',
    deferred: 'Work is intentionally paused; do not treat as completed.',
    blocked: 'Known blocker prevents truthful completion until resolved or explicitly dispositioned.',
  };
}

function taskLifecycleToolPreferenceTable() {
  return [
    { tool: 'task_lifecycle_next', prefer_when: 'Finding actionable work without a task number.', avoid_when: 'You already have a specific task to inspect or finish.' },
    { tool: 'task_lifecycle_show', prefer_when: 'Reading the authoritative task state, assignment, criteria, dependencies, and latest outcome.', avoid_when: 'You need a compact multi-task overview.' },
    { tool: 'task_lifecycle_claim', prefer_when: 'Taking responsibility for unclaimed work.', avoid_when: 'The task is already claimed by you or you lack real authority to cross routing gates.' },
    { tool: 'task_lifecycle_submit_work', prefer_when: 'Ordinary implementation or investigation closeout with notes, verification, evidence, and finish.', avoid_when: 'You are blocked or completing a specialized outcome-contract task.' },
    { tool: 'task_lifecycle_finish', prefer_when: 'Lower-level finish, no-edit report, or dependency/review outcome admission.', avoid_when: 'You still need to write execution/verification notes and prove criteria for ordinary work.' },
    { tool: 'task_lifecycle_report_blocked', prefer_when: 'A known unresolved blocker prevents truthful completion.', avoid_when: 'Work is complete and only needs a normal report.' },
    { tool: 'task_lifecycle_payload_schema', prefer_when: 'You need exact payload_ref shapes for a lifecycle tool.', avoid_when: 'You only need workflow guidance.' },
    { tool: 'mcp_payload_create', prefer_when: 'Companion fields are long or structurally rich.', avoid_when: 'Short ordinary fields fit the lifecycle tool schema.' },
  ];
}

function taskLifecycleHappyPathExamples() {
  return {
    ordinary_submit_work_inline: {
      tool: 'task_lifecycle_submit_work',
      arguments: {
        task_number: 123,
        agent_id: '<agent id>',
        summary: 'Implemented the requested behavior.',
        execution_notes: 'Changed the focused implementation path and preserved existing gates.',
        verification: 'Ran the focused package test and inspected the task readback.',
        changed_files: ['packages/example/src/main.ts'],
      },
    },
    tags: {
      intent: 'Maintain small, site-local descriptive labels for discovery and related-task suggestions.',
      canonical_sequence: ['task_lifecycle_show or task_lifecycle_list', 'task_lifecycle_tags_update with the complete desired tag set and a reason', 're-read task_lifecycle_show to inspect the audited update'],
      semantics: {
        normalization: 'Tags are trimmed, lowercased, converted to kebab-case, deduplicated, and bounded to 20 tags of at most 64 characters each.',
        scope: 'Tags belong to this Site only.',
        authority: 'The active task owner or an architect/operator may update them.',
        boundaries: 'Tags never affect authorization, routing, priority, dependencies, review, or closure.',
        relatedness: 'Explicit overlap is preferred; derived title/goal/context terms are only a fallback for untagged or legacy tasks.',
      },
    },
    ordinary_submit_work_auto_materialized: {
      tool: 'task_lifecycle_submit_work',
      use_when: 'execution_notes, verification, summary, or changed_files are too long for inline transport and you want one governed call instead of a separate mcp_payload_create call',
      arguments: {
        task_number: 123,
        agent_id: '<agent id>',
        summary: '<long summary>',
        execution_notes: '<long execution notes>',
        verification: '<long verification notes>',
        changed_files: ['packages/example/src/main.ts'],
        auto_materialize_payload: true,
      },
      result_contract: 'payload_source.kind=auto_materialized_payload and long_field_transport=auto_materialized_payload',
    },
    resume_existing_submit_work: {
      tool: 'task_lifecycle_submit_work',
      use_when: 'A prior submit_work already recorded substantive task notes and admitted a report, but a later lifecycle gate or restart requires continuing the same closeout.',
      arguments: {
        task_number: 123,
        agent_id: '<same submitting agent id>',
        resume_existing_work: true,
      },
      result_contract: 'Existing Execution Notes and Verification are validated and reused; latest same-agent report summary and changed-file evidence are reused; proof and admission are skipped unless explicitly requested.',
    },
    no_files_changed_finish: {
      tool: 'task_lifecycle_finish',
      arguments: {
        task_number: 124,
        agent_id: '<agent id>',
        summary: 'Inspection-only task completed; no repository files changed.',
        no_files_changed: true,
      },
    },
    blocked_report: {
      tool: 'task_lifecycle_report_blocked',
      arguments: {
        task_number: 125,
        agent_id: '<agent id>',
        reason: 'Required credential is missing.',
        blockers: [{ kind: 'credential_missing', detail: 'The configured token is absent from the runtime environment.' }],
        next_action: 'Operator must add the credential and restart the carrier.',
      },
    },
  };
}

function taskLifecycleAntiPatterns() {
  return [
    { mistake: 'Saying a task is closed when the lifecycle status is submitted, in_review, awaiting_dependencies, blocked, or deferred.', correction: 'Report the exact lifecycle status and whether closure authority exists.' },
    { mistake: 'Putting task_number or agent_id only inside a payload.', correction: 'Keep authority/routing fields top-level; payload carries companion fields.' },
    { mistake: 'Calling finish or submit_work when blocker facts remain unresolved.', correction: 'Use task_lifecycle_report_blocked with blockers and next_action.' },
    { mistake: 'Omitting changed_files and no_files_changed.', correction: 'Provide changed-file evidence or explicitly declare no_files_changed.' },
    { mistake: 'Treating generated reports, review artifacts, or model narration as self-authorizing closure.', correction: 'Use lifecycle readback: outcome, evidence verdict, dependencies, and closure status.' },
    { mistake: 'Forcing authority_basis to bypass routing without real operator/task-owner authority.', correction: 'Only use authority_basis when the authority exists and can be summarized truthfully.' },
    { mistake: 'Using tags to imply ownership, urgency, dependency, review, or closure.', correction: 'Treat tags as descriptive site-local labels and use the authoritative lifecycle/routing fields for state and authority.' },
  ];
}

function taskLifecycleRecoveryGuidance() {
  return [
    { failure: 'Inline field too long or structurally awkward.', action: 'Call task_lifecycle_payload_schema for the target tool, create mcp_payload_create with companion fields under payload, then retry with payload_ref.' },
    { failure: 'Authorization or routing refusal.', action: 'Read task_lifecycle_show and roster/routing state; claim only if eligible or provide a truthful authority_basis when explicitly authorized.' },
    { failure: 'Evidence rejected or acceptance criteria unchecked.', action: 'Fix task notes, verification, changed-file/no-files evidence, and criteria proof before retrying finish.' },
    { failure: 'Review or dependency is blocking closure.', action: 'Find the dependency task or review obligation, finish it with the required outcome, then re-check parent dependency satisfaction.' },
    { failure: 'Tool result is ambiguous or live MCP seems stale after source edits.', action: 'Record the observed result exactly, verify source with focused tests, and request/rely on carrier restart before claiming live behavior changed.' },
    { failure: 'Tag update is refused.', action: 'Re-read the task, use the active task owner or an architect/operator identity, pass the complete normalized tag set, and include a concise reason.' },
  ];
}

function taskLifecycleGuidanceSections() {
  return {
    ordinary_task: {
      intent: 'Do task work through explicit lifecycle records instead of conversational claims.',
      canonical_sequence: [
        'task_lifecycle_show or task_lifecycle_next',
        'task_lifecycle_claim when unclaimed',
        'do the implementation or investigation work',
        'task_lifecycle_submit_work for normal completion',
        'read the returned lifecycle status before reporting done',
      ],
      required_evidence: ['execution_notes', 'verification', 'changed_files or no_files_changed', 'acceptance criteria proof'],
      state_semantics: {
        submitted: 'A report/evidence packet was recorded.',
        in_review: 'Work is submitted but closure is gated by review or dependency policy.',
        closed: 'Closure authority has been recorded.',
      },
    },
    blocked_task: {
      intent: 'Record exact blockers without pretending the task is complete.',
      canonical_sequence: ['task_lifecycle_report_blocked', 'include blocker facts and next_action', 'do not use finish for unresolved blockers'],
      required_evidence: ['reason', 'blockers when specific facts exist', 'next_action when an external action is required'],
    },
    payloads: {
      intent: 'Use immutable payload refs for long companion fields while keeping task_number and agent_id top-level.',
      canonical_sequence: ['mcp_payload_create', 'call lifecycle tool with payload_ref plus required top-level identity/routing fields'],
      inline_limit_guidance: `Inline companion strings over ${DEFAULT_INLINE_PAYLOAD_CHAR_LIMIT} characters should move to payload_ref when the target tool supports it.`,
      top_level_authority_fields: ['task_number', 'agent_id', 'authority_basis when required'],
      companion_fields: ['summary', 'execution_notes', 'verification', 'changed_files', 'findings', 'recovery_truthfulness', 'self_certification'],
      examples: [
        {
          purpose: 'Submit ordinary work with long notes.',
          create_payload: {
            payload_id: 'task-123-submit-work-notes',
            payload: {
              summary: 'Implemented the requested behavior.',
              execution_notes: '<long execution notes>',
              verification: '<long verification notes>',
              changed_files: ['packages/example/src/main.ts'],
            },
            created_by: '<agent id>',
          },
          consume_payload_ref: {
            tool: 'task_lifecycle_submit_work',
            arguments: {
              task_number: 123,
              agent_id: '<agent id>',
              payload_ref: 'mcp_payload:task-123-submit-work-notes@v1',
            },
          },
        },
        {
          purpose: 'Finish an outcome-contract task with structured findings.',
          create_payload: {
            payload_id: 'task-456-review-outcome',
            payload: {
              outcome: 'accepted_with_notes',
              summary: 'The implementation satisfies the contract.',
              findings: [{ severity: 'note', description: 'Minor follow-up remains non-blocking.' }],
            },
            created_by: '<agent id>',
          },
          consume_payload_ref: {
            tool: 'task_lifecycle_finish',
            arguments: {
              task_number: 456,
              agent_id: '<agent id>',
              payload_ref: 'mcp_payload:task-456-review-outcome@v1',
            },
          },
        },
        {
          purpose: 'Report a blocked task with detailed blocker evidence.',
          create_payload: {
            payload_id: 'task-789-blocked-report',
            payload: {
              reason: 'External credential is missing.',
              blockers: [{ kind: 'operator_input_required', detail: '<long concrete evidence>' }],
              next_action: '<long exact action needed to unblock continuation>',
            },
            created_by: '<agent id>',
          },
          consume_payload_ref: {
            tool: 'task_lifecycle_report_blocked',
            arguments: {
              task_number: 789,
              agent_id: '<agent id>',
              payload_ref: 'mcp_payload:task-789-blocked-report@v1',
            },
          },
        },
      ],
    },
    review_and_dependencies: {
      intent: 'Review is dependency/outcome work, not a magic final state.',
      canonical_sequence: ['submit_work with reviewer when ordinary work needs review', 'reviewer claims dependency task', 'reviewer finishes with accepted/accepted_with_notes/rejected outcome'],
      state_semantics: {
        completed_outcome: 'An outcome was admitted for the task.',
        dependency_satisfied: 'Parent gating dependency has an admitted satisfying outcome.',
        rejected_outcome: 'Blocks parent closure until disposition is recorded.',
      },
    },
    closeout_truthfulness: {
      intent: 'Report the strongest true lifecycle state, not the desired state.',
      required_checks: ['latest tool result status', 'final_lifecycle_status or task show status', 'closure evidence', 'review/dependency obligations', 'working tree and verification evidence'],
      common_mistakes: [
        'Calling submitted or in_review closed.',
        'Omitting changed_files or no_files_changed.',
        'Using finish when blocked facts remain unresolved.',
        'Treating generated reports as self-authorizing closure.',
      ],
    },
  };
}

function taskLifecycleToolGuidance(tool: any) {
  const guidance: any = {
    task_lifecycle_submit_work: {
      preferred_for: 'Ordinary task completion with execution notes, verification, evidence admission, and finish/report in one call.',
      caveat: 'A successful submit_work can still return in_review or awaiting_dependencies rather than closed. Use resume_existing_work:true only to continue prior same-agent admitted work without rewriting notes or duplicating proof/admission. Inline companion fields are accepted up to the governed threshold; use payload_ref or opt in with auto_materialize_payload:true for larger artifacts.',
    },
    task_lifecycle_finish: {
      preferred_for: 'Finishing a claimed task or admitting an outcome for an outcome-contract dependency task.',
      caveat: 'Use inline recovery_truthfulness for ordinary recovery packets under the governed threshold; use payload_ref for larger summary/findings/guard packets and include changed_files or no_files_changed for implementation work. When evidence_refs are supplied, use a successful structured_command_execution:<execution_ref> or test_mcp_artifact:<artifact_id>; the finish gate dereferences and verifies those refs. mcp_output refs are diagnostic only, and copied logs, exit files, wrappers, transient paths, and untyped narrative refs are refused.',
    },
    task_lifecycle_report_blocked: {
      preferred_for: 'Recording unresolved blockers with exact next action.',
      caveat: 'Do not use completion tools when the blocker prevents truthful finish.',
    },
    task_lifecycle_claim: {
      preferred_for: 'Taking responsibility for unassigned work.',
      caveat: 'Use authority_basis when crossing role, preferred-agent, or operator gates.',
    },
    task_lifecycle_tags_update: {
      preferred_for: 'Replacing a task\'s complete site-local tag set with an auditable before/after record.',
      caveat: 'Pass the complete desired set, including [] to clear tags. Tags do not route, prioritize, authorize, review, or close work.',
    },
  };
  return guidance[tool] ?? {
    preferred_for: null,
    caveat: 'No tool-specific guidance is registered; use the workflow sections and tool schema together.',
  };
}

function taskLifecyclePayloadSchemas() {
  return {
    task_lifecycle_submit_work: {
      payload_ref_shape: { summary: '<finish summary>', execution_notes: '<Execution Notes replacement>', verification: '<Verification replacement>', changed_files: ['path/to/file'], no_files_changed: false, resume_existing_work: false, self_certification: {}, recovery_truthfulness: {} },
      inline_payload_limit: { threshold_chars: DEFAULT_INLINE_PAYLOAD_CHAR_LIMIT, long_fields: ['summary', 'execution_notes', 'verification', 'changed_files'], remediation: 'Inline fields are accepted up to the governed threshold. Larger inline fields are refused with mcp_payload_create plus payload_ref remediation. For one governed submit_work call, pass auto_materialize_payload:true; the result must include payload_source.kind=auto_materialized_payload.' },
      one_call_fallback: { field: 'auto_materialize_payload', value: true, audit_contract: 'Creates an immutable transient payload artifact and reports it in payload_source.' },
      top_level_fields_remain_required: ['task_number', 'agent_id'],
    },
    task_lifecycle_test_mcp_tool: {
      payload_ref_shape: { summary: '<long child-tool field>', findings: [], changed_files: ['path/to/file'] },
      payload_route: 'payload_ref is resolved by the parent and merged into arguments before spawning the fresh child.',
      top_level_wins: 'Fields in arguments override payload fields, including task_number and agent_id.',
      control_fields_remain_top_level: ['server_path', 'tool_name', 'timeout_seconds'],
    },
    task_lifecycle_review: {
      compatibility_only: true,
      authority_model: 'Migrates legacy review calls into review-contract dependency work and task_outcomes authority. New review work should finish the dependency task with task_lifecycle_finish.',
      payload_ref_shape: { findings: [{ severity: 'note|blocking', description: '<finding text>', location: '<optional location>' }] },
      top_level_fields_remain_required: ['task_number', 'agent_id', 'verdict'],
      invalid_shapes: ['{ findings: { "0": {...} } }', '{ findings: ["text"] }'],
      preferred_tool_for_new_review_work: 'task_lifecycle_finish',
    },
    task_lifecycle_finish: {
      payload_ref_shape: { summary: '<finish summary>', outcome: '<contract outcome when applicable>', findings: [], evidence_refs: ['structured_command_execution:<execution_ref>'], changed_files: ['path/to/file'], no_files_changed: false, self_certification: {}, recovery_truthfulness: {} },
      inline_payload_limit: { threshold_chars: DEFAULT_INLINE_PAYLOAD_CHAR_LIMIT, remediation: 'Inline recovery_truthfulness and other companion fields are accepted up to the governed threshold. Put larger summary, findings, outcome evidence, or guard packets in mcp_payload_create, then call task_lifecycle_finish with payload_ref plus top-level task_number and agent_id.' },
      top_level_fields_remain_required: ['task_number', 'agent_id'],
    },
    task_lifecycle_disposition_closeout: {
      canonical_tool: 'task_lifecycle_disposition_closeout',
      accepted_legacy_alias: 'task_lifecycle_closeout',
      payload_ref_shape: { summary: '<closeout summary>', changed_files: ['path/to/file'], no_files_changed: false, reviewer: '<optional admitted reviewer agent id or unique reviewer role alias>' },
      inline_payload_limit: { threshold_chars: DEFAULT_INLINE_PAYLOAD_CHAR_LIMIT, long_fields: ['summary'], remediation: 'Put closeout summaries above the governed inline threshold and optional changed_files/no_files_changed/reviewer in mcp_payload_create, then call task_lifecycle_disposition_closeout with payload_ref plus top-level task_number and agent_id.' },
      top_level_fields_remain_required: ['task_number', 'agent_id'],
      review_routing: 'The optional reviewer is routed through a review-contract dependency; when omitted, the first reviewer-capable roster agent is selected.',
    },
    task_lifecycle_report_blocked: {
      payload_ref_shape: { reason: '<concise blocker summary>', blockers: [{ kind: '<blocker kind>', detail: '<details>' }], next_action: '<long concrete unblock action>', defer: true },
      inline_payload_limit: { threshold_chars: DEFAULT_INLINE_PAYLOAD_CHAR_LIMIT, long_fields: ['next_action', 'blockers'], remediation: 'Put next_action and blocker details above the governed inline threshold in mcp_payload_create, then call task_lifecycle_report_blocked with payload_ref plus top-level task_number and agent_id. Keep reason concise inline or in the payload.' },
      top_level_fields_remain_required: ['task_number', 'agent_id'],
    },
    task_lifecycle_create: {
      payload_ref_shape: {
        title: '<required task title>',
        goal: '<optional goal; defaults to title>',
        context: '<optional context>',
        required_work: '<markdown string or string[] normalized to newline markdown>',
        non_goals: '<markdown string or string[] normalized to newline markdown>',
        acceptance_criteria: ['string criteria item'],
        tags: ['incident', 'mcp-surface'],
        preferred_role: '<optional role>',
        target_role: '<optional role>',
        recovery_truthfulness_required: false,
        idempotency_key: '<stable retry key; omitted values derive from payload_ref>',
        execution_binding: {
          workspace_root: '<absolute workspace root; defaults to this Site root>',
          repository_root: '<absolute repository root when the task executes repository work>',
          site_root: '<absolute destination Site root; defaults to this task-lifecycle surface Site>',
          executor_kind: 'manual | operator | worker_delegation | delegated_task',
          correlation_key: '<stable result correlation key; defaults to idempotency_key>',
        },
      },
      examples: [
        { payload: { title: 'Fix thing', required_work: ['Inspect failure.', 'Patch narrowly.'], non_goals: ['No unrelated refactor.'], acceptance_criteria: ['Focused test passes.'] } },
      ],
      payload_ref_required: true,
      inline_definition_fields_refused: ['title', 'goal', 'context', 'required_work', 'non_goals', 'acceptance_criteria', 'tags', 'preferred_role', 'target_role', 'recovery_truthfulness_required', 'idempotency_key', 'execution_binding'],
      normalized_fields: { required_work: 'string[] joins with newline after trimming empty entries', non_goals: 'string[] joins with newline after trimming empty entries' },
    },
    task_lifecycle_admit_evidence: {
      payload_ref_shape: { self_certification: { actor_principal: '<agent id>', findings: [], justification: '<why this evidence may be admitted>' } },
      top_level_fields_remain_required: ['task_number', 'agent_id'],
      note: 'Evidence content is read from the task lifecycle store; payload_ref is only for long self_certification guard metadata.',
    },
  };
}
