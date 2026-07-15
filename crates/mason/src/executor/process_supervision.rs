#[cfg(test)]
fn logged(
    command: &str,
    containment: DescendantContainment,
    configure: impl FnOnce(&mut process::Command) -> &mut process::Command,
) -> Result<process::ExitStatus, StepExecutionError> {
    logged_retaining(command, None, containment, configure)
}

fn logged_retaining(
    command: &str,
    descriptor_exec: Option<DescriptorExec>,
    containment: DescendantContainment,
    configure: impl FnOnce(&mut process::Command) -> &mut process::Command,
) -> Result<process::ExitStatus, StepExecutionError> {
    logged_with_limits(
        command,
        descriptor_exec,
        containment,
        StepExecutionLimits::production(),
        LogMode::Stream,
        configure,
    )
}

#[derive(Debug, Clone, Copy)]
struct StepExecutionLimits {
    wall_time: Duration,
    stdout_bytes: u64,
    stderr_bytes: u64,
    total_output_bytes: u64,
}

impl StepExecutionLimits {
    const fn production() -> Self {
        Self {
            wall_time: STEP_WALL_TIME_LIMIT,
            stdout_bytes: STEP_STDOUT_BYTE_LIMIT,
            stderr_bytes: STEP_STDERR_BYTE_LIMIT,
            total_output_bytes: STEP_TOTAL_OUTPUT_BYTE_LIMIT,
        }
    }

