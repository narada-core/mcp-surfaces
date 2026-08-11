# `@narada-core/execution-contract`

## Verification

```powershell
pnpm --filter @narada-core/execution-contract test
```

This package defines the shared execution identity used when a task or delegated workflow crosses from intent into execution.

`ExecutionBinding` records the authorized workspace, executor kind/profile/id, optional repository and Site roots, and a stable correlation key. Consumers normalize it before persistence and use `executionRequestFingerprint` to detect idempotency-key reuse with a different request.

The contract is deliberately substrate-neutral. It does not launch workers, mutate files, own task lifecycle state, or infer authority from a path. Task-lifecycle and delegated-task surfaces own those behaviors and persist the normalized binding in their own records.

Bindings must use absolute paths. A binding is evidence of the selected execution locus, not an authorization grant; the owning surface still applies its own policy and root checks.
