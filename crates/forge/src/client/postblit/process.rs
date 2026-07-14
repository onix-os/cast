//! Bounded Linux subprocess supervision for post-blit trigger handlers.

use std::{
    fmt,
    io::{self, Read},
    mem::MaybeUninit,
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use nix::errno::Errno;
use thiserror::Error;

const EXECUTION_LIMITS: Limits = Limits {
    wall_timeout: Duration::from_secs(5 * 60),
    stdout_bytes: 1024 * 1024,
    stderr_bytes: 1024 * 1024,
    cleanup_timeout: Duration::from_secs(2),
};
const MONITOR_INTERVAL: Duration = Duration::from_millis(2);

// Minimal classic-BPF policy inherited by the trigger and every descendant.
// The leader establishes its private process group before installation, then
// setpgid(2) and setsid(2) fail with EPERM so descendants cannot escape that
// kill/reap boundary.
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_JMP_JSET_K: u16 = 0x45;
const BPF_RET_K: u16 = 0x06;
const SECCOMP_DATA_NR_OFFSET: u32 = 0;
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;
#[cfg(target_arch = "riscv64")]
const AUDIT_ARCH: u32 = 0xc000_00f3;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_ACTION_EPERM: u32 = SECCOMP_RET_ERRNO | nix::libc::EPERM as u32;
// x32 shares AUDIT_ARCH_X86_64 but ORs this bit into its syscall numbers.
// Reject it before syscall matching so it cannot bypass the escape denials.
const X32_SYSCALL_BIT: u32 = 0x4000_0000;
const SECCOMP_SET_MODE_FILTER: nix::libc::c_uint = 1;
const SECCOMP_MODE_FILTER: nix::libc::c_int = 2;

#[derive(Clone, Copy, Debug)]
struct Limits {
    wall_timeout: Duration,
    stdout_bytes: usize,
    stderr_bytes: usize,
    cleanup_timeout: Duration,
}

pub(super) fn output(command: &mut Command) -> Result<Output, Error> {
    output_with_readers(command, EXECUTION_LIMITS, |stdout, stderr| (stdout, stderr))
}

fn output_with_readers<Stdout, Stderr>(
    command: &mut Command,
    limits: Limits,
    readers: impl FnOnce(ChildStdout, ChildStderr) -> (Stdout, Stderr),
) -> Result<Output, Error>
where
    Stdout: Read,
    Stderr: Read,
{
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The filter is installed after the leader creates its group, then is
    // inherited across exec/fork so no descendant can change group or session.
    unsafe {
        command.pre_exec(|| {
            if nix::libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            install_no_escape_filter()
        });
    }

    let started = Instant::now();
    let mut child = command.spawn().map_err(|source| Error::Spawn { source })?;
    // Linux pid_t is a positive i32, so a successfully spawned child always
    // has the same representable PID and process-group identifier here.
    let process_group = child.id() as i32;
    let stdout = child.stdout.take().expect("piped trigger stdout");
    let stderr = child.stderr.take().expect("piped trigger stderr");

    if let Err(source) = set_nonblocking(stdout.as_raw_fd()) {
        return abort(
            Error::PipeSetup {
                stream: Stream::Stdout,
                source,
            },
            &mut child,
            process_group,
            limits.cleanup_timeout,
        );
    }
    if let Err(source) = set_nonblocking(stderr.as_raw_fd()) {
        return abort(
            Error::PipeSetup {
                stream: Stream::Stderr,
                source,
            },
            &mut child,
            process_group,
            limits.cleanup_timeout,
        );
    }

    let (stdout, stderr) = readers(stdout, stderr);
    supervise(&mut child, process_group, stdout, stderr, started, limits)
}

fn supervise<Stdout: Read, Stderr: Read>(
    child: &mut Child,
    process_group: i32,
    mut stdout_reader: Stdout,
    mut stderr_reader: Stderr,
    started: Instant,
    limits: Limits,
) -> Result<Output, Error> {
    let mut stdout = Capture::new(Stream::Stdout, limits.stdout_bytes);
    let mut stderr = Capture::new(Stream::Stderr, limits.stderr_bytes);
    let mut status = None;

    loop {
        for result in [stdout.drain(&mut stdout_reader), stderr.drain(&mut stderr_reader)] {
            if let Err(failure) = result {
                return if status.is_some() {
                    Err(failure)
                } else {
                    abort(failure, child, process_group, limits.cleanup_timeout)
                };
            }
        }

        if status.is_none() {
            match exit_observed(child.id()) {
                Ok(true) => {
                    status = Some(
                        terminate_and_reap(child, process_group, limits.cleanup_timeout)
                            .map_err(|source| Error::Cleanup { source })?,
                    );
                }
                Ok(false) => {}
                Err(source) => {
                    return abort(Error::Monitor { source }, child, process_group, limits.cleanup_timeout);
                }
            }
        }

        if let Some(status) = status
            && stdout.eof
            && stderr.eof
        {
            return Ok(Output {
                status,
                stdout: stdout.bytes,
                stderr: stderr.bytes,
            });
        }

        let elapsed = started.elapsed();
        if elapsed >= limits.wall_timeout {
            let failure = Error::Timeout {
                limit: limits.wall_timeout,
            };
            return if status.is_some() {
                Err(failure)
            } else {
                abort(failure, child, process_group, limits.cleanup_timeout)
            };
        }
        thread::sleep(MONITOR_INTERVAL.min(limits.wall_timeout.saturating_sub(elapsed)));
    }
}

struct Capture {
    stream: Stream,
    limit: usize,
    bytes: Vec<u8>,
    eof: bool,
}

impl Capture {
    fn new(stream: Stream, limit: usize) -> Self {
        Self {
            stream,
            limit,
            bytes: Vec::with_capacity(limit.min(8192)),
            eof: false,
        }
    }

    fn drain(&mut self, pipe: &mut impl Read) -> Result<(), Error> {
        if self.eof {
            return Ok(());
        }

        let mut buffer = [0_u8; 8192];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(());
                }
                Ok(read) if read > self.limit.saturating_sub(self.bytes.len()) => {
                    return Err(Error::OutputLimit {
                        stream: self.stream,
                        limit: self.limit,
                    });
                }
                Ok(read) => self.bytes.extend_from_slice(&buffer[..read]),
                Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                Err(source) if source.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(source) => {
                    return Err(Error::PipeRead {
                        stream: self.stream,
                        source,
                    });
                }
            }
        }
    }
}

