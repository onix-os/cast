
//! Minimal fork-like `clone3(2)` support for atomic cgroup placement.
//!
//! The locked `libc` release exposes `CLONE_INTO_CGROUP` through a 32-bit
//! integer type, which truncates its bit-33 value to zero. Keep the Linux UAPI
//! values and the complete `struct clone_args` ABI local instead of relying on
//! that constant. This primitive deliberately has no `clone(2)` or
//! `pidfd_open(2)` fallback: either the kernel performs the cgroup placement
//! and pidfd allocation as one operation, or the call fails.

use std::ffi::{CStr, CString};
use std::io;
use std::mem::{align_of, size_of};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::ptr::NonNull;

use nix::errno::Errno;
use nix::libc;
use nix::unistd::Pid;

// Linux UAPI values from include/uapi/linux/sched.h. These must remain u64:
// CLONE_INTO_CGROUP cannot be represented by the c_int used by old bindings.
const CLONE_PIDFD: u64 = 1_u64 << 12;
const CLONE_INTO_CGROUP: u64 = 1_u64 << 33;
const PROC_SUPER_MAGIC: libc::c_long = 0x0000_9fa0;
const CURRENT_TASK_DIRECTORY_LABEL: &str = "/proc/<getpid>/task";

// A valid Cast supervisor has exactly one task. The larger bound is only for
// deterministic diagnostics and fail-closed behavior when this guard is
// accidentally called in a highly threaded process. It also bounds readdir(3)
// work if procfs changes while it is being inspected.
const MAX_OBSERVED_TASKS: usize = 1024;
const MAX_TASK_DIRECTORY_READS: usize = MAX_OBSERVED_TASKS + 3;

const NAMESPACE_FLAGS: u64 = libc::CLONE_NEWTIME as u64
    | libc::CLONE_NEWNS as u64
    | libc::CLONE_NEWCGROUP as u64
    | libc::CLONE_NEWUTS as u64
    | libc::CLONE_NEWIPC as u64
    | libc::CLONE_NEWUSER as u64
    | libc::CLONE_NEWPID as u64
    | libc::CLONE_NEWNET as u64;

/// Linux's version-2 `struct clone_args` ABI.
///
/// Passing all 88 bytes makes every field explicit and prevents a future Rust
/// layout change from silently changing the kernel ABI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

const _: () = assert!(size_of::<CloneArgs>() == 11 * size_of::<u64>());
const _: () = assert!(size_of::<CloneArgs>() == 88);
const _: () = assert!(align_of::<CloneArgs>() == 8);

/// The two control-flow outcomes of a successful fork-like `clone3(2)` call.
#[derive(Debug)]
pub(crate) enum Clone3Outcome {
    /// Returned in the original process. The pidfd is the descriptor produced
    /// atomically by the same kernel operation that created `pid`.
    Parent { pid: Pid, pidfd: OwnedFd },
    /// Returned in the new process.
    Child,
}

