import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServerState, handleRequest } from '../src/main.js';
import { sha256Canonical, TicketDraftOperationStore } from '../src/ticket-draft-store.js';

type DynamicTestValue = any & {
  [key: string]: DynamicTestValue;
  [index: number]: DynamicTestValue;
};

type JsonRpcTestResponse = {
  error: DynamicTestValue;
  result: DynamicTestValue;
};

type CapturedRequest = { url: string; init: DynamicTestValue };
type MockResponse = { body?: unknown; ok?: boolean; status?: number; statusText?: string; text?: string };

const rpc = handleRequest as unknown as (...args: Parameters<typeof handleRequest>) => Promise<JsonRpcTestResponse>;

function mockFetch(calls: CapturedRequest[], responses: MockResponse[] = []) {
  return async (url: string, init: DynamicTestValue = {}) => {
    calls.push({ url, init });
    const response = responses.shift() ?? {};
    const status = response.status ?? 200;
    const ok = response.ok ?? (status >= 200 && status < 300);
    const text = response.text ?? JSON.stringify(response.body ?? {});
    return {
      status,
      ok,
      statusText: response.statusText ?? 'OK',
      text: async () => text,
    };
  };
}

const root = mkdtempSync(join(tmpdir(), 'graph-mail-mcp-'));

