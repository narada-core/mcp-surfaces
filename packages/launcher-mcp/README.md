# @narada-core/launcher-mcp

Read-only MCP surface for Narada launcher registry inspection, option modeling, startup planning, and coherence checks.

This surface does not launch agents or execute PowerShell. It reports registry and plan state only.

Launcher exposes two explicit projections over the same read-only handlers:
`stdio` is the compatibility and rollback projection, while `factory` is
the authority-shared PC Site service canary. The factory projection never
bypasses NARS action admission. `launcher_doctor` reports the active
projection and its reconnect posture.

## Telemetry

Telemetry is optional and disabled unless the site enables `.ai/mcp-telemetry.json`. When enabled, this surface emits metadata-only tool status events and does not persist registry records, launch plans, or other raw result payloads.

## Tools

- `launcher_guidance` / `launcher_doctor` - describe the surface and current
  projection posture.
- `launcher_options_list` / `launcher_option_matrix` - inspect admitted launch
  options and their compatibility matrix.
- `launcher_registry_list` / `launcher_plan` - read the registry and produce
  a bounded startup plan.
- `launcher_coherence_check` - validate the read-only registry/plan
  relationship.

## Verification

```powershell
pnpm --filter @narada-core/launcher-mcp test
```
