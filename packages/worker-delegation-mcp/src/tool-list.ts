import { guidanceToolDefinition } from './guidance.js';
type WorkerToolDefinition = {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
  annotations?: Record<string, unknown>;
  outputSchema?: Record<string, unknown>;
};

export function listTools(): WorkerToolDefinition[] {
  return decorateTools([
    guidanceToolDefinition(),
    { name: 'worker_output_show', description: 'Read a materialized worker MCP output ref with offset/limit paging.', inputSchema: objectSchema({
      ref: { type: 'string' },
      output_ref: { type: 'string' },
      offset: { type: 'integer', minimum: 0 },
      limit: { type: 'integer', minimum: 0 },
    }) },
    { name: 'worker_operator_affordances', description: 'Return UI-neutral operator affordances for rendering worker run dashboards, launch controls, artifact refs, and recovery actions.', inputSchema: objectSchema({}) },
    { name: 'worker_policy_inspect', description: 'Inspect the active worker delegation policy, including narada-agent-runtime-server Site binding markers and environment projection.', inputSchema: objectSchema({}) },
    { name: 'worker_cognition_defaults_inspect', description: 'Inspect legacy provider-scoped low, medium, and high cognition defaults. Canonical narada-agent-runtime-server runs use capabilities from their immutable Narada invocation plan instead.', inputSchema: objectSchema({}) },
    { name: 'worker_cognition_defaults_update', description: 'Atomically set a legacy provider-scoped cognition default for compatibility runtimes. Canonical narada-agent-runtime-server runs do not consume this registry.', inputSchema: objectSchema({
      provider: { type: 'string' },
      cognition: { type: 'string', enum: ['low', 'medium', 'high'] },
      model: { type: 'string' },
      reasoning_effort: { type: 'string' },
      actor: { type: 'string', description: 'Optional audit actor label.' },
    }, ['provider', 'cognition', 'model', 'reasoning_effort']) },
    { name: 'worker_config_resolve', description: 'Resolve worker run inputs without launching a worker, including narada-agent-runtime-server Site binding status.', inputSchema: objectSchema({
      worker_session_id: { type: 'string', description: 'Optional existing worker session whose constraints should be inherited like worker_resume.' },
      intent: intentSchema(),
      constraints: constraintRequestSchema(),
    }, ['intent', 'constraints']) },
    { name: 'worker_run', description: 'Start one new worker run for one delegated instruction.', inputSchema: objectSchema({
      idempotency_key: { type: 'string', description: 'Optional stable caller operation key. Exact retries return the existing run; conflicting requests are refused.' },
      intent: intentSchema(),
      constraints: constraintRequestSchema(),
    }, ['intent', 'constraints']) },
    { name: 'worker_edit', description: 'Start one edit-capable worker run shortcut using write authority. The agent runtime resolves a canonical plan during preflight when no invocation_plan_ref is supplied.', inputSchema: objectSchema({
      cwd: { type: 'string' },
      site_root: { type: 'string', description: 'Explicit Narada Site root for narada-agent-runtime-server workers. Defaults to nearest Site marker above cwd.' },
      invocation_plan_ref: { type: 'string', description: 'Optional immutable canonical Narada invocation-plan reference. When omitted, narada-agent-runtime-server resolves and admits its plan during canonical preflight.' },
      provider: { type: 'string', description: 'Legacy compatibility provider selector. Rejected for runtime narada-agent-runtime-server; that runtime uses the canonical invocation plan.' },
      instruction: { type: 'string' },
      required_mcp_tools: { type: 'array', items: { type: 'string' }, description: 'Exact MCP tool names to project for narada-agent-runtime-server workers. Other runtimes report the request as unprojectable.' },
      resumable: { type: 'boolean' },
      wait_for_completion: { type: 'boolean', description: 'When true, wait boundedly for completion. Defaults to false so delegation returns promptly with run_id.' },
      wait_timeout_ms: { type: 'integer', minimum: 1, maximum: 180000, description: 'Maximum synchronous wait before returning the recoverable running run_id. Defaults to 180000.' },
      exit_interview: { type: 'boolean', description: 'Ask the worker to include ergonomics feedback in its final output.' },
      overrides: constraintOverrideSchema(),
    }, ['cwd', 'instruction']) },
    { name: 'worker_resume', description: 'Continue one existing worker session.', inputSchema: objectSchema({
      worker_session_id: { type: 'string' },
      intent: intentSchema(),
      constraints: constraintRequestSchema(),
    }, ['worker_session_id', 'constraints']) },
    { name: 'worker_run_status', description: 'Inspect a worker run by run id without waiting for completion.', inputSchema: objectSchema({
      run_id: { type: 'string' },
      site_root: { type: 'string', description: 'Optional authorized Narada Site root whose worker run store should be inspected.' },
    }, ['run_id']) },
    { name: 'worker_run_reap', description: 'Governed cleanup for a running worker record: abort a managed active run when possible, or persist a terminal orphaned/cancelled result with evidence for a stale run.', inputSchema: objectSchema({
      run_id: { type: 'string' },
      reason: { type: 'string', description: 'Required cleanup rationale.' },
      force: { type: 'boolean', description: 'Allow reaping a non-stale running run. Defaults false.' },
      site_root: { type: 'string', description: 'Optional authorized Narada Site root whose worker run store should be reaped.' },
    }, ['run_id', 'reason']) },
    { name: 'worker_runs_list', description: 'List recent worker runs so callers can rediscover outstanding run ids.', inputSchema: objectSchema({
      limit: { type: 'integer', minimum: 1, maximum: 200 },
      include_running: { type: 'boolean' },
      include_completed: { type: 'boolean' },
      include_summary: { type: 'boolean', description: 'Include full summaries in each compact list item.' },
      verbose: { type: 'boolean', description: 'Include full run path, timing object, session id, and full error fields.' },
      site_root: { type: 'string', description: 'Optional authorized Narada Site root whose worker run store should be listed.' },
    }) },
    { name: 'worker_run_wait', description: 'Wait briefly for one worker run to finish, returning the latest run status on timeout.', inputSchema: objectSchema({
      run_id: { type: 'string' },
      timeout_ms: { type: 'integer', minimum: 0, maximum: 300000 },
      poll_ms: { type: 'integer', minimum: 25, maximum: 10000 },
      summary_only: { type: 'boolean', description: 'Return only run id, status, summary, and error preview.' },
      verbose: { type: 'boolean', description: 'Include the full worker run payload as full_run.' },
      site_root: { type: 'string', description: 'Optional authorized Narada Site root whose worker run store should be read.' },
    }, ['run_id']) },
    { name: 'worker_run_batch', description: 'Start multiple worker runs from one request and return compact per-run status plus launch diagnostics.', inputSchema: objectSchema({
      requests: { type: 'array', minItems: 1, maxItems: 50, items: objectSchema({ intent: intentSchema(), constraints: constraintRequestSchema() }, ['intent', 'constraints']) },
      max_parallel_runs: { type: 'integer', minimum: 1, maximum: 50 },
    }, ['requests']) },
    { name: 'worker_run_wait_batch', description: 'Wait briefly for multiple worker runs and return compact per-run statuses plus normalized synthesis.', inputSchema: objectSchema({
      run_ids: { type: 'array', minItems: 1, items: { type: 'string' } },
      timeout_ms: { type: 'integer', minimum: 0, maximum: 300000 },
      poll_ms: { type: 'integer', minimum: 25, maximum: 10000 },
      summary_only: { type: 'boolean' },
      verbose: { type: 'boolean' },
    }, ['run_ids']) },
    { name: 'worker_runs_synthesize', description: 'Return a normalized cross-worker synthesis for completed or running worker run ids.', inputSchema: objectSchema({
      run_ids: { type: 'array', minItems: 1, items: { type: 'string' } },
    }, ['run_ids']) },
    { name: 'worker_dashboard_describe', description: 'Return a read-only local dashboard/API descriptor plus compact run topology, status, result refs, pending joins, and event stream data for one run or all active runs.', inputSchema: objectSchema({
      mode: { type: 'string', enum: ['all_active', 'single_run'], description: 'Defaults to single_run when run_id is provided, otherwise all_active.' },
      run_id: { type: 'string', description: 'Inspect one worker run.' },
      run_ids: { type: 'array', items: { type: 'string' }, description: 'Optional explicit run set when mode is all_active. Defaults to all known runs filtered to active unless include_terminal is true.' },
      include_terminal: { type: 'boolean', description: 'Include completed, failed, cancelled, and completed_with_errors runs. Defaults to true for single_run and false for all_active.' },
      limit: { type: 'integer', minimum: 1, maximum: 200 },
    }) },
  ]);
}