/// Create a fork-like child directly in `cgroup` and request its pidfd.
///
/// `namespace_flags` is deliberately restricted to Linux `CLONE_NEW*` bits.
/// The primitive owns the process-creation policy: it always requests
/// `CLONE_PIDFD`, `CLONE_INTO_CGROUP`, and `SIGCHLD`, and never requests
/// `CLONE_VM`. Consequently `stack` and `stack_size` are both zero and the
/// child resumes on the copy-on-write copy of the caller's current stack.
///
/// There is intentionally no fallback. Kernel or delegation failures are
/// returned directly to the caller.
///
/// # Safety
///
/// The primitive refuses to call `clone3` unless an authenticated, pinned
/// procfs walk through `/proc/<getpid>/task` observes the caller as the
/// process's exact sole task. It then requires the blocked caller to retain the
/// default, waitable SIGCHLD disposition before entering the syscall. It never
/// trusts procfs magic links such as `/proc/self`. The caller must block every
/// catchable signal before entering this function and retain that mask through
/// the returned child panic boundary, so a signal handler cannot create a task
/// or reap the child between the audits and syscall.
///
/// This audit is an operational Cast-supervisor guard, not a kernel-atomic
/// proof against hostile thread churn: procfs enumeration and `clone3` are
/// separate kernel operations. Cast must exclusively own a cooperative
/// supervisor process that neither runs attacker-controlled code nor exposes
/// another thread-creation path around this call. Under that invariant, the
/// signal mask and exact task audit prevent the child from inheriting userspace
/// locks held by vanished threads. Do not use this primitive as a general
/// fork-after-threads API.
///
/// The caller must still make the [`Clone3Outcome::Child`] branch perform only
/// its audited post-clone sequence, contain panics so they cannot unwind
/// through frames that existed before this call, and ultimately terminate it
/// with `_exit(2)`. The child should wait on the caller's synchronization
/// channel before doing privileged setup so the parent can validate its pidfd
/// and cgroup membership first.
pub(crate) unsafe fn clone3_into_cgroup(namespace_flags: u64, cgroup: BorrowedFd<'_>) -> io::Result<Clone3Outcome> {
    validate_namespace_flags(namespace_flags)?;

    let cgroup_fd = cgroup.as_raw_fd();
    let cgroup_fd = u64::try_from(cgroup_fd).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone3 cgroup descriptor must be non-negative",
        )
    })?;
    // This is intentionally the last operation before constructing the local
    // syscall arguments. Cast's operational supervisor invariant forbids a
    // thread-creation path here; procfs itself cannot make that guarantee
    // atomic against hostile task churn.
    require_single_threaded_process()?;
    require_waitable_sigchld_disposition()?;

    let mut pidfd = -1_i32;
    let args = clone_args(namespace_flags, cgroup_fd, &mut pidfd);

    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = args;
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic clone3 cgroup placement supports only Linux x86_64",
        ));
    };

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    // SAFETY: `args` has the kernel's exact 88-byte layout and remains live and
    // writable for the duration of the syscall. Its only nonzero pointer is to
    // the live parent-local `pidfd` slot. No pointer is retained by the kernel.
    let result = unsafe { libc::syscall(libc::SYS_clone3, &args as *const CloneArgs, size_of::<CloneArgs>()) };

    match result {
        -1 => Err(io::Error::last_os_error()),
        0 => Ok(Clone3Outcome::Child),
        result if result > 0 => finish_parent(result, pidfd),
        result => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("clone3 returned unexpected negative result {result}"),
        )),
    }
}

pub(crate) fn require_single_threaded_process() -> io::Result<()> {
    let current = current_task_id()?;
    let tasks = authenticated_current_process_tasks()?;
    require_single_task(current, tasks.as_slice())
}

/// Require a SIGCHLD disposition that preserves the parent's exclusive wait.
///
/// The caller has already proved that it is the process's sole task and must
/// still have every catchable signal blocked. Under those conditions this
/// read-only `sigaction` query cannot race a cooperating handler or thread.
/// Explicit SIG_IGN, SA_NOCLDWAIT, and custom handlers are rejected because
/// they can auto-reap or synchronously reap the clone child before numeric
/// pre-release `/proc/<pid>` and cgroup membership diagnostics complete.
pub(super) fn require_waitable_sigchld_disposition() -> io::Result<()> {
    // SAFETY: a null action pointer requests a read-only query and `current` is
    // a fully initialized output object after a successful call.
    let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut current) } == -1 {
        let source = io::Error::last_os_error();
        return Err(io::Error::new(
            source.kind(),
            format!("inspect SIGCHLD disposition before fork-like clone: {source}"),
        ));
    }

    if current.sa_sigaction != libc::SIG_DFL {
        let disposition = if current.sa_sigaction == libc::SIG_IGN {
            "SIG_IGN"
        } else {
            "a custom handler"
        };
        return Err(io::Error::other(format!(
            "fork-like clone requires waitable SIGCHLD disposition SIG_DFL; found {disposition}"
        )));
    }
    if current.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err(io::Error::other(
            "fork-like clone requires waitable SIGCHLD disposition without SA_NOCLDWAIT",
        ));
    }
    Ok(())
}

fn current_task_id() -> io::Result<i32> {
    // SAFETY: gettid has no arguments and returns the current kernel task ID.
    let current = unsafe { libc::syscall(libc::SYS_gettid) };
    i32::try_from(current)
        .ok()
        .filter(|tid| *tid > 0)
        .ok_or_else(|| io::Error::other(format!("gettid returned invalid task ID {current}")))
}

