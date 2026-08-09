import { existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { McpProcessClient, isRecord, type JsonRecord } from './process-client.js';

export interface SiteFabricClientOptions {
  siteRoot: string;
  loaderEntrypoint?: string;
  nodeExecutable?: string;
  loaderImplementation?: McpLoaderImplementation;
  allowedSurfaceIds?: readonly string[];
  requestTimeoutMs?: number;
  closeTimeoutMs?: number;
  detachTimeoutMs?: number;
  maxConnections?: number;
  maxMaterializedResultChars?: number;
  materializedResultPageChars?: number;
  env?: NodeJS.ProcessEnv;
}

export type McpLoaderImplementation = 'native' | 'javascript';

export type McpLoaderLaunch = {
  executable: string;
  args: string[];
  implementation: McpLoaderImplementation;
};

function isToolResultEnvelope(value: JsonRecord): boolean {
  return value.isError === true || isRecord(value.structuredContent) || Array.isArray(value.content);
}

export interface SiteFabricToolCallOptions {
  runtimeKind?: string;
  timeoutMs?: number;
}

interface AttachedSurface {
  connectionId: string;
  surfaceId: string;
  runtimeKind: string | null;
}

const DEFAULT_MAX_MATERIALIZED_RESULT_CHARS = 1_000_000;
const DEFAULT_MATERIALIZED_RESULT_PAGE_CHARS = 20_000;
const MAX_MATERIALIZED_RESULT_PAGE_CHARS = 20_000;
const MAX_MATERIALIZED_RESULT_PAGES = 256;
const MAX_MATERIALIZED_RESULT_DEPTH = 4;

export class SiteFabricClient {
  readonly siteRoot: string;
  readonly allowedSurfaceIds: ReadonlySet<string> | null;

  #client: McpProcessClient;
  #connections = new Map<string, AttachedSurface>();
  #attachments = new Map<string, Promise<AttachedSurface>>();
  #detachTimeoutMs: number;
  #maxMaterializedResultChars: number;
  #materializedResultPageChars: number;
  #closed = false;

  private constructor(
    client: McpProcessClient,
    siteRoot: string,
    allowedSurfaceIds: ReadonlySet<string> | null,
    detachTimeoutMs: number,
    maxMaterializedResultChars: number,
    materializedResultPageChars: number,
  ) {
    this.#client = client;
    this.siteRoot = siteRoot;
    this.allowedSurfaceIds = allowedSurfaceIds;
    this.#detachTimeoutMs = detachTimeoutMs;
    this.#maxMaterializedResultChars = maxMaterializedResultChars;
    this.#materializedResultPageChars = materializedResultPageChars;
  }

  static async open(options: SiteFabricClientOptions): Promise<SiteFabricClient> {
    const siteRoot = requiredString(options.siteRoot, 'siteRoot');
    const allowedSurfaceIds = normalizeSurfaceIds(options.allowedSurfaceIds);
    const maxConnections = positiveInteger(options.maxConnections, Math.max(8, allowedSurfaceIds?.size ?? 0), 'maxConnections');
    const detachTimeoutMs = positiveInteger(
      options.detachTimeoutMs,
      options.closeTimeoutMs ?? 5_000,
      'detachTimeoutMs',
    );
    const maxMaterializedResultChars = positiveInteger(
      options.maxMaterializedResultChars,
      DEFAULT_MAX_MATERIALIZED_RESULT_CHARS,
      'maxMaterializedResultChars',
    );
    const materializedResultPageChars = positiveIntegerAtMost(
      options.materializedResultPageChars,
      DEFAULT_MATERIALIZED_RESULT_PAGE_CHARS,
      MAX_MATERIALIZED_RESULT_PAGE_CHARS,
      'materializedResultPageChars',
    );
    const launch = resolveLoaderLaunch(options);
    const args = [
      ...launch.args,
      '--allowed-site-root', siteRoot,
      '--max-connections', String(maxConnections),
    ];
    for (const surfaceId of allowedSurfaceIds ?? []) args.push('--allowed-surface-id', surfaceId);

    const client = await McpProcessClient.start({
      executable: launch.executable,
      args,
      env: { ...process.env, ...options.env, NARADA_SITE_ROOT: siteRoot },
      requestTimeoutMs: options.requestTimeoutMs,
      closeTimeoutMs: options.closeTimeoutMs,
      clientName: 'narada-site-fabric-runtime-client',
    });
    return new SiteFabricClient(
      client,
      siteRoot,
      allowedSurfaceIds,
      detachTimeoutMs,
      maxMaterializedResultChars,
      materializedResultPageChars,
    );
  }

  async attach(surfaceId: string, runtimeKind?: string): Promise<AttachedSurface> {
    this.#assertOpen();
    const normalizedSurfaceId = requiredString(surfaceId, 'surfaceId');
    this.#assertSurfaceAllowed(normalizedSurfaceId);
    const normalizedRuntimeKind = optionalString(runtimeKind);
    const key = connectionKey(normalizedSurfaceId, normalizedRuntimeKind);
    const existing = this.#connections.get(key);
    if (existing) return existing;
    const pending = this.#attachments.get(key);
    if (pending) return await pending;

    const attachment = (async (): Promise<AttachedSurface> => {
      const attached = unwrapOuterToolResult(await this.#client.callTool('mcp_loader_attach_surface', {
        site_root: this.siteRoot,
        surface_id: normalizedSurfaceId,
        ...(normalizedRuntimeKind ? { runtime_kind: normalizedRuntimeKind } : {}),
      }));
      const connectionId = requiredString(attached.connection_id, 'mcp_loader_attach_connection_id');
      this.#assertOpen();
      const connection = { connectionId, surfaceId: normalizedSurfaceId, runtimeKind: normalizedRuntimeKind };
      this.#connections.set(key, connection);
      return connection;
    })();
    this.#attachments.set(key, attachment);
    try {
      return await attachment;
    } finally {
      this.#attachments.delete(key);
    }
  }

  async call(
    surfaceId: string,
    toolName: string,
    args: JsonRecord = {},
    options: SiteFabricToolCallOptions = {},
  ): Promise<JsonRecord> {
    const connection = await this.attach(surfaceId, options.runtimeKind);
    const timeoutMs = options.timeoutMs ?? this.#client.requestTimeoutMs;
    const deadlineAt = Date.now() + timeoutMs;
    const outer = unwrapOuterToolResult(await this.#client.callTool('mcp_loader_call_tool', {
      connection_id: connection.connectionId,
      tool_name: requiredString(toolName, 'toolName'),
      arguments: args,
    }, timeoutMs));
    const context = `${surfaceId}:${toolName}`;
    const childResult = outer.result_bounded === true || typeof outer.details_ref === 'string'
      ? await this.#readMaterializedRecord(
        connection,
        requiredString(outer.details_ref, 'mcp_loader_materialized_result_ref_missing'),
        deadlineAt,
        `mcp-loader:${context}`,
      )
      : asRecordStrict(outer.result, 'mcp_loader_child_result_missing');
    return await this.#unwrapChildResult(childResult, connection, deadlineAt, context);
  }

  async #unwrapChildResult(
    childResult: JsonRecord,
    connection: AttachedSurface,
    deadlineAt: number,
    context: string,
  ): Promise<JsonRecord> {
    let value = unwrapChildToolResult(childResult, context);
    for (let depth = 0; ; depth += 1) {
      if (!isMaterializedOutputPage(value)) return value;
      if (depth >= MAX_MATERIALIZED_RESULT_DEPTH) {
        throw new Error(`mcp_runtime_materialized_result_depth_exceeded:${context}`);
      }
      const loaded = await this.#readMaterializedRecord(
        connection,
        requiredString(value.output_ref ?? value.ref, 'mcp_runtime_materialized_output_ref_missing'),
        deadlineAt,
        context,
      );
      // A materialized page can contain another loader/tool envelope. Normalize
      // every loaded hop before deciding whether another page must be read.
      value = isToolResultEnvelope(loaded) ? unwrapChildToolResult(loaded, context) : loaded;
    }
  }

  async #readMaterializedRecord(
    connection: AttachedSurface,
    ref: string,
    deadlineAt: number,
    context: string,
  ): Promise<JsonRecord> {
    const chunks: string[] = [];
    let offset = 0;
    let declaredLength: number | null = null;
    for (let pageNumber = 0; pageNumber < MAX_MATERIALIZED_RESULT_PAGES; pageNumber += 1) {
      const remainingMs = deadlineAt - Date.now();
      if (remainingMs <= 0) throw new Error(`mcp_runtime_materialized_result_timeout:${context}`);
      const response = unwrapOuterToolResult(await this.#client.callTool('mcp_loader_read_result', {
        connection_id: connection.connectionId,
        ref,
        offset,
        limit: this.#materializedResultPageChars,
      }, remainingMs));
      const page = asRecordStrict(response.result, 'mcp_loader_materialized_result_page_missing');
      if (page.schema !== 'narada.mcp_output_page.v1') {
        throw new Error(`mcp_runtime_materialized_result_page_schema_invalid:${context}`);
      }
      const pageRef = requiredString(page.ref ?? page.output_ref, 'mcp_runtime_materialized_result_page_ref_missing');
      if (pageRef !== ref) throw new Error(`mcp_runtime_materialized_result_ref_mismatch:${context}`);
      const pageOffset = nonNegativeInteger(page.offset, 'mcp_runtime_materialized_result_page_offset_invalid');
      if (pageOffset !== offset) throw new Error(`mcp_runtime_materialized_result_page_offset_mismatch:${context}`);
      const fullLength = nonNegativeInteger(
        page.full_output_char_length,
        'mcp_runtime_materialized_result_length_invalid',
      );
      if (declaredLength === null) declaredLength = fullLength;
      if (declaredLength !== fullLength) {
        throw new Error(`mcp_runtime_materialized_result_length_changed:${context}`);
      }
      if (fullLength > this.#maxMaterializedResultChars) {
        throw new Error(
          `mcp_runtime_materialized_result_too_large:${context}:${fullLength}>${this.#maxMaterializedResultChars}`,
        );
      }
      if (typeof page.output_text !== 'string') {
        throw new Error(`mcp_runtime_materialized_result_page_text_missing:${context}`);
      }
      chunks.push(page.output_text);
      const consumed = offset + page.output_text.length;
      if (consumed > fullLength) throw new Error(`mcp_runtime_materialized_result_length_exceeded:${context}`);
      if (page.next_offset === null || page.next_offset === undefined) {
        if (consumed !== fullLength) throw new Error(`mcp_runtime_materialized_result_incomplete:${context}`);
        return parseJsonRecord(chunks.join(''), `mcp_runtime_materialized_result_not_object:${context}`);
      }
      const nextOffset = nonNegativeInteger(
        page.next_offset,
        'mcp_runtime_materialized_result_next_offset_invalid',
      );
      if (nextOffset !== consumed || nextOffset <= offset) {
        throw new Error(`mcp_runtime_materialized_result_page_progress_invalid:${context}`);
      }
      offset = nextOffset;
    }
    throw new Error(`mcp_runtime_materialized_result_page_limit_exceeded:${context}`);
  }

  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    this.#attachments.clear();
    let firstError: Error | null = null;
    const connections = [...this.#connections.values()].reverse();
    this.#connections.clear();
    const detachResults = await Promise.allSettled(connections.map((connection) => this.#client.callTool(
      'mcp_loader_detach',
      { connection_id: connection.connectionId },
      this.#detachTimeoutMs,
    )));
    for (let index = 0; index < detachResults.length; index += 1) {
      const result = detachResults[index]!;
      if (result.status === 'rejected') {
        const connection = connections[index]!;
        const reason = toError(result.reason);
        firstError ??= new Error(`mcp_loader_detach_failed:${connection.surfaceId}:${reason.message}`);
      }
    }
    await this.#client.close();
    if (firstError) throw firstError;
  }

  #assertOpen(): void {
    if (this.#closed) throw new Error('site_fabric_client_closed');
  }

  #assertSurfaceAllowed(surfaceId: string): void {
    if (this.allowedSurfaceIds && !this.allowedSurfaceIds.has(surfaceId)) {
      throw new Error(`site_fabric_surface_not_allowed:${surfaceId}`);
    }
  }
}

