import { createHash } from 'node:crypto';
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { basename, isAbsolute, join, relative, resolve, sep } from 'node:path';

export const NATIVE_ARTIFACT_POINTER_SCHEMA = 'narada.mcp_runtime_proxy.native_artifact_pointer.v1';

export type NativeArtifactPointer = {
  schema: typeof NATIVE_ARTIFACT_POINTER_SCHEMA;
  generated_at: string;
  build_fingerprint: string;
  artifacts: Record<string, string>;
};

export type NativeArtifactSource = {
  name: string;
  source: string;
};

export function nativeArtifactRoot(packageRoot: string): string {
  return resolve(packageRoot, 'dist', 'native');
}

export function nativeArtifactPointerPath(packageRoot: string): string {
  return join(nativeArtifactRoot(packageRoot), 'current.json');
}

export function readNativeArtifactPointer(packageRoot: string): NativeArtifactPointer | null {
  const path = nativeArtifactPointerPath(packageRoot);
  if (!existsSync(path)) return null;
  try {
    const parsed: unknown = JSON.parse(readFileSync(path, 'utf8'));
    if (!isRecord(parsed)
      || parsed.schema !== NATIVE_ARTIFACT_POINTER_SCHEMA
      || typeof parsed.generated_at !== 'string'
      || typeof parsed.build_fingerprint !== 'string'
      || !isRecord(parsed.artifacts)) {
      return null;
    }
    const artifacts: Record<string, string> = {};
    for (const [name, target] of Object.entries(parsed.artifacts)) {
      if (typeof target !== 'string' || !versionedArtifactPath(packageRoot, target, name)) return null;
      artifacts[name] = target;
    }
    return {
      schema: NATIVE_ARTIFACT_POINTER_SCHEMA,
      generated_at: parsed.generated_at,
      build_fingerprint: parsed.build_fingerprint,
      artifacts,
    };
  } catch {
    return null;
  }
}

export function resolveNativeArtifact(packageRoot: string, artifactName: string): string | null {
  if (!isSafeArtifactName(artifactName)) return null;
  const pointer = readNativeArtifactPointer(packageRoot);
  const pointedTarget = pointer?.artifacts[artifactName];
  const pointedPath = pointedTarget
    ? versionedArtifactPath(packageRoot, pointedTarget, artifactName)
    : null;
  return pointedPath && existsSync(pointedPath) ? pointedPath : null;
}

export function requireNativeArtifact(packageRoot: string, artifactName: string): string {
  const artifact = resolveNativeArtifact(packageRoot, artifactName);
  if (!artifact) throw new Error(`mcp_runtime_proxy_native_artifact_pointer_unavailable:${artifactName}`);
  return artifact;
}

export function isNativeArtifactEntrypoint(
  packageRoot: string,
  artifactName: string,
  entrypoint: string,
): boolean {
  if (!isSafeArtifactName(artifactName)) return false;
  const candidate = resolve(entrypoint);
  const current = resolveNativeArtifact(packageRoot, artifactName);
  if (current && samePath(current, candidate)) return true;

  const versioned = versionedArtifactPath(packageRoot, relative(nativeArtifactRoot(packageRoot), candidate), artifactName);
  return versioned !== null && samePath(versioned, candidate);
}

export function nativeArtifactBuildFingerprint(artifacts: NativeArtifactSource[]): string {
  const hash = createHash('sha256');
  for (const artifact of [...artifacts].sort((left, right) => left.name.localeCompare(right.name))) {
    hash.update(artifact.name);
    hash.update('\0');
    hash.update(readFileSync(artifact.source));
    hash.update('\0');
  }
  return hash.digest('hex');
}

