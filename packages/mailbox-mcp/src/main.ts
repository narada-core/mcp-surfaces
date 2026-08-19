#!/usr/bin/env node
import { buildGuidanceResult } from './guidance.js';
import { guidanceToolDefinition } from './guidance.js';
import { resolve } from 'node:path';
import { buildBoundedToolResult, outputShowAsync } from '@narada-core/mcp-transport';
import { messageMatchesQuery, readMailboxProjection, summarizeMessage } from './mailbox-store.js';
import { MailboxDomainService } from './mailbox-domain.js';

const SERVER_NAME = 'narada-mailbox-mcp';
const SERVER_VERSION = '0.2.0';
const PROTOCOL_VERSION = '2024-11-05';

type MailboxRecord = Record<string, unknown>;
type MailboxServerState = MailboxRecord & {
  siteRoot: string;
  serverName: string;
  domainService: MailboxDomainService;
};

function asRecord(value: unknown): MailboxRecord {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as MailboxRecord : {};
}

if (import.meta.url === `file:///${process.argv[1]?.replace(/\\/g, '/')}`) {
  runStdioServer(parseArgs(process.argv.slice(2))).catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  });
}

export async function runStdioServer(options: unknown): Promise<void> {
  const state = createServerState(options);
  let buffer = '';
  let sawFramedInput = false;
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) {
    buffer += chunk;
    let requests: MailboxRecord[];
    if (buffer.includes('Content-Length:')) {
      sawFramedInput = true;
      const drained = drainJsonRpcFrames(buffer);
      buffer = drained.remaining;
      requests = drained.requests;
    } else {
      const lines = buffer.split(/\r?\n/);
      buffer = lines.pop() ?? '';
      requests = lines.filter((line) => line.trim()).map((line) => asRecord(JSON.parse(line)));
    }
    for (const request of requests) {
      if (!request.id && typeof request.method === 'string' && request.method.startsWith('notifications/')) continue;
      const response = await handleRequest(request, state);
      if (response) writeJsonRpcResponse(response, { framed: sawFramedInput });
    }
  }
}

export function createServerState(options: unknown = {}): MailboxServerState {
  const optionsRecord = asRecord(options);
  const siteRoot = resolve(String(optionsRecord.siteRoot ?? process.cwd()));
  return {
    siteRoot,
    serverName: String(optionsRecord.serverName ?? SERVER_NAME),
    domainService: optionsRecord.domainService instanceof MailboxDomainService
      ? optionsRecord.domainService
      : new MailboxDomainService(siteRoot, {
        ...(typeof optionsRecord.controlPlaneRoot === 'string' && optionsRecord.controlPlaneRoot.trim()
          ? { controlPlaneRoot: resolve(optionsRecord.controlPlaneRoot) }
          : {}),
      }),
  };
}

export async function handleRequest(request: MailboxRecord, state: MailboxServerState) {
  if (!request.id && typeof request.method === 'string' && request.method.startsWith('notifications/')) return null;
  try {
    const result = await dispatchMethod(String(request.method), asRecord(request.params), state);
    return { jsonrpc: '2.0', id: request.id ?? null, result };
  } catch (error) {
    const diagnostic = errorDiagnostic(error);
    return { jsonrpc: '2.0', id: request.id ?? null, error: { code: -32000, message: diagnostic.message, data: diagnostic } };
  }
}

async function dispatchMethod(method: string, params: MailboxRecord, state: MailboxServerState) {
  switch (method) {
    case 'initialize':
      return {
        protocolVersion: params.protocolVersion ?? PROTOCOL_VERSION,
        capabilities: { tools: {}, prompts: {}, completions: {}, logging: {} },
        serverInfo: { name: state.serverName, version: SERVER_VERSION },
      };
    case 'tools/list':
      return { tools: listTools() };
    case 'tools/call':
      return await callTool(params, state);
    case 'prompts/list':
      return { prompts: listPrompts() };
    case 'prompts/get':
      return promptGet(params);
    case 'completion/complete':
      return completeArgument(params);
    case 'logging/setLevel':
      return {};
    default:
      throw new Error(`unsupported_mcp_method: ${method}`);
  }
}

