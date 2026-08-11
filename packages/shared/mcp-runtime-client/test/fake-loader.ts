#!/usr/bin/env node
import { createInterface } from 'node:readline';

type JsonRecord = Record<string, unknown>;

let attachCount = 0;
let responseCount = 0;
let outputCount = 0;
const materializedOutputs = new Map<string, JsonRecord>();
const incompleteOutputRefs = new Set<string>();

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  if (!line.trim()) continue;
  const request = JSON.parse(line) as JsonRecord;
  if (typeof request.id !== 'number') continue;
  const method = String(request.method ?? '');
  const params = record(request.params);
  if (method === 'initialize') {
    respond(request.id, { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'fake-loader', version: '1' } });
    continue;
  }
  if (method !== 'tools/call') {
    respond(request.id, {}, { code: -32601, message: `unknown_method:${method}` });
    continue;
  }
  const name = String(params.name ?? '');
  const args = record(params.arguments);
  if (name === 'mcp_loader_attach_surface') {
    attachCount += 1;
    respond(request.id, toolResult({
      schema: 'narada.mcp_loader.surface_attached.v1',
      connection_id: `connection-${String(args.surface_id)}`,
      surface_id: args.surface_id,
      attach_count: attachCount,
    }));
    continue;
  }
  if (name === 'mcp_loader_detach') {
    const delayMs = Number(process.env.FAKE_LOADER_DETACH_DELAY_MS ?? 0);
    const finish = () => respond(
      request.id,
      toolResult({ schema: 'narada.mcp_loader.detached.v1', status: 'detached' }),
    );
    if (Number.isSafeInteger(delayMs) && delayMs > 0) setTimeout(finish, delayMs);
    else finish();
    continue;
  }
  if (name === 'mcp_loader_read_result') {
    const ref = String(args.ref ?? args.output_ref ?? '');
    const value = materializedOutputs.get(ref);
    if (!value) {
      respond(request.id, toolResult({ schema: 'fake.output.error.v1', status: 'error' }, true));
      continue;
    }
    const fullText = JSON.stringify(value, null, 2);
    const offset = integer(args.offset, 0);
    const limit = integer(args.limit, 20_000);
    const outputText = fullText.slice(offset, offset + limit);
    const nextOffset = offset + outputText.length < fullText.length ? offset + outputText.length : null;
    const connectionId = String(args.connection_id ?? '');
    respond(request.id, toolResult({
      schema: 'narada.mcp_loader.result_page.v1',
      connection_id: connectionId,
      surface_id: connectionId.replace(/^connection-/, ''),
      result: {
        schema: 'narada.mcp_output_page.v1',
        status: 'ok',
        ref,
        offset,
        limit,
        next_offset: nextOffset,
        output_text: outputText,
        output_truncated: nextOffset !== null,
        full_output_char_length: incompleteOutputRefs.has(ref) ? fullText.length + 1 : fullText.length,
      },
    }));
    continue;
  }
  if (name !== 'mcp_loader_call_tool') {
    respond(request.id, toolResult({ schema: 'fake.unknown.v1' }, true));
    continue;
  }
  const childTool = String(args.tool_name ?? '');
  if (childTool === 'hang') continue;
  if (childTool === 'materialized') {
    const domainRef = materialize({
      schema: 'fake.materialized.v1',
      kind: 'double',
      payload: 'm'.repeat(24_000),
    });
    const childResult = toolResult(outputPage(domainRef));
    const loaderRef = materialize(childResult);
    respond(request.id, toolResult({
      schema: 'narada.mcp_loader.tool_result.v1',
      result_bounded: true,
      details_ref: loaderRef,
      result: outputPage(loaderRef, 'mcp_loader_read_result'),
    }));
    continue;
  }
  if (childTool === 'outer-materialized') {
    const loaderRef = materialize(toolResult({ schema: 'fake.materialized.v1', kind: 'outer' }));
    respond(request.id, toolResult({
      schema: 'narada.mcp_loader.tool_result.v1',
      result_bounded: true,
      details_ref: loaderRef,
      result: outputPage(loaderRef, 'mcp_loader_read_result'),
    }));
    continue;
  }
  if (childTool === 'nested-envelope') {
    const domainRef = materialize({
      schema: 'narada.domain_operation.v1',
      operation_ref: 'nested-domain:1',
      outcome: 'completed',
      result: { receipt_id: 'nested-receipt-1' },
    });
    const innerPageRef = materialize(toolResult(outputPage(domainRef)));
    const outerPageRef = materialize(toolResult(outputPage(innerPageRef)));
    respond(request.id, toolResult({
      schema: 'narada.mcp_loader.tool_result.v1',
      result_bounded: true,
      details_ref: outerPageRef,
      result: outputPage(outerPageRef, 'mcp_loader_read_result'),
    }));
    continue;
  }
  if (childTool === 'incomplete') {
    const domainRef = materialize({
      schema: 'fake.materialized.v1',
      kind: 'incomplete',
      payload: 'i'.repeat(4_000),
    });
    incompleteOutputRefs.add(domainRef);
    respond(request.id, toolResult({
      schema: 'narada.mcp_loader.tool_result.v1',
      result_bounded: false,
      result: toolResult(outputPage(domainRef)),
    }));
    continue;
  }
  if (childTool === 'nested-materialized' || childTool === 'too-large') {
    const domainRef = materialize({
      schema: 'fake.materialized.v1',
      kind: childTool,
      payload: childTool === 'too-large' ? 'x'.repeat(40_000) : 'n'.repeat(24_000),
    });
    respond(request.id, toolResult({
      schema: 'narada.mcp_loader.tool_result.v1',
      result_bounded: false,
      result: toolResult(outputPage(domainRef)),
    }));
    continue;
  }
  const childResult = childTool === 'fail'
    ? toolResult({ schema: 'fake.failure.v1', status: 'error' }, true)
    : toolResult({ schema: 'fake.echo.v1', arguments: args.arguments, attach_count: attachCount });
  respond(request.id, toolResult({
    schema: 'narada.mcp_loader.tool_result.v1',
    result_bounded: false,
    result: childResult,
  }));
}

function materialize(value: JsonRecord): string {
  const ref = `mcp_output:fake-${++outputCount}`;
  materializedOutputs.set(ref, value);
  return ref;
}

function outputPage(ref: string, readerTool = 'fake_output_show'): JsonRecord {
  return {
    schema: 'narada.producer_output_page.v1',
    status: 'ok',
    truncated: true,
    output_ref: ref,
    ref,
    result_materialized: true,
    reader_tool: readerTool,
  };
}

function integer(value: unknown, fallback: number): number {
  return Number.isSafeInteger(value) && (value as number) >= 0 ? value as number : fallback;
}

function toolResult(structuredContent: JsonRecord, isError = false): JsonRecord {
  return {
    content: [{ type: 'text', text: JSON.stringify(structuredContent) }],
    structuredContent,
    ...(isError ? { isError: true } : {}),
  };
}

function respond(id: unknown, result: JsonRecord, error?: JsonRecord): void {
  const body = JSON.stringify({ jsonrpc: '2.0', id, ...(error ? { error } : { result }) });
  responseCount += 1;
  if (responseCount % 2 === 0) {
    process.stdout.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\r\n\r\n${body}`);
  } else {
    process.stdout.write(`${body}\n`);
  }
}

function record(value: unknown): JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value) ? value as JsonRecord : {};
}
