import type { WorkerDelegationMode, WorkerIntent, WorkerPreflightCheck } from './worker-types.js';

export type WorkerPromptOptions = {
  intent: WorkerIntent;
  cwd: string;
  mode: WorkerDelegationMode;
  runtime: string;
  preflight: WorkerPreflightCheck[];
  outputContract: Record<string, unknown>;
  exitInterview: boolean;
  requiredMcpTools?: string[];
  authority: string;
  allowedRoots: string[];
};

export function buildWorkerPrompt(options: WorkerPromptOptions): string {
  const requiredMcpTools = options.requiredMcpTools ?? [];
  const writable = options.authority !== 'read';
  const capabilitySnapshot = {
    schema: 'narada.worker.capability_snapshot.v1',
    authority: options.authority,
    effective_mode: writable ? 'workspace_write' : 'read_only',
    validated_against_runtime: true,
    validation_basis: 'worker_policy_and_runtime_adapter_contract',
    provider_boundary: { permission_profile: writable ? 'workspace_write' : 'read_only', writable_roots_injected: writable, source: 'runtime_adapter_and_codex_cli' },
    cwd: options.cwd,
    allowed_roots: options.allowedRoots,
    filesystem: { read: true, write: writable, patch: writable },
    commands: { execute: true, write_effects: writable, direct_file_mutation: writable, working_directory_scoped: true, tests_may_write_build_artifacts: writable },
    approval: { mode: writable ? 'automatic_contained_review' : 'not_required', human_interaction_required: false, sandbox: writable ? 'workspace-write' : 'read-only' },
    tool_bridge: { kind: 'codex_builtin_repo_tools', ordinary_file_mutation_tool: 'apply_patch', exact_byte_file_mutation_tool: 'bounded_shell_command', mcp_projection: requiredMcpTools.length > 0 ? 'explicit_allowlist' : 'none' },
    workflow_primitives: { exact_byte_file_lifecycle: { tool: 'bounded_shell_command', expected_commands: 1, operations: ['create', 'read_verify', 'remove', 'confirm_absent'], encoding_must_be_explicit: true, windows_recipe: 'assign literal path and content variables; use IO.File WriteAllBytes and ReadAllBytes; compare hex; delete; test existence; avoid interpolated command strings' } },
    evaluation_contract: { schema: 'narada.worker.observed_ergonomics.v1', basis: 'observed_fresh_run_only', score_5: 'no_material_observed_friction', score_reduction_requires: 'observed_failure_retry_human_intervention_or_ambiguity_that_changed_execution', automatic_contained_review_is_human_ceremony: false, speculative_improvements_field: 'non_scoring_observations' },
    refusal_contract: { schema: 'narada.worker.refusal.v1', required_fields: ['tool', 'operation', 'cwd', 'target_path', 'declared_capability', 'actual_refusal'] },
  };
  return [
    'Intent',
    options.intent.instruction,
    '',
    'Requested mode',
    options.mode,
    '',
    'Working directory',
    options.cwd,
    '',
    `Effective mode: ${capabilitySnapshot.effective_mode} (injected at provider process boundary and runtime-validated). CWD: ${options.cwd}. Roots: ${options.allowedRoots.join(', ')}.`,
    `Repo bridge: ${capabilitySnapshot.tool_bridge.kind}; MCP projection: ${capabilitySnapshot.tool_bridge.mcp_projection}.`,
    'Use apply_patch for ordinary file edits; use one bounded shell command when exact byte content or atomic lifecycle verification requires it.',
    'For ergonomics ratings, lower a score only for observed failure, retry, human intervention, or ambiguity that changed execution; put hypothetical improvements in non_scoring_observations.',
    'Automatic contained review requires no human interaction and does not count as ceremony.',
    'On refusal return narada.worker.refusal.v1 with tool, operation, cwd, target_path, declared_capability, and actual_refusal.',
    '',
    'Preflight evidence',
    ...options.preflight.map((check) => `- ${check.status} ${check.name}: ${check.message}`),
    '',
    'Mode contract',
    options.mode === 'audit_only' ? 'Audit only: inspect and report. Do not edit files or change target state.' : options.mode === 'plan_only' ? 'Plan only: produce an implementation plan. Do not edit files or change target state.' : options.mode === 'implement_and_verify' ? 'Implement and verify: make the requested changes, run appropriate checks, and report files changed plus verification.' : 'Implement: make the requested changes and report files changed plus remaining verification needs.',
    '',
    'Recursion guard',
    'Do not call any worker_* MCP tools.',
    '',
    'Tool use discipline',
    ...(requiredMcpTools.length > 0 ? [
      'Prefer the explicitly projected MCP filesystem, git, and structured-command tools for inspection and verification.',
      'Do not use direct shell commands for file discovery or file reads when a projected MCP tool can do the work.',
      'When a required MCP tool is unavailable or insufficient, include the concise shell fallback reason in verification.summary.',
    ] : [
      'Use built-in contained repository tools for bounded file inspection, patching, and focused tests.',
      'These built-in tools are the governed repo-work bridge for this isolated delegated run.',
      'No MCP projection is intentional for this run. Built-in patch and bounded shell tools are the complete governed bridge; do not score the absence of MCP projection as friction or a missing affordance unless an operation actually fails.',
      'Use one bounded shell command for exact-byte file lifecycle work and focused verification.',
    ]),
    '',
    'MCP tool projection',
    ...(requiredMcpTools.length > 0 ? [
      'Only the following exact MCP tool names are projected into this worker run:',
      ...requiredMcpTools.map((tool) => `- ${tool}`),
      'Do not call any MCP tool outside this explicit allowlist or guess alternate server/tool names.',
    ] : [
      'No MCP tools are projected into this worker run.',
      'Do not call MCP tools. If the intent requires MCP access, return the required JSON immediately with a clear summary that MCP tools were not projected and a failed not_applicable verification entry; do not probe guessed or hidden tool names.',
    ]),
    ...(requiredMcpTools.length > 0 && (options.mode === 'audit_only' || options.mode === 'plan_only') ? [
      'For focused source inspection, read target files directly through available filesystem MCP tools such as fs_read_file_range and fs_grep_search. Do not ask the delegating caller to provide output_refs for ordinary source files.',
      'If a file is large, generated, or secret-bearing, keep reads bounded and cite the file/path plus relevant line window rather than copying full content.',
    ] : []),
    '',
    'Verification budget discipline',
    'Classify every verification command as focused, broad, or not_applicable in verification[].command_classification.',
    'Focused commands directly validate the requested package or touched files. Broad commands cover unrelated packages, whole-repo suites, or wide scans.',
    'Respect verification_budget and test_budget from the structured output contract. If stop_on_first_failure is true, stop after the first blocking focused failure.',
    'Report verification_budget_respected as true, false, or null, and list broad unrelated failures only in broad_unrelated_failures.',
    ...(options.runtime === 'narada-agent-runtime-server' ? [
      '',
      'NARS worker completion guard',
      'You are running under narada-agent-runtime-server as an automated worker. Complete this turn by returning the required JSON object; do not wait for operator input.',
      'Do not call pause, sleep, wait, delegation, or worker_* tools from inside this worker turn. Lifecycle MCP tools are permitted only when their exact names appear in the explicit MCP projection above; otherwise do not call them.',
      'Only call MCP tools whose exact server/tool names are visible and admitted in this runtime. Do not invent or guess tool names such as andrey-user-filesystem when they are not explicitly available.',
      'If a tool call returns admission_required, surface_registry_tool_not_declared, mcp_runtime_fault, or any unavailable-tool error, stop using that tool family and return the required JSON with the issue in residual_risks or observed_incoherencies.',
      'For tasks answerable from the delegated intent, preflight evidence, or current prompt, do not probe filesystem tools just to gather extra context.',
    ] : []),
    '',
    'Structured output contract',
    JSON.stringify(options.outputContract),
    ...(options.mode === 'audit_only' ? [
      'For audit_only, include concise findings in deliverables as machine-readable JSON strings when possible, using severity, path, recommendation, confidence_level, and evidence_refs.',
    ] : []),
    '',
    'Output requirements',
    'Return one JSON object matching worker_output.schema.json.',
    'For audit_only or plan_only, explicitly state that edits_performed=false in the summary if no files were changed.',
    'Always include explicit edits_performed, target_state_changed, changes, and verification fields.',
    'Always include explicit verification_budget_respected and broad_unrelated_failures fields.',
    'For implement or implement_and_verify, list changed files in changes and checks run in verification.',
    ...(options.exitInterview ? [
      '',
      'Exit interview',
      'Include exit_interview in the output JSON with ergonomics_feedback, friction_points, missing_affordances, observed_incoherencies, and suggested_improvements.',
      'Also include exit_interview.observed_ergonomics with integer scores 1-5 for clarity_before_acting, confidence_in_writable_roots, predictability_of_tool_behavior, diagnostic_usefulness, remaining_ceremony, and remaining_weaknesses, plus non_scoring_observations.',
      'Use narada.worker.observed_ergonomics.v1: lower a score only for observed failure, retry, human intervention, or ambiguity that changed execution.',
      'Every score below 5 must cite the qualifying observed event in friction_points. Speculative alternatives and hypothetical improvements belong only in non_scoring_observations and do not lower scores.',
      'If friction_points is empty, every observed_ergonomics score must be 5.',
      'Intentional absence of MCP projection is not a qualifying event and must not appear in friction_points. A successful bounded-shell operation with complete output is diagnostically complete.',
      'Focus on concrete tool/interface friction encountered during this delegated run, including anything that made progress harder, ambiguous, slower, or less observable.',
    ] : []),
    '',
  ].join('\n');
}