fn set_nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: fd is a live pipe descriptor for both calls and F_SETFL only
    // updates its open-file-description status flags.
    let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn abort<T>(failure: Error, child: &mut Child, process_group: i32, timeout: Duration) -> Result<T, Error> {
    match terminate_and_reap(child, process_group, timeout) {
        Ok(_) => Err(failure),
        Err(source) => Err(Error::CleanupAfterFailure {
            failure: Box::new(failure),
            source,
        }),
    }
}

fn exit_observed(pid: u32) -> io::Result<bool> {
    let mut info = MaybeUninit::<nix::libc::siginfo_t>::zeroed();
    // WNOWAIT leaves the zombie leader in place, pinning both PID and PGID
    // until every descendant has been signaled. Child::try_wait reaps it only
    // after the first group-wide SIGKILL.
    let result = unsafe {
        nix::libc::waitid(
            nix::libc::P_PID,
            pid,
            info.as_mut_ptr(),
            nix::libc::WEXITED | nix::libc::WNOHANG | nix::libc::WNOWAIT,
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
    Ok(unsafe { info.assume_init().si_pid() } != 0)
}

fn terminate_and_reap(child: &mut Child, process_group: i32, timeout: Duration) -> io::Result<ExitStatus> {
    let started = Instant::now();
    let deadline = started.checked_add(timeout).unwrap_or(started);
    let mut detail = None;
    let mut leader_exited = false;
    let mut leader_only_observed = false;

    // Keep the leader unreaped while issuing every numeric-PGID operation.
    // Its zombie pins the PGID, so none of these signals can target a reused
    // group. The inherited seccomp filter prevents descendants escaping the
    // group before a group-wide SIGKILL reaches them.
    loop {
        match signal_group(process_group) {
            Ok(()) => {}
            Err(source) => {
                detail.get_or_insert_with(|| format!("signal process group: {source}"));
            }
        }

        if !leader_exited {
            match exit_observed(child.id()) {
                Ok(found) => leader_exited = found,
                Err(source) => {
                    detail.get_or_insert_with(|| format!("observe direct child: {source}"));
                }
            }
        }

        if leader_exited {
            match process_group_members(process_group) {
                Ok(members) => {
                    let leader = child.id() as i32;
                    let only_zombie_leader = members.as_slice() == [(leader, 'Z')];
                    if only_zombie_leader && leader_only_observed {
                        break;
                    }
                    leader_only_observed = only_zombie_leader;
                }
                Err(source) => {
                    leader_only_observed = false;
                    detail.get_or_insert_with(|| format!("inspect process group: {source}"));
                }
            }
        }

        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "process group {process_group} did not terminate within {timeout:?}{}",
                    detail.map(|detail| format!("; {detail}")).unwrap_or_default()
                ),
            ));
        }
        thread::sleep(MONITOR_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }

    // No numeric PGID operation is permitted after this exact-child reap.
    match child.try_wait()? {
        Some(status) => Ok(status),
        None => Err(io::Error::other(
            "waitid observed trigger termination but Child::try_wait did not reap it",
        )),
    }
}

