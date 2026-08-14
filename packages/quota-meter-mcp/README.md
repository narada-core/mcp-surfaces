# @narada-core/quota-meter-mcp

Host-level native Rust MCP surface for Codex/Kimi quota posture and the
transparent desktop overlay. Provider credentials remain owned by the native
provider clients and are never returned through MCP.

## Tools

| Tool | Purpose |
| --- | --- |
| `quota_meter_guidance` | Explain the surface workflow and boundaries. |
| `quota_meter_glide_status` | Read current quota windows and glide factors with native CLI authentication, never prompting. |
| `quota_meter_overlay_status` | Inspect running state, PID, and persisted overlay position. |
| `quota_meter_overlay_start` | Start the overlay with provider selection and refresh interval. |
| `quota_meter_overlay_stop` | Stop the quota-meter-owned overlay. |

The overlay itself can also be closed with its faint `×` button, or moved by
dragging its header. Its position is persisted by `quota-meter` under the
configured local state root.

## Configuration

The Rust authority talks to `codex app-server` over stdio and to the Kimi usage
endpoint over bounded HTTPS. Native `codex login` and `kimi login` remain the
authentication mechanisms. It does not launch an interactive login.

The Windows overlay remains a WPF PowerShell host, but it refreshes by invoking
the already-running native surface executable in quota-query mode; Node and Bun
are not in the operational path. By default the script is found at
`<src-root>\quota-meter\src\overlay.ps1`. Set `QUOTA_METER_ROOT` or
`QUOTA_METER_OVERLAY_SCRIPT` when it is elsewhere, and
`QUOTA_METER_STATE_ROOT` to relocate bounded PID/position/telemetry state.

## Quick start

```powershell
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml quota_meter
```
