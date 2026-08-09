import {
  DEFAULT_LEGACY_PROTOCOL_VERSION,
  MODERN_PROTOCOL_VERSION,
  withModernRequestMeta,
  type McpProtocolEra,
  type McpProtocolClientOptions,
} from '@narada-core/mcp-protocol';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';

export type JsonRecord = Record<string, unknown>;

export interface McpProcessClientOptions {
  executable: string;
  args?: readonly string[];
  clientCapabilities?: JsonRecord;
  protocolMode?: 'auto' | 'modern' | 'legacy';
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  requestTimeoutMs?: number;
  closeTimeoutMs?: number;
  maxResponseBytes?: number;
  stderrTailBytes?: number;
  clientName?: string;
}

interface PendingRequest {
  method: string;
  resolve: (value: JsonRecord) => void;
  reject: (error: Error) => void;
  timeout: NodeJS.Timeout;
}

const DEFAULT_REQUEST_TIMEOUT_MS = 30_000;
const DEFAULT_CLOSE_TIMEOUT_MS = 5_000;
const DEFAULT_MAX_RESPONSE_BYTES = 4 * 1024 * 1024;
const DEFAULT_STDERR_TAIL_BYTES = 8_000;
const MAX_HEADER_BYTES = 16 * 1024;

export class McpProcessClient {
  readonly process: ChildProcessWithoutNullStreams;
  readonly requestTimeoutMs: number;
  readonly closeTimeoutMs: number;
  readonly maxResponseBytes: number;
  readonly stderrTailBytes: number;

  #nextId = 1;
  #pending = new Map<number, PendingRequest>();
  #stdoutBuffer = Buffer.alloc(0);
  #stderrTail = '';
  #closed = false;
  #failure: Error | null = null;
  #protocolEra: McpProtocolEra = 'legacy';
  #protocolMode: 'auto' | 'modern' | 'legacy' = 'auto';
  #clientProtocolOptions: McpProtocolClientOptions;

  private constructor(options: McpProcessClientOptions) {
    this.requestTimeoutMs = positiveInteger(options.requestTimeoutMs, DEFAULT_REQUEST_TIMEOUT_MS, 'requestTimeoutMs');
    this.closeTimeoutMs = positiveInteger(options.closeTimeoutMs, DEFAULT_CLOSE_TIMEOUT_MS, 'closeTimeoutMs');
    this.maxResponseBytes = positiveInteger(options.maxResponseBytes, DEFAULT_MAX_RESPONSE_BYTES, 'maxResponseBytes');
    this.stderrTailBytes = positiveInteger(options.stderrTailBytes, DEFAULT_STDERR_TAIL_BYTES, 'stderrTailBytes');
    this.#protocolMode = options.protocolMode ?? 'auto';
    this.#clientProtocolOptions = {
      clientInfo: { name: options.clientName ?? 'narada-mcp-runtime-client', version: '0.1.0' },
      clientCapabilities: options.clientCapabilities ?? {},
    };

