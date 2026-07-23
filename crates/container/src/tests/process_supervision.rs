#[test]
fn synchronization_socket_is_close_on_exec_blocking_and_nosignal() {
    let mut sync = SyncSocket::new().unwrap();
    let supervisor_fd = sync.supervisor_fd();
    let child_fd = sync.child_fd();

    assert!(supervisor_fd >= 3);
    assert!(child_fd >= 3);
    for fd in [supervisor_fd, child_fd] {
        let flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD).unwrap());
        assert!(flags.contains(FdFlag::FD_CLOEXEC));
        let status = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL).unwrap());
        assert!(!status.contains(OFlag::O_NONBLOCK));
    }

    sync.close_child_endpoint().unwrap();
    assert_eq!(fcntl(child_fd, FcntlArg::F_GETFD), Err(Errno::EBADF));
    assert_eq!(send_packet_no_signal(supervisor_fd, b"release"), Err(Errno::EPIPE));
    drop(sync);
    assert_eq!(fcntl(supervisor_fd, FcntlArg::F_GETFD), Err(Errno::EBADF));
}

#[test]
fn synchronization_socket_blocks_child_until_one_atomic_release() {
    use std::sync::mpsc;
    use std::time::Duration;

    let sync = SyncSocket::new().unwrap();
    let supervisor_fd = sync.supervisor_fd();
    let child_fd = sync.child_fd();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (message_tx, message_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        let mut message = [0_u8; 1];
        let result = read(child_fd, &mut message).map(|length| (length, message));
        message_tx.send(result).unwrap();
    });

    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(message_rx.recv_timeout(Duration::from_millis(50)).is_err());
    assert_eq!(send_packet_no_signal(supervisor_fd, &[Message::Continue as u8]), Ok(1));
    assert_eq!(
        message_rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap(),
        (1, [Message::Continue as u8])
    );
    reader.join().unwrap();
}

#[test]
fn synchronization_socket_preserves_the_maximum_diagnostic_packet() {
    let sync = SyncSocket::new().unwrap();
    let diagnostic = vec![b'x'; MAX_CHILD_ERROR_BYTES];
    assert_eq!(
        send_packet_no_signal(sync.child_fd(), &diagnostic),
        Ok(MAX_CHILD_ERROR_BYTES)
    );
    assert_eq!(
        read_child_error(sync.supervisor_fd()).unwrap().as_bytes(),
        diagnostic.as_slice()
    );
}

