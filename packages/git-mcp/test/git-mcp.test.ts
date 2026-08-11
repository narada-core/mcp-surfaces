import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  createServerState,
  gitAdd,
  gitBranchCreate,
  gitBranchDelete,
  gitBranchDeleteRemote,
  gitBranchList,
  gitBranchRename,
  gitBranchSetUpstream,
  gitBranchSwitch,
  gitBranchUnsetUpstream,
  gitBeginWorkScope,
  gitChangedSummary,
  gitCommit,
  gitCommitPaths,
  gitDiff,
  gitFetch,
  gitLog,
  gitMerge,
  gitMergeAbort,
  gitMergeContinue,
  gitPush,
  gitRebase,
  gitRebaseAbort,
  gitRebaseContinue,
  gitRepositoriesSummary,
  gitShow,
  gitStatus,
  gitSyncStatus,
  gitUnstage,
  gitWorkflowRecord,
  handleRequest,
} from '../src/main.js';
import { runGit } from '../src/git-runner.js';
import { resolveWorkingDirectory } from '../src/policy.js';
import { buildGuidanceResult } from '../src/guidance.js';

type RpcResponse = {
  result?: Record<string, any>;
  error?: Record<string, any>;
};

const root = mkdtempSync(join(tmpdir(), 'git-mcp-'));
const repo = join(root, 'repo');
const remote = join(root, 'remote.git');

git(root, ['init', '--bare', '--initial-branch=main', remote]);
git(root, ['init', '--initial-branch=main', repo]);
git(repo, ['config', 'user.email', 'agent@example.test']);
git(repo, ['config', 'user.name', 'Agent Test']);
git(repo, ['config', 'core.autocrlf', 'false']);
git(repo, ['remote', 'add', 'origin', remote]);
const siteRoot = join(root, 'site-root');
mkdirSync(join(siteRoot, '.narada'), { recursive: true });
writeFileSync(join(siteRoot, '.narada', 'secrets.json'), JSON.stringify({ env: { GIT_MCP_TEST_SECRET: 'from-site-secret' } }), 'utf8');
const originalGitSecret = process.env.GIT_MCP_TEST_SECRET;
delete process.env.GIT_MCP_TEST_SECRET;

const state = createServerState({ allowedRoot: root, outputRoot: root, mode: 'write', maxOutputBytes: 2 * 1024 * 1024 });
const readState = createServerState({ allowedRoot: root, outputRoot: root, mode: 'read' });
const secretState = createServerState({ allowedRoot: siteRoot, mode: 'read' });
assert.equal(secretState.env.GIT_MCP_TEST_SECRET, 'from-site-secret');
assert.equal(secretState.policy.allowedRoots.includes(siteRoot), true);
assert.equal(process.env.GIT_MCP_TEST_SECRET, undefined);
if (originalGitSecret === undefined) delete process.env.GIT_MCP_TEST_SECRET;
else process.env.GIT_MCP_TEST_SECRET = originalGitSecret;
const rpc = handleRequest as unknown as (request: Record<string, unknown>, requestState: ReturnType<typeof createServerState>) => Promise<RpcResponse>;

const abortController = new AbortController();
abortController.abort();
const cancelledGit = await runGit(repo, ['status'], state.policy, { abortSignal: abortController.signal });
assert.equal(cancelledGit.cancelled, true);
assert.equal(cancelledGit.timed_out, false);
assert.equal(cancelledGit.exit_code, null);

const secretGitEnv = await runGit(repo, ['var', 'GIT_AUTHOR_IDENT'], secretState.policy, { env: secretState.env });
assert.equal(secretGitEnv.exit_code, 0);

const unbornRepo = join(root, 'unborn');
git(root, ['init', '--initial-branch=main', unbornRepo]);
const unbornStatus = await gitStatus({ working_directory: unbornRepo }, state);
assert.equal(unbornStatus.branch, 'main');
assert.equal(unbornStatus.unborn, true);

const tools = await rpc({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} }, state);
const toolNames = tools.result?.tools.map((tool: any) => tool.name).sort();
assert.deepEqual(toolNames.filter((tool: any) => tool.startsWith('git_')), [
  'git_add',
  'git_begin_work_scope',
  'git_branch_create',
  'git_branch_delete',
  'git_branch_delete_remote',
  'git_branch_list',
  'git_branch_rename',
  'git_branch_set_upstream',
  'git_branch_switch',
  'git_branch_unset_upstream',
  'git_changed_summary',
  'git_commit',
  'git_commit_paths',
  'git_diff',
  'git_fetch',
  'git_guidance',
  'git_log',
  'git_merge',
  'git_merge_abort',
  'git_merge_continue',
  'git_output_show',
  'git_policy_inspect',
  'git_push',
  'git_rebase',
  'git_rebase_abort',
  'git_rebase_continue',
  'git_repositories_summary',
  'git_show',
  'git_status',
  'git_sync_status',
  'git_unstage',
  'git_workflow_record',
]);
const gitStatusTool = tools.result?.tools.find((tool: any) => tool.name === 'git_status');
assert.match(gitStatusTool.inputSchema.properties.working_directory.description, /explicit relative/);
const policyReadback = await rpc({ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'git_policy_inspect', arguments: {} } }, state);
assert.equal(policyReadback.result?.structuredContent.relative_path_resolution.omitted_working_directory, 'Use the first allowed root.');
const guidanceReadback = buildGuidanceResult({});
assert.equal((guidanceReadback.path_resolution as any).pathspecs.includes('repository-relative'), true);
assert.equal(resolveWorkingDirectory(undefined, state.policy), root);
const currentDirectoryPolicy = { ...state.policy, allowedRoots: [process.cwd()] };
assert.equal(resolveWorkingDirectory('.', currentDirectoryPolicy), process.cwd());
assert.throws(
  () => resolveWorkingDirectory('..', currentDirectoryPolicy),
  (error: any) => error?.details?.resolution_rule === 'process_current_directory'
    && error?.details?.resolution_base === process.cwd()
    && error?.details?.requested_working_directory === '..',
);

