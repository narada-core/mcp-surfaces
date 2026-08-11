import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
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
  gitEndWorkScope,
  gitFetch,
  gitLog,
  gitMerge,
  gitMergeAbort,
  gitMergeContinue,
  gitPush,
  gitRebase,
  gitRebaseAbort,
  gitRebaseContinue,
  gitReconcileIndex,
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
  'git_end_work_scope',
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
  'git_reconcile_index',
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
  'git_begin_work_scope',
  'git_end_work_scope',
  'git_add',
  'git_unstage',
  'git_commit_paths',
  'git_reconcile_index',
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

const initialWorkScope = await gitBeginWorkScope({ working_directory: repo, owner_id: 'git-mcp-test', allowed_paths: ['README.md'] }, state);
const addResult = await gitAdd({ working_directory: repo, paths: ['README.md'], work_scope_ref: initialWorkScope.work_scope_ref }, state);
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
  params: { name: 'git_unstage', arguments: { working_directory: repo, paths: ['README.md'], work_scope_ref: initialWorkScope.work_scope_ref } },
}, state);
assert.equal(unstageResponse.error, undefined);
assert.equal(unstageResponse.result?.structuredContent.schema, 'narada.git.unstage.v1');
assert.equal(unstageResponse.result?.structuredContent.verified_post_state.verification, 'verified');
assert.deepEqual((unstageResponse.result?.structuredContent.post_status as any).staged, []);
assert.deepEqual((unstageResponse.result?.structuredContent.post_status as any).unstaged, []);
await gitAdd({ working_directory: repo, paths: ['README.md'], work_scope_ref: initialWorkScope.work_scope_ref }, state);

