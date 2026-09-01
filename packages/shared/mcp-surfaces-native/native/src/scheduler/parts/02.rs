fn activation_tool(name: &str, read_only: bool) -> Value {
    let implementation = json!({"type":"string","description":"Current implementation_id returned by scheduler_runtime_status."});
    let string = json!({"type":"string"});
    let record = json!({"type":"object","additionalProperties":true});
    let schema = match name {
        "scheduler_activation_doctor" => {
            json!({"type":"object","properties":{},"additionalProperties":false})
        }
        "scheduler_activation_prepare" => {
            json!({"type":"object","properties":{"implementation_id":implementation},"required":["implementation_id"],"additionalProperties":false})
        }
        "scheduler_binding_list" => {
            json!({"type":"object","properties":{"status":{"type":"string","enum":["active","paused","retired"]},"limit":{"type":"integer","minimum":1,"maximum":500,"default":100},"offset":{"type":"integer","minimum":0,"maximum":10000,"default":0}},"additionalProperties":false})
        }
        "scheduler_binding_show" => {
            json!({"type":"object","properties":{"binding_id":string},"required":["binding_id"],"additionalProperties":false})
        }
        "scheduler_binding_upsert" => {
            json!({"type":"object","properties":{"binding_id":string,"trigger_kind":{"type":"string","enum":["bootstrap","completion","domain_event"]},"source_topic":string,"source_sop_id":string,"terminal_outcomes":{"type":"array","items":{"type":"string"}},"target_sop_id":string,"target_template_version":string,"concurrency":{"type":"string","enum":["singleton","partitioned"]},"delay_by_outcome_ms":{"type":"object","additionalProperties":{"type":"integer","minimum":0}},"default_delay_ms":{"type":"integer","minimum":0},"retry_base_ms":{"type":"integer","minimum":0},"retry_max_ms":{"type":"integer","minimum":0},"max_attempts":{"type":"integer","minimum":1},"blocked_policy":{"type":"string","enum":["manual_unblock"]},"expected_revision":{"type":"integer","minimum":1},"implementation_id":implementation},"required":["binding_id","trigger_kind","source_topic","target_sop_id","target_template_version","concurrency","implementation_id"],"additionalProperties":false})
        }
        "scheduler_binding_pause" | "scheduler_binding_resume" | "scheduler_binding_retire" => {
            json!({"type":"object","properties":{"binding_id":string,"expected_revision":{"type":"integer","minimum":1},"implementation_id":implementation},"required":["binding_id","expected_revision","implementation_id"],"additionalProperties":false})
        }
        "scheduler_event_show" => {
            json!({"type":"object","properties":{"event_id":string},"required":["event_id"],"additionalProperties":false})
        }
        "scheduler_event_admit" => {
            json!({"type":"object","properties":{"event_id":string,"topic":string,"partition_key":string,"aggregate_id":string,"aggregate_revision":{"type":"integer","minimum":0},"schema_version":{"type":"integer","minimum":1},"causation_id":string,"idempotency_key":string,"payload":record,"occurred_at":string,"implementation_id":implementation},"required":["event_id","topic","partition_key","aggregate_id","aggregate_revision","schema_version","causation_id","idempotency_key","payload","occurred_at","implementation_id"],"additionalProperties":false})
        }
        "scheduler_activation_list" => {
            json!({"type":"object","properties":{"status":{"type":"string","enum":["pending","leased","admitted","terminal","blocked"]},"binding_id":string,"source_event_id":string,"sop_run_id":string,"limit":{"type":"integer","minimum":1,"maximum":500,"default":100},"offset":{"type":"integer","minimum":0,"maximum":10000,"default":0}},"additionalProperties":false})
        }
        "scheduler_activation_claim" => {
            json!({"type":"object","properties":{"consumer_id":string,"lease_ms":{"type":"integer","minimum":1000,"maximum":300000},"implementation_id":implementation},"required":["consumer_id","implementation_id"],"additionalProperties":false})
        }
        "scheduler_activation_admit_sop" => {
            json!({"type":"object","properties":{"activation_id":string,"consumer_id":string,"lease_token":string,"sop_run_id":string,"receipt_id":string,"receipt":record,"implementation_id":implementation},"required":["activation_id","consumer_id","lease_token","sop_run_id","receipt_id","receipt","implementation_id"],"additionalProperties":false})
        }
        "scheduler_activation_fail" => {
            json!({"type":"object","properties":{"activation_id":string,"consumer_id":string,"lease_token":string,"retryable":{"type":"boolean"},"error":string,"implementation_id":implementation},"required":["activation_id","consumer_id","lease_token","retryable","error","implementation_id"],"additionalProperties":false})
        }
        "scheduler_activation_resolve" => {
            json!({"type":"object","properties":{"activation_id":string,"sop_run_id":string,"outcome":string,"receipt_id":string,"receipt":record,"implementation_id":implementation},"required":["outcome","receipt_id","receipt","implementation_id"],"additionalProperties":false})
        }
        "scheduler_activation_unblock" => {
            json!({"type":"object","properties":{"activation_id":string,"due_at":string,"implementation_id":implementation},"required":["activation_id","implementation_id"],"additionalProperties":false})
        }
        _ => json!({"type":"object","properties":{},"additionalProperties":false}),
    };
    tool(
        name,
        "Operate the durable Site-scoped Scheduler activation ledger.",
        schema,
        read_only,
        false,
        true,
    )
}

