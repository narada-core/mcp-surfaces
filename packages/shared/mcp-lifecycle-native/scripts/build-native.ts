import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
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

const extension = process.platform === 'win32' ? '.exe' : '';
const names = [`narada-task-lifecycle-mcp${extension}`, `narada-work-lifecycle-mcp${extension}`];
const outputRoot = join(packageRoot, 'dist', 'native');
mkdirSync(outputRoot, { recursive: true });
for (const name of names) {
  const source = join(nativeRoot, 'target', 'release', name);
  if (!existsSync(source)) throw new Error(`mcp_lifecycle_native_artifact_missing:${source}`);
  copyFileSync(source, join(outputRoot, name));
}
process.stdout.write(`${JSON.stringify({
  schema: 'narada.mcp_lifecycle_native_build.v1',
  status: 'built',
  artifacts: names.map((name) => join(outputRoot, name)),
})}\n`);
