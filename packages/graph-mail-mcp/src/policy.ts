import { appendFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

const CONFIG_PATH = '.ai/graph-mail-mcp.json';
const AUDIT_PATH = '.ai/audit/graph-mail-mcp.jsonl';
const DEFAULT_GRAPH_BASE_URL = 'https://graph.microsoft.com/v1.0';

type GraphMailRecord = Record<string, unknown>;

function auditIdentityState(event: GraphMailRecord): GraphMailRecord {
  if (event.identity_state && typeof event.identity_state === 'object') return event;
  const claimed = typeof event.claimed_identity === 'string' && event.claimed_identity.trim()
    ? event.claimed_identity.trim()
    : typeof process.env.NARADA_CLAIMED_IDENTITY === 'string' && process.env.NARADA_CLAIMED_IDENTITY.trim()
      ? process.env.NARADA_CLAIMED_IDENTITY.trim()
      : typeof process.env.NARADA_AGENT_ID === 'string' && process.env.NARADA_AGENT_ID.trim()
        ? process.env.NARADA_AGENT_ID.trim()
        : null;
  const authenticated = typeof process.env.NARADA_AUTHENTICATED_IDENTITY === 'string' && process.env.NARADA_AUTHENTICATED_IDENTITY.trim()
    ? process.env.NARADA_AUTHENTICATED_IDENTITY.trim()
    : null;
  const authority = event.authority && typeof event.authority === 'object'
    ? event.authority
    : { status: 'not_evaluated', operation: null, granted: false, evidence_refs: [] };
  return {
    ...event,
    identity_state: {
      schema: 'narada.agent.identity_state.v1',
      claimed_identity: {
        identity: claimed,
        status: claimed ? 'claimed' : 'unclaimed',
        source: claimed ? (event.claimed_identity ? 'caller_assertion' : 'carrier_environment') : null,
        asserted_at: claimed ? new Date().toISOString() : null,
        evidence_refs: [],
        authority_granted: false,
      },
      authentication: {
        status: authenticated ? 'authenticated' : 'missing',
        authenticated_identity: authenticated,
        evidence_refs: [],
      },
      authority,
    },
  };
}

export type GraphMailPolicy = {
  site_root: string;
  graph_base_url: string;
  allowed_mailboxes: string[];
  allowed_attachment_roots: string[];
  allow_device_code_auth: boolean;
  device_code_tenant_id: string | null;
  device_code_client_id: string | null;
  device_code_allowed_scopes: string[];
  allow_send_draft: boolean;
  send_approval_token: string | null;
  allow_folder_create: boolean;
  allow_message_move: boolean;
  allow_message_mark_read: boolean;
  reply_signature_name: string | null;
  mailbox_organization_approval_token: string | null;
};

function asRecord(value: unknown): GraphMailRecord {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as GraphMailRecord : {};
}

export function decideMessageMarkRead(
  policy: GraphMailPolicy,
  args: GraphMailRecord,
): { status: 'allowed' | 'refused'; reason?: string } {
  if (!policy.allow_message_mark_read) return { status: 'refused', reason: 'message_mark_read_disallowed_by_policy' };
  if (args.confirm_write !== true && args.confirmWrite !== true) return { status: 'refused', reason: 'confirm_write_required' };
  return { status: 'allowed' };
}

function asStringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string' && item.trim() !== '') : [];
}

