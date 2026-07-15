use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) struct AnalyzerLimits {
    pub(super) wall_timeout: Duration,
    pub(super) stdout_bytes: usize,
    pub(super) stderr_bytes: usize,
}

/// Construct an analyzer subprocess with no ambient environment or readable
/// standard input. Analyzer tools are part of frozen execution and must not
/// gain inputs from the process which launched Cast.
pub(in super::super) fn analyzer_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.env_clear().stdin(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if nix::libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            set_analyzer_limit(nix::libc::RLIMIT_AS, ANALYZER_ADDRESS_SPACE_BYTES)?;
            set_analyzer_limit(nix::libc::RLIMIT_FSIZE, ANALYZER_FILE_BYTES)?;
            set_analyzer_limit(nix::libc::RLIMIT_NOFILE, ANALYZER_OPEN_FILES)?;
            set_analyzer_limit(nix::libc::RLIMIT_CORE, 0)?;
            const CLOSE_RANGE_CLOEXEC: nix::libc::c_uint = 1 << 2;
            let result = nix::libc::syscall(
                nix::libc::SYS_close_range,
                3 as nix::libc::c_uint,
                nix::libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            );
            if result == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command
}

fn set_analyzer_limit(resource: nix::libc::__rlimit_resource_t, ceiling: nix::libc::rlim_t) -> std::io::Result<()> {
    let mut inherited = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `inherited` is writable and `resource` is one of the constants
    // supplied by `analyzer_command` above.
    if unsafe { nix::libc::getrlimit(resource, &mut inherited) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let bounded = bounded_analyzer_limit(inherited, ceiling);
    // SAFETY: `bounded` contains a soft limit no larger than its hard limit.
    if unsafe { nix::libc::setrlimit(resource, &bounded) } == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(super) fn bounded_analyzer_limit(inherited: nix::libc::rlimit, ceiling: nix::libc::rlim_t) -> nix::libc::rlimit {
    let hard = inherited.rlim_max.min(ceiling);
    nix::libc::rlimit {
        // A hardening boundary may lower an inherited allowance, but must
        // never silently raise a deliberately lower ambient soft limit.
        rlim_cur: inherited.rlim_cur.min(hard),
        rlim_max: hard,
    }
}

pub(in super::super) fn checked_output_for(info: &PathInfo, mut command: Command) -> Result<Output, BoxError> {
    info.check_deadline()?;
    let wall_timeout = ANALYZER_LIMITS.wall_timeout.min(info.remaining_time()?);
    let limits = AnalyzerLimits {
        wall_timeout,
        ..ANALYZER_LIMITS
    };
    let output = checked_output_with_limits(&mut command, limits)?;
    info.check_deadline()?;
    Ok(output)
}

/// Run one analyzer tool and reject all non-success statuses before consuming
/// any partial stdout. Silently accepting failed analysis would make package
/// relations depend on host/runtime failure state outside the frozen plan.
#[cfg(test)]
pub(in super::super) fn checked_output(mut command: Command) -> Result<Output, BoxError> {
    checked_output_with_limits(&mut command, ANALYZER_LIMITS)
}

pub(super) fn checked_output_with_limits(command: &mut Command, limits: AnalyzerLimits) -> Result<Output, BoxError> {
    let invocation = format!("{command:?}");
    let output = contained_output(command, analyzer_containment(), limits, &invocation)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(Box::new(AnalyzerCommandError {
            invocation,
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AnalyzerContainment {
    /// Frozen packaging runs as PID 1. Killing every other process in that
    /// namespace catches process-group changes, `setsid`, and double forks.
    PidNamespace,
    /// Unit tests do not own their PID namespace and use the command's private
    /// process group as a safe behavioral boundary.
    #[cfg(test)]
    ProcessGroup,
}

fn analyzer_containment() -> AnalyzerContainment {
    #[cfg(test)]
    if getpid().as_raw() != 1 {
        return AnalyzerContainment::ProcessGroup;
    }
    AnalyzerContainment::PidNamespace
}

pub(super) fn contained_output(
    command: &mut Command,
    containment: AnalyzerContainment,
    limits: AnalyzerLimits,
    invocation: &str,
) -> Result<Output, AnalyzerExecutionError> {
    let started = Instant::now();
    if matches!(containment, AnalyzerContainment::PidNamespace) && getpid().as_raw() != 1 {
        return Err(AnalyzerExecutionError::Containment {
            invocation: invocation.to_owned(),
        });
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| AnalyzerExecutionError::Spawn {
            invocation: invocation.to_owned(),
            source,
        })?;
    let child_pid = Pid::from_raw(child.id() as i32);
    let (events, received) = mpsc::channel();
    let stdout_reader = match read_analyzer_pipe(
        child.stdout.take().expect("piped analyzer stdout"),
        AnalyzerPipe::Stdout,
        limits.stdout_bytes,
        events.clone(),
    ) {
        Ok(reader) => reader,
        Err(source) => {
            let error = AnalyzerExecutionError::PipeReaderSpawn {
                invocation: invocation.to_owned(),
                pipe: AnalyzerPipe::Stdout,
                source,
            };
            let cleanup = abort_analyzer(&mut child, containment, child_pid, None, false, [], invocation);
            return Err(with_analyzer_cleanup(error, cleanup));
        }
    };
    let stderr_reader = match read_analyzer_pipe(
        child.stderr.take().expect("piped analyzer stderr"),
        AnalyzerPipe::Stderr,
        limits.stderr_bytes,
        events.clone(),
    ) {
        Ok(reader) => reader,
        Err(source) => {
            let error = AnalyzerExecutionError::PipeReaderSpawn {
                invocation: invocation.to_owned(),
                pipe: AnalyzerPipe::Stderr,
                source,
            };
            let cleanup = abort_analyzer(
                &mut child,
                containment,
                child_pid,
                None,
                false,
                [stdout_reader],
                invocation,
            );
            return Err(with_analyzer_cleanup(error, cleanup));
        }
    };
    drop(events);
    let readers = [stdout_reader, stderr_reader];

    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;
    let mut boundary_terminated = false;

    loop {
        match received.try_recv() {
            Ok(event) => {
                if let Err(error) = accept_analyzer_pipe_event(event, &mut stdout, &mut stderr, invocation) {
                    let cleanup = abort_analyzer(
                        &mut child,
                        containment,
                        child_pid,
                        status,
                        boundary_terminated,
                        readers,
                        invocation,
                    );
                    return Err(with_analyzer_cleanup(error, cleanup));
                }
                continue;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) if stdout.is_some() && stderr.is_some() => {}
            Err(TryRecvError::Disconnected) => {
                let error = AnalyzerExecutionError::PipeChannelClosed {
                    invocation: invocation.to_owned(),
                };
                let cleanup = abort_analyzer(
                    &mut child,
                    containment,
                    child_pid,
                    status,
                    boundary_terminated,
                    readers,
                    invocation,
                );
                return Err(with_analyzer_cleanup(error, cleanup));
            }
        }

        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit_status)) => {
                    status = Some(exit_status);
                    if let Err(termination) = terminate_analyzer_boundary(
                        containment,
                        child_pid,
                        &mut child,
                        status,
                        analyzer_cleanup_deadline(),
                    ) {
                        let operation = AnalyzerExecutionError::Cleanup {
                            invocation: invocation.to_owned(),
                            source: termination,
                        };
                        let readers =
                            join_analyzer_pipe_readers_until(readers, invocation, analyzer_cleanup_deadline());
                        return Err(with_analyzer_cleanup(operation, readers));
                    }
                    boundary_terminated = true;
                }
                Ok(None) => {}
                Err(source) => {
                    let error = AnalyzerExecutionError::Monitor {
                        invocation: invocation.to_owned(),
                        source,
                    };
                    let cleanup = abort_analyzer(
                        &mut child,
                        containment,
                        child_pid,
                        status,
                        boundary_terminated,
                        readers,
                        invocation,
                    );
                    return Err(with_analyzer_cleanup(error, cleanup));
                }
            }
        }

        if let Some(exit_status) = status
            && stdout.is_some()
            && stderr.is_some()
        {
            join_analyzer_pipe_readers_until(readers, invocation, analyzer_cleanup_deadline())?;
            return Ok(Output {
                status: exit_status,
                stdout: stdout.take().expect("checked analyzer stdout"),
                stderr: stderr.take().expect("checked analyzer stderr"),
            });
        }

        let elapsed = started.elapsed();
        if elapsed >= limits.wall_timeout {
            let error = AnalyzerExecutionError::Timeout {
                invocation: invocation.to_owned(),
                timeout: limits.wall_timeout,
            };
            let cleanup = abort_analyzer(
                &mut child,
                containment,
                child_pid,
                status,
                boundary_terminated,
                readers,
                invocation,
            );
            return Err(with_analyzer_cleanup(error, cleanup));
        }

        let remaining = limits.wall_timeout.saturating_sub(elapsed);
        match received.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok(event) => {
                if let Err(error) = accept_analyzer_pipe_event(event, &mut stdout, &mut stderr, invocation) {
                    let cleanup = abort_analyzer(
                        &mut child,
                        containment,
                        child_pid,
                        status,
                        boundary_terminated,
                        readers,
                        invocation,
                    );
                    return Err(with_analyzer_cleanup(error, cleanup));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) if stdout.is_some() && stderr.is_some() => {}
            Err(RecvTimeoutError::Disconnected) => {
                let error = AnalyzerExecutionError::PipeChannelClosed {
                    invocation: invocation.to_owned(),
                };
                let cleanup = abort_analyzer(
                    &mut child,
                    containment,
                    child_pid,
                    status,
                    boundary_terminated,
                    readers,
                    invocation,
                );
                return Err(with_analyzer_cleanup(error, cleanup));
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AnalyzerPipe {
    Stdout,
    Stderr,
}

impl fmt::Display for AnalyzerPipe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdout => formatter.write_str("stdout"),
            Self::Stderr => formatter.write_str("stderr"),
        }
    }
}

pub(super) enum AnalyzerPipeEvent {
    Complete { pipe: AnalyzerPipe, bytes: Vec<u8> },
    LimitExceeded { pipe: AnalyzerPipe, limit: usize },
    ReadFailed { pipe: AnalyzerPipe, source: std::io::Error },
}

pub(super) fn read_analyzer_pipe<R>(
    mut pipe: R,
    name: AnalyzerPipe,
    limit: usize,
    events: mpsc::Sender<AnalyzerPipeEvent>,
) -> std::io::Result<JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    thread::Builder::new()
        .name(format!("mason-analyzer-{name}"))
        .spawn(move || {
            let mut bytes = Vec::with_capacity(limit.min(8192));
            let mut buffer = [0_u8; 8192];
            let event = loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break AnalyzerPipeEvent::Complete { pipe: name, bytes },
                    Ok(read) if read > limit.saturating_sub(bytes.len()) => {
                        break AnalyzerPipeEvent::LimitExceeded { pipe: name, limit };
                    }
                    Ok(read) => bytes.extend_from_slice(&buffer[..read]),
                    Err(source) => break AnalyzerPipeEvent::ReadFailed { pipe: name, source },
                }
            };
            let _ = events.send(event);
        })
}

