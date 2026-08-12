import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';

const write = process.argv.includes('--write');
const output = execFileSync('git', ['ls-files', '--eol', '-z'], { encoding: 'utf8' });
const mismatches = [];

for (const record of output.split('\0')) {
  if (!record) continue;
  const tab = record.indexOf('\t');
  if (tab < 0) throw new Error(`newline_check_record_invalid:${JSON.stringify(record)}`);
  const metadata = record.slice(0, tab);
  const path = record.slice(tab + 1);
  const working = /(?:^|\s)w\/(\S+)/.exec(metadata)?.[1];
  const expected = /(?:^|\s)eol=(lf|crlf)(?:\s|$)/.exec(metadata)?.[1];
  if (!working || !['lf', 'crlf', 'mixed', 'none'].includes(working)) continue;
  if (!expected || working === 'none' || working === expected) continue;
  mismatches.push({ path, expected, actual: working });
}

if (write) {
  for (const mismatch of mismatches) {
    const bytes = readFileSync(mismatch.path);
    const lf = [];
    for (let index = 0; index < bytes.length; index += 1) {
      if (bytes[index] === 0x0d) {
        if (bytes[index + 1] === 0x0a) index += 1;
        lf.push(0x0a);
      } else {
        lf.push(bytes[index]);
      }
    }
    if (mismatch.expected === 'lf') {
      writeFileSync(mismatch.path, Buffer.from(lf));
      continue;
    }
    const crlf = [];
    for (const byte of lf) {
      if (byte === 0x0a) crlf.push(0x0d);
      crlf.push(byte);
    }
    writeFileSync(mismatch.path, Buffer.from(crlf));
  }
  process.stdout.write(`normalized ${mismatches.length} tracked text file(s)\n`);
  process.exit(0);
}

if (mismatches.length > 0) {
  for (const mismatch of mismatches.slice(0, 100)) {
    process.stderr.write(`${mismatch.path}: expected ${mismatch.expected}, found ${mismatch.actual}\n`);
  }
  if (mismatches.length > 100) process.stderr.write(`...and ${mismatches.length - 100} more\n`);
  process.stderr.write('Run pnpm normalize:newlines.\n');
  process.exit(1);
}

process.stdout.write('tracked text newline policy ok\n');