fn current_process_id() -> io::Result<i32> {
    // SAFETY: getpid has no arguments and returns the current process ID.
    let current = unsafe { libc::getpid() };
    if current > 0 {
        Ok(current)
    } else {
        Err(io::Error::other(format!(
            "getpid returned invalid process ID {current}"
        )))
    }
}

fn authenticated_current_process_tasks() -> io::Result<TaskSnapshot> {
    let proc_root = open_directory_at(libc::AT_FDCWD, c"/proc", "open procfs root")?;
    require_procfs(&proc_root, "/proc")?;

    // Use the numeric getpid directory rather than procfs's `self` magic link.
    // The process cannot have its PID recycled while this code is running.
    let process_id = current_process_id()?;
    let process_name = CString::new(process_id.to_string()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("getpid returned process ID containing NUL: {process_id}"),
        )
    })?;
    let process = open_directory_at(
        proc_root.as_raw_fd(),
        &process_name,
        "open numeric current-process directory under authenticated procfs",
    )?;
    require_procfs(&process, "/proc/<getpid>")?;

    // Both path components are opened relative to already pinned directory
    // descriptors with O_NOFOLLOW. No `/proc/self`, `/proc/thread-self`, or
    // `/proc/*/fd` magic-link resolution participates in the audit.
    let tasks = open_directory_at(
        process.as_raw_fd(),
        c"task",
        "open task directory under authenticated numeric procfs process",
    )?;
    require_procfs(&tasks, CURRENT_TASK_DIRECTORY_LABEL)?;
    enumerate_task_ids(tasks)
}

fn open_directory_at(parent: RawFd, name: &CStr, operation: &'static str) -> io::Result<OwnedFd> {
    loop {
        // SAFETY: `name` is NUL-terminated, `parent` is either AT_FDCWD or a
        // live pinned directory descriptor, and openat does not retain either.
        let descriptor = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                0,
            )
        };
        if descriptor >= 0 {
            // SAFETY: successful openat returned a fresh owned descriptor.
            return Ok(unsafe { OwnedFd::from_raw_fd(descriptor) });
        }
        let source = io::Error::last_os_error();
        if source.raw_os_error() != Some(libc::EINTR) {
            return Err(io::Error::new(source.kind(), format!("{operation}: {source}")));
        }
    }
}

fn require_procfs(descriptor: &impl AsRawFd, label: &str) -> io::Result<()> {
    // SAFETY: all-zero statfs storage is valid output and `descriptor` remains
    // live for the duration of fstatfs.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(descriptor.as_raw_fd(), &mut stat) } == -1 {
        let source = io::Error::last_os_error();
        return Err(io::Error::new(
            source.kind(),
            format!("authenticate {label} as procfs: {source}"),
        ));
    }
    if stat.f_type != PROC_SUPER_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "refusing unauthenticated task audit through {label}: expected procfs magic {PROC_SUPER_MAGIC:#x}, found {:#x}",
                stat.f_type
            ),
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct TaskSnapshot {
    ids: [i32; MAX_OBSERVED_TASKS],
    len: usize,
}

impl TaskSnapshot {
    fn new() -> Self {
        Self {
            ids: [0; MAX_OBSERVED_TASKS],
            len: 0,
        }
    }

    fn push(&mut self, tid: i32) -> io::Result<()> {
        let slot = self.ids.get_mut(self.len).ok_or_else(|| {
            io::Error::other(format!(
                "refusing unbounded task audit in {CURRENT_TASK_DIRECTORY_LABEL}: more than {MAX_OBSERVED_TASKS} task IDs"
            ))
        })?;
        *slot = tid;
        self.len += 1;
        Ok(())
    }

    fn as_slice(&self) -> &[i32] {
        &self.ids[..self.len]
    }
}

struct DirectoryStream(Option<NonNull<libc::DIR>>);

impl DirectoryStream {
    fn from_descriptor(descriptor: OwnedFd) -> io::Result<Self> {
        let descriptor = descriptor.into_raw_fd();
        // SAFETY: fdopendir consumes this fresh descriptor on success.
        let stream = unsafe { libc::fdopendir(descriptor) };
        match NonNull::new(stream) {
            Some(stream) => Ok(Self(Some(stream))),
            None => {
                let source = io::Error::last_os_error();
                // SAFETY: fdopendir failed and therefore did not consume the
                // descriptor. Reconstruct ownership so it is closed once.
                drop(unsafe { OwnedFd::from_raw_fd(descriptor) });
                Err(io::Error::new(
                    source.kind(),
                    format!("open {CURRENT_TASK_DIRECTORY_LABEL} directory stream: {source}"),
                ))
            }
        }
    }