const readTools = await rpc({ jsonrpc: '2.0', id: 21, method: 'tools/list', params: {} }, readState);
const readToolNames = readTools.result?.tools.map((tool: any) => tool.name).sort();
assert.equal(readToolNames.includes('git_status'), true);
assert.equal(readToolNames.includes('git_add'), true);
const readAddTool = readTools.result?.tools.find((tool: any) => tool.name === 'git_add');
assert.match(readAddTool.description, /mode=write/);
assert.match(readAddTool.description, /ignored/);

const guidance = await rpc({
  jsonrpc: '2.0',
  id: 20,
  method: 'tools/call',
  params: { name: 'git_guidance', arguments: {} },
}, state);
let guidanceContent = guidance.result?.structuredContent as Record<string, any>;
if (guidanceContent.schema === 'narada.producer_output_page.v1') {
  let guidanceText = '';
  let guidanceOffset = 0;
  while (true) {
    const shownGuidance = await rpc({
      jsonrpc: '2.0',
      id: 26 + guidanceOffset,
      method: 'tools/call',
      params: { name: 'git_output_show', arguments: { ref: guidanceContent.output_ref, offset: guidanceOffset, limit: 4000 } },
    }, state);
    assert.equal(shownGuidance.error, undefined, JSON.stringify(shownGuidance));
    const page = shownGuidance.result?.structuredContent as Record<string, any>;
    assert.equal(page.schema, 'narada.mcp_output_page.v1', JSON.stringify(page));
    guidanceText += String(page.output_text ?? '');
    if (page.next_offset === null || page.next_offset === undefined) break;
    guidanceOffset = Number(page.next_offset);
  }
  guidanceContent = JSON.parse(guidanceText);
}
assert.equal(guidanceContent.surface_id, 'git');
assert.ok((guidanceContent.workflows.normal_publication as string[]).some((step: any) => step.includes('git_workflow_record')));
assert.ok((guidanceContent.workflows.normal_publication as string[]).some((step: any) => step.includes('any unstaged, untracked, or conflict paths')));
assert.deepEqual(guidanceContent.tool_inventory.write, [
  'git_add',
  'git_unstage',
  'git_commit_paths',
  'git_commit',
  'git_push',
  'git_fetch',
  'git_rebase',
  'git_rebase_continue',
  'git_rebase_abort',
  'git_merge',
  'git_merge_continue',
  'git_merge_abort',
  'git_branch_create',
  'git_branch_switch',
  'git_branch_rename',
  'git_branch_delete',
  'git_branch_delete_remote',
  'git_branch_set_upstream',
  'git_branch_unset_upstream',
  'git_workflow_record',
]);

const policy = await rpc({
  jsonrpc: '2.0',
  id: 22,
  method: 'tools/call',
  params: { name: 'git_policy_inspect', arguments: {} },
}, state);
assert.equal(policy.result?.structuredContent.mode, 'write');
assert.equal(policy.result?.structuredContent.max_output_bytes, 2 * 1024 * 1024);
assert.equal(policy.result?.structuredContent.branch_policy, 'merged_only_no_force');
const policyDocument = JSON.parse(policy.result?.content[0].text ?? '{}') as {
  schema?: string;
  mode?: string;
};
assert.equal(policyDocument.schema, 'narada.git.policy.v1');
assert.equal(policyDocument.mode, 'write');

let status = await gitStatus({ working_directory: repo }, state);
assert.equal(status.clean, true);
assert.equal(String(status.repository_root).replaceAll('\\', '/').endsWith('/repo'), true);
assert.deepEqual(status.remote_names, ['origin']);
assert.deepEqual((status.remotes as any[]).map((candidate: any) => ({ name: candidate.name, fetch_url: candidate.fetch_url, push_url: candidate.push_url })), [
  { name: 'origin', fetch_url: remote, push_url: remote },
]);
assert.equal((status.push_target as any).status, 'unresolved');
assert.equal((status.push_target as any).reason, 'upstream_not_configured');
assert.equal((status.push_remediation as any).kind, 'set_upstream_or_push_explicit_target');

const noRemoteRepo = join(root, 'no-remote');
git(root, ['init', '--initial-branch=main', noRemoteRepo]);
git(noRemoteRepo, ['config', 'user.email', 'agent@example.test']);
git(noRemoteRepo, ['config', 'user.name', 'Agent Test']);
writeFileSync(join(noRemoteRepo, 'README.md'), 'local only\n', 'utf8');
git(noRemoteRepo, ['add', 'README.md']);
git(noRemoteRepo, ['commit', '-m', 'Initial local commit']);
const noRemoteStatus = await gitStatus({ working_directory: noRemoteRepo }, state);
assert.deepEqual(noRemoteStatus.remote_names, []);
assert.equal((noRemoteStatus.push_target as any).reason, 'upstream_not_configured');
const missingRemotePush = await rpc({
  jsonrpc: '2.0',
  id: 25,
  method: 'tools/call',
  params: { name: 'git_push', arguments: { working_directory: noRemoteRepo, remote: 'origin', branch: 'main' } },
}, state);
assert.equal(missingRemotePush.error?.data.code, 'git_push_target_unresolved');
assert.equal(missingRemotePush.error?.data.details.effective_target.reason, 'remote_not_configured');
assert.match(missingRemotePush.error?.data.details.remediation.message, /No remote named origin/);

