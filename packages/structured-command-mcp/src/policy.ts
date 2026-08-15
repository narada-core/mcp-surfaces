import { existsSync, readFileSync, statSync } from 'node:fs';
import { extname, relative, resolve } from 'node:path';

// The normal default policy is bounded at fifteen minutes. Sites may select a
// lower maxTimeoutMs for more restrictive deployments.
const DEFAULT_MAX_TIMEOUT_MS = 900_000;

const DEFAULT_BLOCKED_COMMANDS = new Set([
  'cmd',
  'cmd.exe',

  'powershell',
  'powershell.exe',
  'wsl',
  'wsl.exe',
]);

const DEFAULT_ALLOWED_COMMANDS = new Set([
  'railway',
  'wrangler',
]);

const DEFAULT_ALLOWED_PREFIXES = [
  ['pnpm', 'test'],
  ['pnpm', 'build'],
  ['pnpm', 'typecheck'],
  ['pnpm', '--filter'],
  ['cargo', 'fmt'],
  ['cargo', 'check'],
  ['cargo', 'test'],
  ['cargo', 'build'],
  ['cargo', 'native-build'],
  ['cargo', 'native-test'],
  ['cargo', 'native-package'],
  ['cargo', 'native-materialize'],
  ['cargo', 'native-release'],
  ['cargo', 'native-verify'],
  ['narada', 'launcher', 'workspace-plan'],
  ['narada', 'doctor'],
  ['pwsh', '-file'],
  ['pwsh', '-noprofile', '-file'],
  ['pwsh', '-noprofile', '-executionpolicy', 'bypass', '-file'],
];

const DISALLOWED_WRAPPER_EXTENSIONS = new Set(['.cmd', '.bat']);
const TRANSIENT_WRAPPER_PATH = /(^|\/)\.ai\/(?:tmp|temp)(?:\/|$)/i;

export function createExecutionPolicy(options: unknown = {}) {
  const optionsRecord = asRecord(options);
  const allowedRoots = normalizeAllowedRoots(optionsRecord.allowedRoots);
  const allowedCommands = new Set([...DEFAULT_ALLOWED_COMMANDS, ...normalizeList(optionsRecord.allowedCommands).map((item: any) => item.toLowerCase())]);
  const allowedPrefixes = [...DEFAULT_ALLOWED_PREFIXES, ...normalizeList(optionsRecord.allowedPrefixes).map((prefix: any) => normalizePrefix(prefix))];
  const blockedCommands = new Set([...DEFAULT_BLOCKED_COMMANDS, ...normalizeList(optionsRecord.blockedCommands).map((item: any) => item.toLowerCase())]);
  return {
    allowedRoots,
    allowedCommands,
    defaultAllowedCommands: DEFAULT_ALLOWED_COMMANDS,
    allowedPrefixes,
    defaultAllowedPrefixes: DEFAULT_ALLOWED_PREFIXES,
    blockedCommands,
    maxTimeoutMs: clampInteger(optionsRecord.maxTimeoutMs, 1, DEFAULT_MAX_TIMEOUT_MS, DEFAULT_MAX_TIMEOUT_MS),
    maxOutputBytes: clampInteger(optionsRecord.maxOutputBytes, 1, 20 * 1024 * 1024, 1024 * 1024),
  };

}

