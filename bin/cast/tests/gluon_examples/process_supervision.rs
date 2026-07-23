use std::{
    collections::TryReserveError,
    fmt, fs,
    io::{self, Read, Write as _},
    mem::MaybeUninit,
    os::{fd::AsRawFd, unix::process::CommandExt as _},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(2);
const MAX_READS_PER_TICK: usize = 64;
const READ_BUFFER_BYTES: usize = 8 * 1024;
const HELPER_MODE: &str = "CAST_GLUON_EXAMPLE_SUPERVISOR_HELPER";
const HELPER_DESCENDANT_PID_FILE: &str = "CAST_GLUON_EXAMPLE_SUPERVISOR_DESCENDANT_PID_FILE";
static HELPER_TERMINATE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
pub(super) struct Limits {
    wall_timeout: Duration,
    termination_timeout: Duration,
    stdout_bytes: usize,
    stderr_bytes: usize,
}

impl Limits {
    pub(super) const fn cast_child() -> Self {
        Self {
            wall_timeout: Duration::from_secs(15),
            termination_timeout: Duration::from_secs(2),
            stdout_bytes: 4 * 1024 * 1024,
            stderr_bytes: 1024 * 1024,
        }
    }

    const fn regression(stdout_bytes: usize, stderr_bytes: usize) -> Self {
        Self {
            wall_timeout: Duration::from_millis(150),
            termination_timeout: Duration::from_secs(2),
            stdout_bytes,
            stderr_bytes,
        }
    }

    fn deadline(self, started: Instant) -> Result<Instant, Error> {
        if self.wall_timeout.is_zero()
            || self.termination_timeout.is_zero()
            || self.stdout_bytes == 0
            || self.stderr_bytes == 0
        {
            return Err(Error::InvalidLimits);
        }
        started.checked_add(self.wall_timeout).ok_or(Error::InvalidLimits)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stream {
    Stdout,
    Stderr,
}

impl fmt::Display for Stream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdout => formatter.write_str("stdout"),
            Self::Stderr => formatter.write_str("stderr"),
        }
    }
}

#[derive(Debug)]
pub(super) enum Error {
    InvalidLimits,
    Spawn(io::Error),
    MissingPipe(Stream),
    ConfigurePipe {
        stream: Stream,
        source: io::Error,
    },
    ReserveOutput {
        process_group: i32,
        stream: Stream,
        limit: usize,
        source: TryReserveError,
    },
    InspectChild {
        process_group: i32,
        source: io::Error,
    },
    ReadOutput {
        process_group: i32,
        stream: Stream,
        source: io::Error,
    },
    OutputLimit {
        process_group: i32,
        stream: Stream,
        limit: usize,
    },
    TimedOut {
        process_group: i32,
        timeout: Duration,
    },
    DescendantsSurvived {
        process_group: i32,
    },
    Cleanup {
        primary: Box<Error>,
        detail: String,
    },
}

impl Error {
    #[cfg(test)]
    fn process_group(&self) -> Option<i32> {
        match self {
            Self::InvalidLimits | Self::Spawn(_) | Self::MissingPipe(_) | Self::ConfigurePipe { .. } => None,
            Self::ReserveOutput { process_group, .. }
            | Self::InspectChild { process_group, .. }
            | Self::ReadOutput { process_group, .. }
            | Self::OutputLimit { process_group, .. }
            | Self::TimedOut { process_group, .. }
            | Self::DescendantsSurvived { process_group } => Some(*process_group),
            Self::Cleanup { primary, .. } => primary.process_group(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => formatter.write_str("Cast child supervision limits are invalid"),
            Self::Spawn(source) => write!(formatter, "spawn Cast child: {source}"),
            Self::MissingPipe(stream) => write!(formatter, "Cast child has no piped {stream}"),
            Self::ConfigurePipe { stream, source } => {
                write!(formatter, "make Cast child {stream} nonblocking: {source}")
            }
            Self::ReserveOutput {
                process_group,
                stream,
                limit,
                source,
            } => write!(
                formatter,
                "reserve the fixed {limit}-byte {stream} capture for Cast child process group {process_group}: {source}"
            ),
            Self::InspectChild { process_group, source } => {
                write!(formatter, "inspect Cast child process group {process_group}: {source}")
            }
            Self::ReadOutput {
                process_group,
                stream,
                source,
            } => write!(
                formatter,
                "read bounded {stream} from Cast child process group {process_group}: {source}"
            ),
            Self::OutputLimit {
                process_group,
                stream,
                limit,
            } => write!(
                formatter,
                "Cast child process group {process_group} exceeded its {limit}-byte {stream} limit"
            ),
            Self::TimedOut { process_group, timeout } => write!(
                formatter,
                "Cast child process group {process_group} exceeded its {timeout:?} wall deadline"
            ),
            Self::DescendantsSurvived { process_group } => write!(
                formatter,
                "Cast child leader exited while process group {process_group} still had descendants"
            ),
            Self::Cleanup { primary, detail } => {
                write!(formatter, "{primary}; process-group cleanup also failed: {detail}")
            }
        }
    }
}

impl std::error::Error for Error {}

pub(super) fn output(command: &mut Command, limits: Limits) -> Result<Output, Error> {
    let started = Instant::now();
    let deadline = limits.deadline(started)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);

