import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import {
  publishImmutableNativeArtifacts,
  resolveNativeArtifact,
} from '../../mcp-runtime-proxy/dist/src/native-artifact.js';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const nativeRoot = join(packageRoot, 'native');
const executableName = process.platform === 'win32' ? 'narada-mcp-surfaces.exe' : 'narada-mcp-surfaces';
// The crate is a cargo-workspace member, so a bare `cargo build` writes to the
// workspace-root target/. Pin CARGO_TARGET_DIR to the crate-local target so the
// artifact path below is always the one cargo just wrote (no stale publishes).
const targetDir = process.env.CARGO_TARGET_DIR ?? join(nativeRoot, 'target');
const source = join(targetDir, 'release', executableName);
const outputRoot = join(packageRoot, 'dist', 'native');

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
], { cwd: packageRoot, env: { ...process.env, CARGO_TARGET_DIR: targetDir }, stdio: 'inherit', windowsHide: true });
if (result.error) throw result.error;
if (result.status !== 0) throw new Error('mcp_surfaces_native_build_failed:' + (result.status ?? 'signal'));
if (!existsSync(source)) throw new Error('mcp_surfaces_native_artifact_missing:' + source);
const pointer = publishImmutableNativeArtifacts({
  packageRoot,
  artifacts: [{ name: executableName, source }],
});
const currentExecutable = resolveNativeArtifact(packageRoot, executableName);
if (!currentExecutable) throw new Error('mcp_surfaces_native_artifact_publication_missing');
process.stdout.write(JSON.stringify({
  schema: 'narada.mcp_surfaces_native.build.v1',
  status: 'built',
  executable: currentExecutable,
  pointer_path: join(outputRoot, 'current.json'),
  build_fingerprint: pointer.build_fingerprint,
  versioned_directory: join(outputRoot, 'versions', pointer.build_fingerprint),
  platform: process.platform,
  architecture: process.arch,
}) + String.fromCharCode(10));