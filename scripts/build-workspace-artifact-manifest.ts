import { existsSync, mkdirSync, readFileSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildWorkspaceArtifactManifest } from '../packages/shared/mcp-runtime-proxy/src/workspace-artifact-manifest.ts';

const workspaceRoot = resolve(fileURLToPath(new URL('..', import.meta.url)));
const packageRoots = [
  ...directPackageRoots(join(workspaceRoot, 'packages')),
  ...directPackageRoots(join(workspaceRoot, 'packages', 'shared')),
  resolve(workspaceRoot, '..', 'narada', 'packages', 'process-launch-posture'),
];
const outputPath = join(workspaceRoot, '.ai', 'runtime', 'workspace-artifact-manifest.json');
mkdirSync(resolve(outputPath, '..'), { recursive: true });
const materializerPointerPath = join(workspaceRoot, 'packages', 'shared', 'mcp-materializer-native', 'dist', 'native', 'current.json');
const materializerPointer = existsSync(materializerPointerPath) ? JSON.parse(readFileSync(materializerPointerPath, 'utf8')) : null;
const materializerRelative = materializerPointer?.artifacts?.['narada-mcp-materializer.exe'];
const runtimeArtifactPaths = typeof materializerRelative === 'string'
  ? [resolve(materializerPointerPath, '..', materializerRelative)]
  : [];
const manifest = buildWorkspaceArtifactManifest({ workspaceRoot, packageRoots, outputPath, runtimeArtifactPaths });
const missingExports = manifest.packages.flatMap((pkg) => pkg.export_targets
  .filter((target) => target.fingerprint === null)
  .map((target) => `${pkg.name}:${target.target}`));
const unverifiedDependencies = manifest.packages.flatMap((pkg) => pkg.dependency_fingerprints
  .filter((dependency) => dependency.package_json === null)
  .map((dependency) => `${pkg.name}->${dependency.name}`));
if (missingExports.length > 0) {
  throw new Error(`workspace_export_target_missing:${missingExports.join(',')}`);
}
if (unverifiedDependencies.length > 0) {
  throw new Error(`workspace_dependency_unverified:${unverifiedDependencies.join(',')}`);
}
const retiredNamespaceArtifacts = findFilesNamedDist(packageRoots)
  .filter((path) => /\.(?:js|cjs|mjs|d\.ts)$/.test(path))
  .filter((path) => readFileSync(path, 'utf8').includes('@narada2/'));
if (retiredNamespaceArtifacts.length > 0) {
  throw new Error(`workspace_retired_namespace_artifact:${retiredNamespaceArtifacts
    .map((path) => path.replace(/\\/g, '/'))
    .join(',')}`);
}
console.log(JSON.stringify({
  schema: manifest.schema,
  workspace_root: manifest.workspace_root,
  package_count: manifest.packages.length,
  artifact_count: manifest.artifacts.length,
  manifest_fingerprint: manifest.manifest_fingerprint,
  output_path: outputPath,
}));

function directPackageRoots(directory: string): string[] {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && existsSync(join(directory, entry.name, 'package.json')))
    .map((entry) => join(directory, entry.name));
}

function findFilesNamedDist(roots: string[]): string[] {
  const files: string[] = [];
  for (const root of roots) {
    const dist = join(root, 'dist');
    if (!existsSync(dist)) continue;
    visit(dist, files);
  }
  return files;
}

function visit(directory: string, files: string[]): void {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) visit(path, files);
    else if (entry.isFile()) files.push(path);
  }
}