    fn close(mut self) -> io::Result<()> {
        let stream = self
            .0
            .take()
            .ok_or_else(|| io::Error::other("task directory stream was already closed"))?;
        // SAFETY: this takes the sole ownership of the live DIR stream and its
        // underlying descriptor.
        if unsafe { libc::closedir(stream.as_ptr()) } == -1 {
            let source = io::Error::last_os_error();
            Err(io::Error::new(
                source.kind(),
                format!("close {CURRENT_TASK_DIRECTORY_LABEL} directory stream: {source}"),
            ))
        } else {
            Ok(())
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        if let Some(stream) = self.0.take() {
            // SAFETY: Drop owns the remaining live DIR stream. There is no
            // useful recovery path for a close error while unwinding another
            // task-audit error.
            unsafe { libc::closedir(stream.as_ptr()) };
        }
    }
}

fn enumerate_task_ids(descriptor: OwnedFd) -> io::Result<TaskSnapshot> {
    let stream = DirectoryStream::from_descriptor(descriptor)?;
    let result = (|| {
        let mut snapshot = TaskSnapshot::new();
        for _ in 0..MAX_TASK_DIRECTORY_READS {
            Errno::clear();
            // SAFETY: the live DIR stream is exclusively borrowed for this
            // call. readdir returns storage valid until the next call.
            let entry = unsafe {
                libc::readdir(
                    stream
                        .0
                        .as_ref()
                        .ok_or_else(|| io::Error::other("task directory stream closed during enumeration"))?
                        .as_ptr(),
                )
            };
            if entry.is_null() {
                let error = Errno::last();
                if error == Errno::UnknownErrno {
                    return Ok(snapshot);
                }
                let source = io::Error::from_raw_os_error(error as i32);
                return Err(io::Error::new(
                    source.kind(),
                    format!("enumerate {CURRENT_TASK_DIRECTORY_LABEL}: {source}"),
                ));
            }

            // SAFETY: readdir returned a live dirent whose d_name is
            // NUL-terminated until the next directory operation.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            snapshot.push(parse_task_id(name)?)?;
        }
        Err(io::Error::other(format!(
            "refusing unbounded task audit in {CURRENT_TASK_DIRECTORY_LABEL}: exceeded {MAX_TASK_DIRECTORY_READS} directory reads"
        )))
    })();
    let close = stream.close();
    match (result, close) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(snapshot), Ok(())) => Ok(snapshot),
    }
}

fn parse_task_id(name: &[u8]) -> io::Result<i32> {
    if name.is_empty() || name[0] == b'0' || !name.iter().all(u8::is_ascii_digit) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("noncanonical task entry in {CURRENT_TASK_DIRECTORY_LABEL}"),
        ));
    }
    name.iter().try_fold(0_i32, |value, digit| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i32::from(*digit - b'0')))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("task ID exceeds pid_t in {CURRENT_TASK_DIRECTORY_LABEL}"),
                )
            })
    })
}

fn require_single_task(current: i32, tasks: &[i32]) -> io::Result<()> {
    match tasks {
        [tid] if *tid == current => Ok(()),
        [tid] => Err(io::Error::other(format!(
            "fork-like container clone requires current task {current} to be the sole supervisor task; found {tid}"
        ))),
        [] => Err(io::Error::other(format!(
            "fork-like container clone could not find current task {current} in {CURRENT_TASK_DIRECTORY_LABEL}"
        ))),
        [first, second, ..] => Err(io::Error::other(format!(
            "fork-like container clone requires an exactly single-threaded supervisor; observed {} tasks, beginning with task IDs {first} and {second}",
            tasks.len()
        ))),
    }
}

fn validate_namespace_flags(namespace_flags: u64) -> io::Result<()> {
    let unsupported = namespace_flags & !NAMESPACE_FLAGS;
    if unsupported != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("clone3 namespace flags contain non-namespace bits 0x{unsupported:016x}"),
        ));
    }
    Ok(())
}

