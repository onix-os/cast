use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use fs_err as fs;
use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl, open};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, Signal, sigaction};
use nix::sys::signalfd::SigSet;
use nix::sys::stat::Mode;
use nix::unistd::{mkfifo, read};

use super::{
    AnchoredLocator, AnchoredLocatorComponent, AnchoredLocatorError, AnchoredMountTargetKind, Bind, BindSource,
    BlockedSignalMask, CapabilityData, ChildLifecycle, ChildPidfdQuarantine, Container, ContainerError, DevPolicy,
    Error as ContainerRunError, LoopbackPolicy, MAX_CHILD_ERROR_BYTES, MAX_LINUX_CAPABILITY_NUMBER,
    MINIMAL_DEV_IDENTITIES, MINIMAL_DEV_NODES, Message, PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, PR_CAPBSET_READ,
    PreparedAnchoredMount, ProcPolicy, PseudoFilesystemPolicy, PseudoMountDecision, RootFilesystemPolicy,
    RootMountDecision, SignalOverride, SyncSocket, SysPolicy, TMPFS_MAGIC, TmpPolicy, TmpfsLimitReadback, TmpfsLimits,
    TmpfsLimitsError, authenticate_anchored_inputs, capability_is_set, checked_prctl_value, cleanup_pidfd_child,
    close_sync_endpoint, contain_raw_clone_child_panic, descriptor_stat, duplicate_cloexec, namespace_flags,
    normalized_anchored_mount_target, open_anchored_mount_target, open_anchored_resolver_target, prctl,
    prepare_bind_target, prepare_pseudo_mount_targets, pseudo_mount_decisions, read_capabilities, read_child_error,
    require_atomic_cgroup_bind_policy, require_atomic_cgroup_policy, resolver_stat_stable, root_mount_decisions,
    sealed_resolver_file, send_packet_no_signal, send_pidfd_signal, set_mount_access, standard_descriptor_is_unsafe,
    supported_capability_numbers, validate_anchored_bind_inputs, validate_anchored_mount_topology,
    validate_minimal_device_source, validate_resolver_target, validate_tmpfs_limit_readback, verify_tmpfs_limits,
    wait_for_pidfd, wait_for_pidfd_reap,
};

