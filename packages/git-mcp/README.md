# @narada-core/git-mcp

Structured, policy-gated Git MCP surface.

Use this package when agents need bounded Git inspection or publication operations without arbitrary shell access.

## Boundary

- Allowed: inspect status, diffs, logs, and commits under admitted repository roots.
- Allowed in write mode: inspect branches, fetch one explicit branch from a configured remote, rebase or merge one explicit target, recover or abort those operations, create/switch/rename branches, delete only merged local or remote branches, configure upstreams, stage explicit paths, commit staged changes, and push without force.
- Not allowed: arbitrary Git subcommands.
- Not allowed: shell strings or shell interpolation.
- Not allowed: force push.
- Not allowed: arbitrary refspecs, implicit remote discovery, or synchronization that would silently discard untracked files.
- Not allowed: force branch creation, switching, deletion, or remote deletion.
- Not allowed: path access outside admitted roots.

## Modes

Read mode exposes inspection tools and renders write tools as unavailable by policy.

Write mode admits mutation tools:

- `git_add`
- `git_unstage`
- `git_commit_paths`
- `git_commit`
- `git_push`
- `git_fetch`
- `git_rebase`
- `git_rebase_continue`
- `git_rebase_abort`
- `git_merge`
- `git_merge_continue`
- `git_merge_abort`
- `git_branch_create`
- `git_branch_switch`
- `git_branch_rename`
- `git_branch_delete`
- `git_branch_delete_remote`
- `git_branch_set_upstream`
- `git_branch_unset_upstream`
- `git_workflow_record`

Launch with write mode only for agents that are allowed to publish repository changes.

## Concurrent-agent commits

`git_begin_work_scope` issues either a path lease (`scope_kind=paths`, the default) or one exclusive repository-topology lease (`scope_kind=repository_topology`). `git_add`, `git_unstage`, `git_commit_paths`, and `git_commit` require a live path lease. Direct branch/upstream mutations and rebase/merge initiation require the topology lease; continuation and abort retain the same authority while allowing the expected in-progress state. A topology lease conflicts with every live lease. Registry locks identify the owning process instance, not merely its reusable PID: a matching live instance is never stolen because of age, while a dead or PID-reused owner is reclaimed.

The materialized site projection passes `--output-root {site_root}` to Git. The site’s durable `.narada/allowed-roots.json` is therefore the authority for additional repository and worktree roots; the Git surface does not hardcode a user-specific worktree directory. For example, a site may explicitly admit `C:/Users/andrey/wt` there.

These are cooperative leases, not filesystem or shell sandboxes. Editors and direct Git commands can still bypass them. The topology boundary makes that limitation observable: before mutation it compares leased HEAD, index, and tracked/untracked worktree fingerprints and refuses out-of-band drift. Path commits retain their HEAD compare-and-swap and exact-index checks. Separate worktrees remain the isolation mechanism when all writers cannot cooperate.

`git_commit_paths` is the preferred commit path when multiple agents can touch one worktree. It refuses requested paths already present in the shared index, commits only the validated expanded path set through `GIT_INDEX_FILE`, and uses compare-and-swap ref publication to reject concurrent `HEAD` movement. After publication it reconciles only committed paths while holding Git's real `index.lock`, preserving the latest unrelated staged entries. If the lock is held, the commit remains published and returns a durable `reconciliation_ref`; call `git_reconcile_index` with that reference and the original live work scope to retry exactly the pending reconciliation.

## Branch lifecycle

Start with `git_branch_list`, then acquire `git_begin_work_scope` with `scope_kind=repository_topology` before local/remote branch or upstream mutation. Release it with `git_end_work_scope` after the operation. These operations retain their explicit branch, remote, base, and merged-only guards in addition to exclusive topology authority. `git_branch_create` creates a local branch from `HEAD` or an explicit start point but does not check it out or publish it; use `git_branch_switch` separately, then use `git_push` to publish it. Use `git_branch_set_upstream` or `git_branch_unset_upstream` to make tracking explicit. Local and remote deletion require an explicit merged-only base check, and force deletion is unavailable.

## Remote synchronization

