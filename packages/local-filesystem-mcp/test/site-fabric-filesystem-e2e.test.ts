import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  createTemporaryE2eRoot,
  installE2eArtifactRecorder,
  removeTemporaryE2eRoot,
  runMcpProtocolSmoke,
  siteFabricChildEnv,
  spawnJsonlMcpServer,
  type JsonRecord,
} from '@narada-core/mcp-e2e-harness';

const siteRoot = createTemporaryE2eRoot('local-filesystem-site-fabric-e2e');
const outsideRoot = createTemporaryE2eRoot('local-filesystem-outside-e2e');
const resultPath = join(fileURLToPath(new URL('../..', import.meta.url)), '.tmp', 'e2e-results', 'local-filesystem.site-fabric.governed-read-write.json');
const evidence = installE2eArtifactRecorder(resultPath, { test_id: 'local-filesystem.site-fabric.governed-read-write', authority: 'A0' });
const serverPath = fileURLToPath(new URL('../src/main.js', import.meta.url));
const server = spawnJsonlMcpServer(process.execPath, [
  serverPath,
  '--mode', 'write',
  '--allowed-root', siteRoot,
  '--output-root', siteRoot,
], {
  cwd: siteRoot,
  env: siteFabricChildEnv(siteRoot),
  label: 'local-filesystem Site fabric e2e',
});
const blockedServer = spawnJsonlMcpServer(process.execPath, [
  serverPath,
  '--mode', 'write',
  '--allowed-root', siteRoot,
  '--output-root', siteRoot,
], {
  cwd: siteRoot,
  env: siteFabricChildEnv(siteRoot, { NARADA_LOCAL_FILESYSTEM_READ_WORKER_BLOCK_MS: '60000' }),
  label: 'local-filesystem blocked-read Site fabric e2e',
});

function structured(response: JsonRecord): JsonRecord {
  assert.equal(response.error, undefined, JSON.stringify(response));
  return (response.result as JsonRecord)?.structuredContent as JsonRecord ?? response.result as JsonRecord;
}

function deliveredReadContent(response: JsonRecord): string {
  const result = response.result as JsonRecord;
  const blocks = result.content as JsonRecord[];
  const text = String(blocks[0]?.text ?? '');
  const marker = '\ncontent:\n';
  const index = text.indexOf(marker);
  assert.notEqual(index, -1, JSON.stringify(response));
  return text.slice(index + marker.length);
}

try {
  await runMcpProtocolSmoke(server.client, {
    expectedServerName: 'local-filesystem-write',
    requiredTools: ['fs_doctor', 'fs_read_file', 'fs_read_file_range', 'fs_write_file', 'fs_stat', 'fs_glob_search'],
  });

  const doctor = structured(await server.client.request(1, 'tools/call', { name: 'fs_doctor', arguments: {} }));
  assert.equal(doctor.status, 'ok', JSON.stringify(doctor));
  assert.equal((doctor.allowed_root_entries as JsonRecord[]).some((item) => String(item.root).replaceAll('\\\\', '/') === siteRoot.replaceAll('\\\\', '/')), true, JSON.stringify(doctor));

  const filePath = join(siteRoot, 'fixture.txt');
  const written = structured(await server.client.request(2, 'tools/call', {
    name: 'fs_write_file',
    arguments: { path: filePath, content: 'alpha\nbeta\n' },
  }));
  assert.equal(written.status, 'written', JSON.stringify(written));
  assert.equal(readFileSync(filePath, 'utf8'), 'alpha\nbeta\n');

  const readResponse = await server.client.request(3, 'tools/call', {
    name: 'fs_read_file',
    arguments: { path: filePath, limit: 1 },
  });
  const read = structured(readResponse);
  assert.equal(deliveredReadContent(readResponse), 'alpha');
  assert.equal(read.content, undefined);
  assert.equal(read.next_offset, 2);
  assert.equal(read.line_window_complete, false);

  const rangeResponse = await server.client.request(4, 'tools/call', {
    name: 'fs_read_file_range',
    arguments: { path: filePath, start_line: 2, end_line: 2 },
  });
  const range = structured(rangeResponse);
  assert.equal(deliveredReadContent(rangeResponse), 'beta');
  assert.equal(range.total_lines_exact, true);

  const stat = structured(await server.client.request(5, 'tools/call', {
    name: 'fs_stat',
    arguments: { path: filePath },
  }));
  assert.equal(stat.type, 'file');
  assert.equal(stat.path, filePath);

  const glob = structured(await server.client.request(6, 'tools/call', {
    name: 'fs_glob_search',
    arguments: { path: siteRoot, pattern: '*.txt', limit: 10 },
  }));
  assert.equal((glob.matches as string[]).some((item) => item.endsWith('fixture.txt')), true);

  const outside = await server.client.request(7, 'tools/call', {
    name: 'fs_read_file',
    arguments: { path: join(outsideRoot, 'not-admitted.txt') },
  });
  assert.equal((outside.error?.data as JsonRecord)?.code, 'path_outside_allowed_roots', JSON.stringify(outside));

  await runMcpProtocolSmoke(blockedServer.client, {
    expectedServerName: 'local-filesystem-write',
    requiredTools: ['fs_read_file_range'],
  });
  const blockedRead = await blockedServer.client.request(3, 'tools/call', {
    name: 'fs_read_file_range',
    arguments: { path: filePath, start_line: 1, end_line: 1, timeout_ms: 5 },
  });
  assert.equal((blockedRead.error?.data as JsonRecord)?.code, 'fs_read_file_range_timed_out', JSON.stringify(blockedRead));
  assert.equal(((blockedRead.error?.data as JsonRecord)?.details as JsonRecord)?.timeout_ms, 5, JSON.stringify(blockedRead));

  console.log(JSON.stringify({ status: 'passed', test_id: 'local-filesystem.site-fabric.governed-read-write', cleanup: existsSync(filePath) ? 'completed_after_finally' : 'completed' }));
  evidence.update({ status: 'passed' });
} finally {
  await server.close();
  await blockedServer.close();
  const cleanupOk = removeTemporaryE2eRoot(siteRoot) && removeTemporaryE2eRoot(outsideRoot);
  evidence.finalize({ status: cleanupOk ? 'passed' : 'failed', cleanup: { status: cleanupOk ? 'completed_after_finally' : 'failed' } });
  assert.equal(cleanupOk, true);
}

console.log('local-filesystem Site fabric e2e ok');
