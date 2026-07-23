use std::io;
use std::num::NonZeroU64;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::Signal;
use snafu::Snafu;

mod activation;
pub mod cgroup;
mod clone3;
mod credentials;
mod idmap;
mod mounts;
mod payload;
mod private_device_assembly;
mod private_device_broker;
mod private_devices;
mod process_runtime;
mod seccomp;

pub use self::mounts::{AnchoredLocator, AnchoredLocatorComponent, AnchoredLocatorError};
use self::mounts::{AnchoredMountTargetKind, Bind, BindSource, normalized_anchored_mount_target};
#[cfg(test)]
use self::mounts::{
    PreparedAnchoredMount, PseudoMountDecision, RootMountDecision, TMPFS_MAGIC, TmpfsLimitReadback,
    authenticate_anchored_inputs, descriptor_stat, open_anchored_mount_target, open_anchored_resolver_target,
    prepare_bind_target, prepare_pseudo_mount_targets, pseudo_mount_decisions, resolver_stat_stable,
    root_mount_decisions, sealed_resolver_file, set_mount_access, validate_anchored_bind_inputs,
    validate_anchored_mount_topology, validate_resolver_target, validate_tmpfs_limit_readback, verify_tmpfs_limits,
};

#[cfg(test)]
use self::activation::{
    contain_raw_clone_child_panic, namespace_flags, read_child_error, require_atomic_cgroup_bind_policy,
    require_atomic_cgroup_policy,
};
#[cfg(test)]
use self::payload::{
    CapabilityData, MAX_LINUX_CAPABILITY_NUMBER, PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, PR_CAPBSET_READ,
    capability_is_set, checked_prctl_value, prctl, read_capabilities, standard_descriptor_is_unsafe,
    supported_capability_numbers,
};
#[cfg(test)]
use self::process_runtime::{
    BlockedSignalMask, ChildLifecycle, SignalOverride, SyncSocket, close_sync_endpoint, send_packet_no_signal,
    send_pidfd_signal, wait_for_pidfd, wait_for_pidfd_reap,
};
pub use self::process_runtime::{ChildPidfdQuarantine, forward_sigint, set_term_fg};
use self::process_runtime::{cleanup_pidfd_child, format_error};

// One bounded SOCK_SEQPACKET diagnostic; the kernel delivers or rejects the
// complete message without stream fragmentation.
const MAX_CHILD_ERROR_BYTES: usize = 2048;
const MAX_ERROR_SOURCE_DEPTH: usize = 16;
const MAX_CONTROL_EINTR_RETRIES: usize = 3;
const CLONE_STACK_BYTES: usize = 4 * 1024 * 1024;
const PIDFD_REAP_TIMEOUT: Duration = Duration::from_secs(5);
const PIDFD_REAP_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PRIVATE_DEVICE_BROKER_TIMEOUT: Duration = Duration::from_secs(2);

/// Serve one fixed private-device broker request on an already-connected
/// systemd `Accept=yes` socket.
///
/// The protocol accepts no caller-selected device, path, mode, count, or
/// timeout. The process must hold initial-user-namespace `CAP_SYS_ADMIN` and
/// `CAP_MKNOD`; callers without those capabilities receive an error rather
/// than an ambient-device fallback.
pub fn serve_private_device_broker_connection(connection: OwnedFd) -> io::Result<()> {
    private_device_broker::serve_private_device_connection(connection, PRIVATE_DEVICE_BROKER_TIMEOUT)
        .map_err(io::Error::other)
}

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
    /// Mount a fresh tmpfs with exact finite byte and inode ceilings.
    ///
    /// Unlike [`TmpPolicy::Empty`], this policy never falls back to an
    /// unbounded mount if either option is unsupported by the host kernel.
    Bounded(TmpfsLimits),
}

/// Exact finite resource ceilings for a tmpfs mounted at `/tmp`.
///
/// Both values are non-zero by construction. The byte value is passed to the
/// kernel as `size`, and the inode value as `nr_inodes`, without scaling or
/// rounding in userspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TmpfsLimits {
    size_bytes: NonZeroU64,
    inodes: NonZeroU64,
}

