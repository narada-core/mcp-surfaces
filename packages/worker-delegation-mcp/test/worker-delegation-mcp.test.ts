import assert from 'node:assert/strict';
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { mkdtempSync } from 'node:fs';
import { join } from 'node:path';
import { buildCodexArgv, createServerState, handleRequest, parseArgs } from '../src/main.js';
import { commandRequiresWindowsShell, runCodexInvocation, type Invocation } from '../src/codex-adapter.js';
import { admitWorkerAiProcessInvocation, releaseWorkerAiProcessInvocation } from '../src/ai-process-invocation.js';
import { resolveWorkingDirectory } from '../src/policy.js';
import { writeCanonicalPlanRegistry } from './canonical-plan-fixture.js';

type RpcResponse = {
  result?: Record<string, any>;
  error?: Record<string, any>;
};

const root = mkdtempSync(join(testTempRoot(), 'worker-delegation-'));
mkdirSync(join(root, '.narada'), { recursive: true });
mkdirSync(join(root, '.ai'), { recursive: true });
writeFileSync(join(root, '.narada', 'site.json'), JSON.stringify({ schema: 'narada.site.v0', site_id: 'worker-test-site' }), 'utf8');
writeCanonicalPlanRegistry({
  databasePath: join(root, '.ai', 'intelligence-registry.db'),
  planRef: 'plan:worker-test-canonical',
  targetSite: 'site:worker-test-site',
  principal: 'principal:worker-test',
  provider: 'kimi-code-api',
  model: 'k3',
});
writeFileSync(join(root, '.narada', 'intelligence-launch-context.json'), JSON.stringify({
  schema: 'narada.intelligence.launch_context.v1',
  user_site_id: 'site:worker-test-user',
  host_site_id: 'site:worker-test-host',
  principal_id: 'principal:worker-test',
  invocation_plan_ref: 'plan:worker-test-canonical',
  registry_db_path: '.ai\\intelligence-registry.db',
  principal_binding: {
    schema: 'narada.intelligence.principal_binding.v1',
    actor: { principal_id: 'principal:worker-test', auth_type: 'test' },
    memberships: [{ registry: 'site-roster', site_id: 'site:worker-test-user', role: 'resident', evidence_ref: 'test:worker' }],
  },
}), 'utf8');
process.env.NARADA_SITE_ROOT = root;
process.env.CODEX_HOME = root;
process.env.CODEX_CONFIG_DIR = root;
process.env.NARADA_PROVIDER_SECRET_STORE = 'disabled';
const runRoot = join(root, 'runs');
const auditLogDir = join(root, 'audit');
const defaultProviderRegistryPath = join(root, 'default-provider-registry.json');
writeFileSync(defaultProviderRegistryPath, JSON.stringify({
  schema: 'narada.carrier.provider_registry.v1',
  default_provider: 'kimi-code-api',
  providers: {
    'openai-api': {
      base_url: 'https://api.openai.com',
      default_model: 'gpt-5.6-sol',
      available_models: ['gpt-5.6-luna', 'gpt-5.6-terra', 'gpt-5.6-sol'],
      cognition_defaults: {
        low: { model: 'gpt-5.6-luna', reasoning_effort: 'low' },
        medium: { model: 'gpt-5.6-terra', reasoning_effort: 'medium' },
        high: { model: 'gpt-5.6-sol', reasoning_effort: 'high' },
      },
      base_url_env_names: ['OPENAI_API_BASE_URL'],
      credential_requirement: { kind: 'none' },
    },
    'kimi-api': {
      base_url: 'https://api.moonshot.ai',
      default_model: 'kimi-k2.7',
      available_models: ['kimi-k2.7'],
      cognition_defaults: {
        low: { model: 'kimi-k2.7', reasoning_effort: 'low' },
        medium: { model: 'kimi-k2.7', reasoning_effort: 'medium' },
        high: { model: 'kimi-k2.7', reasoning_effort: 'high' },
      },
      base_url_env_names: ['KIMI_API_BASE_URL'],
      credential_requirement: { kind: 'none' },
    },
    'kimi-code-api': {
      base_url: 'https://api.kimi.com/coding/',
      default_model: 'k3',
      available_models: ['k3'],
      cognition_defaults: {
        low: { model: 'k3', reasoning_effort: 'low' },
        medium: { model: 'k3', reasoning_effort: 'medium' },
        high: { model: 'k3', reasoning_effort: 'high' },
      },
      adapter_kind: 'openai-compatible-chat-completions',
      base_url_env_names: ['KIMI_CODE_API_BASE_URL'],
      model_env_names: ['KIMI_CODE_MODEL'],
      credential_env_names: ['KIMI_CODE_API_KEY'],
      credential_requirement: { kind: 'api_key_secret', secret_ref: 'test/kimi-code-api', env_names: ['KIMI_CODE_API_KEY'] },
    },
    'anthropic-api': {
      base_url: 'https://api.anthropic.com',
      default_model: 'claude-test',
      available_models: ['claude-test'],
      cognition_defaults: {
        low: { model: 'claude-test', reasoning_effort: 'low' },
        medium: { model: 'claude-test', reasoning_effort: 'medium' },
        high: { model: 'claude-test', reasoning_effort: 'high' },
      },
      base_url_env_names: ['ANTHROPIC_API_BASE_URL'],
      credential_requirement: { kind: 'none' },
    },
    'deepseek-api': {
      base_url: 'https://api.deepseek.com',
      default_model: 'deepseek-test',
      available_models: ['deepseek-test'],
      cognition_defaults: {
        low: { model: 'deepseek-test', reasoning_effort: 'low' },
        medium: { model: 'deepseek-test', reasoning_effort: 'medium' },
        high: { model: 'deepseek-test', reasoning_effort: 'high' },
      },
      base_url_env_names: ['DEEPSEEK_API_BASE_URL'],
      credential_requirement: { kind: 'none' },
    },
    'glm-api': {
      base_url: 'https://open.bigmodel.cn/api/paas/v4',
      default_model: 'glm-test',
      available_models: ['glm-test'],
      cognition_defaults: {
        low: { model: 'glm-test', reasoning_effort: 'low' },
        medium: { model: 'glm-test', reasoning_effort: 'medium' },
        high: { model: 'glm-test', reasoning_effort: 'high' },
      },
      base_url_env_names: ['GLM_API_BASE_URL'],
      credential_requirement: { kind: 'none' },
    },
    'openrouter-api': {
      base_url: 'https://openrouter.ai/api/v1',
      default_model: 'z-ai/glm-5.2',
      available_models: ['z-ai/glm-5-turbo', 'z-ai/glm-5.2'],
      cognition_defaults: {
        low: { model: 'z-ai/glm-5-turbo', reasoning_effort: 'low' },
        medium: { model: 'z-ai/glm-5.2', reasoning_effort: 'medium' },
        high: { model: 'z-ai/glm-5.2', reasoning_effort: 'high' },
      },
      base_url_env_names: ['OPENROUTER_API_BASE_URL'],
      credential_requirement: { kind: 'none' },
    },
    'codex-subscription': {
      base_url: 'codex://local-subscription',
      default_model: 'gpt-5.6-sol',
      available_models: ['gpt-5.6-luna', 'gpt-5.6-terra', 'gpt-5.6-sol'],
      cognition_defaults: {
        low: { model: 'gpt-5.6-luna', reasoning_effort: 'low' },
        medium: { model: 'gpt-5.6-terra', reasoning_effort: 'medium' },
        high: { model: 'gpt-5.6-sol', reasoning_effort: 'high' },
      },
      model_env_names: ['CODEX_MODEL', 'NARADA_CODEX_MODEL'],
      credential_requirement: { kind: 'local_codex_subscription' },
    },
  },
}), 'utf8');
const fakeCodexScript = join(root, 'exec.cjs');
const fakeCodexErrorScript = join(root, 'exec-error-with-output.cjs');
const fakeCodexHangScript = join(root, 'exec-hang.cjs');
const fakeCodexPersistentReconnectScript = join(root, 'exec-persistent-reconnect.cjs');
const fakeCodexTransientReconnectScript = join(root, 'exec-transient-reconnect.cjs');
const fakeCodexPrestartFailureScript = join(root, 'exec-prestart-failure.cjs');
const fakeAgentRuntimeServerScript = join(root, 'agent-runtime-server.cjs');
const platformRootCase = process.platform === 'win32' ? root.toUpperCase() : root;
writeFileSync(fakeCodexScript, `
const fs = require('fs');
const args = process.argv.slice(2);
const lastMessagePath = args[args.indexOf('-o') + 1];
const isResume = args.includes('resume');
let prompt = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => { prompt += chunk; });
process.stdin.on('end', () => {
  const finish = () => {
  process.stdout.write(JSON.stringify({ thread_id: isResume ? 'thread-resumed' : 'thread-created' }) + '\\n');
  const output = {
    summary: isResume ? 'resumed worker ok' : 'worker ok',
    deliverables: [{ path: 'result.txt', description: prompt.includes('Intent') ? 'saw intent' : 'missing intent' }],
    open_questions: [],
    next_actions: ['done'],
    edits_performed: prompt.includes('Implement:'),
    target_state_changed: prompt.includes('Implement:'),
    changes: prompt.includes('Implement:') ? [{ path: 'result.txt', status: 'modified', summary: 'fake edit result' }] : [],
    verification: [{ tool: 'fake-codex', command: null, status: 'passed', summary: 'fake worker completed', command_classification: 'not_applicable' }],
    verification_budget_respected: true,
    broad_unrelated_failures: [],
    exit_interview: null
  };
  if (prompt.includes('Exit interview')) output.exit_interview = {
    ergonomics_feedback: 'fake worker found the exit interview easy to answer',
    friction_points: ['progress visibility was limited'],
    missing_affordances: ['no push notification'],
    observed_incoherencies: ['status naming was too coarse'],
    suggested_improvements: ['surface latest progress in status']
  };
  fs.writeFileSync(lastMessagePath, JSON.stringify(output));
  };
  setTimeout(finish, prompt.includes('batch delayed') ? 150 : 0);
});
`, 'utf8');
writeFileSync(fakeCodexErrorScript, `
const fs = require('fs');
const args = process.argv.slice(2);
const lastMessagePath = args[args.indexOf('-o') + 1];
process.stdin.resume();
process.stdin.on('end', () => {
  process.stdout.write(JSON.stringify({ thread_id: 'thread-error-output' }) + '\\n');
  process.stdout.write(JSON.stringify({ type: 'error', message: 'simulated mcp tool error' }) + '\\n');
  fs.writeFileSync(lastMessagePath, JSON.stringify({
    summary: 'usable output despite tool error',
    deliverables: [],
    open_questions: [],
    next_actions: [],
    edits_performed: false,
    target_state_changed: false,
    changes: [],
    verification: [{ tool: 'fake-codex', command: null, status: 'failed', summary: 'simulated tool error', command_classification: 'not_applicable' }],
    verification_budget_respected: true,
    broad_unrelated_failures: [],
    exit_interview: null
  }));
});
`, 'utf8');
writeFileSync(fakeCodexHangScript, `
process.stdin.resume();
process.stdin.on('end', () => { setInterval(() => {}, 1000); });
`, 'utf8');
writeFileSync(fakeCodexPersistentReconnectScript, `
process.stdin.resume();
process.stdin.on('end', () => {
  let attempt = 0;
  const timer = setInterval(() => {
    attempt += 1;
    process.stdout.write(JSON.stringify({ type: 'error', message: \`Reconnecting... \${attempt}/5 (stream disconnected: os error 10013)\` }) + '\\n');
    if (attempt >= 5) {
      clearInterval(timer);
      setInterval(() => {}, 1000);
    }
  }, 10);
});
`, 'utf8');
writeFileSync(fakeCodexTransientReconnectScript, `
const fs = require('fs');
const args = process.argv.slice(2);
const lastMessagePath = args[args.indexOf('-o') + 1];
process.stdin.resume();
process.stdin.on('end', () => {
  process.stdout.write(JSON.stringify({ type: 'error', message: 'Reconnecting... 1/5 (transient stream disconnected)' }) + '\\n');
  process.stdout.write(JSON.stringify({ thread_id: 'thread-transient-recovered' }) + '\\n');
  fs.writeFileSync(lastMessagePath, JSON.stringify({
    summary: 'transient provider recovered',
    deliverables: [],
    open_questions: [],
    next_actions: [],
    edits_performed: false,
    target_state_changed: false,
    changes: [],
    verification: [],
    verification_budget_respected: true,
    broad_unrelated_failures: [],
    exit_interview: null
  }));
});
`, 'utf8');
writeFileSync(fakeCodexPrestartFailureScript, `
process.stdin.resume();
process.stdin.on('end', () => {
  process.stderr.write('Not inside a trusted directory and --skip-git-repo-check was not specified.\n');
  process.exit(1);
});
`, 'utf8');
writeFileSync(fakeAgentRuntimeServerScript, `
let buffer = '';
process.stdin.setEncoding('utf8');
process.stdout.write(JSON.stringify({ event: 'session_started', session_id: 'carrier-worker-runtime', agent_id: 'worker.agent', mcp_operational_state: 'healthy' }) + '\\n');
process.stdin.on('data', (chunk) => {
  buffer += chunk;
  const lines = buffer.split(/\\r?\\n/);
  buffer = lines.pop() || '';
  for (const line of lines) {
    if (!line.trim()) continue;
    const frame = JSON.parse(line);
    if (frame.method === 'session.submit') {
      const message = frame.params.content;
      if (message.includes('agent runtime provider failure')) {
        process.stdout.write(JSON.stringify({ event: 'turn_started', request_id: frame.id, turn_id: 'turn-provider-failed' }) + '\\n');
        process.stdout.write(JSON.stringify({ event: 'turn_failed', request_id: frame.id, turn_id: 'turn-provider-failed', error: 'API error 429: rate_limit_reached_error: quota exhausted' }) + '\\n');
        continue;
      }
      if (message.includes('agent runtime mcp tool fault')) {
        process.stdout.write(JSON.stringify({ event: 'turn_started', request_id: frame.id, turn_id: 'turn-mcp-tool-fault' }) + String.fromCharCode(10));
        process.stderr.write('[agent-runtime-server] MCP runtime fault narada-mcp-surfaces-filesystem:fs_grep_search');
        setInterval(() => {}, 1000);
        continue;
      }
      if (message.includes('agent runtime no assistant message')) {
        process.stdout.write(JSON.stringify({ event: 'session_started', session_id: 'carrier-worker-runtime-no-assistant', agent_id: 'worker.agent', mcp_operational_state: 'healthy' }) + '\\n');
        process.stdout.write(JSON.stringify({ event: 'turn_started', request_id: frame.id, turn_id: 'turn-no-assistant' }) + '\\n');
        process.stderr.write('pre-assistant diagnostic detail from runtime\\n');
        process.exit(0);
      }
      if (message.includes('agent runtime terminal no assistant output')) {
        process.stdout.write(JSON.stringify({ event: 'session_started', session_id: 'carrier-worker-runtime-terminal-no-assistant', agent_id: 'worker.agent', mcp_operational_state: 'healthy' }) + '\\n');
        process.stdout.write(JSON.stringify({ event: 'turn_started', request_id: frame.id, turn_id: 'turn-terminal-no-assistant' }) + '\\n');
        process.stdout.write(JSON.stringify({ event: 'turn_complete', request_id: frame.id, turn_id: 'turn-terminal-no-assistant', terminal_state: 'completed' }) + '\\n');
        process.stdout.write(JSON.stringify({ event: 'session_closed', request_id: frame.id, terminal_state: 'closed' }) + '\\n');
        process.exit(0);
      }
      if (message.includes('agent runtime assistant message field')) {
        const output = {
          summary: 'agent runtime message-field output ok',
          deliverables: [],
          open_questions: [],
          next_actions: [],
          edits_performed: false,
          target_state_changed: false,
          changes: [],
          verification: [{ tool: 'fake-agent-runtime-server', command: null, status: 'passed', summary: 'assistant message field extracted', command_classification: 'not_applicable' }],
          verification_budget_respected: true,
          broad_unrelated_failures: [],
          exit_interview: null
        };
        process.stdout.write(JSON.stringify({ event: 'turn_started', request_id: frame.id, turn_id: 'turn-message-field' }) + '\\n');
        process.stdout.write(JSON.stringify({ event: 'assistant_message', request_id: frame.id, turn_id: 'turn-message-field', message: JSON.stringify(output) }) + '\\n');
        process.stdout.write(JSON.stringify({ event: 'turn_complete', request_id: frame.id, turn_id: 'turn-message-field', terminal_state: 'completed', delegated_mutation_admitted: true, carrier_mutation_admitted: true }) + '\\n');
        continue;
      }
      if (message.includes('server runtime loose output')) {
        const output = {
          summary: 'loose agent runtime worker ok',
          edits_performed: false,
          target_state_changed: false,
          verification: { tool: 'fake-agent-runtime-server', status: 'passed', summary: 'loose verification object accepted' },
          verification_budget_respected: true,
          broad_unrelated_failures: [],
          exit_interview: {
            ergonomics_feedback: 'loose output preserved',
            friction_points: ['verification object was not an array'],
            missing_affordances: ['normalizer should preserve exit interviews'],
            observed_incoherencies: [],
            suggested_improvements: ['normalize salvageable NARS worker JSON']
          }
        };
        process.stdout.write(JSON.stringify({ event: 'turn_started', request_id: frame.id, turn_id: 'turn-worker-loose' }) + '\\n');
        process.stdout.write(JSON.stringify({ event: 'assistant_message', request_id: frame.id, turn_id: 'turn-worker-loose', content: '\`\`\`json\\n' + JSON.stringify(output, null, 2) + '\\n\`\`\`' }) + '\\n');
        process.stdout.write(JSON.stringify({ event: 'turn_complete', request_id: frame.id, turn_id: 'turn-worker-loose', terminal_state: 'completed', delegated_mutation_admitted: true, carrier_mutation_admitted: true }) + '\\n');
        continue;
      }
      const output = {
        summary: 'agent runtime worker ok',
        deliverables: [{ path: 'server-result.txt', description: 'server runtime saw ' + (message.includes('Intent') ? 'intent' : 'prompt') }],
        open_questions: [],
        next_actions: ['done'],
        edits_performed: false,
        target_state_changed: false,
        changes: [],
        verification: [{
          tool: 'fake-agent-runtime-server',
          command: null,
          status: 'passed',
          summary: 'fake server completed env=' + JSON.stringify({
            NARADA_AI_MODEL: process.env.NARADA_AI_MODEL || null,
            NARADA_AI_THINKING: process.env.NARADA_AI_THINKING || null,
            CODEX_MODEL: process.env.CODEX_MODEL || null,
            NARADA_MCP_SCOPE: process.env.NARADA_MCP_SCOPE || null,
            NARADA_WORKER_MCP_CONFIG: process.env.NARADA_WORKER_MCP_CONFIG || null
          }),
          command_classification: 'not_applicable'
        }],
        verification_budget_respected: true,
        broad_unrelated_failures: [],
        exit_interview: null
      };
      process.stdout.write(JSON.stringify({ event: 'turn_started', request_id: frame.id, turn_id: 'turn-worker' }) + '\\n');
      process.stdout.write(JSON.stringify({ event: 'assistant_message', request_id: frame.id, turn_id: 'turn-worker', content: JSON.stringify(output) }) + '\\n');
      process.stdout.write(JSON.stringify({ event: 'turn_complete', request_id: frame.id, turn_id: 'turn-worker', terminal_state: 'completed', delegated_mutation_admitted: true, carrier_mutation_admitted: true }) + '\\n');
    }
    if (frame.method === 'session.close') process.exit(0);
  }
});
`, 'utf8');
const rawRpc = handleRequest as unknown as (request: Record<string, unknown>, state: ReturnType<typeof createServerState>) => Promise<RpcResponse>;
const rpc = async (request: Record<string, unknown>, state: ReturnType<typeof createServerState>): Promise<RpcResponse> => {
  const response = await rawRpc(request, state);
  return await materializeOutputRefResponse(response, state);
};
const rpcWithContext = handleRequest as unknown as (request: Record<string, unknown>, state: ReturnType<typeof createServerState>, context: { abortSignal?: AbortSignal }) => Promise<RpcResponse>;
const state = createServerState({
  allowedRoot: root,
  runRoot,
  auditLogDir,
  defaultRuntime: 'codex',
  codexCommand: process.execPath,
  codexCommandArgs: [fakeCodexScript],
  agentRuntimeServerCommand: process.execPath,
  agentRuntimeServerCommandArgs: [fakeAgentRuntimeServerScript],
  providerRegistryPath: defaultProviderRegistryPath,
  maxOutputBytes: 2 * 1024 * 1024,
}, { PATH: process.env.PATH, NARADA_PROVIDER_SECRET_STORE: 'disabled', NARADA_INTELLIGENCE_PROVIDER: 'kimi-code-api', KIMI_CODE_API_KEY: 'kimi-secret-must-not-leak', WORKER_SECRET: 'must-not-leak' });
const registryAliasState = createServerState({ allowedRoot: root }, {
  PATH: process.env.PATH,
  NARADA_SITE_ROOT: root,
  NARADA_PROVIDER_REGISTRY_PATH: defaultProviderRegistryPath,
  NARADA_PROVIDER_SECRET_STORE: 'disabled',
});
assert.equal(registryAliasState.policy.defaultNaradaAgentRuntimeProvider, 'kimi-code-api');
assert.equal(registryAliasState.policy.providerCognitionDefaults['codex-subscription'].low.model, 'gpt-5.6-luna');
const properRoot = mkdtempSync(join(root, 'proper-root-'));
const properRegistryDirectory = join(properRoot, 'packages', 'carrier-provider-contract', 'contracts');
mkdirSync(properRegistryDirectory, { recursive: true });
writeFileSync(join(properRegistryDirectory, 'provider-registry.json'), readFileSync(defaultProviderRegistryPath));
const properRootState = createServerState({ allowedRoot: root }, {
  PATH: process.env.PATH,
  NARADA_SITE_ROOT: root,
  NARADA_PROPER_ROOT: properRoot,
  NARADA_PROVIDER_SECRET_STORE: 'disabled',
});
assert.equal(properRootState.policy.defaultNaradaAgentRuntimeProvider, 'kimi-code-api');
assert.equal(properRootState.policy.providerCognitionDefaults['codex-subscription'].low.model, 'gpt-5.6-luna');

