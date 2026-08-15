import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { buildGuidanceResult } from '../src/guidance.js';
import {
  createServerState,
  buildElevatedWindowBrokerCommand,
  buildStructuredCommandExecutionPayload,
  executeStructuredCommand,
  handleRequest,
} from '../src/main.js';
import { decideStructuredCommandExecution } from '../src/policy.js';
type DynamicTestValue = string & DynamicTestValue[] & {
  [key: string]: DynamicTestValue;
  [index: number]: DynamicTestValue;
};

type JsonRpcTestResponse = {
  result: DynamicTestValue;
  error: DynamicTestValue;
};

type ExecutionResult = Record<string, unknown> & {
  status: string;
  executed: boolean;
  stdout: string;
};
const root = mkdtempSync(join(tmpdir(), 'structured-command-mcp-'));
const auditLogDir = join(root, 'audit');
const trustConfigPath = join(root, 'config.toml');
writeFileSync(trustConfigPath, `[projects.'${root.replaceAll('\\', '\\\\')}']\ntrust_level = "trusted"\n`, 'utf8');

const state = createServerState({
  allowedRoot: root,
  allowCommand: ['node'],
  allowPrefix: ['git status'],
  auditLogDir,
});
const stateWithPwsh = createServerState({
  allowedRoot: root,
  allowCommand: ['node', 'pwsh', 'pwsh.exe', 'powershell.exe'],
  auditLogDir: join(root, 'audit-pwsh'),
});
const stateWithPwshPrefix = createServerState({
  allowedRoot: root,
  allowPrefix: ['pwsh -File'],
  auditLogDir: join(root, 'audit-pwsh-prefix'),
});
const stateFromTrustConfig = createServerState({
  rootsFromTrustConfig: trustConfigPath,
  allowCommand: ['node'],
  auditLogDir: join(root, 'audit-trust-config'),
});
const stateWithDefaultCommands = createServerState({
  allowedRoot: root,
  auditLogDir: join(root, 'audit-default-commands'),
});
const siteRoot = join(root, 'site-root');
const outsideRoot = join(root, 'extra-root');
mkdirSync(join(siteRoot, '.narada'), { recursive: true });
writeFileSync(join(siteRoot, '.narada', 'allowed-roots.json'), JSON.stringify({ extra_allowed_roots: [outsideRoot] }), 'utf8');
writeFileSync(join(siteRoot, '.narada', 'secrets.json'), JSON.stringify({ env: { STRUCTURED_COMMAND_TEST_SECRET: 'from-site-secret' } }), 'utf8');
const originalStructuredCommandSecret = process.env.STRUCTURED_COMMAND_TEST_SECRET;
delete process.env.STRUCTURED_COMMAND_TEST_SECRET;
const stateFromSiteRoot = createServerState({
  siteRoot,
  allowedRoot: root,
  allowCommand: ['node'],
  auditLogDir: join(root, 'audit-site-root'),
});
if (originalStructuredCommandSecret === undefined) delete process.env.STRUCTURED_COMMAND_TEST_SECRET;
else process.env.STRUCTURED_COMMAND_TEST_SECRET = originalStructuredCommandSecret;
const rpc = handleRequest as unknown as (request: Record<string, unknown>, requestState: typeof state) => Promise<JsonRpcTestResponse>;
const exec = executeStructuredCommand as unknown as (args: Record<string, unknown>, requestState: typeof state) => Promise<ExecutionResult>;

const recoveryGuidance = buildGuidanceResult().recovery as string[];
assert.ok(recoveryGuidance.some((entry) => entry.includes('Transport closed')));
assert.ok(recoveryGuidance.some((entry) => entry.includes('input_ref') && entry.includes('execution_ref')));
assert.ok(recoveryGuidance.some((entry) => entry.includes('mcp_loader_surface_restart')));

assert.equal(state.policy.maxTimeoutMs, 900_000);
assert.equal(stateFromSiteRoot.policy.allowedRoots.some((allowedRoot) => allowedRoot === outsideRoot), true);
assert.equal(stateFromSiteRoot.env.STRUCTURED_COMMAND_TEST_SECRET, 'from-site-secret');
assert.equal(originalStructuredCommandSecret === undefined ? process.env.STRUCTURED_COMMAND_TEST_SECRET === undefined : process.env.STRUCTURED_COMMAND_TEST_SECRET === originalStructuredCommandSecret, true);

const stateEnvExecution = await exec({ command: 'node', args: ['-e', 'process.stdout.write(process.env.STRUCTURED_COMMAND_TEST_SECRET || "")'], working_directory: root }, stateFromSiteRoot);
assert.equal(stateEnvExecution.stdout, 'from-site-secret');

for (const command of ['railway', 'wrangler']) {
  const decision = decideStructuredCommandExecution({
    command,
    args: ['--version'],
    workingDirectory: root,
  }, stateWithDefaultCommands.policy);
  assert.equal(decision.status, 'allowed');
  assert.deepEqual(decision.reasons, []);
}

const defaultPwshFile = decideStructuredCommandExecution({
  command: 'pwsh.exe',
  args: ['-File', join(root, 'tool.ps1')],
  workingDirectory: root,
}, stateWithDefaultCommands.policy);
assert.equal(defaultPwshFile.status, 'allowed');
assert.deepEqual(defaultPwshFile.reasons, []);

