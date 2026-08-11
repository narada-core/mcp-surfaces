# @narada-core/worker-delegation-mcp

Policy-gated MCP surface for delegating bounded work to a worker runtime.

The surface starts or resumes one worker run at a time, records the worker request and artifacts under a run directory, and returns deterministic text plus authoritative `structuredContent`. Large results are materialized as `worker_output:*` refs and can be read with `worker_output_show`.

Supported runtimes: `codex` (Codex CLI) and `narada-agent-runtime-server` (Narada Agent Runtime Server over JSONL stdio). DeepSeek is available as the NARS provider `deepseek-api`, not as a direct worker runtime. The worker prompt includes a recursion guard: workers must not call `worker_*` MCP tools.

## Quick Start

Start a read-only audit worker with `worker_run`:

```json
{
  "intent": {
    "instruction": "List all TypeScript files under src/ and report their sizes.",
    "mode": "audit_only"
  },
  "constraints": {
    "cwd": "<src-root>/mcp-surfaces",
    "authority": "read",
    "cognition": "low"
  }
}
```

The call returns immediately with `run_id` and `status: "running"`. Poll `worker_run_status` with the returned `run_id` to collect the result, or use `worker_run_wait` for a bounded wait.

## Tools

- `worker_policy_inspect`: inspect the active delegation policy.
- `worker_run`: start one new delegated worker run with explicit constraints (cwd, authority, cognition, mode).
- `worker_edit`: start one new edit-capable worker run using write authority and low cognition; this is worker delegation, not a deterministic filesystem write tool.
- `worker_resume`: continue one existing worker session.
- `worker_run_status`: inspect a worker run by `run_id` without waiting.
- `worker_runs_list`: list recent runs so a caller can rediscover running or completed work without remembering the run id.
- `worker_run_wait`: wait briefly for one run to finish, returning the latest run payload if the wait times out.
- `worker_output_show`: read materialized worker output by output ref.

Additional MCP surfaces:

- `resources/list` and `resources/read` expose materialized worker output resources.
- `prompts/list` and `prompts/get` expose the `worker_delegation_task` prompt.
- `completion/complete` completes tool names for prompt/tool arguments.
- `logging/setLevel` is accepted as a no-op for client compatibility.

## Policy

The server requires at least one allowed root. A worker `cwd` must be inside an allowed root. Roots can be supplied directly or loaded from trusted projects in a Codex-style trust config.

Defaults:

- runtime: `narada-agent-runtime-server` (default), with `codex` available by explicit override
- default authority: `read`
- default cognition: `low`
- allowed runtimes: `codex`, `narada-agent-runtime-server`
- allowed authorities: `read`, `write`, `command`
- allowed cognition: `low`, `medium`, `high`
- allowed sandboxes: `read-only`, `workspace-write`
- default sandbox: `read-only` (codex), `workspace-write` (narada-agent-runtime-server)
- allowed config keys: `model`, `model_reasoning_effort`
- raw config overrides: disabled
- `danger-full-access`: disabled unless explicitly admitted
- cognition defaults: plain `codex` workers remain runtime-default opaque unless configured by policy or request overrides; `narada-agent-runtime-server` workers resolve provider-specific defaults from the Narada provider registry when available
- worker runs are non-resumable by default; set `constraints.resumable: true` when the returned session should be continued with `worker_resume`
- worker runs are asynchronous by default; set `constraints.wait_for_completion: true` or `wait_for_completion: true` on `worker_edit` when the caller intentionally wants to block for completion
- max parallel runs: `10`
- max prompt bytes: `1048576`
- max output bytes: `2097152`
- max run time: `1800000` ms