    let child = command.spawn().map_err(Error::Spawn)?;
    let process_group = i32::try_from(child.id()).map_err(|_| Error::InvalidLimits)?;
    let mut child = ChildGroup::new(child, process_group, limits.termination_timeout);

    let stdout = match child.child_mut().stdout.take() {
        Some(stdout) => stdout,
        None => return finish_error(child, Error::MissingPipe(Stream::Stdout)),
    };
    let stderr = match child.child_mut().stderr.take() {
        Some(stderr) => stderr,
        None => return finish_error(child, Error::MissingPipe(Stream::Stderr)),
    };
    let stdout = match Capture::new(stdout, child.process_group, Stream::Stdout, limits.stdout_bytes) {
        Ok(capture) => capture,
        Err(error) => return finish_error(child, error),
    };
    let stderr = match Capture::new(stderr, child.process_group, Stream::Stderr, limits.stderr_bytes) {
        Ok(capture) => capture,
        Err(error) => return finish_error(child, error),
    };

    match monitor(&mut child, stdout, stderr, limits, deadline) {
        Ok(completed) => Ok(Output {
            status: completed.status,
            stdout: completed.stdout,
            stderr: completed.stderr,
        }),
        Err(error) => finish_error(child, error),
    }
}

fn finish_error(mut child: ChildGroup, primary: Error) -> Result<Output, Error> {
    match child.terminate() {
        Ok(()) => Err(primary),
        Err(source) => Err(Error::Cleanup {
            primary: Box::new(primary),
            detail: source.to_string(),
        }),
    }
}

