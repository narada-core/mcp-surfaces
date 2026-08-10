import { randomUUID } from 'node:crypto';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { createServer, type Server } from 'node:http';
import {
  assertLiveToolsConform,
  liveToolsContractDigest,
  type LifecycleRequirement,
  type McpToolDefinition,
  type SurfaceDescriptorV2,
} from '@narada-core/mcp-fabric-contracts';
import { describeUnknownError } from './error-description.js';

type JsonRecord = Record<string, unknown>;

const MODERN_PROTOCOL_VERSION = '2026-07-28';
const MODERN_PROTOCOL_VERSION_META = 'io.modelcontextprotocol/protocolVersion';

export type GenerationState =
  | 'starting'
  | 'warming'
  | 'active'
  | 'draining'
  | 'terminated'
  | 'failed';

export type GenerationHealthCall = {
  name: string;
  arguments?: JsonRecord;
};

export type GenerationAdapter<Handle = unknown> = {
  transport: 'stdio' | 'streamable_http';
  start(): Promise<Handle>;
  warm(
    handle: Handle,
    input: {
      initialization: JsonRecord;
      expected_contract_digest: string;
      health_call?: GenerationHealthCall;
    },
  ): Promise<{ contract_digest: string; tools: McpToolDefinition[] }>;
  dispatch(
    handle: Handle,
    request: JsonRecord,
    context: { session_id: string | null },
  ): Promise<JsonRecord>;
  terminate(handle: Handle): Promise<void>;
};

export type GenerationCandidate<Handle = unknown> = {
  generation_id: string;
  lifecycle: LifecycleRequirement;
  descriptor_digest?: string;
  expected_contract_digest: string;
  adapter: GenerationAdapter<Handle>;
  health_call?: GenerationHealthCall;
};

export type GenerationSnapshot = {
  generation_id: string;
  state: GenerationState;
  transport: 'stdio' | 'streamable_http';
  inflight: number;
  started_at: string;
  activated_at: string | null;
  drain_deadline: string | null;
  heartbeat_at: string;
  lease_expires_at: string;
  freshness: 'current' | 'stale' | 'unknown';
  health: 'healthy' | 'degraded' | 'unreachable' | 'unknown';
  descriptor_digest: string | null;
  tool_contract_digest: string | null;
  failure: string | null;
};

export type GenerationObservationEvent = {
  event: 'starting' | 'warming' | 'active' | 'draining' | 'heartbeat' | 'failed' | 'terminated';
  observed_at: string;
  logical_connection_id: string;
  generation: GenerationSnapshot;
};

export type GenerationObservationSink = (event: GenerationObservationEvent) => void;

type InternalGeneration = {
  candidate: GenerationCandidate<any>;
  handle: unknown;
  state: GenerationState;
  inflight: number;
  startedAt: number;
  activatedAt: number | null;
  drainDeadline: number | null;
  heartbeatAt: number;
  leaseExpiresAt: number;
  freshness: 'current' | 'stale' | 'unknown';
  health: 'healthy' | 'degraded' | 'unreachable' | 'unknown';
  failure: string | null;
  retire: {
    promise: Promise<JsonRecord>;
    resolve(value: JsonRecord): void;
    settled: boolean;
  };
  drainTimer: ReturnType<typeof setTimeout> | null;
};

export type RouteResult =
  | {
      status: 'ok';
      generation_id: string;
      session_id: string | null;
      response: JsonRecord;
    }
  | {
      status: 'session_generation_retired';
      generation_id: string;
      session_id: string | null;
      error: JsonRecord;
    };

export class GenerationManager {
  private readonly generations = new Map<string, InternalGeneration>();
  private readonly sessions = new Map<string, string>();
  private readonly initialization: JsonRecord;
  private activeId: string | null = null;

  constructor(private readonly options: {
    drain_timeout_ms?: number;
    lease_ms?: number;
    logical_connection_id?: string;
    observation_sink?: GenerationObservationSink;
    initialization?: JsonRecord;
  } = {}) {
    this.initialization = options.initialization ?? {
      protocolVersion: '2024-11-05',
      capabilities: {},
      clientInfo: { name: 'mcp-runtime-proxy-generation-manager', version: '0.1.0' },
    };
  }