#[test]
fn pidfd_wait_and_signal_preserve_exact_terminal_statuses() {
    let exit_child = Command::new("/bin/sh")
        .args(["-c", "/bin/sleep 0.05; exit 23"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let exit_pid = nix::unistd::Pid::from_raw(i32::try_from(exit_child.id()).unwrap());
    let exit_pidfd = open_test_pidfd(exit_pid);
    drop(exit_child);
    assert_eq!(
        wait_for_pidfd(exit_pidfd.as_fd(), nix::sys::wait::WaitPidFlag::WEXITED).unwrap(),
        nix::sys::wait::WaitStatus::Exited(exit_pid, 23)
    );

    let signal_child = Command::new("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let signal_pid = nix::unistd::Pid::from_raw(i32::try_from(signal_child.id()).unwrap());
    let signal_pidfd = open_test_pidfd(signal_pid);
    drop(signal_child);
    send_pidfd_signal(signal_pidfd.as_fd(), Signal::SIGKILL).unwrap();
    assert_eq!(
        wait_for_pidfd(signal_pidfd.as_fd(), nix::sys::wait::WaitPidFlag::WEXITED).unwrap(),
        nix::sys::wait::WaitStatus::Signaled(signal_pid, Signal::SIGKILL, false)
    );
}

#[test]
fn valid_pidfd_cleanup_kills_and_reaps_without_numeric_wait() {
    let child = Command::new("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
    let pidfd = open_test_pidfd(pid);
    drop(child);

    ChildLifecycle::Pidfd { pid, pidfd }.cleanup().unwrap();
    assert_eq!(nix::sys::wait::waitpid(pid, None), Err(Errno::ECHILD));
}

#[test]
fn pidfd_reap_deadline_is_finite_and_leaves_authority_recoverable() {
    let child = Command::new("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
    let pidfd = open_test_pidfd(pid);
    drop(child);

    let error = wait_for_pidfd_reap(pidfd.as_fd(), Duration::ZERO).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(fcntl(pidfd.as_raw_fd(), FcntlArg::F_GETFD).is_ok());
    ChildLifecycle::Pidfd { pid, pidfd }.cleanup().unwrap();
}

#[test]
fn successful_cgroup_drain_retry_reaps_by_pidfd_and_restores_primary_failure() {
    let child = Command::new("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
    let pidfd = open_test_pidfd(pid);
    drop(child);

    let failure = ContainerRunError::ChildCleanupAfterFailure {
        primary: Box::new(ContainerRunError::UnknownExit),
        cleanup: io::Error::new(io::ErrorKind::TimedOut, "initial exact-child cleanup timed out"),
        pidfd: Some(ChildPidfdQuarantine::new(pidfd)),
    };
    assert!(matches!(
        failure.retry_child_cleanup_after_cgroup(),
        Err(ContainerRunError::UnknownExit)
    ));
    assert_eq!(nix::sys::wait::waitpid(pid, None), Err(Errno::ECHILD));
}

#[test]
fn already_reaped_pidfd_cleanup_accepts_only_the_authoritative_terminal_pair() {
    let mut child = Command::new("/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
    let pidfd = open_test_pidfd(pid);
    assert!(child.wait().unwrap().success());

    // The waited-on target makes pidfd_send_signal return ESRCH and the
    // matching P_PIDFD wait return ECHILD. That exact pair proves that the
    // pidfd target terminated and no waitable child remains.
    assert_eq!(send_pidfd_signal(pidfd.as_fd(), Signal::SIGKILL), Err(Errno::ESRCH));
    assert_eq!(
        wait_for_pidfd(pidfd.as_fd(), nix::sys::wait::WaitPidFlag::WEXITED),
        Err(Errno::ECHILD)
    );
    cleanup_pidfd_child(pidfd).unwrap();
    assert_eq!(nix::sys::wait::waitpid(pid, None), Err(Errno::ECHILD));
}

#[test]
fn dropping_unrecovered_pidfd_authority_aborts_an_isolated_process() {
    const CHILD_ENV: &str = "CONTAINER_PIDFD_FAIL_STOP_TEST_CHILD";
    if std::env::var_os(CHILD_ENV).as_deref() != Some(std::ffi::OsStr::new("1")) {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::dropping_unrecovered_pidfd_authority_aborts_an_isolated_process",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(nix::libc::SIGABRT),
            "dropping exact-child authority did not abort: {}; stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output
                .stderr
                .windows(b"dropping unrecovered exact-child pidfd authority".len())
                .any(|window| window == b"dropping unrecovered exact-child pidfd authority"),
            "fail-stop diagnostic missing from stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    // Reap a real child first so the subprocess leaves no orphan when Drop
    // intentionally aborts. The still-open descriptor is nevertheless a
    // real pidfd and exercises the exact fail-stop ownership boundary.
    let mut child = Command::new("/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
    let pidfd = open_test_pidfd(pid);
    assert!(child.wait().unwrap().success());
    drop(ChildPidfdQuarantine::new(pidfd));
    panic!("dropping unrecovered pidfd authority returned after fail-stop");
}

#[test]
fn invalid_pidfd_cleanup_never_falls_back_and_retains_authority() {
    let ordinary = open("/dev/null", OFlag::O_RDONLY | OFlag::O_CLOEXEC, Mode::empty()).unwrap();
    // SAFETY: open returned one fresh descriptor.
    let ordinary = unsafe { OwnedFd::from_raw_fd(ordinary) };
    let retained_raw = ordinary.as_raw_fd();
    let child = ChildLifecycle::Pidfd {
        pid: nix::unistd::Pid::from_raw(1),
        pidfd: ordinary,
    };

    let mut failure = child.cleanup_after_failure(ContainerRunError::UnknownExit);
    match &failure {
        ContainerRunError::ChildCleanupAfterFailure {
            primary,
            cleanup,
            pidfd,
        } => {
            assert!(matches!(primary.as_ref(), ContainerRunError::UnknownExit));
            assert!(cleanup.to_string().contains("pidfd_send_signal(SIGKILL) failed"));
            assert!(cleanup.to_string().contains("waitid(P_PIDFD, WNOHANG) failed"));
            assert_eq!(pidfd.as_ref().unwrap().as_fd().as_raw_fd(), retained_raw);
        }
        other => panic!("invalid pidfd did not retain structured cleanup authority: {other:?}"),
    }

    let retained = failure.take_child_pidfd().unwrap();
    assert_eq!(retained.as_fd().as_raw_fd(), retained_raw);
    assert!(fcntl(retained.as_fd().as_raw_fd(), FcntlArg::F_GETFD).is_ok());
    assert!(failure.take_child_pidfd().is_none());
    drop(retained.into_owned_fd());
}

#[test]
fn signal_override_restores_the_exact_previous_action() {
    extern "C" fn custom_handler(_: i32) {}

    let signal = Signal::SIGUSR2;
    let mut mask = SigSet::empty();
    mask.add(Signal::SIGUSR1);
    let custom = SigAction::new(SigHandler::Handler(custom_handler), SaFlags::SA_RESTART, mask);
    // SAFETY: custom is initialized and signal is valid. The original is
    // restored before this test returns.
    let original = unsafe { sigaction(signal, &custom).unwrap() };
    SignalOverride::install(signal).unwrap().restore().unwrap();
    // Install the original action while retrieving the action restored by
    // SignalOverride, so the test leaves process state unchanged.
    let restored = unsafe { sigaction(signal, &original).unwrap() };
    assert_eq!(restored.handler(), SigHandler::Handler(custom_handler));
    assert!(restored.flags().contains(SaFlags::SA_RESTART));
    assert!(restored.mask().contains(Signal::SIGUSR1));
}

#[test]
fn blocked_clone_signal_mask_restores_the_exact_previous_mask() {
    fn current_mask() -> nix::libc::sigset_t {
        // SAFETY: a null set pointer requests a read-only mask query and
        // current is a live output object.
        let mut current = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { nix::libc::pthread_sigmask(nix::libc::SIG_SETMASK, std::ptr::null(), &mut current) },
            0
        );
        current
    }

    let before = current_mask();
    let mut blocked = BlockedSignalMask::block_all().unwrap();
    let during = current_mask();
    // SAFETY: both sets are initialized and SIGUSR1 is a valid signal.
    assert_eq!(unsafe { nix::libc::sigismember(&during, nix::libc::SIGUSR1) }, 1);
    blocked.restore().unwrap();
    let after = current_mask();
    // Linux x86_64 exposes signal numbers 1 through 64. The container
    // seccomp and clone3 paths are intentionally restricted to that ABI.
    for signal in 1..=64 {
        // SAFETY: signal spans the Linux signal range and both masks were
        // initialized by pthread_sigmask.
        assert_eq!(
            unsafe { nix::libc::sigismember(&before, signal) },
            unsafe { nix::libc::sigismember(&after, signal) },
            "signal {signal} mask membership changed"
        );
    }
}

#[test]
fn raw_clone_child_guard_can_retain_blocked_mask_until_exit() {
    // SAFETY: a null set pointer requests a read-only mask query.
    let mut before = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { nix::libc::pthread_sigmask(nix::libc::SIG_SETMASK, std::ptr::null(), &mut before) },
        0
    );

    let mut blocked = BlockedSignalMask::block_all().unwrap();
    blocked.retain_blocked_on_drop();
    drop(blocked);
    // SAFETY: current is a live output object.
    let mut current = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { nix::libc::pthread_sigmask(nix::libc::SIG_SETMASK, std::ptr::null(), &mut current) },
        0
    );
    // Restore before asserting so a failed assertion cannot leak the
    // intentionally retained mask into this libtest worker.
    assert_eq!(
        unsafe { nix::libc::pthread_sigmask(nix::libc::SIG_SETMASK, &before, std::ptr::null_mut()) },
        0
    );
    // SAFETY: current is initialized and SIGUSR1 is valid.
    assert_eq!(unsafe { nix::libc::sigismember(&current, nix::libc::SIGUSR1) }, 1);
}

#[test]
fn signal_overrides_are_serialized_across_concurrent_runs() {
    use std::sync::mpsc;
    use std::time::Duration;

    let (first_installed_tx, first_installed_rx) = mpsc::channel();
    let (release_first_tx, release_first_rx) = mpsc::channel();
    let first = std::thread::spawn(move || {
        let override_ = SignalOverride::install(Signal::SIGWINCH).unwrap();
        first_installed_tx.send(()).unwrap();
        release_first_rx.recv().unwrap();
        override_.restore().unwrap();
    });
    first_installed_rx.recv().unwrap();

    let (second_attempting_tx, second_attempting_rx) = mpsc::channel();
    let (second_installed_tx, second_installed_rx) = mpsc::channel();
    let second = std::thread::spawn(move || {
        second_attempting_tx.send(()).unwrap();
        let override_ = SignalOverride::install(Signal::SIGURG).unwrap();
        second_installed_tx.send(()).unwrap();
        override_.restore().unwrap();
    });
    second_attempting_rx.recv().unwrap();
    assert!(second_installed_rx.recv_timeout(Duration::from_millis(50)).is_err());

    release_first_tx.send(()).unwrap();
    second_installed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    first.join().unwrap();
    second.join().unwrap();
}