function workerDashboardOutputSchema(): Record<string, unknown> {
  return objectSchema({
    schema: { type: 'string', const: 'narada.worker.dashboard.v1' },
    status: { type: 'string', const: 'ok' },
    mode: { type: 'string', enum: ['all_active', 'single_run'] },
    include_terminal: { type: 'boolean' },
    dashboard: { type: 'object', additionalProperties: true },
    counts: { type: 'object', additionalProperties: true },
    runs: { type: 'array', items: { type: 'object', additionalProperties: true } },
    topology: { type: 'object', additionalProperties: true },
    steps: { type: 'array', items: { type: 'object', additionalProperties: true } },
    pending_join_gates: { type: 'array', items: { type: 'object', additionalProperties: true } },
    event_stream: { type: 'array', items: { type: 'object', additionalProperties: true } },
  }, ['schema', 'status', 'mode', 'include_terminal', 'dashboard', 'counts', 'runs', 'topology', 'steps', 'pending_join_gates', 'event_stream']);
}

function workerRunsListOutputSchema(): Record<string, unknown> {
  return objectSchema({
    schema: { type: 'string', const: 'narada.worker.runs_list.v1' },
    status: { type: 'string', const: 'ok' },
    count: { type: 'integer' },
    limit: { type: 'integer' },
    scanned: { type: 'integer' },
    scan_limit: { type: 'integer' },
    scan_truncated: { type: 'boolean' },
    verbose: { type: 'boolean' },
    include_summary: { type: 'boolean' },
    runs: { type: 'array', items: objectSchema({
      run_id: { type: 'string' },
      status: { type: 'string' },
      completion_state: { type: ['string', 'null'], enum: ['complete', 'partial', 'pending', null] },
      requested_mode: nullableStringSchema(),
      requested_mode_inferred: { type: 'boolean' },
      authority: nullableStringSchema(),
      started_at: nullableStringSchema(),
      finished_at: nullableStringSchema(),
      duration_ms: { type: ['integer', 'null'] },
      worker_session_id: nullableStringSchema(),
      summary_preview: nullableStringSchema(),
      error_preview: nullableStringSchema(),
      error_classification: nullableStringSchema(),
      warning_count: { type: 'integer' },
      progress_preview: nullableStringSchema(),
      latest_event_type: nullableStringSchema(),
      progress: progressPreviewSchema(),
      summary: { type: 'string' },
      run_dir: { type: 'string' },
      timing: { type: 'object', additionalProperties: true },
      error: nullableStringSchema(),
      diagnostic_tail: nullableStringSchema(),
      status_liveness: { type: 'object', additionalProperties: true },
    }, ['run_id', 'status']) },
  }, ['schema', 'status', 'runs', 'scanned', 'scan_limit', 'scan_truncated']);
}

