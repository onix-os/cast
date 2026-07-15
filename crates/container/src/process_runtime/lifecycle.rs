use std::{
    io,
    os::fd::{AsFd, AsRawFd as _, BorrowedFd, OwnedFd},
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicI32, Ordering},
    },
    time::{Duration, Instant},
};

use nix::{
    errno::Errno,
    sys::{
        signal::{SaFlags, SigAction, SigHandler, Signal, kill, sigaction},
        signalfd::SigSet,
        wait::{Id as WaitId, WaitPidFlag, WaitStatus, waitid, waitpid},
    },
    unistd::{Pid, tcsetpgrp},
};

use super::super::{Error, MAX_CONTROL_EINTR_RETRIES, PIDFD_REAP_POLL_INTERVAL, PIDFD_REAP_TIMEOUT};

static SIGNAL_OVERRIDE_LOCK: Mutex<()> = Mutex::new(());

// libtest runs unit tests on a thread pool, while the legacy compatibility
// path must exercise a fork-like clone child that executes Rust setup code.
// Production builds do not get this escape hatch: they authenticate an exact
// single-task supervisor immediately before clone. Serializing the live unit
// fixtures prevents several test-only activations from cloning across one
// another; the harness-free integration test exercises the production guard.
#[cfg(test)]
pub(crate) static LEGACY_TEST_ACTIVATION_LOCK: Mutex<()> = Mutex::new(());

pub(crate) struct BlockedSignalMask {
    previous: nix::libc::sigset_t,
    active: bool,
    restore_on_drop: bool,
}

