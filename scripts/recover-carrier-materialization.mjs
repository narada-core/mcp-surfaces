import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const workspaceRoot = resolve(fileURLToPath(new URL('..', import.meta.url)));
const manifestPath = join(workspaceRoot, '.ai', 'runtime', 'workspace-artifact-manifest.json');

function packageManager() {
  const npmExecPath = process.env.npm_execpath?.trim();
  if (npmExecPath && /\.(?:cjs|mjs|js)$/i.test(npmExecPath) && existsSync(npmExecPath)) return { command: process.execPath, args: [npmExecPath] };
  const corepack = join(dirname(process.execPath), 'node_modules', 'corepack', 'dist', 'pnpm.js');
  if (existsSync(corepack)) return { command: process.execPath, args: [corepack] };
  return { command: process.platform === 'win32' ? 'pnpm.cmd' : 'pnpm', args: [] };
}

function run(command, args, label) {
  const result = spawnSync(command, args, {
    cwd: workspaceRoot,
    env: { ...process.env, NARADA_MCP_WORKSPACE_ROOT: workspaceRoot, NARADA_MCP_SURFACES_ROOT: join(workspaceRoot, 'packages') },
    encoding: 'utf8', timeout: 600_000, maxBuffer: 4 * 1024 * 1024, windowsHide: true,
  });
  if (result.error) throw new Error(`${label}_failed:${result.error.message}`);
  if (result.status !== 0) {
    const output = (String(result.stdout ?? '') + String(result.stderr ?? '')).slice(-8000);
    throw new Error(label + '_failed:status=' + String(result.status) + ':' + output);
  }
}

async function loadPreflight() {
  const entrypoint = join(workspaceRoot, 'packages', 'shared', 'mcp-runtime-proxy', 'dist', 'src', 'workspace-artifact-manifest.js');
  if (!existsSync(entrypoint)) return null;
  const module = await import(pathToFileURL(entrypoint).href + `?recovery=${Date.now()}`);
  return module.preflightWorkspaceArtifacts;
}

async function workspaceIsStale() {
  if (!existsSync(manifestPath)) return { stale: true, reasons: [{ code: 'workspace_manifest_missing', path: manifestPath }] };
  let manifest;
  try { manifest = JSON.parse(readFileSync(manifestPath, 'utf8')); }
  catch { return { stale: true, reasons: [{ code: 'workspace_manifest_unreadable', path: manifestPath }] }; }
  const entrypoints = Array.isArray(manifest?.packages) ? manifest.packages.flatMap((pkg) =>
    Array.isArray(pkg?.export_targets) ? pkg.export_targets.map((target) => target?.path).filter((value) => typeof value === 'string') : []) : [];
  if (entrypoints.length === 0) return { stale: true, reasons: [{ code: 'workspace_manifest_has_no_export_targets', path: manifestPath }] };
  const preflightWorkspaceArtifacts = await loadPreflight();
  if (typeof preflightWorkspaceArtifacts !== 'function') return { stale: true, reasons: [{ code: 'workspace_preflight_unavailable' }] };
  const reasons = [];
  for (const entrypoint of entrypoints) {
    const result = preflightWorkspaceArtifacts({ surfaceId: null, entrypoint, artifactManifestPath: manifestPath });
    if (!result.ok) reasons.push({ code: result.code ?? 'workspace_artifact_refused', entrypoint, reason: result.reason ?? null, details: result.details ?? null });
    if (reasons.length >= 20) break;
  }
  return { stale: reasons.length > 0, reasons };
}

const before = await workspaceIsStale();
let buildPerformed = false;
if (before.stale) {
  const pnpm = packageManager();
  run(pnpm.command, [...pnpm.args, 'run', 'build'], 'carrier_recovery_build');
  buildPerformed = true;
  const after = await workspaceIsStale();
  if (after.stale) throw new Error(`carrier_recovery_build_did_not_restore_artifacts:${JSON.stringify(after.reasons)}`);
}

const registrarEntrypoint = join(workspaceRoot, 'packages', 'mcp-registrar', 'dist', 'src', 'main.js');
if (!existsSync(registrarEntrypoint)) throw new Error(`carrier_recovery_registrar_missing:${registrarEntrypoint}`);
const registrar = await import(pathToFileURL(registrarEntrypoint).href + `?recovery=${Date.now()}`);
if (typeof registrar.inspectAllCarrierMaterialization !== 'function') throw new Error('carrier_recovery_registrar_inspection_unavailable_after_build');
const inspection = registrar.inspectAllCarrierMaterialization();
const materializationRequired = inspection.status !== 'current';
if (materializationRequired) run(process.execPath, [registrarEntrypoint, '--materialize-all'], 'carrier_recovery_materialize_all');

process.stdout.write(`${JSON.stringify({
  schema: 'narada.carrier_materialization_recovery.v1',
  status: buildPerformed || materializationRequired ? 'recovered' : 'current',
  workspace_root: workspaceRoot,
  workspace_was_stale: before.stale,
  workspace_stale_reasons: before.reasons,
  build_performed: buildPerformed,
  carrier_materialization_required: materializationRequired,
  all_carrier_materialization_performed: materializationRequired,
  inspection,
}, null, 2)}\n`);
