import { randomUUID } from 'node:crypto';
import { appendFileSync, closeSync, copyFileSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync, writeSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import {
  optionalBranchName,
  optionalRefName,
  publicGitPolicy,
  requiredBranchName,
  requireCommitish,
  requireWriteMode,
  resolveWorkingDirectory as resolvePolicyWorkingDirectory,
  validateExplicitFilePath,
  validateGitPathspec,
  type GitMcpPolicy,
} from './policy.js';
import { diagnosticError } from './git-errors.js';
import { combineOutput, ensureGitOk, gitText, runGit } from './git-runner.js';
import { parseStatus } from './status-parser.js';
import {
  createIndexScope,
  createWorkScope,
  resolveScopeToken,
  sha256,
  scopeTokenMap,
  storeScopeToken,
  type GitBaseState,
  type GitIndexScope,
  type GitWorkScope,
} from './scope-tokens.js';
import type { GitMcpState } from './state.js';

const PREVIEW_CHAR_LIMIT = 1000;
const DEFAULT_DIFF_LIMIT = 4000;
const MAX_DIFF_LIMIT = 50_000;
const UNTRACKED_DIFF_CHAR_LIMIT = 20_000;
const UNTRACKED_FILE_COUNT_LIMIT = 25;
const WORKFLOW_PUSH_STATUSES = ['pushed', 'not_attempted', 'failed', 'not_pushable'];

export type GitRequestContext = {
  abortSignal?: AbortSignal;
  env?: NodeJS.ProcessEnv;
};

export async function callGitTool(name: string, args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}): Promise<unknown> {
  const runContext = { ...context, env: state.env };
  if (name === 'git_policy_inspect') return publicGitPolicy(state.policy);
  if (name === 'git_status') return gitStatus(args, state, runContext);
  if (name === 'git_begin_work_scope') return gitBeginWorkScope(args, state, runContext);
  if (name === 'git_end_work_scope') return gitEndWorkScope(args, state, runContext);
  if (name === 'git_sync_status') return gitSyncStatus(args, state, runContext);
  if (name === 'git_branch_list') return gitBranchList(args, state, runContext);
  if (name === 'git_worktree_list') return gitWorktreeList(args, state, runContext);
  if (name === 'git_worktree_add') return gitWorktreeAdd(args, state, runContext);
  if (name === 'git_worktree_remove') return gitWorktreeRemove(args, state, runContext);
  if (name === 'git_worktree_prune') return gitWorktreePrune(args, state, runContext);
  if (name === 'git_branch_create') return gitBranchCreate(args, state, runContext);
  if (name === 'git_branch_switch') return gitBranchSwitch(args, state, runContext);
  if (name === 'git_branch_rename') return gitBranchRename(args, state, runContext);
  if (name === 'git_branch_delete') return gitBranchDelete(args, state, runContext);
  if (name === 'git_branch_delete_remote') return gitBranchDeleteRemote(args, state, runContext);
  if (name === 'git_branch_set_upstream') return gitBranchSetUpstream(args, state, runContext);
  if (name === 'git_branch_unset_upstream') return gitBranchUnsetUpstream(args, state, runContext);
  if (name === 'git_changed_summary') return gitChangedSummary(args, state, runContext);
  if (name === 'git_repositories_summary') return gitRepositoriesSummary(args, state, runContext);
  if (name === 'git_workflow_record') return gitWorkflowRecord(args, state, runContext);
  if (name === 'git_diff') return gitDiff(args, state, runContext);
  if (name === 'git_add') return gitAdd(args, state, runContext);
  if (name === 'git_unstage') return gitUnstage(args, state, runContext);
  if (name === 'git_commit') return gitCommit(args, state, runContext);
  if (name === 'git_commit_paths') return gitCommitPaths(args, state, runContext);
  if (name === 'git_reconcile_index') return gitReconcileIndex(args, state, runContext);
  if (name === 'git_push') return gitPush(args, state, runContext);
  if (name === 'git_fetch') return gitFetch(args, state, runContext);
  if (name === 'git_rebase') return gitRebase(args, state, runContext);
  if (name === 'git_rebase_continue') return gitRebaseContinue(args, state, runContext);
  if (name === 'git_rebase_abort') return gitRebaseAbort(args, state, runContext);
  if (name === 'git_merge') return gitMerge(args, state, runContext);
  if (name === 'git_merge_continue') return gitMergeContinue(args, state, runContext);
  if (name === 'git_merge_abort') return gitMergeAbort(args, state, runContext);
  if (name === 'git_log') return gitLog(args, state, runContext);
  if (name === 'git_show') return gitShow(args, state, runContext);
  throw diagnosticError('git_mcp_unknown_tool', `git_mcp_unknown_tool:${name}`, { tool_name: name });
}

export async function gitSyncStatus(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}): Promise<Record<string, unknown>> {
  const cwd = resolveWorkingDirectory(args, state);
  const status = await gitStatus({ working_directory: cwd }, state, context);
  const operation = await detectSyncOperation(cwd, state, context);
  const conflicts = stringArray(status.conflicts);
  return {
    schema: 'narada.git.sync_status.v1',
    status: 'ok',
    working_directory: cwd,
    operation,
    in_progress: operation !== null,
    conflict_paths: conflicts,
    conflict_count: conflicts.length,
    clean: status.clean,
    branch: status.branch,
    upstream: status.upstream,
    recovery: operation === 'rebase'
      ? ['git_rebase_continue', 'git_rebase_abort', 'git_sync_status']
      : operation === 'merge'
        ? ['git_merge_continue', 'git_merge_abort', 'git_sync_status']
        : [],
    post_status: status,
  };
}

export async function gitFetch(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_fetch');
  const cwd = resolveWorkingDirectory(args, state);
  const remote = requiredRemoteName(args.remote);
  const branch = requiredBranchName(args.branch, 'branch');
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const before = await gitStatus({ working_directory: cwd }, state, context);
  requireConfiguredRemote(before.remotes, remote);
  const result = await runGit(cwd, ['fetch', '--no-tags', remote, branch], state.policy, context);
  ensureGitOk(result, 'git_fetch_failed');
  const after = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: 'narada.git.fetch.v1',
    status: 'ok',
    working_directory: cwd,
    remote,
    branch,
    scope_label: scopeLabel,
    output: combineOutput(result).slice(0, PREVIEW_CHAR_LIMIT),
    pre_status: before,
    ...verifiedMutationPostState(after, { operation: 'fetch', remote, branch }),
    post_status: after,
    summary: `fetched ${remote}/${branch}`,
  };
  audit(state, payload);
  return payload;
}

export async function gitRebase(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_rebase');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const onto = requireCommitish(args.onto);
  const autostash = args.autostash === true;
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const before = await prepareSyncMutation(cwd, autostash, state, context);
  const result = await runGit(cwd, ['rebase', ...(autostash ? ['--autostash'] : []), onto], state.policy, context);
  return finishSyncMutation('rebase', cwd, onto, autostash, scopeLabel, before, result, state, context);
}

export async function gitMerge(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_merge');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const target = requireCommitish(args.target);
  const autostash = args.autostash === true;
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const before = await prepareSyncMutation(cwd, autostash, state, context);
  const result = await runGit(cwd, ['merge', ...(autostash ? ['--autostash'] : []), '--no-edit', target], state.policy, context);
  return finishSyncMutation('merge', cwd, target, autostash, scopeLabel, before, result, state, context);
}

export async function gitRebaseContinue(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_rebase_continue');
  await requireTopologyScopeForMutation(resolveWorkingDirectory(args, state), args, state, context, false);
  return continueSyncOperation('rebase', args, state, context);
}

export async function gitRebaseAbort(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_rebase_abort');
  await requireTopologyScopeForMutation(resolveWorkingDirectory(args, state), args, state, context, false);
  return abortSyncOperation('rebase', args, state, context);
}

export async function gitMergeContinue(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_merge_continue');
  await requireTopologyScopeForMutation(resolveWorkingDirectory(args, state), args, state, context, false);
  return continueSyncOperation('merge', args, state, context);
}

export async function gitMergeAbort(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_merge_abort');
  await requireTopologyScopeForMutation(resolveWorkingDirectory(args, state), args, state, context, false);
  return abortSyncOperation('merge', args, state, context);
}

async function prepareSyncMutation(cwd: string, autostash: boolean, state: GitMcpState, context: GitRequestContext): Promise<Record<string, unknown>> {
  const before = await gitStatus({ working_directory: cwd }, state, context);
  const operation = await detectSyncOperation(cwd, state, context);
  if (operation) {
    throw diagnosticError('git_sync_operation_in_progress', 'git_sync_operation_in_progress', {
      operation,
      mutation_started: false,
      remediation: `Use git_${operation}_continue, git_${operation}_abort, or git_sync_status before starting another operation.`,
    });
  }
  const conflicts = stringArray(before.conflicts);
  if (conflicts.length > 0) {
    throw diagnosticError('git_conflicts_present', 'git_conflicts_present', {
      conflict_paths: conflicts,
      mutation_started: false,
      remediation: 'Resolve the existing conflicts and use the matching continuation or abort tool.',
    });
  }
  const untracked = stringArray(before.untracked);
  if (untracked.length > 0) {
    throw diagnosticError('git_untracked_worktree_requires_manual_preservation', 'git_untracked_worktree_requires_manual_preservation', {
      untracked_paths: untracked,
      autostash,
      mutation_started: false,
      remediation: 'Preserve or remove untracked files explicitly before synchronization; Git autostash does not include them.',
    });
  }
  if (before.clean !== true && !autostash) {
    throw diagnosticError('git_dirty_worktree_requires_autostash', 'git_dirty_worktree_requires_autostash', {
      autostash,
      mutation_started: false,
      remediation: 'Set autostash=true for tracked dirty worktrees or make the worktree clean before synchronization.',
    });
  }
  return before;
}

async function finishSyncMutation(
  operation: 'rebase' | 'merge',
  cwd: string,
  target: string,
  autostash: boolean,
  scopeLabel: string | null,
  before: Record<string, unknown>,
  result: Awaited<ReturnType<typeof runGit>>,
  state: GitMcpState,
  context: GitRequestContext,
) {
  const after = await gitStatus({ working_directory: cwd }, state, context);
  const operationState = await detectSyncOperation(cwd, state, context);
  if (result.exit_code !== 0 && operationState === operation) {
    const conflicts = stringArray(after.conflicts);
    const payload = {
      schema: `narada.git.${operation}.v1`,
      status: 'conflict',
      operation,
      working_directory: cwd,
      target,
      autostash,
      scope_label: scopeLabel,
      mutation_started: true,
      operation_in_progress: true,
      conflict_paths: conflicts,
      conflict_count: conflicts.length,
      output: combineOutput(result).slice(0, PREVIEW_CHAR_LIMIT),
      pre_status: before,
      ...verifiedMutationPostState(after, { operation, target, status: 'conflict' }),
      post_status: after,
      recovery: operation === 'rebase'
        ? ['git_rebase_continue', 'git_rebase_abort', 'git_sync_status']
        : ['git_merge_continue', 'git_merge_abort', 'git_sync_status'],
      summary: `${operation} requires conflict resolution`,
    };
    audit(state, payload);
    return payload;
  }
  ensureGitOk(result, `git_${operation}_failed`);
  const payload = {
    schema: `narada.git.${operation}.v1`,
    status: operation === 'rebase' ? 'rebased' : 'merged',
    operation,
    working_directory: cwd,
    target,
    autostash,
    scope_label: scopeLabel,
    mutation_started: true,
    operation_in_progress: false,
    output: combineOutput(result).slice(0, PREVIEW_CHAR_LIMIT),
    ...verifiedMutationPostState(after, { operation, target, status: operation === 'rebase' ? 'rebased' : 'merged' }),
    pre_status: before,
    post_status: after,
    recovery: [],
    summary: `${operation} completed onto ${target}`,
  };
  audit(state, payload);
  return payload;
}

async function continueSyncOperation(operation: 'rebase' | 'merge', args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext) {
  const cwd = resolveWorkingDirectory(args, state);
  const current = await detectSyncOperation(cwd, state, context);
  if (current !== operation) {
    throw diagnosticError(`git_${operation}_not_in_progress`, `git_${operation}_not_in_progress`, { operation: current, mutation_started: false });
  }
  const status = await gitStatus({ working_directory: cwd }, state, context);
  const conflicts = stringArray(status.conflicts);
  if (conflicts.length > 0) {
    throw diagnosticError(`git_${operation}_conflicts_remaining`, `git_${operation}_conflicts_remaining`, {
      conflict_paths: conflicts,
      mutation_started: false,
      remediation: 'Resolve and stage every conflict path before continuing.',
    });
  }
  const command = operation === 'rebase'
    ? ['-c', 'core.editor=true', 'rebase', '--continue']
    : ['commit', '--no-edit'];
  const result = await runGit(cwd, command, state.policy, { ...context, env: { ...state.env, GIT_EDITOR: 'true' } });
  const after = await gitStatus({ working_directory: cwd }, state, context);
  const stillInProgress = await detectSyncOperation(cwd, state, context);
  if (result.exit_code !== 0 && stillInProgress === operation) {
    return syncRecoveryPayload(operation, 'conflict', cwd, result, after, args);
  }
  ensureGitOk(result, `git_${operation}_continue_failed`);
  const payload = {
    schema: `narada.git.${operation}_continue.v1`,
    status: 'ok',
    operation,
    working_directory: cwd,
    mutation_started: true,
    operation_in_progress: false,
    output: combineOutput(result).slice(0, PREVIEW_CHAR_LIMIT),
    ...verifiedMutationPostState(after, { operation: `${operation}_continue`, status: 'ok' }),
    post_status: after,
    summary: `${operation} continued successfully`,
  };
  audit(state, payload);
  return payload;
}

async function abortSyncOperation(operation: 'rebase' | 'merge', args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext) {
  const cwd = resolveWorkingDirectory(args, state);
  const current = await detectSyncOperation(cwd, state, context);
  if (current !== operation) {
    throw diagnosticError(`git_${operation}_not_in_progress`, `git_${operation}_not_in_progress`, { operation: current, mutation_started: false });
  }
  const result = await runGit(cwd, [operation, '--abort'], state.policy, context);
  ensureGitOk(result, `git_${operation}_abort_failed`);
  const after = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: `narada.git.${operation}_abort.v1`,
    status: 'aborted',
    operation,
    working_directory: cwd,
    mutation_started: true,
    operation_in_progress: false,
    output: combineOutput(result).slice(0, PREVIEW_CHAR_LIMIT),
    ...verifiedMutationPostState(after, { operation: `${operation}_abort`, status: 'aborted' }),
    post_status: after,
    summary: `${operation} aborted`,
  };
  audit(state, payload);
  return payload;
}