Use `git_status`, then `git_fetch` with an explicit configured remote and branch. Acquire an exclusive repository-topology scope before `git_rebase` or `git_merge`; pass the same scope to continuation or abort, then release it. Use `git_sync_status` when an operation is in progress. Tracked dirty files require `autostash: true`; untracked files are refused because Git autostash does not preserve them. Resolve and stage every conflict path before using the matching continue tool, or use the matching abort tool. Arbitrary refspecs, force push, and implicit remote discovery are unavailable.

## Tools

Read tools:

- `git_policy_inspect`: inspect active Git MCP policy.
- `git_status`: branch, upstream, remotes, push readiness, and working tree status.
- `git_sync_status`: whether a rebase or merge is in progress, conflict paths, and recovery actions.
- `git_branch_list`: list local and remote branches with current/upstream metadata.
- `git_worktree_list`: list registered worktrees, including branch, HEAD, lock, and prune metadata.
- `git_diff`: paged working, staged, or commit diff. Pass `offset`, `limit`, and the returned `next_offset` to continue reading. Pass `include_untracked: true` with `scope: "working"` to append bounded untracked-file patches.
- `git_log`: recent commits, optionally scoped by pathspec.
- `git_show`: one commit with metadata and optional patch.
- `git_changed_summary`: compact dirty-tree summary. `pathspec`/`pathspecs` scope primary changed-path counts and arrays while preserving whole-repository counts separately; `expected_paths` classify dirty paths as task-relevant, unrelated, or unknown for commit planning.
- `git_repositories_summary`: multi-repository status and push-readiness summary for handoffs.
- `git_workflow_record`: durable record for completed multi-repository stage/commit/push workflows.

Write-mode tools:

- `git_begin_work_scope` / `git_end_work_scope`: acquire and release the durable path authority required by path-mutating tools.
- `git_add`: stage explicit file paths within the supplied work scope.
- `git_unstage`: remove explicit file paths from the index without changing the working tree.
- `git_commit_paths`: preferred concurrent-agent commit path. It validates explicit paths, builds the commit through a dedicated temporary index, atomically advances the current branch only if `HEAD` is unchanged, and preserves unrelated shared-index entries. A post-commit index-reconciliation failure is returned as `committed_shared_index_reconciliation_required`, never as an ambiguous pre-commit failure.
- `git_reconcile_index`: retry one durable pending shared-index reconciliation after a successful `git_commit_paths` publication.
- `git_commit`: legacy staged-index commit. Use only when an explicitly reviewed shared-index workflow is required.
- `git_push`: push current branch or explicit remote/branch; force push is not supported.
- `git_fetch`: fetch one explicit branch from one configured remote; tags and arbitrary refspecs are not accepted.
- `git_rebase`: rebase onto one explicit target with dirty-worktree guards and structured conflict results.
- `git_rebase_continue` / `git_rebase_abort`: continue after resolving and staging conflicts, or abort safely.
- `git_merge`: merge one explicit target with dirty-worktree guards and structured conflict results.
- `git_merge_continue` / `git_merge_abort`: complete after resolving and staging conflicts, or abort safely.
- `git_branch_create`: create a local branch from `HEAD` or an explicit start point without checking it out.
- `git_worktree_add`: create a worktree at an explicit path under an allowed root; exactly one existing or new branch mode is required.
- `git_worktree_remove`: remove only an explicitly registered clean worktree; force removal is unavailable.
- `git_worktree_prune`: prune stale administrative records without deleting live worktrees.
- `git_branch_switch`: switch to an existing local branch without discard or force behavior.
- `git_branch_rename`: rename an existing local branch.
- `git_branch_delete`: delete a local branch only after a merged-only base check.
- `git_branch_delete_remote`: delete a remote branch only after an explicit merged-only base check.
- `git_branch_set_upstream` / `git_branch_unset_upstream`: manage local upstream configuration for configured remotes.
- `git_workflow_record`: record the final status of a multi-repository publication workflow.

## Large Output

Git tools return bounded output directly in their own result payloads. For `git_diff`, use `next_offset` from the result to fetch the next page. Request narrower pathspecs, lower limits, or `include_patch: false` when a result would be too large.

## Run

```powershell
pnpm --filter @narada-core/git-mcp build
node packages/git-mcp/dist/src/main.js --allowed-root <src-root>/example --mode read
```

Use write mode only when mutation is intended:

```powershell
node packages/git-mcp/dist/src/main.js --allowed-root <src-root>/example --mode write
```

## Verification

```powershell
pnpm --filter @narada-core/git-mcp test
```