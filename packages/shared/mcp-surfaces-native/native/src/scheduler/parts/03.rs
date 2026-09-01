fn task_list(args: &Map<String, Value>) -> Result<Value, Value> {
    let folder = args
        .get("folder")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("\\");
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 500) as usize;
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10_000) as usize;
    let result = schtasks(&strings(&["/query", "/fo", "CSV", "/v", "/tn", folder]))?;
    if result.exit_code != 0 && result.exit_code != 1 {
        return Err(command_failure(
            "scheduler_query_failed",
            "list",
            &result,
            "",
            "",
        ));
    }
    let all_items = compact_rows(&parse_csv(&result.stdout));
    let observed_total = all_items.len();
    let count_exact = result.stdout.len() < MAX_COMMAND_OUTPUT as usize;
    let items = all_items
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let returned = items.len();
    let has_more = offset.saturating_add(returned) < observed_total || !count_exact;
    Ok(
        json!({"schema":"narada.scheduler.task_list.v1","status":"ok","items":items,"count":returned,"returned":returned,"total":if count_exact{json!(observed_total)}else{Value::Null},"observed_total":observed_total,"count_exact":count_exact,"folder":folder,"offset":offset,"limit":limit,"has_more":has_more,"next_offset":if has_more && returned>0{json!(offset + returned)}else{Value::Null},"bounded":true}),
    )
}

fn task_show(args: &Map<String, Value>) -> Result<Value, Value> {
    let task_name = required(args, "task_name", "scheduler_requires_task_name")?;
    let result = schtasks(&strings(&["/query", "/fo", "CSV", "/v", "/tn", &task_name]))?;
    if result.exit_code != 0 {
        return Err(command_failure(
            "scheduler_task_not_found",
            "show",
            &result,
            &task_name,
            "",
        ));
    }
    let rows = parse_csv(&result.stdout);
    if rows.is_empty() {
        return Err(error(
            "scheduler_task_not_found",
            &format!("scheduler_task_not_found:{task_name}"),
            Value::Null,
        ));
    }
    let definition = scheduled_task_definition(&task_name)?;
    Ok(
        json!({"task":rows[0],"task_compact":compact_rows(&rows).into_iter().next(),"task_definition":definition}),
    )
}

fn task_history(args: &Map<String, Value>) -> Result<Value, Value> {
    let task_name = required(args, "task_name", "scheduler_requires_task_name")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 200) as usize;
    let result = schtasks(&strings(&["/query", "/fo", "CSV", "/v", "/tn", &task_name]))?;
    if result.exit_code != 0 && result.exit_code != 1 {
        return Err(command_failure(
            "scheduler_query_failed",
            "history",
            &result,
            &task_name,
            "",
        ));
    }
    let rows = parse_csv(&result.stdout);
    if rows.is_empty() {
        return Err(error(
            "scheduler_task_not_found",
            &format!("scheduler_task_not_found:{task_name}"),
            Value::Null,
        ));
    }
    let items=compact_rows(&rows).into_iter().take(limit).map(|task|json!({"task_name":task["task_name"],"last_run":task["last_run"],"status":task["status"],"last_result":task["last_result"],"next_run":task["next_run"],"schedule":task["schedule"],"trigger_count":task["trigger_count"],"triggers":task["triggers"]})).collect::<Vec<_>>();
    Ok(json!({"task_name":task_name,"count":items.len(),"items":items}))
}

fn scheduled_task_definition(task_name: &str) -> Result<Value, Value> {
    let (name, path) = split_task_path(task_name)?;
    let script = r#"$ErrorActionPreference = "Stop";$taskName = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_NAME");$taskPath = [Environment]::GetEnvironmentVariable("NARADA_SCHEDULER_TASK_PATH");$task = Get-ScheduledTask -TaskName $taskName -TaskPath $taskPath;$action = @($task.Actions)[0];$limitSeconds = [System.Xml.XmlConvert]::ToTimeSpan([string]$task.Settings.ExecutionTimeLimit).TotalSeconds;[ordered]@{ task_name = "$($task.TaskPath)$($task.TaskName)"; state = [string]$task.State; execute = [string]$action.Execute; arguments = [string]$action.Arguments; working_dir = [string]$action.WorkingDirectory; hidden = [bool]$task.Settings.Hidden; execution_time_limit_seconds = [int]$limitSeconds; multiple_instances = [string]$task.Settings.MultipleInstances } | ConvertTo-Json -Compress"#;
    let result = powershell(
        script,
        &[
            ("NARADA_SCHEDULER_TASK_NAME".into(), name),
            ("NARADA_SCHEDULER_TASK_PATH".into(), path),
        ],
    )?;
    if result.exit_code != 0 {
        return Err(command_failure(
            "scheduler_task_definition_query_failed",
            "show",
            &result,
            task_name,
            "",
        ));
    }
    let parsed = serde_json::from_str::<Value>(result.stdout.trim()).map_err(|cause| {
        error(
            "scheduler_task_definition_invalid",
            &format!("scheduler_task_definition_invalid:{cause}"),
            Value::Null,
        )
    })?;
    Ok(normalize_task_definition(parsed))
}