impl TmpfsLimits {
    pub const fn new(size_bytes: u64, inodes: u64) -> Result<Self, TmpfsLimitsError> {
        let Some(size_bytes) = NonZeroU64::new(size_bytes) else {
            return Err(TmpfsLimitsError::ZeroCeiling { field: "size bytes" });
        };
        let Some(inodes) = NonZeroU64::new(inodes) else {
            return Err(TmpfsLimitsError::ZeroCeiling { field: "inodes" });
        };
        Ok(Self { size_bytes, inodes })
    }

    pub const fn size_bytes(self) -> u64 {
        self.size_bytes.get()
    }

    pub const fn inodes(self) -> u64 {
        self.inodes.get()
    }

    fn mount_options(self) -> String {
        format!("size={},nr_inodes={}", self.size_bytes, self.inodes)
    }

    fn fsconfig_options(self) -> [(&'static std::ffi::CStr, std::ffi::CString); 2] {
        [
            (
                c"size",
                std::ffi::CString::new(self.size_bytes.to_string()).expect("u64 decimal contains no NUL"),
            ),
            (
                c"nr_inodes",
                std::ffi::CString::new(self.inodes.to_string()).expect("u64 decimal contains no NUL"),
            ),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
pub enum TmpfsLimitsError {
    #[snafu(display("tmpfs {field} ceiling must be non-zero"))]
    ZeroCeiling { field: &'static str },
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
    /// Mount a sealed three-name `/dev` containing fresh private `null`,
    /// `zero`, and `full` character-device inodes.
    ///
    /// The directory itself is immutable, while each disposable device mount
    /// retains ordinary Unix data-plane and existing-file `O_CREAT` behavior.
    /// No ambient host device inode is mounted or exposed.
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
/// Only mounts declared through the applicable writable-bind API are then made
/// writable again. Anchored callers use [`Container::bind_rw_from_root`] or
/// [`Container::bind_rw_pinned`]; legacy pathname containers use
/// [`Container::bind_rw`]. That exception applies to the exact bind mount, not
/// to nested mounts; each writable nested mount must be declared separately.
/// This keeps undeclared paths, including package-manager content and
/// dependency trees, immutable to the payload. Every container payload,
/// regardless of root policy, enters with a sanitized descriptor table, no
/// Linux capabilities, the fair scheduler, and a mandatory seccomp policy
/// preventing namespace, mount, device-node, and file-handle escape syscalls;
/// pseudo-filesystems remain governed by [`PseudoFilesystemPolicy`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RootFilesystemPolicy {
    ReadOnly,
    #[default]
    ReadWrite,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnedNestedProcFixture {
    Root,
    FirstAnchoredBind,
}

pub struct Container {
    root: PathBuf,
    root_locator: Option<AnchoredLocator>,
    work_dir: Option<PathBuf>,
    binds: Vec<Bind>,
    networking: bool,
    hostname: Option<String>,
    ignore_host_sigint: bool,
    pseudo_filesystems: PseudoFilesystemPolicy,
    loopback: LoopbackPolicy,
    root_filesystem: RootFilesystemPolicy,
    private_devices: Option<private_devices::PrivateDeviceMounts>,
    #[cfg(test)]
    owned_nested_proc_fixture: Option<OwnedNestedProcFixture>,
}

fn duplicate_cloexec(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC does not borrow through `fd` after the call and
    // returns a fresh descriptor on success.
    let duplicated = unsafe { nix::libc::fcntl(fd, nix::libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful F_DUPFD_CLOEXEC returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}

impl Container {
    /// Create a new Container using the default options
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            root_locator: None,
            work_dir: None,
            binds: vec![],
            networking: false,
            hostname: None,
            ignore_host_sigint: false,
            pseudo_filesystems: PseudoFilesystemPolicy::default(),
            loopback: LoopbackPolicy::default(),
            root_filesystem: RootFilesystemPolicy::default(),
            private_devices: None,
            #[cfg(test)]
            owned_nested_proc_fixture: None,
        }
    }

    /// Create a container whose root has an authenticated namespace locator.
    ///
    /// Activation reopens and authenticates the locator after entering the
    /// child's private mount namespace. Only that child-local descriptor is
    /// used to clone and attach the root mount. Nested mounts are deliberately
    /// excluded and must be declared explicitly.
    pub fn new_anchored(root: AnchoredLocator) -> io::Result<Self> {
        let root_path = root.resolved_absolute_path();
        let file_type = root.file_type();
        if file_type != nix::libc::S_IFDIR {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "anchored container root {} has file type {file_type:o}; expected a directory",
                    root_path.display()
                ),
            ));
        }
        Ok(Self {
            root: root_path,
            root_locator: Some(root),
            work_dir: None,
            binds: vec![],
            networking: false,
            hostname: None,
            ignore_host_sigint: false,
            pseudo_filesystems: PseudoFilesystemPolicy::default(),
            loopback: LoopbackPolicy::default(),
            root_filesystem: RootFilesystemPolicy::default(),
            private_devices: None,
            #[cfg(test)]
            owned_nested_proc_fixture: None,
        })
    }

    #[cfg(test)]
    fn with_owned_nested_proc_beneath_root_for_test(mut self) -> Self {
        self.owned_nested_proc_fixture = Some(OwnedNestedProcFixture::Root);
        self
    }

    #[cfg(test)]
    fn with_owned_nested_proc_beneath_first_bind_for_test(mut self) -> Self {
        self.owned_nested_proc_fixture = Some(OwnedNestedProcFixture::FirstAnchoredBind);
        self
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
            source: BindSource::Path(host.into()),
            target: guest.into(),
            read_only: false,
        });
        self
    }

    /// Create a read-only bind mount
    pub fn bind_ro(mut self, host: impl Into<PathBuf>, guest: impl Into<PathBuf>) -> Self {
        self.binds.push(Bind {
            source: BindSource::Path(host.into()),
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
                source: BindSource::Path(source),
                target: guest.into(),
                read_only: true,
            });
        }

        self
    }

