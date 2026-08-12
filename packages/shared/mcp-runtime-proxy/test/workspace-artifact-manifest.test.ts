import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, unlinkSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import {
  buildWorkspaceArtifactManifest,
  preflightWorkspaceArtifacts,
} from '../src/workspace-artifact-manifest.js';

test('workspace artifact manifest refuses missing, stale, and missing export artifacts', () => {
  const root = mkdtempSync(join(tmpdir(), 'workspace-artifact-manifest-'));
  try {
    const packageRoot = join(root, 'package');
    const sourceRoot = join(packageRoot, 'src');
    const runtimeRoot = join(packageRoot, 'dist', 'src');
    mkdirSync(sourceRoot, { recursive: true });
    mkdirSync(runtimeRoot, { recursive: true });
    writeFileSync(join(packageRoot, 'package.json'), JSON.stringify({
      name: '@test/workspace-package',
      version: '1.0.0',
      type: 'module',
      exports: {
        '.': {
          types: './dist/src/server.d.ts',
          import: './dist/src/server.js',
        },
      },
    }), 'utf8');
    writeFileSync(join(packageRoot, 'tsconfig.json'), '{}', 'utf8');
    writeFileSync(join(sourceRoot, 'server.ts'), 'export const value = 1;\n', 'utf8');
    const entrypoint = join(runtimeRoot, 'server.js');
    const declaration = join(runtimeRoot, 'server.d.ts');
    writeFileSync(entrypoint, 'export const value = 1;\n', 'utf8');
    writeFileSync(declaration, 'export declare const value: number;\n', 'utf8');
    const manifestPath = join(root, 'manifest.json');
    const manifest = buildWorkspaceArtifactManifest({
      workspaceRoot: root,
      packageRoots: [packageRoot],
      outputPath: manifestPath,
    });
    assert.equal(manifest.packages.length, 1);
    assert.equal(preflightWorkspaceArtifacts({
      surfaceId: 'test',
      entrypoint,
      artifactManifestPath: manifestPath,
    }).status, 'ok');

    const regeneratedWithoutChanges = buildWorkspaceArtifactManifest({
      workspaceRoot: root,
      packageRoots: [packageRoot],
      outputPath: manifestPath,
    });
    assert.equal(regeneratedWithoutChanges.manifest_fingerprint, manifest.manifest_fingerprint);
    assert.equal(preflightWorkspaceArtifacts({
      surfaceId: 'test',
      entrypoint,
      artifactManifestPath: manifestPath,
    }).status, 'ok');

    utimesSync(join(sourceRoot, 'server.ts'), new Date(0), new Date(0));
    const regeneratedAfterMtimeOnlyChange = buildWorkspaceArtifactManifest({
      workspaceRoot: root,
      packageRoots: [packageRoot],
      outputPath: manifestPath,
    });
    assert.equal(regeneratedAfterMtimeOnlyChange.manifest_fingerprint, manifest.manifest_fingerprint);

    writeFileSync(join(sourceRoot, 'server.ts'), 'export const value = 2;\n', 'utf8');
    const stale = preflightWorkspaceArtifacts({
      surfaceId: 'test',
      entrypoint,
      artifactManifestPath: manifestPath,
    });
    assert.equal(stale.code, 'workspace_manifest_stale');

    const regenerated = buildWorkspaceArtifactManifest({
      workspaceRoot: root,
      packageRoots: [packageRoot],
      outputPath: manifestPath,
    });
    assert.equal(regenerated.manifest_fingerprint.length, 64);
    assert.notEqual(regenerated.manifest_fingerprint, manifest.manifest_fingerprint);
    unlinkSync(entrypoint);
    const missing = preflightWorkspaceArtifacts({
      surfaceId: 'test',
      entrypoint,
      artifactManifestPath: manifestPath,
    });
    assert.equal(missing.code, 'workspace_export_target_missing');

    const noManifest = preflightWorkspaceArtifacts({
      surfaceId: 'test',
      entrypoint,
      artifactManifestPath: join(root, 'missing.json'),
    });
    assert.equal(noManifest.code, 'workspace_manifest_missing');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});


test('only the native artifacts selected by current.json are admitted', () => {
  const root = mkdtempSync(join(tmpdir(), 'workspace-native-artifacts-'));
  try {
    const packageRoot = join(root, 'package');
    const versionOne = join(packageRoot, 'dist', 'native', 'versions', 'one', 'narada-mcp-runtime.exe');
    const versionTwo = join(packageRoot, 'dist', 'native', 'versions', 'two', 'narada-mcp-runtime.exe');
    mkdirSync(join(packageRoot, 'dist', 'native', 'versions', 'one'), { recursive: true });
    mkdirSync(join(packageRoot, 'dist', 'native', 'versions', 'two'), { recursive: true });
    writeFileSync(join(packageRoot, 'package.json'), JSON.stringify({
      name: '@test/versioned-native-package',
      version: '1.0.0',
      type: 'module',
      naradaRuntimeArtifacts: { [process.platform]: ['./dist/native/current.json', './dist/native/versions/*'] },
    }), 'utf8');
    writeFileSync(versionOne, 'generation-one', 'utf8');
    writeFileSync(versionTwo, 'generation-two', 'utf8');
    writeFileSync(join(packageRoot, 'dist', 'native', 'current.json'), JSON.stringify({
      artifacts: { 'narada-mcp-runtime.exe': 'versions/two/narada-mcp-runtime.exe' },
    }), 'utf8');
    const manifestPath = join(root, 'manifest.json');
    buildWorkspaceArtifactManifest({ workspaceRoot: root, packageRoots: [packageRoot], outputPath: manifestPath });
    writeFileSync(join(packageRoot, 'dist', 'native', 'current.json'), JSON.stringify({
      generated_at: '2099-01-01T00:00:00Z',
      artifacts: { 'narada-mcp-runtime.exe': 'versions/two/narada-mcp-runtime.exe' },
    }), 'utf8');
    assert.equal(preflightWorkspaceArtifacts({ surfaceId: 'native-one', entrypoint: versionOne, artifactManifestPath: manifestPath }).code, 'workspace_artifact_missing');
    assert.equal(preflightWorkspaceArtifacts({ surfaceId: 'native-two', entrypoint: versionTwo, artifactManifestPath: manifestPath }).status, 'ok');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
