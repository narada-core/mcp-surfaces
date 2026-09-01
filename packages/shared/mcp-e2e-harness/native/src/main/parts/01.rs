
#[cfg(windows)]
mod windows_scope {
    use std::env;
    use std::ffi::{c_void, OsStr};
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::thread;

    type Bool = i32;
    type Dword = u32;
    type Handle = *mut c_void;

    const FALSE: Bool = 0;
    const TRUE: Bool = 1;
    const INFINITE: Dword = 0xffff_ffff;
    const WAIT_OBJECT_0: Dword = 0;
    const CREATE_SUSPENDED: Dword = 0x0000_0004;
    const CREATE_UNICODE_ENVIRONMENT: Dword = 0x0000_0400;
    const CREATE_NO_WINDOW: Dword = 0x0800_0000;
    const STARTF_USESTDHANDLES: Dword = 0x0000_0100;
    const HANDLE_FLAG_INHERIT: Dword = 0x0000_0001;
    const STD_INPUT_HANDLE: Dword = 0xffff_fff6;
    const STD_OUTPUT_HANDLE: Dword = 0xffff_fff5;
    const STD_ERROR_HANDLE: Dword = 0xffff_fff4;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: Dword = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: Dword = 0x0000_2000;
    const STILL_ACTIVE: Dword = 259;

    #[repr(C)]
    struct SecurityAttributes {
        length: Dword,
        descriptor: *mut c_void,
        inherit_handle: Bool,
    }

    #[repr(C)]
    struct StartupInfo {
        cb: Dword,
        reserved: *mut u16,
        desktop: *mut u16,
        title: *mut u16,
        x: Dword,
        y: Dword,
        x_size: Dword,
        y_size: Dword,
        x_count_chars: Dword,
        y_count_chars: Dword,
        fill_attribute: Dword,
        flags: Dword,
        show_window: u16,
        reserved2: u16,
        reserved2_ptr: *mut u8,
        stdin: Handle,
        stdout: Handle,
        stderr: Handle,
    }

    #[repr(C)]
    struct ProcessInformation {
        process: Handle,
        thread: Handle,
        process_id: Dword,
        thread_id: Dword,
    }