  async bootstrap(candidate: GenerationCandidate): Promise<GenerationSnapshot> {
    if (this.activeId !== null) throw new Error('mcp_generation_active_already_exists');
    const generation = await this.warmCandidate(candidate);
    this.activate(generation, null);
    return snapshot(generation);
  }

  async replace(candidate: GenerationCandidate): Promise<
    | { status: 'activated'; active: GenerationSnapshot; draining: GenerationSnapshot | null }
    | { status: 'warmup_failed'; active: GenerationSnapshot | null; failed: GenerationSnapshot }
    | {
        status: 'restart_required';
        active: GenerationSnapshot | null;
        restart_owner: string;
        recovery: string;
      }
  > {
    const active = this.activeGeneration();
    if (candidate.lifecycle.mode === 'restart_required') {
      const restartOwner = candidate.lifecycle.restart_owner ?? 'carrier-session-owner';
      return {
        status: 'restart_required',
        active: active ? snapshot(active) : null,
        restart_owner: restartOwner,
        recovery: `Hot replacement is refused. Ask ${restartOwner} to restart the carrier/session surface, then reconnect and repeat tools/list.`,
      };
    }
    let replacement: InternalGeneration;
    try {
      replacement = await this.warmCandidate(candidate);
    } catch (error) {
      const failed = this.generations.get(candidate.generation_id);
      if (failed === undefined) throw error;
      return {
        status: 'warmup_failed',
        active: active ? snapshot(active) : null,
        failed: snapshot(failed),
      };
    }
    this.activate(replacement, active);
    return {
      status: 'activated',
      active: snapshot(replacement),
      draining: active ? snapshot(active) : null,
    };
  }

  async route(request: JsonRecord, options: { session_id?: string } = {}): Promise<RouteResult> {
    const active = this.activeGeneration();
    if (active === null) throw new Error('mcp_generation_active_missing');
    const modern = isModernProtocolRequest(request);
    let sessionId = modern ? null : options.session_id ?? null;
    let generation = active;
    if (active.candidate.adapter.transport === 'streamable_http' && !modern) {
      if (sessionId === null) {
        sessionId = randomUUID();
        this.sessions.set(sessionId, active.candidate.generation_id);
      } else {
        const pinnedId = this.sessions.get(sessionId);
        if (pinnedId !== undefined) {
          generation = this.generations.get(pinnedId) ?? active;
        } else {
          this.sessions.set(sessionId, active.candidate.generation_id);
        }
      }
    }
    if (generation.state === 'terminated' || generation.state === 'failed') {
      return retiredRouteResult(generation, sessionId);
    }
    generation.inflight += 1;
    this.touch(generation);
    try {
      const dispatched = generation.candidate.adapter.dispatch(
        generation.handle,
        request,
        { session_id: sessionId },
      );
      const winner = await Promise.race([
        dispatched.then((response) => ({ kind: 'response' as const, response })),
        generation.retire.promise.then((error) => ({ kind: 'retired' as const, error })),
      ]);
      if (winner.kind === 'retired') {
        return {
          status: 'session_generation_retired',
          generation_id: generation.candidate.generation_id,
          session_id: sessionId,
          error: winner.error,
        };
      }
      return {
        status: 'ok',
        generation_id: generation.candidate.generation_id,
        session_id: sessionId,
        response: winner.response,
      };
    } finally {
      generation.inflight -= 1;
      this.touch(generation);
      if (
        generation.state === 'draining'
        && generation.candidate.adapter.transport === 'stdio'
        && generation.inflight === 0
      ) {
        await this.terminateGeneration(generation, 'stdio_drain_completed');
      }
    }
  }

  snapshots(): GenerationSnapshot[] {
    return [...this.generations.values()]
      .map(snapshot)
      .sort((left, right) => left.started_at.localeCompare(right.started_at));
  }

  activeGenerationId(): string | null {
    return this.activeId;
  }

  async close(): Promise<void> {
    await Promise.all([...this.generations.values()]
      .filter((generation) => generation.state !== 'terminated')
      .map((generation) => this.terminateGeneration(generation, 'manager_closed')));
    this.activeId = null;
    this.sessions.clear();
  }

