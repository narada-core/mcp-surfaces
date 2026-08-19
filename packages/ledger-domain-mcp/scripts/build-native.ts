import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { publishImmutableNativeArtifacts, resolveNativeArtifact } from '@narada-core/mcp-runtime-proxy/native-artifact';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const nativeRoot = join(packageRoot, 'native');
const executableName = process.platform === 'win32' ? 'narada-ledger-domain.exe' : 'narada-ledger-domain';
const outputRoot = join(packageRoot, 'dist', 'native');

// narada-ledger-domain is a cargo workspace member, so the release binary is
// emitted into the workspace-root target directory, not native/target.
function workspaceTargetRoot(start: string): string {
  let current = start;
  for (;;) {
    const manifest = join(current, 'Cargo.toml');
    if (existsSync(manifest) && readFileSync(manifest, 'utf8').includes('[workspace]')) {
      return join(current, 'target', 'release');
    }
    const parent = dirname(current);
    if (parent === current) throw new Error('ledger_domain_workspace_root_unresolved');
    current = parent;
  }
}
const source = join(workspaceTargetRoot(nativeRoot), executableName);

if (process.platform !== 'win32' && process.platform !== 'linux' && process.platform !== 'darwin') {
  process.stdout.write(JSON.stringify({ schema: 'narada.ledger_domain.native_build.v1', status: 'skipped', reason: 'unsupported_platform', platform: process.platform }) + '\n');
  process.exit(0);
}

const result = spawnSync('cargo', ['build', '--release', '--locked', '--manifest-path', join(nativeRoot, 'Cargo.toml')], {
  cwd: packageRoot,
  stdio: 'inherit',
  windowsHide: true,
});
if (result.error) throw result.error;
if (result.status !== 0) throw new Error(`ledger_domain_native_build_failed:${result.status ?? 'signal'}`);
if (!existsSync(source)) throw new Error(`ledger_domain_native_artifact_missing:${source}`);
const pointer = publishImmutableNativeArtifacts({ packageRoot, artifacts: [{ name: executableName, source }] });
const executable = resolveNativeArtifact(packageRoot, executableName);
if (!executable) throw new Error('ledger_domain_native_artifact_publication_missing');
process.stdout.write(JSON.stringify({ schema: 'narada.ledger_domain.native_build.v1', status: 'built', executable, pointer_path: join(outputRoot, 'current.json'), build_fingerprint: pointer.build_fingerprint, platform: process.platform, architecture: process.arch }) + '\n');
