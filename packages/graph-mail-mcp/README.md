# @narada-core/graph-mail-mcp

Policy-gated Microsoft Graph mail MCP surface for live reads and draft lifecycle operations.

Use this package when an agent needs live Microsoft Graph state or needs to create and manage Outlook drafts. Routine mailbox reading should use `@narada-core/mailbox-mcp` first.

## Boundary

- Allowed: query live Microsoft Graph mail for configured mailboxes.
- Allowed: show one live Graph message.
- Allowed: create new drafts.
- Allowed: create reply, reply-all, and forward drafts.
- Allowed: update or discard drafts.
- Allowed: inspect, add, upload, and delete message attachments through Graph mail tools.
- Allowed: list mail folders.
- Folder creation and message moves: disallowed by default; enabled only by explicit site policy and per-call confirmation.
- Send from draft: disallowed by default.
- Not exposed: one-shot direct send operations such as Graph `sendMail`, direct reply send, direct reply-all send, or direct forward send.
- Not allowed: PowerShell or arbitrary command execution.

## Runtime Contract

The server needs Microsoft Graph authorization. Prefer non-interactive application credentials, matching the mailbox sync path:

```text
GRAPH_TENANT_ID
GRAPH_CLIENT_ID
GRAPH_CLIENT_SECRET
```

For a generic deployment, the server may receive these values from its process environment. The Staccato site path does not use `.env` files: its unattended runtime uses the delegated SecretStore/token-store integration configured by the site. The server mints and caches short-lived client-credentials tokens as needed when application credentials are supplied.

For diagnostics or explicit override, callers may still provide a ready access token with:

```text
MS_GRAPH_ACCESS_TOKEN
```

The configured identity must have the Graph permissions needed by the site runtime.

For operator-approved local proof and E2E workflows, this surface can run a Microsoft device-code flow when explicitly enabled by site policy. Device-code auth requires an Entra public-client app with device-code/native-client support. It is not the default runtime auth path and must not replace service-owned client credentials for unattended site operation.

## Site Policy

Policy is read from `.ai/graph-mail-mcp.json` under the site root.

Conservative default:

```json
{
  "graph_base_url": "https://graph.microsoft.com/v1.0",
  "allowed_mailboxes": ["support@example.test"],
  "allow_device_code_auth": false,
  "allow_send_draft": false,
  "allow_folder_create": false,
  "allow_message_move": false
}
```

Sending drafts requires explicit opt-in:

```json
{
  "graph_base_url": "https://graph.microsoft.com/v1.0",
  "allowed_mailboxes": ["support@example.test"],
  "allow_device_code_auth": true,
  "device_code_tenant_id": "tenant-id",
  "device_code_client_id": "public-client-id",
  "device_code_allowed_scopes": ["https://graph.microsoft.com/Mail.ReadWrite"],
  "allow_send_draft": true,
  "send_approval_token": "operator-issued-token",
  "allow_folder_create": true,
  "allow_message_move": true,
  "mailbox_organization_approval_token": "operator-issued-token"
}
```

Policy fields:

- `graph_base_url`: optional Graph API base URL. Defaults to `https://graph.microsoft.com/v1.0`.
- `allowed_mailboxes`: optional mailbox allowlist. When exactly one mailbox is allowed, omitted `mailbox_id` arguments resolve to that mailbox. Otherwise omitted `mailbox_id` resolves to `me`, which must be listed explicitly if an allowlist is configured.
- `allowed_attachment_roots`: optional local filesystem roots for guarded attachment upload and inbound download materialization. Relative paths resolve under the site root. Defaults to the site root when omitted.
- `allow_device_code_auth`: defaults to `false`. Enables operator-approved delegated proof auth tools.
- `device_code_tenant_id`: tenant id for device-code auth. May fall back to `GRAPH_TENANT_ID`.
- `device_code_client_id`: public-client app id for device-code auth. Do not use a confidential client or client secret for device-code auth.
- `device_code_allowed_scopes`: exact allowlist of space-separated scope sets accepted by `graph_mail_auth_device_code_start`.
- `allow_send_draft`: defaults to `false`.
- `send_approval_token`: optional token required by `graph_mail_draft_send`.
- `allow_folder_create`: defaults to `false`.
- `allow_message_move`: defaults to `false`.
- `mailbox_organization_approval_token`: optional token required by `graph_mail_folder_create` and `graph_mail_message_move`.

## Audit

Draft mutations and send refusals/completions are written to:

```text
.ai/audit/graph-mail-mcp.jsonl
```

This includes draft create/update/discard requests, draft-send refusals/completions, folder-create refusals/completions, message-move refusals/completions, device-code auth start/completion/refusal/clear events, and attachment upload completions.

Device-code access tokens are stored under `.ai/runtime/graph-mail-mcp/delegated-token.json` and are never returned by MCP tools, telemetry, or audit output.

Attachment uploads are not sent through `graph_request`; upload tools validate the opaque `uploadUrl`, require `https`, and only allow the exact Graph-owned hosts `outlook.office.com`, `outlook.office365.com`, and `graph.microsoft.com`.

## Telemetry

Telemetry is optional and off by default. When a site enables `.ai/mcp-telemetry.json`, this surface emits metadata-only events for tool completion, refusal, and failure. It does not persist raw mail bodies, attachment bytes, recipient lists, access tokens, approval tokens, upload URLs, or Graph response bodies.

## Tools