const tools = await rpc({ jsonrpc: '2.0', id: 1, method: 'tools/list', params: {} }, state);
assert.deepEqual(tools.result?.tools.map((tool: any) => tool.name), [
  'worker_guidance',
  'worker_output_show',
  'worker_operator_affordances',
  'worker_policy_inspect',
  'worker_cognition_defaults_inspect',
  'worker_cognition_defaults_update',
  'worker_config_resolve',
  'worker_run',
  'worker_edit',
  'worker_resume',
  'worker_run_status',
  'worker_run_reap',
  'worker_runs_list',
  'worker_run_wait',
  'worker_run_batch',
  'worker_run_wait_batch',
  'worker_runs_synthesize',
  'worker_dashboard_describe',
]);
for (const tool of tools.result?.tools ?? []) {
  assert.equal(tool.outputSchema?.type, 'object', tool.name);
  assert.equal(typeof tool.annotations?.title, 'string', tool.name);
  assert.equal(typeof tool.annotations?.readOnlyHint, 'boolean', tool.name);
}
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.annotations?.readOnlyHint, false);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_edit')?.annotations?.readOnlyHint, false);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_policy_inspect')?.annotations?.readOnlyHint, true);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_cognition_defaults_inspect')?.annotations?.readOnlyHint, true);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_cognition_defaults_update')?.annotations?.readOnlyHint, false);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_config_resolve')?.annotations?.readOnlyHint, true);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_policy_inspect')?.outputSchema?.properties?.schema?.const, 'narada.worker.policy.v1');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_config_resolve')?.outputSchema?.properties?.schema?.const, 'narada.worker.config_resolve.v1');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_edit')?.outputSchema?.properties?.schema?.const, 'narada.worker.run.v1');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_status')?.outputSchema?.properties?.schema?.const, 'narada.worker.run.v1');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_reap')?.outputSchema?.properties?.schema?.const, 'narada.worker.run_reap.v1');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_wait')?.outputSchema?.properties?.schema?.const, 'narada.worker.run_wait.v1');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_runs_list')?.outputSchema?.properties?.schema?.const, 'narada.worker.runs_list.v1');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_batch')?.outputSchema?.properties?.schema?.const, 'narada.worker.run_batch.v1');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_wait_batch')?.outputSchema?.properties?.schema?.const, 'narada.worker.run_wait_batch.v1');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_batch')?.outputSchema?.properties?.requested_count?.type, 'integer');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_batch')?.outputSchema?.properties?.timing?.type, 'object');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_wait_batch')?.outputSchema?.properties?.finished_count?.type, 'integer');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_wait_batch')?.outputSchema?.properties?.elapsed_ms?.type, 'integer');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_runs_synthesize')?.outputSchema?.properties?.schema?.const, 'narada.worker.runs_synthesis.v1');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_dashboard_describe')?.outputSchema?.properties?.schema?.const, 'narada.worker.dashboard.v1');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_batch')?.annotations?.readOnlyHint, false);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_reap')?.annotations?.readOnlyHint, false);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run_reap')?.annotations?.destructiveHint, true);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_runs_synthesize')?.annotations?.readOnlyHint, true);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_dashboard_describe')?.annotations?.readOnlyHint, true);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_output_show')?.annotations?.readOnlyHint, true);
assert.deepEqual(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.inputSchema?.properties?.constraints?.properties?.authority?.enum, ['read', 'write', 'command']);
assert.deepEqual(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.inputSchema?.properties?.constraints?.properties?.cognition?.enum, ['low', 'medium', 'high']);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.inputSchema?.properties?.constraints?.properties?.wait_for_completion?.type, 'boolean');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.inputSchema?.properties?.constraints?.properties?.exit_interview?.type, 'boolean');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.inputSchema?.properties?.constraints?.properties?.provider?.type, 'string');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.inputSchema?.properties?.constraints?.properties?.verification_budget?.type, 'object');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.inputSchema?.properties?.constraints?.properties?.test_budget?.type, 'object');
assert.deepEqual(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.inputSchema?.properties?.intent?.properties?.mode?.enum, ['audit_only', 'plan_only', 'implement', 'implement_and_verify']);
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.inputSchema?.properties?.constraints?.properties?.preflight_paths?.type, 'array');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.inputSchema?.properties?.constraints?.properties?.required_mcp_tools?.type, 'array');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.inputSchema?.properties?.idempotency_key?.type, 'string');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.outputSchema?.properties?.idempotency_replayed?.type, 'boolean');
assert.equal(tools.result?.tools.find((tool: any) => tool.name === 'worker_run')?.outputSchema?.properties?.structured_outputs?.type, 'object');
const guidance = await rpc({ jsonrpc: '2.0', id: 1_1, method: 'tools/call', params: { name: 'worker_guidance', arguments: { workflow: 'delegation', tool: 'worker_run' } } }, state);
assert.equal(guidance.result?.structuredContent.schema, 'narada.mcp_surface.guidance.v0');
assert.match(guidance.result?.content[0].text, /"guidance_tool": "worker_guidance"/);
assert.match(guidance.result?.content[0].text, /"workflow": "delegation"/);
assert.equal(commandRequiresWindowsShell('codex.cmd', 'win32'), true);
assert.equal(commandRequiresWindowsShell('codex.bat', 'win32'), true);
assert.equal(commandRequiresWindowsShell('codex.ps1', 'win32'), true);
assert.equal(commandRequiresWindowsShell(process.execPath, 'win32'), false);
assert.equal(commandRequiresWindowsShell('codex.cmd', 'linux'), false);

const duplicateInvocation: Invocation = {
  command: process.execPath,
  argv: [fakeCodexScript, 'exec', '-o', join(root, 'duplicate-should-not-run.json'), '-'],
  cwd: root,
  environment: { PATH: process.env.PATH ?? '', NARADA_SITE_ROOT: root },
};
const duplicateAdmission = admitWorkerAiProcessInvocation(duplicateInvocation, { projection: 'worker-delegation', purpose: 'codex_worker_runtime' });
try {
  const duplicateResult = await runCodexInvocation({
    invocation: duplicateInvocation,
    prompt: 'duplicate refusal should happen before spawn',
    eventsPath: join(root, 'duplicate-events.jsonl'),
    diagnosticPath: join(root, 'duplicate-diagnostic.log'),
    lastMessagePath: join(root, 'duplicate-last-message.json'),
    maxRunMs: 1000,
  });
  assert.equal(duplicateResult.exit_code, null);
  assert.equal(duplicateResult.worker_session_id, null);
  assert.match(String(duplicateResult.error), /ai_process_invocation_refused: duplicate_live_invocation/);
  assert.equal(existsSync(join(root, 'duplicate-should-not-run.json')), false);
} finally {
  releaseWorkerAiProcessInvocation(duplicateAdmission);
}

const initialize = await rpc({ jsonrpc: '2.0', id: 11, method: 'initialize', params: {} }, state);
assert.deepEqual(Object.keys(initialize.result?.capabilities ?? {}).sort(), ['completions', 'logging', 'prompts', 'resources', 'tools']);
const prompts = await rpc({ jsonrpc: '2.0', id: 12, method: 'prompts/list', params: {} }, state);
assert.equal(prompts.result?.prompts[0].name, 'worker_delegation_task');
const prompt = await rpc({ jsonrpc: '2.0', id: 13, method: 'prompts/get', params: { name: 'worker_delegation_task' } }, state);
assert.match(prompt.result?.messages[0].content.text, /Delegate bounded work/);
const logging = await rpc({ jsonrpc: '2.0', id: 14, method: 'logging/setLevel', params: { level: 'debug' } }, state);
assert.deepEqual(logging.result, {});

const policy = await rpc({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'worker_policy_inspect', arguments: {} } }, state);
assert.equal(policy.result?.structuredContent.schema, 'narada.worker.policy.v1');
assert.equal(policy.result?.structuredContent.default_runtime, 'codex');
assert.equal(policy.result?.structuredContent.default_authority, 'read');
assert.equal(policy.result?.structuredContent.default_cognition, 'low');
assert.equal(policy.result?.structuredContent.implementation_identity.surface_id, 'worker-delegation-mcp');
assert.deepEqual(policy.result?.structuredContent.allowed_runtimes, ['codex', 'narada-agent-runtime-server']);
assert.equal(policy.result?.structuredContent.default_narada_agent_runtime_provider, 'kimi-code-api');
assert.deepEqual(policy.result?.structuredContent.allowed_narada_agent_runtime_providers, ['openai-api', 'kimi-api', 'kimi-code-api', 'anthropic-api', 'deepseek-api', 'glm-api', 'openrouter-api', 'codex-subscription']);
assert.deepEqual(policy.result?.structuredContent.allowed_authorities, ['read', 'write', 'command']);
assert.deepEqual(policy.result?.structuredContent.allowed_cognition, ['low', 'medium', 'high']);
assert.equal(policy.result?.structuredContent.allow_raw_config_overrides, false);
assert.equal(policy.result?.structuredContent.runtimes.codex.ephemeral, true);
assert.equal(policy.result?.structuredContent.runtimes.codex.id, 'codex');
assert.equal(policy.result?.structuredContent.runtimes.deepseek, undefined);
assert.equal(policy.result?.structuredContent.runtimes['deepseek-api'], undefined);
assert.equal(policy.result?.structuredContent.runtimes['narada-agent-runtime-server'].site_bound, true);
assert.deepEqual(policy.result?.structuredContent.runtimes['narada-agent-runtime-server'].site_root_markers, ['.narada/', '.ai/mcp/']);
assert.deepEqual(policy.result?.structuredContent.runtimes['narada-agent-runtime-server'].accepted_site_environment_keys, ['NARADA_SITE_ROOT', 'NARADA_WORKSPACE_ROOT', 'NARADA_AGENT_ID', 'NARADA_CARRIER_SESSION_ID', 'NARADA_MCP_SCOPE', 'NARADA_MAX_TOOL_ROUNDS', 'NARADA_INTELLIGENCE_PLAN_REF', 'NARADA_INTELLIGENCE_REGISTRY_DB', 'NARADA_INTELLIGENCE_TARGET_SITE', 'NARADA_INTELLIGENCE_USER_SITE', 'NARADA_INTELLIGENCE_HOST_SITE', 'NARADA_INTELLIGENCE_PRINCIPAL_ID', 'NARADA_INTELLIGENCE_PRINCIPAL_BINDING', 'CODEX_HOME', 'CODEX_CONFIG_DIR']);
assert.equal(policy.result?.structuredContent.runtimes['narada-agent-runtime-server'].canonical_provider_binding.selector_crosses_worker_boundary, false);
assert.deepEqual(policy.result?.structuredContent.runtimes['narada-agent-runtime-server'].allowed_providers, ['openai-api', 'kimi-api', 'kimi-code-api', 'anthropic-api', 'deepseek-api', 'glm-api', 'openrouter-api', 'codex-subscription']);
assert.match(policy.result?.structuredContent.runtimes['narada-agent-runtime-server'].site_root_required_remediation, /constraints\.site_root/);
assert.equal(policy.result?.structuredContent.nars_site_semantics.site_bound, true);
assert.deepEqual(policy.result?.structuredContent.nars_site_semantics.required_markers, ['.narada/', '.ai/mcp/']);
assert.deepEqual(policy.result?.structuredContent.nars_site_semantics.accepted_environment_keys, ['NARADA_SITE_ROOT', 'NARADA_WORKSPACE_ROOT', 'NARADA_AGENT_ID', 'NARADA_CARRIER_SESSION_ID', 'NARADA_MCP_SCOPE', 'NARADA_MAX_TOOL_ROUNDS', 'NARADA_INTELLIGENCE_PLAN_REF', 'NARADA_INTELLIGENCE_REGISTRY_DB', 'NARADA_INTELLIGENCE_TARGET_SITE', 'NARADA_INTELLIGENCE_USER_SITE', 'NARADA_INTELLIGENCE_HOST_SITE', 'NARADA_INTELLIGENCE_PRINCIPAL_ID', 'NARADA_INTELLIGENCE_PRINCIPAL_BINDING', 'CODEX_HOME', 'CODEX_CONFIG_DIR']);
assert.equal(policy.result?.structuredContent.nars_site_semantics.canonical_provider_binding.selector_crosses_worker_boundary, false);
assert.match(policy.result?.structuredContent.nars_site_semantics.remediation, /constraints\.site_root/);
assert.equal(policy.result?.structuredContent.max_parallel_runs, 10);
assert.deepEqual(policy.result?.structuredContent.cognition_defaults.low, { model: null, reasoning_effort: null });
assert.deepEqual(policy.result?.structuredContent.cognition_defaults.medium, { model: null, reasoning_effort: null });
assert.deepEqual(policy.result?.structuredContent.cognition_defaults.high, { model: null, reasoning_effort: null });
assert.deepEqual(policy.result?.structuredContent.provider_cognition_defaults['kimi-code-api'].low, { model: 'k3', reasoning_effort: 'low' });
assert.deepEqual(policy.result?.structuredContent.provider_cognition_defaults['kimi-code-api'].medium, { model: 'k3', reasoning_effort: 'medium' });
assert.deepEqual(policy.result?.structuredContent.provider_cognition_defaults['openai-api'], {
  low: { model: 'gpt-5.6-luna', reasoning_effort: 'low' },
  medium: { model: 'gpt-5.6-terra', reasoning_effort: 'medium' },
  high: { model: 'gpt-5.6-sol', reasoning_effort: 'high' },
});
assert.deepEqual(policy.result?.structuredContent.provider_cognition_defaults['codex-subscription'], {
  low: { model: 'gpt-5.6-luna', reasoning_effort: 'low' },
  medium: { model: 'gpt-5.6-terra', reasoning_effort: 'medium' },
  high: { model: 'gpt-5.6-sol', reasoning_effort: 'high' },
});
assert.deepEqual(policy.result?.structuredContent.provider_cognition_defaults['openrouter-api'], {
  low: { model: 'z-ai/glm-5-turbo', reasoning_effort: 'low' },
  medium: { model: 'z-ai/glm-5.2', reasoning_effort: 'medium' },
  high: { model: 'z-ai/glm-5.2', reasoning_effort: 'high' },
});
assert.match(policy.result?.content[0].text, /"schema": "narada\.worker\.policy\.v1"/);
assert.match(policy.result?.content[0].text, /"site_bound": true/);
assert.match(policy.result?.content[0].text, /"\.narada\/"/);
assert.equal(createServerState({ allowedRoot: root }).policy.defaultRuntime, 'narada-agent-runtime-server');
process.env.NARADA_WORKER_DEFAULT_RUNTIME = 'codex';
assert.equal(createServerState({ allowedRoot: root }).policy.defaultRuntime, 'codex');
delete process.env.NARADA_WORKER_DEFAULT_RUNTIME;

const codexCatalogDefaults = state.policy.providerCognitionDefaults['codex-subscription'];
assert.ok(codexCatalogDefaults);
for (const cognition of ['low', 'medium', 'high'] as const) {
  assert.equal(typeof codexCatalogDefaults[cognition].model, 'string');
  assert.equal(typeof codexCatalogDefaults[cognition].reasoningEffort, 'string');
}

const configPreview = await rpc({ jsonrpc: '2.0', id: 21, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: {
  intent: { instruction: 'inspect repository shape' },
  constraints: { cwd: root, authority: 'read', cognition: 'high', required_mcp_tools: ['mcp__narada_andrey_local_filesystem'], verification_budget: { focus: 'focused', max_commands: 1, stop_on_first_failure: true }, test_budget: { focus: 'focused', max_minutes: 2, broad_commands_allowed: false } },
} } }, state);
assert.equal(configPreview.result?.structuredContent.schema, 'narada.worker.config_resolve.v1');
assert.equal(configPreview.result?.structuredContent.dry_run, true);
assert.equal(configPreview.result?.structuredContent.requested_mode, 'audit_only');
assert.equal(configPreview.result?.structuredContent.resolved_worker_config.runtime, 'codex');
assert.equal(configPreview.result?.structuredContent.implementation_identity.surface_id, 'worker-delegation-mcp');
assert.equal(configPreview.result?.structuredContent.resolved_worker_config.implementation_identity.surface_id, 'worker-delegation-mcp');
assert.equal(configPreview.result?.structuredContent.resolved_worker_config.provider, 'codex-subscription');
assert.equal(configPreview.result?.structuredContent.resolved_worker_config.model, codexCatalogDefaults.high.model);
assert.equal(configPreview.result?.structuredContent.resolved_worker_config.reasoning_effort, codexCatalogDefaults.high.reasoningEffort);
assert.equal(configPreview.result?.structuredContent.config_resolution.model_source, 'cognition_default');
assert.equal(configPreview.result?.structuredContent.config_resolution.reasoning_effort_source, 'cognition_default');
assert.equal(configPreview.result?.structuredContent.runtime_availability.available, true);
assert.deepEqual(configPreview.result?.structuredContent.requested_mcp_tools, ['mcp__narada_andrey_local_filesystem']);
assert.equal(configPreview.result?.structuredContent.mcp_tool_verification.runtime_can_project, false);
assert.equal(configPreview.result?.structuredContent.mcp_tool_verification.verification_state, 'requires_projected_runtime');
assert.equal(configPreview.result?.structuredContent.output_contract.schema, 'narada.worker.output_contract.v1');
assert.equal(configPreview.result?.structuredContent.output_contract.findings.required_for_audit_only, true);
assert.equal(configPreview.result?.structuredContent.output_contract.verification_command_classification.required, true);
assert.deepEqual(configPreview.result?.structuredContent.output_contract.verification_budget, { focus: 'focused', max_commands: 1, stop_on_first_failure: true });
assert.deepEqual(configPreview.result?.structuredContent.output_contract.test_budget, { focus: 'focused', max_minutes: 2, broad_commands_allowed: false });
assert.equal(configPreview.result?.structuredContent.resolved_worker_config.environment_keys.includes('KIMI_CODE_API_KEY'), false);
assert.equal(JSON.stringify(configPreview.result?.structuredContent).includes('kimi-secret-must-not-leak'), false);
assert.match(configPreview.result?.structuredContent.invocation.argv.join(' '), /<dry-run>\/worker_output\.schema\.json/);
assert.doesNotMatch(configPreview.result?.structuredContent.warnings.join('\n'), /model_delegated_to_runtime_default|reasoning_effort_delegated_to_runtime_default/);
assert.match(configPreview.result?.content[0].text, /"schema": "narada\.worker\.config_resolve\.v1"/);

const requiredToolsBlocked = await rpc({
  jsonrpc: '2.0',
  id: 22_1,
  method: 'tools/call',
  params: {
    name: 'worker_run',
    arguments: {
      intent: { instruction: 'require unprojectable mcp tools' },
      constraints: { cwd: root, authority: 'read', cognition: 'low', required_mcp_tools: ['local-filesystem.fs_read_file'] },
    },
  },
}, state);
assert.equal(requiredToolsBlocked.error?.data.code, 'worker_required_mcp_tools_unprojectable');
assert.deepEqual(requiredToolsBlocked.error?.data.details.requested_mcp_tools, ['local-filesystem.fs_read_file']);

const explicitConfig = await rpc({ jsonrpc: '2.0', id: 22, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: {
  intent: { instruction: 'inspect repository shape', mode: 'plan_only' },
  constraints: { cwd: root, authority: 'read', overrides: { model: 'gpt-test', reasoning_effort: 'low' } },
} } }, state);
assert.equal(explicitConfig.result?.structuredContent.resolved_worker_config.model, 'gpt-test');
assert.equal(explicitConfig.result?.structuredContent.resolved_worker_config.reasoning_effort, 'low');
assert.equal(explicitConfig.result?.structuredContent.config_resolution.model_source, 'request_override');
assert.equal(explicitConfig.result?.structuredContent.config_resolution.reasoning_effort_source, 'request_override');
assert.doesNotMatch(explicitConfig.result?.structuredContent.warnings.join('\n'), /runtime_default/);

assert.throws(() => createServerState({ allowedRoot: root, allowedRuntime: 'agent-cli' }), /worker_invalid_runtime/);
assert.throws(() => createServerState({ allowedRoot: root, defaultRuntime: 'deepseek-api' }), /worker_runtime_migrated_to_nars_provider/);
assert.throws(() => createServerState({ allowedRoot: root, allowedSandbox: 'invalid' }), /worker_invalid_sandbox/);
assert.throws(() => createServerState({ allowedRoot: root, allowedSandbox: 'danger-full-access' }), /worker_danger_full_access_not_allowed/);
createServerState({ allowedRoot: root, allowedSandboxes: ['read-only', 'workspace-write'] });

const secretProcessValue = process.env.WORKER_DELEGATION_TEST_SECRET;
delete process.env.WORKER_DELEGATION_TEST_SECRET;
const secretSiteRoot = join(root, 'secret-site');
const secretRunRoot = join(root, 'secret-runs');
mkdirSync(join(secretSiteRoot, '.narada'), { recursive: true });
writeFileSync(join(secretSiteRoot, '.narada', 'secrets.json'), JSON.stringify({ env: { WORKER_DELEGATION_TEST_SECRET: 'from-site-secret' } }), 'utf8');
const secretState = createServerState({ siteRoot: secretSiteRoot, allowedRoot: secretSiteRoot, runRoot: secretRunRoot, defaultRuntime: 'codex', codexCommand: process.execPath }, { PATH: process.env.PATH });
assert.equal(secretState.env.WORKER_DELEGATION_TEST_SECRET, 'from-site-secret');
assert.equal(process.env.WORKER_DELEGATION_TEST_SECRET, undefined);
if (secretProcessValue === undefined) delete process.env.WORKER_DELEGATION_TEST_SECRET;
else process.env.WORKER_DELEGATION_TEST_SECRET = secretProcessValue;

