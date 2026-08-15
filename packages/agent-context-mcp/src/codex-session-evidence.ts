
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { boundedCollection } from '@narada-core/mcp-transport/bounded-collection';

const CODEX_SESSION_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function isCodexSessionId(value: any) {
  return typeof value === 'string' && CODEX_SESSION_ID_RE.test(value);
}

export function defaultCodexHome() {
  return process.env.CODEX_HOME || join(homedir(), '.codex');
}

export function discoverCodexSessionEvidence({
  siteRoot,
  admissionId,
  identity,
  codexHome = defaultCodexHome(),
  limit = 200,
  offset = 0,
  candidateLimit = 20,
}: any = {}) {
  if (!siteRoot) throw new Error('siteRoot is required');
  if (!admissionId) throw new Error('admission_id is required');

  const sessionsRoot = join(codexHome, 'sessions');
  const allFiles = existsSync(sessionsRoot) ? listSessionFiles(sessionsRoot) : [];
  const normalizedOffset = Math.max(Math.trunc(Number(offset) || 0), 0);
  const normalizedLimit = Math.min(Math.max(Math.trunc(Number(limit) || 200), 1), 500);
  const normalizedCandidateLimit = Math.min(Math.max(Math.trunc(Number(candidateLimit) || 20), 1), 100);
  const filePage = boundedCollection(allFiles, {
    offset: normalizedOffset,
    limit: normalizedLimit,
    truncationReason: 'codex_session_scan_page',
  });
  const files = filePage.items;
  const siteRootResolved = normalizePath(siteRoot);
  const candidates = [];

  for (const filePath of files) {
    const candidate = readCandidate({ filePath, admissionId, identity, siteRootResolved });
    if (candidate) candidates.push(candidate);
  }

  const admissible = candidates.filter((candidate: any) => candidate.admissible);
  const scanComplete = normalizedOffset === 0 && !filePage.has_more;
  const status = !scanComplete
    ? 'incomplete'
    : admissible.length === 1
    ? 'admissible'
    : admissible.length > 1
      ? 'ambiguous'
      : 'not_found';

  return {
    schema: 'narada.codex.session_evidence.discovery.v0',
    status,
    admission_id: admissionId,
    identity: identity ?? null,
    codex_home: codexHome,
    sessions_root: sessionsRoot,
    candidate_count: candidates.length,
    admissible_count: admissible.length,
    selected: scanComplete && admissible.length === 1 ? admissible[0] : null,
    candidates: candidates.slice(0, normalizedCandidateLimit),
    scan: filePage,
    candidate_page: boundedCollection(candidates, {
      limit: normalizedCandidateLimit,
      truncationReason: 'codex_session_candidate_limit',
    }),
    admissibility_complete: scanComplete,
    exact_resume_proof: {
      status: 'missing_mcp_capability',
      reason: 'Narada has no approved MCP capability to execute `codex resume <codex_session_id>` and verify exact resume without --last or picker state.',
      required_shape: 'codex resume <codex_session_id>',
      forbidden_shapes: ['codex resume --last', 'ambient picker selection', 'manual session selection as authority'],
    },
  };
}

export function verifyCodexExactResume({
  codexSessionId,
  codexSessionFile = null,
  admissionId = null,
}: any = {}) {
  if (!codexSessionId) throw new Error('codex_session_id is required');
  if (!isCodexSessionId(codexSessionId)) throw new Error(`codex_session_id_invalid: ${codexSessionId}`);

  const commandShape = `codex resume ${codexSessionId}`;
  return {
    schema: 'narada.codex.exact_resume_verification.v0',
    status: 'unavailable',
    code: 'codex_exact_resume_verification_unavailable',
    codex_session_id: codexSessionId,
    codex_session_file: codexSessionFile,
    admission_id: admissionId,
    command_shape: commandShape,
    blocked_reason: 'No approved noninteractive MCP capability exists to run this command and verify the resumed Codex session id.',
    required_capability: {
      kind: 'mcp_tool',
      command_shape: commandShape,
      must_verify: [
        'Codex resumes without --last.',
        'Codex does not present or depend on an ambient picker.',
        'The resumed session metadata id equals codex_session_id.',
      ],
    },
    forbidden_shapes: ['codex resume --last', 'ambient picker selection', 'manual session selection as authority'],
  };
}

