import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  CommandResolutionError,
  resolveCommandInvocation,
} from '../src/command-resolution.js';
import { spawnStructured } from '../src/main.js';

function windowsFixture(): string {
  return mkdtempSync(join(tmpdir(), 'structured-command-resolution-'));
}

{
  const directory = windowsFixture();
  const scriptPath = join(directory, 'tool.ps1');
  const hostPath = join(directory, 'pwsh.exe');
  writeFileSync(scriptPath, 'Write-Output tool');
  writeFileSync(hostPath, 'fixture host');

  const invocation = resolveCommandInvocation('tool', ['one'], {
    cwd: directory,
    env: { Path: directory },
    platform: 'win32',
  });

  assert.equal(invocation.evidence.status, 'resolved');
  assert.equal(invocation.evidence.invocation_kind, 'powershell_script');
  assert.equal(invocation.evidence.resolved_path, scriptPath);
  assert.equal(invocation.command, hostPath);
  assert.deepEqual(invocation.args, [
    '-NoLogo',
    '-NoProfile',
    '-NonInteractive',
    '-ExecutionPolicy',
    'Bypass',
    '-File',
    scriptPath,
    'one',
  ]);
}

{
  const directory = windowsFixture();
  writeFileSync(join(directory, 'tool.cmd'), '@echo off');

  assert.throws(
    () => resolveCommandInvocation('tool', [], {
      cwd: directory,
      env: { Path: directory },
      platform: 'win32',
    }),
    (error: unknown) => error instanceof CommandResolutionError
      && error.codeName === 'command_wrapper_unsupported'
      && error.evidence.status === 'unresolved',
  );
}

{
  const directory = windowsFixture();

  assert.throws(
    () => resolveCommandInvocation('missing-tool', [], {
      cwd: directory,
      env: { Path: directory },
      platform: 'win32',
    }),
    (error: unknown) => error instanceof CommandResolutionError
      && error.codeName === 'command_not_found'
      && error.evidence.status === 'unresolved',
  );
}

{
  const result = await spawnStructured('pnpm', ['--version'], {
    cwd: process.cwd(),
    timeoutMs: 10_000,
    maxOutputBytes: 2_000,
    env: process.env,
  });

  assert.equal(result.command_resolution.status, 'resolved');
  assert.equal(result.resolution_error_code, null);
  assert.equal(result.exit_code, 0, result.stderr);
  assert.match(result.stdout, /\d+\.\d+/);
}

console.log('structured command resolution tests passed');
