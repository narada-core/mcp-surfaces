import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { spawnJsonlMcpServer, type JsonRecord } from '../packages/shared/mcp-e2e-harness/dist/src/main.js';
import { createSpeechTestRegistry } from '../packages/speech-mcp/dist/test/test-registry.js';
import { resolveNativeArtifact } from '../packages/shared/mcp-runtime-proxy/dist/src/native-artifact.js';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const COMPACT_OUTPUT_LIMIT = 8_000;
const DEFAULT_OUTPUT_LIMIT = 32_000;

type SurfaceSpec = {
  packageName: string;
  entry: string;
  nativeArtifact?: string;
  nativeArgs?: (root: string) => string[];
};

const surfaces: SurfaceSpec[] = [
  { packageName: 'agent-context-mcp', entry: 'dist/src/main.js' }, { packageName: 'artifacts-mcp', entry: 'dist/src/main.js' },
  { packageName: 'browser-control-mcp', entry: 'dist/src/main.js' }, { packageName: 'calendar-mcp', entry: 'dist/src/main.js' },
  { packageName: 'catalog-observation-mcp', entry: 'dist/src/main.js' }, { packageName: 'cloudflare-carrier-mcp', entry: 'dist/src/main.js' },
  { packageName: 'git-mcp', entry: 'dist/src/main.js' },
  { packageName: 'graph-mail-mcp', entry: 'dist/src/main.js' }, { packageName: 'launcher-mcp', entry: 'dist/src/main.js' },
  { packageName: 'local-filesystem-mcp', entry: 'dist/native/current.json', nativeArtifact: 'narada-local-filesystem-mcp', nativeArgs: (root) => ['--mode', 'read', '--allowed-root', root, '--output-root', root] }, { packageName: 'mailbox-mcp', entry: 'dist/src/main.js' },
  { packageName: 'nars-session-mcp', entry: 'dist/src/main.js' }, { packageName: 'operator-communication-mcp', entry: 'dist/src/main.js' },
  { packageName: 'operator-console-overlay-mcp', entry: 'dist/src/main.js' }, { packageName: 'operator-routing-mcp', entry: 'dist/src/main.js' },
  { packageName: 'project-state-mcp', entry: 'dist/src/main.js' }, { packageName: 'quota-meter-mcp', entry: 'dist/src/main.js' },
  { packageName: 'runtime-introspection-mcp', entry: 'dist/src/main.js' }, { packageName: 'scheduler-mcp', entry: 'dist/src/main.js' },
  { packageName: 'site-coherence-mcp', entry: 'dist/src/main.js' }, { packageName: 'site-inbox-mcp', entry: 'dist/src/main.js' },
  { packageName: 'site-lifecycle-mcp', entry: 'dist/src/main.js' }, { packageName: 'site-registry-mcp', entry: 'dist/src/main.js' },
  { packageName: 'sop-mcp', entry: 'dist/src/main.js' }, { packageName: 'speech-mcp', entry: 'dist/src/main.js' },
  { packageName: 'structured-command-mcp', entry: 'dist/native/current.json', nativeArtifact: 'narada-structured-command-mcp', nativeArgs: (root) => ['--allowed-root', root, '--allow-command', 'node'] },
  // The lifecycle bindings resolve task-governance-core from the sibling narada-core workspace;
  // their dedicated package/loader harnesses cover those entrypoints separately.
];

function nativeArtifactName(stem: string): string {
  return process.platform === 'win32' ? `${stem}.exe` : stem;
}

function argumentsForReadOnlyTool(tool: JsonRecord, root: string): JsonRecord {
  const schema = (tool.inputSchema ?? {}) as JsonRecord;
  const properties = (schema.properties ?? {}) as Record<string, JsonRecord>;
  const output: JsonRecord = {};
  for (const [name, property] of Object.entries(properties)) {
    if (property.default !== undefined) output[name] = property.default;
    else if (Array.isArray(property.enum) && property.enum.length > 0) output[name] = property.enum[0];
    else if (property.type === 'boolean') output[name] = false;
    else if (property.type === 'integer' || property.type === 'number') output[name] = property.minimum ?? 1;
    else if (property.type === 'array') output[name] = [];
    else if (property.type === 'object') output[name] = {};
    else if (property.type === 'string') output[name] = name.includes('root') || name.includes('path') ? root : 'output-bounds-probe';
  }
  return output;
}

