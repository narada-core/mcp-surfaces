fn optional_integer_range(
    args: &Map<String, Value>,
    field: &str,
    min: i64,
    max: i64,
) -> Result<Option<i64>, Value> {
    match args.get(field) {
        None => Ok(None),
        Some(value) => value
            .as_i64()
            .filter(|number| (*number >= min) && (*number <= max))
            .map(Some)
            .ok_or_else(|| {
                error(
                    &format!("{field}_invalid"),
                    &format!("{field}_invalid"),
                    json!({"minimum":min,"maximum":max}),
                )
            }),
    }
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
fn join_command(command: &str, arguments: &str) -> String {
    if arguments.is_empty() {
        command.to_string()
    } else {
        format!("{command} {arguments}")
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn error(code: &str, message: &str, details: Value) -> Value {
    json!({"schema":"narada.scheduler_mcp.error.v1","code":code,"message":message,"details":details})
}

fn base64(bytes: &[u8], url_safe: bool) -> String {
    let alphabet = if url_safe {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    };
    let mut output = String::new();
    let mut index = 0;
    while index < bytes.len() {
        let a = bytes[index] as u32;
        let b = bytes.get(index + 1).copied().unwrap_or(0) as u32;
        let c = bytes.get(index + 2).copied().unwrap_or(0) as u32;
        let value = (a << 16) | (b << 8) | c;
        for shift in [18, 12, 6, 0] {
            output.push(alphabet[((value >> shift) & 63) as usize] as char);
        }
        match bytes.len() - index {
            1 => {
                output.pop();
                output.pop();
                if !url_safe {
                    output.push('=');
                    output.push('=');
                }
            }
            2 => {
                output.pop();
                if !url_safe {
                    output.push('=');
                }
            }
            _ => {}
        }
        index += 3;
    }
    output
}
fn base64_url_decode(value: &str) -> Result<Vec<u8>, Value> {
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    let mut output = Vec::new();
    for byte in value.bytes() {
        let sextet = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => {
                return Err(error(
                    "scheduled_command_payload_invalid",
                    "scheduled_command_payload_invalid",
                    Value::Null,
                ))
            }
        } as u32;
        buffer = (buffer << 6) | sextet;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
            buffer = if bits == 0 {
                0
            } else {
                buffer & ((1_u32 << bits) - 1)
            };
        }
    }
    if bits > 0 && buffer != 0 {
        return Err(error(
            "scheduled_command_payload_invalid",
            "scheduled_command_payload_invalid",
            Value::Null,
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_contract_is_complete() {
        let tools = list_tools();
        let names = tools
            .iter()
            .filter_map(|value| {
                value
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .collect::<Vec<_>>();
        for name in [
            "scheduler_task_create",
            "scheduler_binding_pause",
            "scheduler_activation_resolve",
        ] {
            assert!(names.contains(&name.to_string()), "{name}");
        }
        let find = |name: &str| tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert_eq!(
            find("scheduler_task_list")["inputSchema"]["properties"]["offset"]["default"],
            0
        );
        assert_eq!(
            find("scheduler_task_create")["inputSchema"]["properties"]["dry_run"]["default"],
            false
        );
    }
    #[test]
    fn schedule_translation_matches_existing_contract() {
        assert_eq!(
            schedule_args(
                "hourly",
                json!({"interval_minutes":15}).as_object().unwrap()
            )
            .unwrap(),
            strings(&["/sc", "minute", "/mo", "15"])
        );
        assert_eq!(
            schedule_args(
                "hourly",
                json!({"interval_minutes":120}).as_object().unwrap()
            )
            .unwrap(),
            strings(&["/sc", "hourly", "/mo", "2"])
        );
        assert_eq!(
            schedule_args(
                "hourly",
                json!({"interval_minutes":90}).as_object().unwrap()
            )
            .unwrap(),
            strings(&["/sc", "minute", "/mo", "90"])
        );
    }
    #[test]
    fn launch_payload_round_trips() {
        let plan = launch_plan("node.exe", "\"C:\\site\\job.js\" run", false).unwrap();
        assert_eq!(
            decode_launch_arguments(&plan.launcher_arguments).unwrap(),
            ("node.exe".into(), "\"C:\\site\\job.js\" run".into())
        );
    }
    #[test]
    fn csv_rows_compact_multiple_triggers() {
        let csv="\"TaskName\",\"Status\",\"Schedule Type\",\"Next Run Time\",\"Last Run Time\",\"Last Result\",\"Task To Run\"\r\n\"\\\\Narada\",\"Ready\",\"At logon time\",\"N/A\",\"today\",\"0\",\"pwsh.exe\"\r\n\"\\\\Narada\",\"Ready\",\"Minute\",\"soon\",\"today\",\"0\",\"pwsh.exe\"";
        let rows = compact_rows(&parse_csv(csv));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["trigger_count"], 2);
    }
    #[test]
    fn policy_rejects_shell_transience_and_escape() {
        let root = PathBuf::from("C:\\workspace\\site");
        assert!(action_policy_reasons("cmd.exe", "/c tool.cmd", None, &root)
            .iter()
            .any(|reason| reason.starts_with("scheduler_shell_action_disallowed")));
        assert!(action_policy_reasons(
            "pwsh.exe",
            "-File C:\\workspace\\site\\.ai\\tmp\\tool.ps1",
            None,
            &root
        )
        .iter()
        .any(|reason| reason.starts_with("scheduler_transient_script_path_refused")));
        assert!(
            action_policy_reasons("pwsh.exe", "", Some(Path::new("D:\\other")), &root)
                .iter()
                .any(|reason| reason.starts_with("scheduler_working_dir_outside_allowed_root"))
        );
    }
    #[test]
    fn dry_run_has_no_host_effect() {
        let args=json!({"task_name":"\\Narada\\Fixture","command":"pwsh.exe","arguments":"-NoProfile","implementation_id":implementation_id(),"dry_run":true}).as_object().unwrap().clone();
        let result = task_update_action(&args, Path::new("C:\\workspace"));
        assert_eq!(result.unwrap()["status"], "planned");
        let create=json!({"task_name":"\\Narada\\Fixture","command":"C:\\workspace\\job.exe","schedule":"at_logon","implementation_id":implementation_id(),"dry_run":true}).as_object().unwrap().clone();
        let planned = task_create(&create, Path::new("C:\\workspace")).expect("create plan");
        assert_eq!(planned["status"], "planned");
        assert_eq!(planned["host_effect"], false);
        assert_eq!(planned["schema"], "narada.scheduler.task_create_plan.v1");
    }
}
