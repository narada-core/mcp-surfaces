fn supervisor_path() -> PathBuf {
    if let Some(value) = env::var_os("NARADA_PROCESS_SUPERVISOR_PATH") {
        return absolute(PathBuf::from(value));
    }
    if let Ok(executable) = env::current_exe() {
        if let Some(parent) = executable.parent() {
            let adjacent = parent.join("narada-process-supervisor.exe");
            if adjacent.is_file() {
                return adjacent;
            }
        }
        for ancestor in executable.ancestors().take(12) {
            if ancestor
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.eq_ignore_ascii_case("mcp-surfaces"))
                == Some(true)
            {
                if let Some(src) = ancestor.parent() {
                    return src.join("narada/packages/process-launch-posture/native/target/release/narada-process-supervisor.exe");
                }
            }
        }
    }
    PathBuf::from("narada-process-supervisor.exe")
}

fn task_create(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let task_name = required(args, "task_name", "scheduler_requires_task_name")?;
    let command = required(args, "command", "scheduler_requires_command")?;
    let arguments = optional(args, "arguments").unwrap_or_default();
    let working_dir = optional(args, "working_dir")
        .map(PathBuf::from)
        .map(absolute);
    let schedule = required(args, "schedule", "scheduler_requires_schedule")?;
    let execution_limit = optional_integer_range(args, "execution_time_limit_seconds", 1, 86_400)?;
    let instances = multiple_instances(args.get("multiple_instances"))?;
    assert_action_allowed(&command, &arguments, working_dir.as_deref(), root)?;
    let dry_run = args.get("dry_run").and_then(Value::as_bool) == Some(true);
    let plan = launch_plan(&command, &arguments, !dry_run)?;
    let placeholder = format!(
        "\"{}\" --scheduled-noop-v1",
        plan.launcher_path.to_string_lossy()
    );
    let mut sch_args = strings(&["/create", "/tn", &task_name, "/tr", &placeholder, "/f"]);
    sch_args.extend(schedule_args(&schedule, args)?);
    if dry_run {
        return Ok(
            json!({"schema":"narada.scheduler.task_create_plan.v1","status":"planned","task_name":task_name,"schedule":schedule,"command":join_command(&plan.target_command,&plan.target_arguments),"execute":plan.target_command,"arguments":plan.target_arguments,"launcher_execute":plan.launcher_path.to_string_lossy(),"launcher_arguments":plan.launcher_arguments,"working_dir":working_dir.as_ref().map(|path|path.to_string_lossy().to_string()),"execution_time_limit_seconds":execution_limit,"multiple_instances":instances,"schtasks_create_args":sch_args,"host_effect":false,"bounded":true}),
        );
    }
    let created = schtasks(&sch_args)?;
    if created.exit_code != 0 {
        return Err(command_failure(
            "scheduler_create_failed",
            "create",
            &created,
            &task_name,
            &placeholder,
        ));
    }
    let changed = set_task_action(
        &task_name,
        &plan,
        working_dir.as_deref(),
        execution_limit,
        instances.as_deref(),
    )?;
    if changed.exit_code != 0 {
        return Err(command_failure(
            "scheduler_create_action_failed",
            "create_action",
            &changed,
            &task_name,
            &command,
        ));
    }
    Ok(
        json!({"status":"created","task_name":task_name,"schedule":schedule,"command":join_command(&plan.target_command,&plan.target_arguments),"execute":plan.target_command,"arguments":plan.target_arguments,"launcher_execute":plan.launcher_path.to_string_lossy(),"launcher_arguments":plan.launcher_arguments,"working_dir":working_dir.as_ref().map(|path|path.to_string_lossy().to_string()),"working_dir_applied":working_dir.is_some(),"task_hidden":true,"execution_time_limit_seconds":execution_limit,"multiple_instances":instances,"console_window_policy":"native_create_no_window","mutation_method":"schtasks_create_then_powershell_set_scheduled_task_native_no_window_action_and_hidden_settings"}),
    )
}

