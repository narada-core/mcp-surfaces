import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const workspaceRoot = resolve(fileURLToPath(new URL('..', import.meta.url)));
const manifestPath = join(workspaceRoot, '.ai', 'runtime', 'workspace-artifact-manifest.json');
const evidenceRoot = join(workspaceRoot, '.ai', 'runtime', 'carrier-materialization-recovery');

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
  if (result.error) throw new Error(label + '_failed:' + result.error.message);
  if (result.status !== 0) {
    const output = (String(result.stdout ?? '') + String(result.stderr ?? '')).slice(-8000);
    throw new Error(label + '_failed:status=' + String(result.status) + ':' + output);
  }
}

async function loadPreflight() {
  const entrypoint = join(workspaceRoot, 'packages', 'shared', 'mcp-runtime-proxy', 'dist', 'src', 'workspace-artifact-manifest.js');
  if (!existsSync(entrypoint)) return null;
  const module = await import(pathToFileURL(entrypoint).href + '?recovery=' + Date.now());
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

function writeEvidence(value) {
  const content = JSON.stringify(value, null, 2) + '\n';
  const sha256 = createHash('sha256').update(content, 'utf8').digest('hex');
  const id = new Date().toISOString().replace(/[-:.TZ]/g, '').slice(0, 14) + '-' + sha256.slice(0, 16);
  const path = join(evidenceRoot, id + '.json');
  const temporary = path + '.tmp-' + process.pid;
  mkdirSync(evidenceRoot, { recursive: true });
  writeFileSync(temporary, content, 'utf8');
  try {
    renameSync(temporary, path);
  } finally {
    if (existsSync(temporary)) rmSync(temporary, { force: true });
  }
  return {
    schema: 'narada.carrier_materialization_recovery.evidence_ref.v1',
    ref: 'carrier-materialization-recovery:' + id,
    path,
    relative_path: relative(workspaceRoot, path).replace(/\\/g, '/'),
    sha256,
  };
}

const startedAt = new Date().toISOString();
const before = await workspaceIsStale();
let buildPerformed = false;
let afterBuild = before;
if (before.stale) {
  const pnpm = packageManager();
  run(pnpm.command, [...pnpm.args, 'run', 'build'], 'carrier_recovery_build');
  buildPerformed = true;
  afterBuild = await workspaceIsStale();
  if (afterBuild.stale) throw new Error('carrier_recovery_build_did_not_restore_artifacts:' + JSON.stringify(afterBuild.reasons));
}

const registrarEntrypoint = join(workspaceRoot, 'packages', 'mcp-registrar', 'dist', 'src', 'main.js');
if (!existsSync(registrarEntrypoint)) throw new Error('carrier_recovery_registrar_missing:' + registrarEntrypoint);
const registrar = await import(pathToFileURL(registrarEntrypoint).href + '?recovery=' + Date.now());
if (typeof registrar.inspectAllCarrierMaterialization !== 'function') throw new Error('carrier_recovery_registrar_inspection_unavailable_after_build');
const inspectionBefore = registrar.inspectAllCarrierMaterialization();
const materializationRequired = inspectionBefore.status !== 'current';
if (materializationRequired) run(process.execPath, [registrarEntrypoint, '--materialize-all'], 'carrier_recovery_materialize_all');
const inspectionAfter = materializationRequired ? registrar.inspectAllCarrierMaterialization() : inspectionBefore;
if (inspectionAfter.status !== 'current') throw new Error('carrier_recovery_materialization_did_not_converge:' + JSON.stringify(inspectionAfter.stale_carrier_ids));

const completedAt = new Date().toISOString();
const evidence = writeEvidence({
  schema: 'narada.carrier_materialization_recovery.evidence.v1',
  started_at: startedAt,
  completed_at: completedAt,
  workspace_root: workspaceRoot,
  workspace_before: before,
  workspace_after_build: afterBuild,
  build_performed: buildPerformed,
  materialization_required: materializationRequired,
  materialization_performed: materializationRequired,
  inspection_before: inspectionBefore,
  inspection_after: inspectionAfter,
});
const staleCarrierIds = Array.isArray(inspectionBefore.stale_carrier_ids) ? inspectionBefore.stale_carrier_ids : [];
const reasonCodes = [...new Set(before.reasons.map((reason) => String(reason.code ?? 'unknown')))].sort();
process.stdout.write(JSON.stringify({
  schema: 'narada.carrier_materialization_recovery.v1',
  status: buildPerformed || materializationRequired ? 'recovered' : 'current',
  workspace_root: workspaceRoot,
  workspace_was_stale: before.stale,
  workspace_stale_reason_count: before.reasons.length,
  workspace_stale_reason_codes: reasonCodes,
  build_performed: buildPerformed,
  carrier_count: inspectionBefore.carrier_count,
  stale_carrier_count: staleCarrierIds.length,
  affected_carrier_ids: staleCarrierIds,
  carrier_materialization_required: materializationRequired,
  all_carrier_materialization_performed: materializationRequired,
  restart_required: materializationRequired,
  restart_carrier_ids: materializationRequired ? staleCarrierIds : [],
  evidence,
}) + '\n');