function workerRunWaitOutputSchema(): Record<string, unknown> {
  return objectSchema({
    schema: { type: 'string', const: 'narada.worker.run_wait.v1' },
    status: { type: 'string', const: 'ok' },
    wait: { type: 'object', additionalProperties: true },
    run: { type: 'object', additionalProperties: true },
    full_run: { type: 'object', additionalProperties: true },
  }, ['schema', 'status', 'wait', 'run']);
}

function objectSchema(properties: Record<string, unknown>, required: string[] = []): Record<string, unknown> {
  return { type: 'object', properties, required, additionalProperties: false };
}

function intentSchema(): Record<string, unknown> {
  return objectSchema({
    instruction: { type: 'string' },
    mode: { type: 'string', enum: ['audit_only', 'plan_only', 'implement', 'implement_and_verify'], description: 'Non-mechanical task mode. Defaults to audit_only for read authority and implement for write/command authority.' },
    output_contract: { type: 'object', additionalProperties: true, description: 'Optional caller-owned structured output contract merged into the worker output contract.' },
  }, ['instruction']);
}

function constraintRequestSchema(): Record<string, unknown> {
  return objectSchema({
    cwd: { type: 'string' },
    site_root: { type: 'string', description: 'Explicit Narada Site root for narada-agent-runtime-server workers. Defaults to nearest Site marker above cwd.' },
    invocation_plan_ref: { type: 'string', description: 'Optional immutable canonical Narada invocation-plan reference. When omitted, narada-agent-runtime-server resolves and admits its plan during canonical preflight.' },
    provider: { type: 'string', description: 'Legacy compatibility provider selector. Rejected for runtime narada-agent-runtime-server; that runtime uses the canonical invocation plan.' },
    authority: { type: 'string', enum: ['read', 'write', 'command'] },
    cognition: { type: 'string', enum: ['low', 'medium', 'high'], description: 'Legacy compatibility cognition tier; canonical offerings declare their own plan-scoped capability vocabulary.' },
    resumable: { type: 'boolean' },
    wait_for_completion: { type: 'boolean', description: 'When true, wait boundedly for completion. Defaults to false so delegation returns promptly with run_id.' },
    wait_timeout_ms: { type: 'integer', minimum: 1, maximum: 180000, description: 'Maximum synchronous wait before returning the recoverable running run_id. Defaults to 180000.' },
    max_run_ms: { type: 'integer', minimum: 1, maximum: 1800000, description: 'Maximum worker runtime. The effective value is capped by the worker policy.' },
    exit_interview: { type: 'boolean', description: 'Ask the worker to include ergonomics feedback in its final output.' },
    verification_budget: budgetSchema('Advisory verification budget. Workers classify commands as focused or broad, respect stop discipline, and report budget adherence.'),
    test_budget: budgetSchema('Advisory test budget. Use for focused package-local checks before broader suites; workers report broad unrelated failures separately.'),
    preflight_paths: { type: 'array', items: objectSchema({
      path: { type: 'string' },
      access: { type: 'string', enum: ['read', 'write', 'create'] },
      label: { type: 'string' },
    }, ['path', 'access']), description: 'Optional path capability checks recorded before the worker starts.' },
    required_mcp_tools: { type: 'array', items: { type: 'string' }, description: 'Tool names the worker must have. For narada-agent-runtime-server, worker-delegation projects these through NARADA_WORKER_MCP_CONFIG; other runtimes record them as worker-verification requirements.' },
    overrides: constraintOverrideSchema(),
  }, ['cwd']);
}

