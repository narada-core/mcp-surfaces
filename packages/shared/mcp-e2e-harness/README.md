# MCP E2E Harness

## Verification

```powershell
pnpm --filter @narada-core/mcp-e2e-harness test
```

The mcp-e2e-harness package contains generic mechanics for real MCP end-to-end
tests. It does not create Site configuration, define surface policy, provide
fake domain behavior, or make assertions about a surface.

It provides:

- bounded JSONL and Content-Length MCP child spawning and request/response transport;
- protocol smoke handshakes with server-name and required-tool checks;
- deterministic child shutdown with a bounded kill grace period;
- a Rust/Win32 Job Object `TestProcessScope` for test-owned child trees and descendant cleanup;
- temporary E2E root creation and cleanup;
- JSON-RPC record and structured-content normalization;
- bounded output-reference page collection with monotonic offset checks;
- bounded JSONL evidence readback and result artifact writing;
- TOML path escaping for generated test policy.
- the repository process audit records PID-plus-creation identities, runtime
  versions, helper hash, and scoped conhost.exe residual evidence rather
  than pinning mutable tool versions.

Surface tests remain responsible for their Site fabric, registrar/loader
configuration, provider or carrier authority, assertions, and cleanup of
domain-specific resources.

On Windows, pass a `TestProcessScope` through `spawnJsonlMcpServer` or `spawnContentLengthMcpServer` for deterministic ownership. The scope launches children suspended, assigns them to a kill-on-close Job Object, explicitly terminates the job before joining output relays, and requires tests to close and assert the scope before success.
