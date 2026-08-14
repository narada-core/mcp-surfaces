# @narada-core/worker-delegation-mcp

Policy-gated MCP authority for bounded delegated worker runs.

## Runtime authority

The admitted implementation is native Rust in `packages/shared/mcp-surfaces-native/native`. It launches only the native Rust `narada-agent-runtime-server`; Node, Bun, and the historical TypeScript implementation are not admitted runtime paths. TypeScript artifacts in this package remain compatibility/build-time material and are not the carrier authority.

The native surface resolves an immutable invocation plan before launch. Callers choose cognition (`low`, `medium`, or `high`) and authority (`read`, `write`, or `command`), but cannot override the provider or model outside that plan. Credentials are projected at the process boundary and never returned.

## Typical workflow

1. Inspect `worker_policy_inspect` and optionally `worker_cognition_defaults_inspect`.
2. Preflight cwd and authority with `worker_config_resolve`.
3. Launch with `worker_run` or the write-specialized `worker_edit`.
4. Rediscover with `worker_runs_list`; inspect with `worker_run_status` or the non-polling `worker_run_wait`.
5. Read bounded artifacts through `worker_output_show`.

Minimal launch:

```json
{
  "intent": { "instruction": "Inspect the repository and report the requested evidence." },
  "constraints": {
    "cwd": "<site-root>",
    "authority": "read",
    "cognition": "low"
  }
}
```

`worker_run` returns a durable `run_id` immediately. `worker_resume` requires the prior `worker_session_id` plus a new intent. `worker_run_batch` accepts at most 50 run requests. `worker_run_reap` requires an explicit reason and `force: true` for a nonterminal run.

## Public contract

Every tool has a named, closed, bounded input schema. Run lists and batches are bounded; artifact reads page at no more than 256,000 characters; run records are confined to the configured Site worker root; cwd must be within an admitted root. Unknown arguments are rejected before authority code runs.

The read tools are guidance, policy/default inspection, config resolution, status/list/current-state reads, synthesis, dashboard projection, affordance inspection, and artifact paging. Mutation tools are cognition-default update, run, edit, resume, reap, and batch launch.

## Validation

The default package test exercises the native Rust authority:

```powershell
cargo test --locked --manifest-path packages/shared/mcp-surfaces-native/native/Cargo.toml worker_delegation
```

Legacy TypeScript/Bun/Node scripts are retained only for explicit compatibility comparison and are not evidence for the admitted carrier runtime.