fn task_update_action(args: &Map<String, Value>, root: &Path) -> Result<Value, Value> {
    let task_name = required(args, "task_name", "scheduler_requires_task_name")?;
    let command = required(args, "command", "scheduler_requires_command")?;
    let arguments = optional(args, "arguments").unwrap_or_default();
    let working_dir = optional(args, "working_dir")
        .map(PathBuf::from)
        .map(absolute);
    let execution_limit = optional_integer_range(args, "execution_time_limit_seconds", 1, 86_400)?;
    let instances = multiple_instances(args.get("multiple_instances"))?;
    assert_action_allowed(&command, &arguments, working_dir.as_deref(), root)?;
    let dry_run = args.get("dry_run").and_then(Value::as_bool) == Some(true);
    let plan = launch_plan(&command, &arguments, !dry_run)?;
    let launcher_command = format!(
        "\"{}\" {}",
        plan.launcher_path.to_string_lossy(),
        plan.launcher_arguments
    );
    let preview = strings(&["/change", "/tn", &task_name, "/tr", &launcher_command]);
    if dry_run {
        return Ok(
            json!({"status":"planned","task_name":task_name,"command":join_command(&plan.target_command,&plan.target_arguments),"execute":command,"arguments":arguments,"mutation_method":"powershell_set_scheduled_task_native_no_window_action","console_window_policy":"native_create_no_window","launcher_execute":plan.launcher_path.to_string_lossy(),"launcher_arguments":plan.launcher_arguments,"schtasks_preview_args":preview,"schtasks_preview_not_used_for_mutation":true,"preserves_triggers":true,"preserves_enabled_state":true,"enabled_state_preservation":"scheduled_task_settings_disable_flag","working_dir":working_dir.as_ref().map(|path|path.to_string_lossy().to_string()),"working_dir_applied":false,"working_dir_would_apply":working_dir.is_some(),"execution_time_limit_seconds":execution_limit,"multiple_instances":instances}),
        );
    }
    let changed = set_task_action(
        &task_name,
        &plan,
        working_dir.as_deref(),
        execution_limit,
        instances.as_deref(),
    )?;
    if changed.exit_code != 0 {
        return Err(command_failure(
            "scheduler_update_action_failed",
            "update_action",
            &changed,
            &task_name,
            &command,
        ));
    }
    Ok(
        json!({"status":"updated","task_name":task_name,"command":join_command(&plan.target_command,&plan.target_arguments),"preserves_triggers":true,"preserves_enabled_state":true,"enabled_state_preservation":"scheduled_task_settings_disable_flag","mutation_method":"powershell_set_scheduled_task_native_no_window_action","execute":plan.target_command,"arguments":plan.target_arguments,"launcher_execute":plan.launcher_path.to_string_lossy(),"launcher_arguments":plan.launcher_arguments,"console_window_policy":"native_create_no_window","working_dir":working_dir.as_ref().map(|path|path.to_string_lossy().to_string()),"working_dir_applied":working_dir.is_some(),"execution_time_limit_seconds":execution_limit,"multiple_instances":instances}),
    )
}

fn set_task_action(
    task_name: &str,
    plan: &LaunchPlan,
    working_dir: Option<&Path>,
    execution_limit: Option<i64>,
    instances: Option<&str>,
) -> Result<CommandResult, Value> {
    let (name, path) = split_task_path(task_name)?;
    let script = r#"$ErrorActionPreference = "Stop";$execute = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_EXECUTE");$arguments = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_ARGUMENTS");$workingDirectory = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_WORKING_DIR");$executionLimitSeconds = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_EXECUTION_LIMIT_SECONDS");$multipleInstances = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_MULTIPLE_INSTANCES");$taskName = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_NAME");$taskPath = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_PATH");$existingTask = Get-ScheduledTask -TaskName $taskName -TaskPath $taskPath;$wasDisabled = [string]$existingTask.State -eq "Disabled";if ([string]::IsNullOrWhiteSpace($workingDirectory)) { if ([string]::IsNullOrWhiteSpace($arguments)) { $action = New-ScheduledTaskAction -Execute $execute } else { $action = New-ScheduledTaskAction -Execute $execute -Argument $arguments } } else { if ([string]::IsNullOrWhiteSpace($arguments)) { $action = New-ScheduledTaskAction -Execute $execute -WorkingDirectory $workingDirectory } else { $action = New-ScheduledTaskAction -Execute $execute -Argument $arguments -WorkingDirectory $workingDirectory } };$settingsArguments = @{ Hidden = $true };if ($wasDisabled) { $settingsArguments.Disable = $true };if (-not [string]::IsNullOrWhiteSpace($executionLimitSeconds)) { $settingsArguments.ExecutionTimeLimit = [TimeSpan]::FromSeconds([int]$executionLimitSeconds) };if (-not [string]::IsNullOrWhiteSpace($multipleInstances)) { $settingsArguments.MultipleInstances = $multipleInstances };$settings = New-ScheduledTaskSettingsSet @settingsArguments;Set-ScheduledTask -TaskName $taskName -TaskPath $taskPath -Action $action -Settings $settings | Out-Null"#;
    powershell(
        script,
        &[
            ("NARADA_SCHEDULER_TASK_NAME".into(), name),
            ("NARADA_SCHEDULER_TASK_PATH".into(), path),
            (
                "NARADA_SCHEDULER_EXECUTE".into(),
                plan.launcher_path.to_string_lossy().to_string(),
            ),
            (
                "NARADA_SCHEDULER_ARGUMENTS".into(),
                plan.launcher_arguments.clone(),
            ),
            (
                "NARADA_SCHEDULER_WORKING_DIR".into(),
                working_dir
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_default(),
            ),
            (
                "NARADA_SCHEDULER_EXECUTION_LIMIT_SECONDS".into(),
                execution_limit
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "NARADA_SCHEDULER_MULTIPLE_INSTANCES".into(),
                instances.unwrap_or("").to_string(),
            ),
        ],
    )
}

