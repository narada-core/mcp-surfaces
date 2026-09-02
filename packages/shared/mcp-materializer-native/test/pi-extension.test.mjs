import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';

const templatePath = new URL('../assets/pi-mcp-extension.ts', import.meta.url);
const presentationPath = new URL('../assets/mcp-result-presentation.ts', import.meta.url);
const presentation = await import(presentationPath.href);

async function materializedTemplate(servers) {
  const [template, presentation] = await Promise.all([
    readFile(templatePath, 'utf8'),
    readFile(presentationPath, 'utf8'),
  ]);
  return template
    .replace('__NARADA_MCP_RESULT_PRESENTATION__', presentation)
    .replace('__NARADA_PI_MCP_SERVERS__', JSON.stringify(servers));
}

test('carrier-neutral MCP presentation uses authoritative compact quantities and grammar', () => {
  assert.equal(presentation.compactQuantity(4227), '4.2k');
  assert.equal(presentation.summarizeMcpResult({}, 'x'), 'MCP result (1 character)');
  assert.equal(presentation.summarizeMcpResult({
    structuredContent: { schema: 'narada.mcp_loader.result_page.v1', full_output_char_length: 4227 },
  }, 'short transport text'), 'MCP loader result page · 4.2k characters');
  assert.equal(presentation.summarizeMcpResult({
    structuredContent: { schema: 'narada.mcp_loader.result_page.v1', full_output_char_length: 4227 },
  }, 'full transport text', 'model text'), 'MCP loader result page · 4.2k characters · model-visible 10 characters');
  assert.equal(presentation.collapseMcpResultByDefault(), true);
});

