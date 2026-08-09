import {
  accessSync,
  constants,
  existsSync,
  statSync,
} from 'node:fs';
import {
  delimiter,
  extname,
  isAbsolute,
  resolve,
} from 'node:path';

export type CommandInvocationKind = 'direct' | 'powershell_script';

export type CommandResolutionEvidence =
  | {
      schema: 'narada.structured_command.command_resolution.v1';
      status: 'resolved';
      requested_command: string;
      invocation_kind: CommandInvocationKind;
      resolved_path: string;
      spawn_command: string;
      wrapper: {
        kind: 'powershell';
        path: string;
      } | null;
    }
  | {
      schema: 'narada.structured_command.command_resolution.v1';
      status: 'unresolved';
      requested_command: string;
      code: string;
      message: string;
      searched_candidate_count: number;
      remediation: string;
    }
  | {
      schema: 'narada.structured_command.command_resolution.v1';
      status: 'not_attempted';
      requested_command: string;
      reason: string;
    };

export interface ResolvedCommandInvocation {
  command: string;
  args: string[];
  evidence: Extract<CommandResolutionEvidence, { status: 'resolved' }>;
}

export interface CommandResolutionOptions {
  cwd: string;
  env?: NodeJS.ProcessEnv;
  platform?: NodeJS.Platform;
}

export class CommandResolutionError extends Error {
  readonly codeName: string;
  readonly evidence: Extract<CommandResolutionEvidence, { status: 'unresolved' }>;

  constructor(
    codeName: string,
    requestedCommand: string,
    message: string,
    searchedCandidateCount: number,
    remediation: string,
  ) {
    super(message);
    this.name = 'CommandResolutionError';
    this.codeName = codeName;
    this.evidence = {
      schema: 'narada.structured_command.command_resolution.v1',
      status: 'unresolved',
      requested_command: requestedCommand,
      code: codeName,
      message,
      searched_candidate_count: searchedCandidateCount,
      remediation,
    };
  }
}

const WINDOWS_CANDIDATE_EXTENSIONS = ['.exe', '.com', '.ps1', '.cmd', '.bat', ''] as const;

function environmentValue(env: NodeJS.ProcessEnv, name: string): string | undefined {
  const matchingKey = Object.keys(env).find((key) => key.toLowerCase() === name.toLowerCase());
  return matchingKey === undefined ? undefined : env[matchingKey];
}