function syncRecoveryPayload(operation: 'rebase' | 'merge', status: 'conflict', cwd: string, result: Awaited<ReturnType<typeof runGit>>, after: Record<string, unknown>, args: Record<string, unknown>) {
  return {
    schema: `narada.git.${operation}_continue.v1`,
    status,
    operation,
    working_directory: cwd,
    mutation_started: true,
    operation_in_progress: true,
    conflict_paths: stringArray(after.conflicts),
    output: combineOutput(result).slice(0, PREVIEW_CHAR_LIMIT),
    ...verifiedMutationPostState(after, { operation: `${operation}_continue`, status }),
    post_status: after,
    recovery: operation === 'rebase'
      ? ['git_rebase_continue', 'git_rebase_abort', 'git_sync_status']
      : ['git_merge_continue', 'git_merge_abort', 'git_sync_status'],
    scope_label: optionalNonEmptyString(args.scope_label),
  };
}

async function detectSyncOperation(cwd: string, state: GitMcpState, context: GitRequestContext): Promise<'rebase' | 'merge' | null> {
  for (const marker of ['rebase-merge', 'rebase-apply'] as const) {
    const path = await gitPath(cwd, marker, state, context);
    if (path && existsSync(path)) return 'rebase';
  }
  const mergeHead = await gitPath(cwd, 'MERGE_HEAD', state, context);
  return mergeHead && existsSync(mergeHead) ? 'merge' : null;
}

async function gitPath(cwd: string, name: string, state: GitMcpState, context: GitRequestContext): Promise<string | null> {
  const result = await runGit(cwd, ['rev-parse', '--git-path', name], state.policy, context);
  if (result.exit_code !== 0 || result.timed_out || result.cancelled) return null;
  const value = result.output_text.trim();
  return value ? resolve(cwd, value) : null;
}

export async function gitUnstage(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_unstage');
  const cwd = resolveWorkingDirectory(args, state);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const repositoryRoot = (await gitText(cwd, ['rev-parse', '--show-toplevel'], state.policy, 'git_unstage_failed', context)).trim();
  const workScope = requireWorkScope(args.work_scope_ref, repositoryRoot, state);
  assertScopeHead(workScope, await readGitBaseState(cwd, state, context));
  const paths = await Promise.all(stringArray(args.paths).map((path) => validateExplicitFilePath(cwd, path, (workdir, gitArgs) => runGit(workdir, gitArgs, state.policy, context))));
  if (paths.length === 0) throw diagnosticError('git_unstage_requires_paths');
  assertPathsWithinWorkScope(paths, workScope, 'git_unstage_paths_outside_work_scope');
  let result = await runGit(cwd, ['restore', '--staged', '--', ...paths], state.policy, context);
  if (result.exit_code !== 0 && /could not resolve 'HEAD'|ambiguous argument 'HEAD'|unknown revision/i.test(combineOutput(result))) {
    result = await runGit(cwd, ['rm', '--cached', '--', ...paths], state.policy, context);
  }
  ensureGitOk(result, 'git_unstage_failed');
  const status = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: 'narada.git.unstage.v1',
    status: 'ok',
    working_directory: cwd,
    scope_label: scopeLabel,
    paths,
    unstaged_count: paths.length,
    summary: `unstaged ${paths.length} path${paths.length === 1 ? '' : 's'}`,
    ...verifiedMutationPostState(status, { operation: 'unstage', paths }),
    post_status: status,
  };
  audit(state, payload);
  return payload;
}

async function requireLocalBranch(cwd: string, branch: string, state: GitMcpState, context: GitRequestContext): Promise<void> {
  const result = await runGit(cwd, ['show-ref', '--verify', '--quiet', `refs/heads/${branch}`], state.policy, context);
  if (result.exit_code === 0 && !result.timed_out && !result.cancelled) return;
  if (result.exit_code === 1 && !result.timed_out && !result.cancelled) {
    throw diagnosticError('git_branch_not_found', 'git_branch_not_found', {
      branch,
      mutation_started: false,
    });
  }
  throw diagnosticError('git_branch_lookup_failed', 'git_branch_lookup_failed', {
    branch,
    exit_code: result.exit_code,
    timed_out: result.timed_out,
    cancelled: result.cancelled,
    diagnostic_text: combineOutput(result),
    mutation_started: false,
  });
}

async function requireLocalBranchAbsent(cwd: string, branch: string, state: GitMcpState, context: GitRequestContext): Promise<void> {
  const result = await runGit(cwd, ['show-ref', '--verify', '--quiet', `refs/heads/${branch}`], state.policy, context);
  if (result.exit_code === 1 && !result.timed_out && !result.cancelled) return;
  if (result.exit_code === 0 && !result.timed_out && !result.cancelled) {
    throw diagnosticError('git_branch_already_exists', 'git_branch_already_exists', {
      branch,
      mutation_started: false,
    });
  }
  throw diagnosticError('git_branch_lookup_failed', 'git_branch_lookup_failed', {
    branch,
    exit_code: result.exit_code,
    timed_out: result.timed_out,
    cancelled: result.cancelled,
    diagnostic_text: combineOutput(result),
    mutation_started: false,
  });
}

function requiredRemoteName(value: unknown): string {
  const remote = optionalRefName(value, 'remote');
  if (!remote) throw diagnosticError('git_branch_remote_requires_remote');
  return remote;
}

function requireConfiguredRemote(remotes: unknown, remote: string): void {
  const configured = Array.isArray(remotes)
    ? remotes.map((candidate) => String(asRemote(candidate).name ?? '')).filter(Boolean)
    : [];
  if (configured.includes(remote)) return;
  throw diagnosticError('git_remote_not_configured', 'git_remote_not_configured', {
    remote,
    configured_remotes: configured,
    mutation_started: false,
  });
}

async function requireRemoteBranch(cwd: string, remote: string, branch: string, state: GitMcpState, context: GitRequestContext): Promise<string> {
  const ref = `refs/heads/${branch}`;
  const result = await runGit(cwd, ['ls-remote', '--heads', remote, ref], state.policy, context);
  if (result.timed_out || result.cancelled || result.exit_code !== 0) {
    throw diagnosticError('git_branch_remote_lookup_failed', 'git_branch_remote_lookup_failed', {
      remote,
      branch,
      exit_code: result.exit_code,
      timed_out: result.timed_out,
      cancelled: result.cancelled,
      diagnostic_text: combineOutput(result),
      mutation_started: false,
    });
  }
  const line = result.output_text.split(/\r?\n/).find(Boolean);
  const match = line?.match(/^([0-9a-fA-F]+)\s+refs\/heads\/.+$/);
  if (!match) {
    throw diagnosticError('git_remote_branch_not_found', 'git_remote_branch_not_found', {
      remote,
      branch,
      mutation_started: false,
    });
  }
  return match[1];
}

async function requireMergedInto(
  cwd: string,
  source: string,
  base: string,
  state: GitMcpState,
  context: GitRequestContext,
  details: Record<string, unknown>,
): Promise<void> {
  const result = await runGit(cwd, ['merge-base', '--is-ancestor', source, base], state.policy, context);
  if (result.exit_code === 0 && !result.timed_out && !result.cancelled) return;
  if (result.exit_code === 1 && !result.timed_out && !result.cancelled) {
    throw diagnosticError('git_branch_not_merged', 'git_branch_not_merged', {
      ...details,
      merge_check: 'failed',
      mutation_started: false,
      remediation: 'Choose a base that contains the branch history or preserve the branch; force deletion is not supported.',
    });
  }
  throw diagnosticError('git_branch_merge_check_failed', 'git_branch_merge_check_failed', {
    ...details,
    exit_code: result.exit_code,
    timed_out: result.timed_out,
    cancelled: result.cancelled,
    diagnostic_text: combineOutput(result),
    mutation_started: false,
  });
}

function resolveLocalBranchArgument(value: unknown, status: Record<string, unknown>): string {
  const supplied = optionalBranchName(value, 'local_branch');
  if (supplied) return supplied;
  const current = optionalBranchName(status.branch, 'local_branch');
  if (!current) throw diagnosticError('git_branch_requires_local_branch', 'git_branch_requires_local_branch', {
    mutation_started: false,
    remediation: 'Pass local_branch while on a detached HEAD or unborn branch.',
  });
  return current;
}

export async function gitChangedSummary(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}): Promise<Record<string, unknown>> {
  const status = await gitStatus({
    working_directory: args.working_directory,
    work_scope_ref: args.work_scope_ref,
    include_untracked: true,
    format: 'full',
  }, state, context);
  const entries = Array.isArray(status.status_entries) ? status.status_entries.map(asStatusEntry) : [];
  const trackedEntries = entries.filter((entry) => !entry.untracked);
  const untrackedEntries = entries.filter((entry) => entry.untracked);
  const pathspecFilters: string[] = [...stringArray(args.pathspecs)];
  const singlePathspec = optionalNonEmptyString(args.pathspec);
  if (singlePathspec) pathspecFilters.unshift(singlePathspec);
  const expectedFilters: string[] = stringArray(args.expected_paths);
  const relevanceFilters: string[] = [...pathspecFilters, ...expectedFilters];
  const scopeFilters = pathspecFilters.length > 0 ? pathspecFilters : [];
  const untrackedSampleLimit = clampInteger(args.untracked_sample_limit, 0, 200, 20);
  const wholeTrackedChangedPaths = trackedEntries.map((entry) => String(entry.display_path ?? '')).filter(Boolean);
  const wholeUntrackedPaths = untrackedEntries.map((entry) => String(entry.display_path ?? '')).filter(Boolean);
  const scopedEntries = scopeFilters.length > 0
    ? entries.filter((entry) => scopeFilters.some((filter) => pathMatchesFilter(String(entry.display_path ?? entry.path ?? ''), filter)))
    : entries;
  const scopedTrackedEntries = scopedEntries.filter((entry) => !entry.untracked);
  const scopedUntrackedEntries = scopedEntries.filter((entry) => entry.untracked);
  const trackedChangedPaths = scopedTrackedEntries.map((entry) => String(entry.display_path ?? '')).filter(Boolean);
  const untrackedPaths = scopedUntrackedEntries.map((entry) => String(entry.display_path ?? '')).filter(Boolean);
  const untrackedGroups = groupUntrackedPaths(untrackedPaths, untrackedSampleLimit);
  const untrackedClassifications = untrackedPaths.map(classifyAdvisoryPath);
  const relevantEntries = relevanceFilters.length > 0
    ? entries.filter((entry) => relevanceFilters.some((filter) => pathMatchesFilter(String(entry.display_path ?? entry.path ?? ''), filter)))
    : [];
  const taskScopedClassification = classifyTaskScopedDirtyPaths(entries, relevanceFilters);
  return {
    schema: 'narada.git.changed_summary.v1',
    status: 'ok',
    working_directory: status.working_directory,
    repository_root: status.repository_root,
    branch: status.branch,
    clean: status.clean,
    path_scope_applied: scopeFilters.length > 0,
    path_scope_filters: scopeFilters,
    whole_repository_tracked_changed_count: wholeTrackedChangedPaths.length,
    whole_repository_untracked_count: wholeUntrackedPaths.length,
    whole_repository_tracked_changed_paths: scopeFilters.length > 0 ? wholeTrackedChangedPaths : undefined,
    tracked_changed_count: trackedChangedPaths.length,
    staged_count: Array.isArray(status.staged) ? status.staged.length : 0,
    unstaged_count: Array.isArray(status.unstaged) ? status.unstaged.length : 0,
    conflict_count: Array.isArray(status.conflicts) ? status.conflicts.length : 0,
    untracked_count: untrackedPaths.length,
    advisory_classification: summarizeAdvisoryClassifications(untrackedClassifications),
    untracked_classifications: untrackedClassifications,
    tracked_changed_paths: trackedChangedPaths,
    staged_paths: status.staged,
    unstaged_paths: status.unstaged,
    conflict_paths: status.conflicts,
    untracked_groups: untrackedGroups,
    relevance_filters: relevanceFilters,
    task_scoped_dirty_classification: taskScopedClassification,
    task_relevant_dirty_paths: taskScopedClassification.relevant,
    task_unrelated_dirty_paths: taskScopedClassification.unrelated,
    task_unknown_dirty_paths: taskScopedClassification.unknown,
    relevant_changed_count: relevantEntries.length,
    relevant_changed_paths: relevantEntries.map((entry) => entry.display_path).filter(Boolean),
    relevant_entries: relevantEntries,
    full_diffs_omitted: true,
    diff_tool: 'git_diff',
  };
}

export async function gitBranchList(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  const cwd = resolveWorkingDirectory(args, state);
  const scope = stringEnum(args.scope, ['local', 'remote', 'all'], 'all');
  const limit = clampInteger(args.limit, 1, 500, 100);
  const currentResult = await runGit(cwd, ['branch', '--show-current'], state.policy, context);
  const currentBranch = currentResult.exit_code === 0 ? currentResult.output_text.trim() || null : null;
  const refs = scope === 'local'
    ? ['refs/heads']
    : scope === 'remote'
      ? ['refs/remotes']
      : ['refs/heads', 'refs/remotes'];
  const result = await runGit(cwd, [
    'for-each-ref',
    '--sort=refname',
    '--format=%(refname)\t%(refname:short)\t%(objectname)\t%(HEAD)\t%(upstream:short)\t%(upstream:trackshort)',
    ...refs,
  ], state.policy, context);
  ensureGitOk(result, 'git_branch_list_failed');
  const branches = result.output_text
    .split(/\r?\n/)
    .filter(Boolean)
    .slice(0, limit)
    .map((line) => {
      const [refName = '', name = '', objectId = '', head = '', upstream = '', tracking = ''] = line.split('\t');
      const type = refName.startsWith('refs/remotes/') ? 'remote' : 'local';
      return {
        name,
        type,
        object_id: objectId || null,
        current: type === 'local' && head === '*',
        upstream: upstream || null,
        tracking: tracking || null,
      };
    });
  return {
    schema: 'narada.git.branch_list.v1',
    status: 'ok',
    working_directory: cwd,
    scope,
    limit,
    current_branch: currentBranch,
    returned: branches.length,
    branches,
  };
}