export function extractCodexSessionEvidencePacket({
  siteRoot,
  admissionId,
  identity,
  codexHome = defaultCodexHome(),
  searchText,
  outputPath = 'kb/operations/codex-session-evidence-packet.json',
  limit = 200,
  offset = 0,
  candidateLimit = 20,
}: any = {}) {
  if (!siteRoot) throw new Error('siteRoot is required');
  if (!admissionId) throw new Error('admission_id is required');
  if (!searchText || typeof searchText !== 'string' || searchText.trim().length < 4) {
    throw new Error('search_text_required');
  }

  const outputAbsolute = resolveUnderSiteRoot(siteRoot, outputPath);
  const discovery = discoverCodexSessionEvidence({ siteRoot, admissionId, identity, codexHome, limit, offset, candidateLimit });
  if (discovery.status !== 'admissible' || !discovery.selected?.codex_session_file) {
    return {
      schema: 'narada.codex.session_evidence.extraction.v0',
      status: 'blocked_session_not_admissible',
      admission_id: admissionId,
      identity: identity ?? null,
      search_text: searchText,
      output_path: normalizeRelativePath(relative(siteRoot, outputAbsolute)),
      discovery,
      blocker: 'No exactly one admissible Codex session was found. Extraction requires a valid session id, matching Site cwd, and the admission marker in the transcript.',
    };
  }

  const sourceFile = discovery.selected.codex_session_file;
  const matches = extractMatchingTranscriptEntries({
    sourceFile,
    searchText,
  });
  const packet = {
    schema: 'narada.codex.session_evidence.extraction.v0',
    status: matches.length > 0 ? 'extracted' : 'not_found',
    extracted_at: new Date().toISOString(),
    admission_id: admissionId,
    identity: identity ?? null,
    codex_home: codexHome,
    search_text: searchText,
    output_path: normalizeRelativePath(relative(siteRoot, outputAbsolute)),
    provenance: {
      source_file: sourceFile,
      codex_session_id: discovery.selected.codex_session_id,
      file_name: discovery.selected.file_name,
      session_timestamp: discovery.selected.session_timestamp,
      file_last_write_time: discovery.selected.file_last_write_time,
      extraction_method: 'agent_context_extract_codex_session_evidence_packet literal search over admissible Codex JSONL transcript',
      admission_guard: discovery.selected.guards,
    },
    match_count: matches.length,
    numbered_item_count: countNumberedIssueLines(matches.map((match: any) => match.text).join('\n')),
    matches,
    verification: {
      contains_list_body: countNumberedIssueLines(matches.map((match: any) => match.text).join('\n')) > 0,
      pointer_only: matches.length > 0 && countNumberedIssueLines(matches.map((match: any) => match.text).join('\n')) === 0,
    },
  };

  mkdirSync(dirname(outputAbsolute), { recursive: true });
  writeFileSync(outputAbsolute, `${JSON.stringify(packet, null, 2)}\n`, 'utf8');
  return packet;
}

function listSessionFiles(root: any) {
  const found = [];
  const stack = [root];
  while (stack.length > 0) {
    const current = stack.pop();
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const fullPath = join(current, entry.name);
      if (entry.isDirectory()) {
        stack.push(fullPath);
      } else if (entry.isFile() && entry.name.endsWith('.jsonl')) {
        const stat = statSync(fullPath);
        found.push({ path: fullPath, mtimeMs: stat.mtimeMs, size: stat.size });
      }
    }
  }
  return found
    .sort((a: any, b: any) => b.mtimeMs - a.mtimeMs)
    .map((entry: any) => entry.path);
}