const providerRoot = join(root, 'provider-secret-site');
const providerRunRoot = join(root, 'provider-secret-runs');
const providerRegistryPath = join(providerRoot, 'provider-registry.json');
const providerSecretLookupScript = join(providerRoot, 'secret-lookup.js');
mkdirSync(providerRoot, { recursive: true });
mkdirSync(join(providerRoot, '.narada'), { recursive: true });
writeFileSync(providerRegistryPath, JSON.stringify({
  schema: 'narada.carrier.provider_registry.v1',
  default_provider: 'deepseek-api',
  providers: {
    'deepseek-api': {
      base_url: 'https://api.deepseek.com',
      default_model: 'deepseek-v4-flash',
      available_models: ['deepseek-v4-flash', 'deepseek-v4-pro'],
      cognition_defaults: {
        low: { model: 'deepseek-v4-flash', reasoning_effort: 'low' },
        medium: { model: 'deepseek-v4-flash', reasoning_effort: 'medium' },
        high: { model: 'deepseek-v4-pro', reasoning_effort: 'high' },
      },
      base_url_env_names: ['DEEPSEEK_API_BASE_URL'],
      credential_requirement: {
        kind: 'api_key_secret',
        secret_ref: 'narada/provider/deepseek-api/api-key',
        env_names: ['DEEPSEEK_API_KEY'],
      },
    },
    'codex-subscription': {
      base_url: 'codex://local-subscription',
      default_model: 'gpt-5.6-sol',
      available_models: ['gpt-5.6-luna', 'gpt-5.6-terra', 'gpt-5.6-sol'],
      cognition_defaults: {
        low: { model: 'gpt-5.6-luna', reasoning_effort: 'low' },
        medium: { model: 'gpt-5.6-terra', reasoning_effort: 'medium' },
        high: { model: 'gpt-5.6-sol', reasoning_effort: 'high' },
      },
      base_url_env_names: [],
      model_env_names: ['CODEX_MODEL', 'NARADA_CODEX_MODEL'],
      credential_requirement: { kind: 'local_codex_subscription' },
    },
  },
}), 'utf8');
writeFileSync(providerSecretLookupScript, `
if (process.env.NARADA_SECRET_LOOKUP_NAME === 'narada/provider/deepseek-api/api-key') {
  process.stdout.write('deepseek-from-secret-store');
  process.exit(0);
}
process.exit(2);
`, 'utf8');
const providerState = createServerState({
  siteRoot: providerRoot,
  allowedRoot: providerRoot,
  runRoot: providerRunRoot,
  defaultRuntime: 'codex',
  codexCommand: process.execPath,
  agentRuntimeServerCommand: process.execPath,
  providerRegistryPath,
  secretLookupCommand: process.execPath,
  secretLookupCommandArgs: [providerSecretLookupScript],
}, { PATH: process.env.PATH });
// Provider secrets are intentionally lazy: MCP startup must not synchronously
// query every provider's secret store. The selected provider is loaded when a
// config is resolved for that provider.
assert.equal(providerState.env.DEEPSEEK_API_KEY, undefined);
assert.equal(providerState.env.DEEPSEEK_API_BASE_URL, undefined);
const providerPolicy = await rpc({ jsonrpc: '2.0', id: 197, method: 'tools/call', params: { name: 'worker_policy_inspect', arguments: {} } }, providerState);
assert.equal(JSON.stringify(providerPolicy.result?.structuredContent).includes('deepseek-from-secret-store'), false);
assert.equal(providerPolicy.result?.structuredContent.allowed_narada_agent_runtime_providers.includes('deepseek-api'), true);
assert.equal(providerPolicy.result?.structuredContent.default_narada_agent_runtime_provider, 'deepseek-api');
assert.deepEqual(providerPolicy.result?.structuredContent.provider_cognition_defaults['deepseek-api'].low, { model: 'deepseek-v4-flash', reasoning_effort: 'low' });
assert.deepEqual(providerPolicy.result?.structuredContent.provider_cognition_defaults['deepseek-api'].high, { model: 'deepseek-v4-pro', reasoning_effort: 'high' });
const cognitionDefaultsBefore = await rpc({ jsonrpc: '2.0', id: 1971, method: 'tools/call', params: { name: 'worker_cognition_defaults_inspect', arguments: {} } }, providerState);
assert.equal(cognitionDefaultsBefore.result?.structuredContent.version, 0);
assert.equal(cognitionDefaultsBefore.result?.structuredContent.provider_cognition_defaults['deepseek-api'].high.source, 'provider_registry');
const invalidCognitionUpdate = await rpc({ jsonrpc: '2.0', id: 1972, method: 'tools/call', params: { name: 'worker_cognition_defaults_update', arguments: { provider: 'deepseek-api', cognition: 'high', model: 'not-a-deepseek-model', reasoning_effort: 'high' } } }, providerState);
assert.equal(invalidCognitionUpdate.error?.data.code, 'worker_cognition_model_not_allowed');
const malformedCognitionUpdate = await rpc({ jsonrpc: '2.0', id: 19721, method: 'tools/call', params: { name: 'worker_cognition_defaults_update', arguments: { provider: 'deepseek-api', cognition: 'high', model: 'deepseek-v4-flash', reasoning_effort: 42 } } }, providerState);
assert.equal(malformedCognitionUpdate.error?.data.code, 'worker_cognition_reasoning_effort_required');
assert.equal(malformedCognitionUpdate.error?.data.details.validation_issues[0].path, 'reasoning_effort');
const cognitionUpdate = await rpc({ jsonrpc: '2.0', id: 1973, method: 'tools/call', params: { name: 'worker_cognition_defaults_update', arguments: { provider: 'deepseek-api', cognition: 'high', model: 'deepseek-v4-flash', reasoning_effort: 'max', actor: 'worker-test' } } }, providerState);
assert.equal(cognitionUpdate.result?.structuredContent.version, 1);
assert.equal(cognitionUpdate.result?.structuredContent.current.model, 'deepseek-v4-flash');
assert.equal(cognitionUpdate.result?.structuredContent.current.provider, 'deepseek-api');
assert.equal(existsSync(join(providerRoot, '.narada', 'worker-cognition-defaults.json')), true);
assert.equal(existsSync(join(providerRoot, '.narada', 'worker-cognition-defaults.audit.jsonl')), true);
const directDeepseekResolve = await rpc({ jsonrpc: '2.0', id: 198, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: { intent: { instruction: 'deepseek direct runtime rejected' }, constraints: { cwd: providerRoot, overrides: { runtime: 'deepseek-api' } } } } }, providerState);
assert.equal(directDeepseekResolve.error?.data.code, 'worker_runtime_migrated_to_nars_provider');
assert.match(String(directDeepseekResolve.error?.data.details.remediation), /provider="deepseek-api"/);
const deepseekResolve = await rpc({ jsonrpc: '2.0', id: 199, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: { intent: { instruction: 'deepseek secret check' }, constraints: { cwd: providerRoot, provider: 'deepseek-api', overrides: { runtime: 'narada-agent-runtime-server' } } } } }, providerState);
assert.equal(deepseekResolve.error?.data.code, 'worker_canonical_invocation_plan_override_rejected');
assert.equal(providerState.env.DEEPSEEK_API_KEY, undefined);
assert.equal(providerState.env.DEEPSEEK_API_BASE_URL, undefined);
assert.equal(JSON.stringify(deepseekResolve).includes('deepseek-from-secret-store'), false);
const deepseekDefaultProviderResolve = await rpc({ jsonrpc: '2.0', id: 200, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: { intent: { instruction: 'deepseek registry default provider check' }, constraints: { cwd: providerRoot, cognition: 'high', overrides: { runtime: 'narada-agent-runtime-server' } } } } }, providerState);
assert.equal(deepseekDefaultProviderResolve.error?.data.code, 'worker_canonical_invocation_plan_override_rejected');
const reloadedProviderState = createServerState({
  siteRoot: providerRoot,
  allowedRoot: providerRoot,
  runRoot: providerRunRoot,
  defaultRuntime: 'codex',
  codexCommand: process.execPath,
  agentRuntimeServerCommand: process.execPath,
  providerRegistryPath,
  secretLookupCommand: process.execPath,
  secretLookupCommandArgs: [providerSecretLookupScript],
}, { PATH: process.env.PATH });
const reloadedCognitionDefaults = await rpc({ jsonrpc: '2.0', id: 2001, method: 'tools/call', params: { name: 'worker_cognition_defaults_inspect', arguments: {} } }, reloadedProviderState);
assert.equal(reloadedCognitionDefaults.result?.structuredContent.version, 1);
assert.equal(reloadedCognitionDefaults.result?.structuredContent.provider_cognition_defaults['deepseek-api'].high.source, 'site_runtime_override');
assert.equal(reloadedCognitionDefaults.result?.structuredContent.provider_cognition_defaults['deepseek-api'].high.reasoning_effort, 'max');
assert.deepEqual(reloadedCognitionDefaults.result?.structuredContent.effective_cognition_defaults.high, {
  provider: 'deepseek-api',
  model: 'deepseek-v4-flash',
  reasoning_effort: 'max',
  source: 'site_runtime_override',
  precedence: 'per_run_override > site_effective_cognition_default > explicit_provider_registry_default > global_provider_registry_default > generic_cognition_default',
});
const codexTierExpectations = [
  { cognition: 'low', model: 'gpt-5.6-luna', updateId: 20020, resolveId: 20030 },
  { cognition: 'medium', model: 'gpt-5.6-terra', updateId: 20021, resolveId: 20031 },
  { cognition: 'high', model: 'gpt-5.6-sol', updateId: 20022, resolveId: 20032 },
] as const;
for (const expected of codexTierExpectations) {
  const update = await rpc({ jsonrpc: '2.0', id: expected.updateId, method: 'tools/call', params: { name: 'worker_cognition_defaults_update', arguments: { provider: 'codex-subscription', cognition: expected.cognition, model: expected.model, reasoning_effort: 'max' } } }, reloadedProviderState);
  assert.equal(update.error, undefined, JSON.stringify(update.error));
  assert.deepEqual(update.result?.structuredContent.current, { provider: 'codex-subscription', model: expected.model, reasoning_effort: 'max' });
  const resolved = await rpc({
    jsonrpc: '2.0',
    id: expected.resolveId,
    method: 'tools/call',
    params: {
      name: 'worker_config_resolve',
      arguments: {
        intent: { instruction: `codex ${expected.cognition} tuple resolution` },
        constraints: { cwd: providerRoot, cognition: expected.cognition, overrides: { runtime: 'narada-agent-runtime-server' } },
      },
    },
  }, reloadedProviderState);
  assert.equal(resolved.error?.data.code, 'worker_canonical_invocation_plan_override_rejected');
}
const explicitTupleOverride = await rpc({
  jsonrpc: '2.0',
  id: 20040,
  method: 'tools/call',
  params: {
    name: 'worker_config_resolve',
    arguments: {
      intent: { instruction: 'explicit tuple override precedence' },
      constraints: {
        cwd: providerRoot,
        cognition: 'low',
        provider: 'deepseek-api',
        overrides: { runtime: 'narada-agent-runtime-server', model: 'deepseek-v4-pro', reasoning_effort: 'high' },
      },
    },
  },
}, reloadedProviderState);
assert.equal(explicitTupleOverride.error?.data.code, 'worker_canonical_invocation_plan_override_rejected');
const legacyDefaultsRoot = join(root, 'legacy-cognition-defaults-site');
mkdirSync(join(legacyDefaultsRoot, '.narada'), { recursive: true });
writeFileSync(join(legacyDefaultsRoot, '.narada', 'worker-cognition-defaults.json'), JSON.stringify({
  schema: 'narada.worker.cognition_defaults.v1',
  version: 9,
  updated_at: new Date().toISOString(),
  provider_cognition_defaults: { 'deepseek-api': { high: { model: 'deepseek-chat', reasoning_effort: null } } },
  effective_cognition_defaults: { low: { provider: 'codex-subscription', model: 'gpt-5.6-luna', reasoning_effort: 'max' } },
}), 'utf8');
const legacyDefaultsState = createServerState({ siteRoot: legacyDefaultsRoot, allowedRoot: legacyDefaultsRoot, runRoot: join(legacyDefaultsRoot, 'runs'), providerRegistryPath });
const legacyDefaultsInspect = await rpc({ jsonrpc: '2.0', id: 2004, method: 'tools/call', params: { name: 'worker_cognition_defaults_inspect', arguments: {} } }, legacyDefaultsState);
assert.equal(legacyDefaultsInspect.result?.structuredContent.version, 9);
assert.equal(legacyDefaultsInspect.result?.structuredContent.provider_cognition_defaults['deepseek-api'].high.source, 'provider_registry');
assert.equal(legacyDefaultsInspect.result?.structuredContent.provider_cognition_defaults['deepseek-api'].high.reasoning_effort, 'high');
assert.equal(legacyDefaultsInspect.result?.structuredContent.effective_cognition_defaults.low.provider, 'codex-subscription');
assert.equal(legacyDefaultsInspect.result?.structuredContent.effective_cognition_defaults.low.reasoning_effort, 'max');
const invalidDefaultsRoot = join(root, 'invalid-cognition-defaults-site');
mkdirSync(join(invalidDefaultsRoot, '.narada'), { recursive: true });
writeFileSync(join(invalidDefaultsRoot, '.narada', 'worker-cognition-defaults.json'), JSON.stringify({
  schema: 'narada.worker.cognition_defaults.v1',
  version: 1,
  updated_at: new Date().toISOString(),
  provider_cognition_defaults: {},
  effective_cognition_defaults: { low: { provider: 'codex-subscription', model: 'gpt-5.6-luna', reasoning_effort: 42 } },
}), 'utf8');
assert.throws(() => createServerState({ siteRoot: invalidDefaultsRoot, allowedRoot: invalidDefaultsRoot, runRoot: join(invalidDefaultsRoot, 'runs'), providerRegistryPath }), (error: any) => {
  assert.equal(error.codeName, 'worker_cognition_defaults_invalid');
  assert.equal(error.details.validation_issues[0].path, 'effective_cognition_defaults.low.reasoning_effort');
  return true;
});

if (process.platform === 'win32') {
  const mixedCaseState = createServerState({ allowedRoot: root.toLowerCase(), runRoot, defaultRuntime: 'codex', codexCommand: process.execPath });
  assert.equal(mixedCaseState.policy.allowedRoots.length, 1);
  assert.equal(mixedCaseState.policy.allowedRoots[0].toLowerCase(), root.toLowerCase());
  assert.equal(createServerState({ allowedRoot: platformRootCase, runRoot, defaultRuntime: 'codex', codexCommand: process.execPath }).policy.allowedRoots[0].toLowerCase(), root.toLowerCase());
  assert.equal(resolveWorkingDirectory(platformRootCase, mixedCaseState.policy).toLowerCase(), root.toLowerCase());
  const mixedCaseChild = join(platformRootCase, 'Child');
  mkdirSync(mixedCaseChild, { recursive: true });
  assert.equal(resolveWorkingDirectory(mixedCaseChild, mixedCaseState.policy).toLowerCase(), mixedCaseChild.toLowerCase());

  const ps1Bin = join(root, 'ps1-bin');
  mkdirSync(ps1Bin, { recursive: true });
  const codexPs1 = join(ps1Bin, 'codex.ps1');
  writeFileSync(codexPs1, `
$out = $args[$args.IndexOf('-o') + 1]
Set-Content -LiteralPath $out -Encoding UTF8 -Value '{"summary":"ps1 worker ok","deliverables":[],"open_questions":[],"next_actions":[],"edits_performed":false,"target_state_changed":false,"changes":[],"verification":[],"verification_budget_respected":null,"broad_unrelated_failures":[],"exit_interview":null}'
Write-Output '{"thread_id":"ps1-thread"}'
`, 'utf8');
  const ps1State = createServerState({ allowedRoot: root, runRoot: join(root, 'ps1-runs'), defaultRuntime: 'codex', codexCommand: 'codex.ps1', providerRegistryPath: defaultProviderRegistryPath }, { Path: `${ps1Bin};${process.env.Path ?? process.env.PATH ?? ''}` } as NodeJS.ProcessEnv);
  const ps1Run = await rpc({ jsonrpc: '2.0', id: 158, method: 'tools/call', params: { name: 'worker_run', arguments: runArgs('ps1 command lookup') } }, ps1State);
  const ps1RunDir = ps1Run.result?.structuredContent.run_dir ?? ps1Run.error?.data.details.run_dir;
  assert.equal(typeof ps1RunDir, 'string');
  const ps1Invocation = JSON.parse(readFileSync(join(ps1RunDir, 'worker_invocation.json'), 'utf8'));
  assert.equal(ps1Invocation.command.toLowerCase(), codexPs1.toLowerCase());

  const agentRuntimeShimBin = join(root, 'agent-runtime-shim-bin');
  mkdirSync(agentRuntimeShimBin, { recursive: true });
  const agentRuntimeCmd = join(agentRuntimeShimBin, 'agent-runtime-server.cmd');
  writeFileSync(agentRuntimeCmd, `
@SETLOCAL
@IF NOT DEFINED NODE_PATH (
  @SET "NODE_PATH=${root}\\node_modules"
)
@IF EXIST "%~dp0\\node.exe" (
  "%~dp0\\node.exe" "%~dp0\\..\\agent-runtime-server.cjs" %*
) ELSE (
  node "%~dp0\\..\\agent-runtime-server.cjs" %*
)
`, 'utf8');
  const agentRuntimeShimState = createServerState({
    allowedRoot: root,
    runRoot: join(root, 'agent-runtime-shim-runs'),
    agentRuntimeServerCommand: agentRuntimeCmd,
    providerRegistryPath: defaultProviderRegistryPath,
  }, { PATH: process.env.PATH, NARADA_PROVIDER_SECRET_STORE: 'disabled', NARADA_INTELLIGENCE_PROVIDER: 'kimi-code-api', KIMI_CODE_API_KEY: 'shim-kimi-key' });
  const agentRuntimeShimRun = await rpc({ jsonrpc: '2.0', id: 159, method: 'tools/call', params: { name: 'worker_run', arguments: runArgs('agent runtime shim lookup', { runtime: 'narada-agent-runtime-server' }) } }, agentRuntimeShimState);
  assert.equal(agentRuntimeShimRun.result?.structuredContent.status, 'completed');
  const agentRuntimeShimInvocation = JSON.parse(readFileSync(join(agentRuntimeShimRun.result?.structuredContent.run_dir, 'worker_invocation.json'), 'utf8'));
  assert.equal(agentRuntimeShimInvocation.command, process.execPath);
  assert.equal(agentRuntimeShimInvocation.argv[0], fakeAgentRuntimeServerScript);
  assert.equal(agentRuntimeShimInvocation.argv[1], '--raw-jsonl');
  assert.equal(agentRuntimeShimInvocation.argv[2], '--authority');
  assert.equal(agentRuntimeShimInvocation.argv[3], 'read');
}

const deniedRuntime = await rpc({
  jsonrpc: '2.0',
  id: 3,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('x', { runtime: 'agent-cli' }) },
}, state);
assert.equal(deniedRuntime.error?.data.schema, 'narada.worker.error.v1');
assert.equal(deniedRuntime.error?.data.code, 'worker_invalid_runtime');

const deniedAuthority = await rpc({
  jsonrpc: '2.0',
  id: 31,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('x', {}, 'workspace-edit') },
}, state);
assert.equal(deniedAuthority.error?.data.code, 'worker_invalid_authority');

const deniedConfig = await rpc({
  jsonrpc: '2.0',
  id: 4,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('x', { config: { shell_environment_policy: 'all' } }) },
}, state);
assert.equal(deniedConfig.error?.data.code, 'worker_config_key_not_allowed');

const deniedRawOverrides = await rpc({
  jsonrpc: '2.0',
  id: 41,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { ...runArgs('x'), config_overrides: ['model=\"x\"'] } },
}, state);
assert.equal(deniedRawOverrides.error?.data.code, 'worker_raw_config_overrides_not_allowed');

const deniedNonObjectConfig = await rpc({
  jsonrpc: '2.0',
  id: 411,
  method: 'tools/call',
  params: { name: 'worker_config_resolve', arguments: runArgs('non object config', { config: 'model=gpt-test' }) },
}, state);
assert.equal(deniedNonObjectConfig.error?.data.code, 'worker_invalid_config_input');

const deniedSkipGitRepoCheckString = await rpc({
  jsonrpc: '2.0',
  id: 412,
  method: 'tools/call',
  params: { name: 'worker_config_resolve', arguments: runArgs('string skip git repo check', { skip_git_repo_check: 'true' }) },
}, state);
assert.equal(deniedSkipGitRepoCheckString.error?.data.code, 'worker_invalid_tool_input');

const badConfigPath = join(root, 'bad-config.toml');
writeFileSync(badConfigPath, '[worker]\nrun_root = nope\n', 'utf8');
assert.throws(() => createServerState({ config: badConfigPath, allowedRoot: root }), hasCode('worker_invalid_config_file'));
const conflictingConfigPath = join(root, 'conflicting-config.toml');
writeFileSync(conflictingConfigPath, 'worker = 1\n[worker.policy]\nmax_parallel_runs = 1\n', 'utf8');
assert.throws(() => createServerState({ config: conflictingConfigPath, allowedRoot: root }), hasCode('worker_invalid_config_file'));
const malformedTrustConfigPath = join(root, 'bad-trust-config.toml');
writeFileSync(malformedTrustConfigPath, `[projects.'${root.replace(/\\/g, '\\\\')}']\ntrust_level = "trusted"\nextra = "silently ignored before"\n`, 'utf8');
assert.throws(() => createServerState({ allowedRoot: root, rootsFromTrustConfig: malformedTrustConfigPath }), hasCode('worker_invalid_trust_config'));
assert.throws(() => createServerState({ allowedRoot: root, maxOutputBytes: 'nope' }), hasCode('worker_invalid_config_value'));
assert.throws(() => createServerState({ allowedRoot: root, ephemeral: 'treu' }), hasCode('worker_invalid_config_value'));
assert.throws(() => parseArgs(['--allowed-root']), hasCode('worker_invalid_cli_args'));
assert.throws(() => parseArgs(['--config']), hasCode('worker_invalid_cli_args'));
assert.throws(() => parseArgs(['--run-root']), hasCode('worker_invalid_cli_args'));
assert.throws(() => parseArgs(['--audit-log-dir']), hasCode('worker_invalid_cli_args'));
assert.throws(() => parseArgs(['--codex-command']), hasCode('worker_invalid_cli_args'));
assert.throws(() => parseArgs(['--codex-command-arg']), hasCode('worker_invalid_cli_args'));
assert.deepEqual(parseArgs(['--codex-command-arg', 'codex.js', '--codex-command-arg', 'arg2']).codexCommandArgs, ['codex.js', 'arg2']);
assert.deepEqual(parseArgs(['--agent-runtime-server-command-arg', 'server.js', '--agent-runtime-server-command-arg', '--raw-jsonl']).agentRuntimeServerCommandArgs, ['server.js', '--raw-jsonl']);
assert.equal(parseArgs(['--user-site-root', root]).userSiteRoot, root);
assert.equal(parseArgs(['--cognition-low-reasoning-effort', 'minimal']).cognitionLowReasoningEffort, 'minimal');
assert.equal(parseArgs(['--cognition-high-model', 'gpt-test-high']).cognitionHighModel, 'gpt-test-high');

