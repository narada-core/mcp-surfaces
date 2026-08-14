# @narada-core/calendar-mcp

Policy-gated native Rust Microsoft Graph calendar authority for live reads and guarded event management. Credentials remain server-bound inside the native process.

## Configuration

The server reads `.ai/calendar-mcp.json` from the site root.

```json
{
  "graph_base_url": "https://graph.microsoft.com/v1.0",
  "allowed_mailboxes": ["calendar@example.com"],
  "allow_event_writes": false,
  "write_approval_token": null
}
```

Authentication follows the Graph mail surface: `GRAPH_ACCESS_TOKEN`, `MS_GRAPH_ACCESS_TOKEN`, or client credentials from `GRAPH_TENANT_ID`, `GRAPH_CLIENT_ID`, and `GRAPH_CLIENT_SECRET`. Site `.env` and parent workspace `.env` files are read before process environment.

## Tools

- `calendar_doctor` - inspect readiness, auth mode, policy, and configured mailboxes.
- `calendar_list` - list calendars for an allowed mailbox.
- `calendar_event_query` - query calendar view events over an explicit time window.
- `calendar_event_show` - read one event.
- `calendar_event_create` - create an event only when policy enables writes and `confirm_write=true`.
- `calendar_event_update` - update an event only when policy enables writes and `confirm_write=true`.
- `calendar_event_delete` - delete an event only when policy enables writes and `confirm_write=true`.

Writes are refused by default and audited when attempted.

## Telemetry

Calendar telemetry is optional and disabled unless the site enables `.ai/mcp-telemetry.json`. Audit records under `.ai/audit/calendar-mcp.jsonl` remain the authoritative evidence for write attempts and are not replaced by telemetry. When telemetry is enabled, this surface emits metadata-only tool status events; it does not persist event bodies, attendees, tokens, approval tokens, raw arguments, raw Graph responses, or other result payloads.

## Verification

```powershell
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml calendar
```