    fn stream_limit(self, stream: OutputStream) -> u64 {
        match stream {
            OutputStream::Stdout => self.stdout_bytes,
            OutputStream::Stderr => self.stderr_bytes,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogMode {
    Stream,
    Discard,
}

fn logged_with_limits(
    command: &str,
    descriptor_exec: Option<DescriptorExec>,
    containment: DescendantContainment,
    limits: StepExecutionLimits,
    log_mode: LogMode,
    configure: impl FnOnce(&mut process::Command) -> &mut process::Command,
) -> Result<process::ExitStatus, StepExecutionError> {
    let mut command = process::Command::new(command);
    configure(&mut command);
    // Frozen steps receive only their configured stdio. Mark every other
    // descriptor close-on-exec in the post-fork child; this also covers
    // descriptors inherited by Cast from its own launcher.
    unsafe {
        command.pre_exec(move || {
            if nix::libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            set_child_resource_limits()?;
            const CLOSE_RANGE_CLOEXEC: nix::libc::c_uint = 1 << 2;
            let result = nix::libc::syscall(
                nix::libc::SYS_close_range,
                3 as nix::libc::c_uint,
                nix::libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            );
            if result == -1 {
                return Err(io::Error::last_os_error());
            }
            if let Some(descriptor_exec) = &descriptor_exec {
                // SAFETY: `DescriptorExec` prepared and retained every C
                // string and pointer in the parent. The child has completed
                // its stdio, working-directory, limit, process-group, and
                // descriptor setup, so this is the final operation before
                // replacing the child image.
                descriptor_exec.execveat()?;
            }
            Ok(())
        });
    }
    let mut child = command
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .spawn()
        .map_err(|source| StepExecutionError::Spawn { source })?;
    let child_pid = Pid::from_raw(child.id() as i32);
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut setup_failure = set_nonblocking(stdout.as_raw_fd())
        .and_then(|()| set_nonblocking(stderr.as_raw_fd()))
        .err()
        .map(|source| StepExecutionError::PipeSetup { source });

    let output_budget = Arc::new(Mutex::new(OutputBudget::default()));
    let log_mux = Arc::new(Mutex::new(LogMux::new(log_mode)));
    let stop_readers = Arc::new(AtomicBool::new(false));
    let (alert_sender, alert_receiver) = mpsc::channel();

    let stdout_reader = spawn_log_reader(
        stdout,
        OutputStream::Stdout,
        limits,
        Arc::clone(&output_budget),
        Arc::clone(&log_mux),
        Arc::clone(&stop_readers),
        alert_sender.clone(),
    );
    let stderr_reader = spawn_log_reader(
        stderr,
        OutputStream::Stderr,
        limits,
        output_budget,
        log_mux,
        Arc::clone(&stop_readers),
        alert_sender,
    );

    let mut stdout_reader = match stdout_reader {
        Ok(reader) => Some(reader),
        Err(source) => {
            setup_failure.get_or_insert(StepExecutionError::ReaderThreadSpawn {
                stream: OutputStream::Stdout,
                source,
            });
            None
        }
    };
    let mut stderr_reader = match stderr_reader {
        Ok(reader) => Some(reader),
        Err(source) => {
            setup_failure.get_or_insert(StepExecutionError::ReaderThreadSpawn {
                stream: OutputStream::Stderr,
                source,
            });
            None
        }
    };

    let started = Instant::now();
    let terminal = if let Some(failure) = setup_failure {
        StepTerminal::Failure(failure)
    } else if let Err(source) = ::container::forward_sigint(child_pid) {
        StepTerminal::Failure(StepExecutionError::SignalForward { source })
    } else {
        monitor_step(&mut child, started, limits.wall_time, &alert_receiver)
    };

    // Readers are deliberately stopped only after the complete containment
    // boundary has been killed and the direct child has been reaped. This
    // ordering prevents a daemonized pipe holder from blocking the joins and
    // prevents descendants from surviving into the next frozen step.
    let child_was_reaped = matches!(terminal, StepTerminal::Exited(_));
    let cleanup_failure = cleanup_step(&mut child, containment, child_pid, child_was_reaped);
    stop_readers.store(true, Ordering::Release);

    let stdout_result = join_log_reader(&mut stdout_reader, OutputStream::Stdout);
    let stderr_result = join_log_reader(&mut stderr_reader, OutputStream::Stderr);
    let reader_failure = stdout_result.err().or_else(|| stderr_result.err());

    let result = match terminal {
        StepTerminal::Exited(status) => reader_failure.map_or(Ok(status), Err),
        StepTerminal::ReaderAlert => Err(reader_failure.unwrap_or(StepExecutionError::ReaderAlertLost)),
        StepTerminal::Failure(failure) => Err(failure),
    };

    match (result, cleanup_failure) {
        (result, None) => result,
        (Ok(_), Some(cleanup)) => Err(StepExecutionError::Cleanup {
            operation: cleanup.operation,
            source: cleanup.source,
        }),
        (Err(failure), Some(cleanup)) => Err(StepExecutionError::CleanupAfterFailure {
            failure: Box::new(failure),
            operation: cleanup.operation,
            source: cleanup.source,
        }),
    }
}

/// Apply only child-local limits whose semantics do not depend on the host UID
/// or on a guessed build memory/file-size requirement.
fn set_child_resource_limits() -> io::Result<()> {
    let core = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { nix::libc::setrlimit(nix::libc::RLIMIT_CORE, &core) } == -1 {
        return Err(io::Error::last_os_error());
    }

    let mut inherited_nofile = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut inherited_nofile) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let nofile_max = inherited_nofile.rlim_max.min(STEP_OPEN_FILE_LIMIT);
    let nofile = nix::libc::rlimit {
        rlim_cur: inherited_nofile.rlim_cur.min(nofile_max),
        rlim_max: nofile_max,
    };
    if unsafe { nix::libc::setrlimit(nix::libc::RLIMIT_NOFILE, &nofile) } == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = fcntl(fd, FcntlArg::F_GETFL).map_err(io_error_from_errno)?;
    let flags = OFlag::from_bits_truncate(flags);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))
        .map(|_| ())
        .map_err(io_error_from_errno)
}