function listPrompts() {
  return [{ name: 'mailbox_read_workflow', title: 'Mailbox Workflow', description: 'Guidance for finite mailbox synchronization, mechanical admission, and bounded projection reads.', arguments: [] }];
}

function promptGet(params: MailboxRecord) {
  const name = String(params.name ?? '');
  if (name !== 'mailbox_read_workflow') throw new Error(`unknown_prompt: ${name}`);
  return {
    description: 'Guidance for finite mailbox synchronization, mechanical admission, and bounded projection reads.',
    messages: [{ role: 'user', content: { type: 'text', text: 'Use mailbox_sync_generation only from a durable SOP action with its injected idempotency key. Consume first-observation outbox events durably, apply mailbox_message_admit to the cited fact, and use mailbox_message_show only for bounded human or agent inspection.' } }],
  };
}

function completeArgument(params: MailboxRecord) {
  const argumentName = String(asRecord(asRecord(params).argument).name ?? '');
  const values = argumentName === 'name' ? listTools().map((tool: any) => tool.name).filter(Boolean).slice(0, 100) : [];
  return { completion: { values, total: values.length, hasMore: false } };
}

export function listTools(): unknown[] {
  return [
    guidanceToolDefinition(),
    tool('mailbox_doctor', 'Inspect site-local synced mailbox projection readiness.', {}),
    tool('mailbox_sync_generation', 'Run one finite, idempotent cloud-to-local projection generation and atomically publish first-observation outbox events.', {
      idempotency_key: { type: 'string', description: 'Stable SOP action occurrence key.' },
      scope_id: { type: 'string', description: 'Configured mailbox scope id. Optional only when the config contains one scope.' },
      config_path: { type: 'string', description: 'Site-relative sync config path. Defaults to config/config.json.' },
    }, ['idempotency_key'], false),
    tool('mailbox_reconcile_first_observations', 'Boundedly repair missing first-observation ledger and outbox entries from immutable local message facts. This never sends, moves, or deletes mail.', {
      idempotency_key: { type: 'string', description: 'Stable SOP action occurrence key.' },
      generation_id: { type: 'string', description: 'Completed mailbox sync generation that authorizes this reconciliation.' },
      scope_id: { type: 'string', description: 'Configured mailbox scope id. Optional only when the config contains one scope.' },
      config_path: { type: 'string', description: 'Site-relative sync config path. Defaults to config/config.json.' },
      limit: { type: 'integer', minimum: 1, maximum: 100, default: 100, description: 'Maximum new observations to publish in this bounded operation.' },
    }, ['idempotency_key', 'generation_id'], false),
    tool('mailbox_message_admit', 'Apply the versioned mechanical admission policy to one durable first-observation fact.', {
      idempotency_key: { type: 'string', description: 'Stable SOP action occurrence key.' },
      fact_id: { type: 'string', description: 'Canonical discovered-message fact id from the mailbox outbox event.' },
      source_event_id: { type: 'string', description: 'Durable mailbox.message.first_observed event that cites this fact.' },
      scope_id: { type: 'string', description: 'Configured mailbox scope id. Optional only when the config contains one scope.' },
      policy_version: { type: 'string', description: 'Optional expected policy fingerprint; mismatches fail closed.' },
      config_path: { type: 'string', description: 'Site-relative sync config path. Defaults to config/config.json.' },
    }, ['idempotency_key', 'fact_id', 'source_event_id'], false),
    tool('mailbox_admission_show', 'Read the frozen canonical admission receipt for one mailbox fact.', {
      scope_id: { type: 'string' },
      fact_id: { type: 'string' },
    }, ['scope_id', 'fact_id']),
    tool('mailbox_fact_show', 'Read one exact immutable discovered-message fact by fact id for revision-stable evidence.', {
      fact_id: { type: 'string', description: 'Immutable fact id cited by a Work Lifecycle ticket source.' },
      scope_id: { type: 'string', description: 'Configured mailbox scope id. Optional only when the config contains one scope.' },
      config_path: { type: 'string', description: 'Site-relative sync config path. Defaults to config/config.json.' },
      include_content: { type: 'boolean', default: false, description: 'Explicitly include the original payload content only when the bounded fact payload is small enough. Defaults to a metadata/body-safe projection.' },
    }, ['fact_id']),
    tool('mailbox_message_fact_find', 'Resolve one synchronized immutable message id to its canonical first-observation fact and event references.', {
      scope_id: { type: 'string', description: 'Configured mailbox scope id.' },
      message_id: { type: 'string', description: 'Immutable Graph message id.' },
    }, ['scope_id', 'message_id']),
    tool('mailbox_generation_show', 'Show bounded metadata and receipts for one synchronization generation.', {
      generation_id: { type: 'string' },
      offset: { type: 'integer', minimum: 0, default: 0 },
      limit: { type: 'integer', minimum: 1, maximum: 100, default: 100 },
    }, ['generation_id']),
    tool('mailbox_outbox_consumer_register', 'Register an immutable scoped and topic-filtered durable consumer.', {
      consumer_id: { type: 'string' },
      scope_id: { type: 'string' },
      topics: { type: 'array', minItems: 1, maxItems: 16, items: { type: 'string' } },
      start_at: { type: 'string', description: 'Inclusive ISO-8601 event watermark.' },
    }, ['consumer_id', 'scope_id', 'topics', 'start_at'], false),
    tool('mailbox_outbox_consumer_show', 'Read one durable mailbox outbox consumer registration for crash-safe bootstrap recovery.', {
      consumer_id: { type: 'string' },
    }, ['consumer_id']),
    tool('mailbox_outbox_list', 'List unacknowledged mailbox domain events for a registered consumer.', {
      consumer_id: { type: 'string' },
      limit: { type: 'integer', minimum: 1, maximum: 100, default: 100 },
    }, ['consumer_id']),
    tool('mailbox_outbox_ack', 'Persist an exact consumer receipt for one mailbox outbox event.', {
      consumer_id: { type: 'string' },
      event_id: { type: 'string' },
      receipt: {
        type: 'object',
        additionalProperties: false,
        required: ['schema', 'outcome', 'effect_ref'],
        properties: {
          schema: { type: 'string' },
          outcome: { type: 'string' },
          effect_ref: { type: 'string' },
        },
      },
    }, ['consumer_id', 'event_id', 'receipt'], false),
    tool('mailbox_accounts_list', 'List synced mailbox accounts discovered in the local projection.', {}),
    tool('mailbox_messages_list', 'List synced mailbox messages with bounded filters.', {
      mailbox_id: { type: 'string', description: 'Optional mailbox/account id filter.' },
      folder: { type: 'string', description: 'Optional folder filter.' },
      unread: { type: 'boolean', description: 'Optional unread filter.' },
      since: { type: 'string', description: 'Optional inclusive received/sent timestamp lower bound.' },
      before: { type: 'string', description: 'Optional exclusive received/sent timestamp upper bound.' },
      query: { type: 'string', description: 'Optional case-insensitive subject/body/address/category substring query.' },
      limit: { type: 'integer', minimum: 1, maximum: 100, default: 20, description: 'Maximum messages. Defaults to 20.' },
      include_body: { type: 'boolean', default: false, description: 'Include plain text body in listed messages.' },
    }),
    tool('mailbox_message_show', 'Show one synced mailbox message by message_id.', {
      message_id: { type: 'string', description: 'Message id from mailbox_messages_list or mailbox_search.' },
      mailbox_id: { type: 'string', description: 'Optional mailbox/account id disambiguator.' },
      include_html: { type: 'boolean', default: false, description: 'Include HTML body when present.' },
      include_raw: { type: 'boolean', default: false, description: 'Include raw synced projection record.' },
    }, ['message_id']),
    tool('mailbox_search', 'Search synced mailbox messages.', {
      query: { type: 'string', description: 'Case-insensitive subject/body/address/category substring query.' },
      mailbox_id: { type: 'string', description: 'Optional mailbox/account id filter.' },
      limit: { type: 'integer', minimum: 1, maximum: 100, default: 20, description: 'Maximum messages. Defaults to 20.' },
      include_body: { type: 'boolean', default: false, description: 'Include plain text body in search results.' },
    }, ['query']),
    tool('mailbox_thread_show', 'Show synced messages in one thread/conversation.', {
      thread_id: { type: 'string', description: 'Thread or conversation id.' },
      mailbox_id: { type: 'string', description: 'Optional mailbox/account id filter.' },
      limit: { type: 'integer', minimum: 1, maximum: 100, default: 50, description: 'Maximum messages. Defaults to 50.' },
      include_body: { type: 'boolean', default: true, description: 'Include plain text bodies.' },
    }, ['thread_id']),
    tool('mailbox_output_show', 'Read a materialized Mailbox MCP output ref with offset/limit paging.', {
      ref: { type: 'string' },
      output_ref: { type: 'string' },
      offset: { type: 'integer', minimum: 0 },
      limit: { type: 'integer', minimum: 0 },
    }),
  ];
}

