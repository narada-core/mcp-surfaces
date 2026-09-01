# @narada-core/mailbox-mcp

Mailbox domain surface for finite synchronization, mechanical admission, durable domain events, and bounded reads of site-local mailbox projections.

Mailbox owns cloud-to-local synchronization, first-observation identity, admission decisions, and its transactional outbox. Scheduler consumes those events and activates SOPs. Work Lifecycle, not Mailbox, owns tickets.

## Boundary

- Allowed: run finite idempotent sync generations and reconcile first observations.
- Allowed: mechanically admit a first-observed immutable fact exactly once and publish the frozen decision.
- Allowed: consume scoped, topic-filtered mailbox events through durable acknowledgements.
- Allowed: read site-local synced mailbox projection files.
- Allowed: list accounts, list messages, show one message, search messages, show a thread.
- Not allowed: live Microsoft Graph queries.
- Not allowed: creating, updating, sending, or deleting mail.
- Not allowed: PowerShell or arbitrary command execution.

Use `@narada-core/graph-mail-mcp` for live Microsoft Graph reads and draft lifecycle operations.

## Local Data Contract

By default, the server scans these roots under the site root:

```text
.ai/mailboxes
.ai/synced-mailboxes
operator-surfaces/mailboxes
```

A site can override the roots with `.ai/mailbox-mcp.json`:

```json
{
  "roots": [".ai/mailboxes", "operator-surfaces/helpdesk-mail"]
}
```

Roots must resolve inside the site root. Files are scanned recursively and must end in `.json` or `.jsonl`.

Supported JSON shapes:

- A single message object.
- An array of message objects.
- An object with `messages: [...]`.
- An object with Microsoft Graph-style `value: [...]`.
- JSONL with one message object per line.

Common fields are normalized from Graph/Outlook-like names:

```json
{
  "id": "msg-123",
  "conversationId": "thread-456",
  "mailbox_id": "support@example.test",
  "folder": "Inbox",
  "subject": "Customer follow-up",
  "from": { "address": "customer@example.test" },
  "to": [{ "address": "support@example.test" }],
  "receivedDateTime": "2026-06-04T16:00:00.000Z",
  "isRead": false,
  "bodyPreview": "Can you send an update?",
  "body": { "contentType": "text", "content": "Can you send an update on the open ticket?" },
  "attachments": [{ "name": "screenshot.png", "size": 1234 }]
}
```

The normalized output uses stable fields such as `message_id`, `mailbox_id`, `folder`, `thread_id`, `subject`, `from`, `to`, `received_at`, `unread`, `preview`, `body_text`, `body_html`, and `attachments`.

## Tools

- `mailbox_doctor`: reports roots, scan count, message count, and invalid projection records.
- `mailbox_accounts_list`: lists discovered mailbox accounts, folders, total messages, unread count, and latest message time.
- `mailbox_messages_list`: lists messages with optional `mailbox_id`, `folder`, `unread`, `since`, `before`, and `query` filters.
- `mailbox_message_show`: shows one message by `message_id`; includes plain text body by default.
- `mailbox_search`: searches subject/body/address/category text.
- `mailbox_thread_show`: shows messages in one conversation/thread.
- `mailbox_sync_generation`: runs one finite durable synchronization generation.
- `mailbox_message_admit`: freezes one decision for the fact cited by a first-observed event.
- `mailbox_admission_show`: reads that canonical decision without reevaluating policy.
- `mailbox_outbox_*`: registers immutable scoped subscriptions, lists matching events, and records bounded effect receipts.
- `mailbox_fact_show`: reads one immutable discovered-message fact with a safe body/metadata projection by default; explicit full payload reads are bounded. Attachment bytes are never part of the normal fact projection.

## Agent Guidance

SOP actions are the units of work. A mailbox admission result is a neutral source envelope; downstream SOPs decide whether and how to associate it with a ticket. Site Loop may observe this chain but does not execute mailbox synchronization or ticket reconciliation.

## Verification

```powershell
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml mailbox
pnpm --filter @narada-core/mailbox-mcp test
```

The admitted operational authority is the shared Rust surface. Node and Bun
implementations are not selected by any runtime profile.