    this.process = spawn(options.executable, [...(options.args ?? [])], {
      cwd: options.cwd,
      env: options.env,
      stdio: ['pipe', 'pipe', 'pipe'],
      shell: false,
      windowsHide: true,
    });
    this.process.stdout.on('data', (chunk: Buffer | string) => this.#handleStdout(Buffer.from(chunk)));
    this.process.stderr.setEncoding('utf8');
    this.process.stderr.on('data', (chunk: string) => {
      this.#stderrTail = tailUtf8(`${this.#stderrTail}${chunk}`, this.stderrTailBytes);
    });
    this.process.on('error', (error) => this.#fail(error));
    this.process.on('exit', (code, signal) => {
      if (!this.#closed) {
        this.#fail(new Error(`mcp_process_exited:${code ?? 'null'}:${signal ?? 'null'}${this.#diagnosticSuffix()}`));
      }
    });
  }

  static async start(options: McpProcessClientOptions): Promise<McpProcessClient> {
    const client = new McpProcessClient(options);
    try {
      if (client.#protocolMode !== 'legacy') {
        try {
          const discovery = await client.request('server/discover', withModernRequestMeta({}, client.#clientProtocolOptions), Math.min(client.requestTimeoutMs, 1_500));
          const supportedVersions = Array.isArray(discovery.supportedVersions) ? discovery.supportedVersions.map(String) : [];
          if (supportedVersions.includes(MODERN_PROTOCOL_VERSION)) {
            client.#protocolEra = 'modern';
            return client;
          }
          if (client.#protocolMode === 'modern') throw new Error('mcp_modern_protocol_not_advertised');
        } catch (error) {
          if (client.#protocolMode === 'modern') throw error;
        }
      }
      await client.request('initialize', {
        protocolVersion: DEFAULT_LEGACY_PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: client.#clientProtocolOptions.clientInfo,
      });
      client.notify('notifications/initialized', {});
      return client;
    } catch (error) {
      await client.close();
      throw error;
    }
  }

  get stderrTail(): string {
    return this.#stderrTail;
  }

  get protocolEra(): McpProtocolEra {
    return this.#protocolEra;
  }

  get protocolVersion(): string {
    return this.#protocolEra === 'modern' ? MODERN_PROTOCOL_VERSION : DEFAULT_LEGACY_PROTOCOL_VERSION;
  }

  async request(method: string, params: JsonRecord = {}, timeoutMs = this.requestTimeoutMs): Promise<JsonRecord> {
    this.#assertOpen();
    const boundedTimeoutMs = positiveInteger(timeoutMs, this.requestTimeoutMs, 'timeoutMs');
    const id = this.#nextId++;
    const wireParams = this.#protocolEra === 'modern' && method !== 'initialize'
      ? withModernRequestMeta(params, this.#clientProtocolOptions)
      : params;
    const message = { jsonrpc: '2.0', id, method, params: wireParams };
    return new Promise<JsonRecord>((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`mcp_request_timeout:${method}:${boundedTimeoutMs}ms${this.#diagnosticSuffix()}`));
      }, boundedTimeoutMs);
      this.#pending.set(id, { method, resolve, reject, timeout });
      try {
        this.#write(message);
      } catch (error) {
        clearTimeout(timeout);
        this.#pending.delete(id);
        reject(toError(error));
      }
    });
  }

  notify(method: string, params: JsonRecord = {}): void {
    this.#assertOpen();
    if (this.#protocolEra === 'modern') return;
    this.#write({ jsonrpc: '2.0', method, params });
  }

  async callTool(name: string, args: JsonRecord = {}, timeoutMs = this.requestTimeoutMs): Promise<JsonRecord> {
    return this.request('tools/call', { name, arguments: args }, timeoutMs);
  }

  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    this.#rejectPending(new Error(`mcp_process_closed${this.#diagnosticSuffix()}`));
    if (this.process.exitCode !== null || this.process.signalCode !== null) return;

    const exited = waitForExit(this.process, this.closeTimeoutMs);
    this.process.stdin.end();
    if (await exited) return;

    const killed = waitForExit(this.process, Math.min(this.closeTimeoutMs, 1_000));
    try {
      this.process.kill('SIGTERM');
    } catch {
      // A concurrent exit is observed by killed.
    }
    if (await killed) return;
    try {
      this.process.kill('SIGKILL');
    } catch {
      // The process has already exited.
    }
  }

  #write(message: JsonRecord): void {
    if (this.process.stdin.destroyed || !this.process.stdin.writable) {
      throw new Error(`mcp_process_stdin_closed${this.#diagnosticSuffix()}`);
    }
    this.process.stdin.write(`${JSON.stringify(message)}\n`);
  }

  #handleStdout(chunk: Buffer): void {
    if (this.#closed || this.#failure) return;
    this.#stdoutBuffer = Buffer.concat([this.#stdoutBuffer, chunk]);
    try {
      this.#drainMessages();
    } catch (error) {
      this.#fail(toError(error));
    }
  }

  #drainMessages(): void {
    while (this.#stdoutBuffer.length > 0) {
      while (this.#stdoutBuffer[0] === 0x0a || this.#stdoutBuffer[0] === 0x0d) {
        this.#stdoutBuffer = this.#stdoutBuffer.subarray(1);
      }
      if (this.#stdoutBuffer.length === 0) return;

      const framed = startsWithContentLength(this.#stdoutBuffer);
      if (framed) {
        const delimiter = findHeaderDelimiter(this.#stdoutBuffer);
        if (!delimiter) {
          if (this.#stdoutBuffer.length > MAX_HEADER_BYTES) throw new Error('mcp_response_header_too_large');
          return;
        }
        const header = this.#stdoutBuffer.subarray(0, delimiter.index).toString('ascii');
        const match = /^Content-Length:\s*(\d+)\s*$/im.exec(header);
        if (!match) throw new Error('mcp_response_content_length_missing');
        const length = Number(match[1]);
        if (!Number.isSafeInteger(length) || length < 0 || length > this.maxResponseBytes) {
          throw new Error(`mcp_response_size_invalid:${length}`);
        }
        const bodyStart = delimiter.index + delimiter.length;
        if (this.#stdoutBuffer.length < bodyStart + length) return;
        const body = this.#stdoutBuffer.subarray(bodyStart, bodyStart + length).toString('utf8');
        this.#stdoutBuffer = this.#stdoutBuffer.subarray(bodyStart + length);
        this.#acceptMessage(body);
        continue;
      }

      const newline = this.#stdoutBuffer.indexOf(0x0a);
      if (newline < 0) {
        if (this.#stdoutBuffer.length > this.maxResponseBytes) throw new Error('mcp_response_line_too_large');
        return;
      }
      const line = this.#stdoutBuffer.subarray(0, newline).toString('utf8').trim();
      this.#stdoutBuffer = this.#stdoutBuffer.subarray(newline + 1);
      if (line) this.#acceptMessage(line);
    }
  }

  #acceptMessage(serialized: string): void {
    let value: unknown;
    try {
      value = JSON.parse(serialized);
    } catch {
      throw new Error('mcp_response_json_invalid');
    }
    const message = asRecord(value);
    const id = typeof message.id === 'number' ? message.id : Number.NaN;
    if (!Number.isSafeInteger(id)) return;
    const pending = this.#pending.get(id);
    if (!pending) return;
    this.#pending.delete(id);
    clearTimeout(pending.timeout);
    if (message.error !== undefined) {
      const error = asRecord(message.error);
      pending.reject(new Error(`mcp_request_failed:${pending.method}:${String(error.code ?? 'unknown')}:${String(error.message ?? 'unknown')}${this.#diagnosticSuffix()}`));
      return;
    }
    const result = asRecord(message.result);
    if (this.#protocolEra === 'modern' && typeof result.resultType !== 'string') {
      pending.reject(new Error(`mcp_modern_result_type_missing:${pending.method}`));
      return;
    }
    pending.resolve(result);
  }

  #assertOpen(): void {
    if (this.#failure) throw this.#failure;
    if (this.#closed) throw new Error(`mcp_process_closed${this.#diagnosticSuffix()}`);
  }

  #fail(error: Error): void {
    if (this.#failure || this.#closed) return;
    this.#failure = new Error(`${error.message}${error.message.includes('stderr_tail=') ? '' : this.#diagnosticSuffix()}`);
    this.#rejectPending(this.#failure);
  }

  #rejectPending(error: Error): void {
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(error);
    }
    this.#pending.clear();
  }

  #diagnosticSuffix(): string {
    const stderr = this.#stderrTail.trim();
    return stderr ? `:stderr_tail=${JSON.stringify(stderr)}` : '';
  }
}

export function unwrapToolCallResult(result: JsonRecord): JsonRecord {
  if (result.isError === true) throw new Error(`mcp_tool_error:${toolText(result) || 'unknown'}`);
  const structured = result.structuredContent;
  if (isRecord(structured)) return structured;
  const text = toolText(result);
  if (!text) return {};
  try {
    return asRecord(JSON.parse(text));
  } catch {
    return { text };
  }
}

export function toolText(result: JsonRecord): string {
  if (!Array.isArray(result.content)) return '';
  return result.content
    .map((item) => isRecord(item) && item.type === 'text' && typeof item.text === 'string' ? item.text : '')
    .filter(Boolean)
    .join('\n');
}

export function isRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function asRecord(value: unknown): JsonRecord {
  return isRecord(value) ? value : {};
}

function startsWithContentLength(buffer: Buffer): boolean {
  return buffer.subarray(0, Math.min(buffer.length, 15)).toString('ascii').toLowerCase().startsWith('content-length:');
}

function findHeaderDelimiter(buffer: Buffer): { index: number; length: number } | null {
  const crlf = buffer.indexOf(Buffer.from('\r\n\r\n'));
  if (crlf >= 0) return { index: crlf, length: 4 };
  const lf = buffer.indexOf(Buffer.from('\n\n'));
  return lf >= 0 ? { index: lf, length: 2 } : null;
}

function positiveInteger(value: number | undefined, fallback: number, name: string): number {
  const resolved = value ?? fallback;
  if (!Number.isSafeInteger(resolved) || resolved <= 0) throw new Error(`${name}_must_be_positive_integer`);
  return resolved;
}

function tailUtf8(value: string, maxBytes: number): string {
  const bytes = Buffer.from(value, 'utf8');
  return bytes.length <= maxBytes ? value : bytes.subarray(bytes.length - maxBytes).toString('utf8');
}

function toError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

function waitForExit(process: ChildProcessWithoutNullStreams, timeoutMs: number): Promise<boolean> {
  if (process.exitCode !== null || process.signalCode !== null) return Promise.resolve(true);
  return new Promise((resolve) => {
    let settled = false;
    const finish = (value: boolean) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      process.off('exit', onExit);
      resolve(value);
    };
    const onExit = () => finish(true);
    const timeout = setTimeout(() => finish(false), timeoutMs);
    process.once('exit', onExit);
  });
}
