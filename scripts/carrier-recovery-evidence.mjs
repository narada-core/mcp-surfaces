import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { basename, join, relative } from 'node:path';

export const DEFAULT_RECOVERY_EVIDENCE_MAX_FILES = 64;
const EVIDENCE_FILE_PATTERN = /^\d{14}(?:\d{3})?-[a-f0-9]{16}\.json$/;

export function recoveryEvidenceMaxFiles(value = process.env.NARADA_MCP_RECOVERY_EVIDENCE_MAX_FILES) {
  if (value === undefined || String(value).trim() === '') return DEFAULT_RECOVERY_EVIDENCE_MAX_FILES;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > 10_000) {
    throw new Error('carrier_recovery_evidence_max_files_invalid:' + String(value));
  }
  return parsed;
}

export function writeRecoveryEvidence({ workspaceRoot, evidenceRoot, value, maxFiles = recoveryEvidenceMaxFiles(), now = () => new Date() }) {
  const content = JSON.stringify(value, null, 2) + '\n';
  const sha256 = createHash('sha256').update(content, 'utf8').digest('hex');
  const timestamp = now().toISOString().replace(/\D/g, '');
  const id = timestamp + '-' + sha256.slice(0, 16);
  const path = join(evidenceRoot, id + '.json');
  const temporary = path + '.tmp-' + process.pid;
  mkdirSync(evidenceRoot, { recursive: true });
  if (!existsSync(path)) {
    writeFileSync(temporary, content, 'utf8');
    try {
      try {
        renameSync(temporary, path);
      } catch (error) {
        if (!existsSync(path)) throw error;
      }
    } finally {
      if (existsSync(temporary)) rmSync(temporary, { force: true });
    }
  }

  const currentName = basename(path);
  const retained = readdirSync(evidenceRoot)
    .filter((name) => EVIDENCE_FILE_PATTERN.test(name))
    .sort((left, right) => {
      if (left === currentName) return -1;
      if (right === currentName) return 1;
      return right.localeCompare(left);
    });
  const pruned = retained.slice(maxFiles);
  for (const name of pruned) rmSync(join(evidenceRoot, name), { force: true });

  return {
    schema: 'narada.carrier_materialization_recovery.evidence_ref.v1',
    ref: 'carrier-materialization-recovery:' + id,
    path,
    relative_path: relative(workspaceRoot, path).replace(/\\/g, '/'),
    sha256,
    retention: {
      policy: 'current_then_newest_files',
      max_files: maxFiles,
      retained_count: Math.min(retained.length, maxFiles),
      pruned_count: pruned.length,
    },
  };
}