const defaultPwshNoProfileFile = decideStructuredCommandExecution({
  command: 'pwsh',
  args: ['-NoProfile', '-File', join(root, 'tool.ps1')],
  workingDirectory: root,
}, stateWithDefaultCommands.policy);
assert.equal(defaultPwshNoProfileFile.status, 'allowed');
assert.deepEqual(defaultPwshNoProfileFile.reasons, []);

const defaultPwshExecutionPolicyFile = decideStructuredCommandExecution({
  command: 'pwsh.exe',
  args: ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(root, 'tool.ps1')],
  workingDirectory: root,
}, stateWithDefaultCommands.policy);
assert.equal(defaultPwshExecutionPolicyFile.status, 'allowed');
assert.deepEqual(defaultPwshExecutionPolicyFile.reasons, []);

const cmdWrapper = decideStructuredCommandExecution({
  command: join(root, '.ai', 'tmp', 'native-focused-tests.cmd'),
  workingDirectory: root,
}, stateWithDefaultCommands.policy);
assert.equal(cmdWrapper.status, 'refused');
assert.ok(cmdWrapper.reasons.some((reason) => String(reason).startsWith('wrapper_execution_disallowed:')));

const canonicalBatchEntrypoint = join(root, 'canonical-entrypoint.cmd');
writeFileSync(canonicalBatchEntrypoint, '@echo off\r\n', 'utf8');
const canonicalBatch = decideStructuredCommandExecution({
  command: 'pwsh',
  args: ['-File', canonicalBatchEntrypoint],
  workingDirectory: root,
}, stateWithDefaultCommands.policy);
assert.equal(canonicalBatch.status, 'allowed');
assert.equal(canonicalBatch.reasons.some((reason) => String(reason).startsWith('wrapper_execution_disallowed:')), false);

const transientPowerShellWrapper = decideStructuredCommandExecution({
  command: 'pwsh',
  args: ['-NoProfile', '-File', join(root, '.ai', 'tmp', 'native-focused-tests.ps1')],
  workingDirectory: root,
}, stateWithDefaultCommands.policy);
assert.equal(transientPowerShellWrapper.status, 'refused');
assert.ok(transientPowerShellWrapper.reasons.some((reason) => String(reason).startsWith('transient_wrapper_path_disallowed:')));

const defaultPwshCommand = decideStructuredCommandExecution({
  command: 'pwsh',
  args: ['-Command', 'Write-Output nope'],
  workingDirectory: root,
}, stateWithDefaultCommands.policy);
assert.equal(defaultPwshCommand.status, 'refused');
assert.ok(defaultPwshCommand.reasons.some((reason) => String(reason).startsWith('command_not_allowed:')));

const defaultPnpmTest = decideStructuredCommandExecution({ command: 'pnpm', args: ['test'], workingDirectory: root }, stateWithDefaultCommands.policy);
assert.equal(defaultPnpmTest.status, 'allowed');
assert.deepEqual(defaultPnpmTest.reasons, []);
const defaultPnpmFilteredTest = decideStructuredCommandExecution({ command: 'pnpm', args: ['--filter', '@narada-core/structured-command-mcp', 'test'], workingDirectory: root }, stateWithDefaultCommands.policy);
assert.equal(defaultPnpmFilteredTest.status, 'allowed');
assert.deepEqual(defaultPnpmFilteredTest.reasons, []);
const defaultPnpmFilteredDeploy = decideStructuredCommandExecution({ command: 'pnpm', args: ['--filter', '@narada-core/structured-command-mcp', 'deploy'], workingDirectory: root }, stateWithDefaultCommands.policy);
assert.equal(defaultPnpmFilteredDeploy.status, 'refused');
assert.ok(defaultPnpmFilteredDeploy.reasons.some((reason) => String(reason).startsWith('command_not_allowed:')));

const wrappedCargo = decideStructuredCommandExecution({ command: 'pnpm', args: ['exec', 'cargo', 'check'], workingDirectory: root }, stateWithDefaultCommands.policy);
assert.equal(wrappedCargo.status, 'refused');
assert.ok(wrappedCargo.reasons.includes('package_manager_wrapper_for_native_tool:pnpm cargo'));
assert.ok(wrappedCargo.remediation_hints.includes('Invoke cargo directly; pnpm is not part of the native Rust toolchain.'));

const defaultCargoFmt = decideStructuredCommandExecution({ command: 'cargo', args: ['fmt', '--check'], workingDirectory: root }, stateWithDefaultCommands.policy);
assert.equal(defaultCargoFmt.status, 'allowed');
assert.deepEqual(defaultCargoFmt.reasons, []);
const defaultCargoTest = decideStructuredCommandExecution({ command: 'cargo', args: ['test', '--manifest-path', join(root, 'Cargo.toml')], workingDirectory: root }, stateWithDefaultCommands.policy);
assert.equal(defaultCargoTest.status, 'allowed');
assert.deepEqual(defaultCargoTest.reasons, []);