struct Completed {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn monitor(
    child: &mut ChildGroup,
    mut stdout: Capture<std::process::ChildStdout>,
    mut stderr: Capture<std::process::ChildStderr>,
    limits: Limits,
    deadline: Instant,
) -> Result<Completed, Error> {
    let mut status = None;
    loop {
        require_deadline(deadline, child.process_group, limits.wall_timeout)?;
        stdout.drain(child.process_group, deadline, limits.wall_timeout)?;
        stderr.drain(child.process_group, deadline, limits.wall_timeout)?;

        if status.is_none()
            && child.exit_observed().map_err(|source| Error::InspectChild {
                process_group: child.process_group,
                source,
            })?
        {
            let (exit_status, descendants_survived) =
                child.finish_exited_group().map_err(|source| Error::InspectChild {
                    process_group: child.process_group,
                    source,
                })?;
            if descendants_survived {
                return Err(Error::DescendantsSurvived {
                    process_group: child.process_group,
                });
            }
            status = Some(exit_status);
        }

        require_deadline(deadline, child.process_group, limits.wall_timeout)?;
        if let Some(status) = status {
            if stdout.eof && stderr.eof {
                require_deadline(deadline, child.process_group, limits.wall_timeout)?;
                return Ok(Completed {
                    status,
                    stdout: stdout.bytes,
                    stderr: stderr.bytes,
                });
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(POLL_INTERVAL.min(remaining));
    }
}

fn require_deadline(deadline: Instant, process_group: i32, timeout: Duration) -> Result<(), Error> {
    if Instant::now() > deadline {
        Err(Error::TimedOut { process_group, timeout })
    } else {
        Ok(())
    }
}

struct Capture<Reader> {
    reader: Reader,
    stream: Stream,
    limit: usize,
    bytes: Vec<u8>,
    eof: bool,
}

impl<Reader: Read + AsRawFd> Capture<Reader> {
    fn new(reader: Reader, process_group: i32, stream: Stream, limit: usize) -> Result<Self, Error> {
        set_nonblocking(reader.as_raw_fd()).map_err(|source| Error::ConfigurePipe { stream, source })?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(limit).map_err(|source| Error::ReserveOutput {
            process_group,
            stream,
            limit,
            source,
        })?;
        Ok(Self {
            reader,
            stream,
            limit,
            bytes,
            eof: false,
        })
    }

    fn drain(&mut self, process_group: i32, deadline: Instant, timeout: Duration) -> Result<(), Error> {
        if self.eof {
            return Ok(());
        }
        let mut buffer = [0u8; READ_BUFFER_BYTES];
        for _ in 0..MAX_READS_PER_TICK {
            require_deadline(deadline, process_group, timeout)?;
            match self.reader.read(&mut buffer) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(());
                }
                Ok(length) => {
                    let next = self.bytes.len().checked_add(length).ok_or(Error::OutputLimit {
                        process_group,
                        stream: self.stream,
                        limit: self.limit,
                    })?;
                    if next > self.limit {
                        return Err(Error::OutputLimit {
                            process_group,
                            stream: self.stream,
                            limit: self.limit,
                        });
                    }
                    self.bytes.extend_from_slice(&buffer[..length]);
                }
                Err(source) if source.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
                Err(source) => {
                    return Err(Error::ReadOutput {
                        process_group,
                        stream: self.stream,
                        source,
                    });
                }
            }
        }
        Ok(())
    }
}

fn set_nonblocking(descriptor: i32) -> io::Result<()> {
    // SAFETY: `descriptor` is a live pipe descriptor and both fcntl commands
    // use the documented integer argument/return forms.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the same live pipe is updated without changing existing flags.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct ChildGroup {
    child: Option<Child>,
    process_group: i32,
    termination_timeout: Duration,
    cleanup_deadline: Option<Instant>,
}

impl ChildGroup {
    fn new(child: Child, process_group: i32, termination_timeout: Duration) -> Self {
        Self {
            child: Some(child),
            process_group,
            termination_timeout,
            cleanup_deadline: None,
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("armed child group retains its leader")
    }

    fn exit_observed(&self) -> io::Result<bool> {
        let child = self.child.as_ref().expect("armed child group retains its leader");
        let mut info = MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: `info` points to writable siginfo storage. WNOWAIT leaves
        // the exited leader unreaped so its PID continues to pin the numeric
        // process-group identifier until every group operation is complete.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            let source = io::Error::last_os_error();
            return if source.kind() == io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(source)
            };
        }
        // SAFETY: successful waitid initialized the siginfo object.
        Ok(unsafe { info.assume_init().si_pid() } != 0)
    }

    fn cleanup_deadline(&mut self) -> io::Result<Instant> {
        if let Some(deadline) = self.cleanup_deadline {
            return Ok(deadline);
        }
        let deadline = Instant::now()
            .checked_add(self.termination_timeout)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cleanup deadline overflowed"))?;
        self.cleanup_deadline = Some(deadline);
        Ok(deadline)
    }

    fn finish_exited_group(&mut self) -> io::Result<(ExitStatus, bool)> {
        let cleanup_deadline = self.cleanup_deadline()?;
        let members = process_group_members_until(self.process_group, cleanup_deadline)?;
        let leader = self.child_mut().id() as i32;
        let descendants_survived = members.iter().any(|(process, _)| *process != leader);
        self.terminate_group()?;
        let status = self.reap_and_disarm()?;
        Ok((status, descendants_survived))
    }

    fn terminate(&mut self) -> io::Result<()> {
        if self.child.is_none() {
            return Ok(());
        }
        self.terminate_group()?;
        let _ = self.reap_and_disarm()?;
        Ok(())
    }

