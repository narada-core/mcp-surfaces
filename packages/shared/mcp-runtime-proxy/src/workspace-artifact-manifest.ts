import { createHash } from 'node:crypto';
import {
  existsSync,
  lstatSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { basename, dirname, extname, join, relative, resolve, sep } from 'node:path';
import { describeUnknownError } from './error-description.js';

export const WORKSPACE_ARTIFACT_MANIFEST_SCHEMA = 'narada.workspace_artifact_manifest.v1';

type JsonRecord = Record<string, unknown>;

export type ArtifactFingerprint = {
  path: string;
  sha256: string;
  size: number;
  mtime_ms: number;
};

export type WorkspaceArtifactPackage = {
  name: string;
  root: string;
  package_json: ArtifactFingerprint;
  build_configs: ArtifactFingerprint[];
  source_files: ArtifactFingerprint[];
  export_targets: Array<{
    target: string;
    path: string;
    fingerprint: ArtifactFingerprint | null;
  }>;
  dependency_fingerprints: Array<{
    name: string;
    root: string;
    package_json: ArtifactFingerprint | null;
  }>;
};

export type WorkspaceArtifactManifest = {
  schema: typeof WORKSPACE_ARTIFACT_MANIFEST_SCHEMA;
  generated_at: string;
  workspace_root: string;
  packages: WorkspaceArtifactPackage[];
  artifacts: ArtifactFingerprint[];
  manifest_fingerprint: string;
};

export type WorkspaceArtifactPreflight = {
  schema: 'narada.workspace_artifact_preflight.v1';
  status: 'ok' | 'refused';
  ok: boolean;
  surface_id: string | null;
  entrypoint: string;
  artifact_manifest_path: string | null;
  manifest_fingerprint: string | null;
  code?: 'workspace_manifest_missing' | 'workspace_manifest_stale' | 'workspace_export_target_missing' | 'workspace_artifact_missing' | 'workspace_dependency_unverified' | 'runtime_contract_version_missing' | 'runtime_contract_version_mismatch' | 'materialization_generation_missing' | 'materialization_generation_obsolete' | 'materialization_generation_stale' | 'materialization_managed_projection_stale';
  reason?: string;
  details?: JsonRecord;
};

export function buildWorkspaceArtifactManifest(input: {
  workspaceRoot: string;
  packageRoots: string[];
  outputPath: string;
  runtimeArtifactPaths?: string[];
}): WorkspaceArtifactManifest {
  const workspaceRoot = resolve(input.workspaceRoot);
  const roots = uniquePaths(input.packageRoots);
  const packageJsons = roots
    .map((root) => resolve(root))
    .filter((root) => existsSync(join(root, 'package.json')))
    .map((root) => ({ root, packageJsonPath: join(root, 'package.json') }));
  const packageNames = new Map<string, string>();
  for (const entry of packageJsons) {
    const packageJson = readJson(entry.packageJsonPath);
    if (typeof packageJson.name === 'string') packageNames.set(packageJson.name, entry.root);
  }

  const packages: WorkspaceArtifactPackage[] = packageJsons
    .map(({ root, packageJsonPath }) => buildPackageRecord(root, packageJsonPath, packageNames))
    .sort((left, right) => left.name.localeCompare(right.name));
  const artifacts = uniqueFingerprints(
    [
      ...packages.flatMap((pkg) => pkg.export_targets
        .map((target) => target.fingerprint)
        .filter((value): value is ArtifactFingerprint => value !== null)),
      ...(input.runtimeArtifactPaths ?? [])
        .map((path) => fingerprintFile(resolve(path)))
        .filter((value): value is ArtifactFingerprint => value !== null),
    ],
  ).sort((left, right) => left.path.localeCompare(right.path));
  const unsigned: Omit<WorkspaceArtifactManifest, 'manifest_fingerprint'> = {
    schema: WORKSPACE_ARTIFACT_MANIFEST_SCHEMA,
    generated_at: new Date().toISOString(),
    workspace_root: workspaceRoot,
    packages,
    artifacts,
  };
  const manifest = {
    ...unsigned,
    manifest_fingerprint: fingerprintWorkspaceArtifactManifest(unsigned),
  } satisfies WorkspaceArtifactManifest;
  writeFileSync(resolve(input.outputPath), `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
  return manifest;
}

function expandRuntimeTarget(root: string, target: string): string[] {
  if (/[/\\]dist[/\\]native[/\\]versions[/\\]\*$/.test(target)) {
    const pointerPath = resolve(root, target.replace(/[/\\]versions[/\\]\*$/, '/current.json'));
    if (!existsSync(pointerPath)) return [target];
    const pointer = readJson(pointerPath);
    const artifacts = asRecord(pointer.artifacts);
    const selected = Object.values(artifacts)
      .filter((value): value is string => typeof value === 'string')
      .map((value) => relative(root, resolve(dirname(pointerPath), value)));
    return selected.length > 0 ? selected : [target];
  }
  const wildcardIndex = target.search(/[\\*]/);
  if (wildcardIndex < 0) return [target];
  const directory = resolve(root, target.slice(0, wildcardIndex));
  if (!existsSync(directory)) return [target];
  return walkFiles(directory).map((path) => relative(root, path));
}

export function preflightWorkspaceArtifacts(input: {
  surfaceId: string | null;
  entrypoint: string;
  artifactManifestPath: string | null | undefined;
}): WorkspaceArtifactPreflight {
  const entrypoint = resolve(input.entrypoint);
  const manifestPath = input.artifactManifestPath ? resolve(input.artifactManifestPath) : null;
  if (!manifestPath || !existsSync(manifestPath)) {
    return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_manifest_missing', 'The launch did not provide an existing workspace artifact manifest.');
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(readFileSync(manifestPath, 'utf8'));
  } catch (error) {
    return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_manifest_stale', 'The workspace artifact manifest is unreadable.', {
      error: describeUnknownError(error, 'workspace_artifact_manifest_read_error'),
    });
  }
  if (!isRecord(parsed) || parsed.schema !== WORKSPACE_ARTIFACT_MANIFEST_SCHEMA || typeof parsed.manifest_fingerprint !== 'string') {
    return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_manifest_stale', 'The workspace artifact manifest has an unsupported schema or missing fingerprint.');
  }
  const unsigned = { ...parsed };
  delete unsigned.manifest_fingerprint;
  const actualManifestFingerprint = fingerprintWorkspaceArtifactManifest(unsigned);
  if (actualManifestFingerprint !== parsed.manifest_fingerprint) {
    return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_manifest_stale', 'The workspace artifact manifest fingerprint does not match its contents.', {
      expected_fingerprint: parsed.manifest_fingerprint,
      actual_fingerprint: actualManifestFingerprint,
    });
  }

  const manifest = parsed as unknown as WorkspaceArtifactManifest;
  const packageRecord = manifest.packages.find((pkg) => isPathInside(pkg.root, entrypoint));
  if (!packageRecord) {
    const artifact = manifest.artifacts.find((candidate) => samePath(candidate.path, entrypoint));
    if (!artifact) {
      return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_artifact_missing', 'The entrypoint is not present in the workspace artifact manifest.');
    }
    const current = fingerprintFile(entrypoint);
    if (!current) {
      return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_artifact_missing', 'The manifest entrypoint does not exist on disk.');
    }
    if (current.sha256 !== artifact.sha256 || current.size !== artifact.size) {
      return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_manifest_stale', 'The manifest entrypoint changed after manifest generation.', {
        path: entrypoint,
      });
    }
    return success(input.surfaceId, entrypoint, manifestPath, parsed.manifest_fingerprint);
  }

  const packageJson = fingerprintFile(packageRecord.package_json.path);
  if (!packageJson || packageJson.sha256 !== packageRecord.package_json.sha256) {
    return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_manifest_stale', 'The package manifest changed after artifact generation.', {
      package: packageRecord.name,
      path: packageRecord.package_json.path,
    });
  }
  for (const buildConfig of packageRecord.build_configs) {
    const current = fingerprintFile(buildConfig.path);
    if (!current || current.sha256 !== buildConfig.sha256) {
      return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_manifest_stale', 'The package build configuration changed after artifact generation.', {
        package: packageRecord.name,
        path: buildConfig.path,
      });
    }
  }
  for (const source of packageRecord.source_files) {
    const current = fingerprintFile(source.path);
    if (!current || current.sha256 !== source.sha256) {
      return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_manifest_stale', 'A source file changed after artifact generation.', {
        package: packageRecord.name,
        path: source.path,
      });
    }
  }
  for (const target of packageRecord.export_targets) {
    const current = target.fingerprint ? fingerprintFile(target.path) : null;
    if (!current) {
      return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_export_target_missing', 'A declared package export target is missing.', {
        package: packageRecord.name,
        target: target.target,
        path: target.path,
      });
    }
    if (!target.fingerprint || current.sha256 !== target.fingerprint.sha256 || current.size !== target.fingerprint.size) {
      return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_manifest_stale', 'A declared package export target changed after artifact generation.', {
        package: packageRecord.name,
        target: target.target,
        path: target.path,
      });
    }
  }
  const entryTarget = packageRecord.export_targets.find((target) => samePath(target.path, entrypoint));
  if (!entryTarget || !entryTarget.fingerprint) {
    return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_artifact_missing', 'The requested entrypoint is not a declared runtime artifact.', {
      package: packageRecord.name,
      path: entrypoint,
    });
  }
  for (const dependency of packageRecord.dependency_fingerprints) {
    if (!dependency.package_json) {
      return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_dependency_unverified', 'A local workspace dependency has no package fingerprint.', {
        package: packageRecord.name,
        dependency: dependency.name,
      });
    }
    const current = fingerprintFile(dependency.package_json.path);
    if (!current || current.sha256 !== dependency.package_json.sha256) {
      return refusal(input.surfaceId, entrypoint, manifestPath, 'workspace_dependency_unverified', 'A local workspace dependency changed after artifact generation.', {
        package: packageRecord.name,
        dependency: dependency.name,
        path: dependency.package_json.path,
      });
    }
  }
  return success(input.surfaceId, entrypoint, manifestPath, parsed.manifest_fingerprint);
}

function buildPackageRecord(
  root: string,
  packageJsonPath: string,
  packageNames: Map<string, string>,
): WorkspaceArtifactPackage {
  const packageJson = readJson(packageJsonPath);
  const targets = declaredRuntimeTargets(packageJson);
  const exportTargets = targets.flatMap((target) => expandRuntimeTarget(root, target).map((expandedTarget) => ({
    target: expandedTarget === target ? target : `${target} -> ${expandedTarget}`,
    path: resolve(root, expandedTarget),
    fingerprint: fingerprintFile(resolve(root, expandedTarget)),
  })));
  const dependencies = Object.keys({
    ...asRecord(packageJson.dependencies),
    ...asRecord(packageJson.optionalDependencies),
    ...asRecord(packageJson.peerDependencies),
  })
    .sort()
    .flatMap((name) => {
      const dependencyRoot = packageNames.get(name);
      if (!dependencyRoot) return [];
      return [{
        name,
        root: dependencyRoot,
        package_json: fingerprintFile(join(dependencyRoot, 'package.json')),
      }];
    });
  return {
    name: typeof packageJson.name === 'string' ? packageJson.name : root,
    root,
    package_json: fingerprintFile(packageJsonPath) as ArtifactFingerprint,
    build_configs: ['tsconfig.json', 'tsconfig.build.json']
      .map((name) => fingerprintFile(join(root, name)))
      .filter((value): value is ArtifactFingerprint => value !== null),
    source_files: sourceFilesFor(root),
    export_targets: exportTargets,
    dependency_fingerprints: dependencies,
  };
}

function declaredRuntimeTargets(packageJson: JsonRecord): string[] {
  const targets = new Set<string>();
  for (const key of ['main', 'module']) {
    if (typeof packageJson[key] === 'string') targets.add(packageJson[key] as string);
  }
  collectExportTargets(packageJson.exports, targets);
  const bin = packageJson.bin;
  if (typeof bin === 'string') targets.add(bin);
  else if (isRecord(bin)) {
    for (const value of Object.values(bin)) if (typeof value === 'string') targets.add(value);
  }
  const nativeArtifacts = asRecord(packageJson.naradaRuntimeArtifacts)[process.platform];
  if (Array.isArray(nativeArtifacts)) {
    for (const value of nativeArtifacts) {
      if (typeof value === 'string' && !/[/\\]dist[/\\]native[/\\]current\.json$/i.test(value)) {
        targets.add(value);
      }
    }
  }
  return [...targets].filter((target) => target.startsWith('./') || target.startsWith('../'));
}

function collectExportTargets(value: unknown, targets: Set<string>, key = ''): void {
  if (typeof value === 'string') {
    if (key !== 'types' && key !== 'require') targets.add(value);
    return;
  }
  if (Array.isArray(value)) {
    for (const item of value) collectExportTargets(item, targets, key);
    return;
  }
  if (!isRecord(value)) return;
  for (const [childKey, childValue] of Object.entries(value)) {
    collectExportTargets(childValue, targets, childKey);
  }
}

function sourceFilesFor(root: string): ArtifactFingerprint[] {
  const result: ArtifactFingerprint[] = [];
  for (const directory of ['src', 'bin', 'scripts', join('native', 'src')]) {
    const sourceRoot = join(root, directory);
    if (!existsSync(sourceRoot)) continue;
    for (const path of walkFiles(sourceRoot)) {
      if (['.ts', '.tsx', '.mts', '.cts', '.json', '.rs'].includes(extname(path))) {
        const fingerprint = fingerprintFile(path);
        if (fingerprint) result.push(fingerprint);
      }
    }
  }
  for (const name of ['Cargo.toml', 'Cargo.lock']) {
    const fingerprint = fingerprintFile(join(root, 'native', name));
    if (fingerprint) result.push(fingerprint);
  }
  return result.sort((left, right) => left.path.localeCompare(right.path));
}

function walkFiles(root: string): string[] {
  const result: string[] = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) result.push(...walkFiles(path));
    else if (entry.isFile()) result.push(path);
  }
  return result;
}

function fingerprintFile(path: string): ArtifactFingerprint | null {
  try {
    const stat = statSync(path);
    if (!stat.isFile()) return null;
    const physical = readFileSync(path);
    let identity = physical;
    if (basename(path).toLowerCase() === 'current.json') {
      const parsed = JSON.parse(physical.toString('utf8')) as unknown;
      identity = Buffer.from(JSON.stringify(stripVolatileManifestMetadata(parsed)), 'utf8');
    }
    return {
      path: resolve(path),
      sha256: createHash('sha256').update(identity).digest('hex'),
      size: identity.length,
      mtime_ms: stat.mtimeMs,
    };
  } catch {
    return null;
  }
}

function uniqueFingerprints(values: ArtifactFingerprint[]): ArtifactFingerprint[] {
  const seen = new Set<string>();
  return values.filter((value) => {
    if (seen.has(value.path.toLowerCase())) return false;
    seen.add(value.path.toLowerCase());
    return true;
  });
}

function uniquePaths(values: string[]): string[] {
  const seen = new Set<string>();
  return values.filter((value) => {
    const path = resolve(value);
    if (seen.has(path.toLowerCase())) return false;
    seen.add(path.toLowerCase());
    return true;
  });
}

function success(
  surfaceId: string | null,
  entrypoint: string,
  manifestPath: string,
  manifestFingerprint: string,
): WorkspaceArtifactPreflight {
  return {
    schema: 'narada.workspace_artifact_preflight.v1',
    status: 'ok',
    ok: true,
    surface_id: surfaceId,
    entrypoint,
    artifact_manifest_path: manifestPath,
    manifest_fingerprint: manifestFingerprint,
  };
}

function refusal(
  surfaceId: string | null,
  entrypoint: string,
  manifestPath: string | null,
  code: NonNullable<WorkspaceArtifactPreflight['code']>,
  reason: string,
  details: JsonRecord = {},
): WorkspaceArtifactPreflight {
  return {
    schema: 'narada.workspace_artifact_preflight.v1',
    status: 'refused',
    ok: false,
    surface_id: surfaceId,
    entrypoint,
    artifact_manifest_path: manifestPath,
    manifest_fingerprint: null,
    code,
    reason,
    details,
  };
}

function readJson(path: string): JsonRecord {
  return JSON.parse(readFileSync(path, 'utf8')) as JsonRecord;
}

function asRecord(value: unknown): JsonRecord {
  return isRecord(value) ? value : {};
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function samePath(left: string, right: string): boolean {
  return resolve(left).toLowerCase() === resolve(right).toLowerCase();
}

function isPathInside(root: string, candidate: string): boolean {
  const rootPath = resolve(root);
  const candidatePath = resolve(candidate);
  const suffix = relative(rootPath, candidatePath);
  return suffix === '' || (suffix !== '..' && !suffix.startsWith(`..${sep}`) && !suffix.startsWith('../'));
}

function fingerprintObject(value: unknown): string {
  return createHash('sha256').update(JSON.stringify(value)).digest('hex');
}

export function fingerprintWorkspaceArtifactManifest(value: JsonRecord): string {
  return fingerprintObject(stripVolatileManifestMetadata(value));
}

function stripVolatileManifestMetadata(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(stripVolatileManifestMetadata);
  if (!isRecord(value)) return value;

  const stable: JsonRecord = {};
  for (const [key, child] of Object.entries(value)) {
    if (key === 'generated_at' || key === 'mtime_ms') continue;
    stable[key] = stripVolatileManifestMetadata(child);
  }
  return stable;
}

export function artifactFingerprint(path: string): ArtifactFingerprint | null {
  return fingerprintFile(path);
}