const defaultNaradaPlanner = decideStructuredCommandExecution({
  command: 'narada',
  args: ['launcher', 'workspace-plan', '--agent', 'example', '--format', 'json'],
  workingDirectory: root,
}, stateWithDefaultCommands.policy);
assert.equal(defaultNaradaPlanner.status, 'allowed');
assert.deepEqual(defaultNaradaPlanner.reasons, []);
const mutatingNaradaDoctor = decideStructuredCommandExecution({
  command: 'narada',
  args: ['doctor', '--repair'],
  workingDirectory: root,
}, stateWithDefaultCommands.policy);
assert.equal(mutatingNaradaDoctor.status, 'refused');
assert.ok(mutatingNaradaDoctor.reasons.some((reason) => String(reason).startsWith('command_not_allowed:')));
writeFileSync(join(root, 'package.json'), JSON.stringify({ scripts: { test: 'node -e "process.exit(0)"' } }), 'utf8');
const completeExecution = buildStructuredCommandExecutionPayload({
  decision: { command: 'pnpm', args: ['test'], working_directory: root },
  result: {
    exit_code: 0,
    stdout: '',
    stderr: '',
    stdout_truncated: false,
    stderr_truncated: false,
    timed_out: false,
    cancelled: false,
    command_resolution: {
      status: 'resolved',
      spawn_command: 'pnpm',
      spawn_args: ['test'],
      invocation_argv: ['pnpm', 'test'],
    },
  },
  startedAt: new Date(0).toISOString(),
  timeoutMs: 30_000,
  executionPosture: { test_scope: 'focused', expected_cost: 'low' },
  inputRef: null,
  executionMode: 'sync',
  waitForCompletion: true,
});
assert.equal(completeExecution.status, 'ok');
assert.equal(completeExecution.producer_evidence.status, 'complete');
assert.deepEqual(completeExecution.producer_evidence.invocation_argv, ['pnpm', 'test']);

const undeclaredExecution = buildStructuredCommandExecutionPayload({
  decision: { command: 'pnpm', args: ['test:missing'], working_directory: root },
  result: {
    exit_code: 0,
    stdout: '',
    stderr: '',
    stdout_truncated: false,
    stderr_truncated: false,
    timed_out: false,
    cancelled: false,
    command_resolution: {
      status: 'resolved',
      spawn_command: 'pnpm',
      spawn_args: ['test:missing'],
      invocation_argv: ['pnpm', 'test:missing'],
    },
  },
  startedAt: new Date(0).toISOString(),
  timeoutMs: 30_000,
  executionPosture: { test_scope: 'focused', expected_cost: 'low' },
  inputRef: null,
  executionMode: 'sync',
  waitForCompletion: true,
});
assert.equal(undeclaredExecution.status, 'verification_incomplete');
assert.equal(undeclaredExecution.producer_evidence.status, 'insufficient');

const focusedTestPosture = await exec({
  command: 'pnpm',
  args: ['--filter', '@narada-core/structured-command-mcp', 'test'],
  working_directory: root,
}, stateWithDefaultCommands);
assert.equal(focusedTestPosture.test_scope, 'focused');
assert.equal(focusedTestPosture.expected_cost, 'low');

const backgroundRefused = await (executeStructuredCommand as any)({
  command: 'node',
  args: ['--version'],
  working_directory: root,
  test_scope: 'focused',
  wait_for_completion: false,
}, state);
assert.equal(backgroundRefused.status, 'refused');
assert.ok(backgroundRefused.refusal_reasons.includes('background_requires_known_slow_test_scope'));

const backgroundStarted = await (executeStructuredCommand as any)({
  command: 'node',
  args: ['-e', 'setTimeout(() => process.stdout.write("known-slow-ok"), 150)'],
  working_directory: root,
  timeout_ms: 1000,
  test_scope: 'known_slow',
  wait_for_completion: false,
}, state);
assert.equal(backgroundStarted.status, 'running');
assert.equal(backgroundStarted.pending, true);
assert.equal(backgroundStarted.wait_for_completion, false);
assert.match(String(backgroundStarted.execution_ref), /^structured_command_execution:/);
// A replacement server state bound to the same storage root can observe the
// detached runner's completion; the originating state owns no completion promise.
const replacementState = createServerState({
  allowedRoot: root,
  allowCommand: ['node'],
  allowPrefix: ['git status'],
  auditLogDir,
});
let backgroundCompleted = await (executeStructuredCommand as any)({ execution_ref: backgroundStarted.execution_ref }, replacementState);
for (let attempt = 0; attempt < 100 && backgroundCompleted.status === 'running'; attempt++) {
  await new Promise((resolve) => setTimeout(resolve, 50));
  backgroundCompleted = await (executeStructuredCommand as any)({ execution_ref: backgroundStarted.execution_ref }, replacementState);
}
assert.equal(backgroundCompleted.status, 'ok');
assert.equal(backgroundCompleted.pending, false);
assert.equal(backgroundCompleted.execution_mode, 'background');
assert.equal(backgroundCompleted.stdout, 'known-slow-ok');

const init = await rpc({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} }, state);
assert.equal(init.result.serverInfo.name, 'structured-command-mcp');

const tools = await rpc({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} }, state);
const toolNames = tools.result.tools.map((tool) => tool.name).sort();
assert.deepEqual(toolNames, [
  'structured_command_elevated_window_execute',
  'structured_command_execute',
  'structured_command_execution_policy_inspect',
  'structured_command_execution_show',
  'structured_command_guidance',
  'structured_command_input_create',
  'structured_command_output_show',
  'structured_command_powershell_parse_check',
  'structured_command_start',
]);
const executeTool = tools.result.tools.find((tool) => tool.name === 'structured_command_execute')!;
assert.equal(executeTool.canonical_name, 'structured_command_execute');
assert.equal(executeTool.annotations.canonicalName, 'structured_command_execute');
assert.ok(executeTool.inputSchema.properties.execution_ref);
assert.ok(executeTool.inputSchema.properties.stdout_offset);
assert.ok(executeTool.inputSchema.properties.stdout_limit);
assert.ok(executeTool.inputSchema.properties.wait_for_completion);
assert.ok(executeTool.inputSchema.properties.test_scope);
assert.ok(executeTool.inputSchema.properties.expected_cost);
const startTool = tools.result.tools.find((tool) => tool.name === 'structured_command_start')!;
assert.equal(startTool.annotations.canonicalName, 'structured_command_start');
const showTool = tools.result.tools.find((tool) => tool.name === 'structured_command_execution_show')!;
assert.deepEqual(showTool.inputSchema.required, ['execution_ref']);