fn task_simple_mutation(
    operation: &str,
    args: &Map<String, Value>,
    verb: &str,
    status: &str,
) -> Result<Value, Value> {
    let task_name = required(args, "task_name", "scheduler_requires_task_name")?;
    let mut values = strings(&[verb, "/tn", &task_name]);
    match operation {
        "delete" => values.push("/f".into()),
        "enable" => values.push("/enable".into()),
        "disable" => values.push("/disable".into()),
        _ => {}
    }
    let result = schtasks(&values)?;
    if result.exit_code != 0 {
        return Err(command_failure(
            &format!("scheduler_{operation}_failed"),
            operation,
            &result,
            &task_name,
            "",
        ));
    }
    Ok(json!({"status":status,"task_name":task_name}))
}

fn schedule_args(schedule: &str, args: &Map<String, Value>) -> Result<Vec<String>, Value> {
    match schedule {
        "daily" => Ok(strings(&[
            "/sc",
            "daily",
            "/st",
            optional(args, "start_time").as_deref().unwrap_or("09:00"),
        ])),
        "hourly" => {
            let interval = args
                .get("interval_minutes")
                .and_then(Value::as_i64)
                .unwrap_or(60)
                .clamp(1, 1440);
            if interval < 60 || interval % 60 != 0 {
                Ok(strings(&["/sc", "minute", "/mo", &interval.to_string()]))
            } else {
                Ok(strings(&[
                    "/sc",
                    "hourly",
                    "/mo",
                    &(interval / 60).max(1).to_string(),
                ]))
            }
        }
        "at_startup" => Ok(strings(&["/sc", "onstart"])),
        "at_logon" => Ok(strings(&["/sc", "onlogon"])),
        "once" => Ok(strings(&[
            "/sc",
            "once",
            "/st",
            optional(args, "start_time").as_deref().unwrap_or("09:00"),
        ])),
        _ => Err(error(
            "scheduler_invalid_schedule",
            &format!("scheduler_invalid_schedule:{schedule}"),
            Value::Null,
        )),
    }
}

fn multiple_instances(value: Option<&Value>) -> Result<Option<String>, Value> {
    match value.and_then(Value::as_str) {
        None => Ok(None),
        Some("ignore_new") => Ok(Some("IgnoreNew".into())),
        Some("parallel") => Ok(Some("Parallel".into())),
        Some("queue") => Ok(Some("Queue".into())),
        Some("stop_existing") => Ok(Some("StopExisting".into())),
        Some(other) => Err(error(
            "scheduler_multiple_instances_invalid",
            &format!("scheduler_multiple_instances_invalid:{other}"),
            Value::Null,
        )),
    }
}

fn assert_action_allowed(
    command: &str,
    arguments: &str,
    working_dir: Option<&Path>,
    root: &Path,
) -> Result<(), Value> {
    let reasons = action_policy_reasons(command, arguments, working_dir, root);
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(error(
            "scheduler_action_refused",
            "scheduler_action_refused",
            json!({"refusal_reasons":reasons,"remediation":"Schedule the owning executable directly from an allowed root. Do not use cmd, transient wrappers, or scripts staged under .ai/tmp or .ai/temp."}),
        ))
    }
}