fn signal_group(process_group: i32) -> io::Result<()> {
    let result = unsafe { nix::libc::kill(-process_group, nix::libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }
    let source = io::Error::last_os_error();
    if source.raw_os_error() == Some(Errno::ESRCH as i32) {
        Ok(())
    } else {
        Err(source)
    }
}

fn process_group_members(process_group: i32) -> io::Result<Vec<(i32, char)>> {
    let mut members = Vec::new();
    for entry in fs_err::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        let stat = match fs_err::read_to_string(entry.path().join("stat")) {
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
            members.push((pid, state));
        }
    }
    members.sort_unstable();
    Ok(members)
}

const fn statement(code: u16, k: u32) -> nix::libc::sock_filter {
    nix::libc::sock_filter { code, jt: 0, jf: 0, k }
}

const fn jump(code: u16, k: u32, jt: u8, jf: u8) -> nix::libc::sock_filter {
    nix::libc::sock_filter { code, jt, jf, k }
}

fn install_no_escape_filter() -> io::Result<()> {
    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")
    )))]
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "trigger process-group containment supports only Linux x86_64, aarch64, and riscv64",
        ));
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")
    ))]
    {
        let mut filter = [
            statement(BPF_LD_W_ABS, SECCOMP_DATA_ARCH_OFFSET),
            jump(BPF_JMP_JEQ_K, AUDIT_ARCH, 1, 0),
            statement(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
            statement(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET),
            jump(BPF_JMP_JSET_K, X32_SYSCALL_BIT, 0, 1),
            statement(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
            jump(BPF_JMP_JEQ_K, nix::libc::SYS_setpgid as u32, 0, 1),
            statement(BPF_RET_K, SECCOMP_ACTION_EPERM),
            jump(BPF_JMP_JEQ_K, nix::libc::SYS_setsid as u32, 0, 1),
            statement(BPF_RET_K, SECCOMP_ACTION_EPERM),
            statement(BPF_RET_K, SECCOMP_RET_ALLOW),
        ];

        let no_new_privs = unsafe {
            nix::libc::prctl(
                nix::libc::PR_SET_NO_NEW_PRIVS,
                1 as nix::libc::c_ulong,
                0 as nix::libc::c_ulong,
                0 as nix::libc::c_ulong,
                0 as nix::libc::c_ulong,
            )
        };
        if no_new_privs == -1 {
            return Err(io::Error::last_os_error());
        }
        let no_new_privs = unsafe {
            nix::libc::prctl(
                nix::libc::PR_GET_NO_NEW_PRIVS,
                0 as nix::libc::c_ulong,
                0 as nix::libc::c_ulong,
                0 as nix::libc::c_ulong,
                0 as nix::libc::c_ulong,
            )
        };
        if no_new_privs != 1 {
            return Err(if no_new_privs == -1 {
                io::Error::last_os_error()
            } else {
                io::Error::other("NO_NEW_PRIVS verification failed")
            });
        }

        let program = nix::libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_mut_ptr(),
        };
        let installed = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_seccomp,
                SECCOMP_SET_MODE_FILTER,
                0 as nix::libc::c_uint,
                std::ptr::from_ref(&program),
            )
        };
        if installed == -1 {
            return Err(io::Error::last_os_error());
        }
        if installed != 0 {
            return Err(io::Error::other(
                "seccomp filter installation returned a nonzero result",
            ));
        }

        let mode = unsafe {
            nix::libc::prctl(
                nix::libc::PR_GET_SECCOMP,
                0 as nix::libc::c_ulong,
                0 as nix::libc::c_ulong,
                0 as nix::libc::c_ulong,
                0 as nix::libc::c_ulong,
            )
        };
        if mode == -1 {
            return Err(io::Error::last_os_error());
        }
        if mode != SECCOMP_MODE_FILTER {
            return Err(io::Error::other("seccomp filter mode verification failed"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl fmt::Display for Stream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to spawn trigger: {source}")]
    Spawn {
        #[source]
        source: io::Error,
    },
    #[error("exceeded the fixed {limit:?} wall timeout")]
    Timeout { limit: Duration },
    #[error("{stream} exceeded the fixed {limit}-byte output limit")]
    OutputLimit { stream: Stream, limit: usize },
    #[error("failed to configure nonblocking {stream}: {source}")]
    PipeSetup {
        stream: Stream,
        #[source]
        source: io::Error,
    },
    #[error("failed to read {stream}: {source}")]
    PipeRead {
        stream: Stream,
        #[source]
        source: io::Error,
    },
    #[error("failed to monitor trigger: {source}")]
    Monitor {
        #[source]
        source: io::Error,
    },
    #[error("failed to terminate and reap the trigger process group: {source}")]
    Cleanup {
        #[source]
        source: io::Error,
    },
    #[error("{failure}; trigger process-group cleanup also failed: {source}")]
    CleanupAfterFailure {
        failure: Box<Error>,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::{path::Path, path::PathBuf};

    use super::*;

    #[test]
    fn output_accepts_each_stream_at_its_exact_byte_limit() {
        let mut command = shell("printf 12345678; printf abcdefgh >&2", &[]);

        let output = output_with_limits(&mut command, limits(8, 8, Duration::from_secs(2))).unwrap();

        assert_eq!(output.stdout, b"12345678");
        assert_eq!(output.stderr, b"abcdefgh");
    }

    #[test]
    fn stdout_rejects_n_plus_one_and_reaps_background_work() {
        let temporary = tempfile::tempdir().unwrap();
        let pid_file = temporary.path().join("descendant.pid");
        let mut command = background_command(&pid_file, "printf 123456789; exec /bin/sleep 30");

        let error = output_with_limits(&mut command, limits(8, 64, Duration::from_secs(2))).unwrap_err();

        assert!(matches!(
            error,
            Error::OutputLimit {
                stream: Stream::Stdout,
                limit: 8,
            }
        ));
        assert_pids_reaped(&read_pids(&pid_file));
    }

    #[test]
    fn stderr_rejects_n_plus_one() {
        let mut command = shell("printf abcdefghi >&2", &[]);

        let error = output_with_limits(&mut command, limits(64, 8, Duration::from_secs(2))).unwrap_err();

        assert!(matches!(
            error,
            Error::OutputLimit {
                stream: Stream::Stderr,
                limit: 8,
            }
        ));
    }

    #[test]
    fn timeout_terminates_and_reaps_background_work() {
        let temporary = tempfile::tempdir().unwrap();
        let pid_file = temporary.path().join("descendant.pid");
        let mut command = background_command(&pid_file, "exec /bin/sleep 30");
        let started = Instant::now();

        let error = output_with_limits(&mut command, limits(64, 64, Duration::from_millis(100))).unwrap_err();

        assert!(matches!(error, Error::Timeout { limit } if limit == Duration::from_millis(100)));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_pids_reaped(&read_pids(&pid_file));
    }

    #[test]
    fn reader_failure_terminates_and_reaps_background_work() {
        struct FailAfterReady<R> {
            reader: R,
            ready: PathBuf,
        }

        impl<R: Read> Read for FailAfterReady<R> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if fs_err::metadata(&self.ready).is_ok_and(|metadata| metadata.len() > 0) {
                    Err(io::Error::other("injected trigger pipe failure"))
                } else {
                    self.reader.read(buffer)
                }
            }
        }

        let temporary = tempfile::tempdir().unwrap();
        let pid_file = temporary.path().join("descendant.pid");
        let mut command = background_command(&pid_file, "exec /bin/sleep 30");
        let ready = pid_file.clone();

        let error = output_with_readers(
            &mut command,
            limits(64, 64, Duration::from_secs(2)),
            move |stdout, stderr| (FailAfterReady { reader: stdout, ready }, stderr),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            Error::PipeRead {
                stream: Stream::Stdout,
                ref source,
            } if source.kind() == io::ErrorKind::Other
                && source.to_string() == "injected trigger pipe failure"
        ));
        assert_pids_reaped(&read_pids(&pid_file));
    }

    #[test]
    fn descendants_cannot_escape_the_private_process_group() {
        let temporary = tempfile::tempdir().unwrap();
        let pid_file = temporary.path().join("escape-probes.pid");
        let setsid_result = temporary.path().join("setsid.result");
        let setpgid_result = temporary.path().join("setpgid.result");
        let executable = std::env::current_exe().unwrap();
        let script = concat!(
            "TRIGGER_ESCAPE_RESULT=\"$3\" \"$2\" --ignored --exact ",
            "client::postblit::process::tests::setsid_escape_probe & setsid_probe=$!; ",
            "TRIGGER_ESCAPE_RESULT=\"$4\" \"$2\" --ignored --exact ",
            "client::postblit::process::tests::setpgid_escape_probe & setpgid_probe=$!; ",
            "printf '%s %s' \"$setsid_probe\" \"$setpgid_probe\" > \"$1\"; ",
            "while [ ! -s \"$3\" ] || [ ! -s \"$4\" ]; do :; done; exec /bin/sleep 30",
        );
        let mut command = shell(
            script,
            &[
                pid_file.to_string_lossy().as_ref(),
                executable.to_string_lossy().as_ref(),
                setsid_result.to_string_lossy().as_ref(),
                setpgid_result.to_string_lossy().as_ref(),
            ],
        );

        let error = output_with_limits(&mut command, limits(4096, 4096, Duration::from_millis(500))).unwrap_err();
        let descendants = read_pids(&pid_file);
        let _cleanup = DescendantCleanup(descendants.clone());

        assert!(matches!(error, Error::Timeout { limit } if limit == Duration::from_millis(500)));
        assert_eq!(
            fs_err::read_to_string(setsid_result).unwrap(),
            nix::libc::EPERM.to_string()
        );
        assert_eq!(
            fs_err::read_to_string(setpgid_result).unwrap(),
            nix::libc::EPERM.to_string()
        );
        assert_pids_reaped(&descendants);
    }

    #[test]
    #[ignore = "subprocess probe for inherited trigger seccomp policy"]
    fn setsid_escape_probe() {
        let result = unsafe { nix::libc::setsid() };
        write_probe_result(result);
        thread::sleep(Duration::from_secs(30));
    }

    #[test]
    #[ignore = "subprocess probe for inherited trigger seccomp policy"]
    fn setpgid_escape_probe() {
        let result = unsafe { nix::libc::setpgid(0, 0) };
        write_probe_result(result);
        thread::sleep(Duration::from_secs(30));
    }

    fn output_with_limits(command: &mut Command, limits: Limits) -> Result<Output, Error> {
        output_with_readers(command, limits, |stdout, stderr| (stdout, stderr))
    }

    fn limits(stdout_bytes: usize, stderr_bytes: usize, wall_timeout: Duration) -> Limits {
        Limits {
            wall_timeout,
            stdout_bytes,
            stderr_bytes,
            cleanup_timeout: Duration::from_secs(1),
        }
    }

    fn shell(script: &str, arguments: &[&str]) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script, "trigger-test"]).args(arguments);
        command
    }

    fn background_command(pid_file: &Path, tail: &str) -> Command {
        shell(
            &format!("(/bin/sleep 30) & child=$!; printf '%s' \"$child\" > \"$1\"; {tail}"),
            &[pid_file.to_string_lossy().as_ref()],
        )
    }

    fn read_pids(pid_file: &Path) -> Vec<i32> {
        fs_err::read_to_string(pid_file)
            .unwrap_or_else(|error| panic!("read descendant pid from {}: {error}", pid_file.display()))
            .split_ascii_whitespace()
            .map(|pid| pid.parse::<i32>().unwrap())
            .collect()
    }

    fn assert_pids_reaped(pids: &[i32]) {
        for &pid in pids {
            let result = unsafe { nix::libc::kill(pid, 0) };
            assert_eq!(result, -1, "background descendant {pid} still exists");
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(nix::libc::ESRCH));
        }
    }

    fn write_probe_result(result: i32) {
        let outcome = if result == -1 {
            io::Error::last_os_error()
                .raw_os_error()
                .expect("escape syscall failure has errno")
                .to_string()
        } else {
            "ok".to_owned()
        };
        fs_err::write(
            std::env::var_os("TRIGGER_ESCAPE_RESULT").expect("probe result path"),
            outcome,
        )
        .unwrap();
    }

    struct DescendantCleanup(Vec<i32>);

    impl Drop for DescendantCleanup {
        fn drop(&mut self) {
            for &pid in &self.0 {
                unsafe {
                    nix::libc::kill(pid, nix::libc::SIGKILL);
                }
            }
        }
    }
}
