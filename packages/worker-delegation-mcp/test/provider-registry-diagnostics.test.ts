import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServerState, handleRequest } from '../src/main.js';
import { callWorkerTool } from '../src/worker-tools.js';

type RpcResponse = {
  result?: Record<string, any>;
  error?: Record<string, any>;
};

const root = mkdtempSync(join(tmpdir(), 'worker-provider-registry-diagnostics-'));
mkdirSync(join(root, '.narada'), { recursive: true });
const validRegistryPath = join(root, 'provider-registry.json');
writeFileSync(validRegistryPath, JSON.stringify({
  schema: 'narada.carrier.provider_registry.v1',
  default_provider: 'codex-subscription',
  providers: {
    'codex-subscription': {
      base_url: 'codex://local-subscription',
      default_model: 'gpt-5.6-luna',
      available_models: ['gpt-5.6-luna'],
      cognition_defaults: {
        low: { model: 'gpt-5.6-luna', reasoning_effort: 'max' },
        medium: { model: 'gpt-5.6-luna', reasoning_effort: 'max' },
        high: { model: 'gpt-5.6-luna', reasoning_effort: 'max' },
      },
      credential_requirement: { kind: 'local_codex_subscription' },
    },
  },
}), 'utf8');

const rpc = async (name: string, arguments_: Record<string, unknown>, state: ReturnType<typeof createServerState>): Promise<RpcResponse> => handleRequest({
  jsonrpc: '2.0',
  id: name,
  method: 'tools/call',
  params: { name, arguments: arguments_ },
}, state) as Promise<RpcResponse>;

const availableState = createServerState({
  siteRoot: root,
  allowedRoot: root,
  runRoot: join(root, 'available-runs'),
  defaultRuntime: 'codex',
  codexCommand: process.execPath,
  providerRegistryPath: validRegistryPath,
}, { PATH: process.env.PATH, NARADA_PROVIDER_SECRET_STORE: 'disabled' });
assert.deepEqual(availableState.providerRegistryDiagnostics, {
  status: 'available',
  source: 'legacy_json',
  path: validRegistryPath,
  searchedPaths: [validRegistryPath],
  selection: 'explicit',
  errorCode: null,
  providerCount: 1,
});
const availableInspect = await rpc('worker_cognition_defaults_inspect', {}, availableState);
assert.equal(availableInspect.result?.structuredContent.provider_registry.status, 'available');
assert.equal(availableInspect.result?.structuredContent.provider_registry.provider_count, 1);
assert.equal(availableInspect.result?.structuredContent.effective_cognition_defaults.low.model, 'gpt-5.6-luna');

// A missing effective projection must not erase the complete provider-scoped tuple.
// Codex selects its admitted provider first, then resolves that provider's cognition tier.
(availableState.cognitionDefaults!.effectiveDefaults as Record<string, unknown>).low = null;
const providerScopedResolve = await callWorkerTool('worker_config_resolve', {
  intent: { instruction: 'resolve provider tier with no effective projection' },
  constraints: { cwd: root, authority: 'read', cognition: 'low', overrides: { runtime: 'codex' } },
}, availableState);
assert.equal((providerScopedResolve as any).resolved_worker_config.provider, 'codex-subscription');
assert.equal((providerScopedResolve as any).resolved_worker_config.model, 'gpt-5.6-luna');
assert.equal((providerScopedResolve as any).resolved_worker_config.reasoning_effort, 'max');

const missingRoot = mkdtempSync(join(tmpdir(), 'worker-provider-registry-missing-'));
mkdirSync(join(missingRoot, '.narada'), { recursive: true });
const missingPath = join(missingRoot, 'provider-registry.json');
const missingState = createServerState({
  siteRoot: missingRoot,
  allowedRoot: missingRoot,
  runRoot: join(missingRoot, 'runs'),
  defaultRuntime: 'codex',
  codexCommand: process.execPath,
  providerRegistryPath: missingPath,
}, { PATH: process.env.PATH, NARADA_PROVIDER_SECRET_STORE: 'disabled' });
assert.deepEqual(missingState.providerRegistryDiagnostics, {
  status: 'missing',
  source: 'legacy_json',
  path: null,
  searchedPaths: [missingPath],
  selection: null,
  errorCode: 'not_found',
  providerCount: null,
});
const missingInspect = await rpc('worker_cognition_defaults_inspect', {}, missingState);
assert.equal(missingInspect.result?.structuredContent.provider_registry.status, 'missing');
assert.deepEqual(missingInspect.result?.structuredContent.provider_registry.searched_paths, [missingPath]);
const missingResolve = await rpc('worker_config_resolve', {
  intent: { instruction: 'resolve without a provider registry' },
  constraints: { cwd: missingRoot, cognition: 'low' },
}, missingState);
assert.equal(missingResolve.error?.data.code, 'worker_cognition_defaults_unresolved');
assert.equal(missingResolve.error?.data.details.provider_registry.status, 'missing');
assert.equal(missingResolve.error?.data.details.provider_registry.error_code, 'not_found');

const invalidPath = join(missingRoot, 'invalid-provider-registry.json');
writeFileSync(invalidPath, '{not-json', 'utf8');
const invalidState = createServerState({
  siteRoot: missingRoot,
  allowedRoot: missingRoot,
  runRoot: join(missingRoot, 'invalid-runs'),
  defaultRuntime: 'codex',
  codexCommand: process.execPath,
  providerRegistryPath: invalidPath,
}, { PATH: process.env.PATH, NARADA_PROVIDER_SECRET_STORE: 'disabled' });
assert.equal(invalidState.providerRegistryDiagnostics?.status, 'invalid');
assert.equal(invalidState.providerRegistryDiagnostics?.errorCode, 'invalid_json');

console.log('provider-registry-diagnostics.test.ts: passed');
