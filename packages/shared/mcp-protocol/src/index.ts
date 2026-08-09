export type JsonRecord = Record<string, unknown>;

export const MODERN_PROTOCOL_VERSION = '2026-07-28' as const;
export const DEFAULT_LEGACY_PROTOCOL_VERSION = '2024-11-05' as const;
export const SUPPORTED_PROTOCOL_VERSIONS = Object.freeze([
  MODERN_PROTOCOL_VERSION,
  DEFAULT_LEGACY_PROTOCOL_VERSION,
] as const);
export const MODERN_PROTOCOL_META_KEYS = Object.freeze({
  protocolVersion: 'io.modelcontextprotocol/protocolVersion',
  clientInfo: 'io.modelcontextprotocol/clientInfo',
  clientCapabilities: 'io.modelcontextprotocol/clientCapabilities',
  serverInfo: 'io.modelcontextprotocol/serverInfo',
} as const);

export type McpProtocolEra = 'modern' | 'legacy';
export type McpResultType = 'complete' | 'input_required';
export type McpCacheScope = 'public' | 'private';

export type McpClientIdentity = { name: string; version: string };
export type McpProtocolClientOptions = {
  clientInfo: McpClientIdentity;
  clientCapabilities?: JsonRecord;
};
export type McpProtocolServerOptions = {
  serverInfo: McpClientIdentity;
  capabilities: JsonRecord;
  instructions?: string;
  supportedVersions?: readonly string[];
};

export function isRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export function modernRequestMeta(options: McpProtocolClientOptions): JsonRecord {
  return {
    [MODERN_PROTOCOL_META_KEYS.protocolVersion]: MODERN_PROTOCOL_VERSION,
    [MODERN_PROTOCOL_META_KEYS.clientInfo]: options.clientInfo,
    [MODERN_PROTOCOL_META_KEYS.clientCapabilities]: options.clientCapabilities ?? {},
  };
}

export function withModernRequestMeta(params: JsonRecord = {}, options: McpProtocolClientOptions): JsonRecord {
  const existingMeta = isRecord(params._meta) ? params._meta : {};
  return { ...params, _meta: { ...existingMeta, ...modernRequestMeta(options) } };
}

export function requestedProtocolVersion(params: unknown): string | null {
  const record = isRecord(params) ? params : {};
  const meta = isRecord(record._meta) ? record._meta : {};
  const version = meta[MODERN_PROTOCOL_META_KEYS.protocolVersion];
  return typeof version === 'string' && version.trim() ? version.trim() : null;
}

export function isModernRequest(params: unknown): boolean {
  return requestedProtocolVersion(params) === MODERN_PROTOCOL_VERSION;
}

export function protocolEraForParams(params: unknown): McpProtocolEra {
  return isModernRequest(params) ? 'modern' : 'legacy';
}

export function unsupportedProtocolVersionError(requested: string | null): JsonRecord {
  return {
    code: -32022,
    message: 'Unsupported protocol version',
    data: { supported: [...SUPPORTED_PROTOCOL_VERSIONS], requested },
  };
}

export function serverDiscoverResult(options: McpProtocolServerOptions): JsonRecord {
  return {
    resultType: 'complete',
    supportedVersions: [...(options.supportedVersions ?? SUPPORTED_PROTOCOL_VERSIONS)],
    capabilities: options.capabilities,
    _meta: { [MODERN_PROTOCOL_META_KEYS.serverInfo]: options.serverInfo },
    ...(options.instructions ? { instructions: options.instructions } : {}),
    ttlMs: 3_600_000,
    cacheScope: 'public',
  };
}

export function withResultType<T>(result: T, resultType: McpResultType = 'complete'): T & { resultType: McpResultType } {
  if (!isRecord(result)) return { value: result, resultType } as unknown as T & { resultType: McpResultType };
  if (typeof result.resultType === 'string') return result as T & { resultType: McpResultType };
  return { ...result, resultType } as T & { resultType: McpResultType };
}

export function withCacheMetadata<T extends JsonRecord>(
  result: T,
  options: { ttlMs?: number; cacheScope?: McpCacheScope } = {},
): T & { resultType: McpResultType; ttlMs: number; cacheScope: McpCacheScope } {
  return {
    ...withResultType(result),
    ttlMs: options.ttlMs ?? 300_000,
    cacheScope: options.cacheScope ?? 'private',
  };
}

export function modernServerResult<T>(result: T, serverInfo: McpClientIdentity): T & { _meta: JsonRecord; resultType: McpResultType } {
  const value = withResultType(result) as T & JsonRecord & { resultType: McpResultType };
  return {
    ...value,
    _meta: {
      ...(isRecord(value._meta) ? value._meta : {}),
      [MODERN_PROTOCOL_META_KEYS.serverInfo]: serverInfo,
    },
  } as T & { _meta: JsonRecord; resultType: McpResultType };
}
