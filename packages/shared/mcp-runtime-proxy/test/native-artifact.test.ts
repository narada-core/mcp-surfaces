import assert from 'node:assert/strict';
import { closeSync, existsSync, openSync, readFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import {
  isNativeArtifactEntrypoint,
  nativeArtifactPointerPath,
  nativeArtifactRoot,
  publishImmutableNativeArtifacts,
  readNativeArtifactPointer,
  requireNativeArtifact,
  resolveNativeArtifact,
} from '../src/native-artifact.js';

test('native artifacts publish immutably and resolve through the current pointer', () => {
  const root = mkdtempSync(join(tmpdir(), 'native-artifact-rotation-'));
  try {
    const packageRoot = join(root, 'package');
    const source = join(root, 'narada-mcp-runtime.exe');
    mkdirSync(packageRoot, { recursive: true });
    writeFileSync(source, 'generation-one', 'utf8');

    const first = publishImmutableNativeArtifacts({
      packageRoot,
      artifacts: [{ name: 'narada-mcp-runtime.exe', source }],
      generatedAt: '2026-08-07T00:00:00.000Z',
    });
    const firstPath = resolve(nativeArtifactRoot(packageRoot), first.artifacts['narada-mcp-runtime.exe']);
    assert.equal(readFileSync(firstPath, 'utf8'), 'generation-one');
    assert.equal(resolveNativeArtifact(packageRoot, 'narada-mcp-runtime.exe'), firstPath);
    assert.equal(isNativeArtifactEntrypoint(packageRoot, 'narada-mcp-runtime.exe', firstPath), true);

    const legacyPath = join(nativeArtifactRoot(packageRoot), 'narada-mcp-runtime.exe');
    writeFileSync(legacyPath, 'stale-legacy-generation', 'utf8');
    writeFileSync(source, 'generation-two', 'utf8');
    const oldHandle = openSync(firstPath, 'r');
    const second = publishImmutableNativeArtifacts({
      packageRoot,
      artifacts: [{ name: 'narada-mcp-runtime.exe', source }],
      generatedAt: '2026-08-07T00:00:01.000Z',
    });
    closeSync(oldHandle);

    const secondPath = resolve(nativeArtifactRoot(packageRoot), second.artifacts['narada-mcp-runtime.exe']);
    assert.notEqual(secondPath, firstPath);
    assert.equal(readFileSync(firstPath, 'utf8'), 'generation-one');
    assert.equal(readFileSync(secondPath, 'utf8'), 'generation-two');
    assert.equal(existsSync(legacyPath), false);
    assert.equal(resolveNativeArtifact(packageRoot, 'narada-mcp-runtime.exe'), secondPath);
    assert.equal(isNativeArtifactEntrypoint(packageRoot, 'narada-mcp-runtime.exe', firstPath), true);
    assert.equal(isNativeArtifactEntrypoint(packageRoot, 'narada-mcp-runtime.exe', legacyPath), false);
    assert.equal(readNativeArtifactPointer(packageRoot)?.build_fingerprint, second.build_fingerprint);
    assert.equal(JSON.parse(readFileSync(nativeArtifactPointerPath(packageRoot), 'utf8')).schema, 'narada.mcp_runtime_proxy.native_artifact_pointer.v1');
    rmSync(nativeArtifactPointerPath(packageRoot), { force: true });
    assert.equal(resolveNativeArtifact(packageRoot, 'narada-mcp-runtime.exe'), null);
    assert.throws(() => requireNativeArtifact(packageRoot, 'narada-mcp-runtime.exe'), /native_artifact_pointer_unavailable/);
    assert.equal(resolveNativeArtifact(packageRoot, '../outside.exe'), null);
    assert.equal(isNativeArtifactEntrypoint(packageRoot, '../outside.exe', firstPath), false);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