    /// Expose a writable subtree of the authenticated root as an exact
    /// writable bind mount.
    ///
    /// The source is authenticated beneath the root locator before clone and
    /// reopened again inside the child's private mount namespace. This is the
    /// correct operation for a writable install directory that otherwise lives
    /// inside a recursively read-only frozen root. The exact directory mount
    /// is cloned without nested mounts. Both paths must be absolute.
    pub fn bind_rw_from_root(mut self, source: impl Into<PathBuf>, guest: impl Into<PathBuf>) -> io::Result<Self> {
        let Some(root) = self.root_locator.as_ref() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "root-relative binds require an anchored container",
            ));
        };
        let source = normalized_anchored_mount_target(&source.into()).map_err(container_error_to_invalid_input)?;
        let guest = guest.into();
        normalized_anchored_mount_target(&guest).map_err(container_error_to_invalid_input)?;
        let source = root.child(source).map_err(locator_error_to_invalid_input)?;
        self.binds.push(Bind {
            source: BindSource::Anchored(source),
            target: guest,
            read_only: false,
        });
        Ok(self)
    }

    /// Add a read-write bind whose source has an authenticated locator.
    ///
    /// The child reopens and authenticates the source inside its private mount
    /// namespace before cloning the referenced mount. Directory binds never
    /// import nested mounts from the host. The guest path must be absolute.
    pub fn bind_rw_pinned(self, source: AnchoredLocator, guest: impl Into<PathBuf>) -> io::Result<Self> {
        self.bind_pinned(source, guest.into(), false)
    }

    /// Add a read-only bind whose source has an authenticated locator.
    pub fn bind_ro_pinned(self, source: AnchoredLocator, guest: impl Into<PathBuf>) -> io::Result<Self> {
        self.bind_pinned(source, guest.into(), true)
    }

    fn bind_pinned(mut self, source: AnchoredLocator, guest: PathBuf, read_only: bool) -> io::Result<Self> {
        if self.root_locator.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "locator-pinned binds require an anchored container",
            ));
        }
        normalized_anchored_mount_target(&guest).map_err(container_error_to_invalid_input)?;
        self.binds.push(Bind {
            source: BindSource::Anchored(source),
            target: guest,
            read_only,
        });
        Ok(self)
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
}

fn container_error_to_invalid_input(error: ContainerError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format_error(error))
}

