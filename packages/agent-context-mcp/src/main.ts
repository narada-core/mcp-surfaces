#!/usr/bin/env node

import { buildGuidanceResult } from './guidance.js';
import {
  listAgentContextTools,
  ORIENTATION_NEXT_CALL_JSON_SCHEMA,
  ORIENTATION_REQUIRED_READ_PROGRESS_JSON_SCHEMA,
  type AgentContextToolProjection,
} from './tool-catalog.js';
/**
 * Site-local agent-context MCP server.
 *
 * This is the minimum checkpoint/hydration slice admitted from the
 * agent-context checkpointing lift package. It intentionally avoids importing
 * andrey-user runtime state or broad User Site surfaces.
 */

import { createHash, createHmac, randomUUID } from 'node:crypto';
import { appendFileSync, existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  buildBoundedToolResult,
  listOutputResources,
  outputShowAsync,
  readOutputResource,
} from '@narada-core/mcp-transport';
import { boundedCollection } from '@narada-core/mcp-transport/bounded-collection';
import {
  buildOrientationOccupantBrief,
  buildOrientationReadyProjection,
  parseCarrierSessionActivationReceipt,
  parseCarrierSessionAdmissionReceipt,
  parseCarrierSessionOrientationDeliveryReceipt,
} from '@narada-core/orientation-manifest';
import {
  listAgentStartSessions,
  materializeAgentSessionStart,
  openAgentContextDb,
  projectOrientationAcknowledgement,
  readOrientationEntryPacket,
  readOrientationManifestGeneration,
  recordOrientationAcknowledgement,
  recordOrientationRequiredRead,
  validateIdentityAgainstRoster,
} from './session-start.js';
import {
  assertAdmissionMatchesAgentContext,
  compileAgentContextOrientation,
  orientationEvidenceFromEnvironment,
  resolveAgentContextLawPath,
} from './orientation-manifest.js';
import { continuationProjectionState } from './continuation-projection.js';

const SERVER_VERSION = '0.1.0';
const PROTOCOL_VERSION = '2026-04-18';
const activeRequests = new Map();

const args = parseArgs(process.argv.slice(2));
const siteRoot = resolve(args['site-root'] ?? process.cwd());
const siteId = normalizeSiteId(args['site-id'] ?? process.env.NARADA_SITE_ID ?? deriveSiteId(siteRoot));
const SERVER_NAME = `${siteId.replace(/[^a-z0-9_.-]/gi, '-')}-agent-context-mcp`;
const dbPath = resolve(process.env.NARADA_AGENT_CONTEXT_DB || join(siteRoot, '.ai', 'state', 'agent-context.sqlite'));
const MAX_CONTINUATION_BYTES = 256 * 1024;
const MAX_CONTINUATION_STATE_BYTES = 64 * 1024;
const MAX_CONTINUATION_TEXT_LENGTH = 16 * 1024;
const MAX_CONTINUATION_ARRAY_ITEMS = 200;
const ORIENTATION_INLINE_CONTENT_BYTES = 6_000;
const ORIENTATION_INLINE_RESPONSE_BYTES = 32 * 1024;
const startupTracePath = join(siteRoot, '.ai', 'tmp', 'agent-context-mcp-startup.log');
const startupTraceEnabled = process.env.NARADA_AGENT_CONTEXT_MCP_TRACE === '1';

function traceStartup(event: any, extra: any = {}) {
  if (!startupTraceEnabled) return;
  try {
    mkdirSync(join(siteRoot, '.ai', 'tmp'), { recursive: true });
    appendFileSync(startupTracePath, `${JSON.stringify({
      at: new Date().toISOString(),
      event,
      pid: process.pid,
      ppid: process.ppid,
      argv: process.argv,
      cwd: process.cwd(),
      execPath: process.execPath,
      siteRoot,
      dbPath,
      agentId: process.env.NARADA_AGENT_ID ?? null,
      carrierSessionId: process.env.NARADA_CARRIER_SESSION_ID ?? null,
      ...extra,
    })}\n`);
  } catch {
    // Startup tracing must never interfere with MCP stdio.
  }
}

const ORIENTATION_MANIFEST_RESOURCE_PREFIX =
  'narada-agent-context://orientation-manifest/';

function orientationManifestResourceUri(manifestId: string) {
  return ORIENTATION_MANIFEST_RESOURCE_PREFIX + encodeURIComponent(manifestId);
}

function listResources() {
  if (currentToolProjection() === 'occupant') return { resources: [] };
  const outputResources: any = listOutputResources({ siteRoot });
  const manifestId: any = exactOrientationManifestId();
  const manifestResources: any[] = manifestId
    ? [{
        uri: orientationManifestResourceUri(manifestId),
        name: `Orientation Manifest ${manifestId}`,
        title: 'Exact Carrier-entry Orientation Manifest',
        description: 'Canonical immutable manifest bound to this admitted Carrier Session.',
        mimeType: 'application/json',
      }]
    : [];
  return {
    ...outputResources,
    resources: [...manifestResources, ...(outputResources.resources ?? [])],
  };
}

function readResource(uriValue: any) {
  if (currentToolProjection() === 'occupant') {
    throw new Error('agent_context_resources_not_exposed_in_occupant_projection');
  }
  const uri: any = String(uriValue ?? '');
  if (!uri.startsWith(ORIENTATION_MANIFEST_RESOURCE_PREFIX)) {
    return readOutputResource({ siteRoot, uri });
  }
  const requestedManifestId: any = decodeURIComponent(
    uri.slice(ORIENTATION_MANIFEST_RESOURCE_PREFIX.length),
  );
  const evidence: any = exactOrientationEntryEvidence();
  if (requestedManifestId !== evidence.manifestId) {
    throw new Error(
      `agent_context_orientation_manifest_resource_not_current:${requestedManifestId}`,
    );
  }
  const readback: any = readOrientationManifestGeneration({
    siteRoot,
    dbPath,
    manifestId: requestedManifestId,
    admissionReceipt: evidence.admission,
  });
  return {
    contents: [{
      uri,
      mimeType: 'application/json',
      text: JSON.stringify(readback.manifest, null, 2),
    }],
  };
}

process.on('uncaughtException', (error: any) => {
  traceStartup('uncaughtException', { error: error?.stack ?? String(error) });
  throw error;
});

process.on('unhandledRejection', (error: any) => {
  traceStartup('unhandledRejection', { error: error?.stack ?? String(error) });
});

traceStartup('process_start');

function currentToolProjection(): AgentContextToolProjection {
  const selected = String(
    args['tool-projection']
      ?? process.env.NARADA_AGENT_CONTEXT_TOOL_PROJECTION
      ?? 'occupant',
  ).trim();
  if (selected !== 'occupant' && selected !== 'admin') {
    throw new Error(`agent_context_tool_projection_invalid:${selected}`);
  }
  return selected;
}

function checkpointRowForAgent(db: any, agentId: any, checkpointId: any) {
  if (checkpointId !== null) {
    const current = db.prepare(`
      SELECT * FROM agent_checkpoints
      WHERE agent_id = ? AND checkpoint_id = ?
      LIMIT 1
    `).get(agentId, checkpointId);
    if (current) return current;
    return db.prepare(`
      SELECT * FROM agent_checkpoint_history
      WHERE agent_id = ? AND checkpoint_id = ?
      ORDER BY archived_at DESC
      LIMIT 1
    `).get(agentId, checkpointId);
  }

  return db.prepare('SELECT * FROM agent_checkpoints WHERE agent_id = ? ORDER BY checkpoint_at DESC LIMIT 1').get(agentId);
}