async function callTool(params: MailboxRecord, state: MailboxServerState) {
  const name = params.name;
  const args = asRecord(params.arguments);
  let result: unknown;
  switch (name) {
    case 'mailbox_guidance':
      result = buildGuidanceResult(args);
      break;
    case 'mailbox_doctor':
      result = mailboxDoctor(state);
      break;
    case 'mailbox_sync_generation':
      result = await state.domainService.syncGeneration(args);
      break;
    case 'mailbox_reconcile_first_observations':
      result = await state.domainService.reconcileFirstObservations(args);
      break;
    case 'mailbox_message_admit':
      result = await state.domainService.admitMessage(args);
      break;
    case 'mailbox_admission_show':
      result = state.domainService.admissionShow(args);
      break;
    case 'mailbox_fact_show':
      result = await state.domainService.factShow(args);
      break;
    case 'mailbox_message_fact_find':
      result = state.domainService.messageFactFind(args);
      break;
    case 'mailbox_generation_show':
      result = state.domainService.generationShow(args);
      break;
    case 'mailbox_outbox_consumer_register':
      result = state.domainService.outboxConsumerRegister(args);
      break;
    case 'mailbox_outbox_consumer_show':
      result = state.domainService.outboxConsumerShow(args);
      break;
    case 'mailbox_outbox_list':
      result = state.domainService.outboxList(args);
      break;
    case 'mailbox_outbox_ack':
      result = state.domainService.outboxAck(args);
      break;
    case 'mailbox_accounts_list':
      result = mailboxAccountsList(state);
      break;
    case 'mailbox_messages_list':
      result = mailboxMessagesList(args, state);
      break;
    case 'mailbox_message_show':
      result = mailboxMessageShow(args, state);
      break;
    case 'mailbox_search':
      result = mailboxSearch(args, state);
      break;
    case 'mailbox_thread_show':
      result = mailboxThreadShow(args, state);
      break;
    case 'mailbox_output_show':
      result = await outputShowAsync({ siteRoot: state.siteRoot, args });
      break;
    default:
      throw new Error(`unknown_tool: ${name}`);
  }
  return buildBoundedToolResult({
    siteRoot: state.siteRoot,
    toolName: String(name ?? 'unknown_tool'),
    value: result,
    limit: 6000,
    readerTool: 'mailbox_output_show',
  });
}