fn locator_error_to_invalid_input(error: AnchoredLocatorError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error)
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
    #[snafu(display("private minimal-device provider is unavailable"))]
    PrivateDeviceProviderUnavailable { source: io::Error },
    #[snafu(display("private minimal-device acquisition was rejected: {message}"))]
    PrivateDeviceAcquisitionRejected { message: String },
    /// The host rejected creation of the container's mandatory namespaces.
    ///
    /// Keeping this operation separate from generic nix failures lets callers
    /// recognize namespace-capability denials without hiding unrelated pipe,
    /// signal, wait, or payload failures that happen to use the same errno.
    #[snafu(display("clone isolated namespaces"))]
    CloneNamespaces { source: nix::Error },
    #[snafu(display("clone child atomically into the derivation cgroup"))]
    CloneIntoCgroup { source: io::Error },
    #[snafu(display("atomic cgroup execution requires a descriptor-anchored root"))]
    AtomicCgroupRequiresAnchoredRoot,
    #[snafu(display("inspect filesystem authority exposed by {}", label.display()))]
    InspectCgroupFilesystem { label: PathBuf, source: nix::Error },
    #[snafu(display(
        "atomic cgroup execution forbids a cgroup filesystem as container root {}",
        label.display()
    ))]
    UnsafeCgroupRootFilesystem { label: PathBuf },
    #[snafu(display(
        "atomic cgroup execution forbids writable cgroup filesystem bind source {}",
        label.display()
    ))]
    UnsafeCgroupBindSource { label: PathBuf },
    #[snafu(display("atomic cgroup execution forbids writable host /sys exposure"))]
    UnsafeCgroupSysPolicy,
    #[snafu(display("authenticate derivation cgroup lifecycle"))]
    CgroupLifecycle { source: cgroup::CgroupError },
    #[snafu(display("failed to prove exact clone3 child cleanup: {cleanup}"))]
    ChildCleanup {
        cleanup: io::Error,
        pidfd: Option<ChildPidfdQuarantine>,
    },
    #[snafu(display("{primary}; additionally failed to prove exact clone3 child cleanup: {cleanup}"))]
    ChildCleanupAfterFailure {
        primary: Box<Error>,
        cleanup: io::Error,
        pidfd: Option<ChildPidfdQuarantine>,
    },
    #[snafu(display("remove derivation cgroup after execution: {cleanup}"))]
    CgroupCleanup {
        cleanup: cgroup::CgroupError,
        leaf: Option<Box<cgroup::CgroupLeaf>>,
    },
    #[snafu(display("{failure}; additionally failed to remove derivation cgroup: {cleanup}"))]
    CgroupCleanupAfterFailure {
        failure: Box<Error>,
        cleanup: cgroup::CgroupError,
        leaf: Option<Box<cgroup::CgroupLeaf>>,
    },
    // FIXME: Replace with more fine-grained variants
    #[snafu(display("nix"))]
    Nix { source: nix::Error },
}

impl Error {
    /// Whether this error is a narrowly authenticated host-capability denial
    /// for the mandatory container execution boundary.
    ///
    /// Callers may use this to skip an optional execution proof. It does not
    /// recursively soften arbitrary permission errors: malformed maps,
    /// credential drift, helper lifecycle failures, cleanup failures, and
    /// payload errors remain hard failures even when an inner source happens
    /// to contain EPERM.
    pub fn execution_capability_unavailable(&self) -> bool {
        match self {
            Self::CloneNamespaces { source } => {
                matches!(*source, Errno::EPERM | Errno::EACCES | Errno::ENOSYS)
            }
            Self::CloneIntoCgroup { source } => matches!(
                source.raw_os_error(),
                Some(code)
                    if code == nix::libc::EPERM
                        || code == nix::libc::EACCES
                        || code == nix::libc::ENOSYS
                        || code == nix::libc::E2BIG
            ),
            Self::Idmap { source } => source.execution_capability_unavailable(),
            Self::PrivateDeviceProviderUnavailable { .. } => true,
            Self::Failure { message } => setup_capability_denial(message),
            Self::Signaled { .. }
            | Self::UnknownExit
            | Self::PrivateDeviceAcquisitionRejected { .. }
            | Self::AtomicCgroupRequiresAnchoredRoot
            | Self::InspectCgroupFilesystem { .. }
            | Self::UnsafeCgroupRootFilesystem { .. }
            | Self::UnsafeCgroupBindSource { .. }
            | Self::UnsafeCgroupSysPolicy
            | Self::CgroupLifecycle { .. }
            | Self::ChildCleanup { .. }
            | Self::ChildCleanupAfterFailure { .. }
            | Self::CgroupCleanup { .. }
            | Self::CgroupCleanupAfterFailure { .. }
            | Self::Nix { .. } => false,
        }
    }

