import { createHash, randomUUID } from 'node:crypto';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import type { GitMcpState } from './state.js';

export type GitBaseState = {
  head: string | null;
  index_digest: string | null;
  worktree_digest: string | null;
};

export type GitWorkScope = {
  kind: 'work_scope';
  ref: string;
  repository_root: string;
  owner_id: string;
  registry_path: string;
  authority: 'paths' | 'repository_topology';
  allowed_paths: string[];
  base_state: GitBaseState;
  created_at: string;
  expires_at: string;
};

export type GitIndexScope = {
  kind: 'index_scope';
  ref: string;
  repository_root: string;
  work_scope_ref: string | null;
  staged_paths: string[];
  index_digest: string | null;
  base_head: string | null;
  created_at: string;
  expires_at: string;
};

export type GitScopeToken = GitWorkScope | GitIndexScope;

const TOKEN_TTL_MS = 15 * 60 * 1000;

export function scopeTokenMap(state: GitMcpState): Map<string, GitScopeToken> {
  state.scopeTokens ??= new Map<string, GitScopeToken>();
  return state.scopeTokens;
}

export function createWorkScope(args: {
  repositoryRoot: string;
  ownerId: string;
  registryPath: string;
  authority: 'paths' | 'repository_topology';
  allowedPaths: string[];
  baseState: GitBaseState;
}): GitWorkScope {
  return {
    kind: 'work_scope',
    ref: `gws_${randomUUID().replaceAll('-', '').slice(0, 28)}`,
    repository_root: args.repositoryRoot,
    owner_id: args.ownerId,
    registry_path: args.registryPath,
    authority: args.authority,
    allowed_paths: [...new Set(args.allowedPaths)].sort(),
    base_state: args.baseState,
    ...timestamps(),
  };
}
export function createIndexScope(args: {
  repositoryRoot: string;
  workScopeRef: string | null;
  stagedPaths: string[];
  indexDigest: string | null;
  baseHead: string | null;
}): GitIndexScope {
  return {
    kind: 'index_scope',
    ref: `gis_${randomUUID().replaceAll('-', '').slice(0, 28)}`,
    repository_root: args.repositoryRoot,
    work_scope_ref: args.workScopeRef,
    staged_paths: [...new Set(args.stagedPaths)].sort(),
    index_digest: args.indexDigest,
    base_head: args.baseHead,
    ...timestamps(),
  };
}

export function resolveScopeToken(state: GitMcpState, ref: unknown, kind: GitScopeToken['kind']): GitScopeToken {
  const tokenRef = typeof ref === 'string' ? ref.trim() : '';
  const token = tokenRef ? scopeTokenMap(state).get(tokenRef) : undefined;
  if (!token || token.kind !== kind) {
    throw new Error(`git_${kind}_ref_not_found`);
  }
  if (token.kind === 'work_scope' && !existsSync(join(token.registry_path, `${token.ref}.json`))) {
    scopeTokenMap(state).delete(token.ref);
    throw new Error('git_work_scope_ref_released');
  }
  if (Date.parse(token.expires_at) <= Date.now()) {
    scopeTokenMap(state).delete(token.ref);
    throw new Error(`git_${kind}_ref_expired`);
  }
  return token;
}

export function storeScopeToken(state: GitMcpState, token: GitScopeToken): void {
  const tokens = scopeTokenMap(state);
  const now = Date.now();
  for (const [ref, existing] of tokens) {
    if (Date.parse(existing.expires_at) <= now) tokens.delete(ref);
  }
  tokens.set(token.ref, token);
}

export function sha256(value: unknown): string {
  return createHash('sha256').update(JSON.stringify(value)).digest('hex');
}

function timestamps() {
  const createdAt = new Date();
  return {
    created_at: createdAt.toISOString(),
    expires_at: new Date(createdAt.getTime() + TOKEN_TTL_MS).toISOString(),
  };
}
