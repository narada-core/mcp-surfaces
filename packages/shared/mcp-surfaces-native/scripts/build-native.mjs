import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const nativeRoot = join(packageRoot, 'native');
const executableName = process.platform === 'win32' ? 'narada-mcp-surfaces.exe' : 'narada-mcp-surfaces';
const source = join(nativeRoot, 'target', 'release', executableName);
const outputRoot = join(packageRoot, 'dist', 'native');
const output = join(outputRoot, executableName);

if (!['win32', 'linux', 'darwin'].includes(process.platform)) {
  process.stdout.write(JSON.stringify({
    schema: 'narada.mcp_surfaces_native.build.v1',
    status: 'skipped',
    reason: 'unsupported_platform',
    platform: process.platform,
  }) + String.fromCharCode(10));
  process.exit(0);
}
const result = spawnSync('cargo', [
  'build',
  '--release',
  '--locked',
  '--manifest-path',
  join(nativeRoot, 'Cargo.toml'),
], { cwd: packageRoot, stdio: 'inherit', windowsHide: true });
if (result.error) throw result.error;
if (result.status !== 0) throw new Error('mcp_surfaces_native_build_failed:' + (result.status ?? 'signal'));
if (!existsSync(source)) throw new Error('mcp_surfaces_native_artifact_missing:' + source);
mkdirSync(outputRoot, { recursive: true });
copyFileSync(source, output);
process.stdout.write(JSON.stringify({
  schema: 'narada.mcp_surfaces_native.build.v1',
  status: 'built',
  executable: output,
  platform: process.platform,
  architecture: process.arch,
}) + String.fromCharCode(10));