writeFileSync(join(repo, 'README.md'), 'hello\n', 'utf8');
mkdirSync(join(repo, 'runtime', 'tmp'), { recursive: true });
mkdirSync(join(repo, 'notes'), { recursive: true });
writeFileSync(join(repo, 'runtime', 'tmp', 'artifact.log'), 'runtime artifact\n', 'utf8');
writeFileSync(join(repo, 'notes', 'task.md'), 'task note\n', 'utf8');
status = await gitStatus({ working_directory: repo }, state);
assert.deepEqual(status.untracked, ['README.md', 'notes/task.md', 'runtime/tmp/artifact.log']);
const changedSummary = await gitChangedSummary({ working_directory: repo, expected_paths: ['README.md', 'notes'], untracked_sample_limit: 2 }, state);
assert.equal(changedSummary.schema, 'narada.git.changed_summary.v1');
assert.equal(changedSummary.tracked_changed_count, 0);
assert.equal(changedSummary.untracked_count, 3);
assert.equal((changedSummary.advisory_classification as any).advisory_only, true);
assert.equal((changedSummary.advisory_classification as any).by_classification.runtime_artifact, 1);
assert.equal((changedSummary.untracked_classifications as any[]).find((item: any) => item.path === 'runtime/tmp/artifact.log')?.classification, 'runtime_artifact');
assert.deepEqual((changedSummary.untracked_groups as any[]).map((group: any) => ({ top_level: group.top_level, count: group.count })), [
  { top_level: '(root)', count: 1 },
  { top_level: 'notes', count: 1 },
  { top_level: 'runtime', count: 1 },
]);
assert.equal((changedSummary.untracked_groups as any[]).find((group: any) => group.top_level === 'runtime')?.advisory_classification.by_classification.runtime_artifact, 1);
assert.deepEqual(changedSummary.relevant_changed_paths, ['README.md', 'notes/task.md']);
assert.deepEqual(changedSummary.task_relevant_dirty_paths, ['README.md', 'notes/task.md']);
assert.deepEqual(changedSummary.task_unrelated_dirty_paths, ['runtime/tmp/artifact.log']);
assert.equal((changedSummary.task_scoped_dirty_classification as any).status, 'has_unrelated_dirty_paths');
const untrackedDiff = await gitDiff({ working_directory: repo, scope: 'working', pathspec: 'README.md', include_untracked: true }, state);
assert.deepEqual(untrackedDiff.untracked_paths, ['README.md']);
assert.match(untrackedDiff.diff, /\+hello/);

const addResult = await gitAdd({ working_directory: repo, paths: ['README.md'] }, state);
assert.deepEqual((addResult.post_status as any).staged, ['README.md']);
assert.equal((addResult.verified_post_state as any).verification, 'verified');
assert.equal(addResult.verification_status, 'verified');
assert.deepEqual((addResult.mutation_effect as any).operation, 'add');
const postAddSummary = await gitChangedSummary({ working_directory: repo, pathspec: 'README.md' }, state);
assert.deepEqual(postAddSummary.tracked_changed_paths, ['README.md']);
assert.deepEqual(postAddSummary.relevant_changed_paths, ['README.md']);
assert.equal(postAddSummary.path_scope_applied, true);
assert.deepEqual(postAddSummary.path_scope_filters, ['README.md']);
assert.equal(postAddSummary.untracked_count, 0);
assert.equal(postAddSummary.whole_repository_untracked_count, 2);

const stagedDiff = await gitDiff({ working_directory: repo, scope: 'staged' }, state);
assert.match(stagedDiff.diff, /README\.md/);
assert.match(stagedDiff.diff, /\+hello/);

const unstageResponse = await rpc({
  jsonrpc: '2.0',
  id: 27,
  method: 'tools/call',
  params: { name: 'git_unstage', arguments: { working_directory: repo, paths: ['README.md'] } },
}, state);
assert.equal(unstageResponse.error, undefined);
assert.equal(unstageResponse.result?.structuredContent.schema, 'narada.git.unstage.v1');
assert.equal(unstageResponse.result?.structuredContent.verified_post_state.verification, 'verified');
assert.deepEqual((unstageResponse.result?.structuredContent.post_status as any).staged, []);
assert.deepEqual((unstageResponse.result?.structuredContent.post_status as any).unstaged, []);
await gitAdd({ working_directory: repo, paths: ['README.md'] }, state);

await assert.rejects(
  () => gitCommit({ working_directory: repo, message: 'Reject mixed worktree scope' }, state),
  (error: any) => {
    assert.equal(error.codeName, 'git_commit_scope_required_for_mixed_worktree');
    assert.deepEqual(error.details.staged_paths, ['README.md']);
    assert.ok(error.details.untracked_paths.includes('notes/task.md'));
    assert.ok(error.details.untracked_paths.includes('runtime/tmp/artifact.log'));
    assert.equal(error.details.mutation_started, false);
    assert.equal(error.details.atomic, true);
    assert.match(error.details.remediation, /expected_staged_paths/);
    return true;
  },
);

const commitResult = await gitCommit({ working_directory: repo, message: 'Initial commit', expected_staged_paths: ['README.md'] }, state);
assert.match(commitResult.commit, /^[0-9a-f]{40}$/);
assert.equal((commitResult.verified_post_state as any).verification, 'verified');
assert.equal(commitResult.verification_status, 'verified');
assert.equal((commitResult.mutation_effect as any).operation, 'commit');

const isolatedRepo = join(root, 'isolated-index-repo');
git(root, ['init', '--initial-branch=main', isolatedRepo]);
git(isolatedRepo, ['config', 'user.email', 'agent@example.test']);
git(isolatedRepo, ['config', 'user.name', 'Agent Test']);
writeFileSync(join(isolatedRepo, 'alpha.txt'), 'alpha base\n', 'utf8');
writeFileSync(join(isolatedRepo, 'beta.txt'), 'beta base\n', 'utf8');
git(isolatedRepo, ['add', '.']);
git(isolatedRepo, ['commit', '-m', 'isolated base']);
writeFileSync(join(isolatedRepo, 'alpha.txt'), 'alpha agent\n', 'utf8');
writeFileSync(join(isolatedRepo, 'beta.txt'), 'beta other agent\n', 'utf8');
git(isolatedRepo, ['add', 'beta.txt']);
const isolatedCommit = await gitCommitPaths({
  working_directory: isolatedRepo,
  paths: ['alpha.txt'],
  message: 'Commit alpha through isolated index',
  scope_label: 'agent-alpha',
}, state);
assert.equal(isolatedCommit.isolation, 'dedicated_temporary_index');
assert.deepEqual(isolatedCommit.committed_files, ['alpha.txt']);
assert.deepEqual((isolatedCommit.post_status as any).staged, ['beta.txt']);
assert.equal(git(isolatedRepo, ['show', '--format=', '--name-only', 'HEAD']).trim(), 'alpha.txt');
assert.equal(git(isolatedRepo, ['show', 'HEAD:alpha.txt']).trim(), 'alpha agent');
assert.equal(git(isolatedRepo, ['show', 'HEAD:beta.txt']).trim(), 'beta base');