    fn retry_child_cleanup_after_cgroup(self) -> Result<(), Self> {
        match self {
            Self::ChildCleanup {
                cleanup,
                pidfd: Some(pidfd),
            } => match cleanup_pidfd_child(pidfd.into_owned_fd()) {
                Ok(()) => Ok(()),
                Err(failure) => Err(Self::ChildCleanup {
                    cleanup: io::Error::other(format!(
                        "{cleanup}; retry after successful cgroup drain failed: {}",
                        failure.cleanup
                    )),
                    pidfd: Some(ChildPidfdQuarantine::new(failure.pidfd)),
                }),
            },
            Self::ChildCleanupAfterFailure {
                primary,
                cleanup,
                pidfd: Some(pidfd),
            } => match cleanup_pidfd_child(pidfd.into_owned_fd()) {
                Ok(()) => Err(*primary),
                Err(failure) => Err(Self::ChildCleanupAfterFailure {
                    primary,
                    cleanup: io::Error::other(format!(
                        "{cleanup}; retry after successful cgroup drain failed: {}",
                        failure.cleanup
                    )),
                    pidfd: Some(ChildPidfdQuarantine::new(failure.pidfd)),
                }),
            },
            failure => Err(failure),
        }
    }

    /// Take the retained pidfd quarantine after exact-child cleanup failure.
    pub fn take_child_pidfd(&mut self) -> Option<ChildPidfdQuarantine> {
        match self {
            Self::ChildCleanup { pidfd, .. } | Self::ChildCleanupAfterFailure { pidfd, .. } => pidfd.take(),
            Self::CgroupCleanupAfterFailure { failure, .. } => failure.take_child_pidfd(),
            _ => None,
        }
    }

    /// Take the authenticated cgroup cleanup capability retained by a failed
    /// teardown. Callers that continue running after this error must retry or
    /// quarantine it rather than silently dropping authority to the leaf.
    pub fn take_cgroup_leaf(&mut self) -> Option<cgroup::CgroupLeaf> {
        match self {
            Self::CgroupCleanup { leaf, .. } | Self::CgroupCleanupAfterFailure { leaf, .. } => {
                leaf.take().map(|leaf| *leaf)
            }
            _ => None,
        }
    }
}

