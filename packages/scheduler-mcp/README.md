# @narada-core/scheduler-mcp

## Verification

```powershell
pnpm --filter @narada-core/scheduler-mcp test
```

Windows Task Scheduler MCP surface for governed task registration, inspection, and execution. It wraps `schtasks.exe` for Task Scheduler authority and stores every target behind Narada's native no-console actuator. The actuator is a GUI-subsystem executable and creates its managed target with Windows `CREATE_NO_WINDOW`; Task Scheduler's `Hidden` metadata is not treated as a window-suppression mechanism. Action replacement carries a task's disabled state into the same settings update, so refreshing an action cannot silently enable a paused task.

## Purpose

Provides a policy-gated surface for managing Windows scheduled tasks. Bridges the gap between `trigger_kind: schedule` metadata in SOP and task-lifecycle and actual runtime scheduling.

## Tools

| Tool | Description |
|------|-------------|
| `scheduler_task_list` | List tasks with optional folder filter |
| `scheduler_task_show` | Show full details of one task |
| `scheduler_task_create` | Create a task with daily/hourly/at_startup/at_logon/once schedules |
| `scheduler_task_delete` | Delete a task |
| `scheduler_task_enable` | Enable a disabled task |
| `scheduler_task_disable` | Disable a task |
| `scheduler_task_run` | Run a task immediately |
| `scheduler_task_history` | Show last run time, result code, status |

## Quick Start

```
pnpm --filter @narada-core/scheduler-mcp test
```