const commitResult = await gitCommit({ working_directory: repo, message: 'Initial commit', expected_staged_paths: ['README.md'], work_scope_ref: initialWorkScope.work_scope_ref }, state);
assert.match(commitResult.commit, /^[0-9a-f]{40}$/);
assert.equal((commitResult.verified_post_state as any).verification, 'verified');
assert.equal(commitResult.verification_status, 'verified');
assert.equal((commitResult.mutation_effect as any).operation, 'commit');
await gitEndWorkScope({ working_directory: repo, owner_id: 'git-mcp-test', work_scope_ref: initialWorkScope.work_scope_ref }, state);

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
const isolatedScope = await gitBeginWorkScope({ working_directory: isolatedRepo, owner_id: 'isolated-agent', allowed_paths: ['alpha.txt'] }, state);
const isolatedCommit = await gitCommitPaths({
  working_directory: isolatedRepo,
  paths: ['alpha.txt'],
  message: 'Commit alpha through isolated index',
  scope_label: 'agent-alpha',
  work_scope_ref: isolatedScope.work_scope_ref,
}, state);
assert.equal(isolatedCommit.isolation, 'dedicated_temporary_index');
assert.deepEqual(isolatedCommit.committed_files, ['alpha.txt']);
assert.deepEqual((isolatedCommit.post_status as any).staged, ['beta.txt']);
assert.equal(git(isolatedRepo, ['show', '--format=', '--name-only', 'HEAD']).trim(), 'alpha.txt');
assert.equal(git(isolatedRepo, ['show', 'HEAD:alpha.txt']).trim(), 'alpha agent');
assert.equal(git(isolatedRepo, ['show', 'HEAD:beta.txt']).trim(), 'beta base');
await gitEndWorkScope({ working_directory: isolatedRepo, owner_id: 'isolated-agent', work_scope_ref: isolatedScope.work_scope_ref }, state);
writeFileSync(join(isolatedRepo, 'alpha.txt'), 'alpha after lock\n', 'utf8');
const lockedScope = await gitBeginWorkScope({ working_directory: isolatedRepo, owner_id: 'isolated-agent', allowed_paths: ['alpha.txt'] }, state);
const heldIndexLock = join(isolatedRepo, '.git', 'index.lock');
writeFileSync(heldIndexLock, 'held by competing git process', 'utf8');
const pendingReconciliation = await gitCommitPaths({
  working_directory: isolatedRepo,
  paths: ['alpha.txt'],
  message: 'Commit while shared index is locked',
  work_scope_ref: lockedScope.work_scope_ref,
}, state);
assert.equal(pendingReconciliation.status, 'committed_shared_index_reconciliation_required');
assert.equal((pendingReconciliation as any).reconciliation.reason, 'shared_index_locked');
assert.match((pendingReconciliation as any).reconciliation_ref, /^gir_/);
assert.equal((pendingReconciliation as any).reconciliation.retry_tool, 'git_reconcile_index');
assert.equal(existsSync(heldIndexLock), true);
rmSync(heldIndexLock);
const retriedReconciliation = await gitReconcileIndex({
  working_directory: isolatedRepo,
  reconciliation_ref: (pendingReconciliation as any).reconciliation_ref,
  work_scope_ref: lockedScope.work_scope_ref,
}, state);
assert.equal(retriedReconciliation.status, 'reconciled');
assert.equal(((retriedReconciliation as any).post_status as any).staged.includes('beta.txt'), true);
await gitEndWorkScope({ working_directory: isolatedRepo, owner_id: 'isolated-agent', work_scope_ref: lockedScope.work_scope_ref }, state);

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
const scopeRegistry = join(scopedRepo, '.git', 'narada-work-scopes');
mkdirSync(scopeRegistry, { recursive: true });
const abandonedRegistryLock = join(scopeRegistry, '.lock');
writeFileSync(abandonedRegistryLock, '');
const staleTime = new Date(Date.now() - 60_000);
utimesSync(abandonedRegistryLock, staleTime, staleTime);
writeFileSync(abandonedRegistryLock, JSON.stringify({
  schema: 'narada.git.work_scope_registry_lock.v2',
  pid: process.pid,
  process_instance_id: 'reused-pid-instance',
  lock_nonce: 'reused-pid-lock',
  created_at: new Date().toISOString(),
}));
const reusedPidRecoveryScope = await gitBeginWorkScope({ working_directory: scopedRepo, owner_id: 'pid-reuse-recovery', allowed_paths: ['alpha.txt'] }, state);
await gitEndWorkScope({ working_directory: scopedRepo, owner_id: 'pid-reuse-recovery', work_scope_ref: reusedPidRecoveryScope.work_scope_ref }, state);
const currentProcessRecord = JSON.parse(readFileSync(join(scopeRegistry, '.processes', `${process.pid}.json`), 'utf8')) as { process_instance_id: string };
writeFileSync(abandonedRegistryLock, JSON.stringify({
  schema: 'narada.git.work_scope_registry_lock.v2',
  pid: process.pid,
  process_instance_id: currentProcessRecord.process_instance_id,
  lock_nonce: 'live-long-held-lock',
  created_at: new Date(Date.now() - 3_600_000).toISOString(),
}));
utimesSync(abandonedRegistryLock, new Date(Date.now() - 3_600_000), new Date(Date.now() - 3_600_000));
await assert.rejects(
  () => gitBeginWorkScope({ working_directory: scopedRepo, owner_id: 'must-not-steal-live-lock', allowed_paths: ['alpha.txt'] }, state),
  (error: any) => error.codeName === 'git_work_scope_registry_busy'
    && error.details.owner_process_alive === true
    && error.details.owner_process_instance_matches === true,
);
rmSync(abandonedRegistryLock);
const workScope = await gitBeginWorkScope({ working_directory: scopedRepo, owner_id: 'git-mcp-test', allowed_paths: ['alpha.txt'] }, state);
assert.equal(existsSync(abandonedRegistryLock), false);
assert.match(workScope.work_scope_ref, /^gws_/);
assert.deepEqual(workScope.allowed_paths, ['alpha.txt']);
const competingState = createServerState({ allowedRoot: root, outputRoot: root, mode: 'write' });
await assert.rejects(
  () => gitBeginWorkScope({ working_directory: scopedRepo, owner_id: 'competing-agent', allowed_paths: ['alpha.txt'] }, competingState),
  (error: any) => error.codeName === 'git_work_scope_path_already_owned' && error.details.current_owner_id === 'git-mcp-test',
);
const independentScope = await gitBeginWorkScope({ working_directory: scopedRepo, owner_id: 'competing-agent', allowed_paths: ['beta.txt'] }, competingState);
await gitEndWorkScope({ working_directory: scopedRepo, owner_id: 'competing-agent', work_scope_ref: independentScope.work_scope_ref }, competingState);
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
const betaScope = await gitBeginWorkScope({ working_directory: scopedRepo, owner_id: 'beta-cleanup', allowed_paths: ['beta.txt'] }, state);
await gitUnstage({ working_directory: scopedRepo, paths: ['beta.txt'], work_scope_ref: betaScope.work_scope_ref }, state);
await gitEndWorkScope({ working_directory: scopedRepo, owner_id: 'beta-cleanup', work_scope_ref: betaScope.work_scope_ref }, state);
const scopedCommit = await gitCommit({ working_directory: scopedRepo, message: 'Scoped commit', work_scope_ref: workScope.work_scope_ref, index_scope_ref: scopedAdd.index_scope_ref }, state);
assert.equal(scopedCommit.committed_files.includes('alpha.txt'), true);
assert.match(scopedCommit.commit_ref, /^git_commit:[0-9a-f]{40}$/);
await gitEndWorkScope({ working_directory: scopedRepo, owner_id: 'git-mcp-test', work_scope_ref: workScope.work_scope_ref }, competingState);
await assert.rejects(
  () => gitStatus({ working_directory: scopedRepo, work_scope_ref: workScope.work_scope_ref }, state),
  /git_work_scope_ref_released/,
);
const reclaimedScope = await gitBeginWorkScope({ working_directory: scopedRepo, owner_id: 'competing-agent', allowed_paths: ['alpha.txt'] }, competingState);
await gitEndWorkScope({ working_directory: scopedRepo, owner_id: 'competing-agent', work_scope_ref: reclaimedScope.work_scope_ref }, competingState);
const topologyScope = await gitBeginWorkScope({ working_directory: scopedRepo, owner_id: 'topology-drift-test', scope_kind: 'repository_topology' }, state);
assert.equal(topologyScope.authority, 'repository_topology');
await assert.rejects(
  () => gitBeginWorkScope({ working_directory: scopedRepo, owner_id: 'path-conflicts-with-topology', allowed_paths: ['alpha.txt'] }, competingState),
  (error: any) => error.codeName === 'git_work_scope_path_already_owned' && error.details.paths.includes('<repository-topology>'),
);
writeFileSync(join(scopedRepo, 'alpha.txt'), 'out-of-band editor change\n', 'utf8');
await assert.rejects(
  () => gitBranchCreate({ working_directory: scopedRepo, name: 'must-not-create-after-drift', work_scope_ref: topologyScope.work_scope_ref }, state),
  (error: any) => error.codeName === 'git_repository_topology_scope_base_state_drift'
    && error.details.changed_fields.includes('worktree_digest')
    && error.details.cooperative_boundary === true,
);
await gitEndWorkScope({ working_directory: scopedRepo, owner_id: 'topology-drift-test', work_scope_ref: topologyScope.work_scope_ref }, state);
mkdirSync(join(scopedRepo, 'nested'), { recursive: true });
writeFileSync(join(scopedRepo, 'nested', 'file.txt'), 'nested\n', 'utf8');
const directoryScope = await gitBeginWorkScope({ working_directory: scopedRepo, owner_id: 'directory-test', allowed_paths: ['nested'] }, state);
const directoryAdd = await gitAdd({ working_directory: scopedRepo, paths: ['nested'], work_scope_ref: directoryScope.work_scope_ref }, state);
assert.deepEqual(directoryAdd.paths, ['nested/file.txt']);
assert.deepEqual((directoryAdd.post_status as any).staged, ['nested/file.txt']);
await gitUnstage({ working_directory: scopedRepo, paths: ['nested/file.txt'], work_scope_ref: directoryScope.work_scope_ref }, state);
await gitEndWorkScope({ working_directory: scopedRepo, owner_id: 'directory-test', work_scope_ref: directoryScope.work_scope_ref }, state);