fn open_path_directory(path: &Path) -> OwnedFd {
    let descriptor = open(
        path,
        OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    // SAFETY: successful open returned a fresh owned descriptor.
    unsafe { OwnedFd::from_raw_fd(descriptor) }
}

fn open_path_file(path: &Path) -> OwnedFd {
    let descriptor = open(path, OFlag::O_PATH | OFlag::O_CLOEXEC, Mode::empty()).unwrap();
    // SAFETY: successful open returned a fresh owned descriptor.
    unsafe { OwnedFd::from_raw_fd(descriptor) }
}

fn exact_locator(path: &Path, witness: &impl std::os::fd::AsRawFd) -> AnchoredLocator {
    AnchoredLocator::exact(path, witness).unwrap()
}

fn anchored_container(path: &Path, witness: &impl std::os::fd::AsRawFd) -> Container {
    Container::new_anchored(exact_locator(path, witness)).unwrap()
}

fn open_test_pidfd(pid: nix::unistd::Pid) -> OwnedFd {
    // SAFETY: pidfd_open receives one live child PID and zero reserved
    // flags. This test helper does not participate in production clone3,
    // which receives its pidfd atomically from CLONE_PIDFD.
    let descriptor = unsafe { nix::libc::syscall(nix::libc::SYS_pidfd_open, pid.as_raw(), 0_u32) };
    assert!(descriptor >= 0, "pidfd_open test child: {}", io::Error::last_os_error());
    let descriptor = i32::try_from(descriptor).expect("pidfd fits RawFd");
    // SAFETY: successful pidfd_open returned one fresh descriptor.
    unsafe { OwnedFd::from_raw_fd(descriptor) }
}

fn skip_activation_capability_denial(test: &str, classification: Option<&str>, error: &ContainerRunError) -> bool {
    let Some(classification) = classification else {
        return false;
    };
    if !activation_capability_skip_allowed(std::env::var_os("CONTAINER_REQUIRE_ANCHORED_ACTIVATION").as_deref()) {
        return false;
    }
    eprintln!("SKIP {test}: required host capability unavailable: {classification}: {error}");
    true
}

fn activation_capability_skip_allowed(required: Option<&std::ffi::OsStr>) -> bool {
    required != Some(std::ffi::OsStr::new("1"))
}

fn assert_live_tmpfs_normalization_rejected(
    result: Result<(), ContainerRunError>,
    root: &Path,
    requested_size: u64,
    normalized_size: u64,
    inode_limit: u64,
    anchored: bool,
    test: &str,
) {
    let expected = format!(
        "tmpfs at tmp normalized declared limits: size {requested_size} -> {normalized_size} bytes, inodes {inode_limit} -> {inode_limit}"
    );
    match result {
        Err(ContainerRunError::Failure { message }) if message == expected => {}
        Err(error) => {
            let classification = if anchored {
                classify_anchored_activation_unavailable(&error, root)
            } else {
                None
            }
            .or_else(|| classify_bounded_tmpfs_activation_unavailable(&error, root));
            if skip_activation_capability_denial(test, classification, &error) {
                return;
            }
            panic!("{test} failed: {error}");
        }
        Ok(()) => panic!("{test} accepted a tmpfs limit the kernel must normalize"),
    }
}

fn classify_anchored_activation_unavailable(error: &ContainerRunError, label: &Path) -> Option<&'static str> {
    if host_denied_user_namespace_setup(error) {
        return Some("user-namespace setup denied");
    }
    let ContainerRunError::Failure { message } = error else {
        return None;
    };
    let unavailable = [
        "EPERM: Operation not permitted",
        "EACCES: Permission denied",
        "ENOSYS: Function not implemented",
    ];
    if unavailable
        .iter()
        .any(|denied| message == &format!("mount /: {denied}"))
    {
        return Some("private mount namespace unavailable");
    }
    let capability_denial = unavailable.iter().any(|denied| message.contains(denied));
    if message.starts_with("clone sealed resolver mount through anchored root descriptor:") && capability_denial {
        return Some("detached resolver mounts unavailable");
    }
    if message.starts_with("make sealed resolver mount read-only through anchored root descriptor:")
        && capability_denial
    {
        return Some("resolver mount attributes unavailable");
    }
    if message.starts_with("attach sealed resolver mount through anchored root descriptor:") && capability_denial {
        return Some("resolver mount attachment unavailable");
    }
    if message.starts_with("clone descriptor-backed bind mount for anchored source ")
        && unavailable.iter().any(|denied| message.ends_with(denied))
    {
        return Some("open_tree unavailable");
    }
    for denied in unavailable {
        if message
            == &format!(
                "clone descriptor-backed root mount for anchored root {}: {denied}",
                label.display()
            )
        {
            return Some("open_tree unavailable");
        }
        if message
            == &format!(
                "attach descriptor-backed root mount for anchored root {}: {denied}",
                label.display()
            )
        {
            return Some("move_mount unavailable");
        }
    }
    None
}

fn classify_bounded_tmpfs_activation_unavailable(error: &ContainerRunError, root: &Path) -> Option<&'static str> {
    if host_denied_user_namespace_setup(error) {
        return Some("user-namespace setup denied");
    }
    let ContainerRunError::Failure { message } = error else {
        return None;
    };
    for denied in [
        "EPERM: Operation not permitted",
        "EACCES: Permission denied",
        "ENOSYS: Function not implemented",
    ] {
        if message == &format!("mount /: {denied}") {
            return Some("private mount namespace unavailable");
        }
        if message == &format!("mount {}: {denied}", root.display()) {
            return Some("recursive read-only mount attributes unavailable");
        }
        if message == &format!("mount tmp: {denied}") {
            return Some("bounded tmpfs mount unavailable");
        }
    }
    None
}

fn classify_minimal_dev_activation_unavailable(error: &ContainerRunError, root: &Path) -> Option<&'static str> {
    if let Some(classification) = classify_bounded_tmpfs_activation_unavailable(error, root) {
        return Some(classification);
    }
    let ContainerRunError::Failure { message } = error else {
        return None;
    };
    for denied in [
        "EPERM: Operation not permitted",
        "EACCES: Permission denied",
        "ENOSYS: Function not implemented",
    ] {
        if message == &format!("mount dev: {denied}") {
            return Some("minimal device tmpfs or mount attributes unavailable");
        }
        for device in MINIMAL_DEV_NODES {
            if message == &format!("mount /dev/{device}: {denied}")
                || message == &format!("mount /old_root/dev/{device}: {denied}")
                || message == &format!("open anchored mount source /old_root/dev/{device}: {denied}")
            {
                return Some("authenticated minimal device bind unavailable");
            }
        }
    }
    None
}

fn require_errno<T>(result: io::Result<T>, expected: Errno, operation: &str) -> io::Result<()> {
    match result {
        Err(error) if io_error_matches_errno(&error, expected) => Ok(()),
        Err(error) => Err(io::Error::other(format!(
            "{operation} failed with {error}, expected {expected}"
        ))),
        Ok(_) => Err(io::Error::other(format!(
            "{operation} unexpectedly succeeded, expected {expected}"
        ))),
    }
}

