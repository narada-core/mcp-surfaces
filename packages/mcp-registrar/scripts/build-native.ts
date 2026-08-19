import { existsSync, readFileSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { publishImmutableNativeArtifacts, resolveNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const nativeRoot = join(packageRoot, 'native');
const executableName = `narada-mcp-registrar${process.platform === 'win32' ? '.exe' : ''}`;
// narada-mcp-registrar is a cargo workspace member, so the release binary is
// emitted into the workspace-root target directory, not native/target.
function workspaceTargetRoot(start: string): string {
  let current = start;
  for (;;) {
    const manifest = join(current, 'Cargo.toml');
    if (existsSync(manifest) && readFileSync(manifest, 'utf8').includes('[workspace]')) {
      return join(current, 'target', 'release');
    }
    const parent = dirname(current);
    if (parent === current) throw new Error('mcp_registrar_workspace_root_unresolved');
    current = parent;
  }
}
const source = join(workspaceTargetRoot(nativeRoot), executableName);
if (!['win32', 'linux', 'darwin'].includes(process.platform)) process.exit(0);
// TypeScript incremental builds do not remove outputs for deleted sources.
// The registrar no longer has a JavaScript runtime, so never leave its retired
// authority in the published package or workspace artifact manifest.
rmSync(join(packageRoot, 'dist', 'src'), { recursive: true, force: true });
const result = spawnSync('cargo', ['build', '--release', '--locked', '--manifest-path', join(nativeRoot, 'Cargo.toml')], { cwd: packageRoot, stdio: 'inherit', windowsHide: true });
if (result.error) throw result.error;
if (result.status !== 0) throw new Error(`mcp_registrar_native_build_failed:${result.status ?? 'signal'}`);
if (!existsSync(source)) throw new Error(`mcp_registrar_native_artifact_missing:${source}`);
const pointer = publishImmutableNativeArtifacts({ packageRoot, artifacts: [{ name: executableName, source }] });
const executable = resolveNativeArtifact(packageRoot, executableName);
if (!executable) throw new Error('mcp_registrar_native_artifact_publication_missing');
const workspaceRoot = resolve(packageRoot, '..', '..');
const registrySync = spawnSync(process.execPath, ['--import', 'tsx', join(workspaceRoot, 'scripts', 'sync-declared-site-registries.ts')], {
  cwd: workspaceRoot,
  stdio: 'inherit',
  windowsHide: true,
});
if (registrySync.error) throw registrySync.error;
if (registrySync.status !== 0) throw new Error(`mcp_registrar_site_registry_sync_failed:${registrySync.status ?? 'signal'}`);
process.stdout.write(JSON.stringify({ schema: 'narada.mcp_registrar.native_build.v1', status: 'built', executable, pointer_path: join(packageRoot, 'dist', 'native', 'current.json'), build_fingerprint: pointer.build_fingerprint, platform: process.platform, architecture: process.arch }) + '\n');