  private activeGeneration(): InternalGeneration | null {
    return this.activeId === null ? null : this.generations.get(this.activeId) ?? null;
  }

  private async warmCandidate(candidate: GenerationCandidate): Promise<InternalGeneration> {
    if (this.generations.has(candidate.generation_id)) {
      throw new Error(`mcp_generation_duplicate: ${candidate.generation_id}`);
    }
    const retire = deferred<JsonRecord>();
    const generation: InternalGeneration = {
      candidate,
      handle: null,
      state: 'starting',
      inflight: 0,
      startedAt: Date.now(),
      activatedAt: null,
      drainDeadline: null,
      heartbeatAt: Date.now(),
      leaseExpiresAt: Date.now() + (this.options.lease_ms ?? 30_000),
      freshness: 'current',
      health: 'unknown',
      failure: null,
      retire,
      drainTimer: null,
    };
    this.generations.set(candidate.generation_id, generation);
    this.emit('starting', generation);
    try {
      generation.handle = await candidate.adapter.start();
      generation.state = 'warming';
      this.emit('warming', generation);
      const warm = await candidate.adapter.warm(generation.handle, {
        initialization: this.initialization,
        expected_contract_digest: candidate.expected_contract_digest,
        health_call: candidate.health_call,
      });
      if (warm.contract_digest !== candidate.expected_contract_digest) {
        throw new Error(
          `mcp_generation_contract_digest_mismatch: expected=${candidate.expected_contract_digest} observed=${warm.contract_digest}`,
        );
      }
      generation.health = 'healthy';
      this.touch(generation);
      return generation;
    } catch (error) {
      generation.state = 'failed';
      generation.health = 'unreachable';
      generation.failure = describeUnknownError(error, 'mcp_generation_warmup_failed');
      this.emit('failed', generation);
      if (generation.handle !== null) {
        await candidate.adapter.terminate(generation.handle).catch(() => undefined);
      }
      this.resolveRetirement(generation, 'warmup_failed');
      throw error;
    }
  }

  private activate(replacement: InternalGeneration, old: InternalGeneration | null): void {
    replacement.state = 'active';
    replacement.activatedAt = Date.now();
    this.touch(replacement);
    this.activeId = replacement.candidate.generation_id;
    this.emit('active', replacement);
    if (old === null) return;
    old.state = 'draining';
    const drainTimeout = this.options.drain_timeout_ms ?? 5_000;
    old.drainDeadline = Date.now() + drainTimeout;
    this.emit('draining', old);
    if (old.candidate.adapter.transport === 'stdio' && old.inflight === 0) {
      void this.terminateGeneration(old, 'stdio_drain_completed');
      return;
    }
    old.drainTimer = setTimeout(() => {
      void this.terminateGeneration(old, 'drain_timeout');
    }, drainTimeout);
    old.drainTimer.unref?.();
  }

  private async terminateGeneration(generation: InternalGeneration, reason: string): Promise<void> {
    if (generation.state === 'terminated') return;
    if (generation.drainTimer) clearTimeout(generation.drainTimer);
    generation.drainTimer = null;
    generation.state = 'terminated';
    generation.health = 'unreachable';
    generation.freshness = 'stale';
    this.emit('terminated', generation);
    this.resolveRetirement(generation, reason);
    if (generation.handle !== null) {
      await generation.candidate.adapter.terminate(generation.handle).catch(() => undefined);
    }
  }

  private resolveRetirement(generation: InternalGeneration, reason: string): void {
    if (generation.retire.settled) return;
    generation.retire.settled = true;
    generation.retire.resolve(retiredError(generation, reason));
  }

  private touch(generation: InternalGeneration): void {
    generation.heartbeatAt = Date.now();
    generation.leaseExpiresAt = generation.heartbeatAt + (this.options.lease_ms ?? 30_000);
    this.emit('heartbeat', generation);
  }

  private emit(event: GenerationObservationEvent['event'], generation: InternalGeneration): void {
    if (!this.options.observation_sink) return;
    try {
      this.options.observation_sink({
        event,
        observed_at: new Date().toISOString(),
        logical_connection_id: this.options.logical_connection_id ?? 'generation-manager',
        generation: snapshot(generation),
      });
    } catch {
      // Observation is optional and must not acquire runtime-control authority.
    }
  }
}

