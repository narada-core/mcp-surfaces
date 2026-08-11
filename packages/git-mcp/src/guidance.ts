export type GuidanceRecord = Record<string, unknown>;
export type GuidanceToolDefinition = GuidanceRecord & { name: string; description: string; inputSchema: GuidanceRecord; annotations: GuidanceRecord; outputSchema: GuidanceRecord };

const SURFACE_ID = "git";
const GUIDANCE_TOOL = "git_guidance";
const PURPOSE = "Governed Git inspection, branch lifecycle, remote synchronization, staging, commit, and push workflows.";

export function buildGuidanceResult(args: GuidanceRecord = {}): GuidanceRecord {
  const workflow = typeof args.workflow === 'string' && args.workflow.trim() ? args.workflow.trim() : null;
  const tool = typeof args.tool === 'string' && args.tool.trim() ? args.tool.trim() : null;
  return {
    schema: 'narada.mcp_surface.guidance.v0',
    status: 'ok',
    surface_id: SURFACE_ID,
    guidance_tool: GUIDANCE_TOOL,
    purpose: PURPOSE,
    requested: { workflow, tool },
    path_resolution: {
      working_directory: {
        omitted: 'Select the first allowed root reported by git_policy_inspect.',
        absolute: 'Use the supplied absolute repository directory after allowed-root enforcement.',
        relative: 'Resolve an explicitly supplied relative working_directory against the MCP process current directory; prefer an absolute path when the target matters.',
      },
      pathspecs: 'Pathspec arguments are repository-relative to the selected working_directory. Absolute pathspecs and parent traversal are refused.',
      current_directory_inference: 'The caller current directory does not choose the default repository; only an explicit relative working_directory uses process current directory resolution.',
      filesystem_boundary: 'Filesystem paths use the first allowed root for relative paths; this Git contract is intentionally different and must not be conflated with fs_guidance.',
    },
    first_use: [
      'Call this guidance command when the surface is unfamiliar, when a refusal/error is unclear, or before composing a multi-step workflow.',
      'Inspect policy/doctor/status tools before mutation or open-world operations.',
      'Use bounded list/search/query tools for discovery, then show/read/detail tools before acting on a specific object.',
      'Preserve structuredContent as authoritative evidence; text content is for assistant readability.'
    ],
    tool_preference: [
      { step: 'orient', guidance: 'Use *_guidance first when uncertain, then policy/doctor/status tools.' },
      { step: 'discover', guidance: 'Use bounded list/search/query commands with explicit limits and filters.' },
      { step: 'inspect', guidance: 'Use show/read/detail commands for exact targets before mutation.' },
      { step: 'mutate', guidance: 'Only call mutation tools after policy allows it and intent, target, and expected result are explicit.' },
      { step: 'verify', guidance: 'Read back state with the owning surface after any mutation.' }
    ],
    workflows: {
      normal_publication: [
        'git_status for branch/upstream/dirty posture.',
        'git_changed_summary and git_diff for scoped review.',
        'Prefer git_commit_paths with explicit paths; it uses a dedicated temporary index and cannot consume another agent\'s staged files.',
        'Use git_add only when a staged review or legacy two-step workflow is explicitly required.',
        'git_status or staged git_diff to verify staged content.',
        'If git_status reports any unstaged, untracked, or conflict paths, pass expected_staged_paths to git_commit; the guard protects the exact index scope and never infers mixed-worktree intent.',
        'git_commit with concise message and verification body.',
        'git_push only after target is resolved.',
        'Every successful mutation returns mutation_effect, verification_status, and compact post_state; use those fields as the normal readback. verified_post_state remains a compatibility alias.',
        'git_workflow_record for SHA, push status, staged paths, and unrelated dirty paths.'
      ],
      read_only_review: [
        'git_status for posture.',
        'git_branch_list for local and remote branch discovery.',
        'git_log or git_show for commit detail.',
        'git_diff with pathspecs for bounded inspection.'
      ],
      branch_lifecycle: [
        'git_branch_list before selecting a local branch target.',
        'Acquire git_begin_work_scope with scope_kind=repository_topology; it conflicts with every live path or topology lease.',
        'git_branch_create with an explicit name, optional start point, and the topology work_scope_ref; creation does not check out the branch.',
        'git_branch_switch only for an existing branch; Git must preserve the worktree safely.',
        'git_branch_rename with explicit old_name and new_name.',
        'git_branch_delete only after the merged-only base check passes; force deletion is unavailable.',
        'Release the topology lease with git_end_work_scope after the branch mutation.'
      ],
      remote_administration: [
        'git_branch_list with scope=remote or all before remote administration, then acquire a repository_topology work scope for remote deletion or upstream mutation.',
        'git_branch_set_upstream or git_branch_unset_upstream for local tracking configuration.',
        'git_branch_delete_remote with explicit remote, branch, and base; the remote ref must be merged before deletion.',
        'Use git_push separately to publish a newly created local branch.'
      ],
      remote_synchronization: [
        'git_status, then git_fetch with one configured remote and one explicit branch; fetch never accepts arbitrary refspecs.',
        'git_rebase with onto=remote/branch or git_merge with an explicit target. Set autostash=true only for tracked dirty files.',
        'git_sync_status to inspect operation state and conflicts.',
        'After resolving and staging every conflict path, use the matching continue tool; use the matching abort tool to roll back the operation.',
        'Untracked files are refused during synchronization even with autostash because Git autostash does not preserve them.'
      ]
    },
    tool_inventory: {
      read: ['git_policy_inspect', 'git_status', 'git_sync_status', 'git_branch_list', 'git_changed_summary', 'git_repositories_summary', 'git_diff', 'git_log', 'git_show', 'git_output_show'],
      write: ['git_begin_work_scope', 'git_end_work_scope', 'git_add', 'git_unstage', 'git_commit_paths', 'git_reconcile_index', 'git_commit', 'git_push', 'git_fetch', 'git_rebase', 'git_rebase_continue', 'git_rebase_abort', 'git_merge', 'git_merge_continue', 'git_merge_abort', 'git_branch_create', 'git_branch_switch', 'git_branch_rename', 'git_branch_delete', 'git_branch_delete_remote', 'git_branch_set_upstream', 'git_branch_unset_upstream', 'git_workflow_record'],
      write_mode_note: 'Mutations require git-mcp mode=write and policy approval.'
    },
    examples: [
      { intent: 'First use', call: 'git_guidance({})' },
      { intent: 'Normal concurrent-safe commit workflow', call: 'git_status -> git_changed_summary/git_diff -> git_commit_paths -> git_push -> git_workflow_record' },
      { intent: 'Legacy staged review workflow', call: 'git_status -> git_add -> staged git_diff -> git_commit -> git_push -> git_workflow_record' },
      { intent: 'Branch workflow', call: 'git_branch_list -> git_branch_create -> git_branch_switch -> git_branch_delete' },
      { intent: 'Remote synchronization', call: 'git_status -> git_fetch({ remote: "origin", branch: "main" }) -> git_rebase({ onto: "origin/main", autostash: false }) -> git_sync_status' },
      { intent: 'Tool-specific help', call: "git_guidance({ tool: \"<tool_name>\" })" },
      { intent: 'Workflow-specific help', call: "git_guidance({ workflow: \"<workflow_name>\" })" }
    ],
    anti_patterns: [
      'Do not guess hidden state from a tool name; use doctor/status/list/show tools for evidence.',
      'Do not treat assistant text as the durable record when structuredContent is present.',
      'Do not bypass the owning surface with shell scripts when a governed MCP tool exists.',
      'Do not continue after malformed payloads, empty refs, or ambiguous target identifiers; stop and repair the input.',
      'Do not call git_commit without expected_staged_paths when git_status reports any unstaged, untracked, or conflict paths; scope_label alone does not isolate the index.',
      'Do not force-create, force-switch, force-delete, or force-push branches; these tools intentionally expose no force flags.',
      'Do not delete a local or remote branch until the merged-only base check succeeds.'
    ],
    recovery: [
      'For unknown_tool, call tools/list and this guidance command again after restart.',
      'For policy refusal, inspect the surface policy/doctor output and report the exact refusal reason.',
      'For oversized inputs, use the surface payload_ref or output_ref convention when it exists; otherwise reduce scope.',
      'For git_commit_scope_required_for_mixed_worktree, inspect git_status, pass the exact intended expected_staged_paths, and retry; the refusal is atomic.',
      'For branch deletion refusals, inspect git_branch_list and retry only with the correct merged base; force deletion is not available.',
      'For remote branch errors, verify the configured remote and remote ref before retrying; the remote deletion tool does not fetch implicitly.',
      'For git_dirty_worktree_requires_autostash, retry only with autostash=true when all dirty paths are tracked.',
      'For git_untracked_worktree_requires_manual_preservation, preserve untracked files explicitly before retrying synchronization.',
      'For sync conflicts, use git_sync_status, resolve and stage conflict paths, then use the matching continue tool or abort safely.',
      'For unclear behavior, submit surface_feedback_submit with surface_id, kind, summary, reproduction steps, expected behavior, and impact.'
    ],
    feedback: {
      surface_id: SURFACE_ID,
      tool: 'surface_feedback_submit',
      when: [
        'guidance is missing, stale, or contradicted by live behavior',
        'schema shape makes correct usage hard',
        'errors hide the actionable refusal or recovery path'
      ]
    },
    boundaries: [
      'Guidance is read-only model-facing operating advice.',
      'Guidance does not weaken policy, authorize mutation, or replace tool schemas.',
      'The owning MCP surface remains authoritative for state and enforcement.'
    ]
  };
}

export function guidanceToolDefinition(name: string = GUIDANCE_TOOL, description: string = 'Show model-facing operating guidance for ' + SURFACE_ID + ' MCP workflows.'): GuidanceToolDefinition {
  return {
    name,
    description,
    inputSchema: {
      type: 'object',
      properties: {
        workflow: { type: 'string', description: 'Optional workflow name or area to focus guidance on.' },
        tool: { type: 'string', description: 'Optional tool name for tool-specific guidance.' }
      },
      additionalProperties: false
    },
    annotations: { title: name, readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
    outputSchema: { type: 'object', additionalProperties: true }
  };
}