writeFileSync(join(repo, 'scope-a.txt'), 'a\n', 'utf8');
writeFileSync(join(repo, 'scope-b.txt'), 'b\n', 'utf8');
const multiPathScope = await gitBeginWorkScope({ working_directory: repo, owner_id: 'multi-path-test', allowed_paths: ['scope-a.txt', 'scope-b.txt'] }, state);
await gitAdd({ working_directory: repo, paths: ['scope-a.txt', 'scope-b.txt'], work_scope_ref: multiPathScope.work_scope_ref }, state);
await assert.rejects(
  () => gitCommit({ working_directory: repo, message: 'Reject unrelated staged path', scope_label: 'scope-a-only', expected_staged_paths: ['scope-a.txt'], work_scope_ref: multiPathScope.work_scope_ref }, state),
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
await gitUnstage({ working_directory: repo, paths: ['scope-a.txt', 'scope-b.txt'], work_scope_ref: multiPathScope.work_scope_ref }, state);
await gitEndWorkScope({ working_directory: repo, owner_id: 'multi-path-test', work_scope_ref: multiPathScope.work_scope_ref }, state);

writeFileSync(join(repo, '.gitignore'), 'ignored-staging.txt\n', 'utf8');
writeFileSync(join(repo, 'staging-safe.txt'), 'safe\n', 'utf8');
writeFileSync(join(repo, 'ignored-staging.txt'), 'ignored\n', 'utf8');
const ignoredPathScope = await gitBeginWorkScope({ working_directory: repo, owner_id: 'ignored-path-test', allowed_paths: ['staging-safe.txt', 'ignored-staging.txt'] }, state);
await assert.rejects(
  () => gitAdd({ working_directory: repo, paths: ['staging-safe.txt', 'ignored-staging.txt'], work_scope_ref: ignoredPathScope.work_scope_ref }, state),
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
await gitEndWorkScope({ working_directory: repo, owner_id: 'ignored-path-test', work_scope_ref: ignoredPathScope.work_scope_ref }, state);

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

const broadPathScope = await gitBeginWorkScope({ working_directory: repo, owner_id: 'broad-path-test', allowed_paths: ['README.md'] }, state);
await assert.rejects(
  () => gitAdd({ working_directory: repo, paths: ['.'], work_scope_ref: broadPathScope.work_scope_ref }, state),
  /git_broad_path_not_allowed/,
);
await gitEndWorkScope({ working_directory: repo, owner_id: 'broad-path-test', work_scope_ref: broadPathScope.work_scope_ref }, state);

await assert.rejects(
  () => gitShow({ working_directory: repo, commit: '--all' }, state),
  /git_leading_dash_commitish_not_allowed/,
);

const readmeScope = await gitBeginWorkScope({ working_directory: repo, owner_id: 'readme-update-test', allowed_paths: ['README.md'] }, state);
await gitAdd({ working_directory: repo, paths: ['README.md'], work_scope_ref: readmeScope.work_scope_ref }, state);
await gitCommit({ working_directory: repo, message: 'Update readme', expected_staged_paths: ['README.md'], work_scope_ref: readmeScope.work_scope_ref }, state);
await gitEndWorkScope({ working_directory: repo, owner_id: 'readme-update-test', work_scope_ref: readmeScope.work_scope_ref }, state);

git(repo, ['mv', 'README.md', 'RENAMED.md']);
const renameStatus = await gitStatus({ working_directory: repo }, state);
assert.deepEqual(renameStatus.staged, ['README.md <- RENAMED.md']);
assert.deepEqual((renameStatus.status_entries as any[]).filter((entry: any) => !entry.untracked).map((entry: any) => ({
  x: entry.x,
  y: entry.y,
  path: entry.path,
  original_path: entry.original_path,
})), [{ x: 'R', y: ' ', path: 'RENAMED.md', original_path: 'README.md' }]);
const renameScope = await gitBeginWorkScope({ working_directory: repo, owner_id: 'rename-test', allowed_paths: ['README.md', 'RENAMED.md'] }, state);
const renameCommit = await gitCommit({ working_directory: repo, message: 'Rename readme', expected_staged_paths: ['README.md <- RENAMED.md'], work_scope_ref: renameScope.work_scope_ref }, state);
assert.deepEqual(renameCommit.committed_files, ['README.md <- RENAMED.md']);
assert.deepEqual((renameCommit.committed_entries as any[]).map((entry: any) => ({
  x: entry.x,
  y: entry.y,
  path: entry.path,
  original_path: entry.original_path,
})), [{ x: 'R', y: ' ', path: 'RENAMED.md', original_path: 'README.md' }]);
await gitEndWorkScope({ working_directory: repo, owner_id: 'rename-test', work_scope_ref: renameScope.work_scope_ref }, state);
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

async function topologyMutation<T>(workingDirectory: string, ownerId: string, mutation: (workScopeRef: string) => Promise<T>): Promise<T> {
  const scope = await gitBeginWorkScope({ working_directory: workingDirectory, owner_id: ownerId, scope_kind: 'repository_topology' }, state);
  try {
    return await mutation(String(scope.work_scope_ref));
  } finally {
    await gitEndWorkScope({ working_directory: workingDirectory, owner_id: ownerId, work_scope_ref: scope.work_scope_ref }, state);
  }
}

const createdBranch = await topologyMutation(repo, 'branch-create-feature', (work_scope_ref) => gitBranchCreate({ working_directory: repo, name: 'feature/mcp', work_scope_ref }, state));
assert.equal(createdBranch.checked_out, false);
assert.equal((createdBranch.verified_post_state as any).verification, 'verified');
assert.equal((createdBranch.post_status as any).branch, baseBranch);
const switchedBranch = await topologyMutation(repo, 'branch-switch-feature', (work_scope_ref) => gitBranchSwitch({ working_directory: repo, branch: 'feature/mcp', work_scope_ref }, state));
assert.equal((switchedBranch.verified_post_state as any).verification, 'verified');
assert.equal((switchedBranch.post_status as any).branch, 'feature/mcp');
const renamedBranch = await topologyMutation(repo, 'branch-rename-feature', (work_scope_ref) => gitBranchRename({ working_directory: repo, old_name: 'feature/mcp', new_name: 'feature/renamed', work_scope_ref }, state));
assert.equal((renamedBranch.verified_post_state as any).verification, 'verified');
assert.equal((renamedBranch.post_status as any).branch, 'feature/renamed');
await topologyMutation(repo, 'branch-switch-base-merged', (work_scope_ref) => gitBranchSwitch({ working_directory: repo, branch: baseBranch, work_scope_ref }, state));
const deletedMergedBranch = await topologyMutation(repo, 'branch-delete-merged', (work_scope_ref) => gitBranchDelete({ working_directory: repo, branch: 'feature/renamed', base: baseBranch, work_scope_ref }, state));
assert.equal(deletedMergedBranch.merge_check, 'passed');

await topologyMutation(repo, 'branch-create-unmerged', (work_scope_ref) => gitBranchCreate({ working_directory: repo, name: 'feature/unmerged', start_point: baseBranch, work_scope_ref }, state));
await topologyMutation(repo, 'branch-switch-unmerged', (work_scope_ref) => gitBranchSwitch({ working_directory: repo, branch: 'feature/unmerged', work_scope_ref }, state));
writeFileSync(join(repo, 'unmerged.txt'), 'unmerged branch\n', 'utf8');
git(repo, ['add', 'unmerged.txt']);
git(repo, ['commit', '-m', 'Unmerged branch test']);
await topologyMutation(repo, 'branch-switch-base-unmerged', (work_scope_ref) => gitBranchSwitch({ working_directory: repo, branch: baseBranch, work_scope_ref }, state));
await assert.rejects(
  () => topologyMutation(repo, 'branch-refuse-unmerged', (work_scope_ref) => gitBranchDelete({ working_directory: repo, branch: 'feature/unmerged', base: baseBranch, work_scope_ref }, state)),
  (error: any) => error.codeName === 'git_branch_not_merged',
);
git(repo, ['branch', '-D', 'feature/unmerged']);

await topologyMutation(repo, 'branch-create-remote', (work_scope_ref) => gitBranchCreate({ working_directory: repo, name: 'remote/merged', start_point: baseBranch, work_scope_ref }, state));
await gitPush({ working_directory: repo, remote: 'origin', branch: 'remote/merged' }, state);
const setUpstream = await topologyMutation(repo, 'branch-set-upstream', (work_scope_ref) => gitBranchSetUpstream({ working_directory: repo, local_branch: baseBranch, remote: 'origin', remote_branch: baseBranch, work_scope_ref }, state));
assert.equal((setUpstream.post_status as any).upstream, `origin/${baseBranch}`);
const unsetUpstream = await topologyMutation(repo, 'branch-unset-upstream', (work_scope_ref) => gitBranchUnsetUpstream({ working_directory: repo, local_branch: baseBranch, work_scope_ref }, state));
assert.equal((unsetUpstream.post_status as any).upstream, null);
const deletedRemoteBranch = await topologyMutation(repo, 'branch-delete-remote', (work_scope_ref) => gitBranchDeleteRemote({ working_directory: repo, remote: 'origin', branch: 'remote/merged', base: baseBranch, work_scope_ref }, state));
assert.equal(deletedRemoteBranch.merge_check, 'passed');
await topologyMutation(repo, 'branch-delete-local-remote', (work_scope_ref) => gitBranchDelete({ working_directory: repo, branch: 'remote/merged', base: baseBranch, work_scope_ref }, state));
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
const summaryScope = await gitBeginWorkScope({ working_directory: repo, owner_id: 'rpc-summary-test', allowed_paths: ['summary.txt'] }, state);
await rpc({
  jsonrpc: '2.0',
  id: 61,
  method: 'tools/call',
  params: { name: 'git_add', arguments: { working_directory: repo, paths: ['summary.txt'], work_scope_ref: summaryScope.work_scope_ref } },
}, state);
const commitCall = await rpc({
  jsonrpc: '2.0',
  id: 62,
  method: 'tools/call',
  params: { name: 'git_commit', arguments: { working_directory: repo, message: 'Summary commit', expected_staged_paths: ['summary.txt'], work_scope_ref: summaryScope.work_scope_ref } },
}, state);
assert.match(commitCall.result?.content[0].text, /summary\.txt/);
await gitEndWorkScope({ working_directory: repo, owner_id: 'rpc-summary-test', work_scope_ref: summaryScope.work_scope_ref }, state);

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
async function syncTopologyMutation<T>(ownerId: string, mutation: (workScopeRef: string) => Promise<T>): Promise<T> {
  const scope = await gitBeginWorkScope({ working_directory: syncRepo, owner_id: ownerId, scope_kind: 'repository_topology' }, syncState);
  try {
    return await mutation(String(scope.work_scope_ref));
  } finally {
    await gitEndWorkScope({ working_directory: syncRepo, owner_id: ownerId, work_scope_ref: scope.work_scope_ref }, syncState);
  }
}

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
const rebased = await syncTopologyMutation('rebase-clean', (work_scope_ref) => gitRebase({ working_directory: syncRepo, onto: 'origin/main', autostash: false, work_scope_ref }, syncState));
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
const dirtyRebase = await syncTopologyMutation('rebase-dirty', (work_scope_ref) => gitRebase({ working_directory: syncRepo, onto: 'origin/main', autostash: true, work_scope_ref }, syncState));
assert.equal(dirtyRebase.status, 'rebased');
assert.equal(readFileSync(join(syncRepo, 'local.txt'), 'utf8').replaceAll('\r\n', '\n'), 'local dirty\n');
writeFileSync(join(syncRepo, 'untracked.txt'), 'must preserve\n', 'utf8');
await assert.rejects(
  () => syncTopologyMutation('rebase-untracked', (work_scope_ref) => gitRebase({ working_directory: syncRepo, onto: 'origin/main', autostash: true, work_scope_ref }, syncState)),
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
const conflictScope = await gitBeginWorkScope({ working_directory: syncRepo, owner_id: 'rebase-conflict', scope_kind: 'repository_topology' }, syncState);
const conflictRebase = await gitRebase({ working_directory: syncRepo, onto: 'origin/main', autostash: false, work_scope_ref: conflictScope.work_scope_ref }, syncState);
assert.equal(conflictRebase.status, 'conflict');
assert.equal(conflictRebase.operation_in_progress, true);
assert.deepEqual(conflictRebase.recovery, ['git_rebase_continue', 'git_rebase_abort', 'git_sync_status']);
const syncStatus = await gitSyncStatus({ working_directory: syncRepo }, syncState);
assert.equal(syncStatus.operation, 'rebase');
assert.equal(syncStatus.in_progress, true);
const abortedRebase = await gitRebaseAbort({ working_directory: syncRepo, work_scope_ref: conflictScope.work_scope_ref }, syncState);
await gitEndWorkScope({ working_directory: syncRepo, owner_id: 'rebase-conflict', work_scope_ref: conflictScope.work_scope_ref }, syncState);
assert.equal(abortedRebase.status, 'aborted');
assert.equal((await gitSyncStatus({ working_directory: syncRepo }, syncState)).operation, null);

git(syncRepo, ['branch', 'feature']);
git(syncRepo, ['switch', 'feature']);
writeFileSync(join(syncRepo, 'feature.txt'), 'feature\n', 'utf8');
git(syncRepo, ['add', 'feature.txt']);
git(syncRepo, ['commit', '-m', 'feature change']);
git(syncRepo, ['switch', 'main']);
const merged = await syncTopologyMutation('merge-clean', (work_scope_ref) => gitMerge({ working_directory: syncRepo, target: 'feature', autostash: false, work_scope_ref }, syncState));
assert.equal(merged.status, 'merged');
assert.equal((merged.verified_post_state as any).verification, 'verified');
assert.equal((merged.post_status as any).clean, true);
assert.equal((await syncTopologyMutation('merge-continue-noop', (work_scope_ref) => gitMergeContinue({ working_directory: syncRepo, work_scope_ref }, syncState)).catch(() => null)), null);
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