const busyState = createServerState({ allowedRoot: root, runRoot: join(root, 'busy'), defaultRuntime: 'codex', codexCommand: process.execPath, maxParallelRuns: 1 });
busyState.activeRunCount = 1;
const busyRun = await rpc({
  jsonrpc: '2.0',
  id: 42,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('busy worker') },
}, busyState);
assert.equal(busyRun.error?.data.code, 'worker_parallel_limit_exceeded');
assert.equal(busyState.activeRunCount, 1);

const allowedConfigRun = await rpc({
  jsonrpc: '2.0',
  id: 5,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('run with allowed config', { model: 'gpt-test', reasoning_effort: 'low', config: { model: 'gpt-test' } }) },
}, state);
assert.equal(allowedConfigRun.result?.structuredContent.status, 'completed');
assert.equal(state.activeRunCount, 0);
assert.equal(allowedConfigRun.result?.structuredContent.worker_session_id, 'thread-created');
assert.equal(allowedConfigRun.result?.structuredContent.summary, 'worker ok');
assert.equal(allowedConfigRun.result?.structuredContent.requested_mode, 'audit_only');
assert.equal(allowedConfigRun.result?.structuredContent.edits_performed, false);
assert.equal(allowedConfigRun.result?.structuredContent.target_state_changed, false);
assert.equal(allowedConfigRun.result?.structuredContent.confidence, 'complete');
assert.equal(allowedConfigRun.result?.structuredContent.completion_state, 'complete');
assert.equal(allowedConfigRun.result?.structuredContent.preflight.some((check: any) => check.name === 'cwd_readable' && check.status === 'ok'), true);

const managedCancellationState = createServerState({ allowedRoot: root, runRoot: join(root, 'managed-cancellation'), defaultRuntime: 'codex', codexCommand: process.execPath, codexCommandArgs: [fakeCodexHangScript], providerRegistryPath: defaultProviderRegistryPath });
const managedCancellationStart = await rpc({ jsonrpc: '2.0', id: 501, method: 'tools/call', params: { name: 'worker_run', arguments: { intent: { instruction: 'managed cancellation run' }, constraints: { cwd: root } } } }, managedCancellationState);
assert.equal(managedCancellationStart.result?.structuredContent.status, 'running');
const managedCancellationRunId = String(managedCancellationStart.result?.structuredContent.run_id);
const managedCancellationStatus = await rpc({ jsonrpc: '2.0', id: 502, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: managedCancellationRunId } } }, managedCancellationState);
assert.equal(managedCancellationStatus.result?.structuredContent.status_liveness.process_liveness, 'managed_active');
const managedCancellation = await rpc({ jsonrpc: '2.0', id: 503, method: 'tools/call', params: { name: 'worker_run_reap', arguments: { run_id: managedCancellationRunId, force: true, reason: 'regression test cancellation' } } }, managedCancellationState);
assert.equal(managedCancellation.result?.structuredContent.status, 'reaped');
assert.equal(managedCancellation.result?.structuredContent.evidence.cancellation_propagated, true);
assert.equal(managedCancellation.result?.structuredContent.run.status, 'cancelled');
assert.equal(managedCancellationState.activeRunCount, 0);

const boundedWaitState = createServerState({ allowedRoot: root, runRoot: join(root, 'bounded-wait'), defaultRuntime: 'codex', codexCommand: process.execPath, codexCommandArgs: [fakeCodexScript], providerRegistryPath: defaultProviderRegistryPath });
const boundedWait = await rpc({
  jsonrpc: '2.0',
  id: 504,
  method: 'tools/call',
  params: {
    name: 'worker_run',
    arguments: {
      intent: { instruction: 'bounded synchronous wait' },
      constraints: { cwd: root, wait_for_completion: true, wait_timeout_ms: 1 },
    },
  },
}, boundedWaitState);
assert.equal(boundedWait.result?.structuredContent.status, 'running');
assert.equal(boundedWait.result?.structuredContent.wait_for_completion.status, 'continued_asynchronously');
assert.equal(boundedWait.result?.structuredContent.wait_for_completion.wait_timeout_ms, 1);
assert.equal(boundedWait.result?.structuredContent.next_action.tool, 'worker_run_wait');
const boundedWaitRunId = String(boundedWait.result?.structuredContent.run_id);
const boundedWaitFinished = await rpc({ jsonrpc: '2.0', id: 505, method: 'tools/call', params: { name: 'worker_run_wait', arguments: { run_id: boundedWaitRunId, timeout_ms: 15_000, poll_ms: 25 } } }, boundedWaitState);
assert.equal(boundedWaitFinished.result?.structuredContent.wait.status, 'finished');
assert.equal(boundedWaitState.activeRunCount, 0);

const agentRuntimeState = createServerState({
  allowedRoot: root,
  runRoot: join(root, 'agent-runtime-runs'),
  agentRuntimeServerCommand: process.execPath,
  agentRuntimeServerCommandArgs: [fakeAgentRuntimeServerScript],
  providerRegistryPath: defaultProviderRegistryPath,
}, {
  ...process.env,
  NARADA_SITE_ROOT: '',
  NARADA_WORKSPACE_ROOT: '',
  NARADA_PROVIDER_SECRET_STORE: 'disabled',
  NARADA_INTELLIGENCE_PROVIDER: 'kimi-code-api',
  KIMI_CODE_API_KEY: 'selected-kimi-worker-key',
  OPENAI_API_KEY: 'unrelated-openai-decoy',
  KIMI_API_KEY: 'unrelated-moonshot-decoy',
});

const splitWorkspaceRoot = join(root, 'split-workspace');
const splitSiteRoot = join(splitWorkspaceRoot, '.narada');
mkdirSync(join(splitSiteRoot, '.ai', 'mcp'), { recursive: true });
const splitBindingState = createServerState({
  allowedRoot: root,
  runRoot: join(root, 'split-binding-runs'),
  agentRuntimeServerCommand: process.execPath,
  agentRuntimeServerCommandArgs: [fakeAgentRuntimeServerScript],
  providerRegistryPath: defaultProviderRegistryPath,
}, {
  ...process.env,
  NARADA_PROVIDER_SECRET_STORE: 'disabled',
  NARADA_INTELLIGENCE_PROVIDER: 'kimi-code-api',
  KIMI_CODE_API_KEY: 'selected-kimi-worker-key',
  OPENAI_API_KEY: 'unrelated-openai-decoy',
  NARADA_SITE_ROOT: splitSiteRoot,
  NARADA_WORKSPACE_ROOT: splitWorkspaceRoot,
});
const splitBindingResolve = await rpc({ jsonrpc: '2.0', id: 5001, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: {
  intent: { instruction: 'resolve split Site and workspace binding' },
  constraints: { cwd: splitWorkspaceRoot, authority: 'read', overrides: { runtime: 'narada-agent-runtime-server' } },
} } }, splitBindingState);
assert.equal(splitBindingResolve.result?.structuredContent.resolved_worker_config.site_root, splitSiteRoot);
assert.equal(splitBindingResolve.result?.structuredContent.resolved_worker_config.workspace_root, splitWorkspaceRoot);
assert.equal(splitBindingResolve.result?.structuredContent.resolved_worker_config.site_root_source, 'bound_environment');
assert.equal(splitBindingResolve.result?.structuredContent.resolved_worker_config.site_binding.source, 'bound_environment');
assert.equal(splitBindingResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_SITE_ROOT'), true);
assert.equal(splitBindingResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_WORKSPACE_ROOT'), true);

const agentRuntimeResolve = await rpc({ jsonrpc: '2.0', id: 501, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: runArgs('server runtime resolve', { runtime: 'narada-agent-runtime-server' }) } }, agentRuntimeState);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.runtime, 'narada-agent-runtime-server');
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.authority, 'read');
assert.deepEqual(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.argv, ['--raw-jsonl', '--authority', 'read']);
assert.deepEqual(agentRuntimeResolve.result?.structuredContent.invocation.authority_signal, { kind: 'argv', name: '--authority', value: 'read' });
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_root, root);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_bound, true);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_marker, '.narada/');
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_root_source, 'nearest_marker');
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_binding.site_bound, true);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_binding.source, 'nearest_parent_marker');
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_binding.matched_marker, '.narada/');
assert.deepEqual(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_binding.required_markers, ['.narada/', '.ai/mcp/']);
assert.deepEqual(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_binding.environment_keys, ['NARADA_SITE_ROOT', 'NARADA_WORKSPACE_ROOT', 'NARADA_AGENT_ID', 'NARADA_CARRIER_SESSION_ID']);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.workspace_root, root);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.provider, null);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.provider_source, 'canonical_invocation_plan');
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.cognition, null);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.model, null);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.reasoning_effort, null);
assert.deepEqual(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.provider_runtime_binding, {
  schema: 'narada.worker.canonical-plan-binding.v1',
  source: 'narada-canonical-invocation-plan',
  plan_ref: 'plan:worker-test-canonical',
  provider_model_resolution: 'narada-runtime',
  provider: 'kimi-code-api',
  provider_source: 'canonical_plan_store',
  intent_ref: 'intent:worker-test-canonical',
  purpose: 'local-agent-runtime',
  model_ref: 'model:k3',
  model_provider_ref: 'model-provider:fixture',
  offering_ref: 'model-offering:kimi-code-api-k3',
  invocation_model_key: 'k3',
  options: { thinking: 'high' },
  snapshot_digest: `sha256:${'1'.repeat(64)}`,
  valid_until: '2099-01-01T00:00:00.000Z',
  credential_env_names: ['KIMI_CODE_API_KEY'],
  selector_crosses_worker_boundary: false,
  credential_materialization: 'final-adapter-boundary',
});
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_AGENT_ID'), true);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_CARRIER_SESSION_ID'), true);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_SITE_ROOT'), true);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_INTELLIGENCE_PROVIDER'), false);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_AI_API_KEY'), false);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_AI_BASE_URL'), false);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('KIMI_CODE_API_KEY'), true);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_INTELLIGENCE_PLAN_REF'), true);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('OPENAI_API_KEY'), false);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('KIMI_API_KEY'), false);
assert.equal(JSON.stringify(agentRuntimeResolve.result?.structuredContent).includes('selected-kimi-worker-key'), false);
assert.equal(JSON.stringify(agentRuntimeResolve.result?.structuredContent).includes('unrelated-openai-decoy'), false);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('CODEX_HOME'), true);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('CODEX_CONFIG_DIR'), true);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_WORKER_MCP_CONFIG'), false);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.worker_mcp_projection, undefined);
assert.equal(agentRuntimeResolve.result?.structuredContent.mcp_tool_verification.enforced_by_delegation, false);
assert.equal(agentRuntimeResolve.result?.structuredContent.mcp_tool_verification.enforcement_surface, null);
assert.equal(agentRuntimeResolve.result?.structuredContent.mcp_tool_verification.verification_state, 'no_tools_projected');
assert.equal(agentRuntimeResolve.result?.structuredContent.mcp_tool_verification.no_tools_posture, true);
assert.equal(agentRuntimeResolve.result?.structuredContent.preflight.some((check: any) => check.name === 'mcp_tool_projection' && check.status === 'warning'), true);
assert.equal(agentRuntimeResolve.result?.structuredContent.runtime_availability.available, true);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_bound, true);
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_root_source, 'nearest_marker');
assert.equal(agentRuntimeResolve.result?.structuredContent.resolved_worker_config.site_marker, '.narada/');
assert.match(agentRuntimeResolve.result?.content[0].text, /"site_bound": true/);
assert.match(agentRuntimeResolve.result?.content[0].text, /"site_root": /);
assert.match(agentRuntimeResolve.result?.content[0].text, /"workspace_root": /);
assert.match(agentRuntimeResolve.result?.content[0].text, /"NARADA_SITE_ROOT"/);
assert.match(agentRuntimeResolve.result?.content[0].text, /"provider": null/);

