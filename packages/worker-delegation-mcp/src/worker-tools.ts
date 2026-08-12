import { createHash } from 'node:crypto';
import { constants, accessSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, extname, isAbsolute, join, relative, resolve } from 'node:path';
import { diagnosticError } from './errors.js';
import { buildRuntimeDiagnostics, classifyRuntimeError, compactRunError, partialFailurePosture, readDiagnosticTail, readRunProgress, readTextTail, runtimeFailureRemediation, workerBudgetStatus } from './diagnostics.js';
import { buildCodexArgv, buildInvocation as codexBuildInvocation, runCodexInvocation, type Invocation, type ResolvedWorkerConfig } from './codex-adapter.js';
import { outputContractForMode, outputContractForRequest, parseLastMessage, resultStatus, workerOutputState, type WorkerOutput } from './output-contract.js';
import { buildAgentRuntimeServerArgv, buildInvocation as agentRuntimeServerBuildInvocation, runAgentRuntimeServerInvocation } from './agent-runtime-server-adapter.js';
import { CODEX_SUBSCRIPTION_PROVIDER, NARADA_AGENT_RUNTIME_SITE_REMEDIATION, NARADA_SITE_ROOT_MARKERS, WORKER_REQUIRED_MCP_SCOPE, assertWorkerImplementationFresh, defaultConfigForCognition, defaultSandboxForAuthority, environmentForWorker, publicWorkerPolicy, rejectNaradaAgentRuntimeProviderForRuntime, resolveAuthority, resolveCognition, resolveConfig, resolveNaradaAgentRuntimeProvider, resolveNaradaSiteBinding, resolveSandbox, resolveWorkingDirectory, validateRuntime, workerImplementationGate, workerImplementationIdentity } from './policy.js';
import { IntelligenceLaunchContextError, loadIntelligenceLaunchContext, projectIntelligenceLaunchContext, publicIntelligenceLaunchContext, type IntelligenceLaunchContext } from './intelligence-launch-context.js';
import { CanonicalInvocationPlanError, readCanonicalInvocationPlan, type CanonicalInvocationPlanBinding } from './canonical-invocation-plan.js';
import { publicCognitionDefaults, publicProviderRegistryDiagnostics, updateCognitionDefault } from './cognition-defaults.js';
import { projectCanonicalProviderCredentialEnvironment, resolveWorkerProviderRuntimeBinding } from './provider-runtime-binding.js';
import { buildWorkerPrompt } from './prompt.js';
import { audit, createRunRecord, registerActiveRun, readWorkerSessionRecord, unregisterActiveRun, writeJson, writeText, writeWorkerOutputSchema, writeWorkerSessionRecord } from './run-record.js';
import { candidateRunRoots, listActiveRunIds, listRunCandidates, listRunIds, locateRunResult, readRunIndexRecord, readRunResult, resolveRunInspectionSiteRoot, runArtifacts } from './run-store.js';
import { reapEvidence } from './recovery.js';
import { extractSessionEventEvidence } from './runtime-events.js';
import { normalizeBatchRequests, normalizeOptionalRunIds, normalizeRunIds, requireBatchRequestRecord } from './tool-handlers/batch.js';
import { workerOperatorAffordances } from './tool-handlers/affordances.js';
import { dashboardApiEndpoints, dashboardMode, dashboardPendingJoinGates, dashboardRun } from './tool-handlers/dashboard.js';
import { workerEditRunArgs } from './tool-handlers/edit.js';
import { includeRunByStatus, isTerminalRunStatus, modeWithInference, runListItem, runSortKey, runWaitPayload } from './tool-handlers/status.js';
import type { WorkerMcpState } from './state.js';
import type { WorkerCognitionDefaults, WorkerPolicy, PrimitiveConfigValue, WorkerRuntimeId } from './policy.js';
import type { SupportedRuntime, WorkerConstraintOverrides, WorkerConstraintRequest, WorkerDelegationMode, WorkerExecutorRequest, WorkerPreflightCheck, WorkerPreflightPath, WorkerRunMetadata, WorkerRunToolInput } from './worker-types.js';
import type { RunRecordPaths } from './run-record.js';

export type WorkerRequestContext = {
  abortSignal?: AbortSignal;
};

const MAX_SYNCHRONOUS_WAIT_MS = 180_000;

export async function callWorkerTool(name: string, args: Record<string, unknown>, state: WorkerMcpState, context: WorkerRequestContext = {}): Promise<unknown> {
  if (name === 'worker_operator_affordances') return workerOperatorAffordances(state);
  if (name === 'worker_policy_inspect') return publicWorkerPolicy(state.policy);
  if (name === 'worker_cognition_defaults_inspect') return publicCognitionDefaults(requiredCognitionDefaultsState(state), state.policy.providerCognitionDefaults, state.providerRegistryDiagnostics);
  if (name === 'worker_cognition_defaults_update') return updateCognitionDefault({ state: requiredCognitionDefaultsState(state), defaults: state.policy.providerCognitionDefaults, provider: args.provider, cognition: args.cognition, model: args.model, reasoningEffort: args.reasoning_effort, actor: args.actor });
  if (name === 'worker_config_resolve') return workerConfigResolve(args, state);
  if (name === 'worker_run') return workerRun(args, state, null, context, 'worker_run');
  if (name === 'worker_edit') return workerRun(workerEditRunArgs(args), state, null, context, 'worker_edit');
  if (name === 'worker_resume') return workerRun(args, state, requiredNonEmptyString(args.worker_session_id, 'worker_runtime_resume_not_supported'), context, 'worker_resume');
  if (name === 'worker_run_status') return workerRunStatus(args, state);
  if (name === 'worker_run_reap') return workerRunReap(args, state);
  if (name === 'worker_runs_list') return workerRunsList(args, state);
  if (name === 'worker_run_wait') return workerRunWait(args, state);
  if (name === 'worker_run_batch') return workerRunBatch(args, state, context);
  if (name === 'worker_run_wait_batch') return workerRunWaitBatch(args, state);
  if (name === 'worker_runs_synthesize') return workerRunsSynthesize(args, state);
  if (name === 'worker_dashboard_describe') return workerDashboardDescribe(args, state);
  throw diagnosticError('worker_unknown_tool', `worker_unknown_tool:${name}`, { tool_name: name });
}

function requiredCognitionDefaultsState(state: WorkerMcpState) {
  if (!state.cognitionDefaults) throw diagnosticError('worker_cognition_defaults_unavailable', 'worker_cognition_defaults_unavailable');
  return state.cognitionDefaults;
}

function optionalBoolean(value: unknown, field: string): boolean {
  if (value === undefined || value === null) return false;
  if (typeof value === 'boolean') return value;
  throw diagnosticError('worker_invalid_tool_input', 'worker_boolean_required', { field, value_type: Array.isArray(value) ? 'array' : typeof value });
}

function resolveCognitionProvider(runtime: WorkerRuntimeId, request: WorkerRunToolInput, cognition: Exclude<ResolvedWorkerConfig['cognition'], null>, state: WorkerMcpState) {
  const effectiveCognitionDefault = runtime === 'narada-agent-runtime-server' && !request.constraints.provider
    ? state.cognitionDefaults?.effectiveDefaults[cognition] ?? null
    : null;
  if (runtime === 'codex') {
    return {
      effectiveCognitionDefault: null,
      providerResolution: { provider: CODEX_SUBSCRIPTION_PROVIDER, source: 'codex_runtime_default' },
    };
  }
  return {
    effectiveCognitionDefault,
    providerResolution: effectiveCognitionDefault
      ? { provider: effectiveCognitionDefault.provider, source: effectiveCognitionDefault.source === 'site_runtime_override' ? 'site_cognition_default' : 'registry_default' }
      : resolveNaradaAgentRuntimeProvider(request.constraints.provider, state.policy),
  };
}