function isRunnableFile(path: string, platform: NodeJS.Platform): boolean {
  if (!existsSync(path)) return false;
  try {
    if (!statSync(path).isFile()) return false;
    if (platform !== 'win32') accessSync(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function hasPathSyntax(command: string): boolean {
  return isAbsolute(command) || command.includes('/') || command.includes('\\');
}

function commandCandidates(
  command: string,
  cwd: string,
  env: NodeJS.ProcessEnv,
  platform: NodeJS.Platform,
): string[] {
  const roots = hasPathSyntax(command)
    ? [isAbsolute(command) ? '' : cwd]
    : (environmentValue(env, 'PATH') ?? '')
      .split(platform === 'win32' ? ';' : delimiter)
      .filter(Boolean);

  const extension = extname(command);
  const suffixes = platform === 'win32' && extension.length === 0
    ? WINDOWS_CANDIDATE_EXTENSIONS
    : [''];

  const candidates: string[] = [];
  for (const root of roots) {
    const base = isAbsolute(command) ? command : resolve(root, command);
    for (const suffix of suffixes) candidates.push(`${base}${suffix}`);
  }
  return [...new Set(candidates)];
}

function findCommandPath(
  command: string,
  cwd: string,
  env: NodeJS.ProcessEnv,
  platform: NodeJS.Platform,
): { path: string | null; searchedCandidateCount: number } {
  const candidates = commandCandidates(command, cwd, env, platform);
  return {
    path: candidates.find((candidate) => isRunnableFile(candidate, platform)) ?? null,
    searchedCandidateCount: candidates.length,
  };
}

function resolvePowerShellHost(
  requestedCommand: string,
  cwd: string,
  env: NodeJS.ProcessEnv,
  platform: NodeJS.Platform,
  searchedCandidateCount: number,
): { path: string; searchedCandidateCount: number } {
  let total = searchedCandidateCount;
  for (const candidate of ['pwsh.exe', 'powershell.exe']) {
    const resolved = findCommandPath(candidate, cwd, env, platform);
    total += resolved.searchedCandidateCount;
    if (resolved.path !== null) return { path: resolved.path, searchedCandidateCount: total };
  }
  throw new CommandResolutionError(
    'powershell_host_not_found',
    requestedCommand,
    `No PowerShell host is available to execute the resolved script for ${requestedCommand}`,
    total,
    'Install PowerShell 7 (pwsh.exe) or make powershell.exe available on PATH.',
  );
}

function powershellInvocation(
  requestedCommand: string,
  scriptPath: string,
  args: readonly string[],
  cwd: string,
  env: NodeJS.ProcessEnv,
  platform: NodeJS.Platform,
  searchedCandidateCount: number,
): ResolvedCommandInvocation {
  const host = resolvePowerShellHost(
    requestedCommand,
    cwd,
    env,
    platform,
    searchedCandidateCount,
  );
  return {
    command: host.path,
    args: [
      '-NoLogo',
      '-NoProfile',
      '-NonInteractive',
      '-ExecutionPolicy',
      'Bypass',
      '-File',
      scriptPath,
      ...args,
    ],
    evidence: {
      schema: 'narada.structured_command.command_resolution.v1',
      status: 'resolved',
      requested_command: requestedCommand,
      invocation_kind: 'powershell_script',
      resolved_path: scriptPath,
      spawn_command: host.path,
      wrapper: {
        kind: 'powershell',
        path: host.path,
      },
    },
  };
}

export function commandResolutionNotAttempted(
  requestedCommand: string,
  reason: string,
): Extract<CommandResolutionEvidence, { status: 'not_attempted' }> {
  return {
    schema: 'narada.structured_command.command_resolution.v1',
    status: 'not_attempted',
    requested_command: requestedCommand,
    reason,
  };
}

export function resolveCommandInvocation(
  requestedCommand: string,
  args: readonly string[],
  options: CommandResolutionOptions,
): ResolvedCommandInvocation {
  const command = requestedCommand.trim();
  if (command.length === 0) {
    throw new CommandResolutionError(
      'command_name_empty',
      requestedCommand,
      'The admitted command name is empty.',
      0,
      'Supply a non-empty command name admitted by the execution policy.',
    );
  }

  const env = options.env ?? process.env;
  const platform = options.platform ?? process.platform;
  const resolved = findCommandPath(command, options.cwd, env, platform);
  if (resolved.path === null) {
    throw new CommandResolutionError(
      'command_not_found',
      requestedCommand,
      `The admitted command could not be resolved without a shell: ${requestedCommand}`,
      resolved.searchedCandidateCount,
      'Install the command or add its executable directory to PATH, then retry.',
    );
  }

  if (platform === 'win32') {
    const extension = extname(resolved.path).toLowerCase();
    if (extension === '.ps1') {
      return powershellInvocation(
        requestedCommand,
        resolved.path,
        args,
        options.cwd,
        env,
        platform,
        resolved.searchedCandidateCount,
      );
    }
    if (extension === '.cmd' || extension === '.bat') {
      const siblingPowerShell = resolved.path.slice(0, -extension.length) + '.ps1';
      if (isRunnableFile(siblingPowerShell, platform)) {
        return powershellInvocation(
          requestedCommand,
          siblingPowerShell,
          args,
          options.cwd,
          env,
          platform,
          resolved.searchedCandidateCount + 1,
        );
      }
      throw new CommandResolutionError(
        'command_wrapper_unsupported',
        requestedCommand,
        `The admitted command resolves only to a ${extension} wrapper, which cannot be executed safely with shell=false: ${resolved.path}`,
        resolved.searchedCandidateCount + 1,
        'Provide a native executable or a sibling PowerShell shim; shell-string execution remains disabled.',
      );
    }
  }

  return {
    command: resolved.path,
    args: [...args],
    evidence: {
      schema: 'narada.structured_command.command_resolution.v1',
      status: 'resolved',
      requested_command: requestedCommand,
      invocation_kind: 'direct',
      resolved_path: resolved.path,
      spawn_command: resolved.path,
      wrapper: null,
    },
  };
}