Only a small environment allowlist is passed to workers: `PATH`, `USERPROFILE`, `HOME`, `APPDATA`, `LOCALAPPDATA`, `CODEX_HOME`, `CODEX_MODEL`, `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, `DEEPSEEK_API_BASE_URL`, `KIMI_API_KEY`, `KIMI_CODE_API_KEY`, `MOONSHOT_API_KEY`, `NARADA_AI_MODEL`, `NARADA_AI_THINKING`, and `NARADA_WORKER_MCP_CONFIG` when present. For `narada-agent-runtime-server`, worker-delegation resolves a real Narada Site root before launch and projects `NARADA_SITE_ROOT` plus `NARADA_WORKSPACE_ROOT` into the worker environment. When `constraints.overrides.model` or cognition defaults resolve a model, it projects `NARADA_AI_MODEL`; for `codex-subscription`, it also projects `CODEX_MODEL`. When reasoning effort resolves, it projects `NARADA_AI_THINKING`. Run records store environment key names, not secret values.

## Run

```powershell
pnpm --filter @narada-core/worker-delegation-mcp build
node <src-root>/mcp-surfaces/packages/worker-delegation-mcp/dist/src/main.js --allowed-root <src-root>/mcp-surfaces --run-root D:/tmp/worker-runs
```

Common flags:

- `--allowed-root <path>`: add an allowed worker cwd root; repeatable.
- `--roots-from-trust-config <path>`: admit trusted project roots from a Codex-style config.
- `--run-root <path>`: directory for run records and output refs.
- `--audit-log-dir <path>`: append worker delegation audit events.
- `--codex-command <command>`: Codex executable, default `codex`.
- `--codex-command-arg <arg>`: prepend a fixed argument to the Codex runtime invocation; repeatable.
- `--agent-runtime-server-command <command>`: Narada Agent Runtime Server executable, default `narada-agent-runtime-server`.
- `--agent-runtime-server-command-arg <arg>`: prepend a fixed argument to the Agent Runtime Server invocation; repeatable.
- `--allowed-sandbox <mode>`: add an allowed sandbox; repeatable.
- `--allowed-config-key <key>`: allow a Codex config key; repeatable. Omit model overrides unless the runtime account is known to accept the selected model.
- `--cognition-low-model <model>` and `--cognition-low-reasoning-effort <value>`: defaults for low cognition.
- `--cognition-medium-model <model>` and `--cognition-medium-reasoning-effort <value>`: defaults for medium cognition.
- `--cognition-high-model <model>` and `--cognition-high-reasoning-effort <value>`: defaults for high cognition.
- `--provider-registry-path <path>`: path to Narada's provider registry. When present, worker-delegation loads `default_provider`, admitted provider ids, and `providers.<id>.cognition_defaults` for NARS workers.
- `--max-parallel-runs <count>`: maximum simultaneous worker runs, default `10`; enforced for `worker_run`, `worker_edit`, and `worker_resume`.
- `--max-run-ms <ms>`, `--max-prompt-bytes <bytes>`, `--max-output-bytes <bytes>`: set limits.

## E2E Coverage

The package has two explicit real-boundary delegation proofs:

- `test/real-carrier-e2e.test.ts` is the controlled B4 proof with A0 authority. It starts the
  built worker-delegation MCP child, uses the production
  `narada-agent-runtime-server` carrier, calls a bounded local HTTP provider
  fixture, and verifies provider requests, lifecycle events, durable run
  artifacts, and cleanup.
- The external Narada proof at
  `NARADA_REPO_ROOT/packages/agent-web-ui/test/live-delegated-task-launcher-e2e.mjs` is the
  controlled W1 proof with A0 provider authority. It starts the real launcher, carrier, Site-local MCP
  fabric, `nars-session-mcp`, delegated-task MCP, and worker carrier, then
  verifies durable task and worker evidence.

This W1 path is owned by Narada proper rather than this repository; use the
[cross-repository contract register](../../docs/cross-repository-contracts.md#contract-register)
to resolve the external checkout and its current evidence.

Run the B4 proof with:

```powershell
pnpm --filter @narada-core/worker-delegation-mcp test:e2e:carrier
```

The B4/W1 proofs use a bounded local provider fixture (A0). They prove the
local production topology and carrier protocol, not live external-provider
account authority (A1/A2).

Provider registry cognition defaults are provider-specific. For `narada-agent-runtime-server`, resolution precedence is:

1. Request override: `constraints.overrides.model`, `constraints.overrides.reasoning_effort`, or admitted config keys.
2. `providers.<provider-id>.cognition_defaults.<low|medium|high>` from the provider registry.
3. Legacy global cognition defaults from CLI/config flags such as `--cognition-low-model`.
4. Runtime default opaque when no concrete model or reasoning effort is available.

If `constraints.provider` is absent for a NARS worker, the provider registry `default_provider` is used when available. `worker_policy_inspect` exposes both `default_narada_agent_runtime_provider` and `provider_cognition_defaults`; use it before delegating when model cost or capability matters.

For `codex-subscription`, cognition defaults project reasoning effort but do not freeze a registry-derived model into an explicit override. The NARS carrier resolves the model from the live Codex catalog at runtime. An explicit worker model remains authoritative.

## Agent Contract

Agents should use `worker_policy_inspect` before delegating. A delegation request separates non-mechanically-enforceable intent from mechanically-enforceable constraints:

- `intent.instruction`: what the worker is asked to do and how it should report. This is prompt intent, not enforcement.
- `constraints`: the executable bounds the server validates or applies. `cwd` selects the worker directory, `site_root` optionally selects the Narada Site root for `narada-agent-runtime-server`, `authority` selects read, write, or command capability, `cognition` selects the default model and reasoning tier, `resumable` controls whether the returned session can be continued, `wait_for_completion` controls whether the MCP call blocks, and `overrides` carries explicit low-level execution overrides when policy admits them.

The worker MCP surface enforces constraints and records the resolved executor request. It does not admit worker output as task evidence, close work, or create Narada role authority by itself.

Normalized constraints:

- `authority: "read"`: inspection within the admitted root envelope; default sandbox `read-only`.
- `authority: "write"`: edit-capable work within the admitted root envelope; default sandbox `workspace-write`.
- `authority: "command"`: command-capable delegation through governed MCP command surfaces such as `structured-command`; default sandbox `workspace-write`.
- `cognition: "low"`: policy-selected low cognition tier; for NARS workers this usually resolves to the selected provider's low model/reasoning default.
- `cognition: "medium"`: policy-selected medium cognition tier; for NARS workers this usually resolves to the selected provider's default/general model and medium reasoning effort.
- `cognition: "high"`: policy-selected high cognition tier; for NARS workers this usually resolves to the selected provider's strongest admitted model or high reasoning effort.

Delegation mode is explicit intent, not mechanical authority. `intent.mode` may be `audit_only`, `plan_only`, `implement`, or `implement_and_verify`. Read authority defaults to `audit_only`; write and command authority default to `implement`. The mode is recorded as `requested_mode`, included in the worker prompt, and summarized in run/list output so an audit cannot be mistaken for a migration or implementation. Mechanical enforcement still comes from `constraints.authority`, sandbox, allowed roots, and policy.

`constraints.preflight_paths` can declare path capability checks before the worker starts. Each entry has `path`, `access` (`read`, `write`, or `create`), and optional `label`. This is useful for migration work: declare the old authority path as readable and the proposed new repo path as creatable. `constraints.required_mcp_tools` can declare tool names the worker must have. For `narada-agent-runtime-server`, worker-delegation projects those names through `NARADA_WORKER_MCP_CONFIG` so the worker runtime exposes a scoped MCP tool set; other runtimes record the names as worker-verification requirements. Preflight results are recorded in `executor_request.preflight`, `preflight`, and `blocked_paths`; they are also placed in the worker prompt as evidence. For `implement` and `implement_and_verify`, any blocked preflight check fails before worker dispatch with `worker_preflight_blocked`. Use `plan_only` for a dry implementation plan under read authority.

`worker_run` accepts explicit normalized constraints:

- `intent.instruction` (string, required): natural-language task description.
- `intent.mode` (string, default `"implement"`): `"audit_only"`, `"plan_only"`, `"implement"`, or `"implement_and_verify"`.
- `intent.step_id` (string, optional): caller-chosen step identifier.
- `intent.step_kind` (string, optional): `"worker"` for delegated work.
- `intent.acceptance` (object, optional): acceptance criteria such as `required_files`.
- `constraints.cwd` (string, required): working directory inside an allowed root.
- `constraints.site_root` (string, optional): explicit Narada Site root for `narada-agent-runtime-server`; when omitted, the nearest parent Site marker above `cwd` is used.
- `constraints.authority` (string, default `"read"`): `"read"`, `"write"`, or `"command"`.
- `constraints.cognition` (string, default `"low"`): `"low"`, `"medium"`, or `"high"`.
- `constraints.resumable` (boolean, default `false`): enable continuation via `worker_resume`.
- `constraints.wait_for_completion` (boolean, default `false`): block until worker finishes.
- `constraints.verification_budget` (object, optional): advisory verification discipline, with fields such as `focus` (`"focused"` or `"broad"`), `max_commands`, `max_minutes`, `stop_on_first_failure`, `broad_commands_allowed`, and `notes`.
- `constraints.test_budget` (object, optional): advisory test discipline using the same shape as `verification_budget`.
- `constraints.provider` (string, optional): NARS intelligence provider, only valid with `overrides.runtime: "narada-agent-runtime-server"`; `"deepseek-api"` routes DeepSeek through NARS.
- `constraints.overrides` (object, optional): set `runtime` (`"codex"` or `"narada-agent-runtime-server"`), `sandbox`, `model`, or `reasoning_effort`.
- `constraints.preflight_paths` (array, optional): path existence/access checks before delegation.
- `constraints.required_mcp_tools` (array, optional): MCP tool names the worker must have.

`worker_edit` is only MCP surface sugar: it accepts top-level `cwd`, optional `site_root`, `instruction`, optional `resumable`, optional `wait_for_completion`, and optional `overrides`, then mechanically compiles to `authority: "write"`, `cognition: "low"`, and `mode: "implement"`. It may use the worker runtime's admitted tools and MCP surfaces; use deterministic filesystem MCP tools when the requested operation is a direct file mutation rather than delegated agent work. Do not ask a worker to call `worker_run`, `worker_edit`, `worker_resume`, or other `worker_*` tools.

When a resumable run completes, the server records a session entry under `run_root/sessions`. `worker_resume` uses that entry to inherit the original authority, cognition, sandbox, model, reasoning effort, and config unless the caller explicitly overrides them. This keeps resumable `worker_edit` sessions on write authority and low cognition across continuations and MCP restarts.

Worker-starting tools return `schema: "narada.worker.run.v1"` with:

- `status`: `running`, `completed`, `completed_with_errors`, `failed`, or `cancelled`
- `run_id` and `run_dir`
- `worker_session_id`
- `executor_request`
- `resolved_worker_config`
- `requested_mode`, `edits_performed`, `target_state_changed`, `confidence`, `blocked_paths`, `verification`, `preflight`, and `final_checklist`
- `summary`, `deliverables`, `open_questions`, `next_actions`, `changes`, `verification_results`, `verification_budget_respected`, and `broad_unrelated_failures`
- `artifacts` pointing at request, prompt, invocation, event, diagnostic, last-message, result, and schema files
- resumable sessions additionally write `run_root/sessions/<worker_session_id>.json` with the latest inherited execution policy
- `timing`

By default, `worker_run`, `worker_edit`, and `worker_resume` return after launch with `status: "running"`. Use `worker_run_status` with the returned `run_id` to inspect completion and read the final full payload. Use `worker_runs_list` to rediscover recent or still-running work after the main agent has stopped or lost the run id. It is compact by default: each item includes run id, status, requested mode, whether requested mode was inferred for an old run, authority, started/finished timing, a short summary preview, and an error preview. Set `include_summary: true` for full summaries and `verbose: true` for full list item metadata. Use `worker_run_wait` for a bounded wait, for example `timeout_ms: 10000`, when a run is likely near completion. It returns a compact run status by default; set `summary_only: true` for the smallest useful response or `verbose: true` to include the full run payload as `full_run`. Set `wait_for_completion: true` only for intentionally short calls where blocking the main agent is acceptable.

Worker output must explicitly include `edits_performed`, `target_state_changed`, `changes`, `verification`, `verification_budget_respected`, and `broad_unrelated_failures`. The server does not infer implementation state from deliverables. `changes` is for files or target artifacts changed; `verification` is structured check evidence with `status`, `summary`, optional `tool` or `command`, and `command_classification` (`focused`, `broad`, or `not_applicable`). Broad command failures that appear unrelated to the delegated target should be reported separately in `broad_unrelated_failures`.

Runs that produce a valid `last_message.json` but also encounter runtime or tool errors finish as `completed_with_errors`. The error remains on the payload, but the summary and deliverables are preserved as usable worker output.

Worker prompts include MCP-first tool guidance: use available filesystem, git, and structured-command MCP tools for inspection and verification, and avoid direct shell commands for file discovery or file reads when narrower MCP tools can do the work.

A failed runtime call raises `worker_runtime_failed` or `worker_runtime_timed_out` and includes `run_id` and `run_dir` in error details when available. Inspect the run directory for diagnostics.

If a response is materialized, call `worker_output_show` with the returned `output_ref`. `worker_output_show` also accepts a `path` copied from a run artifact entry when that path is inside the worker run root, so callers can read `executor_request.json`, `worker_prompt.txt`, `last_message.json`, or diagnostics without switching tools. It returns exact stored output text and supports `offset` and `limit`.

## Narada Agent Runtime Server

Override the runtime with `overrides.runtime: 'narada-agent-runtime-server'` on `worker_run` or `worker_edit`. This starts a Narada Agent Runtime Server in raw JSONL stdio mode, sends one `session.submit` frame, records the server event stream in `events.jsonl`, and materializes the final `assistant_message` into the normal worker `last_message.json` contract.

This runtime is a carrier-server posture, not a direct model substrate. It projects `NARADA_SITE_ROOT` and `NARADA_WORKSPACE_ROOT` from the worker `cwd` when those variables are absent, so the child runtime has an explicit Site/workspace anchor. On Windows, npm/pnpm `.cmd` shims are unwrapped to `node <agent-runtime-server entrypoint>` before launch so the JSONL pipe is attached to the real server process; the resolved config records the configured command, while `worker_invocation.json` records the actual spawned command. Use this runtime for delegated work that benefits from Narada carrier semantics, Site MCP fabric, durable session evidence, later resume/handoff behavior, or non-Codex model providers.

## DeepSeek Provider

DeepSeek is routed through `narada-agent-runtime-server` with `constraints.provider: "deepseek-api"`. Direct `overrides.runtime: "deepseek-api"` is rejected with a migration diagnostic; use the provider form instead:

```json
{
  "intent": {
    "instruction": "Inspect the issue and report findings.",
    "mode": "audit_only"
  },
  "constraints": {
    "cwd": "<src-root>/mcp-surfaces",
    "site_root": "<src-root>/mcp-surfaces",
    "authority": "read",
    "provider": "deepseek-api",
    "overrides": {
      "runtime": "narada-agent-runtime-server"
    }
  }
}
```

Provider credentials and cognition defaults are loaded through the Narada provider registry path used by NARS. Credentials come from `credential_requirement`; cognition defaults come from `providers.<provider-id>.cognition_defaults`. Run records expose provider names, resolved model/reasoning values, and environment key names, not secret values.

## Example Tool Arguments

```json
{
  "intent": {
    "instruction": "Inspect the failing test and propose the smallest safe fix. Do not edit files.",
    "mode": "audit_only"
  },
  "constraints": {
    "cwd": "<src-root>/mcp-surfaces",
    "authority": "read",
    "cognition": "medium",
    "resumable": false,
    "wait_for_completion": false,
    "overrides": {
      "sandbox": "read-only",
      "reasoning_effort": "medium"
    }
  }
}
```

Migration audit with explicit preflight:

```json
{
  "intent": {
    "instruction": "Audit the proposed authority migration. Do not edit files. Report old-path dependencies, target repo readiness, risks, and verification steps.",
    "mode": "audit_only"
  },
  "constraints": {
    "cwd": "<src-root>/mcp-surfaces",
    "authority": "read",
    "cognition": "medium",
    "wait_for_completion": false,
    "preflight_paths": [
      { "label": "old authority", "path": "<src-root>/narada.revolution", "access": "read" },
      { "label": "new repo", "path": "<src-root>/narada.revolution", "access": "create" }
    ],
    "required_mcp_tools": [
      "local-filesystem-read.fs_glob_search",
      "local-filesystem-read.fs_read_file",
      "structured-command.structured_command_execute"
    ]
  }
}
```

Edit shortcut:

```json
{
  "cwd": "<src-root>/mcp-surfaces",
  "instruction": "Fix the narrow test failure, keep the change scoped, and report what changed.",
  "resumable": false
}
```

Model overrides are intentionally absent from the edit shortcut example. `worker_edit` uses the low cognition defaults; add `overrides.model` only when the active Codex account and runtime are known to support a different model.

## Optional Live Provider Proof

The external-provider E2E is opt-in and resolves the low-cognition provider/model from an explicit worker-runtime registry. It never uses the frozen migration fixture under Narada.

```powershell
$env:NARADA_E2E_WORKER_EXTERNAL_PROVIDER_LIVE = '1'
$env:NARADA_E2E_WORKER_PROVIDER_REGISTRY = 'D:/path/to/worker-provider-registry.json'
pnpm --filter @narada-core/worker-delegation-mcp test:e2e:external-provider
```

The registry must use `narada.carrier.provider_registry.v1`, expose `cognition_defaults.low`, and declare an OpenAI-compatible chat-completions adapter. Missing registry, credential, supported adapter, or runtime prerequisites produce a structured bounded skip with exit code `2`; they are not treated as provider verdicts. A passing live check is evidence that the configured provider/model adapter and runtime path are reachable; it is not a Task Executability verdict or proof of task correctness. The timeout is bounded by `NARADA_E2E_WORKER_EXTERNAL_PROVIDER_TIMEOUT_MS` (5-120 seconds).

## Verification

```powershell
pnpm --filter @narada-core/worker-delegation-mcp test
pnpm --filter @narada-core/worker-delegation-mcp test:e2e:edit
pnpm --filter @narada-core/worker-delegation-mcp test:e2e:site-fabric
```

The E2E commands start the built worker, loader, and filesystem MCP children,
materialize a temporary Site fabric, and write a bounded result artifact under
`.tmp/e2e-results/`. The normal provider test uses a controlled no-credential
fixture authority and proves local provider/model/thinking binding. The
optional external-provider test is the separate live integration proof; it
resolves its low-cognition selection from the explicit registry and reports a
bounded skip when the external authority is unavailable. The same checks are
available from the repo root as:

```powershell
pnpm test:worker-delegation:e2e
```
