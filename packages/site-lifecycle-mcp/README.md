# `@narada-core/site-lifecycle-mcp`

Governed MCP surface aligned with Narada Site lifecycle CLI commands. It
provides bounded discovery, planning, relation validation, doctor output, and
authority preflight; configuration mutations remain explicitly gated by the
serving Site and operator policy.

## Tools

- `site_lifecycle_guidance`, `site_lifecycle_doctor`,
  `site_lifecycle_command_map`
- `site_create_presets_list`, `site_create_plan`
- `site_list`, `site_discover`, `site_show`, `site_doctor`
- `site_lifecycle_kinds`, `site_lifecycle_preflight`
- `site_relation_list`, `site_relation_validate`
- `site_authority_preflight`

The lifecycle tools are exposed with the following effect posture:

- Read-only inspection/planning: all tools above except `site_discover`,
  `site_init`, and `site_deps_sync`.
- Gated mutations: `site_discover` refreshes the discovery registry;
  `site_init` initializes a Site; and `site_deps_sync` repairs shared package
  links and provenance. These tools return a plan unless `execute: true` is
  supplied. `site_init` and `site_deps_sync` additionally require an
  `authority_basis` object.

## First-use workflow

Call `site_lifecycle_guidance`, inspect the command map and doctor posture,
then use the relevant list/show or plan tool. For a mutation, first review the
returned plan and then repeat the call with `execute: true` (and the required
`authority_basis` where indicated). A plan is not an apply; callers must
satisfy the Site authority and operator gate before any lifecycle mutation.

## Boundary

The surface projects Narada's Site lifecycle contract and does not become the
canonical Site registry, a shell, or a general filesystem mutation surface.
The Site root and CLI entrypoint are selected by server configuration, not by
untrusted tool arguments.

## Verification

```powershell
pnpm --filter @narada-core/site-lifecycle-mcp test
```

The package suite covers protocol smoke, Site-fabric lifecycle behavior, plan
and doctor readback, authority refusal, and disposable fixture cleanup.
