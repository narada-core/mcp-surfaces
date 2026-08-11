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

Prefer `git_commit_paths` when multiple agents can touch one worktree. The tool refuses requested paths already present in the shared index, commits only the validated expanded path set through `GIT_INDEX_FILE`, and uses compare-and-swap ref publication to reject concurrent `HEAD` movement. It does not make concurrent working-tree edits safe; agents must still avoid editing the same file or use separate worktrees for overlapping changes.

## Branch lifecycle

Start with `git_branch_list` to inspect local and remote names, current state, and upstream metadata. `git_branch_create` creates a local branch from `HEAD` or an explicit start point but does not check it out or publish it; use `git_branch_switch` separately, then use `git_push` to publish it. Use `git_branch_set_upstream` or `git_branch_unset_upstream` to make tracking explicit. Local and remote deletion require an explicit merged-only base check, and force deletion is unavailable.

## Remote synchronization

Use `git_status`, then `git_fetch` with an explicit configured remote and branch. Use `git_rebase` or `git_merge` with one explicit target, followed by `git_sync_status` when an operation is in progress. Tracked dirty files require `autostash: true`; untracked files are refused because Git autostash does not preserve them. Resolve and stage every conflict path before using the matching continue tool, or use the matching abort tool. Arbitrary refspecs, force push, and implicit remote discovery are unavailable.

## Tools

Read tools:

- `git_policy_inspect`: inspect active Git MCP policy.
- `git_status`: branch, upstream, remotes, push readiness, and working tree status.
- `git_sync_status`: whether a rebase or merge is in progress, conflict paths, and recovery actions.
- `git_branch_list`: list local and remote branches with current/upstream metadata.
- `git_diff`: paged working, staged, or commit diff. Pass `offset`, `limit`, and the returned `next_offset` to continue reading. Pass `include_untracked: true` with `scope: "working"` to append bounded untracked-file patches.
- `git_log`: recent commits, optionally scoped by pathspec.
- `git_show`: one commit with metadata and optional patch.
- `git_changed_summary`: compact dirty-tree summary. `pathspec`/`pathspecs` scope primary changed-path counts and arrays while preserving whole-repository counts separately; `expected_paths` classify dirty paths as task-relevant, unrelated, or unknown for commit planning.
- `git_repositories_summary`: multi-repository status and push-readiness summary for handoffs.
- `git_workflow_record`: durable record for completed multi-repository stage/commit/push workflows.

Write-mode tools:

- `git_add`: stage explicit file paths.
- `git_unstage`: remove explicit file paths from the index without changing the working tree.
- `git_commit_paths`: preferred concurrent-agent commit path. It validates explicit paths, builds the commit through a dedicated temporary index, atomically advances the current branch only if `HEAD` is unchanged, and preserves unrelated shared-index entries. A post-commit index-reconciliation failure is returned as `committed_shared_index_reconciliation_required`, never as an ambiguous pre-commit failure.
- `git_commit`: legacy staged-index commit. Use only when an explicitly reviewed shared-index workflow is required.
- `git_push`: push current branch or explicit remote/branch; force push is not supported.
- `git_fetch`: fetch one explicit branch from one configured remote; tags and arbitrary refspecs are not accepted.
- `git_rebase`: rebase onto one explicit target with dirty-worktree guards and structured conflict results.
- `git_rebase_continue` / `git_rebase_abort`: continue after resolving and staging conflicts, or abort safely.
- `git_merge`: merge one explicit target with dirty-worktree guards and structured conflict results.
- `git_merge_continue` / `git_merge_abort`: complete after resolving and staging conflicts, or abort safely.
- `git_branch_create`: create a local branch from `HEAD` or an explicit start point without checking it out.
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