const scopedRepo = join(root, 'scoped-repo');
git(root, ['init', '--initial-branch=main', scopedRepo]);
git(scopedRepo, ['config', 'user.email', 'agent@example.test']);
git(scopedRepo, ['config', 'user.name', 'Agent Test']);
writeFileSync(join(scopedRepo, 'alpha.txt'), 'alpha\n', 'utf8');
writeFileSync(join(scopedRepo, 'beta.txt'), 'beta\n', 'utf8');
git(scopedRepo, ['add', '.']);
git(scopedRepo, ['commit', '-m', 'Scoped base']);
writeFileSync(join(scopedRepo, 'alpha.txt'), 'alpha changed\n', 'utf8');
writeFileSync(join(scopedRepo, 'beta.txt'), 'beta changed\n', 'utf8');
const workScope = await gitBeginWorkScope({ working_directory: scopedRepo, allowed_paths: ['alpha.txt'] }, state);
assert.match(workScope.work_scope_ref, /^gws_/);
assert.deepEqual(workScope.allowed_paths, ['alpha.txt']);
const scopedStatus = await gitStatus({ working_directory: scopedRepo, work_scope_ref: workScope.work_scope_ref, format: 'summary' }, state);
assert.equal((scopedStatus.summary as any).matching_path_count, 1);
assert.deepEqual(scopedStatus.paths, ['alpha.txt']);
const scopedAdd = await gitAdd({ working_directory: scopedRepo, paths: ['alpha.txt'], work_scope_ref: workScope.work_scope_ref }, state);
assert.ok(scopedAdd.index_scope_ref);
  assert.match(scopedAdd.index_scope_ref, /^gis_/);
assert.deepEqual(scopedAdd.paths, ['alpha.txt']);
assert.equal((scopedAdd.verified_post_state as any).verification, 'verified');
git(scopedRepo, ['add', 'beta.txt']);
await assert.rejects(
  () => gitCommit({ working_directory: scopedRepo, message: 'Reject out-of-scope index', work_scope_ref: workScope.work_scope_ref, index_scope_ref: scopedAdd.index_scope_ref }, state),
  (error: any) => error.codeName === 'git_index_scope_state_drift' && error.details.mutation_started === false && error.details.atomic === true,
);
await gitUnstage({ working_directory: scopedRepo, paths: ['beta.txt'] }, state);
const scopedCommit = await gitCommit({ working_directory: scopedRepo, message: 'Scoped commit', work_scope_ref: workScope.work_scope_ref, index_scope_ref: scopedAdd.index_scope_ref }, state);
assert.equal(scopedCommit.committed_files.includes('alpha.txt'), true);
assert.match(scopedCommit.commit_ref, /^git_commit:[0-9a-f]{40}$/);
mkdirSync(join(scopedRepo, 'nested'), { recursive: true });
writeFileSync(join(scopedRepo, 'nested', 'file.txt'), 'nested\n', 'utf8');
const directoryAdd = await gitAdd({ working_directory: scopedRepo, paths: ['nested'] }, state);
assert.deepEqual(directoryAdd.paths, ['nested/file.txt']);
assert.deepEqual((directoryAdd.post_status as any).staged, ['nested/file.txt']);
await gitUnstage({ working_directory: scopedRepo, paths: ['nested/file.txt'] }, state);

writeFileSync(join(repo, 'scope-a.txt'), 'a\n', 'utf8');
writeFileSync(join(repo, 'scope-b.txt'), 'b\n', 'utf8');
await gitAdd({ working_directory: repo, paths: ['scope-a.txt', 'scope-b.txt'] }, state);
await assert.rejects(
  () => gitCommit({ working_directory: repo, message: 'Reject unrelated staged path', scope_label: 'scope-a-only', expected_staged_paths: ['scope-a.txt'] }, state),
  (error: any) => {
    assert.equal(error.codeName, 'git_commit_staged_scope_mismatch');
    assert.deepEqual(error.details.expected_staged_paths, ['scope-a.txt']);
    assert.deepEqual(error.details.actual_staged_paths, ['scope-a.txt', 'scope-b.txt']);
    assert.deepEqual(error.details.missing_paths, []);
    assert.deepEqual(error.details.unexpected_paths, ['scope-b.txt']);
    assert.equal(error.details.mutation_started, false);
    assert.equal(error.details.atomic, true);
    return true;
  },
);
assert.deepEqual((await gitStatus({ working_directory: repo }, state)).staged, ['scope-a.txt', 'scope-b.txt']);
await gitUnstage({ working_directory: repo, paths: ['scope-a.txt', 'scope-b.txt'] }, state);

writeFileSync(join(repo, '.gitignore'), 'ignored-staging.txt\n', 'utf8');
writeFileSync(join(repo, 'staging-safe.txt'), 'safe\n', 'utf8');
writeFileSync(join(repo, 'ignored-staging.txt'), 'ignored\n', 'utf8');
await assert.rejects(
  () => gitAdd({ working_directory: repo, paths: ['staging-safe.txt', 'ignored-staging.txt'] }, state),
  (error: any) => {
    assert.equal(error.codeName, 'git_add_ignored_paths');
    assert.deepEqual(error.details.requested_paths, ['staging-safe.txt', 'ignored-staging.txt']);
    assert.deepEqual(error.details.ignored_paths, ['ignored-staging.txt']);
    assert.equal(error.details.mutation_started, false);
    assert.equal(error.details.atomic, true);
    assert.match(error.details.remediation, /Remove ignored paths/);
    assert.match(error.details.ignored_entries[0].diagnostic_text, /ignored-staging\.txt/);
    return true;
  },
);
const afterRejectedBatchAdd = await gitStatus({ working_directory: repo }, state);
assert.deepEqual(afterRejectedBatchAdd.staged, []);
const afterRejectedUntracked = afterRejectedBatchAdd.untracked as string[];
assert.equal(afterRejectedUntracked.includes('staging-safe.txt'), true);
assert.equal(afterRejectedUntracked.includes('ignored-staging.txt'), false);

