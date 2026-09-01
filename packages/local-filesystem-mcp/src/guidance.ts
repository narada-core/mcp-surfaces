import { compactGuidanceResult } from '@narada-core/mcp-fabric-contracts';

export type GuidanceRecord = Record<string, unknown>;
export type GuidanceToolDefinition = GuidanceRecord & { name: string; description: string; inputSchema: GuidanceRecord; annotations: GuidanceRecord; outputSchema: GuidanceRecord };

const SURFACE_ID = "local-filesystem";
const GUIDANCE_TOOL = "fs_guidance";
const PURPOSE = "Governed filesystem inspection and mutation under allowed roots.";

export function buildGuidanceResult(args: GuidanceRecord = {}): GuidanceRecord {
  const workflow = typeof args.workflow === 'string' && args.workflow.trim() ? args.workflow.trim() : null;
  const tool = typeof args.tool === 'string' && args.tool.trim() ? args.tool.trim() : null;
  const patchRecovery = workflow === 'bounded_reads_and_patch_recovery' || tool === 'fs_apply_patch' || tool === 'fs_patch_outcome_show' ? {
    patch_recovery: {
      sequence: [
        'Choose a stable operation_id before calling fs_apply_patch.',
        'Call fs_apply_patch once with that operation_id and the intended patch.',
        'After timeout or transport loss, call fs_patch_outcome_show with the same operation_id.',
        'If recovery reports deadline_exceeded_owner_alive, restart only the owning MCP surface and call fs_patch_outcome_show again.',
        'Retry with the same operation_id and identical patch only when retry_safe is true; every other terminal recovery status requires no retry or manual reconciliation.'
      ],
      statuses: {
        accepted: 'The request is durable but parsing/planning has not completed; owner loss reconciles this to interrupted_before_mutation.',
        checked: 'Dry-run validation completed without mutation.',
        applying: 'Mutation has started and carries durable before/after fingerprints for owner-loss reconciliation.',
        patched: 'Mutation completed and changed-file hashes are durable.',
        patched_recovered: 'The owner exited, but the filesystem matches the complete planned after-state; treat the operation as complete.',
        interrupted_before_mutation: 'The owner exited and the filesystem matches the captured before-state; retry_safe is true for the identical operation_id and patch.',
        interrupted_partial: 'The owner exited and files match neither complete state; retry is unsafe and manual reconciliation is required.',
        interrupted_unknown: 'The owner exited without sufficient recovery evidence; retry is unsafe and manual reconciliation is required.',
        failed_before_mutation: 'Parsing, validation, or planning failed and no mutation started.',
        failed_rolled_back: 'Mutation started, failed, and rollback evidence is included.'
      },
      read_mode: 'fs_patch_outcome_show is available in both read and write modes and persists terminal owner-loss reconciliation.',
    }
  } : {};
  const repositoryInventory = workflow === 'repository_inventory' || tool === 'fs_repository_inventory' ? {
    repository_inventory: {
      sequence: [
        'Call fs_repository_inventory with an explicit directory, pattern, limit, and cache policy.',
        'Use candidate_source_paths for bounded source-oriented follow-up and generated_artifact_paths for runtime cleanup review.',
        'Set include_generated: true only when generated artifacts are part of the explicit investigation.',
        'Call git_changed_summary for authoritative tracked and ignored state; this filesystem view does not infer Git status.'
      ],
      classification: {
        candidate_source: 'A path returned by the bounded inventory that is not in a known generated runtime/artifact location.',
        generated_artifact: 'A path under a known .ai/.narada runtime, temporary, output, or patch-outcome location.'
      },
      default_behavior: 'Known generated runtime/artifact patterns are excluded unless include_generated is true.',
      git_tracking_boundary: 'Filesystem inventory identifies candidate and generated paths; git-mcp owns tracked and ignored classification.'
    }
  } : {};
  const fileMetrics = workflow === 'file_metrics' || tool === 'fs_file_metrics' ? {
    file_metrics: {
      sequence: [
        'Call fs_file_metrics with an explicit directory/root, include pattern, ignore policy, limit, max_total_scan_bytes, and cache policy.',
        'Use the returned files table for path, line_count, byte_count, file_type, and scope classification; structuredContent is authoritative.',
        'Use offset and next_offset to page larger trees. totals are explicitly scoped to the returned page and never imply that file contents were transferred.',
        'Prefer this metadata-only operation over concurrent full-content fs_read_file calls when the task needs line counts, sizes, or bounded source inventory.',
      ],
      semantics: {
        line_count: 'Exact for text files within max_bytes_per_file and the cumulative max_total_scan_bytes budget; larger text files return line_count_status=too_large, budget-exhausted files return line_count_status=scan_budget_exceeded, and binary or unavailable files use their own explicit statuses.',
        byte_count: 'Filesystem byte size from stat metadata; no file content is returned.',
        scope: 'The response declares the allowed root, selected directory, include pattern, ignore patterns, and excluded-path boundary.',
      },
    },
  } : {};
  const search = workflow === 'search' || tool === 'fs_search' || tool === 'fs_search_results_read' || tool === 'fs_grep_search' ? {
    search: {
      preferred_tool: 'fs_search',
      sequence: [
        'Call fs_search with query and an explicit directory. Literal matching and line results are the safe defaults.',
        'Choose syntax=regex only when regular-expression behavior is intended; choose result_kind=files or counts only for those jobs.',
        'Use returned opaque continuation arguments unchanged. If result_ref is returned, page it with fs_search_results_read.',
        'Request diagnostics only when cache, freshness, or producer-limit behavior is part of the investigation.',
      ],
      bounds: { default_results: 20, default_inline_chars: 6000, default_text_chars_per_match: 500, carrier_context_defense_in_depth: 8000 },
      compatibility: 'fs_grep_search remains available for regex-oriented legacy calls, but fs_search is the preferred agent-facing contract.',
    },
  } : {};
  return compactFilesystemGuidance({
    schema: 'narada.mcp_surface.guidance.v0',
    status: 'ok',
    surface_id: SURFACE_ID,
    guidance_tool: GUIDANCE_TOOL,
    purpose: PURPOSE,
    requested: { workflow, tool },
    path_resolution: {
      base: 'The first allowed root returned by fs_doctor.allowed_roots and fs_doctor.relative_path_resolution.base.',
      relative_paths: 'Resolve relative filesystem paths against that first allowed root; do not infer the base from the caller current directory.',
      absolute_paths: 'Prefer absolute paths when multiple roots are allowed; the surface still enforces containment.',
      directory_arguments: 'directory, root, and path arguments used for search accept the same filesystem rule.',
      git_boundary: 'Git working_directory has a separate contract; use git_guidance and git_policy_inspect for it.',
    },
    ...patchRecovery,
    ...repositoryInventory,
    ...fileMetrics,
    ...search,
    first_use: [
      'Call this guidance command when the surface is unfamiliar, when a refusal/error is unclear, or before composing a multi-step workflow.',
      'Inspect policy/doctor/status tools before mutation or open-world operations.',
      'Use bounded list/search/query tools for discovery, then show/read/detail tools before acting on a specific object.',
      'Preserve structuredContent as authoritative evidence; text content is for assistant readability.'
    ],
    tool_preference: [
      { step: 'orient', guidance: 'Use *_guidance first when uncertain, then policy/doctor/status tools.' },
      { step: 'discover', guidance: 'Use fs_search for bounded content discovery; use fs_file_metrics for metadata-only line/byte counts and fs_repository_inventory for repository-oriented source/artifact discovery.' },
      { step: 'inspect', guidance: 'Use show/read/detail commands for exact targets before mutation.' },
      { step: 'mutate', guidance: 'Only call mutation tools after policy allows it and intent, target, and expected result are explicit.' },
      { step: 'verify', guidance: 'Read back state with the owning surface after any mutation.' }
    ],
    examples: [
      { intent: 'First use', call: 'fs_guidance({})' },
      { intent: 'Tool-specific help', call: "fs_guidance({ tool: \"<tool_name>\" })" },
      { intent: 'Workflow-specific help', call: "fs_guidance({ workflow: \"<workflow_name>\" })" },
      { intent: 'Repository inventory', call: 'fs_repository_inventory({ directory: ".", pattern: "**/*", limit: 100 })' },
      { intent: 'Find matching lines safely', call: 'fs_search({ query: "symbolName", directory: "packages/example/src" })' },
      { intent: 'Intentional regular expression', call: 'fs_search({ query: "symbol(Name|Kind)", directory: "packages/example/src", syntax: "regex" })' }
    ],
    anti_patterns: [
      'Do not guess hidden state from a tool name; use doctor/status/list/show tools for evidence.',
      'Do not treat assistant text as the durable record when structuredContent is present.',
      'Do not fan out full-content fs_read_file calls when only line counts or byte sizes are needed; use fs_file_metrics with a bounded limit and page deliberately.',
      'Do not raise max_results to compensate for an overly broad query; inspect the bounded first page, narrow scope, or follow the returned continuation.',
      'Do not bypass the owning surface with shell scripts when a governed MCP tool exists.',
      'Do not create or edit ad-hoc executable wrappers (.cmd/.bat or scripts under .ai/tmp/.ai/temp) to run commands; transient executable writes are refused. Call structured_command_start or the owning MCP surface directly. File creation is not execution evidence.',
      'Do not continue after malformed payloads, empty refs, or ambiguous target identifiers; stop and repair the input.'
    ],
    recovery: [
      'For unknown_tool, call tools/list and this guidance command again after restart.',
      'For policy refusal, inspect the surface policy/doctor output and report the exact refusal reason.',
      'For oversized inputs, use the surface payload_ref or output_ref convention when it exists; otherwise reduce scope.',
      'For command execution, use structured_command_start or the owning MCP surface and preserve its execution_ref; do not use fs_write_file to manufacture a transient wrapper.',
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
      'The owning MCP surface remains authoritative for state and enforcement.',
      'fs_repository_inventory is a bounded filesystem view; use git-mcp for authoritative tracked and ignored state.'
    ]
  });
}

function compactFilesystemGuidance<T extends Record<string, any>>(record: T): T {
  const pathResolution = record.path_resolution;
  const compact = compactGuidanceResult(record);
  (compact as Record<string, any>).path_resolution = pathResolution;
  return compact;
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
