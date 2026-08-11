# Worker Delegation MCP Defects: Fallbacks, Mocks, Stubs

Scope: current `packages/worker-delegation-mcp` implementation and tests for `target.md`.

## Review metadata

Last reviewed: 2026-08-08
Review revision: eeb464249eda123f844997481aaaf26d3b7a5880

Finding numbers are stable IDs (`WD-01` through `WD-12`). `resolved` means
the implementation and a focused regression test now cover the original
condition. `open` means the documented gap remains. `accepted limitation`
means the behavior is deliberate first-version scope, not an accidental
fallback. This register is an implementation review; it is not a claim that
every open item has been fixed.

| ID | Finding | Status | Current evidence |
| --- | --- | --- | --- |
| WD-01 | Fake Codex runtime in successful tests | open | Optional real-carrier coverage exists; the default fixture remains a fixture. |
| WD-02 | Protocol smoke bypasses packaged bin | open | Package-bin smoke coverage remains a separate gap. |
| WD-03 | Hand-written partial TOML parser | open | Parser replacement or explicitly bounded syntax contract is still required. |
| WD-04 | Silent malformed trusted-root lines | open | Policy parsing still needs strict malformed-input diagnostics. |
| WD-05 | Missing worker output misclassified | resolved | Output-contract mapping and regression coverage preserve an explicit absent-file condition. |
| WD-06 | Fallback empty normalized fields | open | Invalid output still needs a fully typed non-worker-authored result shape. |
| WD-07 | Non-object config coerced to empty object | open | Strict schema rejection remains required. |
| WD-08 | Truthy coercion of skip-git flag | open | Boolean input validation remains required. |
| WD-09 | Generic CLI flags accept missing values | open | Startup argument validation remains incomplete. |
| WD-10 | Output-reference failures genericized | resolved | MCP output-reference failures now map to a typed materialization error. |
| WD-11 | Unasserted fixture fallback branch | open | Prompt/output evidence assertion remains required. |
| WD-12 | Missing-executable fixture only | open | Post-spawn runtime-startup failure coverage remains required. |

## Findings

1. Successful worker execution tests use a fake Codex runtime.

   Evidence: `packages/worker-delegation-mcp/test/worker-delegation-mcp.test.ts` writes a temporary script named `exec`, configures `codexCommand: process.execPath`, and relies on Node running that script instead of invoking the real `codex exec`.

   Why this is a defect: this proves the server can spawn a Codex-shaped fixture, but it does not prove compatibility with the real Codex CLI, real `codex exec --json` event shape, real `--output-schema` behavior, or real `-o last_message.json` behavior.

2. The protocol smoke test starts the built source file, not the packaged binary.

   Evidence: `packages/worker-delegation-mcp/test/protocol-smoke.test.ts` resolves `../src/main.js` and runs it with `process.execPath`.

   Why this is a defect: the package contract says the binary name is `worker-delegation-mcp`, but the smoke test bypasses the package `bin` surface. A broken bin mapping or packaging issue can pass this test.

3. Config file loading is still a hand-written partial TOML parser.

   Evidence: `packages/worker-delegation-mcp/src/policy.ts` implements `parseConfigFile` and `parseTomlValue` manually with regexes and comma-splitting arrays.

   Why this is a defect: the target says the server accepts one TOML file. This parser is a strict subset, not TOML. It cannot reliably parse legitimate TOML features such as inline comments, escaped strings, dotted keys, arrays containing commas in strings, or richer scalar syntax.

4. Trusted-project root loading silently ignores unsupported or malformed trust-config lines.

   Evidence: `packages/worker-delegation-mcp/src/policy.ts` `parseTrustedProjectRootsFromTrustConfig` skips empty/comment lines, resets on unknown sections, and ignores non-matching lines inside project sections.

   Why this is a defect: trusted roots are part of execution policy. Silent skipping can turn malformed trust configuration into missing roots without a precise configuration error, which is fallback behavior in policy loading.

