// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::addr_of_mut;
use std::sync::atomic::{AtomicI32, Ordering};

use fs_err::{self as fs, PathExt as _};
use nc::syscalls::syscall5;
use nc::{
    AT_EMPTY_PATH, AT_FDCWD, MOUNT_ATTR_RDONLY, MOVE_MOUNT_F_EMPTY_PATH, OPEN_TREE_CLOEXEC, OPEN_TREE_CLONE,
    SYS_MOUNT_SETATTR, mount_attr_t, move_mount, open_tree,
};
use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::libc::{
    AT_RECURSIVE, PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, PR_CAP_AMBIENT_IS_SET, PR_CAPBSET_DROP, PR_CAPBSET_READ,
    SIGCHLD, SYS_capget, SYS_capset, prctl, syscall,
};
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use nix::sched::{CloneFlags, clone};
use nix::sys::prctl::set_pdeathsig;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, Signal, kill, sigaction};
use nix::sys::signalfd::SigSet;
use nix::sys::stat::{Mode, umask};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{
    Pid, close, getegid, geteuid, getgid, getgroups, getuid, pipe2, pivot_root, read, setgroups, sethostname,
    tcsetpgrp, write,
};
use snafu::{ResultExt, Snafu};

use self::idmap::idmap;

mod idmap;