const tooLongForSync = await (executeStructuredCommand as any)({
  command: 'node',
  args: ['--version'],
  working_directory: root,
  timeout_ms: 240_001,
}, state);
assert.equal(tooLongForSync.status, 'refused');
assert.ok(tooLongForSync.refusal_reasons.includes('synchronous_timeout_exceeds_reliable_bound'));

const broker = buildElevatedWindowBrokerCommand({
  command: 'pwsh.exe',
  args: ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(root, 'admin-tool.ps1'), "O'Hare"],
  workingDirectory: root,
  wait: false,
});
assert.equal(broker.command, 'powershell.exe');
assert.ok(broker.args.includes('-Command'));
assert.match(broker.script, /-Verb RunAs/);
assert.match(broker.script, /O''Hare/);

const elevatedDryRun = await rpc({
  jsonrpc: '2.0',
  id: 51,
  method: 'tools/call',
  params: {
    name: 'structured_command_elevated_window_execute',
    arguments: {
      command: 'pwsh.exe',
      args: ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(root, 'admin-tool.ps1')],
      working_directory: root,
      dry_run: true,
    },
  },
}, stateWithDefaultCommands);
assert.equal(elevatedDryRun.result.structuredContent.status, 'planned');
assert.equal(elevatedDryRun.result.structuredContent.executed, false);
assert.equal(elevatedDryRun.result.structuredContent.broker.command, 'powershell.exe');
assert.match(elevatedDryRun.result.structuredContent.broker.script, /Start-Process/);

const elevatedBlocked = await rpc({
  jsonrpc: '2.0',
  id: 52,
  method: 'tools/call',
  params: {
    name: 'structured_command_elevated_window_execute',
    arguments: {
      command: 'node',
      args: ['--version'],
      working_directory: root,
    },
  },
}, state);
assert.equal(elevatedBlocked.result.structuredContent.status, 'refused');
assert.ok(elevatedBlocked.result.structuredContent.refusal_reasons.includes('confirm_elevation_required'));

const ok = await exec({
  command: 'node',
  args: ['--version'],
  working_directory: root,
  test_scope: 'focused',
  expected_cost: 'low',
}, state);
assert.equal(ok.status, 'ok');
assert.equal(ok.executed, true);
assert.match(ok.stdout, /^v\d+/);
assert.equal(ok.test_scope, 'focused');
assert.equal(ok.expected_cost, 'low');
assert.equal((ok.execution_posture as Record<string, unknown>).source, 'caller_declared');
const okWithLongerTimeout = await exec({
  command: 'node',
  args: ['--version'],
  working_directory: root,
  timeout_ms: 120_000,
}, state);
assert.equal(okWithLongerTimeout.status, 'ok');
assert.equal(okWithLongerTimeout.timeout_ms, 120_000);

const timedOut = await exec({
  command: 'node',
  args: ['-e', 'setTimeout(() => {}, 10000)'],
  working_directory: root,
  timeout_ms: 50,
}, state);
assert.equal(timedOut.status, 'timed_out');
assert.equal(timedOut.executed, true);
assert.equal(timedOut.timed_out, true);
assert.equal(timedOut.cancelled, false);
assert.match(String(timedOut.execution_ref), /^structured_command_execution:/);

// Timeout and cancellation may contend at the same boundary, but the result
// state must remain mutually exclusive rather than reporting both causes.
const timeoutCancellationRace = new AbortController();
const raceAbortTimer = setTimeout(() => timeoutCancellationRace.abort(), 50);
const raced = await (executeStructuredCommand as any)({
  command: 'node',
  args: ['-e', 'setTimeout(() => {}, 10000)'],
  working_directory: root,
  timeout_ms: 50,
}, state, { abortSignal: timeoutCancellationRace.signal });
clearTimeout(raceAbortTimer);
assert.equal(raced.timed_out && raced.cancelled, false);
assert.ok(raced.status === 'timed_out' || raced.status === 'cancelled');

// A timed-out command must not leave descendant processes running: the
// timeout path kills the whole child tree, not just the direct process.
const grandchildPidFile = join(root, 'grandchild.pid');
const timedOutTree = await exec({
  command: 'node',
  args: ['-e', `const { spawn } = require('node:child_process'); const { writeFileSync } = require('node:fs'); const grandchild = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore', windowsHide: true }); writeFileSync(${JSON.stringify(grandchildPidFile)}, String(grandchild.pid)); setInterval(() => {}, 1000);`],
  working_directory: root,
  timeout_ms: 50,
}, state);
assert.equal(timedOutTree.status, 'timed_out');
assert.equal(timedOutTree.timed_out, true);
const grandchildPid = Number(readFileSync(grandchildPidFile, 'utf8'));
assert.ok(Number.isInteger(grandchildPid) && grandchildPid > 0, `expected grandchild pid file, got ${readFileSync(grandchildPidFile, 'utf8')}`);
assert.throws(() => process.kill(grandchildPid, 0), `grandchild process ${grandchildPid} survived the timed-out command`);