function wrapperExecutionReasons(argv: any, cwd: any, policy: any) {
  const reasons = [];
  for (const rawValue of argv) {
    const value = String(rawValue ?? '').trim().replace(/^['"]|['"]$/g, '');
    if (!value) continue;
    const normalized = value.replaceAll('\\', '/');
    const extension = extname(normalized).toLowerCase();
    if (DISALLOWED_WRAPPER_EXTENSIONS.has(extension)) {
      const candidate = resolve(cwd, value);
      const canonicalRepositoryWrapper = !TRANSIENT_WRAPPER_PATH.test(normalized)
        && isInsideAnyRoot(candidate, policy.allowedRoots)
        && existsSync(candidate)
        && statSync(candidate).isFile();
      if (!canonicalRepositoryWrapper) reasons.push(`wrapper_execution_disallowed:${value}`);
      continue;
    }
    if (TRANSIENT_WRAPPER_PATH.test(normalized) && ['.ps1', '.psm1', '.js', '.mjs', '.cjs', '.ts'].includes(extension)) {
      reasons.push(`transient_wrapper_path_disallowed:${value}`);
    }
  }
  return [...new Set(reasons)];
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

export function parseTrustedProjectRootsFromTrustConfig(configPath: any) {
  const source = readFileSync(configPath, 'utf8');
  const roots = [];
  let currentProject = null;
  for (const rawLine of source.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith('#')) continue;
    const header = line.match(/^\[projects\.'([^']+)'\]$/i) ?? line.match(/^\[projects\."([^"]+)"\]$/i);
    if (header) {
      currentProject = header[1];
      continue;
    }
    if (line.startsWith('[')) {
      currentProject = null;
      continue;
    }
    if (!currentProject) continue;
    const trust = line.match(/^trust_level\s*=\s*"([^"]+)"$/i);
    if (trust && trust[1].toLowerCase() === 'trusted') roots.push(currentProject);
  }
  return normalizeAllowedRoots(roots);
}

export function buildAllowedRoots({ trustConfigPaths = [], explicitRoots = [] } : any= {}) {
  const roots = [];
  for (const configPath of normalizeList(trustConfigPaths)) {
    roots.push(...parseTrustedProjectRootsFromTrustConfig(configPath));
  }
  roots.push(...normalizeList(explicitRoots));
  return normalizeAllowedRoots(roots);
}

export function normalizeAllowedRoots(roots: any) {
  const seen = new Set();
  const normalized = [];
  for (const root of normalizeList(roots)) {
    const resolved = resolve(root.trim());
    const key = resolved.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    normalized.push(resolved);
  }
  return normalized;
}

export function decideStructuredCommandExecution({ command, args = [], workingDirectory }: any, policy: any) {
  const normalizedCommand = normalizeCommand(command);
  const argv = [normalizedCommand, ...normalizeArgs(args)];
  const cwd = resolve(workingDirectory ?? '.');
  const reasons = [];

  if (!normalizedCommand) reasons.push('command_required');
  if (policy.blockedCommands.has(normalizedCommand.toLowerCase())) reasons.push(`blocked_command:${normalizedCommand}`);
  if (normalizedCommand.toLowerCase() === 'pnpm' && String(argv[1] ?? '').toLowerCase() === 'exec' && ['cargo', 'cargo.exe'].includes(String(argv[2] ?? '').toLowerCase())) {
    reasons.push('package_manager_wrapper_for_native_tool:pnpm cargo');
  }
  if (!isInsideAnyRoot(cwd, policy.allowedRoots)) reasons.push(`working_directory_outside_allowed_roots:${cwd}`);
  reasons.push(...wrapperExecutionReasons(argv, cwd, policy));
  if (!isCommandAllowed(argv, policy)) reasons.push(`command_not_allowed:${argv.join(' ')}`);

  return {
    schema: 'narada.structured_command.execution_decision.v0',
    status: reasons.length === 0 ? 'allowed' : 'refused',
    reasons,
    remediation_hints: reasons.length === 0 ? [] : buildRemediationHints(argv, reasons),
    mcp_fallbacks: reasons.length === 0 ? [] : buildMcpFallbacks(argv, reasons, cwd),
    command: normalizedCommand,
    args: argv.slice(1),
    working_directory: cwd,
    shell_interpolation: false,
  };
}

function buildRemediationHints(argv: any, reasons: any) {
  const command = argv[0]?.toLowerCase();
  const subcommand = argv[1]?.toLowerCase();
  const hints = [];

  if (command === 'git') {
    const toolBySubcommand: Record<string, string> = {
      add: 'git_add',
      commit: 'git_commit',
      diff: 'git_diff',
      log: 'git_log',
      push: 'git_push',
      show: 'git_show',
      status: 'git_status',
    };
    const tool = toolBySubcommand[subcommand] ?? 'git_status';
    hints.push(`Use the governed Git MCP tool ${tool} instead of shelling out to git.`);
  }

  if (command === 'rg' || command === 'grep' || command === 'findstr') {
    hints.push('Use local-filesystem fs_grep_search for content search or fs_glob_search for file pattern search.');
  }

  if (command === 'ls' || command === 'dir' || command === 'find') {
    hints.push('Use local-filesystem fs_glob_search or fs_read_file for governed filesystem inspection.');
  }

  if (reasons.some((reason: any) => String(reason).startsWith('working_directory_outside_allowed_roots:'))) {
    hints.push('Run from an allowed root or request a policy update through the surface configuration instead of bypassing the root guard.');
  }

  if (reasons.some((reason: any) => String(reason).startsWith('blocked_command:'))) {
    hints.push('Use an explicit argv-based allowed command or a narrower MCP surface; blocked shell interpreters remain disallowed.');
  }

  if (reasons.some((reason: any) => String(reason).startsWith('package_manager_wrapper_for_native_tool:'))) {
    hints.push('Invoke cargo directly; pnpm is not part of the native Rust toolchain.');
  }

  if (reasons.some((reason: any) => String(reason).startsWith('wrapper_execution_disallowed:') || String(reason).startsWith('transient_wrapper_path_disallowed:'))) {
    hints.push('Do not execute cmd.exe or an unapproved/transient wrapper. A repository-owned .cmd/.bat entrypoint must already exist under an allowed root; otherwise run the owning MCP tool directly or use structured_command_start with a canonical allowlisted command and retain its execution_ref as evidence.');
  }

  return [...new Set(hints)];
}

function buildMcpFallbacks(argv: any, reasons: any, cwd: any) {
  const command = argv[0]?.toLowerCase();
  const args = argv.slice(1);
  const fallbacks: any[] = [];
  const refusedByCommandPolicy = reasons.some((reason: any) => String(reason).startsWith('command_not_allowed:'));
  if (!refusedByCommandPolicy) return fallbacks;

  if (command === 'rg' || command === 'grep' || command === 'findstr') {
    const searchPattern = firstSearchPatternArg(args);
    const filesMode = command === 'rg' && args.some((arg: any) => String(arg).toLowerCase() === '--files');
    const scopedPaths = searchPathArgs(args, cwd);
    const ignore = globArgs(args);
    if (!filesMode) {
      for (const path of scopedPaths) fallbacks.push({
        surface_id: 'local-filesystem',
        tool_name: 'fs_grep_search',
        canonical_name: 'fs_grep_search',
        purpose: 'content_search',
        arguments: {
          pattern: searchPattern ?? '<search pattern>',
          path,
          output_mode: 'content',
          ...(ignore.length > 0 ? { ignore } : {}),
        },
      });
    }
    fallbacks.push({
      surface_id: 'local-filesystem',
      tool_name: 'fs_glob_search',
      canonical_name: 'fs_glob_search',
      purpose: filesMode ? 'file_listing' : 'file_pattern_search',
      arguments: {
        pattern: firstGlobArg(args) ?? '*',
        directory: scopedPaths[0] ?? cwd,
        ...(ignore.length > 0 ? { ignore } : {}),
      },
    });
  }

  if (command === 'ls' || command === 'dir' || command === 'find') {
    fallbacks.push({
      surface_id: 'local-filesystem',
      tool_name: 'fs_glob_search',
      canonical_name: 'fs_glob_search',
      purpose: 'filesystem_listing',
      arguments: {
        pattern: '*',
        directory: cwd,
      },
    });
  }

  if (command === 'git') {
    const toolBySubcommand: Record<string, string> = {
      add: 'git_add',
      commit: 'git_commit',
      diff: 'git_diff',
      log: 'git_log',
      push: 'git_push',
      show: 'git_show',
      status: 'git_status',
    };
    const toolName = toolBySubcommand[args[0]?.toLowerCase()] ?? 'git_status';
    const fallbackArguments: Record<string, unknown> = { working_directory: cwd };
    if (toolName === 'git_add') {
      const paths = gitPathArgs(args.slice(1));
      if (paths.length > 0) fallbackArguments.paths = paths;
    }
    if (toolName === 'git_commit') {
      const message = gitOptionValue(args.slice(1), new Set(['-m', '--message']));
      if (message) fallbackArguments.message = message;
    }
    fallbacks.push({
      surface_id: 'git',
      tool: toolName,
      tool_name: toolName,
      canonical_name: toolName,
      purpose: 'git_operation',
      arguments: fallbackArguments,
    });
  }

  return fallbacks;
}

function gitPathArgs(args: any) {
  const paths = [];
  let afterSeparator = false;
  for (const rawArg of args) {
    const arg = String(rawArg ?? '');
    if (arg === '--') {
      afterSeparator = true;
      continue;
    }
    if (!afterSeparator && arg.startsWith('-')) continue;
    if (arg) paths.push(arg);
  }
  return [...new Set(paths)];
}

function gitOptionValue(args: any, options: any) {
  for (let index = 0; index < args.length; index += 1) {
    if (options.has(String(args[index] ?? '')) && args[index + 1]) return String(args[index + 1]);
  }
  return null;
}

function firstSearchPatternArg(args: any) {
  const optionsWithValues = new Set(['-g', '--glob', '-t', '--type', '-T', '--type-not', '-e', '--regexp']);
  for (let index = 0; index < args.length; index += 1) {
    const arg = String(args[index] ?? '');
    if (!arg) continue;
    if (optionsWithValues.has(arg)) {
      if (arg === '-e' || arg === '--regexp') return args[index + 1] ? String(args[index + 1]) : null;
      index += 1;
      continue;
    }
    if (arg.startsWith('-')) continue;
    return arg;
  }
  return null;
}

function firstGlobArg(args: any) {
  for (let index = 0; index < args.length; index += 1) {
    const arg = String(args[index] ?? '');
    if (arg === '-g' || arg === '--glob') return args[index + 1] ? String(args[index + 1]) : null;
  }
  return null;
}

function globArgs(args: any) {
  const values = [];
  for (let index = 0; index < args.length; index += 1) {
    const arg = String(args[index] ?? '');
    if ((arg === '-g' || arg === '--glob') && args[index + 1]) {
      const value = String(args[index + 1]);
      if (value.startsWith('!')) values.push(value.slice(1));
      index += 1;
    }
  }
  return values;
}

function searchPathArgs(args: any, cwd: any) {
  const paths = [];
  const optionsWithValues = new Set(['-g', '--glob', '-t', '--type', '-T', '--type-not', '-e', '--regexp', '-m', '--max-count', '-A', '-B', '-C', '--after-context', '--before-context', '--context']);
  let patternConsumed = false;
  for (let index = 0; index < args.length; index += 1) {
    const arg = String(args[index] ?? '');
    if (!arg) continue;
    if (optionsWithValues.has(arg)) {
      if ((arg === '-e' || arg === '--regexp') && !patternConsumed) patternConsumed = true;
      index += 1;
      continue;
    }
    if (arg.startsWith('-')) continue;
    if (!patternConsumed) {
      patternConsumed = true;
      continue;
    }
    paths.push(resolve(cwd, arg));
  }
  return paths.length > 0 ? [...new Set(paths)] : [cwd];
}

export function publicExecutionPolicy(policy: any) {
  return {
    schema: 'narada.structured_command.execution_policy.v0',
    allowed_roots: policy.allowedRoots,
    allowed_commands: [...policy.allowedCommands].sort(),
    default_allowed_commands: [...(policy.defaultAllowedCommands ?? [])].sort(),
    allowed_prefixes: policy.allowedPrefixes.map((prefix: any) => prefix.join(' ')),
    default_allowed_prefixes: (policy.defaultAllowedPrefixes ?? []).map((prefix: any) => prefix.join(' ')),
    blocked_commands: [...policy.blockedCommands].sort(),
    max_timeout_ms: policy.maxTimeoutMs,
    max_output_bytes: policy.maxOutputBytes,
    shell_interpolation: false,
  };
}

function isCommandAllowed(argv: any, policy: any) {
  const command = argv[0]?.toLowerCase();
  if (!command) return false;
  if (policy.allowedCommands.has(command)) return true;
  return policy.allowedPrefixes.some((prefix: any) => prefix.every((part: any, index: any) => commandPartMatches(argv[index], part, index)) && prefixAllowedByAdditionalGuards(prefix, argv));
}

function prefixAllowedByAdditionalGuards(prefix: any, argv: any) {
  if (prefix[0] === 'pnpm' && prefix[1] === '--filter') {
    const script = argv[3]?.toLowerCase();
    return script === 'test' || script === 'build' || script === 'typecheck' || String(script ?? '').startsWith('test:');
  }
  if (prefix[0] === 'narada' && prefix[1] === 'doctor') {
    const mutatingFlags = new Set(['--repair', '--fix', '--write', 'repair', 'fix']);
    return !argv.slice(2).some((arg: any) => mutatingFlags.has(String(arg).toLowerCase()));
  }
  return true;
}

function commandPartMatches(actual: any, expected: any, index: any) {
  const normalizedActual = String(actual ?? '').toLowerCase();
  const normalizedExpected = String(expected ?? '').toLowerCase();
  if (normalizedActual === normalizedExpected) return true;
  if (index !== 0) return false;
  return normalizeExecutableAlias(normalizedActual) === normalizeExecutableAlias(normalizedExpected);
}

function normalizeExecutableAlias(value: any) {
  if (value === 'pwsh.exe') return 'pwsh';
  return value;
}

function isInsideAnyRoot(path: any, roots: any) {
  return roots.some((root: any) => {
    const rel = relative(root, path);
    return rel === '' || (!rel.startsWith('..') && !/^[a-zA-Z]:/.test(rel));
  });
}

function normalizeCommand(command: any) {
  const value = typeof command === 'string' ? command.trim() : '';
  if (!value || /[\r\n;&|<>]/.test(value)) return '';
  return value;
}

function normalizeArgs(args: any) {
  if (!Array.isArray(args)) return [];
  return args.map((arg: any) => String(arg));
}

function normalizeList(value: any) {
  if (!value) return [];
  if (Array.isArray(value)) return value.map((item: any) => String(item)).filter(Boolean);
  return [String(value)].filter(Boolean);
}

function normalizePrefix(prefix: any) {
  return String(prefix).trim().split(/\s+/).filter(Boolean).map((item: any) => item.toLowerCase());
}

function clampInteger(value: any, min: any, max: any, fallback: any) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.max(min, Math.min(max, Math.trunc(parsed)));
}
