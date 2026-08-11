import { createRequire } from 'node:module';
import { existsSync, readdirSync, readFileSync, realpathSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

type PackageManifest = {
  name?: unknown;
  version?: unknown;
  dependencies?: Record<string, unknown>;
  devDependencies?: Record<string, unknown>;
  optionalDependencies?: Record<string, unknown>;
  peerDependencies?: Record<string, unknown>;
};

type WorkspacePackage = {
  name: string;
  version: string | null;
  package_root: string;
  manifest_path: string;
  workspace_pattern: string;
  workspace_scope: 'local' | 'external';
};

type DependencyResolution = {
  consumer_package: string;
  consumer_package_root: string;
  dependency: string;
  requested: string;
  declared_in: 'dependencies' | 'devDependencies' | 'optionalDependencies' | 'peerDependencies';
  expected_package_root: string;
  expected_workspace_pattern: string;
  installed_manifest_path: string | null;
  installed_package_root: string | null;
  installed_realpath: string | null;
  status: 'resolved_to_declared_workspace_package' | 'unresolved' | 'resolved_outside_declared_workspace_package';
};

type ExternalWorkspacePackageProvenance = {
  status: 'coherent';
  workspace_patterns: string[];
  packages: Array<{
    name: string;
    version: string | null;
    package_root: string;
    manifest_path: string;
    workspace_pattern: string;
  }>;
  dependency_resolutions: DependencyResolution[];
  ambiguities: Array<{
    package_name: string;
    package_roots: string[];
  }>;
};

export type WorkspaceBuildPreparation = {
  schema: 'narada.workspace_build_preparation.v1';
  status: 'ready';
  project_count: number;
  artifact_posture: 'preserve_last_successful_dist';
  external_workspace_package_provenance: ExternalWorkspacePackageProvenance;
};

export function prepareWorkspaceBuild(workspaceRoot: string): WorkspaceBuildPreparation {
  const root = resolve(workspaceRoot);
  const packagesRoot = join(root, 'packages');
  const projectRoots = [
    ...directProjectRoots(packagesRoot),
    ...directProjectRoots(join(packagesRoot, 'shared')),
  ].sort();
  const rootConfig = JSON.parse(readFileSync(join(root, 'tsconfig.json'), 'utf8')) as {
    references?: Array<{ path?: string }>;
  };
  const referenced = new Set((rootConfig.references ?? [])
    .map((entry) => typeof entry.path === 'string' ? resolve(root, entry.path) : '')
    .filter(Boolean));
  const missingReferences = projectRoots.filter((projectRoot) => !referenced.has(projectRoot));
  if (missingReferences.length > 0) {
    throw new Error('workspace_tsconfig_reference_missing:' + missingReferences
      .map((projectRoot) => relative(root, projectRoot).replace(/\\/g, '/'))
      .join(','));
  }

  const workspacePackages = discoverWorkspacePackages(root);
  const packageByName = new Map<string, WorkspacePackage[]>();
  for (const workspacePackage of workspacePackages) {
    const existing = packageByName.get(workspacePackage.name) ?? [];
    existing.push(workspacePackage);
    packageByName.set(workspacePackage.name, existing);
  }
  const ambiguities = [...packageByName.entries()]
    .filter(([, packages]) => packages.length > 1)
    .map(([packageName, packages]) => ({
      package_name: packageName,
      package_roots: packages.map((workspacePackage) => portablePath(workspacePackage.package_root)).sort(),
    }));
  if (ambiguities.length > 0) {
    throw new Error('workspace_package_name_ambiguous:' + ambiguities
      .map((ambiguity) => ambiguity.package_name + '=' + ambiguity.package_roots.join('|'))
      .join(','));
  }

  const externalPackages = workspacePackages.filter((workspacePackage) => workspacePackage.workspace_scope === 'external');
  const dependencyResolutions = resolveExternalDependencies(workspacePackages, packageByName);
  const incoherent = dependencyResolutions.filter((resolution) => resolution.status !== 'resolved_to_declared_workspace_package');
  if (incoherent.length > 0) {
    throw new Error('external_workspace_package_provenance_unresolved:' + incoherent
      .map((resolution) => resolution.consumer_package + '->' + resolution.dependency + '=' + resolution.status)
      .join(','));
  }
  const externalProvenance: ExternalWorkspacePackageProvenance = {
    status: 'coherent',
    workspace_patterns: unique(externalPackages.map((workspacePackage) => workspacePackage.workspace_pattern)).sort(),
    packages: externalPackages
      .map((workspacePackage) => ({
        name: workspacePackage.name,
        version: workspacePackage.version,
        package_root: portablePath(workspacePackage.package_root),
        manifest_path: portablePath(workspacePackage.manifest_path),
        workspace_pattern: workspacePackage.workspace_pattern,
      }))
      .sort((left, right) => left.name.localeCompare(right.name)),
    dependency_resolutions: dependencyResolutions
      .sort((left, right) => (left.consumer_package + ':' + left.dependency).localeCompare(right.consumer_package + ':' + right.dependency)),
    ambiguities,
  };

  return {
    schema: 'narada.workspace_build_preparation.v1',
    status: 'ready',
    project_count: projectRoots.length,
    artifact_posture: 'preserve_last_successful_dist',
    external_workspace_package_provenance: externalProvenance,
  };
}

function directProjectRoots(directory: string): string[] {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && existsSync(join(directory, entry.name, 'tsconfig.json')))
    .map((entry) => resolve(directory, entry.name));
}

