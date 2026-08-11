# @narada-core/site-registry-mcp

## Verification

```powershell
pnpm --filter @narada-core/site-registry-mcp test
```

User Site MCP surface for the canonical Narada Site Registry.

It exposes read-only registry inspection and reconciliation planning:

## Tools

- `site_registry_guidance`
- `site_registry_doctor`
- `site_registry_command_map`
- `site_registry_list`
- `site_registry_show`
- `site_registry_discover_plan`

The surface delegates to the canonical `narada sites registry ...` command exports. It does not read registry storage directly, and discovery is always forced to dry-run posture. Registry mutation remains behind the operator console's explicit plan/apply gateway.

The registrar owns injection. This surface has `user_site` scope and is included in the default User Site carrier bindings.