- `graph_mail_doctor`: reports Graph auth availability, auth mode, and active policy.
- `graph_mail_auth_device_code_start`: starts an operator-approved device-code flow when policy enables it. Returns user code, verification URI, interval, and flow id; never returns tokens.
- `graph_mail_auth_device_code_poll`: polls an existing device-code flow. When authorized, stores a delegated access token and returns metadata only.
- `graph_mail_auth_status`: reports delegated auth metadata without exposing tokens.
- `graph_mail_auth_clear`: clears stored delegated auth material when `confirm_clear: true`.
- `graph_mail_query`: queries live Graph messages with optional mailbox, folder, search, filter, select, and limit arguments.
- `graph_mail_message_show`: shows one live Graph message by `message_id`.
- `graph_mail_folder_list`: lists live Graph mail folders, optionally under `parent_folder_id`.
- `graph_mail_folder_create`: creates a mail folder only when policy allows mailbox organization writes and the call includes `confirm_write: true`.
- `graph_mail_message_move`: moves a message only when policy allows mailbox organization writes and the call includes `confirm_write: true`.
- `graph_mail_attachment_list`: lists attachments for a message or draft.
- `graph_mail_attachment_get`: shows one attachment as metadata by default; bounded inline content requires explicit `include_content: true`.
- `graph_mail_attachment_download_file`: downloads one permitted inbound document to an allowed local path, enforcing MIME and size limits without returning base64 through MCP. Repeated requests are hash-idempotent.
- `graph_mail_attachment_add`: adds a small file attachment with `name`, `content_type`, and `content_base64` using `@odata.type` `#microsoft.graph.fileAttachment`.
- `graph_mail_attachment_upload_session_create`: creates an upload session for a large file attachment with `name`, positive `size`, and optional content metadata.
- `graph_mail_attachment_upload_chunk`: uploads one chunk to a guarded upload URL with `upload_url`, `content_base64`, `range_start`, `range_end`, and `total_size`, using binary body bytes and explicit `Content-Length` / `Content-Range` headers.
- `graph_mail_attachment_upload_file`: preferred path for local files. Reads a file under an allowed attachment root, creates an upload session, uploads bounded binary chunks internally, and returns compact metadata without exposing base64 content or upload URLs.
- `graph_mail_attachment_delete`: deletes one attachment from a message or draft.
- `graph_mail_draft_create`: creates a new draft message.
- `graph_mail_reply_draft_create`: creates a reply draft from an existing message.
- `graph_mail_reply_all_draft_create`: creates a reply-all draft from an existing message.
- `graph_mail_forward_draft_create`: creates a forward draft from an existing message.
- `graph_mail_ticket_draft_upsert`: idempotently creates or recovers the exact unsent draft authorized by a Work Lifecycle effect claim.
- `graph_mail_ticket_draft_discard`: conditionally deletes that exact tracked draft after explicit confirmation, persists a restart-safe discard intent, and returns a digest-verified disposition receipt for Work Lifecycle reconciliation.
- `graph_mail_ticket_draft_disposition_scan`: observes tracked drafts that Graph reports as sent and persists digest-verified disposition receipts; absence alone is not evidence.
- `graph_mail_ticket_draft_disposition_list`: lists unacknowledged sent or discarded disposition receipts for one reconciliation consumer.
- `graph_mail_ticket_draft_disposition_ack`: acknowledges a receipt only after its consumer has durably reconciled it.
- `graph_mail_draft_update`: updates an existing draft.
- `graph_mail_draft_discard`: deletes an ordinary draft, but refuses Work-linked tracked drafts; use `graph_mail_ticket_draft_discard` for those.
- `graph_mail_draft_send`: sends an existing draft only when policy allows it.

Reply and reply-all tools also accept the explicit `comment_html` mode. This
mode performs a governed two-phase operation: Graph first creates the normal
unsent reply (including its generated recipients and quoted history), then the
surface reads that draft and patches an HTML body containing the authored
`comment_html`, the optional site-configured `reply_signature_name`, and the
preserved quote. Callers provide unsigned `comment_html` when the policy
declares a signature. This keeps paragraph
boundaries, reply-all recipients, quote history, and the unsent state together;
`comment_html` cannot be combined with `comment`, `body_text`, or `body_html`.

## Send Safety

`graph_mail_draft_send` refuses by default.

To send, all of the following must be true:

- `.ai/graph-mail-mcp.json` has `allow_send_draft: true`.
- The tool call includes `confirm_send: true`.
- If `send_approval_token` is configured, the tool call includes the same `approval_token`.
- The mailbox is allowed by `allowed_mailboxes`, when an allowlist is configured.

There is intentionally no direct-send tool. Agents must create or update a draft first, then send that existing draft only through the policy-gated send path.

## Mailbox Organization Safety

`graph_mail_folder_create` and `graph_mail_message_move` refuse by default.

To create folders, all of the following must be true:

- `.ai/graph-mail-mcp.json` has `allow_folder_create: true`.
- The tool call includes `confirm_write: true`.
- If `mailbox_organization_approval_token` is configured, the tool call includes the same `approval_token`.
- The mailbox is allowed by `allowed_mailboxes`, when an allowlist is configured.

To move messages, all of the following must be true:

- `.ai/graph-mail-mcp.json` has `allow_message_move: true`.
- The tool call includes `confirm_write: true`.
- If `mailbox_organization_approval_token` is configured, the tool call includes the same `approval_token`.
- The mailbox is allowed by `allowed_mailboxes`, when an allowlist is configured.

## Agent Guidance

Agents should:

- Prefer `mailbox-mcp` for routine reads from synced local projections.
- Use `graph_mail_query` or `graph_mail_message_show` only when live Graph state is needed.
- Use the attachment tools only for live Graph attachment state, and prefer `mailbox-mcp` for routine reads first.
- Use `graph_mail_attachment_upload_file` for local files instead of base64-printing file content through command output.
- Use draft tools for outbound customer-facing work.
- Never send unless an operator has intentionally enabled sending and provided the required confirmation/approval inputs.

## Verification

```powershell
pnpm --filter @narada-core/graph-mail-mcp test
```