/// Typed policy for pseudo-filesystems mounted while entering a container.
///
/// The default preserves the historical container behavior: a writable proc,
/// an empty tmpfs at `/tmp`, and recursive writable host views of `/sys` and
/// `/dev`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PseudoFilesystemPolicy {
    pub proc: ProcPolicy,
    pub tmp: TmpPolicy,
    pub sys: SysPolicy,
    pub dev: DevPolicy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProcPolicy {
    None,
    ReadOnly,
    #[default]
    ReadWrite,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TmpPolicy {
    Disabled,
    /// Mount a fresh, empty tmpfs at `/tmp`.
    #[default]
    Empty,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SysPolicy {
    None,
    HostReadOnly,
    #[default]
    HostReadWrite,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DevPolicy {
    None,
    HostReadOnly,
    #[default]
    HostReadWrite,
    /// Mount a fresh tmpfs containing read-only host binds for exactly
    /// `/dev/null`, `/dev/zero`, and `/dev/full`.
    Minimal,
}

/// Exact device-node names exposed by [`DevPolicy::Minimal`]. No optional
/// nodes are added based on host state.
pub const MINIMAL_DEV_NODES: &[&str] = &["null", "zero", "full"];

/// Policy for changing the loopback interface before entering the root tree.
///
/// The default preserves the historical best-effort `/usr/sbin/ip` behavior.
/// Deterministic callers can leave the interface in its kernel-provided state
/// without consulting the host filesystem or executing a host utility.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoopbackPolicy {
    KernelDefault,
    #[default]
    HostIpIfAvailable,
}

/// Access policy for the container's root filesystem.
///
/// A read-only root is applied recursively after every bind has been mounted.
/// Only mounts declared with [`Container::bind_rw`] are then made writable
/// again. That exception applies to the exact bind mount, not to nested mounts;
/// each writable nested mount must be declared separately. This keeps
/// undeclared paths, including package-manager content and dependency trees,
/// immutable to the payload. Read-only setup also removes mount-administration
/// capability before the payload runs; pseudo-filesystems remain governed by
/// [`PseudoFilesystemPolicy`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RootFilesystemPolicy {
    ReadOnly,
    #[default]
    ReadWrite,
}

pub struct Container {
    root: PathBuf,
    work_dir: Option<PathBuf>,
    binds: Vec<Bind>,
    networking: bool,
    hostname: Option<String>,
    ignore_host_sigint: bool,
    pseudo_filesystems: PseudoFilesystemPolicy,
    loopback: LoopbackPolicy,
    root_filesystem: RootFilesystemPolicy,
}

impl Container {
    /// Create a new Container using the default options
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            work_dir: None,
            binds: vec![],
            networking: false,
            hostname: None,
            ignore_host_sigint: false,
            pseudo_filesystems: PseudoFilesystemPolicy::default(),
            loopback: LoopbackPolicy::default(),
            root_filesystem: RootFilesystemPolicy::default(),
        }
    }

    /// Override the working directory
    pub fn work_dir(self, work_dir: impl Into<PathBuf>) -> Self {
        Self {
            work_dir: Some(work_dir.into()),
            ..self
        }
    }

    /// Create a read-write bind mount
    pub fn bind_rw(mut self, host: impl Into<PathBuf>, guest: impl Into<PathBuf>) -> Self {
        self.binds.push(Bind {
            source: host.into(),
            target: guest.into(),
            read_only: false,
        });
        self
    }

    /// Create a read-only bind mount
    pub fn bind_ro(mut self, host: impl Into<PathBuf>, guest: impl Into<PathBuf>) -> Self {
        self.binds.push(Bind {
            source: host.into(),
            target: guest.into(),
            read_only: true,
        });
        self
    }

    /// Create a read-only bind mount only if the `host` path exists
    pub fn bind_ro_if_exists(mut self, host: impl Into<PathBuf>, guest: impl Into<PathBuf>) -> Self {
        let source = host.into();

        if source.exists() {
            self.binds.push(Bind {
                source,
                target: guest.into(),
                read_only: true,
            });
        }

        self
    }

    /// Configure networking availability
    pub fn networking(self, enabled: bool) -> Self {
        Self {
            networking: enabled,
            ..self
        }
    }

    /// Override hostname (via /etc/hostname)
    pub fn hostname(self, hostname: impl ToString) -> Self {
        Self {
            hostname: Some(hostname.to_string()),
            ..self
        }
    }

    /// Ignore `SIGINT` from the parent process. This allows it to be forwarded to a
    /// spawned process inside the container by using [`forward_sigint`].
    pub fn ignore_host_sigint(self, ignore: bool) -> Self {
        Self {
            ignore_host_sigint: ignore,
            ..self
        }
    }

    /// Select which pseudo-filesystems and host kernel trees are mounted into
    /// the container. Unselected paths remain as provided by the root tree.
    pub fn pseudo_filesystems(self, policy: PseudoFilesystemPolicy) -> Self {
        Self {
            pseudo_filesystems: policy,
            ..self
        }
    }

    /// Select whether container setup may invoke the optional host
    /// `/usr/sbin/ip` utility to raise the loopback interface.
    pub fn loopback(self, policy: LoopbackPolicy) -> Self {
        Self {
            loopback: policy,
            ..self
        }
    }

    /// Select whether undeclared paths in the container root are writable.
    pub fn root_filesystem(self, policy: RootFilesystemPolicy) -> Self {
        Self {
            root_filesystem: policy,
            ..self
        }
    }

    /// Run `f` as a container process payload
    pub fn run<E>(self, mut f: impl FnMut() -> Result<(), E>) -> Result<(), Error>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        static mut STACK: [u8; 4 * 1024 * 1024] = [0u8; 4 * 1024 * 1024];

        // Pipe to synchronize parent & child. Both ends must be close-on-exec:
        // the child retains the write end while running the Rust payload so it
        // can report setup/payload errors, but spawned commands must never
        // inherit that control descriptor.
        let mut sync = SyncPipe::new()?;
        let child_sync = sync.raw();

        let flags = namespace_flags(self.networking);

        let clone_cb = Box::new(|| match enter(&self, child_sync, &mut f) {
            Ok(_) => 0,
            // Write error back to parent process
            Err(error) => {
                let error = format_error(error);
                let mut pos = 0;

                while pos < error.len() {
                    let Ok(len) = write(child_sync.1, &error.as_bytes()[pos..]) else {
                        break;
                    };

                    pos += len;
                }

                let _ = close(child_sync.1);

                1
            }
        });
        let pid = unsafe { clone(clone_cb, &mut *addr_of_mut!(STACK), flags, Some(SIGCHLD)) }.context(NixSnafu)?;

        // Every build receives the same one-identity credential namespace:
        // namespace root maps to the caller and no other IDs exist.
        if let Err(source) = idmap(pid) {
            abort_child(pid);
            return Err(Error::Idmap { source });
        }

        // Allow child to continue
        match write(sync.write_fd(), &[Message::Continue as u8]) {
            Ok(1) => {}
            Ok(_) => {
                abort_child(pid);
                return Err(Error::Nix { source: Errno::EIO });
            }
            Err(source) => {
                abort_child(pid);
                return Err(Error::Nix { source });
            }
        }
        // Write no longer needed
        // Do not abandon a running child if close reports an error. Linux has
        // already released the descriptor even when `close` returns EINTR, so
        // retain the error and supervise the child to completion first.
        let close_write_error = sync.close_write().err();

        if self.ignore_host_sigint
            && let Err(source) = ignore_sigint()
        {
            abort_child(pid);
            return Err(Error::Nix { source });
        }

        let status = wait_for_child(pid).context(NixSnafu)?;

        if self.ignore_host_sigint {
            default_sigint().context(NixSnafu)?;
        }

        let result = match status {
            WaitStatus::Exited(_, 0) => Ok(()),
            WaitStatus::Exited(..) => {
                let mut error = String::new();
                let mut buffer = [0u8; 1024];

                loop {
                    let len = read(sync.read_fd(), &mut buffer).context(NixSnafu)?;

                    if len == 0 {
                        break;
                    }

                    error.push_str(String::from_utf8_lossy(&buffer[..len]).as_ref());
                }

                Err(Error::Failure { message: error })
            }
            WaitStatus::Signaled(_, signal, _) => Err(Error::Signaled { signal }),
            WaitStatus::Stopped(..)
            | WaitStatus::PtraceEvent(..)
            | WaitStatus::PtraceSyscall(_)
            | WaitStatus::Continued(_)
            | WaitStatus::StillAlive => {
                abort_child(pid);
                Err(Error::UnknownExit)
            }
        };

        match (result, close_write_error) {
            (Ok(()), Some(source)) => Err(Error::Nix { source }),
            (result, _) => result,
        }
    }
}

fn namespace_flags(networking: bool) -> CloneFlags {
    let mut flags = CloneFlags::CLONE_NEWNS
        | CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWIPC
        | CloneFlags::CLONE_NEWUTS
        | CloneFlags::CLONE_NEWUSER;
    if !networking {
        flags |= CloneFlags::CLONE_NEWNET;
    }
    flags
}