export async function gitWorktreeList(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  const cwd = resolveWorkingDirectory(args, state);
  const result = await runGit(cwd, ['worktree', 'list', '--porcelain', '-z'], state.policy, context);
  ensureGitOk(result, 'git_worktree_list_failed');
  const worktrees: Array<Record<string, unknown>> = [];
  let current: Record<string, unknown> = {};
  for (const field of result.output_text.split('\0').filter(Boolean)) {
    if (field.startsWith('worktree ')) {
      if (Object.keys(current).length) worktrees.push(current);
      current = { path: field.slice(9) };
    } else if (field.startsWith('HEAD ')) current.head = field.slice(5);
    else if (field.startsWith('branch ')) current.branch = field.slice(7).replace(/^refs\/heads\//, '');
    else if (field === 'bare' || field === 'detached') current[field] = true;
    else if (field === 'locked' || field.startsWith('locked ')) {
      current.locked = true;
      if (field.length > 6) current.lock_reason = field.slice(7);
    } else if (field === 'prunable' || field.startsWith('prunable ')) {
      current.prunable = true;
      if (field.length > 8) current.prune_reason = field.slice(9);
    }
  }
  if (Object.keys(current).length) worktrees.push(current);
  return { schema: 'narada.git.worktree_list.v1', status: 'ok', working_directory: cwd, count: worktrees.length, worktrees };
}

function worktreePath(args: Record<string, unknown>, cwd: string, state: GitMcpState, requireAllowed: boolean): string {
  const path = resolve(cwd, String(args.path ?? ''));
  const key = process.platform === 'win32' ? path.toLowerCase() : path;
  const allowed = state.policy.allowedRoots.some((root) => {
    const rootKey = process.platform === 'win32' ? root.toLowerCase() : root;
    return key === rootKey || key.startsWith(rootKey + (process.platform === 'win32' ? '\\' : '/'));
  });
  if (requireAllowed && !allowed) throw diagnosticError('git_worktree_path_outside_allowed_roots', undefined, { path, allowed_roots: state.policy.allowedRoots, mutation_started: false });
  return path;
}

function worktreePathKey(path: string): string {
  const normalized = resolve(path).replace(/\\/g, '/').replace(/\/$/, '');
  return process.platform === 'win32' ? normalized.toLowerCase() : normalized;
}

export async function gitWorktreeAdd(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_worktree_add');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const path = worktreePath(args, cwd, state, true);
  if (existsSync(path)) throw diagnosticError('git_worktree_path_exists', undefined, { path, mutation_started: false });
  const branch = args.branch === undefined ? null : requiredBranchName(args.branch, 'branch');
  const newBranch = args.new_branch === undefined ? null : requiredBranchName(args.new_branch, 'new_branch');
  if ((branch === null) === (newBranch === null)) throw diagnosticError('git_worktree_requires_exactly_one_branch_mode');
  const startPoint = args.start_point === undefined ? 'HEAD' : requireCommitish(args.start_point);
  const argv = newBranch ? ['worktree', 'add', '-b', newBranch, path, startPoint] : ['worktree', 'add', path, branch!];
  const result = await runGit(cwd, argv, state.policy, context);
  ensureGitOk(result, 'git_worktree_add_failed');
  const payload = { schema: 'narada.git.worktree_mutation.v1', status: 'added', working_directory: cwd, path, branch, new_branch: newBranch, start_point: startPoint, force: false };
  audit(state, payload);
  return payload;
}

export async function gitWorktreeRemove(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_worktree_remove');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const path = worktreePath(args, cwd, state, false);
  const inventory = await gitWorktreeList({ working_directory: cwd }, state, context);
  if (!(inventory.worktrees as Array<Record<string, unknown>>).some((item) => worktreePathKey(String(item.path)) === worktreePathKey(path))) {
    throw diagnosticError('git_worktree_not_registered', undefined, { path, mutation_started: false });
  }
  const dirty = await runGit(path, ['status', '--porcelain=v1', '--untracked-files=all'], state.policy, context);
  ensureGitOk(dirty, 'git_worktree_remove_preflight_failed');
  if (dirty.output_text.trim()) throw diagnosticError('git_worktree_not_clean', undefined, { path, dirty_entries: dirty.output_text.trim().split(/\r?\n/), mutation_started: false });
  const result = await runGit(cwd, ['worktree', 'remove', path], state.policy, context);
  ensureGitOk(result, 'git_worktree_remove_failed');
  const payload = { schema: 'narada.git.worktree_mutation.v1', status: 'removed', working_directory: cwd, path, force: false };
  audit(state, payload);
  return payload;
}

export async function gitWorktreePrune(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_worktree_prune');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const before = await gitWorktreeList({ working_directory: cwd }, state, context);
  const result = await runGit(cwd, ['worktree', 'prune'], state.policy, context);
  ensureGitOk(result, 'git_worktree_prune_failed');
  const after = await gitWorktreeList({ working_directory: cwd }, state, context);
  const payload = { schema: 'narada.git.worktree_prune.v1', status: 'ok', working_directory: cwd, before_count: before.count, after_count: after.count };
  audit(state, payload);
  return payload;
}

export async function gitBranchCreate(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_branch_create');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const branch = requiredBranchName(args.name);
  const startPoint = args.start_point === undefined ? 'HEAD' : requireCommitish(args.start_point);
  const before = await gitStatus({ working_directory: cwd }, state, context);
  await requireLocalBranchAbsent(cwd, branch, state, context);
  const result = await runGit(cwd, ['branch', branch, startPoint], state.policy, context);
  ensureGitOk(result, 'git_branch_create_failed');
  const after = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: 'narada.git.branch_create.v1',
    status: 'ok',
    operation: 'create',
    working_directory: cwd,
    scope_label: scopeLabel,
    branch,
    start_point: startPoint,
    checked_out: false,
    output: combineOutput(result),
    pre_status: before,
    ...verifiedMutationPostState(after, { operation: 'branch_create', branch, start_point: startPoint }),
    post_status: after,
    summary: `created local branch ${branch} from ${startPoint}`,
  };
  audit(state, payload);
  return payload;
}

export async function gitBranchSwitch(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_branch_switch');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const branch = requiredBranchName(args.branch);
  const before = await gitStatus({ working_directory: cwd }, state, context);
  await requireLocalBranch(cwd, branch, state, context);
  const result = await runGit(cwd, ['switch', '--', branch], state.policy, context);
  ensureGitOk(result, 'git_branch_switch_failed');
  const after = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: 'narada.git.branch_switch.v1',
    status: 'ok',
    operation: 'switch',
    working_directory: cwd,
    scope_label: scopeLabel,
    branch,
    force: false,
    discard_changes: false,
    output: combineOutput(result),
    pre_status: before,
    ...verifiedMutationPostState(after, { operation: 'branch_switch', branch }),
    post_status: after,
    summary: `switched to local branch ${branch}`,
  };
  audit(state, payload);
  return payload;
}

export async function gitBranchRename(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_branch_rename');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const oldName = requiredBranchName(args.old_name, 'old_name');
  const newName = requiredBranchName(args.new_name, 'new_name');
  if (oldName === newName) throw diagnosticError('git_branch_rename_names_must_differ');
  const before = await gitStatus({ working_directory: cwd }, state, context);
  await requireLocalBranch(cwd, oldName, state, context);
  await requireLocalBranchAbsent(cwd, newName, state, context);
  const result = await runGit(cwd, ['branch', '-m', oldName, newName], state.policy, context);
  ensureGitOk(result, 'git_branch_rename_failed');
  const after = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: 'narada.git.branch_rename.v1',
    status: 'ok',
    operation: 'rename',
    working_directory: cwd,
    scope_label: scopeLabel,
    old_name: oldName,
    new_name: newName,
    output: combineOutput(result),
    pre_status: before,
    ...verifiedMutationPostState(after, { operation: 'branch_rename', old_name: oldName, new_name: newName }),
    post_status: after,
    summary: `renamed local branch ${oldName} to ${newName}`,
  };
  audit(state, payload);
  return payload;
}

export async function gitBranchDelete(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_branch_delete');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const branch = requiredBranchName(args.branch);
  const before = await gitStatus({ working_directory: cwd }, state, context);
  await requireLocalBranch(cwd, branch, state, context);
  const currentBranch = typeof before.branch === 'string' ? before.branch : null;
  if (currentBranch === branch) {
    throw diagnosticError('git_branch_cannot_delete_current', 'git_branch_cannot_delete_current', {
      branch,
      current_branch: currentBranch,
      mutation_started: false,
    });
  }
  const base = args.base === undefined
    ? currentBranch
    : requireCommitish(args.base);
  if (!base) throw diagnosticError('git_branch_delete_requires_base_on_detached_head');
  await requireMergedInto(cwd, branch, base, state, context, { branch, base });
  const result = await runGit(cwd, ['branch', '-d', branch], state.policy, context);
  ensureGitOk(result, 'git_branch_delete_failed');
  const after = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: 'narada.git.branch_delete.v1',
    status: 'ok',
    operation: 'delete',
    working_directory: cwd,
    scope_label: scopeLabel,
    branch,
    base,
    merge_check: 'passed',
    force: false,
    output: combineOutput(result),
    pre_status: before,
    ...verifiedMutationPostState(after, { operation: 'branch_delete', branch, base }),
    post_status: after,
    summary: `deleted merged local branch ${branch}`,
  };
  audit(state, payload);
  return payload;
}

export async function gitBranchDeleteRemote(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_branch_delete_remote');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const remote = requiredRemoteName(args.remote);
  const branch = requiredBranchName(args.branch);
  const base = requireCommitish(args.base);
  const before = await gitStatus({ working_directory: cwd }, state, context);
  requireConfiguredRemote(before.remotes, remote);
  const remoteObjectId = await requireRemoteBranch(cwd, remote, branch, state, context);
  await requireMergedInto(cwd, remoteObjectId, base, state, context, { remote, branch, base, remote_object_id: remoteObjectId });
  const result = await runGit(cwd, ['push', remote, '--delete', branch], state.policy, context);
  ensureGitOk(result, 'git_branch_delete_remote_failed');
  const after = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: 'narada.git.branch_delete_remote.v1',
    status: 'ok',
    operation: 'delete_remote',
    working_directory: cwd,
    scope_label: scopeLabel,
    remote,
    branch,
    base,
    remote_object_id: remoteObjectId,
    merge_check: 'passed',
    force: false,
    output: combineOutput(result),
    pre_status: before,
    ...verifiedMutationPostState(after, { operation: 'branch_delete_remote', remote, branch, base }),
    post_status: after,
    summary: `deleted merged remote branch ${remote}/${branch}`,
  };
  audit(state, payload);
  return payload;
}

export async function gitBranchSetUpstream(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_branch_set_upstream');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const before = await gitStatus({ working_directory: cwd }, state, context);
  const localBranch = resolveLocalBranchArgument(args.local_branch, before);
  const remote = requiredRemoteName(args.remote);
  const remoteBranch = optionalBranchName(args.remote_branch) ?? localBranch;
  await requireLocalBranch(cwd, localBranch, state, context);
  requireConfiguredRemote(before.remotes, remote);
  await requireRemoteBranch(cwd, remote, remoteBranch, state, context);
  const result = await runGit(cwd, ['branch', '--set-upstream-to', `${remote}/${remoteBranch}`, localBranch], state.policy, context);
  ensureGitOk(result, 'git_branch_set_upstream_failed');
  const after = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: 'narada.git.branch_set_upstream.v1',
    status: 'ok',
    operation: 'set_upstream',
    working_directory: cwd,
    scope_label: scopeLabel,
    local_branch: localBranch,
    remote,
    remote_branch: remoteBranch,
    output: combineOutput(result),
    pre_status: before,
    ...verifiedMutationPostState(after, { operation: 'branch_set_upstream', local_branch: localBranch, remote, remote_branch: remoteBranch }),
    post_status: after,
    summary: `set ${localBranch} upstream to ${remote}/${remoteBranch}`,
  };
  audit(state, payload);
  return payload;
}