fn accept_analyzer_pipe_event(
    event: AnalyzerPipeEvent,
    stdout: &mut Option<Vec<u8>>,
    stderr: &mut Option<Vec<u8>>,
    invocation: &str,
) -> Result<(), AnalyzerExecutionError> {
    match event {
        AnalyzerPipeEvent::Complete { pipe, bytes } => {
            let destination = match pipe {
                AnalyzerPipe::Stdout => stdout,
                AnalyzerPipe::Stderr => stderr,
            };
            if destination.replace(bytes).is_some() {
                return Err(AnalyzerExecutionError::DuplicatePipeResult {
                    invocation: invocation.to_owned(),
                    pipe,
                });
            }
            Ok(())
        }
        AnalyzerPipeEvent::LimitExceeded { pipe, limit } => Err(AnalyzerExecutionError::OutputLimit {
            invocation: invocation.to_owned(),
            pipe,
            limit,
        }),
        AnalyzerPipeEvent::ReadFailed { pipe, source } => Err(AnalyzerExecutionError::PipeRead {
            invocation: invocation.to_owned(),
            pipe,
            source,
        }),
    }
}

fn abort_analyzer<const N: usize>(
    child: &mut Child,
    containment: AnalyzerContainment,
    child_pid: Pid,
    status: Option<ExitStatus>,
    boundary_terminated: bool,
    readers: [JoinHandle<()>; N],
    invocation: &str,
) -> Result<(), AnalyzerExecutionError> {
    let deadline = analyzer_cleanup_deadline();
    let termination = if boundary_terminated {
        Ok(())
    } else {
        terminate_analyzer_boundary(containment, child_pid, child, status, deadline).map_err(|source| {
            AnalyzerExecutionError::Cleanup {
                invocation: invocation.to_owned(),
                source,
            }
        })
    };
    // Even when signalling/reaping fails, still attempt to join readers. This
    // preserves both failures instead of abandoning detached reader threads at
    // the first cleanup error.
    let reader_cleanup = join_analyzer_pipe_readers_until(readers, invocation, deadline);
    combine_analyzer_cleanup(termination, reader_cleanup)
}