export function defaultMcpLoaderEntrypoint(): string {
  return resolveMcpLoaderPackagePath('dist', 'src', 'main.js');
}

export function defaultMcpLoaderNativeEntrypoint(): string {
  return resolveMcpLoaderPackagePath(
    'dist',
    'native',
    process.platform === 'win32' ? 'narada-mcp-loader.exe' : 'narada-mcp-loader',
  );
}

export function defaultMcpLoaderLaunch(implementation?: McpLoaderImplementation): McpLoaderLaunch {
  const resolvedImplementation = implementation ?? defaultMcpLoaderImplementation();
  if (resolvedImplementation === 'native') {
    const executable = defaultMcpLoaderNativeEntrypoint();
    if (!existsSync(executable)) throw new Error(`mcp_runtime_client_native_loader_missing:${executable}`);
    return { executable, args: [], implementation: 'native' };
  }
  return { executable: process.execPath, args: [defaultMcpLoaderEntrypoint()], implementation: 'javascript' };
}

function resolveLoaderLaunch(options: SiteFabricClientOptions): McpLoaderLaunch {
  if (options.loaderEntrypoint || options.nodeExecutable) {
    if (options.loaderImplementation === 'native') throw new Error('mcp_runtime_client_native_loader_conflicts_with_legacy_launch_options');
    return {
      executable: options.nodeExecutable ?? process.execPath,
      args: [options.loaderEntrypoint ?? defaultMcpLoaderEntrypoint()],
      implementation: 'javascript',
    };
  }
  return defaultMcpLoaderLaunch(options.loaderImplementation);
}