function workerConfigResolve(args: Record<string, unknown>, state: WorkerMcpState): Record<string, unknown> {
  if (args.config_overrides !== undefined) throw diagnosticError('worker_raw_config_overrides_not_allowed');
  const resumeSessionId = args.worker_session_id === undefined || args.worker_session_id === null || args.worker_session_id === ''
    ? null
    : requiredNonEmptyString(args.worker_session_id, 'worker_runtime_resume_not_supported');
  const request = normalizeWorkerRunToolInput(args, resumeSessionId !== null);
  const inheritedSession = resumeSessionId ? readWorkerSessionRecord(state.policy, resumeSessionId) : null;
  if (inheritedSession) inheritSessionConstraints(request, inheritedSession.resolved_worker_config);
  const requestedOverrides = request.constraints.overrides ? { ...request.constraints.overrides, config: { ...(request.constraints.overrides.config ?? {}) } } : {};
  const authority = resolveAuthority(request.constraints.authority, state.policy);
  const cognition = resolveCognition(request.constraints.cognition, state.policy);
  let overrides = request.constraints.overrides ?? {};
  const runtime = validateRuntime(overrides.runtime, state.policy);
  rejectNaradaAgentRuntimeProviderForRuntime(request.constraints.provider, runtime);
  const cwd = resolveWorkingDirectory(request.constraints.cwd, state.policy);
  const preResolvedSiteBinding = runtime === 'narada-agent-runtime-server'
    ? resolveNaradaSiteBinding(cwd, state.policy, request.constraints.site_root, {
      siteRoot: state.env.NARADA_SITE_ROOT,
      workspaceRoot: state.env.NARADA_WORKSPACE_ROOT,
    })
    : null;
  const canonicalPlanLaunch = runtime === 'narada-agent-runtime-server'
    ? requireCanonicalInvocationPlan(state, preResolvedSiteBinding!.siteRoot, request)
    : null;
  if (!canonicalPlanLaunch) {
    request.constraints.authority = authority;
    request.constraints.cognition = cognition;
  }
  const { effectiveCognitionDefault, providerResolution } = canonicalPlanLaunch
    ? { effectiveCognitionDefault: null, providerResolution: { provider: null, source: 'canonical_invocation_plan' } }
    : resolveCognitionProvider(runtime, request, cognition, state);
  const sandbox = resolveSandbox(overrides.sandbox ?? defaultSandboxForAuthority(authority), state.policy, runtime);
  if (runtime !== 'narada-agent-runtime-server') {
    applyCognitionDefaults(request, cognition, state, runtime, providerResolution.provider);
    applyEffectiveCognitionTuple(request, effectiveCognitionDefault);
  }
  overrides = request.constraints.overrides ?? {};
  const resolvedConfigInput = resolveConfig(overrides, state.policy);
  const maxRunMs = boundedInteger(request.constraints.max_run_ms, state.policy.maxRunMs, 1, state.policy.maxRunMs, 'worker_max_run_ms_invalid');
  const requestedMode = request.intent.mode ?? defaultModeForAuthority(authority);
  request.intent.mode = requestedMode;
  const preflight = buildPreflight({ cwd, authority, mode: requestedMode, waitForCompletion: request.constraints.wait_for_completion === true, isResume: resumeSessionId !== null, preflightPaths: request.constraints.preflight_paths ?? [], requiredMcpTools: request.constraints.required_mcp_tools ?? [], allowedRoots: state.policy.allowedRoots });
  const outputContract = outputContractForRequest(request, requestedMode);
  const environment = environmentForWorker(state.env);
  delete environment.NARADA_WORKER_MCP_CONFIG;
  delete environment.NARADA_MCP_SCOPE;
  const runtimeAvailability = checkRuntimeAvailability(runtime, state.policy, environment);
  const implementationGate = workerImplementationGate();
  const launchability = mcpToolLaunchability(request.constraints.required_mcp_tools ?? [], runtime);
  const prompt = buildWorkerPrompt({ intent: request.intent, cwd, mode: requestedMode, runtime, preflight, outputContract, exitInterview: request.constraints.exit_interview !== false && requestedMode.startsWith('implement'), requiredMcpTools: request.constraints.required_mcp_tools ?? [], authority, allowedRoots: state.policy.allowedRoots });
  const promptBytes = Buffer.byteLength(prompt, 'utf8');
  if (promptBytes > state.policy.maxPromptBytes) throw diagnosticError('worker_prompt_too_large', 'worker_prompt_too_large', { prompt_byte_length: promptBytes, max_prompt_bytes: state.policy.maxPromptBytes });
  const skipGitRepoCheck = optionalBoolean(overrides.skip_git_repo_check, 'skip_git_repo_check');
  const resumable = resumeSessionId !== null || request.constraints.resumable === true;
  const ephemeral = !resumable;
  const dryRunPaths = {
    schemaPath: '<dry-run>/worker_output.schema.json',
    lastMessagePath: '<dry-run>/last_message.json',
  };

  let resolvedWorkerConfig: ResolvedWorkerConfig;
  let invocation: Invocation;
  let intelligenceContext: IntelligenceLaunchContext | null = canonicalPlanLaunch?.context ?? null;
  if (runtime === 'narada-agent-runtime-server') {
    const agentRuntime = state.policy.runtimes.naradaAgentRuntimeServer;
    const resolvedSiteBinding = preResolvedSiteBinding!;
    const siteRoot = resolvedSiteBinding.siteRoot;
    const siteBinding = naradaAgentRuntimeSiteBinding(cwd, resolvedSiteBinding);
    intelligenceContext = canonicalPlanLaunch?.context ?? readWorkerIntelligenceContext(state, siteRoot);
    if (intelligenceContext.status === 'ready') Object.assign(environment, projectIntelligenceLaunchContext(intelligenceContext));
    const canonicalProviderProjection = canonicalPlanLaunch!.plan
      ? projectCanonicalProviderCredential(state, environment, canonicalPlanLaunch!.plan)
      : null;
    environment.NARADA_SITE_ROOT = siteRoot;
    environment.NARADA_WORKSPACE_ROOT = resolvedSiteBinding.workspaceRoot;
    environment.NARADA_AGENT_ID ??= 'narada.architect';
    environment.NARADA_CARRIER_SESSION_ID = resumeSessionId ?? '<dry-run-session>';
    environment.NARADA_MAX_TOOL_ROUNDS = String(state.policy.maxToolRounds);
    const workerMcpProjection = buildWorkerMcpProjection(request.constraints.required_mcp_tools ?? []);
    if (workerMcpProjection) {
      environment.NARADA_MCP_SCOPE = WORKER_REQUIRED_MCP_SCOPE;
      environment.NARADA_WORKER_MCP_CONFIG = JSON.stringify(workerMcpProjection);
    }
    const argv = buildAgentRuntimeServerArgv({ authority, workerSessionId: resumeSessionId ?? undefined });
    resolvedWorkerConfig = {
      runtime: 'narada-agent-runtime-server',
      authority,
      cognition: null,
      command: runtimeAvailability.command ?? agentRuntime.command,
      command_args: agentRuntime.commandArgs,
      argv,
      cwd,
      site_root: siteRoot,
      workspace_root: resolvedSiteBinding.workspaceRoot,
      site_bound: true,
      site_marker: resolvedSiteBinding.marker,
      site_root_source: resolvedSiteBinding.source,
      site_binding: siteBinding,
      provider: null,
      provider_source: 'canonical_invocation_plan',
      provider_runtime_binding: canonicalPlanRuntimeBinding(intelligenceContext, canonicalPlanLaunch!.plan, canonicalProviderProjection),
      intelligence_context: publicIntelligenceLaunchContext(intelligenceContext),
      required_mcp_tools: request.constraints.required_mcp_tools ?? [],
      mcp_scope: workerMcpProjection ? WORKER_REQUIRED_MCP_SCOPE : null,
      ...(workerMcpProjection ? { worker_mcp_projection: workerMcpProjection } : {}),
      sandbox,
      model: null,
      reasoning_effort: null,
      config: resolvedConfigInput.config,
      skip_git_repo_check: skipGitRepoCheck,
      resumable,
      ephemeral,
      json_events: agentRuntime.jsonEvents,
      implementation_identity: workerImplementationIdentity(),
      prompt_byte_length: promptBytes,
      max_output_bytes: state.policy.maxOutputBytes,
      max_run_ms: maxRunMs,
      max_tool_rounds: state.policy.maxToolRounds,
      environment_keys: Object.keys(environment).sort(),
    };
    invocation = agentRuntimeServerBuildInvocation(resolvedWorkerConfig, environment);
  } else {
    const codexRuntime = state.policy.runtimes.codex;
    const argv = buildCodexArgv({
      cwd,
      sandbox,
      schemaPath: dryRunPaths.schemaPath,
      lastMessagePath: dryRunPaths.lastMessagePath,
      workerSessionId: resumeSessionId ?? undefined,
      ephemeral,
      skipGitRepoCheck,
      config: resolvedConfigInput.config,
      allowedRoots: state.policy.allowedRoots,
    });
    resolvedWorkerConfig = {
      runtime: 'codex',
      authority,
      cognition,
      command: runtimeAvailability.command ?? codexRuntime.command,
      command_args: codexRuntime.commandArgs,
      argv,
      cwd,
      provider: providerResolution.provider,
      provider_source: providerResolution.source,
      required_mcp_tools: request.constraints.required_mcp_tools ?? [],
      sandbox,
      model: resolvedConfigInput.model,
      reasoning_effort: resolvedConfigInput.reasoning_effort,
      config: resolvedConfigInput.config,
      skip_git_repo_check: skipGitRepoCheck,
      resumable,
      ephemeral,
      json_events: codexRuntime.jsonEvents,
      implementation_identity: workerImplementationIdentity(),
      prompt_byte_length: promptBytes,
      max_output_bytes: state.policy.maxOutputBytes,
      max_run_ms: maxRunMs,
      max_tool_rounds: state.policy.maxToolRounds,
      environment_keys: Object.keys(environment).sort(),
    };
    invocation = codexBuildInvocation(resolvedWorkerConfig, environment);
  }
  assertExplicitCognitionTuple(resolvedWorkerConfig, state);

  const configResolution = configResolutionMetadata({ requestedOverrides, resolvedConfigInput, runtime, cognition, provider: providerResolution.provider, policy: state.policy, cognitionDefaultSource: providerResolution.provider ? state.cognitionDefaults?.sources[providerResolution.provider]?.[cognition] ?? 'provider_registry' : 'generic_cognition_default' });
  const warnings = [
    'dry_run_paths_are_placeholders: invocation argv uses <dry-run> paths and does not create run artifacts',
    ...(configResolution.model_source === 'runtime_default_opaque' ? ['model_delegated_to_runtime_default: concrete model is not knowable before launching this runtime'] : []),
    ...(configResolution.reasoning_effort_source === 'runtime_default_opaque' ? ['reasoning_effort_delegated_to_runtime_default: concrete reasoning effort is not knowable before launching this runtime'] : []),
    ...preflight.filter((check) => check.status === 'blocked').map((check) => `blocked_preflight: ${check.message}`),
    ...(implementationGate.admitted === true ? [] : [`blocked_implementation_gate: ${String(implementationGate.blocking_status ?? implementationGate.reason_code ?? 'worker_implementation_stale')}`]),
    ...(intelligenceContext?.status === 'ready' ? [] : intelligenceContext ? [`blocked_intelligence_context: ${intelligenceContext.missing.join(', ')}`] : []),
  ];
  const intelligenceContextLaunchability = runtime === 'narada-agent-runtime-server' && intelligenceContext
    ? intelligenceContextStatus(intelligenceContext)
    : { required: false, ready: true, missing: [] };
  const effectiveLaunchable = runtimeAvailability.available && launchability.launchable && implementationGate.admitted === true && intelligenceContextLaunchability.ready;
  return {
    schema: 'narada.worker.config_resolve.v1',
    status: 'ok',
    dry_run: true,
    requested_mode: requestedMode,
    resume_worker_session_id: resumeSessionId,
    launchable: effectiveLaunchable,
    launchability: {
      ...launchability,
      intelligence_context: intelligenceContextLaunchability,
      runtime_available: runtimeAvailability.available,
      implementation_gate: implementationGate,
      effective_launchable: effectiveLaunchable,
    },
    resolved_worker_config: resolvedWorkerConfig,
    invocation: invocationArtifact(invocation, resolvedWorkerConfig),
    preflight,
    requested_mcp_tools: request.constraints.required_mcp_tools ?? [],
    mcp_tool_verification: mcpToolVerification(request.constraints.required_mcp_tools ?? [], runtime),
    output_contract: outputContract,
    runtime_availability: runtimeAvailability.available
      ? { available: true, command: runtimeAvailability.command ?? resolvedWorkerConfig.command }
      : { available: false, reason: runtimeAvailability.reason ?? null, remediation: runtimeAvailability.remediation ?? null, command: runtimeAvailability.command ?? null },
    config_resolution: configResolution,
    implementation_identity: workerImplementationIdentity(),
    implementation_gate: implementationGate,
    warnings,
  };
}

function naradaAgentRuntimeSiteBinding(cwd: string, siteBinding: { siteRoot: string; workspaceRoot: string; marker: string; source: 'explicit' | 'bound_environment' | 'nearest_marker' }): Record<string, unknown> {
  return {
    site_bound: true,
    site_root: siteBinding.siteRoot,
    workspace_root: siteBinding.workspaceRoot,
    source: siteBinding.source === 'explicit'
      ? 'constraints.site_root'
      : siteBinding.source === 'bound_environment'
        ? 'bound_environment'
        : 'nearest_parent_marker',
    matched_marker: siteBinding.marker,
    required_markers: [...NARADA_SITE_ROOT_MARKERS],
    environment_keys: ['NARADA_SITE_ROOT', 'NARADA_WORKSPACE_ROOT', 'NARADA_AGENT_ID', 'NARADA_CARRIER_SESSION_ID'],
    remediation: NARADA_AGENT_RUNTIME_SITE_REMEDIATION,
  };
}

function readWorkerIntelligenceContext(state: WorkerMcpState, sessionSiteRoot: string): IntelligenceLaunchContext {
  try {
    return loadIntelligenceLaunchContext({
      userSiteRoot: resolve(state.userSiteRoot ?? state.siteRoot ?? state.env.NARADA_SITE_ROOT ?? sessionSiteRoot),
      sessionSiteRoot,
      processEnv: state.env,
    });
  } catch (error) {
    if (error instanceof IntelligenceLaunchContextError) {
      throw diagnosticError(error.codeName, error.message, error.details);
    }
    throw error;
  }
}

function requireCanonicalInvocationPlan(
  state: WorkerMcpState,
  sessionSiteRoot: string,
  request: WorkerRunToolInput,
): { context: IntelligenceLaunchContext; planRef: string | null; plan: CanonicalInvocationPlanBinding | null } {
  const legacyFields = [
    request.constraints.provider !== undefined ? 'constraints.provider' : null,
    request.constraints.cognition !== undefined ? 'constraints.cognition' : null,
    request.constraints.overrides?.model !== undefined ? 'constraints.overrides.model' : null,
    request.constraints.overrides?.reasoning_effort !== undefined ? 'constraints.overrides.reasoning_effort' : null,
    request.constraints.overrides?.config && Object.prototype.hasOwnProperty.call(request.constraints.overrides.config, 'model')
      ? 'constraints.overrides.config.model'
      : null,
    request.constraints.overrides?.config && Object.prototype.hasOwnProperty.call(request.constraints.overrides.config, 'model_reasoning_effort')
      ? 'constraints.overrides.config.model_reasoning_effort'
      : null,
  ].filter((field): field is string => field !== null);
  if (legacyFields.length > 0) {
    throw diagnosticError(
      'worker_canonical_invocation_plan_override_rejected',
      'narada-agent-runtime-server accepts an immutable canonical invocation plan, not provider/model/cognition overrides.',
      {
        runtime: 'narada-agent-runtime-server',
        legacy_fields: legacyFields,
        migration: 'Resolve the invocation plan in Narada and pass only invocation_plan_ref.',
      },
    );
  }
  const context = readWorkerIntelligenceContext(state, sessionSiteRoot);
  const explicitPlanRef = request.constraints.invocation_plan_ref?.trim() || null;
  const contextPlanRef = context.invocation_plan_ref;
  if (explicitPlanRef && !/^plan:[A-Za-z0-9][A-Za-z0-9._:-]*$/.test(explicitPlanRef)) {
    throw diagnosticError('worker_canonical_invocation_plan_invalid', 'invocation_plan_ref must be an immutable canonical plan reference.');
  }
  if (explicitPlanRef && contextPlanRef && explicitPlanRef !== contextPlanRef) {
    throw diagnosticError('worker_canonical_invocation_plan_mismatch', 'The request plan reference differs from the Site launch context plan reference.', {
      request_plan_ref: explicitPlanRef,
      context_plan_ref: contextPlanRef,
    });
  }
  const planRef = explicitPlanRef ?? contextPlanRef;
  if (context.status !== 'ready' || !context.target_site) {
    throw diagnosticError('worker_intelligence_context_required', 'The agent runtime requires a complete intelligence launch context.', {
      context_path: context.context_path,
      missing: context.missing,
    });
  }
  if (!planRef) {
    return { context, planRef: null, plan: null };
  }
  let plan: CanonicalInvocationPlanBinding;
  try {
    plan = readCanonicalInvocationPlan({
      databasePath: context.registry_db_path,
      planRef,
      expectedPurpose: 'local-agent-runtime',
      expectedTargetSite: context.target_site,
    });
  } catch (error) {
    if (error instanceof CanonicalInvocationPlanError) {
      throw diagnosticError(error.codeName, error.message, error.details);
    }
    throw error;
  }
  return {
    planRef,
    plan,
    context: {
      ...context,
      invocation_plan_ref: planRef,
      environment: { ...context.environment, NARADA_INTELLIGENCE_PLAN_REF: planRef },
    },
  };
}

function requireWorkerIntelligenceContext(state: WorkerMcpState, sessionSiteRoot: string, environment: Record<string, string>): IntelligenceLaunchContext {
  const context = readWorkerIntelligenceContext(state, sessionSiteRoot);
  Object.assign(environment, projectIntelligenceLaunchContext(context));
  return context;
}

function intelligenceContextStatus(context: IntelligenceLaunchContext): Record<string, unknown> {
  return {
    required: true,
    ready: context.status === 'ready',
    code: context.status === 'ready' ? null : 'worker_intelligence_context_required',
    reason: context.status === 'ready' ? null : 'Narada runtime requires a complete intelligence launch context before spawn',
    context_path: context.context_path,
    context_exists: context.context_exists,
    registry_db_path: context.registry_db_path,
    registry_db_exists: context.registry_db_exists,
    target_site: context.target_site,
    user_site: context.user_site,
    missing: context.missing,
  };
}

type CanonicalProviderProjection = {
  provider: string;
  source: 'canonical_plan_store';
  credential_env_names: string[];
};

function canonicalPlanRuntimeBinding(context: IntelligenceLaunchContext, plan: CanonicalInvocationPlanBinding | null, projection: CanonicalProviderProjection | null): Record<string, unknown> {
  if (!plan || !projection) {
    return {
      schema: 'narada.worker.runtime-preflight-plan-binding.v1',
      source: 'agent_runtime_canonical_preflight',
      plan_ref: null,
      registry_db_path: context.registry_db_path,
      target_site: context.target_site,
      selector_crosses_worker_boundary: false,
    };
  }
  return {
    schema: 'narada.worker.canonical-plan-binding.v1',
    source: 'narada-canonical-invocation-plan',
    plan_ref: context.invocation_plan_ref,
    provider_model_resolution: 'narada-runtime',
    provider: projection.provider,
    provider_source: projection.source,
    intent_ref: plan.intent_ref,
    purpose: plan.purpose,
    model_ref: plan.model_ref,
    model_provider_ref: plan.model_provider_ref,
    offering_ref: plan.offering_ref,
    invocation_model_key: plan.invocation_model_key,
    options: plan.options,
    snapshot_digest: plan.snapshot_digest,
    valid_until: plan.valid_until,
    credential_env_names: [...projection.credential_env_names],
    selector_crosses_worker_boundary: false,
    credential_materialization: 'final-adapter-boundary',
  };
}