export class JsonLineStdioGenerationAdapter implements GenerationAdapter<StdioHandle> {
  readonly transport = 'stdio' as const;

  constructor(private readonly options: {
    descriptor: SurfaceDescriptorV2;
    command: string;
    args?: string[];
    cwd?: string;
    env?: NodeJS.ProcessEnv;
    timeout_ms?: number;
  }) {}

  async start(): Promise<StdioHandle> {
    const child = spawn(this.options.command, this.options.args ?? [], {
      cwd: this.options.cwd,
      env: { ...process.env, ...this.options.env },
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
      detached: process.platform !== 'win32',
    });
    const handle: StdioHandle = {
      child,
      pending: new Map(),
      nextId: 1,
      buffer: '',
      closed: false,
    };
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk: string) => this.onData(handle, chunk));
    child.once('exit', (code, signal) => {
      handle.closed = true;
      for (const pending of handle.pending.values()) {
        clearTimeout(pending.timer);
        pending.reject(new Error(`mcp_generation_child_exited: code=${code} signal=${signal}`));
      }
      handle.pending.clear();
    });
    return handle;
  }

  async warm(
    handle: StdioHandle,
    input: {
      initialization: JsonRecord;
      expected_contract_digest: string;
      health_call?: GenerationHealthCall;
    },
  ): Promise<{ contract_digest: string; tools: McpToolDefinition[] }> {
    await this.request(handle, 'initialize', input.initialization);
    this.notify(handle, 'notifications/initialized', {});
    const list = await this.request(handle, 'tools/list', {});
    const tools = Array.isArray(list.tools) ? list.tools as McpToolDefinition[] : [];
    assertLiveToolsConform(this.options.descriptor, tools);
    if (input.health_call) {
      const health = await this.request(handle, 'tools/call', {
        name: input.health_call.name,
        arguments: input.health_call.arguments ?? {},
      });
      if (health.isError === true) throw new Error('mcp_generation_health_call_failed');
    }
    return {
      contract_digest: liveToolsContractDigest(this.options.descriptor, tools),
      tools,
    };
  }

  dispatch(handle: StdioHandle, request: JsonRecord): Promise<JsonRecord> {
    return this.request(
      handle,
      String(request.method ?? 'tools/call'),
      isRecord(request.params) ? request.params : {},
    ).then((result) => {
      if (isModernProtocolRequest(request) && typeof result.resultType !== 'string') {
        throw new Error('mcp_modern_result_type_missing');
      }
      return result;
    });
  }

  async terminate(handle: StdioHandle): Promise<void> {
    if (handle.closed) return;
    await terminateProcessTree(handle.child);
  }

  private request(handle: StdioHandle, method: string, params: JsonRecord): Promise<JsonRecord> {
    if (handle.closed) return Promise.reject(new Error('mcp_generation_child_closed'));
    const id = handle.nextId++;
    return new Promise<JsonRecord>((resolve, reject) => {
      const timer = setTimeout(() => {
        handle.pending.delete(id);
        reject(new Error(`mcp_generation_request_timeout: ${method}`));
      }, this.options.timeout_ms ?? 5_000);
      handle.pending.set(id, { resolve, reject, timer });
      handle.child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
    });
  }

  private notify(handle: StdioHandle, method: string, params: JsonRecord): void {
    handle.child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method, params })}\n`);
  }

  private onData(handle: StdioHandle, chunk: string): void {
    handle.buffer += chunk;
    const lines = handle.buffer.split(/\r?\n/);
    handle.buffer = lines.pop() ?? '';
    for (const line of lines) {
      if (!line.trim()) continue;
      let message: JsonRecord;
      try {
        message = JSON.parse(line) as JsonRecord;
      } catch {
        continue;
      }
      const id = Number(message.id);
      const pending = handle.pending.get(id);
      if (!pending) continue;
      handle.pending.delete(id);
      clearTimeout(pending.timer);
      if (isRecord(message.error)) {
        pending.reject(new Error(describeUnknownError(message.error, 'mcp_generation_child_error')));
      } else {
        pending.resolve(isRecord(message.result) ? message.result : {});
      }
    }
  }
}

type StdioHandle = {
  child: ChildProcessWithoutNullStreams;
  pending: Map<number, {
    resolve(value: JsonRecord): void;
    reject(error: Error): void;
    timer: ReturnType<typeof setTimeout>;
  }>;
  nextId: number;
  buffer: string;
  closed: boolean;
};

export class StreamableHttpGenerationAdapter implements GenerationAdapter<{ url: string }> {
  readonly transport = 'streamable_http' as const;

  constructor(private readonly options: {
    descriptor: SurfaceDescriptorV2;
    url: string;
    headers?: Record<string, string>;
    timeout_ms?: number;
    terminate?: () => Promise<void>;
  }) {}

  async start(): Promise<{ url: string }> {
    return { url: this.options.url };
  }

  async warm(
    handle: { url: string },
    input: {
      initialization: JsonRecord;
      expected_contract_digest: string;
      health_call?: GenerationHealthCall;
    },
  ): Promise<{ contract_digest: string; tools: McpToolDefinition[] }> {
    await this.request(handle, { jsonrpc: '2.0', id: 1, method: 'initialize', params: input.initialization }, null);
    const listed = await this.request(
      handle,
      { jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} },
      null,
    );
    const result = isRecord(listed.result) ? listed.result : {};
    const tools = Array.isArray(result.tools) ? result.tools as McpToolDefinition[] : [];
    assertLiveToolsConform(this.options.descriptor, tools);
    if (input.health_call) {
      const health = await this.request(handle, {
        jsonrpc: '2.0',
        id: 3,
        method: 'tools/call',
        params: {
          name: input.health_call.name,
          arguments: input.health_call.arguments ?? {},
        },
      }, null);
      if (health.error !== undefined || (isRecord(health.result) && health.result.isError === true)) {
        throw new Error('mcp_generation_health_call_failed');
      }
    }
    return {
      contract_digest: liveToolsContractDigest(this.options.descriptor, tools),
      tools,
    };
  }

  async dispatch(
    handle: { url: string },
    request: JsonRecord,
    context: { session_id: string | null },
  ): Promise<JsonRecord> {
    const response = await this.request(handle, request, context.session_id);
    if (isRecord(response.error)) {
      throw new Error(describeUnknownError(response.error, 'mcp_http_generation_error'));
    }
    const result = isRecord(response.result) ? response.result : response;
    if (isModernProtocolRequest(request) && typeof result.resultType !== 'string') {
      throw new Error('mcp_modern_result_type_missing');
    }
    return result;
  }

  async terminate(): Promise<void> {
    await this.options.terminate?.();
  }

  private async request(
    handle: { url: string },
    message: JsonRecord,
    sessionId: string | null,
  ): Promise<JsonRecord> {
    const modern = isModernProtocolRequest(message);
    const method = typeof message.method === 'string' ? message.method : '';
    const params = isRecord(message.params) ? message.params : {};
    const name = method === 'tools/call' && typeof params.name === 'string'
      ? params.name
      : method;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.options.timeout_ms ?? 5_000);
    try {
      const response = await fetch(handle.url, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          ...this.options.headers,
          ...(modern
            ? {
                'MCP-Protocol-Version': MODERN_PROTOCOL_VERSION,
                'Mcp-Method': method,
                'Mcp-Name': name,
              }
            : sessionId === null ? {} : { 'mcp-session-id': sessionId }),
        },
        body: JSON.stringify(message),
        signal: controller.signal,
      });
      if (!response.ok) throw new Error(`mcp_http_generation_status: ${response.status}`);
      return await response.json() as JsonRecord;
    } finally {
      clearTimeout(timer);
    }
  }
}

export async function startStableHttpGenerationEndpoint(
  manager: GenerationManager,
): Promise<{ url: string; close(): Promise<void> }> {
  const server = createServer((request, response) => {
    void (async () => {
      if (request.method !== 'POST' || request.url !== '/mcp') {
        response.writeHead(404).end();
        return;
      }
      const chunks: Buffer[] = [];
      for await (const chunk of request) chunks.push(Buffer.from(chunk));
      const message = JSON.parse(Buffer.concat(chunks).toString('utf8')) as JsonRecord;
      const sessionHeader = isModernProtocolRequest(message) ? undefined : request.headers['mcp-session-id'];
      const sessionId = Array.isArray(sessionHeader) ? sessionHeader[0] : sessionHeader;
      const routed = await manager.route(message, { session_id: sessionId });
      if (routed.session_id) response.setHeader('mcp-session-id', routed.session_id);
      response.setHeader('content-type', 'application/json');
      response.end(JSON.stringify(
        routed.status === 'ok'
          ? routed.response
          : { jsonrpc: '2.0', id: message.id ?? null, error: routed.error },
      ));
    })().catch((error) => {
      response.writeHead(500, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ error: error instanceof Error ? error.message : String(error) }));
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => resolve());
  });
  return {
    url: serverUrl(server),
    close: () => new Promise<void>((resolve, reject) =>
      server.close((error) => error ? reject(error) : resolve())),
  };
}

function retiredRouteResult(
  generation: InternalGeneration,
  sessionId: string | null,
): RouteResult {
  return {
    status: 'session_generation_retired',
    generation_id: generation.candidate.generation_id,
    session_id: sessionId,
    error: retiredError(generation, 'already_retired'),
  };
}

function isModernProtocolRequest(message: JsonRecord): boolean {
  const params = isRecord(message.params) ? message.params : {};
  const meta = isRecord(params._meta) ? params._meta : {};
  return meta[MODERN_PROTOCOL_VERSION_META] === MODERN_PROTOCOL_VERSION;
}

function retiredError(generation: InternalGeneration, reason: string): JsonRecord {
  return {
    code: 'session_generation_retired',
    generation_id: generation.candidate.generation_id,
    reason,
    recovery: generation.candidate.adapter.transport === 'streamable_http'
      ? 'Open a new MCP HTTP session without the retired mcp-session-id, then initialize and repeat tools/list.'
      : 'Reconnect to the stable logical stdio endpoint, initialize, and repeat tools/list.',
  };
}

function snapshot(generation: InternalGeneration): GenerationSnapshot {
  return {
    generation_id: generation.candidate.generation_id,
    state: generation.state,
    transport: generation.candidate.adapter.transport,
    inflight: generation.inflight,
    started_at: new Date(generation.startedAt).toISOString(),
    activated_at: generation.activatedAt === null
      ? null
      : new Date(generation.activatedAt).toISOString(),
    drain_deadline: generation.drainDeadline === null
      ? null
      : new Date(generation.drainDeadline).toISOString(),
    heartbeat_at: new Date(generation.heartbeatAt).toISOString(),
    lease_expires_at: new Date(generation.leaseExpiresAt).toISOString(),
    freshness: generation.freshness,
    health: generation.health,
    descriptor_digest: generation.candidate.descriptor_digest ?? null,
    tool_contract_digest: generation.candidate.expected_contract_digest,
    failure: generation.failure,
  };
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve(value: T): void;
  settled: boolean;
} {
  let resolvePromise!: (value: T) => void;
  const result = {
    promise: new Promise<T>((resolve) => { resolvePromise = resolve; }),
    resolve(value: T) { resolvePromise(value); },
    settled: false,
  };
  return result;
}

async function terminateProcessTree(child: ChildProcessWithoutNullStreams): Promise<void> {
  if (child.exitCode !== null || child.pid === undefined) return;
  if (process.platform === 'win32') {
    await new Promise<void>((resolve) => {
      const killer = spawn('taskkill', ['/PID', String(child.pid), '/T', '/F'], {
        stdio: 'ignore',
        windowsHide: true,
      });
      killer.once('exit', () => resolve());
      killer.once('error', () => {
        child.kill();
        resolve();
      });
    });
    return;
  }
  try {
    process.kill(-child.pid, 'SIGTERM');
  } catch {
    child.kill('SIGTERM');
  }
}

function serverUrl(server: Server): string {
  const address = server.address();
  if (address === null || typeof address === 'string') {
    throw new Error('mcp_generation_http_endpoint_address_unavailable');
  }
  return `http://127.0.0.1:${address.port}/mcp`;
}

function isRecord(value: unknown): value is JsonRecord {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