function readCandidate({ filePath, admissionId, identity, siteRootResolved }: any) {
  let content;
  try {
    content = readFileSync(filePath, 'utf8');
  } catch (error) {
    return {
      status: 'unreadable',
      codex_session_file: filePath,
      error: error instanceof Error ? error.message : String(error),
      admissible: false,
    };
  }
  if (!content.trim()) return null;

  const meta = parseSessionMeta(content);
  const filenameId = parseSessionIdFromFilename(filePath);
  const sessionId = meta?.payload?.id ?? filenameId;
  const cwd = meta?.payload?.cwd ?? null;
  const markers = {
    admission_id: content.includes(admissionId),
    identity: identity ? content.includes(identity) : null,
  };
  const cwdMatches = cwd ? normalizePath(cwd) === siteRootResolved : false;
  const idValid = isCodexSessionId(sessionId);
  const admissible = idValid && cwdMatches && markers.admission_id === true;

  return {
    status: admissible ? 'admissible_candidate' : 'candidate',
    codex_session_id: sessionId ?? null,
    codex_session_file: filePath,
    file_name: basename(filePath),
    file_size: statSync(filePath).size,
    file_last_write_time: statSync(filePath).mtime.toISOString(),
    session_timestamp: meta?.payload?.timestamp ?? null,
    cwd,
    originator: meta?.payload?.originator ?? null,
    cli_version: meta?.payload?.cli_version ?? null,
    source: meta?.payload?.source ?? null,
    markers,
    guards: {
      session_id_valid: idValid,
      cwd_matches_site_root: cwdMatches,
      admission_id_marker_present: markers.admission_id === true,
    },
    admissible,
    admissibility_reason: admissible
      ? 'session_meta id is valid, cwd matches Site root, and session transcript contains the admission id marker'
      : 'requires valid session_meta id, matching cwd, and admission id marker in session transcript',
  };
}

function extractMatchingTranscriptEntries({ sourceFile, searchText }: any) {
  const lines = readFileSync(sourceFile, 'utf8').split(/\r?\n/);
  const needle = searchText.toLowerCase();
  const matches = [];
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index].trim();
    if (!line) continue;
    let parsed;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }
    const text = extractTranscriptText(parsed);
    if (!text || !text.toLowerCase().includes(needle)) continue;
    matches.push({
      line_number: index + 1,
      timestamp: parsed.timestamp ?? null,
      type: parsed.type ?? null,
      payload_type: parsed.payload?.type ?? null,
      role: parsed.payload?.role ?? null,
      phase: parsed.payload?.phase ?? null,
      text,
    });
  }
  return matches;
}

function extractTranscriptText(entry: any) {
  const payload = entry?.payload;
  if (!payload || typeof payload !== 'object') return '';
  if (typeof payload.message === 'string') return payload.message;
  if (typeof payload.text === 'string') return payload.text;
  if (Array.isArray(payload.content)) {
    return payload.content
      .map((block: any) => {
        if (!block || typeof block !== 'object') return '';
        if (typeof block.text === 'string') return block.text;
        return '';
      })
      .filter(Boolean)
      .join('\n');
  }
  return '';
}

function countNumberedIssueLines(text: any) {
  return text
    .split(/\r?\n/)
    .filter((line: any) => /^\s*\d+\.\s+/.test(line))
    .length;
}

function parseSessionMeta(content: any) {
  const firstLine = content.split(/\r?\n/, 1)[0];
  try {
    const parsed = JSON.parse(firstLine);
    return parsed?.type === 'session_meta' ? parsed : null;
  } catch {
    return null;
  }
}

function parseSessionIdFromFilename(filePath: any) {
  const match = basename(filePath).match(/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\.jsonl$/i);
  return match?.[1] ?? null;
}

function normalizePath(value: any) {
  return resolve(value).toLowerCase();
}

function normalizeRelativePath(path: any) {
  return path.replace(/\\/g, '/');
}

function resolveUnderSiteRoot(siteRoot: any, outputPath: any) {
  const absolute = resolve(siteRoot, outputPath);
  const rel = relative(siteRoot, absolute);
  if (rel === '..' || rel.startsWith('..\\') || rel.startsWith('../')) {
    throw new Error(`output_path_outside_site_root: ${outputPath}`);
  }
  return absolute;
}