const nonDefaultProviderState = createServerState({
  allowedRoot: root,
  runRoot: join(root, 'agent-runtime-openai-runs'),
  agentRuntimeServerCommand: process.execPath,
  agentRuntimeServerCommandArgs: [fakeAgentRuntimeServerScript],
  providerRegistryPath: defaultProviderRegistryPath,
}, {
  ...process.env,
  NARADA_SITE_ROOT: '',
  NARADA_WORKSPACE_ROOT: '',
  NARADA_PROVIDER_SECRET_STORE: 'disabled',
  NARADA_INTELLIGENCE_PROVIDER: 'openai-api',
  KIMI_CODE_API_KEY: 'unrelated-kimi-decoy',
});
const nonDefaultProviderResolve = await rpc({ jsonrpc: '2.0', id: 50201, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: runArgs('server runtime non-default provider resolve', { runtime: 'narada-agent-runtime-server' }) } }, nonDefaultProviderState);
assert.equal(nonDefaultProviderResolve.error, undefined, JSON.stringify(nonDefaultProviderResolve));
assert.equal(nonDefaultProviderResolve.result?.structuredContent.resolved_worker_config.provider_runtime_binding.provider, 'kimi-code-api');
assert.equal(nonDefaultProviderResolve.result?.structuredContent.resolved_worker_config.provider_runtime_binding.provider_source, 'canonical_plan_store');
assert.deepEqual(nonDefaultProviderResolve.result?.structuredContent.resolved_worker_config.provider_runtime_binding.credential_env_names, ['KIMI_CODE_API_KEY']);
assert.equal(nonDefaultProviderResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_INTELLIGENCE_PROVIDER'), false);
assert.equal(nonDefaultProviderResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('KIMI_CODE_API_KEY'), true);

const agentRuntimeProviderResolve = await rpc({ jsonrpc: '2.0', id: 5011, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: {
  intent: { instruction: 'server runtime provider resolve' },
  constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true, provider: 'codex-subscription', overrides: { runtime: 'narada-agent-runtime-server' } },
} } }, agentRuntimeState);
assert.equal(agentRuntimeProviderResolve.error?.data.code, 'worker_canonical_invocation_plan_override_rejected');
const agentRuntimeScopedMcpResolve = await rpc({ jsonrpc: '2.0', id: 50101, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: {
  intent: { instruction: 'server runtime scoped mcp resolve' },
  constraints: { cwd: root, authority: 'read', wait_for_completion: true, required_mcp_tools: ['mailbox_messages_list'], overrides: { runtime: 'narada-agent-runtime-server' } },
} } }, agentRuntimeState);
assert.deepEqual(agentRuntimeScopedMcpResolve.result?.structuredContent.resolved_worker_config.worker_mcp_projection, {
  schema: 'narada.worker.mcp_projection.v1',
  native_mcp_mode: 'scoped',
  mcp_tool_allowlist: ['mailbox_messages_list'],
  include_startup_tools: true,
  include_output_readback_tools: false,
  full_site_mcp_requires_explicit_mode: true,
});
assert.equal(agentRuntimeScopedMcpResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_WORKER_MCP_CONFIG'), true);
assert.equal(agentRuntimeScopedMcpResolve.result?.structuredContent.resolved_worker_config.mcp_scope, 'local-site');
assert.equal(agentRuntimeScopedMcpResolve.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_MCP_SCOPE'), true);
assert.equal(agentRuntimeScopedMcpResolve.result?.structuredContent.mcp_tool_verification.enforced_by_delegation, true);
assert.equal(agentRuntimeScopedMcpResolve.result?.structuredContent.mcp_tool_verification.enforcement_surface, 'NARADA_WORKER_MCP_CONFIG');
const agentRuntimeModelResolve = await rpc({ jsonrpc: '2.0', id: 50111, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: {
  intent: { instruction: 'server runtime model resolve' },
  constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true, provider: 'codex-subscription', overrides: { runtime: 'narada-agent-runtime-server', model: 'gpt-5.5', reasoning_effort: 'medium' } },
} } }, agentRuntimeState);
assert.equal(agentRuntimeModelResolve.error?.data.code, 'worker_canonical_invocation_plan_override_rejected');
const agentRuntimeProviderMismatch = await rpc({ jsonrpc: '2.0', id: 5012, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: {
  intent: { instruction: 'provider mismatch' },
  constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true, provider: 'codex-subscription', overrides: { runtime: 'codex' } },
} } }, agentRuntimeState);
assert.equal(agentRuntimeProviderMismatch.error?.data.code, 'worker_narada_provider_runtime_mismatch');
const agentRuntimeRun = await rpc({ jsonrpc: '2.0', id: 502, method: 'tools/call', params: { name: 'worker_run', arguments: {
  intent: { instruction: 'server runtime worker' },
  constraints: { cwd: root, authority: 'read', wait_for_completion: true, required_mcp_tools: ['mailbox_messages_list'], overrides: { runtime: 'narada-agent-runtime-server' } },
} } }, agentRuntimeState);
assert.equal(agentRuntimeRun.result?.structuredContent.status, 'completed');
assert.equal(agentRuntimeRun.result?.structuredContent.runtime, 'narada-agent-runtime-server');
assert.equal(agentRuntimeRun.result?.structuredContent.worker_session_id, 'carrier-worker-runtime');
assert.equal(agentRuntimeRun.result?.structuredContent.summary, 'agent runtime worker ok');
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.command, process.execPath);
assert.deepEqual(agentRuntimeRun.result?.structuredContent.resolved_worker_config.command_args, [fakeAgentRuntimeServerScript]);
assert.deepEqual(agentRuntimeRun.result?.structuredContent.resolved_worker_config.argv, ['--raw-jsonl', '--authority', 'read', '--session', agentRuntimeRun.result?.structuredContent.run_id]);
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.site_root, root);
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.site_binding.source, 'nearest_parent_marker');
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.provider, null);
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.provider_source, 'canonical_invocation_plan');
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.cognition, null);
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.model, null);
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.reasoning_effort, null);
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.implementation_identity.surface_id, 'worker-delegation-mcp');
assert.equal(agentRuntimeRun.result?.structuredContent.session_event_evidence.prompt_admission, 'turn_started_without_visible_send_frame');
assert.equal(agentRuntimeRun.result?.structuredContent.session_event_evidence.assistant_message_seen, true);
assert.deepEqual(agentRuntimeRun.result?.structuredContent.session_event_evidence.mutation_admission, { carrier_mutation_admitted: true, delegated_mutation_admitted: true });
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_AGENT_ID'), true);
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_CARRIER_SESSION_ID'), true);
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.environment_keys.includes('NARADA_WORKER_MCP_CONFIG'), true);
assert.equal(agentRuntimeRun.result?.structuredContent.resolved_worker_config.mcp_scope, 'local-site');
assert.deepEqual(agentRuntimeRun.result?.structuredContent.resolved_worker_config.worker_mcp_projection.mcp_tool_allowlist, ['mailbox_messages_list']);
assert.equal(agentRuntimeRun.result?.structuredContent.mcp_tool_verification.enforced_by_delegation, true);
assert.equal(agentRuntimeRun.result?.structuredContent.verification_results[0].tool, 'fake-agent-runtime-server');
assert.equal(JSON.parse(agentRuntimeRun.result?.structuredContent.verification_results[0].summary.match(/env=(.*)$/)?.[1] ?? '{}').NARADA_WORKER_MCP_CONFIG.includes('mailbox_messages_list'), true);
assert.equal(JSON.parse(agentRuntimeRun.result?.structuredContent.verification_results[0].summary.match(/env=(.*)$/)?.[1] ?? '{}').NARADA_MCP_SCOPE, 'local-site');
const agentRuntimePrompt = readFileSync(join(agentRuntimeRun.result?.structuredContent.run_dir, 'worker_prompt.txt'), 'utf8');
assert.match(agentRuntimePrompt, /NARS worker completion guard/);
assert.match(agentRuntimePrompt, /Do not call pause, sleep, wait, delegation, or worker_\* tools/);
assert.match(agentRuntimePrompt, /Lifecycle MCP tools are permitted only when their exact names appear in the explicit MCP projection above/);
assert.match(agentRuntimePrompt, /Do not invent or guess tool names such as andrey-user-filesystem/);
assert.match(agentRuntimePrompt, /admission_required, surface_registry_tool_not_declared, mcp_runtime_fault/);
assert.match(agentRuntimePrompt, /Only the following exact MCP tool names are projected into this worker run/);
assert.match(agentRuntimePrompt, /- mailbox_messages_list/);
assert.match(readFileSync(join(agentRuntimeRun.result?.structuredContent.run_dir, 'events.jsonl'), 'utf8'), /turn_complete/);
const agentRuntimeInvocation = JSON.parse(readFileSync(join(agentRuntimeRun.result?.structuredContent.run_dir, 'worker_invocation.json'), 'utf8'));
assert.equal(agentRuntimeInvocation.authority, 'read');
assert.deepEqual(agentRuntimeInvocation.authority_signal, { kind: 'argv', name: '--authority', value: 'read' });
assert.equal(agentRuntimeInvocation.implementation_identity.surface_id, 'worker-delegation-mcp');
assert.equal(agentRuntimeInvocation.argv.includes('--authority'), true);
assert.equal(agentRuntimeInvocation.argv[agentRuntimeInvocation.argv.indexOf('--authority') + 1], 'read');
const agentRuntimeModelRun = await rpc({ jsonrpc: '2.0', id: 50201, method: 'tools/call', params: { name: 'worker_run', arguments: {
  intent: { instruction: 'server runtime model worker' },
  constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true, provider: 'codex-subscription', overrides: { runtime: 'narada-agent-runtime-server', model: 'gpt-5.5', reasoning_effort: 'medium' } },
} } }, agentRuntimeState);
assert.equal(agentRuntimeModelRun.error?.data.code, 'worker_canonical_invocation_plan_override_rejected');
const agentRuntimeWriteRun = await rpc({ jsonrpc: '2.0', id: 50200, method: 'tools/call', params: { name: 'worker_run', arguments: {
  intent: { instruction: 'server runtime write authority', mode: 'implement' },
  constraints: { cwd: root, authority: 'write', wait_for_completion: true, overrides: { runtime: 'narada-agent-runtime-server' } },
} } }, agentRuntimeState);
assert.equal(agentRuntimeWriteRun.result?.structuredContent.resolved_worker_config.authority, 'write');
assert.deepEqual(agentRuntimeWriteRun.result?.structuredContent.resolved_worker_config.argv, ['--raw-jsonl', '--authority', 'write', '--session', agentRuntimeWriteRun.result?.structuredContent.run_id]);
const agentRuntimeWriteInvocation = JSON.parse(readFileSync(join(agentRuntimeWriteRun.result?.structuredContent.run_dir, 'worker_invocation.json'), 'utf8'));
assert.equal(agentRuntimeWriteInvocation.authority, 'write');
assert.deepEqual(agentRuntimeWriteInvocation.authority_signal, { kind: 'argv', name: '--authority', value: 'write' });
assert.equal(agentRuntimeWriteInvocation.argv[agentRuntimeWriteInvocation.argv.indexOf('--authority') + 1], 'write');
const agentRuntimeProviderRun = await rpc({ jsonrpc: '2.0', id: 50201, method: 'tools/call', params: { name: 'worker_run', arguments: {
  intent: { instruction: 'server runtime provider worker' },
  constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true, provider: 'codex-subscription', overrides: { runtime: 'narada-agent-runtime-server' } },
} } }, agentRuntimeState);
assert.equal(agentRuntimeProviderRun.error?.data.code, 'worker_canonical_invocation_plan_override_rejected');
const agentRuntimeFailed = await rpc({ jsonrpc: '2.0', id: 5021, method: 'tools/call', params: { name: 'worker_run', arguments: runArgs('agent runtime provider failure', { runtime: 'narada-agent-runtime-server' }) } }, agentRuntimeState);
assert.equal(agentRuntimeFailed.error?.data.code, 'worker_runtime_failed');
assert.match(agentRuntimeFailed.error?.data.details.error, /rate_limit_reached_error/);
const agentRuntimeFailedRunId = String(agentRuntimeFailed.error?.data.details.run_id);
const agentRuntimeFailedStatus = await rpc({ jsonrpc: '2.0', id: 5022, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: agentRuntimeFailedRunId } } }, agentRuntimeState);
assert.match(agentRuntimeFailedStatus.result?.structuredContent.error, /rate_limit_reached_error/);
assert.equal(agentRuntimeFailedStatus.result?.structuredContent.error_classification, 'provider_rate_limited');
assert.equal(agentRuntimeFailedStatus.result?.structuredContent.runtime_diagnostics.phase, 'runtime_reported_failure');
assert.equal(agentRuntimeFailedStatus.result?.structuredContent.runtime_diagnostics.exit_code, 0);
assert.equal(agentRuntimeFailedStatus.result?.structuredContent.error_provenance.primary_source, 'provider');
assert.match(agentRuntimeFailedStatus.result?.structuredContent.error_provenance.provider_error, /rate_limit_reached_error/);
assert.match(agentRuntimeFailedStatus.result?.structuredContent.runtime_diagnostics.error_provenance.artifact_error, /missing_file/);
assert.match(agentRuntimeFailedStatus.result?.structuredContent.runtime_diagnostics.stdout_tail, /turn_failed/);
assert.equal(agentRuntimeFailedStatus.result?.structuredContent.progress.latest_event_type, 'turn_failed');
const agentRuntimeFailedWait = await rpc({ jsonrpc: '2.0', id: 5023, method: 'tools/call', params: { name: 'worker_run_wait', arguments: { run_id: agentRuntimeFailedRunId } } }, agentRuntimeState);
assert.match(agentRuntimeFailedWait.result?.structuredContent.run.error_preview, /rate_limit_reached_error/);
const agentRuntimeMcpToolFault = await rpc({ jsonrpc: '2.0', id: 50230, method: 'tools/call', params: { name: 'worker_run', arguments: runArgs('agent runtime mcp tool fault', { runtime: 'narada-agent-runtime-server' }) } }, agentRuntimeState);
assert.equal(agentRuntimeMcpToolFault.error?.data.code, 'worker_runtime_failed');
const agentRuntimeMcpToolFaultRunId = String(agentRuntimeMcpToolFault.error?.data.details.run_id);
const agentRuntimeMcpToolFaultStatus = await rpc({ jsonrpc: '2.0', id: 502301, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: agentRuntimeMcpToolFaultRunId } } }, agentRuntimeState);
assert.match(agentRuntimeMcpToolFaultStatus.result?.structuredContent.error, /MCP runtime fault/);
assert.equal(agentRuntimeMcpToolFaultStatus.result?.structuredContent.error_classification, 'mcp_tool_failure');
assert.equal(agentRuntimeMcpToolFaultStatus.result?.structuredContent.runtime_diagnostics.phase, 'mcp_tool_failure');
assert.deepEqual(agentRuntimeMcpToolFaultStatus.result?.structuredContent.runtime_diagnostics.assistant_extraction.terminal_events, ['mcp_tool_error']);
const agentRuntimeMcpToolFaultWait = await rpc({ jsonrpc: '2.0', id: 502302, method: 'tools/call', params: { name: 'worker_run_wait', arguments: { run_id: agentRuntimeMcpToolFaultRunId, timeout_ms: 0 } } }, agentRuntimeState);
assert.equal(agentRuntimeMcpToolFaultWait.result?.structuredContent.wait.status, 'finished');
assert.equal(agentRuntimeMcpToolFaultWait.result?.structuredContent.run.status, 'failed');
const agentRuntimeNoAssistant = await rpc({ jsonrpc: '2.0', id: 50231, method: 'tools/call', params: { name: 'worker_run', arguments: runArgs('agent runtime no assistant message', { runtime: 'narada-agent-runtime-server' }) } }, agentRuntimeState);
assert.equal(agentRuntimeNoAssistant.error?.data.code, 'worker_runtime_failed');
const agentRuntimeNoAssistantRunId = String(agentRuntimeNoAssistant.error?.data.details.run_id);
const agentRuntimeNoAssistantStatus = await rpc({ jsonrpc: '2.0', id: 50232, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: agentRuntimeNoAssistantRunId } } }, agentRuntimeState);
assert.equal(agentRuntimeNoAssistantStatus.result?.structuredContent.runtime_diagnostics.phase, 'pre_first_assistant_failure');
assert.equal(agentRuntimeNoAssistantStatus.result?.structuredContent.worker_output_state, 'absent');
assert.equal(agentRuntimeNoAssistantStatus.result?.structuredContent.worker_authored_output_present, false);
assert.equal(agentRuntimeNoAssistantStatus.result?.structuredContent.summary, null);
assert.equal(agentRuntimeNoAssistantStatus.result?.structuredContent.deliverables, null);
assert.equal(agentRuntimeNoAssistantStatus.result?.structuredContent.worker_output_error.reason, 'missing_file');
assert.equal(agentRuntimeNoAssistantStatus.result?.structuredContent.runtime_diagnostics.exit_code, 0);
assert.equal(agentRuntimeNoAssistantStatus.result?.structuredContent.session_event_evidence.prompt_admission, 'turn_started_without_visible_send_frame');
assert.equal(agentRuntimeNoAssistantStatus.result?.structuredContent.session_event_evidence.assistant_message_seen, false);
assert.deepEqual(agentRuntimeNoAssistantStatus.result?.structuredContent.session_event_evidence.terminal_events, []);
assert.equal(agentRuntimeNoAssistantStatus.result?.structuredContent.runtime_diagnostics.session_event_evidence.assistant_message_seen, false);
assert.match(agentRuntimeNoAssistantStatus.result?.structuredContent.runtime_diagnostics.diagnostic_tail, /pre-assistant diagnostic detail/);
assert.match(agentRuntimeNoAssistantStatus.result?.structuredContent.runtime_diagnostics.stdout_tail, /turn-no-assistant/);
assert.match(agentRuntimeNoAssistantStatus.result?.structuredContent.runtime_diagnostics.remediation.join(' '), /stdout_tail/);
const agentRuntimeTerminalNoAssistant = await rpc({ jsonrpc: '2.0', id: 50233, method: 'tools/call', params: { name: 'worker_run', arguments: runArgs('agent runtime terminal no assistant output', { runtime: 'narada-agent-runtime-server' }) } }, agentRuntimeState);
assert.equal(agentRuntimeTerminalNoAssistant.error?.data.code, 'worker_runtime_failed');
assert.match(agentRuntimeTerminalNoAssistant.error?.data.details.error, /agent_runtime_completed_without_assistant_output/);
const agentRuntimeTerminalNoAssistantRunId = String(agentRuntimeTerminalNoAssistant.error?.data.details.run_id);
const agentRuntimeTerminalNoAssistantStatus = await rpc({ jsonrpc: '2.0', id: 50234, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: agentRuntimeTerminalNoAssistantRunId } } }, agentRuntimeState);
assert.equal(agentRuntimeTerminalNoAssistantStatus.result?.structuredContent.runtime_diagnostics.phase, 'completed_without_assistant_output');
assert.deepEqual(agentRuntimeTerminalNoAssistantStatus.result?.structuredContent.session_event_evidence.terminal_events, ['turn_complete', 'session_closed']);
assert.equal(agentRuntimeTerminalNoAssistantStatus.result?.structuredContent.runtime_diagnostics.assistant_extraction.assistant_message_seen, false);
assert.equal(agentRuntimeTerminalNoAssistantStatus.result?.structuredContent.runtime_diagnostics.assistant_extraction.assistant_message_extracted, false);
assert.deepEqual(agentRuntimeTerminalNoAssistantStatus.result?.structuredContent.runtime_diagnostics.assistant_extraction.terminal_events, ['turn_complete', 'session_closed']);
assert.match(agentRuntimeTerminalNoAssistantStatus.result?.structuredContent.runtime_diagnostics.remediation.join(' '), /assistant_extraction/);
const agentRuntimeMessageField = await rpc({ jsonrpc: '2.0', id: 50235, method: 'tools/call', params: { name: 'worker_run', arguments: runArgs('agent runtime assistant message field', { runtime: 'narada-agent-runtime-server' }) } }, agentRuntimeState);
assert.equal(agentRuntimeMessageField.result?.structuredContent.status, 'completed');
assert.equal(agentRuntimeMessageField.result?.structuredContent.summary, 'agent runtime message-field output ok');
assert.equal(agentRuntimeMessageField.result?.structuredContent.session_event_evidence.assistant_message_seen, true);
assert.equal(agentRuntimeMessageField.result?.structuredContent.worker_authored_output_present, true);
const agentRuntimeLoose = await rpc({ jsonrpc: '2.0', id: 5024, method: 'tools/call', params: { name: 'worker_run', arguments: runArgs('server runtime loose output', { runtime: 'narada-agent-runtime-server' }) } }, agentRuntimeState);
assert.equal(agentRuntimeLoose.result?.structuredContent.status, 'completed');
assert.equal(agentRuntimeLoose.result?.structuredContent.summary, 'loose agent runtime worker ok');
assert.equal(agentRuntimeLoose.result?.structuredContent.verification_results[0].summary, 'loose verification object accepted');
assert.equal(agentRuntimeLoose.result?.structuredContent.exit_interview.ergonomics_feedback, 'loose output preserved');
assert.deepEqual(agentRuntimeLoose.result?.structuredContent.exit_interview.friction_points, ['verification object was not an array']);
const nonSiteRoot = join(root, 'not-a-site-outside-site-root');
mkdirSync(nonSiteRoot, { recursive: true });
const nonSiteState = createServerState({
  allowedRoot: nonSiteRoot,
  runRoot: join(root, 'non-site-runs'),
  agentRuntimeServerCommand: process.execPath,
  agentRuntimeServerCommandArgs: [fakeAgentRuntimeServerScript],
});
const nonSiteResolve = await rpc({ jsonrpc: '2.0', id: 503, method: 'tools/call', params: { name: 'worker_config_resolve', arguments: {
  intent: { instruction: 'server runtime outside site' },
  constraints: { cwd: nonSiteRoot, authority: 'read', cognition: 'low', wait_for_completion: true, overrides: { runtime: 'narada-agent-runtime-server' } },
} } }, nonSiteState);
assert.equal(nonSiteResolve.error?.data.code, 'worker_narada_site_root_not_found');
assert.deepEqual(nonSiteResolve.error?.data.details.required_markers, ['.narada/', '.ai/mcp/']);
assert.match(nonSiteResolve.error?.data.details.remediation, /\.narada\/ or \.ai\/mcp\//);
assert.match(nonSiteResolve.error?.data.details.remediation, /constraints\.site_root/);

if (process.platform === 'win32') {
  const caseInsensitiveRun = await rpc({
    jsonrpc: '2.0',
    id: 50,
    method: 'tools/call',
    params: { name: 'worker_run', arguments: runArgs(platformRootCase, { model: 'gpt-test', reasoning_effort: 'low', config: { model: 'gpt-test' } }) },
  }, state);
  assert.equal(caseInsensitiveRun.error, undefined);
  assert.equal(caseInsensitiveRun.result?.structuredContent.preflight.some((check: any) => check.name === 'cwd_readable' && check.status === 'ok'), true);
}
assert.deepEqual(allowedConfigRun.result?.structuredContent.final_checklist, ['state whether files were edited', 'list evidence inspected', 'list blocked or unreadable paths', 'separate recommendations from completed work']);
const completedRunDir = allowedConfigRun.result?.structuredContent.run_dir;
assert.deepEqual(allowedConfigRun.result?.content.map((item: any) => item.type), ['text']);
const listedResources = await rpc({ jsonrpc: '2.0', id: 51, method: 'resources/list', params: {} }, state);
const promptArtifact = listedResources.result?.resources.find((resource: any) => String(resource.uri).startsWith('worker-artifact:') && resource.name.endsWith('/worker_prompt.txt'));
assert.ok(promptArtifact);
const promptResource = await rpc({ jsonrpc: '2.0', id: 52, method: 'resources/read', params: { uri: promptArtifact.uri } }, state);
assert.match(promptResource.result?.contents[0].text, /Do not call any worker_\* MCP tools\./);
for (const file of ['request.json', 'executor_request.json', 'resolved_worker_config.json', 'worker_prompt.txt', 'worker_invocation.json', 'events.jsonl', 'diagnostic.log', 'last_message.json', 'result.json', 'worker_output.schema.json']) {
  assert.equal(existsSync(join(completedRunDir, file)), true, file);
}
const workerOutputSchema = JSON.parse(readFileSync(join(completedRunDir, 'worker_output.schema.json'), 'utf8'));
assertStrictStructuredOutputSchema(workerOutputSchema, 'worker_output_schema');
assert.equal(workerOutputSchema.required.includes('exit_interview'), true);
assert.equal(workerOutputSchema.required.includes('verification_budget_respected'), true);
assert.equal(workerOutputSchema.required.includes('broad_unrelated_failures'), true);
assert.deepEqual(workerOutputSchema.properties.verification.items.required, ['tool', 'command', 'status', 'summary', 'command_classification']);
assert.deepEqual(workerOutputSchema.properties.verification.items.properties.tool.type, ['string', 'null']);
assert.deepEqual(workerOutputSchema.properties.verification.items.properties.command.type, ['string', 'null']);
assert.deepEqual(workerOutputSchema.properties.verification.items.properties.command_classification.enum, ['focused', 'broad', 'not_applicable']);
assert.deepEqual(workerOutputSchema.properties.exit_interview.type, ['object', 'null']);
const request = JSON.parse(readFileSync(join(completedRunDir, 'request.json'), 'utf8'));
assert.equal(request.intent.instruction, 'run with allowed config');
assert.equal(request.constraints.cwd, root);
assert.equal(request.constraints.authority, 'read');
assert.equal(request.constraints.cognition, 'low');
assert.equal(request.constraints.resumable, undefined);
assert.equal(request.constraints.overrides.model, 'gpt-test');
const resolvedConfig = JSON.parse(readFileSync(join(completedRunDir, 'resolved_worker_config.json'), 'utf8'));
assert.equal(resolvedConfig.runtime, 'codex');
assert.equal(resolvedConfig.authority, 'read');
assert.equal(resolvedConfig.cognition, 'low');
assert.equal(resolvedConfig.command, process.execPath);
assert.deepEqual(resolvedConfig.command_args, [fakeCodexScript]);
assert.equal(resolvedConfig.resumable, false);
assert.equal(resolvedConfig.ephemeral, true);
assert.equal(resolvedConfig.config.model, 'gpt-test');
assert.equal(resolvedConfig.config.model_reasoning_effort, 'low');
assert.equal(resolvedConfig.environment_keys.includes('PATH'), true);
assert.equal(JSON.stringify(resolvedConfig).includes('must-not-leak'), false);
assert.equal(JSON.stringify(resolvedConfig).includes('deepseek-from-secret-store'), false);
const executorRequest = JSON.parse(readFileSync(join(completedRunDir, 'executor_request.json'), 'utf8'));
assert.equal(executorRequest.schema, 'narada.worker.executor_request.v1');
assert.equal(executorRequest.intent.instruction, 'run with allowed config');
assert.equal(executorRequest.intent.mode, 'audit_only');
assert.equal(executorRequest.requested_mode, 'audit_only');
assert.equal(executorRequest.preflight.some((check: any) => check.name === 'cwd_readable' && check.status === 'ok'), true);
assert.equal(executorRequest.resolved_execution_policy.cwd, root);
assert.equal(executorRequest.resolved_execution_policy.authority, 'read');
assert.equal(executorRequest.resolved_execution_policy.cognition, 'low');
const invocation = JSON.parse(readFileSync(join(completedRunDir, 'worker_invocation.json'), 'utf8'));
assert.equal(invocation.argv[0], fakeCodexScript);
assert.equal(invocation.argv[1], 'exec');
assert.equal(invocation.argv.includes('--ephemeral'), true);
assert.equal(invocation.argv.includes('--json'), true);
assert.equal(invocation.argv.at(-1), '-');

const legacyRunId = 'run-20990101T000000Z-legacy1';
const legacyRunDir = join(runRoot, legacyRunId);
mkdirSync(legacyRunDir, { recursive: true });
writeFileSync(join(legacyRunDir, 'result.json'), JSON.stringify({
  schema: 'narada.worker.run.v1',
  status: 'completed',
  run_id: legacyRunId,
  run_dir: legacyRunDir,
  runtime: 'codex',
  worker_session_id: null,
  resolved_worker_config: { authority: 'read' },
  requested_mode: 'audit_only',
  executor_request: { intent: {} },
  summary: 'legacy run',
  deliverables: [],
  open_questions: [],
  next_actions: [],
  artifacts: [],
  timing: { started_at: '2099-01-01T00:00:00.000Z', finished_at: '2099-01-01T00:00:01.000Z', duration_ms: 1000 },
  error: null,
}), 'utf8');
const legacyList = await rpc({ jsonrpc: '2.0', id: 520, method: 'tools/call', params: { name: 'worker_runs_list', arguments: { limit: 200 } } }, state);
const legacyListItem = legacyList.result?.structuredContent.runs.find((run: any) => run.run_id === legacyRunId);
assert.equal(legacyListItem?.requested_mode, 'audit_only');
assert.equal(legacyListItem?.requested_mode_inferred, false);

const asyncRun = await rpc({
  jsonrpc: '2.0',
  id: 521,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'default async run' }, constraints: { cwd: root, authority: 'read', cognition: 'low' } } },
}, state);
assert.equal(asyncRun.result?.structuredContent.status, 'running');
assert.equal(asyncRun.result?.structuredContent.confidence, 'pending');
assert.equal(asyncRun.result?.structuredContent.completion_state, 'pending');
assert.equal(asyncRun.result?.structuredContent.result_state.state, 'pending');
assert.equal(asyncRun.result?.structuredContent.result_state.scaffold, true);
assert.equal(asyncRun.result?.structuredContent.result_state.terminal, false);
assert.deepEqual(asyncRun.result?.content.map((item: any) => item.type), ['text']);
assert.equal(asyncRun.result?.structuredContent.timing.finished_at, null);
assert.deepEqual(asyncRun.result?.structuredContent.progress, { event_count: 0, latest_event_type: null, latest_event_preview: null, latest_event_at: null, readable: true, tail_truncated: false });
assert.equal(state.activeRunCount === 0 || state.activeRunCount === 1, true);
const listedRuns = await rpc({ jsonrpc: '2.0', id: 522, method: 'tools/call', params: { name: 'worker_runs_list', arguments: { limit: 20 } } }, state);
assert.ok(listedRuns.result, JSON.stringify(listedRuns.error));
assert.equal(listedRuns.result?.structuredContent.runs.some((run: any) => run.run_id === asyncRun.result?.structuredContent.run_id), true);
assert.equal(typeof listedRuns.result?.structuredContent.scanned, 'number');
assert.equal(typeof listedRuns.result?.structuredContent.scan_limit, 'number');
assert.equal(typeof listedRuns.result?.structuredContent.scan_truncated, 'boolean');
assert.equal(listedRuns.result?.structuredContent.runs[0].summary, undefined);
assert.equal(typeof listedRuns.result?.structuredContent.runs[0].summary_preview === 'string' || listedRuns.result?.structuredContent.runs[0].summary_preview === null, true);
assert.equal(['complete', 'partial', 'pending', null].includes(listedRuns.result?.structuredContent.runs[0].completion_state), true);
assert.equal(typeof listedRuns.result?.structuredContent.runs[0].requested_mode, 'string');
assert.equal(typeof listedRuns.result?.structuredContent.runs[0].authority, 'string');
const asyncStatus = await rpc({ jsonrpc: '2.0', id: 523, method: 'tools/call', params: { name: 'worker_run_wait', arguments: { run_id: asyncRun.result?.structuredContent.run_id, timeout_ms: 15_000, poll_ms: 25 } } }, state);
assert.equal(asyncStatus.result?.structuredContent.schema, 'narada.worker.run_wait.v1');
assert.equal(asyncStatus.result?.structuredContent.wait.status, 'finished');
assert.equal(asyncStatus.result?.structuredContent.run.summary, undefined);
assert.equal(asyncStatus.result?.structuredContent.run.summary_preview, 'worker ok');
assert.match(String(asyncStatus.result?.structuredContent.run.progress_preview), /thread-created/);
assert.equal(asyncStatus.result?.structuredContent.full_run, undefined);
assert.equal(state.activeRunCount, 0);
const terminalReap = await rpc({ jsonrpc: '2.0', id: 52301, method: 'tools/call', params: { name: 'worker_run_reap', arguments: { run_id: asyncRun.result?.structuredContent.run_id, reason: 'already terminal no-op test' } } }, state);
assert.equal(terminalReap.result?.structuredContent.status, 'already_terminal');
assert.equal(terminalReap.result?.structuredContent.reaped, false);
const directStatus = await rpc({ jsonrpc: '2.0', id: 5231, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: asyncRun.result?.structuredContent.run_id } } }, state);
assert.match(String(directStatus.result?.structuredContent.progress.latest_event_preview), /thread-created/);
assert.equal(directStatus.result?.structuredContent.progress_state.state, 'completed');
assert.equal(directStatus.result?.structuredContent.progress_state.recommended_action, 'inspect_result');
assert.equal(typeof directStatus.result?.structuredContent.budget_status.elapsed_ms, 'number');
assert.equal(Array.isArray(directStatus.result?.structuredContent.recent_activity), true);
assert.equal(directStatus.result?.structuredContent.recent_activity.length > 0, true);
assert.equal(typeof directStatus.result?.structuredContent.recent_activity[0].kind, 'string');
assert.equal(directStatus.result?.structuredContent.exit_interview, null);
assert.equal(directStatus.result?.structuredContent.artifact_readback.readable_via_worker_delegation, true);
assert.equal(directStatus.result?.structuredContent.artifact_readback.local_filesystem_access_required, false);
const idempotentArgs = {
  idempotency_key: 'sop_handoff:run-1:agent-step',
  intent: {
    instruction: 'idempotent child execution',
    output_contract: {
      structured_output_key: 'sop_handoff_result',
      strict: true,
    },
  },
  constraints: {
    cwd: root,
    authority: 'read',
    cognition: 'low',
    wait_for_completion: true,
    wait_timeout_ms: 15_000,
  },
};
const idempotentFirst = await rpc({ jsonrpc: '2.0', id: 5232, method: 'tools/call', params: { name: 'worker_run', arguments: idempotentArgs } }, state);
assert.equal(idempotentFirst.error, undefined, JSON.stringify(idempotentFirst.error));
assert.equal(idempotentFirst.result?.structuredContent.status, 'completed');
assert.equal(idempotentFirst.result?.structuredContent.idempotency_key, idempotentArgs.idempotency_key);
assert.equal(idempotentFirst.result?.structuredContent.idempotency_replayed, false);
assert.equal(idempotentFirst.result?.structuredContent.output_contract.structured_output_key, 'sop_handoff_result');
const idempotentReplay = await rpc({ jsonrpc: '2.0', id: 5233, method: 'tools/call', params: { name: 'worker_run', arguments: idempotentArgs } }, state);
assert.equal(idempotentReplay.error, undefined, JSON.stringify(idempotentReplay.error));
assert.equal(idempotentReplay.result?.structuredContent.run_id, idempotentFirst.result?.structuredContent.run_id);
assert.equal(idempotentReplay.result?.structuredContent.idempotency_replayed, true);
const idempotentConflict = await rpc({
  jsonrpc: '2.0',
  id: 5234,
  method: 'tools/call',
  params: {
    name: 'worker_run',
    arguments: {
      ...idempotentArgs,
      intent: { ...idempotentArgs.intent, instruction: 'conflicting child execution' },
    },
  },
}, state);
assert.equal(idempotentConflict.error?.data.code, 'worker_run_idempotency_conflict');
const runDashboard = await rpc({ jsonrpc: '2.0', id: 52315, method: 'tools/call', params: { name: 'worker_dashboard_describe', arguments: { run_id: asyncRun.result?.structuredContent.run_id } } }, state);
assert.equal(runDashboard.result?.structuredContent.schema, 'narada.worker.dashboard.v1');
assert.equal(runDashboard.result?.structuredContent.mode, 'single_run');
assert.equal(runDashboard.result?.structuredContent.include_terminal, true);
assert.equal(runDashboard.result?.structuredContent.dashboard.server.started, false);
assert.equal(runDashboard.result?.structuredContent.dashboard.api_endpoints.some((endpoint: any) => endpoint.path === 'mcp://tools/worker_run_status'), true);
assert.equal(runDashboard.result?.structuredContent.runs[0].run_id, asyncRun.result?.structuredContent.run_id);
assert.equal(runDashboard.result?.structuredContent.runs[0].progress_state.state, 'completed');
assert.equal(typeof runDashboard.result?.structuredContent.runs[0].budget_status.event_count, 'number');
assert.equal(runDashboard.result?.structuredContent.runs[0].recent_activity.length > 0, true);
assert.equal(runDashboard.result?.structuredContent.runs[0].worker_session_id, 'thread-created');
assert.equal(runDashboard.result?.structuredContent.runs[0].result_refs.some((ref: any) => ref.name === 'events.jsonl'), true);
assert.equal(runDashboard.result?.structuredContent.topology.nodes[0].id, asyncRun.result?.structuredContent.run_id);
assert.deepEqual(runDashboard.result?.structuredContent.topology.edges, []);
assert.equal(runDashboard.result?.structuredContent.steps[0].step_id, `run:${asyncRun.result?.structuredContent.run_id}`);
assert.equal(runDashboard.result?.structuredContent.event_stream.some((event: any) => event.run_id === asyncRun.result?.structuredContent.run_id && String(event.preview).includes('thread-created')), true);
assert.match(runDashboard.result?.content[0].text, /"schema": "narada\.worker\.dashboard\.v1"/);
const activeDashboard = await rpc({ jsonrpc: '2.0', id: 52316, method: 'tools/call', params: { name: 'worker_dashboard_describe', arguments: { mode: 'all_active', limit: 50 } } }, state);
assert.equal(activeDashboard.result?.structuredContent.mode, 'all_active');
assert.equal(activeDashboard.result?.structuredContent.runs.some((run: any) => run.run_id === asyncRun.result?.structuredContent.run_id), false);
assert.equal(activeDashboard.result?.structuredContent.counts.terminal, 0);
const batchRun = await rpc({ jsonrpc: '2.0', id: 52311, method: 'tools/call', params: { name: 'worker_run_batch', arguments: { requests: [
                { intent: { instruction: 'batch one' }, constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true } },
  { intent: { instruction: 'batch two' }, constraints: { cwd: root, authority: 'read', wait_for_completion: true, required_mcp_tools: ['local-filesystem.fs_read_file'], overrides: { runtime: 'narada-agent-runtime-server' } } },
] } } }, state);
assert.equal(batchRun.result?.structuredContent.schema, 'narada.worker.run_batch.v1');
assert.equal(batchRun.result?.structuredContent.status, 'ok');
assert.equal(batchRun.result?.structuredContent.run_ids.length, 2);
const batchWait = await rpc({ jsonrpc: '2.0', id: 52312, method: 'tools/call', params: { name: 'worker_run_wait_batch', arguments: { run_ids: batchRun.result?.structuredContent.run_ids, timeout_ms: 0, summary_only: true } } }, state);
assert.equal(batchWait.result?.structuredContent.schema, 'narada.worker.run_wait_batch.v1');
assert.equal(batchWait.result?.structuredContent.finished_count, 2);
assert.equal(batchWait.result?.structuredContent.errored_count, 0);
assert.equal(batchWait.result?.structuredContent.synthesis.rows.length, 2);
assert.equal(batchWait.result?.structuredContent.synthesis.rows[1].verification[0].tool, 'fake-agent-runtime-server');
const partialBatchWait = await rpc({ jsonrpc: '2.0', id: 523121, method: 'tools/call', params: { name: 'worker_run_wait_batch', arguments: { run_ids: [batchRun.result?.structuredContent.run_ids[0], 'run-missing-for-partial-batch'], timeout_ms: 0, summary_only: true } } }, state);
assert.equal(partialBatchWait.error, undefined);
assert.equal(partialBatchWait.result?.structuredContent.status, 'partial');
assert.equal(partialBatchWait.result?.structuredContent.requested_count, 2);
assert.equal(partialBatchWait.result?.structuredContent.finished_count, 1);
assert.equal(partialBatchWait.result?.structuredContent.errored_count, 1);
assert.equal(partialBatchWait.result?.structuredContent.runs[1].run_id, 'run-missing-for-partial-batch');
assert.equal(partialBatchWait.result?.structuredContent.runs[1].wait.status, 'error');
const batchSynthesis = await rpc({ jsonrpc: '2.0', id: 52313, method: 'tools/call', params: { name: 'worker_runs_synthesize', arguments: { run_ids: batchRun.result?.structuredContent.run_ids } } }, state);
assert.equal(batchSynthesis.result?.structuredContent.schema, 'narada.worker.runs_synthesis.v1');
assert.equal(batchSynthesis.result?.structuredContent.synthesis.rows[0].summary, 'worker ok');
assert.equal(batchSynthesis.result?.structuredContent.synthesis.rows[0].partial_failure.status, 'not_failed');
assert.equal(typeof batchSynthesis.result?.structuredContent.synthesis.rows[0].budget_status.event_count, 'number');
const batchSecondStatus = await rpc({ jsonrpc: '2.0', id: 52314, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: batchRun.result?.structuredContent.run_ids[1] } } }, state);
assert.deepEqual(batchSecondStatus.result?.structuredContent.requested_mcp_tools, ['local-filesystem.fs_read_file']);
assert.equal(batchSecondStatus.result?.structuredContent.mcp_tool_verification.runtime_can_project, true);
assert.equal(batchSecondStatus.result?.structuredContent.mcp_tool_verification.verification_state, 'projected_to_worker_runtime');
assert.equal(batchSecondStatus.result?.structuredContent.mcp_tool_verification.fallback_reason_required, false);
assert.equal(batchSecondStatus.result?.structuredContent.output_contract.confidence_level.minimum, 0);
const invalidItemBatch = await rpc({ jsonrpc: '2.0', id: 523145, method: 'tools/call', params: { name: 'worker_run_batch', arguments: { requests: [
  { intent: { instruction: 'batch valid before invalid item' }, constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true } },
  null,
] } } }, state);
assert.equal(invalidItemBatch.result?.structuredContent.schema, 'narada.worker.run_batch.v1');
assert.equal(invalidItemBatch.result?.structuredContent.status, 'completed_with_errors');
assert.equal(invalidItemBatch.result?.structuredContent.started_count, 1);
assert.equal(invalidItemBatch.result?.structuredContent.failed_count, 1);
assert.equal(invalidItemBatch.result?.structuredContent.failures[0].index, 1);
assert.equal(invalidItemBatch.result?.structuredContent.failures[0].code, 'worker_run_batch_item_invalid');
const serializedBatchStartedAt = Date.now();
const serializedBatch = await rpc({ jsonrpc: '2.0', id: 52315, method: 'tools/call', params: { name: 'worker_run_batch', arguments: {
  max_parallel_runs: 1,
  requests: [
    { intent: { instruction: 'batch delayed one' }, constraints: { cwd: root, authority: 'read', cognition: 'low' } },
    { intent: { instruction: 'batch delayed two' }, constraints: { cwd: root, authority: 'read', cognition: 'low' } },
  ],
} } }, state);
assert.equal(serializedBatch.error, undefined);
assert.equal(serializedBatch.result?.structuredContent.run_ids.length, 2);
assert.ok(Date.now() - serializedBatchStartedAt >= 100, 'second async batch run must wait for first run capacity');
assert.equal(serializedBatch.result?.structuredContent.runs[0].status, 'completed');
await rpc({ jsonrpc: '2.0', id: 523151, method: 'tools/call', params: { name: 'worker_run_wait_batch', arguments: {
  run_ids: serializedBatch.result?.structuredContent.run_ids,
  timeout_ms: 5000,
  summary_only: true,
} } }, state);
const exitInterviewRun = await rpc({ jsonrpc: '2.0', id: 5233, method: 'tools/call', params: { name: 'worker_run', arguments: { intent: { instruction: 'ask for ergonomics feedback' }, constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true, exit_interview: true } } } }, state);
assert.equal(exitInterviewRun.result?.structuredContent.status, 'completed');
assert.equal(exitInterviewRun.result?.structuredContent.exit_interview.ergonomics_feedback, 'fake worker found the exit interview easy to answer');
assert.deepEqual(exitInterviewRun.result?.structuredContent.exit_interview.friction_points, ['progress visibility was limited']);
assert.deepEqual(exitInterviewRun.result?.structuredContent.exit_interview.observed_incoherencies, ['status naming was too coarse']);
assert.match(readFileSync(join(exitInterviewRun.result?.structuredContent.run_dir, 'worker_prompt.txt'), 'utf8'), /Exit interview/);
const orphanedRunId = 'run-20000101T000002Z-orphan1';
const orphanedRunDir = join(runRoot, orphanedRunId);
mkdirSync(orphanedRunDir, { recursive: true });
writeFileSync(join(orphanedRunDir, 'events.jsonl'), '', 'utf8');
writeFileSync(join(orphanedRunDir, 'last_message.json'), JSON.stringify({
  summary: 'orphaned worker output',
  deliverables: [{ path: 'artifact.txt', description: 'usable artifact' }],
  open_questions: [],
  next_actions: ['inspect recovered output'],
  edits_performed: true,
  target_state_changed: true,
  changes: [{ path: 'artifact.txt', status: 'modified', summary: 'recovered change' }],
  verification: [{ tool: 'manual', command: null, status: 'passed', summary: 'output parsed' }],
}), 'utf8');
writeFileSync(join(orphanedRunDir, 'result.json'), JSON.stringify({
  schema: 'narada.worker.run.v1',
  status: 'running',
  run_id: orphanedRunId,
  run_dir: orphanedRunDir,
  runtime: 'codex',
  worker_session_id: null,
  resolved_worker_config: { authority: 'write', max_run_ms: 1000 },
  executor_request: { requested_mode: 'implement' },
  requested_mode: 'implement',
  edits_performed: null,
  target_state_changed: null,
  confidence: 'complete',
  blocked_paths: [],
  verification: [],
  runtime_warnings: [],
  warning_count: 0,
  preflight: [],
  final_checklist: [],
  summary: '',
  deliverables: [],
  open_questions: [],
  next_actions: [],
  changes: [],
  verification_results: [],
  artifacts: [],
  timing: { started_at: '2000-01-01T00:00:02.000Z', finished_at: null, duration_ms: null },
  error: null,
}), 'utf8');
const orphanedStatus = await rpc({ jsonrpc: '2.0', id: 5232, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: orphanedRunId } } }, state);
assert.equal(orphanedStatus.result?.structuredContent.status, 'completed_with_errors');
assert.equal(orphanedStatus.result?.structuredContent.summary, 'orphaned worker output');
assert.equal(orphanedStatus.result?.structuredContent.warning_count, 1);
assert.match(orphanedStatus.result?.structuredContent.error, /worker_run_orphaned_final_output/);
const legacyHomeRunId = 'run-20000101T000002Z-legacyhome';
const legacyHomeRunDir = join(root, 'worker-delegation', 'runs', legacyHomeRunId);
mkdirSync(legacyHomeRunDir, { recursive: true });
writeFileSync(join(legacyHomeRunDir, 'events.jsonl'), JSON.stringify({ type: 'turn.completed', timestamp: '2000-01-01T00:00:03.000Z', text: 'legacy complete' }) + '\n', 'utf8');
writeFileSync(join(legacyHomeRunDir, 'diagnostic.log'), 'legacy diagnostic detail\n', 'utf8');
writeFileSync(join(legacyHomeRunDir, 'worker_invocation.json'), JSON.stringify({ command: 'codex', argv: ['exec'], cwd: root }), 'utf8');
writeFileSync(join(legacyHomeRunDir, 'resolved_worker_config.json'), JSON.stringify({ runtime: 'codex', authority: 'read', secret_like: 'not-secret' }), 'utf8');
writeFileSync(join(legacyHomeRunDir, 'result.json'), JSON.stringify({
  schema: 'narada.worker.run.v1',
  status: 'completed',
  run_id: legacyHomeRunId,
  run_dir: legacyHomeRunDir,
  runtime: 'codex',
  worker_session_id: 'legacy-session',
  resolved_worker_config: { authority: 'read', max_run_ms: 1000 },
  executor_request: { requested_mode: 'audit_only', preflight: [] },
  requested_mode: 'audit_only',
  edits_performed: false,
  target_state_changed: false,
  confidence: 'complete',
  completion_state: 'complete',
  blocked_paths: [],
  verification: [],
  runtime_warnings: [],
  warning_count: 0,
  preflight: [],
  final_checklist: [],
  summary: 'legacy completed worker',
  deliverables: [],
  open_questions: [],
  next_actions: [],
  changes: [],
  verification_results: [],
  artifacts: [],
  timing: { started_at: '2000-01-01T00:00:02.000Z', finished_at: '2000-01-01T00:00:03.000Z', duration_ms: 1000 },
  error: null,
}), 'utf8');
const originalUserProfile = process.env.USERPROFILE;
process.env.USERPROFILE = root;
const legacyHomeStatus = await rpc({ jsonrpc: '2.0', id: 52322, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: legacyHomeRunId } } }, state);
if (originalUserProfile === undefined) delete process.env.USERPROFILE; else process.env.USERPROFILE = originalUserProfile;
assert.equal(legacyHomeStatus.result?.structuredContent.status, 'completed');
assert.equal(legacyHomeStatus.result?.structuredContent.summary, 'legacy completed worker');
assert.equal(legacyHomeStatus.result?.structuredContent.artifact_readback.rediscovered, true);
assert.equal(legacyHomeStatus.result?.structuredContent.artifact_readback.resources_available, false);
assert.match(legacyHomeStatus.result?.structuredContent.artifact_readback.diagnostic_tail, /legacy diagnostic detail/);
assert.match(legacyHomeStatus.result?.structuredContent.artifact_readback.events_tail, /legacy complete/);
const expiredRunId = 'run-20000101T000002Z-expire1';
const expiredRunDir = join(runRoot, expiredRunId);
mkdirSync(expiredRunDir, { recursive: true });
writeFileSync(join(expiredRunDir, 'events.jsonl'), JSON.stringify({ type: 'item.completed', timestamp: '2000-01-01T00:00:02.000Z' }) + '\n', 'utf8');
writeFileSync(join(expiredRunDir, 'diagnostic.log'), 'runtime process stopped before final message\n', 'utf8');
writeFileSync(join(expiredRunDir, 'result.json'), JSON.stringify({
  schema: 'narada.worker.run.v1',
  status: 'running',
  run_id: expiredRunId,
  run_dir: expiredRunDir,
  runtime: 'codex',
  worker_session_id: null,
  resolved_worker_config: { authority: 'read', max_run_ms: 1000 },
  executor_request: { requested_mode: 'audit_only', preflight: [] },
  requested_mode: 'audit_only',
  edits_performed: null,
  target_state_changed: null,
  confidence: 'complete',
  completion_state: 'complete',
  blocked_paths: [],
  verification: [],
  runtime_warnings: [],
  warning_count: 0,
  preflight: [],
  final_checklist: [],
  summary: '',
  deliverables: [],
  open_questions: [],
  next_actions: [],
  changes: [],
  verification_results: [],
  artifacts: [],
  timing: { started_at: '2000-01-01T00:00:02.000Z', finished_at: null, duration_ms: null },
  error: null,
}), 'utf8');
const expiredStatus = await rpc({ jsonrpc: '2.0', id: 52320, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: expiredRunId } } }, state);
assert.equal(expiredStatus.result?.structuredContent.status, 'failed');
assert.equal(expiredStatus.result?.structuredContent.completion_state, 'partial');
assert.equal(expiredStatus.result?.structuredContent.error_classification, 'worker_run_expired_without_terminal_output');
assert.match(expiredStatus.result?.structuredContent.error, /expired_without_terminal_output/);
const persistedExpiredStatus = JSON.parse(readFileSync(join(expiredRunDir, 'result.json'), 'utf8'));
assert.equal(persistedExpiredStatus.status, 'failed');
assert.equal(persistedExpiredStatus.error_classification, 'worker_run_expired_without_terminal_output');
assert.match(persistedExpiredStatus.diagnostic_tail, /runtime process stopped before final message/);
assert.equal(expiredStatus.result?.structuredContent.progress.latest_event_type, 'item.completed');
assert.equal(expiredStatus.result?.structuredContent.progress.latest_event_at, '2000-01-01T00:00:02.000Z');
const staleRunId = 'run-20990101T000002Z-stale1';
const staleRunDir = join(runRoot, staleRunId);
const staleStartedAt = new Date(Date.now() - 10 * 60_000).toISOString();
mkdirSync(staleRunDir, { recursive: true });
writeFileSync(join(staleRunDir, 'events.jsonl'), JSON.stringify({ type: 'item.completed', timestamp: staleStartedAt }) + '\n', 'utf8');
writeFileSync(join(staleRunDir, 'result.json'), JSON.stringify({
  schema: 'narada.worker.run.v1',
  status: 'running',
  run_id: staleRunId,
  run_dir: staleRunDir,
  runtime: 'codex',
  worker_session_id: null,
  resolved_worker_config: { authority: 'read', max_run_ms: 60 * 60_000 },
  executor_request: { requested_mode: 'audit_only', preflight: [] },
  requested_mode: 'audit_only',
  edits_performed: null,
  target_state_changed: null,
  confidence: 'complete',
  completion_state: 'complete',
  blocked_paths: [],
  verification: [],
  runtime_warnings: [],
  warning_count: 0,
  preflight: [],
  final_checklist: [],
  summary: '',
  deliverables: [],
  open_questions: [],
  next_actions: [],
  changes: [],
  verification_results: [],
  artifacts: [],
  timing: { started_at: staleStartedAt, finished_at: null, duration_ms: null },
  error: null,
}), 'utf8');
const staleStatus = await rpc({ jsonrpc: '2.0', id: 52319, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: staleRunId } } }, state);
assert.equal(staleStatus.result?.structuredContent.status, 'running');
assert.equal(staleStatus.result?.structuredContent.completion_state, 'partial');
assert.equal(staleStatus.result?.structuredContent.status_liveness.state, 'stale');
assert.equal(staleStatus.result?.structuredContent.status_liveness.process_liveness, 'unknown');
assert.equal(typeof staleStatus.result?.structuredContent.status_liveness.stale_for_ms, 'number');
assert.equal(staleStatus.result?.structuredContent.progress_state.state, 'idle_stale');
assert.equal(staleStatus.result?.structuredContent.progress_state.recommended_action, 'inspect_artifacts');
assert.equal(staleStatus.result?.structuredContent.budget_status.event_count, 1);
assert.equal(staleStatus.result?.structuredContent.recent_activity[0].kind, 'model_turn');
const freshRunId = 'run-20990101T000003Z-fresh1';
const freshRunDir = join(runRoot, freshRunId);
const freshStartedAt = new Date().toISOString();
mkdirSync(freshRunDir, { recursive: true });
writeFileSync(join(freshRunDir, 'events.jsonl'), JSON.stringify({ type: 'item.completed', timestamp: freshStartedAt }) + '\n', 'utf8');
writeFileSync(join(freshRunDir, 'result.json'), JSON.stringify({
  schema: 'narada.worker.run.v1',
  status: 'running',
  run_id: freshRunId,
  run_dir: freshRunDir,
  runtime: 'codex',
  worker_session_id: null,
  resolved_worker_config: { authority: 'read', max_run_ms: 60 * 60_000 },
  executor_request: { requested_mode: 'audit_only', preflight: [] },
  requested_mode: 'audit_only',
  confidence: 'complete',
  completion_state: 'complete',
  runtime_warnings: [],
  warning_count: 0,
  summary: '',
  deliverables: [],
  open_questions: [],
  next_actions: [],
  timing: { started_at: freshStartedAt, finished_at: null, duration_ms: null },
  error: null,
}), 'utf8');
const activeReapDenied = await rpc({ jsonrpc: '2.0', id: 523191, method: 'tools/call', params: { name: 'worker_run_reap', arguments: { run_id: freshRunId, reason: 'active refusal test' } } }, state);
assert.equal(activeReapDenied.error?.data?.code, 'worker_run_reap_refused_active_run');
const staleReap = await rpc({ jsonrpc: '2.0', id: 523192, method: 'tools/call', params: { name: 'worker_run_reap', arguments: { run_id: staleRunId, reason: 'test stale cleanup' } } }, state);
assert.equal(staleReap.result?.structuredContent.status, 'reaped');
assert.equal(staleReap.result?.structuredContent.reaped, true);
assert.equal(staleReap.result?.structuredContent.run.status, 'cancelled');
assert.equal(staleReap.result?.structuredContent.run.error_classification, 'worker_run_reaped_stale_orphan');
assert.equal(staleReap.result?.structuredContent.evidence.stale_confirmed, true);
assert.equal(staleReap.result?.structuredContent.evidence.process_verification, 'not_available:no_run_pid_recorded');
const staleReapedPersisted = JSON.parse(readFileSync(join(staleRunDir, 'result.json'), 'utf8'));
assert.equal(staleReapedPersisted.status, 'cancelled');
assert.equal(staleReapedPersisted.reaped.reason, 'test stale cleanup');
const eventRecoveredRunId = 'run-20000101T000003Z-events1';
const eventRecoveredRunDir = join(runRoot, eventRecoveredRunId);
mkdirSync(eventRecoveredRunDir, { recursive: true });
writeFileSync(join(eventRecoveredRunDir, 'events.jsonl'), [
  JSON.stringify({ type: 'thread.started', thread_id: 'thread-events-recovered' }),
  JSON.stringify({ type: 'agent_message', message: 'Recovered recommendation from events.', timestamp: '2000-01-01T00:00:04.000Z' }),
  JSON.stringify({ type: 'turn.completed', timestamp: '2000-01-01T00:00:05.000Z' }),
].join('\n') + '\n', 'utf8');
writeFileSync(join(eventRecoveredRunDir, 'result.json'), JSON.stringify({
  schema: 'narada.worker.run.v1',
  status: 'running',
  run_id: eventRecoveredRunId,
  run_dir: eventRecoveredRunDir,
  runtime: 'codex',
  worker_session_id: null,
  resolved_worker_config: { authority: 'read', max_run_ms: 60_000 },
  executor_request: { requested_mode: 'audit_only', preflight: [] },
  requested_mode: 'audit_only',
  edits_performed: null,
  target_state_changed: null,
  confidence: 'complete',
  blocked_paths: [],
  verification: [],
  runtime_warnings: [],
  warning_count: 0,
  preflight: [],
  final_checklist: [],
  summary: '',
  deliverables: [],
  open_questions: [],
  next_actions: [],
  changes: [],
  verification_results: [],
  artifacts: [],
  timing: { started_at: '2000-01-01T00:00:03.000Z', finished_at: null, duration_ms: null },
  error: null,
}), 'utf8');
const eventRecoveredStatus = await rpc({ jsonrpc: '2.0', id: 52321, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: eventRecoveredRunId } } }, state);
assert.equal(eventRecoveredStatus.result?.structuredContent.status, 'completed_with_errors');
assert.equal(eventRecoveredStatus.result?.structuredContent.summary, 'Recovered recommendation from events.');
assert.match(eventRecoveredStatus.result?.structuredContent.error, /worker_run_recovered_from_events/);
assert.equal(eventRecoveredStatus.result?.structuredContent.timing.finished_at, '2000-01-01T00:00:05.000Z');
assert.equal(JSON.parse(readFileSync(join(eventRecoveredRunDir, 'result.json'), 'utf8')).status, 'completed_with_errors');
const recoveredResources = await rpc({ jsonrpc: '2.0', id: 52322, method: 'resources/list', params: {} }, state);
const recoveredLastMessageResource = recoveredResources.result?.resources.find((resource: any) => resource.name === `${eventRecoveredRunId}/last_message.json`);
assert.ok(recoveredLastMessageResource);
const recoveredLastMessage = await rpc({ jsonrpc: '2.0', id: 52323, method: 'resources/read', params: { uri: recoveredLastMessageResource.uri } }, state);
assert.match(recoveredLastMessage.result?.contents[0].text, /Recovered recommendation from events/);
const recentRuns = await rpc({ jsonrpc: '2.0', id: 524, method: 'tools/call', params: { name: 'worker_runs_list', arguments: { limit: 10 } } }, state);
const recentAsyncRun = recentRuns.result?.structuredContent.runs.find((run: any) => run.run_id === asyncRun.result?.structuredContent.run_id);
assert.ok(recentAsyncRun);
assert.match(String(recentAsyncRun.progress_preview), /thread-created/);
const verboseRuns = await rpc({ jsonrpc: '2.0', id: 525, method: 'tools/call', params: { name: 'worker_runs_list', arguments: { limit: 20, verbose: true } } }, state);
const verboseAsyncRun = verboseRuns.result?.structuredContent.runs.find((run: any) => run.run_id === asyncRun.result?.structuredContent.run_id);
assert.equal(verboseAsyncRun.summary, 'worker ok');
assert.equal(typeof verboseAsyncRun.run_dir, 'string');
assert.equal(verboseAsyncRun.progress.readable, true);
const summaryOnlyWait = await rpc({ jsonrpc: '2.0', id: 526, method: 'tools/call', params: { name: 'worker_run_wait', arguments: { run_id: asyncRun.result?.structuredContent.run_id, timeout_ms: 0, summary_only: true } } }, state);
assert.deepEqual(Object.keys(summaryOnlyWait.result?.structuredContent.run).sort(), ['error_preview', 'progress', 'run_id', 'status', 'summary']);
assert.match(String(summaryOnlyWait.result?.structuredContent.run.progress.latest_event_preview), /thread-created/);
const verboseWait = await rpc({ jsonrpc: '2.0', id: 527, method: 'tools/call', params: { name: 'worker_run_wait', arguments: { run_id: asyncRun.result?.structuredContent.run_id, timeout_ms: 0, verbose: true } } }, state);
assert.equal(verboseWait.result?.structuredContent.full_run.summary, 'worker ok');