function defaultMcpLoaderImplementation(): McpLoaderImplementation {
  const profile = process.env.NARADA_RUNTIME_PROFILE?.trim();
  if (profile === 'bun' || profile === 'node-compat') return 'javascript';
  const nativeEntrypoint = defaultMcpLoaderNativeEntrypoint();
  if (existsSync(nativeEntrypoint)) return 'native';
  if (profile === 'native') throw new Error(`mcp_runtime_client_native_loader_missing:${nativeEntrypoint}`);
  return 'javascript';
}

function resolveMcpLoaderPackagePath(...segments: string[]): string {
  const sourceDirectory = dirname(fileURLToPath(import.meta.url));
  const parent = resolve(sourceDirectory, '..');
  const packageRoot = parent.endsWith(`${separator()}dist`) ? resolve(parent, '..') : parent;
  return resolve(packageRoot, '..', '..', 'mcp-loader-mcp', ...segments);
}

function unwrapOuterToolResult(result: JsonRecord): JsonRecord {
  return unwrapChildToolResult(result, 'mcp-loader');
}

function unwrapChildToolResult(result: JsonRecord, context: string): JsonRecord {
  if (result.isError === true) throw new Error(`mcp_tool_error:${context}:${toolText(result) || 'unknown'}`);
  if (isRecord(result.structuredContent)) return result.structuredContent;
  const text = toolText(result);
  if (!text) return {};
  try {
    return asRecordStrict(JSON.parse(text), `mcp_tool_result_not_object:${context}`);
  } catch (error) {
    if (error instanceof SyntaxError) throw new Error(`mcp_tool_result_not_json:${context}`);
    throw error;
  }
}