function normalizeContinuation(value: any, checkpointId: any, checkpointAt: any) {
  if (value == null) return null;
  if (typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('continuation_invalid: expected an object');
  }

  const allowedKeys = new Set([
    'schema',
    'continuation_id',
    'objective',
    'current_state',
    'completed_work',
    'decisions',
    'evidence_refs',
    'open_blockers',
    'next_action',
    'canonical_sources',
    'constraints',
    'resume_mode',
    'created_at',
  ]);
  for (const key of Object.keys(value)) {
    if (!allowedKeys.has(key)) throw new Error(`continuation_field_unknown: ${key}`);
  }

  if (value.schema !== 'narada.continuation.v1') {
    throw new Error('continuation_schema_invalid');
  }

  const continuationId = value.continuation_id == null
    ? `cont_${randomUUID().replace(/-/g, '')}`
    : continuationText(value.continuation_id, 'continuation_id');
  const objective = continuationText(value.objective, 'objective', true);
  const currentState = continuationText(value.current_state, 'current_state', true);
  const resumeMode = value.resume_mode ?? 'fresh_session';
  if (resumeMode !== 'fresh_session' && resumeMode !== 'same_session') {
    throw new Error('continuation_resume_mode_invalid');
  }

  const createdAt = value.created_at == null
    ? checkpointAt
    : normalizeContinuationTimestamp(value.created_at);
  const canonical = {
    schema: 'narada.continuation.v1',
    continuation_id: continuationId,
    objective,
    current_state: currentState,
    completed_work: continuationStringArray(value.completed_work, 'completed_work'),
    decisions: continuationStringArray(value.decisions, 'decisions'),
    evidence_refs: continuationStringArray(value.evidence_refs, 'evidence_refs'),
    open_blockers: continuationStringArray(value.open_blockers, 'open_blockers'),
    next_action: continuationText(value.next_action, 'next_action'),
    canonical_sources: continuationStringArray(value.canonical_sources, 'canonical_sources'),
    constraints: continuationStringArray(value.constraints, 'constraints'),
    resume_mode: resumeMode,
    source_checkpoint_ref: `agent_context_checkpoint:${checkpointId}`,
    created_at: createdAt,
  };
  const content: Record<string, any> = { ...canonical };
  delete content.source_checkpoint_ref;
  const serialized = JSON.stringify(content);
  if (Buffer.byteLength(JSON.stringify(canonical), 'utf8') > MAX_CONTINUATION_STATE_BYTES) {
    throw new Error('continuation_too_large');
  }

  return {
    ...canonical,
    content_hash: createHash('sha256').update(serialized, 'utf8').digest('hex'),
  };
}

function continuationText(value: any, key: any, required: any = false) {
  if (value == null && !required) return null;
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`continuation_${key}_invalid`);
  }
  if (value.length > MAX_CONTINUATION_TEXT_LENGTH) {
    throw new Error(`continuation_${key}_too_long`);
  }
  return value;
}

function continuationStringArray(value: any, key: any) {
  if (value == null) return [];
  if (!Array.isArray(value)) throw new Error(`continuation_${key}_invalid`);
  if (value.length > MAX_CONTINUATION_ARRAY_ITEMS) {
    throw new Error(`continuation_${key}_too_many_items`);
  }
  return value.map((item: any, index: any) => {
    if (typeof item !== 'string' || item.trim() === '') {
      throw new Error(`continuation_${key}_${index}_invalid`);
    }
    if (item.length > MAX_CONTINUATION_TEXT_LENGTH) {
      throw new Error(`continuation_${key}_${index}_too_long`);
    }
    return item;
  });
}

function normalizeContinuationTimestamp(value: any) {
  if (typeof value !== 'string' || Number.isNaN(Date.parse(value))) {
    throw new Error('continuation_created_at_invalid');
  }
  return new Date(value).toISOString();
}

function normalizeContinuationExportPath(value: any, agentId: any, checkpointId: any) {
  const defaultPath = `.ai/continuations/${safePathSegment(agentId)}-${checkpointId}.md`;
  const path = value == null ? defaultPath : value;
  if (typeof path !== 'string' || path.trim() === '' || path.includes('\0') || isAbsolute(path) || path.includes(':')) {
    throw new Error('continuation_export_path_must_be_site_relative');
  }
  const normalizedPath = path.replace(/\\/g, '/');
  if (!normalizedPath.toLowerCase().endsWith('.md')) {
    throw new Error('continuation_export_path_must_be_markdown');
  }
  const exportRoot = resolve(siteRoot, '.ai', 'continuations');
  const artifactPath = resolve(siteRoot, normalizedPath);
  if (!pathWithin(exportRoot, artifactPath)) {
    throw new Error('continuation_export_path_outside_export_root');
  }
  return relative(siteRoot, artifactPath).replace(/\\/g, '/');
}

function safePathSegment(value: any) {
  const segment = String(value ?? '')
    .replace(/[^a-z0-9_.-]+/gi, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 80);
  return segment || 'agent';
}

function renderContinuationMarkdown({ agentId, checkpoint, continuation }: any) {
  const lines = [
    '<!-- narada.continuation.handoff.v1 -->',
    `<!-- narada.continuation.content-hash: ${continuation.content_hash} -->`,
    `<!-- narada.continuation.source-checkpoint-ref: ${continuation.source_checkpoint_ref} -->`,
    '',
    `# Continuation: ${markdownInline(continuation.objective)}`,
    '',
    '- **Schema:** `narada.continuation.v1`',
    `- **Continuation ID:** \`${markdownInline(continuation.continuation_id)}\``,
    `- **Agent:** \`${markdownInline(agentId)}\``,
    `- **Checkpoint:** \`${markdownInline(checkpoint.checkpoint_id)}\``,
    `- **Checkpointed:** ${markdownInline(checkpoint.checkpoint_at)}`,
    `- **Created:** ${markdownInline(continuation.created_at)}`,
    `- **Resume mode:** \`${markdownInline(continuation.resume_mode)}\``,
    '',
    '## Current state',
    '',
    markdownBlock(continuation.current_state),
    '',
    '## Next action',
    '',
    markdownBlock(continuation.next_action ?? 'No next action recorded.'),
    '',
  ];
  appendMarkdownList(lines, 'Completed work', continuation.completed_work);
  appendMarkdownList(lines, 'Decisions', continuation.decisions);
  appendMarkdownList(lines, 'Evidence references', continuation.evidence_refs);
  appendMarkdownList(lines, 'Open blockers', continuation.open_blockers);
  appendMarkdownList(lines, 'Canonical sources', continuation.canonical_sources);
  appendMarkdownList(lines, 'Constraints', continuation.constraints);
  lines.push('> This file is a bounded projection of agent-context checkpoint state. Verify live Git, task, and agent-context state before acting.', '');
  return lines.join('\n');
}

function appendMarkdownList(lines: any, title: any, values: any) {
  lines.push(`## ${title}`, '');
  if (!Array.isArray(values) || values.length === 0) {
    lines.push('_None._', '');
    return;
  }
  for (const value of values) lines.push(`- ${markdownInline(value)}`);
  lines.push('');
}

function markdownInline(value: any) {
  return String(value ?? '').replace(/[\r\n]+/g, ' ').trim();
}

function markdownBlock(value: any) {
  return String(value ?? '').replace(/\r\n/g, '\n').trim();
}

function writeContinuationArtifact(artifactPath: any, markdown: any, overwrite: any) {
  mkdirSync(dirname(artifactPath), { recursive: true });
  const bytes = Buffer.from(markdown, 'utf8');
  if (existsSync(artifactPath)) {
    const existing = readFileSync(artifactPath);
    if (existing.equals(bytes)) return { bytes: bytes.length, wrote: false };
    if (overwrite !== true) throw new Error('continuation_export_target_exists');
    writeFileSync(artifactPath, bytes);
    return { bytes: bytes.length, wrote: true };
  }
  writeFileSync(artifactPath, bytes, { flag: 'wx' });
  return { bytes: bytes.length, wrote: true };
}

let inputBuffer = '';
let transportMode = 'content-length';
const isMainModule = process.argv[1] != null && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMainModule) {
  assertSiteRoot();
  traceStartup('site_root_ok');
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk: any) => {
    if (inputBuffer.length === 0) {
      traceStartup('first_stdin_chunk', {
        bytes: Buffer.byteLength(chunk, 'utf8'),
        sample: JSON.stringify(chunk.slice(0, 300)),
      });
    }
    inputBuffer += chunk;
    processInputBuffer();
  });
}

function parseArgs(argv: any): Record<string, any> {
  const parsed: Record<string, any> = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (!arg.startsWith('--')) continue;
    const key = arg.slice(2);
    const next = argv[i + 1];
    if (next && !next.startsWith('--')) {
      parsed[key] = next;
      i++;
    } else {
      parsed[key] = true;
    }
  }
  return parsed;
}