pub(super) fn analyzer_cleanup_deadline() -> Instant {
    Instant::now()
        .checked_add(ANALYZER_CLEANUP_TIMEOUT)
        .unwrap_or_else(Instant::now)
}

pub(super) fn join_analyzer_pipe_readers_until<const N: usize>(
    readers: [JoinHandle<()>; N],
    invocation: &str,
    deadline: Instant,
) -> Result<(), AnalyzerExecutionError> {
    while !readers.iter().all(JoinHandle::is_finished) {
        let now = Instant::now();
        if now >= deadline {
            return Err(AnalyzerExecutionError::ReaderCleanupTimeout {
                invocation: invocation.to_owned(),
                timeout: ANALYZER_CLEANUP_TIMEOUT,
            });
        }
        thread::sleep(deadline.saturating_duration_since(now).min(Duration::from_millis(2)));
    }
    for reader in readers {
        reader.join().map_err(|_| AnalyzerExecutionError::PipeReaderPanicked {
            invocation: invocation.to_owned(),
        })?;
    }
    Ok(())
}

fn combine_analyzer_cleanup(
    first: Result<(), AnalyzerExecutionError>,
    second: Result<(), AnalyzerExecutionError>,
) -> Result<(), AnalyzerExecutionError> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(AnalyzerExecutionError::MultipleCleanup {
            first: Box::new(first),
            second: Box::new(second),
        }),
    }
}