const prefixedState = createServerState({ allowedRoot: root, runRoot: join(root, 'prefixed'), defaultRuntime: 'codex', codexCommand: process.execPath, codexCommandArgs: [fakeCodexScript], providerRegistryPath: defaultProviderRegistryPath });
const prefixedRun = await rpc({
  jsonrpc: '2.0',
  id: 53,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('run with command args') },
}, prefixedState);
assert.equal(prefixedRun.result?.structuredContent.status, 'completed');
const prefixedInvocation = JSON.parse(readFileSync(join(prefixedRun.result?.structuredContent.run_dir, 'worker_invocation.json'), 'utf8'));
assert.equal(prefixedInvocation.command, process.execPath);
assert.equal(prefixedInvocation.argv[0], fakeCodexScript);
assert.equal(prefixedInvocation.argv[1], 'exec');

const readAuthority = await rpc({
  jsonrpc: '2.0',
  id: 54,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('read authority') },
}, state);
assert.equal(readAuthority.result?.structuredContent.resolved_worker_config.authority, 'read');
assert.equal(readAuthority.result?.structuredContent.resolved_worker_config.cognition, 'low');
assert.equal(readAuthority.result?.structuredContent.resolved_worker_config.sandbox, 'read-only');
assert.equal(readAuthority.result?.structuredContent.resolved_worker_config.provider, 'codex-subscription');
assert.equal(readAuthority.result?.structuredContent.resolved_worker_config.model, codexCatalogDefaults.low.model);
assert.equal(readAuthority.result?.structuredContent.resolved_worker_config.reasoning_effort, codexCatalogDefaults.low.reasoningEffort);
const mediumCognition = await rpc({
  jsonrpc: '2.0',
  id: 541,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('medium cognition', {}, 'read', 'medium') },
}, state);
assert.equal(mediumCognition.result?.structuredContent.resolved_worker_config.authority, 'read');
assert.equal(mediumCognition.result?.structuredContent.resolved_worker_config.cognition, 'medium');
assert.equal(mediumCognition.result?.structuredContent.resolved_worker_config.sandbox, 'read-only');
assert.equal(mediumCognition.result?.structuredContent.resolved_worker_config.provider, 'codex-subscription');
assert.equal(mediumCognition.result?.structuredContent.resolved_worker_config.model, codexCatalogDefaults.medium.model);
assert.equal(mediumCognition.result?.structuredContent.resolved_worker_config.reasoning_effort, codexCatalogDefaults.medium.reasoningEffort);
const writeAuthority = await rpc({
  jsonrpc: '2.0',
  id: 55,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('write authority', {}, 'write') },
}, state);
assert.equal(writeAuthority.result?.structuredContent.resolved_worker_config.authority, 'write');
assert.equal(writeAuthority.result?.structuredContent.resolved_worker_config.sandbox, 'workspace-write');
const commandAuthority = await rpc({
  jsonrpc: '2.0',
  id: 56,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('command authority', {}, 'command') },
}, state);
assert.equal(commandAuthority.result?.structuredContent.resolved_worker_config.authority, 'command');
assert.equal(commandAuthority.result?.structuredContent.resolved_worker_config.sandbox, 'workspace-write');
assert.equal(commandAuthority.result?.structuredContent.resolved_worker_config.provider, 'codex-subscription');
assert.equal(commandAuthority.result?.structuredContent.resolved_worker_config.model, codexCatalogDefaults.low.model);
assert.equal(commandAuthority.result?.structuredContent.resolved_worker_config.reasoning_effort, codexCatalogDefaults.low.reasoningEffort);