function projectCanonicalProviderCredential(state: WorkerMcpState, environment: Record<string, string>, plan: CanonicalInvocationPlanBinding): CanonicalProviderProjection {
  const provider = plan.provider;
  resolveNaradaAgentRuntimeProvider(provider, state.policy);
  const metadata = state.providerRuntimeMetadata[provider];
  if (!metadata) throw diagnosticError('worker_provider_metadata_missing', 'worker_provider_metadata_missing', { provider });

  // Canonical NARS owns provider/model selection. The worker boundary may
  // carry only the selected provider's credential transport, never the
  // ambient selector or legacy NARADA_AI_* tuple.
  state.ensureProviderCredential?.(provider);
  const bindingEnvironment = { ...state.env };
  delete bindingEnvironment.NARADA_INTELLIGENCE_PROVIDER;
  delete bindingEnvironment.NARADA_AI_API_KEY;
  delete bindingEnvironment.NARADA_AI_BASE_URL;
  delete bindingEnvironment.NARADA_AI_MODEL;
  delete bindingEnvironment.NARADA_AI_THINKING;
  const binding = resolveWorkerProviderRuntimeBinding({
    provider,
    metadataByProvider: state.providerRuntimeMetadata,
    env: bindingEnvironment,
    model: null,
    reasoningEffort: null,
  });
  projectCanonicalProviderCredentialEnvironment(environment, binding, state.providerRuntimeMetadata);
  return {
    provider,
    source: 'canonical_plan_store',
    credential_env_names: [...binding.credential_env_names],
  };
}

function invocationArtifact(invocation: Invocation, resolvedWorkerConfig: ResolvedWorkerConfig): Record<string, unknown> {
  return {
    command: invocation.command,
    argv: invocation.argv,
    cwd: invocation.cwd,
    authority: resolvedWorkerConfig.authority,
    authority_signal: resolvedWorkerConfig.runtime === 'narada-agent-runtime-server'
      ? { kind: 'argv', name: '--authority', value: resolvedWorkerConfig.authority }
      : null,
    implementation_identity: resolvedWorkerConfig.implementation_identity ?? workerImplementationIdentity(),
    environment_keys: resolvedWorkerConfig.environment_keys,
  };
}

function normalizeStringList(value: unknown, code: string): string[] {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw diagnosticError(code, code);
  return value.map((item) => requiredNonEmptyString(item, code));
}

function normalizePreflightPaths(value: unknown): WorkerPreflightPath[] {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw diagnosticError('worker_invalid_preflight_paths', 'worker_invalid_preflight_paths');
  return value.map((item, index) => {
    const record = asRecord(item);
    const path = requiredNonEmptyString(record.path, 'worker_invalid_preflight_paths');
    const access = String(record.access ?? 'read').trim();
    if (access !== 'read' && access !== 'write' && access !== 'create') throw diagnosticError('worker_invalid_preflight_paths', 'worker_invalid_preflight_path_access', { index, access });
    const result: WorkerPreflightPath = { path, access };
    if (record.label !== undefined && record.label !== null && String(record.label).trim()) result.label = String(record.label).trim();
    return result;
  });
}

export async function workerRun(args: Record<string, unknown>, state: WorkerMcpState, resumeSessionId: string | null, context: WorkerRequestContext = {}, auditTool = resumeSessionId ? 'worker_resume' : 'worker_run'): Promise<Record<string, unknown>> {
  acquireWorkerRunSlot(state);
  let releaseOnReturn = true;
  const releaseSlot = () => {
    releaseWorkerRunSlot(state);
    releaseOnReturn = false;
  };
  const deferRelease = () => { releaseOnReturn = false; };
  try {
    return await workerRunInner(args, state, resumeSessionId, context, auditTool, { releaseSlot, deferRelease });
  } finally {
    if (releaseOnReturn) releaseWorkerRunSlot(state);
  }
}

async function workerRunInner(args: Record<string, unknown>, state: WorkerMcpState, resumeSessionId: string | null, context: WorkerRequestContext, auditTool: string, slot: { releaseSlot: () => void; deferRelease: () => void }): Promise<Record<string, unknown>> {
  const startedAt = new Date();
  if (args.config_overrides !== undefined) throw diagnosticError('worker_raw_config_overrides_not_allowed');
  const request = normalizeWorkerRunToolInput(args, resumeSessionId !== null);
  if (resumeSessionId !== null && request.idempotency_key) throw diagnosticError('worker_resume_idempotency_key_not_allowed');
  const inheritedSession = resumeSessionId ? readWorkerSessionRecord(state.policy, resumeSessionId) : null;
  if (inheritedSession) inheritSessionConstraints(request, inheritedSession.resolved_worker_config);
  const idempotency = request.idempotency_key
    ? workerRunIdempotency(request.idempotency_key, request, resumeSessionId)
    : null;
  if (idempotency && existsSync(resolve(state.policy.runRoot, idempotency.runId))) {
    return replayIdempotentWorkerRun(state, idempotency);
  }
  const authority = resolveAuthority(request.constraints.authority, state.policy);
  const cognition = resolveCognition(request.constraints.cognition, state.policy);
  let overrides = request.constraints.overrides ?? {};
  const runtime = validateRuntime(overrides.runtime, state.policy);
  rejectNaradaAgentRuntimeProviderForRuntime(request.constraints.provider, runtime);
  const cwd = resolveWorkingDirectory(request.constraints.cwd, state.policy);
  const preResolvedSiteBinding = runtime === 'narada-agent-runtime-server'
    ? resolveNaradaSiteBinding(cwd, state.policy, request.constraints.site_root, {
      siteRoot: state.env.NARADA_SITE_ROOT,
      workspaceRoot: state.env.NARADA_WORKSPACE_ROOT,
    })
    : null;
  const canonicalPlanLaunch = runtime === 'narada-agent-runtime-server'
    ? requireCanonicalInvocationPlan(state, preResolvedSiteBinding!.siteRoot, request)
    : null;
  const { effectiveCognitionDefault, providerResolution } = canonicalPlanLaunch
    ? { effectiveCognitionDefault: null, providerResolution: { provider: null, source: 'canonical_invocation_plan' } }
    : resolveCognitionProvider(runtime, request, cognition, state);
  const sandbox = resolveSandbox(overrides.sandbox ?? defaultSandboxForAuthority(authority), state.policy, runtime);
  if (runtime !== 'narada-agent-runtime-server') {
    applyCognitionDefaults(request, cognition, state, runtime, providerResolution.provider);
    applyEffectiveCognitionTuple(request, effectiveCognitionDefault);
  }
  overrides = request.constraints.overrides ?? {};
  const resolvedConfigInput = resolveConfig(overrides, state.policy);
  const maxRunMs = boundedInteger(request.constraints.max_run_ms, state.policy.maxRunMs, 1, state.policy.maxRunMs, 'worker_max_run_ms_invalid');
  const requestedMode = request.intent.mode ?? defaultModeForAuthority(authority);
  request.intent.mode = requestedMode;
  const preflight = buildPreflight({ cwd, authority, mode: requestedMode, waitForCompletion: request.constraints.wait_for_completion === true, isResume: resumeSessionId !== null, preflightPaths: request.constraints.preflight_paths ?? [], requiredMcpTools: request.constraints.required_mcp_tools ?? [], allowedRoots: state.policy.allowedRoots });
  const outputContract = outputContractForRequest(request, requestedMode);
  enforcePreflightForMode(requestedMode, preflight);

  // Runtime availability preflight: fail fast with a clear remediation if the selected runtime is not runnable.
  const environment = environmentForWorker(state.env);
  delete environment.NARADA_WORKER_MCP_CONFIG;
  const runtimeAvailability = checkRuntimeAvailability(runtime, state.policy, environment);
  if (!runtimeAvailability.available) {
    throw diagnosticError('worker_runtime_unavailable', `worker_runtime_unavailable:${runtime}`, {
      runtime,
      reason: runtimeAvailability.reason,
      remediation: runtimeAvailability.remediation,
    });
  }
  ensureRequiredMcpToolsProjectable(request.constraints.required_mcp_tools ?? [], runtime);
  assertWorkerImplementationFresh();

  const prompt = buildWorkerPrompt({ intent: request.intent, cwd, mode: requestedMode, runtime, preflight, outputContract, exitInterview: request.constraints.exit_interview !== false && requestedMode.startsWith('implement'), requiredMcpTools: request.constraints.required_mcp_tools ?? [], authority, allowedRoots: state.policy.allowedRoots });
  const resumable = resumeSessionId !== null || request.constraints.resumable === true;
  const ephemeral = !resumable;
  const promptBytes = Buffer.byteLength(prompt, 'utf8');
  if (promptBytes > state.policy.maxPromptBytes) throw diagnosticError('worker_prompt_too_large', 'worker_prompt_too_large', { prompt_byte_length: promptBytes, max_prompt_bytes: state.policy.maxPromptBytes });

  let runRecord: RunRecordPaths;
  if (idempotency) {
    try {
      runRecord = createRunRecord(state.policy, idempotency.runId);
    } catch (error) {
      if (!isAlreadyExistsError(error)) throw error;
      return replayIdempotentWorkerRun(state, idempotency);
    }
    writeJson(join(runRecord.runDir, 'idempotency.json'), {
      schema: 'narada.worker.run_idempotency.v1',
      run_id: runRecord.runId,
      idempotency_key: idempotency.key,
      request_fingerprint: idempotency.requestFingerprint,
      created_at: startedAt.toISOString(),
    });
  } else {
    runRecord = createRunRecord(state.policy);
  }
  writeWorkerOutputSchema(runRecord.schemaPath, outputContract);
  const skipGitRepoCheck = optionalBoolean(overrides.skip_git_repo_check, 'skip_git_repo_check');

  let resolvedWorkerConfig: ResolvedWorkerConfig;
  let invocation: Invocation;
  let intelligenceContext: IntelligenceLaunchContext | null = canonicalPlanLaunch?.context ?? null;

  if (runtime === 'narada-agent-runtime-server') {
    const agentRuntime = state.policy.runtimes.naradaAgentRuntimeServer;
    const resolvedSiteBinding = preResolvedSiteBinding!;
    const siteRoot = resolvedSiteBinding.siteRoot;
    const siteBinding = naradaAgentRuntimeSiteBinding(cwd, resolvedSiteBinding);
    intelligenceContext = canonicalPlanLaunch?.context ?? requireWorkerIntelligenceContext(state, siteRoot, environment);
    Object.assign(environment, projectIntelligenceLaunchContext(intelligenceContext));
    const canonicalProviderProjection = canonicalPlanLaunch!.plan
      ? projectCanonicalProviderCredential(state, environment, canonicalPlanLaunch!.plan)
      : null;
    const workerSessionId = resumeSessionId ?? runRecord.runId;
    environment.NARADA_SITE_ROOT = siteRoot;
    environment.NARADA_WORKSPACE_ROOT = resolvedSiteBinding.workspaceRoot;
    environment.NARADA_AGENT_ID ??= 'narada.architect';
    environment.NARADA_CARRIER_SESSION_ID = workerSessionId;
    environment.NARADA_MAX_TOOL_ROUNDS = String(state.policy.maxToolRounds);
    const workerMcpProjection = buildWorkerMcpProjection(request.constraints.required_mcp_tools ?? []);
    if (workerMcpProjection) {
      environment.NARADA_MCP_SCOPE = WORKER_REQUIRED_MCP_SCOPE;
      environment.NARADA_WORKER_MCP_CONFIG = JSON.stringify(workerMcpProjection);
    }
    const argv = buildAgentRuntimeServerArgv({ authority, workerSessionId });
    const baseConfig: ResolvedWorkerConfig = {
      runtime: 'narada-agent-runtime-server',
      authority,
      cognition: null,
      command: runtimeAvailability.command ?? agentRuntime.command,
      command_args: agentRuntime.commandArgs,
      argv,
      cwd,
      site_root: siteRoot,
      workspace_root: resolvedSiteBinding.workspaceRoot,
      site_bound: true,
      site_marker: resolvedSiteBinding.marker,
      site_root_source: resolvedSiteBinding.source,
      site_binding: siteBinding,
      provider: null,
      provider_source: 'canonical_invocation_plan',
      provider_runtime_binding: canonicalPlanRuntimeBinding(intelligenceContext, canonicalPlanLaunch!.plan, canonicalProviderProjection),
      intelligence_context: publicIntelligenceLaunchContext(intelligenceContext),
      required_mcp_tools: request.constraints.required_mcp_tools ?? [],
      mcp_scope: workerMcpProjection ? WORKER_REQUIRED_MCP_SCOPE : null,
      ...(workerMcpProjection ? { worker_mcp_projection: workerMcpProjection } : {}),
      sandbox,
      model: null,
      reasoning_effort: null,
      config: resolvedConfigInput.config,
      skip_git_repo_check: skipGitRepoCheck,
      resumable,
      ephemeral,
      json_events: agentRuntime.jsonEvents,
      implementation_identity: workerImplementationIdentity(),
      prompt_byte_length: promptBytes,
      max_output_bytes: state.policy.maxOutputBytes,
      max_run_ms: maxRunMs,
      max_tool_rounds: state.policy.maxToolRounds,
      environment_keys: Object.keys(environment).sort(),
    };
    resolvedWorkerConfig = baseConfig;
    invocation = agentRuntimeServerBuildInvocation(baseConfig, environment);
  } else {
    const codexRuntime = state.policy.runtimes.codex;
    const argv = buildCodexArgv({
      cwd,
      sandbox,
      schemaPath: runRecord.schemaPath,
      lastMessagePath: runRecord.lastMessagePath,
      workerSessionId: resumeSessionId ?? undefined,
      ephemeral,
      skipGitRepoCheck,
      config: resolvedConfigInput.config,
      allowedRoots: state.policy.allowedRoots,
    });
    const baseConfig: ResolvedWorkerConfig = {
      runtime: 'codex',
      authority,
      cognition,
      command: runtimeAvailability.command ?? codexRuntime.command,
      command_args: codexRuntime.commandArgs,
      argv,
      cwd,
      provider: providerResolution.provider,
      provider_source: providerResolution.source,
      required_mcp_tools: request.constraints.required_mcp_tools ?? [],
      sandbox,
      model: resolvedConfigInput.model,
      reasoning_effort: resolvedConfigInput.reasoning_effort,
      config: resolvedConfigInput.config,
      skip_git_repo_check: skipGitRepoCheck,
      resumable,
      ephemeral,
      json_events: codexRuntime.jsonEvents,
      implementation_identity: workerImplementationIdentity(),
      prompt_byte_length: promptBytes,
      max_output_bytes: state.policy.maxOutputBytes,
      max_run_ms: maxRunMs,
      max_tool_rounds: state.policy.maxToolRounds,
      environment_keys: Object.keys(environment).sort(),
    };
    resolvedWorkerConfig = baseConfig;
    invocation = codexBuildInvocation(baseConfig, environment);
  }
  assertExplicitCognitionTuple(resolvedWorkerConfig, state);
  const executorRequest: WorkerExecutorRequest = {
    schema: 'narada.worker.executor_request.v1',
    run_id: runRecord.runId,
    ...(idempotency ? {
      idempotency_key: idempotency.key,
      request_fingerprint: idempotency.requestFingerprint,
    } : {}),
    resume_worker_session_id: resumeSessionId,
    intent: request.intent,
    requested_mode: requestedMode,
    preflight,
    requested_mcp_tools: request.constraints.required_mcp_tools ?? [],
    mcp_tool_verification: mcpToolVerification(request.constraints.required_mcp_tools ?? [], runtime),
    output_contract: outputContract,
    resolved_execution_policy: resolvedWorkerConfig,
  };

  mkdirSync(runRecord.runDir, { recursive: true });
  writeJson(runRecord.requestPath, request);
  writeJson(runRecord.executorRequestPath, executorRequest);
  writeJson(runRecord.resolvedConfigPath, resolvedWorkerConfig);
  writeText(runRecord.promptPath, prompt);
  writeJson(runRecord.invocationPath, invocationArtifact(invocation, resolvedWorkerConfig));
  writeText(runRecord.eventsPath, '');
  writeText(runRecord.diagnosticPath, '');

  const waitForCompletion = request.constraints.wait_for_completion === true;
  const waitTimeoutMs = boundedInteger(
    request.constraints.wait_timeout_ms,
    MAX_SYNCHRONOUS_WAIT_MS,
    1,
    MAX_SYNCHRONOUS_WAIT_MS,
    'worker_wait_timeout_invalid',
  );
  const launchedAt = new Date();
  const runningPayload = buildWorkerRunPayload({
    status: 'running',
    runRecord,
    runtime,
    workerSessionId: resumeSessionId,
    resolvedWorkerConfig,
    executorRequest,
    startedAt,
    finishedAt: null,
    error: null,
    metadata: buildRunMetadata({ requestedMode, preflight, status: 'running', output: null, error: null }),
  });
  writeJson(runRecord.resultPath, runningPayload);
  registerActiveRun(state.policy, {
    schema: 'narada.worker.active_run.v1',
    run_id: runRecord.runId,
    run_dir: runRecord.runDir,
    started_at: startedAt.toISOString(),
    owner_pid: process.pid,
    registered_at: new Date().toISOString(),
  });
  const runAbortController = new AbortController();
  const abortFromParent = () => runAbortController.abort();
  if (context.abortSignal?.aborted) runAbortController.abort();
  else if (context.abortSignal) context.abortSignal.addEventListener('abort', abortFromParent, { once: true });
  state.activeRunControllers ??= new Map();
  state.activeRunCompletions ??= new Map();
  state.activeRunCancellationRequests ??= new Set();
  state.activeRunControllers.set(runRecord.runId, runAbortController);
  const cleanupActiveRun = () => {
    unregisterActiveRun(state.policy, runRecord.runId);
    if (context.abortSignal) context.abortSignal.removeEventListener('abort', abortFromParent);
    state.activeRunControllers?.delete(runRecord.runId);
    state.activeRunCompletions?.delete(runRecord.runId);
    state.activeRunCancellationRequests?.delete(runRecord.runId);
  };
  const completion = () => completeWorkerRun({
    state,
    runRecord,
    invocation,
    prompt,
    resolvedWorkerConfig,
    runtime,
    resumeSessionId,
    resumable,
    startedAt,
    executorRequest,
    auditTool,
    inheritedOriginTool: inheritedSession?.origin_tool,
    inheritedCreatedRunId: inheritedSession?.created_run_id,
    abortSignal: runAbortController.signal,
  });
  if (!waitForCompletion) {
    audit(state.policy, { tool: auditTool, payload: { ...runningPayload, event: 'worker_run_started', launched_at: launchedAt.toISOString() } });
    slot.deferRelease();
    const pendingCompletion = completion();
    state.activeRunCompletions.set(runRecord.runId, pendingCompletion);
    void pendingCompletion.catch(() => {
      // The failure payload has already been written by completeWorkerRun.
    }).finally(() => {
      cleanupActiveRun();
      slot.releaseSlot();
    });
    return runningPayload;
  }
  const activeCompletion = completion();
  state.activeRunCompletions.set(runRecord.runId, activeCompletion);
  let continuedAsynchronously = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const outcome = await Promise.race([
      activeCompletion.then((result) => ({ status: 'completed' as const, result })),
      new Promise<{ status: 'running' }>((resolvePromise) => {
        timer = setTimeout(() => resolvePromise({ status: 'running' }), waitTimeoutMs);
      }),
    ]);
    if (outcome.status === 'completed') return outcome.result;

    continuedAsynchronously = true;
    slot.deferRelease();
    void activeCompletion.catch(() => {
      // The failure payload has already been written by completeWorkerRun.
    }).finally(() => {
      cleanupActiveRun();
      slot.releaseSlot();
    });
    return {
      ...runningPayload,
      wait_for_completion: {
        requested: true,
        status: 'continued_asynchronously',
        wait_timeout_ms: waitTimeoutMs,
        reason: 'bounded_synchronous_wait_elapsed',
      },
      next_action: {
        tool: 'worker_run_wait',
        arguments: { run_id: runRecord.runId },
      },
    };
  } finally {
    if (timer) clearTimeout(timer);
    if (!continuedAsynchronously) cleanupActiveRun();
  }
}

