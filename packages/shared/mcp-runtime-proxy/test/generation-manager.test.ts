import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import {
  defineSurface,
  type DefinedSurface,
  type McpToolDefinition,
} from '@narada-core/mcp-fabric-contracts';
import {
  GenerationManager,
  JsonLineStdioGenerationAdapter,
  StreamableHttpGenerationAdapter,
  startStableHttpGenerationEndpoint,
  type GenerationAdapter,
} from '../src/generation-manager.js';
import { describeUnknownError } from '../src/error-description.js';

const definitions: McpToolDefinition[] = [
  {
    name: 'generation_fixture_guidance',
    description: 'Show generation fixture guidance.',
    inputSchema: { type: 'object', properties: {}, additionalProperties: false },
    annotations: { readOnlyHint: true },
  },
  {
    name: 'generation_fixture_health',
    description: 'Read generation fixture health.',
    inputSchema: { type: 'object', properties: {}, additionalProperties: false },
    annotations: { readOnlyHint: true },
  },
  {
    name: 'generation_fixture_echo',
    description: 'Echo from one concrete generation.',
    inputSchema: {
      type: 'object',
      properties: {
        value: { type: 'string' },
        delay_ms: { type: 'integer', minimum: 0, maximum: 2000 },
      },
      required: ['value'],
      additionalProperties: false,
    },
    annotations: { readOnlyHint: true },
  },
];

const modernMeta = {
  _meta: {
    'io.modelcontextprotocol/protocolVersion': '2026-07-28',
    'io.modelcontextprotocol/clientInfo': { name: 'generation-test-client', version: '1.0.0' },
    'io.modelcontextprotocol/clientCapabilities': {},
  },
};

test('structured MCP errors never collapse to object stringification', () => {
  assert.equal(describeUnknownError({ message: 'child failed', data: { code: 'child_failure' } }), 'child failed');
  assert.match(describeUnknownError({ data: { code: 'child_failure', reason: 'structured failure' } }), /structured failure/);
  assert.doesNotMatch(describeUnknownError({ reason: 'structured failure' }), /\[object Object\]/);
});

function surface(transport: 'stdio' | 'streamable_http', url?: string): DefinedSurface {
  return defineSurface({
    surface_id: `generation-${transport.replace('_', '-')}`,
    surface_version: '1.0.0',
    package: '@test/generation-fixture',
    tools: definitions.map((definition) => ({
      definition,
      effect: { class: 'read', idempotency: 'replayable', confirmation: 'never' },
    })),
    projections: [{
      id: transport,
      transport: transport === 'stdio'
        ? { kind: 'stdio', command: 'node', args: [], env: [] }
        : { kind: 'streamable_http', url: url!, headers: [] },
      injection_scope: 'local_site',
      default_injection: 'disabled',
      runtime_requirements: [],
      authority_requirements: [],
      lifecycle: transport === 'stdio'
        ? { mode: 'replayable' }
        : { mode: 'session_pinned' },
    }],
  });
}

function stdioCandidate(
  generationId: string,
  version: string,
  expectedDigest?: string,
) {
  const defined = surface('stdio');
  const fixturePath = fileURLToPath(
    new URL('./fixtures/generation-stdio-fixture.js', import.meta.url),
  );
  return {
    generation_id: generationId,
    lifecycle: defined.descriptor.projections[0]!.lifecycle,
    expected_contract_digest: expectedDigest ?? defined.tool_contract_digest,
    health_call: { name: 'generation_fixture_health' },
    adapter: new JsonLineStdioGenerationAdapter({
      descriptor: defined.descriptor,
      command: process.execPath,
      args: [fixturePath],
      cwd: path.dirname(fixturePath),
      env: { GENERATION_VERSION: version },
    }),
  };
}