fn setup_capability_denial(message: &str) -> bool {
    let known_setup_operation = [
        "clear inherited supplementary groups",
        "normalize payload real, effective, and saved-set GIDs",
        "normalize payload real, effective, and saved-set UIDs",
        "clone descriptor-backed root mount",
        "clone descriptor-backed bind mount",
        "attach descriptor-backed root mount",
        "mount ",
        "pivot_root",
        "sethostname",
        "unmount old root",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix));
    let known_capability_errno = [
        ": EPERM: Operation not permitted",
        ": EACCES: Permission denied",
        ": ENOSYS: Function not implemented",
    ]
    .iter()
    .any(|suffix| message.ends_with(suffix));
    known_setup_operation && known_capability_errno
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
    #[snafu(display("close the supervisor synchronization endpoint in the clone child"))]
    CloseSupervisorSync { source: nix::Error },
    #[snafu(display("clear inherited supplementary groups"))]
    ClearSupplementaryGroups {
        source: credentials::CredentialSyscallError,
    },
    #[snafu(display("normalize payload real, effective, and saved-set GIDs"))]
    NormalizePayloadGroupCredentials {
        source: credentials::CredentialSyscallError,
    },
    #[snafu(display("normalize payload real, effective, and saved-set UIDs"))]
    NormalizePayloadUserCredentials {
        source: credentials::CredentialSyscallError,
    },
    #[snafu(display("read isolated supplementary groups"))]
    ReadSupplementaryGroups {
        source: credentials::CredentialSyscallError,
    },
    #[snafu(display("read payload real, effective, and saved-set GIDs"))]
    ReadPayloadGroupCredentials {
        source: credentials::CredentialSyscallError,
    },
    #[snafu(display("read payload real, effective, and saved-set UIDs"))]
    ReadPayloadUserCredentials {
        source: credentials::CredentialSyscallError,
    },
    #[snafu(display("unexpected payload credentials {credentials}"))]
    UnexpectedPayloadCredentials {
        credentials: credentials::PayloadCredentials,
    },
    #[snafu(display("restrict payload scheduler to the fair class"))]
    RestrictPayloadScheduler { source: nix::Error },
    #[snafu(display(
        "payload retained real-time scheduling access: policy={policy}, RLIMIT_RTPRIO={soft_limit}/{hard_limit}"
    ))]
    PayloadRetainsRealtimeScheduling {
        policy: i32,
        soft_limit: nix::libc::rlim_t,
        hard_limit: nix::libc::rlim_t,
    },
    #[snafu(display("drop all payload capabilities"))]
    DropPayloadCapabilities { source: nix::Error },
    #[snafu(display("payload retained capability {capability}"))]
    PayloadRetainsCapability { capability: u32 },
    #[snafu(display("install mandatory payload seccomp policy"))]
    InstallPayloadSeccomp { source: io::Error },
    #[snafu(display("restore the clone child's inherited signal mask before payload execution"))]
    RestoreCloneSignalMask { source: io::Error },
    #[snafu(display("inspect payload standard descriptor {fd}"))]
    InspectPayloadStandardDescriptor { fd: RawFd, source: nix::Error },
    #[snafu(display(
        "payload standard descriptor {fd} is a pathname capability: kind={kind:o}, status_flags={status_flags:#x}"
    ))]
    UnsafePayloadStandardDescriptor {
        fd: RawFd,
        kind: nix::libc::mode_t,
        status_flags: nix::libc::c_int,
    },
    #[snafu(display(
        "payload standard descriptor {fd} is a writable cgroup filesystem capability: filesystem={filesystem:#x}, status_flags={status_flags:#x}"
    ))]
    UnsafeCgroupStandardDescriptor {
        fd: RawFd,
        filesystem: nix::libc::c_long,
        status_flags: nix::libc::c_int,
    },
    #[snafu(display("sanitize payload descriptors"))]
    SanitizePayloadDescriptors { source: nix::Error },
    #[snafu(display("payload error descriptor {fd} overlaps standard I/O"))]
    InvalidPayloadErrorDescriptor { fd: RawFd },
    #[snafu(display("clone child inherited invalid cgroup descriptor pair {descriptors:?}"))]
    InvalidInheritedCgroupDescriptors { descriptors: [RawFd; 2] },
    #[snafu(display("close inherited cgroup descriptor {descriptor}"))]
    CloseInheritedCgroupDescriptor { descriptor: RawFd, source: nix::Error },
    #[snafu(display("clone child retained cgroup descriptor {descriptor} after close"))]
    RetainedInheritedCgroupDescriptor { descriptor: RawFd },
    #[snafu(display("sethostname"))]
    SetHostname { source: nix::Error },
    #[snafu(display("{operation} for anchored root {}", label.display()))]
    ActivateAnchoredRoot {
        operation: &'static str,
        label: PathBuf,
        source: nix::Error,
    },
    #[snafu(display("reopen anchored container root {} in the active mount namespace", path.display()))]
    ReopenAnchoredRoot {
        path: PathBuf,
        source: AnchoredLocatorError,
    },
    #[snafu(display("anchored container root {} has file type {file_type:o}; expected a directory", path.display()))]
    AnchoredRootNotDirectory {
        path: PathBuf,
        file_type: nix::libc::mode_t,
    },
    #[snafu(display("reopen anchored bind source {} in the active mount namespace", path.display()))]
    ReopenAnchoredBindSource {
        path: PathBuf,
        source: AnchoredLocatorError,
    },
    #[snafu(display("clone descriptor-backed bind mount for anchored source {}", path.display()))]
    CloneAnchoredBindSource { path: PathBuf, source: nix::Error },
    #[snafu(display("{operation} through anchored root descriptor"))]
    ConfigureAnchoredNetworking { operation: &'static str, source: io::Error },
    #[snafu(display("unsafe host resolver source with mode {mode:o} and {links} links"))]
    UnsafeResolverSource { mode: nix::libc::mode_t, links: u64 },
    #[snafu(display("host resolver source is {actual} bytes; limit is {limit}"))]
    ResolverSourceTooLarge { actual: u64, limit: u64 },
    #[snafu(display("host resolver source changed while its bounded snapshot was read"))]
    ResolverSourceChanged,
    #[snafu(display(
        "sealed resolver has kind {kind:o}, mode {mode:o}, size {size} (expected {expected_size}), and seals {seals:#x}"
    ))]
    InvalidSealedResolver {
        kind: nix::libc::mode_t,
        mode: nix::libc::mode_t,
        size: u64,
        expected_size: u64,
        seals: nix::libc::c_int,
    },
    #[snafu(display(
        "unsafe anchored resolver target {} with mode {mode:o} and {links} links",
        path.display()
    ))]
    UnsafeResolverTarget {
        path: PathBuf,
        mode: nix::libc::mode_t,
        links: u64,
    },
    #[snafu(display(
        "anchored container rejects pathname bind source {}; use a locator-pinned or root-relative bind",
        path.display()
    ))]
    UnpinnedAnchoredMountSource { path: PathBuf },
    #[snafu(display("anchored bind source {} cannot be used by a pathname container", path.display()))]
    AnchoredBindOnPathContainer { path: PathBuf },
    #[snafu(display("unsupported anchored mount source {} with mode {mode:o}", path.display()))]
    UnsupportedAnchoredMountSource { path: PathBuf, mode: nix::libc::mode_t },
    #[snafu(display("anchored container declares {actual} mounts; limit is {limit}"))]
    TooManyAnchoredMounts { actual: usize, limit: usize },
    #[snafu(display(
        "anchored mount targets {} and {} overlap",
        first.display(),
        second.display()
    ))]
    OverlappingAnchoredMountTargets { first: PathBuf, second: PathBuf },
    #[snafu(display("invalid anchored mount target {}", path.display()))]
    InvalidAnchoredMountTarget { path: PathBuf },
    #[snafu(display("open anchored mount target {}", path.display()))]
    OpenAnchoredMountTarget { path: PathBuf, source: io::Error },
    #[snafu(display("unsafe anchored mount target {} with mode {mode:o}", path.display()))]
    UnsafeAnchoredMountTarget { path: PathBuf, mode: nix::libc::mode_t },
    #[snafu(display(
        "anchored mount target {} has type {actual:?}, expected {expected:?}",
        path.display()
    ))]
    AnchoredMountTargetType {
        path: PathBuf,
        expected: AnchoredMountTargetKind,
        actual: AnchoredMountTargetKind,
    },
    #[snafu(display("{operation} returned an invalid mount descriptor"))]
    InvalidMountDescriptor { operation: &'static str },
    #[snafu(display("inspect tmpfs mount at {}", target.display()))]
    InspectTmpfs { target: PathBuf, source: io::Error },
    #[snafu(display(
        "mount at {} has filesystem magic {filesystem:#x}, expected tmpfs",
        target.display()
    ))]
    UnexpectedTmpfsFilesystem {
        target: PathBuf,
        filesystem: nix::libc::c_long,
    },
    #[snafu(display("tmpfs limit readback at {} overflowed its u64 representation", target.display()))]
    InvalidTmpfsLimitReadback { target: PathBuf },
    #[snafu(display(
        "tmpfs at {} normalized declared limits: size {expected_size_bytes} -> {observed_size_bytes} bytes, inodes {expected_inodes} -> {observed_inodes}",
        target.display()
    ))]
    TmpfsLimitsNormalized {
        target: PathBuf,
        expected_size_bytes: u64,
        observed_size_bytes: u64,
        expected_inodes: u64,
        observed_inodes: u64,
    },
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
mod tests;
