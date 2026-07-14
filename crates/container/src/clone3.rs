// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Minimal fork-like `clone3(2)` support for atomic cgroup placement.
//!
//! The locked `libc` release exposes `CLONE_INTO_CGROUP` through a 32-bit
//! integer type, which truncates its bit-33 value to zero. Keep the Linux UAPI
//! values and the complete `struct clone_args` ABI local instead of relying on
//! that constant. This primitive deliberately has no `clone(2)` or
//! `pidfd_open(2)` fallback: either the kernel performs the cgroup placement
//! and pidfd allocation as one operation, or the call fails.

use std::io;
use std::mem::{align_of, size_of};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use nix::libc;
use nix::unistd::Pid;

// Linux UAPI values from include/uapi/linux/sched.h. These must remain u64:
// CLONE_INTO_CGROUP cannot be represented by the c_int used by old bindings.
const CLONE_PIDFD: u64 = 1_u64 << 12;
const CLONE_INTO_CGROUP: u64 = 1_u64 << 33;

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
/// Like `fork(2)` in a multi-threaded process, the child branch may observe
/// library state whose locks were held by vanished threads. The caller must
/// make the [`Clone3Outcome::Child`] branch perform only its audited post-clone
/// sequence, must not unwind through frames that existed before this call, and
/// must ultimately terminate it with `_exit(2)`. The child should wait on the
/// caller's synchronization channel before doing privileged setup so the
/// parent can validate its pidfd and cgroup membership first.
pub(crate) unsafe fn clone3_into_cgroup(namespace_flags: u64, cgroup: BorrowedFd<'_>) -> io::Result<Clone3Outcome> {
    validate_namespace_flags(namespace_flags)?;

    let cgroup_fd = cgroup.as_raw_fd();
    let cgroup_fd = u64::try_from(cgroup_fd).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone3 cgroup descriptor must be non-negative",
        )
    })?;
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

/// Kill and reap a child after a post-success kernel-contract failure.
///
/// A successfully returned child remains our unreaped child, so its numeric
/// PID cannot be recycled before this wait completes. This makes kill+waitpid
/// safe even though the descriptor being rejected is not trusted as a pidfd.
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
                // A SIGCHLD handler may have reaped the child concurrently;
                // either way, no zombie or live child remains ours.
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
}
