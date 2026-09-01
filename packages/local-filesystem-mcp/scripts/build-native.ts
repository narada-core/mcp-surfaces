import { existsSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { publishImmutableNativeArtifacts, resolveNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
// tsc -b does not remove outputs for deleted TypeScript authorities. Remove
// the retired runtime and test projections before publishing this package.
for (const name of ['main', 'guidance', 'policy', 'search', 'search-runner', 'patch-apply', 'result-rendering']) {
  rmSync(join(packageRoot, 'dist', 'src', `${name}.js`), { force: true });
  rmSync(join(packageRoot, 'dist', 'src', `${name}.d.ts`), { force: true });
  rmSync(join(packageRoot, 'dist', 'src', `${name}.js.map`), { force: true });
}
rmSync(join(packageRoot, 'dist', 'test'), { recursive: true, force: true });
const nativeRoot = join(packageRoot, 'native');
const executableName = `narada-local-filesystem-mcp${process.platform === 'win32' ? '.exe' : ''}`;
const source = join(nativeRoot, 'target', 'release', executableName);

if (!['win32', 'linux', 'darwin'].includes(process.platform)) {
  process.stdout.write(JSON.stringify({
    schema: 'narada.local_filesystem.native_build.v1',
    status: 'skipped',
    reason: 'unsupported_platform',
    platform: process.platform,
  }) + '\n');
  process.exit(0);
}

const result = spawnSync('cargo', [
  'build',
  '--release',
  '--locked',
  '--manifest-path',
  join(nativeRoot, 'Cargo.toml'),
], {
  cwd: packageRoot,
  env: { ...process.env, CARGO_TARGET_DIR: join(nativeRoot, 'target') },
  stdio: 'inherit',
  windowsHide: true,
});
if (result.error) throw result.error;
if (result.status !== 0) throw new Error(`local_filesystem_native_build_failed:${result.status ?? 'signal'}`);
if (!existsSync(source)) throw new Error(`local_filesystem_native_artifact_missing:${source}`);

const pointer = publishImmutableNativeArtifacts({ packageRoot, artifacts: [{ name: executableName, source }] });
const executable = resolveNativeArtifact(packageRoot, executableName);
if (!executable) throw new Error('local_filesystem_native_artifact_publication_missing');
process.stdout.write(JSON.stringify({
  schema: 'narada.local_filesystem.native_build.v1',
  status: 'built',
  executable,
  pointer_path: join(packageRoot, 'dist', 'native', 'current.json'),
  build_fingerprint: pointer.build_fingerprint,
  platform: process.platform,
  architecture: process.arch,
}) + '\n');