fn clone_args(namespace_flags: u64, cgroup_fd: u64, pidfd: &mut i32) -> CloneArgs {
    CloneArgs {
        flags: namespace_flags | CLONE_PIDFD | CLONE_INTO_CGROUP,
        pidfd: std::ptr::from_mut(pidfd) as usize as u64,
        exit_signal: libc::SIGCHLD as u64,
        // A zero stack is required for fork-like semantics without CLONE_VM.
        stack: 0,
        stack_size: 0,
        cgroup: cgroup_fd,
        ..CloneArgs::default()
    }
}

fn finish_parent(result: libc::c_long, raw_pidfd: i32) -> io::Result<Clone3Outcome> {
    let pid_raw = i32::try_from(result).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("clone3 returned parent pid outside pid_t range: {result}"),
        )
    })?;
    if pid_raw <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("clone3 returned invalid parent pid {pid_raw}"),
        ));
    }
    let pid = Pid::from_raw(pid_raw);

    if raw_pidfd < 0 {
        let source = io::Error::new(
            io::ErrorKind::InvalidData,
            format!("clone3 created child {pid_raw} without a valid pidfd"),
        );
        return Err(reject_spawned_child(pid_raw, source));
    }

    let descriptor_flags = match get_descriptor_flags(raw_pidfd) {
        Ok(flags) => flags,
        Err(source) => {
            let source = io::Error::new(
                source.kind(),
                format!("cannot inspect clone3 pidfd for child {pid_raw}: {source}"),
            );
            return Err(reject_spawned_child(pid_raw, source));
        }
    };

    // SAFETY: successful clone3 with CLONE_PIDFD writes a fresh descriptor to
    // the parent-local pidfd slot. F_GETFD above additionally proved that this
    // non-negative descriptor still names a live parent table entry. Ownership
    // is transferred exactly once here.
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw_pidfd) };
    if descriptor_flags & libc::FD_CLOEXEC == 0 {
        let source = io::Error::new(
            io::ErrorKind::InvalidData,
            format!("clone3 pidfd for child {pid_raw} is not close-on-exec"),
        );
        return Err(reject_spawned_child(pid_raw, source));
    }

    Ok(Clone3Outcome::Parent { pid, pidfd })
}

fn get_descriptor_flags(descriptor: i32) -> io::Result<i32> {
    loop {
        // SAFETY: F_GETFD only reads the descriptor table entry and takes no
        // variadic third argument.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        if flags >= 0 {
            return Ok(flags);
        }
        let source = io::Error::last_os_error();
        if source.raw_os_error() != Some(libc::EINTR) {
            return Err(source);
        }
    }
}

/// Kill and reap a child before pidfd authority has been validated and exposed.
///
/// This emergency path is confined to `finish_parent`: `clone3` reported a
/// child, but the CLONE_PIDFD output itself is missing, malformed, unusable, or
/// violates its close-on-exec contract, so no pidfd authority can safely be
/// returned to the caller. The child remains our unreaped child under the
/// required waitable SIGCHLD disposition, so its numeric PID cannot be recycled
/// before this wait completes. Once `finish_parent` returns a validated pidfd,
/// every signal and wait in the normal lifecycle is pidfd-only and this helper
/// is unreachable.
fn reject_spawned_child(pid: libc::pid_t, source: io::Error) -> io::Error {
    match kill_and_reap(pid) {
        Ok(()) => source,
        Err(cleanup) => io::Error::new(
            source.kind(),
            format!("{source}; additionally failed to kill and reap child {pid}: {cleanup}"),
        ),
    }
}

