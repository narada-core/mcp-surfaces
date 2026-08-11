import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import {
  preserveLegacyNativeArtifact,
  publishImmutableNativeArtifacts,
  resolveNativeArtifact,
} from '@narada-core/mcp-runtime-proxy/native-artifact';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const nativeRoot = join(packageRoot, 'native');
const result = spawnSync('cargo', [
  'build', '--release', '--locked', '--manifest-path', join(nativeRoot, 'Cargo.toml'),
], { cwd: packageRoot, stdio: 'inherit', windowsHide: true });
if (result.error) throw result.error;
if (result.status !== 0) throw new Error('mcp_lifecycle_native_build_failed:' + (result.status ?? 'signal'));

const extension = process.platform === 'win32' ? '.exe' : '';
const names = ['narada-task-lifecycle-mcp' + extension, 'narada-work-lifecycle-mcp' + extension];
const outputRoot = join(packageRoot, 'dist', 'native');
const sources = names.map((name) => {
  const source = join(nativeRoot, 'target', 'release', name);
  if (!existsSync(source)) throw new Error('mcp_lifecycle_native_artifact_missing:' + source);
  return { name, source };
});
const pointer = publishImmutableNativeArtifacts({ packageRoot, artifacts: sources });
for (const artifact of sources) {
  preserveLegacyNativeArtifact(artifact.source, join(outputRoot, artifact.name));
}
const artifacts = sources.map((artifact) => ({
  path: resolveNativeArtifact(packageRoot, artifact.name),
  publication: 'immutable',
}));
process.stdout.write(JSON.stringify({
  schema: 'narada.mcp_lifecycle_native_build.v1',
  status: 'built',
  artifacts,
  build_fingerprint: pointer.build_fingerprint,
  pointer_path: join(outputRoot, 'current.json'),
}) + '\n');
