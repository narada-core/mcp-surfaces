import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = mkdtempSync(join(tmpdir(), 'narada-clean-room-site-'));
const surfacesRoot = resolve(fileURLToPath(new URL('..', import.meta.url)));
try {
  const result = spawnSync(process.execPath, ['--import', 'tsx', 'test/clean-room-site-journey.e2e.test.ts'], {
    cwd: surfacesRoot,
    env: { ...process.env, NARADA_CLEAN_ROOM_SITE_ROOT: root },
    encoding: 'utf8',
    timeout: 180_000,
  });
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  if (result.error) throw result.error;
  if (result.status !== 0) process.exitCode = result.status ?? 1;
} finally {
  rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}