function assertBounded(surface: string, tool: string, response: any, compact: boolean) {
  const value = response.result ?? response.error ?? response;
  const serialized = JSON.stringify(value);
  const limit = compact ? COMPACT_OUTPUT_LIMIT : DEFAULT_OUTPUT_LIMIT;
  const serializedSize = Buffer.byteLength(serialized, 'utf8');
  assert.ok(serializedSize <= limit, `${surface}/${tool} output is ${serializedSize} bytes (limit ${limit})`);
  const text = response.result?.content?.map((item: any) => String(item.text ?? '')).join('') ?? '';
  const textSize = Buffer.byteLength(text, 'utf8');
  assert.ok(textSize <= limit, `${surface}/${tool} text is ${textSize} bytes (limit ${limit})`);
  return { structuredSize: serializedSize, textSize, limit };
}

test('every built MCP surface keeps public command output bounded', async (t) => {
  const missing = surfaces.filter((surface) => {
    const packageRoot = join(repoRoot, 'packages', surface.packageName);
    if (!existsSync(join(packageRoot, surface.entry))) return true;
    return surface.nativeArtifact !== undefined
      && resolveNativeArtifact(packageRoot, nativeArtifactName(surface.nativeArtifact)) === null;
  });
  if (missing.length > 0) {
    t.skip('build all MCP surfaces before running this repository-wide probe');
    return;
  }
  const root = mkdtempSync(join(tmpdir(), 'narada-all-surface-output-bounds-'));
  writeFileSync(join(root, 'AGENTS.md'), '# Output bounds fixture\n', 'utf8');
  const observations: Array<{ surface: string; command: string; structured: number; text: number; limit: number }> = [];
  try {
    for (const surface of surfaces) {
      const { packageName, entry, nativeArtifact, nativeArgs } = surface;
      const packageRoot = join(repoRoot, 'packages', packageName);
      const native = nativeArtifact !== undefined;
      const entrypoint = native
        ? resolveNativeArtifact(packageRoot, nativeArtifactName(nativeArtifact))
        : join(packageRoot, entry);
      if (!entrypoint) {
        t.skip(`missing built entrypoint: ${join(packageRoot, entry)}`);
        return;
      }
      const args = native
        ? nativeArgs!(root)
        : packageName === 'operator-console-overlay-mcp'
          ? ['--narada-root', root]
          : packageName === 'quota-meter-mcp'
            ? ['--quota-meter-root', root, '--state-root', root]
            : packageName === 'scheduler-mcp'
              ? ['--allowed-root', root]
              : packageName === 'sop-mcp'
                ? ['--sop-root', root]
                : packageName === 'speech-mcp'
                  ? ['--provider-registry-path', join(root, 'provider-registry.v2.json')]
                  : ['--site-root', root];
      if (packageName === 'work-lifecycle-mcp') {
        const preparation = spawnSync(process.execPath, [entrypoint, '--prepare', '--site-root', root], { encoding: 'utf8', windowsHide: true });
        assert.equal(preparation.status, 0, `${packageName} preparation failed: ${preparation.stderr}`);
      }
      if (packageName === 'speech-mcp') {
        writeFileSync(join(root, 'provider-registry.v2.json'), JSON.stringify(createSpeechTestRegistry()), 'utf8');
      }
      if (!native && (packageName === 'git-mcp' || packageName === 'local-filesystem-mcp' || packageName === 'structured-command-mcp')) {
        args.push('--allowed-root', root, '--output-root', root, '--mode', 'read');
      }
      const command = native ? entrypoint : process.execPath;
      const commandArgs = native ? args : [entrypoint, ...args];
      const server = spawnJsonlMcpServer(command, commandArgs, { label: `${packageName} output bounds`, timeoutMs: 15_000 });
      try {
        const initialize = await server.client.request(1, 'initialize', { protocolVersion: '2024-11-05' });
        assert.equal(initialize.error, undefined, `${packageName} initialize failed`);
        const listed = await server.client.request(2, 'tools/list', {});
        assert.equal(listed.error, undefined, `${packageName} tools/list failed`);
        const tools = (listed.result?.tools ?? []) as JsonRecord[];
        assert.ok(tools.length > 0, `${packageName} exposed no tools`);
        for (const tool of tools) {
          const name = String(tool.name);
          const readOnly = (tool.annotations as JsonRecord | undefined)?.readOnlyHint === true;
          const response = await server.client.request(`probe-${name}`, 'tools/call', {
            name,
            arguments: readOnly ? argumentsForReadOnlyTool(tool, root) : { __output_bounds_probe__: true },
          });
          const observed = assertBounded(packageName, name, response, name.endsWith('_guidance'));
          observations.push({ surface: packageName, command: name, structured: observed.structuredSize, text: observed.textSize, limit: observed.limit });
        }
      } finally {
        await server.close();
      }
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  if (process.env.NARADA_OUTPUT_BOUNDS_REPORT === '1') {
    console.table(observations);
  }
});