function discoverWorkspacePackages(root: string): WorkspacePackage[] {
  return readWorkspacePatterns(root).flatMap((workspacePattern) => {
    const external = !pathWithin(root, resolveWorkspacePattern(root, workspacePattern));
    return expandWorkspacePattern(root, workspacePattern)
      .map((packageRoot) => {
        const manifestPath = join(packageRoot, 'package.json');
        const manifest = readPackageManifest(manifestPath);
        if (typeof manifest.name !== 'string' || !manifest.name.trim()) {
          throw new Error('workspace_package_name_missing:' + portablePath(manifestPath));
        }
        return {
          name: manifest.name,
          version: typeof manifest.version === 'string' ? manifest.version : null,
          package_root: packageRoot,
          manifest_path: manifestPath,
          workspace_pattern: workspacePattern,
          workspace_scope: external ? 'external' : 'local',
        } satisfies WorkspacePackage;
      });
  }).filter((workspacePackage, index, packages) => packages.findIndex((candidate) => (
    candidate.name === workspacePackage.name
      && samePath(candidate.package_root, workspacePackage.package_root)
  )) === index);
}

function readWorkspacePatterns(root: string): string[] {
  const workspacePath = join(root, 'pnpm-workspace.yaml');
  if (!existsSync(workspacePath)) return [];
  const content = readFileSync(workspacePath, 'utf8');
  return [...content.matchAll(/^\s*-\s*(?:"([^"]+)"|'([^']+)'|([^\s#]+))/gm)]
    .map((match) => match[1] ?? match[2] ?? match[3] ?? '')
    .filter(Boolean);
}

function resolveWorkspacePattern(root: string, pattern: string): string {
  const wildcardIndex = pattern.search(/[*?]/);
  const prefix = wildcardIndex < 0 ? pattern : pattern.slice(0, wildcardIndex);
  const separator = Math.max(prefix.lastIndexOf('/'), prefix.lastIndexOf('\\'));
  return resolve(root, separator >= 0 ? prefix.slice(0, separator) : '.');
}

function expandWorkspacePattern(root: string, pattern: string): string[] {
  const wildcardIndex = pattern.search(/[*?]/);
  if (wildcardIndex < 0) {
    const candidate = resolve(root, pattern);
    return existsSync(join(candidate, 'package.json')) ? [candidate] : [];
  }
  const base = resolveWorkspacePattern(root, pattern);
  if (!existsSync(base)) return [];
  return readdirSync(base, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && existsSync(join(base, entry.name, 'package.json')))
    .map((entry) => resolve(base, entry.name));
}

function readPackageManifest(path: string): PackageManifest {
  try {
    return JSON.parse(readFileSync(path, 'utf8')) as PackageManifest;
  } catch (error) {
    throw new Error('workspace_package_manifest_invalid:' + portablePath(path) + ':' + String(error));
  }
}

function resolveExternalDependencies(
  workspacePackages: WorkspacePackage[],
  packageByName: Map<string, WorkspacePackage[]>,
): DependencyResolution[] {
  const resolutions: DependencyResolution[] = [];
  for (const consumer of workspacePackages) {
    const manifest = readPackageManifest(consumer.manifest_path);
    for (const [section, values] of dependencySections(manifest)) {
      for (const [dependency, requestedValue] of Object.entries(values)) {
        const expected = packageByName.get(dependency)?.[0];
        if (!expected || expected.workspace_scope !== 'external') continue;
        const requested = typeof requestedValue === 'string' ? requestedValue : String(requestedValue);
        const installed = resolveInstalledPackage(consumer.manifest_path, dependency);
        const resolvedToExpected = installed !== null && samePath(installed.package_root, expected.package_root);
        resolutions.push({
          consumer_package: consumer.name,
          consumer_package_root: portablePath(consumer.package_root),
          dependency,
          requested,
          declared_in: section,
          expected_package_root: portablePath(expected.package_root),
          expected_workspace_pattern: expected.workspace_pattern,
          installed_manifest_path: installed?.manifest_path ?? null,
          installed_package_root: installed ? portablePath(installed.package_root) : null,
          installed_realpath: installed?.realpath ?? null,
          status: installed === null
            ? 'unresolved'
            : resolvedToExpected
              ? 'resolved_to_declared_workspace_package'
              : 'resolved_outside_declared_workspace_package',
        });
      }
    }
  }
  return resolutions;
}

function dependencySections(manifest: PackageManifest): Array<[DependencyResolution['declared_in'], Record<string, unknown>]> {
  return [
    ['dependencies', manifest.dependencies ?? {}],
    ['devDependencies', manifest.devDependencies ?? {}],
    ['optionalDependencies', manifest.optionalDependencies ?? {}],
    ['peerDependencies', manifest.peerDependencies ?? {}],
  ];
}

function resolveInstalledPackage(
  consumerManifestPath: string,
  packageName: string,
): { manifest_path: string; package_root: string; realpath: string } | null {
  try {
    const linkedManifest = join(dirname(consumerManifestPath), 'node_modules', ...packageName.split('/'), 'package.json');
    if (existsSync(linkedManifest)) {
      const packageRoot = realpathSync(dirname(linkedManifest));
      const manifestPath = join(packageRoot, 'package.json');
      const manifest = readPackageManifest(manifestPath);
      if (manifest.name === packageName) {
        const realpath = portablePath(packageRoot);
        return { manifest_path: manifestPath, package_root: packageRoot, realpath };
      }
    }
    const require = createRequire(consumerManifestPath);
    let entrypoint: string;
    try {
      entrypoint = require.resolve(packageName + '/package.json');
    } catch {
      entrypoint = require.resolve(packageName);
    }
    const realEntrypoint = realpathSync(entrypoint);
    let current = dirname(realEntrypoint);
    while (true) {
      const manifestPath = join(current, 'package.json');
      if (existsSync(manifestPath)) {
        const manifest = readPackageManifest(manifestPath);
        if (manifest.name === packageName) {
          const realpath = portablePath(realpathSync(current));
          return { manifest_path: manifestPath, package_root: current, realpath };
        }
      }
      const parent = dirname(current);
      if (parent === current) return null;
      current = parent;
    }
  } catch {
    return null;
  }
}

function pathWithin(root: string, candidate: string): boolean {
  const normalizedRoot = resolve(root);
  const normalizedCandidate = resolve(candidate);
  return normalizedCandidate === normalizedRoot
    || normalizedCandidate.startsWith(normalizedRoot + '\\')
    || normalizedCandidate.startsWith(normalizedRoot + '/');
}

function samePath(left: string, right: string): boolean {
  return portablePath(left).toLowerCase() === portablePath(right).toLowerCase();
}

function portablePath(path: string): string {
  return resolve(path).replace(/\\/g, '/');
}

function unique(values: string[]): string[] {
  return [...new Set(values)];
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : null;
if (invokedPath === fileURLToPath(import.meta.url)) {
  const workspaceRoot = resolve(fileURLToPath(new URL('..', import.meta.url)));
  process.stdout.write(JSON.stringify(prepareWorkspaceBuild(workspaceRoot)) + '\n');
}