/// Reenter the container
fn enter<E>(container: &Container, sync: (i32, i32), mut f: impl FnMut() -> Result<(), E>) -> Result<(), ContainerError>
where
    E: std::error::Error + Send + Sync + 'static,
{
    // Ensure process is cleaned up if parent dies
    set_pdeathsig(Signal::SIGKILL).context(SetPDeathSigSnafu)?;

    // Wait for continue message
    let mut message = [0u8; 1];
    let len = read(sync.0, &mut message).context(ReadContinueMsgSnafu)?;
    if len != 1 || message[0] != Message::Continue as u8 {
        return InvalidContinueMsgSnafu.fail();
    }

    // Close unused read end
    close(sync.0).context(CloseReadFdSnafu)?;

    // The parent deliberately leaves setgroups enabled until this point.  A
    // rootless user namespace otherwise freezes the caller's ambient
    // supplementary groups into every build.  Drop them before any mount,
    // process, or package-analysis work can observe them, then prove the
    // namespace-visible identity is the fixed root credential contract.
    isolate_payload_credentials()?;

    setup(container)?;

    if matches!(container.root_filesystem, RootFilesystemPolicy::ReadOnly) {
        drop_mount_administration()?;
    }

    let result = f().boxed().context(RunSnafu);
    if result.is_ok() {
        // Errors retain the write end so the outer clone callback can report
        // them. A successful Rust payload has nothing left to report.
        let _ = close(sync.1);
    }
    result
}

fn isolate_payload_credentials() -> Result<(), ContainerError> {
    setgroups(&[]).context(ClearSupplementaryGroupsSnafu)?;
    let supplementary_gids = getgroups()
        .context(ReadSupplementaryGroupsSnafu)?
        .into_iter()
        .map(|gid| gid.as_raw())
        .collect::<Vec<_>>();
    validate_payload_credentials(
        getuid().as_raw(),
        geteuid().as_raw(),
        getgid().as_raw(),
        getegid().as_raw(),
        supplementary_gids,
    )
}

fn validate_payload_credentials(
    real_uid: u32,
    effective_uid: u32,
    real_gid: u32,
    effective_gid: u32,
    supplementary_gids: Vec<u32>,
) -> Result<(), ContainerError> {
    if real_uid != 0 || effective_uid != 0 || real_gid != 0 || effective_gid != 0 || !supplementary_gids.is_empty() {
        return UnexpectedPayloadCredentialsSnafu {
            real_uid,
            effective_uid,
            real_gid,
            effective_gid,
            supplementary_gids,
        }
        .fail();
    }
    Ok(())
}

const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
const CAP_SYS_ADMIN: u32 = 21;

#[repr(C)]
struct CapabilityHeader {
    version: u32,
    pid: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
struct CapabilityData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// Remove the one capability that could undo the frozen mount policy.
///
/// Clearing the live sets is not sufficient for namespace UID zero: a later
/// `execve` can regain capabilities from the bounding set. Drop the bounding
/// entry first, clear the ambient set, then clear and verify every live set.
fn drop_mount_administration() -> Result<(), ContainerError> {
    unsafe {
        checked_prctl(prctl(PR_CAPBSET_DROP, CAP_SYS_ADMIN, 0, 0, 0)).context(DropMountAdministrationSnafu)?;
        checked_prctl(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0))
            .context(DropMountAdministrationSnafu)?;
    }

    let mut capabilities = read_capabilities().context(DropMountAdministrationSnafu)?;
    clear_capability(&mut capabilities, CAP_SYS_ADMIN);
    write_capabilities(&capabilities).context(DropMountAdministrationSnafu)?;

    let retained_live = capability_is_set(
        &read_capabilities().context(DropMountAdministrationSnafu)?,
        CAP_SYS_ADMIN,
    );
    let retained_bounding = unsafe {
        checked_prctl_value(prctl(PR_CAPBSET_READ, CAP_SYS_ADMIN, 0, 0, 0)).context(DropMountAdministrationSnafu)? != 0
    };
    let retained_ambient = unsafe {
        checked_prctl_value(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, CAP_SYS_ADMIN, 0, 0))
            .context(DropMountAdministrationSnafu)?
            != 0
    };
    if retained_live || retained_bounding || retained_ambient {
        return PayloadRetainsMountAdministrationSnafu.fail();
    }
    Ok(())
}

fn read_capabilities() -> Result<[CapabilityData; 2], Errno> {
    let mut header = CapabilityHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapabilityData::default(); 2];
    let result = unsafe { syscall(SYS_capget, &mut header, data.as_mut_ptr()) };
    checked_syscall(result)?;
    Ok(data)
}

fn write_capabilities(data: &[CapabilityData; 2]) -> Result<(), Errno> {
    let mut header = CapabilityHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let result = unsafe { syscall(SYS_capset, &mut header, data.as_ptr()) };
    checked_syscall(result)
}

fn clear_capability(data: &mut [CapabilityData; 2], capability: u32) {
    let word = capability as usize / u32::BITS as usize;
    let mask = !(1_u32 << (capability % u32::BITS));
    data[word].effective &= mask;
    data[word].permitted &= mask;
    data[word].inheritable &= mask;
}

fn capability_is_set(data: &[CapabilityData; 2], capability: u32) -> bool {
    let word = capability as usize / u32::BITS as usize;
    let mask = 1_u32 << (capability % u32::BITS);
    data[word].effective & mask != 0 || data[word].permitted & mask != 0 || data[word].inheritable & mask != 0
}

fn checked_syscall(result: nix::libc::c_long) -> Result<(), Errno> {
    if result == -1 { Err(Errno::last()) } else { Ok(()) }
}

fn checked_prctl(result: nix::libc::c_int) -> Result<(), Errno> {
    checked_prctl_value(result).map(|_| ())
}

fn checked_prctl_value(result: nix::libc::c_int) -> Result<nix::libc::c_int, Errno> {
    if result == -1 { Err(Errno::last()) } else { Ok(result) }
}