impl BlockedSignalMask {
    pub(crate) fn block_all() -> io::Result<Self> {
        // SAFETY: both sets are fully initialized output objects and
        // pthread_sigmask changes only the calling thread's mask.
        let mut blocked: nix::libc::sigset_t = unsafe { std::mem::zeroed() };
        if unsafe { nix::libc::sigfillset(&mut blocked) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: zero is a valid initial representation for this output set;
        // pthread_sigmask fills it with the previous mask on success.
        let mut previous: nix::libc::sigset_t = unsafe { std::mem::zeroed() };
        let status = unsafe { nix::libc::pthread_sigmask(nix::libc::SIG_SETMASK, &blocked, &mut previous) };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
        Ok(Self {
            previous,
            active: true,
            restore_on_drop: true,
        })
    }

    /// Preserve the blocked mask if this guard is dropped before its explicit
    /// restore point. The raw clone child uses this immediately after the
    /// fork-like return: any setup error or panic must reach `_exit` without
    /// permitting inherited signal handlers to run against copied userspace
    /// state. The parent's copy keeps ordinary RAII restoration enabled.
    pub(crate) fn retain_blocked_on_drop(&mut self) {
        self.restore_on_drop = false;
    }

    pub(crate) fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        // SAFETY: previous was produced by pthread_sigmask for this same
        // thread before clone. A zero third argument discards the old mask.
        let status =
            unsafe { nix::libc::pthread_sigmask(nix::libc::SIG_SETMASK, &self.previous, std::ptr::null_mut()) };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for BlockedSignalMask {
    fn drop(&mut self) {
        if self.restore_on_drop {
            let _ = self.restore();
        }
    }
}

pub(crate) struct SignalOverride {
    signal: Signal,
    previous: SigAction,
    restored: bool,
    _serial: MutexGuard<'static, ()>,
}

impl SignalOverride {
    pub(crate) fn install(signal: Signal) -> Result<Self, nix::Error> {
        let serial = SIGNAL_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let action = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        // SAFETY: action is fully initialized and signal is validated by nix.
        let previous = unsafe { sigaction(signal, &action)? };
        Ok(Self {
            signal,
            previous,
            restored: false,
            _serial: serial,
        })
    }

    pub(crate) fn restore(mut self) -> Result<(), nix::Error> {
        // SAFETY: previous was returned by sigaction for this exact signal.
        unsafe { sigaction(self.signal, &self.previous)? };
        self.restored = true;
        Ok(())
    }
}

impl Drop for SignalOverride {
    fn drop(&mut self) {
        if !self.restored {
            // Best-effort restoration during early return or unwinding. The
            // explicit success path reports restoration failure.
            unsafe {
                let _ = sigaction(self.signal, &self.previous);
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum ChildLifecycle {
    Legacy { pid: Pid },
    Pidfd { pid: Pid, pidfd: OwnedFd },
}

impl ChildLifecycle {
    pub(crate) fn pid(&self) -> Pid {
        match self {
            Self::Legacy { pid } | Self::Pidfd { pid, .. } => *pid,
        }
    }

    pub(crate) fn wait(&self) -> Result<WaitStatus, Errno> {
        match self {
            Self::Legacy { pid } => wait_for_child(*pid),
            Self::Pidfd { pidfd, .. } => wait_for_pidfd(pidfd.as_fd(), WaitPidFlag::WEXITED),
        }
    }

    pub(crate) fn cleanup(self) -> Result<(), Error> {
        match self {
            Self::Legacy { pid } => {
                abort_child(pid);
                Ok(())
            }
            Self::Pidfd { pidfd, .. } => match cleanup_pidfd_child(pidfd) {
                Ok(()) => Ok(()),
                Err(failure) => Err(Error::ChildCleanup {
                    cleanup: failure.cleanup,
                    pidfd: Some(ChildPidfdQuarantine::new(failure.pidfd)),
                }),
            },
        }
    }

    pub(crate) fn cleanup_after_failure(self, primary: Error) -> Error {
        match self.cleanup() {
            Ok(()) => primary,
            Err(Error::ChildCleanup { cleanup, pidfd }) => Error::ChildCleanupAfterFailure {
                primary: Box::new(primary),
                cleanup,
                pidfd,
            },
            Err(unexpected) => Error::ChildCleanupAfterFailure {
                primary: Box::new(primary),
                cleanup: io::Error::other(format!("unexpected exact-child cleanup error: {unexpected}")),
                pidfd: None,
            },
        }
    }
}

/// Exact clone3-child authority retained after cleanup could not prove reap.
///
/// Losing the last exact handle while the child may still be live or unreaped
/// would turn a lifecycle failure into an unauthenticated numeric-PID problem.
/// Drop therefore fails stop instead of closing the descriptor or starting a
/// helper thread: a helper would permanently violate the exact single-task
/// precondition for every later fork-like clone in this supervisor. Callers
/// that can recover must borrow the descriptor or explicitly take ownership
/// with [`Self::into_owned_fd`] before this guard is dropped.
#[derive(Debug)]
pub struct ChildPidfdQuarantine {
    pidfd: Option<OwnedFd>,
}

impl ChildPidfdQuarantine {
    pub(crate) fn new(pidfd: OwnedFd) -> Self {
        Self { pidfd: Some(pidfd) }
    }

    pub fn into_owned_fd(mut self) -> OwnedFd {
        self.pidfd.take().expect("pidfd quarantine must own its descriptor")
    }
}

impl AsFd for ChildPidfdQuarantine {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.pidfd
            .as_ref()
            .expect("pidfd quarantine must own its descriptor")
            .as_fd()
    }
}

impl Drop for ChildPidfdQuarantine {
    fn drop(&mut self) {
        if self.pidfd.is_some() {
            const MESSAGE: &[u8] =
                b"fatal: dropping unrecovered exact-child pidfd authority; refusing to continue supervisor\n";
            // SAFETY: MESSAGE is a live immutable byte slice for the complete
            // write. A diagnostic failure is deliberately ignored because the
            // immediately following abort is the authoritative fail-stop.
            unsafe {
                nix::libc::write(nix::libc::STDERR_FILENO, MESSAGE.as_ptr().cast(), MESSAGE.len());
            }
            std::process::abort();
        }
    }
}

#[derive(Debug)]
pub(crate) struct PidfdCleanupFailure {
    pub(crate) cleanup: io::Error,
    pub(crate) pidfd: OwnedFd,
}

pub(crate) fn send_pidfd_signal(pidfd: BorrowedFd<'_>, signal: Signal) -> Result<(), Errno> {
    let mut interrupted = 0;
    loop {
        // SAFETY: pidfd is a live borrowed descriptor, a null siginfo requests
        // ordinary process-directed signal semantics, and flags must be zero.
        let result = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
                signal as nix::libc::c_int,
                std::ptr::null::<nix::libc::siginfo_t>(),
                0_u32,
            )
        };
        match Errno::result(result) {
            Err(Errno::EINTR) if interrupted < MAX_CONTROL_EINTR_RETRIES => interrupted += 1,
            Ok(_) => return Ok(()),
            Err(source) => return Err(source),
        }
    }
}

pub(crate) fn wait_for_pidfd(pidfd: BorrowedFd<'_>, flags: WaitPidFlag) -> Result<WaitStatus, Errno> {
    let mut interrupted = 0;
    loop {
        match waitid(WaitId::PIDFd(pidfd), flags) {
            Err(Errno::EINTR) if interrupted < MAX_CONTROL_EINTR_RETRIES => interrupted += 1,
            result => return result,
        }
    }
}

pub(crate) fn cleanup_pidfd_child(pidfd: OwnedFd) -> Result<(), PidfdCleanupFailure> {
    let signal = send_pidfd_signal(pidfd.as_fd(), Signal::SIGKILL);
    if signal.is_ok() {
        return wait_for_pidfd_reap(pidfd.as_fd(), PIDFD_REAP_TIMEOUT)
            .map_or_else(|cleanup| Err(PidfdCleanupFailure { cleanup, pidfd }), |_| Ok(()));
    }

    // Do not block when the authoritative signal operation failed: the exact
    // child may still be parked on the release socket. One nonblocking pidfd
    // wait may nevertheless prove that it exited independently.
    let signal = signal.unwrap_err();
    match wait_for_pidfd(pidfd.as_fd(), WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG) {
        Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => Ok(()),
        Ok(status) => Err(PidfdCleanupFailure {
            cleanup: io::Error::other(format!(
                "pidfd_send_signal(SIGKILL) failed: {signal}; waitid(P_PIDFD, WNOHANG) did not prove exact child termination: {status:?}"
            )),
            pidfd,
        }),
        // Linux defines pidfd_send_signal(ESRCH) to mean that the exact target
        // has terminated and already been waited on. The matching P_PIDFD
        // ECHILD result confirms that no waitable child remains. No other error
        // pair is accepted: in particular, an ordinary or closed descriptor
        // must remain a structured cleanup failure rather than impersonating a
        // completed pidfd lifecycle.
        Err(Errno::ECHILD) if signal == Errno::ESRCH => Ok(()),
        Err(wait) => Err(PidfdCleanupFailure {
            cleanup: io::Error::new(
                io::Error::from_raw_os_error(wait as i32).kind(),
                format!("pidfd_send_signal(SIGKILL) failed: {signal}; waitid(P_PIDFD, WNOHANG) failed: {wait}"),
            ),
            pidfd,
        }),
    }
}

pub(crate) fn wait_for_pidfd_reap(pidfd: BorrowedFd<'_>, timeout: Duration) -> io::Result<WaitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match wait_for_pidfd(pidfd, WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG) {
            Ok(status @ (WaitStatus::Exited(..) | WaitStatus::Signaled(..))) => return Ok(status),
            Ok(WaitStatus::StillAlive) => {}
            Ok(status) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("waitid(P_PIDFD, WNOHANG) returned nonterminal child status {status:?}"),
                ));
            }
            Err(source) => {
                return Err(io::Error::new(
                    io::Error::from_raw_os_error(source as i32).kind(),
                    format!("waitid(P_PIDFD, WNOHANG) while reaping SIGKILLed child: {source}"),
                ));
            }
        }

        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("waitid(P_PIDFD, WNOHANG) did not reap SIGKILLed child within {timeout:?}"),
            ));
        }
        std::thread::sleep(PIDFD_REAP_POLL_INTERVAL.min(deadline.duration_since(now)));
    }
}