function budgetSchema(description: string): Record<string, unknown> {
  return {
    type: 'object',
    description,
    properties: {
      focus: { type: 'string', enum: ['focused', 'broad'], description: 'Default command scope expected for verification.' },
      max_commands: { type: 'integer', minimum: 0, maximum: 100 },
      max_minutes: { type: 'number', minimum: 0 },
      stop_on_first_failure: { type: 'boolean' },
      broad_commands_allowed: { type: 'boolean' },
      notes: { type: 'string' },
    },
    additionalProperties: false,
  };
}

function constraintOverrideSchema(): Record<string, unknown> {
  return objectSchema({
    runtime: { type: 'string', enum: ['codex', 'narada-agent-runtime-server'] },
    sandbox: { type: 'string', enum: ['read-only', 'workspace-write', 'danger-full-access'] },
    model: { type: 'string' },
    reasoning_effort: { type: 'string' },
    config: { type: 'object', additionalProperties: { type: ['string', 'number', 'boolean'] } },
    skip_git_repo_check: { type: 'boolean' },
  });
}

function decorateTools(tools: WorkerToolDefinition[]): WorkerToolDefinition[] {
  return tools.map((tool) => ({ ...tool, annotations: toolAnnotations(tool.name), outputSchema: toolOutputSchema(tool.name) }));
}