// A descendant that ignores SIGTERM must not survive either: on POSIX the
// process group gets SIGTERM and a bounded-grace SIGKILL escalation, and on
// Windows taskkill /T /F forces the tree down regardless.
const stubbornPidFile = join(root, 'stubborn-grandchild.pid');
const timedOutStubbornTree = await exec({
  command: 'node',
  args: ['-e', `const { spawn } = require('node:child_process'); const { writeFileSync } = require('node:fs'); const grandchild = spawn(process.execPath, ['-e', 'process.on("SIGTERM", () => {}); setInterval(() => {}, 1000)'], { stdio: 'ignore', windowsHide: true }); writeFileSync(${JSON.stringify(stubbornPidFile)}, String(grandchild.pid)); setInterval(() => {}, 1000);`],
  working_directory: root,
  timeout_ms: 50,
}, state);
assert.equal(timedOutStubbornTree.status, 'timed_out');
assert.equal(timedOutStubbornTree.timed_out, true);
const stubbornGrandchildPid = Number(readFileSync(stubbornPidFile, 'utf8'));
assert.ok(Number.isInteger(stubbornGrandchildPid) && stubbornGrandchildPid > 0, `expected stubborn grandchild pid file, got ${readFileSync(stubbornPidFile, 'utf8')}`);
assert.throws(() => process.kill(stubbornGrandchildPid, 0), `stubborn grandchild process ${stubbornGrandchildPid} survived the timed-out command`);

// The surface stays usable on the same state after a timed-out call.
const afterTimeout = await exec({
  command: 'node',
  args: ['--version'],
  working_directory: root,
  timeout_ms: 30_000,
}, state);
assert.equal(afterTimeout.status, 'ok');

const stateWithTinyOutput = createServerState({
  allowedRoot: root,
  allowCommand: ['node'],
  auditLogDir: join(root, 'audit-tiny-output'),
  maxOutputBytes: 120,
});
const truncatedTail = await exec({
  command: 'node',
  args: ['-e', 'process.stdout.write("prefix-".repeat(200) + "TAIL_SENTINEL")'],
  working_directory: root,
}, stateWithTinyOutput);
assert.equal(truncatedTail.status, 'ok');
assert.equal(truncatedTail.stdout_truncated, true);
assert.match(truncatedTail.stdout, /preserved tail/);
assert.match(truncatedTail.stdout, /TAIL_SENTINEL/);

const okFromTrustConfig = await exec({
  command: 'node',
  args: ['--version'],
  working_directory: root,
}, stateFromTrustConfig);
assert.equal(okFromTrustConfig.status, 'ok');

assert.equal(okFromTrustConfig.executed, true);

const allowedPwsh = decideStructuredCommandExecution({
  command: 'pwsh',
  args: ['-NoProfile'],
  workingDirectory: root,
}, stateWithPwsh.policy);
assert.equal(allowedPwsh.status, 'allowed');
assert.deepEqual(allowedPwsh.reasons, []);

const allowedPwshExe = decideStructuredCommandExecution({
  command: 'pwsh.exe',
  args: ['-NoProfile'],
  workingDirectory: root,
}, stateWithPwsh.policy);
assert.equal(allowedPwshExe.status, 'allowed');
assert.deepEqual(allowedPwshExe.reasons, []);

const allowedPwshExeByPwshPrefix = decideStructuredCommandExecution({
  command: 'pwsh.exe',
  args: ['-File', join(root, 'tool.ps1')],
  workingDirectory: root,
}, stateWithPwshPrefix.policy);
assert.equal(allowedPwshExeByPwshPrefix.status, 'allowed');
assert.deepEqual(allowedPwshExeByPwshPrefix.reasons, []);

const blockedWindowsPowerShell = decideStructuredCommandExecution({
  command: 'powershell.exe',
  args: ['-NoProfile'],
  workingDirectory: root,
}, stateWithPwsh.policy);
assert.equal(blockedWindowsPowerShell.status, 'refused');
assert.ok(blockedWindowsPowerShell.reasons.includes('blocked_command:powershell.exe'));

const refusedRoot = await exec({
  command: 'node',
  args: ['--version'],
  working_directory: tmpdir(),
}, state);
assert.equal(refusedRoot.status, 'refused');

const refusedCall = await rpc({
  jsonrpc: '2.0',
  id: 32,
  method: 'tools/call',
  params: {
    name: 'structured_command_execute',
    arguments: { command: 'pwsh.exe', args: ['-Command', 'Write-Output nope'], working_directory: root },
  },
}, state);
assert.match(refusedCall.result.content[0].text, /structured_command_execute: refused/);
assert.match(refusedCall.result.content[0].text, /refusal_reasons: command_not_allowed/);
assert.equal(refusedCall.result.structuredContent.status, 'refused');
assert.equal(refusedCall.result.structuredContent.executed, false);
assert.ok(refusedCall.result.structuredContent.refusal_reasons.some((reason) => String(reason).startsWith('command_not_allowed:')));

const refusedGitAdd = await exec({
  command: 'git',
  args: ['add', 'README.md'],
  working_directory: root,
}, state);
assert.equal(refusedGitAdd.status, 'refused');
assert.deepEqual(refusedGitAdd.remediation_hints, ['Use the governed Git MCP tool git_add instead of shelling out to git.']);