async function completeWorkerRun(options: {
  state: WorkerMcpState;
  runRecord: RunRecordPaths;
  invocation: Invocation;
  prompt: string;
  resolvedWorkerConfig: ResolvedWorkerConfig;
  runtime: string;
  resumeSessionId: string | null;
  resumable: boolean;
  startedAt: Date;
  executorRequest: WorkerExecutorRequest;
  auditTool: string;
  inheritedOriginTool?: string;
  inheritedCreatedRunId?: string;
  abortSignal?: AbortSignal;
}): Promise<Record<string, unknown>> {
  const { state, runRecord, invocation, prompt, resolvedWorkerConfig, runtime, resumeSessionId, resumable, startedAt, executorRequest, auditTool } = options;
  try {
  const runner = runtime === 'narada-agent-runtime-server'
      ? runAgentRuntimeServerInvocation
      : runCodexInvocation;
  const codexResult = await runner({
    invocation,
    prompt,
    eventsPath: runRecord.eventsPath,
    diagnosticPath: runRecord.diagnosticPath,
    lastMessagePath: runRecord.lastMessagePath,
    maxRunMs: resolvedWorkerConfig.max_run_ms,
    abortSignal: options.abortSignal,
  });
  const parsed = parseLastMessage(runRecord.lastMessagePath);
  const rawOutcome = resultStatus(codexResult, parsed);
  const cancellationRequested = state.activeRunCancellationRequests?.has(runRecord.runId) === true;
  const outcome = cancellationRequested
    ? { status: 'cancelled' as const, error: 'cancelled', warnings: [...rawOutcome.warnings, 'worker_run_cancellation_requested'] }
    : rawOutcome;
  const output = parsed.ok ? parsed.data : null;
  const finishedAt = new Date();
  const runtimeDiagnostics = buildRuntimeDiagnostics({
    runtime,
    codexResult,
    parsed,
    outcomeError: outcome.error,
    eventsPath: runRecord.eventsPath,
    diagnosticPath: runRecord.diagnosticPath,
  });
  const provenance = asRecord(runtimeDiagnostics?.error_provenance);
  const primaryError = outcome.status === 'completed' ? outcome.error : typeof provenance.primary_error === 'string' && provenance.primary_error ? provenance.primary_error : outcome.error;
  const payload = buildWorkerRunPayload({
    status: outcome.status,
    runRecord,
    runtime,
    workerSessionId: codexResult.worker_session_id ?? resumeSessionId,
    resolvedWorkerConfig,
    executorRequest,
    startedAt,
    finishedAt,
    error: primaryError,
    runtimeWarnings: outcome.warnings,
    output,
    workerOutputError: parsed.ok === false ? { reason: parsed.reason, message: parsed.message, state: workerOutputState(parsed) } : undefined,
    runtimeDiagnostics,
    metadata: buildRunMetadata({ requestedMode: executorRequest.requested_mode, preflight: executorRequest.preflight, status: outcome.status, output, error: primaryError }),
  });
  writeJson(runRecord.resultPath, payload);
  const workerSessionId = codexResult.worker_session_id ?? resumeSessionId;
  if (workerSessionId && (outcome.status === 'completed' || outcome.status === 'completed_with_errors') && resumable) {
    writeWorkerSessionRecord(state.policy, {
      schema: 'narada.worker.session.v1',
      worker_session_id: workerSessionId,
      origin_tool: options.inheritedOriginTool ?? auditTool,
      created_run_id: options.inheritedCreatedRunId ?? runRecord.runId,
      updated_run_id: runRecord.runId,
      resolved_worker_config: resolvedWorkerConfig,
      updated_at: finishedAt.toISOString(),
    });
  }
  audit(state.policy, { tool: auditTool, payload });
  if (outcome.status === 'failed') throw diagnosticError('worker_runtime_failed', 'worker_runtime_failed', { error: outcome.error, run_id: runRecord.runId, run_dir: runRecord.runDir });
  if (outcome.status === 'cancelled') throw diagnosticError('worker_runtime_cancelled', 'worker_runtime_cancelled', { run_id: runRecord.runId, run_dir: runRecord.runDir });
  return payload;
  } catch (error) {
    const codeName = (error as { codeName?: unknown })?.codeName;
    if (codeName === 'worker_runtime_failed' || codeName === 'worker_runtime_cancelled') throw error;
    const finishedAt = new Date();
    const message = error instanceof Error ? error.message : String(error);
    const cancellationRequested = state.activeRunCancellationRequests?.has(runRecord.runId) === true;
    const status = cancellationRequested ? 'cancelled' as const : 'failed' as const;
    const terminalError = cancellationRequested ? 'cancelled' : message;
    const payload = buildWorkerRunPayload({
      status,
      runRecord,
      runtime,
      workerSessionId: resumeSessionId,
      resolvedWorkerConfig,
      executorRequest,
      startedAt,
      finishedAt,
      error: terminalError,
      runtimeDiagnostics: {
        schema: 'narada.worker.runtime_diagnostics.v1',
        phase: cancellationRequested ? 'worker_cancellation_requested' : 'worker_delegation_exception',
        runtime,
        error: terminalError,
        error_provenance: {
          schema: 'narada.worker.error_provenance.v1',
          primary_error: terminalError,
          primary_source: cancellationRequested ? 'cancellation' : 'worker_delegation',
          transport_error: null,
          provider_error: null,
          event_error: null,
          artifact_error: null,
          outcome_error: message,
          observed_error_candidates: [],
        },
        remediation: runtimeFailureRemediation(cancellationRequested ? 'worker_cancellation_requested' : 'worker_delegation_exception'),
        diagnostic_tail: readDiagnosticTail(runRecord.diagnosticPath),
        stdout_tail: readTextTail(runRecord.eventsPath, 800),
      },
      metadata: buildRunMetadata({ requestedMode: executorRequest.requested_mode, preflight: executorRequest.preflight, status, output: null, error: terminalError }),
    });
    writeJson(runRecord.resultPath, payload);
    audit(state.policy, { tool: auditTool, payload });
    if (cancellationRequested) throw diagnosticError('worker_runtime_cancelled', 'worker_runtime_cancelled', { run_id: runRecord.runId, run_dir: runRecord.runDir });
    throw error;
  }
}

