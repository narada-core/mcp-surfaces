import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { buildCreateScheduleArgs, buildScheduledTaskLaunchPlan, buildScheduledTaskMutationScript, buildTaskRunCommand, compactScheduledTaskRows, createServerState, handleRequest, normalizeScheduledTaskDefinition, schedulerRuntimeStatus, scheduledActionPolicyReasons, schedulerFailureDetails, splitScheduledTaskPath } from '../src/main.js';

const state = createServerState({});
const runtimeStatus = schedulerRuntimeStatus();
assert.equal(runtimeStatus.status, 'fresh');
assert.equal(state.implementationId, runtimeStatus.implementation_id);
assert.match(buildScheduledTaskMutationScript(), /ExecutionTimeLimit/);
assert.match(buildScheduledTaskMutationScript(), /MultipleInstances/);
assert.match(buildScheduledTaskMutationScript(), /-TaskPath \$taskPath/);
assert.match(buildScheduledTaskMutationScript(), /Get-ScheduledTask -TaskName \$taskName -TaskPath \$taskPath/);
assert.match(buildScheduledTaskMutationScript(), /if \(\$wasDisabled\) \{ \$settingsArguments\.Disable = \$true \}/);
assert.match(buildScheduledTaskMutationScript(), /New-ScheduledTaskPrincipal .*InteractiveToken .*Limited/);
assert.match(buildScheduledTaskMutationScript(), /AllowStartIfOnBatteries = \$true/);
assert.match(buildScheduledTaskMutationScript(), /DontStopIfGoingOnBatteries = \$true/);
assert.match(buildScheduledTaskMutationScript(), /-Principal \$principal/);
assert.deepEqual(splitScheduledTaskPath('\\Narada\\SonarOperatingProgramDispatch'), {
  taskName: 'SonarOperatingProgramDispatch',
  taskPath: '\\Narada\\',
});
const noWindowPlan = buildScheduledTaskLaunchPlan(
  'node.exe',
  '"C:\\workspace\\narada.sonar\\scripts\\sonar-operating-program.ts" dispatch-once',
);
assert.equal(noWindowPlan.console_window_policy, 'native_create_no_window');
assert.equal(noWindowPlan.launcher_argv[0], '--scheduled-v1');
assert.deepEqual(normalizeScheduledTaskDefinition({
  execute: noWindowPlan.launcher_path,
  arguments: noWindowPlan.launcher_arguments,
}), {
  execute: 'node.exe',
  arguments: '"C:\\workspace\\narada.sonar\\scripts\\sonar-operating-program.ts" dispatch-once',
  console_window_policy: 'native_create_no_window',
  launcher_execute: noWindowPlan.launcher_path,
  launcher_arguments: noWindowPlan.launcher_arguments,
});
assert.deepEqual(splitScheduledTaskPath('\\Narada-Sonar-Daemon'), {
  taskName: 'Narada-Sonar-Daemon',
  taskPath: '\\',
});

const canonicalWrapperRoot = mkdtempSync(join(tmpdir(), 'scheduler-mcp-wrapper-'));
try {
  const canonicalWrapper = join(canonicalWrapperRoot, 'canonical-entrypoint.cmd');
  writeFileSync(canonicalWrapper, '@echo off\r\n', 'utf8');
  const canonicalWrapperState = createServerState({ allowedRoots: [canonicalWrapperRoot] });
  const canonicalWrapperReasons = scheduledActionPolicyReasons('pwsh.exe', `-File "${canonicalWrapper}"`, canonicalWrapperRoot, canonicalWrapperState);
  assert.equal(canonicalWrapperReasons.some((reason) => reason.startsWith('scheduler_transient_wrapper_refused:')), false);
} finally {
  rmSync(canonicalWrapperRoot, { recursive: true, force: true });
}