fn kill_and_reap(pid: libc::pid_t) -> io::Result<()> {
    // SAFETY: `pid` is the positive PID returned by clone3 in this parent and
    // SIGKILL has no pointer arguments.
    if unsafe { libc::kill(pid, libc::SIGKILL) } == -1 {
        let source = io::Error::last_os_error();
        if source.raw_os_error() != Some(libc::ESRCH) {
            return Err(source);
        }
    }

    loop {
        let mut status = 0;
        // SAFETY: status is a live output slot and `pid` names our direct,
        // unreaped child. A zero options word requests a blocking reap.
        let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
        if waited == pid {
            return Ok(());
        }
        if waited == -1 {
            let source = io::Error::last_os_error();
            match source.raw_os_error() {
                Some(libc::EINTR) => continue,
                // The audited SIG_DFL/no-SA_NOCLDWAIT contract should make
                // this unreachable. If the kernel nevertheless reports
                // ECHILD, no waitable child remains ours.
                Some(libc::ECHILD) => return Ok(()),
                _ => return Err(source),
            }
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("waitpid for child {pid} returned unexpected pid {waited}"),
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;

    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

    use super::*;

    #[test]
    fn clone_args_matches_linux_v2_layout_exactly() {
        assert_eq!(size_of::<CloneArgs>(), 88);
        assert_eq!(align_of::<CloneArgs>(), 8);
        assert_eq!(offset_of!(CloneArgs, flags), 0);
        assert_eq!(offset_of!(CloneArgs, pidfd), 8);
        assert_eq!(offset_of!(CloneArgs, child_tid), 16);
        assert_eq!(offset_of!(CloneArgs, parent_tid), 24);
        assert_eq!(offset_of!(CloneArgs, exit_signal), 32);
        assert_eq!(offset_of!(CloneArgs, stack), 40);
        assert_eq!(offset_of!(CloneArgs, stack_size), 48);
        assert_eq!(offset_of!(CloneArgs, tls), 56);
        assert_eq!(offset_of!(CloneArgs, set_tid), 64);
        assert_eq!(offset_of!(CloneArgs, set_tid_size), 72);
        assert_eq!(offset_of!(CloneArgs, cgroup), 80);
    }

    #[test]
    fn high_clone_flags_keep_their_full_u64_values() {
        assert_eq!(CLONE_PIDFD, 0x0000_0000_0000_1000);
        assert_eq!(CLONE_INTO_CGROUP, 0x0000_0002_0000_0000);
        assert!(CLONE_INTO_CGROUP > u32::MAX as u64);
        assert_eq!(CLONE_PIDFD & CLONE_INTO_CGROUP, 0);
    }

    #[test]
    fn arguments_are_fork_like_and_request_atomic_placement() {
        let mut pidfd = -1_i32;
        let namespace_flags = libc::CLONE_NEWUSER as u64 | libc::CLONE_NEWNS as u64;
        let cgroup_fd = 37_u64;
        let args = clone_args(namespace_flags, cgroup_fd, &mut pidfd);

        assert_eq!(args.flags, namespace_flags | CLONE_PIDFD | CLONE_INTO_CGROUP);
        assert_eq!(args.pidfd, std::ptr::from_mut(&mut pidfd) as usize as u64);
        assert_eq!(args.exit_signal, libc::SIGCHLD as u64);
        assert_eq!(args.stack, 0);
        assert_eq!(args.stack_size, 0);
        assert_eq!(args.cgroup, cgroup_fd);
        assert_eq!(
            CloneArgs {
                flags: args.flags,
                pidfd: args.pidfd,
                exit_signal: args.exit_signal,
                cgroup: args.cgroup,
                ..CloneArgs::default()
            },
            args
        );
    }

    #[test]
    fn every_linux_namespace_flag_is_accepted() {
        validate_namespace_flags(0).unwrap();
        validate_namespace_flags(NAMESPACE_FLAGS).unwrap();
        for flag in [
            libc::CLONE_NEWTIME as u64,
            libc::CLONE_NEWNS as u64,
            libc::CLONE_NEWCGROUP as u64,
            libc::CLONE_NEWUTS as u64,
            libc::CLONE_NEWIPC as u64,
            libc::CLONE_NEWUSER as u64,
            libc::CLONE_NEWPID as u64,
            libc::CLONE_NEWNET as u64,
        ] {
            validate_namespace_flags(flag).unwrap();
        }
    }

    #[test]
    fn process_sharing_and_internal_flags_are_rejected() {
        for flag in [
            libc::CLONE_VM as u64,
            libc::CLONE_THREAD as u64,
            libc::CLONE_FILES as u64,
            libc::CLONE_PARENT_SETTID as u64,
            CLONE_PIDFD,
            CLONE_INTO_CGROUP,
        ] {
            let error = validate_namespace_flags(flag).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("non-namespace bits"));
        }
    }

    #[test]
    fn task_ids_are_canonical_positive_pid_t_values() {
        assert_eq!(parse_task_id(b"1").unwrap(), 1);
        assert_eq!(parse_task_id(b"2147483647").unwrap(), i32::MAX);
        for invalid in [
            b"".as_slice(),
            b"0".as_slice(),
            b"01".as_slice(),
            b"-1".as_slice(),
            b"1a".as_slice(),
            b"2147483648".as_slice(),
        ] {
            assert!(parse_task_id(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn clone_supervisor_requires_its_exact_sole_task() {
        require_single_task(42, &[42]).unwrap();
        assert!(require_single_task(42, &[]).is_err());
        assert!(require_single_task(42, &[41]).is_err());
        assert!(require_single_task(42, &[42, 43]).is_err());
    }

    #[test]
    fn clone3_rejects_nonwaitable_sigchld_dispositions_in_isolated_processes() {
        const CHILD_ENV: &str = "CONTAINER_CLONE3_SIGCHLD_DISPOSITION_TEST";
        const TEST_NAME: &str = "clone3::tests::clone3_rejects_nonwaitable_sigchld_dispositions_in_isolated_processes";

        let Some(case) = std::env::var_os(CHILD_ENV) else {
            for case in ["default", "sig_ign", "no_cld_wait", "custom_handler"] {
                let output = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
                    .env(CHILD_ENV, case)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "isolated SIGCHLD disposition case {case:?} failed: {}; stderr={}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        };

        extern "C" fn custom_sigchld_handler(_: libc::c_int) {}

        let case = case.to_str().expect("test case is UTF-8");
        let action = match case {
            "default" => SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty()),
            "sig_ign" => SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty()),
            "no_cld_wait" => SigAction::new(SigHandler::SigDfl, SaFlags::SA_NOCLDWAIT, SigSet::empty()),
            "custom_handler" => SigAction::new(
                SigHandler::Handler(custom_sigchld_handler),
                SaFlags::empty(),
                SigSet::empty(),
            ),
            other => panic!("unknown isolated SIGCHLD test case {other:?}"),
        };
        // SAFETY: this exact-test subprocess installs one valid action and exits
        // immediately after the read-only disposition audit.
        unsafe { sigaction(Signal::SIGCHLD, &action) }.unwrap();

        if case == "default" {
            require_waitable_sigchld_disposition().unwrap();
            return;
        }
        let error = require_waitable_sigchld_disposition().unwrap_err();
        match case {
            "sig_ign" => assert!(error.to_string().contains("found SIG_IGN")),
            "no_cld_wait" => assert!(error.to_string().contains("without SA_NOCLDWAIT")),
            "custom_handler" => assert!(error.to_string().contains("found a custom handler")),
            _ => unreachable!(),
        }
    }

    #[test]
    fn task_audit_authenticates_procfs_and_rejects_an_ordinary_directory() {
        let proc_root = open_directory_at(libc::AT_FDCWD, c"/proc", "open procfs for test").unwrap();
        require_procfs(&proc_root, "/proc").unwrap();

        let ordinary = tempfile::tempdir().unwrap();
        let ordinary = std::fs::File::open(ordinary.path()).unwrap();
        let error = require_procfs(&ordinary, "ordinary test directory").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("expected procfs magic"));
    }

    #[test]
    fn real_parked_second_thread_is_observed_and_rejected() {
        let (ready_sender, ready_receiver) = mpsc::sync_channel(0);
        let (release_sender, release_receiver) = mpsc::sync_channel::<()>(0);
        let parked = thread::spawn(move || {
            let tid = current_task_id();
            let _ = ready_sender.send(tid);
            let _ = release_receiver.recv();
        });

        let parked_tid = ready_receiver.recv().unwrap();
        let snapshot = authenticated_current_process_tasks();
        let guard = require_single_threaded_process();
        drop(release_sender);
        parked.join().unwrap();

        let parked_tid = parked_tid.unwrap();
        let snapshot = snapshot.unwrap();
        assert!(
            snapshot.as_slice().contains(&parked_tid),
            "authenticated task snapshot did not contain parked task {parked_tid}: {:?}",
            snapshot.as_slice()
        );
        let error = guard.unwrap_err();
        assert!(error.to_string().contains("exactly single-threaded supervisor"));
    }
}
