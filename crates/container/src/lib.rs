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
use nix::libc::{AT_RECURSIVE, SIGCHLD};
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use nix::sched::{CloneFlags, clone};
use nix::sys::prctl::set_pdeathsig;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, Signal, kill, sigaction};
use nix::sys::signalfd::SigSet;
use nix::sys::stat::{Mode, umask};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{Pid, Uid, close, pipe2, pivot_root, read, sethostname, tcsetpgrp, write};
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

pub struct Container {
    root: PathBuf,
    work_dir: Option<PathBuf>,
    binds: Vec<Bind>,
    networking: bool,
    hostname: Option<String>,
    ignore_host_sigint: bool,
    pseudo_filesystems: PseudoFilesystemPolicy,
    loopback: LoopbackPolicy,
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

    /// Run `f` as a container process payload
    pub fn run<E>(self, mut f: impl FnMut() -> Result<(), E>) -> Result<(), Error>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        static mut STACK: [u8; 4 * 1024 * 1024] = [0u8; 4 * 1024 * 1024];

        let rootless = !Uid::effective().is_root();

        // Pipe to synchronize parent & child. Both ends must be close-on-exec:
        // the child retains the write end while running the Rust payload so it
        // can report setup/payload errors, but spawned commands must never
        // inherit that control descriptor.
        let mut sync = SyncPipe::new()?;
        let child_sync = sync.raw();

        let mut flags =
            CloneFlags::CLONE_NEWNS | CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWIPC | CloneFlags::CLONE_NEWUTS;

        if rootless {
            flags |= CloneFlags::CLONE_NEWUSER;
        }

        if !self.networking {
            flags |= CloneFlags::CLONE_NEWNET;
        }

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

        // Update uid / gid map to map current user to root in container
        if rootless && let Err(source) = idmap(pid) {
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

    setup(container)?;

    let result = f().boxed().context(RunSnafu);
    if result.is_ok() {
        // Errors retain the write end so the outer clone callback can report
        // them. A successful Rust payload has nothing left to report.
        let _ = close(sync.1);
    }
    result
}

/// Setup the container
fn setup(container: &Container) -> Result<(), ContainerError> {
    if container.networking {
        setup_networking(&container.root)?;
    }

    if matches!(container.loopback, LoopbackPolicy::HostIpIfAvailable) {
        setup_localhost()?;
    }

    pivot(&container.root, &container.binds, container.pseudo_filesystems)?;

    if let Some(hostname) = &container.hostname {
        sethostname(hostname).context(SetHostnameSnafu)?;
    }

    if let Some(dir) = &container.work_dir {
        set_current_dir(dir)?;
    }

    Ok(())
}

/// Pivot the process into the rootfs
fn pivot(root: &Path, binds: &[Bind], pseudo_filesystems: PseudoFilesystemPolicy) -> Result<(), ContainerError> {
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
    pivot_root(root, &old_root).context(PivotRootSnafu)?;

    set_current_dir("/")?;

    for decision in pseudo_mount_decisions(pseudo_filesystems) {
        apply_pseudo_mount(decision, OLD_PATH)?;
    }

    umount2(OLD_PATH, MntFlags::MNT_DETACH).context(UnmountOldRootSnafu)?;
    fs::remove_dir(OLD_PATH).context(FsErrSnafu)?;

    umask(Mode::S_IWGRP | Mode::S_IWOTH);

    Ok(())
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
        set_mount_read_only(target, true)?;
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

fn set_mount_read_only(target: &Path, recursive: bool) -> Result<(), ContainerError> {
    unsafe {
        let inner = || {
            let fd = open_tree(AT_FDCWD, target, OPEN_TREE_CLOEXEC).map_err(Errno::from_i32)?;
            let attr = mount_attr_t {
                attr_set: MOUNT_ATTR_RDONLY as u64,
                attr_clr: 0,
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
    #[snafu(display("error setting up rootless id map"))]
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
    use std::os::unix::fs::FileTypeExt as _;
    use std::os::unix::net::UnixListener;

    use fs_err as fs;
    use nix::errno::Errno;
    use nix::fcntl::{FcntlArg, FdFlag, fcntl};

    use super::{
        Container, DevPolicy, LoopbackPolicy, MINIMAL_DEV_NODES, ProcPolicy, PseudoFilesystemPolicy,
        PseudoMountDecision, SyncPipe, SysPolicy, TmpPolicy, prepare_bind_target, pseudo_mount_decisions,
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
