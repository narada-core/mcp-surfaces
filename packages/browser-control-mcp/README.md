# @narada-core/browser-control-mcp

Bounded host-level browser control for authenticated UX verification.

The admitted runtime authority is the shared native Rust executable
`narada-mcp-surfaces --surface-id browser-control --native-authority`. The
TypeScript package preserves descriptor and parity-oracle sources; it is not a
runtime fallback for native carrier profiles.

The surface attaches only to an explicitly selected, already-running browser
profile and session through a loopback Chrome DevTools Protocol endpoint. It
does not launch browsers, perform login, extract cookies or credentials, run
arbitrary JavaScript, or expose unrestricted CDP.

Every attachment requires an exact HTTP(S) origin allowlist. Navigation and
DOM actions are limited to those origins. Login, submission, and destructive
intent must set `confirmed: true`; password, token, cookie, secret, and
authentication fields are refused regardless of confirmation.

Use the initial browser sign-in interactively, then attach with
`browser_control_attach` and verify routes using accessibility snapshots,
screenshots, bounded waits, clicks, fills, and assertions. Large snapshots and
screenshots are returned through the shared `mcp_output_show` output-ref
reader.

The browser lifecycle remains owned by the operator/browser host. Detaching
only closes this MCP connection.

## Tools

- `browser_control_guidance` - workflow, policy, and boundary guidance.
- `browser_control_attach` / `browser_control_detach` - bind and release an
  explicitly selected CDP session.
- `browser_control_status` / `browser_control_session_inventory` - inspect
  connection and bounded host-session state.
- `browser_control_navigate`, `browser_control_wait`, `browser_control_click`,
  `browser_control_fill`, and `browser_control_assert` - perform allowlisted,
  bounded DOM actions.
- `browser_control_accessibility_snapshot` and `browser_control_screenshot` -
  obtain bounded UX evidence.

## Verification

```powershell
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml browser_control_authority
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml --test browser_control_protocol
pnpm --filter @narada-core/browser-control-mcp test
```
