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
  assert.equal(presentation.collapseMcpResultByDefault(), true);
});

test('generated Pi extension handshakes, registers, calls, and closes admitted MCP server', async () => {
  const root = await mkdtemp(join(tmpdir(), 'narada-pi-extension-'));
  let shutdown;
  try {
    const serverPath = join(root, 'server.mjs');
    await writeFile(serverPath, `
import { createInterface } from "node:readline";
createInterface({ input: process.stdin }).on("line", (line) => {
  const message = JSON.parse(line);
  if (message.method === "initialize") {
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { protocolVersion: "2026-07-28", capabilities: { tools: {} }, serverInfo: { name: "fixture", version: "1" } } }) + "\\n");
  } else if (message.method === "tools/list") {
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { tools: [{ name: "fixture_echo", description: "Echo", inputSchema: { type: "object", properties: { value: { type: "string" } }, required: ["value"] } }] } }) + "\\n");
  } else if (message.method === "tools/call") {
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
      oversized: { schema: "narada.epistemic.query.v2", status: "ok", payload: "x".repeat(21000) },
    };
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: {
      content: [{ type: "text", text: "summary without lease" }],
      structuredContent: fixtures[message.params.arguments.value] ?? { value: message.params.arguments.value, schema_lease: "lease-fixture" }
    } }) + "\\n");
  }
});
`, 'utf8');

    const servers = [{
      name: 'fixture',
      command: process.execPath,
      args: [serverPath],
      enabled: true,
      startupTimeoutMs: 2000,
    }];
    const source = await materializedTemplate(servers);
    const extensionPath = join(root, 'index.ts');
    await writeFile(extensionPath, source, 'utf8');
    const extension = (await import(pathToFileURL(extensionPath).href)).default;

    const handlers = new Map();
    const registered = [];
    const pi = {
      on(name, handler) {
        handlers.set(name, handler);
      },
      getAllTools() {
        return [];
      },
      registerTool(tool) {
        registered.push(tool);
      },
    };
    extension(pi);
    shutdown = () => handlers.get('session_shutdown')?.();
    await handlers.get('session_start')?.({}, { ui: { notify() {} } });

    assert.equal(registered.length, 1);
    assert.equal(registered[0].name, 'fixture_echo');
    assert.deepEqual(registered[0].parameters.required, ['value']);
    const result = await registered[0].execute('call-1', { value: 'hello' }, new AbortController().signal);
    assert.deepEqual(JSON.parse(result.content[0].text), {
      value: 'hello',
      schema_lease: 'lease-fixture',
    });
    assert.doesNotMatch(result.content[0].text, /summary without lease/);
    const smallCollapsed = registered[0].renderResult(result, { expanded: false }).render(160).join('\n');
    assert.match(smallCollapsed, /MCP result.*Ctrl\+O to expand/);
    assert.doesNotMatch(smallCollapsed, /schema_lease/);

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
      assert.match(rendered, /Ctrl\+O to expand/);
    }

    const oversized = await registered[0].execute('call-oversized', { value: 'oversized' }, new AbortController().signal);
    assert.ok(oversized.content[0].text.length < 1000);
    assert.equal(JSON.parse(oversized.content[0].text).model_visible_truncated, true);
    assert.doesNotMatch(oversized.content[0].text, /x{100}/);
    const oversizedExpanded = registered[0].renderResult(oversized, { expanded: true }).render(160).join('\n');
    assert.match(oversizedExpanded, /x{100}/);

    const largeResult = {
      content: [{ type: 'text', text: 'x'.repeat(5000) }],
      details: { uiSummary: 'fixture_echo: large fixture' },
    };
    const collapsed = registered[0].renderResult(largeResult, { expanded: false });
    assert.match(collapsed.render(120).join('\n'), /large fixture.*Ctrl\+O to expand/);
    assert.doesNotMatch(collapsed.render(120).join('\n'), /x{100}/);
    const expanded = registered[0].renderResult(largeResult, { expanded: true });
    assert.match(expanded.render(120).join('\n'), /x{100}/);

  } finally {
    await shutdown?.();
    await rm(root, { recursive: true, force: true });
  }
});

test('generated Pi extension qualifies flat-namespace collisions deterministically', async () => {
  const source = await readFile(templatePath, 'utf8');
  assert.match(source, /serverPrefix.*replace/);
  assert.match(source, /serverPrefix}__\$\{tool\.name/);
  assert.match(source, /qualified tool name collision/);
});

test('generated Pi extension hides agent context from a naked carrier', async () => {
  const source = await readFile(templatePath, 'utf8');
  assert.match(source, /config\.name !== "agent-context"/);
  assert.match(source, /NARADA_CARRIER_SESSION_ADMISSION_RECEIPT/);
  assert.match(source, /SERVERS\.filter\(shouldBootstrapServer\)/);
});

test('generated Pi extension routes Git away from structured-command', async () => {
  const source = await readFile(templatePath, 'utf8');
  assert.match(source, /Git is not a structured-command fallback/);
  assert.match(source, /activate <site-id>-git/);
});

test('generated Pi extension projects task lifecycle to its bounded bridge tool', async () => {
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
    extension({
      on(name, handler) { handlers.set(name, handler); },
      getAllTools() { return []; },
      registerTool(tool) { registered.push(tool); },
    });
    await handlers.get('session_start')?.({}, { ui: { notify() {} } });
    assert.deepEqual(registered.map((tool) => tool.name), ['task_lifecycle_bridge_poll']);
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
    const source = await materializedTemplate([{ name: 'fixture', command: process.execPath, args: [serverPath], enabled: true, startupTimeoutMs: 2000 }]);
    const extensionPath = join(root, 'index.ts');
    await writeFile(extensionPath, source, 'utf8');
    const extension = (await import(`${pathToFileURL(extensionPath).href}?budget=1`)).default;
    const handlers = new Map();
    const registered = [];
    extension({
      on(name, handler) { handlers.set(name, handler); },
      getAllTools() { return []; },
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