function toolAnnotations(name: string) {
  const startsWorker = name === 'worker_run' || name === 'worker_edit' || name === 'worker_resume' || name === 'worker_run_batch';
  const mutatesRunRecord = name === 'worker_run_reap' || name === 'worker_cognition_defaults_update';
  return {
    title: name,
    readOnlyHint: !startsWorker && !mutatesRunRecord,
    destructiveHint: mutatesRunRecord,
    idempotentHint: /guidance|inspect|config_resolve|run_status|runs_list|run_wait|synthesize|dashboard_describe/.test(name),
    openWorldHint: true,
  };
}

function toolOutputSchema(name: string): Record<string, unknown> {
  if (name === 'worker_policy_inspect') return workerPolicyOutputSchema();
  if (name === 'worker_cognition_defaults_inspect' || name === 'worker_cognition_defaults_update') return { type: 'object', additionalProperties: true };
  if (name === 'worker_operator_affordances') return { type: 'object', additionalProperties: true };
  if (name === 'worker_config_resolve') return workerConfigResolveOutputSchema();
  if (name === 'worker_run' || name === 'worker_edit' || name === 'worker_resume' || name === 'worker_run_status') return workerRunOutputSchema();
  if (name === 'worker_run_reap') return workerRunReapOutputSchema();
  if (name === 'worker_run_wait') return workerRunWaitOutputSchema();
  if (name === 'worker_runs_list') return workerRunsListOutputSchema();
  if (name === 'worker_run_batch') return batchOutputSchema('narada.worker.run_batch.v1');
  if (name === 'worker_run_wait_batch') return batchOutputSchema('narada.worker.run_wait_batch.v1');
  if (name === 'worker_runs_synthesize') return batchOutputSchema('narada.worker.runs_synthesis.v1');
  if (name === 'worker_dashboard_describe') return workerDashboardOutputSchema();
  return { type: 'object', additionalProperties: true };
}

function workerRunReapOutputSchema(): Record<string, unknown> {
  return objectSchema({
    schema: { type: 'string', const: 'narada.worker.run_reap.v1' },
    status: { type: 'string', enum: ['reaped', 'already_terminal'] },
    run_id: { type: 'string' },
    reaped: { type: 'boolean' },
    evidence: { type: 'object', additionalProperties: true },
    run: { type: 'object', additionalProperties: true },
  }, ['schema', 'status', 'run_id', 'reaped', 'evidence', 'run']);
}

function batchOutputSchema(schemaConst: string): Record<string, unknown> {
  return objectSchema({
    schema: { type: 'string', const: schemaConst },
    status: { type: 'string' },
    max_parallel_runs: { type: 'integer' },
    requested_count: { type: 'integer' },
    started_count: { type: 'integer' },
    failed_count: { type: 'integer' },
    finished_count: { type: 'integer' },
    timed_out_count: { type: 'integer' },
    errored_count: { type: 'integer' },
    timeout_ms: { type: 'integer' },
    elapsed_ms: { type: 'integer' },
    runs: { type: 'array', items: { type: 'object', additionalProperties: true } },
    synthesis: { type: 'object', additionalProperties: true },
    failures: { type: 'array', items: { type: 'object', additionalProperties: true } },
    run_ids: stringArraySchema(),
    timing: { type: 'object', additionalProperties: true },
  }, ['schema', 'status']);
}

