import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { buildAllowedRoots, normalizeAllowedRoots, parseTrustedProjectRootsFromTrustConfig, resolveAllowedPath, resolveAnchoredAllowedRoot, rootEntriesToRoots } from '../src/policy.js';
import { createServerState, errorDiagnostic, handleRequest, handleRequestAsync, listTools } from '../src/main.js';
import { parsePatch } from '../src/patch-apply.js';
import { buildGuidanceResult } from '../src/guidance.js';
import { RIPGREP_FIELD_SEPARATOR, grepMatchObject, runRipgrepPageAsync } from '../src/search.js';

type DynamicTestValue = string & DynamicTestValue[] & {
  [key: string]: DynamicTestValue;
  [index: number]: DynamicTestValue;
};

type JsonRpcTestResponse = DynamicTestValue & {
  result: DynamicTestValue;
  error: DynamicTestValue;
};

function call(state: any, id: string | number, name: string, args: Record<string, unknown> = {}) {
  return handleRequest({
    jsonrpc: '2.0',
    id,
    method: 'tools/call',
    params: { name, arguments: args },
  }, state) as unknown as JsonRpcTestResponse;
}

function parseFirstJsonRpcFrame(output: string) {
  const headerEnd = output.indexOf('\r\n\r\n');
  assert.notEqual(headerEnd, -1, 'expected framed JSON-RPC response');
  const header = output.slice(0, headerEnd);
  const match = /Content-Length:\s*(\d+)/i.exec(header);
  assert.ok(match, 'expected Content-Length response header');
  const bodyStart = headerEnd + 4;
  const body = output.slice(bodyStart, bodyStart + Number(match[1]));
  return JSON.parse(body);
}

function parseJsonRpcFrames(output: string) {
  const frames: any[] = [];
  let remaining = output;
  while (remaining.length > 0) {
    const headerEnd = remaining.indexOf('\r\n\r\n');
    if (headerEnd < 0) break;
    const header = remaining.slice(0, headerEnd);
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    assert.ok(match, 'expected Content-Length response header');
    const bodyStart = headerEnd + 4;
    const bodyEnd = bodyStart + Number(match[1]);
    frames.push(JSON.parse(remaining.slice(bodyStart, bodyEnd)));
    remaining = remaining.slice(bodyEnd);
  }
  return frames;
}

function sha256(value: unknown) {
  return createHash('sha256').update(String(value)).digest('hex');
}

function deliveredReadContent(response: JsonRpcTestResponse) {
  const text = String(response.result.content[0].text);
  const marker = '\ncontent:\n';
  const index = text.indexOf(marker);
  assert.notEqual(index, -1, 'expected filesystem read content delivery marker');
  return text.slice(index + marker.length);
}