function buildWorkerRunPayload(options: {
  status: 'running' | 'completed' | 'completed_with_errors' | 'failed' | 'cancelled';
  runRecord: RunRecordPaths;
  runtime: string;
  workerSessionId: string | null;
  resolvedWorkerConfig: ResolvedWorkerConfig;
  executorRequest: WorkerExecutorRequest;
  startedAt: Date;
  finishedAt: Date | null;
  error: string | null;
  runtimeWarnings?: string[];
  output?: WorkerOutput | null;
  workerOutputError?: { reason: string; message: string; state: 'absent' | 'invalid_json' | 'invalid_shape' };
  runtimeDiagnostics?: Record<string, unknown>;
  metadata: WorkerRunMetadata;
}): Record<string, unknown> {
  const resultState = options.status === 'running'
    ? {
        state: 'pending',
        terminal: false,
        scaffold: true,
        message: 'Worker run is active; terminal result fields are not available until worker_run_wait or worker_run_status returns a terminal status.',
      }
    : { state: options.metadata.confidence, terminal: true, scaffold: false };
  return {
    schema: 'narada.worker.run.v1',
    status: options.status,
    run_id: options.runRecord.runId,
    run_dir: options.runRecord.runDir,
    runtime: options.runtime,
    worker_session_id: options.workerSessionId,
    idempotency_key: options.executorRequest.idempotency_key ?? null,
    request_fingerprint: options.executorRequest.request_fingerprint ?? null,
    idempotency_replayed: false,
    resolved_worker_config: options.resolvedWorkerConfig,
    executor_request: options.executorRequest,
    requested_mode: options.metadata.requested_mode,
    edits_performed: options.metadata.edits_performed,
    target_state_changed: options.metadata.target_state_changed,
    confidence: options.metadata.confidence,
    completion_state: options.status === 'running' ? 'pending' : options.metadata.confidence,
    result_state: resultState,
    blocked_paths: options.metadata.blocked_paths,
    verification: options.metadata.verification,
    requested_mcp_tools: options.executorRequest.requested_mcp_tools ?? [],
    mcp_tool_verification: options.executorRequest.mcp_tool_verification ?? mcpToolVerification([]),
    output_contract: options.executorRequest.output_contract ?? outputContractForMode(options.metadata.requested_mode),
    runtime_warnings: options.runtimeWarnings ?? [],
    warning_count: options.runtimeWarnings?.length ?? 0,
    preflight: options.metadata.preflight,
    final_checklist: options.metadata.final_checklist,
    summary: options.output ? options.output.summary : null,
    deliverables: options.output ? options.output.deliverables : null,
    open_questions: options.output ? options.output.open_questions : null,
    next_actions: options.output ? options.output.next_actions : null,
    changes: options.output ? options.output.changes : null,
    structured_outputs: options.output?.structured_outputs ?? {},
    verification_results: options.output ? options.output.verification : null,
    verification_budget_respected: options.output?.verification_budget_respected ?? null,
    broad_unrelated_failures: options.output?.broad_unrelated_failures ?? [],
    exit_interview: options.output?.exit_interview ?? null,
    review_verdict: options.output?.review_verdict ?? null,
    acceptance_verdict: options.output?.acceptance_verdict ?? null,
    verdict: options.output?.verdict ?? null,
    progress: readRunProgress(options.runRecord.eventsPath),
    session_event_evidence: extractSessionEventEvidence(options.runRecord.eventsPath),
    artifacts: runArtifacts(options.runRecord),
    timing: {
      started_at: options.startedAt.toISOString(),
      finished_at: options.finishedAt?.toISOString() ?? null,
      duration_ms: options.finishedAt ? options.finishedAt.getTime() - options.startedAt.getTime() : null,
    },
    error: options.error,
    error_classification: options.error ? classifyRuntimeError(options.error) : null,
    ...(options.runtimeDiagnostics ? { runtime_diagnostics: options.runtimeDiagnostics } : {}),
    ...(options.runtimeDiagnostics?.error_provenance ? { error_provenance: options.runtimeDiagnostics.error_provenance } : {}),
    ...(typeof options.runtimeDiagnostics?.diagnostic_tail === 'string' ? { diagnostic_tail: options.runtimeDiagnostics.diagnostic_tail } : {}),
    worker_output_state: options.status === 'running' ? 'pending' : options.output ? 'available' : options.workerOutputError?.state ?? 'absent',
    worker_authored_output_present: Boolean(options.output),
    ...(options.workerOutputError ? { worker_output_error: options.workerOutputError } : {}),
  };
}


function workerRunStatus(args: Record<string, unknown>, state: WorkerMcpState): Record<string, unknown> {
  const runId = requiredNonEmptyString(args.run_id, 'worker_run_id_required');
  const requestedSiteRoot = resolveRunInspectionSiteRoot(state, args.site_root);
  const run = readRunResult(state, runId, true, requestedSiteRoot ?? undefined);
  if (!run) throw diagnosticError('worker_run_not_found', 'worker_run_not_found', { run_id: runId });
  return withManagedProcessLiveness(run, state, runId);
}

function withManagedProcessLiveness(run: Record<string, unknown>, state: WorkerMcpState, runId: string): Record<string, unknown> {
  if (run.status !== 'running' || !state.activeRunControllers?.has(runId)) return run;
  return { ...run, status_liveness: { ...asRecord(run.status_liveness), process_liveness: 'managed_active', process_verification: 'abort_controller_registered' } };
}

async function workerRunReap(args: Record<string, unknown>, state: WorkerMcpState): Promise<Record<string, unknown>> {
  const runId = requiredNonEmptyString(args.run_id, 'worker_run_id_required');
  const reason = requiredNonEmptyString(args.reason, 'worker_run_reap_reason_required');
  const force = Boolean(args.force);
  const requestedSiteRoot = resolveRunInspectionSiteRoot(state, args.site_root);
  const located = locateRunResult(state, runId, requestedSiteRoot ?? undefined);
  if (!located) throw diagnosticError('worker_run_not_found', 'worker_run_not_found', { run_id: runId, searched_run_roots: candidateRunRoots(state, requestedSiteRoot ?? undefined) });
  const current = readRunResult(state, runId, true, requestedSiteRoot ?? undefined);
  if (!current) throw diagnosticError('worker_run_not_found', 'worker_run_not_found', { run_id: runId, searched_run_roots: candidateRunRoots(state, requestedSiteRoot ?? undefined) });
  if (isTerminalRunStatus(String(current.status ?? ''))) {
    return { schema: 'narada.worker.run_reap.v1', status: 'already_terminal', run_id: runId, reaped: false, evidence: reapEvidence(current, reason, force), run: current };
  }
  const liveness = asRecord(current.status_liveness);
  if (current.status !== 'running') throw diagnosticError('worker_run_reap_not_running', 'worker_run_reap_not_running', { run_id: runId, status: current.status });
  if (liveness.state !== 'stale' && !force) {
    throw diagnosticError('worker_run_reap_refused_active_run', 'worker_run_reap_refused_active_run', { run_id: runId, status_liveness: liveness, remediation: 'wait_or_pass_force_true_with_operator_reason' });
  }
  const controller = state.activeRunControllers?.get(runId);
  const completion = state.activeRunCompletions?.get(runId);
  if (controller) {
    state.activeRunCancellationRequests ??= new Set();
    state.activeRunCancellationRequests.add(runId);
    controller.abort();
  }
  if (completion) await waitForActiveCompletion(completion, 5000);
  const settled = readRunResult(state, runId, true, requestedSiteRoot ?? undefined);
  if (!settled) throw diagnosticError('worker_run_not_found', 'worker_run_not_found', { run_id: runId });
  if (isTerminalRunStatus(String(settled.status ?? ''))) {
    const evidence = { ...reapEvidence(settled, reason, force), ...(controller ? { process_liveness: 'managed_active', process_verification: 'abort_controller_signalled' } : {}), cancellation_requested: Boolean(controller), cancellation_propagated: Boolean(controller) };
    return { schema: 'narada.worker.run_reap.v1', status: 'reaped', run_id: runId, reaped: true, evidence, run: settled };
  }
  const timing = asRecord(current.timing);
  const startedAtMs = Date.parse(String(timing.started_at ?? ''));
  const finishedAt = new Date().toISOString();
  const finishedAtMs = Date.parse(finishedAt);
  const warning = controller ? 'worker_run_reaped_stale_orphan: active run was cancelled through its abort controller before reaping' : 'worker_run_reaped_stale_orphan: run record was still running but caller explicitly reaped it as stale/orphaned';
  const runtimeWarnings = uniqueStrings([...(Array.isArray(current.runtime_warnings) ? current.runtime_warnings.map(String) : []), warning]);
  const evidence = { ...reapEvidence(current, reason, force), ...(controller ? { process_liveness: 'managed_active', process_verification: 'abort_controller_signalled' } : {}), cancellation_requested: Boolean(controller), cancellation_propagated: Boolean(controller) };
  const reapedRun = {
    ...current,
    status: 'cancelled',
    confidence: 'partial',
    completion_state: 'partial',
    runtime_warnings: runtimeWarnings,
    warning_count: runtimeWarnings.length,
    timing: {
      ...timing,
      finished_at: finishedAt,
      duration_ms: Number.isFinite(startedAtMs) ? Math.max(0, finishedAtMs - startedAtMs) : timing.duration_ms ?? null,
    },
    error: warning,
    error_classification: 'worker_run_reaped_stale_orphan',
    reaped: evidence,
  };
  writeJson(located.resultPath, reapedRun);
  if (!completion) state.activeRunCancellationRequests?.delete(runId);
  audit(state.policy, { tool: 'worker_run_reap', run_id: runId, reason, force, evidence, result_path: located.resultPath, at: finishedAt });
  return { schema: 'narada.worker.run_reap.v1', status: 'reaped', run_id: runId, reaped: true, evidence, run: readRunResult(state, runId, true, requestedSiteRoot ?? undefined) };
}

async function waitForActiveCompletion(completion: Promise<Record<string, unknown>>, timeoutMs: number): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  await Promise.race([
    completion.then(() => undefined, () => undefined),
    new Promise<void>((resolvePromise) => { timer = setTimeout(resolvePromise, timeoutMs); }),
  ]);
  if (timer) clearTimeout(timer);
}

function workerRunsList(args: Record<string, unknown>, state: WorkerMcpState): Record<string, unknown> {
  const limit = boundedInteger(args.limit, 20, 1, 200, 'worker_runs_list_limit_invalid');
  const includeCompleted = args.include_completed === undefined ? true : Boolean(args.include_completed);
  const includeRunning = args.include_running === undefined ? true : Boolean(args.include_running);
  const verbose = Boolean(args.verbose);
  const includeSummary = Boolean(args.include_summary) || verbose;
  const requestedSiteRoot = resolveRunInspectionSiteRoot(state, args.site_root);

  const candidates = includeCompleted
    ? listRunCandidates(state, requestedSiteRoot ?? undefined)
    : listActiveRunIds(state, requestedSiteRoot ?? undefined).map((runId) => ({ runId, modifiedAtMs: 0 }));
  const orderedCandidates = candidates.sort((a, b) => b.modifiedAtMs - a.modifiedAtMs || b.runId.localeCompare(a.runId));
  const scanLimit = Math.min(orderedCandidates.length, Math.max(limit, 200));
  const indexedRuns: Array<{ runId: string; record: Record<string, unknown> }> = [];
  const runs: Record<string, unknown>[] = [];
  let scanned = 0;
  for (const candidate of orderedCandidates.slice(0, scanLimit)) {
    scanned += 1;
    const indexed = readRunIndexRecord(state, candidate.runId, requestedSiteRoot ?? undefined);
    if (!indexed || (includeCompleted && !includeRunning && !includeRunByStatus(String(indexed.status ?? ''), { includeCompleted, includeRunning }))) continue;
    indexedRuns.push({ runId: candidate.runId, record: indexed });
  }
  indexedRuns.sort((a, b) => runSortKey(b.record).localeCompare(runSortKey(a.record)));
  for (const candidate of indexedRuns) {
    if (runs.length >= limit) break;
    const run = readRunResult(state, candidate.runId, false, requestedSiteRoot ?? undefined);
    if (!run || !includeRunByStatus(String(run.status ?? ''), { includeCompleted, includeRunning })) continue;
    runs.push(run);
  }
  runs.sort((a, b) => runSortKey(b).localeCompare(runSortKey(a)));
  return {
    schema: 'narada.worker.runs_list.v1',
    status: 'ok',
    count: runs.length,
    limit,
    scanned,
    scan_limit: scanLimit,
    scan_truncated: scanLimit < orderedCandidates.length,
    verbose,
    include_summary: includeSummary,
    runs: runs.map((run) => runListItem(run, { verbose, includeSummary })),
  };
}

async function workerRunWait(args: Record<string, unknown>, state: WorkerMcpState): Promise<Record<string, unknown>> {
  const runId = requiredNonEmptyString(args.run_id, 'worker_run_id_required');
  const timeoutMs = boundedInteger(args.timeout_ms, 10_000, 0, 300_000, 'worker_run_wait_timeout_invalid');
  const pollMs = boundedInteger(args.poll_ms, 250, 25, 10_000, 'worker_run_wait_poll_invalid');
  const verbose = Boolean(args.verbose);
  const summaryOnly = Boolean(args.summary_only);
  const requestedSiteRoot = resolveRunInspectionSiteRoot(state, args.site_root);
  const started = Date.now();
  while (true) {
    const loaded = readRunResult(state, runId, true, requestedSiteRoot ?? undefined);
    if (!loaded) throw diagnosticError('worker_run_not_found', 'worker_run_not_found', { run_id: runId });
    const result = withManagedProcessLiveness(loaded, state, runId);
    if (String(result.status ?? '') !== 'running') return runWaitPayload(result, { status: 'finished', timeoutMs, elapsedMs: Date.now() - started, verbose, summaryOnly });
    if (Date.now() - started >= timeoutMs) return runWaitPayload(result, { status: 'timed_out', timeoutMs, elapsedMs: Date.now() - started, verbose, summaryOnly });
    await new Promise((resolvePromise) => setTimeout(resolvePromise, Math.min(pollMs, Math.max(1, timeoutMs - (Date.now() - started)))));
  }
}