const grouped = compactScheduledTaskRows([
  { TaskName: '\\Narada-Sonar-Daemon', Status: 'Ready', 'Schedule Type': 'At logon time', 'Next Run Time': 'N/A', 'Last Run Time': 'today', 'Last Result': '0', 'Task To Run': 'pwsh.exe' },
  { TaskName: '\\Narada-Sonar-Daemon', Status: 'Ready', 'Schedule Type': 'One Time Only, Minute', 'Next Run Time': 'soon', 'Last Run Time': 'today', 'Last Result': '0', 'Task To Run': 'pwsh.exe', 'Repeat: Every': '5 minutes' },
]);
assert.equal(grouped.length, 1);
assert.equal(grouped[0].task_name, '\\Narada-Sonar-Daemon');
assert.equal(grouped[0].trigger_count, 2);
assert.deepEqual((grouped[0].triggers as Record<string, unknown>[]).map((trigger) => trigger.schedule), ['At logon time', 'One Time Only, Minute']);

const accessDenied = schedulerFailureDetails({ operation: 'create', exitCode: 1, stderr: 'ERROR: Access is denied.', taskName: '\\NeedsAdmin', command: 'pwsh.exe -File tool.ps1' });
assert.equal(accessDenied.classification, 'requires_elevation');
assert.equal(accessDenied.requires_elevation, true);
assert.match(String(accessDenied.remediation), /elevated PowerShell/);

const invalidArgs = schedulerFailureDetails({ operation: 'create', exitCode: 2147500037, stderr: '', taskName: '\\BadTask' });
assert.equal(invalidArgs.classification, 'invalid_arguments_or_unsupported_scheduler_option');
const timedOut = schedulerFailureDetails({ operation: 'update_action', exitCode: -2, taskName: '\\SlowTask', command: 'pwsh.exe', timedOut: true, timeoutMs: 25 });
assert.equal(timedOut.classification, 'scheduler_command_timed_out');
assert.equal(timedOut.timed_out, true);
assert.equal(timedOut.timeout_ms, 25);
assert.equal(timedOut.operator_command, null);
assert.match(String(timedOut.operator_command_note), /preview-only/);
assert.equal(buildTaskRunCommand('pwsh.exe', '-File tool.ps1'), 'pwsh.exe -File tool.ps1');
assert.deepEqual(buildCreateScheduleArgs('hourly', { interval_minutes: 15 }), ['/sc', 'minute', '/mo', '15']);
assert.deepEqual(buildCreateScheduleArgs('hourly', { interval_minutes: 120 }), ['/sc', 'hourly', '/mo', '2']);
assert.deepEqual(buildCreateScheduleArgs('hourly', { interval_minutes: 90 }), ['/sc', 'minute', '/mo', '90']);
assert.ok(scheduledActionPolicyReasons('cmd.exe', '/c tool.cmd', null, state).some((reason) => reason.startsWith('scheduler_shell_action_disallowed:')));
assert.ok(scheduledActionPolicyReasons('pwsh.exe', '-File C:\\workspace\\site\\tool.cmd', null, state).some((reason) => reason.startsWith('scheduler_transient_wrapper_refused:')));
assert.ok(scheduledActionPolicyReasons('pwsh.exe', '-File C:\\workspace\\site\\.ai\\tmp\\tool.ps1', null, state).some((reason) => reason.startsWith('scheduler_transient_script_path_refused:')));
assert.ok(scheduledActionPolicyReasons('pwsh.exe', '-File "C:\\workspace\\site\\.ai\\tmp\\tool.ps1"', null, state).some((reason) => reason.startsWith('scheduler_transient_script_path_refused:')));
const rootedState = createServerState({ allowedRoots: ['C:\\workspace\\site'] });
assert.ok(scheduledActionPolicyReasons('pwsh.exe', '-File C:\\workspace\\site\\tool.ps1', 'D:\\other-site', rootedState).some((reason) => reason.startsWith('scheduler_working_dir_outside_allowed_root:')));

async function callTool(name: string, args: Record<string, unknown>): Promise<Record<string, any>> {
  return handleRequest({ jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args } }, state) as Promise<Record<string, any>>;
}
function view(res: Record<string, any>): Record<string, any> {
  return res.result.structuredContent as Record<string, any>;
}