const refusedGitStatus = await exec({
  command: 'git',
  args: ['status', '--short'],
  working_directory: root,
}, stateWithDefaultCommands);
assert.equal(refusedGitStatus.status, 'refused');
assert.deepEqual(refusedGitStatus.remediation_hints, ['Use the governed Git MCP tool git_status instead of shelling out to git.']);
assert.deepEqual(refusedGitStatus.mcp_fallbacks, [{
  surface_id: 'git',
  tool: 'git_status',
  tool_name: 'git_status',
  canonical_name: 'git_status',
  purpose: 'git_operation',
  arguments: { working_directory: root },
}]);

const refusedSearch = await rpc({
  jsonrpc: '2.0',
  id: 33,
  method: 'tools/call',
  params: {
    name: 'structured_command_execute',
    arguments: { command: 'rg', args: ['needle'], working_directory: root },
  },
}, state);
assert.equal(refusedSearch.result.structuredContent.status, 'refused');
assert.deepEqual(refusedSearch.result.structuredContent.remediation_hints, ['Use local-filesystem fs_grep_search for content search or fs_glob_search for file pattern search.']);
assert.deepEqual(refusedSearch.result.structuredContent.mcp_fallbacks[0], {
  surface_id: 'local-filesystem',
  tool_name: 'fs_grep_search',
  canonical_name: 'fs_grep_search',
  purpose: 'content_search',
  arguments: {
    pattern: 'needle',
    path: root,
    output_mode: 'content',
  },
});
assert.equal(refusedSearch.result.structuredContent.mcp_fallbacks[1].tool_name, 'fs_glob_search');
assert.equal(refusedSearch.result.structuredContent.decision.mcp_fallbacks[0].tool_name, 'fs_grep_search');
assert.match(refusedSearch.result.content[0].text, /remediation_hints: Use local-filesystem fs_grep_search/);

const refusedScopedRg = await exec({
  command: 'rg',
  args: ['needle', 'packages/one', 'packages/two', '-g', '!dist/**'],
  working_directory: root,
}, state);
assert.equal(refusedScopedRg.status, 'refused');
const refusedScopedRgFallbacks = refusedScopedRg.mcp_fallbacks as Array<Record<string, any>>;
assert.deepEqual(refusedScopedRgFallbacks.filter((fallback) => fallback.tool_name === 'fs_grep_search').map((fallback) => fallback.arguments.path), [
  join(root, 'packages/one'),
  join(root, 'packages/two'),
]);
assert.deepEqual(refusedScopedRgFallbacks[0].arguments.ignore, ['dist/**']);

const refusedRgFiles = await exec({
  command: 'rg',
  args: ['--files', '-g', '*.ts'],
  working_directory: root,
}, state) as any;
assert.equal(refusedRgFiles.status, 'refused');
assert.equal(refusedRgFiles.mcp_fallbacks[0].tool_name, 'fs_glob_search');
assert.deepEqual(refusedRgFiles.mcp_fallbacks[0].arguments, { pattern: '*.ts', directory: root });

const ps1Path = join(root, 'parse-ok.ps1');
writeFileSync(ps1Path, 'Write-Output "ok"\n', 'utf8');
const parseCheck = await rpc({
  jsonrpc: '2.0',
  id: 34,
  method: 'tools/call',
  params: {
    name: 'structured_command_powershell_parse_check',
    arguments: { path: ps1Path, working_directory: root },
  },
}, state);
const parsePayload = parseCheck.result.structuredContent;
if (parsePayload.resolution_error_code === 'powershell_host_not_found') {
  assert.equal(parsePayload.command_resolution.code, 'powershell_host_not_found');
} else {
  assert.equal(parsePayload.status, 'ok');
  assert.equal(parsePayload.arbitrary_command_execution_admitted, false);
}

const psd1Path = join(root, 'parse-ok.psd1');
writeFileSync(psd1Path, '@{ Name = "ok" }\n', 'utf8');
const dataParseCheck = await rpc({
  jsonrpc: '2.0',
  id: 35,
  method: 'tools/call',
  params: {
    name: 'structured_command_powershell_parse_check',
    arguments: { path: psd1Path, working_directory: root },
  },
}, state);
const dataParsePayload = dataParseCheck.result.structuredContent;
if (parsePayload.resolution_error_code === 'powershell_host_not_found') {
  assert.equal(dataParsePayload.command_resolution.code, 'powershell_host_not_found');
} else {
  assert.equal(dataParsePayload.status, 'ok');
}

const environmentState = createServerState(
  { allowedRoot: root, allowCommand: ['node'] },
  { HOME: 'C:/fixture-home', GIT_AUTHOR_NAME: 'Fixture Author', PATH: 'C:/fixture-bin' },
);
assert.equal(environmentState.env.HOME, 'C:/fixture-home');
assert.equal(environmentState.env.GIT_AUTHOR_NAME, 'Fixture Author');
assert.equal(environmentState.env.USERPROFILE, 'C:/fixture-home');
assert.equal(environmentState.env.PATH, 'C:/fixture-bin');

const longInlineScript = `${' '.repeat(318)}process.stdout.write('long-inline-ok')`;
const okLongInlineArg = await exec({
  command: 'node',
  args: ['-e', longInlineScript],
  working_directory: root,
}, state);
assert.equal(okLongInlineArg.status, 'ok');
assert.equal(okLongInlineArg.executed, true);
assert.equal(okLongInlineArg.stdout, 'long-inline-ok');

await assert.rejects(
  () => exec({
    command: 'node',
    args: ['x'.repeat(20001)],
    working_directory: root,
  }, state),
  /structured_command_input_too_long:arguments\.args\[0\]:20001>20000/,
);

