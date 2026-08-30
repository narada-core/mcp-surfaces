import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';

const templatePath = new URL('../assets/pi-mcp-extension.ts', import.meta.url);

test('generated Pi extension handshakes, registers, calls, and closes admitted MCP server', async () => {
  const root = await mkdtemp(join(tmpdir(), 'narada-pi-extension-'));
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
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: {
      content: [{ type: "text", text: "summary without lease" }],
      structuredContent: { value: message.params.arguments.value, schema_lease: "lease-fixture" }
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
    const source = (await readFile(templatePath, 'utf8')).replace(
      '__NARADA_PI_MCP_SERVERS__',
      JSON.stringify(servers),
    );
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

    const largeResult = {
      content: [{ type: 'text', text: 'x'.repeat(5000) }],
      details: { uiSummary: 'fixture_echo: large fixture' },
    };
    const collapsed = registered[0].renderResult(largeResult, { expanded: false });
    assert.match(collapsed.render(120).join('\n'), /large fixture.*Ctrl\+O to expand/);
    assert.doesNotMatch(collapsed.render(120).join('\n'), /x{100}/);
    const expanded = registered[0].renderResult(largeResult, { expanded: true });
    assert.match(expanded.render(120).join('\n'), /x{100}/);

    await handlers.get('session_shutdown')?.();
  } finally {
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
    const source = (await readFile(templatePath, 'utf8')).replace(
      '__NARADA_PI_MCP_SERVERS__',
      JSON.stringify([{ name: 'task-lifecycle', command: process.execPath, args: [serverPath], enabled: true, startupTimeoutMs: 2000 }]),
    );
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
    const source = (await readFile(templatePath, 'utf8')).replace(
      '__NARADA_PI_MCP_SERVERS__',
      JSON.stringify([{ name: 'fixture', command: process.execPath, args: [serverPath], enabled: true, startupTimeoutMs: 2000 }]),
    );
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