function workerConfigResolveOutputSchema(): Record<string, unknown> {
  return objectSchema({
    schema: { type: 'string', const: 'narada.worker.config_resolve.v1' },
    status: { type: 'string', const: 'ok' },
    dry_run: { type: 'boolean', const: true },
    requested_mode: { type: 'string', enum: ['audit_only', 'plan_only', 'implement', 'implement_and_verify'] },
    resume_worker_session_id: nullableStringSchema(),
    launchable: { type: 'boolean' },
    launchability: { type: 'object', additionalProperties: true },
    resolved_worker_config: { type: 'object', additionalProperties: true },
    invocation: { type: 'object', additionalProperties: true },
    preflight: { type: 'array', items: objectSchema({ name: { type: 'string' }, status: { type: 'string', enum: ['ok', 'warning', 'blocked'] }, message: { type: 'string' } }, ['name', 'status', 'message']) },
    requested_mcp_tools: stringArraySchema(),
    mcp_tool_verification: { type: 'object', additionalProperties: true },
    output_contract: { type: 'object', additionalProperties: true },
    runtime_availability: { type: 'object', additionalProperties: true },
    config_resolution: { type: 'object', additionalProperties: true },
    implementation_identity: { type: 'object', additionalProperties: true },
    implementation_gate: { type: 'object', additionalProperties: true },
    warnings: stringArraySchema(),
  }, ['schema', 'status', 'dry_run', 'requested_mode', 'resume_worker_session_id', 'launchable', 'launchability', 'resolved_worker_config', 'invocation', 'preflight', 'runtime_availability', 'config_resolution', 'warnings']);
}

function workerPolicyOutputSchema(): Record<string, unknown> {
  return objectSchema({
    schema: { type: 'string', const: 'narada.worker.policy.v1' },
    status: { type: 'string' },
    implementation_identity: { type: 'object', additionalProperties: true },
    implementation_gate: { type: 'object', additionalProperties: true },
    default_runtime: { type: 'string' },
    default_authority: { type: 'string' },
    default_cognition: { type: 'string' },
    allowed_authorities: stringArraySchema(),
    allowed_cognition: stringArraySchema(),
    run_root: { type: 'string' },
    audit_log_dir: nullableStringSchema(),
    allowed_roots: stringArraySchema(),
    roots_from_trust_config: nullableStringSchema(),
    allowed_runtimes: stringArraySchema(),
    allowed_sandboxes: stringArraySchema(),
    allowed_config_keys: stringArraySchema(),
    allow_raw_config_overrides: { type: 'boolean' },
    allow_danger_full_access: { type: 'boolean' },
    nars_site_semantics: { type: 'object', additionalProperties: true },
    max_parallel_runs: { type: 'integer' },
    max_prompt_bytes: { type: 'integer' },
    max_output_bytes: { type: 'integer' },
    max_run_ms: { type: 'integer' },
    max_tool_rounds: { type: 'integer' },
    cognition_defaults: { type: 'object', additionalProperties: true },
    runtimes: { type: 'object', additionalProperties: true },
  }, ['schema', 'status']);
}

