# @narada-core/mcp-materialization-contract-native

Shared Rust authority for carrier-materialization ownership, canonicalization,
and generation fingerprints. The native materializer and registrar use this
contract to validate deterministic all-carrier publication without delegating
authority to generated JavaScript.

## Verification

```powershell
cargo test --locked --manifest-path packages/shared/mcp-materialization-contract-native/native/Cargo.toml
pnpm --filter @narada-core/mcp-materialization-contract-native test
```