function assertSiteRoot() {
  const envSiteRoot = process.env.NARADA_SITE_ROOT;
  if (envSiteRoot && !samePath(envSiteRoot, siteRoot)) {
    throw new Error(`agent_context_site_root_mismatch: env NARADA_SITE_ROOT=${envSiteRoot}; bound_site_root=${siteRoot}`);
  }
  if (!pathWithin(siteRoot, dbPath)) {
    throw new Error(`agent_context_db_path_outside_site_root: db_path=${dbPath}; bound_site_root=${siteRoot}`);
  }
  const agPath = resolveAgentContextLawPath(siteRoot).path;
  if (!existsSync(agPath)) {
    throw new Error(`agent_context_missing_agents_md: ${agPath}`);
  }
}

function deriveSiteId(root: string): string {
  const normalized = root.replace(/\\/g, '/').replace(/\/+$/g, '');
  const parts = normalized.split('/').filter(Boolean);
  const last = parts[parts.length - 1] ?? 'unknown-site';
  if (last === '.narada' && parts.length > 1) return parts[parts.length - 2];
  return last;
}

function normalizeSiteId(value: unknown): string {
  const text = String(value ?? '').trim();
  if (!text) return 'unknown-site';
  return text.replace(/^narada[.-]/, 'narada.');
}

function samePath(left: string, right: string): boolean {
  return resolve(left).toLowerCase() === resolve(right).toLowerCase();
}

function pathWithin(root: string, candidate: string): boolean {
  const relativePath = relative(resolve(root), resolve(candidate));
  return relativePath === '' || (
    relativePath !== '..'
    && !relativePath.startsWith('..\\')
    && !relativePath.startsWith('../')
    && !relativePath.includes(':')
  );
}

function normalizeContinuationRef(value: any) {
  if (value == null) return null;
  if (typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('continuation_ref_invalid: expected an object');
  }

  const schema = value.schema;
  const path = value.path;
  const sha256 = value.sha256;
  const createdAt = value.created_at;
  if (schema !== 'narada.continuation.handoff.v1') {
    throw new Error('continuation_ref_schema_invalid');
  }
  if (typeof path !== 'string' || path.trim() === '' || path.includes('\0') || isAbsolute(path)) {
    throw new Error('continuation_ref_path_must_be_site_relative');
  }
  if (typeof sha256 !== 'string' || !/^[a-f0-9]{64}$/i.test(sha256)) {
    throw new Error('continuation_ref_sha256_invalid');
  }
  if (typeof createdAt !== 'string' || Number.isNaN(Date.parse(createdAt))) {
    throw new Error('continuation_ref_created_at_invalid');
  }

  const normalizedPath = path.replace(/\\/g, '/');
  if (!pathWithin(siteRoot, resolve(siteRoot, normalizedPath))) {
    throw new Error('continuation_ref_path_outside_site_root');
  }

  const artifactPath = resolve(siteRoot, normalizedPath);
  let artifactBytes;
  try {
    const artifactStats = statSync(artifactPath);
    if (!artifactStats.isFile()) throw new Error('not_a_file');
    if (artifactStats.size > MAX_CONTINUATION_BYTES) throw new Error('too_large');
    artifactBytes = readFileSync(artifactPath);
  } catch (error) {
    throw new Error(`continuation_ref_unreadable: ${error instanceof Error ? error.message : String(error)}`);
  }

  const actualSha256 = createHash('sha256').update(artifactBytes).digest('hex');
  if (actualSha256 !== sha256.toLowerCase()) {
    throw new Error('continuation_ref_sha256_mismatch');
  }

  return {
    schema,
    path: normalizedPath,
    sha256: sha256.toLowerCase(),
    created_at: createdAt,
  };
}

function processInputBuffer() {
  while (true) {
    if (inputBuffer.startsWith('{')) {
      const lineEnd = inputBuffer.indexOf('\n');
      if (lineEnd === -1) return;
      const line = inputBuffer.slice(0, lineEnd).trim();
      inputBuffer = inputBuffer.slice(lineEnd + 1);
      transportMode = 'ndjson';
      if (line) handleMessage(JSON.parse(line));
      continue;
    }
    const crlfHeaderEnd = inputBuffer.indexOf('\r\n\r\n');
    const lfHeaderEnd = inputBuffer.indexOf('\n\n');
    const headerEnd = crlfHeaderEnd === -1
      ? lfHeaderEnd
      : lfHeaderEnd === -1
        ? crlfHeaderEnd
        : Math.min(crlfHeaderEnd, lfHeaderEnd);
    if (headerEnd === -1) return;
    const separatorLength = inputBuffer.startsWith('\r\n\r\n', headerEnd) ? 4 : 2;
    const header = inputBuffer.slice(0, headerEnd);
    const match = header.match(/Content-Length:\s*(\d+)/i);
    if (!match) throw new Error('mcp_content_length_missing');
    const length = Number(match[1]);
    const bodyStart = headerEnd + separatorLength;
    if (inputBuffer.length < bodyStart + length) return;
    const body = inputBuffer.slice(bodyStart, bodyStart + length);
    inputBuffer = inputBuffer.slice(bodyStart + length);
    handleMessage(JSON.parse(body));
  }
}