function mailboxDoctor(state: MailboxServerState): MailboxRecord {
  const scan = readMailboxProjection(state.siteRoot);
  return {
    schema: 'narada.mailbox_mcp.doctor.v1',
    status: 'ok',
    site_root: scan.site_root,
    roots: scan.roots,
    scanned_files: scan.scanned_files,
    skipped_non_message_records: scan.skipped_non_message_records,
    message_count: scan.messages.length,
    invalid_count: scan.invalid_count,
    invalid_records: scan.invalid_records,
    server_name: state.serverName,
  };
}

function mailboxAccountsList(state: MailboxServerState): MailboxRecord {
  const scan = readMailboxProjection(state.siteRoot);
  const accounts = new Map<string, MailboxRecord>();
  for (const message of scan.messages) {
    const account = accounts.get(message.mailbox_id) ?? {
      mailbox_id: message.mailbox_id,
      message_count: 0,
      unread_count: 0,
      folders: [],
      latest_message_at: null,
    };
    account.message_count = Number(account.message_count ?? 0) + 1;
    if (message.unread === true) account.unread_count = Number(account.unread_count ?? 0) + 1;
    if (message.folder && !asStringArray(account.folders).includes(message.folder)) account.folders = [...asStringArray(account.folders), message.folder].sort();
    const timestamp = message.received_at ?? message.sent_at;
    if (timestamp && (!account.latest_message_at || String(timestamp) > String(account.latest_message_at))) account.latest_message_at = timestamp;
    accounts.set(message.mailbox_id, account);
  }
  return {
    schema: 'narada.mailbox_mcp.accounts.v1',
    status: 'ok',
    site_root: scan.site_root,
    count: accounts.size,
    accounts: [...accounts.values()].sort((left, right) => String(left.mailbox_id).localeCompare(String(right.mailbox_id))),
  };
}

