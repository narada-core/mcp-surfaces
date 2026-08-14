use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const MAX_CONTROL_CHUNK: u64 = 512 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub fn is_host_mode(args: &[String]) -> bool {
    args.first().map(String::as_str) == Some("--resident-runtime-host")
}

pub fn run(args: &[String]) -> Result<(), String> {
    let runtime = required(args, "--runtime")?;
    let site_root = PathBuf::from(required(args, "--site-root")?);
    let identity = required(args, "--identity")?;
    let session_id = required(args, "--session")?;
    let authority = optional(args, "--authority");
    let mcp_scope = optional(args, "--mcp-scope");
    let orientation_entry_file = optional(args, "--orientation-entry-file");
    let intelligence_provider = optional(args, "--intelligence-provider");
    let enable_native_shell = args.iter().any(|value| value == "--enable-native-shell");
    validate_id(&session_id)?;
    let session_dir = site_root
        .join(".narada")
        .join("crew")
        .join("nars-sessions")
        .join(&session_id);
    fs::create_dir_all(&session_dir)
        .map_err(|error| format!("resident_host_session_create_failed:{error}"))?;
    let control_path = session_dir.join("control.jsonl");
    let events_path = session_dir.join("events.jsonl");
    let diagnostics_path = session_dir.join("diagnostic.log");
    let host_path = session_dir.join("host.json");
    if !control_path.exists() {
        File::create(&control_path)
            .map_err(|error| format!("resident_host_control_create_failed:{error}"))?;
    }

    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&events_path)
        .map_err(|error| format!("resident_host_events_open_failed:{error}"))?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&diagnostics_path)
        .map_err(|error| format!("resident_host_diagnostic_open_failed:{error}"))?;
    let mut command = Command::new(&runtime);
    command
        .args([
            "--raw-jsonl",
            "--session",
            &session_id,
            "--identity",
            &identity,
            "--site-root",
            &site_root.to_string_lossy(),
        ])
        .current_dir(&site_root)
        .env("NARADA_SITE_ROOT", &site_root)
        .env("NARADA_CARRIER_SESSION_ID", &session_id)
        .env("NARADA_RUNTIME_SESSION_ID", &session_id)
        .env("NARADA_AGENT_ID", &identity)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if let Some(authority) = authority.as_deref() {
        command.env("NARADA_AUTHORITY", authority);
    }
    if let Some(mcp_scope) = mcp_scope.as_deref() {
        command.env("NARADA_MCP_SCOPE", mcp_scope);
    }
    if let Some(entry_file) = orientation_entry_file.as_deref() {
        command
            .arg("--orientation-entry-file")
            .arg(entry_file)
            .env("NARADA_ORIENTATION_REQUIRED", "1")
            .env("NARADA_ORIENTATION_ENTRY_FILE", entry_file);
    }
    if let Some(provider) = intelligence_provider.as_deref() {
        command.env("NARADA_INTELLIGENCE_PROVIDER", provider);
    }
    if enable_native_shell {
        command.env("NARADA_ENABLE_NATIVE_SHELL", "1");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("resident_host_runtime_spawn_failed:{error}"))?;
    let mut input = child
        .stdin
        .take()
        .ok_or("resident_host_runtime_stdin_missing")?;
    write_atomic(
        &host_path,
        &json!({"schema":"narada.site_loop.native_resident_host.v1","status":"running","host_pid":std::process::id(),"runtime_pid":child.id(),"runtime":runtime,"site_root":site_root,"identity":identity,"carrier_session_id":session_id,"control_path":control_path,"events_path":events_path,"started_at":now()}),
    )?;

    let mut offset = 0_u64;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("resident_host_runtime_wait_failed:{error}"))?
        {
            write_atomic(
                &host_path,
                &json!({"schema":"narada.site_loop.native_resident_host.v1","status":"stopped","host_pid":std::process::id(),"runtime_pid":child.id(),"carrier_session_id":session_id,"exit_code":status.code(),"finished_at":now()}),
            )?;
            return if status.success() {
                Ok(())
            } else {
                Err(format!("resident_host_runtime_failed:{:?}", status.code()))
            };
        }
        if session_dir.join("retired.json").is_file() {
            let _ = writeln!(
                input,
                "{}",
                json!({"id":format!("resident-retire-{session_id}"),"method":"session.close","params":{"reason":"resident_carrier_retired"}})
            );
            let _ = input.flush();
            thread::sleep(Duration::from_millis(250));
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            continue;
        }
        offset = forward_control(&control_path, offset, &mut input)?;
        thread::sleep(POLL_INTERVAL);
    }
}