5. Missing worker output must remain an explicit absent-output condition.

   Status: resolved on review revision
   `eeb464249eda123f844997481aaaf26d3b7a5880`.

   Evidence: `packages/worker-delegation-mcp/src/output-contract.ts` maps a
   missing output file to the typed absent state, and
   `packages/worker-delegation-mcp/test/worker-delegation-mcp.test.ts` asserts
   the absent-output response. The implementation no longer relies on
   `invalid_shape` to describe a missing file.

6. Failed worker results still use fallback empty normalized fields.

   Evidence: `packages/worker-delegation-mcp/src/worker-tools.ts` sets `summary: output?.summary ?? ""`, `deliverables: output?.deliverables ?? []`, `open_questions: output?.open_questions ?? []`, and `next_actions: output?.next_actions ?? []` even when parsing failed.

   Why this is a defect: this is less misleading than synthetic prose, but it still fills worker-authored fields with fallback values. Consumers reading `result.json` must notice `status` or `worker_output_error` to distinguish empty worker output from no valid worker output.

7. Non-object `config` tool input is silently treated as an empty config object.

   Evidence: `packages/worker-delegation-mcp/src/policy.ts` calls `asRecord(input.config)`, and `asRecord` returns `{}` for non-object values.

   Why this is a defect: the tool schema says `config` is an object. Passing a string, array, boolean, or number should be rejected as invalid input, not ignored as if no config was supplied.

8. `skip_git_repo_check` uses broad JavaScript truthiness instead of schema validation.

   Evidence: `packages/worker-delegation-mcp/src/worker-tools.ts` sets `const skipGitRepoCheck = Boolean(args.skip_git_repo_check)`.

   Why this is a defect: values such as `"false"`, `"0"`, arrays, or objects become `true`. That is silent coercion on a policy-relevant execution flag.

9. Generic CLI flags with missing values are silently converted to booleans.

   Evidence: `packages/worker-delegation-mcp/src/mcp-server.ts` only special-cases missing values for allowed-root, allowed-sandbox, and allowed-config-key. Other flags, including `--config`, `--run-root`, `--audit-log-dir`, and `--codex-command`, become `true` when no value follows.

   Why this is a defect: malformed startup invocations can be accepted and then defaulted or ignored downstream. For example, `--config` with no value is not loaded because policy loading only reads it when it is a string.

10. Output-reference read failures must remain typed materialization failures.

    Status: resolved on review revision
    `eeb464249eda123f844997481aaaf26d3b7a5880`.

    Evidence: `packages/worker-delegation-mcp/src/mcp-server.ts` maps output
    reference read and materialization failures to
    `worker_output_materialization_failed`; focused worker-output tests cover
    missing or unreadable references.

11. The main successful test fixture contains a fallback branch that is not asserted.

    Evidence: `packages/worker-delegation-mcp/test/worker-delegation-mcp.test.ts` writes a deliverable description of `prompt.includes("Task") ? "saw task" : "missing task"`, but the test does not assert the deliverable description.

    Why this is a defect: this weakens the test as evidence for prompt construction. The fixture can report a degraded `"missing task"` deliverable and the test still passes because it checks only summary/session/artifacts.

12. The spawn-failure regression still depends on a missing executable fixture.

    Evidence: `packages/worker-delegation-mcp/test/worker-delegation-mcp.test.ts` configures `codexCommand: join(root, "missing-codex.exe")`.

    Why this is a defect: testing startup failure is valid, but this remains a stubbed runtime condition. It proves missing executable handling only; it does not cover real runtime startup failures after process creation, such as invalid Codex arguments, auth failures, or early CLI initialization errors.

## Current Non-Defects

- Target-defined defaults such as default runtime `codex`, default role `specialist`, default sandbox, default resume task, and default output slice values are not defects by themselves.
- Rejecting `worker_autopilot` as `worker_unknown_tool` is target-defined first-version scope, not a stub.
- Empty `events.jsonl` and `diagnostic.log` placeholders are target-defined run-record artifacts when the runtime emits no events or diagnostics.
