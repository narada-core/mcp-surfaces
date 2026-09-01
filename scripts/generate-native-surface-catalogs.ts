import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const workspaceRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const executableName = (stem) => `${stem}${process.platform === 'win32' ? '.exe' : ''}`;

function nativeTools(executable, args) {
  const result = spawnSync(executable, args, {
    cwd: workspaceRoot,
    input: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list', params: {} }) + '\n',
    encoding: 'utf8',
    windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`native_surface_catalog_tools_list_failed:${executable}:${result.stderr}`);
  const line = result.stdout.trim().split(/\r?\n/).filter(Boolean).at(-1);
  if (!line) throw new Error(`native_surface_catalog_tools_list_empty:${executable}`);
  const response = JSON.parse(line);
  if (response.error) throw new Error(`native_surface_catalog_tools_list_refused:${response.error.message}`);
  return response.result.tools;
}

function executable(stem, envName) {
  const override = process.env[envName]?.trim();
  if (override) return override;
  const candidates = [
    join(workspaceRoot, 'target', 'release', executableName(stem)),
    join(workspaceRoot, 'target', 'debug', executableName(stem)),
  ];
  const result = candidates.find((candidate) => existsSync(candidate));
  if (!result) throw new Error(`native_surface_catalog_executable_missing:${stem}`);
  return result;
}

function writeTypescript(path, exportName, modes) {
  mkdirSync(dirname(path), { recursive: true });
  const modeEntries = Object.entries(modes).map(([mode, tools]) =>
    `  ${JSON.stringify(mode)}: ${JSON.stringify(tools, null, 2)},`).join('\n');
  const source = `import type { McpToolDefinition } from '@narada-core/mcp-fabric-contracts';\n\n// Generated from the native tools/list registry. Do not hand-edit.\nconst TOOLS = {\n${modeEntries}\n} as unknown as Record<string, McpToolDefinition[]>;\n\nexport function ${exportName}(mode${Object.keys(modes).length > 1 ? ': string' : ''} = 'write'): any[] {\n  const selected = TOOLS[mode] ?? TOOLS[${JSON.stringify(Object.keys(modes).at(-1))}];\n  return selected.map((tool) => ({ ...tool }));\n}\n`;
  writeFileSync(path, source, 'utf8');
}

function writeJson(path, value) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(value, null, 2) + '\n', 'utf8');
}

const filesystem = {
  read: nativeTools(executable('narada-local-filesystem-mcp', 'NARADA_NATIVE_FILESYSTEM_CATALOG_EXECUTABLE'), [
    '--mode', 'read', '--allowed-root', workspaceRoot, '--output-root', workspaceRoot,
  ]),
  write: nativeTools(executable('narada-local-filesystem-mcp', 'NARADA_NATIVE_FILESYSTEM_CATALOG_EXECUTABLE'), [
    '--mode', 'write', '--allowed-root', workspaceRoot, '--output-root', workspaceRoot,
  ]),
};
const structured = nativeTools(executable('narada-structured-command-mcp', 'NARADA_NATIVE_STRUCTURED_COMMAND_CATALOG_EXECUTABLE'), [
  '--allowed-root', workspaceRoot, '--site-root', workspaceRoot, '--storage-root', workspaceRoot,
  '--allow-command', 'node', '--allow-command', 'pnpm', '--allow-command', 'npm', '--allow-command', 'python',
  '--allow-prefix', 'uv run --with sympy python',
]);

writeTypescript(
  join(workspaceRoot, 'packages', 'local-filesystem-mcp', 'src', 'native-tool-catalog.ts'),
  'nativeFilesystemTools',
  filesystem,
);
writeTypescript(
  join(workspaceRoot, 'packages', 'structured-command-mcp', 'src', 'native-tool-catalog.ts'),
  'nativeStructuredCommandTools',
  { all: structured },
);
writeJson(join(workspaceRoot, 'packages', 'local-filesystem-mcp', 'native', 'tool-catalog.json'), {
  schema: 'narada.local_filesystem.native_tool_catalog.v1',
  surface_id: 'local-filesystem',
  modes: filesystem,
});
writeJson(join(workspaceRoot, 'packages', 'structured-command-mcp', 'native', 'tool-catalog.json'), {
  schema: 'narada.structured_command.native_tool_catalog.v1',
  surface_id: 'structured-command',
  tools: structured,
});
process.stdout.write(JSON.stringify({
  schema: 'narada.native_surface_catalog_generation.v1',
  status: 'generated',
  surfaces: {
    'local-filesystem': { read: filesystem.read.length, write: filesystem.write.length },
    'structured-command': { tools: structured.length },
  },
}) + '\n');