const tempRoot = mkdtempSync(join(tmpdir(), 'local-filesystem-mcp-'));
try {
  const trusted = join(tempRoot, 'trusted');
  const other = join(tempRoot, 'other');
  mkdirSync(trusted, { recursive: true });
  mkdirSync(other, { recursive: true });
  writeFileSync(join(trusted, 'a.txt'), 'alpha\nbeta\n', 'utf8');
  writeFileSync(join(trusted, 'binary.bin'), Buffer.from([0, 1, 2, 3]));
  writeFileSync(join(trusted, 'large-read.txt'), `${'x'.repeat(7000)}\n`, 'utf8');
  writeFileSync(join(trusted, 'bounded-read.txt'), Array.from({ length: 700 }, (_, index) => `line-${String(index + 1).padStart(3, '0')} ${'x'.repeat(70)}`).join('\n'), 'utf8');
  writeFileSync(join(trusted, 'grep-one.txt'), 'alpha\nneedle one\n', 'utf8');
  writeFileSync(join(trusted, 'grep-two.txt'), 'needle two\nplain\n', 'utf8');
  const sourcePath = join(trusted, 'packages', 'task-lifecycle-mcp', 'src');
  mkdirSync(sourcePath, { recursive: true });
  writeFileSync(join(sourcePath, 'mcp-freshness-service.ts'), "import { createHash } from 'node:crypto';\nexport const classifierFalsePositive = false;\n", 'utf8');
  const danglingRoot = join(tempRoot, 'dangling-root');
  mkdirSync(danglingRoot, { recursive: true });
  let danglingSymlinkCreated = false;
  try {
    symlinkSync(join(danglingRoot, 'missing-target'), join(danglingRoot, 'dangling-link'));
    danglingSymlinkCreated = true;
  } catch {
    danglingSymlinkCreated = false;
  }

  const revolutionRoot = join(trusted, 'OneDrive - Global Maxima LLC', '!Business', '!Clients', '!Revolution', '.narada');
  mkdirSync(join(revolutionRoot, 'config'), { recursive: true });
  writeFileSync(join(revolutionRoot, 'config', 'config.json'), '{"site":"revolution"}\n', 'utf8');
  writeFileSync(join(revolutionRoot, 'config', 'settings.yaml'), 'site: revolution\n', 'utf8');

  const largeRoot = join(trusted, 'large-search');
  mkdirSync(largeRoot, { recursive: true });
  for (let i = 0; i < 120; i += 1) {
    writeFileSync(join(largeRoot, `very-long-file-name-${String(i).padStart(3, '0')}-${'x'.repeat(60)}.txt`), 'needle\n', 'utf8');
  }
  const largeMetricsRoot = join(trusted, 'large-metrics');
  mkdirSync(largeMetricsRoot, { recursive: true });
  for (let i = 0; i < 505; i += 1) {
    writeFileSync(join(largeMetricsRoot, `metric-${String(i).padStart(3, '0')}.txt`), 'line\n', 'utf8');
  }
  const metricsExclusionRoot = join(trusted, 'metrics-exclusion');
  mkdirSync(metricsExclusionRoot, { recursive: true });
  writeFileSync(join(metricsExclusionRoot, 'included.txt'), 'included\n', 'utf8');
  for (let i = 0; i < 105; i += 1) {
    writeFileSync(join(metricsExclusionRoot, `excluded-${String(i).padStart(3, '0')}.txt`), 'excluded\n', 'utf8');
  }
  const largeGrepRoot = join(trusted, 'large-grep-output');
  mkdirSync(largeGrepRoot, { recursive: true });
  const largeGrepLine = `${'needle '.repeat(20)}${'x'.repeat(20_000)}\n`;
  for (let i = 0; i < 80; i += 1) {
    writeFileSync(join(largeGrepRoot, `large-grep-${String(i).padStart(3, '0')}.txt`), largeGrepLine, 'utf8');
  }

  const ignoredRoot = join(trusted, 'glob-ignore');
  mkdirSync(join(ignoredRoot, 'src'), { recursive: true });
  mkdirSync(join(ignoredRoot, 'node_modules', 'pkg'), { recursive: true });
  mkdirSync(join(ignoredRoot, 'dist'), { recursive: true });
  mkdirSync(join(ignoredRoot, 'custom-skip'), { recursive: true });
  mkdirSync(join(ignoredRoot, '.ai', 'runtime'), { recursive: true });
  writeFileSync(join(ignoredRoot, 'src', 'keep.txt'), 'keep\n', 'utf8');
  writeFileSync(join(ignoredRoot, 'node_modules', 'pkg', 'dependency.txt'), 'skip\n', 'utf8');
  writeFileSync(join(ignoredRoot, 'dist', 'bundle.txt'), 'skip\n', 'utf8');
  writeFileSync(join(ignoredRoot, 'custom-skip', 'custom.txt'), 'custom\n', 'utf8');
  writeFileSync(join(ignoredRoot, '.ai', 'runtime', 'artifact.txt'), 'generated\n', 'utf8');

  const configPath = join(tempRoot, 'config.toml');
  writeFileSync(configPath, `
[projects.'${trusted.replace(/\\/g, '\\\\')}']
trust_level = "trusted"

[projects.'${other.replace(/\\/g, '\\\\')}']
trust_level = "untrusted"
`, 'utf8');

  const rootEntries = parseTrustedProjectRootsFromTrustConfig(configPath);
  const roots = rootEntries.map((entry: any) => entry.root);
  assert.deepEqual(roots, [resolve(trusted)]);
  assert.equal(resolveAllowedPath(join(trusted, 'a.txt'), rootEntries).path, resolve(join(trusted, 'a.txt')));
  assert.throws(() => resolveAllowedPath(join(other, 'x.txt'), rootEntries), /path_outside_allowed_roots/);
  assert.throws(() => parsePatch(`*** Begin Patch\n*** Move to: missing-update.txt\n*** End Patch\n`), /patch_move_without_update_file/);
  assert.equal(parsePatch(`\n\n*** Begin Patch\n*** Add File: blank-leading.txt\n+ok\n*** End Patch\n`).length, 1);
  const windowsCountMatch = grepMatchObject(`C:/repo/file.txt${RIPGREP_FIELD_SEPARATOR}12`, 'count_matches');
  assert.equal(windowsCountMatch.path, 'C:/repo/file.txt');
  assert.equal(windowsCountMatch.count, 12);
  const cancelledSearch = new AbortController();
  cancelledSearch.abort();
  await assert.rejects(
    () => runRipgrepPageAsync(['--files', trusted], {
      operation: 'fs_glob_search',
      noMatchStatus: 0,
      offset: 0,
      limit: 10,
      timeoutMs: 60_000,
      freshness: null,
      diagnosticError: (_code: any, message: any) => new Error(message),
      abortSignal: cancelledSearch.signal,
    }),
    /fs_glob_search_cancelled/
  );


  const readToolNames = listTools('read').map((tool: any) => tool.name);
  assert.ok(readToolNames.includes('fs_guidance'));
  assert.ok(readToolNames.includes('fs_read_file'));
  assert.ok(readToolNames.includes('fs_read_file_range'));
  assert.ok(readToolNames.includes('fs_grep_search'));
  assert.ok(readToolNames.includes('fs_repository_inventory'));
  assert.ok(readToolNames.includes('fs_file_metrics'));
  const globToolDescription = listTools('read').find((tool: any) => tool.name === 'fs_glob_search')?.description;
  assert.match(String(globToolDescription), /Empty matches return ok with count 0/);
  const inventoryToolDescription = listTools('read').find((tool: any) => tool.name === 'fs_repository_inventory')?.description;
  assert.match(String(inventoryToolDescription), /generated runtime artifacts/);
  const fileMetricsToolDescription = listTools('read').find((tool: any) => tool.name === 'fs_file_metrics')?.description;
  assert.match(String(fileMetricsToolDescription), /metadata-only file metrics/);
  const grepToolDescription = listTools('read').find((tool: any) => tool.name === 'fs_grep_search')?.description;
  assert.match(String(grepToolDescription), /line-numbered matches/);
  assert.match(String(grepToolDescription), /empty matches return ok with count 0/);
  assert.equal(readToolNames.includes('mcp_output_show'), false);
  assert.equal(readToolNames.includes('fs_write_file'), false);
  assert.equal(readToolNames.includes('fs_apply_patch'), false);
  assert.equal(readToolNames.includes('fs_patch_outcome_show'), true);
  const patchOutcomeTool = listTools('read').find((tool: any) => tool.name === 'fs_patch_outcome_show') as DynamicTestValue;
  assert.equal(patchOutcomeTool.annotations.readOnlyHint, false);
  assert.equal(patchOutcomeTool.annotations.idempotentHint, true);
  const recoveryGuidance = buildGuidanceResult({ workflow: 'bounded_reads_and_patch_recovery' }) as DynamicTestValue;
  assert.equal(Array.isArray(recoveryGuidance.patch_recovery.sequence), true);
  assert.match(String(recoveryGuidance.patch_recovery.sequence[2]), /fs_patch_outcome_show/);
  assert.match(String(recoveryGuidance.patch_recovery.statuses.failed_before_mutation), /no mutation/);
  const inventoryGuidance = buildGuidanceResult({ workflow: 'repository_inventory' }) as DynamicTestValue;
  assert.equal(Array.isArray(inventoryGuidance.repository_inventory.sequence), true);
  assert.match(String(inventoryGuidance.repository_inventory.sequence[3]), /git_changed_summary/);
  assert.match(String(inventoryGuidance.repository_inventory.default_behavior), /excluded/);

  const writeToolNames = listTools('write').map((tool: any) => tool.name);
  assert.ok(writeToolNames.includes('fs_guidance'));
  assert.ok(writeToolNames.includes('fs_read_file'));
  assert.equal(writeToolNames.includes('mcp_output_show'), false);
  assert.ok(writeToolNames.includes('fs_write_file'));
  assert.ok(writeToolNames.includes('fs_str_replace_file'));
  assert.ok(writeToolNames.includes('fs_replace_range'));
  assert.ok(writeToolNames.includes('fs_apply_patch'));
  assert.ok(writeToolNames.includes('fs_move_path'));
  assert.ok(writeToolNames.includes('fs_create_directory'));
  assert.ok(writeToolNames.includes('fs_rename_directory'));
  assert.ok(writeToolNames.includes('fs_delete_directory'));
  const applyPatchToolDescription = listTools('write').find((tool: any) => tool.name === 'fs_apply_patch')?.description;
  assert.match(String(applyPatchToolDescription), /unified diff or Codex-style apply_patch/);
  const applyPatchToolSchema = listTools('write').find((tool: any) => tool.name === 'fs_apply_patch')?.inputSchema as Record<string, DynamicTestValue>;
  assert.equal(applyPatchToolSchema.properties.timeout_ms.type, 'integer');

  const fakeUserHome = join(tempRoot, 'fake-user-home');
  const fakeCodexRoot = join(fakeUserHome, '.codex');
  mkdirSync(fakeCodexRoot, { recursive: true });
  writeFileSync(join(fakeCodexRoot, 'config.toml'), 'model = "test"\n', 'utf8');
  const anchoredEntry = resolveAnchoredAllowedRoot('user_home:.codex', { user_home: fakeUserHome });
  assert.equal(anchoredEntry.root, resolve(fakeCodexRoot));
  assert.equal(anchoredEntry.provenance.anchor, 'user_home');
  assert.equal(anchoredEntry.provenance.relative_path, '.codex');
  assert.throws(() => resolveAnchoredAllowedRoot('user_home:../outside', { user_home: fakeUserHome }), /anchored_allowed_root_path_escapes_anchor/);
  assert.throws(() => resolveAnchoredAllowedRoot('workspace:.codex', { user_home: fakeUserHome }), /anchored_allowed_root_unknown_anchor/);
  const anchoredRootEntries = buildAllowedRoots({ anchoredRoots: ['user_home:.codex'], anchors: { user_home: fakeUserHome } });
  assert.deepEqual(rootEntriesToRoots(anchoredRootEntries), [resolve(fakeCodexRoot)]);
  const anchoredConfigPath = join(tempRoot, 'anchored-roots.json');
  writeFileSync(anchoredConfigPath, JSON.stringify({ anchored_allowed_roots: ['user_home:.codex'] }), 'utf8');
  const anchoredConfigEntries = buildAllowedRoots({ rootsConfigPath: anchoredConfigPath, anchors: { user_home: fakeUserHome } });
  assert.deepEqual(rootEntriesToRoots(anchoredConfigEntries), [resolve(fakeCodexRoot)]);
  assert.equal(anchoredConfigEntries[0].provenance.source, 'roots_config_anchored_allowed_root');
  const anchoredState = createServerState({ mode: 'read', anchoredAllowedRoots: ['user_home:.codex'], anchors: { user_home: fakeUserHome }, outputRoot: tempRoot });
  const anchoredRead = call(anchoredState, 1002, 'fs_read_file', { path: join(fakeCodexRoot, 'config.toml') });
  assert.equal(deliveredReadContent(anchoredRead), 'model = "test"');
  const doctorResponse = call(anchoredState, 1003, 'fs_doctor');
  assert.equal(doctorResponse.result.structuredContent.allowed_roots[0], resolve(fakeCodexRoot));
  assert.equal(doctorResponse.result.structuredContent.allowed_root_entries[0].provenance.flag, '--anchored-allowed-root');

  const readState = createServerState({ mode: 'read', allowedRoots: [trusted], outputRoot: tempRoot });
  const siteRoot = join(tempRoot, 'site-root');
  mkdirSync(join(siteRoot, '.narada'), { recursive: true });
  writeFileSync(join(siteRoot, '.narada', 'secrets.json'), JSON.stringify({ env: { LOCAL_FILESYSTEM_TEST_SECRET: 'from-site-secret' } }), 'utf8');
  const handoffRoot = join(tempRoot, 'handoff-root');
  const nonAdmittedTempRoot = join(tempRoot, 'non-admitted-temp');
  mkdirSync(handoffRoot, { recursive: true });
  mkdirSync(nonAdmittedTempRoot, { recursive: true });
  writeFileSync(join(handoffRoot, 'narada-worker-runtime-handoff.md'), 'handoff\n', 'utf8');
  writeFileSync(join(nonAdmittedTempRoot, 'handoff.md'), 'blocked\n', 'utf8');
  writeFileSync(join(siteRoot, '.narada', 'allowed-roots.json'), JSON.stringify({ temp_allowed_roots: [handoffRoot] }), 'utf8');
  const originalFilesystemSecret = process.env.LOCAL_FILESYSTEM_TEST_SECRET;
  delete process.env.LOCAL_FILESYSTEM_TEST_SECRET;
  const secretState = createServerState({ mode: 'read', siteRoot, allowedRoots: [trusted], outputRoot: tempRoot });
  assert.equal((secretState.env as NodeJS.ProcessEnv).LOCAL_FILESYSTEM_TEST_SECRET, 'from-site-secret');
  const handoffRead = call(secretState, 213, 'fs_read_file', { path: join(handoffRoot, 'narada-worker-runtime-handoff.md') });
  assert.equal(deliveredReadContent(handoffRead), 'handoff');
  const blockedTempRead = call(secretState, 214, 'fs_read_file', { path: join(nonAdmittedTempRoot, 'handoff.md') });
  assert.equal(blockedTempRead.error.data.code, 'path_outside_allowed_roots');
  assert.equal(process.env.LOCAL_FILESYSTEM_TEST_SECRET, undefined);
  if (originalFilesystemSecret === undefined) delete process.env.LOCAL_FILESYSTEM_TEST_SECRET;
  else process.env.LOCAL_FILESYSTEM_TEST_SECRET = originalFilesystemSecret;
  const initResponse = handleRequest({
    jsonrpc: '2.0',
    id: 1000,
    method: 'initialize',
    params: { protocolVersion: '2024-11-05' },
  }, readState) as unknown as JsonRpcTestResponse;
  assert.equal(initResponse.result.serverInfo.name, 'local-filesystem-read');
  const toolsListResponse = handleRequest({
    jsonrpc: '2.0',
    id: 1001,
    method: 'tools/list',
    params: {},
  }, readState) as unknown as JsonRpcTestResponse;
  assert.equal(toolsListResponse.result.tools.some((tool: any) => tool.name === 'fs_read_file'), true);

  const readResponse = call(readState, 1, 'fs_read_file', { path: join(trusted, 'a.txt'), limit: 1 });
  assert.equal(deliveredReadContent(readResponse), 'alpha');
  assert.equal(readResponse.result.structuredContent.content, undefined);
  assert.equal(readResponse.result.structuredContent.content_delivery.duplicated_in_structured_content, false);
  assert.equal(JSON.stringify(readResponse.result).split('alpha').length - 1, 1);
  assert.equal(readResponse.result.structuredContent.content_sha256, sha256('alpha\nbeta\n'));
  assert.equal(readResponse.result.structuredContent.content_window_sha256, sha256('alpha'));
  assert.equal(readResponse.result.structuredContent.next_offset, 2);
  assert.equal(readResponse.result.structuredContent.total_lines, null);
  assert.equal(readResponse.result.structuredContent.total_lines_exact, false);
  assert.equal(readResponse.result.structuredContent.total_lines_status, 'unknown_after_window');
  assert.equal(readResponse.result.structuredContent.line_window_complete, false);
  assert.equal(readResponse.result.content[0].text.startsWith(`path: ${join(trusted, 'a.txt')}`), true);
  assert.match(readResponse.result.content[0].text, /lines: 1-1 of unknown/);
  assert.match(readResponse.result.content[0].text, /total_lines_exact: false/);
  assert.match(readResponse.result.content[0].text, /total_lines_status: unknown_after_window/);
  assert.match(readResponse.result.content[0].text, /content:\nalpha$/);

  const boundedReadResponse = call(readState, 1011, 'fs_read_file', { path: join(trusted, 'bounded-read.txt'), limit: 100 });
  assert.equal(boundedReadResponse.result.structuredContent.schema, 'local.filesystem.read.v1');
  assert.equal(boundedReadResponse.result.structuredContent.returned_lines, 100);
  assert.equal(boundedReadResponse.result.structuredContent.next_offset, 101);
  assert.equal(boundedReadResponse.result.structuredContent.timeout_ms, 5000);
  assert.match(deliveredReadContent(boundedReadResponse), /line-100/);
  assert.equal(boundedReadResponse.result.structuredContent.content_sha256, sha256(readFileSync(join(trusted, 'bounded-read.txt'))));
  assert.equal(boundedReadResponse.result.structuredContent.content_hash_scope, 'full_file');
  assert.equal(boundedReadResponse.result.structuredContent.hash_source, 'live_file_bytes');
  assert.equal(boundedReadResponse.result.structuredContent.cache_used, false);
  assert.equal(boundedReadResponse.result.structuredContent.limit_adjusted, false);
  assert.equal(boundedReadResponse.result.structuredContent.pagination_required, true);

  const refusedClamp = call(readState, 10115, 'fs_read_file', { path: join(trusted, 'bounded-read.txt'), limit: 5000 });
  assert.equal(refusedClamp.error.data.code, 'fs_read_file_limit_exceeds_max');
  assert.equal(refusedClamp.error.data.details.requested_limit, 5000);
  assert.equal(refusedClamp.error.data.details.max_limit, 1000);
  assert.equal(refusedClamp.error.data.details.pagination_required, true);
  const refusedRangeClamp = call(readState, 10116, 'fs_read_file_range', { path: join(trusted, 'bounded-read.txt'), start_line: 1, end_line: 1001 });
  assert.equal(refusedRangeClamp.error.data.code, 'fs_read_file_range_limit_exceeds_max');

  const oversizedReadResponse = call(readState, 1012, 'fs_read_file', { path: join(trusted, 'bounded-read.txt'), limit: 500 });
  assert.equal(oversizedReadResponse.result.structuredContent.schema, 'local.filesystem.read_window_too_large.v1');
  assert.equal(oversizedReadResponse.result.structuredContent.status, 'truncated');
  assert.equal(oversizedReadResponse.result.structuredContent.content_omitted, true);
  assert.equal(oversizedReadResponse.result.structuredContent.path, join(trusted, 'bounded-read.txt'));
  assert.equal(oversizedReadResponse.result.structuredContent.limit, 500);
  assert.equal(oversizedReadResponse.result.structuredContent.recommended_tool, 'fs_read_file_range');
  assert.deepEqual(oversizedReadResponse.result.structuredContent.recommended_args, {
    path: join(trusted, 'bounded-read.txt'),
    start_line: 1,
    end_line: 100,
  });
  assert.equal(String(oversizedReadResponse.result.content[0].text).includes('line-500'), false);

  writeFileSync(join(trusted, 'read-timeout.txt'), `${Array.from({ length: 250000 }, (_, index) => `line-${index}`).join('\n')}\n`, 'utf8');
  const timeoutReadResponse = call(readState, 1013, 'fs_read_file', { path: join(trusted, 'read-timeout.txt'), offset: 200000, limit: 1000, timeout_ms: 1 });
  assert.equal(timeoutReadResponse.error.data.code, 'fs_read_file_timed_out');
  assert.equal(timeoutReadResponse.error.data.details.timeout_kind, 'read_timeout');
  assert.equal(timeoutReadResponse.error.data.details.recommended_tool, 'fs_read_file_range');
  assert.equal(timeoutReadResponse.error.data.details.recommended_args.start_line, 200000);
  assert.match(oversizedReadResponse.result.content[0].text, /read_window_too_large/);

  (readState.env as NodeJS.ProcessEnv).NARADA_LOCAL_FILESYSTEM_READ_WORKER_BLOCK_MS = '60000';
  const blockedReadStartedAt = Date.now();
  const requestPathTimeout = await handleRequestAsync({
    jsonrpc: '2.0',
    id: 1014,
    method: 'tools/call',
    params: { name: 'fs_read_file_range', arguments: { path: join(trusted, 'a.txt'), start_line: 1, end_line: 1, timeout_ms: 5 } },
  }, readState) as unknown as JsonRpcTestResponse;
  const blockedReadElapsedMs = Date.now() - blockedReadStartedAt;
  delete (readState.env as NodeJS.ProcessEnv).NARADA_LOCAL_FILESYSTEM_READ_WORKER_BLOCK_MS;
  assert.equal(requestPathTimeout.error.data.code, 'fs_read_file_range_timed_out');
  assert.equal(requestPathTimeout.error.data.details.timeout_kind, 'read_timeout');
  assert.equal(requestPathTimeout.error.data.details.timeout_ms, 5);
  assert.equal(typeof requestPathTimeout.error.data.details.elapsed_ms, 'number');
  assert.equal(blockedReadElapsedMs < 1000, true);
  assert.equal(requestPathTimeout.error.data.details.path, join(trusted, 'a.txt'));
  assert.equal(requestPathTimeout.error.data.details.recommended_tool, 'fs_read_file_range');
  assert.equal(requestPathTimeout.error.data.details.recommended_args.start_line, 1);
  assert.equal(requestPathTimeout.error.data.details.recommended_args.end_line, 1);

  const rangeResponse = call(readState, 11, 'fs_read_file_range', { path: join(trusted, 'a.txt'), start_line: 2, end_line: 2 });
  assert.equal(deliveredReadContent(rangeResponse), 'beta');
  assert.equal(rangeResponse.result.structuredContent.next_offset, null);
  assert.equal(rangeResponse.result.structuredContent.total_lines, 2);
  assert.equal(rangeResponse.result.structuredContent.total_lines_exact, true);
  assert.equal(rangeResponse.result.structuredContent.total_lines_status, 'exact');
  assert.equal(rangeResponse.result.structuredContent.line_window_complete, true);
  assert.match(rangeResponse.result.content[0].text, /lines: 2-2 of 2/);
  assert.match(rangeResponse.result.content[0].text, /content:\nbeta$/);

  const sourceReadResponse = call(readState, 111, 'fs_read_file', { path: join(sourcePath, 'mcp-freshness-service.ts') });
  assert.equal(sourceReadResponse.result.structuredContent.schema, 'local.filesystem.read.v1');
  assert.match(deliveredReadContent(sourceReadResponse), /classifierFalsePositive/);
  const sourceRangeResponse = call(readState, 112, 'fs_read_file_range', { path: join(sourcePath, 'mcp-freshness-service.ts'), start_line: 1, end_line: 1 });
  assert.equal(deliveredReadContent(sourceRangeResponse), "import { createHash } from 'node:crypto';");

  const revolutionConfigPath = join(revolutionRoot, 'config', 'config.json');
  const revolutionReadResponse = call(readState, 12, 'fs_read_file', { path: revolutionConfigPath });
  assert.equal(revolutionReadResponse.result.structuredContent.relative_path.endsWith('!Revolution/.narada/config/config.json'), true);

  const largeRead = call(readState, 121, 'fs_read_file', { path: join(trusted, 'large-read.txt') });
  assert.equal(largeRead.result.structuredContent.schema, 'local.filesystem.read.v1');
  assert.equal(largeRead.result.structuredContent.returned_lines, 1);
  assert.equal(largeRead.result.structuredContent.line_window_complete, true);
  assert.equal(largeRead.result.structuredContent.next_offset, null);
  assert.equal(typeof deliveredReadContent(largeRead), 'string');

  const statResponse = call(readState, 123, 'fs_stat', { path: join(trusted, 'a.txt') });
  assert.equal(statResponse.result.structuredContent.schema, 'local.filesystem.stat.v1');
  assert.equal(statResponse.result.structuredContent.sha256, sha256('alpha\nbeta\n'));
  const directoryStatResponse = call(readState, 125, 'fs_stat', { path: largeRoot });
  assert.equal(directoryStatResponse.result.structuredContent.type, 'directory');
  assert.equal(directoryStatResponse.result.structuredContent.entry_count, 120);
  assert.match(directoryStatResponse.result.structuredContent.tree_sha256, /^[0-9a-f]{64}$/);
  const binaryRead = call(readState, 124, 'fs_read_file', { path: join(trusted, 'binary.bin') });
  assert.equal(binaryRead.error.data.code, 'binary_file_not_supported');

  for (const [id, pattern] of [[13, '**/*config*'], [14, '**/*.json'], [15, '**/*.{json,yaml,yml}']]) {
    const globResponse = call(readState, id, 'fs_glob_search', { directory: revolutionRoot, pattern });
    const globPayload = globResponse.result.structuredContent;
    assert.equal(globPayload.matches.some((match) => match.replace(/\\/g, '/').endsWith('config/config.json')), true, `${pattern} should find config/config.json`);
    assert.equal(globPayload.offset, 0);
    assert.equal(globPayload.limit, 100);
  }
  const defaultIgnoredGlob = call(readState, 151, 'fs_glob_search', { directory: ignoredRoot, pattern: '**/*.txt', limit: 20 });
  const defaultIgnoredMatches = defaultIgnoredGlob.result.structuredContent.matches.map((match) => match.replace(/\\/g, '/'));
  assert.equal(defaultIgnoredMatches.some((match) => match.endsWith('src/keep.txt')), true);
  assert.equal(defaultIgnoredMatches.some((match) => match.includes('/node_modules/')), false);
  assert.equal(defaultIgnoredMatches.some((match) => match.includes('/dist/')), false);
  assert.equal(defaultIgnoredMatches.some((match) => match.endsWith('custom-skip/custom.txt')), true);

  const repositoryInventory = call(readState, 153, 'fs_repository_inventory', { directory: ignoredRoot, pattern: '**/*.txt', limit: 20 });
  const repositoryInventoryPayload = repositoryInventory.result.structuredContent;
  const repositoryInventoryMatches = repositoryInventoryPayload.matches.map((match) => match.replace(/\\/g, '/'));
  assert.equal(repositoryInventoryPayload.schema, 'local.filesystem.repository_inventory.v1');
  assert.equal(repositoryInventoryPayload.generated_artifacts_excluded_by_default, true);
  assert.equal(repositoryInventoryPayload.candidate_source_paths.some((match) => match.replace(/\\/g, '/').endsWith('src/keep.txt')), true);
  assert.equal(repositoryInventoryMatches.some((match) => match.includes('/node_modules/')), false);
  assert.equal(repositoryInventoryMatches.some((match) => match.includes('/dist/')), false);
  assert.equal(repositoryInventoryMatches.some((match) => match.includes('/.ai/runtime/')), false);
  assert.equal(repositoryInventoryPayload.git_tracking_boundary.authority, 'git-mcp');
  assert.equal(repositoryInventoryPayload.git_tracking_boundary.next_tool, 'git_changed_summary');

  const repositoryInventoryWithGenerated = call(readState, 154, 'fs_repository_inventory', { directory: ignoredRoot, pattern: '**/*.txt', include_generated: true, limit: 20 });
  const generatedInventoryPayload = repositoryInventoryWithGenerated.result.structuredContent;
  assert.equal(generatedInventoryPayload.generated_artifacts_excluded_by_default, false);
  assert.equal(generatedInventoryPayload.generated_artifact_paths.some((match) => match.replace(/\\/g, '/').includes('/.ai/runtime/')), true);

  const fileMetrics = call(readState, 155, 'fs_file_metrics', { directory: ignoredRoot, pattern: '**/*.txt', ignore: ['**/.ai/runtime/**'], limit: 20, cache_policy: 'bypass' });
  const fileMetricsPayload = fileMetrics.result.structuredContent;
  assert.equal(fileMetricsPayload.schema, 'local.filesystem.file_metrics.v1');
  assert.equal(fileMetricsPayload.scope.contents_returned, false);
  assert.equal(fileMetricsPayload.totals_scope, 'returned_page');
  assert.equal(fileMetricsPayload.totals.file_count, 2);
  assert.equal(fileMetricsPayload.totals.line_count, 2);
  assert.equal(fileMetricsPayload.totals.byte_count, Buffer.byteLength('keep\n') + Buffer.byteLength('custom\n'));
  assert.equal(fileMetricsPayload.files.some((file) => file.relative_path === 'src/keep.txt' && Number(file.line_count) === 1 && Number(file.byte_count) === 5 && file.file_type === 'txt'), true);
  assert.equal(fileMetricsPayload.files.some((file) => file.relative_path === 'custom-skip/custom.txt' && Number(file.line_count) === 1 && Number(file.byte_count) === 7), true);
  assert.equal(fileMetricsPayload.files.every((file) => Object.prototype.hasOwnProperty.call(file, 'content') === false), true);
  assert.equal(fileMetricsPayload.scope.ignore_patterns.some((pattern) => pattern.includes('node_modules')), true);
  assert.equal(fileMetricsPayload.scope.ignore_patterns.some((pattern) => pattern.includes('.ai/runtime')), true);
  assert.equal(fileMetricsPayload.scope.ignored_path_count, 3);
  assert.equal(fileMetricsPayload.scope.ignored_paths_complete, true);
  assert.equal(fileMetricsPayload.scope.ignored_paths.some((path) => path.includes('node_modules')), true);
  assert.equal(fileMetricsPayload.scope.ignored_paths.some((path) => path.includes('.ai/runtime')), true);
  assert.equal(fileMetricsPayload.scope.boundary.realpath_enforced, true);

  const cancelledMetrics = new AbortController();
  cancelledMetrics.abort();
  const cancelledMetricsResponse = await handleRequestAsync({
    jsonrpc: '2.0',
    id: 14,
    method: 'tools/call',
    params: { name: 'fs_file_metrics', arguments: { directory: largeRoot, pattern: '**/*.txt', limit: 1, cache_policy: 'bypass' } },
  }, readState, { abortSignal: cancelledMetrics.signal }) as unknown as JsonRpcTestResponse;
  assert.equal(cancelledMetricsResponse.error.data.code, 'fs_file_metrics_cancelled');
  const binaryMetrics = call(readState, 156, 'fs_file_metrics', { root: trusted, pattern: '**/binary.bin', limit: 20, cache_policy: 'bypass' });
  const binaryMetricsFile = binaryMetrics.result.structuredContent.files[0];
  assert.equal(binaryMetrics.result.structuredContent.schema, 'local.filesystem.file_metrics.v1');
  assert.equal(binaryMetricsFile.line_count, null);
  assert.equal(binaryMetricsFile.line_count_status, 'binary');
  assert.equal(binaryMetricsFile.byte_count, 4);
  assert.equal(binaryMetricsFile.file_type, 'binary');
  assert.equal(binaryMetrics.result.structuredContent.totals.binary_file_count, 1);

  const tooLargeMetrics = call(readState, 159, 'fs_file_metrics', { root: trusted, pattern: '**/large-read.txt', limit: 20, max_bytes_per_file: 100, cache_policy: 'bypass' });
  const tooLargeMetricsFile = tooLargeMetrics.result.structuredContent.files[0];
  assert.equal(tooLargeMetricsFile.line_count, null);
  assert.equal(tooLargeMetricsFile.line_count_status, 'too_large');
  assert.equal(tooLargeMetricsFile.byte_count, 7001);
  assert.equal(tooLargeMetrics.result.structuredContent.totals.too_large_file_count, 1);
  assert.equal(tooLargeMetrics.result.structuredContent.totals.unavailable_file_count, 0);

  const pagedMetrics = call(readState, 157, 'fs_file_metrics', { directory: largeRoot, pattern: '**/*.txt', limit: 5, cache_policy: 'bypass' });
  assert.equal(pagedMetrics.result.structuredContent.returned, 5);
  assert.equal(pagedMetrics.result.structuredContent.has_more, true);
  assert.equal(pagedMetrics.result.structuredContent.next_offset, 5);
  assert.equal(pagedMetrics.result.structuredContent.totals.file_count, 5);
  assert.equal(pagedMetrics.result.structuredContent.totals.line_count, 5);
  assert.equal(pagedMetrics.result.structuredContent.scope.contents_returned, false);
  const pagedMetricsSecond = call(readState, 171, 'fs_file_metrics', { directory: largeRoot, pattern: '**/*.txt', offset: 5, limit: 5, cache_policy: 'bypass' });
  assert.deepEqual(pagedMetricsSecond.result.structuredContent.scope.ignored_paths, []);
  assert.equal(pagedMetricsSecond.result.structuredContent.scope.ignored_paths_complete, false);
  assert.equal(pagedMetricsSecond.result.structuredContent.scope.out_of_scope_paths_complete, false);
  const scanBudgetMetrics = call(readState, 172, 'fs_file_metrics', { directory: largeRoot, pattern: '**/*.txt', limit: 1, max_total_scan_bytes: 1, cache_policy: 'bypass' });
  assert.equal(scanBudgetMetrics.result.structuredContent.scan_budget_bytes, 1);
  assert.equal(scanBudgetMetrics.result.structuredContent.scan_bytes_reserved, 0);
  assert.equal(scanBudgetMetrics.result.structuredContent.files[0].line_count, null);
  assert.equal(scanBudgetMetrics.result.structuredContent.files[0].line_count_status, 'scan_budget_exceeded');
  assert.equal(scanBudgetMetrics.result.structuredContent.totals.scan_budget_exceeded_file_count, 1);
  const truncatedExcludedMetrics = call(readState, 176, 'fs_file_metrics', { directory: metricsExclusionRoot, pattern: '**/*.txt', ignore: ['**/excluded-*.txt'], limit: 20, cache_policy: 'bypass' });
  assert.equal(truncatedExcludedMetrics.result.structuredContent.scope.ignored_path_count, 105);
  assert.equal(truncatedExcludedMetrics.result.structuredContent.scope.ignored_paths.length, 100);
  assert.equal(truncatedExcludedMetrics.result.structuredContent.scope.ignored_paths_truncated, true);
  assert.equal(truncatedExcludedMetrics.result.structuredContent.scope.ignored_paths_complete, false);

  writeFileSync(join(trusted, 'metrics-snapshot-a.txt'), 'one\ntwo\nthree\n', 'utf8');
  writeFileSync(join(trusted, 'metrics-snapshot-z.txt'), 'old\n', 'utf8');
  const metricsSnapshot = call(readState, 167, 'fs_file_metrics', { root: trusted, pattern: '**/metrics-snapshot-*.txt', limit: 1, cache_policy: 'snapshot' });
  const metricsSnapshotId = metricsSnapshot.result.structuredContent.snapshot_id;
  assert.match(metricsSnapshotId, /^fm_[0-9a-f]+$/);
  assert.equal(metricsSnapshot.result.structuredContent.snapshot_complete, true);
  assert.equal(metricsSnapshot.result.structuredContent.snapshot_lifecycle.scope, 'process_local');
  assert.equal(metricsSnapshot.result.structuredContent.snapshot_lifecycle.survives_restart, false);
  assert.equal(metricsSnapshot.result.structuredContent.snapshot_lifecycle.eviction_policy, 'least_recently_used');
  assert.equal(metricsSnapshot.result.structuredContent.snapshot_lifecycle.max_entries, 4);
  writeFileSync(join(trusted, 'metrics-snapshot-z.txt'), 'new\none\ntwo\n', 'utf8');
  const metricsSnapshotSecond = call(readState, 168, 'fs_file_metrics', { root: trusted, pattern: '**/metrics-snapshot-*.txt', offset: 1, limit: 1, snapshot_id: metricsSnapshotId });
  assert.equal(metricsSnapshotSecond.result.structuredContent.requested_snapshot_id, metricsSnapshotId);
  const snapshotFiles = [
    ...metricsSnapshot.result.structuredContent.files,
    ...metricsSnapshotSecond.result.structuredContent.files,
  ];
  assert.deepEqual(
    new Set(snapshotFiles.map((file) => file.relative_path)),
    new Set(['metrics-snapshot-a.txt', 'metrics-snapshot-z.txt']),
  );
  assert.equal(snapshotFiles.find((file) => file.relative_path === 'metrics-snapshot-z.txt')?.line_count, 1);

  const largeMetricsSnapshot = call(readState, 173, 'fs_file_metrics', { directory: largeMetricsRoot, pattern: '**/*.txt', limit: 1, cache_policy: 'snapshot' });
  assert.equal(largeMetricsSnapshot.result.structuredContent.count, 505);
  assert.equal(largeMetricsSnapshot.result.structuredContent.snapshot_complete, true);
  assert.match(largeMetricsSnapshot.result.structuredContent.snapshot_id, /^fm_[0-9a-f]+$/);
  const largeMetricsSnapshotPage = call(readState, 174, 'fs_file_metrics', { directory: largeMetricsRoot, pattern: '**/*.txt', offset: 504, limit: 1, snapshot_id: largeMetricsSnapshot.result.structuredContent.snapshot_id });
  assert.equal(largeMetricsSnapshotPage.result.structuredContent.requested_snapshot_id, largeMetricsSnapshot.result.structuredContent.snapshot_id);
  assert.equal(largeMetricsSnapshotPage.result.structuredContent.files.length, 1);
  const largeMetricsRefresh = call(readState, 175, 'fs_file_metrics', { directory: largeMetricsRoot, pattern: '**/*.txt', limit: 1, cache_policy: 'refresh' });
  assert.equal(largeMetricsRefresh.result.structuredContent.count, 505);
  assert.equal(largeMetricsRefresh.result.structuredContent.snapshot_complete, true);

  writeFileSync(join(other, 'metrics-outside-target.txt'), 'outside\n', 'utf8');
  let outsideMetricsLinkCreated = false;
  try {
    symlinkSync(join(other, 'metrics-outside-target.txt'), join(trusted, 'metrics-outside-link.txt'));
    outsideMetricsLinkCreated = true;
  } catch {
    outsideMetricsLinkCreated = false;
  }
  if (outsideMetricsLinkCreated) {
    const outsideMetrics = call(readState, 169, 'fs_file_metrics', { root: trusted, pattern: '**/metrics-outside-link.txt', limit: 20, cache_policy: 'bypass' });
    assert.equal(outsideMetrics.result.structuredContent.files.some((file) => file.relative_path === 'metrics-outside-link.txt'), false);
    let outsideMetricsDirectoryLinkCreated = false;
    try {
      symlinkSync(other, join(trusted, 'metrics-outside-dir'), 'junction');
      outsideMetricsDirectoryLinkCreated = true;
    } catch {
      outsideMetricsDirectoryLinkCreated = false;
    }
    if (outsideMetricsDirectoryLinkCreated) {
      const outsideDirectoryMetrics = call(readState, 170, 'fs_file_metrics', { directory: join(trusted, 'metrics-outside-dir'), pattern: '**/*.txt', limit: 20, cache_policy: 'bypass' });
      assert.equal(outsideDirectoryMetrics.error.data.code, 'file_metrics_directory_outside_allowed_root');
    }
  }

  const ambiguousMetrics = call(readState, 158, 'fs_file_metrics', { directory: trusted, root: trusted, pattern: '**/*.txt' });
  assert.equal(ambiguousMetrics.error.data.code, 'file_metrics_directory_ambiguous');

  const callerIgnoredGlob = call(readState, 152, 'fs_glob_search', { directory: ignoredRoot, pattern: '**/*.txt', ignore: ['**/custom-skip/**'], limit: 20 });
  const callerIgnoredMatches = callerIgnoredGlob.result.structuredContent.matches.map((match) => match.replace(/\\/g, '/'));
  assert.equal(callerIgnoredMatches.some((match) => match.endsWith('src/keep.txt')), true);
  assert.equal(callerIgnoredMatches.some((match) => match.endsWith('custom-skip/custom.txt')), false);

  const pagedGlob = call(readState, 16, 'fs_glob_search', { directory: largeRoot, pattern: '**/*.txt', limit: 5 });
  assert.equal(pagedGlob.result.structuredContent.returned, 5);
  assert.equal(pagedGlob.result.structuredContent.has_more, true);
  assert.equal(pagedGlob.result.structuredContent.truncated, undefined);
  assert.equal(pagedGlob.result.structuredContent.count, null);
  assert.equal(pagedGlob.result.structuredContent.count_exact, false);
  assert.equal(pagedGlob.result.structuredContent.order, 'ripgrep_traversal');
  assert.equal(pagedGlob.result.structuredContent.cache_hit, false);
  assert.equal(pagedGlob.result.structuredContent.cache_policy, 'auto');
  assert.equal(pagedGlob.result.structuredContent.timeout_ms, 60000);
  assert.equal(pagedGlob.result.structuredContent.snapshot_complete, false);
  assert.equal(pagedGlob.result.structuredContent.freshness.type, 'directory');
  assert.match(pagedGlob.result.structuredContent.freshness.tree_sha256, /^[0-9a-f]{64}$/);
  assert.equal(pagedGlob.result.structuredContent.matches_format, 'path');
  assert.equal(pagedGlob.result.structuredContent.next_offset, 5);
  assert.ok(Number(pagedGlob.result.structuredContent.scanned) < 20);
  assert.equal(pagedGlob.result.structuredContent.scanned_unit, 'matched_entries');
  assert.match(pagedGlob.result.content[0].text, /fs_glob_search: ok\ncount: unknown\ncount_exact: false\nmatched_entries_scanned: \d+\nscanned_unit: matched_entries\nreturned: 5\norder: ripgrep_traversal\ncache_hit: false\ncache_policy: auto\nsnapshot_id: null\nrequested_snapshot_id: null\nsnapshot_complete: false\ncache_memory_bytes: null\ntimeout_ms: 60000\nfreshness: type=directory tree_sha256=[0-9a-f]{64} tree_entry_count=120 tree_truncated=false\nmatches_format: path\nhas_more: true/);
  assert.equal(pagedGlob.result.structuredContent.matches.length, 5);
  const snapshotGlob = call(readState, 161, 'fs_glob_search', { directory: largeRoot, pattern: '**/*.txt', limit: 5, cache_policy: 'snapshot' });
  assert.equal(snapshotGlob.result.structuredContent.snapshot_complete, true);
  assert.match(snapshotGlob.result.structuredContent.snapshot_id, /^[0-9a-f]{24}$/);
  const snapshotGlobSecond = call(readState, 162, 'fs_glob_search', { directory: largeRoot, pattern: '**/*.txt', offset: 5, limit: 5, snapshot_id: snapshotGlob.result.structuredContent.snapshot_id });
  assert.equal(snapshotGlobSecond.result.structuredContent.requested_snapshot_id, snapshotGlob.result.structuredContent.snapshot_id);
  assert.equal(snapshotGlobSecond.result.structuredContent.cache_hit, true);
  const missingSnapshotGlob = call(readState, 163, 'fs_glob_search', { directory: largeRoot, pattern: '**/*.txt', offset: 5, limit: 5, snapshot_id: 'missing-snapshot' });
  assert.equal(missingSnapshotGlob.error.data.code, 'fs_glob_search_snapshot_not_found');
  const bypassGlob = call(readState, 164, 'fs_glob_search', { directory: largeRoot, pattern: '**/*.txt', offset: 5, limit: 5, cache_policy: 'bypass' });
  assert.equal(bypassGlob.result.structuredContent.cache_policy, 'bypass');
  assert.equal(bypassGlob.result.structuredContent.cache_hit, false);
  (readState.env as NodeJS.ProcessEnv).NARADA_LOCAL_FILESYSTEM_SEARCH_RUNNER_DELAY_MS = '50';
  const timeoutGlob = call(readState, 165, 'fs_glob_search', { directory: largeRoot, pattern: '**/*.txt', limit: 5, timeout_ms: 1, cache_policy: 'bypass' });
  delete (readState.env as NodeJS.ProcessEnv).NARADA_LOCAL_FILESYSTEM_SEARCH_RUNNER_DELAY_MS;
  assert.equal(timeoutGlob.error.data.code, 'fs_glob_search_timed_out');
  assert.equal(timeoutGlob.error.data.details.timeout_kind, 'search_helper_timeout');
  assert.equal(timeoutGlob.error.data.details.partial_results_returned, false);
  assert.equal(timeoutGlob.error.data.details.continuation_available, false);
  assert.equal(timeoutGlob.error.data.details.search_scope, largeRoot);
  assert.equal(timeoutGlob.error.data.details.requested_limit, 5);
  assert.equal(timeoutGlob.error.data.details.requested_cache_policy, 'bypass');
  assert.equal(timeoutGlob.error.data.details.complete_snapshot_required, false);
  assert.equal(timeoutGlob.error.data.details.remediation.some((hint) => /cache_policy=snapshot/.test(String(hint))), true);
  const pagedGlobSecond = call(readState, 17, 'fs_glob_search', { directory: largeRoot, pattern: '**/*.txt', offset: 5, limit: 5 });
  assert.equal(pagedGlobSecond.result.structuredContent.offset, 5);
  assert.equal(pagedGlobSecond.result.structuredContent.returned, 5);
  assert.equal(pagedGlobSecond.result.structuredContent.count_exact, true);
  assert.equal(pagedGlobSecond.result.structuredContent.cache_hit, true);
  assert.equal(pagedGlobSecond.result.structuredContent.snapshot_complete, true);
  assert.match(pagedGlobSecond.result.structuredContent.snapshot_id, /^[0-9a-f]{24}$/);
  assert.equal(typeof pagedGlobSecond.result.structuredContent.cache_memory_bytes, 'number');
  assert.equal(pagedGlobSecond.result.structuredContent.count, 120);
  const pagedGlobThird = call(readState, 171, 'fs_glob_search', { directory: largeRoot, pattern: '**/*.txt', offset: 10, limit: 5 });
  assert.equal(pagedGlobThird.result.structuredContent.cache_hit, true);
  assert.equal(pagedGlobThird.result.structuredContent.returned, 5);
  const timeoutMetrics = call(readState, 160, 'fs_file_metrics', { directory: largeRoot, pattern: '**/*.txt', limit: 1, timeout_ms: 1, cache_policy: 'bypass' });
  assert.ok(timeoutMetrics.error);
  assert.equal(timeoutMetrics.error.data.details.timeout_ms, 1);

  const largeGlob = call(readState, 18, 'fs_glob_search', { directory: largeRoot, pattern: '**/*.txt', limit: 120 });
  assert.equal(largeGlob.result.structuredContent.schema, 'local.filesystem.glob.v1');
  assert.equal(largeGlob.result.structuredContent.returned, 120);
  assert.equal(largeGlob.result.structuredContent.has_more, false);
  assert.equal(largeGlob.result.structuredContent.next_offset, null);
  assert.equal(largeGlob.result.structuredContent.matches.length, 120);
  const emptyGlob = call(readState, 180, 'fs_glob_search', { directory: trusted, pattern: '**/*.does-not-exist', limit: 5 });
  assert.equal(emptyGlob.result.structuredContent.schema, 'local.filesystem.glob.v1');
  assert.equal(emptyGlob.result.structuredContent.status, 'ok');
  assert.equal(emptyGlob.result.structuredContent.count, 0);
  assert.equal(emptyGlob.result.structuredContent.returned, 0);
  assert.deepEqual(emptyGlob.result.structuredContent.matches, []);
  assert.equal(emptyGlob.result.structuredContent.no_match_diagnostics.status, 'no_matches_observed');
  assert.equal(emptyGlob.result.structuredContent.no_match_diagnostics.stale_cache_evidence, false);
  assert.match(emptyGlob.result.structuredContent.no_match_diagnostics.remediation, /cache_policy="refresh"/);
  if (danglingSymlinkCreated) {
    const danglingState = createServerState({ mode: 'read', allowedRoots: [danglingRoot], outputRoot: tempRoot });
    const danglingGlob = call(danglingState, 181, 'fs_glob_search', { directory: danglingRoot, pattern: '**/*.does-not-exist', limit: 5 });
    assert.equal(danglingGlob.result.structuredContent.schema, 'local.filesystem.glob.v1');
    assert.equal(danglingGlob.result.structuredContent.returned, 0);
  }

  const grepFiles = call(readState, 20, 'fs_grep_search', { path: trusted, pattern: 'needle', output_mode: 'files_with_matches', limit: 10 });
  assert.equal(grepFiles.result.structuredContent.schema, 'local.filesystem.grep.v1');
  assert.equal(grepFiles.result.structuredContent.output_mode, 'files_with_matches');
  assert.equal(grepFiles.result.structuredContent.matches.some((match) => match.includes('grep-one.txt')), true);
  assert.equal(grepFiles.result.structuredContent.matches_format, 'human');
  assert.equal(grepFiles.result.structuredContent.match_objects_authoritative, true);
  assert.equal(grepFiles.result.structuredContent.match_objects.some((match) => String(match.path).includes('grep-one.txt')), true);
  assert.match(grepFiles.result.content[0].text, /fs_grep_search: ok\nmode: files_with_matches\ncount: /);
  assert.equal(grepFiles.result.content[0].text.includes('grep-one.txt'), true);
  const grepContent = call(readState, 21, 'fs_grep_search', { path: trusted, pattern: 'needle one', output_mode: 'content', limit: 10 });
  assert.equal(grepContent.result.structuredContent.output_mode, 'content');
  assert.equal(grepContent.result.structuredContent.matches.some((match) => match.includes('needle one')), true);
  assert.equal(grepContent.result.structuredContent.match_objects.some((match) => Number(match.line) === 2 && String(match.text) === 'needle one'), true);
  assert.equal(grepContent.result.content[0].text.includes('needle one'), true);
  const scopedGrep = call(readState, 213, 'fs_grep_search', {
    directory: sourcePath,
    glob: '*.ts',
    pattern: 'classifierFalsePositive',
    output_mode: 'content',
    cache_policy: 'bypass',
    limit: 10,
  });
  assert.ifError(scopedGrep.error);
  assert.equal(scopedGrep.result.structuredContent.scope.argument, 'directory');
  assert.equal(scopedGrep.result.structuredContent.scope.requested_path, sourcePath);
  assert.equal(scopedGrep.result.structuredContent.scope.include_glob, '*.ts');
  assert.equal(scopedGrep.result.structuredContent.returned, 1);
  assert.match(scopedGrep.result.structuredContent.matches[0], /mcp-freshness-service\.ts/);
  const ambiguousGrep = call(readState, 214, 'fs_grep_search', {
    directory: trusted,
    path: trusted,
    pattern: 'needle',
  });
  assert.equal(ambiguousGrep.error.data.code, 'grep_scope_ambiguous');
  const defaultIgnoredGrep = call(readState, 211, 'fs_grep_search', { path: ignoredRoot, pattern: 'skip|keep|custom', output_mode: 'content', limit: 20 });
  assert.equal(defaultIgnoredGrep.result.structuredContent.returned, 2);
  assert.match(defaultIgnoredGrep.result.structuredContent.matches.join('\n'), /keep\.txt/);
  assert.match(defaultIgnoredGrep.result.structuredContent.matches.join('\n'), /custom\.txt/);
  assert.doesNotMatch(defaultIgnoredGrep.result.structuredContent.matches.join('\n'), /node_modules/);
  assert.doesNotMatch(defaultIgnoredGrep.result.structuredContent.matches.join('\n'), /dist/);
  const callerIgnoredGrep = call(readState, 212, 'fs_grep_search', { path: ignoredRoot, pattern: 'skip|keep|custom', output_mode: 'content', ignore: ['**/custom-skip/**'], limit: 20 });
  assert.equal(callerIgnoredGrep.result.structuredContent.returned, 1);
  assert.match(callerIgnoredGrep.result.structuredContent.matches[0], /keep\.txt/);
  assert.doesNotMatch(callerIgnoredGrep.result.structuredContent.matches.join('\n'), /custom-skip/);
  const grepCounts = call(readState, 22, 'fs_grep_search', { path: join(trusted, 'grep-one.txt'), pattern: 'needle', output_mode: 'count_matches', limit: 10 });
  assert.equal(grepCounts.result.structuredContent.output_mode, 'count_matches');
  assert.equal(grepCounts.result.structuredContent.match_objects.some((match) => String(match.path).includes('grep-one.txt') && Number(match.count) === 1), true);
  assert.equal(grepCounts.result.structuredContent.matches.some((match) => /grep-one\.txt: 1/.test(match.replace(/\\/g, '/'))), true);
  assert.equal(grepCounts.result.structuredContent.matches.some((match) => match.includes('\u001f')), false);
  writeFileSync(join(trusted, 'grep-leading-dash.txt'), 'prefix --literal-pattern suffix\n', 'utf8');
  const grepLeadingDashPattern = call(readState, 220, 'fs_grep_search', { path: trusted, pattern: '--literal-pattern', output_mode: 'content', limit: 10 });
  assert.equal(grepLeadingDashPattern.result.structuredContent.output_mode, 'content');
  assert.equal(grepLeadingDashPattern.result.structuredContent.matches.some((match) => match.includes('--literal-pattern')), true);
  const emptyGrep = call(readState, 221, 'fs_grep_search', { path: trusted, pattern: 'does-not-exist', output_mode: 'content', limit: 5 });
  assert.equal(emptyGrep.result.structuredContent.schema, 'local.filesystem.grep.v1');
  assert.equal(emptyGrep.result.structuredContent.status, 'ok');
  assert.equal(emptyGrep.result.structuredContent.count, 0);
  assert.equal(emptyGrep.result.structuredContent.returned, 0);
  assert.deepEqual(emptyGrep.result.structuredContent.matches, []);
  assert.deepEqual(emptyGrep.result.structuredContent.match_objects, []);
  (readState.env as NodeJS.ProcessEnv).NARADA_LOCAL_FILESYSTEM_SEARCH_RUNNER_DELAY_MS = '50';
  const timeoutGrep = call(readState, 222, 'fs_grep_search', { path: largeRoot, pattern: 'needle', output_mode: 'content', limit: 5, timeout_ms: 1, cache_policy: 'snapshot' });
  delete (readState.env as NodeJS.ProcessEnv).NARADA_LOCAL_FILESYSTEM_SEARCH_RUNNER_DELAY_MS;
  assert.equal(timeoutGrep.error.data.code, 'fs_grep_search_timed_out');
  assert.equal(timeoutGrep.error.data.details.timeout_kind, 'search_helper_timeout');
  assert.equal(timeoutGrep.error.data.details.partial_results_returned, false);
  assert.equal(timeoutGrep.error.data.details.continuation_available, false);
  assert.equal(timeoutGrep.error.data.details.search_scope, largeRoot);
  assert.equal(timeoutGrep.error.data.details.requested_limit, 5);
  assert.equal(timeoutGrep.error.data.details.requested_cache_policy, 'snapshot');
  assert.equal(timeoutGrep.error.data.details.complete_snapshot_required, true);
  assert.equal(timeoutGrep.error.data.details.remediation.some((hint) => String(hint).includes('Narrow the directory/path')), true);
  const largeGrepSecondPage = call(readState, 223, 'fs_grep_search', { path: largeGrepRoot, pattern: 'needle', output_mode: 'content', offset: 5, limit: 5, cache_policy: 'bypass' });
  assert.ifError(largeGrepSecondPage.error);
  assert.equal(largeGrepSecondPage.result.structuredContent.returned, 5);
  assert.equal(largeGrepSecondPage.result.structuredContent.count_exact, false);
  assert.equal(largeGrepSecondPage.result.structuredContent.has_more, true);
  assert.equal(largeGrepSecondPage.result.structuredContent.next_offset, 10);
  const hugeGrepRoot = join(trusted, 'huge-grep-output');
  mkdirSync(hugeGrepRoot, { recursive: true });
  writeFileSync(join(hugeGrepRoot, 'huge.txt'), `needle ${'x'.repeat(2 * 1024 * 1024)}\n`, 'utf8');
  const hugeGrep = call(readState, 224, 'fs_grep_search', { path: hugeGrepRoot, pattern: 'needle', output_mode: 'content', limit: 80, cache_policy: 'bypass' });
  assert.ifError(hugeGrep.error);
  assert.equal(hugeGrep.result.structuredContent.returned, 1);
  assert.equal(hugeGrep.result.structuredContent.page_matches_truncated, 1);
  assert.ok(hugeGrep.result.structuredContent.page_match_bytes <= hugeGrep.result.structuredContent.page_match_bytes_limit);
  assert.match(hugeGrep.result.structuredContent.matches[0], /\[truncated\]/);
  const badGrepMode = call(readState, 23, 'fs_grep_search', { path: trusted, pattern: 'needle', output_mode: 'bad_mode' });
  assert.match(badGrepMode.error.message, /grep_output_mode_unsupported/);
  const badGrepPattern = call(readState, 231, 'fs_grep_search', { path: trusted, pattern: '[' });
  assert.equal(badGrepPattern.error.data.code, 'fs_grep_search_failed');
  assert.equal(badGrepPattern.error.data.details.operation, 'fs_grep_search');
  assert.equal(typeof badGrepPattern.error.data.details.status, 'number');
  assert.match(String(badGrepPattern.error.data.details.stderr), /regex parse error|unclosed character class/i);

  const blockedWrite = call(readState, 2, 'fs_write_file', { path: join(trusted, 'b.txt'), content: 'x' });
  assert.match(blockedWrite.error.message, /tool_not_available_in_read_mode/);
  const blockedCreateDirectory = call(readState, 201, 'fs_create_directory', { path: join(trusted, 'read-mode-folder') });
  assert.match(blockedCreateDirectory.error.message, /tool_not_available_in_read_mode/);

  const auditDir = join(tempRoot, 'audit');
  const writeState = createServerState({ mode: 'write', rootsFromCodexConfig: configPath, auditLogDir: auditDir, outputRoot: tempRoot });
  const outsideWrite = call(writeState, 24, 'fs_write_file', { path: join(other, 'outside.txt'), content: 'blocked' });
  assert.equal(outsideWrite.error.data.schema, 'local.filesystem.error.v1');
  assert.equal(outsideWrite.error.data.code, 'path_outside_allowed_roots');
  assert.equal(outsideWrite.error.data.details.operation, 'fs_write_file');
  assert.equal(outsideWrite.error.data.details.requested_path, join(other, 'outside.txt'));
  assert.equal(typeof outsideWrite.error.data.details.active_resolution_base, 'string');
  assert.match(outsideWrite.error.data.details.remediation, /absolute path/);
  assert.equal(outsideWrite.error.data.details.diagnostic_owner, 'local-filesystem-mcp');
  assert.equal(outsideWrite.error.data.details.diagnostic_rule, 'surface_policy_or_tool_validation');
  assert.match(outsideWrite.error.data.details.false_positive_route, /surface_id=local-filesystem/);
  assert.deepEqual(outsideWrite.error.data.details.allowed_roots, roots);

  const writeResponse = call(writeState, 3, 'fs_write_file', { path: join(trusted, 'b.txt'), content: 'created' });
  assert.equal(writeResponse.result.structuredContent.schema, 'local.filesystem.write_file.v1');
  assert.equal(writeResponse.result.structuredContent.status, 'written');
  assert.equal(writeResponse.result.structuredContent.relative_path, 'b.txt');
  assert.match(writeResponse.result.content[0].text, /fs_write_file: written\npath: /);
  assert.match(writeResponse.result.content[0].text, /relative_path: b\.txt/);
  assert.match(readFileSync(join(auditDir, 'filesystem-mcp-audit.jsonl'), 'utf8'), /fs_write_file/);
  const transientScriptDirectory = join(trusted, '.ai', 'tmp');
  mkdirSync(transientScriptDirectory, { recursive: true });
  const transientWrapperWrite = call(writeState, 308, 'fs_write_file', { path: join(transientScriptDirectory, 'generated.cmd'), content: '@echo off\r\n' });
  assert.equal(transientWrapperWrite.error.data.code, 'transient_executable_write_disallowed');
  assert.equal(existsSync(join(transientScriptDirectory, 'generated.cmd')), false);
  writeFileSync(join(transientScriptDirectory, 'existing.ps1'), 'Write-Output before\n', 'utf8');
  const transientScriptReplace = call(writeState, 309, 'fs_str_replace_file', { path: join(transientScriptDirectory, 'existing.ps1'), old: 'before', new: 'after' });
  assert.equal(transientScriptReplace.error.data.code, 'transient_executable_write_disallowed');
  const transientPatch = call(writeState, 310, 'fs_apply_patch', { patch: `*** Begin Patch\n*** Add File: .ai/tmp/patched.js\n+console.log('blocked');\n*** End Patch` });
  assert.equal(transientPatch.error.data.code, 'transient_executable_write_disallowed');
  const verifyWriteRead = call(writeState, 30, 'fs_read_file', { path: join(trusted, 'b.txt') });
  assert.equal(deliveredReadContent(verifyWriteRead), 'created');
  const guardedReadThenReplace = call(writeState, 3001, 'fs_str_replace_file', { path: join(trusted, 'b.txt'), old: 'created', new: 'created-via-guard', expected_sha256: verifyWriteRead.result.structuredContent.content_sha256 });
  assert.equal(guardedReadThenReplace.result.structuredContent.status, 'replaced');
  const changedAfterRead = call(writeState, 3002, 'fs_str_replace_file', { path: join(trusted, 'b.txt'), old: 'created-via-guard', new: 'blocked', expected_sha256: verifyWriteRead.result.structuredContent.content_sha256 });
  assert.equal(changedAfterRead.error.data.code, 'fs_str_replace_file_expected_sha256_mismatch');
  assert.equal(changedAfterRead.error.data.details.expected_sha256, verifyWriteRead.result.structuredContent.content_sha256);
  assert.equal(changedAfterRead.error.data.details.actual_sha256, sha256('created-via-guard'));
  assert.equal(changedAfterRead.error.data.details.concurrency_diagnosis.actual_hash_scope, 'full_file');
  assert.equal(changedAfterRead.error.data.details.concurrency_diagnosis.cache_used, false);
  assert.match(changedAfterRead.error.data.details.remediation, /Read the file again/);
  const payloadDir = join(tempRoot, '.ai', 'tmp', 'mcp-payloads', 'workspace');
  mkdirSync(payloadDir, { recursive: true });
  const payloadPath = join(payloadDir, 'large-write.json');
  const payloadContent = `${'payload-line\n'.repeat(1000)}`;
  writeFileSync(payloadPath, JSON.stringify({ path: join(trusted, 'payload-write.txt'), content: payloadContent, overwrite: true }), 'utf8');
  const payloadWrite = call(writeState, 307, 'fs_write_file', { payload_path: '.ai/tmp/mcp-payloads/workspace/large-write.json' });
  assert.equal(payloadWrite.result.structuredContent.status, 'written');
  assert.equal(payloadWrite.result.structuredContent.payload_source.kind, 'file');
  assert.equal(payloadWrite.result.structuredContent.payload_source.transient_not_authority, true);
  assert.equal(readFileSync(join(trusted, 'payload-write.txt'), 'utf8'), payloadContent);
  const missingParentWrite = call(writeState, 304, 'fs_write_file', { path: join(trusted, 'missing-write-parent', 'file.txt'), content: 'blocked', create_parent_directories: false });
  assert.equal(missingParentWrite.error.data.code, 'write_file_parent_not_found');
  const existingParentWrite = call(writeState, 306, 'fs_write_file', { path: join(trusted, 'existing-parent-create-disabled.txt'), content: 'ok', create_parent_directories: false });
  assert.equal(existingParentWrite.result.structuredContent.status, 'written');
  assert.equal(readFileSync(join(trusted, 'existing-parent-create-disabled.txt'), 'utf8'), 'ok');
  const parentWrite = call(writeState, 305, 'fs_write_file', { path: join(trusted, 'implicit-write-parent', 'file.txt'), content: 'ok', create_parent_directories: true });
  assert.equal(parentWrite.result.structuredContent.create_parent_directories, true);
  assert.equal(readFileSync(join(trusted, 'implicit-write-parent', 'file.txt'), 'utf8'), 'ok');
  const createOnlyBlocked = call(writeState, 301, 'fs_write_file', { path: join(trusted, 'b.txt'), content: 'blocked', create_only: true });
  assert.equal(createOnlyBlocked.error.data.code, 'write_file_destination_exists');
  const overwriteBlockedWrite = call(writeState, 302, 'fs_write_file', { path: join(trusted, 'b.txt'), content: 'blocked', overwrite: false });
  assert.equal(overwriteBlockedWrite.error.data.code, 'write_file_overwrite_refused');
  const staleWrite = call(writeState, 303, 'fs_write_file', { path: join(trusted, 'b.txt'), content: 'blocked', expected_sha256: 'bad' });
  assert.equal(staleWrite.error.data.code, 'fs_write_file_expected_sha256_mismatch');

  const stdioWritePath = join(trusted, 'stdio-framed-write.txt');
  const serverPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
  const framedRequestBody = JSON.stringify({
    jsonrpc: '2.0',
    id: 300,
    method: 'tools/call',
    params: {
      name: 'fs_write_file',
      arguments: { path: stdioWritePath, content: 'framed\n' },
    },
  });
  const framedWrite = spawnSync(process.execPath, [
    serverPath,
    '--mode', 'write',
    '--allowed-root', trusted,
    '--output-root', tempRoot,
    '--audit-log-dir', auditDir,
  ], {
    input: `Content-Length: ${Buffer.byteLength(framedRequestBody, 'utf8')}\n\n${framedRequestBody}`,
    encoding: 'utf8',
    timeout: 5000,
    windowsHide: true,
  });
  assert.equal(framedWrite.status, 0, framedWrite.stderr);
  const framedResponse = parseFirstJsonRpcFrame(framedWrite.stdout);
  assert.equal(framedResponse.id, 300);
  assert.equal(framedResponse.result.structuredContent.status, 'written');
  assert.equal(readFileSync(stdioWritePath, 'utf8'), 'framed\n');

  const delayedWriteBody = JSON.stringify({
    jsonrpc: '2.0',
    id: 301,
    method: 'tools/call',
    params: {
      name: 'fs_write_file',
      arguments: { path: join(trusted, 'delayed-write.txt'), content: 'delayed\n', timeout_ms: 1 },
    },
  });
  const doctorBody = JSON.stringify({
    jsonrpc: '2.0',
    id: 302,
    method: 'tools/call',
    params: { name: 'fs_doctor', arguments: {} },
  });
  const delayedWrite = spawnSync(process.execPath, [
    serverPath,
    '--mode', 'write',
    '--allowed-root', trusted,
    '--output-root', tempRoot,
    '--audit-log-dir', auditDir,
  ], {
    input: `Content-Length: ${Buffer.byteLength(delayedWriteBody, 'utf8')}\r\n\r\n${delayedWriteBody}Content-Length: ${Buffer.byteLength(doctorBody, 'utf8')}\r\n\r\n${doctorBody}`,
    encoding: 'utf8',
    timeout: 5000,
    env: { ...process.env, NARADA_LOCAL_FILESYSTEM_WRITE_DELAY_MS: '50' },
    windowsHide: true,
  });
  assert.equal(delayedWrite.status, 0, delayedWrite.stderr);
  const delayedFrames = parseJsonRpcFrames(delayedWrite.stdout);
  const delayedWriteResponse = delayedFrames.find((frame) => frame.id === 301);
  const delayedDoctorResponse = delayedFrames.find((frame) => frame.id === 302);
  assert.equal(delayedWriteResponse.error.data.code, 'fs_write_file_timed_out');
  assert.equal(delayedDoctorResponse.result.structuredContent.status, 'ok');

  const blockedReadBody = JSON.stringify({
    jsonrpc: '2.0',
    id: 303,
    method: 'tools/call',
    params: {
      name: 'fs_read_file_range',
      arguments: { path: join(trusted, 'a.txt'), start_line: 1, end_line: 1, timeout_ms: 5 },
    },
  });
  const blockedReadDoctorBody = JSON.stringify({
    jsonrpc: '2.0',
    id: 304,
    method: 'tools/call',
    params: { name: 'fs_doctor', arguments: {} },
  });
  const framedBlockedReadStartedAt = Date.now();
  const blockedRead = spawnSync(process.execPath, [
    serverPath,
    '--mode', 'read',
    '--allowed-root', trusted,
    '--output-root', tempRoot,
  ], {
    input: `Content-Length: ${Buffer.byteLength(blockedReadBody, 'utf8')}\r\n\r\n${blockedReadBody}Content-Length: ${Buffer.byteLength(blockedReadDoctorBody, 'utf8')}\r\n\r\n${blockedReadDoctorBody}`,
    encoding: 'utf8',
    timeout: 5000,
    env: { ...process.env, NARADA_LOCAL_FILESYSTEM_READ_WORKER_BLOCK_MS: '60000' },
    windowsHide: true,
  });
  const framedBlockedReadElapsedMs = Date.now() - framedBlockedReadStartedAt;
  assert.equal(blockedRead.status, 0, blockedRead.stderr);
  assert.equal(framedBlockedReadElapsedMs < 5000, true);
  const blockedReadFrames = parseJsonRpcFrames(blockedRead.stdout);
  const blockedReadResponse = blockedReadFrames.find((frame) => frame.id === 303);
  const blockedReadDoctorResponse = blockedReadFrames.find((frame) => frame.id === 304);
  assert.equal(blockedReadResponse.error.data.code, 'fs_read_file_range_timed_out');
  assert.equal(blockedReadResponse.error.data.details.timeout_kind, 'read_timeout');
  assert.equal(blockedReadResponse.error.data.details.timeout_ms, 5);
  assert.equal(blockedReadDoctorResponse.result.structuredContent.status, 'ok');

  const replaceRangeResponse = call(writeState, 31, 'fs_replace_range', { path: join(trusted, 'b.txt'), start_line: 1, end_line: 1, replacement: 'range-edited' });
  assert.equal(replaceRangeResponse.result.structuredContent.schema, 'local.filesystem.replace_range.v1');
  assert.equal(replaceRangeResponse.result.structuredContent.status, 'replaced_range');
  assert.equal(typeof replaceRangeResponse.result.structuredContent.before_sha256, 'string');
  assert.equal(readFileSync(join(trusted, 'b.txt'), 'utf8'), 'range-edited');
  const staleRangeResponse = call(writeState, 311, 'fs_replace_range', { path: join(trusted, 'b.txt'), start_line: 1, end_line: 1, replacement: 'blocked', expected_sha256: 'bad' });
  assert.equal(staleRangeResponse.error.data.code, 'fs_replace_range_expected_sha256_mismatch');

  writeFileSync(join(trusted, 'ambiguous.txt'), 'same\nsame\n', 'utf8');
  const staleStrReplace = call(writeState, 313, 'fs_str_replace_file', { path: join(trusted, 'ambiguous.txt'), old: 'same', new: 'different', expected_sha256: 'bad' });
  assert.equal(staleStrReplace.error.data.code, 'fs_str_replace_file_expected_sha256_mismatch');
  const ambiguousReplace = call(writeState, 312, 'fs_str_replace_file', { path: join(trusted, 'ambiguous.txt'), old: 'same', new: 'different' });
  assert.equal(ambiguousReplace.error.data.code, 'str_replace_ambiguous');
  assert.equal(ambiguousReplace.error.data.details.occurrences, 2);
  assert.equal(ambiguousReplace.error.data.details.matches[0].line, 1);

  writeFileSync(join(trusted, 'crlf-visible.txt'), 'first\r\nvisible line\r\nthird\r\n', 'utf8');
  const newlineMismatchReplace = call(writeState, 314, 'fs_str_replace_file', { path: join(trusted, 'crlf-visible.txt'), old: 'visible line\nthird', new: 'replacement' });
  assert.equal(newlineMismatchReplace.error.data.code, 'str_replace_not_found');
  assert.equal(newlineMismatchReplace.error.data.details.likely_newline_or_context_mismatch, true);
  assert.equal(newlineMismatchReplace.error.data.details.mismatch_reason, 'normalized_newline_match_only');
  assert.equal(newlineMismatchReplace.error.data.details.recommended_tool, 'fs_replace_range');
  assert.deepEqual(newlineMismatchReplace.error.data.details.candidate_line_ranges[0], { start_line: 2, end_line: 3, preview: 'visible line\nthird' });
  assert.equal(newlineMismatchReplace.error.data.details.recommended_args.start_line, 2);
  assert.equal(newlineMismatchReplace.error.data.details.recommended_args.end_line, 3);

  writeFileSync(join(trusted, 'patch.txt'), 'one\ntwo\n', 'utf8');
  const patchDryRun = call(writeState, 321, 'fs_apply_patch', { patch: `--- patch.txt\n+++ patch.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+patched\n`, dry_run: true, expected_sha256: { 'patch.txt': sha256('one\ntwo\n') } });
  assert.equal(patchDryRun.result.structuredContent.status, 'checked');
  assert.equal(patchDryRun.result.structuredContent.dry_run, true);
  assert.equal(patchDryRun.result.structuredContent.timeout_ms, 10000);
  assert.equal(patchDryRun.result.structuredContent.changed_files[0].operation, 'update');
  assert.equal(readFileSync(join(trusted, 'patch.txt'), 'utf8'), 'one\ntwo\n');
  const unmatchedPatchGuard = call(writeState, 323, 'fs_apply_patch', { patch: `--- patch.txt\n+++ patch.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+patched\n`, expected_sha256: { 'typo-patch.txt': sha256('one\ntwo\n') } });
  assert.equal(unmatchedPatchGuard.error.data.code, 'fs_apply_patch_expected_sha256_unmatched');
  assert.deepEqual(unmatchedPatchGuard.error.data.details.unmatched_expected_sha256_keys, ['typo-patch.txt']);
  const stalePatchGuard = call(writeState, 322, 'fs_apply_patch', { patch: `--- patch.txt\n+++ patch.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+patched\n`, expected_sha256: { 'patch.txt': 'bad' } });
  assert.equal(stalePatchGuard.error.data.code, 'fs_apply_patch_expected_sha256_mismatch');
  assert.notEqual(stalePatchGuard.error.message, '[object Object]');
  assert.equal(typeof stalePatchGuard.error.message, 'string');
  const foreignDiagnostic = errorDiagnostic({ data: { schema: 'foreign.error.v1', code: 'foreign_failure', message: 'structured failure', details: { path: 'patch.txt' } } });
  assert.equal(foreignDiagnostic.code, 'foreign_failure');
  assert.equal(foreignDiagnostic.message, 'structured failure');
  assert.deepEqual(foreignDiagnostic.details, { path: 'patch.txt', source_schema: 'foreign.error.v1' });
  const objectMessageDiagnostic = errorDiagnostic({ code: 'object_failure', message: { reason: 'structured object message' } });
  assert.match(objectMessageDiagnostic.message, /structured object message/);
  assert.notEqual(objectMessageDiagnostic.message, '[object Object]');
  const patchText = `--- patch.txt\n+++ patch.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+patched\n`;
  const patchResponse = call(writeState, 32, 'fs_apply_patch', { operation_id: 'idempotent-patch-test', patch: patchText, expected_sha256: { 'patch.txt': sha256('one\ntwo\n') } });
  assert.equal(patchResponse.result.structuredContent.schema, 'local.filesystem.apply_patch.v1');
  assert.equal(patchResponse.result.structuredContent.status, 'patched');
  assert.equal(patchResponse.result.structuredContent.changed_files[0].operation, 'update');
  assert.equal(patchResponse.result.structuredContent.changed_files[0].relative_path, 'patch.txt');
  assert.equal(readFileSync(join(trusted, 'patch.txt'), 'utf8'), 'one\npatched\n');
  const replayedPatch = call(writeState, 3200, 'fs_apply_patch', { operation_id: 'idempotent-patch-test', patch: patchText });
  assert.equal(replayedPatch.result.structuredContent.status, 'patched');
  assert.equal(replayedPatch.result.structuredContent.operation_replayed, true);
  assert.equal(readFileSync(join(trusted, 'patch.txt'), 'utf8'), 'one\npatched\n');
  const conflictingPatch = call(writeState, 32001, 'fs_apply_patch', { operation_id: 'idempotent-patch-test', patch: `${patchText}\n` });
  assert.equal(conflictingPatch.error.data.code, 'patch_operation_id_conflict');
  const recoveredPatchOutcome = call(writeState, 3201, 'fs_patch_outcome_show', { operation_id: patchResponse.result.structuredContent.operation_id });
  assert.equal(recoveredPatchOutcome.result.structuredContent.status, 'patched');
  assert.equal(recoveredPatchOutcome.result.structuredContent.changed_files[0].after_sha256, sha256('one\npatched\n'));
  assert.equal(patchResponse.result.structuredContent.outcome_reader.tool, 'fs_patch_outcome_show');

  writeFileSync(join(trusted, 'rollback-outcome.txt'), 'before\n', 'utf8');
  const rollbackOutcomePatch = call(writeState, 3202, 'fs_apply_patch', { operation_id: 'rollback-outcome-test', patch: `--- rollback-outcome.txt\n+++ rollback-outcome.txt\n@@ -1 +1 @@\n-before\n+after\n--- missing-outcome.txt\n+++ missing-outcome.txt\n@@ -1 +1 @@\n-old\n+new\n` });
  assert.equal(rollbackOutcomePatch.error.data.code, 'patch_source_not_found');
  const rollbackOutcome = call(writeState, 3203, 'fs_patch_outcome_show', { operation_id: 'rollback-outcome-test' });
  assert.equal(rollbackOutcome.result.structuredContent.status, 'failed_before_mutation');
  assert.equal(rollbackOutcome.result.structuredContent.mutation_started, false);
  assert.equal(rollbackOutcome.result.structuredContent.rollback_performed, false);
  assert.equal(readFileSync(join(trusted, 'rollback-outcome.txt'), 'utf8'), 'before\n');

  writeFileSync(join(trusted, 'patch-timeout.txt'), `${Array.from({ length: 80000 }, (_, index) => `line-${index}`).join('\n')}\n`, 'utf8');
  const timeoutPatch = call(writeState, 324, 'fs_apply_patch', {
    patch: `*** Begin Patch\n*** Update File: patch-timeout.txt\n@@\n-missing-line\n+replacement\n*** End Patch\n`,
    timeout_ms: 1,
  });
  assert.equal(timeoutPatch.error.data.code, 'fs_apply_patch_timed_out');
  assert.equal(timeoutPatch.error.data.details.timeout_kind, 'filesystem_operation_timeout');
  assert.equal(timeoutPatch.error.data.details.timeout_ms, 1);
  const patchOutcomeDirectory = join(tempRoot, '.narada', 'local-filesystem-mcp', 'patch-outcomes');
  mkdirSync(patchOutcomeDirectory, { recursive: true });
  const writeInterruptedPatchOutcome = (
    operationId: string,
    patch: string,
    beforeState: Array<{ path: string; exists: boolean; sha256: string | null }>,
    afterState: Array<{ path: string; exists: boolean; sha256: string | null }>,
  ) => {
    writeFileSync(join(patchOutcomeDirectory, `${operationId}.json`), `${JSON.stringify({
      schema: 'local.filesystem.apply_patch.outcome.v1',
      status: 'applying',
      operation_id: operationId,
      patch_sha256: sha256(patch),
      mutation_started: true,
      owner_pid: 2147483647,
      deadline_at: new Date(0).toISOString(),
      recovery_plan: { before_state: beforeState, after_state: afterState, changed_files: [] },
    }, null, 2)}\n`, 'utf8');
  };

  const retryRecoveryPath = join(trusted, 'patch-recovery-retry.txt');
  const retryRecoveryPatch = `--- patch-recovery-retry.txt\n+++ patch-recovery-retry.txt\n@@ -1 +1 @@\n-before\n+after\n`;
  writeFileSync(retryRecoveryPath, 'before\n', 'utf8');
  writeInterruptedPatchOutcome(
    'patch-recovery-retry',
    retryRecoveryPatch,
    [{ path: retryRecoveryPath, exists: true, sha256: sha256('before\n') }],
    [{ path: retryRecoveryPath, exists: true, sha256: sha256('after\n') }],
  );
  const retryRecoveryOutcome = call(writeState, 325, 'fs_patch_outcome_show', { operation_id: 'patch-recovery-retry' });
  assert.equal(retryRecoveryOutcome.result.structuredContent.status, 'interrupted_before_mutation');
  assert.equal(retryRecoveryOutcome.result.structuredContent.retry_safe, true);
  const recoveredRetry = call(writeState, 326, 'fs_apply_patch', { operation_id: 'patch-recovery-retry', patch: retryRecoveryPatch });
  assert.equal(recoveredRetry.result.structuredContent.status, 'patched');
  assert.equal(recoveredRetry.result.structuredContent.recovery_count, 1);
  assert.equal(readFileSync(retryRecoveryPath, 'utf8'), 'after\n');

  const completedRecoveryPath = join(trusted, 'patch-recovery-complete.txt');
  const completedRecoveryPatch = `--- patch-recovery-complete.txt\n+++ patch-recovery-complete.txt\n@@ -1 +1 @@\n-before\n+after\n`;
  writeFileSync(completedRecoveryPath, 'after\n', 'utf8');
  writeInterruptedPatchOutcome(
    'patch-recovery-complete',
    completedRecoveryPatch,
    [{ path: completedRecoveryPath, exists: true, sha256: sha256('before\n') }],
    [{ path: completedRecoveryPath, exists: true, sha256: sha256('after\n') }],
  );
  const completedRecoveryOutcome = call(writeState, 327, 'fs_patch_outcome_show', { operation_id: 'patch-recovery-complete' });
  assert.equal(completedRecoveryOutcome.result.structuredContent.status, 'patched_recovered');
  assert.equal(completedRecoveryOutcome.result.structuredContent.retry_safe, false);

  const partialRecoveryA = join(trusted, 'patch-recovery-partial-a.txt');
  const partialRecoveryB = join(trusted, 'patch-recovery-partial-b.txt');
  writeFileSync(partialRecoveryA, 'after-a\n', 'utf8');
  writeFileSync(partialRecoveryB, 'before-b\n', 'utf8');
  writeInterruptedPatchOutcome(
    'patch-recovery-partial',
    'synthetic-partial-patch',
    [
      { path: partialRecoveryA, exists: true, sha256: sha256('before-a\n') },
      { path: partialRecoveryB, exists: true, sha256: sha256('before-b\n') },
    ],
    [
      { path: partialRecoveryA, exists: true, sha256: sha256('after-a\n') },
      { path: partialRecoveryB, exists: true, sha256: sha256('after-b\n') },
    ],
  );
  const partialRecoveryOutcome = call(writeState, 328, 'fs_patch_outcome_show', { operation_id: 'patch-recovery-partial' });
  assert.equal(partialRecoveryOutcome.result.structuredContent.status, 'interrupted_partial');
  assert.equal(partialRecoveryOutcome.result.structuredContent.retry_safe, false);

  writeFileSync(join(trusted, 'patch-header.txt'), 'old\n', 'utf8');
  const headerPatchResponse = call(writeState, 34, 'fs_apply_patch', {
    patch: `--- a/patch-header.txt\t2026-06-02\n+++ b/patch-header.txt\t2026-06-02\n@@ -1 +1 @@\n-old\n+new\n`,
  });
  assert.equal(headerPatchResponse.result.structuredContent.status, 'patched');
  assert.equal(readFileSync(join(trusted, 'patch-header.txt'), 'utf8'), 'new\n');

  const applyPatchGrammar = call(writeState, 35, 'fs_apply_patch', {
    patch: `*** Begin Patch\n*** Update File: patch-header.txt\n@@\n-new\n+newer\n*** End Patch\n`,
  });
  assert.equal(applyPatchGrammar.result.structuredContent.status, 'patched');
  assert.equal(readFileSync(join(trusted, 'patch-header.txt'), 'utf8'), 'newer\n');

  const addPatchResponse = call(writeState, 351, 'fs_apply_patch', {
    patch: `*** Begin Patch\n*** Add File: codex-added.txt\n+added\n+file\n*** End Patch\n`,
  });
  assert.equal(addPatchResponse.result.structuredContent.status, 'patched');
  assert.equal(addPatchResponse.result.structuredContent.changed_files[0].operation, 'add');
  assert.equal(readFileSync(join(trusted, 'codex-added.txt'), 'utf8'), 'added\nfile\n');

  const deletePatchResponse = call(writeState, 352, 'fs_apply_patch', {
    patch: `*** Begin Patch\n*** Delete File: codex-added.txt\n*** End Patch\n`,
  });
  assert.equal(deletePatchResponse.result.structuredContent.changed_files[0].deleted, true);
  assert.equal(deletePatchResponse.result.structuredContent.changed_files[0].operation, 'delete');
  assert.equal(existsSync(join(trusted, 'codex-added.txt')), false);

  writeFileSync(join(trusted, 'delete-unified.txt'), 'gone\n', 'utf8');
  const unifiedDeletePatchResponse = call(writeState, 353, 'fs_apply_patch', {
    patch: `--- delete-unified.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n`,
  });
  assert.equal(unifiedDeletePatchResponse.result.structuredContent.changed_files[0].deleted, true);
  assert.equal(existsSync(join(trusted, 'delete-unified.txt')), false);

  writeFileSync(join(trusted, 'delete-stale.txt'), 'changed\n', 'utf8');
  const staleDeletePatchResponse = call(writeState, 354, 'fs_apply_patch', {
    patch: `--- delete-stale.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\n`,
  });
  assert.equal(staleDeletePatchResponse.error.data.code, 'patch_delete_content_mismatch');

  writeFileSync(join(trusted, 'move-codex.txt'), 'move me\n', 'utf8');
  const movePatchResponse = call(writeState, 355, 'fs_apply_patch', {
    patch: `*** Begin Patch\n*** Update File: move-codex.txt\n*** Move to: moved-codex.txt\n@@\n move me\n*** End Patch\n`,
  });
  assert.equal(movePatchResponse.result.structuredContent.status, 'patched');
  assert.equal(movePatchResponse.result.structuredContent.changed_files[0].operation, 'move');
  assert.equal(existsSync(join(trusted, 'move-codex.txt')), false);
  assert.equal(readFileSync(join(trusted, 'moved-codex.txt'), 'utf8'), 'move me\n');

  const malformedUnifiedPatch = call(writeState, 356, 'fs_apply_patch', {
    patch: `+++ malformed.txt\n@@ -1 +1 @@\n-old\n+new\n`,
  });
  assert.equal(malformedUnifiedPatch.error.data.code, 'patch_new_file_without_old_file_header');
  assert.equal(malformedUnifiedPatch.error.data.details.expected_format, 'unified_diff_or_codex_apply_patch');

  const malformedCodexPatch = call(writeState, 357, 'fs_apply_patch', {
    operation_id: 'malformed-parse-outcome',
    patch: `*** Begin Patch\n*** Move to: missing-update.txt\n*** End Patch\n`,
  });
  assert.equal(malformedCodexPatch.error.data.code, 'patch_move_without_update_file');
  assert.equal(malformedCodexPatch.error.data.details.expected_format, 'codex_apply_patch');
  const malformedOutcome = call(readState, 3571, 'fs_patch_outcome_show', { operation_id: 'malformed-parse-outcome' });
  assert.equal(malformedOutcome.result.structuredContent.status, 'failed_before_mutation');
  assert.equal(malformedOutcome.result.structuredContent.mutation_started, false);

  const malformedCodexAddPatch = call(writeState, 358, 'fs_apply_patch', {
    patch: `*** Begin Patch\n*** Add File: malformed-add.txt\nmissing-plus\n*** End Patch\n`,
  });
  assert.equal(malformedCodexAddPatch.error.data.code, 'patch_add_line_kind_unsupported');
  assert.equal(malformedCodexAddPatch.error.data.details.line, 'missing-plus');

  const missingSourcePatch = call(writeState, 359, 'fs_apply_patch', {
    patch: `--- missing-source.txt\n+++ missing-source.txt\n@@ -1 +1 @@\n-old\n+new\n`,
  });
  assert.equal(missingSourcePatch.error.data.code, 'patch_source_not_found');
  assert.equal(missingSourcePatch.error.data.details.expected_format_for_new_files, 'unified diff with --- /dev/null or Codex *** Add File');

  writeFileSync(join(trusted, 'patch-a.txt'), 'one\ntwo\n', 'utf8');
  writeFileSync(join(trusted, 'patch-b.txt'), 'red\nblue\n', 'utf8');
  const badPatch = call(writeState, 33, 'fs_apply_patch', { patch: `--- patch-a.txt\n+++ patch-a.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+changed\n--- patch-b.txt\n+++ patch-b.txt\n@@ -1,2 +1,2 @@\n red\n-missing\n+changed\n` });
  assert.match(badPatch.error.message, /patch_remove_mismatch/);
  assert.equal(readFileSync(join(trusted, 'patch-a.txt'), 'utf8'), 'one\ntwo\n');
  assert.equal(readFileSync(join(trusted, 'patch-b.txt'), 'utf8'), 'red\nblue\n');

  const staleMoveGuard = call(writeState, 401, 'fs_move_path', { from: join(trusted, 'b.txt'), to: join(trusted, 'renamed.txt'), expected_from_sha256: 'bad' });
  assert.equal(staleMoveGuard.error.data.code, 'fs_move_path_expected_metadata_mismatch');
  const moveSourceStat = statSync(join(trusted, 'b.txt'));
  const moveResponse = call(writeState, 4, 'fs_move_path', {
    from: join(trusted, 'b.txt'),
    to: join(trusted, 'renamed.txt'),
    expected_from: {
      mtime: moveSourceStat.mtime.toISOString(),
      size: moveSourceStat.size,
      sha256: sha256('range-edited'),
    },
  });
  assert.equal(moveResponse.result.structuredContent.schema, 'local.filesystem.move_path.v1');
  assert.equal(moveResponse.result.structuredContent.status, 'moved');
  assert.equal(moveResponse.result.structuredContent.to.relative_path, 'renamed.txt');
  assert.match(readFileSync(join(auditDir, 'filesystem-mcp-audit.jsonl'), 'utf8'), /fs_move_path/);

  writeFileSync(join(trusted, 'sentinel-source.txt'), 'sentinel', 'utf8');
  const sentinelMove = call(writeState, 402, 'fs_move_path', {
    from: join(trusted, 'sentinel-source.txt'),
    to: join(trusted, 'sentinel-dest.txt'),
    expected_from: { mtime: '', size: 0, sha256: '', tree_sha256: '', entry_count: 0 },
    expected_to: { mtime: '', size: 0, sha256: '', tree_sha256: '', entry_count: 0 },
  });
  assert.equal(sentinelMove.result.structuredContent.status, 'moved');
  writeFileSync(join(trusted, 'nonzero-source.txt'), 'nonzero', 'utf8');
  const structuredZeroGuard = call(writeState, 403, 'fs_move_path', {
    from: join(trusted, 'nonzero-source.txt'),
    to: join(trusted, 'nonzero-dest.txt'),
    expected_from: { size: 0 },
  });
  assert.equal(structuredZeroGuard.error.data.code, 'fs_move_path_expected_metadata_mismatch');

  const outsideMove = call(writeState, 25, 'fs_move_path', { from: join(trusted, 'renamed.txt'), to: join(other, 'outside-renamed.txt') });
  assert.equal(outsideMove.error.data.code, 'path_outside_allowed_roots');
  assert.equal(outsideMove.error.data.details.operation, 'fs_move_path');
  assert.equal(outsideMove.error.data.details.field, 'to');

  const overwriteBlocked = call(writeState, 5, 'fs_move_path', { from: join(trusted, 'renamed.txt'), to: join(trusted, 'a.txt') });
  assert.match(overwriteBlocked.error.message, /move_destination_exists/);
  writeFileSync(join(trusted, 'overwrite-source.txt'), 'source', 'utf8');
  writeFileSync(join(trusted, 'overwrite-dest.txt'), 'dest', 'utf8');
  const overwriteDestStat = statSync(join(trusted, 'overwrite-dest.txt'));
  const overwriteMove = call(writeState, 6, 'fs_move_path', {
    from: join(trusted, 'overwrite-source.txt'),
    to: join(trusted, 'overwrite-dest.txt'),
    overwrite: true,
    expected_to_mtime: overwriteDestStat.mtime.toISOString(),
    expected_to_size: overwriteDestStat.size,
  });
  assert.equal(overwriteMove.result.structuredContent.overwrite, true);
  assert.equal(readFileSync(join(trusted, 'overwrite-dest.txt'), 'utf8'), 'source');

  const sameMove = call(writeState, 7, 'fs_move_path', { from: join(trusted, 'overwrite-dest.txt'), to: join(trusted, 'overwrite-dest.txt') });
  assert.match(sameMove.error.message, /move_source_and_destination_same/);
  mkdirSync(join(trusted, 'dir-source'), { recursive: true });
  writeFileSync(join(trusted, 'dir-source', 'child.txt'), 'child', 'utf8');
  const staleTreeMove = call(writeState, 802, 'fs_move_path', { from: join(trusted, 'dir-source'), to: join(trusted, 'dir-target'), expected_from: { tree_sha256: 'bad', entry_count: 1 } });
  assert.equal(staleTreeMove.error.data.code, 'fs_move_path_expected_metadata_mismatch');
  const insideMove = call(writeState, 8, 'fs_move_path', { from: join(trusted, 'dir-source'), to: join(trusted, 'dir-source', 'nested') });
  assert.match(insideMove.error.message, /move_destination_inside_source/);
  mkdirSync(join(trusted, 'dir-dest'), { recursive: true });
  writeFileSync(join(trusted, 'file-source.txt'), 'file', 'utf8');
  const typeMismatchMove = call(writeState, 9, 'fs_move_path', { from: join(trusted, 'file-source.txt'), to: join(trusted, 'dir-dest'), overwrite: true });
  assert.match(typeMismatchMove.error.message, /move_destination_type_mismatch/);

  const createDirectoryResponse = call(writeState, 40, 'fs_create_directory', { path: join(trusted, 'folders', 'created'), recursive: true });
  assert.equal(createDirectoryResponse.result.structuredContent.schema, 'local.filesystem.create_directory.v1');
  assert.equal(createDirectoryResponse.result.structuredContent.status, 'created');
  assert.equal(createDirectoryResponse.result.structuredContent.relative_path, 'folders/created');
  assert.equal(existsSync(join(trusted, 'folders', 'created')), true);
  assert.match(readFileSync(join(auditDir, 'filesystem-mcp-audit.jsonl'), 'utf8'), /fs_create_directory/);

  const createExistingDirectory = call(writeState, 41, 'fs_create_directory', { path: join(trusted, 'folders', 'created') });
  assert.equal(createExistingDirectory.result.structuredContent.status, 'exists');
  assert.equal(createExistingDirectory.result.structuredContent.created, false);
  assert.match(readFileSync(join(auditDir, 'filesystem-mcp-audit.jsonl'), 'utf8'), /"created":false/);

  const createOverFile = call(writeState, 42, 'fs_create_directory', { path: join(trusted, 'a.txt') });
  assert.match(createOverFile.error.message, /create_directory_destination_not_directory/);

  const outsideCreateDirectory = call(writeState, 43, 'fs_create_directory', { path: join(other, 'outside-folder') });
  assert.equal(outsideCreateDirectory.error.data.code, 'path_outside_allowed_roots');
  assert.equal(outsideCreateDirectory.error.data.details.operation, 'fs_create_directory');

  const missingParentCreateDirectory = call(writeState, 431, 'fs_create_directory', { path: join(trusted, 'missing-parent', 'child') });
  assert.equal(missingParentCreateDirectory.error.data.code, 'create_directory_parent_not_found');
  assert.equal(missingParentCreateDirectory.error.data.details.parent.relative_path, 'missing-parent');

  const renameSourceStat = statSync(join(trusted, 'folders', 'created'));
  const staleRenameGuard = call(writeState, 432, 'fs_rename_directory', { from: join(trusted, 'folders', 'created'), to: join(trusted, 'folders', 'renamed'), expected_from_size: renameSourceStat.size + 1 });
  assert.equal(staleRenameGuard.error.data.code, 'fs_rename_directory_expected_metadata_mismatch');
  const renameDirectoryResponse = call(writeState, 44, 'fs_rename_directory', {
    from: join(trusted, 'folders', 'created'),
    to: join(trusted, 'folders', 'renamed'),
    expected_from: {
      mtime: renameSourceStat.mtime.toISOString(),
      size: renameSourceStat.size,
      entry_count: 0,
      tree_sha256: call(writeState, 433, 'fs_stat', { path: join(trusted, 'folders', 'created') }).result.structuredContent.tree_sha256,
    },
  });
  assert.equal(renameDirectoryResponse.result.structuredContent.schema, 'local.filesystem.rename_directory.v1');
  assert.equal(renameDirectoryResponse.result.structuredContent.status, 'moved');
  assert.equal(renameDirectoryResponse.result.structuredContent.to.relative_path, 'folders/renamed');
  assert.equal(existsSync(join(trusted, 'folders', 'created')), false);
  assert.equal(existsSync(join(trusted, 'folders', 'renamed')), true);
  assert.match(readFileSync(join(auditDir, 'filesystem-mcp-audit.jsonl'), 'utf8'), /fs_rename_directory/);

  const renameFileAsDirectory = call(writeState, 45, 'fs_rename_directory', { from: join(trusted, 'a.txt'), to: join(trusted, 'a-dir') });
  assert.match(renameFileAsDirectory.error.message, /rename_directory_source_not_directory/);

  mkdirSync(join(trusted, 'folders', 'existing'), { recursive: true });
  const renameDestinationExists = call(writeState, 46, 'fs_rename_directory', { from: join(trusted, 'folders', 'renamed'), to: join(trusted, 'folders', 'existing') });
  assert.match(renameDestinationExists.error.message, /move_destination_exists/);

  writeFileSync(join(trusted, 'folders', 'renamed', 'child.txt'), 'child', 'utf8');
  const deleteNonEmptyDirectory = call(writeState, 47, 'fs_delete_directory', { path: join(trusted, 'folders', 'renamed') });
  assert.match(deleteNonEmptyDirectory.error.message, /delete_directory_not_empty/);
  assert.equal(existsSync(join(trusted, 'folders', 'renamed')), true);

  const deleteStat = statSync(join(trusted, 'folders', 'renamed'));
  const staleDeleteGuard = call(writeState, 471, 'fs_delete_directory', { path: join(trusted, 'folders', 'renamed'), recursive: true, expected_size: deleteStat.size + 1 });
  assert.equal(staleDeleteGuard.error.data.code, 'fs_delete_directory_expected_metadata_mismatch');
  const deleteDirectoryResponse = call(writeState, 48, 'fs_delete_directory', {
    path: join(trusted, 'folders', 'renamed'),
    recursive: true,
    expected: call(writeState, 472, 'fs_stat', { path: join(trusted, 'folders', 'renamed') }).result.structuredContent,
  });
  assert.equal(deleteDirectoryResponse.result.structuredContent.schema, 'local.filesystem.delete_directory.v1');
  assert.equal(deleteDirectoryResponse.result.structuredContent.status, 'deleted');
  assert.equal(deleteDirectoryResponse.result.structuredContent.relative_path, 'folders/renamed');
  assert.equal(existsSync(join(trusted, 'folders', 'renamed')), false);
  assert.match(readFileSync(join(auditDir, 'filesystem-mcp-audit.jsonl'), 'utf8'), /fs_delete_directory/);

  const deleteFileAsDirectory = call(writeState, 49, 'fs_delete_directory', { path: join(trusted, 'a.txt') });
  assert.match(deleteFileAsDirectory.error.message, /delete_directory_target_not_directory/);

  const outsideDeleteDirectory = call(writeState, 50, 'fs_delete_directory', { path: join(other, 'outside-folder') });
  assert.equal(outsideDeleteDirectory.error.data.code, 'path_outside_allowed_roots');
  assert.equal(outsideDeleteDirectory.error.data.details.operation, 'fs_delete_directory');
} finally {
  rmSync(tempRoot, { recursive: true, force: true });
}

console.log('local filesystem MCP tests passed');
