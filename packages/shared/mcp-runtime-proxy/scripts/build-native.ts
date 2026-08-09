import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { nativeArtifactRoot, preserveLegacyNativeArtifact, publishImmutableNativeArtifacts, resolveNativeArtifact } from '../src/native-artifact.js';
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

const pointer = publishImmutableNativeArtifacts({ packageRoot, artifacts: publishArtifacts });
for (const artifact of artifacts) {
  preserveLegacyNativeArtifact(artifact.source, join(outputRoot, artifact.name));
}
if (boaBuild.status === 'built') {
  preserveLegacyNativeArtifact(boaArtifact.source, join(outputRoot, boaArtifact.name));
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
  legacy_executables: executableNames.map((name) => join(outputRoot, name)),
  pointer_path: join(outputRoot, 'current.json'),
  build_fingerprint: pointer.build_fingerprint,
  versioned_directory: join(outputRoot, 'versions', pointer.build_fingerprint),
  boa_fixture: { ...boaBuild, executable: currentBoaExecutable },
  platform: process.platform,
  architecture: process.arch,
}) + '\n');