const dryRunCreate = await callTool('scheduler_task_create', {
  task_name: '\\Narada\\DryRun',
  command: 'pwsh.exe',
  arguments: '-NoProfile -File C:\\workspace\\narada.sonar\\scripts\\supervisor.ps1 start',
  working_dir: 'C:\\workspace\\narada.sonar',
  schedule: 'hourly',
  interval_minutes: 15,
  implementation_id: runtimeStatus.implementation_id,
  dry_run: true,
});
const dryRunCreateData = view(dryRunCreate);
assert.equal(dryRunCreateData.status, 'planned');
assert.equal(dryRunCreateData.principal.logon_type, 'InteractiveToken');
assert.equal(dryRunCreateData.principal.run_level, 'Limited');
assert.equal(dryRunCreateData.battery_policy.allow_start, true);
assert.equal(dryRunCreateData.battery_policy.stop_when_switching_to_battery, false);
assert.deepEqual(dryRunCreateData.schtasks_preview_args.slice(0, 4), ['/create', '/tn', '\\Narada\\DryRun', '/tr']);

if (process.env.NARADA_RUN_LIVE_SCHEDULER_TESTS === '1') {
  const list = await callTool('scheduler_task_list', { limit: 5 });
  const listData = view(list);
  assert.ok(Array.isArray(listData.items), 'list should return items array');
  assert.ok(typeof listData.count === 'number', 'list should have count');
  if (listData.items.length > 0) {
    const first = listData.items[0] as Record<string, any>;
    assert.ok(first.task_name, 'task should have task_name');
    assert.ok(first.status, 'task should have status');
  }

  const knownTask = listData.items[0] as Record<string, any> | undefined;
  if (knownTask) {
    const show = await callTool('scheduler_task_show', { task_name: knownTask.task_name as string });
    const showData = view(show);
    assert.ok(showData.task, 'show should return task');
    assert.equal(showData.task.TaskName, knownTask.task_name);

    const history = await callTool('scheduler_task_history', { task_name: knownTask.task_name as string, limit: 3 });
    const historyData = view(history);
    assert.ok(Array.isArray(historyData.items), 'history should return items');
  }
} else {
  console.log('scheduler-mcp live scheduler assertions skipped; set NARADA_RUN_LIVE_SCHEDULER_TESTS=1 to run them');
}

const dryRunUpdate = await callTool('scheduler_task_update_action', {
  task_name: '\\Narada-Sonar-Daemon',
  command: 'pwsh.exe',
  implementation_id: runtimeStatus.implementation_id,
  arguments: '-NoProfile -File C:\\workspace\\narada.sonar\\scripts\\supervisor.ps1 start',
  working_dir: 'C:\\workspace\\narada.sonar',
  execution_time_limit_seconds: 180,
  multiple_instances: 'ignore_new',
  dry_run: true,
});
const dryRunUpdateData = view(dryRunUpdate);
assert.equal(dryRunUpdateData.status, 'planned');
assert.equal(dryRunUpdateData.preserves_triggers, true);
assert.equal(dryRunUpdateData.preserves_enabled_state, true);
assert.equal(dryRunUpdateData.enabled_state_preservation, 'scheduled_task_settings_disable_flag');
assert.equal(dryRunUpdateData.mutation_method, 'powershell_set_scheduled_task_native_no_window_action');
assert.equal(dryRunUpdateData.console_window_policy, 'native_create_no_window');
assert.match(String(dryRunUpdateData.launcher_execute), /narada-process-supervisor\.exe$/i);
assert.match(String(dryRunUpdateData.launcher_arguments), /^--scheduled-v1\s+[A-Za-z0-9_-]+$/);
assert.equal(dryRunUpdateData.execute, 'pwsh.exe');
assert.equal(dryRunUpdateData.arguments, '-NoProfile -File C:\\workspace\\narada.sonar\\scripts\\supervisor.ps1 start');
assert.equal(dryRunUpdateData.schtasks_fallback, undefined);
assert.equal(dryRunUpdateData.schtasks_preview_not_used_for_mutation, true);
assert.deepEqual(dryRunUpdateData.schtasks_preview_args.slice(0, 4), ['/change', '/tn', '\\Narada-Sonar-Daemon', '/tr']);
assert.equal(dryRunUpdateData.working_dir, 'C:\\workspace\\narada.sonar');
assert.equal(dryRunUpdateData.working_dir_applied, false);
assert.equal(dryRunUpdateData.working_dir_would_apply, true);
assert.equal(dryRunUpdateData.execution_time_limit_seconds, 180);
assert.equal(dryRunUpdateData.multiple_instances, 'IgnoreNew');

console.log('scheduler-mcp behavior ok');