/// Setup the container
fn setup(container: &Container) -> Result<(), ContainerError> {
    if container.networking {
        setup_networking(&container.root)?;
    }

    if matches!(container.loopback, LoopbackPolicy::HostIpIfAvailable) {
        setup_localhost()?;
    }

    pivot(
        &container.root,
        &container.binds,
        container.pseudo_filesystems,
        container.root_filesystem,
    )?;

    if let Some(hostname) = &container.hostname {
        sethostname(hostname).context(SetHostnameSnafu)?;
    }

    if let Some(dir) = &container.work_dir {
        set_current_dir(dir)?;
    }

    Ok(())
}

/// Pivot the process into the rootfs
fn pivot(
    root: &Path,
    binds: &[Bind],
    pseudo_filesystems: PseudoFilesystemPolicy,
    root_filesystem: RootFilesystemPolicy,
) -> Result<(), ContainerError> {
    const OLD_PATH: &str = "old_root";

    let old_root = root.join(OLD_PATH);

    add_mount(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE)?;
    add_mount(Some(root), root, None, MsFlags::MS_BIND)?;

    for bind in binds {
        let source = bind.source.fs_err_canonicalize().context(FsErrSnafu)?;
        let target = root.join(bind.target.strip_prefix("/").unwrap_or(&bind.target));

        bind_mount(&source, &target, bind.read_only)?;
    }

    ensure_directory(&old_root)?;
    for decision in root_mount_decisions(root, binds, root_filesystem) {
        match decision {
            RootMountDecision::ReadOnlyRecursive(target) => set_mount_access(&target, true, true)?,
            RootMountDecision::ReadWriteExact(target) => set_mount_access(&target, false, false)?,
        }
    }
    pivot_root(root, &old_root).context(PivotRootSnafu)?;

    set_current_dir("/")?;

    for decision in pseudo_mount_decisions(pseudo_filesystems) {
        apply_pseudo_mount(decision, OLD_PATH)?;
    }

    umount2(OLD_PATH, MntFlags::MNT_DETACH).context(UnmountOldRootSnafu)?;
    if matches!(root_filesystem, RootFilesystemPolicy::ReadWrite) {
        fs::remove_dir(OLD_PATH).context(FsErrSnafu)?;
    }

    umask(Mode::S_IWGRP | Mode::S_IWOTH);

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RootMountDecision {
    ReadOnlyRecursive(PathBuf),
    ReadWriteExact(PathBuf),
}

fn root_mount_decisions(root: &Path, binds: &[Bind], policy: RootFilesystemPolicy) -> Vec<RootMountDecision> {
    if matches!(policy, RootFilesystemPolicy::ReadWrite) {
        return Vec::new();
    }

    std::iter::once(RootMountDecision::ReadOnlyRecursive(root.to_owned()))
        .chain(binds.iter().filter(|bind| !bind.read_only).map(|bind| {
            RootMountDecision::ReadWriteExact(root.join(bind.target.strip_prefix("/").unwrap_or(&bind.target)))
        }))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PseudoMountDecision {
    Proc { read_only: bool },
    EmptyTmp,
    HostSys { read_only: bool },
    HostDev { read_only: bool },
    MinimalDev,
}

fn pseudo_mount_decisions(policy: PseudoFilesystemPolicy) -> Vec<PseudoMountDecision> {
    let mut decisions = Vec::with_capacity(4);
    match policy.proc {
        ProcPolicy::None => {}
        ProcPolicy::ReadOnly => decisions.push(PseudoMountDecision::Proc { read_only: true }),
        ProcPolicy::ReadWrite => decisions.push(PseudoMountDecision::Proc { read_only: false }),
    }
    if matches!(policy.tmp, TmpPolicy::Empty) {
        decisions.push(PseudoMountDecision::EmptyTmp);
    }
    match policy.sys {
        SysPolicy::None => {}
        SysPolicy::HostReadOnly => decisions.push(PseudoMountDecision::HostSys { read_only: true }),
        SysPolicy::HostReadWrite => decisions.push(PseudoMountDecision::HostSys { read_only: false }),
    }
    match policy.dev {
        DevPolicy::None => {}
        DevPolicy::HostReadOnly => decisions.push(PseudoMountDecision::HostDev { read_only: true }),
        DevPolicy::HostReadWrite => decisions.push(PseudoMountDecision::HostDev { read_only: false }),
        DevPolicy::Minimal => decisions.push(PseudoMountDecision::MinimalDev),
    }
    decisions
}

fn apply_pseudo_mount(decision: PseudoMountDecision, old_path: &str) -> Result<(), ContainerError> {
    match decision {
        PseudoMountDecision::Proc { read_only } => add_mount(
            Some(Path::new("proc")),
            Path::new("proc"),
            Some("proc"),
            if read_only {
                MsFlags::MS_RDONLY
            } else {
                MsFlags::empty()
            },
        ),
        PseudoMountDecision::EmptyTmp => add_mount(
            Some(Path::new("tmpfs")),
            Path::new("tmp"),
            Some("tmpfs"),
            MsFlags::empty(),
        ),
        PseudoMountDecision::HostSys { read_only } => mount_host_tree(old_path, "sys", read_only),
        PseudoMountDecision::HostDev { read_only } => mount_host_tree(old_path, "dev", read_only),
        PseudoMountDecision::MinimalDev => mount_minimal_dev(old_path),
    }
}

fn mount_host_tree(old_path: &str, name: &str, read_only: bool) -> Result<(), ContainerError> {
    let source = Path::new("/").join(old_path).join(name);
    let target = Path::new(name);
    add_mount(
        Some(source.as_path()),
        target,
        None,
        MsFlags::MS_BIND | MsFlags::MS_REC | MsFlags::MS_SLAVE,
    )?;
    if read_only {
        set_mount_access(target, true, true)?;
    }
    Ok(())
}

fn mount_minimal_dev(old_path: &str) -> Result<(), ContainerError> {
    add_mount(
        Some(Path::new("tmpfs")),
        Path::new("dev"),
        Some("tmpfs"),
        MsFlags::empty(),
    )?;
    for device in MINIMAL_DEV_NODES {
        bind_minimal_device(old_path, device)?;
    }
    Ok(())
}

fn bind_minimal_device(old_path: &str, device: &str) -> Result<(), ContainerError> {
    let source = Path::new("/").join(old_path).join("dev").join(device);
    let target = Path::new("dev").join(device);
    bind_mount(&source, &target, true)
}

fn set_mount_access(target: &Path, read_only: bool, recursive: bool) -> Result<(), ContainerError> {
    unsafe {
        let inner = || {
            let fd = open_tree(AT_FDCWD, target, OPEN_TREE_CLOEXEC).map_err(Errno::from_i32)?;
            let attr = mount_attr_t {
                attr_set: if read_only { MOUNT_ATTR_RDONLY as u64 } else { 0 },
                attr_clr: if read_only { 0 } else { MOUNT_ATTR_RDONLY as u64 },
                program: 0,
                userns_fd: 0,
            };
            let flags = AT_EMPTY_PATH as usize | if recursive { AT_RECURSIVE as usize } else { 0 };
            let result = syscall5(
                SYS_MOUNT_SETATTR,
                fd as usize,
                c"".as_ptr() as usize,
                flags,
                &attr as *const mount_attr_t as usize,
                size_of::<mount_attr_t>(),
            )
            .map_err(Errno::from_i32);
            let close_result = close(fd);

            result?;
            close_result?;
            Ok(())
        };

        inner().context(MountSnafu {
            target: target.to_owned(),
        })
    }
}

fn setup_networking(root: &Path) -> Result<(), ContainerError> {
    ensure_directory(root.join("etc"))?;
    fs::copy("/etc/resolv.conf", root.join("etc/resolv.conf")).context(FsErrSnafu)?;
    Ok(())
}

fn setup_localhost() -> Result<(), ContainerError> {
    // TODO: maybe it's better to hunt down the API to do this instead?
    if PathBuf::from("/usr/sbin/ip").exists() {
        Command::new("/usr/sbin/ip")
            .args(["link", "set", "lo", "up"])
            .output()
            .context(SetupLocalhostSnafu)?;
    }
    Ok(())
}

fn ensure_directory(path: impl AsRef<Path>) -> Result<(), ContainerError> {
    let path = path.as_ref();
    if !path.exists() {
        fs::create_dir_all(path).context(FsErrSnafu)?;
    }
    Ok(())
}

fn ensure_empty_file(path: impl AsRef<Path>) -> Result<(), ContainerError> {
    let path = path.as_ref();
    if !path.exists() {
        fs::File::create_new(path).context(FsErrSnafu)?;
    }
    Ok(())
}

fn prepare_bind_target(source: &Path, target: &Path) -> Result<(), ContainerError> {
    let metadata = fs::metadata(source).context(FsErrSnafu)?;
    if metadata.is_dir() {
        ensure_directory(target)?;
    } else {
        if let Some(parent) = target.parent() {
            ensure_directory(parent)?;
        }

        ensure_empty_file(target)?;
    }
    Ok(())
}

fn bind_mount(source: &Path, target: &Path, read_only: bool) -> Result<(), ContainerError> {
    prepare_bind_target(source, target)?;

    unsafe {
        let inner = || {
            // Bind mount to fd
            let fd = open_tree(AT_FDCWD, source, OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC).map_err(Errno::from_i32)?;

            let result = (|| {
                // Set rd flag if applicable
                if read_only {
                    let attr = mount_attr_t {
                        attr_set: MOUNT_ATTR_RDONLY as u64,
                        attr_clr: 0,
                        program: 0,
                        userns_fd: 0,
                    };
                    syscall5(
                        SYS_MOUNT_SETATTR,
                        fd as usize,
                        c"".as_ptr() as usize,
                        AT_EMPTY_PATH as usize,
                        &attr as *const mount_attr_t as usize,
                        size_of::<mount_attr_t>(),
                    )
                    .map_err(Errno::from_i32)?;
                }

                // Move detached mount to target
                move_mount(fd, Path::new(""), AT_FDCWD, target, MOVE_MOUNT_F_EMPTY_PATH).map_err(Errno::from_i32)
            })();
            let close_result = close(fd);

            result?;
            close_result?;
            Ok(())
        };

        inner().context(MountSnafu {
            target: target.to_owned(),
        })
    }
}

fn add_mount<T: AsRef<Path>>(
    source: Option<T>,
    target: T,
    fs_type: Option<&str>,
    flags: MsFlags,
) -> Result<(), ContainerError> {
    let target = target.as_ref();
    ensure_directory(target)?;
    mount(
        source.as_ref().map(AsRef::as_ref),
        target,
        fs_type,
        flags,
        Option::<&str>::None,
    )
    .context(MountSnafu {
        target: target.to_owned(),
    })?;
    Ok(())
}

fn set_current_dir(path: impl AsRef<Path>) -> Result<(), ContainerError> {
    let path = path.as_ref();
    std::env::set_current_dir(path).with_context(|_| SetCurrentDirSnafu { path: path.to_owned() })
}

fn ignore_sigint() -> Result<(), nix::Error> {
    let action = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
    unsafe { sigaction(Signal::SIGINT, &action)? };
    Ok(())
}

fn default_sigint() -> Result<(), nix::Error> {
    let action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
    unsafe { sigaction(Signal::SIGINT, &action)? };
    Ok(())
}

fn wait_for_child(pid: Pid) -> Result<WaitStatus, nix::Error> {
    loop {
        match waitpid(pid, None) {
            Err(Errno::EINTR) => {}
            result => return result,
        }
    }
}

fn abort_child(pid: Pid) {
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

fn format_error(error: impl std::error::Error) -> String {
    let sources = sources(&error);
    sources.join(": ")
}

fn sources(error: &dyn std::error::Error) -> Vec<String> {
    let mut sources = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source.take() {
        sources.push(error.to_string());
        source = error.source();
    }
    sources
}

/// Parent-owned synchronization descriptors. The child receives raw copies
/// after `clone`; this guard closes every parent copy on both ordinary and
/// early-return paths.
struct SyncPipe {
    read: Option<RawFd>,
    write: Option<RawFd>,
}

impl SyncPipe {
    fn new() -> Result<Self, Error> {
        let (read, write) = pipe2(OFlag::O_CLOEXEC).context(NixSnafu)?;
        Ok(Self {
            read: Some(read),
            write: Some(write),
        })
    }

    fn raw(&self) -> (RawFd, RawFd) {
        (self.read_fd(), self.write_fd())
    }

    fn read_fd(&self) -> RawFd {
        self.read.expect("sync pipe read end must remain open")
    }

    fn write_fd(&self) -> RawFd {
        self.write.expect("sync pipe write end must remain open")
    }

    fn close_write(&mut self) -> Result<(), nix::Error> {
        let fd = self.write.take().expect("sync pipe write end must remain open");
        close(fd)
    }
}

impl Drop for SyncPipe {
    fn drop(&mut self) {
        if let Some(fd) = self.read.take() {
            let _ = close(fd);
        }
        if let Some(fd) = self.write.take() {
            let _ = close(fd);
        }
    }
}

struct Bind {
    source: PathBuf,
    target: PathBuf,
    read_only: bool,
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("exited with failure: {message}"))]
    Failure { message: String },
    #[snafu(display("stopped by signal: {signal}"))]
    Signaled { signal: Signal },
    #[snafu(display("unknown exit reason"))]
    UnknownExit,
    #[snafu(display("error setting up isolated-root credential map"))]
    Idmap { source: idmap::Error },
    // FIXME: Replace with more fine-grained variants
    #[snafu(display("nix"))]
    Nix { source: nix::Error },
}

#[derive(Debug, Snafu)]
enum ContainerError {
    #[snafu(display("run"))]
    Run {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[snafu(display("set current dir"))]
    SetCurrentDirError { path: PathBuf, source: io::Error },
    #[snafu(display("setup localhost"))]
    SetupLocalhost { source: io::Error },
    #[snafu(display("set_pdeathsig"))]
    SetPDeathSig { source: nix::Error },
    #[snafu(display("wait for continue message"))]
    ReadContinueMsg { source: nix::Error },
    #[snafu(display("invalid continue message"))]
    InvalidContinueMsg,
    #[snafu(display("close read end of pipe"))]
    CloseReadFd { source: nix::Error },
    #[snafu(display("clear inherited supplementary groups"))]
    ClearSupplementaryGroups { source: nix::Error },
    #[snafu(display("read isolated supplementary groups"))]
    ReadSupplementaryGroups { source: nix::Error },
    #[snafu(display(
        "unexpected payload credentials uid {real_uid}/{effective_uid}, gid {real_gid}/{effective_gid}, supplementary {supplementary_gids:?}"
    ))]
    UnexpectedPayloadCredentials {
        real_uid: u32,
        effective_uid: u32,
        real_gid: u32,
        effective_gid: u32,
        supplementary_gids: Vec<u32>,
    },
    #[snafu(display("drop payload mount-administration capability"))]
    DropMountAdministration { source: nix::Error },
    #[snafu(display("payload retained mount-administration capability"))]
    PayloadRetainsMountAdministration,
    #[snafu(display("sethostname"))]
    SetHostname { source: nix::Error },
    #[snafu(display("pivot_root"))]
    PivotRoot { source: nix::Error },
    #[snafu(display("unmount old root"))]
    UnmountOldRoot { source: nix::Error },
    #[snafu(display("mount {}", target.display()))]
    Mount { source: nix::Error, target: PathBuf },
    #[snafu(display("filesystem"))]
    FsErr { source: io::Error },
}