function send(payload: any) {
  const body = JSON.stringify(payload);
  if (transportMode === 'ndjson') {
    process.stdout.write(`${body}\n`);
    return;
  }
  process.stdout.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\r\n\r\n${body}`);
}

function respond(id: any, result: any) {
  send({ jsonrpc: '2.0', id, result });
}

function respondError(id: any, error: any) {
  send({
    jsonrpc: '2.0',
    id,
    error: {
      code: -32000,
      message: error instanceof Error ? error.message : String(error),
    },
  });
}

function sendProgress(message: any, progress: any, progressMessage: any) {
  const progressToken = message?.params?._meta?.progressToken;
  if (progressToken === undefined) return;
  send({
    jsonrpc: '2.0',
    method: 'notifications/progress',
    params: { progressToken, progress, total: 1, message: progressMessage },
  });
}

export function listTools(projection: AgentContextToolProjection = currentToolProjection()) {
  return listAgentContextTools(projection);
}

async function handleMessage(message: any) {
  if (!message || typeof message !== 'object') return;
  if (message.error) return;
  if (!message.id && message.method === 'notifications/cancelled') {
    const requestId = String(message.params?.requestId ?? '');
    activeRequests.get(requestId)?.abort();
    return;
  }
  if (!message.id && typeof message.method === 'string' && message.method.startsWith('notifications/')) return;
  const id = message.id ?? null;
  const requestId = id == null ? null : String(id);
  const abortController = requestId == null ? null : new AbortController();
  if (requestId) activeRequests.set(requestId, abortController);
  try {
    sendProgress(message, 0, 'started');
    if (message.method === 'initialize') {
      traceStartup('initialize');
      respond(id, {
        protocolVersion: message.params?.protocolVersion ?? PROTOCOL_VERSION,
        capabilities: { tools: {}, resources: {}, prompts: {}, completions: {}, logging: {} },
        serverInfo: { name: SERVER_NAME, version: SERVER_VERSION },
      });
      return;
    }
    if (message.method === 'notifications/initialized') return;
    if (message.method === 'tools/list') {
      traceStartup('tools_list');
      respond(id, { tools: listTools() });
      return;
    }
    if (message.method === 'tools/call') {
      const name = message.params?.name;
      const toolArgs = message.params?.arguments ?? {};
      const result = await callTool(name, toolArgs);
      respond(
        id,
        name === 'agent_orientation_read' && currentToolProjection() === 'occupant'
          ? buildInlineOrientationToolResult(result)
          : buildBoundedToolResult({
              siteRoot,
              toolName: String(name ?? 'unknown_tool'),
              value: result,
              limit: 6000,
              readerTool: 'mcp_output_show',
            }),
      );
      return;
    }
    if (message.method === 'resources/list') {
      respond(id, listResources());
      return;
    }
    if (message.method === 'resources/read') {
      respond(id, readResource(message.params?.uri));
      return;
    }
    if (message.method === 'prompts/list') {
      respond(id, { prompts: listPrompts() });
      return;
    }
    if (message.method === 'prompts/get') {
      respond(id, promptGet(message.params ?? {}));
      return;
    }
    if (message.method === 'completion/complete') {
      respond(id, completeArgument(message.params ?? {}));
      return;
    }
    if (message.method === 'logging/setLevel') {
      respond(id, {});
      return;
    }
    respondError(id, new Error(`unsupported_method: ${message.method}`));
  } catch (error) {
    respondError(id, error);
  } finally {
    sendProgress(message, 1, abortController?.signal.aborted ? 'cancelled' : 'completed');
    if (requestId) activeRequests.delete(requestId);
  }
}

function listPrompts() {
  if (currentToolProjection() === 'occupant') return [];
  return [{ name: 'agent_context_startup', title: 'Agent Context Startup', description: 'Guidance for exact admitted Orientation Manifest delivery and bounded continuity.', arguments: [] }];
}

function promptGet(params: any) {
  if (currentToolProjection() === 'occupant') {
    throw new Error('agent_context_prompts_not_exposed_in_occupant_projection');
  }
  const name = String(params.name ?? '');
  if (name !== 'agent_context_startup') throw new Error(`unknown_prompt: ${name}`);
  return {
    description: 'Guidance for exact admitted Orientation Manifest delivery and bounded continuity.',
    messages: [{ role: 'user', content: { type: 'text', text: 'This is the enforced Carrier-entry orientation turn. Call agent_orientation_read({}) and then execute each returned next_call exactly. A continuation is opaque: never inspect or alter it. Stop only when status=ready and ordinary_work_gate=open. Agent Context retains required-read and acknowledgement evidence. The inline brief names exact continuity and work entry snapshots or explicit omissions and carries one canonical manifest_ref. Acknowledgement proves delivery and completed reads, not comprehension or authority for a later action.' } }],
  };
}

function completeArgument(params: any) {
  const argumentName = String((params.argument && typeof params.argument === 'object' ? params.argument.name : '') ?? '');
  const allValues = argumentName === 'name'
    ? listTools().map((tool: any) => tool.name).filter(Boolean)
    : [];
  const context = params.context && typeof params.context === 'object' ? params.context : {};
  const page = boundedCollection(allValues, {
    offset: Number.isInteger(context._narada_offset) ? context._narada_offset : 0,
    limit: 100,
    truncationReason: 'tool_completion_page',
  });
  return {
    completion: {
      values: page.items,
      total: page.total_count ?? allValues.length,
      hasMore: page.has_more,
    },
    narada_paging: page,
  };
}

function assistantTextContent(text: string) {
  return { type: 'text', text, annotations: { audience: ['assistant'] } };
}

function buildInlineOrientationToolResult(value: any) {
  const text: any = JSON.stringify(value);
  const contentBytes: any = Buffer.byteLength(text, 'utf8');
  const responseBytes: any = contentBytes
    + Buffer.byteLength(JSON.stringify(value), 'utf8');
  if (
    contentBytes > ORIENTATION_INLINE_CONTENT_BYTES
    || responseBytes > ORIENTATION_INLINE_RESPONSE_BYTES
  ) {
    throw new Error(
      'agent_context_orientation_inline_budget_exceeded:'
      + `content=${contentBytes}:response=${responseBytes}`,
    );
  }
  return {
    content: [assistantTextContent(text)],
    structuredContent: value,
  };
}

async function callTool(name: any, toolArgs: any) {
  if (!listTools().some((tool: any) => tool.name === name)) {
    throw new Error(`agent_context_tool_not_exposed_in_${currentToolProjection()}_projection:${name}`);
  }
  switch (name) {
    case 'agent_orientation_read':
      return currentToolProjection() === 'occupant'
        ? orientationOccupantRead(toolArgs)
        : orientationRead(toolArgs);
    case 'agent_orientation_acknowledge':
      return orientationAcknowledge(toolArgs);
    case 'agent_context_guidance':
      return buildGuidanceResult(toolArgs);
    case 'agent_context_doctor':
      return doctor();
    case 'mcp_output_show':
      return await outputShowAsync({ siteRoot, args: toolArgs });
    case 'agent_context_whoami':
      return whoami(toolArgs);
    case 'agent_context_start_session':
      return startSession(toolArgs);
    case 'agent_context_checkpoint':
      return checkpoint(toolArgs);
    case 'agent_context_rehydrate':
      return rehydrate(toolArgs);
    case 'agent_context_continuation_export':
      return continuationExport(toolArgs);
    case 'agent_context_continuation_read':
      return continuationRead(toolArgs);
    case 'agent_context_hydrate_current':
      return hydrateCurrent(toolArgs);
    case 'agent_context_startup_sequence':
      return startupSequence(toolArgs);
    case 'agent_context_list_sessions':
      return listSessions(toolArgs);
    default:
      throw new Error(`unknown_tool: ${name}`);
  }
}

function withDb(fn: any) {
  const db = openAgentContextDb(siteRoot, dbPath);
  try {
    ensureCheckpointTables(db);
    return fn(db);
  } finally {
    db.close();
  }
}

function ensureCheckpointTables(db: any) {
  db.exec(`
    CREATE TABLE IF NOT EXISTS agent_checkpoints (
      checkpoint_id TEXT PRIMARY KEY,
      agent_id TEXT NOT NULL,
      session_id TEXT,
      checkpoint_at TEXT NOT NULL,
      active_task_json TEXT,
      files_touched_json TEXT,
      key_decisions_json TEXT,
      open_questions_json TEXT,
      git_head TEXT,
      payload_json TEXT
    );

    CREATE INDEX IF NOT EXISTS idx_agent_checkpoints_agent
      ON agent_checkpoints(agent_id, checkpoint_at DESC);

    CREATE TABLE IF NOT EXISTS agent_checkpoint_history (
      history_id TEXT PRIMARY KEY,
      checkpoint_id TEXT NOT NULL,
      agent_id TEXT NOT NULL,
      session_id TEXT,
      checkpoint_at TEXT NOT NULL,
      active_task_json TEXT,
      files_touched_json TEXT,
      key_decisions_json TEXT,
      open_questions_json TEXT,
      git_head TEXT,
      payload_json TEXT,
      archived_at TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_checkpoint_history_agent
      ON agent_checkpoint_history(agent_id, archived_at DESC);
  `);
}

function doctor() {
  return withDb((db: any) => {
    const tables = [
      'agent_start_events',
      'agent_events',
      'agent_checkpoints',
      'agent_checkpoint_history',
      'orientation_manifest_generations',
    ].map((table: any) => ({
      table,
      exists: !!db.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?").get(table),
    }));
    return {
      status: tables.every((table: any) => table.exists) ? 'ok' : 'degraded',
      site_id: siteId,
      server_name: SERVER_NAME,
      site_root: siteRoot,
      db_path: dbPath,
      tables,
    };
  });
}

function startSession(toolArgs: any) {
  const identity = requiredString(toolArgs, 'identity');
  assertAgentContextIdentity(identity);
  const evidence = resolveExactOrientationEvidence(toolArgs);
  return materializeAgentSessionStart({
    siteRoot,
    siteId,
    identity,
    runtime: toolArgs.runtime ?? 'codex',
    dbPath,
    cwd: toolArgs.cwd ?? siteRoot,
    dryRun: toolArgs.dry_run === true,
    carrierSessionId: process.env.NARADA_CARRIER_SESSION_ID ?? null,
    admissionReceipt: evidence.admission_receipt,
    activationReceipt: evidence.activation_receipt,
    generatedAt: toolArgs.generated_at ?? null,
  });
}

function checkpoint(toolArgs: any) {
  const agentId = toolArgs.agent_id ?? process.env.NARADA_AGENT_ID;
  if (!agentId) throw new Error('agent_id_required');
  assertAgentContextIdentity(agentId);

  return withDb((db: any) => {
    const now = new Date().toISOString();
    const checkpointId = `chk_${randomUUID().replace(/-/g, '')}`;
    const existing = db.prepare('SELECT * FROM agent_checkpoints WHERE agent_id = ?').get(agentId);
    const previousCheckpoint = existing ? rowToCheckpoint(existing) : null;
    const payload = checkpointPayload(toolArgs, agentId, now, checkpointId);
    payload.continuation_projection = continuationProjectionState({
      agentId,
      continuation: payload.continuation,
      continuationRef: payload.continuation_ref,
      previousCheckpoint,
    });
    if (existing) {
      db.prepare(`
        INSERT INTO agent_checkpoint_history (
          history_id, checkpoint_id, agent_id, session_id, checkpoint_at,
          active_task_json, files_touched_json, key_decisions_json,
          open_questions_json, git_head, payload_json, archived_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(
        `hist_${randomUUID().replace(/-/g, '')}`,
        existing.checkpoint_id,
        existing.agent_id,
        existing.session_id,
        existing.checkpoint_at,
        existing.active_task_json,
        existing.files_touched_json,
        existing.key_decisions_json,
        existing.open_questions_json,
        existing.git_head,
        existing.payload_json,
        now
      );
      db.prepare('DELETE FROM agent_checkpoints WHERE checkpoint_id = ?').run(existing.checkpoint_id);
    }

    db.prepare(`
      INSERT INTO agent_checkpoints (
        checkpoint_id, agent_id, session_id, checkpoint_at,
        active_task_json, files_touched_json, key_decisions_json,
        open_questions_json, git_head, payload_json
      )
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      checkpointId,
      agentId,
      toolArgs.session_id ?? process.env.NARADA_AGENT_START_EVENT_ID ?? null,
      now,
      jsonOrNull(toolArgs.active_task),
      JSON.stringify(arrayValue(toolArgs.files_touched)),
      JSON.stringify(arrayValue(toolArgs.key_decisions)),
      JSON.stringify(arrayValue(toolArgs.open_questions)),
      toolArgs.git_head ?? null,
      JSON.stringify(payload)
    );

    return {
      status: 'checkpointed',
      checkpoint_id: checkpointId,
      archived_prior: existing?.checkpoint_id ?? null,
      agent_id: agentId,
      checkpoint_at: now,
      db_path: dbPath,
      site_root: siteRoot,
      continuation: payload.continuation ?? null,
      continuation_ref: payload.continuation_ref ?? null,
      continuation_projection: payload.continuation_projection ?? null,
    };
  });
}

function rehydrate(toolArgs: any) {
  const agentId = requiredString(toolArgs, 'agent_id');
  assertAgentContextIdentity(agentId);
  const checkpointId = optionalCheckpointId(toolArgs);
  const limit = Math.min(Math.max(Number(toolArgs.limit ?? 1), 1), 50);
  const offset = Math.max(Math.trunc(Number(toolArgs.offset ?? 0)) || 0, 0);

  return withDb((db: any) => {
    if (checkpointId !== null) {
      const row = checkpointRowForAgent(db, agentId, checkpointId);
      if (!row) {
        return {
          status: 'checkpoint_not_found',
          agent_id: agentId,
          checkpoint_id: checkpointId,
          message: 'No site-local current or archived checkpoint found for the requested checkpoint_id.',
        };
      }
      return { status: 'ok', ...rowToCheckpoint(row) };
    }

    if (toolArgs.history === true || limit > 1 || offset > 0) {
      const totalRow = db.prepare(`
        SELECT COUNT(*) AS total_count FROM agent_checkpoint_history WHERE agent_id = ?
      `).get(agentId);
      const totalCount = Number(totalRow?.total_count ?? 0);
      const rows = db.prepare(`
        SELECT * FROM agent_checkpoint_history
        WHERE agent_id = ?
        ORDER BY archived_at DESC
        LIMIT ?
        OFFSET ?
      `).all(agentId, limit, offset);
      const hasMore = offset + rows.length < totalCount;
      return {
        status: rows.length > 0 ? 'ok' : 'no_checkpoint_history',
        agent_id: agentId,
        count: rows.length,
        offset,
        total_count: totalCount,
        has_more: hasMore,
        next_offset: hasMore ? offset + rows.length : null,
        truncated: hasMore,
        truncation_reason: hasMore ? 'checkpoint_history_page_limit' : null,
        checkpoints: rows.map(rowToCheckpoint),
      };
    }

    const row = checkpointRowForAgent(db, agentId, null);
    if (!row) {
      return { status: 'no_checkpoint', agent_id: agentId, message: 'No site-local checkpoint found.' };
    }
    return { status: 'ok', ...rowToCheckpoint(row) };
  });
}

function continuationExport(toolArgs: any) {
  const agentId = toolArgs.agent_id ?? process.env.NARADA_AGENT_ID;
  if (!agentId) throw new Error('agent_id_required');
  assertAgentContextIdentity(agentId);

  return withDb((db: any) => {
    const row = db.prepare('SELECT * FROM agent_checkpoints WHERE agent_id = ? ORDER BY checkpoint_at DESC LIMIT 1').get(agentId);
    if (!row) return { status: 'no_checkpoint', agent_id: agentId, message: 'No site-local checkpoint found.' };

    const checkpoint = rowToCheckpoint(row);
    if (!checkpoint.continuation) {
      return {
        status: 'no_continuation',
        agent_id: agentId,
        checkpoint_id: checkpoint.checkpoint_id,
        message: 'The latest checkpoint has no canonical continuation state.',
      };
    }

    const relativePath = normalizeContinuationExportPath(toolArgs.path, agentId, checkpoint.checkpoint_id);
    const artifactPath = resolve(siteRoot, relativePath);
    const markdown = renderContinuationMarkdown({ agentId, checkpoint, continuation: checkpoint.continuation });
    const writeResult = writeContinuationArtifact(artifactPath, markdown, toolArgs.overwrite === true);
    const artifactBytes = readFileSync(artifactPath);
    const reference = normalizeContinuationRef({
      schema: 'narada.continuation.handoff.v1',
      path: relativePath,
      sha256: createHash('sha256').update(artifactBytes).digest('hex'),
      created_at: new Date().toISOString(),
    });
    const nextPayload = {
      ...checkpoint.payload,
      continuation_ref: reference,
      continuation_projection: continuationProjectionState({
        agentId,
        continuation: checkpoint.continuation,
        continuationRef: reference,
      }),
    };
    db.prepare('UPDATE agent_checkpoints SET payload_json = ? WHERE checkpoint_id = ?')
      .run(JSON.stringify(nextPayload), checkpoint.checkpoint_id);

    return {
      status: 'exported',
      site_id: siteId,
      site_root: siteRoot,
      agent_id: agentId,
      checkpoint_id: checkpoint.checkpoint_id,
      checkpoint_at: checkpoint.checkpoint_at,
      continuation: checkpoint.continuation,
      continuation_ref: reference,
      continuation_projection: nextPayload.continuation_projection,
      artifact: {
        path: relativePath,
        bytes: artifactBytes.length,
        wrote: writeResult.wrote,
      },
    };
  });
}

function continuationRead(toolArgs: any) {
  const agentId = toolArgs.agent_id ?? process.env.NARADA_AGENT_ID;
  if (!agentId) throw new Error('agent_id_required');
  assertAgentContextIdentity(agentId);
  const checkpointId = optionalCheckpointId(toolArgs);

  return withDb((db: any) => {
    const row = checkpointRowForAgent(db, agentId, checkpointId);
    if (!row) {
      return checkpointId === null
        ? { status: 'no_checkpoint', agent_id: agentId, message: 'No site-local checkpoint found.' }
        : {
            status: 'checkpoint_not_found',
            agent_id: agentId,
            checkpoint_id: checkpointId,
            message: 'No site-local current or archived checkpoint found for the requested checkpoint_id.',
          };
    }

    const checkpoint = rowToCheckpoint(row);
    const checkpointLabel = checkpointId === null ? 'latest checkpoint' : `checkpoint ${checkpointId}`;
    const base = {
      site_id: siteId,
      site_root: siteRoot,
      agent_id: agentId,
      checkpoint_id: checkpoint.checkpoint_id,
      checkpoint_at: checkpoint.checkpoint_at,
      continuation: checkpoint.continuation,
      continuation_ref: checkpoint.continuation_ref,
      continuation_projection: checkpoint.continuation_projection ?? null,
    };
    if (!checkpoint.continuation_ref) {
      return {
        ...base,
        status: checkpoint.continuation ? 'unlinked' : 'no_continuation',
        message: checkpoint.continuation
          ? `Canonical continuation exists in the ${checkpointLabel} but has no portable Markdown reference.`
          : `The ${checkpointLabel} has no canonical continuation state.`,
        next_action: checkpoint.continuation_projection?.next_action ?? null,
      };
    }

    try {
      const reference = normalizeContinuationRef(checkpoint.continuation_ref);
      if (!reference) throw new Error('continuation_ref_missing');
      const markdown = readFileSync(resolve(siteRoot, reference.path), 'utf8');
      if (checkpoint.continuation) {
        const handoffMarker = '<!-- narada.continuation.handoff.v1 -->';
        const contentHashMarker = `<!-- narada.continuation.content-hash: ${checkpoint.continuation.content_hash} -->`;
        if (!markdown.includes(handoffMarker) || !markdown.includes(contentHashMarker)) {
          return {
            ...base,
            continuation_ref: reference,
            status: 'stale',
            reason: 'continuation_artifact_content_hash_mismatch',
            artifact: { path: reference.path, verified: false },
          };
        }
      }
      return {
        ...base,
        continuation_ref: reference,
        status: 'ok',
        artifact: {
          path: reference.path,
          sha256: reference.sha256,
          created_at: reference.created_at,
          bytes: Buffer.byteLength(markdown, 'utf8'),
          verified: true,
          markdown,
        },
      };
    } catch (error) {
      return {
        ...base,
        status: 'stale',
        reason: error instanceof Error ? error.message : String(error),
        artifact: { path: checkpoint.continuation_ref.path, verified: false },
      };
    }
  });
}

function resolveExactOrientationEvidence(toolArgs: any = {}) {
  const inherited: any = orientationEvidenceFromEnvironment();
  const suppliedAdmission: any = toolArgs.admission_receipt === undefined
    || toolArgs.admission_receipt === null
    ? null
    : parseCarrierSessionAdmissionReceipt(toolArgs.admission_receipt);
  const suppliedActivation: any = toolArgs.activation_receipt === undefined
    || toolArgs.activation_receipt === null
    ? null
    : parseCarrierSessionActivationReceipt(toolArgs.activation_receipt);

  if (
    suppliedAdmission
    && inherited.admission_receipt
    && JSON.stringify(suppliedAdmission) !== JSON.stringify(inherited.admission_receipt)
  ) {
    throw new Error('agent_context_conflicting_admission_receipts');
  }
  if (
    suppliedActivation
    && inherited.activation_receipt
    && JSON.stringify(suppliedActivation) !== JSON.stringify(inherited.activation_receipt)
  ) {
    throw new Error('agent_context_conflicting_activation_receipts');
  }

  return {
    admission_receipt: suppliedAdmission ?? inherited.admission_receipt,
    activation_receipt: suppliedActivation ?? inherited.activation_receipt,
  };
}

function whoami(toolArgs: any = {}) {
  const evidence: any = resolveExactOrientationEvidence(toolArgs);
  if (!evidence.admission_receipt) {
    return {
      schema: 'narada.agent_context.identity_resolution.v1',
      status: 'blocked',
      reason: 'agent_context_exact_admission_receipt_required',
      rejected_fallbacks: ['latest_checkpoint', 'latest_start_event', 'identity_name_inference'],
    };
  }

  const receipt: any = evidence.admission_receipt;
  const identity: any = receipt.agent_identity.local_agent_id;
  const admitted: any = assertAdmissionMatchesAgentContext(receipt, {
    siteId,
    identity: process.env.NARADA_AGENT_ID ?? identity,
    carrierSessionId: process.env.NARADA_CARRIER_SESSION_ID ?? null,
    observedAt: new Date().toISOString(),
  });
  return {
    schema: 'narada.agent_context.identity_resolution.v1',
    status: 'ok',
    identity: admitted.agent_identity.local_agent_id,
    canonical_agent_id: admitted.agent_identity.canonical_agent_id,
    confidence: 'exact',
    source: 'carrier_session_admission_receipt',
    admission_receipt_ref: admitted.receipt_id,
    carrier_session: admitted.coordinate,
    authority_readback_ref: admitted.authority_readback_ref,
    hint_match: toolArgs.hint
      ? admitted.agent_identity.local_agent_id === toolArgs.hint
        || admitted.agent_identity.canonical_agent_id === toolArgs.hint
      : null,
  };
}

function hydrateCurrent(toolArgs: any = {}) {
  if (toolArgs.checkpoint_startup === true) {
    return {
      schema: 'narada.agent_context.orientation_hydration.v1',
      status: 'blocked',
      reason: 'orientation_assembly_read_only',
      required_next_step: 'Use agent_context_checkpoint as a separate explicit mutation.',
    };
  }
  const evidence: any = resolveExactOrientationEvidence(toolArgs);
  if (!evidence.admission_receipt) {
    return {
      schema: 'narada.agent_context.orientation_hydration.v1',
      status: 'blocked',
      reason: 'agent_context_exact_admission_receipt_required',
      rejected_fallbacks: ['latest_checkpoint', 'latest_start_event', 'identity_name_inference'],
    };
  }
  const identity: any = evidence.admission_receipt.agent_identity.local_agent_id;
  const admission: any = assertAdmissionMatchesAgentContext(evidence.admission_receipt, {
    siteId,
    identity: process.env.NARADA_AGENT_ID ?? identity,
    carrierSessionId: process.env.NARADA_CARRIER_SESSION_ID ?? null,
    observedAt: toolArgs.generated_at ?? new Date().toISOString(),
  });
  const checkpointId = optionalCheckpointId(toolArgs);
  const checkpointResult: any = checkpointId === null
    ? {
      status: 'omitted',
      reason: 'exact_checkpoint_not_selected',
      checkpoint_id: null,
    }
    : rehydrate({ agent_id: identity, checkpoint_id: checkpointId });
  const portableContinuation: any = checkpointId === null
    ? {
      status: 'omitted',
      reason: 'exact_checkpoint_not_selected',
      checkpoint_id: null,
    }
    : continuationRead({ agent_id: identity, checkpoint_id: checkpointId });
  const hydratedAt = new Date().toISOString();
  const roster: any = validateIdentityAgainstRoster(siteRoot, identity);
  const roleBinding: any = roster.valid
    ? roster.role_binding
    : {
      binding_authority: 'unavailable',
      binding_source: 'unavailable',
      reason: roster.error ?? 'role_binding_unavailable',
    };
  const compilation: any = compileAgentContextOrientation({
    siteRoot,
    siteId,
    admissionReceipt: admission,
    activationReceipt: evidence.activation_receipt,
    observedAt: toolArgs.generated_at ?? hydratedAt,
    roleBinding,
    exactCheckpoint: checkpointId === null ? null : checkpointResult,
    portableContinuation: checkpointId === null ? null : portableContinuation,
    mcpServers: [],
  });
  const manifest: any = compilation.manifest;
  return {
    schema: 'narada.agent_context.orientation_hydration.v1',
    status: manifest.delivery === 'deliverable' ? 'ok' : 'blocked',
    source_mutation: false,
    site_id: siteId,
    site_root: siteRoot,
    hydrated_at: manifest.generated_at,
    whoami: whoami({ admission_receipt: admission, hint: identity }),
    admission_receipt_ref: admission.receipt_id,
    orientation_manifest: manifest,
    continuity_selection: checkpointId === null
      ? { mode: 'omitted', checkpoint_id: null }
      : { mode: 'exact', checkpoint_id: checkpointId },
    checkpoint: checkpointResult,
    portable_continuation: portableContinuation,
    continuity_advisory_next_action: checkpointResult.status === 'ok'
      ? checkpointResult.next_intended_action ?? null
      : null,
  };
}

function exactOrientationManifestId(toolArgs: any = {}) {
  const supplied = typeof toolArgs.manifest_id === 'string' && toolArgs.manifest_id.trim()
    ? toolArgs.manifest_id.trim()
    : null;
  const inherited = typeof process.env.NARADA_ORIENTATION_MANIFEST_ID === 'string'
    && process.env.NARADA_ORIENTATION_MANIFEST_ID.trim()
    ? process.env.NARADA_ORIENTATION_MANIFEST_ID.trim()
    : null;
  if (supplied && inherited && supplied !== inherited) {
    throw new Error('agent_context_conflicting_orientation_manifest_ids');
  }
  return supplied ?? inherited;
}

function exactOrientationDeliveryReceipt() {
  const raw = process.env.NARADA_ORIENTATION_DELIVERY_RECEIPT;
  if (typeof raw !== 'string' || raw.trim() === '') {
    throw new Error('agent_context_exact_orientation_delivery_receipt_required');
  }
  try {
    return parseCarrierSessionOrientationDeliveryReceipt(JSON.parse(raw));
  } catch (error: any) {
    throw new Error(
      `agent_context_orientation_delivery_receipt_invalid:${error?.message ?? String(error)}`,
    );
  }
}

function exactOrientationEntryEvidence() {
  const evidence: any = resolveExactOrientationEvidence();
  if (!evidence.admission_receipt) {
    throw new Error('agent_context_exact_admission_receipt_required');
  }
  const identity = evidence.admission_receipt.agent_identity.local_agent_id;
  const admission = assertAdmissionMatchesAgentContext(evidence.admission_receipt, {
    siteId,
    identity: process.env.NARADA_AGENT_ID ?? identity,
    carrierSessionId: process.env.NARADA_CARRIER_SESSION_ID ?? null,
    observedAt: new Date().toISOString(),
  });
  const manifestId = exactOrientationManifestId();
  if (!manifestId) {
    throw new Error('agent_context_exact_orientation_manifest_id_required');
  }
  return {
    admission,
    manifestId,
    deliveryReceipt: exactOrientationDeliveryReceipt(),
  };
}

function orientationContinuationSignature(encoded: string, brief: any, delivery: any) {
  return createHmac(
    'sha256',
    `${delivery.receipt_id}\u0000${brief.brief_digest}`,
  ).update(encoded).digest('base64url');
}

function orientationContinuationForInternalCall(call: any, brief: any, delivery: any) {
  if (!call) return null;
  const payload: any = call.tool === 'agent_orientation_read'
    ? {
        schema: 'narada.agent_context.orientation_continuation.v1',
        phase: 'required_read',
        step_id: String(call.arguments?.step_id ?? ''),
        offset: Number(call.arguments?.offset ?? 0),
      }
    : call.tool === 'agent_orientation_acknowledge'
      ? {
          schema: 'narada.agent_context.orientation_continuation.v1',
          phase: 'acknowledge',
        }
      : null;
  if (!payload || (payload.phase === 'required_read' && !payload.step_id)) {
    throw new Error(`agent_context_orientation_internal_next_call_invalid:${String(call.tool)}`);
  }
  const encoded: any = Buffer.from(JSON.stringify(payload), 'utf8').toString('base64url');
  const signature: any = orientationContinuationSignature(encoded, brief, delivery);
  return {
    surface_id: 'agent-context',
    tool: 'agent_orientation_read',
    arguments: { continuation: `oc1.${encoded}.${signature}` },
  };
}

function parseOrientationContinuation(value: any, brief: any, delivery: any) {
  if (typeof value !== 'string' || !value.trim()) {
    throw new Error('agent_context_orientation_continuation_required');
  }
  const parts: any[] = value.trim().split('.');
  if (parts.length !== 3 || parts[0] !== 'oc1') {
    throw new Error('agent_context_orientation_continuation_invalid');
  }
  const expected: any = orientationContinuationSignature(parts[1], brief, delivery);
  if (parts[2] !== expected) {
    throw new Error('agent_context_orientation_continuation_binding_mismatch');
  }
  let payload: any;
  try {
    payload = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8'));
  } catch {
    throw new Error('agent_context_orientation_continuation_invalid');
  }
  if (
    payload?.schema !== 'narada.agent_context.orientation_continuation.v1'
    || !['required_read', 'acknowledge'].includes(payload.phase)
  ) {
    throw new Error('agent_context_orientation_continuation_invalid');
  }
  if (payload.phase === 'required_read') {
    if (
      typeof payload.step_id !== 'string'
      || !payload.step_id
      || !Number.isInteger(payload.offset)
      || payload.offset < 0
    ) {
      throw new Error('agent_context_orientation_continuation_invalid');
    }
  }
  return payload;
}

function occupantReadProgress(progress: any) {
  return {
    total: Number(progress.total ?? 0),
    completed: Number(progress.completed ?? 0),
    pending: Number(progress.pending ?? 0),
  };
}

function occupantReadyResult(packet: any, acknowledgementRef: any = null) {
  const orientation: any = buildOrientationReadyProjection(packet.orientation_brief);
  return {
    schema: 'narada.agent_context.orientation_ready.v1',
    status: 'ready',
    source_mutation: false,
    local_persistence: true,
    ordinary_work_gate: 'open',
    orientation,
    manifest_ref: packet.manifest_ref,
    acknowledgement_ref: acknowledgementRef ?? packet.acknowledgement_ref,
    next_call: null,
    suggested_next_call: orientation.next_meaningful_call,
  };
}

function occupantEntryResult(packet: any, delivery: any) {
  if (packet.ordinary_work_gate === 'open') return occupantReadyResult(packet);
  return {
    schema: 'narada.agent_context.orientation_entry.v3',
    status: 'orientation_required',
    source_mutation: false,
    ordinary_work_gate: 'acknowledgement_required',
    orientation_brief: buildOrientationOccupantBrief(packet.orientation_brief),
    manifest_ref: packet.manifest_ref,
    required_read_progress: occupantReadProgress(packet.required_read_progress),
    next_call: orientationContinuationForInternalCall(
      packet.next_call,
      packet.orientation_brief,
      delivery,
    ),
  };
}

function occupantRequiredReadResult(result: any, packet: any, delivery: any) {
  const stepIndex: any = packet.orientation_brief.required_reads.findIndex(
    (step: any) => step.step_id === result.step_id,
  );
  const gateOpen: any = packet.ordinary_work_gate === 'open';
  return {
    schema: 'narada.agent_context.orientation_material.v1',
    status: gateOpen ? 'ready' : 'orientation_required',
    source_mutation: false,
    local_persistence: true,
    ordinary_work_gate: gateOpen ? 'open' : 'acknowledgement_required',
    material: {
      delivery_status: result.status,
      ordinal: stepIndex >= 0 ? stepIndex + 1 : null,
      source_ref: result.source?.artifact_ref ?? null,
      content: result.content,
      page: result.page ? {
        returned_bytes: result.page.returned_bytes,
        eof: result.page.eof,
      } : null,
    },
    required_read_progress: occupantReadProgress(result.required_read_progress),
    next_call: gateOpen ? null : orientationContinuationForInternalCall(
      result.next_call,
      packet.orientation_brief,
      delivery,
    ),
  };
}

function orientationOccupantRead(toolArgs: any = {}) {
  const evidence: any = exactOrientationEntryEvidence();
  const packet: any = readOrientationEntryPacket({
    siteRoot,
    dbPath,
    manifestId: evidence.manifestId,
    admissionReceipt: evidence.admission,
    deliveryReceipt: evidence.deliveryReceipt,
  });
  if (packet.ordinary_work_gate === 'open') {
    return occupantReadyResult(packet);
  }
  const continuation: any = toolArgs.continuation;
  if (continuation === undefined) return occupantEntryResult(packet, evidence.deliveryReceipt);

  let decoded: any;
  try {
    decoded = parseOrientationContinuation(
      continuation,
      packet.orientation_brief,
      evidence.deliveryReceipt,
    );
  } catch (error) {
    return {
      schema: 'narada.agent_context.orientation_recovery.v1',
      status: 'orientation_required',
      ordinary_work_gate: packet.ordinary_work_gate,
      reason_code: error instanceof Error ? error.message : String(error),
      remediation: 'Use this response next_call exactly; do not reconstruct a continuation.',
      next_call: orientationContinuationForInternalCall(
        packet.next_call,
        packet.orientation_brief,
        evidence.deliveryReceipt,
      ),
    };
  }

  if (decoded.phase === 'acknowledge') {
    const record: any = orientationAcknowledge({});
    const acknowledgedPacket: any = readOrientationEntryPacket({
      siteRoot,
      dbPath,
      manifestId: evidence.manifestId,
      admissionReceipt: evidence.admission,
      deliveryReceipt: evidence.deliveryReceipt,
    });
    return occupantReadyResult(
      acknowledgedPacket,
      `agent-context:orientation_acknowledgements:${record.acknowledgement.acknowledgement_id}`,
    );
  }

  const result: any = recordOrientationRequiredRead({
    siteRoot,
    dbPath,
    admissionReceipt: evidence.admission,
    deliveryReceipt: evidence.deliveryReceipt,
    brief: packet.orientation_brief,
    stepId: decoded.step_id,
    byteOffset: decoded.offset,
    resultValidator: (candidate: any) => {
      buildInlineOrientationToolResult(
        occupantRequiredReadResult(candidate, packet, evidence.deliveryReceipt),
      );
    },
  });
  return occupantRequiredReadResult(result, packet, evidence.deliveryReceipt);
}

function orientationRead(toolArgs: any = {}) {
  const evidence = exactOrientationEntryEvidence();
  const packet: any = readOrientationEntryPacket({
    siteRoot,
    dbPath,
    manifestId: evidence.manifestId,
    admissionReceipt: evidence.admission,
    deliveryReceipt: evidence.deliveryReceipt,
  });
  const stepId: any = typeof toolArgs.step_id === 'string'
    ? toolArgs.step_id.trim()
    : '';
  const selection: any = typeof toolArgs.selection === 'string'
    ? toolArgs.selection.trim()
    : '';
  if (stepId && selection) {
    throw new Error('agent_context_orientation_read_mode_ambiguous');
  }
  if (!stepId && toolArgs.offset !== undefined) {
    throw new Error('agent_context_orientation_required_read_step_id_required_for_offset');
  }
  if (selection) {
    const selectionField: any = selection === 'continuity'
      ? 'continuity_selection'
      : selection === 'work'
        ? 'work_selection'
        : null;
    if (!selectionField) {
      throw new Error(`agent_context_orientation_selection_invalid:${selection}`);
    }
    const selected: any = packet.orientation_brief[selectionField];
    if (selected.mode === 'omitted') {
      return {
        schema: 'narada.agent_context.orientation_selection_read.v1',
        status: 'omitted',
        source_mutation: false,
        ordinary_work_gate: packet.ordinary_work_gate,
        selection_kind: selection,
        manifest_ref: packet.manifest_ref,
        selection: selected,
        projection: null,
      };
    }
    const readback: any = readOrientationManifestGeneration({
      siteRoot,
      dbPath,
      manifestId: evidence.manifestId,
      admissionReceipt: evidence.admission,
    });
    const compartment: any = selection === 'continuity'
      ? 'continuity'
      : 'work_orientation';
    const entry: any = readback.manifest.entries.find(
      (candidate: any) => candidate.compartment === compartment
        && candidate.projection_status === 'available',
    );
    if (
      !entry
      || entry.artifact_ref !== selected.artifact_ref
      || entry.revision !== selected.revision
    ) {
      throw new Error(
        `agent_context_orientation_selection_binding_mismatch:${selection}`,
      );
    }
    return {
      schema: 'narada.agent_context.orientation_selection_read.v1',
      status: 'exact',
      source_mutation: false,
      ordinary_work_gate: packet.ordinary_work_gate,
      selection_kind: selection,
      manifest_ref: packet.manifest_ref,
      selection: selected,
      projection: {
        entry_id: entry.entry_id,
        source_authority_ref: entry.source_authority_ref,
        artifact_ref: entry.artifact_ref,
        revision: entry.revision,
        observed_at: entry.observed_at,
        revalidation_rule: entry.revalidation_rule,
        payload: entry.payload,
        rendered_text: entry.rendered_text,
      },
    };
  }
  if (!stepId) return packet;
  return recordOrientationRequiredRead({
    siteRoot,
    dbPath,
    admissionReceipt: evidence.admission,
    deliveryReceipt: evidence.deliveryReceipt,
    brief: packet.orientation_brief,
    stepId,
    byteOffset: toolArgs.offset ?? 0,
  });
}

function orientationAcknowledge(toolArgs: any = {}) {
  const evidence = exactOrientationEntryEvidence();
  const entryPacket: any = readOrientationEntryPacket({
    siteRoot,
    dbPath,
    manifestId: evidence.manifestId,
    admissionReceipt: evidence.admission,
    deliveryReceipt: evidence.deliveryReceipt,
  });
  const record: any = recordOrientationAcknowledgement({
    siteRoot,
    dbPath,
    admissionReceipt: evidence.admission,
    deliveryReceipt: evidence.deliveryReceipt,
    brief: entryPacket.orientation_brief,
  });
  projectOrientationAcknowledgement({
    siteRoot,
    entryFile: process.env.NARADA_ORIENTATION_ENTRY_FILE,
    acknowledgement: record.acknowledgement,
  });
  return record;
}

function startupSequence(toolArgs: any = {}) {
  for (const forbidden of ['checkpoint_id', 'checkpoint_startup', 'generated_at']) {
    if (toolArgs[forbidden] !== undefined) {
      return {
        schema: 'narada.agent_context.orientation_delivery.v1',
        status: 'blocked',
        source_mutation: false,
        reason: 'orientation_startup_exact_generation_only',
        rejected_argument: forbidden,
        required_next_step: 'Use agent_context_hydrate_current for a separately identified diagnostic candidate generation.',
      };
    }
  }
  // Preserve conflict detection for callers of the compatibility alias, while
  // making the authoritative read use only the exact Carrier-entry evidence.
  resolveExactOrientationEvidence(toolArgs);
  exactOrientationManifestId(toolArgs);
  const packet: any = orientationRead();
  return {
    ...packet,
    compatibility_alias: 'agent_context_startup_sequence',
    canonical_tool: 'agent_orientation_read',
  };
}

function listSessions(toolArgs: any = {}) {
  return withDb((db: any) => listAgentStartSessions({
    db,
    identity: toolArgs.identity ?? null,
    limit: toolArgs.limit ?? 100,
    offset: toolArgs.offset ?? 0,
  }));
}

function checkpointPayload(toolArgs: any, agentId: any, checkpointAt: any, checkpointId: any): Record<string, any> {
  return {
    schema: 'narada.agent_context.checkpoint.v1',
    site_id: siteId,
    site_root: siteRoot,
    agent_id: agentId,
    checkpoint_at: checkpointAt,
    active_task: toolArgs.active_task ?? null,
    files_touched: arrayValue(toolArgs.files_touched),
    key_decisions: arrayValue(toolArgs.key_decisions),
    open_questions: arrayValue(toolArgs.open_questions),
    git_head: toolArgs.git_head ?? null,
    last_workboard_check_at: toolArgs.last_workboard_check_at ?? null,
    next_intended_action: toolArgs.next_intended_action ?? null,
    authority_basis: toolArgs.authority_basis ?? null,
    continuation_blockers: arrayValue(toolArgs.continuation_blockers),
    evidence_refs: arrayValue(toolArgs.evidence_refs),
    worktree_state: toolArgs.worktree_state ?? null,
    tactical_resume_notes: arrayValue(toolArgs.tactical_resume_notes),
    continuation: normalizeContinuation(toolArgs.continuation, checkpointId, checkpointAt),
    continuation_ref: normalizeContinuationRef(toolArgs.continuation_ref),
  };
}

function rowToCheckpoint(row: any) {
  const payload = parseJson(row.payload_json, {});
  return {
    checkpoint_id: row.checkpoint_id,
    agent_id: row.agent_id,
    session_id: row.session_id ?? null,
    checkpoint_at: row.checkpoint_at,
    active_task: parseJson(row.active_task_json, null),
    files_touched: parseJson(row.files_touched_json, []),
    key_decisions: parseJson(row.key_decisions_json, []),
    open_questions: parseJson(row.open_questions_json, []),
    git_head: row.git_head ?? null,
    last_workboard_check_at: payload.last_workboard_check_at ?? null,
    next_intended_action: payload.next_intended_action ?? null,
    authority_basis: payload.authority_basis ?? null,
    continuation_blockers: payload.continuation_blockers ?? [],
    evidence_refs: payload.evidence_refs ?? [],
    worktree_state: payload.worktree_state ?? null,
    tactical_resume_notes: payload.tactical_resume_notes ?? [],
    continuation: payload.continuation ?? null,
    continuation_ref: payload.continuation_ref ?? null,
    continuation_projection: payload.continuation_projection ?? null,
    payload,
  };
}

function assertAgentContextIdentity(agentId: any) {
  if (typeof agentId !== 'string' || agentId.trim() === '') {
    throw new Error(`agent_context_identity_invalid: ${agentId}`);
  }
  const roster = validateIdentityAgainstRoster(siteRoot, agentId);
  if (!roster.valid) throw new Error(roster.error);
  return roster;
}

function requiredString(value: any, key: any) {
  const result = value?.[key];
  if (typeof result !== 'string' || result.trim() === '') {
    throw new Error(`${key}_required`);
  }
  return result;
}

function optionalCheckpointId(value: any) {
  const checkpointId = value?.checkpoint_id;
  if (checkpointId == null) return null;
  if (typeof checkpointId !== 'string' || checkpointId.trim() === '') {
    throw new Error('checkpoint_id_invalid');
  }
  return checkpointId.trim();
}

function arrayValue(value: any) {
  return Array.isArray(value) ? value : [];
}

function jsonOrNull(value: any) {
  return value == null ? null : JSON.stringify(value);
}

function parseJson(value: any, fallback: any) {
  if (value == null || value === '') return fallback;
  try {
    return JSON.parse(value);
  } catch {
    return fallback;
  }
}