    fn terminate_group(&mut self) -> io::Result<()> {
        let cleanup_deadline = self.cleanup_deadline()?;
        signal_process_group(self.process_group, libc::SIGTERM)?;
        let soft_deadline = Instant::now()
            .checked_add(self.termination_timeout / 4)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "termination deadline overflowed"))?
            .min(cleanup_deadline);
        if self.wait_until_only_zombie_leader(soft_deadline, cleanup_deadline)? {
            return Ok(());
        }

        signal_process_group(self.process_group, libc::SIGKILL)?;
        if !self.wait_until_only_zombie_leader(cleanup_deadline, cleanup_deadline)? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("process group {} did not terminate and reap", self.process_group),
            ));
        }
        Ok(())
    }

    fn wait_until_only_zombie_leader(&self, phase_deadline: Instant, cleanup_deadline: Instant) -> io::Result<bool> {
        let leader = self.child.as_ref().expect("armed child group retains its leader").id() as i32;
        let mut leader_only_observed = false;
        loop {
            let leader_exited = self.exit_observed()?;
            let members = process_group_members_until(self.process_group, cleanup_deadline)?;
            let only_zombie_leader = leader_exited && members.as_slice() == [(leader, 'Z')];
            if only_zombie_leader && leader_only_observed {
                require_cleanup_deadline(cleanup_deadline, self.process_group)?;
                return Ok(true);
            }
            leader_only_observed = only_zombie_leader;
            if Instant::now() >= phase_deadline {
                return Ok(false);
            }
            thread::sleep(POLL_INTERVAL.min(phase_deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn reap_and_disarm(&mut self) -> io::Result<ExitStatus> {
        let status = self
            .child_mut()
            .try_wait()?
            .ok_or_else(|| io::Error::other("waitid observed exit but Child::try_wait did not reap it"))?;
        // No numeric process-group operation is permitted after this point.
        self.child.take();
        Ok(status)
    }
}

impl Drop for ChildGroup {
    fn drop(&mut self) {
        if self.child.is_none() {
            return;
        }
        let _ = signal_process_group(self.process_group, libc::SIGKILL);
        let deadline = self.cleanup_deadline().unwrap_or_else(|_| Instant::now());
        if self.wait_until_only_zombie_leader(deadline, deadline).unwrap_or(false) {
            let _ = self.reap_and_disarm();
        }
    }
}

fn signal_process_group(process_group: i32, signal: i32) -> io::Result<()> {
    // SAFETY: a negative PID targets exactly the dedicated process group.
    if unsafe { libc::kill(-process_group, signal) } == 0 {
        return Ok(());
    }
    let source = io::Error::last_os_error();
    if source.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(source)
    }
}

fn process_group_members(process_group: i32) -> io::Result<Vec<(i32, char)>> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "process-group scan deadline overflowed"))?;
    process_group_members_until(process_group, deadline)
}

fn process_group_members_until(process_group: i32, deadline: Instant) -> io::Result<Vec<(i32, char)>> {
    let mut members = Vec::new();
    for entry in fs::read_dir("/proc")? {
        require_cleanup_deadline_with_members(deadline, process_group, &members)?;
        let entry = entry?;
        let Some(process) = entry.file_name().to_str().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        let stat = match fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(source),
        };
        let fields = stat
            .rsplit_once(')')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed /proc process stat"))?
            .1;
        let mut fields = fields.split_ascii_whitespace();
        let state = fields
            .next()
            .and_then(|field| field.chars().next())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing /proc process state"))?;
        let _parent = fields.next();
        let group = fields
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing /proc process group"))?
            .parse::<i32>()
            .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?;
        if group == process_group {
            members.push((process, state));
        }
    }
    members.sort_unstable();
    require_cleanup_deadline_with_members(deadline, process_group, &members)?;
    Ok(members)
}

fn require_cleanup_deadline_with_members(
    deadline: Instant,
    process_group: i32,
    members: &[(i32, char)],
) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "inspection of process group {process_group} exceeded its absolute cleanup deadline; observed members {members:?}"
            ),
        ))
    } else {
        Ok(())
    }
}

fn require_cleanup_deadline(deadline: Instant, process_group: i32) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("inspection of process group {process_group} exceeded its absolute cleanup deadline"),
        ))
    } else {
        Ok(())
    }
}

