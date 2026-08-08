# @narada-core/project-state-mcp

Read-only local-site MCP projection for a Narada project's virtual project-state
registry.

The adapter owns no SQLite schema and does not mutate a site. It invokes the
site-owned `scripts/project-state-cli.mjs` with a fixed executable, fixed
project root, and bounded stdout. The site's authored SQL snapshot remains the
authority; generated SQLite/JSON files are derived outputs. Every tool is
replayable, virtual-only, and disabled by default in the local-site projection.

## Tools

- `project_state_guidance`
- `project_state_doctor`
- `project_state_command_map`
- `project_state_program_list` / `project_state_program_show`
- `project_state_project_list` / `project_state_project_show`
- `project_state_matrix`
- `project_state_gaps`
- `project_state_handoff`
- `project_state_standards_list` / `project_state_standard_show`
- `project_state_applicability`
- `project_state_standard_trace` / `project_state_standard_gaps`
- `project_state_validate`

`project_state_handoff` returns the site's auditable virtual-only release
summary: lifecycle/maturity cells for every object, repository evidence with
replay commands, every deferred gate, and explicit re-entry triggers. It does
not grant physical, qualification, supplier, external-evidence, or flight
credit.

The standards tools expose the site's bounded applicability profile and trace
internal control paraphrases to program, project, object, lifecycle cell,
maturity, process phase, repository evidence, project-defined review gate, and
open gap. They do not reproduce standard text or claim ISO conformity,
certification, qualification, or flight credit.

The projection receives `--project-root {site_root}`. Callers cannot replace the
root or CLI path through tool arguments.

## Verification

```powershell
pnpm --filter @narada-core/project-state-mcp test
```