export function loadGraphMailPolicy(siteRootInput: string): GraphMailPolicy {
  const siteRoot = resolve(siteRootInput);
  const path = join(siteRoot, CONFIG_PATH);
  const config = existsSync(path) ? asRecord(JSON.parse(readFileSync(path, 'utf8'))) : {};
  return {
    site_root: siteRoot,
    graph_base_url: typeof config.graph_base_url === 'string' && config.graph_base_url.trim() !== ''
      ? config.graph_base_url.replace(/\/+$/, '')
      : DEFAULT_GRAPH_BASE_URL,
    allowed_mailboxes: asStringArray(config.allowed_mailboxes ?? config.allowedMailboxes),
    allowed_attachment_roots: asStringArray(config.allowed_attachment_roots ?? config.allowedAttachmentRoots).map((root) => resolve(siteRoot, root)),
    allow_device_code_auth: config.allow_device_code_auth === true || config.allowDeviceCodeAuth === true,
    device_code_tenant_id: typeof config.device_code_tenant_id === 'string'
      ? config.device_code_tenant_id
      : typeof config.deviceCodeTenantId === 'string'
        ? config.deviceCodeTenantId
        : null,
    device_code_client_id: typeof config.device_code_client_id === 'string'
      ? config.device_code_client_id
      : typeof config.deviceCodeClientId === 'string'
        ? config.deviceCodeClientId
        : null,
    device_code_allowed_scopes: asStringArray(config.device_code_allowed_scopes ?? config.deviceCodeAllowedScopes),
    allow_send_draft: config.allow_send_draft === true || config.allowSendDraft === true,
    send_approval_token: typeof config.send_approval_token === 'string'
      ? config.send_approval_token
      : typeof config.sendApprovalToken === 'string'
        ? config.sendApprovalToken
        : null,
    allow_folder_create: config.allow_folder_create === true || config.allowFolderCreate === true,
    allow_message_move: config.allow_message_move === true || config.allowMessageMove === true,
    allow_message_mark_read: config.allow_message_mark_read === true || config.allowMessageMarkRead === true,
    reply_signature_name: typeof config.reply_signature_name === 'string' && config.reply_signature_name.trim() !== ''
      ? config.reply_signature_name.trim()
      : typeof config.replySignatureName === 'string' && config.replySignatureName.trim() !== ''
        ? config.replySignatureName.trim()
        : null,
    mailbox_organization_approval_token: typeof config.mailbox_organization_approval_token === 'string'
      ? config.mailbox_organization_approval_token
      : typeof config.mailboxOrganizationApprovalToken === 'string'
        ? config.mailboxOrganizationApprovalToken
        : null,
  };
}

export function assertMailboxAllowed(policy: GraphMailPolicy, mailboxId: string): void {
  if (policy.allowed_mailboxes.length === 0) return;
  if (!policy.allowed_mailboxes.includes(mailboxId)) {
    throw new Error(`mailbox_not_allowed: ${mailboxId}`);
  }
}

export function decideDraftSend(policy: GraphMailPolicy, args: GraphMailRecord): { status: 'allowed' | 'refused'; reason?: string } {
  if (!policy.allow_send_draft) return { status: 'refused', reason: 'send_draft_disallowed_by_policy' };
  if (args.confirm_send !== true && args.confirmSend !== true) return { status: 'refused', reason: 'confirm_send_required' };
  if (policy.send_approval_token && args.approval_token !== policy.send_approval_token) {
    return { status: 'refused', reason: 'send_approval_token_required' };
  }
  return { status: 'allowed' };
}

export function decideMailboxOrganizationWrite(
  policy: GraphMailPolicy,
  args: GraphMailRecord,
  operation: 'folder_create' | 'message_move',
): { status: 'allowed' | 'refused'; reason?: string } {
  if (operation === 'folder_create' && !policy.allow_folder_create) return { status: 'refused', reason: 'folder_create_disallowed_by_policy' };
  if (operation === 'message_move' && !policy.allow_message_move) return { status: 'refused', reason: 'message_move_disallowed_by_policy' };
  if (args.confirm_write !== true && args.confirmWrite !== true) return { status: 'refused', reason: 'confirm_write_required' };
  if (policy.mailbox_organization_approval_token && args.approval_token !== policy.mailbox_organization_approval_token) {
    return { status: 'refused', reason: 'mailbox_organization_approval_token_required' };
  }
  return { status: 'allowed' };
}

export function recordGraphMailAudit(siteRootInput: string, event: GraphMailRecord): void {
  const siteRoot = resolve(siteRootInput);
  const auditPath = join(siteRoot, AUDIT_PATH);
  mkdirSync(dirname(auditPath), { recursive: true });
  appendFileSync(auditPath, `${JSON.stringify({
    schema: 'narada.graph_mail_mcp.audit.v1',
    recorded_at: new Date().toISOString(),
    ...auditIdentityState(event),
  })}\n`);
}
