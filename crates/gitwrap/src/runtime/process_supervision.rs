/// Runs Git with bounded pipes and a finite process-group deadline.
async fn run_git<I, S>(args: I, limits: Limits) -> Result<std::process::Output, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = git_command(limits);
    command.args(args);
    run_command(command, limits, None, None::<fn(FetchProgress)>).await
}

async fn run_git_monitored<I, S, F>(
    args: I,
    limits: Limits,
    repository: &Path,
    callback: Option<F>,
) -> Result<std::process::Output, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
    F: Fn(FetchProgress),
{
    let mut command = git_command(limits);
    command.args(args);
    run_command(
        command,
        limits,
        Some(MonitoredRepository::Path(repository.to_owned())),
        callback,
    )
    .await
}

async fn run_git_in_directory<I, S, F>(
    args: I,
    limits: Limits,
    directory: &fs::File,
    monitored_repository: Option<MonitoredRepository>,
    callback: Option<F>,
) -> Result<std::process::Output, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
    F: Fn(FetchProgress),
{
    let mut command = git_command(limits);
    command.args(args);
    set_command_directory(&mut command, directory);
    run_command(command, limits, monitored_repository, callback).await
}

enum MonitoredRepository {
    Path(PathBuf),
    Directory(fs::File),
}

impl MonitoredRepository {
    fn directory(directory: &fs::File) -> Result<Self, Error> {
        Ok(Self::Directory(directory.try_clone().map_err(InnerError::from)?))
    }

    fn scanner(&self, limits: Limits) -> Result<RepositoryUsageScanner, Error> {
        match self {
            Self::Path(path) => RepositoryUsageScanner::new(path, limits, ScanMode::Live),
            Self::Directory(directory) => RepositoryUsageScanner::from_directory(
                directory.try_clone().map_err(InnerError::from)?,
                limits,
                ScanMode::Live,
            ),
        }
    }

    fn verify(&self, limits: Limits) -> Result<RepositoryUsage, Error> {
        match self {
            Self::Path(path) => verify_repository_usage_or_absent_for_creation(path, limits),
            Self::Directory(directory) => verify_repository_usage_directory(directory, limits),
        }
    }
}

