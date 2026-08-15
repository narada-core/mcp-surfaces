# Delegation target retired

The historical TypeScript delegation target is retired. The authoritative contract is the native Rust implementation and tests under `packages/shared/mcp-surfaces-native/native`.

Do not recreate a TypeScript MCP entrypoint or package for delegation. Update the native Rust contract and its bounded tests instead.