export function publishImmutableNativeArtifacts(input: {
  packageRoot: string;
  artifacts: NativeArtifactSource[];
  generatedAt?: string;
}): NativeArtifactPointer {
  const packageRoot = resolve(input.packageRoot);
  const nativeRoot = nativeArtifactRoot(packageRoot);
  const artifacts = [...input.artifacts].sort((left, right) => left.name.localeCompare(right.name));
  if (artifacts.some((artifact) => !isSafeArtifactName(artifact.name))) {
    throw new Error('mcp_runtime_proxy_native_artifact_name_invalid');
  }
  for (const artifact of artifacts) {
    if (!existsSync(artifact.source)) throw new Error(`mcp_runtime_proxy_native_artifact_missing:${artifact.source}`);
  }
  const buildFingerprint = nativeArtifactBuildFingerprint(artifacts);
  const versionRoot = join(nativeRoot, 'versions', buildFingerprint);
  mkdirSync(versionRoot, { recursive: true });
  const pointerArtifacts: Record<string, string> = {};
  for (const artifact of artifacts) {
    const destination = join(versionRoot, artifact.name);
    copyImmutableFile(artifact.source, destination);
    pointerArtifacts[artifact.name] = relative(nativeRoot, destination).split(sep).join('/');
  }
  for (const artifact of artifacts) {
    const legacyAlias = join(nativeRoot, artifact.name);
    try {
      rmSync(legacyAlias, { force: true });
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code;
      if (process.platform !== 'win32' || (code !== 'EPERM' && code !== 'EBUSY')) throw error;
      // A resident process may still hold this retired, non-authoritative alias.
      // Publication remains safe because all resolution uses the immutable pointer below.
    }
  }
  const pointer: NativeArtifactPointer = {
    schema: NATIVE_ARTIFACT_POINTER_SCHEMA,
    generated_at: input.generatedAt ?? new Date().toISOString(),
    build_fingerprint: buildFingerprint,
    artifacts: pointerArtifacts,
  };
  writeJsonAtomically(nativeArtifactPointerPath(packageRoot), pointer);
  return pointer;
}

function versionedArtifactPath(packageRoot: string, target: string, artifactName: string): string | null {
  if (!isSafeArtifactName(artifactName) || !target || basename(target) !== artifactName) return null;
  const nativeRoot = nativeArtifactRoot(packageRoot);
  const candidate = resolve(nativeRoot, target);
  const versionsRoot = resolve(nativeRoot, 'versions');
  if (!isPathInside(versionsRoot, candidate)) return null;
  return candidate;
}

function isSafeArtifactName(name: string): boolean {
  return name.length > 0 && name === basename(name) && name !== '.' && name !== '..';
}

function copyImmutableFile(source: string, destination: string): void {
  mkdirSync(resolve(destination, '..'), { recursive: true });
  if (existsSync(destination)) {
    if (!sameFileContent(source, destination)) {
      throw new Error(`mcp_runtime_proxy_native_artifact_collision:${destination}`);
    }
    return;
  }
  const temporary = `${destination}.tmp-${process.pid}-${Date.now()}`;
  copyFileSync(source, temporary);
  try {
    try {
      renameSync(temporary, destination);
    } catch (error) {
      if (!existsSync(destination)) throw error;
      if (!sameFileContent(source, destination)) throw error;
    }
  } finally {
    if (existsSync(temporary)) rmSync(temporary, { force: true });
  }
}

function writeJsonAtomically(path: string, value: unknown): void {
  mkdirSync(resolve(path, '..'), { recursive: true });
  const temporary = `${path}.tmp-${process.pid}-${Date.now()}`;
  writeFileSync(temporary, `${JSON.stringify(value, null, 2)}\n`, 'utf8');
  try {
    try {
      renameSync(temporary, path);
    } catch (error) {
      if (!existsSync(path)) throw error;
      rmSync(path, { force: true });
      renameSync(temporary, path);
    }
  } finally {
    if (existsSync(temporary)) rmSync(temporary, { force: true });
  }
}

function sameFileContent(left: string, right: string): boolean {
  return createHash('sha256').update(readFileSync(left)).digest('hex')
    === createHash('sha256').update(readFileSync(right)).digest('hex');
}

function samePath(left: string, right: string): boolean {
  return process.platform === 'win32'
    ? left.toLowerCase() === right.toLowerCase()
    : left === right;
}

function isPathInside(parent: string, candidate: string): boolean {
  const relativePath = relative(parent, candidate);
  return relativePath === ''
    || (!isAbsolute(relativePath) && !relativePath.startsWith(`..${sep}`) && relativePath !== '..');
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