const logResult = await gitLog({ working_directory: repo, limit: 5 }, state);
assert.equal(logResult.returned, 1);
assert.equal(logResult.commits[0].subject, 'Initial commit');

const showResult = await gitShow({ working_directory: repo, commit: 'HEAD', include_patch: true }, state);
assert.equal(showResult.subject, 'Initial commit');
assert.match(showResult.patch, /README\.md/);

const failedShow = await rpc({
  jsonrpc: '2.0',
  id: 23,
  method: 'tools/call',
  params: { name: 'git_show', arguments: { working_directory: repo, commit: 'missing-ref' } },
}, state);
assert.equal(failedShow.error?.data.code, 'git_show_failed');
assert.equal(typeof failedShow.error?.data.details.exit_code, 'number');
assert.equal(typeof failedShow.error?.data.details.diagnostic_text, 'string');

const unknownTool = await rpc({
  jsonrpc: '2.0',
  id: 24,
  method: 'tools/call',
  params: { name: 'git_unknown', arguments: {} },
}, state);
assert.equal(unknownTool.error?.data.code, 'git_mcp_unknown_tool');

writeFileSync(join(repo, 'README.md'), 'hello\nworld\n', 'utf8');
const workingDiff = await gitDiff({ working_directory: repo, scope: 'working', pathspec: 'README.md' }, state);
assert.match(workingDiff.diff, /\+world/);
assert.equal(workingDiff.offset, 0);
assert.equal(workingDiff.next_offset, null);
const multiPathDiff = await gitDiff({ working_directory: repo, scope: 'working', pathspecs: ['README.md', 'notes/task.md'], include_untracked: true }, state);
assert.deepEqual(multiPathDiff.pathspecs, ['README.md', 'notes/task.md']);
assert.match(multiPathDiff.diff, /README\.md/);
assert.deepEqual(multiPathDiff.untracked_paths, ['notes/task.md']);
await assert.rejects(
  () => gitDiff({ working_directory: repo, scope: 'working', pathspec: 'README.md notes/task.md' }, state),
  /git_pathspec_may_be_multiple_paths/,
);

await assert.rejects(
  () => gitAdd({ working_directory: repo, paths: ['.'] }, state),
  /git_broad_path_not_allowed/,
);

await assert.rejects(
  () => gitShow({ working_directory: repo, commit: '--all' }, state),
  /git_leading_dash_commitish_not_allowed/,
);

await gitAdd({ working_directory: repo, paths: ['README.md'] }, state);
await gitCommit({ working_directory: repo, message: 'Update readme', expected_staged_paths: ['README.md'] }, state);

git(repo, ['mv', 'README.md', 'RENAMED.md']);
const renameStatus = await gitStatus({ working_directory: repo }, state);
assert.deepEqual(renameStatus.staged, ['README.md <- RENAMED.md']);
assert.deepEqual((renameStatus.status_entries as any[]).filter((entry: any) => !entry.untracked).map((entry: any) => ({
  x: entry.x,
  y: entry.y,
  path: entry.path,
  original_path: entry.original_path,
})), [{ x: 'R', y: ' ', path: 'RENAMED.md', original_path: 'README.md' }]);
const renameCommit = await gitCommit({ working_directory: repo, message: 'Rename readme', expected_staged_paths: ['README.md <- RENAMED.md'] }, state);
assert.deepEqual(renameCommit.committed_files, ['README.md <- RENAMED.md']);
assert.deepEqual((renameCommit.committed_entries as any[]).map((entry: any) => ({
  x: entry.x,
  y: entry.y,
  path: entry.path,
  original_path: entry.original_path,
})), [{ x: 'R', y: ' ', path: 'RENAMED.md', original_path: 'README.md' }]);
git(repo, ['commit', '--allow-empty', '-m', 'éééé']);

const byteLimitedState = createServerState({ allowedRoot: root, outputRoot: root, maxOutputBytes: 3 });
const byteLimited = await runGit(repo, ['log', '-1', '--format=%s'], byteLimitedState.policy);
assert.equal(byteLimited.output_truncated, true);
assert.equal(Buffer.byteLength(byteLimited.output_text, 'utf8') <= 3, true);

const bigFile = join(repo, 'big.txt');
writeFileSync(bigFile, 'small\n', 'utf8');
git(repo, ['add', 'big.txt']);
git(repo, ['commit', '-m', 'Add big file base']);
writeFileSync(bigFile, `${'x\n'.repeat(2_300_000)}`, 'utf8');
const bigDiff = await gitDiff({ working_directory: repo, scope: 'working', pathspec: 'big.txt' }, state);
assert.equal(bigDiff.diff_truncated, true);
git(repo, ['restore', '--', 'big.txt']);

const pushResult = await gitPush({ working_directory: repo, remote: 'origin', branch: currentBranch(repo) }, state);
assert.match(pushResult.output, /(new branch|main -> main|master -> master)/);
assert.equal(pushResult.verification_status, 'verified');
assert.equal((pushResult.mutation_effect as any).operation, 'push');

const initialBranchList = await gitBranchList({ working_directory: repo, scope: 'local' }, state);
assert.equal(initialBranchList.schema, 'narada.git.branch_list.v1');
const baseBranch = currentBranch(repo);
assert.equal(initialBranchList.current_branch, baseBranch);
assert.equal((initialBranchList.branches as any[]).some((branch: any) => branch.name === baseBranch && branch.type === 'local' && branch.current === true), true);