pub(super) fn with_analyzer_cleanup(
    operation: AnalyzerExecutionError,
    cleanup: Result<(), AnalyzerExecutionError>,
) -> AnalyzerExecutionError {
    match cleanup {
        Ok(()) => operation,
        Err(cleanup) => AnalyzerExecutionError::OperationCleanup {
            operation: Box::new(operation),
            cleanup: Box::new(cleanup),
        },
    }
}

fn terminate_analyzer_boundary(
    containment: AnalyzerContainment,
    child_pid: Pid,
    child: &mut Child,
    status: Option<ExitStatus>,
    deadline: Instant,
) -> std::io::Result<()> {
    let mut errors = Vec::new();
    let target = analyzer_descendant_signal_target(containment, child_pid);
    if matches!(containment, AnalyzerContainment::PidNamespace) && getpid().as_raw() != 1 {
        errors.push("refusing namespace-wide analyzer cleanup outside PID 1".to_owned());
    } else if let Err(error) = kill(target, Signal::SIGKILL)
        && error != Errno::ESRCH
    {
        errors.push(format!("signal analyzer boundary: {error}"));
    }

    // Reap the direct child through Child first. Never use blocking wait here:
    // SIGKILL can be delayed by an uninterruptible kernel wait, and analyzer
    // finalization must remain bounded even in that case.
    let mut direct_status = status;
    if direct_status.is_none() {
        match child.try_wait() {
            Ok(Some(exit_status)) => direct_status = Some(exit_status),
            Ok(None) => {
                if let Err(source) = child.kill() {
                    errors.push(format!("signal direct analyzer child: {source}"));
                }
            }
            Err(source) => errors.push(format!("inspect direct analyzer child: {source}")),
        }
    }
    while direct_status.is_none() {
        match child.try_wait() {
            Ok(Some(exit_status)) => direct_status = Some(exit_status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    errors.push("timed out reaping direct analyzer child".to_owned());
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }
            Err(source) => {
                errors.push(format!("reap direct analyzer child: {source}"));
                break;
            }
        }
    }

    if matches!(containment, AnalyzerContainment::PidNamespace) {
        loop {
            if Instant::now() >= deadline {
                errors.push("timed out reaping analyzer descendants".to_owned());
                break;
            }
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => {}
                Ok(
                    WaitStatus::Stopped(..)
                    | WaitStatus::PtraceEvent(..)
                    | WaitStatus::PtraceSyscall(..)
                    | WaitStatus::Continued(..)
                    | WaitStatus::StillAlive,
                ) => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(Errno::EINTR) => {}
                Err(Errno::ECHILD) => break,
                Err(error) => {
                    errors.push(format!("reap analyzer descendants: {error}"));
                    break;
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::other(errors.join("; ")))
    }
}

pub(super) fn analyzer_descendant_signal_target(containment: AnalyzerContainment, _child_pid: Pid) -> Pid {
    match containment {
        AnalyzerContainment::PidNamespace => Pid::from_raw(-1),
        #[cfg(test)]
        AnalyzerContainment::ProcessGroup => Pid::from_raw(-_child_pid.as_raw()),
    }
}

#[derive(Debug, Error)]
#[error("analyzer command {invocation} failed with {status}: {stderr}")]
struct AnalyzerCommandError {
    invocation: String,
    status: ExitStatus,
    stderr: String,
}

#[derive(Debug, Error)]
pub(super) enum AnalyzerExecutionError {
    #[error("refusing to start analyzer command {invocation} outside the required PID-1 namespace boundary")]
    Containment { invocation: String },
    #[error("failed to start analyzer command {invocation}: {source}")]
    Spawn {
        invocation: String,
        #[source]
        source: std::io::Error,
    },
    #[error("analyzer command {invocation} exceeded its {timeout:?} wall timeout")]
    Timeout { invocation: String, timeout: Duration },
    #[error("analyzer command {invocation} exceeded its {pipe} limit of {limit} bytes")]
    OutputLimit {
        invocation: String,
        pipe: AnalyzerPipe,
        limit: usize,
    },
    #[error("failed to read {pipe} from analyzer command {invocation}: {source}")]
    PipeRead {
        invocation: String,
        pipe: AnalyzerPipe,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to monitor analyzer command {invocation}: {source}")]
    Monitor {
        invocation: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to terminate and reap analyzer command {invocation}: {source}")]
    Cleanup {
        invocation: String,
        #[source]
        source: std::io::Error,
    },
    #[error("analyzer command {invocation} reported {pipe} more than once")]
    DuplicatePipeResult { invocation: String, pipe: AnalyzerPipe },
    #[error("analyzer command {invocation} closed its pipe result channel early")]
    PipeChannelClosed { invocation: String },
    #[error("analyzer command {invocation} pipe reader panicked")]
    PipeReaderPanicked { invocation: String },
    #[error("failed to start analyzer command {invocation} {pipe} reader: {source}")]
    PipeReaderSpawn {
        invocation: String,
        pipe: AnalyzerPipe,
        #[source]
        source: std::io::Error,
    },
    #[error("analyzer command {invocation} pipe readers did not stop within the {timeout:?} cleanup timeout")]
    ReaderCleanupTimeout { invocation: String, timeout: Duration },
    #[error("multiple analyzer cleanup steps failed: {first}; {second}")]
    MultipleCleanup {
        first: Box<AnalyzerExecutionError>,
        second: Box<AnalyzerExecutionError>,
    },
    #[error("{operation}; analyzer cleanup also failed: {cleanup}")]
    OperationCleanup {
        operation: Box<AnalyzerExecutionError>,
        cleanup: Box<AnalyzerExecutionError>,
    },
}