async function workerRunBatch(args: Record<string, unknown>, state: WorkerMcpState, context: WorkerRequestContext): Promise<Record<string, unknown>> {
  const requests = normalizeBatchRequests(args.requests);
  const maxParallelRuns = boundedInteger(args.max_parallel_runs, Math.min(state.policy.maxParallelRuns, requests.length), 1, state.policy.maxParallelRuns, 'worker_run_batch_parallel_invalid');
  const startedAt = new Date();
  const runs: Record<string, unknown>[] = [];
  const failures: Record<string, unknown>[] = [];
  const activeBatchRuns = new Set<string>();
  const waitForBatchCapacity = async () => {
    while (activeBatchRuns.size >= maxParallelRuns) {
      const completions = [...activeBatchRuns].map((runId) => {
        const completion = state.activeRunCompletions?.get(runId);
        if (!completion) {
          activeBatchRuns.delete(runId);
          return Promise.resolve();
        }
        return completion.then(() => undefined, () => undefined).finally(() => activeBatchRuns.delete(runId));
      });
      if (completions.length === 0) break;
      await Promise.race(completions);
    }
  };
  for (let index = 0; index < requests.length; index += 1) {
    try {
      await waitForBatchCapacity();
      const run = await workerRun(requireBatchRequestRecord(requests[index]), state, null, context, 'worker_run_batch');
      runs.push({ index, ...runListItem(run, { verbose: true, includeSummary: true }) });
      const runId = String(run.run_id ?? '');
      if (runId && String(run.status ?? '') === 'running' && state.activeRunCompletions?.has(runId)) activeBatchRuns.add(runId);
    } catch (error) {
      const diagnostic = error instanceof Error ? error.message : String(error);
      failures.push({ index, error: diagnostic, code: (error as { codeName?: unknown })?.codeName ?? 'worker_run_batch_item_failed' });
    }
  }
  return {
    schema: 'narada.worker.run_batch.v1',
    status: failures.length === 0 ? 'ok' : 'completed_with_errors',
    max_parallel_runs: maxParallelRuns,
    requested_count: requests.length,
    started_count: runs.length,
    failed_count: failures.length,
    run_ids: runs.map((run) => String(run.run_id ?? '')).filter(Boolean),
    runs: runs.map((run) => {
      const runId = String(run.run_id ?? '');
      if (!runId) return run;
      try {
        const latest = readRunResult(state, runId);
        if (!latest) return run;
        return { index: run.index, ...runListItem(withManagedProcessLiveness(latest, state, runId), { verbose: true, includeSummary: true }) };
      } catch {
        return run;
      }
    }),
    failures,
    timing: { started_at: startedAt.toISOString(), finished_at: new Date().toISOString() },
  };
}

async function workerRunWaitBatch(args: Record<string, unknown>, state: WorkerMcpState): Promise<Record<string, unknown>> {
  const runIds = normalizeRunIds(args.run_ids);
  const timeoutMs = boundedInteger(args.timeout_ms, 10_000, 0, 300_000, 'worker_run_wait_timeout_invalid');
  const pollMs = boundedInteger(args.poll_ms, 250, 25, 10_000, 'worker_run_wait_poll_invalid');
  const verbose = Boolean(args.verbose);
  const summaryOnly = Boolean(args.summary_only);
  const started = Date.now();
  const results: Record<string, unknown>[] = [];
  for (const runId of runIds) {
    const elapsed = Date.now() - started;
    try {
      const wait = await workerRunWait({ run_id: runId, timeout_ms: Math.max(0, timeoutMs - elapsed), poll_ms: pollMs, verbose, summary_only: summaryOnly }, state);
      const waitRecord = asRecord(wait);
      results.push(waitRecord.run ? { ...asRecord(waitRecord.run), wait: asRecord(waitRecord.wait) } : waitRecord);
    } catch (error) {
      results.push({
        run_id: runId,
        status: 'error',
        wait: {
          status: 'error',
          error: error instanceof Error ? error.message : String(error),
          code: (error as { codeName?: unknown })?.codeName ?? 'worker_run_wait_batch_item_failed',
        },
      });
    }
  }
  const errored = results.filter((run) => asRecord(run.wait).status === 'error');
  return {
    schema: 'narada.worker.run_wait_batch.v1',
    status: errored.length === 0 ? 'ok' : 'partial',
    requested_count: runIds.length,
    finished_count: results.filter((run) => asRecord(run.wait).status === 'finished').length,
    timed_out_count: results.filter((run) => asRecord(run.wait).status === 'timed_out').length,
    errored_count: errored.length,
    timeout_ms: timeoutMs,
    elapsed_ms: Date.now() - started,
    runs: results,
    synthesis: synthesizeRuns(runIds.map((runId) => readRunResult(state, runId, false)).filter((run): run is Record<string, unknown> => Boolean(run))),
  };
}

function workerRunsSynthesize(args: Record<string, unknown>, state: WorkerMcpState): Record<string, unknown> {
  const runIds = normalizeRunIds(args.run_ids);
  const runs = runIds.map((runId) => readRunResult(state, runId)).filter((run): run is Record<string, unknown> => Boolean(run));
  return {
    schema: 'narada.worker.runs_synthesis.v1',
    status: 'ok',
    requested_count: runIds.length,
    run_ids: runIds,
    synthesis: synthesizeRuns(runs),
  };
}

function workerDashboardDescribe(args: Record<string, unknown>, state: WorkerMcpState): Record<string, unknown> {
  const mode = dashboardMode(args.mode, args.run_id);
  const includeTerminal = args.include_terminal === undefined ? mode === 'single_run' : Boolean(args.include_terminal);
  const limit = boundedInteger(args.limit, 25, 1, 200, 'worker_runs_list_limit_invalid');
  const explicitRunIds = args.run_ids === undefined || args.run_ids === null ? null : normalizeOptionalRunIds(args.run_ids);
  const selectedRunIds = typeof args.run_id === 'string' && args.run_id.trim()
    ? [requiredNonEmptyString(args.run_id, 'worker_run_id_required')]
    : explicitRunIds
      ? explicitRunIds
      : mode === 'all_active'
        ? listRunIds(state)
        : [];
  const runs = selectedRunIds
    .map((runId) => readRunResult(state, runId, mode === 'single_run'))
    .filter((run): run is Record<string, unknown> => Boolean(run))
    .filter((run) => includeTerminal || !isTerminalRunStatus(String(run.status ?? '')))
    .sort((a, b) => runSortKey(b).localeCompare(runSortKey(a)))
    .slice(0, limit);
  const compactRuns = runs.map((run) => dashboardRun(run));
  const pendingJoinGates = dashboardPendingJoinGates(compactRuns);
  return {
    schema: 'narada.worker.dashboard.v1',
    status: 'ok',
    mode,
    include_terminal: includeTerminal,
    dashboard: {
      kind: 'read_only_dashboard_descriptor',
      server: { started: false, reason: 'mcp_tool_is_request_response; use the listed JSON API tool calls or wrap them in a local HTTP process if a long-lived dashboard is required' },
      suggested_local_command: null,
      api_endpoints: dashboardApiEndpoints(),
      refresh: { recommended_poll_ms: 1000, cacheable: false },
    },
    counts: {
      total: compactRuns.length,
      active: compactRuns.filter((run) => !isTerminalRunStatus(String(run.status ?? ''))).length,
      terminal: compactRuns.filter((run) => isTerminalRunStatus(String(run.status ?? ''))).length,
      failed: compactRuns.filter((run) => run.status === 'failed' || run.status === 'completed_with_errors').length,
    },
    runs: compactRuns,
    topology: {
      graph_kind: 'run_dag',
      dependency_source: 'worker-delegation run records; explicit inter-run dependencies are not currently recorded',
      nodes: compactRuns.map((run) => ({ id: run.run_id, label: run.run_id, status: run.status, worker_session_id: run.worker_session_id ?? null })),
      edges: [],
    },
    steps: compactRuns.map((run) => ({
      step_id: `run:${run.run_id}`,
      run_id: run.run_id,
      state: isTerminalRunStatus(String(run.status ?? '')) ? 'completed' : 'running',
      status: run.status,
      started_at: run.started_at,
      finished_at: run.finished_at,
      duration_ms: run.duration_ms,
    })),
    pending_join_gates: pendingJoinGates,
    event_stream: compactRuns.flatMap((run) => Array.isArray(run.events) ? run.events.map((event) => ({ run_id: run.run_id, ...asRecord(event) })) : []),
  };
}

function synthesizeRuns(runs: Record<string, unknown>[]): Record<string, unknown> {
  return {
    count: runs.length,
    rows: runs.map((run) => {
      const exitInterview = asRecord(run.exit_interview);
      return {
        run_id: run.run_id,
        status: run.status,
        requested_mode: modeWithInference(run).requestedMode,
        summary: String(run.summary ?? ''),
        deliverables: Array.isArray(run.deliverables) ? run.deliverables : [],
        risks: [...stringArrayFromUnknown(run.open_questions), ...stringArrayFromUnknown(run.runtime_warnings)],
        verification: Array.isArray(run.verification_results) ? run.verification_results : [],
        changed_files: Array.isArray(run.changes) ? run.changes.map((change) => asRecord(change).path).filter(Boolean) : [],
        partial_failure: partialFailurePosture(run),
        progress: run.progress ?? null,
        budget_status: run.budget_status ?? workerBudgetStatus(run),
        session_event_evidence: asRecord(run.runtime_diagnostics).session_event_evidence ?? null,
        warnings: stringArrayFromUnknown(run.runtime_warnings),
        ergonomics_feedback: typeof exitInterview.ergonomics_feedback === 'string' ? exitInterview.ergonomics_feedback : null,
        error_preview: compactRunError(run),
      };
    }),
  };
}

function uniqueStrings(values: string[]): string[] { return [...new Set(values.filter(Boolean))]; }

function stringArrayFromUnknown(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : [];
}

function boundedInteger(value: unknown, defaultValue: number, min: number, max: number, code: string): number {
  if (value === undefined || value === null || value === '') return defaultValue;
  const parsed = typeof value === 'number' ? value : Number.parseInt(String(value), 10);
  if (!Number.isInteger(parsed) || parsed < min || parsed > max) throw diagnosticError(code, code, { value, min, max });
  return parsed;
}

function acquireWorkerRunSlot(state: WorkerMcpState): void {
  if (state.activeRunCount >= state.policy.maxParallelRuns) {
    throw diagnosticError('worker_parallel_limit_exceeded', 'worker_parallel_limit_exceeded', { active_run_count: state.activeRunCount, max_parallel_runs: state.policy.maxParallelRuns });
  }
  state.activeRunCount += 1;
}

function releaseWorkerRunSlot(state: WorkerMcpState): void {
  state.activeRunCount = Math.max(0, state.activeRunCount - 1);
}

function cognitionDefaultsForLaunch(cognition: ResolvedWorkerConfig['cognition'], policy: WorkerPolicy, runtime: WorkerRuntimeId, provider: string | null): WorkerCognitionDefaults {
  if (cognition === null) return { model: null, reasoningEffort: null };
  if (runtime === 'codex') {
    return provider
      ? policy.providerCognitionDefaults[provider]?.[cognition] ?? { model: null, reasoningEffort: null }
      : { model: null, reasoningEffort: null };
  }
  return defaultConfigForCognition(cognition, policy, provider);
}

function assertExplicitCognitionTuple(config: ResolvedWorkerConfig, state?: WorkerMcpState): void {
  if (config.runtime === 'narada-agent-runtime-server') {
    const planRef = asRecord(config.intelligence_context).invocation_plan_ref;
    const runtimeBinding = asRecord(config.provider_runtime_binding);
    const runtimeOwnsPreflight = runtimeBinding.source === 'agent_runtime_canonical_preflight';
    if (!runtimeOwnsPreflight && (typeof planRef !== 'string' || !planRef.startsWith('plan:'))) {
      throw diagnosticError('worker_canonical_invocation_plan_required', 'No canonical invocation-plan authority is available.');
    }
    return;
  }
  const missingFields = [
    config.provider ? null : 'provider',
    config.model ? null : 'model',
    config.reasoning_effort ? null : 'reasoning_effort',
  ].filter((field): field is string => field !== null);
  if (missingFields.length === 0) return;
  throw diagnosticError('worker_cognition_defaults_unresolved', 'worker_cognition_defaults_unresolved', {
    runtime: config.runtime,
    cognition: config.cognition,
    provider: config.provider ?? null,
    field: missingFields[0],
    missing_fields: missingFields,
    resolved: {
      provider: config.provider ?? null,
      model: config.model,
      reasoning_effort: config.reasoning_effort,
    },
    provider_registry: publicProviderRegistryDiagnostics(state?.providerRegistryDiagnostics),
  });
}

function applyCognitionDefaults(request: WorkerRunToolInput, cognition: ResolvedWorkerConfig['cognition'], state: WorkerMcpState, runtime: WorkerRuntimeId = 'codex', provider: string | null = null): void {
  const defaults = cognitionDefaultsForLaunch(cognition, state.policy, runtime, provider);
  if (!defaults.model && !defaults.reasoningEffort) return;
  const overrides = { ...(request.constraints.overrides ?? {}) };
  const config = { ...(overrides.config ?? {}) };
  if (defaults.model && overrides.model === undefined && config.model === undefined) overrides.model = defaults.model;
  if (defaults.reasoningEffort && overrides.reasoning_effort === undefined && config.model_reasoning_effort === undefined) {
    overrides.reasoning_effort = defaults.reasoningEffort;
  }
  if (Object.keys(config).length > 0) overrides.config = config;
  if (Object.keys(overrides).length > 0) request.constraints.overrides = overrides;
}

function applyEffectiveCognitionTuple(request: WorkerRunToolInput, effectiveDefault: { model: string | null; reasoningEffort: string | null } | null): void {
  if (!effectiveDefault) return;
  const overrides = { ...(request.constraints.overrides ?? {}) };
  const config = { ...(overrides.config ?? {}) };
  if (effectiveDefault.model && overrides.model === undefined && config.model === undefined) overrides.model = effectiveDefault.model;
  if (effectiveDefault.reasoningEffort && overrides.reasoning_effort === undefined && config.model_reasoning_effort === undefined) overrides.reasoning_effort = effectiveDefault.reasoningEffort;
  if (Object.keys(config).length > 0) overrides.config = config;
  request.constraints.overrides = overrides;
}

