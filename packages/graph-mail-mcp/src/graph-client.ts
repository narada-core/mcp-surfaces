import { assertMailboxAllowed, GraphMailPolicy } from './policy.js';

type GraphMailRecord = Record<string, unknown>;
type FetchLike = (input: string, init?: GraphMailRecord) => Promise<{
  ok?: boolean;
  status: number;
  statusText?: string;
  text?: () => Promise<string>;
  json?: () => Promise<unknown>;
}>;

export type GraphMailClientOptions = {
  policy: GraphMailPolicy;
  accessToken: string | null;
  fetchImpl: FetchLike;
};

export type GraphMailRequest = {
  method?: string;
  mailbox_id?: string;
  path: string;
  query?: Record<string, string | number | boolean | null | undefined>;
  body?: unknown;
  headers?: Record<string, string>;
};

function trimSlashes(value: string): string {
  return value.replace(/^\/+|\/+$/g, '');
}

function mailboxPrefix(mailboxId: string): string {
  return mailboxId === 'me' ? '/me' : `/users/${encodeURIComponent(mailboxId)}`;
}

export function graphMailboxPath(mailboxIdInput: unknown, suffix: string, policy: GraphMailPolicy): string {
  const mailboxId = typeof mailboxIdInput === 'string' && mailboxIdInput.trim() !== ''
    ? mailboxIdInput
    : policy.allowed_mailboxes.length === 1
      ? policy.allowed_mailboxes[0]
      : 'me';
  assertMailboxAllowed(policy, mailboxId);
  return `${mailboxPrefix(mailboxId)}/${trimSlashes(suffix)}`;
}

export function buildGraphUrl(policy: GraphMailPolicy, path: string, query: GraphMailRequest['query'] = {}): string {
  const url = new URL(`${policy.graph_base_url}/${trimSlashes(path)}`);
  for (const [key, value] of Object.entries(query ?? {})) {
    if (value === undefined || value === null || value === '') continue;
    url.searchParams.set(key, String(value));
  }
  return url.toString();
}

export async function graphRequest(options: GraphMailClientOptions, request: GraphMailRequest): Promise<unknown> {
  if (!options.accessToken) throw new Error('graph_access_token_missing');
  const method = String(request.method ?? 'GET').toUpperCase();
  const url = buildGraphUrl(options.policy, request.path, request.query);
  const headers: Record<string, string> = {
    Authorization: `Bearer ${options.accessToken}`,
    Accept: 'application/json',
    ...request.headers,
  };
  const init: GraphMailRecord = { method, headers };
  if (request.body !== undefined) {
    headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(request.body);
  }
  const response = await options.fetchImpl(url, init);
  const text = typeof response.text === 'function' ? await response.text() : '';
  if (!response.ok && response.status < 200 || response.status >= 300) {
    throw new Error(`graph_request_failed:${response.status}:${text || response.statusText || 'unknown_error'}`);
  }
  if (response.status === 202 || response.status === 204 || text.trim() === '') {
    return { status: 'accepted', http_status: response.status };
  }
  try {
    return JSON.parse(text);
  } catch {
    return { status: 'ok', text };
  }
}

export function graphTop(value: unknown, fallback = 20): number {
  const parsed = Number(value ?? fallback);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.max(1, Math.min(100, Math.trunc(parsed)));
}

export function requiredString(args: unknown, key: string): string {
  const record = args && typeof args === 'object' && !Array.isArray(args) ? args as GraphMailRecord : {};
  const value = record[key];
  if (typeof value !== 'string' || value.trim() === '') throw new Error(`${key}_required`);
  return value;
}

export function recipients(value: unknown): unknown[] {
  if (!Array.isArray(value)) return [];
  return value.map((item) => {
    if (typeof item === 'string') return { emailAddress: { address: item } };
    return item;
  });
}

export function messagePatchFromArgs(args: GraphMailRecord): GraphMailRecord {
  const patch: GraphMailRecord = {};
  if (typeof args.subject === 'string') patch.subject = args.subject;
  if (typeof args.body_text === 'string') patch.body = { contentType: 'Text', content: args.body_text };
  if (typeof args.body_html === 'string') patch.body = { contentType: 'HTML', content: args.body_html };
  if (Array.isArray(args.to_recipients)) patch.toRecipients = recipients(args.to_recipients);
  if (Array.isArray(args.cc_recipients)) patch.ccRecipients = recipients(args.cc_recipients);
  if (Array.isArray(args.bcc_recipients)) patch.bccRecipients = recipients(args.bcc_recipients);
  if (typeof args.importance === 'string') patch.importance = args.importance;
  return patch;
}

const ATTACHMENT_UPLOAD_HOST_ALLOWLIST = new Set(['outlook.office.com', 'outlook.office365.com', 'graph.microsoft.com']);

export function assertAttachmentUploadUrlAllowed(uploadUrl: string): URL {
  let url: URL;
  try {
    url = new URL(uploadUrl);
  } catch {
    throw new Error('attachment_upload_url_invalid');
  }
  if (process.env.NARADA_GRAPH_MAIL_ALLOW_INSECURE_TEST === '1' && url.protocol === 'http:' && url.hostname === '127.0.0.1') {
    return url;
  }
  if (url.protocol !== 'https:') {
    throw new Error('attachment_upload_url_must_be_https');
  }
  if (!ATTACHMENT_UPLOAD_HOST_ALLOWLIST.has(url.hostname.toLowerCase())) {
    throw new Error(`attachment_upload_url_host_not_allowed: ${url.hostname}`);
  }
  return url;
}