const editRun = await rpc({
  jsonrpc: '2.0',
  id: 561,
  method: 'tools/call',
  params: { name: 'worker_edit', arguments: { cwd: root, instruction: 'edit shortcut', wait_for_completion: true, overrides: { model: 'gpt-edit-test' } } },
}, state);
assert.equal(editRun.result?.structuredContent.status, 'completed');
assert.equal(editRun.result?.structuredContent.resolved_worker_config.authority, 'write');
assert.equal(editRun.result?.structuredContent.resolved_worker_config.cognition, 'low');
assert.equal(editRun.result?.structuredContent.resolved_worker_config.sandbox, 'workspace-write');
assert.equal(editRun.result?.structuredContent.resolved_worker_config.provider, 'codex-subscription');
assert.equal(editRun.result?.structuredContent.resolved_worker_config.model, 'gpt-edit-test');
assert.equal(editRun.result?.structuredContent.resolved_worker_config.reasoning_effort, codexCatalogDefaults.low.reasoningEffort);
assert.equal(editRun.result?.structuredContent.requested_mode, 'implement');
assert.equal(editRun.result?.structuredContent.edits_performed, true);
assert.equal(editRun.result?.structuredContent.target_state_changed, true);
assert.equal(editRun.result?.structuredContent.changes[0].status, 'modified');
assert.equal(editRun.result?.structuredContent.verification_results[0].status, 'passed');
assert.deepEqual(editRun.result?.structuredContent.final_checklist, ['list files changed', 'list tests or checks run', 'include git/worktree status if available', 'list remaining blockers']);
const editRequest = JSON.parse(readFileSync(join(editRun.result?.structuredContent.run_dir, 'request.json'), 'utf8'));
assert.equal(editRequest.intent.instruction, 'edit shortcut');
assert.equal(editRequest.intent.mode, 'implement');
assert.equal(editRequest.constraints.authority, 'write');
assert.equal(editRequest.constraints.cognition, 'low');
assert.equal(editRequest.constraints.resumable, undefined);
assert.equal(editRequest.constraints.overrides.model, 'gpt-edit-test');
assert.equal(editRequest.constraints.overrides.reasoning_effort, codexCatalogDefaults.low.reasoningEffort);

const defaultEditRun = await rpc({
  jsonrpc: '2.0',
  id: 5611,
  method: 'tools/call',
  params: { name: 'worker_edit', arguments: { cwd: root, instruction: 'default edit shortcut', wait_for_completion: true } },
}, state);
assert.equal(defaultEditRun.result?.structuredContent.resolved_worker_config.provider, 'codex-subscription');
assert.equal(defaultEditRun.result?.structuredContent.resolved_worker_config.model, codexCatalogDefaults.low.model);
assert.equal(defaultEditRun.result?.structuredContent.resolved_worker_config.reasoning_effort, codexCatalogDefaults.low.reasoningEffort);

const customLowCognitionState = createServerState({
  allowedRoot: root,
  runRoot: join(root, 'low-cognition-defaults'),
  defaultRuntime: 'codex',
  codexCommand: process.execPath,
  codexCommandArgs: [fakeCodexScript],
  providerCognitionDefaults: {
    'codex-subscription': {
      low: { model: 'gpt-low-default', reasoning_effort: 'minimal' },
    },
  },
});
const customLowCognition = await rpc({
  jsonrpc: '2.0',
  id: 562,
  method: 'tools/call',
  params: { name: 'worker_edit', arguments: { cwd: root, instruction: 'custom low cognition defaults', wait_for_completion: true } },
}, customLowCognitionState);
assert.equal(customLowCognition.result?.structuredContent.resolved_worker_config.model, 'gpt-low-default');
assert.equal(customLowCognition.result?.structuredContent.resolved_worker_config.reasoning_effort, 'minimal');

const callerEditOverride = await rpc({
  jsonrpc: '2.0',
  id: 563,
  method: 'tools/call',
  params: { name: 'worker_edit', arguments: { cwd: root, instruction: 'caller edit override', wait_for_completion: true, overrides: { reasoning_effort: 'high' } } },
}, customLowCognitionState);
assert.equal(callerEditOverride.result?.structuredContent.resolved_worker_config.model, 'gpt-low-default');
assert.equal(callerEditOverride.result?.structuredContent.resolved_worker_config.reasoning_effort, 'high');

const explicitTuplePreview = await rpc({
  jsonrpc: '2.0',
  id: 5631,
  method: 'tools/call',
  params: {
    name: 'worker_config_resolve',
    arguments: {
      intent: { instruction: 'explicit cognition tuple' },
      constraints: {
        cwd: root,
        authority: 'read',
        cognition: 'high',
        overrides: { model: 'gpt-explicit', reasoning_effort: 'max' },
      },
    },
  },
}, state);
assert.equal(explicitTuplePreview.result?.structuredContent.resolved_worker_config.provider, 'codex-subscription');
assert.equal(explicitTuplePreview.result?.structuredContent.resolved_worker_config.model, 'gpt-explicit');
assert.equal(explicitTuplePreview.result?.structuredContent.resolved_worker_config.reasoning_effort, 'max');
assert.equal(explicitTuplePreview.result?.structuredContent.config_resolution.model_source, 'request_override');
assert.equal(explicitTuplePreview.result?.structuredContent.config_resolution.reasoning_effort_source, 'request_override');
assert.match(explicitTuplePreview.result?.structuredContent.invocation.argv.join(' '), /model="gpt-explicit"/);
assert.match(explicitTuplePreview.result?.structuredContent.invocation.argv.join(' '), /model_reasoning_effort="max"/);

const unresolvedCognitionState = createServerState({
  allowedRoot: root,
  runRoot: join(root, 'unresolved-cognition-defaults'),
  defaultRuntime: 'codex',
  codexCommand: process.execPath,
  codexCommandArgs: [fakeCodexScript],
  providerRegistryPath: defaultProviderRegistryPath,
});
unresolvedCognitionState.policy.providerCognitionDefaults['codex-subscription'].low = { model: null, reasoningEffort: null };
const unresolvedCognition = await rpc({
  jsonrpc: '2.0',
  id: 5632,
  method: 'tools/call',
  params: {
    name: 'worker_run',
    arguments: { intent: { instruction: 'unresolved cognition tuple' }, constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true } },
  },
}, unresolvedCognitionState);
assert.equal(unresolvedCognition.error?.data.code, 'worker_cognition_defaults_unresolved');
assert.equal(unresolvedCognition.error?.data.details.field, 'model');
assert.deepEqual(unresolvedCognition.error?.data.details.missing_fields, ['model', 'reasoning_effort']);
assert.equal(unresolvedCognitionState.activeRunCount, 0);

const resumableEdit = await rpc({
  jsonrpc: '2.0',
  id: 564,
  method: 'tools/call',
  params: { name: 'worker_edit', arguments: { cwd: root, instruction: 'resumable edit inheritance', resumable: true, wait_for_completion: true } },
}, state);
assert.ok(resumableEdit.result, JSON.stringify(resumableEdit.error));
assert.equal(resumableEdit.result?.structuredContent.worker_session_id, 'thread-created');
assert.equal(resumableEdit.result?.structuredContent.resolved_worker_config.provider, 'codex-subscription');
assert.equal(resumableEdit.result?.structuredContent.resolved_worker_config.model, codexCatalogDefaults.low.model);
assert.equal(resumableEdit.result?.structuredContent.resolved_worker_config.reasoning_effort, codexCatalogDefaults.low.reasoningEffort);
assert.equal(resumableEdit.result?.structuredContent.resolved_worker_config.ephemeral, false);
const editSessionRecord = JSON.parse(readFileSync(join(runRoot, 'sessions', `${encodeURIComponent('thread-created')}.json`), 'utf8'));
assert.equal(editSessionRecord.origin_tool, 'worker_edit');
assert.equal(editSessionRecord.resolved_worker_config.authority, 'write');
assert.equal(editSessionRecord.resolved_worker_config.cognition, 'low');
assert.equal(editSessionRecord.resolved_worker_config.provider, 'codex-subscription');
assert.equal(editSessionRecord.resolved_worker_config.model, codexCatalogDefaults.low.model);
assert.equal(editSessionRecord.resolved_worker_config.reasoning_effort, codexCatalogDefaults.low.reasoningEffort);
const restartedState = createServerState({ allowedRoot: root, runRoot, auditLogDir, defaultRuntime: 'codex', codexCommand: process.execPath, codexCommandArgs: [fakeCodexScript] }, { PATH: process.env.PATH });
const resumedEdit = await rpc({
  jsonrpc: '2.0',
  id: 565,
  method: 'tools/call',
  params: { name: 'worker_resume', arguments: { worker_session_id: 'thread-created', constraints: { cwd: root, wait_for_completion: true } } },
}, restartedState);
assert.ok(resumedEdit.result, JSON.stringify(resumedEdit.error));
assert.equal(resumedEdit.result?.structuredContent.status, 'completed');
assert.equal(resumedEdit.result?.structuredContent.resolved_worker_config.authority, 'write');
assert.equal(resumedEdit.result?.structuredContent.resolved_worker_config.cognition, 'low');
assert.equal(resumedEdit.result?.structuredContent.resolved_worker_config.provider, 'codex-subscription');
assert.equal(resumedEdit.result?.structuredContent.resolved_worker_config.model, codexCatalogDefaults.low.model);
assert.equal(resumedEdit.result?.structuredContent.resolved_worker_config.reasoning_effort, codexCatalogDefaults.low.reasoningEffort);
assert.equal(resumedEdit.result?.structuredContent.resolved_worker_config.argv.includes('--ephemeral'), false);