#[repr(u8)]
enum Message {
    Continue = 1,
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::os::unix::fs::FileTypeExt as _;
    use std::os::unix::net::UnixListener;
    use std::path::Path;

    use fs_err as fs;
    use nix::errno::Errno;
    use nix::fcntl::{FcntlArg, FdFlag, fcntl};

    use super::{
        CAP_SYS_ADMIN, CapabilityData, Container, ContainerError, DevPolicy, Error as ContainerRunError,
        LoopbackPolicy, MINIMAL_DEV_NODES, PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, PR_CAPBSET_READ, ProcPolicy,
        PseudoFilesystemPolicy, PseudoMountDecision, RootFilesystemPolicy, RootMountDecision, SyncPipe, SysPolicy,
        TmpPolicy, capability_is_set, checked_prctl_value, clear_capability, namespace_flags, prctl,
        prepare_bind_target, pseudo_mount_decisions, read_capabilities, root_mount_decisions, set_mount_access,
        validate_payload_credentials,
    };

    #[test]
    fn default_policy_preserves_historical_mounts() {
        let container = Container::new("/");

        assert_eq!(container.pseudo_filesystems, PseudoFilesystemPolicy::default());
        assert_eq!(container.loopback, LoopbackPolicy::HostIpIfAvailable);
        assert_eq!(
            pseudo_mount_decisions(PseudoFilesystemPolicy::default()),
            vec![
                PseudoMountDecision::Proc { read_only: false },
                PseudoMountDecision::EmptyTmp,
                PseudoMountDecision::HostSys { read_only: false },
                PseudoMountDecision::HostDev { read_only: false },
            ]
        );
    }

