import assert from 'node:assert/strict';
import test from 'node:test';
import {
  MODERN_PROTOCOL_VERSION,
  modernServerResult,
  modernRequestMeta,
  protocolEraForParams,
  serverDiscoverResult,
  withCacheMetadata,
  withModernRequestMeta,
  withResultType,
} from '../src/index.js';

test('modern request metadata is self-describing and preserves caller metadata', () => {
  const params = withModernRequestMeta({ value: 1, _meta: { traceparent: 'trace' } }, {
    clientInfo: { name: 'test-client', version: '1.0.0' },
    clientCapabilities: { tools: {} },
  });
  assert.equal((params._meta as Record<string, unknown>).traceparent, 'trace');
  assert.equal((params._meta as Record<string, unknown>)['io.modelcontextprotocol/protocolVersion'], MODERN_PROTOCOL_VERSION);
  assert.equal(protocolEraForParams(params), 'modern');
});

test('modern result helpers add resultType and cache metadata', () => {
  assert.equal(withResultType({ status: 'ok' }).resultType, 'complete');
  assert.deepEqual(withCacheMetadata({ tools: [] }, { ttlMs: 42, cacheScope: 'public' }), {
    tools: [], resultType: 'complete', ttlMs: 42, cacheScope: 'public',
  });
});

test('server discover is a complete cacheable modern result', () => {
  const result = serverDiscoverResult({
    serverInfo: { name: 'server', version: '1.0.0' },
    capabilities: { tools: {} },
  });
  assert.equal(result.resultType, 'complete');
  assert.deepEqual(result.supportedVersions, [MODERN_PROTOCOL_VERSION, '2024-11-05']);
  assert.equal(result.cacheScope, 'public');
  assert.equal((result._meta as Record<string, unknown>)['io.modelcontextprotocol/serverInfo'] !== undefined, true);
  assert.deepEqual(modernRequestMeta({ clientInfo: { name: 'c', version: '1' } }), {
    'io.modelcontextprotocol/protocolVersion': MODERN_PROTOCOL_VERSION,
    'io.modelcontextprotocol/clientInfo': { name: 'c', version: '1' },
    'io.modelcontextprotocol/clientCapabilities': {},
  });
});