async fn run_command<F>(
    mut command: process::Command,
    limits: Limits,
    monitored_repository: Option<MonitoredRepository>,
    callback: Option<F>,
) -> Result<std::process::Output, Error>
where
    F: Fn(FetchProgress),
{
    let limits = limits.validate()?;
    if let Some(repository) = monitored_repository.as_ref() {
        repository.verify(limits)?;
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let started = Instant::now();
    let deadline = started
        .checked_add(limits.wall_timeout)
        .ok_or(InnerError::InvalidLimits)?;
    let mut child = command.spawn().map_err(InnerError::from)?;
    let process_group = child
        .id()
        .ok_or_else(|| InnerError::Io(std_io::Error::other("spawned Git process has no process identifier")))?
        as i32;
    let mut process_group_guard = ProcessGroupGuard::new(process_group);
    let stdout = child.stdout.take().expect("piped Git stdout");
    let stderr = child.stderr.take().expect("piped Git stderr");
    let mut stdout_reader = Box::pin(read_bounded(stdout, "stdout", limits.stdout_bytes));
    let mut stderr_reader = Box::pin(read_stderr(stderr, limits, callback));
    // Run quota scans in this supervisor instead of detaching `spawn_blocking`
    // work which could accumulate after repeated cancellations. Each bounded
    // scan slice returns to `select!`, allowing stdout/stderr draining and
    // child-status handling to make progress between filesystem operations.
    let mut quota_tick = Box::pin(sleep(limits.quota_poll_interval));
    let mut quota_scanner = None;
    let mut status = None;
    let mut stdout = None;
    let mut stderr_done = false;
    let mut boundary_terminated = false;

    loop {
        if status.is_some() && stdout.is_some() && stderr_done {
            break;
        }
        tokio::select! {
            result = &mut stdout_reader, if stdout.is_none() => match result {
                Ok(bytes) => stdout = Some(bytes),
                Err(error) => return abort_with(&mut child, &mut process_group_guard, status.is_some(), boundary_terminated, limits, error).await,
            },
            result = &mut stderr_reader, if !stderr_done => match result {
                Ok(()) => stderr_done = true,
                Err(error) => return abort_with(&mut child, &mut process_group_guard, status.is_some(), boundary_terminated, limits, error).await,
            },
            result = child.wait(), if status.is_none() => match result {
                Ok(found) => {
                    status = Some(found);
                    // Git is the process-group leader. Once it exits, no
                    // transport/helper is allowed to keep the boundary or a
                    // captured pipe alive until the outer wall deadline.
                    terminate_boundary(
                        &mut child,
                        process_group,
                        true,
                        limits.termination_timeout,
                    )
                    .await?;
                    boundary_terminated = true;
                    process_group_guard.disarm();
                }
                Err(source) => {
                    let error = InnerError::Io(source).into();
                    return abort_with(&mut child, &mut process_group_guard, false, false, limits, error).await;
                }
            },
            () = &mut quota_tick, if monitored_repository.is_some() && status.is_none() => {
                let repository = monitored_repository.as_ref().expect("guarded repository root");
                if quota_scanner.is_none() {
                    match repository.scanner(limits) {
                        Ok(scanner) => quota_scanner = Some(scanner),
                        Err(error) => {
                            return abort_with(&mut child, &mut process_group_guard, status.is_some(), boundary_terminated, limits, error).await;
                        }
                    }
                }
                let complete = match quota_scanner
                    .as_mut()
                    .expect("initialized quota scanner")
                    .advance(512, Some(deadline))
                {
                    Ok(complete) => complete,
                    Err(error) => {
                        return abort_with(&mut child, &mut process_group_guard, status.is_some(), boundary_terminated, limits, error).await;
                    }
                };
                if complete {
                    quota_scanner = None;
                    quota_tick.as_mut().reset(Instant::now() + limits.quota_poll_interval);
                } else {
                    quota_tick.as_mut().reset(Instant::now());
                }
            },
            () = sleep_until(deadline) => {
                let error = InnerError::Timeout { timeout: limits.wall_timeout }.into();
                return abort_with(&mut child, &mut process_group_guard, status.is_some(), boundary_terminated, limits, error).await;
            }
        }
    }

    // Git transports belong to the same private process group. Kill any
    // descendant which survived the direct process even on a nominally
    // successful exit, then prove the group disappeared.
    if !boundary_terminated {
        terminate_boundary(&mut child, process_group, true, limits.termination_timeout).await?;
        process_group_guard.disarm();
    }
    let status = status.expect("completed Git status");
    if status.success() {
        Ok(std::process::Output {
            status,
            stdout: stdout.expect("completed Git stdout"),
            // Diagnostics are consumed under a byte ceiling but never exposed:
            // transports may repeat credential-bearing URLs.
            stderr: Vec::new(),
        })
    } else {
        Err(InnerError::Run { code: status.code() }.into())
    }
}

/// Cancellation safety for callers which drop the async operation before its
/// internal deadline resolves. Normal error paths additionally await direct
/// child reaping and group disappearance.
struct ProcessGroupGuard {
    process_group: i32,
    armed: bool,
}

impl ProcessGroupGuard {
    fn new(process_group: i32) -> Self {
        Self {
            process_group,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if self.armed {
            unsafe {
                nix::libc::kill(-self.process_group, nix::libc::SIGKILL);
            }
        }
    }
}

async fn abort_with<T>(
    child: &mut process::Child,
    process_group: &mut ProcessGroupGuard,
    already_reaped: bool,
    boundary_terminated: bool,
    limits: Limits,
    error: Error,
) -> Result<T, Error> {
    if !boundary_terminated {
        terminate_boundary(
            child,
            process_group.process_group,
            already_reaped,
            limits.termination_timeout,
        )
        .await?;
        process_group.disarm();
    }
    Err(error)
}

async fn terminate_boundary(
    child: &mut process::Child,
    process_group: i32,
    already_reaped: bool,
    termination_timeout: Duration,
) -> Result<(), Error> {
    signal_process_group(process_group, nix::libc::SIGKILL)?;
    let deadline = Instant::now()
        .checked_add(termination_timeout)
        .ok_or(InnerError::InvalidLimits)?;
    if !already_reaped {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match timeout(remaining, child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(source)) => return Err(InnerError::Io(source).into()),
            Err(_) => {
                return Err(InnerError::BoundaryTermination {
                    timeout: termination_timeout,
                }
                .into())
            }
        }
    }

    loop {
        if !process_group_exists(process_group)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(InnerError::BoundaryTermination {
                timeout: termination_timeout,
            }
            .into());
        }
        sleep(Duration::from_millis(5)).await;
    }
}

fn signal_process_group(process_group: i32, signal: i32) -> Result<(), Error> {
    let result = unsafe { nix::libc::kill(-process_group, signal) };
    if result == 0 {
        Ok(())
    } else {
        let error = std_io::Error::last_os_error();
        if error.raw_os_error() == Some(nix::libc::ESRCH) {
            Ok(())
        } else {
            Err(InnerError::Io(error).into())
        }
    }
}

fn process_group_exists(process_group: i32) -> Result<bool, Error> {
    let result = unsafe { nix::libc::kill(-process_group, 0) };
    if result == 0 {
        Ok(true)
    } else {
        let error = std_io::Error::last_os_error();
        if error.raw_os_error() == Some(nix::libc::ESRCH) {
            Ok(false)
        } else {
            Err(InnerError::Io(error).into())
        }
    }
}

async fn read_bounded<R>(mut reader: R, stream: &'static str, limit: usize) -> Result<Vec<u8>, Error>
where
    R: io::AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader.read(&mut chunk).await.map_err(InnerError::from)?;
        if count == 0 {
            return Ok(bytes);
        }
        if count > limit.saturating_sub(bytes.len()) {
            return Err(InnerError::OutputLimit { stream, limit }.into());
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

async fn read_stderr<R, F>(reader: R, limits: Limits, callback: Option<F>) -> Result<(), Error>
where
    R: io::AsyncRead + Unpin,
    F: Fn(FetchProgress),
{
    if let Some(callback) = callback {
        ProgressParser::new(reader, limits.stderr_bytes, limits.progress_segment_bytes)
            .parse(callback)
            .await
    } else {
        read_bounded(reader, "stderr", limits.stderr_bytes).await.map(|_| ())
    }
}