    #[test]
    fn disabled_policy_produces_no_mount_decisions() {
        let policy = PseudoFilesystemPolicy {
            proc: ProcPolicy::None,
            tmp: TmpPolicy::Disabled,
            sys: SysPolicy::None,
            dev: DevPolicy::None,
        };

        assert!(pseudo_mount_decisions(policy).is_empty());
    }

    #[test]
    fn policy_maps_to_ordered_mount_decisions() {
        let policy = PseudoFilesystemPolicy {
            proc: ProcPolicy::ReadOnly,
            tmp: TmpPolicy::Disabled,
            sys: SysPolicy::HostReadOnly,
            dev: DevPolicy::Minimal,
        };
        let container = Container::new("/").pseudo_filesystems(policy);

        assert_eq!(container.pseudo_filesystems, policy);
        assert_eq!(
            pseudo_mount_decisions(policy),
            vec![
                PseudoMountDecision::Proc { read_only: true },
                PseudoMountDecision::HostSys { read_only: true },
                PseudoMountDecision::MinimalDev,
            ]
        );
    }

    #[test]
    fn deterministic_loopback_policy_is_explicit() {
        let container = Container::new("/").loopback(LoopbackPolicy::KernelDefault);

        assert_eq!(container.loopback, LoopbackPolicy::KernelDefault);
    }

