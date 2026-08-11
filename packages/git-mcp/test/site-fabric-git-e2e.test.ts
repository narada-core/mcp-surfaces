import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  createTemporaryE2eRoot,
  removeTemporaryE2eRoot,
  runMcpProtocolSmoke,
  siteFabricChildEnv,
  spawnJsonlMcpServer,
  type JsonRecord,
} from '@narada-core/mcp-e2e-harness';

const siteRoot = createTemporaryE2eRoot('git-site-fabric-e2e');
const repo = join(siteRoot, 'repo');
const remote = join(siteRoot, 'remote.git');
mkdirSync(repo, { recursive: true });
execFileSync('git', ['init', '--initial-branch=main', repo], { stdio: 'ignore', windowsHide: true });
execFileSync('git', ['init', '--bare', '--initial-branch=main', remote], { stdio: 'ignore', windowsHide: true });
execFileSync('git', ['config', 'user.email', 'e2e@example.test'], { cwd: repo, stdio: 'ignore', windowsHide: true });
execFileSync('git', ['config', 'user.name', 'MCP E2E'], { cwd: repo, stdio: 'ignore', windowsHide: true });
execFileSync('git', ['remote', 'add', 'origin', remote], { cwd: repo, stdio: 'ignore', windowsHide: true });
writeFileSync(join(repo, 'README.md'), 'site fabric git e2e\n', 'utf8');

const serverPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
const server = spawnJsonlMcpServer(process.execPath, [serverPath, '--allowed-root', siteRoot, '--mode', 'write'], {
  cwd: siteRoot,
  env: siteFabricChildEnv(siteRoot),
  label: 'git Site fabric e2e',
});

function structured(response: JsonRecord): JsonRecord {
  assert.equal(response.error, undefined, JSON.stringify(response));
  return (response.result as JsonRecord)?.structuredContent as JsonRecord ?? response.result as JsonRecord;
}