test('generated Pi extension handshakes, registers, calls, and closes admitted MCP server', async () => {
  const root = await mkdtemp(join(tmpdir(), 'narada-pi-extension-'));
  let shutdown;
  try {
    const serverPath = join(root, 'server.mjs');
    const admissionPath = join(root, 'admission.json');
    await writeFile(admissionPath, JSON.stringify({ authority_context: { identity: { agent_id: "marici.Nima" } } }), 'utf8');
    await writeFile(serverPath, `
import { createInterface } from "node:readline";
createInterface({ input: process.stdin }).on("line", (line) => {
  const message = JSON.parse(line);
  if (message.method === "initialize") {
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { protocolVersion: "2026-07-28", capabilities: { tools: {} }, serverInfo: { name: "fixture", version: "1" } } }) + "\\n");
  } else if (message.method === "tools/list") {
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { tools: [
      { name: "fixture_echo", description: "Echo", inputSchema: { type: "object", properties: { value: { type: "string" } }, required: ["value"] } },
      { name: "mcp_loader_runtime_status", inputSchema: { type: "object", properties: {} } },
      { name: "mcp_runtime_proxy_status", inputSchema: { type: "object", properties: {} } },
    ] } }) + "\\n");
  } else if (message.method === "tools/call") {
    const runtimeStatus = message.params.name === "mcp_loader_runtime_status"
      ? { schema: "narada.mcp_loader.runtime_status.v1", status: "ok", runtime_freshness: { status: "current" } }
      : message.params.name === "mcp_runtime_proxy_status"
        ? { schema: "narada.mcp_runtime_proxy.status.v1", status: "ok", runtime_freshness: { status: "current" } }
        : null;
    if (runtimeStatus) {
      process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { structuredContent: runtimeStatus } }) + "\\n");
      return;
    }
    if (message.params.name === "mcp_loader_inspect_binding_tool") {
      process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { structuredContent: { schema_lease: "query-lease" } } }) + "\\n");
      return;
    }
    if (message.params.name === "mcp_loader_call_binding_tool") {
      const bindingStructured = message.params.arguments?.value === "submit_review_admit_nested"
        ? { schema: "narada.epistemic.submit_review_admit.v1", status: "admitted", submission: { proposal_id: "proposal-nested", proposal_digest: "digest-nested", content_fingerprint: "content-nested", operation_count: 2, operations: [{ op: "entity_create" }] }, review: { status: "policy_valid", review_details: { repeated: "input" } }, admission: { status: "admitted", proposal_id: "proposal-nested", ledger_head: "head-nested", event: { operations: [{ op: "entity_create" }] } }, review_gate_preserved: true, certifies_truth: false }
        : { schema: "narada.epistemic.query.v1", items: [{ event_id: "ev-000000000012-a" }, { event_id: "ev-000000000015-b" }] };
      process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { structuredContent: { result: { structuredContent: bindingStructured } } } }) + "\\n");
      return;
    }
    if (message.params.name === "fixture_echo" && message.params.arguments?.value === "submit_review_admit_nested") {
      const fixture_nested_submit = { schema: "narada.epistemic.submit_review_admit.v1", status: "admitted", submission: { proposal_id: "proposal-nested", proposal_digest: "digest-nested", content_fingerprint: "content-nested", operation_count: 2, operations: [{ op: "entity_create" }] }, review: { status: "policy_valid", review_details: { repeated: "input" } }, admission: { status: "admitted", proposal_id: "proposal-nested", ledger_head: "head-nested", event: { operations: [{ op: "entity_create" }] } }, review_gate_preserved: true, certifies_truth: false };
      process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { structuredContent: { result: { structuredContent: fixture_nested_submit } } } }) + "\\n");
      return;
    }
    const fixtures = {
      stat: { schema: "local.filesystem.stat.v1", path: "C:/repo/file.md", relative_path: "file.md", type: "file", size: 13558 },
      grep_result: { schema: "local.filesystem.grep.v1", status: "ok", output_mode: "content", offset: 0, limit: 2, count: 2, count_exact: true, count_semantics: "count is the exact full result count", scanned: 2, scanned_unit: "matched_entries", returned: 2, max_matches: 30, max_output_chars: 4000, order: "ripgrep_traversal", cache_hit: true, snapshot_reused: true, cache_policy: "auto", snapshot_id: "s_grep", requested_snapshot_id: null, snapshot_complete: true, cache_memory_bytes: 40, page_match_bytes: 40, page_match_bytes_limit: 4000, page_matches_truncated: 0, timeout_ms: 30000, freshness: { path: "C:/repo", type: "directory", size: 100, mtime: "2026-09-02T00:00:00Z", mtime_ms: 1, tree_sha256: "secret-tree" }, scope: { requested_path: "packages", root: "C:/repo", path: "C:/repo/packages", argument: "directory", include_glob: null, default_exclusions_applied: true }, has_more: false, next_offset: null, continuation: null, matches_format: "structured", match_objects_authoritative: true, match_objects: [{ path: "packages/a.ts", line: 10, text: "const a = 1;", raw: "packages/a.ts|10|const a = 1;" }, { path: "packages/b.ts", line: 20, text: "const b = 2;", raw: "packages/b.ts|20|const b = 2;" }] },
      guidance_result: {
        schema: "narada.epistemic.guidance.v2",
        purpose: "Preserve evolving problem situations, not certify truth.",
        workflow: [
          { step: 1, tool: "epistemic_graph_submit_review_admit", preferred: true, why: "Perform the ordinary submit, preserved policy review, and admission workflow atomically. Omit expected_ledger_head to snapshot the current head and omit idempotency_key for deterministic retry safety." },
          { step: 2, tool: "epistemic_graph_capture_sources", alternative: true, why: "Create a reviewable source proposal when manual review before admission is intended; operations may be empty for pure source capture." },
          { step: 3, tool: "epistemic_graph_proposal_submit", alternative: true, why: "Persist a reviewable proposal without source batching." },
          { step: 4, tools: ["epistemic_graph_proposal_review", "epistemic_graph_proposal_admit"], manual_only: true, why: "Use separate calls only when the operator wants an explicit pause between proposal, review, and admission." },
          { step: 5, tool: "epistemic_graph_neighborhood", why: "Verify the admitted problem situation and its relations." },
        ],
        read_routing: [
          { tool: "epistemic_graph_team_work_overview", use_when: "A bounded turn-boundary view is needed." },
          { tool: "epistemic_graph_issue_tree_frontier", use_when: "The unresolved leaves of one known tree are needed." },
          { tool: "epistemic_graph_neighborhood", use_when: "One entity's explicit one-hop relations are needed." },
          { tool: "epistemic_graph_snapshot", use_when: "A broad paged visualization is needed." },
        ],
        payload_transport: {
          accepted_by: ["epistemic_graph_proposal_submit", "epistemic_graph_submit_review_admit"],
          call_shape: { payload_ref: "mcp_payload:<payload-id>@v<positive-revision>" },
          exclusive_form: "Supply payload_ref alone; do not combine it with actor, authority_basis, operations, or another inline proposal field.",
          authority_rule: "A payload reference is immutable transport, not mutation authority. The target Site binding and graph mutation boundary still validate actor, authority_basis, schema, and operations.",
          retry_rule: "The same valid immutable revision is safe to retry only when its content remains contract-valid.",
        },
        identity_rule: {
          relations: "Omit relation_id to derive it deterministically.",
          idempotency: "Omit idempotency_key for deterministic content-hash retry identity; supply one only to name a wider caller-defined retry scope.",
        },
        revision_pattern: {
          entity_title_correction: "Declare a successor entity with the corrected title and connect it to the prior entity using supersedes. Keep the prior identity as immutable history.",
          discovery: "Query or inspect the predecessor neighborhood before declaring the successor.",
          reason: "The graph is append-only.",
        },
        concurrency_rule: "Omit expected_ledger_head to snapshot the live head during submission while retaining CAS protection through admission. Supply a concrete status.ledger_head only when an external read must be the concurrency boundary. If review reports stale, query again and submit a new proposal; do not rewrite the immutable proposal.",
        admission_meaning: "policy-valid contribution; never truth certification",
        problem_policy: "Transform apparent solutions into successor problems; record closure only as an attributed assessment.",
        immutable_payload_recovery: { noisy: "x".repeat(9000) },
        communication_model: { noisy: "y".repeat(3000) },
        minimal_example: { noisy: "z".repeat(500) },
        requested: { workflow: null, tool: null },
      },
      tools_result: { schema: "narada.mcp_loader.tools.v1", status: "ok", connection_id: "c-tools", surface_id: "epistemic-graph", compact: true, tool_count: 3, tools: [{ name: "epistemic_graph_guidance", description: "Explain the problem-situation graph workflow.", inputSchema: { type: "object" } }, { name: "epistemic_graph_query", description: "Run a bounded query with a very long description that must not enter the model projection.", inputSchema: { type: "object", properties: { query: { type: "object" } } } }, { name: "epistemic_graph_submit_review_admit", description: "Submit, review, and admit a contribution.", inputSchema: { type: "object", required: ["actor"] } }], runtime_freshness: { status: "current", reload_required: false, source: "omit" }, runtime_lifecycle: { restartability_status: "available", session_restart_required: false, guidance: "omit" } },
      empty_range: { schema: "local.filesystem.read.v1", relative_path: "file.md", total_lines: 250, returned_lines: 0, offset: 300, requested_start_line: 300, requested_end_line: 380 },
      empty_valid_range: { schema: "local.filesystem.read.v1", relative_path: "file.md", total_lines: 250, returned_lines: 0, offset: 200, requested_start_line: 200, requested_end_line: 200 },
      replace: { schema: "local.filesystem.str_replace_file.v1", status: "replaced", relative_path: "file.md", occurrences: 1 },
      bridge: { schema: "narada.task.inbox.bridge.v1", status: "planned", count: 0, envelopes: [] },
      generic_large: (() => {
        const value = { schema: "narada.mcp_loader.result_page.v1", full_output_char_length: 4227, payload: "" };
        value.payload = "x".repeat(4227 - JSON.stringify(value).length);
        return value;
      })(),
      surface_handle_opened: { schema: "narada.mcp_loader.surface_handle_opened.v1", status: "reopened", surface_handle: "msh_h123", handle_scope: "loader_process", handle_survives_child_restart: true, handle_survives_loader_restart: false, logical_connection_id: "logical-1", connection_id: "connection-1", binding_id: "repo-git", ownership: { owner: "mcp-loader", owner_pid: 12345 }, generation_id: "generation-1", site_root: "C:/repo", surface_id: "git", runtime_kind: "native", runtime_requirements: ["none"], tool_count: 24, created_at: "2026-09-02T00:00:00Z", call: { tool_name: "mcp_loader_call_surface_tool", arguments: { surface_handle: "msh_h123", tool_name: "<child_tool>" } } },
      connection_inventory: { schema: "narada.mcp_loader.connection_inventory.v1", status: "ok", compact: true, max_connections: 8, connection_count: 2, available_slots: 6, live_count: 2, closed_count: 0, live_connection_ids: ["c-1", "c-2"], closed_connection_ids: [], connections: [{ connection_id: "c-1", binding_id: "repo-git", generation_id: "generation-1", surface_id: "git", liveness: "live", age_ms: 12345, pending_request_count: 0, actions: { inspect: { tool_name: "mcp_loader_surface_status", arguments: { connection_id: "c-1" } }, detach: { tool_name: "mcp_loader_detach", arguments: { connection_id: "c-1" } }, restart: { actuator: "mcp-loader", tool_name: "mcp_loader_surface_restart", arguments: { connection_id: "c-1" } } }, ownership: { owner: "mcp-loader", owner_pid: 12345 } }, { connection_id: "c-2", binding_id: "repo-worker", generation_id: "generation-2", surface_id: "worker", liveness: "live", age_ms: 67890, pending_request_count: 1, actions: { inspect: { tool_name: "mcp_loader_surface_status" }, detach: { tool_name: "mcp_loader_detach" }, restart: { actuator: "carrier-supervisor", tool_name: null } }, ownership: { owner: "mcp-loader", owner_pid: 12346 } }], runtime_freshness: { status: "current", reload_required: false, runtime_entrypoint: { path: "C:/long/path.exe" } }, recovery: { noisy: true } },
      site_surfaces: { schema: "narada.mcp_loader.site_surfaces.v1", status: "ok", compact: true, site_root: "C:/repo", surface_count: 2, surfaces: [{ binding_id: "repo-git", surface_id: "git", server_name: "narada-repo-git", runtime_requirements: [], next_call: { tool_name: "mcp_loader_attach_surface", arguments: { site_root: "C:/repo", binding_id: "repo-git", surface_id: "git", noisy: "omit" } }, noisy: "omit" }, { binding_id: "repo-worker", surface_id: "worker", server_name: "narada-repo-worker", runtime_requirements: ["runtime"], next_call: { tool_name: "mcp_loader_attach_surface", arguments: { site_root: "C:/repo", binding_id: "repo-worker", surface_id: "worker" } } }] },
      team_work_overview: { schema: "narada.epistemic.team_work_overview.v1", status: "ok", mode: "detailed", query_origin: "named_template", template: "epistemic:team-work-overview", ledger_head: "secret-head", ledger_sequence: 42, items: [{ tree_id: "tree:1", objective: "Objective", member: "agent-a", status: "blocked", leaf: { node_id: "leaf:1", title: "Leaf", noisy: "omit" }, latest_attributable_transition: "2026-09-01T00:00:00Z", attribution_basis: "canonical", freshness: { classification: "fresh", rule: "bounded" }, blocker_count: 1, directed_handoff_count: 2, live_presence: { claimed: false, capability: "unavailable", reason: "omit" }, noisy: "omit" }], returned: 1, limit: 10, has_more: false, next_cursor: null, coverage: { queried_members: ["agent-a"], queried_trees: ["tree:1"], complete: true, total_matching: 1, omitted_count: 0, unattributed_active_tree_count: 0, partial_evidence_classes: [], unavailable_evidence_classes: ["live_process_heartbeat"], noisy: "omit" }, semantics: { frontier: "unresolved only; never ownership or current activity", communications: "coordination and attribution only; never scientific evidence", live_presence: "not claimed without a separate typed heartbeat capability" }, bounded: true },
      query_result: { schema: "narada.epistemic.query.v2", query_mode: "datalog", query_origin: "raw", template: null, ledger_head: "secret-head", items: [{ entity_id: "claim:1", payload: { title: "Keep", detail: "answer" } }], count: 1, returned_count: 1, count_semantics: "returned_page", limit: 10, output_bytes: 321, max_output_bytes: 10000, has_more: false, next_cursor: null, normalization: { applied: true, normalized_count: 1 }, query_cost: { planner_mode: "bounded_clause_plan", datoms_loaded: 10, hard_caps: { max_datoms: 100 } } },
      inbox_query: { schema: "narada.epistemic.query.v2", query_mode: "datalog", query_origin: "named_template", template: "epistemic:inbox", ledger_head: "head", items: [{ sender: "sender-a", recipient: "recipient-b", event_id: "event-a", body: "body alpha" }, { sender: "sender-c", recipient: "recipient-b", event_id: "event-c", body: "body beta" }] },
      issue_tree: { schema: "narada.epistemic.issue-tree.resume.v1", status: "ok", tree: { tree_id: "tree:ax", objective: "AX", version: "3" }, selected: { node_id: "issue:selected", version: "2", title: "Selected", state: "selected", score: 0.9 }, frontier: { items: [{ node_id: "issue:selected", state: "selected", score: 0.9 }, { node_id: "issue:open", state: "open", score: 0.8 }], returned: 2, complete: true, total: 2, total_exact: true }, continuation: null, certifies_truth: false, noncertification: "coordination state; not evidence" },
      producer_command_page: { schema: "narada.producer_output_page.v1", status: "ok", output_ref: "mcp_output:command", reader_tool: "mcp_loader_read_result", full_output_char_length: 3000, output_text: JSON.stringify({ schema: "narada.structured_command.execution_result.v0", status: "ok", executed: true, pending: false, execution_ref: "execution-page", exit_code: 0, command: "pnpm", args: ["test"], working_directory: "C:/repo", stdout: "command body", stderr: "", stdout_truncated: false, stderr_truncated: false, timed_out: false, cancelled: false, execution_posture: { noisy: true }, test_scope: "noisy" }) },
      producer_page: { schema: "narada.producer_output_page.v1", status: "ok", output_ref: "mcp_output:fixture", reader_tool: "mcp_loader_read_result", full_output_char_length: 1234, output_text: JSON.stringify({ schema: "child.result.v1", answer: "only child output" }) },
      loader_page: { schema: "narada.mcp_loader.tool_result.v1", status: "ok", result_summary: { schema: "child.result.v1", status: "ok" }, result: { schema: "narada.producer_output_page.v1", status: "ok", output_ref: "mcp_output:nested", reader_tool: "mcp_loader_read_result", full_output_char_length: 1234, output_text: JSON.stringify({ schema: "child.result.v1", answer: "only nested child output" }) } },
      loader_result: { schema: "narada.mcp_loader.tool_result.v1", connection_id: "c1", surface_id: "s1", result_summary: { schema: "child.result.v1", status: "ok" }, result: { schema: "child.result.v1", answer: "only unwrapped child result" } },
      runtime_freshness: { schema: "narada.mcp_loader.runtime_freshness.v1", status: "current", reload_required: false, freshness_scope: "native_loader_artifact", process_started_at: "2026-09-01T00:00:00Z", runtime_entrypoint: { path: "C:/long/runtime.exe" }, source_entrypoint: { path: "C:/long/source.rs" }, reasons: [], reload_action: { owner: "carrier_or_runtime_supervisor", capability: "restart_mcp_loader_process", guidance: "omit while current" }, noisy: "omit" },
      schema_lease_compact: { schema: "narada.mcp_loader.schema_lease.v1", status: "issued", connection_id: "c-compact", logical_connection_id: "logical-compact", generation_id: "generation-compact", surface_id: "surface-compact", tool_name: "surface_read", tool_schema_digest: "tool-digest", tool_contract_digest: "contract-digest", input_schema_digest: "input-digest", output_schema_digest: "output-digest", description: "Read the surface", annotations: { readOnlyHint: true }, argument_skeleton: { site_root: "x" }, minimal_valid_arguments: { site_root: "x" }, minimal_valid_arguments_status: "validated", verbose_contract_call: { tool_name: "mcp_loader_inspect_tool" }, schema_lease: "lease-compact", lease_scope: "loader_process_child_generation", transferable: false, authorization_resolution: "lease_renewed", input_contract: { type: "object", required: ["site_root"], properties: { site_root: { type: "string" } } } },
      schema_lease_verbose: { schema: "narada.mcp_loader.schema_lease.v1", status: "issued", connection_id: "c-verbose", logical_connection_id: "logical-verbose", generation_id: "generation-verbose", surface_id: "surface-verbose", tool_name: "surface_write", tool_schema_digest: "verbose-tool-digest", tool_contract_digest: "verbose-contract-digest", input_schema_digest: "verbose-input-digest", output_schema_digest: "verbose-output-digest", description: "Write the surface", annotations: { readOnlyHint: false }, argument_skeleton: { value: "x" }, minimal_valid_arguments: { value: "x" }, minimal_valid_arguments_status: "validated", verbose_contract_call: { tool_name: "mcp_loader_inspect_tool" }, schema_lease: "lease-verbose", lease_scope: "loader_process_child_generation", transferable: false, authorization_resolution: "lease_renewed", tool_contract: { name: "surface_write", description: "Write the surface", inputSchema: { type: "object", required: ["value"], properties: { value: { type: "string" } } }, annotations: { readOnlyHint: false } } },
      structured_command: { schema: "narada.structured_command.execution_result.v0", status: "running", executed: true, pending: true, execution_ref: "execution-1", command: "pnpm", args: ["test"], working_directory: "C:/repo", started_at: "2026-09-01T00:00:00Z", timeout_ms: 900000, execution_posture: { test_scope: "noisy", expected_cost: "medium" }, test_scope: "noisy", expected_cost: "medium", stdout: "", stderr: "", stdout_truncated: false, stderr_truncated: false, timed_out: false, cancelled: false, stdout_char_length: 0, stderr_char_length: 0 },
      loader_structured_command_running: { schema: "narada.mcp_loader.tool_result.v1", status: "ok", connection_id: "c1", tool_name: "structured_command_execute", result_summary: { schema: "narada.structured_command.execution_result.v0", status: "running" }, result: { structuredContent: { schema: "narada.structured_command.execution_result.v0", status: "running", executed: true, pending: true, execution_ref: "execution-nested", command: "pnpm", args: ["test"], working_directory: "C:/repo", started_at: "2026-09-01T00:00:00Z", timeout_ms: 900000, stdout: "", stderr: "", execution_posture: { test_scope: "noisy", expected_cost: "medium" }, noisy: "omit" } } },
      git_status: { schema: "narada.git.status.v1", status: "ok", working_directory: "C:/repo", repository_root: "C:/repo", branch: "main", upstream: "origin/main", ahead: 0, behind: 0, clean: true, status_entries: [{ x: " ", y: "M", path: "owned.ts", original_path: null, conflict: false }], staged: [], unstaged: ["owned.ts"], untracked: [], conflicts: [], summary: { staged_count: 0, unstaged_count: 1, untracked_count: 0, conflict_count: 0, matching_path_count: 1, clean: true }, remotes: [{ name: "origin", fetch_url: "https://secret.example/repo.git" }], push_target: { status: "resolved", remote: "origin", branch: "main", source: "upstream" }, push_remediation: null, query: { noisy: true } },
      git_add: { schema: "narada.git.add.v1", status: "ok", operation: "add", working_directory: "C:/repo", paths: ["owned.ts"], work_scope_ref: "scope-1", output: "verbose output", summary: "staged explicit paths", post_status: { schema: "narada.git.status.v1", status: "ok", repository_root: "C:/repo", branch: "main", upstream: "origin/main", ahead: 0, behind: 0, clean: true, staged: ["owned.ts"], unstaged: [], untracked: [], conflicts: [], summary: { staged_count: 1, unstaged_count: 0, untracked_count: 0, conflict_count: 0, clean: true }, remotes: [{ fetch_url: "https://secret.example" }] } },
      git_commit: { schema: "narada.git.commit.v1", status: "ok", working_directory: "C:/repo", commit: "abc123", commit_ref: "git_commit:abc123", committed_entries: [{ path: "owned.ts", secret: "omit" }], committed_files: ["owned.ts"], committed_file_count: 1, work_scope_ref: "scope-1", output: "[main abc123] message", summary: "[main abc123] message", post_status: { schema: "narada.git.status.v1", status: "ok", branch: "main", upstream: "origin/main", ahead: 1, behind: 0, clean: true, staged: [], unstaged: [], untracked: [], conflicts: [], summary: { staged_count: 0, unstaged_count: 0, untracked_count: 0, conflict_count: 0, clean: true } } },
      git_push: { schema: "narada.git.push.v1", status: "ok", working_directory: "C:/repo", remote: "origin", branch: "main", commit: "abc123", commit_ref: "git_commit:abc123", work_scope_ref: "scope-1", output: "To origin", summary: "To origin", post_status: { schema: "narada.git.status.v1", noisy: true } },
      surface_attached: { schema: "narada.mcp_loader.surface_attached.v1", status: "attached", connection_id: "c-attach", logical_connection_id: "logical-attach", generation_id: "generation-attach", site_root: "C:/repo", surface_id: "git", binding_id: "repo-git", admission_envelope_id: "ambient", binding_digest: "binding-digest", authority_epoch: 4, runtime_kind: "native", runtime_requirements: ["none"], runtime_lifecycle: { schema: "narada.mcp_loader.runtime_lifecycle.v1", managed_by: "mcp-loader", restartable: true, restartability_status: "available", restart_scope: "attached_child_process", session_restart_required: false, guidance: "long lifecycle guidance", actions: { inspect: { tool_name: "mcp_loader_surface_status" } }, loader_restart_action: { tool_name: "restart_mcp_loader_process" } }, runtime_freshness: { schema: "narada.mcp_loader.runtime_freshness.v1", status: "current", reload_required: false, process_started_at: "2026-09-01T00:00:00Z", runtime_entrypoint: { path: "C:/long/path.exe" }, source_files: [{ name: "source" }], reasons: [] }, runtime_command: "C:/long/runtime.exe", entrypoint: "C:/long/entrypoint.exe", args: ["--secret-looking-arg"], child_invocation_kind: "native_applet", server_info: { name: "git-mcp", version: "0.1.0", extra: "omit" }, tool_count: 24, tool_discovery: { tool_name: "mcp_loader_list_tools", arguments: { connection_id: "c-attach" }, extra: "omit" }, tool_inspection: { required_arguments: ["connection_id", "tool_name"], extra: "omit" }, descriptor_digest: "descriptor-digest", tool_contract_digest: "contract-digest", declared_tool_contract_digest: "declared-digest", lifecycle: { mode: "replayable", reason: "long reason" }, ownership: { owner: "mcp-loader", owner_pid: 12345 } },
      site_inventory: (() => {
        const observed = Array.from({ length: 600 }, (_, index) => "surface_tool_" + index);
        return { schema: "narada.mcp_loader.site_tool_inventory_check.v1", status: "drift", site_root: "C:/repo", observed_at: "2026-09-01T00:00:00Z", requested_surface_ids: null, runtime_kind: "codex", attempted_surface_ids: ["surface-a", "surface-b"], observed_surface_ids: ["surface-a", "surface-b"], unobserved_surface_ids: [], runtime_skipped_surface_ids: [], runtime_skipped_count: 0, observation_coverage: "complete", checked_surface_count: 2, violation_count: 1, observed_tools: { "surface-a": observed, "surface-b": observed }, observed_read_only_tools: { "surface-a": observed }, observed_mutating_tools: { "surface-b": observed }, observed_unclassified_tools: { "surface-a": [] }, finding_status_counts: { drift: 1, ok: 1 }, findings: [{ surface_id: "surface-a", status: "drift", declared_count: 2, observed_count: 2, missing_from_fabric: ["surface_tool_new"], extra_in_fabric: ["surface_tool_old"], duplicate_declared_tools: [], duplicate_observed_tools: ["surface_tool_old"], unclassified_observed_tools: [] }, { surface_id: "surface-b", status: "ok", declared_count: 1, observed_count: 1, missing_from_fabric: [], extra_in_fabric: [], duplicate_declared_tools: [], duplicate_observed_tools: [], unclassified_observed_tools: [] }], observation_ref: "mcp_output:inventory", observation_sha256: "inventory-digest", observation_byte_size: 50000, observation_retention: { owner: "mcp-loader", lifecycle: "temporary" } };
      })(),
      mcp_page: { schema: "narada.mcp_output_page.v1", status: "ok", ref: "mcp_output:page", path: ".ai/tmp/mcp-outputs/workspace/page.json", full_output_char_length: 1234, output_text: JSON.stringify({ schema: "child.result.v1", answer: "only mcp output" }) },
      worker_page: { schema: "narada.worker.output_page.v1", status: "ok", ref: "worker_output:page", path: "worker.json", output_text: JSON.stringify({ schema: "child.result.v1", answer: "only worker output" }) },
      authoritative_json: { schema: "local.filesystem.read.v1", content_delivery: { format: "filesystem_read_text", channel: "content" }, relative_path: "file.json" },
      filesystem_read_result: (() => {
        const body = "const filesystemRead = true;\\n";
        const path = "C:/repo/" + "nested/".repeat(200) + "read.ts";
        const readValue = {
          schema: "local.filesystem.read.v1",
          path,
          root: "C:/repo",
          relative_path: path.slice("C:/repo/".length),
          total_lines: 1,
          total_lines_exact: true,
          total_lines_status: "exact",
          line_window_complete: true,
          offset: 1,
          limit: 400,
          requested_limit: 400,
          requested_start_line: null,
          requested_end_line: null,
          served_end_line: 1,
          returned_lines: 1,
          next_offset: null,
          next_start_line: null,
          continuation: null,
          content: body,
          content_sha256: "body",
          content_hash_scope: "full_file",
          hash_source: "live_file_bytes",
          cache_used: false,
          content_window_sha256: "window",
          max_limit: 1000,
          limit_adjusted: false,
          pagination_required: false,
          has_more: false,
          requested_range_complete: true,
          timeout_ms: 5000,
        };
        const { content, ...structuredContent } = readValue;
        structuredContent.content_delivery = {
          channel: "content",
          block_index: 0,
          format: "filesystem_read_text",
          duplicated_in_structured_content: false,
        };
        return {
          schema: "narada.mcp_loader.tool_result.v1",
          connection_id: "c-filesystem",
          surface_id: "local-filesystem",
          result_summary: { schema: readValue.schema, status: "ok" },
          result: {
            content: [{ type: "text", text: JSON.stringify(readValue, null, 2) }],
            structuredContent,
            isError: false,
          },
        };
      })(),
      write_file: { schema: "local.filesystem.write_file.v1", status: "written", path: "C:/site/.ai/file.md", root: "C:/site", relative_path: ".ai/file.md", size: 17, create_parent_directories: true, before_sha256: "before", after_sha256: "after", sha256: "after", content_sha256: "after", timeout_ms: 30000 },
      submit_review_admit: { schema: "narada.epistemic.submit_review_admit.v1", status: "admitted", submission: { proposal_id: "proposal-1", proposal_digest: "digest-1", content_fingerprint: "content-1", operation_count: 4, operations: [{ op: "entity_create", title: "repeated input" }] }, review: { status: "policy_valid", review_details: { repeated: "input" } }, admission: { status: "admitted", proposal_id: "proposal-1", ledger_head: "head-1", event: { operations: [{ op: "entity_create", title: "repeated input" }] } }, review_gate_preserved: true, certifies_truth: false },
      schema_lease: { schema: "narada.mcp_loader.schema_lease.v1", status: "issued", connection_id: "c1", surface_id: "git", tool_name: "git_status", schema_lease: "lease-1", tool_schema_digest: "digest-1", input_schema_digest: "input-digest", binding_resolution: { surface_handle: "handle", runtime_lifecycle: { noisy: true } }, input_contract: { required: [], properties: ["working_directory"] }, tool_contract: { name: "git_status", inputSchema: { type: "object" } } },
      inventory: { schema: "narada.mcp_loader.site_tool_inventory_check.v1", status: "drift", observation_coverage: "partial", checked_surface_count: 3, violation_count: 1, finding_status_counts: { drift: 1 }, observation_ref: "mcp_payload:inventory", observed_tools: { git: ["tool-a", "tool-b"], worker: ["tool-c"] }, runtime_freshness: { noisy: true }, findings: [{ surface_id: "git", status: "drift", declared_count: 10, observed_count: 8, missing_from_fabric: ["tool-a"], extra_in_fabric: ["tool-z"], duplicate_declared_tools: [], duplicate_observed_tools: [], unclassified_observed_tools: [] }] },
      oversized: { schema: "narada.epistemic.query.v2", status: "ok", payload: "x".repeat(21000) },
    };
    fixtures.schema_lease_batch = {
      schema: "narada.mcp_loader.schema_lease_batch.v1",
      status: "issued",
      connection_id: "c-batch",
      surface_handle: "msh-batch",
      lease_count: 1,
      leases: [fixtures.schema_lease_compact],
      binding_resolution: { binding_id: "repo-git", canonical_binding_id: "repo-git" },
    };
    fixtures.loader_grep_result = {
      schema: "narada.mcp_loader.tool_result.v1",
      status: "ok",
      connection_id: "c1",
      surface_id: "local-filesystem",
      result_summary: { schema: "local.filesystem.grep.v1", status: "ok" },
      result: { structuredContent: fixtures.grep_result },
    };
    fixtures.loader_guidance_result = {
      schema: "narada.mcp_loader.tool_result.v1",
      status: "ok",
      connection_id: "c1",
      surface_id: "epistemic-graph",
      result_summary: { schema: "narada.epistemic.guidance.v2", status: "ok" },
      result: { structuredContent: fixtures.guidance_result },
    };
    fixtures.loader_tools_result = {
      schema: "narada.mcp_loader.tool_result.v1",
      status: "ok",
      connection_id: "c-tools",
      surface_id: "epistemic-graph",
      result_summary: { schema: "narada.mcp_loader.tools.v1", status: "ok", tool_count: 3 },
      result: { structuredContent: fixtures.tools_result },
    };
    fixtures.loader_tools_page = {
      schema: "narada.mcp_loader.tool_result.v1",
      status: "ok",
      connection_id: "c-tools",
      surface_id: "epistemic-graph",
      result_summary: { schema: "narada.mcp_loader.tools.v1", status: "ok", tool_count: 3 },
      result_bounded: true,
      result: { schema: "narada.producer_output_page.v1", status: "ok", output_ref: "mcp_output:tools", reader_tool: "mcp_loader_read_result", full_output_char_length: 5900, output_text: JSON.stringify({ structuredContent: fixtures.tools_result, isError: null }, null, 2) },
    };
    fixtures.loader_tools_truncated_page = {
      ...fixtures.loader_tools_page,
      details_ref: "mcp_output:tools-truncated",
      details_reader: "mcp_loader_read_result",
      result: {
        ...fixtures.loader_tools_page.result,
        output_ref: "mcp_output:tools-truncated",
        ref: "mcp_output:tools-truncated",
        output_text: fixtures.loader_tools_page.result.output_text.slice(0, 120),
        next_offset: 120,
        transport_next_offset: 120,
        full_output_char_length: fixtures.loader_tools_page.result.output_text.length,
      },
    };
    const requestedValue = message.params.arguments.value;
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: {
      content: [{ type: "text", text: requestedValue === "authoritative_json" ? JSON.stringify(fixtures.producer_page) : "summary without lease" }],
      structuredContent: fixtures[requestedValue] ?? { value: requestedValue, schema_lease: "lease-fixture" }
    } }) + "\\n");
  }
});
`, 'utf8');

    const servers = [{
      name: 'mcp-loader',
      command: process.execPath,
      args: [serverPath, '--binding-admission-path', admissionPath],
      enabled: true,
      startupTimeoutMs: 2000,
    }];
    const source = await materializedTemplate(servers);
    const extensionPath = join(root, 'index.ts');
    await writeFile(extensionPath, source, 'utf8');
    const extension = (await import(pathToFileURL(extensionPath).href)).default;

    const handlers = new Map();
    const registered = [];
    const notifications = [];
    const eventHandlers = new Map();
    const shortcuts = new Map();
    const pi = {
      events: {
        on(name, handler) { eventHandlers.set(name, handler); },
        off(name) { eventHandlers.delete(name); },
      },
      on(name, handler) {
        handlers.set(name, handler);
      },
      getAllTools() {
        return [];
      },
      registerCommand() {},
      registerShortcut(shortcut, options) {
        shortcuts.set(shortcut, options);
      },
      registerTool(tool) {
        registered.push(tool);
      },
    };
    extension(pi);
    shutdown = () => handlers.get('session_shutdown')?.();
    await handlers.get('session_start')?.({}, { ui: { notify(message, level) { notifications.push({ message, level }); } } });

    assert.match(notifications[0].message, /mcp=loading/);
    assert.match(notifications[1].message, /mcp=attached/);
    assert.match(notifications[1].message, /loader=current/);
    assert.match(notifications[1].message, /proxy=current/);
    assert.match(notifications[1].message, /restart_owner=none/);
    assert.match(notifications[1].message, /identity=marici\.Nima/);
    const inboxPoll = await new Promise((resolve, reject) => eventHandlers.get('narada:mcp:marici-inbox-poll')({
      siteRoot: 'C:/Users/andrey/src/marici',
      sinceSequence: 10,
      requestId: 'fixture-poll',
      resolve,
      reject,
    }));
    assert.deepEqual(inboxPoll, {
      count: 2,
      previousCursor: 10,
      newCursor: 15,
      source: 'epistemic_graph_communication',
    });
    assert.equal(registered.length, 3);
    assert.equal(registered[0].name, 'fixture_echo');
    assert.deepEqual(registered[0].parameters.required, ['value']);
    const result = await registered[0].execute('call-1', { value: 'hello' }, new AbortController().signal);
    assert.deepEqual(JSON.parse(result.content[0].text), {
      value: 'hello',
      schema_lease: 'lease-fixture',
    });
    assert.doesNotMatch(result.content[0].text, /summary without lease/);
    const inboxQuery = await registered[0].execute('call-inbox-query', { value: 'inbox_query' }, new AbortController().signal);
    assert.deepEqual(JSON.parse(inboxQuery.content[0].text), ['body alpha', 'body beta']);
    assert.doesNotMatch(inboxQuery.content[0].text, /sender-a|recipient-b|event-a|ledger_head/);
    assert.notEqual(registered[0].renderResult(inboxQuery, { expanded: true }).render(160).join(String.fromCharCode(10)), inboxQuery.content[0].text);
    const queryResult = await registered[0].execute('call-query-result', { value: 'query_result' }, new AbortController().signal);
    const queryModel = JSON.parse(queryResult.content[0].text);
    assert.deepEqual(Object.keys(queryModel).sort(), ['count', 'has_more', 'items', 'next_cursor', 'returned_count', 'schema'].sort());
    assert.deepEqual(queryModel.items, [{ entity_id: 'claim:1', payload: { title: 'Keep', detail: 'answer' } }]);
    assert.equal(queryModel.query_mode, undefined);
    assert.equal(queryModel.ledger_head, undefined);
    assert.equal(queryModel.query_cost, undefined);
    assert.ok(queryResult.content[0].text.length < 700);

    const grepResult = await registered[0].execute('call-loader-grep-result', { value: 'loader_grep_result' }, new AbortController().signal);
    const grepModel = JSON.parse(grepResult.content[0].text);
    assert.deepEqual(Object.keys(grepModel).sort(), ['count', 'count_exact', 'has_more', 'limit', 'match_objects', 'next_offset', 'offset', 'output_mode', 'returned', 'schema', 'snapshot_complete', 'snapshot_id', 'status'].sort());
    assert.deepEqual(grepModel.match_objects, [
      { path: 'packages/a.ts', line: 10, text: 'const a = 1;' },
      { path: 'packages/b.ts', line: 20, text: 'const b = 2;' },
    ]);
    assert.equal(grepModel.scope, undefined);
    assert.equal(grepModel.continuation, undefined);
    assert.ok(grepResult.content[0].text.length < 800);

    const guidanceResult = await registered[0].execute('call-loader-guidance-result', { value: 'loader_guidance_result' }, new AbortController().signal);
    const guidanceModel = JSON.parse(guidanceResult.content[0].text);
    assert.deepEqual(Object.keys(guidanceModel).sort(), ['admission_meaning', 'concurrency_rule', 'identity_rule', 'payload_transport', 'problem_policy', 'purpose', 'read_routing', 'schema', 'workflow'].sort());
    assert.deepEqual(guidanceModel.workflow, [
      { step: 1, tool: 'epistemic_graph_submit_review_admit', preferred: true },
      { step: 2, tool: 'epistemic_graph_capture_sources', alternative: true },
      { step: 3, tool: 'epistemic_graph_proposal_submit', alternative: true },
      { step: 4, tools: ['epistemic_graph_proposal_review', 'epistemic_graph_proposal_admit'], manual_only: true },
      { step: 5, tool: 'epistemic_graph_neighborhood' },
    ]);
    assert.deepEqual(guidanceModel.read_routing, [
      { tool: 'epistemic_graph_team_work_overview' },
      { tool: 'epistemic_graph_issue_tree_frontier' },
      { tool: 'epistemic_graph_neighborhood' },
      { tool: 'epistemic_graph_snapshot' },
    ]);
    assert.deepEqual(guidanceModel.payload_transport, {
      accepted_by: ['epistemic_graph_proposal_submit', 'epistemic_graph_submit_review_admit'],
      call_shape: { payload_ref: 'mcp_payload:<payload-id>@v<positive-revision>' },
      exclusive_form: 'Supply payload_ref alone; do not combine it with actor, authority_basis, operations, or another inline proposal field.',
      authority_rule: 'A payload reference is immutable transport, not mutation authority. The target Site binding and graph mutation boundary still validate actor, authority_basis, schema, and operations.',
    });
    assert.equal(guidanceModel.workflow[0].why, undefined);
    assert.equal(guidanceModel.immutable_payload_recovery, undefined);
    assert.equal(guidanceModel.minimal_example, undefined);
    assert.equal(guidanceModel.requested, undefined);
    assert.ok(guidanceResult.content[0].text.length < 2000, `guidance model length=${guidanceResult.content[0].text.length}`);

    const toolsResult = await registered[0].execute('call-tools-result', { value: 'tools_result' }, new AbortController().signal);
    const toolsModel = JSON.parse(toolsResult.content[0].text);
    assert.deepEqual(Object.keys(toolsModel).sort(), ['connection_id', 'schema', 'status', 'surface_id', 'tool_count', 'tools'].sort());
    assert.deepEqual(toolsModel.tools, [
      { name: 'epistemic_graph_guidance' },
      { name: 'epistemic_graph_query' },
      { name: 'epistemic_graph_submit_review_admit' },
    ]);
    assert.equal(toolsModel.tools[0].description, undefined);
    assert.equal(toolsModel.compact, undefined);
    assert.equal(toolsModel.runtime_freshness, undefined);
    assert.equal(toolsModel.runtime_lifecycle, undefined);
    assert.ok(toolsResult.content[0].text.length < 500);

    const loaderToolsResult = await registered[0].execute('call-loader-tools-result', { value: 'loader_tools_result' }, new AbortController().signal);
    assert.deepEqual(JSON.parse(loaderToolsResult.content[0].text), toolsModel);
    const loaderToolsPage = await registered[0].execute('call-loader-tools-page', { value: 'loader_tools_page' }, new AbortController().signal);
    assert.deepEqual(JSON.parse(loaderToolsPage.content[0].text), toolsModel);
    const loaderToolsTruncatedPage = await registered[0].execute('call-loader-tools-truncated-page', { value: 'loader_tools_truncated_page' }, new AbortController().signal);
    const loaderToolsTruncatedModel = JSON.parse(loaderToolsTruncatedPage.content[0].text);
    assert.deepEqual(Object.keys(loaderToolsTruncatedModel).sort(), ['connection_id', 'details_reader', 'details_ref', 'next_offset', 'result_bounded', 'result_summary', 'schema', 'status', 'surface_id', 'transport_next_offset'].sort());
    assert.equal(loaderToolsTruncatedModel.result_summary.schema, 'narada.mcp_loader.tools.v1');
    assert.equal(loaderToolsTruncatedModel.details_ref, 'mcp_output:tools-truncated');
    assert.equal(loaderToolsTruncatedModel.details_reader, 'mcp_loader_read_result');
    assert.equal(loaderToolsTruncatedModel.next_offset, 120);
    assert.doesNotMatch(loaderToolsTruncatedPage.content[0].text, /structuredContent|output_text|epistemic_graph_guidance/);
    assert.ok(loaderToolsTruncatedPage.content[0].text.length < 500);

    const authoritativeJson = await registered[0].execute('call-authoritative-json', { value: 'authoritative_json' }, new AbortController().signal);
    const filesystemRead = await registered[0].execute('call-filesystem-read-result', { value: 'filesystem_read_result' }, new AbortController().signal);
    assert.equal(filesystemRead.content[0].text, 'const filesystemRead = true;\n');
    assert.ok(filesystemRead.details.fullOutputCharLength > 5000, `filesystem read full length=${filesystemRead.details.fullOutputCharLength}`);
    assert.ok(filesystemRead.details.modelVisibleCharLength < 100, `filesystem read model length=${filesystemRead.details.modelVisibleCharLength}`);
    assert.equal(filesystemRead.details.modelVisibleTruncated, false);
    assert.doesNotMatch(filesystemRead.content[0].text, /local\.filesystem\.read\.v1|content_sha256|nested/);
    const writeReceipt = await registered[0].execute('call-write-file', { value: 'write_file' }, new AbortController().signal);
    const writeModel = JSON.parse(writeReceipt.content[0].text);
    assert.deepEqual(Object.keys(writeModel).sort(), ['after_sha256', 'before_sha256', 'relative_path', 'schema', 'sha256', 'size', 'status'].sort());
    assert.equal(writeModel.path, undefined);
    assert.notEqual(registered[0].renderResult(writeReceipt, { expanded: true }).render(160).join('\n'), writeReceipt.content[0].text);

    const submitReceipt = await registered[0].execute('call-submit-review-admit', { value: 'submit_review_admit' }, new AbortController().signal);
    const submitModel = JSON.parse(submitReceipt.content[0].text);
    assert.equal(submitModel.proposal_id, 'proposal-1');
    assert.equal(submitModel.operation_count, 4);
    assert.equal(submitModel.operations, undefined);
    assert.equal(submitModel.review_details, undefined);
    assert.notEqual(registered[0].renderResult(submitReceipt, { expanded: true }).render(160).join('\n'), submitReceipt.content[0].text);
    const nestedSubmitReceipt = await registered[0].execute('call-submit-review-admit-nested', { value: 'submit_review_admit_nested' }, new AbortController().signal);
    const nestedSubmitModel = JSON.parse(nestedSubmitReceipt.content[0].text);
    assert.deepEqual(Object.keys(nestedSubmitModel).sort(), ['admission_status', 'certifies_truth', 'content_fingerprint', 'ledger_head', 'operation_count', 'proposal_digest', 'proposal_id', 'review_gate_preserved', 'review_status', 'schema', 'status'].sort());
    assert.equal(nestedSubmitModel.operations, undefined);
    assert.equal(nestedSubmitModel.review_details, undefined);
    assert.ok(nestedSubmitReceipt.content[0].text.length < 500);


    assert.match(authoritativeJson.content[0].text, /output_ref|reader_tool/);
    assert.equal(authoritativeJson.details.modelVisibleTruncated, false);

    const lease = await registered[0].execute('call-schema-lease', { value: 'schema_lease' }, new AbortController().signal);
    const leaseModel = JSON.parse(lease.content[0].text);
    assert.deepEqual(Object.keys(leaseModel).sort(), ['connection_id', 'input_contract', 'schema', 'schema_lease', 'status', 'surface_id', 'tool_name'].sort());
    assert.equal(leaseModel.binding_resolution, undefined);
    assert.equal(leaseModel.tool_contract, undefined);
    assert.notEqual(registered[0].renderResult(lease, { expanded: true }).render(160).join('\n'), lease.content[0].text);

    const batchLease = await registered[0].execute('call-schema-lease-batch', { value: 'schema_lease_batch' }, new AbortController().signal);
    const batchLeaseModel = JSON.parse(batchLease.content[0].text);
    assert.deepEqual(Object.keys(batchLeaseModel).sort(), ['connection_id', 'lease_count', 'leases', 'schema', 'status', 'surface_handle'].sort());
    assert.deepEqual(Object.keys(batchLeaseModel.leases[0]).sort(), ['input_contract', 'schema_lease', 'tool_name'].sort());
    assert.equal(batchLeaseModel.binding_resolution, undefined);
    assert.equal(batchLeaseModel.leases[0].connection_id, undefined);
    assert.equal(batchLeaseModel.leases[0].tool_contract, undefined);
    assert.ok(batchLease.content[0].text.length < 700);

    const commandExecution = await registered[0].execute('call-structured-command', { value: 'structured_command' }, new AbortController().signal);
    const commandModel = JSON.parse(commandExecution.content[0].text);
    assert.deepEqual(Object.keys(commandModel).sort(), ['execution_ref', 'schema', 'status'].sort());
    assert.equal(commandModel.command, undefined);
    assert.equal(commandModel.args, undefined);
    assert.equal(commandModel.working_directory, undefined);
    assert.equal(commandModel.execution_posture, undefined);
    assert.equal(commandModel.test_scope, undefined);
    assert.ok(commandExecution.content[0].text.length < 180);

    const nestedCommandExecution = await registered[0].execute('call-loader-structured-command-running', { value: 'loader_structured_command_running' }, new AbortController().signal);
    const nestedCommandModel = JSON.parse(nestedCommandExecution.content[0].text);
    assert.deepEqual(Object.keys(nestedCommandModel).sort(), ['execution_ref', 'schema', 'status'].sort());
    assert.equal(nestedCommandModel.execution_ref, 'execution-nested');
    assert.equal(nestedCommandModel.executed, undefined);
    assert.equal(nestedCommandModel.pending, undefined);
    assert.ok(nestedCommandExecution.content[0].text.length < 180);

    const commandPage = await registered[0].execute('call-producer-command-page', { value: 'producer_command_page' }, new AbortController().signal);
    const commandPageModel = JSON.parse(commandPage.content[0].text);
    assert.deepEqual(Object.keys(commandPageModel).sort(), ['executed', 'execution_ref', 'exit_code', 'pending', 'schema', 'status', 'stdout'].sort());
    assert.equal(commandPageModel.command, undefined);
    assert.equal(commandPageModel.args, undefined);
    assert.equal(commandPageModel.execution_posture, undefined);
    assert.equal(commandPageModel.output_ref, undefined);
    assert.equal(commandPageModel.stdout, 'command body');
    assert.ok(commandPage.content[0].text.length < 300);

    const handleOpened = await registered[0].execute('call-surface-handle-opened', { value: 'surface_handle_opened' }, new AbortController().signal);
    const handleModel = JSON.parse(handleOpened.content[0].text);
    assert.deepEqual(Object.keys(handleModel).sort(), ['binding_id', 'generation_id', 'handle_scope', 'handle_survives_child_restart', 'handle_survives_loader_restart', 'runtime_kind', 'schema', 'site_root', 'status', 'surface_handle', 'surface_id', 'tool_count'].sort());
    assert.equal(handleModel.logical_connection_id, undefined);
    assert.equal(handleModel.connection_id, undefined);
    assert.equal(handleModel.ownership, undefined);
    assert.equal(handleModel.call, undefined);
    assert.equal(handleModel.created_at, undefined);
    assert.ok(handleOpened.content[0].text.length < 500);

    const connectionInventory = await registered[0].execute('call-connection-inventory', { value: 'connection_inventory' }, new AbortController().signal);
    const connectionInventoryModel = JSON.parse(connectionInventory.content[0].text);
    assert.deepEqual(Object.keys(connectionInventoryModel).sort(), ['available_slots', 'connection_count', 'connections', 'live_count', 'schema', 'status'].sort());
    assert.equal(connectionInventoryModel.compact, undefined);
    assert.equal(connectionInventoryModel.max_connections, undefined);
    assert.equal(connectionInventoryModel.closed_count, undefined);
    assert.equal(connectionInventoryModel.live_connection_ids, undefined);
    assert.equal(connectionInventoryModel.connections[0].age_ms, undefined);
    assert.equal(connectionInventoryModel.connections[0].ownership, undefined);
    assert.equal(connectionInventoryModel.connections[0].actions, undefined);
    assert.equal(connectionInventoryModel.connections[0].pending_request_count, undefined);
    assert.equal(connectionInventoryModel.connections[1].pending_request_count, 1);
    assert.equal(connectionInventoryModel.connections[0].restart_owner, 'mcp-loader');
    assert.equal(connectionInventoryModel.connections[1].restart_owner, 'carrier-supervisor');
    assert.equal(connectionInventoryModel.runtime_freshness, undefined);
    assert.ok(connectionInventory.content[0].text.length < 700);

    const siteSurfaces = await registered[0].execute('call-site-surfaces', { value: 'site_surfaces' }, new AbortController().signal);
    const siteSurfacesModel = JSON.parse(siteSurfaces.content[0].text);
    assert.deepEqual(Object.keys(siteSurfacesModel).sort(), ['schema', 'site_root', 'status', 'surface_count', 'surfaces'].sort());
    assert.deepEqual(siteSurfacesModel.surfaces, [{ binding_id: 'repo-git', surface_id: 'git' }, { binding_id: 'repo-worker', surface_id: 'worker' }]);
    assert.equal(siteSurfacesModel.compact, undefined);
    assert.equal(siteSurfacesModel.surfaces[0].server_name, undefined);
    assert.equal(siteSurfacesModel.surfaces[1].runtime_requirements, undefined);
    assert.equal(siteSurfacesModel.surfaces[0].next_call, undefined);
    assert.ok(siteSurfaces.content[0].text.length < 350);

    const runtimeFreshness = await registered[0].execute('call-runtime-freshness', { value: 'runtime_freshness' }, new AbortController().signal);
    const runtimeModel = JSON.parse(runtimeFreshness.content[0].text);
    assert.deepEqual(Object.keys(runtimeModel).sort(), ['reload_required', 'schema', 'status'].sort());
    assert.equal(runtimeModel.runtime_entrypoint, undefined);
    assert.equal(runtimeModel.source_entrypoint, undefined);
    assert.equal(runtimeModel.reload_action, undefined);
    assert.ok(runtimeFreshness.content[0].text.length < 180);

    const teamOverview = await registered[0].execute('call-team-work-overview', { value: 'team_work_overview' }, new AbortController().signal);
    const teamModel = JSON.parse(teamOverview.content[0].text);
    assert.deepEqual(Object.keys(teamModel).sort(), ['coverage', 'has_more', 'items', 'limit', 'mode', 'next_cursor', 'returned', 'schema', 'semantics', 'status'].sort());
    assert.equal(teamModel.query_origin, undefined);
    assert.equal(teamModel.template, undefined);
    assert.equal(teamModel.ledger_head, undefined);
    assert.equal(teamModel.coverage.queried_members, undefined);
    assert.equal(teamModel.items[0].noisy, undefined);
    assert.equal(teamModel.items[0].live_presence.reason, undefined);
    assert.deepEqual(teamModel.items[0].freshness, { classification: 'fresh', rule: 'bounded' });
    assert.ok(teamOverview.content[0].text.length < 1200);

    const gitStatus = await registered[0].execute('call-git-status', { value: 'git_status' }, new AbortController().signal);
    const gitStatusModel = JSON.parse(gitStatus.content[0].text);
    assert.deepEqual(Object.keys(gitStatusModel).sort(), ['ahead', 'behind', 'branch', 'clean', 'conflicts', 'repository_root', 'schema', 'staged', 'status', 'summary', 'untracked', 'unstaged', 'upstream'].sort());
    assert.equal(gitStatusModel.working_directory, undefined);
    assert.equal(gitStatusModel.status_entries, undefined);
    assert.equal(gitStatusModel.remotes, undefined);
    assert.equal(gitStatusModel.push_target, undefined);
    assert.ok(gitStatus.content[0].text.length < 700);

    const gitAdd = await registered[0].execute('call-git-add', { value: 'git_add' }, new AbortController().signal);
    const gitAddModel = JSON.parse(gitAdd.content[0].text);
    assert.deepEqual(Object.keys(gitAddModel).sort(), ['operation', 'paths', 'post_status', 'schema', 'status', 'summary', 'work_scope_ref'].sort());
    assert.equal(gitAddModel.output, undefined);
    assert.equal(gitAddModel.post_status.remotes, undefined);
    assert.ok(gitAdd.content[0].text.length < 900);

    const gitCommit = await registered[0].execute('call-git-commit', { value: 'git_commit' }, new AbortController().signal);
    const gitCommitModel = JSON.parse(gitCommit.content[0].text);
    assert.deepEqual(Object.keys(gitCommitModel).sort(), ['commit', 'committed_file_count', 'committed_files', 'post_status', 'schema', 'status', 'summary', 'work_scope_ref'].sort());
    assert.equal(gitCommitModel.committed_entries, undefined);
    assert.equal(gitCommitModel.output, undefined);
    assert.ok(gitCommit.content[0].text.length < 700);

    const gitPush = await registered[0].execute('call-git-push', { value: 'git_push' }, new AbortController().signal);
    const gitPushModel = JSON.parse(gitPush.content[0].text);
    assert.deepEqual(Object.keys(gitPushModel).sort(), ['branch', 'commit', 'remote', 'schema', 'status', 'summary', 'work_scope_ref'].sort());
    assert.equal(gitPushModel.output, undefined);
    assert.equal(gitPushModel.post_status, undefined);
    assert.ok(gitPush.content[0].text.length < 300);

    const attached = await registered[0].execute('call-surface-attached', { value: 'surface_attached' }, new AbortController().signal);
    const attachedModel = JSON.parse(attached.content[0].text);
    assert.deepEqual(Object.keys(attachedModel).sort(), ['binding_id', 'connection_id', 'generation_id', 'logical_connection_id', 'runtime_freshness', 'runtime_kind', 'runtime_lifecycle', 'schema', 'server_info', 'site_root', 'status', 'surface_id', 'tool_count', 'tool_discovery', 'tool_inspection'].sort());
    assert.deepEqual(attachedModel.server_info, { name: 'git-mcp', version: '0.1.0' });
    assert.deepEqual(attachedModel.tool_discovery, { tool_name: 'mcp_loader_list_tools' });
    assert.deepEqual(attachedModel.tool_inspection, { required_arguments: ['connection_id', 'tool_name'] });
    assert.deepEqual(attachedModel.runtime_lifecycle, { managed_by: 'mcp-loader', restartable: true, restartability_status: 'available', restart_scope: 'attached_child_process', session_restart_required: false });
    assert.deepEqual(attachedModel.runtime_freshness, { status: 'current', reload_required: false });
    assert.equal(attachedModel.entrypoint, undefined);
    assert.equal(attachedModel.runtime_command, undefined);
    assert.equal(attachedModel.args, undefined);
    assert.equal(attachedModel.ownership, undefined);
    assert.ok(attached.content[0].text.length < 1200);

    const inventory = await registered[0].execute('call-inventory', { value: 'inventory' }, new AbortController().signal);
    const inventoryModel = JSON.parse(inventory.content[0].text);
    assert.equal(inventoryModel.observed_tools, undefined);
    assert.equal(inventoryModel.runtime_freshness, undefined);
    assert.deepEqual(inventoryModel.findings[0].missing_from_fabric, ['tool-a']);
    assert.deepEqual(inventoryModel.findings[0].extra_in_fabric, ['tool-z']);
    assert.notEqual(registered[0].renderResult(inventory, { expanded: true }).render(160).join('\n'), inventory.content[0].text);

    for (const [value, answer] of [
      ['producer_page', 'only child output'],
      ['loader_page', 'only nested child output'],
      ['loader_result', 'only unwrapped child result'],
      ['mcp_page', 'only mcp output'],
      ['worker_page', 'only worker output'],
    ]) {
      const page = await registered[0].execute(`call-${value}`, { value }, new AbortController().signal);
      assert.deepEqual(JSON.parse(page.content[0].text), { schema: 'child.result.v1', answer });
      assert.doesNotMatch(page.content[0].text, /output_ref|reader_tool|full_output_char_length|result_summary/);
      assert.equal(page.details.modelVisibleTruncated, false);
      assert.ok(page.details.fullOutputCharLength > page.details.modelVisibleCharLength);
      const pageFull = registered[0].renderResult(page, { expanded: true }).render(160).join('\n');
      assert.notEqual(pageFull, page.content[0].text);
    }
    const smallCollapsed = registered[0].renderResult(result, { expanded: false }).render(160).join('\n');
    assert.match(smallCollapsed, /MCP result.*model-visible.*f8: single-line/);
    assert.doesNotMatch(smallCollapsed, /schema_lease/);
    const smallExpanded = registered[0].renderResult(result, { expanded: true }).render(160).join('\n');
    assert.match(smallExpanded, /^full-output · \d+ characters — f8: single-line/);
    assert.equal(result.details.fullOutputCharLength, result.details.modelVisibleCharLength);
    assert.equal(result.details.modelVisibleTruncated, false);

    const shortcut = shortcuts.get('f8');
    assert.match(shortcut.description, /compact, single-line, model-visible, full-output, hide/);
    const expansionStates = [];
    await shortcut.handler({ ui: { setToolsExpanded(value) { expansionStates.push(value); } } });
    const singleLine = registered[0].renderResult(result, { expanded: false }).render(160).join('\n');
    assert.match(singleLine, /MCP result.*model-visible.*f8: model-visible/);
    assert.deepEqual(expansionStates, [true, false]);
    const singleLineCall = registered[0].renderCall({}, {
      fg(_kind, text) { return text; },
      bold(text) { return text; },
    }, {}).render(160);
    assert.deepEqual(singleLineCall, []);

    await shortcut.handler({ ui: { setToolsExpanded(value) { expansionStates.push(value); } } });
    const modelVisible = registered[0].renderResult(result, { expanded: false }).render(160).join('\n');
    assert.match(modelVisible, /^model-visible · \d+ characters/);
    assert.match(modelVisible, /schema_lease/);
    assert.deepEqual(expansionStates, [true, false, true, false]);

    await shortcut.handler({ ui: { setToolsExpanded(value) { expansionStates.push(value); } } });
    const fullOutput = registered[0].renderResult(result, { expanded: false }).render(160).join('\n');
    assert.match(fullOutput, /^full-output · \d+ characters/);
    assert.match(fullOutput, /schema_lease/);
    assert.deepEqual(expansionStates, [true, false, true, false, true]);

    await shortcut.handler({ ui: { setToolsExpanded(value) { expansionStates.push(value); } } });
    assert.deepEqual(registered[0].renderResult(result, { expanded: false }).render(160), []);
    assert.deepEqual(expansionStates, [true, false, true, false, true, false]);
    const hiddenCall = registered[0].renderCall({}, {
      fg(_kind, text) { return text; },
      bold(text) { return text; },
    }, {}).render(160);
    assert.deepEqual(hiddenCall, []);

    await shortcut.handler({ ui: { setToolsExpanded(value) { expansionStates.push(value); } } });
    const compactView = registered[0].renderResult(result, { expanded: false }).render(160).join('\n');
    assert.match(compactView, /f8: single-line/);
    assert.deepEqual(expansionStates, [true, false, true, false, true, false, true, false]);
    const compactCall = registered[0].renderCall({}, {
      fg(_kind, text) { return text; },
      bold(text) { return text; },
    }, {}).render(160);
    assert.deepEqual(compactCall, ['fixture_echo']);
    const themedCollapsed = registered[0].renderResult(result, { expanded: false }, {
      fg(kind, text) { return `<${kind}>${text}</${kind}>`; },
    }).render(160).join('\n');
    assert.match(themedCollapsed, /<muted>MCP result \(.*\) · model-visible .* — f8: single-line<\/muted>/);
    const ansiCollapsedLines = registered[0].renderResult(result, { expanded: false }, {
      fg(_kind, text) { return `\x1b[90m${text}\x1b[39m`; },
    }).render(7);
    assert.doesNotMatch(ansiCollapsedLines.join('\n'), /(?:^|\n)\[(?:39|90)m|(?:^|\n)m(?:$|\n)/);
    assert.equal((ansiCollapsedLines.join('').match(/\x1b\[[0-?]*[ -/]*[@-~]/g) ?? []).length, 2);

    const issueTree = await registered[0].execute('call-issue-tree', { value: 'issue_tree' }, new AbortController().signal);
    const authoritativeIssueTree = JSON.parse(issueTree.content[0].text);
    assert.equal(authoritativeIssueTree.selected.node_id, 'issue:selected');
    assert.deepEqual(authoritativeIssueTree.frontier.items.map((item) => item.node_id), ['issue:selected', 'issue:open']);
    assert.equal(authoritativeIssueTree.frontier.complete, true);
    const issueTreeCollapsed = registered[0].renderResult(issueTree, { expanded: false }).render(160).join('\n');
    assert.match(issueTreeCollapsed, /issue tree · issue:selected selected · 1 open leaf · version 3/);
    assert.ok(issueTreeCollapsed.length < 160);
    const issueTreeExpanded = registered[0].renderResult(issueTree, { expanded: true }).render(1000).join('\n');
    const expandedIssueTree = JSON.parse(issueTreeExpanded.split('\n').slice(1).join('\n'));
    assert.deepEqual(expandedIssueTree.selected, authoritativeIssueTree.selected);
    assert.deepEqual(expandedIssueTree.frontier, authoritativeIssueTree.frontier);
    assert.equal(expandedIssueTree.noncertification, 'coordination state; not evidence');

    for (const [value, expected] of [
      ['stat', /file file\.md · 14k bytes/],
      ['empty_range', /range 300–380 is past EOF; file\.md has 250 lines/],
      ['empty_valid_range', /no lines returned from 200–200; file\.md has 250 lines/],
      ['replace', /replaced 1 occurrence in file\.md/],
      ['bridge', /planned · 0 envelopes/],
      ['generic_large', /MCP loader result page · 4\.2k characters/],
    ]) {
      const fixture = await registered[0].execute('call-' + value, { value }, new AbortController().signal);
      const rendered = registered[0].renderResult(fixture, { expanded: false }).render(160).join('\n');
      assert.match(rendered, expected);
      assert.match(rendered, /f8: single-line/);
    }

    const oversized = await registered[0].execute('call-oversized', { value: 'oversized' }, new AbortController().signal);
    assert.ok(oversized.content[0].text.length < 1000);
    assert.equal(JSON.parse(oversized.content[0].text).model_visible_truncated, true);
    assert.doesNotMatch(oversized.content[0].text, /x{100}/);
    const oversizedExpanded = registered[0].renderResult(oversized, { expanded: true }).render(160).join('\n');
    assert.match(oversizedExpanded, /x{100}/);
    await shortcut.handler({ ui: { setToolsExpanded() {} } });
    const oversizedSingleLine = registered[0].renderResult(oversized, { expanded: false }).render(160).join('\n');
    assert.match(oversizedSingleLine, /model-visible.*f8: model-visible/);
    assert.doesNotMatch(oversizedSingleLine, /x{100}/);
    await shortcut.handler({ ui: { setToolsExpanded() {} } });
    const oversizedModelVisible = registered[0].renderResult(oversized, { expanded: false }).render(160).join('\n');
    assert.match(oversizedModelVisible, /^model-visible · [0-9.]+[kmb]? characters/);
    assert.doesNotMatch(oversizedModelVisible, /x{100}/);
    await shortcut.handler({ ui: { setToolsExpanded() {} } });
    const oversizedFullOutput = registered[0].renderResult(oversized, { expanded: false }).render(160).join('\n');
    assert.match(oversizedFullOutput, /^full-output · [0-9.]+[kmb]? characters/);
    assert.match(oversizedFullOutput, /x{100}/);
    await shortcut.handler({ ui: { setToolsExpanded() {} } });
    assert.deepEqual(registered[0].renderResult(oversized, { expanded: false }).render(160), []);
    await shortcut.handler({ ui: { setToolsExpanded() {} } });

    const largeResult = {
      content: [{ type: 'text', text: 'x'.repeat(5000) }],
      details: { uiSummary: 'fixture_echo: large fixture' },
    };
    const collapsed = registered[0].renderResult(largeResult, { expanded: false });
    assert.match(collapsed.render(120).join('\n'), /large fixture.*model-visible.*f8: single-line/);
    assert.doesNotMatch(collapsed.render(120).join('\n'), /x{100}/);
    const expanded = registered[0].renderResult(largeResult, { expanded: true });
    assert.match(expanded.render(120).join('\n'), /x{100}/);

  } finally {
    await shutdown?.();
    await rm(root, { recursive: true, force: true });
  }
});

test('generated Pi extension respects the epistemic inbox query limit', async () => {
  const source = await readFile(templatePath, 'utf8');
  assert.match(source, /MARICI_INBOX_PAGE_SIZE = 100/);
  assert.match(source, /limit: MARICI_INBOX_PAGE_SIZE/);
  assert.match(source, /MARICI_INBOX_MAX_PAGES = 41/);
});

test('generated Pi extension qualifies flat-namespace collisions deterministically', async () => {
  const source = await readFile(templatePath, 'utf8');
  assert.match(source, /serverPrefix.*replace/);
  assert.match(source, /serverPrefix}__\$\{tool\.name/);
  assert.match(source, /qualified tool name collision/);
});

test('generated Pi extension eagerly bootstraps only mcp-loader', async () => {
  const source = await readFile(templatePath, 'utf8');
  assert.match(source, /config\.name === "mcp-loader"/);
  assert.match(source, /SERVERS\.filter\(shouldBootstrapServer\)/);
});

test('generated Pi extension routes Git away from structured-command', async () => {
  const source = await readFile(templatePath, 'utf8');
  assert.match(source, /Git is not a structured-command fallback/);
  assert.match(source, /activate <site-id>-git/);
});

test('generated Pi extension does not eagerly bootstrap task lifecycle', async () => {
  const root = await mkdtemp(join(tmpdir(), 'narada-pi-lifecycle-projection-'));
  try {
    const serverPath = join(root, 'server.mjs');
    await writeFile(serverPath, `
import { createInterface } from "node:readline";
createInterface({ input: process.stdin }).on("line", (line) => {
  const message = JSON.parse(line);
  if (message.method === "initialize") {
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { protocolVersion: "2026-07-28", capabilities: { tools: {} }, serverInfo: { name: "task-lifecycle", version: "1" } } }) + "\\n");
  } else if (message.method === "tools/list") {
    const tools = [
      { name: "task_lifecycle_bridge_poll", inputSchema: { type: "object", properties: {} } },
      ...Array.from({ length: 69 }, (_, index) => ({ name: "task_lifecycle_other_" + index, inputSchema: { type: "object", properties: {} } })),
    ];
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { tools } }) + "\\n");
  }
});
`, 'utf8');
    const source = await materializedTemplate([{ name: 'task-lifecycle', command: process.execPath, args: [serverPath], enabled: true, startupTimeoutMs: 2000 }]);
    const extensionPath = join(root, 'index.ts');
    await writeFile(extensionPath, source, 'utf8');
    const extension = (await import(`${pathToFileURL(extensionPath).href}?projection=1`)).default;
    const handlers = new Map();
    const registered = [];
    const notifications = [];
    extension({
      events: { on() {}, off() {} },
      on(name, handler) { handlers.set(name, handler); },
      getAllTools() { return []; },
      registerCommand() {},
      registerShortcut() {},
      registerTool(tool) { registered.push(tool); },
    });
    await assert.rejects(
      handlers.get('session_start')?.({}, { ui: { notify(message, level) { notifications.push({ message, level }); } } }),
      /No admitted MCP server completed startup/,
    );
    assert.match(notifications[0].message, /mcp=loading/);
    assert.match(notifications[1].message, /mcp=failed/);
    assert.match(notifications[1].message, /restart_owner=session_then_carrier_or_runtime_supervisor/);
    assert.match(notifications[1].message, /retry the session restart/);
    assert.deepEqual(registered.map((tool) => tool.name), []);
    await handlers.get('session_shutdown')?.();
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('generated Pi extension refuses bootstrap schema growth beyond its hard budget', async () => {
  const root = await mkdtemp(join(tmpdir(), 'narada-pi-budget-'));
  try {
    const serverPath = join(root, 'server.mjs');
    await writeFile(serverPath, `
import { createInterface } from "node:readline";
createInterface({ input: process.stdin }).on("line", (line) => {
  const message = JSON.parse(line);
  if (message.method === "initialize") {
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { protocolVersion: "2026-07-28", capabilities: { tools: {} }, serverInfo: { name: "fixture", version: "1" } } }) + "\\n");
  } else if (message.method === "tools/list") {
    const tools = Array.from({ length: 81 }, (_, index) => ({ name: "tool_" + index, inputSchema: { type: "object", properties: {} } }));
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { tools } }) + "\\n");
  }
});
`, 'utf8');
    const source = await materializedTemplate([{ name: 'mcp-loader', command: process.execPath, args: [serverPath], enabled: true, startupTimeoutMs: 2000 }]);
    const extensionPath = join(root, 'index.ts');
    await writeFile(extensionPath, source, 'utf8');
    const extension = (await import(`${pathToFileURL(extensionPath).href}?budget=1`)).default;
    const handlers = new Map();
    const registered = [];
    extension({
      events: { on() {}, off() {} },
      on(name, handler) { handlers.set(name, handler); },
      getAllTools() { return []; },
      registerCommand() {},
      registerShortcut() {},
      registerTool(tool) { registered.push(tool); },
    });
    await assert.rejects(
      handlers.get('session_start')?.({}, { ui: { notify() {} } }),
      /narada_pi_bootstrap_context_budget_exceeded/,
    );
    assert.equal(registered.length, 0);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