function workerRunOutputSchema(): Record<string, unknown> {
  return objectSchema({
    schema: { type: 'string', const: 'narada.worker.run.v1' },
    status: { type: 'string', enum: ['running', 'completed', 'completed_with_errors', 'failed', 'cancelled'] },
    run_id: { type: 'string' },
    run_dir: { type: 'string' },
    runtime: { type: 'string', enum: ['codex', 'narada-agent-runtime-server'] },
    worker_session_id: nullableStringSchema(),
    idempotency_key: nullableStringSchema(),
    request_fingerprint: nullableStringSchema(),
    idempotency_replayed: { type: 'boolean' },
    resolved_worker_config: { type: 'object', additionalProperties: true },
    executor_request: { type: 'object', additionalProperties: true },
    requested_mode: { type: 'string', enum: ['audit_only', 'plan_only', 'implement', 'implement_and_verify'] },
    edits_performed: { type: ['boolean', 'null'] },
    target_state_changed: { type: ['boolean', 'null'] },
    confidence: { type: 'string', enum: ['complete', 'partial', 'pending'] },
    completion_state: { type: 'string', enum: ['complete', 'partial', 'pending'] },
    blocked_paths: stringArraySchema(),
    verification: stringArraySchema(),
    runtime_warnings: stringArraySchema(),
    requested_mcp_tools: stringArraySchema(),
    mcp_tool_verification: { type: 'object', additionalProperties: true },
    output_contract: { type: 'object', additionalProperties: true },
    warning_count: { type: 'integer' },
    preflight: { type: 'array', items: objectSchema({ name: { type: 'string' }, status: { type: 'string', enum: ['ok', 'warning', 'blocked'] }, message: { type: 'string' } }, ['name', 'status', 'message']) },
    final_checklist: stringArraySchema(),
    summary: nullableStringSchema(),
    deliverables: nullableArraySchema(objectSchema({ path: { type: 'string' }, description: { type: 'string' } }, ['path', 'description'])),
    open_questions: nullableStringArraySchema(),
    next_actions: nullableStringArraySchema(),
    changes: nullableArraySchema(objectSchema({ path: { type: 'string' }, status: { type: 'string' }, summary: { type: 'string' } }, ['path', 'status', 'summary'])),
    structured_outputs: { type: 'object', additionalProperties: true },
    verification_results: nullableArraySchema(objectSchema({ tool: { type: ['string', 'null'] }, command: { type: ['string', 'null'] }, status: { type: 'string' }, summary: { type: 'string' }, command_classification: { type: 'string', enum: ['focused', 'broad', 'not_applicable'] } }, ['tool', 'command', 'status', 'summary'])),
    verification_budget_respected: { type: ['boolean', 'null'] },
    broad_unrelated_failures: { type: 'array', items: objectSchema({ command: { type: ['string', 'null'] }, status: { type: 'string' }, summary: { type: 'string' } }, ['command', 'status', 'summary']) },
    exit_interview: { type: ['object', 'null'], properties: {
      ergonomics_feedback: { type: 'string' },
      friction_points: stringArraySchema(),
      missing_affordances: stringArraySchema(),
      observed_incoherencies: stringArraySchema(),
      suggested_improvements: stringArraySchema(),
    }, additionalProperties: false },
    progress: progressPreviewSchema(),
    artifacts: { type: 'array', items: objectSchema({ name: { type: 'string' }, path: { type: 'string' } }, ['name', 'path']) },
    artifact_readback: { type: 'object', additionalProperties: true },
    timing: { type: 'object', additionalProperties: true },
    error: nullableStringSchema(),
    worker_output_state: { type: 'string', enum: ['pending', 'available', 'absent', 'invalid_json', 'invalid_shape'] },
    worker_authored_output_present: { type: 'boolean' },
    worker_output_error: { type: 'object', additionalProperties: true },
    diagnostic_tail: nullableStringSchema(),
    error_classification: nullableStringSchema(),
    status_liveness: { type: 'object', additionalProperties: true },
    progress_state: { type: 'object', additionalProperties: true },
    budget_status: { type: 'object', additionalProperties: true },
    recent_activity: { type: 'array', items: { type: 'object', additionalProperties: true } },
  }, ['schema', 'status', 'run_id', 'run_dir', 'resolved_worker_config', 'summary', 'deliverables', 'open_questions', 'next_actions']);
}

function stringArraySchema(): Record<string, unknown> {
  return { type: 'array', items: { type: 'string' } };
}

function nullableStringArraySchema(): Record<string, unknown> {
  return { type: ['array', 'null'], items: { type: 'string' } };
}

function nullableArraySchema(items: Record<string, unknown>): Record<string, unknown> {
  return { type: ['array', 'null'], items };
}

function nullableStringSchema(): Record<string, unknown> {
  return { type: ['string', 'null'] };
}

function progressPreviewSchema(): Record<string, unknown> {
  return objectSchema({
    event_count: { type: 'integer' },
    latest_event_type: nullableStringSchema(),
    latest_event_preview: nullableStringSchema(),
    latest_event_at: nullableStringSchema(),
    readable: { type: 'boolean' },
    tail_truncated: { type: 'boolean' },
    error_preview: { type: 'string' },
  }, ['event_count', 'latest_event_type', 'latest_event_preview', 'readable', 'tail_truncated']);
}