    #[test]
    fn read_only_root_reopens_only_explicit_read_write_binds() {
        let default = Container::new("/root");
        assert_eq!(default.root_filesystem, RootFilesystemPolicy::ReadWrite);
        assert!(root_mount_decisions(&default.root, &default.binds, default.root_filesystem).is_empty());

        let restricted = Container::new("/root")
            .root_filesystem(RootFilesystemPolicy::ReadOnly)
            .bind_rw("/host/work", "/work")
            .bind_ro("/host/input", "/work/input")
            .bind_rw("/host/cache", "/work/cache");

        assert_eq!(
            root_mount_decisions(&restricted.root, &restricted.binds, restricted.root_filesystem),
            vec![
                RootMountDecision::ReadOnlyRecursive("/root".into()),
                RootMountDecision::ReadWriteExact("/root/work".into()),
                RootMountDecision::ReadWriteExact("/root/work/cache".into()),
            ]
        );
    }

    #[test]
    fn mount_administration_is_removed_from_every_live_capability_set() {
        let mut capabilities = [
            CapabilityData {
                effective: u32::MAX,
                permitted: u32::MAX,
                inheritable: u32::MAX,
            },
            CapabilityData {
                effective: u32::MAX,
                permitted: u32::MAX,
                inheritable: u32::MAX,
            },
        ];
        let unrelated_low = CAP_SYS_ADMIN - 1;
        let unrelated_high = u32::BITS + 1;

        assert!(capability_is_set(&capabilities, CAP_SYS_ADMIN));
        clear_capability(&mut capabilities, CAP_SYS_ADMIN);

        assert!(!capability_is_set(&capabilities, CAP_SYS_ADMIN));
        assert!(capability_is_set(&capabilities, unrelated_low));
        assert!(capability_is_set(&capabilities, unrelated_high));
    }

    #[test]
    fn read_only_root_is_enforced_by_the_live_kernel_mount_and_capability_paths() {
        let root = tempfile::tempdir().unwrap();
        let writable = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("locked")).unwrap();
        fs::write(root.path().join("locked/input"), b"immutable").unwrap();

        let result = Container::new(root.path())
            .root_filesystem(RootFilesystemPolicy::ReadOnly)
            .pseudo_filesystems(PseudoFilesystemPolicy {
                proc: ProcPolicy::None,
                tmp: TmpPolicy::Disabled,
                sys: SysPolicy::None,
                dev: DevPolicy::None,
            })
            .loopback(LoopbackPolicy::KernelDefault)
            .bind_rw(writable.path(), "/work")
            .run::<io::Error>(|| {
                require_errno(
                    fs::write("/locked/initial-mutation", b"rejected"),
                    Errno::EROFS,
                    "write undeclared root path before remount attempts",
                )?;
                fs::write("/work/result", b"writable bind")?;
                require_mount_administration_absent()?;

                let remount = nix::mount::mount::<str, str, str, str>(
                    None,
                    "/",
                    None,
                    nix::mount::MsFlags::MS_BIND | nix::mount::MsFlags::MS_REMOUNT,
                    None,
                );
                if !matches!(remount, Err(Errno::EPERM)) {
                    return Err(io::Error::other(format!(
                        "root remount without CAP_SYS_ADMIN did not fail with EPERM: {remount:?}"
                    )));
                }

                match set_mount_access(Path::new("/"), false, true) {
                    Err(ContainerError::Mount {
                        source: Errno::EPERM, ..
                    }) => {}
                    Err(error) => {
                        return Err(io::Error::other(format!(
                            "mount_setattr write-enable failed unexpectedly: {error}"
                        )));
                    }
                    Ok(()) => {
                        return Err(io::Error::other(
                            "mount_setattr write-enable succeeded without CAP_SYS_ADMIN",
                        ));
                    }
                }

                require_errno(
                    fs::write("/locked/post-remount-mutation", b"rejected"),
                    Errno::EROFS,
                    "write undeclared root path after remount attempts",
                )
            });