fn io_error_matches_errno(error: &io::Error, expected: Errno) -> bool {
    if error.raw_os_error() == Some(expected as i32) {
        return true;
    }

    // fs_err preserves ErrorKind while wrapping the original OS error with
    // path context, so raw_os_error() is deliberately unavailable. EROFS has
    // its own exact ErrorKind and is the only contextualized error admitted by
    // this helper's callers; do not soften EPERM/EACCES or other errno pairs.
    expected == Errno::EROFS && error.kind() == io::ErrorKind::ReadOnlyFilesystem
}

fn exercise_bounded_tmpfs(size_bytes: u64, inode_limit: u64) -> io::Result<()> {
    let tmp = std::fs::File::open("/tmp")?;
    let mut stat: nix::libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { nix::libc::fstatfs(tmp.as_raw_fd(), &mut stat) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let observed_size = u64::try_from(stat.f_bsize)
        .ok()
        .and_then(|block_size| block_size.checked_mul(stat.f_blocks))
        .ok_or_else(|| io::Error::other("tmpfs byte readback overflow"))?;
    if observed_size != size_bytes || stat.f_files != inode_limit {
        return Err(io::Error::other(format!(
            "tmpfs readback mismatch: size={observed_size}, inodes={}",
            stat.f_files
        )));
    }

    let available_inodes = stat.f_ffree;
    if available_inodes == 0 || available_inodes >= inode_limit {
        return Err(io::Error::other(format!(
            "unexpected available tmpfs inode count {available_inodes} of {inode_limit}"
        )));
    }
    for index in 0..available_inodes {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(format!("/tmp/inode-{index}"))?;
    }
    require_errno(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open("/tmp/inode-over-limit"),
        Errno::ENOSPC,
        "allocate tmpfs inode N+1",
    )?;
    for index in 0..available_inodes {
        std::fs::remove_file(format!("/tmp/inode-{index}"))?;
    }

    let mut bytes = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open("/tmp/byte-limit")?;
    bytes.write_all(&vec![0_u8; usize::try_from(size_bytes).unwrap()])?;
    if bytes.metadata()?.len() != size_bytes {
        return Err(io::Error::other("tmpfs accepted fewer than N declared bytes"));
    }
    require_errno(bytes.write_all(&[1]), Errno::ENOSPC, "allocate tmpfs byte N+1")
}

fn exercise_read_only_minimal_dev() -> io::Result<()> {
    let mut actual = std::fs::read_dir("/dev")?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    actual.sort();
    let mut expected = MINIMAL_DEV_NODES
        .iter()
        .map(|name| std::ffi::OsString::from(*name))
        .collect::<Vec<_>>();
    expected.sort();
    if actual != expected {
        return Err(io::Error::other(format!(
            "minimal /dev entries differ: expected {expected:?}, found {actual:?}"
        )));
    }

    require_errno(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open("/dev/extra"),
        Errno::EROFS,
        "create undeclared minimal /dev entry",
    )?;

    // Match Python's `Path(os.devnull).open("wb")`: its create-and-truncate
    // flags must retain ordinary character-device data semantics even though
    // the bind mount itself is read-only against inode metadata mutation.
    let mut null = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open("/dev/null")?;
    null.write_all(b"discarded")?;
    drop(null);

    let mut null = std::fs::File::open("/dev/null")?;
    let mut byte = [0_u8; 1];
    if null.read(&mut byte)? != 0 {
        return Err(io::Error::other("/dev/null did not return EOF"));
    }

    let mut zero = std::fs::File::open("/dev/zero")?;
    let mut zeros = [1_u8; 16];
    zero.read_exact(&mut zeros)?;
    if zeros != [0_u8; 16] {
        return Err(io::Error::other("/dev/zero returned non-zero bytes"));
    }

    let mut full = std::fs::OpenOptions::new().write(true).open("/dev/full")?;
    require_errno(full.write_all(&[1]), Errno::ENOSPC, "write /dev/full")
}

fn require_payload_security_boundary() -> io::Result<()> {
    let capabilities = read_capabilities().map_err(errno_to_io)?;
    for capability in supported_capability_numbers().map_err(errno_to_io)? {
        if capability_is_set(&capabilities, capability) {
            return Err(io::Error::other(format!(
                "capability {capability} remains in a live payload set"
            )));
        }
        let bounding =
            unsafe { checked_prctl_value(prctl(PR_CAPBSET_READ, capability, 0, 0, 0)).map_err(errno_to_io)? };
        let ambient = unsafe {
            checked_prctl_value(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, capability, 0, 0)).map_err(errno_to_io)?
        };
        if bounding != 0 || ambient != 0 {
            return Err(io::Error::other(format!(
                "capability {capability} remains recoverable: bounding={bounding}, ambient={ambient}"
            )));
        }
    }

    let policy = unsafe { nix::libc::sched_getscheduler(0) };
    if policy != nix::libc::SCHED_OTHER {
        return Err(io::Error::other(format!(
            "payload scheduler policy is {policy}, not SCHED_OTHER"
        )));
    }
    let mut limit = nix::libc::rlimit {
        rlim_cur: nix::libc::RLIM_INFINITY,
        rlim_max: nix::libc::RLIM_INFINITY,
    };
    if unsafe { nix::libc::getrlimit(nix::libc::RLIMIT_RTPRIO, &mut limit) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if limit.rlim_cur != 0 || limit.rlim_max != 0 {
        return Err(io::Error::other(format!(
            "payload RLIMIT_RTPRIO remains {}/{}",
            limit.rlim_cur, limit.rlim_max
        )));
    }

    let no_new_privileges = unsafe { prctl(nix::libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) };
    if no_new_privileges != 1 {
        return Err(if no_new_privileges == -1 {
            io::Error::last_os_error()
        } else {
            io::Error::other(format!(
                "payload PR_GET_NO_NEW_PRIVS returned {no_new_privileges}, expected 1"
            ))
        });
    }
    let seccomp_mode = unsafe { prctl(nix::libc::PR_GET_SECCOMP, 0, 0, 0, 0) };
    if seccomp_mode != 2 {
        return Err(if seccomp_mode == -1 {
            io::Error::last_os_error()
        } else {
            io::Error::other(format!(
                "payload PR_GET_SECCOMP returned {seccomp_mode}, expected filter mode 2"
            ))
        });
    }

    require_raw_syscall_errno(
        unsafe { nix::libc::syscall(nix::libc::SYS_clone3, std::ptr::null::<nix::libc::c_void>(), 0_usize) },
        Errno::ENOSYS,
        "clone3 under payload filter",
    )?;
    require_raw_syscall_errno(
        unsafe { nix::libc::syscall(nix::libc::SYS_unshare, 0_u64) },
        Errno::EPERM,
        "unshare under payload filter",
    )?;
    require_raw_syscall_errno(
        unsafe {
            nix::libc::syscall(
                nix::libc::SYS_mount,
                std::ptr::null::<nix::libc::c_void>(),
                std::ptr::null::<nix::libc::c_void>(),
                std::ptr::null::<nix::libc::c_void>(),
                0_u64,
                std::ptr::null::<nix::libc::c_void>(),
            )
        },
        Errno::EPERM,
        "mount under payload filter",
    )?;

    let thread = std::thread::Builder::new()
        .name("seccomp-clone-fallback".to_owned())
        .spawn(|| 0x5ec_c0de_u32)?;
    if thread
        .join()
        .map_err(|_| io::Error::other("payload pthread probe panicked"))?
        != 0x5ec_c0de
    {
        return Err(io::Error::other("payload pthread probe returned the wrong value"));
    }
    Ok(())
}

fn require_raw_syscall_errno(result: nix::libc::c_long, expected: Errno, operation: &str) -> io::Result<()> {
    if result != -1 {
        return Err(io::Error::other(format!(
            "{operation} unexpectedly returned {result}, expected {expected}"
        )));
    }
    let found = Errno::last();
    if found != expected {
        return Err(io::Error::other(format!(
            "{operation} failed with {found}, expected {expected}"
        )));
    }
    Ok(())
}

fn errno_to_io(error: Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

fn host_denied_user_namespace_setup(error: &ContainerRunError) -> bool {
    match error {
        ContainerRunError::CloneNamespaces {
            source: Errno::EPERM | Errno::EACCES | Errno::ENOSYS,
        } => true,
        ContainerRunError::Failure { message }
            if message.starts_with("clear inherited supplementary groups:")
                && message.contains("EPERM: Operation not permitted") =>
        {
            true
        }
        ContainerRunError::Idmap {
            source: super::idmap::Error::WriteUidMap { source } | super::idmap::Error::WriteGidMap { source },
        } => source.kind() == io::ErrorKind::PermissionDenied || source.raw_os_error() == Some(Errno::EPERM as i32),
        _ => false,
    }
}

include!("anchored_identity.rs");
include!("anchored_inputs.rs");
include!("policy_and_mounts.rs");
include!("live_activation.rs");
include!("security_contract.rs");
include!("process_supervision.rs");