fn normalize_task_definition(mut definition: Value) -> Value {
    let execute = definition
        .get("execute")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let arguments = definition
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let supervisor = supervisor_path();
    let object = definition
        .as_object_mut()
        .unwrap_or_else(|| panic!("task definition object"));
    if !same_windows_path(&execute, &supervisor.to_string_lossy()) {
        object.insert(
            "console_window_policy".into(),
            json!("unmanaged_direct_process"),
        );
        object.insert("launcher_execute".into(), Value::Null);
        object.insert("launcher_arguments".into(), Value::Null);
        return definition;
    }
    match decode_launch_arguments(&arguments) {
        Ok((command, args)) => {
            object.insert("execute".into(), json!(command));
            object.insert("arguments".into(), json!(args));
            object.insert(
                "console_window_policy".into(),
                json!("native_create_no_window"),
            );
            object.insert("launcher_execute".into(), json!(execute));
            object.insert("launcher_arguments".into(), json!(arguments));
        }
        Err(problem) => {
            object.insert(
                "console_window_policy".into(),
                json!("native_launcher_contract_invalid"),
            );
            object.insert("launcher_execute".into(), json!(execute));
            object.insert("launcher_arguments".into(), json!(arguments));
            object.insert(
                "launcher_contract_error".into(),
                problem.get("message").cloned().unwrap_or(problem),
            );
        }
    }
    definition
}

#[derive(Debug)]
struct LaunchPlan {
    launcher_path: PathBuf,
    launcher_arguments: String,
    target_command: String,
    target_arguments: String,
}
fn launch_plan(
    command: &str,
    arguments: &str,
    require_available: bool,
) -> Result<LaunchPlan, Value> {
    let target = command.trim();
    let target = if target.len() >= 2 && target.starts_with('"') && target.ends_with('"') {
        &target[1..target.len() - 1]
    } else {
        target
    };
    if target.is_empty() {
        return Err(error(
            "scheduler_requires_command",
            "scheduler_requires_command",
            Value::Null,
        ));
    }
    if target.contains('\0') || arguments.contains('\0') {
        return Err(error(
            "scheduler_no_window_launch_invalid",
            "scheduled_command_nul_refused",
            Value::Null,
        ));
    }
    let launcher = supervisor_path();
    if require_available && !launcher.is_file() {
        return Err(error(
            "scheduler_no_window_launcher_unavailable",
            "scheduler_no_window_launcher_unavailable",
            json!({"launcher_path":launcher.to_string_lossy(),"remediation":"Build @narada-core/process-launch-posture before mutating scheduled tasks."}),
        ));
    }
    let mut payload = target.as_bytes().to_vec();
    payload.push(0);
    payload.extend_from_slice(arguments.as_bytes());
    Ok(LaunchPlan {
        launcher_path: launcher,
        launcher_arguments: format!("--scheduled-v1 {}", base64(&payload, true)),
        target_command: target.to_string(),
        target_arguments: arguments.to_string(),
    })
}

fn decode_launch_arguments(arguments: &str) -> Result<(String, String), Value> {
    let Some(payload) = arguments.trim().strip_prefix("--scheduled-v1 ") else {
        return Err(error(
            "scheduled_command_arguments_invalid",
            "scheduled_command_arguments_invalid",
            Value::Null,
        ));
    };
    if payload.is_empty() || payload.contains(char::is_whitespace) {
        return Err(error(
            "scheduled_command_arguments_invalid",
            "scheduled_command_arguments_invalid",
            Value::Null,
        ));
    }
    let bytes = base64_url_decode(payload)?;
    let separators = bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == 0)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if separators.len() != 1 || separators[0] == 0 {
        return Err(error(
            "scheduled_command_payload_invalid",
            "scheduled_command_payload_invalid",
            Value::Null,
        ));
    }
    let index = separators[0];
    let command = String::from_utf8(bytes[..index].to_vec()).map_err(|_| {
        error(
            "scheduled_command_payload_invalid",
            "scheduled_command_payload_invalid",
            Value::Null,
        )
    })?;
    let arguments = String::from_utf8(bytes[index + 1..].to_vec()).map_err(|_| {
        error(
            "scheduled_command_payload_invalid",
            "scheduled_command_payload_invalid",
            Value::Null,
        )
    })?;
    Ok((command, arguments))
}