        match result {
            Ok(()) => {
                assert_eq!(fs::read(writable.path().join("result")).unwrap(), b"writable bind");
                assert!(!root.path().join("locked/initial-mutation").exists());
                assert!(!root.path().join("locked/post-remount-mutation").exists());
            }
            Err(error) if host_denied_user_namespace_setup(&error) => {
                eprintln!("SKIP live read-only-root kernel test: host denied user-namespace credential setup: {error}");
            }
            Err(error) => panic!("live read-only-root kernel test failed: {error}"),
        }
    }

    fn require_errno<T>(result: io::Result<T>, expected: Errno, operation: &str) -> io::Result<()> {
        match result {
            Err(error) if error.raw_os_error() == Some(expected as i32) => Ok(()),
            Err(error) => Err(io::Error::other(format!(
                "{operation} failed with {error}, expected {expected}"
            ))),
            Ok(_) => Err(io::Error::other(format!(
                "{operation} unexpectedly succeeded, expected {expected}"
            ))),
        }
    }

    fn require_mount_administration_absent() -> io::Result<()> {
        let capabilities = read_capabilities().map_err(errno_to_io)?;
        if capability_is_set(&capabilities, CAP_SYS_ADMIN) {
            return Err(io::Error::other("CAP_SYS_ADMIN remains in a live capability set"));
        }
        let bounding =
            unsafe { checked_prctl_value(prctl(PR_CAPBSET_READ, CAP_SYS_ADMIN, 0, 0, 0)).map_err(errno_to_io)? };
        let ambient = unsafe {
            checked_prctl_value(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, CAP_SYS_ADMIN, 0, 0))
                .map_err(errno_to_io)?
        };
        if bounding != 0 || ambient != 0 {
            return Err(io::Error::other(format!(
                "CAP_SYS_ADMIN remains recoverable: bounding={bounding}, ambient={ambient}"
            )));
        }
        Ok(())
    }

    fn errno_to_io(error: Errno) -> io::Error {
        io::Error::from_raw_os_error(error as i32)
    }

    fn host_denied_user_namespace_setup(error: &ContainerRunError) -> bool {
        match error {
            ContainerRunError::Nix { source: Errno::EPERM } => true,
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

    #[test]
    fn user_namespace_is_mandatory_for_rootful_and_rootless_callers() {
        for networking in [false, true] {
            let flags = namespace_flags(networking);
            assert!(flags.contains(nix::sched::CloneFlags::CLONE_NEWUSER));
            assert_eq!(flags.contains(nix::sched::CloneFlags::CLONE_NEWNET), !networking);
        }
    }

    #[test]
    fn payload_credentials_reject_every_inherited_identity() {
        assert!(validate_payload_credentials(0, 0, 0, 0, Vec::new()).is_ok());
        for credentials in [
            (1000, 0, 0, 0, Vec::new()),
            (0, 1000, 0, 0, Vec::new()),
            (0, 0, 1000, 0, Vec::new()),
            (0, 0, 0, 1000, Vec::new()),
            (0, 0, 0, 0, vec![4, 24, 27]),
        ] {
            assert!(
                validate_payload_credentials(
                    credentials.0,
                    credentials.1,
                    credentials.2,
                    credentials.3,
                    credentials.4,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn minimal_dev_has_an_exact_non_entropy_device_set() {
        assert_eq!(MINIMAL_DEV_NODES, ["null", "zero", "full"]);
    }

    #[test]
    fn synchronization_pipe_is_close_on_exec() {
        let mut sync = SyncPipe::new().unwrap();
        let read_fd = sync.read_fd();
        let write_fd = sync.write_fd();

        for fd in [read_fd, write_fd] {
            let flags = FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD).unwrap());
            assert!(flags.contains(FdFlag::FD_CLOEXEC));
        }

        sync.close_write().unwrap();
        assert_eq!(fcntl(write_fd, FcntlArg::F_GETFD), Err(Errno::EBADF));
        drop(sync);
        assert_eq!(fcntl(read_fd, FcntlArg::F_GETFD), Err(Errno::EBADF));
    }

    #[test]
    fn special_file_bind_gets_a_file_mountpoint() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("device.sock");
        let _listener = UnixListener::bind(&source).unwrap();
        let target = temporary.path().join("mountpoints/device");

        assert!(fs::metadata(&source).unwrap().file_type().is_socket());
        prepare_bind_target(&source, &target).unwrap();

        let target_metadata = fs::metadata(target).unwrap();
        assert!(target_metadata.is_file());
        assert_eq!(target_metadata.len(), 0);
    }
}