function toolText(result: JsonRecord): string {
  if (!Array.isArray(result.content)) return '';
  return result.content
    .map((item) => isRecord(item) && item.type === 'text' && typeof item.text === 'string' ? item.text : '')
    .filter(Boolean)
    .join('\n');
}

function normalizeSurfaceIds(values: readonly string[] | undefined): ReadonlySet<string> | null {
  if (values === undefined) return null;
  const normalized = new Set(values.map((value) => requiredString(value, 'allowedSurfaceId')));
  if (normalized.size === 0) throw new Error('allowedSurfaceIds_must_not_be_empty');
  return normalized;
}

function connectionKey(surfaceId: string, runtimeKind: string | null): string {
  return `${surfaceId}\u0000${runtimeKind ?? ''}`;
}

function asRecordStrict(value: unknown, code: string): JsonRecord {
  if (!isRecord(value)) throw new Error(code);
  return value;
}

function requiredString(value: unknown, name: string): string {
  const normalized = typeof value === 'string' ? value.trim() : '';
  if (!normalized) throw new Error(`${name}_must_be_non_empty_string`);
  return normalized;
}

function optionalString(value: unknown): string | null {
  const normalized = typeof value === 'string' ? value.trim() : '';
  return normalized || null;
}

function positiveInteger(value: number | undefined, fallback: number, name: string): number {
  const resolved = value ?? fallback;
  if (!Number.isSafeInteger(resolved) || resolved <= 0) throw new Error(`${name}_must_be_positive_integer`);
  return resolved;
}

function positiveIntegerAtMost(value: number | undefined, fallback: number, maximum: number, name: string): number {
  const resolved = positiveInteger(value, fallback, name);
  if (resolved > maximum) throw new Error(`${name}_must_be_at_most_${maximum}`);
  return resolved;
}

function nonNegativeInteger(value: unknown, code: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) throw new Error(code);
  return value as number;
}

function isMaterializedOutputPage(value: JsonRecord): boolean {
  return value.schema === 'narada.producer_output_page.v1' && value.result_materialized === true;
}

function parseJsonRecord(serialized: string, code: string): JsonRecord {
  try {
    return asRecordStrict(JSON.parse(serialized), code);
  } catch (error) {
    if (error instanceof SyntaxError) throw new Error(`${code}:invalid_json`);
    throw error;
  }
}

function separator(): string {
  return process.platform === 'win32' ? '\\' : '/';
}

function toError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}