fn tool(
    name: &str,
    description: &str,
    input_schema: Value,
    read_only: bool,
    destructive: bool,
    idempotent: bool,
) -> Value {
    json!({"name":name,"description":description,"inputSchema":input_schema,"annotations":{"title":name,"readOnlyHint":read_only,"destructiveHint":destructive,"idempotentHint":idempotent,"openWorldHint":false},"outputSchema":{"type":"object","additionalProperties":true}})
}

fn task_description(name: &str) -> &'static str {
    match name {
        "scheduler_runtime_status" => {
            "Report the native Scheduler implementation identity required for safe mutations."
        }
        "scheduler_task_list" => "List scheduled tasks, optionally filtered by folder path.",
        "scheduler_task_show" => {
            "Show full details and the native action definition for one scheduled task."
        }
        "scheduler_task_create" => {
            "Create a scheduled task whose target runs through the native no-console actuator."
        }
        "scheduler_task_delete" => "Delete a scheduled task.",
        "scheduler_task_update_action" => {
            "Update only a task action while preserving triggers and enabled state."
        }
        "scheduler_task_enable" => "Enable a scheduled task.",
        "scheduler_task_disable" => "Disable a scheduled task.",
        "scheduler_task_stop" => "Stop a running task without changing registration.",
        "scheduler_task_run" => "Run a scheduled task immediately.",
        "scheduler_task_history" => "Show bounded run summary history for a scheduled task.",
        _ => "Operate Windows Task Scheduler.",
    }
}

#[derive(Debug)]
struct CommandResult {
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
}

fn schtasks(arguments: &[String]) -> Result<CommandResult, Value> {
    let executable = env::var_os("NARADA_SCHEDULER_SCHTASKS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("schtasks.exe"));
    run_command(&executable, arguments, &[], COMMAND_TIMEOUT)
}

fn powershell(script: &str, environment: &[(String, String)]) -> Result<CommandResult, Value> {
    let executable = env::var_os("NARADA_SCHEDULER_POWERSHELL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("powershell.exe"));
    let utf16 = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let encoded = base64(&utf16, false);
    run_command(
        &executable,
        &[
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-EncodedCommand".into(),
            encoded,
        ],
        environment,
        COMMAND_TIMEOUT,
    )
}

fn run_command(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    timeout: Duration,
) -> Result<CommandResult, Value> {
    let marker = format!("narada-scheduler-{}", Uuid::new_v4());
    let temp = env::temp_dir();
    let stdout_path = temp.join(format!("{marker}.stdout"));
    let stderr_path = temp.join(format!("{marker}.stderr"));
    let stdout_file = File::create(&stdout_path).map_err(|cause| {
        error(
            "scheduler_command_capture_failed",
            &cause.to_string(),
            Value::Null,
        )
    })?;
    let stderr_file = File::create(&stderr_path).map_err(|cause| {
        error(
            "scheduler_command_capture_failed",
            &cause.to_string(),
            Value::Null,
        )
    })?;
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .stdin(Stdio::null());
    for (key, value) in environment {
        command.env(key, value);
    }
    hide_window(&mut command);
    let mut child = command.spawn().map_err(|cause| {
        let _ = fs::remove_file(&stdout_path);
        let _ = fs::remove_file(&stderr_path);
        error(
            "scheduler_command_spawn_failed",
            &cause.to_string(),
            json!({"executable":executable.to_string_lossy()}),
        )
    })?;
    let started = Instant::now();
    let mut timed_out = false;
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(1),
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                break child
                    .wait()
                    .ok()
                    .and_then(|status| status.code())
                    .unwrap_or(-2);
            }
            Err(cause) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&stdout_path);
                let _ = fs::remove_file(&stderr_path);
                return Err(error(
                    "scheduler_command_wait_failed",
                    &cause.to_string(),
                    Value::Null,
                ));
            }
        }
    };
    let stdout = read_bounded(&stdout_path);
    let mut stderr = read_bounded(&stderr_path);
    let _ = fs::remove_file(stdout_path);
    let _ = fs::remove_file(stderr_path);
    if timed_out {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "scheduler command timed out after {}ms",
            timeout.as_millis()
        ));
    }
    Ok(CommandResult {
        stdout,
        stderr,
        exit_code,
        timed_out,
    })
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
}
#[cfg(not(windows))]
fn hide_window(_command: &mut Command) {}
fn read_bounded(path: &Path) -> String {
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    let mut bytes = Vec::new();
    let _ = file.take(MAX_COMMAND_OUTPUT).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).to_string()
}