fn wait_for_child(pid: Pid) -> Result<WaitStatus, nix::Error> {
    loop {
        match waitpid(pid, None) {
            Err(Errno::EINTR) => {}
            result => return result,
        }
    }
}

pub(crate) fn abort_child(pid: Pid) {
    let _ = kill(pid, Signal::SIGKILL);
    let _ = wait_for_child(pid);
}

pub fn set_term_fg(pgid: Pid) -> Result<(), nix::Error> {
    // Ignore SIGTTOU and get previous handler
    let prev_handler = unsafe {
        sigaction(
            Signal::SIGTTOU,
            &SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty()),
        )?
    };
    // Set term fg to pid
    let res = tcsetpgrp(io::stdin().as_raw_fd(), pgid);
    // Set up old handler
    unsafe { sigaction(Signal::SIGTTOU, &prev_handler)? };

    match res {
        Ok(_) => {}
        // Ignore ENOTTY error
        Err(nix::Error::ENOTTY) => {}
        Err(e) => return Err(e),
    }

    Ok(())
}

/// Forwards `SIGINT` from the current process to the [`Pid`] process
pub fn forward_sigint(pid: Pid) -> Result<(), nix::Error> {
    static PID: AtomicI32 = AtomicI32::new(0);

    PID.store(pid.as_raw(), Ordering::Relaxed);

    extern "C" fn on_int(_: i32) {
        let pid = Pid::from_raw(PID.load(Ordering::Relaxed));
        let _ = kill(pid, Signal::SIGINT);
    }

    let action = SigAction::new(SigHandler::Handler(on_int), SaFlags::empty(), SigSet::empty());
    unsafe { sigaction(Signal::SIGINT, &action)? };

    Ok(())
}
