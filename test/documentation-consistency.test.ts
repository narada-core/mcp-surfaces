import assert from 'node:assert/strict';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const packagesRoot = join(repoRoot, 'packages');
const markdownTick = String.fromCharCode(96);

function walkFiles(root) {
  const result = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    if (entry.name === 'node_modules' || entry.name === 'dist') continue;
    const path = join(root, entry.name);
    if (entry.isDirectory()) result.push(...walkFiles(path));
    else result.push(path);
  }
  return result;
}

function packageManifests() {
  return walkFiles(packagesRoot)
    .filter((path) => path.endsWith('package.json'))
    .map((path) => ({
      path,
      relativePath: relative(repoRoot, dirname(path)).replaceAll('\\', '/'),
      manifest: JSON.parse(readFileSync(path, 'utf8')),
    }));
}

function packageReadme(entry) {
  return join(entry.path, '..', 'README.md');
}

function inventoryEntries() {
  const path = join(repoRoot, 'docs', 'package-inventory.md');
  const lines = readFileSync(path, 'utf8').split(/\r?\n/);
  return lines
    .filter((line) => {
      const firstColumn = line.split('|')[1]?.trim() || '';
      return firstColumn.startsWith(markdownTick + 'packages/');
    })
    .map((line) => {
      const columns = line.split('|').slice(1, -1).map((column) => column.trim());
      const stripTicks = (value) => value.replace(new RegExp('^' + markdownTick + '|' + markdownTick + '$', 'g'), '');
      return {
        relativePath: stripTicks(columns[0]),
        name: stripTicks(columns[1]),
      };
    });
}

function isRunnable(entry) {
  const parts = entry.relativePath.split('/');
  return parts.length === 2 && parts[0] === 'packages';
}

function requiredVerificationCommand(text, packageName) {
  return text.includes('pnpm --filter ' + packageName + ' test') ||
    (text.includes('pnpm') && text.includes('test'));
}

function localMarkdownLinks(path, text) {
  const links = [];
  const pattern = /\[[^\]]*\]\(([^)]+)\)/g;
  for (const match of text.matchAll(pattern)) {
    let target = match[1].trim();
    if (target.startsWith('<') && target.includes('>')) {
      target = target.slice(1, target.indexOf('>'));
    } else {
      target = target.split(/\s+/)[0];
    }
    if (!target || target.startsWith('#') || /^[a-z][a-z0-9+.-]*:/i.test(target)) continue;
    const targetPath = target.split('#', 1)[0];
    if (!targetPath) continue;
    let resolved;
    if (targetPath.startsWith('packages/') || targetPath.startsWith('docs/')) {
      resolved = resolve(repoRoot, targetPath);
    } else {
      resolved = resolve(dirname(path), targetPath);
    }
    links.push({ target, resolved });
  }
  return links;
}

test('package manifests match the canonical package inventory', () => {
  const manifests = packageManifests();
  const inventory = inventoryEntries();
  const manifestKeys = manifests.map((entry) => entry.relativePath + '|' + entry.manifest.name).sort();
  const inventoryKeys = inventory.map((entry) => entry.relativePath + '|' + entry.name).sort();
  const missing = manifestKeys.filter((entry) => !inventoryKeys.includes(entry));
  const stale = inventoryKeys.filter((entry) => !manifestKeys.includes(entry));
  assert.deepEqual(
    { count: inventory.length, missing, stale },
    { count: manifests.length, missing: [], stale: [] },
    'canonical inventory differs from package manifests',
  );
});

test('every package has a contract README', () => {
  const failures = [];
  for (const entry of packageManifests()) {
    const readme = packageReadme(entry);
    if (!statSafe(readme)) {
      failures.push(entry.relativePath + ': missing README.md');
      continue;
    }
    const text = readFileSync(readme, 'utf8');
    if (!/^# .+/m.test(text)) failures.push(entry.relativePath + ': missing H1');

    const verification = /^## (?:Verification|Verify)\s*$/m.test(text);
    if (!verification || !requiredVerificationCommand(text, entry.manifest.name)) {
      failures.push(entry.relativePath + ': missing finite Verification command');
    }

    if (isRunnable(entry)) {
      const tools = /^(?:##|###)\s+.*\bTools?\b(?:\s+.*)?\s*$/im.test(text);
      if (!tools) failures.push(entry.relativePath + ': missing Tools or Tool groups section');
    }
  }
  assert.deepEqual(failures, [], failures.join('\n'));
});

test('documented local Markdown links resolve', () => {
  const candidates = [
    join(repoRoot, 'docs', 'package-inventory.md'),
    join(repoRoot, 'docs', 'package-readme-contract.md'),
    ...packageManifests().map(packageReadme),
  ];
  const failures = [];
  for (const path of candidates) {
    const text = readFileSync(path, 'utf8');
    for (const link of localMarkdownLinks(path, text)) {
      if (!statSafe(link.resolved)) {
        failures.push(relative(repoRoot, path).replaceAll('\\', '/') + ' -> ' + link.target);
      }
    }
  }
  assert.deepEqual(failures, [], failures.join('\n'));
});

test('registrar wiring and recovery docs name the current runtime matrix', () => {
  const wiring = readFileSync(join(repoRoot, 'docs', 'mcp-wiring.md'), 'utf8');
  const recovery = readFileSync(join(repoRoot, 'docs', 'mcp-materialization-recovery.md'), 'utf8');
  for (const profile of ['native', 'bun', 'node-compat']) {
    assert.match(wiring, new RegExp(profile, 'i'), 'wiring docs omit runtime profile ' + profile);
    assert.match(recovery, new RegExp(profile, 'i'), 'recovery docs omit runtime profile ' + profile);
  }
  const allCarrierCommand = /(?:--materialize-all|pnpm materialize:carrier|materialize-all-carriers\.ps1)/i;
  assert.match(wiring, allCarrierCommand, 'wiring docs omit an all-carrier materialization command');
  assert.match(recovery, allCarrierCommand, 'recovery docs omit an all-carrier materialization command');
});

function statSafe(path) {
  try {
    return statSync(path).isFile();
  } catch {
    return false;
  }
}