import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export type KimiMcpServerConfig = {
  transport: 'stdio';
  command: string;
  args: string[];
  env?: Record<string, string>;
  env_vars?: string[];
};

export type KimiCarrierConfig = {
  mcpServers: Record<string, KimiMcpServerConfig>;
};

export async function materializeKimiCarrierConfig(outputPath: string): Promise<KimiCarrierConfig> {
  const workspace = resolve(fileURLToPath(new URL('../../../..', import.meta.url)));
  const profile = process.env.USERPROFILE?.trim();
  assert.ok(profile, 'USERPROFILE is required for the native carrier contract fixture');
  const packageRoot = join(workspace, 'packages', 'shared', 'mcp-materializer-native');
  const pointer = JSON.parse(readFileSync(join(packageRoot, 'dist', 'native', 'current.json'), 'utf8')) as {
    artifacts: Record<string, string>;
  };
  const executable = resolve(packageRoot, 'dist', 'native', pointer.artifacts['narada-mcp-materializer.exe']);
  const carrierHome = dirname(outputPath);
  mkdirSync(carrierHome, { recursive: true });
  const contractPath = join(carrierHome, 'carrier-materialization.json');
  const installedIndex = join(carrierHome, 'installed-carriers.json');
  writeFileSync(contractPath, JSON.stringify({
    schema: 'narada.native_carrier_contract.v2',
    sites: [{
      site_id: 'andrey-user',
      registry_path: join(profile, 'Narada', '.narada', 'capabilities', 'mcp-surfaces.json'),
      surface_ids: ['agent-context', 'local-filesystem', 'mcp-registrar', 'mcp-loader', 'task-lifecycle', 'surface-feedback'],
    }],
    carriers: [{
      carrier_id: 'kimi-test',
      carrier_kind: 'kimi',
      config_relative_path: relative(carrierHome, outputPath).replace(/\\/g, '/'),
    }],
  }, null, 2), 'utf8');
  const run = spawnSync(executable, [
    'materialize-site',
    '--contract', contractPath,
    '--workspace-root', workspace,
    '--home', carrierHome,
    '--matrix', resolve(workspace, '..', 'narada', 'packages', 'operator-surface-runtime-contract', 'contracts', 'runtime-implementation-matrix.json'),
    '--installed-index', installedIndex,
  ], { cwd: workspace, encoding: 'utf8', windowsHide: true });
  assert.equal(run.status, 0, run.stderr || run.stdout);
  const verification = spawnSync(executable, [
    'verify-all', '--installed-index', installedIndex,
  ], { cwd: workspace, encoding: 'utf8', windowsHide: true });
  assert.equal(verification.status, 0, verification.stderr || verification.stdout);
  const parsed = JSON.parse(readFileSync(outputPath, 'utf8')) as Record<string, unknown>;
  assert.ok(isRecord(parsed.mcpServers), 'materialized Kimi config must contain mcpServers');
  assert.ok(Object.keys(parsed.mcpServers).length > 0, 'materialized Kimi config must contain at least one server');
  return parsed as KimiCarrierConfig;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
