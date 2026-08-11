# @narada-core/mcp-affordances

## Verification

```powershell
pnpm --filter @narada-core/mcp-affordances test
```

Shared Narada MCP affordance schema and validation helpers.

Affordance documents are UI-neutral. MCP surfaces use them to describe operator-facing views, actions, links, refresh hints, and safety posture without coupling to AgentWebUI or any other renderer.

Renderers should treat affordance documents as declarative hints. Actual authority remains with MCP tool schemas and policy checks.