fn forward_control(path: &Path, offset: u64, target: &mut impl Write) -> Result<u64, String> {
    let length = fs::metadata(path)
        .map_err(|error| format!("resident_host_control_stat_failed:{error}"))?
        .len();
    let start = offset.min(length);
    if length == start {
        return Ok(start);
    }
    let mut file =
        File::open(path).map_err(|error| format!("resident_host_control_open_failed:{error}"))?;
    file.seek(SeekFrom::Start(start))
        .map_err(|error| format!("resident_host_control_seek_failed:{error}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_CONTROL_CHUNK)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("resident_host_control_read_failed:{error}"))?;
    let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return Ok(start);
    };
    let complete = &bytes[..=last_newline];
    for line in complete
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let value: Value = serde_json::from_slice(line)
            .map_err(|error| format!("resident_host_control_invalid_json:{error}"))?;
        serde_json::to_writer(&mut *target, &value)
            .map_err(|error| format!("resident_host_control_encode_failed:{error}"))?;
        target
            .write_all(b"\n")
            .map_err(|error| format!("resident_host_control_forward_failed:{error}"))?;
    }
    target
        .flush()
        .map_err(|error| format!("resident_host_control_flush_failed:{error}"))?;
    Ok(start + complete.len() as u64)
}

fn required(args: &[String], key: &str) -> Result<String, String> {
    let position = args
        .iter()
        .position(|value| value == key)
        .ok_or_else(|| format!("resident_host_argument_required:{key}"))?;
    args.get(position + 1)
        .cloned()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("resident_host_argument_value_required:{key}"))
}

fn optional(args: &[String], key: &str) -> Option<String> {
    let position = args.iter().position(|value| value == key)?;
    args.get(position + 1)
        .cloned()
        .filter(|value| !value.trim().is_empty())
}

fn validate_id(value: &str) -> Result<(), String> {
    if !value.is_empty()
        && value.len() <= 256
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        Ok(())
    } else {
        Err("resident_host_session_id_invalid".to_string())
    }
}

fn write_atomic(path: &Path, value: &Value) -> Result<(), String> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value)
            .map_err(|error| format!("resident_host_record_encode_failed:{error}"))?,
    )
    .map_err(|error| format!("resident_host_record_write_failed:{error}"))?;
    if path.exists() {
        fs::remove_file(path)
            .map_err(|error| format!("resident_host_record_replace_failed:{error}"))?;
    }
    fs::rename(&temporary, path)
        .map_err(|error| format!("resident_host_record_promote_failed:{error}"))
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_only_complete_bounded_control_frames() {
        let root =
            std::env::temp_dir().join(format!("narada-resident-host-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        let path = root.join("control.jsonl");
        fs::write(&path, b"{\"id\":1}\n{\"id\":2}").expect("control");
        let mut output = Vec::new();
        let offset = forward_control(&path, 0, &mut output).expect("forward");
        assert_eq!(offset, 9);
        assert_eq!(String::from_utf8(output).expect("utf8"), "{\"id\":1}\n");
        fs::write(&path, b"{\"id\":1}\n{\"id\":2}\n").expect("complete");
        let offset = forward_control(&path, offset, &mut Vec::new()).expect("second");
        assert_eq!(offset, 18);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