test('real stdio replacement is atomic and drains an in-flight old call', async () => {
  const manager = new GenerationManager({ drain_timeout_ms: 500 });
  try {
    await manager.bootstrap(stdioCandidate('stdio-1', 'one'));
    const oldCall = manager.route({
      method: 'tools/call',
      params: {
        name: 'generation_fixture_echo',
        arguments: { value: 'old', delay_ms: 80 },
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 15));
    const replaced = await manager.replace(stdioCandidate('stdio-2', 'two'));
    assert.equal(replaced.status, 'activated');
    const newCall = await manager.route({
      method: 'tools/call',
      params: {
        name: 'generation_fixture_echo',
        arguments: { value: 'new' },
      },
    });
    assert.equal(newCall.status, 'ok');
    if (newCall.status === 'ok') {
      assert.equal((newCall.response.structuredContent as any).version, 'two');
    }
    const oldResult = await oldCall;
    assert.equal(oldResult.status, 'ok');
    if (oldResult.status === 'ok') {
      assert.equal((oldResult.response.structuredContent as any).version, 'one');
    }
    assert.equal(manager.activeGenerationId(), 'stdio-2');
    assert.equal(
      manager.snapshots().find((generation) => generation.generation_id === 'stdio-1')?.state,
      'terminated',
    );
  } finally {
    await manager.close();
  }
});

test('failed contract warm-up leaves the old generation active', async () => {
  const manager = new GenerationManager();
  try {
    const first = stdioCandidate('stable', 'stable');
    await manager.bootstrap(first);
    const failed = await manager.replace(stdioCandidate('bad', 'bad', '0'.repeat(64)));
    assert.equal(failed.status, 'warmup_failed');
    assert.equal(manager.activeGenerationId(), 'stable');
    const response = await manager.route({
      method: 'tools/call',
      params: { name: 'generation_fixture_echo', arguments: { value: 'still-live' } },
    });
    assert.equal(response.status, 'ok');
    if (response.status === 'ok') {
      assert.equal((response.response.structuredContent as any).version, 'stable');
    }
  } finally {
    await manager.close();
  }
});

test('restart_required replacement refuses before starting a generation', async () => {
  const manager = new GenerationManager();
  let starts = 0;
  const adapter: GenerationAdapter<object> = {
    transport: 'stdio',
    async start() { starts += 1; return {}; },
    async warm() { return { contract_digest: 'x', tools: [] }; },
    async dispatch() { return {}; },
    async terminate() {},
  };
  const result = await manager.replace({
    generation_id: 'restart-required',
    lifecycle: { mode: 'restart_required', restart_owner: 'mcp-loader' },
    expected_contract_digest: 'x',
    adapter,
  });
  assert.equal(result.status, 'restart_required');
  assert.equal(starts, 0);
  if (result.status === 'restart_required') {
    assert.equal(result.restart_owner, 'mcp-loader');
    assert.match(result.recovery, /restart the carrier\/session surface/);
  }
});

test('stable HTTP endpoint pins existing sessions and routes new sessions concurrently', async () => {
  const backendOne = await startBackend('one');
  const backendTwo = await startBackend('two');
  const manager = new GenerationManager({ drain_timeout_ms: 100 });
  const stable = await startStableHttpGenerationEndpoint(manager);
  try {
    await manager.bootstrap(httpCandidate('http-1', backendOne));
    const initial = await post(stable.url, {
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: 'generation_fixture_echo', arguments: { value: 'initial' } },
    });
    const oldSession = initial.sessionId;
    assert.ok(oldSession);
    assert.equal((initial.body.structuredContent as any).version, 'one');

    const replaced = await manager.replace(httpCandidate('http-2', backendTwo));
    assert.equal(replaced.status, 'activated');
    const [pinned, fresh] = await Promise.all([
      post(stable.url, {
        jsonrpc: '2.0',
        id: 2,
        method: 'tools/call',
        params: { name: 'generation_fixture_echo', arguments: { value: 'pinned' } },
      }, oldSession),
      post(stable.url, {
        jsonrpc: '2.0',
        id: 3,
        method: 'tools/call',
        params: { name: 'generation_fixture_echo', arguments: { value: 'fresh' } },
      }),
    ]);
    assert.equal((pinned.body.structuredContent as any).version, 'one');
    assert.equal((fresh.body.structuredContent as any).version, 'two');
    assert.notEqual(fresh.sessionId, oldSession);

    await new Promise((resolve) => setTimeout(resolve, 140));
    const retired = await post(stable.url, {
      jsonrpc: '2.0',
      id: 4,
      method: 'tools/call',
      params: { name: 'generation_fixture_echo', arguments: { value: 'retired' } },
    }, oldSession);
    assert.equal((retired.body.error as any).code, 'session_generation_retired');
    assert.match((retired.body.error as any).recovery, /new MCP HTTP session/);
  } finally {
    await stable.close();
    await manager.close();
    await backendOne.close();
    await backendTwo.close();
  }
});

test('modern HTTP requests are stateless and use standard routing headers', async () => {
  const backend = await startModernAwareBackend('modern');
  const manager = new GenerationManager();
  const stable = await startStableHttpGenerationEndpoint(manager);
  try {
    await manager.bootstrap(httpCandidate('modern-http', backend));
    const message = {
      jsonrpc: '2.0',
      id: 'modern-call',
      method: 'tools/call',
      params: {
        name: 'generation_fixture_echo',
        arguments: { value: 'modern' },
        ...modernMeta,
      },
    };
    const missingHeaders = await post(stable.url, message);
    assert.equal(missingHeaders.body.error?.code, -32020);
    const response = await post(stable.url, message, undefined, {
      'MCP-Protocol-Version': '2026-07-28',
      'Mcp-Method': 'tools/call',
      'Mcp-Name': 'generation_fixture_echo',
    });
    assert.equal(response.sessionId, null);
    assert.equal(response.body.resultType, 'complete');
    assert.equal((response.body.structuredContent as any).version, 'modern');
  } finally {
    await stable.close();
    await manager.close();
    await backend.close();
  }
});