    #[repr(C)]
    struct JobObjectBasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: Dword,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: Dword,
        affinity: usize,
        priority_class: Dword,
        scheduling_class: Dword,
    }

    #[repr(C)]
    struct IoCounters {
        read_operations: u64,
        write_operations: u64,
        other_operations: u64,
        read_bytes: u64,
        write_bytes: u64,
        other_bytes: u64,
    }

    #[repr(C)]
    struct JobObjectExtendedLimitInformation {
        basic: JobObjectBasicLimitInformation,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CloseHandle(handle: Handle) -> Bool;
        fn CreateJobObjectW(attributes: *mut SecurityAttributes, name: *const u16) -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            class: Dword,
            info: *mut c_void,
            length: Dword,
        ) -> Bool;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> Bool;
        fn CreateProcessW(
            application_name: *const u16,
            command_line: *mut u16,
            process_attributes: *mut SecurityAttributes,
            thread_attributes: *mut SecurityAttributes,
            inherit_handles: Bool,
            creation_flags: Dword,
            environment: *mut c_void,
            current_directory: *const u16,
            startup_info: *mut StartupInfo,
            process_information: *mut ProcessInformation,
        ) -> Bool;
        fn ResumeThread(thread: Handle) -> Dword;
        fn WaitForSingleObject(handle: Handle, milliseconds: Dword) -> Dword;
        fn GetExitCodeProcess(process: Handle, exit_code: *mut Dword) -> Bool;
        fn TerminateJobObject(job: Handle, exit_code: Dword) -> Bool;
        fn CreatePipe(
            read_pipe: *mut Handle,
            write_pipe: *mut Handle,
            attributes: *mut SecurityAttributes,
            size: Dword,
        ) -> Bool;
        fn SetHandleInformation(handle: Handle, mask: Dword, flags: Dword) -> Bool;
        fn GetStdHandle(which: Dword) -> Handle;
        fn ReadFile(
            file: Handle,
            buffer: *mut u8,
            length: Dword,
            read: *mut Dword,
            overlapped: *mut c_void,
        ) -> Bool;
        fn WriteFile(
            file: Handle,
            buffer: *const u8,
            length: Dword,
            written: *mut Dword,
            overlapped: *mut c_void,
        ) -> Bool;
        fn GetLastError() -> Dword;
        fn PeekNamedPipe(
            file: Handle,
            buffer: *mut u8,
            length: Dword,
            read: *mut Dword,
            available: *mut Dword,
            left: *mut Dword,
        ) -> Bool;
        fn Sleep(milliseconds: Dword) -> ();
    }

    struct HandleGuard(Handle);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    fn fail(operation: &str) -> String {
        format!("{operation} failed with Win32 error {}", unsafe {
            GetLastError()
        })
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn quote_windows(value: &str) -> String {
        let mut result = String::from("\"");
        let mut slashes = 0usize;
        for character in value.chars() {
            match character {
                '\\' => slashes += 1,
                '"' => {
                    result.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                    result.push('"');
                    slashes = 0;
                }
                _ => {
                    result.extend(std::iter::repeat_n('\\', slashes));
                    result.push(character);
                    slashes = 0;
                }
            }
        }
        result.extend(std::iter::repeat_n('\\', slashes * 2));
        result.push('"');
        result
    }

    unsafe fn close(handle: Handle) {
        if !handle.is_null() {
            CloseHandle(handle);
        }
    }

    unsafe fn make_pipe() -> Result<(Handle, Handle), String> {
        let mut read = null_mut();
        let mut write = null_mut();
        let mut attributes = SecurityAttributes {
            length: size_of::<SecurityAttributes>() as Dword,
            descriptor: null_mut(),
            inherit_handle: TRUE,
        };
        if CreatePipe(&mut read, &mut write, &mut attributes, 0) == FALSE {
            return Err(fail("CreatePipe"));
        }
        Ok((read, write))
    }

    unsafe fn make_parent_pipe_end_non_inheritable(handle: Handle) -> Result<(), String> {
        if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) == FALSE {
            return Err(fail("SetHandleInformation"));
        }
        Ok(())
    }

    unsafe fn relay_stdin(input_value: usize, output_value: usize, stop: Arc<AtomicBool>) {
        let input = input_value as Handle;
        let output = output_value as Handle;
        let mut buffer = [0u8; 16 * 1024];
        while !stop.load(Ordering::SeqCst) {
            let mut available = 0;
            if PeekNamedPipe(input, null_mut(), 0, null_mut(), &mut available, null_mut()) == FALSE
            {
                break;
            }
            if available == 0 {
                Sleep(10);
                continue;
            }
            let mut read = 0;
            if ReadFile(
                input,
                buffer.as_mut_ptr(),
                buffer.len() as Dword,
                &mut read,
                null_mut(),
            ) == FALSE
                || read == 0
            {
                break;
            }
            let mut offset = 0usize;
            while offset < read as usize {
                let mut written = 0;
                if WriteFile(
                    output,
                    buffer[offset..read as usize].as_ptr(),
                    (read as usize - offset) as Dword,
                    &mut written,
                    null_mut(),
                ) == FALSE
                    || written == 0
                {
                    break;
                }
                offset += written as usize;
            }
        }
        close(input);
        close(output);
    }
    unsafe fn relay(input_value: usize, output_value: usize) {
        let input = input_value as Handle;
        let output = output_value as Handle;
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let mut read = 0;
            if ReadFile(
                input,
                buffer.as_mut_ptr(),
                buffer.len() as Dword,
                &mut read,
                null_mut(),
            ) == FALSE
                || read == 0
            {
                break;
            }
            let mut offset = 0usize;
            while offset < read as usize {
                let mut written = 0;
                if WriteFile(
                    output,
                    buffer[offset..read as usize].as_ptr(),
                    (read as usize - offset) as Dword,
                    &mut written,
                    null_mut(),
                ) == FALSE
                    || written == 0
                {
                    close(input);
                    close(output);
                    return;
                }
                offset += written as usize;
            }
        }
        close(input);
        close(output);
    }

    fn run(command: String, arguments: Vec<String>) -> Result<u32, String> {
        unsafe {
            let job = CreateJobObjectW(null_mut(), null());
            if job.is_null() {
                return Err(fail("CreateJobObjectW"));
            }
            let job_guard = HandleGuard(job);
            let mut limits: JobObjectExtendedLimitInformation = std::mem::zeroed();
            limits.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                &mut limits as *mut _ as *mut c_void,
                size_of::<JobObjectExtendedLimitInformation>() as Dword,
            ) == FALSE
            {
                return Err(fail("SetInformationJobObject"));
            }

            let parent_stdin = GetStdHandle(STD_INPUT_HANDLE);
            let parent_stdout = GetStdHandle(STD_OUTPUT_HANDLE);
            let parent_stderr = GetStdHandle(STD_ERROR_HANDLE);
            if parent_stdin.is_null() || parent_stdout.is_null() || parent_stderr.is_null() {
                return Err("the process-scope helper requires all standard handles".to_string());
            }
            // The child receives the dedicated pipe ends through STARTF_USESTDHANDLES.
            // Keep the helper's inherited standard handles private so
            // CREATE_PROCESS_INHERIT_HANDLES cannot leak the outer console
            // handles into the scoped command or its descendants.
            make_parent_pipe_end_non_inheritable(parent_stdin)?;
            make_parent_pipe_end_non_inheritable(parent_stdout)?;
            make_parent_pipe_end_non_inheritable(parent_stderr)?;

            let (child_stdin_read, parent_stdin_write) = make_pipe()?;
            let (parent_stdout_read, child_stdout_write) = make_pipe()?;
            let (parent_stderr_read, child_stderr_write) = make_pipe()?;
            make_parent_pipe_end_non_inheritable(parent_stdin_write)?;
            make_parent_pipe_end_non_inheritable(parent_stdout_read)?;
            make_parent_pipe_end_non_inheritable(parent_stderr_read)?;

            let command_line = std::iter::once(command.clone())
                .chain(arguments.iter().cloned())
                .map(|item| quote_windows(&item))
                .collect::<Vec<_>>()
                .join(" ");
            let mut command_line_w = wide(&command_line);
            let mut directory_w = match env::current_dir() {
                Ok(path) => Some(wide(&path.to_string_lossy())),
                Err(_) => None,
            };
            let mut startup: StartupInfo = std::mem::zeroed();
            startup.cb = size_of::<StartupInfo>() as Dword;
            startup.flags = STARTF_USESTDHANDLES;
            startup.stdin = child_stdin_read;
            startup.stdout = child_stdout_write;
            startup.stderr = child_stderr_write;
            let mut process_info: ProcessInformation = std::mem::zeroed();

            let created = CreateProcessW(
                null(),
                command_line_w.as_mut_ptr(),
                null_mut(),
                null_mut(),
                TRUE,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
                null_mut(),
                directory_w.as_mut().map_or(null(), |value| value.as_ptr()),
                &mut startup,
                &mut process_info,
            );
            close(child_stdin_read);
            close(child_stdout_write);
            close(child_stderr_write);
            if created == FALSE {
                close(parent_stdout_read);
                close(parent_stderr_read);
                return Err(fail("CreateProcessW"));
            }

            let process_guard = HandleGuard(process_info.process);
            let thread_guard = HandleGuard(process_info.thread);
            if AssignProcessToJobObject(job, process_info.process) == FALSE {
                TerminateJobObject(job, 1);
                return Err(fail("AssignProcessToJobObject"));
            }
            if ResumeThread(process_info.thread) == u32::MAX {
                TerminateJobObject(job, 1);
                return Err(fail("ResumeThread"));
            }

            let stdin_input = parent_stdin as usize;
            let stdin_output = parent_stdin_write as usize;
            let stdout_input = parent_stdout_read as usize;
            let stdout_output = parent_stdout as usize;
            let stderr_input = parent_stderr_read as usize;
            let stderr_output = parent_stderr as usize;
            let stop_stdin = Arc::new(AtomicBool::new(false));
            let stop_stdin_thread = Arc::clone(&stop_stdin);
            let stdin_thread =
                thread::spawn(move || relay_stdin(stdin_input, stdin_output, stop_stdin_thread));
            let stdout_thread = thread::spawn(move || relay(stdout_input, stdout_output));
            let stderr_thread = thread::spawn(move || relay(stderr_input, stderr_output));

            let wait_result = WaitForSingleObject(process_info.process, INFINITE);
            stop_stdin.store(true, Ordering::SeqCst);
            let mut exit_code = STILL_ACTIVE;
            if GetExitCodeProcess(process_info.process, &mut exit_code) == FALSE {
                exit_code = 1;
            }

            // The scope ends with the root process. Explicitly terminate the
            // job before joining the relay threads so a descendant that keeps
            // an inherited stdout/stderr handle open cannot hold this helper
            // (and therefore the test harness) indefinitely. The job object
            // remains kill-on-close as a final cleanup boundary.
            if wait_result == WAIT_OBJECT_0 {
                let _ = TerminateJobObject(job, exit_code);
            } else {
                let _ = TerminateJobObject(job, 1);
                exit_code = 1;
            }

            // TerminateJobObject is asynchronous with respect to descendants.
            // Wait for the job itself, not only its root process, so every
            // descendant has released SQLite/file handles before the scoped
            // child is reported as closed to the harness.
            let _ = WaitForSingleObject(job, INFINITE);

            let _ = stdin_thread.join();
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            drop(thread_guard);
            drop(process_guard);
            drop(job_guard);
            Ok(exit_code)
        }
    }

    pub fn main() {
        let mut args = env::args().skip(1);
        if args.next().as_deref() != Some("--") {
            eprintln!("usage: narada-test-process-scope.exe -- <command> [args...]");
            std::process::exit(64);
        }
        let command = match args.next() {
            Some(value) => value,
            None => {
                eprintln!("missing command");
                std::process::exit(64);
            }
        };
        match run(command, args.collect()) {
            Ok(code) => std::process::exit(code as i32),
            Err(error) => {
                eprintln!("narada-test-process-scope:{error}");
                std::process::exit(70);
            }
        }
    }
}

