import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
const denied = [
  /mcp-registrar[\\/]src[\\/]main\.ts/,
  /mcp-registrar[\\/]dist[\\/]src[\\/]main\.js/,
  /from\s+['"][^'"]*mcp-registrar(?:\/dist\/src\/main\.js)?['"]/,
];
const ignored = new Set([
  '.git',
  '.ai',
  '.tmp',
  '.tmp-tests',
  'artifacts',
  'dist',
  'executions',
  'node_modules',
  'target',
]);
const findings = [];

function visit(directory) {
  for (const name of readdirSync(directory)) {
    if (ignored.has(name)) continue;
    const path = join(directory, name);
    const stats = statSync(path);
    if (stats.isDirectory()) visit(path);
    else if (/\.(?:ts|tsx|js|mjs|cjs|rs|json|md|toml|ps1)$/.test(name)) {
      const text = readFileSync(path, 'utf8');
      for (const pattern of denied) {
        if (pattern.test(text)) findings.push(`${relative(root, path)}: ${pattern}`);
      }
    }
  }
}

visit(root);
if (findings.length) {
  console.error(['registrar_typescript_authority_detected', ...findings.slice(0, 100)].join('\n'));
  process.exit(1);
}
console.log('registrar native authority guard ok');
