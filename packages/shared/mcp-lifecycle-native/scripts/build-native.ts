import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const nativeRoot = join(packageRoot, 'native');
const result = spawnSync('cargo', [
  'build', '--release', '--locked', '--manifest-path', join(nativeRoot, 'Cargo.toml'),
], { cwd: packageRoot, stdio: 'inherit', windowsHide: true });
if (result.error) throw result.error;
if (result.status !== 0) throw new Error(`mcp_lifecycle_native_build_failed:${result.status ?? 'signal'}`);

function sha256(path: string): string {
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

const extension = process.platform === 'win32' ? '.exe' : '';
const names = [`narada-task-lifecycle-mcp${extension}`, `narada-work-lifecycle-mcp${extension}`];
const outputRoot = join(packageRoot, 'dist', 'native');
mkdirSync(outputRoot, { recursive: true });
const artifacts = names.map((name) => {
  const source = join(nativeRoot, 'target', 'release', name);
  const destination = join(outputRoot, name);
  if (!existsSync(source)) throw new Error(`mcp_lifecycle_native_artifact_missing:${source}`);

  // Windows refuses to overwrite a running executable. A no-op rebuild commonly
  // produces the same bytes, so preserve the in-use publication in that case.
  const unchanged = existsSync(destination) && sha256(source) === sha256(destination);
  if (!unchanged) copyFileSync(source, destination);
  return { path: destination, publication: unchanged ? 'unchanged' : 'copied' };
});
process.stdout.write(`${JSON.stringify({
  schema: 'narada.mcp_lifecycle_native_build.v1',
  status: 'built',
  artifacts,
})}\n`);
