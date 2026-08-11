import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { prepareWorkspaceBuild } from '../scripts/prepare-workspace-build.ts';

const repositoryRoot = fileURLToPath(new URL('../', import.meta.url));

test('routine workspace build never invokes destructive dist cleanup', () => {
  const packageJson = JSON.parse(readFileSync(join(repositoryRoot, 'package.json'), 'utf8')) as {
    scripts?: Record<string, string>;
  };
  const scripts = packageJson.scripts ?? {};
  assert.equal(scripts.build, 'pnpm run build:bun');
  for (const build of [scripts['build:bun'] ?? '', scripts['build:node'] ?? '']) {
    assert.match(build, /prepare-workspace-build\.ts/u);
    assert.match(build, /tsc -b --force/u);
    assert.doesNotMatch(build, /clean-workspace-dist|tsc -b --clean|rimraf|(?:^|\s)rm\s/u);
  }
  assert.equal(existsSync(join(repositoryRoot, 'scripts', 'clean-workspace-dist.ts')), false);
});

test('failed build preparation preserves the last successful runtime artifact', () => {
  const fixture = mkdtempSync(join(tmpdir(), 'mcp-surfaces-build-preparation-'));
  try {
    const projectRoot = join(fixture, 'packages', 'example-mcp');
    const sentinel = join(projectRoot, 'dist', 'src', 'main.js');
    mkdirSync(join(projectRoot, 'dist', 'src'), { recursive: true });
    writeFileSync(join(projectRoot, 'tsconfig.json'), '{}\n');
    writeFileSync(join(fixture, 'tsconfig.json'), '{"files":[],"references":[]}\n');
    writeFileSync(sentinel, 'last-successful-generation\n');

    assert.throws(
      () => prepareWorkspaceBuild(fixture),
      /workspace_tsconfig_reference_missing:packages\/example-mcp/u,
    );
    assert.equal(readFileSync(sentinel, 'utf8'), 'last-successful-generation\n');
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
});

test('successful build preparation is read-only and reports the availability posture', () => {
  const fixture = mkdtempSync(join(tmpdir(), 'mcp-surfaces-build-preparation-'));
  try {
    const projectRoot = join(fixture, 'packages', 'example-mcp');
    const sentinel = join(projectRoot, 'dist', 'src', 'main.js');
    mkdirSync(join(projectRoot, 'dist', 'src'), { recursive: true });
    writeFileSync(join(projectRoot, 'tsconfig.json'), '{}\n');
    writeFileSync(join(fixture, 'tsconfig.json'), JSON.stringify({
      files: [],
      references: [{ path: './packages/example-mcp' }],
    }));
    writeFileSync(sentinel, 'last-successful-generation\n');

    assert.deepEqual(prepareWorkspaceBuild(fixture), {
      schema: 'narada.workspace_build_preparation.v1',
      status: 'ready',
      project_count: 1,
      artifact_posture: 'preserve_last_successful_dist',
      external_workspace_package_provenance: {
        status: 'coherent',
        workspace_patterns: [],
        packages: [],
        dependency_resolutions: [],
        ambiguities: [],
      },
    });
    assert.equal(readFileSync(sentinel, 'utf8'), 'last-successful-generation\n');
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
});

test('successful build preparation records external workspace package provenance', () => {
  const fixture = mkdtempSync(join(tmpdir(), 'mcp-surfaces-build-preparation-'));
  const external = mkdtempSync(join(tmpdir(), 'mcp-surfaces-external-workspace-'));
  try {
    const projectRoot = join(fixture, 'packages', 'example-mcp');
    const externalPackageRoot = join(external, 'packages', 'external-mcp');
    const externalWorkspaceName = basename(external);
    mkdirSync(projectRoot, { recursive: true });
    mkdirSync(externalPackageRoot, { recursive: true });
    writeFileSync(join(projectRoot, 'tsconfig.json'), '{}\n');
    writeFileSync(join(fixture, 'tsconfig.json'), JSON.stringify({
      files: [],
      references: [{ path: './packages/example-mcp' }],
    }));
    writeFileSync(join(fixture, 'pnpm-workspace.yaml'), [
      'packages:',
      '  - packages/*',
      `  - ../${externalWorkspaceName}/packages/*`,
      '',
    ].join('\n'));
    writeFileSync(join(externalPackageRoot, 'package.json'), JSON.stringify({
      name: '@fixture/external-mcp',
      version: '1.0.0',
    }));

    const result = prepareWorkspaceBuild(fixture);
    assert.deepEqual(result.external_workspace_package_provenance, {
      status: 'coherent',
      workspace_patterns: [`../${externalWorkspaceName}/packages/*`],
      packages: [{
        name: '@fixture/external-mcp',
        version: '1.0.0',
        package_root: externalPackageRoot.split(String.fromCharCode(92)).join('/'),
        manifest_path: join(externalPackageRoot, 'package.json').split(String.fromCharCode(92)).join('/'),
        workspace_pattern: `../${externalWorkspaceName}/packages/*`,
      }],
      dependency_resolutions: [],
      ambiguities: [],
    });
  } finally {
    rmSync(fixture, { recursive: true, force: true });
    rmSync(external, { recursive: true, force: true });
  }
});

test('workspace preparation refuses unresolved external workspace provenance', () => {
  const fixture = mkdtempSync(join(tmpdir(), 'mcp-surfaces-build-preparation-'));
  const external = mkdtempSync(join(tmpdir(), 'mcp-surfaces-external-workspace-'));
  try {
    const projectRoot = join(fixture, 'packages', 'example-mcp');
    const externalPackageRoot = join(external, 'packages', 'external-mcp');
    const externalWorkspaceName = external.slice(external.lastIndexOf(String.fromCharCode(92)) + 1);
    mkdirSync(projectRoot, { recursive: true });
    mkdirSync(externalPackageRoot, { recursive: true });
    writeFileSync(join(projectRoot, 'tsconfig.json'), '{}\n');
    writeFileSync(join(projectRoot, 'package.json'), JSON.stringify({
      name: '@fixture/example-mcp',
      dependencies: { '@fixture/external-mcp': 'workspace:*' },
    }));
    writeFileSync(join(fixture, 'tsconfig.json'), JSON.stringify({
      files: [],
      references: [{ path: './packages/example-mcp' }],
    }));
    writeFileSync(join(fixture, 'pnpm-workspace.yaml'), [
      'packages:',
      '  - packages/*',
      `  - ../${externalWorkspaceName}/packages/*`,
      '',
    ].join('\n'));
    writeFileSync(join(externalPackageRoot, 'package.json'), JSON.stringify({
      name: '@fixture/external-mcp',
      version: '1.0.0',
      exports: { '.': './dist/index.js' },
    }));

    assert.throws(
      () => prepareWorkspaceBuild(fixture),
      /external_workspace_package_provenance_unresolved:@fixture\/example-mcp->@fixture\/external-mcp=unresolved/u,
    );
  } finally {
    rmSync(fixture, { recursive: true, force: true });
    rmSync(external, { recursive: true, force: true });
  }
});

test('workspace preparation refuses duplicate package names across local and external roots', () => {
  const fixture = mkdtempSync(join(tmpdir(), 'mcp-surfaces-build-preparation-'));
  const external = mkdtempSync(join(tmpdir(), 'mcp-surfaces-duplicate-workspace-'));
  try {
    const projectRoot = join(fixture, 'packages', 'local-mcp');
    const externalPackageRoot = join(external, 'packages', 'external-mcp');
    const externalWorkspaceName = external.slice(external.lastIndexOf(String.fromCharCode(92)) + 1);
    mkdirSync(projectRoot, { recursive: true });
    mkdirSync(externalPackageRoot, { recursive: true });
    writeFileSync(join(projectRoot, 'tsconfig.json'), '{}\n');
    writeFileSync(join(fixture, 'tsconfig.json'), JSON.stringify({
      files: [],
      references: [{ path: './packages/local-mcp' }],
    }));
    writeFileSync(join(fixture, 'pnpm-workspace.yaml'), [
      'packages:',
      '  - packages/*',
      `  - ../${externalWorkspaceName}/packages/*`,
      '',
    ].join('\n'));
    writeFileSync(join(projectRoot, 'package.json'), JSON.stringify({ name: '@fixture/duplicate' }));
    writeFileSync(join(externalPackageRoot, 'package.json'), JSON.stringify({ name: '@fixture/duplicate' }));

    assert.throws(
      () => prepareWorkspaceBuild(fixture),
      /workspace_package_name_ambiguous/u,
    );
  } finally {
    rmSync(fixture, { recursive: true, force: true });
    rmSync(external, { recursive: true, force: true });
  }
});