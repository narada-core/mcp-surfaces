//! Exclusive authority locking (`fs2`) for serialized ledger mutation.
//! Contention is polled within a bounded timeout and then refused with
//! `authority_busy`; other acquisition failures refuse with
//! `authority_lock_failed`.

use fs2::FileExt;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::digest::safe_name;
use crate::error::ErrorSchema;

/// Bounded lock-acquisition policy.
#[derive(Clone, Copy, Debug)]
pub struct AuthorityLockPolicy {
    pub timeout: Duration,
    pub poll: Duration,
}

impl Default for AuthorityLockPolicy {
    /// 10 s timeout, 25 ms poll.
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            poll: Duration::from_millis(25),
        }
    }
}

/// Run `action` while holding an exclusive `fs2` file lock named
/// `<safe_name(key)>.lock` inside `lock_directory`.
pub fn with_authority_lock<T>(
    schema: ErrorSchema,
    lock_directory: &Path,
    key: &str,
    policy: AuthorityLockPolicy,
    action: impl FnOnce() -> Result<T, Value>,
) -> Result<T, Value> {
    fs::create_dir_all(lock_directory)
        .map_err(schema.io_error("authority_lock_store_create_failed"))?;
    let lock_path = lock_directory.join(format!("{}.lock", safe_name(key)));
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(schema.io_error("authority_lock_open_failed"))?;
    let started = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => break,
            Err(source) if lock_contended(&source) && started.elapsed() < policy.timeout => {
                thread::sleep(policy.poll)
            }
            Err(source) if lock_contended(&source) => {
                return Err(schema.error(
                    "authority_busy",
                    "authority lock could not be acquired within the bounded timeout",
                    json!({"lock_key":key,"timeout_ms":policy.timeout.as_millis(),"source":source.to_string()}),
                ));
            }
            Err(source) => {
                return Err(schema.error(
                    "authority_lock_failed",
                    "authority lock acquisition failed",
                    json!({"lock_key":key,"source":source.to_string()}),
                ));
            }
        }
    }
    let result = action();
    let unlock = FileExt::unlock(&file);
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(failure), _) => Err(failure),
        (Ok(_), Err(source)) => Err(schema.error(
            "authority_unlock_failed",
            "authority mutation completed but its process lock could not be released",
            json!({"lock_key":key,"source":source.to_string()}),
        )),
    }
}

/// Whether an acquisition failure is contention (retryable within the
/// timeout): `WouldBlock` plus the Windows raw errors 32/33.
pub fn lock_contended(source: &std::io::Error) -> bool {
    source.kind() == std::io::ErrorKind::WouldBlock
        || matches!(source.raw_os_error(), Some(32 | 33))
}
