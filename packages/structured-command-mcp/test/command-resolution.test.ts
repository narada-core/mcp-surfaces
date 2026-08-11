import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
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
  assert.deepEqual(invocation.evidence.spawn_args, invocation.args);
  assert.deepEqual(invocation.evidence.invocation_argv, [hostPath, ...invocation.args]);
}

{
  const directory = windowsFixture();
  const entrypoint = join(directory, 'node_modules', 'corepack', 'dist', 'pnpm.js');
  mkdirSync(join(directory, 'node_modules', 'corepack', 'dist'), { recursive: true });
  writeFileSync(join(directory, 'node.exe'), 'fixture node');
  writeFileSync(entrypoint, 'fixture entrypoint');
  writeFileSync(
    join(directory, 'pnpm.ps1'),
    '& "$basedir/node$exe" "$basedir/node_modules/corepack/dist/pnpm.js" $args',
  );

  const invocation = resolveCommandInvocation('pnpm', ['--version'], {
    cwd: directory,
    env: { Path: directory },
    platform: 'win32',
  });

  assert.equal(invocation.evidence.invocation_kind, 'node_script_shim');
  assert.equal(invocation.evidence.resolved_path, entrypoint);
  assert.deepEqual(invocation.args, [entrypoint, '--version']);
}

{
  const emptyDirectory = windowsFixture();
  const validDirectory = windowsFixture();
  writeFileSync(join(emptyDirectory, 'tool.ps1'), '');
  const validScriptPath = join(validDirectory, 'tool.ps1');
  writeFileSync(validScriptPath, 'Write-Output tool');
  writeFileSync(join(validDirectory, 'pwsh.exe'), 'fixture host');

  const invocation = resolveCommandInvocation('tool', [], {
    cwd: emptyDirectory,
    env: { Path: `${emptyDirectory};${validDirectory}` },
    platform: 'win32',
  });

  assert.equal(invocation.evidence.resolved_path, validScriptPath);
}

{
  const directory = windowsFixture();
  const aliasDirectory = join(directory, 'WindowsApps');
  const realHostDirectory = windowsFixture();
  mkdirSync(aliasDirectory);
  writeFileSync(join(directory, 'tool.ps1'), 'Write-Output tool');
  writeFileSync(join(aliasDirectory, 'pwsh.exe'), 'brokered alias');
  const realHostPath = join(realHostDirectory, 'pwsh.exe');
  writeFileSync(realHostPath, 'real host');

  const invocation = resolveCommandInvocation('tool', [], {
    cwd: directory,
    env: { Path: `${directory};${aliasDirectory};${realHostDirectory}` },
    platform: 'win32',
  });

  assert.equal(invocation.evidence.wrapper?.path, realHostPath);
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
  assert.equal(result.command_resolution.spawn_args.at(-1), '--version');
  assert.deepEqual(
    result.command_resolution.invocation_argv,
    [result.command_resolution.spawn_command, ...result.command_resolution.spawn_args],
  );
}

console.log('structured command resolution tests passed');
