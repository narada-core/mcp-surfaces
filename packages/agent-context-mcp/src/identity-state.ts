export type IdentityClaimSource =
  | 'caller_assertion'
  | 'carrier_environment'
  | 'carrier_session_admission_receipt'
  | 'durable_record';

export type AuthenticationStatus = 'authenticated' | 'missing' | 'failed';
export type AuthorityStatus = 'authorized' | 'denied' | 'not_evaluated';

export interface ClaimedIdentityState {
  identity: string | null;
  status: 'claimed' | 'unclaimed';
  source: IdentityClaimSource | null;
  asserted_at: string | null;
  evidence_refs: string[];
  authority_granted: false;
}

export interface AuthenticationState {
  status: AuthenticationStatus;
  authenticated_identity: string | null;
  evidence_refs: string[];
}

export interface AuthorityState {
  status: AuthorityStatus;
  operation: string | null;
  granted: boolean;
  evidence_refs: string[];
}

export interface IdentityState {
  schema: 'narada.agent.identity_state.v1';
  claimed_identity: ClaimedIdentityState;
  authentication: AuthenticationState;
  authority: AuthorityState;
}

export interface IdentityStateInput {
  claimed_identity?: unknown;
  claimedIdentity?: unknown;
  claimed_identity_source?: IdentityClaimSource;
  claimed_identity_evidence_refs?: unknown;
  authenticated_identity?: string | null;
  authentication_evidence_refs?: unknown;
  authentication_failed?: boolean;
  authority?: Partial<AuthorityState> | null;
  now?: string | null;
}

function nonEmptyString(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value.trim() : null;
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === 'string' && item.trim() !== '').map((item) => item.trim())
    : [];
}

/**
 * Normalize a claim without treating it as authentication or authority.
 * A claim is intentionally useful even when no carrier authentication exists.
 */
export function normalizeClaimedIdentity(
  value: unknown,
  options: {
    source?: IdentityClaimSource;
    evidence_refs?: unknown;
    asserted_at?: string | null;
  } = {},
): ClaimedIdentityState {
  const input = typeof value === 'string'
    ? { identity: value }
    : value && typeof value === 'object' && !Array.isArray(value)
      ? value as Record<string, unknown>
      : null;
  const identity = nonEmptyString(input?.identity ?? input?.value ?? input?.claimed_identity);
  const source = options.source
    ?? (nonEmptyString(input?.source) as IdentityClaimSource | null)
    ?? (identity ? 'caller_assertion' : null);
  const evidenceRefs = stringArray(options.evidence_refs ?? input?.evidence_refs);
  const assertedAt = options.asserted_at ?? nonEmptyString(input?.asserted_at);
  return {
    identity,
    status: identity ? 'claimed' : 'unclaimed',
    source: identity ? source : null,
    asserted_at: identity ? assertedAt ?? null : null,
    evidence_refs: evidenceRefs,
    authority_granted: false,
  };
}

/** Build the independent identity/authentication/authority state carried by records. */
export function buildIdentityState(input: IdentityStateInput = {}): IdentityState {
  const claimed = normalizeClaimedIdentity(
    input.claimed_identity ?? input.claimedIdentity ?? null,
    {
      source: input.claimed_identity_source,
      evidence_refs: input.claimed_identity_evidence_refs,
      asserted_at: input.now ?? null,
    },
  );
  const authenticatedIdentity = nonEmptyString(input.authenticated_identity);
  const authentication: AuthenticationState = {
    status: input.authentication_failed
      ? 'failed'
      : authenticatedIdentity
        ? 'authenticated'
        : 'missing',
    authenticated_identity: authenticatedIdentity,
    evidence_refs: stringArray(input.authentication_evidence_refs),
  };
  const requestedAuthority = input.authority ?? {};
  const authorityStatus = requestedAuthority.status ?? 'not_evaluated';
  const granted = authorityStatus === 'authorized' && requestedAuthority.granted === true;
  const normalizedAuthorityStatus: AuthorityStatus = authorityStatus === 'denied'
    ? 'denied'
    : authorityStatus === 'authorized'
      ? (granted ? 'authorized' : 'denied')
      : 'not_evaluated';
  return {
    schema: 'narada.agent.identity_state.v1',
    claimed_identity: claimed,
    authentication,
    authority: {
      status: normalizedAuthorityStatus,
      operation: nonEmptyString(requestedAuthority.operation),
      granted,
      evidence_refs: stringArray(requestedAuthority.evidence_refs),
    },
  };
}

export function identityStateFromEnvironment({
  claimedIdentity = null,
  authenticatedIdentity = null,
  authenticationEvidenceRefs = [],
  authority = null,
  now = null,
}: {
  claimedIdentity?: unknown;
  authenticatedIdentity?: string | null;
  authenticationEvidenceRefs?: unknown;
  authority?: Partial<AuthorityState> | null;
  now?: string | null;
} = {}): IdentityState {
  const envClaim = process.env.NARADA_CLAIMED_IDENTITY ?? process.env.NARADA_AGENT_ID ?? null;
  return buildIdentityState({
    claimed_identity: claimedIdentity ?? envClaim,
    claimed_identity_source: claimedIdentity == null && envClaim
      ? 'carrier_environment'
      : undefined,
    authenticated_identity: authenticatedIdentity,
    authentication_evidence_refs: authenticationEvidenceRefs,
    authority,
    now,
  });
}

export function assertClaimMatchesAuthenticatedIdentity(state: IdentityState): void {
  const claimed = state.claimed_identity.identity;
  const authenticated = state.authentication.authenticated_identity;
  if (claimed && authenticated && claimed !== authenticated) {
    throw new Error('agent_context_claimed_identity_mismatch');
  }
}