export async function gitBranchUnsetUpstream(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_branch_unset_upstream');
  const cwd = resolveWorkingDirectory(args, state);
  await requireTopologyScopeForMutation(cwd, args, state, context);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const before = await gitStatus({ working_directory: cwd }, state, context);
  const localBranch = resolveLocalBranchArgument(args.local_branch, before);
  await requireLocalBranch(cwd, localBranch, state, context);
  const result = await runGit(cwd, ['branch', '--unset-upstream', localBranch], state.policy, context);
  ensureGitOk(result, 'git_branch_unset_upstream_failed');
  const after = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: 'narada.git.branch_unset_upstream.v1',
    status: 'ok',
    operation: 'unset_upstream',
    working_directory: cwd,
    scope_label: scopeLabel,
    local_branch: localBranch,
    output: combineOutput(result),
    pre_status: before,
    ...verifiedMutationPostState(after, { operation: 'branch_unset_upstream', local_branch: localBranch }),
    post_status: after,
    summary: `unset upstream for ${localBranch}`,
  };
  audit(state, payload);
  return payload;
}
export async function gitBeginWorkScope(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_begin_work_scope');
  const cwd = resolveWorkingDirectory(args, state);
  const ownerId = requiredNonEmptyString(args.owner_id, 'git_begin_work_scope_requires_owner_id');
  const authority = args.scope_kind === 'repository_topology' ? 'repository_topology' : 'paths';
  const requestedPaths = stringArray(args.allowed_paths);
  if (authority === 'paths' && requestedPaths.length === 0) throw diagnosticError('git_begin_work_scope_requires_allowed_paths');
  if (authority === 'repository_topology' && requestedPaths.length > 0) throw diagnosticError('git_repository_scope_does_not_accept_paths');
  const allowedPaths = requestedPaths.map((path) => {
    const validated = validateGitPathspec(path);
    if (validated === '.' || /[*?\[]/.test(validated)) {
      throw diagnosticError('git_begin_work_scope_requires_explicit_paths', 'git_begin_work_scope_requires_explicit_paths', { path: validated });
    }
    return validated.replaceAll('\\', '/');
  });
  const root = (await gitText(cwd, ['rev-parse', '--show-toplevel'], state.policy, 'git_begin_work_scope_failed', context)).trim();
  const baseState = await readGitBaseState(cwd, state, context);
  const suppliedBase = asRecord(args.base_state);
  for (const field of ['head', 'index_digest', 'worktree_digest'] as const) {
    if (suppliedBase[field] !== undefined && suppliedBase[field] !== baseState[field]) {
      throw diagnosticError('git_work_scope_base_state_mismatch', 'git_work_scope_base_state_mismatch', {
        field, supplied: suppliedBase[field], actual: baseState[field], mutation_started: false, atomic: true,
      });
    }
  }
  const registry = await gitPath(cwd, 'narada-work-scopes', state, context);
  if (!registry) throw diagnosticError('git_work_scope_registry_unavailable');
  mkdirSync(registry, { recursive: true });
  const token = createWorkScope({ repositoryRoot: root, ownerId, registryPath: registry, authority, allowedPaths, baseState });
  withWorkScopeRegistryLock(registry, () => {
    const now = Date.now();
    for (const name of readdirSync(registry)) {
      if (!name.endsWith('.json')) continue;
      const leasePath = join(registry, name);
      let lease: GitWorkScope;
      try { lease = JSON.parse(readFileSync(leasePath, 'utf8')) as GitWorkScope; } catch {
        throw diagnosticError('git_work_scope_registry_corrupt', 'git_work_scope_registry_corrupt', { lease_path: leasePath, mutation_started: false });
      }
      if (Date.parse(lease.expires_at) <= now) {
        rmSync(leasePath, { force: true });
        continue;
      }
      const overlap = token.authority === 'repository_topology' || lease.authority === 'repository_topology'
        ? ['<repository-topology>']
        : token.allowed_paths.filter((path) => lease.allowed_paths.some((held) => pathMatches(path, held) || pathMatches(held, path)));
      if (overlap.length) {
        throw diagnosticError('git_work_scope_path_already_owned', 'git_work_scope_path_already_owned', {
          requested_owner_id: ownerId, current_owner_id: lease.owner_id, current_work_scope_ref: lease.ref,
          paths: overlap, expires_at: lease.expires_at, mutation_started: false, atomic: true,
        });
      }
    }
    writeFileSync(join(registry, `${token.ref}.json`), JSON.stringify(token, null, 2), { flag: 'wx' });
  });
  storeScopeToken(state, token);
  return {
    schema: 'narada.git.work_scope.v1', status: 'ok', working_directory: cwd, repository_root: root,
    work_scope_ref: token.ref, owner_id: token.owner_id, authority: token.authority, allowed_paths: token.allowed_paths, base_state: token.base_state,
    created_at: token.created_at, expires_at: token.expires_at, mutation_started: true,
    summary: token.authority === 'repository_topology' ? 'repository topology scope leased' : `work scope leased for ${token.allowed_paths.length} path${token.allowed_paths.length === 1 ? '' : 's'}`,
  };
}

export async function gitEndWorkScope(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_end_work_scope');
  const cwd = resolveWorkingDirectory(args, state);
  const ownerId = requiredNonEmptyString(args.owner_id, 'git_end_work_scope_requires_owner_id');
  const workScopeRef = requiredNonEmptyString(args.work_scope_ref, 'git_end_work_scope_requires_work_scope_ref');
  if (!/^gws_[a-f0-9]{28}$/.test(workScopeRef)) throw diagnosticError('git_work_scope_ref_invalid');
  const root = (await gitText(cwd, ['rev-parse', '--show-toplevel'], state.policy, 'git_end_work_scope_failed', context)).trim();
  const registry = await gitPath(cwd, 'narada-work-scopes', state, context);
  if (!registry) throw diagnosticError('git_work_scope_registry_unavailable');
  let token: GitWorkScope | null = null;
  withWorkScopeRegistryLock(registry, () => {
    const leasePath = join(registry, `${workScopeRef}.json`);
    if (!existsSync(leasePath)) throw diagnosticError('git_work_scope_ref_not_found', 'git_work_scope_ref_not_found', { work_scope_ref: workScopeRef });
    try { token = JSON.parse(readFileSync(leasePath, 'utf8')) as GitWorkScope; } catch {
      throw diagnosticError('git_work_scope_registry_corrupt', 'git_work_scope_registry_corrupt', { lease_path: leasePath, mutation_started: false });
    }
    if (token!.repository_root !== root) throw diagnosticError('git_work_scope_repository_mismatch');
    if (token!.owner_id !== ownerId) throw diagnosticError('git_work_scope_owner_mismatch', 'git_work_scope_owner_mismatch', { expected_owner_id: token!.owner_id, supplied_owner_id: ownerId });
    rmSync(leasePath, { force: true });
  });
  scopeTokenMap(state).delete(workScopeRef);
  return { schema: 'narada.git.work_scope_end.v1', status: 'released', work_scope_ref: workScopeRef, owner_id: ownerId, released_paths: token!.allowed_paths, mutation_started: true };
}

const WORK_SCOPE_REGISTRY_MALFORMED_LOCK_GRACE_MS = 30_000;
const WORK_SCOPE_PROCESS_INSTANCE_ID = randomUUID();

type WorkScopeLockRecord = {
  schema: 'narada.git.work_scope_registry_lock.v2';
  pid: number;
  process_instance_id: string;
  lock_nonce: string;
  created_at: string;
};

function registerWorkScopeProcess(registry: string): void {
  const processDirectory = join(registry, '.processes');
  mkdirSync(processDirectory, { recursive: true });
  const path = join(processDirectory, `${process.pid}.json`);
  const temporaryPath = `${path}.${WORK_SCOPE_PROCESS_INSTANCE_ID}.tmp`;
  writeFileSync(temporaryPath, JSON.stringify({
    schema: 'narada.git.work_scope_process.v1',
    pid: process.pid,
    process_instance_id: WORK_SCOPE_PROCESS_INSTANCE_ID,
    heartbeat_at: new Date().toISOString(),
  }));
  renameSync(temporaryPath, path);
}

function registeredProcessInstance(registry: string, pid: number): string | null {
  try {
    const record = JSON.parse(readFileSync(join(registry, '.processes', `${pid}.json`), 'utf8')) as { process_instance_id?: unknown };
    return typeof record.process_instance_id === 'string' ? record.process_instance_id : null;
  } catch {
    return null;
  }
}

function withWorkScopeRegistryLock<T>(registry: string, action: () => T): T {
  registerWorkScopeProcess(registry);
  const lockPath = join(registry, '.lock');
  let recoveredStaleLock = false;
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      const record: WorkScopeLockRecord = {
        schema: 'narada.git.work_scope_registry_lock.v2',
        pid: process.pid,
        process_instance_id: WORK_SCOPE_PROCESS_INSTANCE_ID,
        lock_nonce: randomUUID(),
        created_at: new Date().toISOString(),
      };
      const handle = openSync(lockPath, 'wx');
      try {
        writeSync(handle, JSON.stringify(record));
        fsyncSync(handle);
      } finally {
        closeSync(handle);
      }
      try { return action(); } finally { rmSync(lockPath, { force: true }); }
    } catch (error) {
      if (!existsSync(lockPath)) throw error;
      const ageMs = Date.now() - statSync(lockPath).mtimeMs;
      let owner: WorkScopeLockRecord | null = null;
      try {
        const parsed = JSON.parse(readFileSync(lockPath, 'utf8')) as Partial<WorkScopeLockRecord>;
        if (typeof parsed.pid === 'number' && typeof parsed.process_instance_id === 'string') owner = parsed as WorkScopeLockRecord;
      } catch { /* malformed lock handled below */ }
      const ownerAlive = owner !== null && processIsAlive(owner.pid);
      const registeredInstance = owner ? registeredProcessInstance(registry, owner.pid) : null;
      const ownerInstanceMatches = owner !== null && registeredInstance === owner.process_instance_id;
      const stale = owner
        ? !ownerAlive || !ownerInstanceMatches
        : ageMs > WORK_SCOPE_REGISTRY_MALFORMED_LOCK_GRACE_MS;
      if (attempt === 0 && stale) {
        rmSync(lockPath, { force: true });
        recoveredStaleLock = true;
        continue;
      }
      throw diagnosticError('git_work_scope_registry_busy', 'git_work_scope_registry_busy', {
        mutation_started: false,
        retryable: true,
        lock_age_ms: ageMs,
        owner_pid: owner?.pid ?? null,
        owner_process_alive: ownerAlive,
        owner_process_instance_id: owner?.process_instance_id ?? null,
        registered_process_instance_id: registeredInstance,
        owner_process_instance_matches: ownerInstanceMatches,
        stale_lock_recovered: recoveredStaleLock,
      });
    }
  }
  throw diagnosticError('git_work_scope_registry_busy');
}

function processIsAlive(pid: number): boolean {
  try { process.kill(pid, 0); return true; } catch { return false; }
}
export async function gitStatus(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}): Promise<Record<string, unknown>> {
  const cwd = resolveWorkingDirectory(args, state);
  const root = await gitText(cwd, ['rev-parse', '--show-toplevel'], state.policy, 'git_status_failed', context);
  const status = await gitText(cwd, ['status', '--porcelain=v1', '-z', '-b', '--untracked-files=all'], state.policy, 'git_status_failed', context);
  const parsed = parseStatus(status);
  const workScope = args.work_scope_ref ? requireWorkScope(args.work_scope_ref, root.trim(), state) : null;
  const pathspecs = optionalPathspecs(args.pathspec, args.pathspecs);
  const filtered = filterStatus(parsed, {
    pathspecs,
    allowedPaths: workScope?.allowed_paths ?? null,
    stagedOnly: args.staged_only === true,
    includeUntracked: args.include_untracked !== false,
    format: stringEnum(args.format, ['full', 'paths', 'summary'], 'full') as 'full' | 'paths' | 'summary',
  });
  const branch = typeof filtered.branch === 'string' ? filtered.branch : null;
  const remotes = await listRemotes(cwd, state.policy, context);
  const configuredPushTarget = await resolvePushTarget(cwd, null, null, state.policy, context, remotes);
  return {
    schema: 'narada.git.status.v1',
    status: 'ok',
    working_directory: cwd,
    repository_root: root.trim(),
    ...filtered,
    query: {
      work_scope_ref: workScope?.ref ?? null,
      pathspecs,
      staged_only: args.staged_only === true,
      include_untracked: args.include_untracked !== false,
      format: filtered.format,
    },
    remotes,
    remote_names: remotes.map((remote) => remote.name),
    push_target: configuredPushTarget,
    push_remediation: pushRemediation(configuredPushTarget, branch, remotes),
  };
}

function filterStatus(parsed: Record<string, unknown>, options: {
  pathspecs: string[];
  allowedPaths: string[] | null;
  stagedOnly: boolean;
  includeUntracked: boolean;
  format: 'full' | 'paths' | 'summary';
}): Record<string, unknown> {
  const entries = Array.isArray(parsed.status_entries) ? parsed.status_entries.map(asStatusEntry) : [];
  const selected = entries.filter((entry) => {
    const path = String(entry.path ?? entry.display_path ?? '').replaceAll('\\', '/');
    const inScope = !options.allowedPaths || options.allowedPaths.some((allowed) => pathMatches(path, allowed));
    const inPathspec = options.pathspecs.length === 0 || options.pathspecs.some((pathspec) => pathMatches(path, pathspec));
    const staged = !options.stagedOnly || entry.staged === true;
    const untracked = options.includeUntracked || entry.untracked !== true;
    return inScope && inPathspec && staged && untracked;
  });
  const staged = selected.filter((entry) => entry.staged).map((entry) => entry.display_path);
  const unstaged = selected.filter((entry) => entry.unstaged).map((entry) => entry.display_path);
  const untracked = selected.filter((entry) => entry.untracked).map((entry) => entry.display_path);
  const conflicts = selected.filter((entry) => entry.conflict).map((entry) => entry.display_path);
  const summary = {
    staged_count: staged.length,
    unstaged_count: unstaged.length,
    untracked_count: untracked.length,
    conflict_count: conflicts.length,
    matching_path_count: selected.length,
    clean: staged.length + unstaged.length + untracked.length + conflicts.length === 0,
  };
  const full = {
    ...parsed,
    status_entries: selected,
    staged,
    unstaged,
    untracked,
    conflicts,
    clean: summary.clean,
    summary,
    format: options.format,
  };
  if (options.format === 'full') return full;
  return {
    ...full,
    status_entries: [],
    staged: [],
    unstaged: [],
    untracked: [],
    conflicts: [],
    paths: selected.map((entry) => entry.display_path),
  };
}

