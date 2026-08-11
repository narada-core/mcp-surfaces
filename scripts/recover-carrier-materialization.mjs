import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { writeRecoveryEvidence } from './carrier-recovery-evidence.mjs';
import { fileURLToPath, pathToFileURL } from 'node:url';

const workspaceRoot = resolve(fileURLToPath(new URL('..', import.meta.url)));
const manifestPath = join(workspaceRoot, '.ai', 'runtime', 'workspace-artifact-manifest.json');
const evidenceRoot = join(workspaceRoot, '.ai', 'runtime', 'carrier-materialization-recovery');
const restartPressurePath = join(workspaceRoot, '.ai', 'runtime', 'carrier-restart-pressure.json');

function readRestartPressure() {
  if (!existsSync(restartPressurePath)) return { schema: 'narada.carrier_restart_pressure.v1', carriers: {} };
  try {
    const value = JSON.parse(readFileSync(restartPressurePath, 'utf8'));
    return value?.schema === 'narada.carrier_restart_pressure.v1' && value.carriers && typeof value.carriers === 'object'
      ? value
      : { schema: 'narada.carrier_restart_pressure.v1', carriers: {} };
  } catch {
    throw new Error('carrier_restart_pressure_unreadable:' + restartPressurePath);
  }
}

function writeRestartPressure(value) {
  mkdirSync(dirname(restartPressurePath), { recursive: true });
  const temporary = restartPressurePath + '.tmp-' + process.pid;
  writeFileSync(temporary, JSON.stringify(value, null, 2) + '\n', 'utf8');
  renameSync(temporary, restartPressurePath);
}

const acknowledgeCarrierIndex = process.argv.indexOf('--ack-carrier');
if (acknowledgeCarrierIndex >= 0) {
  const carrierId = process.argv[acknowledgeCarrierIndex + 1]?.trim();
  if (!carrierId) throw new Error('carrier_restart_ack_carrier_id_required');
  const pressure = readRestartPressure();
  const acknowledged = pressure.carriers[carrierId] ?? null;
  const expectedRefIndex = process.argv.indexOf('--expected-pressure-ref');
  const expectedRef = expectedRefIndex >= 0 ? process.argv[expectedRefIndex + 1]?.trim() : null;
  if (acknowledged && (!expectedRef || acknowledged.evidence_ref !== expectedRef)) {
    process.stdout.write(JSON.stringify({ schema: 'narada.carrier_restart_acknowledgement.v1', status: 'stale_ack_refused', carrier_id: carrierId, expected_pressure_ref: expectedRef, current_pressure: acknowledged, remaining_carrier_ids: Object.keys(pressure.carriers).sort() }) + '\n');
    process.exit(2);
  }
  delete pressure.carriers[carrierId];
  pressure.updated_at = new Date().toISOString();
  writeRestartPressure(pressure);
  process.stdout.write(JSON.stringify({ schema: 'narada.carrier_restart_acknowledgement.v1', status: acknowledged ? 'acknowledged' : 'already_current', carrier_id: carrierId, acknowledged_pressure: acknowledged, remaining_carrier_ids: Object.keys(pressure.carriers).sort() }) + '\n');
  process.exit(0);
}

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
const staleCarrierIds = Array.isArray(inspectionBefore.stale_carrier_ids) ? inspectionBefore.stale_carrier_ids : [];
const evidence = writeRecoveryEvidence({
  workspaceRoot,
  evidenceRoot,
  value: {
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
  },
  pin: materializationRequired,
});
const pressure = readRestartPressure();
if (materializationRequired) {
  for (const carrierId of staleCarrierIds) {
    pressure.carriers[carrierId] = {
      carrier_id: carrierId,
      materialized_at: completedAt,
      evidence_ref: evidence.ref,
      latest_materialization_ref: evidence.latest_materialization?.ref ?? null,
    };
  }
  pressure.updated_at = completedAt;
  writeRestartPressure(pressure);
}
const restartCarrierIds = Object.keys(pressure.carriers).sort();
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
  restart_required: restartCarrierIds.length > 0,
  restart_carrier_ids: restartCarrierIds,
  restart_pressure_path: restartPressurePath,
  restart_pressure: pressure.carriers,
  evidence,
}) + '\n');