try {
  await runMcpProtocolSmoke(server.client, {
    requiredTools: [
      'git_policy_inspect', 'git_status', 'git_branch_list', 'git_diff', 'git_begin_work_scope', 'git_end_work_scope', 'git_add', 'git_commit', 'git_push', 'git_log',
      'git_branch_create', 'git_branch_switch', 'git_branch_rename', 'git_branch_delete', 'git_branch_delete_remote',
      'git_branch_set_upstream', 'git_branch_unset_upstream',
    ],
  });

  const policy = structured(await server.client.request(1, 'tools/call', { name: 'git_policy_inspect', arguments: {} }));
  assert.equal(policy.mode, 'write', JSON.stringify(policy));

  const initial = structured(await server.client.request(2, 'tools/call', {
    name: 'git_status', arguments: { working_directory: repo },
  }));
  assert.deepEqual(initial.untracked, ['README.md']);

  const diff = structured(await server.client.request(3, 'tools/call', {
    name: 'git_diff', arguments: { working_directory: repo, scope: 'working', pathspecs: ['README.md'], include_untracked: true, limit: 2000 },
  }));
  assert.equal(diff.schema, 'narada.git.diff.v1', JSON.stringify(diff));
  assert.match(String(diff.diff), /site fabric git e2e/);

  const initialScope = structured(await server.client.request(30, 'tools/call', {
    name: 'git_begin_work_scope', arguments: { working_directory: repo, owner_id: 'site-fabric-initial', allowed_paths: ['README.md'] },
  }));
  const added = structured(await server.client.request(4, 'tools/call', {
    name: 'git_add', arguments: { working_directory: repo, paths: ['README.md'], work_scope_ref: initialScope.work_scope_ref },
  }));
  assert.deepEqual((added.post_status as JsonRecord).staged, ['README.md'], JSON.stringify(added));

  const committed = structured(await server.client.request(4, 'tools/call', {
    name: 'git_commit', arguments: { working_directory: repo, message: 'Site fabric E2E commit', work_scope_ref: initialScope.work_scope_ref },
  }));
  assert.match(String(committed.commit), /^[0-9a-f]{40}$/);
  structured(await server.client.request(31, 'tools/call', {
    name: 'git_end_work_scope', arguments: { working_directory: repo, owner_id: 'site-fabric-initial', work_scope_ref: initialScope.work_scope_ref },
  }));

  const pushed = structured(await server.client.request(5, 'tools/call', {
    name: 'git_push', arguments: { working_directory: repo, remote: 'origin', branch: 'main' },
  }));
  assert.equal(pushed.status, 'ok', JSON.stringify(pushed));
  assert.match(String(execFileSync('git', ['rev-parse', 'refs/heads/main'], { cwd: remote, encoding: 'utf8', windowsHide: true })).trim(), /^[0-9a-f]{40}$/);
  const initialRemoteBranches = structured(await server.client.request(16, 'tools/call', {
    name: 'git_branch_list', arguments: { working_directory: repo, scope: 'remote' },
  }));
  assert.equal((initialRemoteBranches.branches as JsonRecord[]).some((branch) => branch.name === 'origin/main'), true, JSON.stringify(initialRemoteBranches));

  const createdBranch = structured(await server.client.request(8, 'tools/call', {
    name: 'git_branch_create', arguments: { working_directory: repo, name: 'site-feature' },
  }));
  assert.equal(createdBranch.checked_out, false, JSON.stringify(createdBranch));
  const listedBranches = structured(await server.client.request(9, 'tools/call', {
    name: 'git_branch_list', arguments: { working_directory: repo, scope: 'local' },
  }));
  assert.equal((listedBranches.branches as JsonRecord[]).some((branch) => branch.name === 'site-feature'), true);
  const switchedBranch = structured(await server.client.request(10, 'tools/call', {
    name: 'git_branch_switch', arguments: { working_directory: repo, branch: 'site-feature' },
  }));
  assert.equal((switchedBranch.post_status as JsonRecord).branch, 'site-feature');
  const renamedBranch = structured(await server.client.request(11, 'tools/call', {
    name: 'git_branch_rename', arguments: { working_directory: repo, old_name: 'site-feature', new_name: 'site-feature-renamed' },
  }));
  assert.equal((renamedBranch.post_status as JsonRecord).branch, 'site-feature-renamed');
  await server.client.request(12, 'tools/call', { name: 'git_branch_switch', arguments: { working_directory: repo, branch: 'main' } });
  const deletedBranch = structured(await server.client.request(13, 'tools/call', {
    name: 'git_branch_delete', arguments: { working_directory: repo, branch: 'site-feature-renamed', base: 'main' },
  }));
  assert.equal(deletedBranch.merge_check, 'passed');
  const setUpstream = structured(await server.client.request(14, 'tools/call', {
    name: 'git_branch_set_upstream', arguments: { working_directory: repo, local_branch: 'main', remote: 'origin', remote_branch: 'main' },
  }));
  assert.equal((setUpstream.post_status as JsonRecord).upstream, 'origin/main');
  const unsetUpstream = structured(await server.client.request(15, 'tools/call', {
    name: 'git_branch_unset_upstream', arguments: { working_directory: repo, local_branch: 'main' },
  }));
  assert.equal((unsetUpstream.post_status as JsonRecord).upstream, null);

  const remoteMergedBranch = structured(await server.client.request(17, 'tools/call', {
    name: 'git_branch_create', arguments: { working_directory: repo, name: 'site-remote-merged' },
  }));
  assert.equal(remoteMergedBranch.checked_out, false, JSON.stringify(remoteMergedBranch));
  const remoteMergedPush = structured(await server.client.request(18, 'tools/call', {
    name: 'git_push', arguments: { working_directory: repo, remote: 'origin', branch: 'site-remote-merged' },
  }));
  assert.equal(remoteMergedPush.status, 'ok', JSON.stringify(remoteMergedPush));
  const remoteBranchesBeforeDelete = structured(await server.client.request(19, 'tools/call', {
    name: 'git_branch_list', arguments: { working_directory: repo, scope: 'remote' },
  }));
  assert.equal((remoteBranchesBeforeDelete.branches as JsonRecord[]).some((branch) => branch.name === 'origin/site-remote-merged'), true, JSON.stringify(remoteBranchesBeforeDelete));
  const deletedRemoteBranch = structured(await server.client.request(20, 'tools/call', {
    name: 'git_branch_delete_remote', arguments: { working_directory: repo, remote: 'origin', branch: 'site-remote-merged', base: 'main' },
  }));
  assert.equal(deletedRemoteBranch.merge_check, 'passed', JSON.stringify(deletedRemoteBranch));
  const remoteBranchesAfterDelete = structured(await server.client.request(21, 'tools/call', {
    name: 'git_branch_list', arguments: { working_directory: repo, scope: 'remote' },
  }));
  assert.equal((remoteBranchesAfterDelete.branches as JsonRecord[]).some((branch) => branch.name === 'origin/site-remote-merged'), false, JSON.stringify(remoteBranchesAfterDelete));

  const remoteUnmergedBranch = structured(await server.client.request(22, 'tools/call', {
    name: 'git_branch_create', arguments: { working_directory: repo, name: 'site-remote-unmerged' },
  }));
  assert.equal(remoteUnmergedBranch.checked_out, false, JSON.stringify(remoteUnmergedBranch));
  await server.client.request(23, 'tools/call', {
    name: 'git_branch_switch', arguments: { working_directory: repo, branch: 'site-remote-unmerged' },
  });
  writeFileSync(join(repo, 'remote-unmerged.txt'), 'remote unmerged branch\n', 'utf8');
  const remoteUnmergedScope = structured(await server.client.request(32, 'tools/call', {
    name: 'git_begin_work_scope', arguments: { working_directory: repo, owner_id: 'site-fabric-unmerged', allowed_paths: ['remote-unmerged.txt'] },
  }));
  const remoteUnmergedAdded = structured(await server.client.request(24, 'tools/call', {
    name: 'git_add', arguments: { working_directory: repo, paths: ['remote-unmerged.txt'], work_scope_ref: remoteUnmergedScope.work_scope_ref },
  }));
  assert.deepEqual((remoteUnmergedAdded.post_status as JsonRecord).staged, ['remote-unmerged.txt'], JSON.stringify(remoteUnmergedAdded));
  const remoteUnmergedCommit = structured(await server.client.request(25, 'tools/call', {
    name: 'git_commit', arguments: { working_directory: repo, message: 'Site fabric E2E unmerged remote branch', work_scope_ref: remoteUnmergedScope.work_scope_ref },
  }));
  assert.match(String(remoteUnmergedCommit.commit), /^[0-9a-f]{40}$/);
  structured(await server.client.request(33, 'tools/call', {
    name: 'git_end_work_scope', arguments: { working_directory: repo, owner_id: 'site-fabric-unmerged', work_scope_ref: remoteUnmergedScope.work_scope_ref },
  }));
  const remoteUnmergedPush = structured(await server.client.request(26, 'tools/call', {
    name: 'git_push', arguments: { working_directory: repo, remote: 'origin', branch: 'site-remote-unmerged' },
  }));
  assert.equal(remoteUnmergedPush.status, 'ok', JSON.stringify(remoteUnmergedPush));
  await server.client.request(27, 'tools/call', {
    name: 'git_branch_switch', arguments: { working_directory: repo, branch: 'main' },
  });
  const refusedRemoteDelete = await server.client.request(28, 'tools/call', {
    name: 'git_branch_delete_remote', arguments: { working_directory: repo, remote: 'origin', branch: 'site-remote-unmerged', base: 'main' },
  });
  assert.equal((refusedRemoteDelete.error?.data as JsonRecord)?.code, 'git_branch_not_merged', JSON.stringify(refusedRemoteDelete));
  const remoteBranchesAfterRefusal = structured(await server.client.request(29, 'tools/call', {
    name: 'git_branch_list', arguments: { working_directory: repo, scope: 'remote' },
  }));
  assert.equal((remoteBranchesAfterRefusal.branches as JsonRecord[]).some((branch) => branch.name === 'origin/site-remote-unmerged'), true, JSON.stringify(remoteBranchesAfterRefusal));

  const log = structured(await server.client.request(6, 'tools/call', {
    name: 'git_log', arguments: { working_directory: repo, limit: 5 },
  }));
  assert.equal((log.commits as JsonRecord[])[0].subject, 'Site fabric E2E commit');

  const broadScope = structured(await server.client.request(34, 'tools/call', {
    name: 'git_begin_work_scope', arguments: { working_directory: repo, owner_id: 'site-fabric-broad-refusal', allowed_paths: ['README.md'] },
  }));
  const refused = await server.client.request(7, 'tools/call', {
    name: 'git_add', arguments: { working_directory: repo, paths: ['.'], work_scope_ref: broadScope.work_scope_ref },
  });
  assert.equal((refused.error?.data as JsonRecord)?.code, 'git_broad_path_not_allowed', JSON.stringify(refused));
  structured(await server.client.request(35, 'tools/call', {
    name: 'git_end_work_scope', arguments: { working_directory: repo, owner_id: 'site-fabric-broad-refusal', work_scope_ref: broadScope.work_scope_ref },
  }));

  console.log(JSON.stringify({ status: 'passed', test_id: 'git.site-fabric.commit-push-policy', cleanup: 'completed_after_finally' }));
} finally {
  await server.close();
  assert.equal(removeTemporaryE2eRoot(siteRoot), true);
  rmSync(remote, { recursive: true, force: true });
}

console.log('git Site fabric e2e ok');