const resumableRun = await rpc({
  jsonrpc: '2.0',
  id: 57,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'resumable run' }, constraints: { cwd: root, authority: 'read', cognition: 'low', resumable: true, wait_for_completion: true } } },
}, state);
assert.equal(resumableRun.result?.structuredContent.resolved_worker_config.resumable, true);
assert.equal(resumableRun.result?.structuredContent.resolved_worker_config.ephemeral, false);
const resumableInvocation = JSON.parse(readFileSync(join(resumableRun.result?.structuredContent.run_dir, 'worker_invocation.json'), 'utf8'));
assert.equal(resumableInvocation.argv.includes('--ephemeral'), false);
assert.match(readFileSync(join(completedRunDir, 'worker_prompt.txt'), 'utf8'), /Do not call any worker_\* MCP tools\./);
assert.match(readFileSync(join(completedRunDir, 'worker_prompt.txt'), 'utf8'), /Prefer available MCP filesystem, git, and structured-command tools/);
assert.match(readFileSync(join(completedRunDir, 'worker_prompt.txt'), 'utf8'), /Do not use direct shell commands for file discovery or file reads/);
assert.match(readFileSync(join(completedRunDir, 'worker_prompt.txt'), 'utf8'), /Requested mode\naudit_only/);
assert.match(readFileSync(join(completedRunDir, 'worker_prompt.txt'), 'utf8'), /Audit only: inspect and report/);
assert.doesNotMatch(readFileSync(join(completedRunDir, 'worker_prompt.txt'), 'utf8'), /NARS worker completion guard/);
assert.match(readFileSync(join(completedRunDir, 'events.jsonl'), 'utf8'), /thread-created/);
assert.equal(readdirSync(runRoot).some((name: any) => /^run-\d{8}T\d{6}Z-[0-9a-f]{8}$/.test(name)), true);
assert.equal(existsSync(join(auditLogDir, 'worker-delegation-mcp.jsonl')), true);

const argv = buildCodexArgv({
  cwd: 'C:/repo',
  sandbox: 'read-only',
  schemaPath: 'schema.json',
  lastMessagePath: 'last.json',
  workerSessionId: 'thread-1',
  ephemeral: true,
  skipGitRepoCheck: true,
  config: { model: 'gpt-test', model_reasoning_effort: 'medium' },
});
assert.deepEqual(argv.slice(0, 11), ['exec', '--ephemeral', '-C', 'C:/repo', '--sandbox', 'read-only', '--json', '--output-schema', 'schema.json', '-o', 'last.json']);
assert.deepEqual(argv.slice(11, 13), ['resume', 'thread-1']);
assert.equal(argv.includes('--skip-git-repo-check'), true);
assert.equal(argv.at(-1), '-');

const resume = await rpc({
  jsonrpc: '2.0',
  id: 6,
  method: 'tools/call',
  params: { name: 'worker_resume', arguments: { worker_session_id: 'thread-existing', constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true } } },
}, state);
assert.equal(resume.result?.structuredContent.status, 'completed');
assert.equal(resume.result?.structuredContent.worker_session_id, 'thread-resumed');
const resumeConfig = JSON.parse(readFileSync(join(resume.result?.structuredContent.run_dir, 'resolved_worker_config.json'), 'utf8'));
assert.equal(resumeConfig.resumable, true);
assert.equal(resumeConfig.ephemeral, false);
assert.equal(resumeConfig.argv.includes('resume'), true);
assert.equal(resumeConfig.argv.includes('thread-existing'), true);

const spawnFailureState = createServerState({ allowedRoot: root, runRoot: join(root, 'spawn-failure'), defaultRuntime: 'codex', codexCommand: join(root, 'missing-codex.exe'), providerRegistryPath: defaultProviderRegistryPath });
const spawnFailure = await rpc({
  jsonrpc: '2.0',
  id: 61,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('spawn failure') },
}, spawnFailureState);
assert.equal(spawnFailure.error?.data.code, 'worker_runtime_unavailable');
assert.match(spawnFailure.error?.data.details.reason, /command not found/);
assert.equal(typeof spawnFailure.error?.data.details.remediation, 'string');

const unavailableRoot = mkdtempSync(join(testTempRoot(), 'worker-delegation-unavailable-'));
const unavailableState = createServerState({ allowedRoot: unavailableRoot, runRoot: join(unavailableRoot, 'runs'), defaultRuntime: 'codex', codexCommand: 'definitely-not-a-real-codex-binary', providerRegistryPath: defaultProviderRegistryPath });
const unavailableRun = await rpc({
  jsonrpc: '2.0',
  id: 611,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'unavailable runtime' }, constraints: { cwd: unavailableRoot, wait_for_completion: true } } },
}, unavailableState);
assert.equal(unavailableRun.error?.data.code, 'worker_runtime_unavailable');

const deepseekRoot = mkdtempSync(join(testTempRoot(), 'worker-delegation-deepseek-spawn-'));
const deepseekState = createServerState({ allowedRoot: deepseekRoot, runRoot: join(deepseekRoot, 'runs'), defaultRuntime: 'codex', codexCommand: process.execPath }, { PATH: process.env.PATH, NARADA_PROVIDER_SECRET_STORE: 'disabled' });
const deepseekUnavailable = await rpc({
  jsonrpc: '2.0',
  id: 612,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'deepseek unavailable' }, constraints: { cwd: deepseekRoot, wait_for_completion: true, overrides: { runtime: 'deepseek-api' } } } },
}, deepseekState);
assert.equal(deepseekUnavailable.error?.data.code, 'worker_runtime_migrated_to_nars_provider');
assert.match(String(deepseekUnavailable.error?.data.details.remediation), /narada-agent-runtime-server/);

const eventRoot = mkdtempSync(join(testTempRoot(), 'worker-delegation-bad-event-'));
const badEventScript = join(eventRoot, 'exec.cjs');
writeFileSync(badEventScript, `
const fs = require('fs');
const args = process.argv.slice(2);
const lastMessagePath = args[args.indexOf('-o') + 1];
process.stdin.resume();
process.stdin.on('end', () => {
  process.stdout.write('not json\\n');
  fs.writeFileSync(lastMessagePath, JSON.stringify({ summary: 'ok', deliverables: [], open_questions: [], next_actions: [], edits_performed: false, target_state_changed: false, changes: [], verification: [] }));
});
`, 'utf8');
const badEventState = createServerState({ allowedRoot: eventRoot, runRoot: join(eventRoot, 'runs'), defaultRuntime: 'codex', codexCommand: process.execPath, codexCommandArgs: [badEventScript], providerRegistryPath: defaultProviderRegistryPath });
const badEvent = await rpc({
  jsonrpc: '2.0',
  id: 62,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'bad event' }, constraints: { cwd: eventRoot, wait_for_completion: true } } },
}, badEventState);
assert.equal(badEvent.result?.structuredContent.status, 'completed_with_errors');
assert.equal(badEvent.result?.structuredContent.summary, 'ok');
assert.match(badEvent.result?.structuredContent.error, /invalid json event/);
assert.equal(badEvent.result?.structuredContent.warning_count, 0);

const completedWithToolErrorState = createServerState({ allowedRoot: root, runRoot: join(root, 'completed-with-tool-error'), defaultRuntime: 'codex', codexCommand: process.execPath, codexCommandArgs: [fakeCodexErrorScript], providerRegistryPath: defaultProviderRegistryPath });
const completedWithToolError = await rpc({
  jsonrpc: '2.0',
  id: 621,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('tool error with output') },
}, completedWithToolErrorState);
assert.equal(completedWithToolError.result?.structuredContent.status, 'completed');
assert.equal(completedWithToolError.result?.structuredContent.summary, 'usable output despite tool error');
assert.equal(completedWithToolError.result?.structuredContent.error, null);
assert.equal(completedWithToolError.result?.structuredContent.warning_count, 1);
assert.deepEqual(completedWithToolError.result?.structuredContent.runtime_warnings, ['simulated mcp tool error']);
const filteredCompletedWithErrors = await rpc({ jsonrpc: '2.0', id: 622, method: 'tools/call', params: { name: 'worker_runs_list', arguments: { include_completed: false } } }, completedWithToolErrorState);
assert.equal(filteredCompletedWithErrors.result?.structuredContent.runs.some((run: any) => run.status === 'completed'), false);

const preflightRun = await rpc({
  jsonrpc: '2.0',
  id: 623,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'preflight paths', mode: 'plan_only' }, constraints: { cwd: root, authority: 'read', wait_for_completion: true, preflight_paths: [{ path: root, access: 'read', label: 'old authority' }], required_mcp_tools: ['local-filesystem-read.fs_glob_search', 'structured-command.structured_command_execute'], overrides: { runtime: 'narada-agent-runtime-server' } } } },
}, state);
assert.equal(preflightRun.result?.structuredContent.requested_mode, 'plan_only');
assert.equal(preflightRun.result?.structuredContent.edits_performed, false);
assert.equal(preflightRun.result?.structuredContent.preflight.some((check: any) => check.message.includes('old authority') && check.status === 'ok'), true);
assert.equal(preflightRun.result?.structuredContent.preflight.some((check: any) => check.name === 'effective_authority' && check.status === 'warning' && check.message.includes('raw MCP surfaces may advertise mutation-capable tools')), true);
assert.equal(preflightRun.result?.structuredContent.output_contract.effective_authority, 'read');
assert.match(preflightRun.result?.structuredContent.output_contract.tool_capability_note, /mutation tools/);
assert.match(preflightRun.result?.structuredContent.output_contract.focused_readback.behavior, /ordinary target source files directly/);
assert.match(readFileSync(join(preflightRun.result?.structuredContent.run_dir, 'worker_prompt.txt'), 'utf8'), /effective_authority=read/);
assert.match(readFileSync(join(preflightRun.result?.structuredContent.run_dir, 'worker_prompt.txt'), 'utf8'), /Do not ask the delegating caller to provide output_refs/);
assert.equal(preflightRun.result?.structuredContent.preflight.some((check: any) => check.name === 'required_mcp_tools' && check.status === 'warning' && check.message.includes('runtime_inventory_not_preflighted') && check.message.includes('structured-command.structured_command_execute')), true);

const recursiveRequiredToolPreflight = await rpc({
  jsonrpc: '2.0',
  id: 62305,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'repair worker delegation recursively', mode: 'plan_only' }, constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true, required_mcp_tools: ['worker-delegation.worker_run_status'] } } },
}, state);
assert.equal(recursiveRequiredToolPreflight.error?.data.code, 'worker_preflight_blocked');
assert.equal(recursiveRequiredToolPreflight.error?.data.details.blocked_preflight.some((check: any) => check.name === 'required_mcp_tools_self_deadlock' && check.message.includes('reroute')), true);

const readOnlyCreatePreflight = await rpc({
  jsonrpc: '2.0',
  id: 6231,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'read authority cannot create paths', mode: 'plan_only' }, constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true, preflight_paths: [{ path: join(root, 'new-repo'), access: 'create', label: 'new repo' }] } } },
}, state);
assert.equal(readOnlyCreatePreflight.error?.data.code, 'worker_preflight_blocked');
assert.equal(readOnlyCreatePreflight.error?.data.details.blocked_preflight.some((check: any) => check.name === 'read_authority_mutation_boundary' && check.message.includes('create')), true);

const blockedPreflight = await rpc({
  jsonrpc: '2.0',
  id: 624,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'blocked preflight', mode: 'implement' }, constraints: { cwd: root, authority: 'read', cognition: 'low', wait_for_completion: true, preflight_paths: [{ path: join(root, 'missing-input'), access: 'read', label: 'missing input' }] } } },
}, state);
assert.equal(blockedPreflight.error?.data.code, 'worker_preflight_blocked');
assert.equal(blockedPreflight.error?.data.details.requested_mode, 'implement');
assert.equal(blockedPreflight.error?.data.details.blocked_preflight.some((check: any) => check.message.includes('missing input')), true);
assert.equal(blockedPreflight.error?.data.details.blocked_preflight.some((check: any) => check.name === 'mode_authority_alignment'), true);

const runtimeErrorRoot = mkdtempSync(join(testTempRoot(), 'worker-delegation-runtime-error-'));
const runtimeErrorScript = join(runtimeErrorRoot, 'exec.cjs');
writeFileSync(runtimeErrorScript, `
process.stdin.resume();
process.stdin.on('end', () => {
  process.stdout.write(JSON.stringify({ type: 'thread.started', thread_id: 'thread-runtime-error' }) + '\\n');
  process.stdout.write(JSON.stringify({ type: 'error', error: { type: 'invalid_request_error', message: 'model not available for account' } }) + '\\n');
  process.exit(1);
});
`, 'utf8');
const runtimeErrorState = createServerState({ allowedRoot: runtimeErrorRoot, runRoot: join(runtimeErrorRoot, 'runs'), defaultRuntime: 'codex', codexCommand: process.execPath, codexCommandArgs: [runtimeErrorScript], providerRegistryPath: defaultProviderRegistryPath });
const runtimeError = await rpc({
  jsonrpc: '2.0',
  id: 63,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'runtime error' }, constraints: { cwd: runtimeErrorRoot, wait_for_completion: true } } },
}, runtimeErrorState);
assert.equal(runtimeError.error?.data.code, 'worker_runtime_failed');
assert.equal(runtimeError.error?.data.details.error, 'model not available for account');

const persistentReconnectState = createServerState({
  allowedRoot: root,
  runRoot: join(root, 'persistent-reconnect'),
  maxRunMs: 2000,
  defaultRuntime: 'codex',
  codexCommand: process.execPath,
  codexCommandArgs: [fakeCodexPersistentReconnectScript],
  providerRegistryPath: defaultProviderRegistryPath,
});
const persistentReconnectStartedAt = Date.now();
const persistentReconnect = await rpc({
  jsonrpc: '2.0',
  id: 6321,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'persistent provider reconnect failure' }, constraints: { cwd: root, wait_for_completion: true } } },
}, persistentReconnectState);
assert.equal(persistentReconnect.error?.data.code, 'worker_runtime_failed');
assert.match(String(persistentReconnect.error?.data.details.error), /provider reconnect failure/);
const persistentReconnectRunId = String(persistentReconnect.error?.data.details.run_id);
const persistentReconnectStatus = await rpc({
  jsonrpc: '2.0',
  id: 6322,
  method: 'tools/call',
  params: { name: 'worker_run_status', arguments: { run_id: persistentReconnectRunId } },
}, persistentReconnectState);
assert.equal(persistentReconnectStatus.result?.structuredContent.status, 'failed');
assert.ok(Number(persistentReconnectStatus.result?.structuredContent.timing.duration_ms) < 2000);
assert.equal(persistentReconnectStatus.result?.structuredContent.error_classification, 'provider_network');
assert.match(String(persistentReconnectStatus.result?.structuredContent.runtime_diagnostics.provider_error), /os error 10013/);
assert.ok(Date.now() - persistentReconnectStartedAt < 2000);

const transientReconnectState = createServerState({
  allowedRoot: root,
  runRoot: join(root, 'transient-reconnect'),
  maxRunMs: 2000,
  defaultRuntime: 'codex',
  codexCommand: process.execPath,
  codexCommandArgs: [fakeCodexTransientReconnectScript],
  providerRegistryPath: defaultProviderRegistryPath,
});
const transientReconnect = await rpc({
  jsonrpc: '2.0',
  id: 6323,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'transient provider reconnect recovery' }, constraints: { cwd: root, wait_for_completion: true } } },
}, transientReconnectState);
assert.equal(transientReconnect.result?.structuredContent.status, 'completed');
assert.equal(transientReconnect.result?.structuredContent.summary, 'transient provider recovered');

const prestartFailureState = createServerState({ allowedRoot: root, runRoot: join(root, 'prestart-failure'), defaultRuntime: 'codex', codexCommand: process.execPath, codexCommandArgs: [fakeCodexPrestartFailureScript], providerRegistryPath: defaultProviderRegistryPath });
const prestartFailure = await rpc({
  jsonrpc: '2.0',
  id: 631,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: { intent: { instruction: 'prestart failure' }, constraints: { cwd: root, wait_for_completion: true } } },
}, prestartFailureState);
assert.equal(prestartFailure.error?.data.code, 'worker_runtime_failed');
const prestartRunId = readdirSync(join(root, 'prestart-failure')).find((entry: any) => entry.startsWith('run-'));
assert.ok(prestartRunId);
const prestartStatus = await rpc({ jsonrpc: '2.0', id: 632, method: 'tools/call', params: { name: 'worker_run_status', arguments: { run_id: prestartRunId } } }, prestartFailureState);
assert.equal(prestartStatus.result?.structuredContent.error_classification, 'codex_untrusted_directory');
assert.match(prestartStatus.result?.structuredContent.diagnostic_tail, /Not inside a trusted directory/);
const prestartList = await rpc({ jsonrpc: '2.0', id: 633, method: 'tools/call', params: { name: 'worker_runs_list', arguments: {} } }, prestartFailureState);
assert.match(prestartList.result?.structuredContent.runs[0].error_preview, /Not inside a trusted directory/);
assert.equal(prestartList.result?.structuredContent.runs[0].error_classification, 'codex_untrusted_directory');

const materializedState = createServerState({ allowedRoot: root, runRoot: join(root, 'small-output'), maxOutputBytes: 1000, defaultRuntime: 'codex', codexCommand: process.execPath, codexCommandArgs: [fakeCodexScript], providerRegistryPath: defaultProviderRegistryPath });
const materialized = await rawRpc({ jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name: 'worker_policy_inspect', arguments: {} } }, materializedState);
assert.equal(materialized.result?.structuredContent.schema, 'narada.producer_output_page.v1');
assert.equal(materialized.result?.structuredContent.result_materialized, true);
assert.equal(materialized.result?.structuredContent.reader_tool, 'worker_output_show');
assert.equal(materialized.result?.structuredContent.tool_name, 'worker_policy_inspect');
assert.equal(String(materialized.result?.content[0].text).includes('provider_cognition_defaults'), false);
assert.deepEqual(materialized.result?.content.map((item: any) => item.type), ['text']);
const materializedResources = await rpc({ jsonrpc: '2.0', id: 71, method: 'resources/list', params: {} }, materializedState);
assert.equal(materializedResources.result?.resources.some((resource: any) => String(resource.uri).startsWith('worker-output:')), false);
const executorRequestResource = (await rpc({ jsonrpc: '2.0', id: 72, method: 'resources/list', params: {} }, state)).result?.resources.find((resource: any) => resource.name === `${allowedConfigRun.result?.structuredContent.run_id}/executor_request.json`);
assert.ok(executorRequestResource);
const shownArtifact = await rpc({ jsonrpc: '2.0', id: 801, method: 'resources/read', params: { uri: executorRequestResource.uri } }, state);
assert.match(shownArtifact.result?.contents[0].text, /narada.worker.executor_request.v1/);

const cancelled = new AbortController();
cancelled.abort();
const cancelledRun = await rpcWithContext({
  jsonrpc: '2.0',
  id: 82,
  method: 'tools/call',
  params: { name: 'worker_run', arguments: runArgs('cancel before runtime starts') },
}, state, { abortSignal: cancelled.signal });
assert.equal(cancelledRun.error?.data.code, 'worker_runtime_cancelled');

const unknown = await rpc({ jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name: 'worker_autopilot', arguments: {} } }, state);
assert.equal(unknown.error?.data.code, 'worker_unknown_tool');

function hasCode(code: string): (error: unknown) => boolean {
  return (error: any) => error?.codeName === code;
}

function runArgs(instruction: string, constraints: Record<string, unknown> = {}, authority : any= 'read', cognition : any= 'low'): Record<string, unknown> {
  const canonicalRuntime = constraints.runtime === 'narada-agent-runtime-server';
  return {
    intent: { instruction },
    constraints: {
      cwd: root,
      authority,
      ...(canonicalRuntime ? {} : { cognition }),
      wait_for_completion: true,
      overrides: constraints,
    },
  };
}

function assertStrictStructuredOutputSchema(schema: any, path: string): void {
  if (!schema || typeof schema !== 'object') return;
  if (schema.properties && typeof schema.properties === 'object' && !Array.isArray(schema.properties)) {
    const propertyNames = Object.keys(schema.properties);
    const required = Array.isArray(schema.required) ? schema.required : [];
    assert.deepEqual([...required].sort(), propertyNames.sort(), `${path}.required must include every fixed property for Codex structured output`);
    assert.equal(schema.additionalProperties, false, `${path}.additionalProperties must be false for Codex structured output`);
    for (const propertyName of propertyNames) {
      assertStrictStructuredOutputSchema(schema.properties[propertyName], `${path}.properties.${propertyName}`);
    }
  }
  if (schema.items) assertStrictStructuredOutputSchema(schema.items, `${path}.items`);
}

function testTempRoot(): string {
  const root = join(process.cwd(), '.tmp-tests');
  mkdirSync(root, { recursive: true });
  return root;
}

async function materializeOutputRefResponse(response: RpcResponse, state: ReturnType<typeof createServerState>): Promise<RpcResponse> {
  const envelope = response.result?.structuredContent;
  if (envelope?.schema !== 'narada.producer_output_page.v1' || typeof envelope.output_ref !== 'string') return response;
  let offset = 0;
  let outputText = '';
  while (true) {
    const pageResponse = await rawRpc({
      jsonrpc: '2.0',
      id: `output-readback-${offset}`,
      method: 'tools/call',
      params: { name: 'worker_output_show', arguments: { ref: envelope.output_ref, offset, limit: 20000 } },
    }, state);
    const page = pageResponse.result?.structuredContent;
    if (page?.schema !== 'narada.mcp_output_page.v1' || typeof page.output_text !== 'string') {
      throw new Error(`worker_config_resolve_output_readback_invalid:${JSON.stringify(pageResponse)}`);
    }
    outputText += page.output_text;
    if (page.next_offset === null || page.next_offset === undefined) break;
    offset = Number(page.next_offset);
  }
  const structuredContent = JSON.parse(outputText) as Record<string, any>;
  return {
    ...response,
    result: {
      ...response.result,
      content: [{ type: 'text', text: outputText }],
      structuredContent,
    },
  };
}