fn io_error_from_errno(error: Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

#[derive(Debug)]
enum StepTerminal {
    Exited(process::ExitStatus),
    ReaderAlert,
    Failure(StepExecutionError),
}

fn monitor_step(
    child: &mut process::Child,
    started: Instant,
    wall_time: Duration,
    alerts: &mpsc::Receiver<()>,
) -> StepTerminal {
    loop {
        if alerts.try_recv().is_ok() {
            return StepTerminal::ReaderAlert;
        }

        match child.try_wait() {
            Ok(Some(status)) => return StepTerminal::Exited(status),
            Ok(None) => {}
            Err(source) => return StepTerminal::Failure(StepExecutionError::Wait { source }),
        }

        if started.elapsed() >= wall_time {
            return StepTerminal::Failure(StepExecutionError::Timeout { limit: wall_time });
        }

        thread::sleep(STEP_MONITOR_INTERVAL.min(wall_time.saturating_sub(started.elapsed())));
    }
}

#[derive(Debug)]
struct CleanupFailure {
    operation: &'static str,
    source: io::Error,
}

fn cleanup_step(
    child: &mut process::Child,
    containment: DescendantContainment,
    child_pid: Pid,
    child_was_reaped: bool,
) -> Option<CleanupFailure> {
    let mut failure = terminate_step_descendants(containment, child_pid)
        .err()
        .map(|source| CleanupFailure {
            operation: "terminate containment boundary",
            source,
        });

    if !child_was_reaped {
        // Namespace-wide cleanup normally reaps the direct child itself. A
        // direct kill is also attempted so a boundary-cleanup error cannot
        // leave child.wait() blocking forever.
        match kill(child_pid, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => {
                failure.get_or_insert(CleanupFailure {
                    operation: "kill direct child",
                    source: error.into(),
                });
            }
        }

        if let Err(source) = child.wait()
            && !(containment == DescendantContainment::PidNamespace
                && source.raw_os_error() == Some(Errno::ECHILD as i32))
        {
            failure.get_or_insert(CleanupFailure {
                operation: "reap direct child",
                source,
            });
        }
    }

    failure
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DescendantContainment {
    /// Production execution runs as PID 1 in a dedicated namespace. Signaling
    /// every other visible process catches daemonization, `setsid`, and
    /// double-fork escapes before the next step or packaging can begin.
    PidNamespace,
    /// Direct unit tests do not own their PID namespace. A private process
    /// group provides a safe behavioral test boundary there.
    #[cfg(test)]
    ProcessGroup,
}

fn require_pid_namespace_init(pid: Pid) -> Result<(), Error> {
    if pid.as_raw() == 1 {
        Ok(())
    } else {
        Err(Error::PidNamespaceInitRequired(pid.as_raw()))
    }
}

fn terminate_step_descendants(containment: DescendantContainment, child_pid: Pid) -> io::Result<()> {
    match containment {
        DescendantContainment::PidNamespace => {
            if getpid().as_raw() != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "refusing namespace-wide descendant cleanup outside PID 1",
                ));
            }

            match kill(descendant_signal_target(containment, child_pid), Signal::SIGKILL) {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            }
            loop {
                match waitpid(Pid::from_raw(-1), None) {
                    Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => {}
                    Ok(
                        WaitStatus::Stopped(..)
                        | WaitStatus::PtraceEvent(..)
                        | WaitStatus::PtraceSyscall(..)
                        | WaitStatus::Continued(..)
                        | WaitStatus::StillAlive,
                    ) => {}
                    Err(Errno::EINTR) => {}
                    Err(Errno::ECHILD) => break,
                    Err(error) => return Err(error.into()),
                }
            }
        }
        #[cfg(test)]
        DescendantContainment::ProcessGroup => {
            match kill(descendant_signal_target(containment, child_pid), Signal::SIGKILL) {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

fn descendant_signal_target(containment: DescendantContainment, _child_pid: Pid) -> Pid {
    match containment {
        DescendantContainment::PidNamespace => Pid::from_raw(-1),
        #[cfg(test)]
        DescendantContainment::ProcessGroup => Pid::from_raw(-_child_pid.as_raw()),
    }
}