function configResolutionMetadata(options: {
  requestedOverrides: WorkerConstraintOverrides;
  resolvedConfigInput: { config: Record<string, PrimitiveConfigValue>; model: string | null; reasoning_effort: string | null };
  runtime: WorkerRuntimeId;
  cognition: ResolvedWorkerConfig['cognition'];
  provider: string | null;
  policy: WorkerPolicy;
  cognitionDefaultSource: string;
}): Record<string, unknown> {
  if (options.runtime === 'narada-agent-runtime-server') {
    return {
      model_source: 'canonical_invocation_plan',
      model_resolution: 'narada_runtime_admitted_plan',
      reasoning_effort_source: 'canonical_invocation_plan',
      cognition_default_source: 'canonical_invocation_plan',
      precedence: 'canonical_invocation_plan_only',
      allowed_config_keys: options.policy.allowedConfigKeys,
      explicit_config_keys: Object.keys(options.resolvedConfigInput.config).sort(),
    };
  }
  const config = options.requestedOverrides.config ?? {};
  const cognitionDefaults = cognitionDefaultsForLaunch(options.cognition, options.policy, options.runtime, options.provider);
  return {
    model_source: configValueSource({
      explicit: options.requestedOverrides.model !== undefined || config.model !== undefined,
      hasResolvedValue: options.resolvedConfigInput.model !== null,
      cognitionDefault: cognitionDefaults.model,
      runtime: options.runtime,
    }),
    model_resolution: 'resolved_before_runtime',
    reasoning_effort_source: configValueSource({
      explicit: options.requestedOverrides.reasoning_effort !== undefined || config.model_reasoning_effort !== undefined,
      hasResolvedValue: options.resolvedConfigInput.reasoning_effort !== null,
      cognitionDefault: cognitionDefaults.reasoningEffort,
      runtime: options.runtime,
    }),
    cognition_default_source: options.cognitionDefaultSource,
    precedence: 'request_override > site_runtime_override > provider_registry > generic_cognition_default > runtime_default',
    allowed_config_keys: options.policy.allowedConfigKeys,
    explicit_config_keys: Object.keys(options.resolvedConfigInput.config).sort(),
  };
}

function configValueSource(options: { explicit: boolean; hasResolvedValue: boolean; cognitionDefault: string | null; runtime: WorkerRuntimeId }): string {
  if (options.explicit) return 'request_override';
  if (options.hasResolvedValue && options.cognitionDefault) return 'cognition_default';
  if (options.hasResolvedValue) return 'resolved_config';
  return 'runtime_default_opaque';
}

function inheritSessionConstraints(request: WorkerRunToolInput, inherited: ResolvedWorkerConfig): void {
  if (request.constraints.authority === undefined) request.constraints.authority = inherited.authority;
  if (request.constraints.cognition === undefined && inherited.runtime !== 'narada-agent-runtime-server' && inherited.cognition !== null) request.constraints.cognition = inherited.cognition;
  if (request.constraints.provider === undefined && inherited.provider && inherited.runtime !== 'narada-agent-runtime-server' && inherited.runtime !== 'codex') request.constraints.provider = inherited.provider;
  if (request.constraints.invocation_plan_ref === undefined && inherited.runtime === 'narada-agent-runtime-server') {
    const inheritedPlanRef = asRecord(inherited.intelligence_context).invocation_plan_ref;
    if (typeof inheritedPlanRef === 'string') request.constraints.invocation_plan_ref = inheritedPlanRef;
  }
  if (request.constraints.required_mcp_tools === undefined) {
    const projection = asRecord(inherited.worker_mcp_projection);
    const inheritedTools = inherited.required_mcp_tools ?? projection.mcp_tool_allowlist;
    if (Array.isArray(inheritedTools)) {
      request.constraints.required_mcp_tools = inheritedTools.filter((tool): tool is string => typeof tool === 'string' && Boolean(tool.trim()));
    }
  }
  const overrides = { ...(request.constraints.overrides ?? {}) };
  const config = { ...(overrides.config ?? {}) };
  if (overrides.runtime === undefined) overrides.runtime = inherited.runtime;
  if (overrides.sandbox === undefined) overrides.sandbox = inherited.sandbox;
  if (inherited.runtime !== 'narada-agent-runtime-server' && overrides.model === undefined && config.model === undefined && inherited.model) overrides.model = inherited.model;
  if (inherited.runtime !== 'narada-agent-runtime-server' && overrides.reasoning_effort === undefined && config.model_reasoning_effort === undefined && inherited.reasoning_effort) {
    overrides.reasoning_effort = inherited.reasoning_effort;
  }
  for (const [key, value] of Object.entries(inherited.config)) {
    if (config[key] === undefined && key !== 'model' && key !== 'model_reasoning_effort') config[key] = value;
  }
  if (Object.keys(config).length > 0) overrides.config = config;
  if (Object.keys(overrides).length > 0) request.constraints.overrides = overrides;
}

function defaultModeForAuthority(authority: ResolvedWorkerConfig['authority']): WorkerDelegationMode {
  return authority === 'write' || authority === 'command' ? 'implement' : 'audit_only';
}

function parseDelegationMode(value: unknown, authority: string | undefined): WorkerDelegationMode {
  if (value === undefined || value === null || value === '') return authority === 'write' || authority === 'command' ? 'implement' : 'audit_only';
  const mode = String(value).trim();
  if (mode === 'audit_only' || mode === 'plan_only' || mode === 'implement' || mode === 'implement_and_verify') return mode;
  throw diagnosticError('worker_invalid_mode', 'worker_invalid_mode', { mode });
}

function buildPreflight(options: { cwd: string; authority: string; mode: WorkerDelegationMode; waitForCompletion: boolean; isResume: boolean; preflightPaths: WorkerPreflightPath[]; requiredMcpTools: string[]; allowedRoots: string[] }): WorkerPreflightCheck[] {
  const checks: WorkerPreflightCheck[] = [
    { name: 'cwd_readable', status: existsSync(options.cwd) ? 'ok' : 'blocked', message: existsSync(options.cwd) ? `cwd exists: ${options.cwd}` : `cwd is missing: ${options.cwd}` },
    { name: 'requested_mode', status: 'ok', message: `requested_mode=${options.mode}` },
    { name: 'authority', status: 'ok', message: `authority=${options.authority}` },
  ];
  if ((options.mode === 'implement' || options.mode === 'implement_and_verify') && options.authority === 'read') {
    checks.push({ name: 'mode_authority_alignment', status: 'blocked', message: `${options.mode} requires write or command authority; read authority can only audit or plan` });
  } else {
    checks.push({ name: 'mode_authority_alignment', status: 'ok', message: `${options.mode} is allowed with ${options.authority} authority` });
  }
  if (options.authority === 'read') {
    checks.push({ name: 'effective_authority', status: 'warning', message: 'effective_authority=read; raw MCP surfaces may advertise mutation-capable tools, but this delegation permits inspection and reporting only' });
  }
  for (const item of options.preflightPaths.filter((path) => path.access === 'write' || path.access === 'create')) {
    if (options.authority === 'read') checks.push({ name: 'read_authority_mutation_boundary', status: 'blocked', message: `read authority cannot perform ${item.access} preflight for ${resolve(item.path)}` });
  }
  checks.push({ name: 'execution_style', status: options.waitForCompletion ? 'ok' : 'warning', message: options.waitForCompletion ? 'caller will wait for completion' : 'async run; caller must use worker_run_status, worker_runs_list, or worker_run_wait to rediscover result' });
  if (options.isResume) checks.push({ name: 'resume', status: 'ok', message: 'continuing an existing worker session' });
  for (const item of options.preflightPaths) checks.push(preflightPathCheck(item, options.allowedRoots));
  if (options.requiredMcpTools.length === 0) {
    checks.push({ name: 'mcp_tool_projection', status: 'warning', message: 'no_mcp_tools_projected; worker must not call MCP tools; add constraints.required_mcp_tools with exact names for MCP-dependent work' });
  } else {
    checks.push({ name: 'required_mcp_tools', status: 'warning', message: `runtime_inventory_not_preflighted; narada-agent-runtime-server projects requested tools through NARADA_WORKER_MCP_CONFIG, other runtimes must verify before work: ${options.requiredMcpTools.join(', ')}` });
    const recursiveTools = options.requiredMcpTools.filter(isWorkerDelegationToolName);
    if (recursiveTools.length > 0) {
      checks.push({
        name: 'required_mcp_tools_self_deadlock',
        status: 'blocked',
        message: `worker delegation cannot launch a worker whose required MCP tools recurse into worker-delegation: ${recursiveTools.join(', ')}; reroute through the delegating agent or a non-worker repair surface`,
      });
    }
  }
  return checks;
}

function isWorkerDelegationToolName(toolName: string): boolean {
  const normalized = toolName.toLowerCase();
  return normalized.includes('worker-delegation') || normalized.includes('worker_delegation') || /(^|[.\/:-])worker_/.test(normalized);
}

function preflightPathCheck(item: WorkerPreflightPath, allowedRoots: string[]): WorkerPreflightCheck {
  const path = resolve(item.path);
  const label = item.label ? `${item.label}: ` : '';
  if (!allowedRoots.some((root) => areSamePath(path, root) || isPathInside(path, root))) {
    return { name: `path_${item.access}`, status: 'blocked', message: `${label}${path} is outside allowed roots` };
  }
  try {
    if (item.access === 'read') {
      accessSync(path, constants.R_OK);
      return { name: 'path_read', status: 'ok', message: `${label}${path} is readable` };
    }
    if (item.access === 'write') {
      accessSync(path, constants.W_OK);
      return { name: 'path_write', status: 'ok', message: `${label}${path} is writable` };
    }
    const parent = dirname(path);
    accessSync(parent, constants.W_OK);
    return { name: 'path_create', status: existsSync(path) ? 'warning' : 'ok', message: existsSync(path) ? `${label}${path} already exists; create target is not empty by default` : `${label}${path} can be created; parent is writable` };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { name: `path_${item.access}`, status: 'blocked', message: `${label}${path} failed ${item.access} preflight: ${message}` };
  }
}

function isPathInside(path: string, root: string): boolean {
  const rel = relative(normalizePathComparisonKey(root), normalizePathComparisonKey(path));
  return rel !== '' && !rel.startsWith('..') && !isAbsolute(rel);
}

function areSamePath(left: string, right: string): boolean {
  return normalizePathComparisonKey(left) === normalizePathComparisonKey(right);
}

function checkRuntimeAvailability(runtime: WorkerRuntimeId, policy: WorkerPolicy, env: Record<string, string>): { available: boolean; reason?: string; remediation?: string; command?: string } {
  if (runtime === 'narada-agent-runtime-server') return commandAvailable(policy.runtimes.naradaAgentRuntimeServer.command, env);
  return commandAvailable(policy.runtimes.codex.command, env);
}

function commandAvailable(command: string, env: Record<string, string>): { available: boolean; reason?: string; remediation?: string; command?: string } {
  if (isAbsolute(command)) {
    if (!existsSync(command)) {
      return { available: false, reason: `command not found: ${command}`, remediation: 'install the runtime or configure an explicit command path that exists' };
    }
    return { available: true, command };
  }
  const pathEnv = env.PATH || '';
  const extensions = process.platform === 'win32' ? [...new Set([...(env.PATHEXT || '.COM;.EXE;.BAT;.CMD').split(';'), '.PS1'])] : [''];
  for (const dir of pathEnv.split(process.platform === 'win32' ? ';' : ':')) {
    const candidateNames = process.platform === 'win32' && extname(command) ? [command] : extensions.map((ext) => command + ext);
    for (const name of candidateNames) {
      const candidate = resolve(dir, name);
      if (existsSync(candidate)) return { available: true, command: candidate };
    }
  }
  return { available: false, reason: `command not found on PATH: ${command}`, remediation: 'install the runtime, add it to PATH, or configure an absolute command path' };
}

function normalizePathComparisonKey(path: string): string {
  const normalized = resolve(path);
  return process.platform === 'win32' ? normalized.toLowerCase() : normalized;
}

function enforcePreflightForMode(mode: WorkerDelegationMode, preflight: WorkerPreflightCheck[]): void {
  const blocked = preflight.filter((check) => check.status === 'blocked');
  if (blocked.length === 0) return;
  throw diagnosticError('worker_preflight_blocked', 'worker_preflight_blocked', { requested_mode: mode, blocked_preflight: blocked });
}

function buildRunMetadata(options: {
  requestedMode: WorkerDelegationMode;
  preflight: WorkerPreflightCheck[];
  status: 'running' | 'completed' | 'completed_with_errors' | 'failed' | 'cancelled';
  output: WorkerOutput | null;
  error: string | null;
}): WorkerRunMetadata {
  const readOnlyMode = options.requestedMode === 'audit_only' || options.requestedMode === 'plan_only';
  const blockedPaths = options.preflight.filter((check) => check.status === 'blocked').map((check) => check.message);
  const verification = options.output?.verification.map((item) => [item.status, item.tool ?? item.command, item.summary].filter(Boolean).join(': ')) ?? [];
  return {
    requested_mode: options.requestedMode,
    edits_performed: options.status === 'running' ? null : options.output ? options.output.edits_performed : readOnlyMode ? false : null,
    target_state_changed: options.status === 'running' ? null : options.output ? options.output.target_state_changed : readOnlyMode ? false : null,
    confidence: options.status === 'running' ? 'pending' : options.error || blockedPaths.length > 0 || options.status === 'completed_with_errors' || options.status === 'failed' ? 'partial' : 'complete',
    blocked_paths: blockedPaths,
    verification,
    preflight: options.preflight,
    final_checklist: finalChecklist(options.requestedMode),
  };
}

function finalChecklist(mode: WorkerDelegationMode): string[] {
  if (mode === 'audit_only' || mode === 'plan_only') {
    return ['state whether files were edited', 'list evidence inspected', 'list blocked or unreadable paths', 'separate recommendations from completed work'];
  }
  return ['list files changed', 'list tests or checks run', 'include git/worktree status if available', 'list remaining blockers'];
}