fn process_exists(process: i32) -> io::Result<bool> {
    // SAFETY: signal zero only observes the test-owned process identifier.
    if unsafe { libc::kill(process, 0) } == 0 {
        return Ok(true);
    }
    let source = io::Error::last_os_error();
    match source.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(source),
    }
}

fn helper_command(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("resolve test-owned helper executable"));
    command
        .arg("--exact")
        .arg("process_supervision::cast_child_supervisor_helper")
        .arg("--nocapture")
        .env(HELPER_MODE, mode);
    command
}

extern "C" fn mark_helper_termination(_signal: libc::c_int) {
    HELPER_TERMINATE.store(true, Ordering::SeqCst);
}

fn install_helper_termination_handler() {
    HELPER_TERMINATE.store(false, Ordering::SeqCst);
    // SAFETY: zero is a valid initial representation for `sigaction`; the
    // mask and handler are initialized before installation for this isolated
    // test-owned process.
    let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
    action.sa_sigaction = mark_helper_termination as *const () as libc::sighandler_t;
    // SAFETY: both pointers refer to live `sigaction` storage, and SIGTERM is
    // installed only inside the disposable helper process.
    unsafe {
        assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0);
        assert_eq!(libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()), 0);
    }
}

fn run_descendant_tree_helper() {
    install_helper_termination_handler();
    let pid_file = std::env::var_os(HELPER_DESCENDANT_PID_FILE).expect("descendant helper PID file is configured");
    let mut descendant = helper_command("descendant-hang")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn test-owned descendant in the inherited process group");
    fs::write(&pid_file, descendant.id().to_string()).expect("record test-owned descendant PID");

    let mut termination_deadline = None;
    loop {
        if let Some(status) = descendant.try_wait().expect("inspect test-owned descendant") {
            assert!(
                HELPER_TERMINATE.load(Ordering::SeqCst),
                "descendant exited before the supervisor signalled its process group: {status}"
            );
            return;
        }
        if HELPER_TERMINATE.load(Ordering::SeqCst) {
            let deadline = *termination_deadline.get_or_insert_with(|| Instant::now() + Duration::from_millis(250));
            assert!(
                Instant::now() <= deadline,
                "group-signalled descendant was not reaped by its helper parent"
            );
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn spawn_orphan_descendant_helper() {
    let pid_file = std::env::var_os(HELPER_DESCENDANT_PID_FILE).expect("descendant helper PID file is configured");
    let descendant = helper_command("descendant-hang")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn test-owned descendant in the inherited process group");
    fs::write(&pid_file, descendant.id().to_string()).expect("record test-owned orphan descendant PID");
    drop(descendant);
}

fn assert_group_absent(error: &Error) {
    let process_group = error
        .process_group()
        .expect("supervision error retains its process group");
    assert!(
        process_group_members(process_group).unwrap().is_empty(),
        "failed supervision left process group {process_group} alive"
    );
}

#[test]
fn bounded_cast_child_supervisor_times_out_and_reaps_group() {
    let error = output(&mut helper_command("hang"), Limits::regression(64 * 1024, 64 * 1024)).unwrap_err();
    assert!(
        matches!(&error, Error::TimedOut { .. }),
        "unexpected supervision error: {error:?}"
    );
    assert_group_absent(&error);
}

#[test]
fn bounded_cast_child_supervisor_rejects_stdout_overflow_and_reaps_group() {
    const LIMIT: usize = 8 * 1024;
    let error = output(&mut helper_command("stdout-overflow"), Limits::regression(LIMIT, LIMIT)).unwrap_err();
    assert!(matches!(
        &error,
        Error::OutputLimit {
            stream: Stream::Stdout,
            limit: LIMIT,
            ..
        }
    ));
    assert_group_absent(&error);
}

#[test]
fn bounded_cast_child_supervisor_kills_and_reaps_descendant_tree() {
    let directory = tempfile::TempDir::new().expect("create descendant supervision fixture");
    let pid_file = directory.path().join("descendant.pid");
    let mut command = helper_command("descendant-tree");
    command.env(HELPER_DESCENDANT_PID_FILE, &pid_file);
    let limits = Limits {
        wall_timeout: Duration::from_secs(1),
        termination_timeout: Duration::from_secs(2),
        stdout_bytes: 64 * 1024,
        stderr_bytes: 64 * 1024,
    };

    let error = output(&mut command, limits).unwrap_err();
    assert!(matches!(&error, Error::TimedOut { .. }));
    assert_group_absent(&error);

    let descendant = fs::read_to_string(pid_file)
        .expect("read test-owned descendant PID")
        .parse::<i32>()
        .expect("parse test-owned descendant PID");
    assert!(
        !process_exists(descendant).unwrap(),
        "failed supervision left descendant process {descendant} alive or unreaped"
    );
}

#[test]
fn bounded_cast_child_supervisor_rejects_exited_leader_with_descendant() {
    let directory = tempfile::TempDir::new().expect("create orphan-descendant supervision fixture");
    let pid_file = directory.path().join("descendant.pid");
    let mut command = helper_command("leader-exit-descendant");
    command.env(HELPER_DESCENDANT_PID_FILE, &pid_file);

    let error = output(&mut command, Limits::regression(64 * 1024, 64 * 1024)).unwrap_err();
    assert!(matches!(&error, Error::DescendantsSurvived { .. }));
    assert_group_absent(&error);

    let descendant = fs::read_to_string(pid_file)
        .expect("read test-owned orphan descendant PID")
        .parse::<i32>()
        .expect("parse test-owned orphan descendant PID");
    assert!(
        !process_exists(descendant).unwrap(),
        "exited-leader cleanup left descendant process {descendant} alive or unreaped"
    );
}

#[test]
fn bounded_cast_child_supervisor_escalates_ignored_term_to_kill() {
    let error = output(
        &mut helper_command("ignore-term"),
        Limits::regression(64 * 1024, 64 * 1024),
    )
    .unwrap_err();
    assert!(
        matches!(&error, Error::TimedOut { .. }),
        "unexpected supervision error: {error:?}"
    );
    assert_group_absent(&error);
}

#[test]
fn bounded_cast_child_supervisor_reuses_one_cleanup_deadline() {
    let mut command = helper_command("hang");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let child = command.spawn().expect("spawn test-owned cleanup-deadline helper");
    let process_group = i32::try_from(child.id()).expect("test-owned helper PID fits process-group type");
    let mut child = ChildGroup::new(child, process_group, Duration::from_secs(2));

    let first = child.cleanup_deadline().expect("start cleanup deadline");
    thread::sleep(Duration::from_millis(10));
    let second = child.cleanup_deadline().expect("reuse cleanup deadline");
    assert_eq!(first, second, "cleanup phases reset their absolute deadline");

    drop(child);
    assert!(
        process_group_members(process_group).unwrap().is_empty(),
        "cleanup-deadline guard left process group {process_group} alive"
    );
}

#[test]
fn bounded_cast_child_supervisor_drop_kills_and_reaps_group() {
    let mut command = helper_command("hang");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let child = command.spawn().expect("spawn test-owned drop-guard helper");
    let process_group = i32::try_from(child.id()).expect("test-owned helper PID fits process-group type");
    let child = ChildGroup::new(child, process_group, Duration::from_secs(2));

    drop(child);

    assert!(
        process_group_members(process_group).unwrap().is_empty(),
        "dropping supervision guard left process group {process_group} alive"
    );
}

#[test]
fn cast_child_supervisor_helper() {
    let Ok(mode) = std::env::var(HELPER_MODE) else {
        return;
    };
    match mode.as_str() {
        "hang" => loop {
            thread::park_timeout(Duration::from_secs(60));
        },
        "stdout-overflow" => {
            let bytes = [b'x'; READ_BUFFER_BYTES];
            let mut stdout = io::stdout().lock();
            for _ in 0..8 {
                stdout.write_all(&bytes).unwrap();
            }
            stdout.flush().unwrap();
        }
        "descendant-tree" => run_descendant_tree_helper(),
        "leader-exit-descendant" => spawn_orphan_descendant_helper(),
        "ignore-term" => {
            install_helper_termination_handler();
            loop {
                thread::park_timeout(Duration::from_secs(60));
            }
        }
        "descendant-hang" => loop {
            thread::park_timeout(Duration::from_secs(60));
        },
        _ => panic!("unknown supervisor helper mode {mode:?}"),
    }
}
