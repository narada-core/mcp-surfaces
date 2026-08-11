# @narada-core/quota-meter-mcp

## Verification

```powershell
pnpm --filter @narada-core/quota-meter-mcp test
```

Host-level MCP surface for the local `quota-meter` CLI. It reads current
Codex/Kimi glide status and manages the transparent desktop overlay without
handling provider credentials itself.

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

By default the surface targets `<src-root>\quota-meter` on Windows. Set
`QUOTA_METER_ROOT` when the checkout is elsewhere. `QUOTA_METER_NODE` can
override the Node executable, and `QUOTA_METER_STATE_ROOT` can override the
overlay PID/position directory. Native `codex login` and `kimi login` remain
the authentication mechanisms; the surface never reads or returns tokens.
On Windows, the surface also restores the standard `windir` environment value
from `SystemRoot` when it is missing so the WPF overlay font subsystem can
initialize correctly.

## Quick start

```text
pnpm --filter @narada-core/quota-meter-mcp test
```