function mcpToolVerification(requestedTools: string[], runtime: SupportedRuntime | null = null): Record<string, unknown> {
  const hasProjection = requestedTools.length > 0;
  const projectedByRuntime = hasProjection && runtime === 'narada-agent-runtime-server';
  return {
    requested_mcp_tools: requestedTools,
    runtime_can_project: projectedByRuntime,
    verification_state: hasProjection ? (projectedByRuntime ? 'projected_to_worker_runtime' : 'requires_projected_runtime') : 'no_tools_projected',
    enforced_by_delegation: projectedByRuntime,
    enforcement_surface: projectedByRuntime ? 'NARADA_WORKER_MCP_CONFIG' : null,
    evidence_field: 'verification',
    fallback_reason_required: hasProjection && !projectedByRuntime,
    no_tools_posture: !hasProjection,
    remediation: hasProjection ? null : 'Declare exact constraints.required_mcp_tools for MCP-dependent work; otherwise keep the worker MCP-free.',
  };
}

function mcpToolLaunchability(requestedTools: string[], runtime: SupportedRuntime | null): Record<string, unknown> & { launchable: boolean } {
  if (requestedTools.length === 0) {
    return {
      launchable: true,
      code: null,
      reason: null,
      requested_mcp_tools: [],
      supported_runtime: 'narada-agent-runtime-server',
    };
  }
  if (runtime === 'narada-agent-runtime-server') {
    return {
      launchable: true,
      code: null,
      reason: null,
      requested_mcp_tools: requestedTools,
      supported_runtime: 'narada-agent-runtime-server',
    };
  }
  return {
    launchable: false,
    code: 'worker_required_mcp_tools_unprojectable',
    reason: 'Codex runtime cannot project requested MCP tools',
    requested_mcp_tools: requestedTools,
    supported_runtime: 'narada-agent-runtime-server',
  };
}

function ensureRequiredMcpToolsProjectable(requestedTools: string[], runtime: SupportedRuntime | null): void {
  if (requestedTools.length === 0) return;
  if (runtime === 'narada-agent-runtime-server') return;
  throw diagnosticError('worker_required_mcp_tools_unprojectable', 'worker_required_mcp_tools_unprojectable', {
    runtime,
    requested_mcp_tools: requestedTools,
    supported_runtime: 'narada-agent-runtime-server',
  });
}

function buildWorkerMcpProjection(requiredMcpTools: string[]): Record<string, unknown> | null {
  if (requiredMcpTools.length === 0) return null;
  return {
    schema: 'narada.worker.mcp_projection.v1',
    native_mcp_mode: 'scoped',
    mcp_tool_allowlist: requiredMcpTools,
    include_startup_tools: true,
    include_output_readback_tools: false,
    full_site_mcp_requires_explicit_mode: true,
  };
}

function normalizeWorkerRunToolInput(args: Record<string, unknown>, isResume: boolean): WorkerRunToolInput {
  const intentInput = asRecord(args.intent);
  const constraintsInput = asRecord(args.constraints);
  const instructionValue = intentInput.instruction ?? (isResume ? 'Continue the previous worker session and return an updated structured result.' : undefined);
  const instruction = requiredNonEmptyString(instructionValue, 'worker_prompt_too_large');
  const authorityValue = String(asRecord(args.constraints).authority ?? args.authority ?? '');
  const outputContract = asRecord(intentInput.output_contract);
  const idempotencyKey = optionalIdempotencyKey(args.idempotency_key);
  return {
    ...(idempotencyKey ? { idempotency_key: idempotencyKey } : {}),
    intent: {
      instruction,
      mode: parseDelegationMode(intentInput.mode ?? args.mode, authorityValue),
      ...(Object.keys(outputContract).length > 0 ? { output_contract: outputContract } : {}),
    },
    constraints: normalizeWorkerConstraintRequest(args, constraintsInput),
  };
}

type WorkerRunIdempotency = {
  key: string;
  runId: string;
  requestFingerprint: string;
};

function workerRunIdempotency(key: string, request: WorkerRunToolInput, resumeSessionId: string | null): WorkerRunIdempotency {
  const requestFingerprint = sha256(canonicalJson({
    schema: 'narada.worker.run_request.v1',
    intent: request.intent,
    constraints: request.constraints,
    resume_worker_session_id: resumeSessionId,
  }));
  return {
    key,
    runId: `run-idem-${sha256(key).slice(0, 40)}`,
    requestFingerprint,
  };
}

function replayIdempotentWorkerRun(state: WorkerMcpState, expected: WorkerRunIdempotency): Record<string, unknown> {
  const runDir = resolve(state.policy.runRoot, expected.runId);
  const claimPath = join(runDir, 'idempotency.json');
  let claim: Record<string, unknown>;
  try {
    claim = asRecord(JSON.parse(readFileSync(claimPath, 'utf8')));
  } catch {
    throw diagnosticError('worker_run_idempotency_claim_incomplete', 'worker_run_idempotency_claim_incomplete', {
      run_id: expected.runId,
      remediation: 'Retry the same request; no second worker run was launched.',
    });
  }
  if (claim.schema !== 'narada.worker.run_idempotency.v1'
    || claim.idempotency_key !== expected.key
    || claim.request_fingerprint !== expected.requestFingerprint) {
    throw diagnosticError('worker_run_idempotency_conflict', 'worker_run_idempotency_conflict', {
      run_id: expected.runId,
      idempotency_key: expected.key,
      existing_request_fingerprint: claim.request_fingerprint ?? null,
      request_fingerprint: expected.requestFingerprint,
    });
  }
  const run = readRunResult(state, expected.runId, false);
  if (!run) {
    throw diagnosticError('worker_run_idempotency_claim_incomplete', 'worker_run_idempotency_claim_incomplete', {
      run_id: expected.runId,
      idempotency_key: expected.key,
      request_fingerprint: expected.requestFingerprint,
      remediation: 'Retry the same request; no second worker run was launched.',
    });
  }
  const recovered = parseLastMessage(join(runDir, 'last_message.json'));
  if (!recovered.ok || !recovered.data.structured_outputs || Object.keys(recovered.data.structured_outputs).length === 0) {
    return { ...run, idempotency_replayed: true };
  }
  const existingStructuredOutputs = run.structured_outputs && typeof run.structured_outputs === 'object' && !Array.isArray(run.structured_outputs)
    ? run.structured_outputs as Record<string, unknown>
    : {};
  if (canonicalJson(existingStructuredOutputs) === canonicalJson(recovered.data.structured_outputs)) {
    return { ...run, idempotency_replayed: true };
  }
  const repaired = {
    ...run,
    summary: recovered.data.summary,
    deliverables: recovered.data.deliverables,
    open_questions: recovered.data.open_questions,
    next_actions: recovered.data.next_actions,
    changes: recovered.data.changes,
    structured_outputs: recovered.data.structured_outputs,
    verification_results: recovered.data.verification,
    verification_budget_respected: recovered.data.verification_budget_respected,
    broad_unrelated_failures: recovered.data.broad_unrelated_failures,
    exit_interview: recovered.data.exit_interview,
    review_verdict: recovered.data.review_verdict,
    acceptance_verdict: recovered.data.acceptance_verdict,
    verdict: recovered.data.verdict,
    worker_output_state: 'available',
    worker_authored_output_present: true,
    worker_output_recovered_from_last_message: true,
  };
  writeJson(join(runDir, 'result.json'), repaired);
  return { ...repaired, idempotency_replayed: true };
}

function optionalIdempotencyKey(value: unknown): string | null {
  if (value === undefined || value === null || value === '') return null;
  const key = requiredNonEmptyString(value, 'worker_idempotency_key_required');
  if (key.length > 512) throw diagnosticError('worker_idempotency_key_too_long', 'worker_idempotency_key_too_long', { max_length: 512 });
  return key;
}

function sha256(value: string): string {
  return createHash('sha256').update(value).digest('hex');
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  const record = value as Record<string, unknown>;
  return `{${Object.keys(record).filter((key) => record[key] !== undefined).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`).join(',')}}`;
}

function isAlreadyExistsError(error: unknown): boolean {
  return Boolean(error && typeof error === 'object' && 'code' in error && (error as { code?: unknown }).code === 'EEXIST');
}

function normalizeWorkerConstraintRequest(args: Record<string, unknown>, constraintsInput: Record<string, unknown>): WorkerConstraintRequest {
  const overridesInput = asRecord(constraintsInput.overrides);
  const constraints: WorkerConstraintRequest = {
    cwd: requiredNonEmptyString(constraintsInput.cwd ?? args.cwd, 'worker_cwd_required'),
  };
  copyString(constraints, 'site_root', constraintsInput.site_root ?? args.site_root);
  copyString(constraints, 'invocation_plan_ref', constraintsInput.invocation_plan_ref ?? args.invocation_plan_ref);
  copyString(constraints, 'provider', constraintsInput.provider ?? args.provider);
  copyString(constraints, 'authority', constraintsInput.authority ?? args.authority);
  copyString(constraints, 'cognition', constraintsInput.cognition ?? args.cognition);
  if (constraintsInput.resumable !== undefined || args.resumable !== undefined) constraints.resumable = Boolean(constraintsInput.resumable ?? args.resumable);
  if (constraintsInput.wait_for_completion !== undefined || args.wait_for_completion !== undefined) constraints.wait_for_completion = Boolean(constraintsInput.wait_for_completion ?? args.wait_for_completion);
  if (constraintsInput.wait_timeout_ms !== undefined || args.wait_timeout_ms !== undefined) constraints.wait_timeout_ms = Number(constraintsInput.wait_timeout_ms ?? args.wait_timeout_ms);
  if (constraintsInput.exit_interview !== undefined || args.exit_interview !== undefined) constraints.exit_interview = Boolean(constraintsInput.exit_interview ?? args.exit_interview);
  const verificationBudget = normalizeBudget(constraintsInput.verification_budget ?? args.verification_budget);
  if (verificationBudget) constraints.verification_budget = verificationBudget;
  const testBudget = normalizeBudget(constraintsInput.test_budget ?? args.test_budget);
  if (testBudget) constraints.test_budget = testBudget;
  const preflightPaths = normalizePreflightPaths(constraintsInput.preflight_paths ?? args.preflight_paths);
  if (preflightPaths.length > 0) constraints.preflight_paths = preflightPaths;
  const requiredMcpToolsInput = constraintsInput.required_mcp_tools !== undefined ? constraintsInput.required_mcp_tools : args.required_mcp_tools;
  if (requiredMcpToolsInput !== undefined) constraints.required_mcp_tools = normalizeStringList(requiredMcpToolsInput, 'worker_invalid_required_mcp_tools');
  const overrides: NonNullable<WorkerConstraintRequest['overrides']> = {};
  copyString(overrides, 'runtime', overridesInput.runtime ?? constraintsInput.runtime ?? args.runtime);
  copyString(overrides, 'sandbox', overridesInput.sandbox ?? constraintsInput.sandbox ?? args.sandbox);
  copyString(overrides, 'model', overridesInput.model ?? constraintsInput.model ?? args.model);
  copyString(overrides, 'reasoning_effort', overridesInput.reasoning_effort ?? constraintsInput.reasoning_effort ?? args.reasoning_effort);
  const config = primitiveConfigRecord(overridesInput.config ?? constraintsInput.config ?? args.config);
  if (Object.keys(config).length > 0) overrides.config = config;
  if (overridesInput.skip_git_repo_check !== undefined || constraintsInput.skip_git_repo_check !== undefined || args.skip_git_repo_check !== undefined) {
    overrides.skip_git_repo_check = optionalBoolean(overridesInput.skip_git_repo_check ?? constraintsInput.skip_git_repo_check ?? args.skip_git_repo_check, 'skip_git_repo_check');
  }
  if (Object.keys(overrides).length > 0) constraints.overrides = overrides;
  return constraints;
}

function normalizeBudget(value: unknown): NonNullable<WorkerConstraintRequest['verification_budget']> | undefined {
  const input = asRecord(value);
  const result: NonNullable<WorkerConstraintRequest['verification_budget']> = {};
  if (input.focus === 'focused' || input.focus === 'broad') result.focus = input.focus;
  if (input.max_commands !== undefined) result.max_commands = boundedNumber(input.max_commands, 'worker_invalid_verification_budget', 0, 100);
  if (input.max_minutes !== undefined) result.max_minutes = boundedNumber(input.max_minutes, 'worker_invalid_verification_budget', 0, 24 * 60);
  if (input.stop_on_first_failure !== undefined) result.stop_on_first_failure = Boolean(input.stop_on_first_failure);
  if (input.broad_commands_allowed !== undefined) result.broad_commands_allowed = Boolean(input.broad_commands_allowed);
  copyString(result as Record<string, unknown>, 'notes', input.notes);
  return Object.keys(result).length > 0 ? result : undefined;
}

function boundedNumber(value: unknown, code: string, min: number, max: number): number {
  const number = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(number) || number < min || number > max) throw diagnosticError(code, code, { value, min, max });
  return number;
}

function copyString(target: Record<string, unknown>, key: string, value: unknown): void {
  if (value === undefined || value === null || value === '') return;
  target[key] = String(value).trim();
}

function primitiveConfigRecord(value: unknown): Record<string, PrimitiveConfigValue> {
  if (value === undefined || value === null) return {};
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw diagnosticError('worker_invalid_config_input', 'config must be an object', { value_type: Array.isArray(value) ? 'array' : typeof value });
  }
  const record = asRecord(value);
  const result: Record<string, PrimitiveConfigValue> = {};
  for (const [key, item] of Object.entries(record)) {
    if (typeof item === 'string' || typeof item === 'boolean' || typeof item === 'number' && Number.isFinite(item)) {
      result[key] = item;
      continue;
    }
    throw diagnosticError('worker_config_key_not_allowed', 'worker_config_value_must_be_primitive', { key });
  }
  return result;
}

function requiredNonEmptyString(value: unknown, code: string): string {
  const text = String(value ?? '').trim();
  if (!text) throw diagnosticError(code);
  return text;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}
