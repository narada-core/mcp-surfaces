fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("materialized");
    let nonce = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let temporary = parent.join(format!(".{name}.narada-{}-{nonce}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    replace_file(&temporary, path)?;
    let installed = fs::read(path)?;
    if installed != content {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable_replace_verification_failed",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        REPLACEFILE_WRITE_THROUGH,
    };
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let temporary_wide = wide(temporary);
    let destination_wide = wide(destination);
    let mut result = unsafe {
        if destination.exists() {
            ReplaceFileW(
                destination_wide.as_ptr(),
                temporary_wide.as_ptr(),
                ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        } else {
            MoveFileExW(
                temporary_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if result == 0 && destination.exists() {
        result = unsafe {
            MoveFileExW(
                temporary_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
    }
    if result == 0 {
        let error = std::io::Error::last_os_error();
        let _ = fs::remove_file(temporary);
        return Err(error);
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(temporary, destination)
}

fn rollback(snapshots: &[Snapshot]) -> Vec<String> {
    let mut errors = Vec::new();
    for snapshot in snapshots.iter().rev() {
        let result = match &snapshot.content {
            Some(content) => atomic_write(&snapshot.path, content),
            None if snapshot.path.exists() => fs::remove_file(&snapshot.path),
            None => Ok(()),
        };
        if let Err(error) = result {
            errors.push(format!("{}:{error}", path_text(&snapshot.path)));
        }
    }
    errors
}

fn json_failure(error: serde_json::Error) -> Failure {
    Failure::new("materializer_json_failed", error.to_string())
}
fn pretty_json(value: &Value) -> Result<Vec<u8>, Failure> {
    contract_pretty_json(value).map_err(|error| Failure::new("materializer_json_failed", error))
}
fn sha256(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}
fn suffix_path(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.to_string_lossy()))
}
fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