const policy = await rpc({
  jsonrpc: '2.0',
  id: 3,
  method: 'tools/call',
  params: {
    name: 'structured_command_execution_policy_inspect',
    arguments: {},
  },
}, state);
assert.match(policy.result.content[0].text, /structured_command\.execution_policy/);
assert.ok(policy.result.content[0].text.length <= 4000);
assert.equal(policy.result.structuredContent.truncated, false);
assert.equal(policy.result.structuredContent.output_ref, undefined);
assert.equal(policy.result.structuredContent.allowed_commands.includes('railway'), true);
assert.equal(policy.result.structuredContent.allowed_commands.includes('wrangler'), true);
assert.deepEqual(policy.result.structuredContent.default_allowed_commands, ['railway', 'wrangler']);
assert.deepEqual(policy.result.structuredContent.default_allowed_prefixes, [
  'pnpm test',
  'pnpm build',
  'pnpm typecheck',
  'pnpm --filter',
  'cargo fmt',
  'cargo check',
  'cargo test',
  'narada launcher workspace-plan',
  'narada doctor',
  'pwsh -file',
  'pwsh -noprofile -file',
  'pwsh -noprofile -executionpolicy bypass -file',
]);
assert.equal(policy.result.structuredContent.shell_interpolation, false);
assert.deepEqual(policy.result.structuredContent.allowed_roots, [root]);

const unknownTool = await rpc({
  jsonrpc: '2.0',
  id: 31,
  method: 'tools/call',
  params: {
    name: 'structured_command_missing_tool',
    arguments: {},
  },
}, state);
assert.equal(unknownTool.error.data.schema, 'narada.structured_command.error.v0');
assert.equal(unknownTool.error.data.code, 'structured_command_unknown_tool');
assert.equal(unknownTool.error.data.details.tool_name, 'structured_command_missing_tool');

const input = await rpc({
  jsonrpc: '2.0',
  id: 4,
  method: 'tools/call',
  params: {
    name: 'structured_command_input_create',
    arguments: { input_id: 'inputtest1', command: 'node', args: ['--version'], working_directory: root },
  },
}, state);
assert.match(input.result.content[0].text, /structured_command\.input_create_result/);

const inputRef = input.result.structuredContent.input_ref;
assert.match(inputRef, /^structured_command_input:/);
assert.equal(input.result.structuredContent.status, 'created');
assert.equal(typeof input.result.structuredContent.sha256, 'string');

const badInputRef = await rpc({
  jsonrpc: '2.0',
  id: 41,
  method: 'tools/call',
  params: {
    name: 'structured_command_execute',
    arguments: { input_ref: 'structured_command_output:not_an_input' },
  },
}, state);
assert.equal(badInputRef.error.data.schema, 'narada.structured_command.error.v0');
assert.equal(badInputRef.error.data.code, 'structured_command_invalid_input_ref');
assert.equal(badInputRef.error.data.details.expected_kind, 'input');

const smallCall = await rpc({
  jsonrpc: '2.0',
  id: 5,
  method: 'tools/call',
  params: {
    name: 'structured_command_execute',
    arguments: { input_ref: inputRef },
  },
}, state);
assert.equal(smallCall.result.content[0].type, 'text');
assert.match(smallCall.result.content[0].text, /structured_command_execute: ok/);
assert.match(smallCall.result.content[0].text, /stdout:/);
assert.ok(smallCall.result.content[0].text.length <= 4000);
assert.equal(smallCall.result.structuredContent.schema, 'narada.structured_command.execution_result.v0');
assert.equal(smallCall.result.structuredContent.status, 'ok');
assert.equal(smallCall.result.structuredContent.exit_code, 0);
assert.match(smallCall.result.structuredContent.stdout, /^v\d+/);
assert.match(smallCall.result.structuredContent.execution_ref, /^structured_command_execution:/);
assert.equal(smallCall.result.structuredContent.stdout_next_offset, null);

const largeCall = await rpc({
  jsonrpc: '2.0',
  id: 6,
  method: 'tools/call',
  params: {
    name: 'structured_command_execute',
    arguments: {
      command: 'node',
      args: ['-e', "process.stdout.write('x'.repeat(6500))"],
      working_directory: root,
    },
  },
}, state);
assert.equal(largeCall.result.content[0].type, 'text');
assert.ok(largeCall.result.content[0].text.length <= 4000);
assert.match(largeCall.result.content[0].text, /stdout preview truncated/);
assert.equal(largeCall.result.structuredContent.schema, 'narada.structured_command.execution_result.v0');
assert.equal(largeCall.result.structuredContent.status, 'ok');
assert.equal(largeCall.result.structuredContent.stdout_char_length, 6500);
assert.equal(largeCall.result.structuredContent.stdout, 'x'.repeat(1000));
assert.match(largeCall.result.structuredContent.execution_ref, /^structured_command_execution:/);
assert.equal(largeCall.result.structuredContent.stdout_next_offset, 1000);
assert.equal(largeCall.result.structuredContent.stdout_output_truncated, true);