const createdBranch = await gitBranchCreate({ working_directory: repo, name: 'feature/mcp' }, state);
assert.equal(createdBranch.checked_out, false);
assert.equal((createdBranch.verified_post_state as any).verification, 'verified');
assert.equal((createdBranch.post_status as any).branch, baseBranch);
const switchedBranch = await gitBranchSwitch({ working_directory: repo, branch: 'feature/mcp' }, state);
assert.equal((switchedBranch.verified_post_state as any).verification, 'verified');
assert.equal((switchedBranch.post_status as any).branch, 'feature/mcp');
const renamedBranch = await gitBranchRename({ working_directory: repo, old_name: 'feature/mcp', new_name: 'feature/renamed' }, state);
assert.equal((renamedBranch.verified_post_state as any).verification, 'verified');
assert.equal((renamedBranch.post_status as any).branch, 'feature/renamed');
await gitBranchSwitch({ working_directory: repo, branch: baseBranch }, state);
const deletedMergedBranch = await gitBranchDelete({ working_directory: repo, branch: 'feature/renamed', base: baseBranch }, state);
assert.equal(deletedMergedBranch.merge_check, 'passed');

await gitBranchCreate({ working_directory: repo, name: 'feature/unmerged', start_point: baseBranch }, state);
await gitBranchSwitch({ working_directory: repo, branch: 'feature/unmerged' }, state);
writeFileSync(join(repo, 'unmerged.txt'), 'unmerged branch\n', 'utf8');
git(repo, ['add', 'unmerged.txt']);
git(repo, ['commit', '-m', 'Unmerged branch test']);
await gitBranchSwitch({ working_directory: repo, branch: baseBranch }, state);
await assert.rejects(
  () => gitBranchDelete({ working_directory: repo, branch: 'feature/unmerged', base: baseBranch }, state),
  (error: any) => error.codeName === 'git_branch_not_merged',
);
git(repo, ['branch', '-D', 'feature/unmerged']);

await gitBranchCreate({ working_directory: repo, name: 'remote/merged', start_point: baseBranch }, state);
await gitPush({ working_directory: repo, remote: 'origin', branch: 'remote/merged' }, state);
const setUpstream = await gitBranchSetUpstream({ working_directory: repo, local_branch: baseBranch, remote: 'origin', remote_branch: baseBranch }, state);
assert.equal((setUpstream.post_status as any).upstream, `origin/${baseBranch}`);
const unsetUpstream = await gitBranchUnsetUpstream({ working_directory: repo, local_branch: baseBranch }, state);
assert.equal((unsetUpstream.post_status as any).upstream, null);
const deletedRemoteBranch = await gitBranchDeleteRemote({ working_directory: repo, remote: 'origin', branch: 'remote/merged', base: baseBranch }, state);
assert.equal(deletedRemoteBranch.merge_check, 'passed');
await gitBranchDelete({ working_directory: repo, branch: 'remote/merged', base: baseBranch }, state);
const remoteBranchList = await gitBranchList({ working_directory: repo, scope: 'remote' }, state);
assert.equal((remoteBranchList.branches as any[]).some((branch: any) => branch.name === 'origin/remote/merged'), false);

const repositoriesSummary = await gitRepositoriesSummary({
  working_directories: [repo, noRemoteRepo],
  scope_label: 'test-summary',
  expected_paths_by_repository: { [repo]: [] },
}, state);
assert.equal(repositoriesSummary.scope_label, 'test-summary');
assert.equal(repositoriesSummary.repository_count, 2);
assert.equal((repositoriesSummary.repositories as any[])[0].remotes[0].name, 'origin');
assert.equal((repositoriesSummary.repositories as any[])[1].push_target.reason, 'upstream_not_configured');

const workflowRecord = await gitWorkflowRecord({
  workflow_id: 'wf-test',
  scope_label: 'test-summary',
  summary: 'test workflow record',
  repositories: [
    {
      working_directory: repo,
      staged_paths: ['README.md'],
      committed_sha: String(pushResult.pre_status ? '' : ''),
      pushed: true,
      push_status: 'pushed',
      unrelated_dirty_paths_left: [],
    },
    {
      working_directory: noRemoteRepo,
      staged_paths: ['README.md'],
      pushed: false,
      push_status: 'not_pushable',
      push_reason: 'no remote configured',
      unrelated_dirty_paths_left: [],
    },
  ],
}, state);
assert.equal(workflowRecord.workflow_id, 'wf-test');
assert.equal(workflowRecord.scope_label, 'test-summary');
assert.equal(existsSync(workflowRecord.ledger_path), true);
const workflowLedgerLines = readFileSync(workflowRecord.ledger_path, 'utf8').trim().split(/\r?\n/);
assert.equal(JSON.parse(workflowLedgerLines.at(-1) ?? '{}').workflow_id, 'wf-test');
await assert.rejects(
  () => gitWorkflowRecord({
    scope_label: 'bad-status',
    repositories: [
      { working_directory: repo, push_status: 'maybe' },
    ],
  }, state),
  /git_invalid_enum/,
);

const statusCall = await rpc({
  jsonrpc: '2.0',
  id: 3,
  method: 'tools/call',
  params: { name: 'git_status', arguments: { working_directory: repo } },
}, state);
assert.equal(statusCall.result?.structuredContent.clean, false);

const outside = await rpc({
  jsonrpc: '2.0',
  id: 4,
  method: 'tools/call',
  params: { name: 'git_status', arguments: { working_directory: tmpdir() } },
}, state);
assert.equal(outside.error?.data.code, 'git_working_directory_outside_allowed_roots');

const optionLikePush = await rpc({
  jsonrpc: '2.0',
  id: 5,
  method: 'tools/call',
  params: { name: 'git_push', arguments: { working_directory: repo, remote: '--force', branch: 'main' } },
}, state);
assert.equal(optionLikePush.error?.data.code, 'git_leading_dash_remote_not_allowed');

const readModeAdd = await rpc({
  jsonrpc: '2.0',
  id: 6,
  method: 'tools/call',
  params: { name: 'git_add', arguments: { working_directory: repo, paths: ['RENAMED.md'] } },
}, readState);
assert.equal(readModeAdd.error?.data.code, 'git_write_mode_required');
assert.equal(readModeAdd.error?.data.details.required_mode, 'write');
assert.match(readModeAdd.error?.data.details.hint, /mode=write/);
const readModeUnstage = await rpc({
  jsonrpc: '2.0',
  id: 64,
  method: 'tools/call',
  params: { name: 'git_unstage', arguments: { working_directory: repo, paths: ['RENAMED.md'] } },
}, readState);
assert.equal(readModeUnstage.error?.data.code, 'git_write_mode_required');

