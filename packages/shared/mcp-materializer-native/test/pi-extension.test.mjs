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
      process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { structuredContent: { result: { structuredContent: { schema: "narada.epistemic.query.v1", items: [{ event_id: "ev-000000000012-a" }, { event_id: "ev-000000000015-b" }] } } } } }) + "\\n");
      return;
    }
    const fixtures = {
      stat: { schema: "local.filesystem.stat.v1", path: "C:/repo/file.md", relative_path: "file.md", type: "file", size: 13558 },
      empty_range: { schema: "local.filesystem.read.v1", relative_path: "file.md", total_lines: 250, returned_lines: 0, offset: 300, requested_start_line: 300, requested_end_line: 380 },
      empty_valid_range: { schema: "local.filesystem.read.v1", relative_path: "file.md", total_lines: 250, returned_lines: 0, offset: 200, requested_start_line: 200, requested_end_line: 200 },
      replace: { schema: "local.filesystem.str_replace_file.v1", status: "replaced", relative_path: "file.md", occurrences: 1 },
      bridge: { schema: "narada.task.inbox.bridge.v1", status: "planned", count: 0, envelopes: [] },
      generic_large: (() => {
        const value = { schema: "narada.mcp_loader.result_page.v1", full_output_char_length: 4227, payload: "" };
        value.payload = "x".repeat(4227 - JSON.stringify(value).length);
        return value;
      })(),
      issue_tree: { schema: "narada.epistemic.issue-tree.resume.v1", status: "ok", tree: { tree_id: "tree:ax", objective: "AX", version: "3" }, selected: { node_id: "issue:selected", version: "2", title: "Selected", state: "selected", score: 0.9 }, frontier: { items: [{ node_id: "issue:selected", state: "selected", score: 0.9 }, { node_id: "issue:open", state: "open", score: 0.8 }], returned: 2, complete: true, total: 2, total_exact: true }, continuation: null, certifies_truth: false, noncertification: "coordination state; not evidence" },
      producer_page: { schema: "narada.producer_output_page.v1", status: "ok", output_ref: "mcp_output:fixture", reader_tool: "mcp_loader_read_result", full_output_char_length: 1234, output_text: JSON.stringify({ schema: "child.result.v1", answer: "only child output" }) },
      loader_page: { schema: "narada.mcp_loader.tool_result.v1", status: "ok", result_summary: { schema: "child.result.v1", status: "ok" }, result: { schema: "narada.producer_output_page.v1", status: "ok", output_ref: "mcp_output:nested", reader_tool: "mcp_loader_read_result", full_output_char_length: 1234, output_text: JSON.stringify({ schema: "child.result.v1", answer: "only nested child output" }) } },
      loader_result: { schema: "narada.mcp_loader.tool_result.v1", connection_id: "c1", surface_id: "s1", result_summary: { schema: "child.result.v1", status: "ok" }, result: { schema: "child.result.v1", answer: "only unwrapped child result" } },
      schema_lease_compact: { schema: "narada.mcp_loader.schema_lease.v1", status: "issued", connection_id: "c-compact", logical_connection_id: "logical-compact", generation_id: "generation-compact", surface_id: "surface-compact", tool_name: "surface_read", tool_schema_digest: "tool-digest", tool_contract_digest: "contract-digest", input_schema_digest: "input-digest", output_schema_digest: "output-digest", description: "Read the surface", annotations: { readOnlyHint: true }, argument_skeleton: { site_root: "x" }, minimal_valid_arguments: { site_root: "x" }, minimal_valid_arguments_status: "validated", verbose_contract_call: { tool_name: "mcp_loader_inspect_tool" }, schema_lease: "lease-compact", lease_scope: "loader_process_child_generation", transferable: false, authorization_resolution: "lease_renewed", input_contract: { type: "object", required: ["site_root"], properties: { site_root: { type: "string" } } } },
      schema_lease_verbose: { schema: "narada.mcp_loader.schema_lease.v1", status: "issued", connection_id: "c-verbose", logical_connection_id: "logical-verbose", generation_id: "generation-verbose", surface_id: "surface-verbose", tool_name: "surface_write", tool_schema_digest: "verbose-tool-digest", tool_contract_digest: "verbose-contract-digest", input_schema_digest: "verbose-input-digest", output_schema_digest: "verbose-output-digest", description: "Write the surface", annotations: { readOnlyHint: false }, argument_skeleton: { value: "x" }, minimal_valid_arguments: { value: "x" }, minimal_valid_arguments_status: "validated", verbose_contract_call: { tool_name: "mcp_loader_inspect_tool" }, schema_lease: "lease-verbose", lease_scope: "loader_process_child_generation", transferable: false, authorization_resolution: "lease_renewed", tool_contract: { name: "surface_write", description: "Write the surface", inputSchema: { type: "object", required: ["value"], properties: { value: { type: "string" } } }, annotations: { readOnlyHint: false } } },
      site_inventory: (() => {
        const observed = Array.from({ length: 600 }, (_, index) => "surface_tool_" + index);
        return { schema: "narada.mcp_loader.site_tool_inventory_check.v1", status: "drift", site_root: "C:/repo", observed_at: "2026-09-01T00:00:00Z", requested_surface_ids: null, runtime_kind: "codex", attempted_surface_ids: ["surface-a", "surface-b"], observed_surface_ids: ["surface-a", "surface-b"], unobserved_surface_ids: [], runtime_skipped_surface_ids: [], runtime_skipped_count: 0, observation_coverage: "complete", checked_surface_count: 2, violation_count: 1, observed_tools: { "surface-a": observed, "surface-b": observed }, observed_read_only_tools: { "surface-a": observed }, observed_mutating_tools: { "surface-b": observed }, observed_unclassified_tools: { "surface-a": [] }, finding_status_counts: { drift: 1, ok: 1 }, findings: [{ surface_id: "surface-a", status: "drift", declared_count: 2, observed_count: 2, missing_from_fabric: ["surface_tool_new"], extra_in_fabric: ["surface_tool_old"], duplicate_declared_tools: [], duplicate_observed_tools: ["surface_tool_old"], unclassified_observed_tools: [] }, { surface_id: "surface-b", status: "ok", declared_count: 1, observed_count: 1, missing_from_fabric: [], extra_in_fabric: [], duplicate_declared_tools: [], duplicate_observed_tools: [], unclassified_observed_tools: [] }], observation_ref: "mcp_output:inventory", observation_sha256: "inventory-digest", observation_byte_size: 50000, observation_retention: { owner: "mcp-loader", lifecycle: "temporary" } };
      })(),
      mcp_page: { schema: "narada.mcp_output_page.v1", status: "ok", ref: "mcp_output:page", path: ".ai/tmp/mcp-outputs/workspace/page.json", full_output_char_length: 1234, output_text: JSON.stringify({ schema: "child.result.v1", answer: "only mcp output" }) },
      worker_page: { schema: "narada.worker.output_page.v1", status: "ok", ref: "worker_output:page", path: "worker.json", output_text: JSON.stringify({ schema: "child.result.v1", answer: "only worker output" }) },
      authoritative_json: { schema: "local.filesystem.read.v1", content_delivery: { format: "filesystem_read_text", channel: "content" }, relative_path: "file.json" },
      schema_lease: { schema: "narada.mcp_loader.schema_lease.v1", status: "issued", connection_id: "c1", surface_id: "git", tool_name: "git_status", schema_lease: "lease-1", tool_schema_digest: "digest-1", input_schema_digest: "input-digest", binding_resolution: { surface_handle: "handle", runtime_lifecycle: { noisy: true } }, input_contract: { required: [], properties: ["working_directory"] }, tool_contract: { name: "git_status", inputSchema: { type: "object" } } },
      inventory: { schema: "narada.mcp_loader.site_tool_inventory_check.v1", status: "drift", observation_coverage: "partial", checked_surface_count: 3, violation_count: 1, finding_status_counts: { drift: 1 }, observation_ref: "mcp_payload:inventory", observed_tools: { git: ["tool-a", "tool-b"], worker: ["tool-c"] }, runtime_freshness: { noisy: true }, findings: [{ surface_id: "git", status: "drift", declared_count: 10, observed_count: 8, missing_from_fabric: ["tool-a"], extra_in_fabric: ["tool-z"], duplicate_declared_tools: [], duplicate_observed_tools: [], unclassified_observed_tools: [] }] },
      oversized: { schema: "narada.epistemic.query.v2", status: "ok", payload: "x".repeat(21000) },
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
    const authoritativeJson = await registered[0].execute('call-authoritative-json', { value: 'authoritative_json' }, new AbortController().signal);
    assert.match(authoritativeJson.content[0].text, /output_ref|reader_tool/);
    assert.equal(authoritativeJson.details.modelVisibleTruncated, false);

    const lease = await registered[0].execute('call-schema-lease', { value: 'schema_lease' }, new AbortController().signal);
    const leaseModel = JSON.parse(lease.content[0].text);
    assert.deepEqual(Object.keys(leaseModel).sort(), ['connection_id', 'input_contract', 'schema', 'schema_lease', 'status', 'surface_id', 'tool_name'].sort());
    assert.equal(leaseModel.binding_resolution, undefined);
    assert.equal(leaseModel.tool_contract, undefined);
    assert.notEqual(registered[0].renderResult(lease, { expanded: true }).render(160).join('\n'), lease.content[0].text);

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
    assert.match(smallCollapsed, /MCP result.*model-visible.*f8: model-visible/);
    assert.doesNotMatch(smallCollapsed, /schema_lease/);
    assert.equal(result.details.fullOutputCharLength, result.details.modelVisibleCharLength);
    assert.equal(result.details.modelVisibleTruncated, false);

    const shortcut = shortcuts.get('f8');
    assert.match(shortcut.description, /compact, model-visible, full-output/);
    const expansionStates = [];
    await shortcut.handler({ ui: { setToolsExpanded(value) { expansionStates.push(value); } } });
    const modelVisible = registered[0].renderResult(result, { expanded: false }).render(160).join('\n');
    assert.match(modelVisible, /^model-visible · \d+ characters/);
    assert.match(modelVisible, /schema_lease/);
    assert.deepEqual(expansionStates, [true, false]);

    await shortcut.handler({ ui: { setToolsExpanded(value) { expansionStates.push(value); } } });
    const fullOutput = registered[0].renderResult(result, { expanded: false }).render(160).join('\n');
    assert.match(fullOutput, /^full-output · \d+ characters/);
    assert.match(fullOutput, /schema_lease/);
    assert.deepEqual(expansionStates, [true, false, true]);

    await shortcut.handler({ ui: { setToolsExpanded(value) { expansionStates.push(value); } } });
    assert.match(registered[0].renderResult(result, { expanded: false }).render(160).join('\n'), /f8: model-visible/);
    assert.deepEqual(expansionStates, [true, false, true, false]);
    const themedCollapsed = registered[0].renderResult(result, { expanded: false }, {
      fg(kind, text) { return `<${kind}>${text}</${kind}>`; },
    }).render(160).join('\n');
    assert.match(themedCollapsed, /<muted>MCP result \(.*\) · model-visible .* — f8: model-visible<\/muted>/);
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
      assert.match(rendered, /f8: model-visible/);
    }

    const oversized = await registered[0].execute('call-oversized', { value: 'oversized' }, new AbortController().signal);
    assert.ok(oversized.content[0].text.length < 1000);
    assert.equal(JSON.parse(oversized.content[0].text).model_visible_truncated, true);
    assert.doesNotMatch(oversized.content[0].text, /x{100}/);
    const oversizedExpanded = registered[0].renderResult(oversized, { expanded: true }).render(160).join('\n');
    assert.match(oversizedExpanded, /x{100}/);
    await shortcut.handler({ ui: { setToolsExpanded() {} } });
    const oversizedModelVisible = registered[0].renderResult(oversized, { expanded: false }).render(160).join('\n');
    assert.match(oversizedModelVisible, /^model-visible · [0-9.]+[kmb]? characters/);
    assert.doesNotMatch(oversizedModelVisible, /x{100}/);
    await shortcut.handler({ ui: { setToolsExpanded() {} } });
    const oversizedFullOutput = registered[0].renderResult(oversized, { expanded: false }).render(160).join('\n');
    assert.match(oversizedFullOutput, /^full-output · [0-9.]+[kmb]? characters/);
    assert.match(oversizedFullOutput, /x{100}/);
    await shortcut.handler({ ui: { setToolsExpanded() {} } });

    const largeResult = {
      content: [{ type: 'text', text: 'x'.repeat(5000) }],
      details: { uiSummary: 'fixture_echo: large fixture' },
    };
    const collapsed = registered[0].renderResult(largeResult, { expanded: false });
    assert.match(collapsed.render(120).join('\n'), /large fixture.*model-visible.*f8: model-visible/);
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