try {
  {
    const fairStore = new TicketDraftOperationStore(join(root, 'disposition-fairness'));
    try {
      for (const suffix of ['oldest', 'newest']) {
        const now = suffix === 'oldest' ? '2026-08-01T00:00:00.000Z' : '2026-08-02T00:00:00.000Z';
        const operationKey = `draft_operation_fairness_${suffix}`;
        fairStore.beginImmediate();
        fairStore.insertPending({
          operation_key: operationKey,
          action_idempotency_key: `action-${suffix}`,
          request_digest: `request-${suffix}`,
          draft_request_digest: `draft-request-${suffix}`,
          ticket_id: `ticket-${suffix}`,
          effect_claim_id: `claim-${suffix}`,
          mailbox_id: 'support@example.test',
          source_message_id: `source-${suffix}`,
          reply_mode: 'reply',
          now,
        });
        fairStore.complete(operationKey, {
          draft_id: `draft-${suffix}`,
          receipt_id: `receipt-${suffix}`,
          draft_ref: { draft_id: `draft-${suffix}` },
          now,
        });
        fairStore.commit();
      }
      const firstCandidate = fairStore.listDispositionScanCandidates(1)[0];
      assert.equal(firstCandidate.operation_key, 'draft_operation_fairness_oldest');
      fairStore.markDispositionScanned(firstCandidate.operation_key, '2026-08-03T00:00:00.000Z');
      const secondCandidate = fairStore.listDispositionScanCandidates(1)[0];
      assert.equal(secondCandidate.operation_key, 'draft_operation_fairness_newest');
    } finally {
      fairStore.close();
    }
  }

  mkdirSync(join(root, '.ai'), { recursive: true });
  writeFileSync(join(root, '.ai', 'graph-mail-mcp.json'), JSON.stringify({
    graph_base_url: 'https://graph.example.test/v1.0',
    allowed_mailboxes: ['support@example.test'],
  }));

  const attachmentCalls: CapturedRequest[] = [];
  const attachmentState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(attachmentCalls, [
      { body: { value: [{ id: 'att-list-1' }] } },
      { body: { id: 'att-get-1', name: 'report.pdf', contentBytes: 'YWJj', content_base64: 'YWJj', content: 'legacy-content', data: 'legacy-data', bytes: 'legacy-bytes', raw: 'legacy-raw' } },
      { body: { id: 'att-added-1', name: 'report.pdf' } },
      { body: { id: 'upload-session-1', uploadUrl: 'https://outlook.office.com/upload/abc', expirationDateTime: '2026-06-08T20:00:00Z' } },
      { body: { id: 'upload-session-2', uploadUrl: 'https://outlook.office365.com/upload/file-abc', expirationDateTime: '2026-06-08T20:00:00Z' } },
      { status: 202, text: '' },
      { status: 201, body: { id: 'att-uploaded-1', name: 'local.bin' } },
      { status: 204, text: '' },
    ]),
  });

  const doctor = await rpc({
    jsonrpc: '2.0',
    id: 1,
    method: 'tools/call',
    params: { name: 'graph_mail_doctor', arguments: {} },
  }, attachmentState);
  assert.equal(doctor.error, undefined);
  assert.equal(doctor.result.structuredContent.has_access_token, true);
  assert.equal(doctor.result.structuredContent.auth_mode, 'access_token');
  assert.equal(doctor.result.structuredContent.allow_device_code_auth, false);
  assert.equal(doctor.result.structuredContent.device_code_tenant_configured, false);
  assert.equal(doctor.result.structuredContent.device_code_client_configured, false);
  assert.deepEqual(doctor.result.structuredContent.device_code_allowed_scopes, []);
  assert.equal(doctor.result.structuredContent.delegated_token.status, 'missing');
  assert.equal(doctor.result.structuredContent.allow_send_draft, false);
  assert.equal(doctor.result.structuredContent.allow_folder_create, false);
  assert.equal(doctor.result.structuredContent.allow_message_move, false);
  assert.equal(doctor.result.structuredContent.allow_message_mark_read, false);
  assert.equal(doctor.result.structuredContent.mailbox_organization_approval_token_configured, false);
  assert.deepEqual(doctor.result.structuredContent.allowed_attachment_roots, [root]);

  const tools = await rpc({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} }, attachmentState);
  assert.equal(tools.error, undefined);
  const toolRows = tools.result.tools;
  assert.deepEqual(toolRows.map((tool: DynamicTestValue) => tool.name), [
    'graph_mail_guidance',
    'graph_mail_doctor',
    'graph_mail_auth_device_code_start',
    'graph_mail_auth_device_code_poll',
    'graph_mail_auth_status',
    'graph_mail_auth_clear',
    'graph_mail_query',
    'graph_mail_message_show',
    'graph_mail_folder_list',
    'graph_mail_folder_create',
    'graph_mail_message_move',
    'graph_mail_message_mark_read',
    'graph_mail_attachment_list',
    'graph_mail_attachment_get',
    'graph_mail_attachment_download_file',
    'graph_mail_attachment_add',
    'graph_mail_attachment_upload_session_create',
    'graph_mail_attachment_upload_chunk',
    'graph_mail_attachment_upload_file',
    'graph_mail_attachment_delete',
    'graph_mail_draft_create',
    'graph_mail_reply_draft_create',
    'graph_mail_reply_all_draft_create',
    'graph_mail_forward_draft_create',
    'graph_mail_reply_all_to_last_in_thread_draft_create',
    'graph_mail_ticket_draft_upsert',
    'graph_mail_ticket_draft_discard',
    'graph_mail_ticket_draft_disposition_scan',
    'graph_mail_ticket_draft_disposition_list',
    'graph_mail_ticket_draft_disposition_ack',
    'graph_mail_draft_update',
    'graph_mail_draft_discard',
    'graph_mail_draft_send',
    'graph_mail_output_show',
  ]);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_attachment_list')?.annotations.readOnlyHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_attachment_delete')?.annotations.destructiveHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_auth_device_code_start')?.annotations.readOnlyHint, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_auth_status')?.annotations.readOnlyHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_auth_clear')?.annotations.destructiveHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_folder_list')?.annotations.readOnlyHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_folder_create')?.annotations.readOnlyHint, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_message_move')?.annotations.destructiveHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_message_mark_read')?.annotations.idempotentHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_message_mark_read')?.annotations.destructiveHint, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_attachment_upload_chunk')?.annotations.readOnlyHint, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_ticket_draft_upsert')?.annotations.idempotentHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_ticket_draft_discard')?.annotations.destructiveHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_ticket_draft_discard')?.annotations.idempotentHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_ticket_draft_upsert')?.annotations.readOnlyHint, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_ticket_draft_disposition_scan')?.annotations.idempotentHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_ticket_draft_disposition_scan')?.annotations.readOnlyHint, false);
  assert.equal(typeof toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_reply_all_draft_create')?.inputSchema.properties.comment_html, 'object');
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_ticket_draft_disposition_list')?.annotations.readOnlyHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_folder_list')?.inputSchema.properties.limit.default, 50);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_folder_create')?.inputSchema.properties.confirm_write.default, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_message_move')?.inputSchema.properties.confirm_write.default, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_message_mark_read')?.inputSchema.properties.confirm_write.default, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_auth_clear')?.inputSchema.properties.confirm_clear.default, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_attachment_get')?.inputSchema.properties.include_content.default, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_attachment_download_file')?.annotations.readOnlyHint, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_attachment_download_file')?.annotations.idempotentHint, true);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_attachment_list')?.inputSchema.properties.limit.default, 20);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_folder_create')?.inputSchema.required.join(','), 'display_name');
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_message_move')?.inputSchema.required.join(','), 'message_id,destination_folder_id');
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_attachment_upload_session_create')?.inputSchema.properties.size.minimum, 1);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_attachment_upload_chunk')?.inputSchema.required.join(','), 'upload_url,content_base64,range_start,range_end,total_size');
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_attachment_upload_file')?.inputSchema.required.join(','), 'file_path');
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_draft_update')?.inputSchema.properties.allow_replace_full_body.default, false);
  assert.equal(toolRows.find((tool: DynamicTestValue) => tool.name === 'graph_mail_draft_update')?.inputSchema.properties.allow_replace_quoted_body.default, false);

  const blockedFolderCalls: CapturedRequest[] = [];
  const blockedFolderState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(blockedFolderCalls, [{ body: { value: [{ id: 'folder-1', displayName: 'Inbox' }] } }]),
  });

  const folderList = await rpc({
    jsonrpc: '2.0',
    id: 31,
    method: 'tools/call',
    params: {
      name: 'graph_mail_folder_list',
      arguments: {
        mailbox_id: 'support@example.test',
        parent_folder_id: 'archive',
        select: 'id,displayName',
        limit: 10,
      },
    },
  }, blockedFolderState);
  assert.equal(folderList.error, undefined);
  assert.equal(blockedFolderCalls[0].init.method, 'GET');
  assert.equal(blockedFolderCalls[0].init.headers.Authorization, 'Bearer test-token');
  assert.equal(blockedFolderCalls[0].url, 'https://graph.example.test/v1.0/users/support%40example.test/mailFolders/archive/childFolders?%24top=10&%24select=id%2CdisplayName');
  assert.equal(folderList.result.structuredContent.folders.value[0].id, 'folder-1');

  const blockedFolderCreate = await rpc({
    jsonrpc: '2.0',
    id: 34,
    method: 'tools/call',
    params: {
      name: 'graph_mail_folder_create',
      arguments: {
        mailbox_id: 'support@example.test',
        parent_folder_id: 'archive',
        display_name: 'Customers',
        confirm_write: true,
      },
    },
  }, blockedFolderState);
  assert.equal(blockedFolderCreate.error, undefined);
  assert.equal(blockedFolderCreate.result.structuredContent.status, 'refused');
  assert.equal(blockedFolderCreate.result.structuredContent.reason, 'folder_create_disallowed_by_policy');

  const blockedMessageMove = await rpc({
    jsonrpc: '2.0',
    id: 35,
    method: 'tools/call',
    params: {
      name: 'graph_mail_message_move',
      arguments: {
        mailbox_id: 'support@example.test',
        message_id: 'message-1',
        destination_folder_id: 'folder-2',
        confirm_write: true,
      },
    },
  }, blockedFolderState);
  assert.equal(blockedMessageMove.error, undefined);
  assert.equal(blockedMessageMove.result.structuredContent.status, 'refused');
  assert.equal(blockedMessageMove.result.structuredContent.reason, 'message_move_disallowed_by_policy');
  const blockedMessageMarkRead = await rpc({
    jsonrpc: '2.0',
    id: 351,
    method: 'tools/call',
    params: {
      name: 'graph_mail_message_mark_read',
      arguments: {
        mailbox_id: 'support@example.test',
        message_id: 'message-1',
        confirm_write: true,
        idempotency_key: 'blocked-message-1-read',
      },
    },
  }, blockedFolderState);
  assert.equal(blockedMessageMarkRead.error, undefined);
  assert.equal(blockedMessageMarkRead.result.structuredContent.outcome, 'failed');
  assert.equal(blockedMessageMarkRead.result.structuredContent.result.status, 'refused');
  assert.equal(blockedMessageMarkRead.result.structuredContent.result.reason, 'message_mark_read_disallowed_by_policy');
  assert.equal(blockedFolderCalls.length, 1);
  const blockedMailboxOrganizationAudit = readFileSync(join(root, '.ai', 'audit', 'graph-mail-mcp.jsonl'), 'utf8');
  assert.match(blockedMailboxOrganizationAudit, /folder_create_refused/);
  assert.match(blockedMailboxOrganizationAudit, /message_move_refused/);
  assert.match(blockedMailboxOrganizationAudit, /message_mark_read_refused/);

  const blockedAuth = await rpc({
    jsonrpc: '2.0',
    id: 38,
    method: 'tools/call',
    params: {
      name: 'graph_mail_auth_device_code_start',
      arguments: { scope: 'https://graph.microsoft.com/Mail.ReadWrite' },
    },
  }, blockedFolderState);
  assert.equal(blockedAuth.error, undefined);
  assert.equal(blockedAuth.result.structuredContent.status, 'refused');
  assert.equal(blockedAuth.result.structuredContent.reason, 'device_code_auth_disallowed_by_policy');

  writeFileSync(join(root, '.ai', 'graph-mail-mcp.json'), JSON.stringify({
    graph_base_url: 'https://graph.example.test/v1.0',
    allowed_mailboxes: ['support@example.test'],
    allow_device_code_auth: true,
    device_code_tenant_id: 'tenant-1',
    device_code_client_id: 'client-1',
    device_code_allowed_scopes: ['https://graph.microsoft.com/Mail.ReadWrite'],
  }));
  const authCalls: CapturedRequest[] = [];
  const authState = createServerState({
    siteRoot: root,
    clientSecret: 'client-credentials-secret-must-not-be-used-for-device-code',
    fetchImpl: mockFetch(authCalls, [
      {
        body: {
          device_code: 'secret-device-code',
          user_code: 'ABCD-EFGH',
          verification_uri: 'https://microsoft.com/devicelogin',
          expires_in: 900,
          interval: 5,
          message: 'Open the verification URL and enter the code.',
        },
      },
      { ok: false, status: 400, text: '{"error":"authorization_pending"}' },
      { body: { access_token: 'delegated-token-1', expires_in: 3600 } },
      { body: { value: [{ id: 'folder-delegated', displayName: 'Inbox' }] } },
    ]),
  });

  const authStart = await rpc({
    jsonrpc: '2.0',
    id: 39,
    method: 'tools/call',
    params: {
      name: 'graph_mail_auth_device_code_start',
      arguments: { scope: 'https://graph.microsoft.com/Mail.ReadWrite' },
    },
  }, authState);
  assert.equal(authStart.error, undefined);
  assert.equal(authStart.result.structuredContent.status, 'authorization_pending');
  assert.equal(authStart.result.structuredContent.user_code, 'ABCD-EFGH');
  assert.equal(authStart.result.structuredContent.device_code, undefined);
  const flowId = String(authStart.result.structuredContent.flow_id);
  assert.equal(authCalls[0].url, 'https://login.microsoftonline.com/tenant-1/oauth2/v2.0/devicecode');
  assert.match(String(authCalls[0].init.body), /client_id=client-1/);

  const authPending = await rpc({
    jsonrpc: '2.0',
    id: 40,
    method: 'tools/call',
    params: {
      name: 'graph_mail_auth_device_code_poll',
      arguments: { flow_id: flowId },
    },
  }, authState);
  assert.equal(authPending.error, undefined);
  assert.equal(authPending.result.structuredContent.status, 'authorization_pending');
  assert.doesNotMatch(String(authCalls[1].init.body), /client_secret=/);

  const authPoll = await rpc({
    jsonrpc: '2.0',
    id: 41,
    method: 'tools/call',
    params: {
      name: 'graph_mail_auth_device_code_poll',
      arguments: { flow_id: flowId },
    },
  }, authState);
  assert.equal(authPoll.error, undefined);
  assert.equal(authPoll.result.structuredContent.status, 'authorized');
  assert.equal(authPoll.result.structuredContent.access_token, undefined);

  const authStatus = await rpc({
    jsonrpc: '2.0',
    id: 42,
    method: 'tools/call',
    params: { name: 'graph_mail_auth_status', arguments: {} },
  }, authState);
  assert.equal(authStatus.error, undefined);
  assert.equal(authStatus.result.structuredContent.delegated_token.status, 'available');
  assert.equal(authStatus.result.structuredContent.delegated_token.access_token, undefined);

  const delegatedFolderList = await rpc({
    jsonrpc: '2.0',
    id: 43,
    method: 'tools/call',
    params: {
      name: 'graph_mail_folder_list',
      arguments: { mailbox_id: 'support@example.test' },
    },
  }, authState);
  assert.equal(delegatedFolderList.error, undefined);
  assert.equal(authCalls[3].init.headers.Authorization, 'Bearer delegated-token-1');

  const authClearRefused = await rpc({
    jsonrpc: '2.0',
    id: 44,
    method: 'tools/call',
    params: { name: 'graph_mail_auth_clear', arguments: {} },
  }, authState);
  assert.equal(authClearRefused.error, undefined);
  assert.equal(authClearRefused.result.structuredContent.status, 'refused');
  assert.equal(authClearRefused.result.structuredContent.reason, 'confirm_clear_required');

  const authClear = await rpc({
    jsonrpc: '2.0',
    id: 45,
    method: 'tools/call',
    params: { name: 'graph_mail_auth_clear', arguments: { confirm_clear: true } },
  }, authState);
  assert.equal(authClear.error, undefined);
  assert.equal(authClear.result.structuredContent.status, 'cleared');

  const invalidClientCalls: CapturedRequest[] = [];
  const invalidClientState = createServerState({
    siteRoot: root,
    clientSecret: 'client-credentials-secret-must-not-be-used-for-device-code',
    fetchImpl: mockFetch(invalidClientCalls, [
      {
        body: {
          device_code: 'secret-device-code-2',
          user_code: 'WXYZ-1234',
          verification_uri: 'https://microsoft.com/devicelogin',
          expires_in: 900,
          interval: 5,
        },
      },
      {
        ok: false,
        status: 401,
        text: JSON.stringify({
          error: 'invalid_client',
          error_description: "AADSTS7000218: The request body must contain the following parameter: 'client_assertion' or 'client_secret'.",
        }),
      },
    ]),
  });
  const invalidStart = await rpc({
    jsonrpc: '2.0',
    id: 46,
    method: 'tools/call',
    params: {
      name: 'graph_mail_auth_device_code_start',
      arguments: { scope: 'https://graph.microsoft.com/Mail.ReadWrite' },
    },
  }, invalidClientState);
  const invalidPoll = await rpc({
    jsonrpc: '2.0',
    id: 47,
    method: 'tools/call',
    params: {
      name: 'graph_mail_auth_device_code_poll',
      arguments: { flow_id: String(invalidStart.result.structuredContent.flow_id) },
    },
  }, invalidClientState);
  assert.equal(invalidPoll.error, undefined);
  assert.equal(invalidPoll.result.structuredContent.status, 'refused');
  assert.equal(invalidPoll.result.structuredContent.reason, 'device_code_client_must_be_public_client');
  assert.doesNotMatch(String(invalidClientCalls[1].init.body), /client_secret=/);

  writeFileSync(join(root, '.ai', 'graph-mail-mcp.json'), JSON.stringify({
    graph_base_url: 'https://graph.example.test/v1.0',
    allowed_mailboxes: ['support@example.test'],
    allow_folder_create: true,
    allow_message_move: true,
    allow_message_mark_read: true,
    mailbox_organization_approval_token: 'organize-123',
  }));
  const folderCalls: CapturedRequest[] = [];
  const folderState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(folderCalls, [
      { body: { id: 'folder-2', displayName: 'Customers' } },
      { body: { id: 'message-2', parentFolderId: 'folder-2' } },
      { status: 204, text: '' },
    ]),
  });

  const folderCreateMissingToken = await rpc({
    jsonrpc: '2.0',
    id: 36,
    method: 'tools/call',
    params: {
      name: 'graph_mail_folder_create',
      arguments: {
        mailbox_id: 'support@example.test',
        display_name: 'Customers',
        confirm_write: true,
      },
    },
  }, folderState);
  assert.equal(folderCreateMissingToken.error, undefined);
  assert.equal(folderCreateMissingToken.result.structuredContent.status, 'refused');
  assert.equal(folderCreateMissingToken.result.structuredContent.reason, 'mailbox_organization_approval_token_required');

  const messageMoveMissingConfirm = await rpc({
    jsonrpc: '2.0',
    id: 37,
    method: 'tools/call',
    params: {
      name: 'graph_mail_message_move',
      arguments: {
        mailbox_id: 'support@example.test',
        message_id: 'message-1',
        destination_folder_id: 'folder-2',
        approval_token: 'organize-123',
      },
    },
  }, folderState);
  assert.equal(messageMoveMissingConfirm.error, undefined);
  assert.equal(messageMoveMissingConfirm.result.structuredContent.status, 'refused');
  assert.equal(messageMoveMissingConfirm.result.structuredContent.reason, 'confirm_write_required');
  assert.equal(folderCalls.length, 0);

  const folderCreate = await rpc({
    jsonrpc: '2.0',
    id: 32,
    method: 'tools/call',
    params: {
      name: 'graph_mail_folder_create',
      arguments: {
        mailbox_id: 'support@example.test',
        parent_folder_id: 'archive',
        display_name: 'Customers',
        confirm_write: true,
        approval_token: 'organize-123',
      },
    },
  }, folderState);
  assert.equal(folderCreate.error, undefined);
  assert.equal(folderCalls[0].init.method, 'POST');
  assert.equal(folderCalls[0].url, 'https://graph.example.test/v1.0/users/support%40example.test/mailFolders/archive/childFolders');
  assert.equal(JSON.parse(folderCalls[0].init.body).displayName, 'Customers');
  assert.equal(folderCreate.result.structuredContent.folder.id, 'folder-2');

  const messageMove = await rpc({
    jsonrpc: '2.0',
    id: 33,
    method: 'tools/call',
    params: {
      name: 'graph_mail_message_move',
      arguments: {
        mailbox_id: 'support@example.test',
        message_id: 'message-1',
        destination_folder_id: 'folder-2',
        confirm_write: true,
        approval_token: 'organize-123',
      },
    },
  }, folderState);
  assert.equal(messageMove.error, undefined);
  assert.equal(folderCalls[1].init.method, 'POST');
  assert.equal(folderCalls[1].url, 'https://graph.example.test/v1.0/users/support%40example.test/messages/message-1/move');
  assert.equal(JSON.parse(folderCalls[1].init.body).destinationId, 'folder-2');
  assert.equal(messageMove.result.structuredContent.message.id, 'message-2');
  const messageMarkRead = await rpc({
    jsonrpc: '2.0',
    id: 331,
    method: 'tools/call',
    params: {
      name: 'graph_mail_message_mark_read',
      arguments: {
        mailbox_id: 'support@example.test',
        message_id: 'message-1',
        confirm_write: true,
        idempotency_key: 'message-1-read',
      },
    },
  }, folderState);
  assert.equal(messageMarkRead.error, undefined);
  assert.equal(folderCalls[2].init.method, 'PATCH');
  assert.equal(folderCalls[2].url, 'https://graph.example.test/v1.0/users/support%40example.test/messages/message-1');
  assert.deepEqual(JSON.parse(folderCalls[2].init.body), { isRead: true });
  assert.equal(messageMarkRead.result.structuredContent.outcome, 'completed');
  assert.equal(messageMarkRead.result.structuredContent.result.status, 'marked_read');
  const allowedMailboxOrganizationAudit = readFileSync(join(root, '.ai', 'audit', 'graph-mail-mcp.jsonl'), 'utf8');
  assert.match(allowedMailboxOrganizationAudit, /folder_create_completed/);
  assert.match(allowedMailboxOrganizationAudit, /message_move_completed/);
  assert.match(allowedMailboxOrganizationAudit, /message_mark_read_completed/);

  const attachmentList = await rpc({
    jsonrpc: '2.0',
    id: 3,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_list',
      arguments: {
        message_id: 'message-1',
        limit: 3,
      },
    },
  }, attachmentState);
  assert.equal(attachmentList.error, undefined);
  assert.equal(attachmentCalls[0].init.method, 'GET');
  assert.equal(attachmentCalls[0].init.headers.Authorization, 'Bearer test-token');
  assert.equal(attachmentCalls[0].url, 'https://graph.example.test/v1.0/users/support%40example.test/messages/message-1/attachments?%24top=3');
  assert.equal(attachmentList.result.structuredContent.attachments.value[0].id, 'att-list-1');

  const attachmentGet = await rpc({
    jsonrpc: '2.0',
    id: 4,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_get',
      arguments: {
        message_id: 'message-1',
        attachment_id: 'att-get-1',
        include_content: false,
      },
    },
  }, attachmentState);
  assert.equal(attachmentGet.error, undefined);
  assert.equal(attachmentCalls[1].init.method, 'GET');
  assert.equal(attachmentCalls[1].init.headers.Authorization, 'Bearer test-token');
  assert.equal(attachmentCalls[1].url, 'https://graph.example.test/v1.0/users/support%40example.test/messages/message-1/attachments/att-get-1');
  assert.equal(attachmentGet.result.structuredContent.attachment.id, 'att-get-1');
  assert.equal(attachmentGet.result.structuredContent.attachment.contentBytes, undefined);
  assert.equal(attachmentGet.result.structuredContent.attachment.content_base64, undefined);
  assert.equal(attachmentGet.result.structuredContent.attachment.content, undefined);
  assert.equal(attachmentGet.result.structuredContent.attachment.data, undefined);
  assert.equal(attachmentGet.result.structuredContent.attachment.bytes, undefined);
  assert.equal(attachmentGet.result.structuredContent.attachment.raw, undefined);

  const downloadCalls: CapturedRequest[] = [];
  const downloadState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(downloadCalls, [{
      body: {
        id: 'att-download-1',
        name: 'download.pdf',
        contentType: 'application/pdf',
        contentBytes: Buffer.from('download-body').toString('base64'),
        size: Buffer.byteLength('download-body'),
      },
    }]),
  });
  const downloaded = await rpc({
    jsonrpc: '2.0',
    id: 41,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_download_file',
      arguments: {
        message_id: 'message-1',
        attachment_id: 'att-download-1',
        file_path: 'incoming-attachments/download.pdf',
      },
    },
  }, downloadState);
  assert.equal(downloaded.error, undefined);
  assert.equal(downloaded.result.structuredContent.status, 'materialized');
  assert.equal(readFileSync(join(root, 'incoming-attachments', 'download.pdf'), 'utf8'), 'download-body');
  assert.equal(downloadCalls[0].init.headers.Authorization, 'Bearer test-token');

  const attachmentAdd = await rpc({
    jsonrpc: '2.0',
    id: 5,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_add',
      arguments: {
        message_id: 'message-1',
        name: 'report.pdf',
        content_type: 'application/pdf',
        content_base64: Buffer.from('attachment-body').toString('base64'),
        is_inline: true,
        content_id: 'cid-123',
      },
    },
  }, attachmentState);
  assert.equal(attachmentAdd.error, undefined);
  assert.equal(attachmentCalls[2].init.method, 'POST');
  assert.equal(attachmentCalls[2].init.headers.Authorization, 'Bearer test-token');
  assert.equal(attachmentCalls[2].url, 'https://graph.example.test/v1.0/users/support%40example.test/messages/message-1/attachments');
  const attachmentAddBody = JSON.parse(attachmentCalls[2].init.body);
  assert.equal(attachmentAddBody['@odata.type'], '#microsoft.graph.fileAttachment');
  assert.equal(attachmentAddBody.name, 'report.pdf');
  assert.equal(attachmentAddBody.contentType, 'application/pdf');
  assert.equal(attachmentAddBody.contentBytes, Buffer.from('attachment-body').toString('base64'));
  assert.equal(attachmentAddBody.isInline, true);
  assert.equal(attachmentAddBody.contentId, 'cid-123');

  const oversizedSmallAttachment = await rpc({
    jsonrpc: '2.0',
    id: 21,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_add',
      arguments: {
        message_id: 'message-1',
        name: 'too-large.bin',
        content_type: 'application/octet-stream',
        content_base64: Buffer.alloc(3 * 1024 * 1024 + 1).toString('base64'),
      },
    },
  }, attachmentState);
  assert.match(oversizedSmallAttachment.error.message, /attachment_small_file_too_large/);

  const uploadSessionCreate = await rpc({
    jsonrpc: '2.0',
    id: 6,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_upload_session_create',
      arguments: {
        draft_id: 'draft-1',
        name: 'video.mp4',
        size: 42,
        content_type: 'video/mp4',
        is_inline: false,
        content_id: 'cid-video',
      },
    },
  }, attachmentState);
  assert.equal(uploadSessionCreate.error, undefined);
  assert.equal(attachmentCalls[3].init.method, 'POST');
  assert.equal(attachmentCalls[3].init.headers.Authorization, 'Bearer test-token');
  assert.equal(attachmentCalls[3].url, 'https://graph.example.test/v1.0/users/support%40example.test/messages/draft-1/attachments/createUploadSession');
  const uploadSessionBody = JSON.parse(attachmentCalls[3].init.body);
  assert.equal(uploadSessionBody.AttachmentItem.attachmentType, 'file');
  assert.equal(uploadSessionBody.AttachmentItem.name, 'video.mp4');
  assert.equal(uploadSessionBody.AttachmentItem.size, 42);
  assert.equal(uploadSessionBody.AttachmentItem.contentType, 'video/mp4');
  assert.equal(uploadSessionBody.AttachmentItem.isInline, false);
  assert.equal(uploadSessionBody.AttachmentItem.contentId, 'cid-video');

  const localAttachmentBytes = Buffer.concat([Buffer.alloc(327680, 1), Buffer.from('tail')]);
  writeFileSync(join(root, 'local.bin'), localAttachmentBytes);
  const uploadFile = await rpc({
    jsonrpc: '2.0',
    id: 22,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_upload_file',
      arguments: {
        draft_id: 'draft-1',
        file_path: 'local.bin',
        chunk_size: 327680,
      },
    },
  }, attachmentState);
  assert.equal(uploadFile.error, undefined);
  assert.equal(attachmentCalls[4].init.method, 'POST');
  assert.equal(attachmentCalls[4].url, 'https://graph.example.test/v1.0/users/support%40example.test/messages/draft-1/attachments/createUploadSession');
  const uploadFileSessionBody = JSON.parse(attachmentCalls[4].init.body);
  assert.equal(uploadFileSessionBody.AttachmentItem.name, 'local.bin');
  assert.equal(uploadFileSessionBody.AttachmentItem.size, localAttachmentBytes.byteLength);
  assert.equal(uploadFileSessionBody.AttachmentItem.contentType, 'application/octet-stream');
  assert.equal(attachmentCalls[5].init.method, 'PUT');
  assert.equal(attachmentCalls[5].url, 'https://outlook.office365.com/upload/file-abc');
  assert.equal(attachmentCalls[5].init.headers['Content-Range'], `bytes 0-327679/${localAttachmentBytes.byteLength}`);
  assert.equal(Buffer.from(attachmentCalls[5].init.body).byteLength, 327680);
  assert.equal(attachmentCalls[6].init.headers['Content-Range'], `bytes 327680-${localAttachmentBytes.byteLength - 1}/${localAttachmentBytes.byteLength}`);
  assert.equal(Buffer.from(attachmentCalls[6].init.body).toString('utf8'), 'tail');
  assert.equal(uploadFile.result.structuredContent.status, 'uploaded');
  assert.equal(uploadFile.result.structuredContent.name, 'local.bin');
  assert.equal(uploadFile.result.structuredContent.size, localAttachmentBytes.byteLength);
  assert.equal(uploadFile.result.structuredContent.chunk_count, 2);
  assert.equal(uploadFile.result.structuredContent.sha256, createHash('sha256').update(localAttachmentBytes).digest('hex'));
  assert.equal(uploadFile.result.structuredContent.attachment.id, 'att-uploaded-1');

  const attachmentDelete = await rpc({
    jsonrpc: '2.0',
    id: 7,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_delete',
      arguments: {
        draft_id: 'draft-1',
        attachment_id: 'att-delete-1',
      },
    },
  }, attachmentState);
  assert.equal(attachmentDelete.error, undefined);
  assert.equal(attachmentCalls[7].init.method, 'DELETE');
  assert.equal(attachmentCalls[7].init.headers.Authorization, 'Bearer test-token');
  assert.equal(attachmentCalls[7].url, 'https://graph.example.test/v1.0/users/support%40example.test/messages/draft-1/attachments/att-delete-1');

  const uploadCalls: CapturedRequest[] = [];
  const uploadState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(uploadCalls, [{ status: 202, text: '' }]),
  });

  const uploadChunk = await rpc({
    jsonrpc: '2.0',
    id: 8,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_upload_chunk',
      arguments: {
        upload_url: 'https://outlook.office.com/upload/abc',
        content_base64: Buffer.from('chunk-bytes').toString('base64'),
        range_start: 0,
        range_end: 10,
        total_size: 11,
      },
    },
  }, uploadState);
  assert.equal(uploadChunk.error, undefined);
  assert.equal(uploadCalls[0].init.method, 'PUT');
  assert.equal(uploadCalls[0].init.headers.Authorization, undefined);
  assert.equal(uploadCalls[0].init.headers['Content-Length'], '11');
  assert.equal(uploadCalls[0].init.headers['Content-Range'], 'bytes 0-10/11');
  assert.equal(uploadCalls[0].init.headers['Content-Type'], 'application/octet-stream');
  assert.equal(Buffer.from(uploadCalls[0].init.body).toString('utf8'), 'chunk-bytes');

  const forbiddenHttpUpload = await rpc({
    jsonrpc: '2.0',
    id: 9,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_upload_chunk',
      arguments: {
        upload_url: 'http://outlook.office.com/upload/abc',
        content_base64: Buffer.from('x').toString('base64'),
        range_start: 0,
        range_end: 0,
        total_size: 1,
      },
    },
  }, uploadState);
  assert.match(forbiddenHttpUpload.error.message, /attachment_upload_url_must_be_https/);

  const forbiddenHostUpload = await rpc({
    jsonrpc: '2.0',
    id: 10,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_upload_chunk',
      arguments: {
        upload_url: 'https://evil.example/upload/abc',
        content_base64: Buffer.from('x').toString('base64'),
        range_start: 0,
        range_end: 0,
        total_size: 1,
      },
    },
  }, uploadState);
  assert.match(forbiddenHostUpload.error.message, /attachment_upload_url_host_not_allowed/);
  assert.equal(uploadCalls.length, 1);

  const invalidUploadUrl = await rpc({
    jsonrpc: '2.0',
    id: 19,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_upload_chunk',
      arguments: {
        upload_url: 'not a url',
        content_base64: Buffer.from('x').toString('base64'),
        range_start: 0,
        range_end: 0,
        total_size: 1,
      },
    },
  }, uploadState);
  assert.match(invalidUploadUrl.error.message, /attachment_upload_url_invalid/);
  assert.doesNotMatch(invalidUploadUrl.error.message, /not a url/);

  const failedUploadCalls: CapturedRequest[] = [];
  const failedUploadState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(failedUploadCalls, [{ status: 400, text: 'failed https://outlook.office.com/upload/secret-token' }]),
  });
  const failedUpload = await rpc({
    jsonrpc: '2.0',
    id: 20,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_upload_chunk',
      arguments: {
        upload_url: 'https://outlook.office.com/upload/secret-token',
        content_base64: Buffer.from('x').toString('base64'),
        range_start: 0,
        range_end: 0,
        total_size: 1,
      },
    },
  }, failedUploadState);
  assert.match(failedUpload.error.message, /attachment_upload_failed:400:failed \[redacted-upload-url\]/);
  assert.doesNotMatch(failedUpload.error.message, /secret-token/);

  const clientCredentialCalls: CapturedRequest[] = [];
  const clientCredentialState = createServerState({
    siteRoot: root,
    tenantId: 'tenant-1',
    clientId: 'client-1',
    clientSecret: 'secret-1',
    tokenEndpoint: 'https://login.example.test/token',
    fetchImpl: mockFetch(clientCredentialCalls, [
      { text: JSON.stringify({ access_token: 'app-token', expires_in: 3600 }) },
      { body: { value: [{ id: 'msg-app-1' }] } },
    ]),
  });

  const clientCredentialDoctor = await rpc({
    jsonrpc: '2.0',
    id: 11,
    method: 'tools/call',
    params: { name: 'graph_mail_doctor', arguments: {} },
  }, clientCredentialState);
  assert.equal(clientCredentialDoctor.error, undefined);
  assert.equal(clientCredentialDoctor.result.structuredContent.has_access_token, true);
  assert.equal(clientCredentialDoctor.result.structuredContent.auth_mode, 'client_credentials');
  assert.equal(clientCredentialCalls.length, 0);

  const clientCredentialQuery = await rpc({
    jsonrpc: '2.0',
    id: 12,
    method: 'tools/call',
    params: { name: 'graph_mail_query', arguments: { mailbox_id: 'support@example.test', limit: 1 } },
  }, clientCredentialState);
  assert.equal(clientCredentialQuery.error, undefined);
  assert.equal(clientCredentialCalls[0].url, 'https://login.example.test/token');
  assert.equal(clientCredentialCalls[0].init.method, 'POST');
  assert.match(clientCredentialCalls[1].url, /^https:\/\/graph\.example\.test\/v1\.0\/users\/support%40example\.test\/messages\?/);
  assert.equal(clientCredentialCalls[1].init.headers.Authorization, 'Bearer app-token');

  const draftCalls: CapturedRequest[] = [];
  const draftState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(draftCalls, [
      { body: { value: [{ id: 'msg-1' }] } },
      { body: { id: 'draft-1', subject: 'Customer follow-up' } },
    ]),
  });

  const query = await rpc({
    jsonrpc: '2.0',
    id: 13,
    method: 'tools/call',
    params: {
      name: 'graph_mail_query',
      arguments: {
        mailbox_id: 'support@example.test',
        query: 'follow up',
        limit: 5,
      },
    },
  }, draftState);
  assert.equal(query.error, undefined);
  assert.match(draftCalls[0].url, /^https:\/\/graph\.example\.test\/v1\.0\/users\/support%40example\.test\/messages\?/);
  assert.match(draftCalls[0].url, /%24top=5/);
  assert.match(draftCalls[0].url, /%24search=%22follow\+up%22/);
  assert.equal(draftCalls[0].init.headers.Authorization, 'Bearer test-token');

  const create = await rpc({
    jsonrpc: '2.0',
    id: 14,
    method: 'tools/call',
    params: {
      name: 'graph_mail_draft_create',
      arguments: {
        mailbox_id: 'support@example.test',
        subject: 'Customer follow-up',
        body_text: 'Draft body',
        to_recipients: ['customer@example.test'],
      },
    },
  }, draftState);
  assert.equal(create.error, undefined);
  assert.equal(draftCalls[1].init.method, 'POST');
  assert.equal(draftCalls[1].url, 'https://graph.example.test/v1.0/users/support%40example.test/messages');
  const createBody = JSON.parse(draftCalls[1].init.body);
  assert.equal(createBody.subject, 'Customer follow-up');
  assert.equal(createBody.body.contentType, 'Text');
  assert.equal(createBody.toRecipients[0].emailAddress.address, 'customer@example.test');

  const ticketDraftCalls: CapturedRequest[] = [];
  const ticketDraftState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(ticketDraftCalls, [
      { body: { value: [] } },
      { body: { id: 'ticket-draft-1', isDraft: true, conversationId: 'conversation-1' } },
    ]),
  });
  const ticketDraftRequest = {
    source_id: 'source-1',
    mailbox_id: 'support@example.test',
    source_message_id: 'source-message-1',
    reply_mode: 'reply_all',
    body_text: 'Prepared but unsent response.',
  };
  const ticketDraftArguments = {
    ticket_id: 'ticket-1',
    effect_claim_id: 'effect-claim-1',
    draft_operation_key: 'draft_operation_ticket_1',
    draft_request_digest: sha256Canonical(ticketDraftRequest),
    draft_source_id: ticketDraftRequest.source_id,
    mailbox_id: ticketDraftRequest.mailbox_id,
    source_message_id: ticketDraftRequest.source_message_id,
    reply_mode: ticketDraftRequest.reply_mode,
    body_text: ticketDraftRequest.body_text,
    idempotency_key: 'sop-action-ticket-draft-1',
  };
  const ticketDraft = await rpc({
    jsonrpc: '2.0',
    id: 1401,
    method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_upsert', arguments: ticketDraftArguments },
  }, ticketDraftState);
  assert.equal(ticketDraft.error, undefined);
  assert.equal(ticketDraft.result.structuredContent.schema, 'narada.domain_operation.v1');
  assert.equal(ticketDraft.result.structuredContent.result.draft_id, 'ticket-draft-1');
  assert.equal(ticketDraft.result.structuredContent.result.idempotency_replayed_or_recovered, false);
  assert.equal(ticketDraftCalls.length, 2);
  assert.equal(ticketDraftCalls[0].init.method, 'GET');
  assert.match(ticketDraftCalls[0].url, /singleValueExtendedProperties/);
  assert.equal(ticketDraftCalls[1].init.method, 'POST');
  assert.match(ticketDraftCalls[1].url, /source-message-1\/createReplyAll$/);
  const ticketDraftBody = JSON.parse(ticketDraftCalls[1].init.body);
  assert.equal(ticketDraftBody.message.body.content, ticketDraftRequest.body_text);
  assert.equal(
    ticketDraftBody.message.singleValueExtendedProperties[0].value,
    ticketDraftArguments.draft_operation_key,
  );

  const ticketDraftReplay = await rpc({
    jsonrpc: '2.0',
    id: 1402,
    method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_upsert', arguments: ticketDraftArguments },
  }, ticketDraftState);
  assert.equal(ticketDraftReplay.error, undefined);
  assert.equal(ticketDraftReplay.result.structuredContent.result.draft_id, 'ticket-draft-1');
  assert.equal(ticketDraftReplay.result.structuredContent.result.idempotency_replayed_or_recovered, true);
  assert.equal(ticketDraftCalls.length, 2, 'exact replay must not call Graph');

  const discardDraftCalls: CapturedRequest[] = [];
  const discardDraftState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(discardDraftCalls, [
      { body: { value: [] } },
      { body: { id: 'ticket-draft-discard-1', isDraft: true, conversationId: 'conversation-discard-1' } },
      {
        body: {
          id: 'ticket-draft-discard-1',
          isDraft: true,
          changeKey: 'discard-change-1',
          singleValueExtendedProperties: [{
            id: 'String {d700a6f2-79ad-4f44-9df7-3e9b622f09f8} Name NaradaTicketDraftOperation',
            value: 'draft_operation_ticket_discard_1',
          }],
        },
      },
      { body: { value: [{ id: 'ticket-draft-discard-1', isDraft: true, changeKey: 'discard-change-1' }] } },
      { status: 204, text: '' },
    ]),
  });
  const discardDraftRequest = {
    source_id: 'source-discard-1',
    mailbox_id: 'support@example.test',
    source_message_id: 'source-message-discard-1',
    reply_mode: 'reply',
    body_text: 'Controlled unsent draft to discard.',
  };
  const discardDraftCreateArguments = {
    ticket_id: 'ticket-discard-1',
    effect_claim_id: 'effect-claim-discard-1',
    draft_operation_key: 'draft_operation_ticket_discard_1',
    draft_request_digest: sha256Canonical(discardDraftRequest),
    draft_source_id: discardDraftRequest.source_id,
    mailbox_id: discardDraftRequest.mailbox_id,
    source_message_id: discardDraftRequest.source_message_id,
    reply_mode: discardDraftRequest.reply_mode,
    body_text: discardDraftRequest.body_text,
    idempotency_key: 'sop-action-ticket-draft-discard-create-1',
  };
  const discardDraftCreated = await rpc({
    jsonrpc: '2.0', id: 14011, method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_upsert', arguments: discardDraftCreateArguments },
  }, discardDraftState);
  assert.equal(discardDraftCreated.error, undefined);
  const genericTrackedDiscard = await rpc({
    jsonrpc: '2.0', id: 140115, method: 'tools/call',
    params: {
      name: 'graph_mail_draft_discard',
      arguments: { mailbox_id: 'support@example.test', draft_id: 'ticket-draft-discard-1' },
    },
  }, discardDraftState);
  assert.match(String(genericTrackedDiscard.error?.message), /graph_ticket_draft_requires_ticket_discard_tool/);
  assert.equal(discardDraftCalls.filter((call) => call.init.method === 'DELETE').length, 0);
  const discardArguments = {
    ticket_id: discardDraftCreateArguments.ticket_id,
    effect_claim_id: discardDraftCreateArguments.effect_claim_id,
    draft_operation_key: discardDraftCreateArguments.draft_operation_key,
    mailbox_id: discardDraftCreateArguments.mailbox_id,
    draft_id: 'ticket-draft-discard-1',
    idempotency_key: 'operator-discard-ticket-draft-1',
    confirm_discard: true,
  };
  const discarded = await rpc({
    jsonrpc: '2.0', id: 14012, method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_discard', arguments: discardArguments },
  }, discardDraftState);
  assert.equal(discarded.error, undefined);
  assert.equal(discarded.result.structuredContent.status, 'discarded');
  assert.equal(discarded.result.structuredContent.idempotency_replayed_or_recovered, false);
  const discardReceipt = discarded.result.structuredContent.disposition_receipt;
  assert.equal(discardReceipt.disposition, 'discarded');
  assert.equal(discardReceipt.evidence_kind, 'operator_confirmed_graph_discard');
  assert.equal(discardReceipt.graph_delete_confirmed, true);
  assert.equal(discardDraftCalls[4].init.method, 'DELETE');
  assert.equal(discardDraftCalls[4].init.headers['If-Match'], 'discard-change-1');
  const { receipt_sha256: discardReceiptSha256, ...unsignedDiscardReceipt } = discardReceipt;
  assert.equal(discardReceiptSha256, sha256Canonical(unsignedDiscardReceipt));
  const discardedReplay = await rpc({
    jsonrpc: '2.0', id: 14013, method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_discard', arguments: discardArguments },
  }, discardDraftState);
  assert.equal(discardedReplay.error, undefined);
  assert.equal(discardedReplay.result.structuredContent.idempotency_replayed_or_recovered, true);
  assert.equal(discardDraftCalls.length, 5, 'discard replay must not call Graph');
  const discardAck = await rpc({
    jsonrpc: '2.0', id: 14014, method: 'tools/call',
    params: {
      name: 'graph_mail_ticket_draft_disposition_ack',
      arguments: {
        observation_id: discardReceipt.observation_id,
        consumer_id: 'work-reconciler',
        reconciliation_ref: 'work-event-discard-1',
        reconciliation_receipt: { event_id: 'work-event-discard-1', status: 'reconciled' },
      },
    },
  }, discardDraftState);
  assert.equal(discardAck.error, undefined);

  const interruptedDiscardCalls: CapturedRequest[] = [];
  let injectDiscardCrash = true;
  const interruptedDiscardState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(interruptedDiscardCalls, [
      { body: { value: [] } },
      { body: { id: 'ticket-draft-discard-recovered', isDraft: true } },
      { body: { value: [{ id: 'ticket-draft-discard-recovered', isDraft: true, changeKey: 'discard-change-recovered' }] } },
      { status: 204, text: '' },
      { body: { value: [] } },
    ]),
    ticketDraftFaultInjector: (point: 'after_graph_commit_before_receipt' | 'after_graph_discard_before_receipt') => {
      if (point !== 'after_graph_discard_before_receipt' || !injectDiscardCrash) return;
      injectDiscardCrash = false;
      throw new Error('injected_after_graph_discard');
    },
  });
  const interruptedCreateRequest = {
    source_id: 'source-discard-recovered',
    mailbox_id: 'support@example.test',
    source_message_id: 'source-message-discard-recovered',
    reply_mode: 'reply',
    body_text: 'Controlled crash-recovery draft.',
  };
  const interruptedCreateArguments = {
    ticket_id: 'ticket-discard-recovered',
    effect_claim_id: 'effect-claim-discard-recovered',
    draft_operation_key: 'draft_operation_ticket_discard_recovered',
    draft_request_digest: sha256Canonical(interruptedCreateRequest),
    draft_source_id: interruptedCreateRequest.source_id,
    mailbox_id: interruptedCreateRequest.mailbox_id,
    source_message_id: interruptedCreateRequest.source_message_id,
    reply_mode: interruptedCreateRequest.reply_mode,
    body_text: interruptedCreateRequest.body_text,
    idempotency_key: 'sop-action-ticket-draft-discard-recovered-create',
  };
  const interruptedCreated = await rpc({
    jsonrpc: '2.0', id: 14015, method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_upsert', arguments: interruptedCreateArguments },
  }, interruptedDiscardState);
  assert.equal(interruptedCreated.error, undefined);
  const interruptedDiscardArguments = {
    ticket_id: interruptedCreateArguments.ticket_id,
    effect_claim_id: interruptedCreateArguments.effect_claim_id,
    draft_operation_key: interruptedCreateArguments.draft_operation_key,
    mailbox_id: interruptedCreateArguments.mailbox_id,
    draft_id: 'ticket-draft-discard-recovered',
    idempotency_key: 'operator-discard-ticket-draft-recovered',
    confirm_discard: true,
  };
  const interruptedDiscard = await rpc({
    jsonrpc: '2.0', id: 14016, method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_discard', arguments: interruptedDiscardArguments },
  }, interruptedDiscardState);
  assert.match(String(interruptedDiscard.error?.message), /injected_after_graph_discard/);
  const recoveredDiscard = await rpc({
    jsonrpc: '2.0', id: 14017, method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_discard', arguments: interruptedDiscardArguments },
  }, interruptedDiscardState);
  assert.equal(recoveredDiscard.error, undefined);
  assert.equal(recoveredDiscard.result.structuredContent.status, 'discarded');
  assert.equal(recoveredDiscard.result.structuredContent.idempotency_replayed_or_recovered, true);
  assert.equal(
    recoveredDiscard.result.structuredContent.disposition_receipt.evidence_kind,
    'operator_authorized_graph_absence_after_verified_discard',
  );
  assert.equal(recoveredDiscard.result.structuredContent.disposition_receipt.graph_delete_confirmed, false);
  assert.equal(interruptedDiscardCalls.filter((call) => call.init.method === 'DELETE').length, 1);
  const recoveredDiscardAck = await rpc({
    jsonrpc: '2.0', id: 14018, method: 'tools/call',
    params: {
      name: 'graph_mail_ticket_draft_disposition_ack',
      arguments: {
        observation_id: recoveredDiscard.result.structuredContent.disposition_receipt.observation_id,
        consumer_id: 'work-reconciler',
        reconciliation_ref: 'work-event-discard-recovered',
        reconciliation_receipt: { event_id: 'work-event-discard-recovered', status: 'reconciled' },
      },
    },
  }, interruptedDiscardState);
  assert.equal(recoveredDiscardAck.error, undefined);

  const absentDispositionCalls: CapturedRequest[] = [];
  const absentDispositionState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(absentDispositionCalls, [{ body: { value: [] } }]),
  });
  const absentDispositionScan = await rpc({
    jsonrpc: '2.0',
    id: 14021,
    method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_disposition_scan', arguments: { limit: 1 } },
  }, absentDispositionState);
  assert.equal(absentDispositionScan.error, undefined);
  assert.equal(absentDispositionScan.result.structuredContent.operations_scanned, 1);
  assert.equal(absentDispositionScan.result.structuredContent.observations_recorded, 0);
  assert.equal(absentDispositionScan.result.structuredContent.still_pending, 1);

  const sentDispositionCalls: CapturedRequest[] = [];
  const sentDispositionState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(sentDispositionCalls, [{
      body: {
        value: [{
          id: 'ticket-draft-1',
          isDraft: false,
          changeKey: 'sent-change-1',
          lastModifiedDateTime: '2026-07-31T15:00:00.000Z',
        }],
      },
    }]),
  });
  const sentDispositionScan = await rpc({
    jsonrpc: '2.0',
    id: 14022,
    method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_disposition_scan', arguments: { limit: 1 } },
  }, sentDispositionState);
  assert.equal(sentDispositionScan.error, undefined);
  assert.equal(sentDispositionScan.result.structuredContent.observations_recorded, 1);
  const dispositionList = await rpc({
    jsonrpc: '2.0',
    id: 14023,
    method: 'tools/call',
    params: {
      name: 'graph_mail_ticket_draft_disposition_list',
      arguments: { consumer_id: 'work-reconciler', limit: 5 },
    },
  }, sentDispositionState);
  assert.equal(dispositionList.error, undefined);
  assert.equal(dispositionList.result.structuredContent.count, 1);
  const dispositionReceipt = dispositionList.result.structuredContent.items[0];
  assert.equal(dispositionReceipt.schema, 'narada.graph_mail.ticket_draft_disposition_receipt.v1');
  assert.equal(dispositionReceipt.disposition, 'sent');
  assert.equal(dispositionReceipt.is_draft, false);
  const { receipt_sha256: receiptSha256, ...unsignedDispositionReceipt } = dispositionReceipt;
  assert.equal(receiptSha256, sha256Canonical(unsignedDispositionReceipt));
  const dispositionAckArguments = {
    observation_id: dispositionReceipt.observation_id,
    consumer_id: 'work-reconciler',
    reconciliation_ref: 'work-event-1',
    reconciliation_receipt: { event_id: 'work-event-1', status: 'reconciled' },
  };
  const dispositionAck = await rpc({
    jsonrpc: '2.0', id: 14024, method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_disposition_ack', arguments: dispositionAckArguments },
  }, sentDispositionState);
  assert.equal(dispositionAck.error, undefined);
  assert.equal(dispositionAck.result.structuredContent.status, 'acknowledged');
  const dispositionAckReplay = await rpc({
    jsonrpc: '2.0', id: 14025, method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_disposition_ack', arguments: dispositionAckArguments },
  }, sentDispositionState);
  assert.equal(dispositionAckReplay.error, undefined);
  assert.equal(dispositionAckReplay.result.structuredContent.status, 'already_acknowledged');
  const emptyDispositionList = await rpc({
    jsonrpc: '2.0', id: 14026, method: 'tools/call',
    params: {
      name: 'graph_mail_ticket_draft_disposition_list',
      arguments: { consumer_id: 'work-reconciler', limit: 5 },
    },
  }, sentDispositionState);
  assert.equal(emptyDispositionList.result.structuredContent.count, 0);

  const recoveryCalls: CapturedRequest[] = [];
  let injectCrash = true;
  const recoveryState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(recoveryCalls, [
      { body: { value: [] } },
      { body: { id: 'ticket-draft-recovered', isDraft: true, conversationId: 'conversation-2' } },
      { body: { value: [{ id: 'ticket-draft-recovered', isDraft: true, conversationId: 'conversation-2' }] } },
    ]),
    ticketDraftFaultInjector: () => {
      if (!injectCrash) return;
      injectCrash = false;
      throw new Error('injected_after_graph_commit');
    },
  });
  const recoveryDraftRequest = {
    source_id: 'source-2',
    mailbox_id: 'support@example.test',
    source_message_id: 'source-message-2',
    reply_mode: 'reply',
    body_html: '<p>Prepared response.</p>',
  };
  const recoveryArguments = {
    ticket_id: 'ticket-2',
    effect_claim_id: 'effect-claim-2',
    draft_operation_key: 'draft_operation_ticket_2',
    draft_request_digest: sha256Canonical(recoveryDraftRequest),
    draft_source_id: recoveryDraftRequest.source_id,
    mailbox_id: recoveryDraftRequest.mailbox_id,
    source_message_id: recoveryDraftRequest.source_message_id,
    reply_mode: recoveryDraftRequest.reply_mode,
    body_html: recoveryDraftRequest.body_html,
    idempotency_key: 'sop-action-ticket-draft-2',
  };
  const interrupted = await rpc({
    jsonrpc: '2.0',
    id: 1403,
    method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_upsert', arguments: recoveryArguments },
  }, recoveryState);
  assert.match(String(interrupted.error?.message), /injected_after_graph_commit/);
  const recovered = await rpc({
    jsonrpc: '2.0',
    id: 1404,
    method: 'tools/call',
    params: { name: 'graph_mail_ticket_draft_upsert', arguments: recoveryArguments },
  }, recoveryState);
  assert.equal(recovered.error, undefined);
  assert.equal(recovered.result.structuredContent.result.draft_id, 'ticket-draft-recovered');
  assert.equal(recovered.result.structuredContent.result.idempotency_replayed_or_recovered, true);
  assert.equal(recoveryCalls.filter((call) => call.init.method === 'POST').length, 1);

  const replyDraftCalls: CapturedRequest[] = [];
  const replyDraftState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(replyDraftCalls, [
      { body: { id: 'draft-reply-1', inReplyTo: 'message-original-1', body: { content: 'quoted text' } } },
      { body: { id: 'draft-reply-1', inReplyTo: 'message-original-1', body: { content: 'new reply' } } },
      { body: { id: 'draft-reply-1', inReplyTo: 'message-original-1', body: { content: 'new reply' } } },
    ]),
  });
  const refusedReplyBodyUpdate = await rpc({
    jsonrpc: '2.0',
    id: 141,
    method: 'tools/call',
    params: {
      name: 'graph_mail_draft_update',
      arguments: { draft_id: 'draft-reply-1', body_text: 'new reply' },
    },
  }, replyDraftState);
  assert.equal(refusedReplyBodyUpdate.error, undefined);
  assert.equal(refusedReplyBodyUpdate.result.structuredContent.status, 'refused');
  assert.equal(replyDraftCalls.length, 1);
  assert.equal(replyDraftCalls[0].init.method, 'GET');
  assert.match(String(readFileSync(join(root, '.ai', 'audit', 'graph-mail-mcp.jsonl'), 'utf8')), /reply_or_forward_body_replacement_requires_explicit_authorization/);

  const allowedReplyBodyUpdate = await rpc({
    jsonrpc: '2.0',
    id: 142,
    method: 'tools/call',
    params: {
      name: 'graph_mail_draft_update',
      arguments: { draft_id: 'draft-reply-1', body_text: 'new reply', allow_replace_quoted_body: true },
    },
  }, replyDraftState);
  assert.equal(allowedReplyBodyUpdate.error, undefined);
  assert.equal(allowedReplyBodyUpdate.result.structuredContent.status, 'updated');
  assert.equal(replyDraftCalls.length, 3);
  assert.equal(replyDraftCalls[2].init.method, 'PATCH');
  assert.equal(JSON.parse(replyDraftCalls[2].init.body).body.content, 'new reply');

  const htmlReplyCalls: CapturedRequest[] = [];
  const htmlReplyState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(htmlReplyCalls, [
      {
        body: {
          id: 'draft-html-1',
          isDraft: true,
          toRecipients: [{ emailAddress: { address: 'sender@example.test' } }],
          ccRecipients: [{ emailAddress: { address: 'peer@example.test' } }],
        },
      },
      {
        body: {
          id: 'draft-html-1',
          isDraft: true,
          toRecipients: [{ emailAddress: { address: 'sender@example.test' } }],
          ccRecipients: [{ emailAddress: { address: 'peer@example.test' } }],
          body: { contentType: 'HTML', content: '<div class="graph-quote">Original quoted history</div>' },
        },
      },
      {
        body: {
          id: 'draft-html-1',
          isDraft: true,
          toRecipients: [{ emailAddress: { address: 'sender@example.test' } }],
          ccRecipients: [{ emailAddress: { address: 'peer@example.test' } }],
          body: { contentType: 'HTML', content: '<p>First paragraph.</p><p>Second paragraph.</p><div data-narada-quoted-history="true"><div class="graph-quote">Original quoted history</div></div>' },
        },
      },
    ]),
  });
  const htmlReply = await rpc({
    jsonrpc: '2.0',
    id: 143,
    method: 'tools/call',
    params: {
      name: 'graph_mail_reply_all_draft_create',
      arguments: {
        mailbox_id: 'support@example.test',
        message_id: 'message-original-1',
        comment_html: '<p>First paragraph.</p><p>Second paragraph.</p>',
      },
    },
  }, htmlReplyState);
  assert.equal(htmlReply.error, undefined);
  assert.equal(htmlReply.result.structuredContent.status, 'created');
  assert.equal(htmlReply.result.structuredContent.reply_body_mode, 'comment_html');
  assert.equal(htmlReply.result.structuredContent.quote_preserved, true);
  assert.equal(htmlReply.result.structuredContent.unsent, true);
  assert.equal(htmlReply.result.structuredContent.draft.isDraft, true);
  assert.equal(htmlReplyCalls.length, 3);
  assert.equal(htmlReplyCalls[0].init.method, 'POST');
  assert.deepEqual(JSON.parse(htmlReplyCalls[0].init.body), {});
  assert.equal(htmlReplyCalls[1].init.method, 'GET');
  assert.equal(htmlReplyCalls[2].init.method, 'PATCH');
  const htmlPatch = JSON.parse(htmlReplyCalls[2].init.body);
  assert.equal(htmlPatch.body.contentType, 'HTML');
  assert.match(htmlPatch.body.content, /<p>First paragraph\.<\/p><p>Second paragraph\.<\/p>/);
  assert.match(htmlPatch.body.content, /Original quoted history/);

  const blockedMailbox = await rpc({
    jsonrpc: '2.0',
    id: 15,
    method: 'tools/call',
    params: { name: 'graph_mail_query', arguments: { mailbox_id: 'blocked@example.test' } },
  }, draftState);
  assert.match(blockedMailbox.error.message, /mailbox_not_allowed/);

  const policyCalls: CapturedRequest[] = [];
  const policyState = createServerState({ siteRoot: root, accessToken: 'test-token', fetchImpl: mockFetch(policyCalls, [{ status: 204, text: '' }]) });

  const refused = await rpc({
    jsonrpc: '2.0',
    id: 16,
    method: 'tools/call',
    params: {
      name: 'graph_mail_draft_send',
      arguments: {
        mailbox_id: 'support@example.test',
        draft_id: 'draft-1',
        confirm_send: true,
      },
    },
  }, policyState);
  assert.equal(refused.error, undefined);
  assert.equal(refused.result.structuredContent.status, 'refused');
  assert.equal(refused.result.structuredContent.reason, 'send_draft_disallowed_by_policy');
  assert.equal(policyCalls.length, 0);
  const auditPath = join(root, '.ai', 'audit', 'graph-mail-mcp.jsonl');
  assert.equal(existsSync(auditPath), true);
  assert.match(readFileSync(auditPath, 'utf8'), /draft_send_refused/);

  writeFileSync(join(root, '.ai', 'graph-mail-mcp.json'), JSON.stringify({
    graph_base_url: 'https://graph.example.test/v1.0',
    allowed_mailboxes: ['support@example.test'],
    allow_send_draft: true,
    send_approval_token: 'approve-123',
  }));

  const deniedNoToken = await rpc({
    jsonrpc: '2.0',
    id: 17,
    method: 'tools/call',
    params: {
      name: 'graph_mail_draft_send',
      arguments: {
        mailbox_id: 'support@example.test',
        draft_id: 'draft-1',
        confirm_send: true,
      },
    },
  }, policyState);
  assert.equal(deniedNoToken.result.structuredContent.status, 'refused');
  assert.equal(deniedNoToken.result.structuredContent.reason, 'send_approval_token_required');

  const sent = await rpc({
    jsonrpc: '2.0',
    id: 18,
    method: 'tools/call',
    params: {
      name: 'graph_mail_draft_send',
      arguments: {
        mailbox_id: 'support@example.test',
        draft_id: 'draft-1',
        confirm_send: true,
        approval_token: 'approve-123',
      },
    },
  }, policyState);
  assert.equal(sent.error, undefined);
  assert.equal(sent.result.structuredContent.status, 'sent');
  assert.equal(policyCalls[0].init.method, 'POST');
  assert.equal(policyCalls[0].url, 'https://graph.example.test/v1.0/users/support%40example.test/messages/draft-1/send');

  writeFileSync(join(root, '.ai', 'mcp-telemetry.json'), JSON.stringify({
    enabled: true,
    level: 'all',
    surfaces: {
      'graph-mail': { enabled: true, level: 'all' },
    },
  }, null, 2), 'utf8');

  const telemetryCalls: CapturedRequest[] = [];
  const telemetryState = createServerState({
    siteRoot: root,
    accessToken: 'test-token',
    fetchImpl: mockFetch(telemetryCalls, [
      { body: { id: 'draft-telemetry-1', subject: 'Telemetry subject sentinel' } },
      { status: 202, text: '' },
      { body: { id: 'attachment-telemetry-1' } },
    ]),
  });
  const telemetryDraft = await rpc({
    jsonrpc: '2.0',
    id: 23,
    method: 'tools/call',
    params: {
      name: 'graph_mail_draft_create',
      arguments: {
        mailbox_id: 'support@example.test',
        subject: 'Telemetry subject sentinel',
        body_text: 'Telemetry body sentinel',
      },
    },
  }, telemetryState);
  assert.equal(telemetryDraft.error, undefined);

  const telemetryUploadChunk = await rpc({
    jsonrpc: '2.0',
    id: 24,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_upload_chunk',
      arguments: {
        upload_url: 'https://outlook.office.com/upload/telemetry-upload-sentinel',
        content_base64: Buffer.from('telemetry').toString('base64'),
        range_start: 0,
        range_end: 8,
        total_size: 9,
      },
    },
  }, telemetryState);
  assert.equal(telemetryUploadChunk.error, undefined);

  const telemetryAttachmentAdd = await rpc({
    jsonrpc: '2.0',
    id: 25,
    method: 'tools/call',
    params: {
      name: 'graph_mail_attachment_add',
      arguments: {
        mailbox_id: 'support@example.test',
        draft_id: 'draft-telemetry-1',
        name: 'telemetry.txt',
        content_type: 'text/plain',
        content_base64: Buffer.from('Telemetry attachment add sentinel').toString('base64'),
      },
    },
  }, telemetryState);
  assert.equal(telemetryAttachmentAdd.error, undefined);

  const telemetryPath = join(root, '.ai', 'telemetry', 'graph-mail.jsonl');
  const telemetryLines = readFileSync(telemetryPath, 'utf8').trim().split('\n').filter(Boolean);
  assert.ok(telemetryLines.length >= 1);
  const telemetryEvents = telemetryLines.map((line) => JSON.parse(line));
  assert.equal(telemetryEvents.some((event: DynamicTestValue) => event.tool_name === 'graph_mail_draft_create'), true);
  assert.equal(telemetryEvents.some((event: DynamicTestValue) => event.tool_name === 'graph_mail_attachment_upload_chunk'), true);
  assert.equal(telemetryEvents.some((event: DynamicTestValue) => event.tool_name === 'graph_mail_attachment_add'), true);
  for (const telemetryEvent of telemetryEvents as DynamicTestValue[]) {
    assert.equal(telemetryEvent.surface_id, 'graph-mail');
    assert.equal(JSON.stringify(telemetryEvent).includes('Telemetry subject sentinel'), false);
    assert.equal(JSON.stringify(telemetryEvent).includes('Telemetry body sentinel'), false);
    assert.equal(JSON.stringify(telemetryEvent).includes('telemetry-upload-sentinel'), false);
    assert.equal(JSON.stringify(telemetryEvent).includes('Telemetry attachment base64 sentinel'), false);
    assert.equal(JSON.stringify(telemetryEvent).includes('Telemetry attachment add sentinel'), false);
  }

  console.log('graph-mail-mcp behavior ok');
} finally {
  rmSync(root, { recursive: true, force: true });
}