function mailboxMessagesList(args: MailboxRecord, state: MailboxServerState): MailboxRecord {
  const scan = readMailboxProjection(state.siteRoot);
  const rows = filterMessages(scan.messages, args);
  const limit = boundedLimit(args.limit, 20);
  const includeBody = args.include_body === true;
  return {
    schema: 'narada.mailbox_mcp.messages.v1',
    status: 'ok',
    site_root: scan.site_root,
    filters: {
      mailbox_id: stringOrNull(args.mailbox_id),
      folder: stringOrNull(args.folder),
      unread: typeof args.unread === 'boolean' ? args.unread : null,
      since: stringOrNull(args.since),
      before: stringOrNull(args.before),
      query: stringOrNull(args.query),
    },
    count: rows.length,
    messages: rows.slice(0, limit).map((message) => summarizeMessage(message, { includeBody })),
  };
}

function mailboxMessageShow(args: MailboxRecord, state: MailboxServerState): MailboxRecord {
  const messageId = requiredString(args, 'message_id');
  const mailboxId = stringOrNull(args.mailbox_id);
  const scan = readMailboxProjection(state.siteRoot);
  const message = scan.messages.find((candidate) => candidate.message_id === messageId && (!mailboxId || candidate.mailbox_id === mailboxId));
  if (!message) return { schema: 'narada.mailbox_mcp.message.v1', status: 'not_found', site_root: scan.site_root, message_id: messageId };
  return {
    schema: 'narada.mailbox_mcp.message.v1',
    status: 'ok',
    site_root: scan.site_root,
    message: summarizeMessage(message, { includeBody: true, includeHtml: args.include_html === true, includeRaw: args.include_raw === true }),
  };
}

function mailboxSearch(args: MailboxRecord, state: MailboxServerState): MailboxRecord {
  requiredString(args, 'query');
  return mailboxMessagesList(args, state);
}

function mailboxThreadShow(args: MailboxRecord, state: MailboxServerState): MailboxRecord {
  const threadId = requiredString(args, 'thread_id');
  const mailboxId = stringOrNull(args.mailbox_id);
  const includeBody = args.include_body !== false;
  const limit = boundedLimit(args.limit, 50);
  const scan = readMailboxProjection(state.siteRoot);
  const messages = scan.messages
    .filter((message) => message.thread_id === threadId && (!mailboxId || message.mailbox_id === mailboxId))
    .sort((left, right) => String(left.received_at ?? left.sent_at ?? '').localeCompare(String(right.received_at ?? right.sent_at ?? '')));
  return {
    schema: 'narada.mailbox_mcp.thread.v1',
    status: messages.length > 0 ? 'ok' : 'not_found',
    site_root: scan.site_root,
    thread_id: threadId,
    count: messages.length,
    messages: messages.slice(0, limit).map((message) => summarizeMessage(message, { includeBody })),
  };
}