const failedLargeStdoutCall = await rpc({
  jsonrpc: '2.0',
  id: 61,
  method: 'tools/call',
  params: {
    name: 'structured_command_execute',
    arguments: {
      command: 'node',
      args: ['-e', "process.stdout.write('x'.repeat(6500)); process.stderr.write('not ok 1 - failure before payload\\n'); process.exit(1);"],
      working_directory: root,
    },
  },
}, state);
assert.equal(failedLargeStdoutCall.result.structuredContent.status, 'failed');
assert.match(failedLargeStdoutCall.result.content[0].text, /structured_command_execute: failed/);
assert.match(failedLargeStdoutCall.result.content[0].text, /stderr:\nnot ok 1 - failure before payload/);
assert.ok(failedLargeStdoutCall.result.content[0].text.indexOf('stderr:') < failedLargeStdoutCall.result.content[0].text.indexOf('stdout:'));
assert.ok(failedLargeStdoutCall.result.content[0].text.length <= 4000);
assert.equal(failedLargeStdoutCall.result.structuredContent.stdout_char_length, 6500);
assert.equal(failedLargeStdoutCall.result.structuredContent.stdout, 'x'.repeat(1000));
assert.equal(failedLargeStdoutCall.result.structuredContent.stderr, 'not ok 1 - failure before payload\n');
assert.match(failedLargeStdoutCall.result.structuredContent.execution_ref, /^structured_command_execution:/);
assert.equal(failedLargeStdoutCall.result.structuredContent.stdout_next_offset, 1000);

const failedStdoutPage = await rpc({
  jsonrpc: '2.0',
  id: 62,
  method: 'tools/call',
  params: {
    name: 'structured_command_execute',
    arguments: { execution_ref: failedLargeStdoutCall.result.structuredContent.execution_ref, stdout_offset: 6000, stdout_limit: 1000 },
  },
}, state);
assert.equal(failedStdoutPage.result.structuredContent.stdout, 'x'.repeat(500));
assert.equal(failedStdoutPage.result.structuredContent.stdout_next_offset, null);

const stdoutPage1 = await rpc({
  jsonrpc: '2.0',
  id: 7,
  method: 'tools/call',
  params: {
    name: 'structured_command_execute',
    arguments: { execution_ref: largeCall.result.structuredContent.execution_ref, stdout_limit: 4000 },
  },
}, state);
assert.equal(stdoutPage1.result.structuredContent.stdout, 'x'.repeat(4000));
assert.equal(stdoutPage1.result.structuredContent.stdout_offset, 0);
assert.equal(stdoutPage1.result.structuredContent.stdout_limit, 4000);
assert.equal(stdoutPage1.result.structuredContent.stdout_next_offset, 4000);
assert.equal(stdoutPage1.result.structuredContent.stdout_output_truncated, true);
assert.equal(stdoutPage1.result.structuredContent.stdout_char_length, 6500);

const stdoutCustomPage = await rpc({
  jsonrpc: '2.0',
  id: 8,
  method: 'tools/call',
  params: {
    name: 'structured_command_execute',
    arguments: { execution_ref: largeCall.result.structuredContent.execution_ref, stdout_offset: 1000, stdout_limit: 1200 },
  },
}, state);
assert.equal(stdoutCustomPage.result.structuredContent.stdout, 'x'.repeat(1200));
assert.equal(stdoutCustomPage.result.structuredContent.stdout_offset, 1000);
assert.equal(stdoutCustomPage.result.structuredContent.stdout_limit, 1200);
assert.equal(stdoutCustomPage.result.structuredContent.stdout_next_offset, 2200);

const stdoutFinalPage = await rpc({
  jsonrpc: '2.0',
  id: 9,
  method: 'tools/call',
  params: {
    name: 'structured_command_execute',
    arguments: { execution_ref: largeCall.result.structuredContent.execution_ref, stdout_offset: 6000, stdout_limit: 4000 },
  },
}, state);
assert.equal(stdoutFinalPage.result.structuredContent.stdout, 'x'.repeat(500));
assert.equal(stdoutFinalPage.result.structuredContent.stdout_offset, 6000);
assert.equal(stdoutFinalPage.result.structuredContent.stdout_limit, 4000);
assert.equal(stdoutFinalPage.result.structuredContent.stdout_next_offset, null);
assert.equal(stdoutFinalPage.result.structuredContent.stdout_output_truncated, false);
assert.equal(stdoutFinalPage.result.structuredContent.stdout_char_length, 6500);

const audit = readFileSync(join(auditLogDir, 'structured-command.jsonl'), 'utf8');
assert.match(audit, /structured_command\.execution_result/);

mkdirSync(join(root, '.ai'), { recursive: true });
writeFileSync(join(root, '.ai', 'mcp-telemetry.json'), JSON.stringify({
  enabled: true,
  level: 'all',
  surfaces: {
    'structured-command': { enabled: true, level: 'all' },
  },
}, null, 2), 'utf8');

const telemetryExec = await rpc({
  jsonrpc: '2.0',
  id: 63,
  method: 'tools/call',
  params: {
    name: 'structured_command_execute',
    arguments: {
      command: 'node',
      args: ['--version'],
      working_directory: root,
    },
  },
}, state);
assert.equal(telemetryExec.result.structuredContent.status, 'ok');
const telemetryPath = join(root, '.ai', 'telemetry', 'structured-command.jsonl');
const telemetryLines = readFileSync(telemetryPath, 'utf8').trim().split('\n').filter(Boolean);
assert.ok(telemetryLines.length >= 1);
const telemetryEvent = JSON.parse(telemetryLines[telemetryLines.length - 1]);
assert.equal(telemetryEvent.surface_id, 'structured-command');
assert.equal(telemetryEvent.tool_name, 'structured_command_execute');
assert.equal(JSON.stringify(telemetryEvent).includes('--version'), false);

console.log('structured command MCP tests passed');
