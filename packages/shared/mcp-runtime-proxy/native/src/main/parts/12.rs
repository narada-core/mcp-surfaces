
fn registrar_carrier_compatibility_notification(surface_id: Option<&str>, request: &Value) -> bool {
    surface_id == Some("mcp-registrar")
        && !protocol::is_modern_request(request)
        && request.get("method").and_then(Value::as_str) == Some("notifications/initialized")
}

fn executable_on_path(command: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")
        .or_else(|| env::var_os("Path"))
        .or_else(|| env::var_os("path"))?;
    let path_str = path_var.to_string_lossy();
    let separator = if cfg!(windows) { ';' } else { ':' };
    let names: Vec<String> = if cfg!(windows) {
        let base = command.strip_suffix(".exe").unwrap_or(command);
        vec![command.to_string(), base.to_string(), format!("{base}.exe")]
    } else {
        vec![command.to_string()]
    };
    let extensions: Vec<&str> = if cfg!(windows) {
        vec![".exe", ".cmd", ".bat", ""]
    } else {
        vec![""]
    };

    for dir in path_str.split(separator) {
        if dir.is_empty() {
            continue;
        }
        let dir_path = PathBuf::from(dir);
        for name in &names {
            for ext in &extensions {
                let candidate = dir_path.join(format!("{name}{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn positive(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("mcp_runtime_proxy_invalid_{name}:{value}"))
}
fn append_tail(target: &mut Vec<u8>, bytes: &[u8]) {
    target.extend_from_slice(bytes);
    if target.len() > TAIL_LIMIT {
        target.drain(..target.len() - TAIL_LIMIT);
    }
}
fn safe_segment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || "._-".contains(character) {
                character
            } else {
                '_'
            }
        })
        .take(80)
        .collect()
}
fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn json_io(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
fn io_string(error: io::Error) -> String {
    error.to_string()
}
fn lock_error<T>(error: std::sync::PoisonError<T>) -> String {
    format!("mcp_runtime_proxy_lock_poisoned:{error}")
}
fn default_diagnostics_dir() -> PathBuf {
    env::var_os("NARADA_SITE_ROOT")
        .or_else(|| env::var_os("NARADA_WORKSPACE_ROOT"))
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_default())
        .join(".ai")
        .join("runtime")
        .join("mcp-runtime-proxy")
}

#[cfg(windows)]
struct KillJob(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Drop for KillJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn assign_kill_job(child: &Arc<Mutex<Child>>) -> Result<KillJob, String> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(format!(
            "mcp_runtime_proxy_job_create_failed:{}",
            io::Error::last_os_error()
        ));
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let configured = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &mut limits as *mut _ as *mut c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if configured == 0 {
        unsafe {
            CloseHandle(job);
        }
        return Err(format!(
            "mcp_runtime_proxy_job_configure_failed:{}",
            io::Error::last_os_error()
        ));
    }
    let handle =
        child.lock().map_err(lock_error)?.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    if unsafe { AssignProcessToJobObject(job, handle) } == 0 {
        unsafe {
            CloseHandle(job);
        }
        return Err(format!(
            "mcp_runtime_proxy_job_assign_failed:{}",
            io::Error::last_os_error()
        ));
    }
    Ok(KillJob(job))
}

#[cfg(not(windows))]
struct KillJob;
#[cfg(not(windows))]
fn assign_kill_job(_child: &Arc<Mutex<Child>>) -> Result<KillJob, String> {
    Ok(KillJob)
}

#[cfg(windows)]
fn resume_main_thread(process_id: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!(
                "mcp_runtime_proxy_thread_snapshot_failed:{}",
                io::Error::last_os_error()
            ));
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut thread_id = 0;
        if Thread32First(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32OwnerProcessID == process_id {
                    thread_id = entry.th32ThreadID;
                    break;
                }
                if Thread32Next(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        if thread_id == 0 {
            return Err("mcp_runtime_proxy_suspended_child_thread_missing".to_string());
        }
        let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id);
        if thread.is_null() {
            return Err(format!(
                "mcp_runtime_proxy_thread_open_failed:{}",
                io::Error::last_os_error()
            ));
        }
        let resumed = ResumeThread(thread);
        CloseHandle(thread);
        if resumed == u32::MAX {
            return Err(format!(
                "mcp_runtime_proxy_thread_resume_failed:{}",
                io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}