function filterMessages(messages: ReturnType<typeof readMailboxProjection>['messages'], args: MailboxRecord) {
  const mailboxId = stringOrNull(args.mailbox_id);
  const folder = stringOrNull(args.folder);
  const since = stringOrNull(args.since);
  const before = stringOrNull(args.before);
  return messages.filter((message) => {
    const timestamp = message.received_at ?? message.sent_at ?? '';
    if (mailboxId && message.mailbox_id !== mailboxId) return false;
    if (folder && message.folder !== folder) return false;
    if (typeof args.unread === 'boolean' && message.unread !== args.unread) return false;
    if (since && timestamp < since) return false;
    if (before && timestamp >= before) return false;
    return messageMatchesQuery(message, args.query);
  });
}

function asStringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : [];
}

function boundedLimit(value: unknown, fallback: number): number {
  const parsed = Number(value ?? fallback);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.max(1, Math.min(100, Math.trunc(parsed)));
}

function requiredString(args: unknown, key: string): string {
  const value = asRecord(args)[key];
  if (typeof value !== 'string' || value.trim() === '') throw new Error(`${key}_required`);
  return value;
}

function stringOrNull(value: unknown): string | null {
  return typeof value === 'string' && value.trim() !== '' ? value : null;
}

function tool(name: string, description: string, properties: unknown, required: string[] = [], readOnly = true): unknown {
  return {
    name,
    description,
    annotations: toolAnnotations(name, readOnly),
    inputSchema: {
      type: 'object',
      properties,
      additionalProperties: false,
      ...(required.length > 0 ? { required } : {}),
    },
    outputSchema: { type: 'object', additionalProperties: true },
  };
}

function toolAnnotations(name: string, readOnly = true) {
  return {
    title: name,
    readOnlyHint: readOnly,
    destructiveHint: false,
    idempotentHint: true,
    openWorldHint: false,
  };
}

function parseArgs(argv: string[]): MailboxRecord {
  const parsed: MailboxRecord = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (!arg.startsWith('--')) continue;
    const key = arg.slice(2).replace(/-([a-z])/g, (_, c) => c.toUpperCase());
    const next = argv[i + 1];
    if (next && !next.startsWith('--')) {
      parsed[key] = next;
      i++;
    } else {
      parsed[key] = true;
    }
  }
  return parsed;
}

function drainJsonRpcFrames(buffer: string): { requests: MailboxRecord[]; remaining: string } {
  const requests: MailboxRecord[] = [];
  let rest = buffer;
  while (true) {
    const headerEnd = rest.indexOf('\r\n\r\n');
    const separatorLength = headerEnd >= 0 ? 4 : 2;
    const lfHeaderEnd = headerEnd >= 0 ? headerEnd : rest.indexOf('\n\n');
    if (lfHeaderEnd < 0) break;
    const header = rest.slice(0, lfHeaderEnd);
    const match = /^Content-Length:\s*(\d+)/im.exec(header);
    if (!match) break;
    const length = Number(match[1]);
    const bodyStart = lfHeaderEnd + separatorLength;
    if (rest.length < bodyStart + length) break;
    requests.push(asRecord(JSON.parse(rest.slice(bodyStart, bodyStart + length))));
    rest = rest.slice(bodyStart + length);
  }
  return { requests, remaining: rest };
}

function writeJsonRpcResponse(payload: unknown, options: unknown = {}): void {
  const text = JSON.stringify(payload);
  if (asRecord(options).framed) process.stdout.write(`Content-Length: ${Buffer.byteLength(text, 'utf8')}\r\n\r\n${text}`);
  else process.stdout.write(`${text}\n`);
}

function errorDiagnostic(error: unknown): { schema: string; message: string } {
  return {
    schema: 'narada.mailbox_mcp.error.v1',
    message: error instanceof Error ? error.message : String(error),
  };
}