const readModeBranchCreate = await rpc({
  jsonrpc: '2.0',
  id: 65,
  method: 'tools/call',
  params: { name: 'git_branch_create', arguments: { working_directory: repo, name: 'read-mode-branch' } },
}, readState);
assert.equal(readModeBranchCreate.error?.data.code, 'git_write_mode_required');

writeFileSync(join(repo, 'summary.txt'), 'summary\n', 'utf8');
await rpc({
  jsonrpc: '2.0',
  id: 61,
  method: 'tools/call',
  params: { name: 'git_add', arguments: { working_directory: repo, paths: ['summary.txt'] } },
}, state);
const commitCall = await rpc({
  jsonrpc: '2.0',
  id: 62,
  method: 'tools/call',
  params: { name: 'git_commit', arguments: { working_directory: repo, message: 'Summary commit', expected_staged_paths: ['summary.txt'] } },
}, state);
assert.match(commitCall.result?.content[0].text, /summary\.txt/);

const pushCall = await rpc({
  jsonrpc: '2.0',
  id: 63,
  method: 'tools/call',
  params: { name: 'git_push', arguments: { working_directory: repo, remote: 'origin', branch: currentBranch(repo) } },
}, state);

writeFileSync(join(repo, 'RENAMED.md'), `${'changed\n'.repeat(2_300_000)}`, 'utf8');
const materialized = await rpc({
  jsonrpc: '2.0',
  id: 7,
  method: 'tools/call',
  params: { name: 'git_diff', arguments: { working_directory: repo, scope: 'working', pathspec: 'RENAMED.md', limit: 1000 } },
}, state);
assert.equal(materialized.result?.structuredContent.schema, 'narada.git.diff.v1');
assert.equal(materialized.result?.structuredContent.output_ref, undefined);
assert.match(materialized.result?.structuredContent.diff, /diff --git/);
assert.equal(materialized.result?.structuredContent.offset, 0);
assert.equal(materialized.result?.structuredContent.limit, 1000);
assert.equal(materialized.result?.structuredContent.next_offset, 1000);
assert.equal(materialized.result?.structuredContent.diff_truncated, true);
assert.equal(materialized.result?.content.length, 1);
const diffPage2 = await gitDiff({ working_directory: repo, scope: 'working', pathspec: 'RENAMED.md', offset: materialized.result?.structuredContent.next_offset, limit: 2000 }, state);
assert.equal(diffPage2.offset, 1000);
assert.equal(diffPage2.limit, 2000);
assert.equal(diffPage2.diff.length, 2000);

const largeInlineDiff = await rpc({
  jsonrpc: '2.0',
  id: 71,
  method: 'tools/call',
  params: { name: 'git_diff', arguments: { working_directory: repo, scope: 'working', pathspec: 'RENAMED.md', limit: 12000 } },
}, state);
assert.equal(largeInlineDiff.result?.structuredContent.schema, 'narada.producer_output_page.v1');
assert.equal(largeInlineDiff.result?.structuredContent.result_materialized, true);
assert.equal(largeInlineDiff.result?.structuredContent.reader_tool, 'git_output_show');
assert.match(String(largeInlineDiff.result?.structuredContent.output_ref), /^mcp_output:/);
assert.match(String(largeInlineDiff.result?.structuredContent.remediation), /bounded produced JSON pages/);
const shownLargeInlineDiff = await rpc({
  jsonrpc: '2.0',
  id: 72,
  method: 'tools/call',
  params: { name: 'git_output_show', arguments: { ref: largeInlineDiff.result?.structuredContent.output_ref, limit: 20000 } },
}, state);
assert.equal(shownLargeInlineDiff.result?.structuredContent.schema, 'narada.mcp_output_page.v1');
assert.equal(shownLargeInlineDiff.result?.structuredContent.output_scope.reader_tool, 'git_output_show');
assert.equal(shownLargeInlineDiff.result?.structuredContent.output_scope.server_output_root, root);
assert.match(shownLargeInlineDiff.result?.structuredContent.output_text, /"schema": "narada.git.diff.v1"/);
assert.match(shownLargeInlineDiff.result?.structuredContent.output_text, /"limit": 12000/);
assert.match(shownLargeInlineDiff.result?.structuredContent.output_text, /"next_offset": 12000/);
const missingOutputRef = await rpc({
  jsonrpc: '2.0',
  id: 73,
  method: 'tools/call',
  params: { name: 'git_output_show', arguments: { ref: 'mcp_output:missing' } },
}, state);
assert.equal(missingOutputRef.error?.data.code, 'git_output_ref_scope_unreadable');
assert.equal(missingOutputRef.error?.data.details.output_root, root);
assert.match(missingOutputRef.error?.data.details.remediation, /same Git MCP server/);
const foreignRootAttempt = await rpc({
  jsonrpc: '2.0',
  id: 74,
  method: 'tools/call',
  params: { name: 'git_output_show', arguments: { ref: 'mcp_output:missing', target_site_root: join(root, 'other-site') } },
}, state);
assert.equal(foreignRootAttempt.error?.data.code, 'git_output_ref_scope_unreadable');
assert.match(foreignRootAttempt.error?.data.message, /target_site_root_not_supported/);