function httpCandidate(
  generationId: string,
  backend: { url: string; close(): Promise<void> },
) {
  const defined = surface('streamable_http', backend.url);
  return {
    generation_id: generationId,
    lifecycle: defined.descriptor.projections[0]!.lifecycle,
    expected_contract_digest: defined.tool_contract_digest,
    health_call: { name: 'generation_fixture_health' },
    adapter: new StreamableHttpGenerationAdapter({
      descriptor: defined.descriptor,
      url: backend.url,
    }),
  };
}

async function startBackend(version: string): Promise<{ url: string; close(): Promise<void> }> {
  const server = createServer((request, response) => {
    void (async () => {
      const chunks: Buffer[] = [];
      for await (const chunk of request) chunks.push(Buffer.from(chunk));
      const message = JSON.parse(Buffer.concat(chunks).toString('utf8')) as Record<string, any>;
      let result: Record<string, unknown>;
      if (message.method === 'initialize') {
        result = { protocolVersion: '2024-11-05', capabilities: { tools: {} } };
      } else if (message.method === 'tools/list') {
        result = { tools: definitions };
      } else if (message.params?.name === 'generation_fixture_health') {
        result = { structuredContent: { status: 'ok', version } };
      } else {
        result = {
          content: [{ type: 'text', text: version }],
          structuredContent: { version, value: message.params?.arguments?.value },
        };
      }
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ jsonrpc: '2.0', id: message.id ?? null, result }));
    })();
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => resolve());
  });
  const address = server.address();
  if (address === null || typeof address === 'string') throw new Error('backend_address_missing');
  return {
    url: `http://127.0.0.1:${address.port}/mcp`,
    close: () => new Promise<void>((resolve, reject) =>
      server.close((error) => error ? reject(error) : resolve())),
  };
}

async function startModernAwareBackend(version: string): Promise<{ url: string; close(): Promise<void> }> {
  const server = createServer((request, response) => {
    void (async () => {
      const chunks: Buffer[] = [];
      for await (const chunk of request) chunks.push(Buffer.from(chunk));
      const message = JSON.parse(Buffer.concat(chunks).toString('utf8')) as Record<string, any>;
      const meta = message.params?._meta;
      const modern = meta?.['io.modelcontextprotocol/protocolVersion'] === '2026-07-28';
      const method = String(message.method ?? '');
      const expectedName = method === 'tools/call' ? String(message.params?.name ?? '') : method;
      if (modern) {
        if (request.headers['mcp-protocol-version'] !== '2026-07-28') throw new Error('modern_protocol_header_missing');
        if (request.headers['mcp-method'] !== method) throw new Error('modern_method_header_missing');
        if (request.headers['mcp-name'] !== expectedName) throw new Error('modern_name_header_missing');
        if (request.headers['mcp-session-id'] !== undefined) throw new Error('modern_session_header_present');
      }
      let result: Record<string, unknown>;
      if (method === 'initialize') {
        result = { protocolVersion: '2024-11-05', capabilities: { tools: {} } };
      } else if (method === 'tools/list') {
        result = modern
          ? { resultType: 'complete', tools: definitions, ttlMs: 300_000, cacheScope: 'public' }
          : { tools: definitions };
      } else {
        result = modern
          ? { resultType: 'complete', content: [{ type: 'text', text: version }], structuredContent: { version, value: message.params?.arguments?.value } }
          : { content: [{ type: 'text', text: version }], structuredContent: { version, value: message.params?.arguments?.value } };
      }
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ jsonrpc: '2.0', id: message.id ?? null, result }));
    })().catch((error) => {
      response.writeHead(500, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ error: error instanceof Error ? error.message : String(error) }));
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => resolve());
  });
  const address = server.address();
  if (address === null || typeof address === 'string') throw new Error('backend_address_missing');
  return {
    url: `http://127.0.0.1:${address.port}/mcp`,
    close: () => new Promise<void>((resolve, reject) =>
      server.close((error) => error ? reject(error) : resolve())),
  };
}

async function post(
  url: string,
  message: Record<string, unknown>,
  sessionId?: string,
  headers: Record<string, string> = {},
): Promise<{ body: Record<string, any>; sessionId: string | null }> {
  const response = await fetch(url, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      ...(sessionId ? { 'mcp-session-id': sessionId } : {}),
      ...headers,
    },
    body: JSON.stringify(message),
  });
  return {
    body: await response.json() as Record<string, any>,
    sessionId: response.headers.get('mcp-session-id'),
  };
}
