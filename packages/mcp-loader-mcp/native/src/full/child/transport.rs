use crate::full::*;

impl ChildSession {
    pub(crate) fn request(
        &self,
        method: &str,
        params: Value,
        timeout_ms: u64,
    ) -> Result<Value, Diagnostic> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Diagnostic::new(
                "connection_detached",
                "connection_detached",
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| Diagnostic::new("pending_lock_failed", "pending_lock_failed"))?
            .insert(id, sender);
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let write_result = self
            .stdin
            .lock()
            .map_err(|_| Diagnostic::new("child_stdin_lock_failed", "child_stdin_lock_failed"))
            .and_then(|mut stdin| {
                write_wire(&mut *stdin, &request, false).map_err(|error| {
                    Diagnostic::new(
                        "child_write_failed",
                        format!("child_write_failed:{}", error),
                    )
                })
            });
        if let Err(error) = write_result {
            let _ = self.pending.lock().map(|mut pending| pending.remove(&id));
            return Err(error);
        }
        match receiver.recv_timeout(Duration::from_millis(timeout_ms)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.pending.lock().map(|mut pending| pending.remove(&id));
                Err(Diagnostic::new(
                    "child_timeout",
                    format!("child_timeout:{}:{}ms", method, timeout_ms),
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(Diagnostic::new("child_exited", "child_exited"))
            }
        }
    }

    pub(crate) fn notify(&self, method: &str, params: Value) -> Result<(), Diagnostic> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let request = json!({"jsonrpc":"2.0","method":method,"params":params});
        self.stdin
            .lock()
            .map_err(|_| Diagnostic::new("child_stdin_lock_failed", "child_stdin_lock_failed"))
            .and_then(|mut stdin| {
                write_wire(&mut *stdin, &request, false).map_err(|error| {
                    Diagnostic::new(
                        "child_write_failed",
                        format!("child_write_failed:{}", error),
                    )
                })
            })
    }

    pub(crate) fn alive(&self) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        let alive = self
            .child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok())
            .is_some_and(|status| status.is_none());
        if !alive {
            self.closed.store(true, Ordering::SeqCst);
        }
        alive
    }

    pub(crate) fn terminate(&self) -> Value {
        self.closed.store(true, Ordering::SeqCst);
        let mut child = match self.child.lock() {
            Ok(child) => child,
            Err(_) => return json!({"status":"termination_lock_failed"}),
        };
        if let Ok(Some(status)) = child.try_wait() {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                return json!({"status":"already_exited","exit_code":status.code(),"signal":status.signal(),"forced":false});
            }
            #[cfg(not(unix))]
            return json!({"status":"already_exited","exit_code":status.code(),"signal":Value::Null,"forced":false});
        }
        let killed = child.kill().is_ok();
        self.killed.store(killed, Ordering::SeqCst);
        let waited = child.wait().ok();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            return json!({"status":if killed {"terminated"} else {"termination_failed"},"exit_code":waited.as_ref().and_then(|status| status.code()),"signal":waited.as_ref().and_then(|status| status.signal()),"forced":killed});
        }
        #[cfg(not(unix))]
        json!({"status":if killed {"terminated"} else {"termination_failed"},"exit_code":waited.as_ref().and_then(|status| status.code()),"signal":Value::Null,"forced":killed})
    }

    pub(crate) fn stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    }
    pub(crate) fn exit_code(&self) -> Option<i32> {
        self.child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok().flatten())
            .and_then(|status| status.code())
    }
    #[cfg(unix)]
    pub(crate) fn signal_code(&self) -> Option<i32> {
        use std::os::unix::process::ExitStatusExt;
        self.child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok().flatten())
            .and_then(|status| status.signal())
    }
    #[cfg(not(unix))]
    pub(crate) fn signal_code(&self) -> Option<i32> {
        None
    }
    pub(crate) fn killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }
}
