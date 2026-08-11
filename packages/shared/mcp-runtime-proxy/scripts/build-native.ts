import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { nativeArtifactRoot, publishImmutableNativeArtifacts, resolveNativeArtifact } from '../src/native-artifact.js';
import { fileURLToPath } from 'node:url';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const enforcementContractSource = join(packageRoot, 'src', 'orientation-entry-enforcement-contract.json');
const enforcementContractOutput = join(packageRoot, 'dist', 'src', 'orientation-entry-enforcement-contract.json');
mkdirSync(dirname(enforcementContractOutput), { recursive: true });
copyFileSync(enforcementContractSource, enforcementContractOutput);
if (process.platform !== 'win32') {
  process.stdout.write(`${JSON.stringify({
    schema: 'narada.mcp_runtime_proxy.native_build.v1',
    status: 'skipped',
    reason: 'windows_only',
    platform: process.platform,
    architecture: process.arch,
  })}\n`);
  process.exit(0);
}
const nativeRoot = join(packageRoot, 'native');
const outputRoot = nativeArtifactRoot(packageRoot);
const executableNames = ['narada-mcp-runtime.exe', 'narada-mcp-rhai-filesystem.exe'];
const artifacts = executableNames.map((name) => ({
  name,
  source: join(nativeRoot, 'target', 'release', name),
}));
const boaManifest = join(nativeRoot, 'boa-fixture', 'Cargo.toml');
const boaArtifact = {
  name: 'narada-mcp-boa-fixture.exe',
  source: join(nativeRoot, 'boa-fixture', 'target', 'release', 'narada-mcp-boa-fixture.exe'),
};

const result = spawnSync('cargo', [
  'build',
  '--release',
  '--locked',
  '--manifest-path',
  join(nativeRoot, 'Cargo.toml'),
], {
  cwd: packageRoot,
  stdio: 'inherit',
  windowsHide: true,
});
if (result.error) throw result.error;
if (result.status !== 0) throw new Error('mcp_runtime_proxy_native_build_failed:' + (result.status ?? 'signal'));
for (const artifact of artifacts) {
  if (!existsSync(artifact.source)) throw new Error('mcp_runtime_proxy_native_artifact_missing:' + artifact.source);
}

let boaBuild: { status: 'built' | 'skipped'; reason?: string } = { status: 'skipped', reason: 'windows_only' };
const publishArtifacts = [...artifacts];
if (process.platform === 'win32') {
  const boaResult = spawnSync('cargo', [
    'build',
    '--release',
    '--locked',
    '--manifest-path',
    boaManifest,
  ], {
    cwd: packageRoot,
    stdio: 'inherit',
    windowsHide: true,
  });
  if (!boaResult.error && boaResult.status === 0 && existsSync(boaArtifact.source)) {
    publishArtifacts.push(boaArtifact);
    boaBuild = { status: 'built' };
  } else {
    boaBuild = { status: 'skipped', reason: boaResult.error?.code === 'ENOENT' ? 'cargo_unavailable' : 'boa_build_failed' };
  }
}

const pointerPath = join(outputRoot, 'current.json');
const previousPointer = existsSync(pointerPath) ? readFileSync(pointerPath) : null;
const pointer = publishImmutableNativeArtifacts({ packageRoot, artifacts: publishArtifacts });
const workspaceRoot = resolve(packageRoot, '..', '..', '..');
const registrySync = spawnSync(process.env['npm_node_execpath']?.trim() || 'node', [
  '--import', 'tsx', join(workspaceRoot, 'scripts', 'sync-declared-site-registries.ts'),
], {
  cwd: workspaceRoot,
  stdio: 'inherit',
  windowsHide: true,
});
if (registrySync.error || registrySync.status !== 0) {
  if (previousPointer === null) unlinkSync(pointerPath);
  else writeFileSync(pointerPath, previousPointer);
  if (registrySync.error) throw registrySync.error;
  throw new Error(`mcp_runtime_proxy_site_registry_sync_failed:${registrySync.status ?? 'signal'}`);
}
const currentExecutable = resolveNativeArtifact(packageRoot, 'narada-mcp-runtime.exe');
if (!currentExecutable) throw new Error('mcp_runtime_proxy_native_artifact_publication_missing');
const currentExecutables = executableNames
  .map((name) => resolveNativeArtifact(packageRoot, name))
  .filter((value): value is string => value !== null);
const currentBoaExecutable = boaBuild.status === 'built'
  ? resolveNativeArtifact(packageRoot, boaArtifact.name)
  : null;

process.stdout.write(JSON.stringify({
  schema: 'narada.mcp_runtime_proxy.native_build.v1',
  executable: currentExecutable,
  executables: currentExecutables,
  pointer_path: pointerPath,
  build_fingerprint: pointer.build_fingerprint,
  versioned_directory: join(outputRoot, 'versions', pointer.build_fingerprint),
  boa_fixture: { ...boaBuild, executable: currentBoaExecutable },
  platform: process.platform,
  architecture: process.arch,
}) + '\n');