const syncRoot = mkdtempSync(join(tmpdir(), 'git-mcp-sync-'));
const syncRemote = join(syncRoot, 'remote.git');
const syncRepo = join(syncRoot, 'repo');
const syncPeer = join(syncRoot, 'peer');
git(syncRoot, ['init', '--bare', '--initial-branch=main', syncRemote]);
git(syncRoot, ['init', '--initial-branch=main', syncRepo]);
git(syncRepo, ['config', 'user.email', 'agent@example.test']);
git(syncRepo, ['config', 'user.name', 'Agent Test']);
git(syncRepo, ['remote', 'add', 'origin', syncRemote]);
writeFileSync(join(syncRepo, 'conflict.txt'), 'base\n', 'utf8');
git(syncRepo, ['add', 'conflict.txt']);
git(syncRepo, ['commit', '-m', 'sync base']);
git(syncRepo, ['push', '--set-upstream', 'origin', 'main']);
git(syncRoot, ['clone', syncRemote, syncPeer]);
git(syncPeer, ['config', 'user.email', 'peer@example.test']);
git(syncPeer, ['config', 'user.name', 'Peer Test']);
const syncState = createServerState({ allowedRoot: syncRoot, outputRoot: syncRoot, mode: 'write' });
const syncReadState = createServerState({ allowedRoot: syncRoot, outputRoot: syncRoot, mode: 'read' });

writeFileSync(join(syncPeer, 'remote.txt'), 'remote\n', 'utf8');
git(syncPeer, ['add', 'remote.txt']);
git(syncPeer, ['commit', '-m', 'peer change']);
git(syncPeer, ['push', 'origin', 'main']);
const fetched = await gitFetch({ working_directory: syncRepo, remote: 'origin', branch: 'main', scope_label: 'remote-sync-test' }, syncState);
assert.equal(fetched.schema, 'narada.git.fetch.v1');
assert.equal(fetched.status, 'ok');
assert.equal((fetched.verified_post_state as any).verification, 'verified');
assert.equal((fetched.post_status as any).behind, 1);

writeFileSync(join(syncRepo, 'local.txt'), 'local\n', 'utf8');
git(syncRepo, ['add', 'local.txt']);
git(syncRepo, ['commit', '-m', 'local change']);
const rebased = await gitRebase({ working_directory: syncRepo, onto: 'origin/main', autostash: false }, syncState);
assert.equal(rebased.schema, 'narada.git.rebase.v1');
assert.equal(rebased.status, 'rebased');
assert.equal((rebased.verified_post_state as any).verification, 'verified');
assert.equal((rebased.post_status as any).behind, 0);

writeFileSync(join(syncPeer, 'remote-two.txt'), 'remote two\n', 'utf8');
git(syncPeer, ['add', 'remote-two.txt']);
git(syncPeer, ['commit', '-m', 'peer second change']);
git(syncPeer, ['push', 'origin', 'main']);
await gitFetch({ working_directory: syncRepo, remote: 'origin', branch: 'main' }, syncState);
writeFileSync(join(syncRepo, 'local.txt'), 'local dirty\n', 'utf8');
const dirtyRebase = await gitRebase({ working_directory: syncRepo, onto: 'origin/main', autostash: true }, syncState);
assert.equal(dirtyRebase.status, 'rebased');
assert.equal(readFileSync(join(syncRepo, 'local.txt'), 'utf8').replaceAll('\r\n', '\n'), 'local dirty\n');
writeFileSync(join(syncRepo, 'untracked.txt'), 'must preserve\n', 'utf8');
await assert.rejects(
  () => gitRebase({ working_directory: syncRepo, onto: 'origin/main', autostash: true }, syncState),
  (error: any) => error.codeName === 'git_untracked_worktree_requires_manual_preservation',
);
writeFileSync(join(syncRepo, 'local.txt'), 'local dirty\n', 'utf8');
git(syncRepo, ['add', 'local.txt']);
git(syncRepo, ['commit', '-m', 'preserve local dirty test']);
git(syncRepo, ['clean', '-f']);

writeFileSync(join(syncPeer, 'conflict.txt'), 'remote conflict\n', 'utf8');
git(syncPeer, ['add', 'conflict.txt']);
git(syncPeer, ['commit', '-m', 'remote conflict']);
git(syncPeer, ['push', 'origin', 'main']);
writeFileSync(join(syncRepo, 'conflict.txt'), 'local conflict\n', 'utf8');
git(syncRepo, ['add', 'conflict.txt']);
git(syncRepo, ['commit', '-m', 'local conflict']);
await gitFetch({ working_directory: syncRepo, remote: 'origin', branch: 'main' }, syncState);
const conflictRebase = await gitRebase({ working_directory: syncRepo, onto: 'origin/main', autostash: false }, syncState);
assert.equal(conflictRebase.status, 'conflict');
assert.equal(conflictRebase.operation_in_progress, true);
assert.deepEqual(conflictRebase.recovery, ['git_rebase_continue', 'git_rebase_abort', 'git_sync_status']);
const syncStatus = await gitSyncStatus({ working_directory: syncRepo }, syncState);
assert.equal(syncStatus.operation, 'rebase');
assert.equal(syncStatus.in_progress, true);
const abortedRebase = await gitRebaseAbort({ working_directory: syncRepo }, syncState);
assert.equal(abortedRebase.status, 'aborted');
assert.equal((await gitSyncStatus({ working_directory: syncRepo }, syncState)).operation, null);

git(syncRepo, ['branch', 'feature']);
git(syncRepo, ['switch', 'feature']);
writeFileSync(join(syncRepo, 'feature.txt'), 'feature\n', 'utf8');
git(syncRepo, ['add', 'feature.txt']);
git(syncRepo, ['commit', '-m', 'feature change']);
git(syncRepo, ['switch', 'main']);
const merged = await gitMerge({ working_directory: syncRepo, target: 'feature', autostash: false }, syncState);
assert.equal(merged.status, 'merged');
assert.equal((merged.verified_post_state as any).verification, 'verified');
assert.equal((merged.post_status as any).clean, true);
assert.equal((await gitMergeContinue({ working_directory: syncRepo }, syncState).catch(() => null)), null);
await assert.rejects(
  () => gitFetch({ working_directory: syncRepo, remote: 'origin', branch: 'main' }, syncReadState),
  (error: any) => error.codeName === 'git_write_mode_required',
);

function git(cwd: string, args: string[]): string {
  return execFileSync('git', args, { cwd, encoding: 'utf8', windowsHide: true });
}

function currentBranch(cwd: string): string {
  return git(cwd, ['branch', '--show-current']).trim();
}