function pathMatches(path: string, pattern: string): boolean {
  const normalizedPath = path.replaceAll('\\', '/');
  const normalizedPattern = pattern.replaceAll('\\', '/');
  if (!/[*?\[]/.test(normalizedPattern)) {
    return normalizedPath === normalizedPattern || normalizedPath.startsWith(`${normalizedPattern.replace(/\/$/, '')}/`);
  }
  const expression = normalizedPattern
    .replace(/[.+^${}()|\\]/g, '\\$&')
    .replace(/\*/g, '.*')
    .replace(/\?/g, '.')
    .replace(/\[/g, '\\[')
    .replace(/\]/g, '\\]');
  return new RegExp(`^${expression}$`).test(normalizedPath);
}

async function readGitBaseState(cwd: string, state: GitMcpState, context: GitRequestContext): Promise<GitBaseState> {
  const headResult = await runGit(cwd, ['rev-parse', 'HEAD'], state.policy, context);
  const indexResult = await runGit(cwd, ['write-tree'], state.policy, context);
  const worktreeResult = await runGit(cwd, ['status', '--porcelain=v1', '-z', '--untracked-files=all'], state.policy, context);
  return {
    head: headResult.exit_code === 0 && !headResult.timed_out && !headResult.cancelled
      ? headResult.output_text.trim()
      : null,
    index_digest: indexResult.exit_code === 0 && !indexResult.timed_out && !indexResult.cancelled
      ? indexResult.output_text.trim()
      : null,
    worktree_digest: worktreeResult.exit_code === 0 && !worktreeResult.timed_out && !worktreeResult.cancelled
      ? sha256(worktreeResult.output_text)
      : null,
  };
}

function requireWorkScope(ref: unknown, repositoryRoot: string, state: GitMcpState): GitWorkScope {
  try {
    const token = resolveScopeToken(state, ref, 'work_scope');
    if (token.repository_root !== repositoryRoot) throw new Error('git_work_scope_repository_mismatch');
    return token as GitWorkScope;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw diagnosticError(message, message, {
      work_scope_ref: ref ?? null,
      repository_root: repositoryRoot,
      mutation_started: false,
      atomic: true,
    });
  }
}

function requireRepositoryTopologyScope(ref: unknown, repositoryRoot: string, state: GitMcpState): GitWorkScope {
  const scope = requireWorkScope(ref, repositoryRoot, state);
  if (scope.authority !== 'repository_topology') {
    throw diagnosticError('git_repository_topology_scope_required', 'git_repository_topology_scope_required', {
      work_scope_ref: scope.ref,
      supplied_authority: scope.authority ?? 'paths',
      mutation_started: false,
      atomic: true,
      remediation: 'Acquire git_begin_work_scope with scope_kind=repository_topology and no allowed_paths.',
    });
  }
  return scope;
}

async function requireTopologyScopeForMutation(
  cwd: string,
  args: Record<string, unknown>,
  state: GitMcpState,
  context: GitRequestContext,
  validateBaseState = true,
): Promise<GitWorkScope> {
  const repositoryRoot = (await gitText(cwd, ['rev-parse', '--show-toplevel'], state.policy, 'git_repository_topology_scope_failed', context)).trim();
  const scope = requireRepositoryTopologyScope(args.work_scope_ref, repositoryRoot, state);
  if (validateBaseState) assertTopologyScopeBaseState(scope, await readGitBaseState(cwd, state, context));
  return scope;
}

function assertPathsWithinWorkScope(paths: string[], workScope: GitWorkScope, code: string): void {
  const outOfScope = paths.filter((path) => !workScope.allowed_paths.some((allowed) => pathMatches(path, allowed)));
  if (outOfScope.length > 0) {
    throw diagnosticError(code, code, {
      work_scope_ref: workScope.ref,
      owner_id: workScope.owner_id,
      allowed_paths: workScope.allowed_paths,
      out_of_scope_paths: outOfScope,
      mutation_started: false,
      atomic: true,
    });
  }
}

function requireIndexScope(ref: unknown, repositoryRoot: string, state: GitMcpState): GitIndexScope {
  try {
    const token = resolveScopeToken(state, ref, 'index_scope');
    if (token.repository_root !== repositoryRoot) throw new Error('git_index_scope_repository_mismatch');
    return token as GitIndexScope;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw diagnosticError(message, message, {
      index_scope_ref: ref ?? null,
      repository_root: repositoryRoot,
      mutation_started: false,
      atomic: true,
    });
  }
}

function assertTopologyScopeBaseState(scope: GitWorkScope, current: GitBaseState): void {
  const changedFields = (['head', 'index_digest', 'worktree_digest'] as const)
    .filter((field) => scope.base_state[field] !== current[field]);
  if (changedFields.length > 0) {
    throw diagnosticError('git_repository_topology_scope_base_state_drift', 'git_repository_topology_scope_base_state_drift', {
      changed_fields: changedFields,
      expected_base_state: scope.base_state,
      actual_base_state: current,
      scope_ref: scope.ref,
      mutation_started: false,
      atomic: true,
      cooperative_boundary: true,
      remediation: 'Out-of-band Git or editor activity changed repository state after lease acquisition. Release this scope, reconcile the changes, and acquire a fresh repository_topology scope.',
    });
  }
}

function assertScopeHead(scope: GitWorkScope | GitIndexScope, current: GitBaseState): void {
  const expectedHead = scope.kind === 'work_scope' ? scope.base_state.head : scope.base_head;
  if (expectedHead !== current.head) {
    throw diagnosticError('git_work_scope_base_state_drift', 'git_work_scope_base_state_drift', {
      expected_head: expectedHead,
      actual_head: current.head,
      scope_ref: scope.ref,
      mutation_started: false,
      atomic: true,
      remediation: 'Create a fresh work scope after the repository HEAD changes; unrelated worktree edits remain untouched.',
    });
  }
}

function compactPostState(status: Record<string, unknown>, verification: 'verified' | 'unknown' = 'verified') {
  const summary = asRecord(status.summary);
  return {
    verification,
    branch: status.branch ?? null,
    upstream: status.upstream ?? null,
    ahead: status.ahead ?? 0,
    behind: status.behind ?? 0,
    clean: status.clean === true,
    staged_count: Number(summary.staged_count ?? stringArray(status.staged).length),
    unstaged_count: Number(summary.unstaged_count ?? stringArray(status.unstaged).length),
    untracked_count: Number(summary.untracked_count ?? stringArray(status.untracked).length),
    conflict_count: Number(summary.conflict_count ?? stringArray(status.conflicts).length),
    matching_path_count: Number(summary.matching_path_count ?? stringArray(status.status_entries).length),
  };
}

function verifiedMutationPostState(status: Record<string, unknown>, mutationEffect: Record<string, unknown>) {
  const postState = compactPostState(status);
  return {
    mutation_effect: mutationEffect,
    post_state: postState,
    verification_status: postState.verification,
    // Compatibility name retained while callers migrate to the concise
    // mutation contract above.
    verified_post_state: postState,
  };
}

function normalizeExpectedCommit(value: unknown): string | null {
  if (value === undefined || value === null || value === '') return null;
  const raw = String(value).trim();
  const commit = raw.startsWith('git_commit:') ? raw.slice('git_commit:'.length) : raw;
  if (!/^[0-9a-fA-F]{7,64}$/.test(commit)) {
    throw diagnosticError('git_expected_commit_invalid', 'git_expected_commit_invalid', { expected_commit: raw });
  }
  return commit.toLowerCase();
}

export async function gitRepositoriesSummary(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  const workingDirectories = stringArray(args.working_directories);
  if (workingDirectories.length === 0) throw diagnosticError('git_repositories_summary_requires_working_directories');
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const expectedPathsByRepository = asRecord(args.expected_paths_by_repository);
  const repositories = [];
  for (const workingDirectory of workingDirectories) {
    const status = await gitStatus({ working_directory: workingDirectory }, state, context);
    const repositoryRoot = String(status.repository_root ?? workingDirectory);
    const expectedPaths = stringArray(expectedPathsByRepository[repositoryRoot] ?? expectedPathsByRepository[workingDirectory]);
    const statusEntries = Array.isArray(status.status_entries) ? status.status_entries.map(asStatusEntry) : [];
    const dirtyPaths = statusEntries.map((entry) => String(entry.display_path ?? entry.path ?? '')).filter(Boolean);
    const expectedSet = new Set(expectedPaths);
    const unexpectedDirtyPaths = expectedPaths.length > 0
      ? dirtyPaths.filter((path) => !expectedSet.has(path))
      : [];
    const latestCommitResult = await runGit(String(status.working_directory), ['log', '-1', '--pretty=format:%H%x1f%h%x1f%s'], state.policy, context);
    const [hash = null, shortHash = null, subject = null] = latestCommitResult.exit_code === 0
      ? latestCommitResult.output_text.split('\x1f')
      : [];
    repositories.push({
      working_directory: status.working_directory,
      repository_root: status.repository_root,
      branch: status.branch,
      upstream: status.upstream,
      ahead: status.ahead,
      behind: status.behind,
      clean: status.clean,
      staged: status.staged,
      unstaged: status.unstaged,
      untracked: status.untracked,
      conflicts: status.conflicts,
      remotes: status.remotes,
      push_target: status.push_target,
      push_remediation: status.push_remediation,
      expected_paths: expectedPaths,
      unexpected_dirty_paths: unexpectedDirtyPaths,
      latest_commit: hash ? { hash, short_hash: shortHash, subject } : null,
    });
  }
  return {
    schema: 'narada.git.repositories_summary.v1',
    status: 'ok',
    scope_label: scopeLabel,
    repository_count: repositories.length,
    repositories,
  };
}

export async function gitWorkflowRecord(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_workflow_record');
  const scopeLabel = requiredNonEmptyString(args.scope_label, 'git_workflow_record_requires_scope_label');
  const repositoriesInput = arrayOfRecords(args.repositories);
  if (repositoriesInput.length === 0) throw diagnosticError('git_workflow_record_requires_repositories');
  const repositories = [];
  for (const input of repositoriesInput) {
    const cwd = resolveWorkingDirectory({ working_directory: input.working_directory }, state);
    const status = await gitStatus({ working_directory: cwd }, state, context);
    repositories.push({
      working_directory: cwd,
      repository_root: status.repository_root,
      branch: status.branch,
      upstream: status.upstream,
      staged_paths: stringArray(input.staged_paths),
      committed_sha: optionalNonEmptyString(input.committed_sha),
      pushed: input.pushed === true,
      push_status: stringEnum(input.push_status, WORKFLOW_PUSH_STATUSES, 'not_attempted'),
      push_reason: optionalNonEmptyString(input.push_reason),
      unrelated_dirty_paths_left: stringArray(input.unrelated_dirty_paths_left),
      ...verifiedMutationPostState(status, { operation: 'workflow_record_repository' }),
      post_status: status,
    });
  }
  const record = {
    schema: 'narada.git.workflow_record.v1',
    status: 'recorded',
    workflow_id: optionalNonEmptyString(args.workflow_id) ?? `gitwf_${new Date().toISOString().replace(/[-:.]/g, '').slice(0, 15)}_${randomUUID().slice(0, 8)}`,
    scope_label: scopeLabel,
    recorded_at: new Date().toISOString(),
    summary: optionalNonEmptyString(args.summary),
    repositories,
  };
  const ledgerPath = appendWorkflowLedger(state, record);
  return { ...record, ledger_path: ledgerPath };
}

export async function gitDiff(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  const cwd = resolveWorkingDirectory(args, state);
  const scope = stringEnum(args.scope, ['working', 'staged', 'commit'], 'working');
  const pathspecs = optionalPathspecs(args.pathspec, args.pathspecs);
  const offset = clampInteger(args.offset, 0, Number.MAX_SAFE_INTEGER, 0);
  const limit = clampInteger(args.limit ?? args.diff_limit, 1, MAX_DIFF_LIMIT, DEFAULT_DIFF_LIMIT);
  const includeUntracked = args.include_untracked === true;
  if (includeUntracked && scope !== 'working') throw diagnosticError('git_diff_include_untracked_requires_working_scope');
  const gitArgs = scope === 'staged'
    ? ['diff', '--cached', '--no-ext-diff']
    : scope === 'commit'
      ? ['show', '--format=', '--patch', '--no-ext-diff', requireCommitish(args.commit)]
      : ['diff', '--no-ext-diff'];
  if (pathspecs.length > 0) gitArgs.push('--', ...pathspecs);
  const result = await runGit(cwd, gitArgs, state.policy, context);
  ensureGitOk(result, 'git_diff_failed');
  const untracked = includeUntracked ? await untrackedDiff(cwd, pathspecs, state.policy, context) : null;
  const fullDiff = [result.output_text, untracked?.diff].filter(Boolean).join(result.output_text && untracked?.diff ? '\n' : '');
  const diff = fullDiff.slice(offset, offset + limit);
  const nextOffset = offset + diff.length < fullDiff.length ? offset + diff.length : null;
  return {
    schema: 'narada.git.diff.v1',
    status: 'ok',
    working_directory: cwd,
    scope,
    pathspec: pathspecs.length === 1 ? pathspecs[0] : null,
    pathspecs,
    ...(scope === 'commit' ? { commit: String(args.commit) } : {}),
    offset,
    limit,
    next_offset: nextOffset,
    include_untracked: includeUntracked,
    untracked_diff_included: includeUntracked,
    ...(untracked ? {
      untracked_paths: untracked.paths,
      untracked_paths_omitted: untracked.pathsOmitted,
      untracked_diff_truncated: untracked.truncated,
    } : {}),
    diff,
    diff_preview: diff.slice(0, PREVIEW_CHAR_LIMIT),
    diff_omitted: false,
    diff_truncated: result.output_truncated || nextOffset !== null || (untracked?.truncated ?? false),
    diff_char_length: fullDiff.length,
  };
}

export async function gitAdd(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_add');
  const cwd = resolveWorkingDirectory(args, state);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const requestedPaths = stringArray(args.paths);
  if (requestedPaths.length === 0) throw diagnosticError('git_add_requires_paths');
  const before = await gitStatus({ working_directory: cwd }, state, context);
  const repositoryRoot = String(before.repository_root ?? cwd);
  const workScope = requireWorkScope(args.work_scope_ref, repositoryRoot, state);
  const baseState = await readGitBaseState(cwd, state, context);
  assertScopeHead(workScope, baseState);
  {
    const existingOutOfScope = stringArray(before.staged)
      .map(normalizeCommitScopePath)
      .filter((path) => path && !workScope.allowed_paths.some((allowed) => pathMatches(path, allowed)));
    if (existingOutOfScope.length > 0) {
      throw diagnosticError('git_work_scope_has_out_of_scope_staged_paths', 'git_work_scope_has_out_of_scope_staged_paths', {
        work_scope_ref: workScope.ref,
        out_of_scope_staged_paths: existingOutOfScope,
        mutation_started: false,
        atomic: true,
        remediation: 'Unstage the unrelated paths explicitly or start a scope that includes the complete intended index.',
      });
    }
  }
  const paths = await expandGitAddPaths(cwd, requestedPaths, state, context);
  {
    const outOfScope = paths.filter((path) => !workScope.allowed_paths.some((allowed) => pathMatches(path, allowed)));
    if (outOfScope.length > 0) {
      throw diagnosticError('git_add_paths_outside_work_scope', 'git_add_paths_outside_work_scope', {
        work_scope_ref: workScope.ref,
        allowed_paths: workScope.allowed_paths,
        out_of_scope_paths: outOfScope,
        mutation_started: false,
        atomic: true,
      });
    }
  }
  await preflightGitAddPaths(cwd, paths, state, context);
  const result = await runGit(cwd, ['add', '--', ...requestedPaths], state.policy, context);
  ensureGitOk(result, 'git_add_failed');
  const status = await gitStatus({ working_directory: cwd }, state, context);
  const stagedPaths = stringArray(status.staged).map(normalizeCommitScopePath).filter(Boolean);
  if (workScope) {
    const outOfScope = stagedPaths.filter((path) => !workScope.allowed_paths.some((allowed) => pathMatches(path, allowed)));
    if (outOfScope.length > 0) {
      throw diagnosticError('git_add_produced_out_of_scope_index', 'git_add_produced_out_of_scope_index', {
        work_scope_ref: workScope.ref,
        out_of_scope_staged_paths: outOfScope,
        mutation_started: true,
        atomic: false,
        remediation: 'The index changed unexpectedly; inspect git_status and unstage only the unrelated paths before retrying.',
      });
    }
  }
  const afterBaseState = await readGitBaseState(cwd, state, context);
  const indexScope = workScope
    ? createIndexScope({
        repositoryRoot,
        workScopeRef: workScope.ref,
        stagedPaths,
        indexDigest: afterBaseState.index_digest,
        baseHead: workScope.base_state.head,
      })
    : null;
  if (indexScope) storeScopeToken(state, indexScope);
  const payload = {
    schema: 'narada.git.add.v1',
    status: 'ok',
    working_directory: cwd,
    scope_label: scopeLabel,
    paths,
    requested_paths: requestedPaths,
    staged_count: stagedPaths.length,
    work_scope_ref: workScope?.ref ?? null,
    index_scope_ref: indexScope?.ref ?? null,
    index_scope: indexScope,
    preflight: {
      status: 'passed',
      checked_path_count: paths.length,
      ignored_path_count: 0,
      atomic: true,
    },
    summary: `staged ${stagedPaths.length} path${stagedPaths.length === 1 ? '' : 's'}`,
    ...verifiedMutationPostState(status, { operation: 'add', requested_paths: requestedPaths, staged_paths: stagedPaths }),
    post_status: status,
  };
  audit(state, payload);
  return payload;
}

async function expandGitAddPaths(cwd: string, requestedPaths: string[], state: GitMcpState, context: GitRequestContext): Promise<string[]> {
  const expanded: string[] = [];
  for (const requested of requestedPaths) {
    const path = validateGitPathspec(requested);
    if (path === '.') throw diagnosticError('git_broad_path_not_allowed', 'git_broad_path_not_allowed', { path });
    if (/[*?\[]/.test(path)) throw diagnosticError('git_add_requires_explicit_paths', 'git_add_requires_explicit_paths', { path });
    const absolute = resolve(cwd, path);
    if (existsSync(absolute) && statSync(absolute).isDirectory()) {
      const result = await runGit(cwd, ['ls-files', '-z', '--cached', '--others', '--exclude-standard', '--', path], state.policy, context);
      ensureGitOk(result, 'git_add_directory_enumeration_failed');
      const children = result.output_text.split('\0').filter(Boolean);
      if (children.length === 0) {
        throw diagnosticError('git_add_directory_has_no_stageable_paths', 'git_add_directory_has_no_stageable_paths', {
          path,
          mutation_started: false,
          remediation: 'Select a tracked or non-ignored file under the directory explicitly.',
        });
      }
      expanded.push(...children);
      continue;
    }
    expanded.push(await validateExplicitFilePath(cwd, path, (workdir, gitArgs) => runGit(workdir, gitArgs, state.policy, context)));
  }
  return [...new Set(expanded.map((path) => path.replaceAll('\\', '/')))];
}

async function preflightGitAddPaths(cwd: string, paths: string[], state: GitMcpState, context: GitRequestContext): Promise<void> {
  const checks = await Promise.all(paths.map(async (path) => {
    const result = await runGit(cwd, ['check-ignore', '--verbose', '--', path], state.policy, context);
    if (result.exit_code === 0 && !result.timed_out && !result.cancelled) {
      return {
        path,
        ignored: true,
        diagnostic_text: combineOutput(result),
      };
    }
    if (result.exit_code === 1 && !result.timed_out && !result.cancelled) {
      return { path, ignored: false };
    }
    throw diagnosticError('git_add_ignore_check_failed', 'git_add_ignore_check_failed', {
      path,
      exit_code: result.exit_code,
      timed_out: result.timed_out,
      cancelled: result.cancelled,
      diagnostic_text: combineOutput(result),
    });
  }));
  const ignoredEntries = checks.filter((check) => check.ignored);
  if (ignoredEntries.length === 0) return;
  throw diagnosticError('git_add_ignored_paths', 'git_add_ignored_paths', {
    requested_paths: paths,
    ignored_paths: ignoredEntries.map((entry) => entry.path),
    ignored_entries: ignoredEntries,
    preflight: 'failed',
    mutation_started: false,
    atomic: true,
    remediation: 'Remove ignored paths from the request or update the repository ignore policy; git_add does not force ignored files.',
  });
}

async function reconcileSharedIndex(cwd: string, commit: string, committedPaths: string[], state: GitMcpState, context: GitRequestContext) {
  const indexPath = await gitPath(cwd, 'index', state, context);
  if (!indexPath) return { status: 'pending', reason: 'shared_index_path_unavailable' };
  const lockPath = `${indexPath}.lock`;
  const reconciliationIndex = await gitPath(cwd, `narada-indexes/reconcile-${randomUUID()}.index`, state, context);
  if (!reconciliationIndex) return { status: 'pending', reason: 'reconciliation_index_path_unavailable' };
  let lockOwned = false;
  try {
    const handle = openSync(lockPath, 'wx');
    closeSync(handle);
    lockOwned = true;
  } catch {
    return { status: 'pending', reason: 'shared_index_locked', retryable: true };
  }
  try {
    mkdirSync(dirname(reconciliationIndex), { recursive: true });
    if (existsSync(indexPath)) copyFileSync(indexPath, reconciliationIndex);
    else {
      const emptyContext = { ...context, env: { ...(context.env ?? process.env), GIT_INDEX_FILE: reconciliationIndex } };
      ensureGitOk(await runGit(cwd, ['read-tree', commit], state.policy, emptyContext), 'git_commit_paths_reconciliation_read_tree_failed');
    }
    const reconciliationContext = { ...context, env: { ...(context.env ?? process.env), GIT_INDEX_FILE: reconciliationIndex } };
    const result = await runGit(cwd, ['reset', commit, '--', ...committedPaths], state.policy, reconciliationContext);
    if (result.exit_code !== 0 || result.timed_out || result.cancelled) {
      return { status: 'pending', reason: 'reset_failed', exit_code: result.exit_code, output: result.output_text };
    }
    copyFileSync(reconciliationIndex, lockPath);
    renameSync(lockPath, indexPath);
    lockOwned = false;
    return { status: 'reconciled', lock_protocol: 'git_index_lock_atomic_replace' };
  } finally {
    rmSync(reconciliationIndex, { force: true });
    if (lockOwned) rmSync(lockPath, { force: true });
  }
}

type GitIndexReconciliationRecord = {
  schema: 'narada.git.index_reconciliation.v1';
  reconciliation_ref: string;
  repository_root: string;
  commit: string;
  paths: string[];
  work_scope_ref: string;
  created_at: string;
};

async function persistIndexReconciliation(cwd: string, commit: string, paths: string[], workScopeRef: string, state: GitMcpState, context: GitRequestContext): Promise<GitIndexReconciliationRecord> {
  const reconciliationRef = `gir_${randomUUID().replaceAll('-', '').slice(0, 28)}`;
  const directory = await gitPath(cwd, 'narada-index-reconciliations', state, context);
  if (!directory) throw diagnosticError('git_index_reconciliation_store_unavailable');
  mkdirSync(directory, { recursive: true });
  const repositoryRoot = (await gitText(cwd, ['rev-parse', '--show-toplevel'], state.policy, 'git_index_reconciliation_store_failed', context)).trim();
  const record: GitIndexReconciliationRecord = {
    schema: 'narada.git.index_reconciliation.v1',
    reconciliation_ref: reconciliationRef,
    repository_root: repositoryRoot,
    commit,
    paths,
    work_scope_ref: workScopeRef,
    created_at: new Date().toISOString(),
  };
  writeFileSync(join(directory, `${reconciliationRef}.json`), JSON.stringify(record, null, 2), { flag: 'wx' });
  return record;
}

export async function gitReconcileIndex(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_reconcile_index');
  const cwd = resolveWorkingDirectory(args, state);
  const reconciliationRef = requiredNonEmptyString(args.reconciliation_ref, 'git_reconcile_index_requires_reconciliation_ref');
  if (!/^gir_[a-f0-9]{28}$/.test(reconciliationRef)) throw diagnosticError('git_index_reconciliation_ref_invalid');
  const directory = await gitPath(cwd, 'narada-index-reconciliations', state, context);
  if (!directory) throw diagnosticError('git_index_reconciliation_store_unavailable');
  const recordPath = join(directory, `${reconciliationRef}.json`);
  if (!existsSync(recordPath)) throw diagnosticError('git_index_reconciliation_ref_not_found');
  let record: GitIndexReconciliationRecord;
  try { record = JSON.parse(readFileSync(recordPath, 'utf8')) as GitIndexReconciliationRecord; } catch {
    throw diagnosticError('git_index_reconciliation_record_corrupt', 'git_index_reconciliation_record_corrupt', { reconciliation_ref: reconciliationRef });
  }
  const root = (await gitText(cwd, ['rev-parse', '--show-toplevel'], state.policy, 'git_reconcile_index_failed', context)).trim();
  if (record.repository_root !== root) throw diagnosticError('git_index_reconciliation_repository_mismatch');
  const workScope = requireWorkScope(args.work_scope_ref, root, state);
  if (workScope.ref !== record.work_scope_ref) throw diagnosticError('git_index_reconciliation_work_scope_mismatch');
  assertPathsWithinWorkScope(record.paths, workScope, 'git_index_reconciliation_paths_outside_work_scope');
  const reconciliation = await reconcileSharedIndex(cwd, record.commit, record.paths, state, context);
  if (reconciliation.status !== 'reconciled') {
    return { schema: 'narada.git.index_reconciliation_result.v1', status: 'pending', reconciliation_ref: reconciliationRef, reconciliation, mutation_started: false };
  }
  rmSync(recordPath, { force: true });
  const postStatus = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: 'narada.git.index_reconciliation_result.v1', status: 'reconciled', reconciliation_ref: reconciliationRef,
    commit: record.commit, reconciled_paths: record.paths, reconciliation, mutation_started: true, post_status: postStatus,
  };
  audit(state, payload);
  return payload;
}

export async function gitCommitPaths(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_commit_paths');
  const cwd = resolveWorkingDirectory(args, state);
  const message = requiredNonEmptyString(args.message, 'git_commit_paths_requires_message');
  const body = optionalNonEmptyString(args.body);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const requestedPaths = stringArray(args.paths);
  if (requestedPaths.length === 0) throw diagnosticError('git_commit_paths_requires_paths');
  const paths = await expandGitAddPaths(cwd, requestedPaths, state, context);
  await preflightGitAddPaths(cwd, paths, state, context);
  const before = await gitStatus({ working_directory: cwd }, state, context);
  const repositoryRoot = String(before.repository_root ?? cwd);
  const workScope = requireWorkScope(args.work_scope_ref, repositoryRoot, state);
  assertScopeHead(workScope, await readGitBaseState(cwd, state, context));
  assertPathsWithinWorkScope(paths, workScope, 'git_commit_paths_outside_work_scope');
  const stagedBefore = stringArray(before.staged).map(normalizeCommitScopePath).filter(Boolean);
  const overlap = paths.filter((path) => stagedBefore.some((staged) => pathMatches(staged, path) || pathMatches(path, staged)));
  if (overlap.length) throw diagnosticError('git_commit_paths_shared_index_overlap', 'git_commit_paths_shared_index_overlap', { paths: overlap, mutation_started: false, atomic: true });
  const conflicts = stringArray(before.conflicts).map(normalizeCommitScopePath).filter(Boolean);
  const requestedConflicts = paths.filter((path) => conflicts.some((item) => pathMatches(item, path) || pathMatches(path, item)));
  if (requestedConflicts.length) throw diagnosticError('git_commit_paths_conflicts', 'git_commit_paths_conflicts', { conflict_paths: requestedConflicts, mutation_started: false, atomic: true });
  const expectedHead = (await gitText(cwd, ['rev-parse', 'HEAD'], state.policy, 'git_commit_paths_head_unavailable', context)).trim();
  const branchRef = (await gitText(cwd, ['symbolic-ref', '-q', 'HEAD'], state.policy, 'git_commit_paths_requires_attached_branch', context)).trim();
  const isolatedIndex = await gitPath(cwd, `narada-indexes/${randomUUID()}.index`, state, context);
  if (!isolatedIndex) throw diagnosticError('git_commit_paths_index_path_unavailable');
  mkdirSync(dirname(isolatedIndex), { recursive: true });
  const isolatedContext = { ...context, env: { ...(context.env ?? process.env), GIT_INDEX_FILE: isolatedIndex } };
  let commit = '';
  let committedPaths: string[] = [];
  try {
    ensureGitOk(await runGit(cwd, ['read-tree', expectedHead], state.policy, isolatedContext), 'git_commit_paths_read_tree_failed');
    ensureGitOk(await runGit(cwd, ['add', '--', ...paths], state.policy, isolatedContext), 'git_commit_paths_add_failed');
    const staged = await runGit(cwd, ['diff', '--cached', '--name-only', '-z'], state.policy, isolatedContext);
    ensureGitOk(staged, 'git_commit_paths_diff_failed');
    committedPaths = staged.output_text.split('\0').filter(Boolean).map((path) => path.replaceAll('\\', '/'));
    if (!committedPaths.length) throw diagnosticError('git_commit_paths_requires_changes');
    const tree = (await gitText(cwd, ['write-tree'], state.policy, 'git_commit_paths_write_tree_failed', isolatedContext)).trim();
    const commitArgs = ['commit-tree', tree, '-p', expectedHead, '-m', message];
    if (body) commitArgs.push('-m', body);
    commit = (await gitText(cwd, commitArgs, state.policy, 'git_commit_paths_commit_tree_failed', isolatedContext)).trim();
    const update = await runGit(cwd, ['update-ref', branchRef, commit, expectedHead], state.policy, context);
    if (update.exit_code !== 0 || update.timed_out || update.cancelled) throw diagnosticError('git_commit_paths_head_changed', 'git_commit_paths_head_changed', { expected_head: expectedHead, candidate_commit: commit, mutation_started: false, atomic: true });
    const reconciliation = await reconcileSharedIndex(cwd, commit, committedPaths, state, context);
    if (reconciliation.status !== 'reconciled') {
      const record = await persistIndexReconciliation(cwd, commit, committedPaths, workScope.ref, state, context);
      const pending = { schema: 'narada.git.commit_paths.v1', status: 'committed_shared_index_reconciliation_required', working_directory: cwd, scope_label: scopeLabel, isolation: 'dedicated_temporary_index', expected_head: expectedHead, branch_ref: branchRef, commit, commit_ref: `git_commit:${commit}`, committed_files: committedPaths, committed_count: committedPaths.length, mutation_performed: true, verification_status: 'commit_ref_updated_reconciliation_pending', reconciliation_ref: record.reconciliation_ref, reconciliation: { ...reconciliation, retry_tool: 'git_reconcile_index', retry_arguments: { working_directory: cwd, reconciliation_ref: record.reconciliation_ref, work_scope_ref: workScope.ref } }, post_status: null, summary: 'commit published locally; call git_reconcile_index with the durable reconciliation reference' };
      audit(state, pending);
      return pending;
    }
  } finally {
    rmSync(isolatedIndex, { force: true });
  }
  const postStatus = await gitStatus({ working_directory: cwd }, state, context);
  const payload = { schema: 'narada.git.commit_paths.v1', status: 'committed', working_directory: cwd, scope_label: scopeLabel, isolation: 'dedicated_temporary_index', expected_head: expectedHead, branch_ref: branchRef, commit, commit_ref: `git_commit:${commit}`, committed_files: committedPaths, committed_count: committedPaths.length, shared_index_preserved_paths: stringArray(postStatus.staged), ...verifiedMutationPostState(postStatus, { operation: 'commit_paths', commit, paths: committedPaths }), post_status: postStatus, summary: `committed ${committedPaths.length} explicit path${committedPaths.length === 1 ? '' : 's'} through an isolated index` };
  audit(state, payload);
  return payload;
}

export async function gitCommit(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_commit');
  const cwd = resolveWorkingDirectory(args, state);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const message = requiredNonEmptyString(args.message, 'git_commit_requires_message');
  const body = optionalNonEmptyString(args.body);
  const expectedStagedPaths = optionalExpectedStagedPaths(args.expected_staged_paths);
  const statusBefore = await gitStatus({ working_directory: cwd }, state, context);
  const repositoryRoot = String(statusBefore.repository_root ?? cwd);
  const workScope = requireWorkScope(args.work_scope_ref, repositoryRoot, state);
  const indexScope = args.index_scope_ref ? requireIndexScope(args.index_scope_ref, repositoryRoot, state) : null;
  const baseStateBefore = await readGitBaseState(cwd, state, context);
  if (workScope) assertScopeHead(workScope, baseStateBefore);
  if (indexScope) {
    assertScopeHead(indexScope, baseStateBefore);
    if (indexScope.work_scope_ref && workScope?.ref !== indexScope.work_scope_ref) {
      throw diagnosticError('git_index_scope_work_scope_mismatch', 'git_index_scope_work_scope_mismatch', {
        index_scope_ref: indexScope.ref,
        index_work_scope_ref: indexScope.work_scope_ref,
        supplied_work_scope_ref: workScope?.ref ?? null,
        mutation_started: false,
        atomic: true,
      });
    }
    if (indexScope.index_digest !== baseStateBefore.index_digest) {
      throw diagnosticError('git_index_scope_state_drift', 'git_index_scope_state_drift', {
        index_scope_ref: indexScope.ref,
        expected_index_digest: indexScope.index_digest,
        actual_index_digest: baseStateBefore.index_digest,
        mutation_started: false,
        atomic: true,
        remediation: 'The index changed after git_add; inspect the index and obtain a fresh index_scope_ref.',
      });
    }
  }
  if (!Array.isArray(statusBefore.staged) || statusBefore.staged.length === 0) {
    throw diagnosticError('git_commit_requires_staged_changes');
  }
  const actualStagedPaths = Array.isArray(statusBefore.status_entries)
    ? statusBefore.status_entries
      .filter((entry) => asStatusEntry(entry).staged)
      .map((entry) => normalizeCommitScopePath(asStatusEntry(entry).display_path ?? asStatusEntry(entry).path))
      .filter(Boolean)
    : stringArray(statusBefore.staged).map(normalizeCommitScopePath).filter(Boolean);
  const unstagedPaths = (stringArray(statusBefore.unstaged) ?? []).map(normalizeCommitScopePath).filter(Boolean);
  const untrackedPaths = (stringArray(statusBefore.untracked) ?? []).map(normalizeCommitScopePath).filter(Boolean);
  const conflictPaths = (stringArray(statusBefore.conflicts) ?? []).map(normalizeCommitScopePath).filter(Boolean);
  if (workScope) {
    const stagedAuthorityPaths = Array.isArray(statusBefore.status_entries)
      ? statusBefore.status_entries
        .filter((entry) => asStatusEntry(entry).staged)
        .flatMap((entry) => {
          const statusEntry = asStatusEntry(entry);
          return [statusEntry.path, statusEntry.original_path].filter((path): path is string => Boolean(path));
        })
        .map(normalizeCommitScopePath)
        .filter(Boolean)
      : actualStagedPaths;
    const outOfScope = stagedAuthorityPaths.filter((path) => !workScope.allowed_paths.some((allowed) => pathMatches(path, allowed)));
    if (outOfScope.length > 0) {
      throw diagnosticError('git_commit_paths_outside_work_scope', 'git_commit_paths_outside_work_scope', {
        work_scope_ref: workScope.ref,
        out_of_scope_staged_paths: outOfScope,
        mutation_started: false,
        atomic: true,
        remediation: 'Unstage unrelated paths or create a scope covering the complete intended index.',
      });
    }
  }
  if (indexScope) {
    const expectedSet = new Set(indexScope.staged_paths);
    const actualSet = new Set(actualStagedPaths);
    const missingPaths = indexScope.staged_paths.filter((path) => !actualSet.has(path));
    const unexpectedPaths = actualStagedPaths.filter((path) => !expectedSet.has(path));
    if (missingPaths.length > 0 || unexpectedPaths.length > 0) {
      throw diagnosticError('git_index_scope_staged_set_mismatch', 'git_index_scope_staged_set_mismatch', {
        index_scope_ref: indexScope.ref,
        expected_staged_paths: indexScope.staged_paths,
        actual_staged_paths: actualStagedPaths,
        missing_paths: missingPaths,
        unexpected_paths: unexpectedPaths,
        mutation_started: false,
        atomic: true,
      });
    }
  }
  if (!expectedStagedPaths && !indexScope && !workScope && (unstagedPaths.length > 0 || untrackedPaths.length > 0 || conflictPaths.length > 0)) {
    throw diagnosticError('git_commit_scope_required_for_mixed_worktree', 'git_commit_scope_required_for_mixed_worktree', {
      scope_label: scopeLabel,
      staged_paths: actualStagedPaths,
      unstaged_paths: unstagedPaths,
      untracked_paths: untrackedPaths,
      conflict_paths: conflictPaths,
      mutation_started: false,
      atomic: true,
      remediation: 'Pass expected_staged_paths containing exactly the intended staged paths; git_commit will not infer scope from a mixed worktree.',
    });
  }
  if (expectedStagedPaths) {
    const expectedSet = new Set(expectedStagedPaths);
    const actualSet = new Set(actualStagedPaths);
    const missingPaths = expectedStagedPaths.filter((path) => !actualSet.has(path));
    const unexpectedPaths = actualStagedPaths.filter((path) => !expectedSet.has(path));
    if (missingPaths.length > 0 || unexpectedPaths.length > 0) {
      throw diagnosticError('git_commit_staged_scope_mismatch', 'git_commit_staged_scope_mismatch', {
        scope_label: scopeLabel,
        expected_staged_paths: expectedStagedPaths,
        actual_staged_paths: actualStagedPaths,
        missing_paths: missingPaths,
        unexpected_paths: unexpectedPaths,
        mutation_started: false,
        atomic: true,
        remediation: 'Stage exactly the expected paths before retrying, or omit expected_staged_paths only when committing the whole current index is intentional.',
      });
    }
  }
  const commitArgs = ['commit', '-m', message];
  if (body) commitArgs.push('-m', body);
  const result = await runGit(cwd, commitArgs, state.policy, context);
  ensureGitOk(result, 'git_commit_failed');
  const commit = (await gitText(cwd, ['rev-parse', 'HEAD'], state.policy, 'git_commit_failed', context)).trim();
  const committedEntries = Array.isArray(statusBefore.status_entries)
    ? statusBefore.status_entries.filter((entry) => asStatusEntry(entry).staged)
    : [];
  const committedFiles = committedEntries.map((entry) => asStatusEntry(entry).display_path).filter(Boolean);
  const postStatus = await gitStatus({ working_directory: cwd }, state, context);
  const payload = {
    schema: 'narada.git.commit.v1',
    status: 'ok',
    working_directory: cwd,
    scope_label: scopeLabel,
    commit,
    committed_entries: committedEntries,
    committed_files: committedFiles,
    committed_file_count: committedEntries.length,
    commit_ref: `git_commit:${commit}`,
    work_scope_ref: workScope?.ref ?? indexScope?.work_scope_ref ?? null,
    index_scope_ref: indexScope?.ref ?? null,
    summary: firstNonEmptyLine(result.output_text) ?? `created ${commit}`,
    output: combineOutput(result),
    ...verifiedMutationPostState(postStatus, { operation: 'commit', commit, committed_files: committedFiles }),
    post_status: postStatus,
  };
  audit(state, payload);
  return payload;
}

export async function gitPush(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  requireWriteMode(state.policy, 'git_push');
  const cwd = resolveWorkingDirectory(args, state);
  const scopeLabel = optionalNonEmptyString(args.scope_label);
  const remote = optionalRefName(args.remote, 'remote');
  const branch = optionalRefName(args.branch, 'branch');
  const expectedCommit = normalizeExpectedCommit(args.expected_commit);
  const pushArgs = ['push'];
  if (remote || branch) {
    if (!remote || !branch) throw diagnosticError('git_push_remote_and_branch_required_together');
    pushArgs.push(remote, branch);
  }
  const before = await gitStatus({ working_directory: cwd }, state, context);
  const beforeBaseState = await readGitBaseState(cwd, state, context);
  if (expectedCommit && beforeBaseState.head !== expectedCommit) {
    throw diagnosticError('git_push_head_mismatch', 'git_push_head_mismatch', {
      expected_commit: expectedCommit,
      actual_head: beforeBaseState.head,
      mutation_started: false,
      atomic: true,
      remediation: 'Re-read the commit result and explicitly retry with the current intended commit.',
    });
  }
  const remotes = Array.isArray(before.remotes) ? before.remotes.map(asRemote) : [];
  const effectiveTarget = await resolvePushTarget(cwd, remote, branch, state.policy, context, remotes);
  if (effectiveTarget.status !== 'resolved') {
    throw diagnosticError('git_push_target_unresolved', `git_push_target_unresolved:${effectiveTarget.reason ?? 'unknown'}`, {
      working_directory: cwd,
      remote: remote ?? null,
      branch: branch ?? null,
      effective_target: effectiveTarget,
      remotes,
      remediation: pushRemediation(effectiveTarget, typeof before.branch === 'string' ? before.branch : null, remotes),
    });
  }
  const result = await runGit(cwd, pushArgs, state.policy, context);
  ensureGitOk(result, 'git_push_failed');
  const postStatus = await gitStatus({ working_directory: cwd }, state, context);
  const postBaseState = await readGitBaseState(cwd, state, context);
  const headVerified = !expectedCommit || postBaseState.head === expectedCommit;
  if (!headVerified) {
    throw diagnosticError('git_push_post_state_head_mismatch', 'git_push_post_state_head_mismatch', {
      expected_commit: expectedCommit,
      actual_head: postBaseState.head,
      mutation_started: true,
      atomic: false,
      remediation: 'The push completed but local HEAD changed during verification; inspect the remote target before retrying.',
    });
  }
  const payload = {
    schema: 'narada.git.push.v1',
    status: 'ok',
    working_directory: cwd,
    scope_label: scopeLabel,
    remote: remote ?? null,
    branch: branch ?? null,
    effective_remote: effectiveTarget.remote,
    effective_branch: effectiveTarget.branch,
    effective_target_status: effectiveTarget.status,
    expected_commit: expectedCommit,
    remote_target_verified: true,
    head_verified: headVerified,
    ...(effectiveTarget.reason ? { effective_target_reason: effectiveTarget.reason } : {}),
    output: combineOutput(result),
    pre_status: before,
    ...verifiedMutationPostState(postStatus, { operation: 'push', remote: effectiveTarget.remote, branch: effectiveTarget.branch, expected_commit: expectedCommit }),
    post_status: postStatus,
  };
  audit(state, payload);
  return payload;
}

export async function gitLog(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  const cwd = resolveWorkingDirectory(args, state);
  const limit = clampInteger(args.limit, 1, 100, 20);
  const pathspec = optionalPathspec(args.pathspec);
  const gitArgs = ['log', `-${limit}`, '--pretty=format:%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s'];
  if (pathspec) gitArgs.push('--', pathspec);
  const output = await gitText(cwd, gitArgs, state.policy, 'git_log_failed', context);
  const commits = output.split(/\r?\n/).filter(Boolean).map((line) => {
    const [hash, shortHash, authorName, authorEmail, authorDate, subject] = line.split('\x1f');
    return { hash, short_hash: shortHash, author_name: authorName, author_email: authorEmail, author_date: authorDate, subject };
  });
  return {
    schema: 'narada.git.log.v1',
    status: 'ok',
    working_directory: cwd,
    limit,
    pathspec,
    returned: commits.length,
    commits,
  };
}

export async function gitShow(args: Record<string, unknown>, state: GitMcpState, context: GitRequestContext = {}) {
  const cwd = resolveWorkingDirectory(args, state);
  const commit = requireCommitish(args.commit);
  const includePatch = args.include_patch !== false;
  const pathspec = optionalPathspec(args.pathspec);
  const metadata = await gitText(cwd, ['show', '--no-patch', '--format=%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s%x1f%b', commit], state.policy, 'git_show_failed', context);
  const [hash, shortHash, authorName, authorEmail, authorDate, subject, ...bodyParts] = metadata.split('\x1f');
  const patchArgs = ['show', '--format=', '--patch', '--no-ext-diff', commit];
  if (pathspec) patchArgs.push('--', pathspec);
  const patchResult = includePatch ? await runGit(cwd, patchArgs, state.policy, context) : null;
  if (patchResult) ensureGitOk(patchResult, 'git_show_failed');
  const patch = patchResult ? patchResult.output_text.trimStart() : '';
  return {
    schema: 'narada.git.show.v1',
    status: 'ok',
    working_directory: cwd,
    commit,
    hash,
    short_hash: shortHash,
    author_name: authorName,
    author_email: authorEmail,
    author_date: authorDate,
    subject,
    body: bodyParts.join('\x1f').trimEnd(),
    include_patch: includePatch,
    pathspec,
    patch,
    patch_preview: patch.slice(0, PREVIEW_CHAR_LIMIT),
    patch_omitted: false,
    patch_truncated: patchResult?.output_truncated ?? false,
    patch_char_length: patch.length,
  };
}

function resolveWorkingDirectory(args: Record<string, unknown>, state: GitMcpState): string {
  return resolvePolicyWorkingDirectory(args.working_directory, state.policy);
}

async function resolvePushTarget(cwd: string, remote: string | null, branch: string | null, policy: GitMcpPolicy, context: GitRequestContext = {}, remotes: Array<Record<string, unknown>> | null = null) {
  const knownRemotes = remotes ?? await listRemotes(cwd, policy, context);
  if (remote && branch) {
    const exists = knownRemotes.some((candidate) => candidate.name === remote);
    return exists
      ? { status: 'resolved', remote, branch, reason: null }
      : { status: 'unresolved', remote, branch, reason: 'remote_not_configured' };
  }
  const currentBranchResult = await runGit(cwd, ['branch', '--show-current'], policy, context);
  if (currentBranchResult.exit_code !== 0 || currentBranchResult.timed_out) {
    return { status: 'unresolved', remote: null, branch: null, reason: 'current_branch_unavailable' };
  }
  const currentBranch = currentBranchResult.output_text.trim() || null;
  if (!currentBranch) return { status: 'unresolved', remote: null, branch: null, reason: 'detached_head_or_unborn_branch' };
  const upstreamRemoteResult = await runGit(cwd, ['config', '--get', `branch.${currentBranch}.remote`], policy, context);
  const upstreamMergeResult = await runGit(cwd, ['config', '--get', `branch.${currentBranch}.merge`], policy, context);
  if (upstreamRemoteResult.exit_code !== 0 || upstreamMergeResult.exit_code !== 0) {
    return { status: 'unresolved', remote: null, branch: currentBranch, reason: 'upstream_not_configured' };
  }
  const upstreamRemote = upstreamRemoteResult.output_text.trim() || null;
  const upstreamMerge = upstreamMergeResult.output_text.trim() || null;
  const upstreamRemoteExists = upstreamRemote ? knownRemotes.some((candidate) => candidate.name === upstreamRemote) : false;
  if (upstreamRemote && !upstreamRemoteExists) {
    return { status: 'unresolved', remote: upstreamRemote, branch: upstreamMerge?.replace(/^refs\/heads\//, '') ?? currentBranch, reason: 'upstream_remote_not_configured' };
  }
  return {
    status: upstreamRemote && upstreamMerge ? 'resolved' : 'unresolved',
    remote: upstreamRemote,
    branch: upstreamMerge?.replace(/^refs\/heads\//, '') ?? currentBranch,
    reason: upstreamRemote && upstreamMerge ? null : 'upstream_not_configured',
  };
}

async function listRemotes(cwd: string, policy: GitMcpPolicy, context: GitRequestContext = {}) {
  const result = await runGit(cwd, ['remote', '-v'], policy, context);
  ensureGitOk(result, 'git_status_failed');
  const byKey = new Map<string, { name: string; fetch_url: string | null; push_url: string | null }>();
  for (const line of result.output_text.split(/\r?\n/).filter(Boolean)) {
    const match = line.match(/^(\S+)\s+(.+)\s+\((fetch|push)\)$/);
    if (!match) continue;
    const [, name, url, kind] = match;
    const existing = byKey.get(name) ?? { name, fetch_url: null, push_url: null };
    if (kind === 'fetch') existing.fetch_url = url;
    if (kind === 'push') existing.push_url = url;
    byKey.set(name, existing);
  }
  return [...byKey.values()];
}

async function untrackedDiff(cwd: string, pathspecs: string[], policy: GitMcpPolicy, context: GitRequestContext = {}) {
  const lsArgs = ['ls-files', '--others', '--exclude-standard', '-z'];
  if (pathspecs.length > 0) lsArgs.push('--', ...pathspecs);
  const listed = await runGit(cwd, lsArgs, policy, context);
  ensureGitOk(listed, 'git_diff_failed');
  const paths = listed.output_text.split('\0').filter(Boolean).slice(0, UNTRACKED_FILE_COUNT_LIMIT);
  const allPaths = listed.output_text.split('\0').filter(Boolean);
  let diff = '';
  let truncated = listed.output_truncated || allPaths.length > paths.length;
  for (const path of paths) {
    const result = await runGit(cwd, ['diff', '--no-ext-diff', '--no-index', '--', '/dev/null', path], policy, context);
    if (![0, 1].includes(result.exit_code ?? -1) || result.timed_out || result.cancelled) ensureGitOk(result, 'git_diff_failed');
    const next = result.output_text.replaceAll('/dev/null', 'a/dev/null');
    const remaining = UNTRACKED_DIFF_CHAR_LIMIT - diff.length;
    if (remaining <= 0) {
      truncated = true;
      break;
    }
    diff += next.length <= remaining ? next : next.slice(0, remaining);
    truncated ||= result.output_truncated || next.length > remaining;
  }
  return { diff, paths, pathsOmitted: Math.max(0, allPaths.length - paths.length), truncated };
}

function pushRemediation(target: Record<string, unknown>, branch: string | null, remotes: Array<Record<string, unknown>>) {
  const reason = String(target.reason ?? '');
  if (target.status === 'resolved') return null;
  if (reason === 'remote_not_configured') {
    return {
      kind: 'configure_remote_or_choose_existing_remote',
      message: `No remote named ${target.remote ?? ''} is configured.`,
      configured_remotes: remotes.map((remote) => remote.name),
      suggested_next_step: 'Add the intended remote, or pass one of the configured remote names.',
    };
  }
  if (reason === 'upstream_not_configured') {
    return {
      kind: 'set_upstream_or_push_explicit_target',
      message: 'Current branch has no upstream configured.',
      configured_remotes: remotes.map((remote) => remote.name),
      suggested_push_shape: branch && remotes.length > 0 ? `git_push(remote=${remotes[0].name}, branch=${branch})` : null,
    };
  }
  if (reason === 'upstream_remote_not_configured') {
    return {
      kind: 'repair_upstream_remote',
      message: `Configured upstream remote ${target.remote ?? ''} is missing from git remote list.`,
      configured_remotes: remotes.map((remote) => remote.name),
      suggested_next_step: 'Add the missing remote or update the branch upstream.',
    };
  }
  return {
    kind: 'inspect_repository_push_target',
    message: `Push target is unresolved: ${reason || 'unknown'}.`,
    configured_remotes: remotes.map((remote) => remote.name),
  };
}

function audit(state: GitMcpState, payload: unknown): void {
  if (!state.auditLogDir) return;
  mkdirSync(state.auditLogDir, { recursive: true });
  appendFileSync(resolve(state.auditLogDir, 'git-mcp.jsonl'), `${JSON.stringify(payload)}\n`, 'utf8');
}

function appendWorkflowLedger(state: GitMcpState, payload: unknown): string {
  const directory = state.auditLogDir ?? resolve(state.outputRoot, 'git-mcp-audit');
  mkdirSync(directory, { recursive: true });
  const path = resolve(directory, 'git-workflows.jsonl');
  appendFileSync(path, `${JSON.stringify(payload)}\n`, 'utf8');
  return path;
}

function optionalPathspec(value: unknown): string | null {
  if (value === undefined || value === null || value === '') return null;
  const pathspec = validateGitPathspec(value);
  if (/\s/.test(pathspec)) {
    throw diagnosticError('git_pathspec_may_be_multiple_paths', 'git_pathspec_may_be_multiple_paths', {
      pathspec,
      remediation: 'Pass multiple paths with the pathspecs array; pathspec is reserved for one unambiguous Git pathspec.',
    });
  }
  return pathspec;
}

function optionalPathspecs(pathspec: unknown, pathspecs: unknown): string[] {
  const values = stringArray(pathspecs).map((value) => optionalPathspec(value)).filter((value): value is string => Boolean(value));
  const single = optionalPathspec(pathspec);
  if (single && values.length > 0) throw diagnosticError('git_diff_pathspec_conflict', 'git_diff_pathspec_conflict', { remediation: 'Use either pathspec or pathspecs, not both.' });
  return single ? [single] : values;
}

function requiredNonEmptyString(value: unknown, code: string): string {
  const text = String(value ?? '').trim();
  if (!text) throw diagnosticError(code);
  return text;
}

function optionalNonEmptyString(value: unknown): string | null {
  const text = String(value ?? '').trim();
  return text ? text : null;
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.map(String) : [];
}

function optionalExpectedStagedPaths(value: unknown): string[] | null {
  if (value === undefined) return null;
  if (!Array.isArray(value) || value.length === 0 || value.some((path) => typeof path !== 'string' || !path.trim())) {
    throw diagnosticError('git_commit_expected_staged_paths_invalid', 'git_commit_expected_staged_paths_invalid', {
      remediation: 'Pass a non-empty array of explicit staged path strings, or omit expected_staged_paths.',
      mutation_started: false,
    });
  }
  return [...new Set(value.map(normalizeCommitScopePath))];
}

function normalizeCommitScopePath(value: unknown): string {
  return String(value ?? '').trim().replaceAll('\\', '/').replace(/^\.\//, '');
}

function arrayOfRecords(value: unknown): Array<Record<string, unknown>> {
  return Array.isArray(value) ? value.map(asRecord).filter((record) => Object.keys(record).length > 0) : [];
}

function stringEnum(value: unknown, allowed: string[], defaultValue: string): string {
  const text = String(value ?? defaultValue);
  if (!allowed.includes(text)) throw diagnosticError('git_invalid_enum', 'git_invalid_enum', { value: text, allowed });
  return text;
}

function clampInteger(value: unknown, min: number, max: number, defaultValue: number): number {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return defaultValue;
  return Math.max(min, Math.min(max, Math.trunc(parsed)));
}

function firstNonEmptyLine(value: string): string | null {
  return value.split(/\r?\n/).map((line) => line.trim()).find(Boolean) ?? null;
}

function groupUntrackedPaths(paths: string[], sampleLimit: number): Array<Record<string, unknown>> {
  const groups = new Map<string, string[]>();
  for (const path of paths) {
    const normalized = path.replaceAll('\\', '/');
    const top = normalized.includes('/') ? normalized.split('/')[0] : '(root)';
    const list = groups.get(top) ?? [];
    list.push(path);
    groups.set(top, list);
  }
  let remainingSamples = sampleLimit;
  return [...groups.entries()].sort((a, b) => b[1].length - a[1].length || a[0].localeCompare(b[0])).map(([top_level, groupPaths]) => {
    const sample = remainingSamples > 0 ? groupPaths.slice(0, remainingSamples) : [];
    remainingSamples = Math.max(0, remainingSamples - sample.length);
    return { top_level, count: groupPaths.length, sample, sample_omitted: Math.max(0, groupPaths.length - sample.length), advisory_classification: summarizeAdvisoryClassifications(groupPaths.map(classifyAdvisoryPath)) };
  });
}

function classifyAdvisoryPath(path: string): Record<string, unknown> {
  const normalized = path.replaceAll('\\', '/').replace(/^\.\//, '');
  const lower = normalized.toLowerCase();
  const rules: Array<[string, RegExp, string]> = [
    ['runtime_artifact', /(^|\/)\.ai\/(runtime|tmp|output)\/|(^|\.)tmp\/|(^|\/)runtime\//, 'runtime or temporary artifact path'],
    ['dependency_artifact', /(^|\/)node_modules\//, 'dependency install artifact'],
    ['build_artifact', /(^|\/)(dist|build|coverage|\.wrangler)\//, 'build or coverage artifact'],
    ['probe_artifact', /(^|\/)(probe|smoke|tmp|temp|scratch|debug)[-_./]|[-_.](probe|smoke|tmp|temp|scratch|debug)\./, 'probe/debug/generated filename pattern'],
    ['log_artifact', /\.(log|trace|jsonl)$/i, 'log or event stream file'],
  ];
  const matched = rules.find(([, pattern]) => pattern.test(lower));
  return {
    path,
    classification: matched?.[0] ?? 'unknown',
    confidence: matched ? 'medium' : 'unknown',
    reason: matched?.[2] ?? 'no bounded advisory rule matched',
    advisory_only: true,
  };
}

function summarizeAdvisoryClassifications(classifications: Record<string, unknown>[]): Record<string, unknown> {
  const byClassification: Record<string, number> = {};
  for (const item of classifications) {
    const key = typeof item.classification === 'string' ? item.classification : 'unknown';
    byClassification[key] = (byClassification[key] ?? 0) + 1;
  }
  return { advisory_only: true, classified_count: classifications.filter((item) => item.classification !== 'unknown').length, by_classification: byClassification };
}

function classifyTaskScopedDirtyPaths(entries: Record<string, unknown>[], filters: string[]): Record<string, unknown> {
  const dirtyPaths = entries.map((entry) => String(entry.display_path ?? entry.path ?? '')).filter(Boolean);
  if (filters.length === 0) {
    return {
      status: 'no_expected_pathset',
      filters,
      relevant: [],
      unrelated: [],
      unknown: dirtyPaths,
      relevant_count: 0,
      unrelated_count: 0,
      unknown_count: dirtyPaths.length,
    };
  }
  const relevant = [];
  const unrelated = [];
  for (const path of dirtyPaths) {
    if (filters.some((filter) => pathMatchesFilter(path, filter))) relevant.push(path);
    else unrelated.push(path);
  }
  return {
    status: unrelated.length > 0 ? 'has_unrelated_dirty_paths' : 'all_dirty_paths_match_expected_pathset',
    filters,
    relevant,
    unrelated,
    unknown: [],
    relevant_count: relevant.length,
    unrelated_count: unrelated.length,
    unknown_count: 0,
  };
}

function pathMatchesFilter(path: string, filter: string): boolean {
  const normalizedPath = path.replaceAll('\\', '/');
  const normalizedFilter = String(filter ?? '').trim().replaceAll('\\', '/').replace(/^\.\//, '');
  if (!normalizedFilter) return false;
  if (normalizedPath === normalizedFilter) return true;
  if (normalizedPath.startsWith(`${normalizedFilter.replace(/\/$/, '')}/`)) return true;
  if (normalizedFilter.endsWith('/**')) return normalizedPath.startsWith(normalizedFilter.slice(0, -3));
  return false;
}

function asStatusEntry(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function asRemote(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
}