// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::NonNull;
use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicI32, Ordering},
};
use std::time::{Duration, Instant};

use fs_err::{self as fs, PathExt as _};
use nc::syscalls::syscall5;
use nc::{
    AT_EMPTY_PATH, AT_FDCWD, MOUNT_ATTR_RDONLY, MOVE_MOUNT_F_EMPTY_PATH, OPEN_TREE_CLOEXEC, OPEN_TREE_CLONE,
    SYS_MOUNT_SETATTR, mount_attr_t, move_mount, open_tree,
};
use nix::errno::Errno;
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
use nix::sys::wait::{Id as WaitId, WaitPidFlag, WaitStatus, waitid, waitpid};
use nix::unistd::{
    Pid, close, fchdir, getegid, geteuid, getgid, getgroups, getuid, pivot_root, read, setgroups, sethostname,
    tcsetpgrp,
};
use snafu::{ResultExt, Snafu};

use self::clone3::{Clone3Outcome, clone3_into_cgroup};
use self::idmap::idmap;

pub mod cgroup;
mod clone3;
mod idmap;
mod seccomp;

// linux/mount.h. nc 0.9 exposes only the source-empty-path flag.
const MOVE_MOUNT_T_EMPTY_PATH: u32 = 0x0000_0040;
// One bounded SOCK_SEQPACKET diagnostic; the kernel delivers or rejects the
// complete message without stream fragmentation.
const MAX_CHILD_ERROR_BYTES: usize = 2048;
const MAX_ERROR_SOURCE_DEPTH: usize = 16;
const MAX_CONTROL_EINTR_RETRIES: usize = 3;
const CLONE_STACK_BYTES: usize = 4 * 1024 * 1024;
const PIDFD_REAP_TIMEOUT: Duration = Duration::from_secs(5);
const PIDFD_REAP_POLL_INTERVAL: Duration = Duration::from_millis(10);

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

// Linux's stable memory-device identities from include/uapi/linux/major.h and
// drivers/char/mem.c. Keep the identity next to the declared name: accepting
// an arbitrary character device under one of these names would let a hostile
// host substitute an entropy, terminal, or otherwise privileged device.
const MINIMAL_DEV_IDENTITIES: &[(&str, u64, u64)] = &[("null", 1, 3), ("zero", 1, 5), ("full", 1, 7)];

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

pub struct Container {
    root: PathBuf,
    root_anchor: Option<OwnedFd>,
    work_dir: Option<PathBuf>,
    binds: Vec<Bind>,
    networking: bool,
    hostname: Option<String>,
    ignore_host_sigint: bool,
    pseudo_filesystems: PseudoFilesystemPolicy,
    loopback: LoopbackPolicy,
    root_filesystem: RootFilesystemPolicy,
}

fn duplicate_root_anchor(anchor: RawFd) -> io::Result<OwnedFd> {
    let duplicated = duplicate_cloexec(anchor)?;

    // F_DUPFD_CLOEXEC preserves the open-file status flags. Requiring O_PATH
    // makes descriptor-based open_tree activation an explicit API contract,
    // rather than silently accepting a descriptor with different semantics.
    // SAFETY: `duplicated` is live for the duration of each fcntl call.
    let status_flags = unsafe { nix::libc::fcntl(duplicated.as_raw_fd(), nix::libc::F_GETFL) };
    if status_flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if status_flags & nix::libc::O_PATH != nix::libc::O_PATH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "anchored container root descriptor must be opened with O_PATH",
        ));
    }

    // SAFETY: zero is valid initialization for stat and the descriptor and
    // output pointer remain live for the call.
    let mut stat: nix::libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { nix::libc::fstat(duplicated.as_raw_fd(), &mut stat) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if stat.st_mode & nix::libc::S_IFMT != nix::libc::S_IFDIR {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "anchored container root descriptor must reference a directory",
        ));
    }

    // The kernel promises this for F_DUPFD_CLOEXEC; retaining the check makes
    // descriptor inheritance fail closed even on an unexpected platform ABI.
    // SAFETY: `duplicated` is live for the fcntl call.
    let descriptor_flags = unsafe { nix::libc::fcntl(duplicated.as_raw_fd(), nix::libc::F_GETFD) };
    if descriptor_flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if descriptor_flags & nix::libc::FD_CLOEXEC == 0 {
        return Err(io::Error::other(
            "anchored container root descriptor duplicate is not close-on-exec",
        ));
    }

    Ok(duplicated)
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
            root_anchor: None,
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

    /// Create a container whose root is pinned by an already authenticated
    /// directory descriptor.
    ///
    /// `label` is retained only for diagnostics. Container activation clones
    /// and attaches the mount referenced by `anchor` through descriptor-empty
    /// paths; it never resolves `label`. Only the referenced mount is cloned:
    /// nested mounts present below the directory are deliberately excluded and
    /// must be requested through [`PseudoFilesystemPolicy`] or an explicit
    /// bind. The descriptor is duplicated immediately, so the caller may close
    /// its copy after this function returns.
    pub fn new_anchored(label: impl Into<PathBuf>, anchor: &impl AsRawFd) -> io::Result<Self> {
        let root_anchor = duplicate_root_anchor(anchor.as_raw_fd())?;
        Ok(Self {
            root: label.into(),
            root_anchor: Some(root_anchor),
            work_dir: None,
            binds: vec![],
            networking: false,
            hostname: None,
            ignore_host_sigint: false,
            pseudo_filesystems: PseudoFilesystemPolicy::default(),
            loopback: LoopbackPolicy::default(),
            root_filesystem: RootFilesystemPolicy::default(),
        })
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
    /// The source is resolved beneath the retained root descriptor, never
    /// through the diagnostic root pathname. This is the correct operation for
    /// a writable install directory that otherwise lives inside a recursively
    /// read-only frozen root. The exact directory mount is cloned without any
    /// nested mounts that may have been injected below it. Both source and
    /// guest paths must be absolute.
    pub fn bind_rw_from_root(mut self, source: impl Into<PathBuf>, guest: impl Into<PathBuf>) -> io::Result<Self> {
        if self.root_anchor.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "root-relative binds require an anchored container",
            ));
        }
        let source = normalized_anchored_mount_target(&source.into()).map_err(container_error_to_invalid_input)?;
        let guest = guest.into();
        normalized_anchored_mount_target(&guest).map_err(container_error_to_invalid_input)?;
        self.binds.push(Bind {
            source: BindSource::RootRelative(source),
            target: guest,
            read_only: false,
        });
        Ok(self)
    }

    /// Add a read-write bind whose source object is already descriptor-pinned.
    ///
    /// The descriptor is duplicated immediately. Anchored containers reject
    /// ordinary pathname binds at execution time, so external build, artifact,
    /// and cache directories must be opened by the supervising runtime and
    /// supplied through this API. Directory binds clone only the referenced
    /// mount and never import nested mounts from the host. The guest path must
    /// be absolute.
    pub fn bind_rw_pinned(
        self,
        source: &impl AsRawFd,
        source_label: impl Into<PathBuf>,
        guest: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        self.bind_pinned(source, source_label.into(), guest.into(), false)
    }

    /// Add a read-only bind whose source object is already descriptor-pinned.
    pub fn bind_ro_pinned(
        self,
        source: &impl AsRawFd,
        source_label: impl Into<PathBuf>,
        guest: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        self.bind_pinned(source, source_label.into(), guest.into(), true)
    }

    fn bind_pinned(
        mut self,
        source: &impl AsRawFd,
        source_label: PathBuf,
        guest: PathBuf,
        read_only: bool,
    ) -> io::Result<Self> {
        if self.root_anchor.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "descriptor-pinned binds require an anchored container",
            ));
        }
        normalized_anchored_mount_target(&guest).map_err(container_error_to_invalid_input)?;
        let source = duplicate_cloexec(source.as_raw_fd())?;
        descriptor_target_kind(source.as_raw_fd(), &source_label).map_err(container_error_to_invalid_input)?;
        self.binds.push(Bind {
            source: BindSource::Pinned {
                descriptor: source,
                label: source_label,
            },
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

    /// Run `f` as a container process payload.
    ///
    /// This compatibility path preserves legacy `clone(2)` activation. Frozen
    /// derivations must use [`Container::run_in_cgroup`] so aggregate resource
    /// accounting begins atomically at process creation. Legacy activation is
    /// also fail-closed: it blocks catchable signals and requires the calling
    /// process to have exactly one authenticated procfs task before clone. It
    /// is therefore not a fork-after-threads compatibility escape hatch.
    pub fn run<E>(self, f: impl FnMut() -> Result<(), E>) -> Result<(), Error>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        self.run_internal(None, f)
    }

    /// Run `f` with atomic placement in an authenticated cgroup v2 leaf.
    ///
    /// There is deliberately no numeric `cgroup.procs` migration and no
    /// fallback to legacy clone. The kernel must create both the child and its
    /// pidfd with `clone3(CLONE_INTO_CGROUP | CLONE_PIDFD)` before any child
    /// instruction is released into trusted setup. Writable exposure of the
    /// host `/sys` tree is rejected because it would give the payload direct
    /// access to cgroup migration controls outside its leaf.
    pub fn run_in_cgroup<E>(self, leaf: cgroup::CgroupLeaf, f: impl FnMut() -> Result<(), E>) -> Result<(), Error>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        self.run_internal(Some(leaf), f)
    }

    fn run_internal<E>(
        mut self,
        mut cgroup_leaf: Option<cgroup::CgroupLeaf>,
        mut f: impl FnMut() -> Result<(), E>,
    ) -> Result<(), Error>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        #[cfg(test)]
        let _legacy_test_activation = if cgroup_leaf.is_none() {
            Some(
                LEGACY_TEST_ACTIVATION_LOCK
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            )
        } else {
            None
        };

        // Pin every anchored bind source in the supervising process. The
        // child later clones mounts from these descriptors through an empty
        // path, so neither a pathname substitution after `run` starts nor a
        // cwd change during setup can redirect a declared source.
        let preparation = (|| {
            if cgroup_leaf.is_some() {
                require_atomic_cgroup_policy(&self)?;
            }
            let anchored_bind_sources = if let Some(root_anchor) = &self.root_anchor {
                pin_anchored_bind_sources(root_anchor.as_raw_fd(), &self.binds).map_err(|error| Error::Failure {
                    message: format_error(error),
                })?
            } else {
                Vec::new()
            };
            if cgroup_leaf.is_some() {
                require_atomic_cgroup_bind_policy(&anchored_bind_sources)?;
            }

            // clone(2) needs a caller-owned stack. Fork-like clone3 without
            // CLONE_VM must instead use stack=0/stack_size=0 and resumes on the
            // copy-on-write copy of this Rust stack.
            let stack = if cgroup_leaf.is_none() {
                Some(CloneStack::new().map_err(|source| Error::Failure {
                    message: format!("allocate guarded clone stack: {source}"),
                })?)
            } else {
                None
            };

            // Both ends are close-on-exec. The child retains the writer only
            // long enough to return one bounded setup/payload diagnostic.
            let sync = SyncSocket::new()?;
            Ok::<_, Error>((anchored_bind_sources, stack, sync))
        })();
        let (mut anchored_bind_sources, mut stack, mut sync) = match preparation {
            Ok(prepared) => prepared,
            Err(failure) => {
                return Err(match cgroup_leaf.take() {
                    Some(leaf) => cleanup_unstarted_cgroup(failure, leaf),
                    None => failure,
                });
            }
        };
        let child_sync = sync.raw();
        let flags = namespace_flags(self.networking);

        let spawn = if let Some(leaf) = cgroup_leaf.as_ref() {
            let result = (|| {
                let placement = leaf.placement().map_err(|source| Error::CgroupLifecycle { source })?;
                let inherited = placement.inherited_raw_fds();
                let mut signal_mask = BlockedSignalMask::block_all().map_err(|source| Error::CloneIntoCgroup {
                    source: io::Error::new(
                        source.kind(),
                        format!("block signals before clone3 task audit: {source}"),
                    ),
                })?;
                // SAFETY: the child outcome below closes both cgroup
                // descriptors, never unwinds, and terminates through _exit.
                let outcome = match unsafe { clone3_into_cgroup(flags.bits() as u64, placement.target()) } {
                    Ok(outcome) => outcome,
                    Err(source) => {
                        if let Err(restore) = signal_mask.restore() {
                            return Err(Error::CloneIntoCgroup {
                                source: io::Error::new(
                                    source.kind(),
                                    format!(
                                        "{source}; additionally failed to restore the supervisor signal mask: {restore}"
                                    ),
                                ),
                            });
                        }
                        return Err(Error::CloneIntoCgroup { source });
                    }
                };
                Ok::<_, Error>((outcome, inherited, signal_mask))
            })();
            match result {
                Ok((Clone3Outcome::Parent { pid, pidfd }, _, mut signal_mask)) => {
                    let child = ChildLifecycle::Pidfd { pid, pidfd };
                    if let Err(source) = signal_mask.restore() {
                        let primary = Error::CloneIntoCgroup {
                            source: io::Error::new(
                                source.kind(),
                                format!("restore supervisor signal mask after clone3: {source}"),
                            ),
                        };
                        Err(child.cleanup_after_failure(primary))
                    } else {
                        Ok(child)
                    }
                }
                Ok((Clone3Outcome::Child, inherited, mut signal_mask)) => {
                    // This is the raw fork-like child. If setup fails before
                    // the explicit pre-payload restore, inherited signal
                    // handlers must remain blocked until `_exit` rather than
                    // running against copied userspace lock state.
                    signal_mask.retain_blocked_on_drop();
                    let exit_code = contain_raw_clone_child_panic(child_sync.1, || {
                        match close_inherited_cgroup_descriptors(inherited) {
                            Ok(()) => child_exit_code(
                                &mut self,
                                &mut anchored_bind_sources,
                                child_sync,
                                Some(signal_mask),
                                &mut f,
                            ),
                            Err(error) => report_child_error(child_sync.1, error),
                        }
                    });
                    // SAFETY: this is the raw fork-like clone3 child. Running
                    // destructors or unwinding through pre-clone frames is not
                    // sound; _exit terminates only this process immediately.
                    unsafe { nix::libc::_exit(exit_code) }
                }
                Err(failure) => Err(failure),
            }
        } else {
            (|| {
                let signal_mask = BlockedSignalMask::block_all().map_err(|source| Error::Failure {
                    message: format!("block signals before legacy clone task audit: {source}"),
                })?;

                // Production legacy activation is a compatibility boundary,
                // not permission to fork after threads. Container's unit-test
                // build can only prevent concurrent test activations; libtest
                // still owns other tasks, so that gate is not a single-task
                // proof. A harness-free integration binary exercises this
                // exact production audit from a genuinely single-task process.
                #[cfg(not(test))]
                let signal_mask = {
                    let mut signal_mask = signal_mask;
                    if let Err(source) = clone3::require_single_threaded_process() {
                        let message = match signal_mask.restore() {
                            Ok(()) => {
                                format!("legacy clone requires an authenticated single-task supervisor: {source}")
                            }
                            Err(restore) => format!(
                                "legacy clone requires an authenticated single-task supervisor: {source}; additionally failed to restore the supervisor signal mask: {restore}"
                            ),
                        };
                        return Err(Error::Failure { message });
                    }
                    signal_mask
                };
                #[cfg(not(test))]
                let signal_mask = {
                    let mut signal_mask = signal_mask;
                    if let Err(source) = clone3::require_waitable_sigchld_disposition() {
                        let message = match signal_mask.restore() {
                            Ok(()) => format!(
                                "legacy clone requires a waitable SIGCHLD disposition before numeric child supervision: {source}"
                            ),
                            Err(restore) => format!(
                                "legacy clone requires a waitable SIGCHLD disposition before numeric child supervision: {source}; additionally failed to restore the supervisor signal mask: {restore}"
                            ),
                        };
                        return Err(Error::Failure { message });
                    }
                    signal_mask
                };

                let mut child_signal_mask = Some(signal_mask);
                let clone_result = {
                    let clone_cb = Box::new(|| {
                        let exit_code = if let Some(mut signal_mask) = child_signal_mask.take() {
                            signal_mask.retain_blocked_on_drop();
                            contain_raw_clone_child_panic(child_sync.1, || {
                                child_exit_code(
                                    &mut self,
                                    &mut anchored_bind_sources,
                                    child_sync,
                                    Some(signal_mask),
                                    &mut f,
                                )
                            })
                        } else {
                            report_child_error_bytes(
                                child_sync.1,
                                b"legacy clone child lost its blocked signal-mask guard before trusted setup",
                            )
                        };
                        // SAFETY: this is the raw fork-like legacy child. It
                        // must not run destructors or return through frames
                        // copied from the supervising process.
                        unsafe { nix::libc::_exit(exit_code) }
                    });
                    let stack = stack.as_mut().expect("legacy activation owns a clone stack");
                    // SAFETY: the guarded stack remains live through clone;
                    // the child retains its blocked signal mask through
                    // trusted setup and restores it only before the payload.
                    unsafe { clone(clone_cb, stack.as_mut_slice(), flags, Some(SIGCHLD)) }
                };

                let mut signal_mask = child_signal_mask.take().ok_or_else(|| Error::Failure {
                    message: "recover the parent signal-mask guard after legacy clone: legacy clone callback unexpectedly consumed parent state"
                        .to_owned(),
                })?;
                let restore = signal_mask.restore();
                match (clone_result, restore) {
                    (Ok(pid), Ok(())) => Ok(ChildLifecycle::Legacy { pid }),
                    (Err(source), Ok(())) => Err(Error::CloneNamespaces { source }),
                    (Ok(pid), Err(source)) => {
                        abort_child(pid);
                        Err(Error::Failure {
                            message: format!("restore the supervisor signal mask after legacy clone: {source}"),
                        })
                    }
                    (Err(clone), Err(restore)) => Err(Error::Failure {
                        message: format!(
                            "restore the supervisor signal mask after failed legacy clone: {restore}; clone also failed: {clone}"
                        ),
                    }),
                }
            })()
        };

        let child = match spawn {
            Ok(child) => child,
            Err(failure) => {
                return Err(match cgroup_leaf.take() {
                    Some(leaf) => cleanup_unstarted_cgroup(failure, leaf),
                    None => failure,
                });
            }
        };

        if let Err(source) = sync.close_child_endpoint() {
            let failure = Err(child.cleanup_after_failure(Error::Nix { source }));
            return match cgroup_leaf.take() {
                Some(leaf) => finalize_started_cgroup(failure, leaf),
                None => failure,
            };
        }

        // Both activation paths need the numeric PID for the pre-release
        // user-namespace map. The clone3 path also uses it for the exact
        // cgroup-membership diagnostic, but routes every signal and wait
        // exclusively through the retained pidfd. The legacy path remains
        // numeric under its audited single-task and waitable-SIGCHLD contract.
        let pid = child.pid();
        let result = (|| {
            // Every build receives the same one-identity credential namespace:
            // namespace root maps to the caller and no other IDs exist.
            if let Err(source) = idmap(pid) {
                return Err(child.cleanup_after_failure(Error::Idmap { source }));
            }

            if let Some(leaf) = cgroup_leaf.as_ref() {
                let expected_tgid = match u32::try_from(pid.as_raw()) {
                    Ok(tgid) => tgid,
                    Err(_) => {
                        return Err(child.cleanup_after_failure(Error::Failure {
                            message: format!("clone3 returned invalid child TGID {}", pid.as_raw()),
                        }));
                    }
                };
                if let Err(source) = leaf.require_sole_member(expected_tgid) {
                    return Err(child.cleanup_after_failure(Error::CgroupLifecycle { source }));
                }
            }

            // Signal dispositions are process-global. Serialize the override
            // and install it before releasing the child, then restore the
            // exact prior action on every path through the RAII guard.
            let mut sigint_override = if self.ignore_host_sigint {
                match SignalOverride::install(Signal::SIGINT) {
                    Ok(override_) => Some(override_),
                    Err(source) => {
                        return Err(child.cleanup_after_failure(Error::Nix { source }));
                    }
                }
            } else {
                None
            };

            match send_packet_no_signal(sync.supervisor_fd(), &[Message::Continue as u8]) {
                Ok(1) => {}
                Ok(_) => {
                    return Err(child.cleanup_after_failure(Error::Nix { source: Errno::EIO }));
                }
                Err(source) => {
                    return Err(child.cleanup_after_failure(Error::Nix { source }));
                }
            }
            let status = match child.wait() {
                Ok(status) => status,
                Err(source) => {
                    return Err(child.cleanup_after_failure(Error::Nix { source }));
                }
            };

            if let Some(override_) = sigint_override.take() {
                override_.restore().context(NixSnafu)?;
            }

            match status {
                WaitStatus::Exited(_, 0) => Ok(()),
                WaitStatus::Exited(..) => {
                    let error = read_child_error(sync.supervisor_fd()).context(NixSnafu)?;
                    Err(Error::Failure { message: error })
                }
                WaitStatus::Signaled(_, signal, _) => Err(Error::Signaled { signal }),
                WaitStatus::Stopped(..)
                | WaitStatus::PtraceEvent(..)
                | WaitStatus::PtraceSyscall(_)
                | WaitStatus::Continued(_)
                | WaitStatus::StillAlive => Err(child.cleanup_after_failure(Error::UnknownExit)),
            }
        })();

        match cgroup_leaf.take() {
            Some(leaf) => finalize_started_cgroup(result, leaf),
            None => result,
        }
    }
}

const CGROUP_SUPER_MAGIC: nix::libc::c_long = 0x0027_e0eb;
const CGROUP2_SUPER_MAGIC: nix::libc::c_long = 0x6367_7270;

fn require_atomic_cgroup_policy(container: &Container) -> Result<(), Error> {
    let Some(root) = container.root_anchor.as_ref() else {
        return Err(Error::AtomicCgroupRequiresAnchoredRoot);
    };
    let filesystem =
        descriptor_filesystem_magic(root.as_raw_fd()).map_err(|source| Error::InspectCgroupFilesystem {
            label: container.root.clone(),
            source,
        })?;
    if is_cgroup_filesystem(filesystem) {
        return Err(Error::UnsafeCgroupRootFilesystem {
            label: container.root.clone(),
        });
    }
    if container.pseudo_filesystems.sys == SysPolicy::HostReadWrite {
        return Err(Error::UnsafeCgroupSysPolicy);
    }
    Ok(())
}

fn require_atomic_cgroup_bind_policy(bind_sources: &[PinnedAnchoredBindSource]) -> Result<(), Error> {
    for bind in bind_sources.iter().filter(|bind| !bind.read_only) {
        let filesystem =
            descriptor_filesystem_magic(bind.source.as_raw_fd()).map_err(|source| Error::InspectCgroupFilesystem {
                label: bind.source_label.clone(),
                source,
            })?;
        if is_cgroup_filesystem(filesystem) {
            return Err(Error::UnsafeCgroupBindSource {
                label: bind.source_label.clone(),
            });
        }
    }
    Ok(())
}

fn descriptor_filesystem_magic(fd: RawFd) -> Result<nix::libc::c_long, Errno> {
    // SAFETY: stat is a live writable output object and fd remains live for
    // the complete fstatfs call.
    let mut stat: nix::libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { nix::libc::fstatfs(fd, &mut stat) } == -1 {
        return Err(Errno::last());
    }
    Ok(stat.f_type)
}

fn is_cgroup_filesystem(filesystem: nix::libc::c_long) -> bool {
    matches!(filesystem, CGROUP_SUPER_MAGIC | CGROUP2_SUPER_MAGIC)
}

fn child_exit_code<E>(
    container: &mut Container,
    anchored_bind_sources: &mut Vec<PinnedAnchoredBindSource>,
    sync: (RawFd, RawFd),
    signal_mask: Option<BlockedSignalMask>,
    f: &mut impl FnMut() -> Result<(), E>,
) -> i32
where
    E: std::error::Error + Send + Sync + 'static,
{
    match close_sync_endpoint(sync.0) {
        Ok(()) => {}
        Err(source) => {
            return report_child_error(sync.1, ContainerError::CloseSupervisorSync { source });
        }
    }
    match enter(container, anchored_bind_sources, sync.1, signal_mask, f) {
        Ok(()) => 0,
        Err(error) => report_child_error(sync.1, error),
    }
}

fn report_child_error(error_writer: RawFd, error: ContainerError) -> i32 {
    let error = format_error(error);
    report_child_error_bytes(error_writer, error.as_bytes())
}

fn report_child_error_bytes(error_writer: RawFd, error: &[u8]) -> i32 {
    let error = &error[..error.len().min(MAX_CHILD_ERROR_BYTES)];
    for _ in 0..3 {
        match send_packet_no_signal(error_writer, error) {
            Ok(_) => break,
            Err(Errno::EINTR) => continue,
            Err(_) => break,
        }
    }
    let _ = close(error_writer);
    1
}

fn contain_raw_clone_child_panic(error_writer: RawFd, child: impl FnOnce() -> i32) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(child)) {
        Ok(exit_code) => exit_code,
        Err(_) => report_child_error_bytes(
            error_writer,
            b"raw fork-like clone child panicked; payload setup was aborted before returning through the cloned parent stack",
        ),
    }
}

fn close_inherited_cgroup_descriptors(descriptors: [RawFd; 2]) -> Result<(), ContainerError> {
    if descriptors[0] == descriptors[1] || descriptors.iter().any(|descriptor| *descriptor < 0) {
        return InvalidInheritedCgroupDescriptorsSnafu { descriptors }.fail();
    }
    for descriptor in descriptors {
        match close(descriptor) {
            Ok(()) | Err(Errno::EINTR) => {}
            Err(source) => {
                return Err(source).context(CloseInheritedCgroupDescriptorSnafu { descriptor });
            }
        }
        // Linux closes a descriptor even when close(2) reports EINTR. Prove
        // the child retained no cgroup capability before it waits or performs
        // any namespace setup.
        let result = unsafe { nix::libc::fcntl(descriptor, nix::libc::F_GETFD) };
        if result != -1 || Errno::last() != Errno::EBADF {
            return RetainedInheritedCgroupDescriptorSnafu { descriptor }.fail();
        }
    }
    Ok(())
}

fn cleanup_unstarted_cgroup(failure: Error, mut leaf: cgroup::CgroupLeaf) -> Error {
    match leaf.remove_unstarted() {
        Ok(()) => failure,
        Err(cleanup) => Error::CgroupCleanupAfterFailure {
            failure: Box::new(failure),
            cleanup,
            leaf: Some(Box::new(leaf)),
        },
    }
}

fn finalize_started_cgroup(result: Result<(), Error>, mut leaf: cgroup::CgroupLeaf) -> Result<(), Error> {
    match leaf.kill_and_remove(cgroup::DrainPolicy::default()) {
        // cgroup.kill plus a successful drain proves that no task remains in
        // the leaf. If an earlier exact-child cleanup timed out, make one more
        // pidfd-only reap attempt before returning the structured failure.
        Ok(()) => match result {
            Ok(()) => Ok(()),
            Err(failure) => failure.retry_child_cleanup_after_cgroup(),
        },
        Err(cleanup) => Err(match result {
            Ok(()) => Error::CgroupCleanup {
                cleanup,
                leaf: Some(Box::new(leaf)),
            },
            Err(failure) => Error::CgroupCleanupAfterFailure {
                failure: Box::new(failure),
                cleanup,
                leaf: Some(Box::new(leaf)),
            },
        }),
    }
}

fn read_child_error(fd: RawFd) -> Result<String, Errno> {
    // The child has already been reaped, so its one bounded atomic write is
    // complete. A raw-forked descendant could nevertheless retain a copy of
    // the close-on-exec writer without executing; nonblocking reads ensure
    // such a leaked writer cannot hold supervision open forever.
    set_fd_nonblocking(fd)?;

    let mut bytes = Vec::with_capacity(MAX_CHILD_ERROR_BYTES);
    // One SOCK_SEQPACKET diagnostic is at most this size. Reading it into a
    // smaller buffer would truncate and discard the packet remainder.
    let mut buffer = [0_u8; MAX_CHILD_ERROR_BYTES];
    let mut interrupted = 0;
    while bytes.len() < MAX_CHILD_ERROR_BYTES {
        let remaining = MAX_CHILD_ERROR_BYTES - bytes.len();
        let chunk = remaining.min(buffer.len());
        let len = match read(fd, &mut buffer[..chunk]) {
            Err(Errno::EINTR) if interrupted < MAX_CONTROL_EINTR_RETRIES => {
                interrupted += 1;
                continue;
            }
            Err(Errno::EAGAIN) => break,
            result => result?,
        };
        interrupted = 0;
        if len == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..len]);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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
fn enter<E>(
    container: &mut Container,
    anchored_bind_sources: &mut Vec<PinnedAnchoredBindSource>,
    sync: RawFd,
    mut signal_mask: Option<BlockedSignalMask>,
    mut f: impl FnMut() -> Result<(), E>,
) -> Result<(), ContainerError>
where
    E: std::error::Error + Send + Sync + 'static,
{
    // Ensure process is cleaned up if parent dies
    set_pdeathsig(Signal::SIGKILL).context(SetPDeathSigSnafu)?;

    // Wait for continue message
    let mut message = [0u8; 1];
    let len = read(sync, &mut message).context(ReadContinueMsgSnafu)?;
    if len != 1 || message[0] != Message::Continue as u8 {
        return InvalidContinueMsgSnafu.fail();
    }

    // The parent deliberately leaves setgroups enabled until this point.  A
    // rootless user namespace otherwise freezes the caller's ambient
    // supplementary groups into every build.  Drop them before any mount,
    // process, or package-analysis work can observe them, then prove the
    // namespace-visible identity is the fixed root credential contract.
    isolate_payload_credentials()?;

    setup(container, anchored_bind_sources)?;

    // Root and bind-source descriptors are setup capabilities, not payload
    // capabilities. The clone child has a private descriptor table, so
    // dropping its copies leaves the supervising parent's copies intact while
    // ensuring Rust payload code cannot inspect authenticated host objects.
    anchored_bind_sources.clear();
    drop(container.root_anchor.take());
    // Descriptor-pinned bind handles stored by Container are setup-only too.
    // Clearing the child copy does not affect the supervising parent's
    // copy-on-write Container value.
    container.binds.clear();

    // Descriptor and privilege confinement are container invariants, not
    // optional consequences of selecting a descriptor-backed or read-only
    // root. A pathname container retaining a host directory descriptor can
    // otherwise escape its pivoted root with openat(2), while a writable-root
    // payload can use namespace-root capabilities to recreate setup-only
    // authority such as the auxiliary GID or arbitrary device nodes.
    validate_payload_standard_fds()?;
    sanitize_payload_fds(sync)?;
    restrict_payload_scheduler()?;
    drop_all_payload_capabilities()?;
    seccomp::install_payload_filter().context(InstallPayloadSeccompSnafu)?;

    // Both fork-like activation paths block every catchable signal before
    // their exact task audit. Keep that mask through trusted setup, then
    // restore it only inside the child boundary immediately before arbitrary
    // payload code.
    if let Some(signal_mask) = signal_mask.as_mut() {
        signal_mask.restore().context(RestoreCloneSignalMaskSnafu)?;
    }

    let result = f().boxed().context(RunSnafu);
    if result.is_ok() {
        // Errors retain the write end so the outer clone callback can report
        // them. A successful Rust payload has nothing left to report.
        let _ = close(sync);
    }
    result
}

fn sanitize_payload_fds(error_writer: RawFd) -> Result<(), ContainerError> {
    if error_writer < 3 {
        return InvalidPayloadErrorDescriptorSnafu { fd: error_writer }.fail();
    }
    close_range(3, error_writer as u32 - 1)?;
    close_range(error_writer as u32 + 1, u32::MAX)
}

fn validate_payload_standard_fds() -> Result<(), ContainerError> {
    for fd in 0..=2 {
        // A closed standard descriptor carries no authority and is allowed.
        // Any live descriptor is inspected before the rest of the inherited
        // table is closed. Directory and O_PATH descriptors are pathname
        // capabilities rather than ordinary byte streams and must never be
        // handed to untrusted payload code through stdin/stdout/stderr.
        let mut stat: nix::libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { nix::libc::fstat(fd, &mut stat) } == -1 {
            let source = Errno::last();
            if source == Errno::EBADF {
                continue;
            }
            return Err(source).context(InspectPayloadStandardDescriptorSnafu { fd });
        }
        let status_flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
        if status_flags == -1 {
            return Err(Errno::last()).context(InspectPayloadStandardDescriptorSnafu { fd });
        }
        let filesystem = descriptor_filesystem_magic(fd).context(InspectPayloadStandardDescriptorSnafu { fd })?;
        let kind = stat.st_mode & nix::libc::S_IFMT;
        if standard_descriptor_is_unsafe(kind, status_flags) {
            return UnsafePayloadStandardDescriptorSnafu { fd, kind, status_flags }.fail();
        }
        if is_cgroup_filesystem(filesystem) && status_flags & nix::libc::O_ACCMODE != nix::libc::O_RDONLY {
            return UnsafeCgroupStandardDescriptorSnafu {
                fd,
                filesystem,
                status_flags,
            }
            .fail();
        }
    }
    Ok(())
}

fn standard_descriptor_is_unsafe(kind: nix::libc::mode_t, status_flags: nix::libc::c_int) -> bool {
    kind == nix::libc::S_IFDIR || status_flags & nix::libc::O_PATH == nix::libc::O_PATH
}

fn close_range(first: u32, last: u32) -> Result<(), ContainerError> {
    if first > last {
        return Ok(());
    }
    // SAFETY: close_range takes scalar arguments only. This child has a private
    // descriptor table because CLONE_FILES is not requested.
    let result = unsafe { syscall(nix::libc::SYS_close_range, first, last, 0_u32) };
    if result == -1 {
        return Err(Errno::last()).context(SanitizePayloadDescriptorsSnafu);
    }
    Ok(())
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
// Version 3 exposes two u32 words. Linux capability numbers are contiguous;
// PR_CAPBSET_READ reports EINVAL at the first unsupported number.
const MAX_LINUX_CAPABILITY_NUMBER: u32 = 63;

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

/// Remove every Linux capability from the untrusted payload.
///
/// A denylist is not a stable confinement boundary: namespace UID zero can use
/// capabilities such as CAP_SETGID, CAP_CHOWN, CAP_MKNOD,
/// CAP_DAC_READ_SEARCH, or CAP_SYS_CHROOT to recover setup-only authority or
/// bypass pathname policy. Clearing only the live sets is also insufficient,
/// because a later execve can regain capabilities from the bounding set. Drop
/// every supported bounding entry first, clear the ambient and live sets, then
/// verify all three sources of authority.
fn drop_all_payload_capabilities() -> Result<(), ContainerError> {
    let capabilities = supported_capability_numbers().context(DropPayloadCapabilitiesSnafu)?;
    unsafe {
        for &capability in &capabilities {
            let present = checked_prctl_value(prctl(PR_CAPBSET_READ, capability, 0, 0, 0))
                .context(DropPayloadCapabilitiesSnafu)?;
            if present != 0 {
                checked_prctl(prctl(PR_CAPBSET_DROP, capability, 0, 0, 0)).context(DropPayloadCapabilitiesSnafu)?;
            }
        }
        checked_prctl(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0))
            .context(DropPayloadCapabilitiesSnafu)?;
    }

    write_capabilities(&[CapabilityData::default(); 2]).context(DropPayloadCapabilitiesSnafu)?;

    let live = read_capabilities().context(DropPayloadCapabilitiesSnafu)?;
    for capability in capabilities {
        let retained_live = capability_is_set(&live, capability);
        let retained_bounding = unsafe {
            checked_prctl_value(prctl(PR_CAPBSET_READ, capability, 0, 0, 0)).context(DropPayloadCapabilitiesSnafu)? != 0
        };
        let retained_ambient = unsafe {
            checked_prctl_value(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, capability, 0, 0))
                .context(DropPayloadCapabilitiesSnafu)?
                != 0
        };
        if retained_live || retained_bounding || retained_ambient {
            return PayloadRetainsCapabilitySnafu { capability }.fail();
        }
    }
    Ok(())
}

fn supported_capability_numbers() -> Result<Vec<u32>, Errno> {
    let mut capabilities = Vec::new();
    let mut reached_kernel_end = false;
    for capability in 0..=MAX_LINUX_CAPABILITY_NUMBER {
        let result = unsafe { prctl(PR_CAPBSET_READ, capability, 0, 0, 0) };
        if result == -1 {
            let source = Errno::last();
            if source == Errno::EINVAL {
                reached_kernel_end = true;
                break;
            }
            return Err(source);
        }
        capabilities.push(capability);
    }
    if capabilities.is_empty() {
        return Err(Errno::EINVAL);
    }
    if !reached_kernel_end {
        // Capability ABI v3 has exactly 64 live-set bits. If a future kernel
        // exposes capability 64, silently clearing only the old range would
        // leave an unknown privilege recoverable after execve.
        let unsupported = MAX_LINUX_CAPABILITY_NUMBER + 1;
        let result = unsafe { prctl(PR_CAPBSET_READ, unsupported, 0, 0, 0) };
        if result != -1 {
            return Err(Errno::EOVERFLOW);
        }
        let source = Errno::last();
        if source != Errno::EINVAL {
            return Err(source);
        }
    }
    Ok(capabilities)
}

/// Make `cpu.max` an enforceable aggregate ceiling for the eventual cgroup
/// boundary. The controller throttles fair-class work; therefore the payload
/// must neither inherit a real-time/deadline policy nor retain a route back to
/// one. All capabilities, including CAP_SYS_NICE, are removed separately by
/// [`drop_all_payload_capabilities`].
fn restrict_payload_scheduler() -> Result<(), ContainerError> {
    let zero = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { nix::libc::setrlimit(nix::libc::RLIMIT_RTPRIO, &zero) } == -1 {
        return Err(Errno::last()).context(RestrictPayloadSchedulerSnafu);
    }

    let parameter = nix::libc::sched_param { sched_priority: 0 };
    if unsafe { nix::libc::sched_setscheduler(0, nix::libc::SCHED_OTHER, &parameter) } == -1 {
        return Err(Errno::last()).context(RestrictPayloadSchedulerSnafu);
    }

    let policy = unsafe { nix::libc::sched_getscheduler(0) };
    if policy == -1 {
        return Err(Errno::last()).context(RestrictPayloadSchedulerSnafu);
    }
    let mut limit = nix::libc::rlimit {
        rlim_cur: nix::libc::RLIM_INFINITY,
        rlim_max: nix::libc::RLIM_INFINITY,
    };
    if unsafe { nix::libc::getrlimit(nix::libc::RLIMIT_RTPRIO, &mut limit) } == -1 {
        return Err(Errno::last()).context(RestrictPayloadSchedulerSnafu);
    }
    if policy != nix::libc::SCHED_OTHER || limit.rlim_cur != 0 || limit.rlim_max != 0 {
        return PayloadRetainsRealtimeSchedulingSnafu {
            policy,
            soft_limit: limit.rlim_cur,
            hard_limit: limit.rlim_max,
        }
        .fail();
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
fn setup(container: &Container, anchored_bind_sources: &[PinnedAnchoredBindSource]) -> Result<(), ContainerError> {
    if container.networking && container.root_anchor.is_none() {
        setup_networking(&container.root)?;
    }

    if matches!(container.loopback, LoopbackPolicy::HostIpIfAvailable) {
        setup_localhost()?;
    }

    if let Some(anchor) = &container.root_anchor {
        pivot_anchored(
            &container.root,
            anchor.as_raw_fd(),
            anchored_bind_sources,
            container.networking,
            container.pseudo_filesystems,
            container.root_filesystem,
        )?;
    } else {
        pivot(
            &container.root,
            &container.binds,
            container.pseudo_filesystems,
            container.root_filesystem,
        )?;
    }

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
    add_mount(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE)?;
    add_mount(Some(root), root, None, MsFlags::MS_BIND)?;

    pivot_mounted_root(root, binds, pseudo_filesystems, root_filesystem)
}

/// Clone and attach the exact mount referenced by `anchor`, then pivot through
/// the retained mount descriptor. `label` is diagnostic-only: even if another
/// process removes or replaces it, both sides of activation remain anchored by
/// descriptor-empty paths.
fn pivot_anchored(
    label: &Path,
    anchor: RawFd,
    bind_sources: &[PinnedAnchoredBindSource],
    networking: bool,
    pseudo_filesystems: PseudoFilesystemPolicy,
    root_filesystem: RootFilesystemPolicy,
) -> Result<(), ContainerError> {
    add_mount(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE)?;

    // Prepare every source as a detached mount before entering the root. This
    // does not modify the authenticated tree and ensures later setup never
    // reopens a source pathname.
    let mut prepared_mounts = prepare_anchored_binds(bind_sources)?;
    if networking {
        prepared_mounts.push(prepare_anchored_resolver_mount()?);
    }
    for decision in pseudo_mount_decisions(pseudo_filesystems) {
        prepared_mounts.push(prepare_anchored_pseudo_mount(decision)?);
    }
    validate_anchored_mount_topology(&prepared_mounts)?;

    let root_mount = clone_anchored_root(label, anchor)?;
    attach_anchored_root(label, anchor, &root_mount)?;
    fchdir(root_mount.as_raw_fd()).with_context(|_| ActivateAnchoredRootSnafu {
        label: label.to_owned(),
        operation: "enter attached root mount",
    })?;

    // Pin every target against the untouched cloned root before the first
    // submount is attached. Earlier mounts can therefore never provide a later
    // target, even if a caller accidentally declares overlapping paths.
    let ready_mounts = pin_anchored_mount_targets(root_mount.as_raw_fd(), prepared_mounts)?;

    if matches!(root_filesystem, RootFilesystemPolicy::ReadOnly) {
        set_mount_access_fd(root_mount.as_raw_fd(), true, true).with_context(|_| MountSnafu {
            target: label.to_owned(),
        })?;
    }
    for prepared in &ready_mounts {
        attach_ready_anchored_mount(prepared)?;
    }

    let root = Path::new(".");
    // The same-path pivot idiom stacks the old namespace root over the new
    // descriptor-mounted root without creating a put_old directory in the
    // authenticated backing tree. cwd denotes the old root immediately after
    // pivot_root; detach it before entering the new `/`.
    pivot_root(root, root).context(PivotRootSnafu)?;
    umount2(root, MntFlags::MNT_DETACH).context(UnmountOldRootSnafu)?;
    set_current_dir("/")?;
    umask(Mode::S_IWGRP | Mode::S_IWOTH);
    Ok(())
}

fn clone_anchored_root(label: &Path, anchor: RawFd) -> Result<OwnedFd, ContainerError> {
    // SAFETY: `anchor` is the live duplicate owned by Container and an empty
    // path is explicitly admitted by AT_EMPTY_PATH. Deliberately omitting
    // AT_RECURSIVE clones only the authenticated root mount, so undeclared
    // nested mounts cannot enter the frozen root. A successful call returns a
    // fresh detached mount descriptor.
    let descriptor = unsafe {
        open_tree(
            anchor,
            Path::new(""),
            OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH as u32,
        )
    }
    .map_err(Errno::from_i32)
    .with_context(|_| ActivateAnchoredRootSnafu {
        label: label.to_owned(),
        operation: "clone descriptor-backed root mount",
    })?;
    // SAFETY: successful open_tree returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn attach_anchored_root(label: &Path, anchor: RawFd, root_mount: &OwnedFd) -> Result<(), ContainerError> {
    // SAFETY: root_mount is a live detached mount descriptor, anchor is the
    // duplicated O_PATH directory descriptor, and both remain owned until
    // after pivot_root. The flags explicitly admit both empty paths.
    unsafe {
        move_mount(
            root_mount.as_raw_fd(),
            Path::new(""),
            anchor,
            Path::new(""),
            MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH,
        )
    }
    .map_err(Errno::from_i32)
    .with_context(|_| ActivateAnchoredRootSnafu {
        label: label.to_owned(),
        operation: "attach descriptor-backed root mount",
    })
    .map(|_| ())
}

// Linux PATH_MAX includes the terminating NUL byte.
const MAX_ANCHORED_MOUNT_TARGET_BYTES: usize = 4095;
const MAX_ANCHORED_MOUNT_TARGET_COMPONENTS: usize = 256;
const MAX_ANCHORED_MOUNT_COMPONENT_BYTES: usize = 255;
const MAX_ANCHORED_MOUNTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnchoredMountTargetKind {
    Directory,
    RegularFile,
}

struct PreparedAnchoredMount {
    source_mount: OwnedFd,
    target: PathBuf,
    target_kind: AnchoredMountTargetKind,
}

struct ReadyAnchoredMount {
    source_mount: OwnedFd,
    target: PathBuf,
    target_descriptor: OwnedFd,
}

struct PinnedAnchoredBindSource {
    source: OwnedFd,
    source_label: PathBuf,
    target: PathBuf,
    target_kind: AnchoredMountTargetKind,
    read_only: bool,
}

fn pin_anchored_bind_sources(root: RawFd, binds: &[Bind]) -> Result<Vec<PinnedAnchoredBindSource>, ContainerError> {
    if binds.len() > MAX_ANCHORED_MOUNTS {
        return Err(ContainerError::TooManyAnchoredMounts {
            actual: binds.len(),
            limit: MAX_ANCHORED_MOUNTS,
        });
    }
    binds
        .iter()
        .map(|bind| {
            let (source, source_label) = match &bind.source {
                BindSource::Path(path) => {
                    return Err(ContainerError::UnpinnedAnchoredMountSource { path: path.clone() });
                }
                BindSource::RootRelative(path) => {
                    let source = openat2_anchored(
                        root,
                        path,
                        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                        0,
                        nix::libc::RESOLVE_BENEATH
                            | nix::libc::RESOLVE_NO_XDEV
                            | nix::libc::RESOLVE_NO_MAGICLINKS
                            | nix::libc::RESOLVE_NO_SYMLINKS,
                    )
                    .map_err(|source| ContainerError::OpenAnchoredMountSource {
                        path: path.clone(),
                        source,
                    })?;
                    (source, path.clone())
                }
                BindSource::Pinned { descriptor, label } => {
                    let source = duplicate_cloexec(descriptor.as_raw_fd()).map_err(|source| {
                        ContainerError::OpenAnchoredMountSource {
                            path: label.clone(),
                            source,
                        }
                    })?;
                    (source, label.clone())
                }
            };
            let target_kind = descriptor_target_kind(source.as_raw_fd(), &source_label)?;
            Ok(PinnedAnchoredBindSource {
                source,
                source_label,
                target: normalized_anchored_mount_target(&bind.target)?,
                target_kind,
                read_only: bind.read_only,
            })
        })
        .collect()
}

fn prepare_anchored_binds(
    bind_sources: &[PinnedAnchoredBindSource],
) -> Result<Vec<PreparedAnchoredMount>, ContainerError> {
    bind_sources
        .iter()
        .map(|bind| {
            // Clone exactly the pinned object. In particular, a directory
            // source must not recursively import mounts that appeared below
            // it on the host; pseudo-filesystem trees are the only explicitly
            // recursive anchored imports.
            let flags = OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH as u32;
            // SAFETY: source is the live O_PATH descriptor pinned before
            // clone(2), the empty path is admitted explicitly, and a
            // successful open_tree returns a fresh detached mount descriptor.
            let descriptor = unsafe { open_tree(bind.source.as_raw_fd(), Path::new(""), flags) }
                .map_err(Errno::from_i32)
                .with_context(|_| MountSnafu {
                    target: bind.source_label.clone(),
                })?;
            // SAFETY: successful open_tree returned a fresh owned descriptor.
            let source_mount = unsafe { OwnedFd::from_raw_fd(descriptor) };
            if bind.read_only {
                set_mount_access_fd(source_mount.as_raw_fd(), true, false).with_context(|_| MountSnafu {
                    target: bind.source_label.clone(),
                })?;
            }
            Ok(PreparedAnchoredMount {
                source_mount,
                target: bind.target.clone(),
                target_kind: bind.target_kind,
            })
        })
        .collect()
}

fn descriptor_target_kind(fd: RawFd, label: &Path) -> Result<AnchoredMountTargetKind, ContainerError> {
    let stat = descriptor_stat(fd).context(FsErrSnafu)?;
    match stat.st_mode & nix::libc::S_IFMT {
        nix::libc::S_IFDIR => Ok(AnchoredMountTargetKind::Directory),
        nix::libc::S_IFREG => Ok(AnchoredMountTargetKind::RegularFile),
        mode => Err(ContainerError::UnsupportedAnchoredMountSource {
            path: label.to_owned(),
            mode,
        }),
    }
}

fn normalized_anchored_mount_target(target: &Path) -> Result<PathBuf, ContainerError> {
    let bytes = target.as_os_str().as_bytes();
    if !target.is_absolute() || bytes.is_empty() || bytes.len() > MAX_ANCHORED_MOUNT_TARGET_BYTES || bytes.contains(&0)
    {
        return Err(ContainerError::InvalidAnchoredMountTarget {
            path: target.to_owned(),
        });
    }
    if bytes
        .split(|byte| *byte == b'/')
        .any(|component| component == b"." || component == b"..")
    {
        return Err(ContainerError::InvalidAnchoredMountTarget {
            path: target.to_owned(),
        });
    }
    let mut normalized = PathBuf::new();
    let mut components = 0usize;
    for component in target.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(component) => {
                components = components.saturating_add(1);
                if components > MAX_ANCHORED_MOUNT_TARGET_COMPONENTS
                    || component.as_bytes().len() > MAX_ANCHORED_MOUNT_COMPONENT_BYTES
                {
                    return Err(ContainerError::InvalidAnchoredMountTarget {
                        path: target.to_owned(),
                    });
                }
                normalized.push(component);
            }
            std::path::Component::CurDir | std::path::Component::ParentDir | std::path::Component::Prefix(_) => {
                return Err(ContainerError::InvalidAnchoredMountTarget {
                    path: target.to_owned(),
                });
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(ContainerError::InvalidAnchoredMountTarget {
            path: target.to_owned(),
        });
    }
    Ok(normalized)
}

fn validate_anchored_mount_topology(mounts: &[PreparedAnchoredMount]) -> Result<(), ContainerError> {
    if mounts.len() > MAX_ANCHORED_MOUNTS {
        return Err(ContainerError::TooManyAnchoredMounts {
            actual: mounts.len(),
            limit: MAX_ANCHORED_MOUNTS,
        });
    }
    for (index, mount) in mounts.iter().enumerate() {
        for other in &mounts[index + 1..] {
            if mount.target == other.target
                || mount.target.starts_with(&other.target)
                || other.target.starts_with(&mount.target)
            {
                return Err(ContainerError::OverlappingAnchoredMountTargets {
                    first: mount.target.clone(),
                    second: other.target.clone(),
                });
            }
        }
    }
    Ok(())
}

fn pin_anchored_mount_targets(
    root: RawFd,
    mounts: Vec<PreparedAnchoredMount>,
) -> Result<Vec<ReadyAnchoredMount>, ContainerError> {
    mounts
        .into_iter()
        .map(|mount| {
            let target_descriptor = open_anchored_mount_target(root, &mount.target, mount.target_kind)?;
            Ok(ReadyAnchoredMount {
                source_mount: mount.source_mount,
                target: mount.target,
                target_descriptor,
            })
        })
        .collect()
}

fn attach_ready_anchored_mount(prepared: &ReadyAnchoredMount) -> Result<(), ContainerError> {
    move_mount_empty(
        prepared.source_mount.as_raw_fd(),
        prepared.target_descriptor.as_raw_fd(),
    )
    .with_context(|_| MountSnafu {
        target: prepared.target.clone(),
    })
}

fn open_anchored_mount_target(
    root: RawFd,
    target: &Path,
    kind: AnchoredMountTargetKind,
) -> Result<OwnedFd, ContainerError> {
    let flags = nix::libc::O_PATH
        | nix::libc::O_CLOEXEC
        | nix::libc::O_NOFOLLOW
        | if matches!(kind, AnchoredMountTargetKind::Directory) {
            nix::libc::O_DIRECTORY
        } else {
            0
        };
    let descriptor = openat2_anchored(
        root,
        target,
        flags,
        0,
        nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_XDEV
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_SYMLINKS,
    )
    .map_err(|source| ContainerError::OpenAnchoredMountTarget {
        path: target.to_owned(),
        source,
    })?;
    let stat = descriptor_stat(descriptor.as_raw_fd()).map_err(|source| ContainerError::OpenAnchoredMountTarget {
        path: target.to_owned(),
        source,
    })?;
    let actual = match stat.st_mode & nix::libc::S_IFMT {
        nix::libc::S_IFDIR => AnchoredMountTargetKind::Directory,
        nix::libc::S_IFREG => AnchoredMountTargetKind::RegularFile,
        mode => {
            return Err(ContainerError::UnsafeAnchoredMountTarget {
                path: target.to_owned(),
                mode,
            });
        }
    };
    if actual != kind {
        return Err(ContainerError::AnchoredMountTargetType {
            path: target.to_owned(),
            expected: kind,
            actual,
        });
    }
    Ok(descriptor)
}

fn move_mount_empty(source: RawFd, target: RawFd) -> Result<(), Errno> {
    // SAFETY: both descriptors are live mount/source target references and the
    // explicit flags admit both empty paths.
    unsafe {
        move_mount(
            source,
            Path::new(""),
            target,
            Path::new(""),
            MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH,
        )
    }
    .map_err(Errno::from_i32)
    .map(|_| ())
}

fn descriptor_stat(fd: RawFd) -> io::Result<nix::libc::stat> {
    // SAFETY: zero is valid initialization for stat, fd is live, and the
    // output object remains exclusively borrowed for the call.
    let mut stat: nix::libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { nix::libc::fstat(fd, &mut stat) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat)
}

fn openat2_anchored(
    parent: RawFd,
    path: &Path,
    flags: nix::libc::c_int,
    mode: nix::libc::mode_t,
    resolve: u64,
) -> io::Result<OwnedFd> {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: zero is valid for every open_how field.
    let mut how: nix::libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: every pointer remains live for the call and a successful syscall
    // returns a fresh descriptor.
    let descriptor = unsafe {
        syscall(
            nix::libc::SYS_openat2,
            parent,
            path.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = RawFd::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn canonical_bind_sources(binds: &[Bind]) -> Result<Vec<PathBuf>, ContainerError> {
    binds
        .iter()
        .map(|bind| match &bind.source {
            BindSource::Path(path) => path.fs_err_canonicalize().context(FsErrSnafu),
            BindSource::RootRelative(path) | BindSource::Pinned { label: path, .. } => {
                Err(ContainerError::AnchoredBindOnPathContainer { path: path.clone() })
            }
        })
        .collect()
}

fn pivot_mounted_root(
    root: &Path,
    binds: &[Bind],
    pseudo_filesystems: PseudoFilesystemPolicy,
    root_filesystem: RootFilesystemPolicy,
) -> Result<(), ContainerError> {
    let sources = canonical_bind_sources(binds)?;
    pivot_mounted_root_with_sources(root, binds, &sources, pseudo_filesystems, root_filesystem)
}

fn pivot_mounted_root_with_sources(
    root: &Path,
    binds: &[Bind],
    sources: &[PathBuf],
    pseudo_filesystems: PseudoFilesystemPolicy,
    root_filesystem: RootFilesystemPolicy,
) -> Result<(), ContainerError> {
    const OLD_PATH: &str = "old_root";

    let old_root = root.join(OLD_PATH);

    mount_binds(root, binds, sources)?;

    ensure_directory(&old_root)?;
    apply_root_mount_policy(root, binds, root_filesystem)?;
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

fn mount_binds(root: &Path, binds: &[Bind], sources: &[PathBuf]) -> Result<(), ContainerError> {
    for (bind, source) in binds.iter().zip(sources) {
        let target = root.join(bind.target.strip_prefix("/").unwrap_or(&bind.target));
        bind_mount(source, &target, bind.read_only)?;
    }
    Ok(())
}

fn apply_root_mount_policy(
    root: &Path,
    binds: &[Bind],
    root_filesystem: RootFilesystemPolicy,
) -> Result<(), ContainerError> {
    for decision in root_mount_decisions(root, binds, root_filesystem) {
        match decision {
            RootMountDecision::ReadOnlyRecursive(target) => set_mount_access(&target, true, true)?,
            RootMountDecision::ReadWriteExact(target) => set_mount_access(&target, false, false)?,
        }
    }
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

/// Prepare a pseudo-filesystem as a detached mount. Target descriptors are
/// opened later, as one batch against the untouched authenticated root.
fn prepare_anchored_pseudo_mount(decision: PseudoMountDecision) -> Result<PreparedAnchoredMount, ContainerError> {
    let (source_mount, target) = match decision {
        PseudoMountDecision::Proc { read_only } => {
            let source = detached_filesystem_mount(c"proc", read_only, Path::new("proc"))?;
            (source, PathBuf::from("proc"))
        }
        PseudoMountDecision::EmptyTmp => {
            let source = detached_filesystem_mount(c"tmpfs", false, Path::new("tmp"))?;
            (source, PathBuf::from("tmp"))
        }
        PseudoMountDecision::HostSys { read_only } => {
            let source = detached_host_mount(Path::new("/sys"), read_only)?;
            (source, PathBuf::from("sys"))
        }
        PseudoMountDecision::HostDev { read_only } => {
            let source = detached_host_mount(Path::new("/dev"), read_only)?;
            (source, PathBuf::from("dev"))
        }
        PseudoMountDecision::MinimalDev => (prepare_anchored_minimal_dev()?, PathBuf::from("dev")),
    };
    Ok(PreparedAnchoredMount {
        source_mount,
        target,
        target_kind: AnchoredMountTargetKind::Directory,
    })
}

fn detached_filesystem_mount(
    filesystem: &std::ffi::CStr,
    read_only: bool,
    label: &Path,
) -> Result<OwnedFd, ContainerError> {
    const FSOPEN_CLOEXEC: nix::libc::c_uint = 0x0000_0001;
    const FSCONFIG_CMD_CREATE: nix::libc::c_uint = 6;
    const FSMOUNT_CLOEXEC: nix::libc::c_uint = 0x0000_0001;

    // SAFETY: filesystem is NUL terminated and successful fsopen returns a
    // fresh context descriptor.
    let context = unsafe { syscall(nix::libc::SYS_fsopen, filesystem.as_ptr(), FSOPEN_CLOEXEC) };
    if context == -1 {
        return Err(Errno::last()).context(MountSnafu {
            target: label.to_owned(),
        });
    }
    let context =
        RawFd::try_from(context).map_err(|_| ContainerError::InvalidMountDescriptor { operation: "fsopen" })?;
    // SAFETY: successful fsopen returned a fresh owned descriptor.
    let context = unsafe { OwnedFd::from_raw_fd(context) };

    // SAFETY: CREATE accepts null key/value and borrows only the live context.
    let configured = unsafe {
        syscall(
            nix::libc::SYS_fsconfig,
            context.as_raw_fd(),
            FSCONFIG_CMD_CREATE,
            std::ptr::null::<nix::libc::c_char>(),
            std::ptr::null::<nix::libc::c_void>(),
            0,
        )
    };
    if configured == -1 {
        return Err(Errno::last()).context(MountSnafu {
            target: label.to_owned(),
        });
    }

    // SAFETY: the configured context is live and successful fsmount returns a
    // fresh detached mount descriptor.
    let mount = unsafe { syscall(nix::libc::SYS_fsmount, context.as_raw_fd(), FSMOUNT_CLOEXEC, 0) };
    if mount == -1 {
        return Err(Errno::last()).context(MountSnafu {
            target: label.to_owned(),
        });
    }
    let mount = RawFd::try_from(mount).map_err(|_| ContainerError::InvalidMountDescriptor { operation: "fsmount" })?;
    // SAFETY: successful fsmount returned a fresh owned descriptor.
    let mount = unsafe { OwnedFd::from_raw_fd(mount) };
    if read_only {
        set_mount_access_fd(mount.as_raw_fd(), true, true).with_context(|_| MountSnafu {
            target: label.to_owned(),
        })?;
    }
    Ok(mount)
}

fn detached_host_mount(source: &Path, read_only: bool) -> Result<OwnedFd, ContainerError> {
    // SAFETY: source remains live for the call and successful open_tree returns
    // a fresh detached recursive mount descriptor.
    let mount = unsafe {
        open_tree(
            AT_FDCWD,
            source,
            OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_RECURSIVE as u32,
        )
    }
    .map_err(Errno::from_i32)
    .with_context(|_| MountSnafu {
        target: source.to_owned(),
    })?;
    // SAFETY: successful open_tree returned a fresh owned descriptor.
    let mount = unsafe { OwnedFd::from_raw_fd(mount) };
    if read_only {
        set_mount_access_fd(mount.as_raw_fd(), true, true).with_context(|_| MountSnafu {
            target: source.to_owned(),
        })?;
    }
    Ok(mount)
}

fn prepare_anchored_minimal_dev() -> Result<OwnedFd, ContainerError> {
    let dev_mount = detached_filesystem_mount(c"tmpfs", false, Path::new("dev"))?;
    for &(device, expected_major, expected_minor) in MINIMAL_DEV_IDENTITIES {
        let name = std::ffi::CString::new(device).expect("fixed device names contain no NUL");
        let placeholder = openat_anchored(
            dev_mount.as_raw_fd(),
            &name,
            nix::libc::O_WRONLY | nix::libc::O_CREAT | nix::libc::O_EXCL | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC,
            0o600,
        )
        .map_err(|source| ContainerError::OpenAnchoredMountTarget {
            path: PathBuf::from("dev").join(device),
            source,
        })?;
        // SAFETY: placeholder is a live regular file descriptor.
        if unsafe { nix::libc::fchmod(placeholder.as_raw_fd(), 0o600) } == -1 {
            return Err(Errno::last()).context(MountSnafu {
                target: PathBuf::from("dev").join(device),
            });
        }

        let host_device = Path::new("/dev").join(device);
        // SAFETY: host_device remains live and successful open_tree returns a
        // fresh detached bind mount descriptor without opening device data.
        let device_mount = unsafe { open_tree(AT_FDCWD, &host_device, OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC) }
            .map_err(Errno::from_i32)
            .with_context(|_| MountSnafu {
                target: host_device.clone(),
            })?;
        // SAFETY: successful open_tree returned a fresh owned descriptor.
        let device_mount = unsafe { OwnedFd::from_raw_fd(device_mount) };
        validate_minimal_device_source(device_mount.as_raw_fd(), &host_device, expected_major, expected_minor)?;
        set_mount_access_fd(device_mount.as_raw_fd(), true, false).with_context(|_| MountSnafu {
            target: PathBuf::from("dev").join(device),
        })?;
        let target = openat2_anchored(
            dev_mount.as_raw_fd(),
            Path::new(device),
            nix::libc::O_PATH | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC,
            0,
            nix::libc::RESOLVE_BENEATH
                | nix::libc::RESOLVE_NO_XDEV
                | nix::libc::RESOLVE_NO_MAGICLINKS
                | nix::libc::RESOLVE_NO_SYMLINKS,
        )
        .with_context(|_| OpenAnchoredMountTargetSnafu {
            path: PathBuf::from("dev").join(device),
        })?;
        move_mount_empty(device_mount.as_raw_fd(), target.as_raw_fd()).with_context(|_| MountSnafu {
            target: PathBuf::from("dev").join(device),
        })?;
    }
    Ok(dev_mount)
}

fn validate_minimal_device_source(
    fd: RawFd,
    label: &Path,
    expected_major: u64,
    expected_minor: u64,
) -> Result<(), ContainerError> {
    let stat = descriptor_stat(fd).context(FsErrSnafu)?;
    let mode = stat.st_mode & nix::libc::S_IFMT;
    if mode != nix::libc::S_IFCHR {
        return Err(ContainerError::UnsupportedAnchoredMountSource {
            path: label.to_owned(),
            mode,
        });
    }
    let actual_major = nix::libc::major(stat.st_rdev) as u64;
    let actual_minor = nix::libc::minor(stat.st_rdev) as u64;
    if (actual_major, actual_minor) != (expected_major, expected_minor) {
        return Err(ContainerError::UnexpectedMinimalDeviceIdentity {
            path: label.to_owned(),
            expected_major,
            expected_minor,
            actual_major,
            actual_minor,
        });
    }
    Ok(())
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
    for &(device, expected_major, expected_minor) in MINIMAL_DEV_IDENTITIES {
        bind_minimal_device(old_path, device, expected_major, expected_minor)?;
    }
    Ok(())
}

fn bind_minimal_device(
    old_path: &str,
    device: &str,
    expected_major: u64,
    expected_minor: u64,
) -> Result<(), ContainerError> {
    let source = Path::new("/").join(old_path).join("dev").join(device);
    let target = Path::new("dev").join(device);
    let source_name =
        std::ffi::CString::new(source.as_os_str().as_bytes()).expect("constructed device path has no NUL");
    let source_descriptor = openat_anchored(
        AT_FDCWD,
        &source_name,
        nix::libc::O_PATH | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC,
        0,
    )
    .map_err(|source_error| ContainerError::OpenAnchoredMountSource {
        path: source.clone(),
        source: source_error,
    })?;
    validate_minimal_device_source(source_descriptor.as_raw_fd(), &source, expected_major, expected_minor)?;

    ensure_empty_file(&target)?;
    // Clone from the identity-validated descriptor, not the pathname, so a
    // concurrent host replacement cannot change the device that is attached.
    // SAFETY: source_descriptor remains live, AT_EMPTY_PATH admits the empty
    // path, and success returns a fresh detached mount descriptor.
    let device_mount = unsafe {
        open_tree(
            source_descriptor.as_raw_fd(),
            Path::new(""),
            OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH as u32,
        )
    }
    .map_err(Errno::from_i32)
    .with_context(|_| MountSnafu { target: source.clone() })?;
    // SAFETY: successful open_tree returned a fresh owned descriptor.
    let device_mount = unsafe { OwnedFd::from_raw_fd(device_mount) };
    set_mount_access_fd(device_mount.as_raw_fd(), true, false).with_context(|_| MountSnafu { target: source })?;
    // SAFETY: device_mount is a live detached mount descriptor and target is
    // a controlled placeholder in the fresh minimal-dev tmpfs.
    unsafe {
        move_mount(
            device_mount.as_raw_fd(),
            Path::new(""),
            AT_FDCWD,
            &target,
            MOVE_MOUNT_F_EMPTY_PATH,
        )
    }
    .map_err(Errno::from_i32)
    .with_context(|_| MountSnafu { target })?;
    Ok(())
}

fn set_mount_access(target: &Path, read_only: bool, recursive: bool) -> Result<(), ContainerError> {
    // SAFETY: target remains live for the call and successful open_tree
    // returns a fresh descriptor.
    let fd = unsafe { open_tree(AT_FDCWD, target, OPEN_TREE_CLOEXEC) }
        .map_err(Errno::from_i32)
        .with_context(|_| MountSnafu {
            target: target.to_owned(),
        })?;
    // SAFETY: successful open_tree returned a fresh owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    set_mount_access_fd(fd.as_raw_fd(), read_only, recursive).with_context(|_| MountSnafu {
        target: target.to_owned(),
    })
}

fn set_mount_access_fd(fd: RawFd, read_only: bool, recursive: bool) -> Result<(), Errno> {
    let attr = mount_attr_t {
        attr_set: if read_only { MOUNT_ATTR_RDONLY as u64 } else { 0 },
        attr_clr: if read_only { 0 } else { MOUNT_ATTR_RDONLY as u64 },
        program: 0,
        userns_fd: 0,
    };
    let flags = AT_EMPTY_PATH as usize | if recursive { AT_RECURSIVE as usize } else { 0 };
    // SAFETY: fd is live, empty path is admitted by AT_EMPTY_PATH, and attr is
    // initialized and borrowed only for the syscall.
    unsafe {
        syscall5(
            SYS_MOUNT_SETATTR,
            fd as usize,
            c"".as_ptr() as usize,
            flags,
            &attr as *const mount_attr_t as usize,
            size_of::<mount_attr_t>(),
        )
    }
    .map_err(Errno::from_i32)
    .map(|_| ())
}

fn setup_networking(root: &Path) -> Result<(), ContainerError> {
    ensure_directory(root.join("etc"))?;
    fs::copy("/etc/resolv.conf", root.join("etc/resolv.conf")).context(FsErrSnafu)?;
    Ok(())
}

/// Prepare resolver configuration without consulting the mutable root label.
/// Bounded, stable resolver bytes are copied into a sealed memfd and exposed as
/// a read-only detached file mount. Its target is pinned later together with
/// every other mount target, before the cloned root is modified.
fn prepare_anchored_resolver_mount() -> Result<PreparedAnchoredMount, ContainerError> {
    let resolver = read_host_resolver_bounded()?;
    Ok(PreparedAnchoredMount {
        source_mount: detached_resolver_mount(&resolver)?,
        target: PathBuf::from("etc/resolv.conf"),
        target_kind: AnchoredMountTargetKind::RegularFile,
    })
}

const MAX_RESOLVER_BYTES: usize = 64 * 1024;
const RESOLVER_MODE: nix::libc::mode_t = 0o644;

#[cfg(test)]
fn open_anchored_resolver_target(anchor: RawFd) -> Result<OwnedFd, ContainerError> {
    let path = Path::new("etc/resolv.conf");
    let target = openat2_anchored(
        anchor,
        path,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_XDEV
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_SYMLINKS,
    )
    .map_err(|source| ContainerError::OpenAnchoredMountTarget {
        path: path.to_owned(),
        source,
    })?;
    validate_resolver_target(target.as_raw_fd(), path)?;
    Ok(target)
}

#[cfg(test)]
fn validate_resolver_target(fd: RawFd, path: &Path) -> Result<(), ContainerError> {
    let stat = descriptor_stat(fd).map_err(|source| ContainerError::OpenAnchoredMountTarget {
        path: path.to_owned(),
        source,
    })?;
    let mode = stat.st_mode & nix::libc::S_IFMT;
    if mode != nix::libc::S_IFREG {
        return Err(ContainerError::UnsafeResolverTarget {
            path: path.to_owned(),
            mode,
            links: stat.st_nlink as u64,
        });
    }
    Ok(())
}

const MAX_RESOLVER_STABILITY_ATTEMPTS: usize = 3;

fn read_host_resolver_bounded() -> Result<Vec<u8>, ContainerError> {
    for _ in 0..MAX_RESOLVER_STABILITY_ATTEMPTS {
        match read_host_resolver_bounded_once() {
            Err(ContainerError::ResolverSourceChanged) => {}
            result => return result,
        }
    }
    Err(ContainerError::ResolverSourceChanged)
}

fn read_host_resolver_bounded_once() -> Result<Vec<u8>, ContainerError> {
    // O_PATH pins the object without opening FIFO/device data. Only after its
    // structure is proven to be a regular file do we reopen this exact
    // descriptor through procfs with O_NONBLOCK for bounded reading.
    let pinned = openat_anchored(
        AT_FDCWD,
        c"/etc/resolv.conf",
        nix::libc::O_PATH | nix::libc::O_CLOEXEC,
        0,
    )
    .context(ConfigureAnchoredNetworkingSnafu {
        operation: "pin host resolver source",
    })?;
    let pinned_stat = validate_resolver_source(pinned.as_raw_fd())?;
    let mut reader = reopen_pinned_readonly(pinned.as_raw_fd()).context(ConfigureAnchoredNetworkingSnafu {
        operation: "open pinned host resolver source for bounded reading",
    })?;
    let reader_stat = validate_resolver_source(reader.as_raw_fd())?;
    if reader_stat.st_dev != pinned_stat.st_dev || reader_stat.st_ino != pinned_stat.st_ino {
        return Err(ContainerError::ResolverSourceChanged);
    }

    let mut resolver = Vec::with_capacity((pinned_stat.st_size as usize).min(MAX_RESOLVER_BYTES));
    (&mut reader)
        .take((MAX_RESOLVER_BYTES + 1) as u64)
        .read_to_end(&mut resolver)
        .context(ConfigureAnchoredNetworkingSnafu {
            operation: "read bounded host resolver source",
        })?;
    if resolver.len() > MAX_RESOLVER_BYTES {
        return Err(ContainerError::ResolverSourceTooLarge {
            actual: resolver.len() as u64,
            limit: MAX_RESOLVER_BYTES as u64,
        });
    }
    let final_reader = validate_resolver_source(reader.as_raw_fd())?;
    let final_pinned = validate_resolver_source(pinned.as_raw_fd())?;
    if !resolver_stat_stable(&pinned_stat, &reader_stat)
        || !resolver_stat_stable(&reader_stat, &final_reader)
        || !resolver_stat_stable(&final_reader, &final_pinned)
    {
        return Err(ContainerError::ResolverSourceChanged);
    }
    Ok(resolver)
}

fn resolver_stat_stable(first: &nix::libc::stat, second: &nix::libc::stat) -> bool {
    first.st_dev == second.st_dev
        && first.st_ino == second.st_ino
        && first.st_size == second.st_size
        && first.st_mtime == second.st_mtime
        && first.st_mtime_nsec == second.st_mtime_nsec
        && first.st_ctime == second.st_ctime
        && first.st_ctime_nsec == second.st_ctime_nsec
}

fn validate_resolver_source(fd: RawFd) -> Result<nix::libc::stat, ContainerError> {
    let stat = descriptor_stat(fd).context(ConfigureAnchoredNetworkingSnafu {
        operation: "inspect host resolver source",
    })?;
    let mode = stat.st_mode & nix::libc::S_IFMT;
    let size = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
    if mode != nix::libc::S_IFREG {
        return Err(ContainerError::UnsafeResolverSource {
            mode,
            links: stat.st_nlink as u64,
        });
    }
    if size > MAX_RESOLVER_BYTES as u64 {
        return Err(ContainerError::ResolverSourceTooLarge {
            actual: size,
            limit: MAX_RESOLVER_BYTES as u64,
        });
    }
    Ok(stat)
}

fn reopen_pinned_readonly(fd: RawFd) -> io::Result<fs::File> {
    let diagnostic_path = PathBuf::from(format!("/proc/self/fd/{fd}"));
    let path = std::ffi::CString::new(diagnostic_path.as_os_str().as_bytes())
        .expect("decimal descriptor path cannot contain NUL");
    let descriptor = openat_anchored(
        AT_FDCWD,
        &path,
        nix::libc::O_RDONLY | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC,
        0,
    )?;
    Ok(fs::File::from_parts(descriptor.into(), diagnostic_path))
}

fn detached_resolver_mount(resolver: &[u8]) -> Result<OwnedFd, ContainerError> {
    let file = sealed_resolver_file(resolver)?;

    // SAFETY: file is live, AT_EMPTY_PATH admits the empty path, and success
    // returns a fresh detached file-mount descriptor.
    let mount = unsafe {
        open_tree(
            file.as_raw_fd(),
            Path::new(""),
            OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH as u32,
        )
    }
    .map_err(Errno::from_i32)
    .map_err(errno_to_io)
    .context(ConfigureAnchoredNetworkingSnafu {
        operation: "clone sealed resolver mount",
    })?;
    // SAFETY: successful open_tree returned a fresh owned descriptor.
    let mount = unsafe { OwnedFd::from_raw_fd(mount) };
    set_mount_access_fd(mount.as_raw_fd(), true, false)
        .map_err(errno_to_io)
        .context(ConfigureAnchoredNetworkingSnafu {
            operation: "make sealed resolver mount read-only",
        })?;
    Ok(mount)
}

fn sealed_resolver_file(resolver: &[u8]) -> Result<fs::File, ContainerError> {
    if resolver.len() > MAX_RESOLVER_BYTES {
        return Err(ContainerError::ResolverSourceTooLarge {
            actual: resolver.len() as u64,
            limit: MAX_RESOLVER_BYTES as u64,
        });
    }
    // SAFETY: the name is static and NUL terminated; success returns a fresh
    // descriptor transferred exactly once to OwnedFd.
    let descriptor = unsafe {
        nix::libc::memfd_create(
            c"container-resolv.conf".as_ptr(),
            nix::libc::MFD_ALLOW_SEALING | nix::libc::MFD_CLOEXEC,
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error()).context(ConfigureAnchoredNetworkingSnafu {
            operation: "create sealed resolver file",
        });
    }
    // SAFETY: memfd_create returned a fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let mut file = fs::File::from_parts(descriptor.into(), "sealed resolv.conf");
    file.write_all(resolver).context(ConfigureAnchoredNetworkingSnafu {
        operation: "write sealed resolver file",
    })?;
    // SAFETY: file is a live memfd and mode contains only permission bits.
    if unsafe { nix::libc::fchmod(file.as_raw_fd(), RESOLVER_MODE) } == -1 {
        return Err(io::Error::last_os_error()).context(ConfigureAnchoredNetworkingSnafu {
            operation: "set deterministic resolver mode",
        });
    }
    file.sync_all().context(ConfigureAnchoredNetworkingSnafu {
        operation: "sync sealed resolver file",
    })?;
    let required_seals =
        nix::libc::F_SEAL_WRITE | nix::libc::F_SEAL_GROW | nix::libc::F_SEAL_SHRINK | nix::libc::F_SEAL_SEAL;
    // SAFETY: file is a live sealable memfd and the variadic argument is the
    // documented seal bitmask.
    if unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_ADD_SEALS, required_seals) } == -1 {
        return Err(io::Error::last_os_error()).context(ConfigureAnchoredNetworkingSnafu {
            operation: "seal resolver file",
        });
    }
    // SAFETY: file remains live and F_GET_SEALS has no variadic argument.
    let actual_seals = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GET_SEALS) };
    if actual_seals == -1 {
        return Err(io::Error::last_os_error()).context(ConfigureAnchoredNetworkingSnafu {
            operation: "verify resolver file seals",
        });
    }
    let stat = descriptor_stat(file.as_raw_fd()).context(ConfigureAnchoredNetworkingSnafu {
        operation: "verify sealed resolver file metadata",
    })?;
    let kind = stat.st_mode & nix::libc::S_IFMT;
    let mode = stat.st_mode & 0o777;
    let size = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
    if kind != nix::libc::S_IFREG
        || mode != RESOLVER_MODE
        || size != resolver.len() as u64
        || actual_seals & required_seals != required_seals
    {
        return Err(ContainerError::InvalidSealedResolver {
            kind,
            mode,
            size,
            expected_size: resolver.len() as u64,
            seals: actual_seals,
        });
    }
    Ok(file)
}

fn errno_to_io(error: Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

fn openat_anchored(
    parent: RawFd,
    name: &std::ffi::CStr,
    flags: nix::libc::c_int,
    mode: nix::libc::mode_t,
) -> io::Result<OwnedFd> {
    // SAFETY: parent and name remain live for the call and a successful openat
    // returns a fresh descriptor transferred exactly once to OwnedFd.
    let descriptor = unsafe { nix::libc::openat(parent, name.as_ptr(), flags, mode) };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
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

static SIGNAL_OVERRIDE_LOCK: Mutex<()> = Mutex::new(());

// libtest runs unit tests on a thread pool, while the legacy compatibility
// path must exercise a fork-like clone child that executes Rust setup code.
// Production builds do not get this escape hatch: they authenticate an exact
// single-task supervisor immediately before clone. Serializing the live unit
// fixtures prevents several test-only activations from cloning across one
// another; the harness-free integration test exercises the production guard.
#[cfg(test)]
static LEGACY_TEST_ACTIVATION_LOCK: Mutex<()> = Mutex::new(());

struct BlockedSignalMask {
    previous: nix::libc::sigset_t,
    active: bool,
    restore_on_drop: bool,
}

impl BlockedSignalMask {
    fn block_all() -> io::Result<Self> {
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
    fn retain_blocked_on_drop(&mut self) {
        self.restore_on_drop = false;
    }

    fn restore(&mut self) -> io::Result<()> {
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

struct SignalOverride {
    signal: Signal,
    previous: SigAction,
    restored: bool,
    _serial: MutexGuard<'static, ()>,
}

impl SignalOverride {
    fn install(signal: Signal) -> Result<Self, nix::Error> {
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

    fn restore(mut self) -> Result<(), nix::Error> {
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
enum ChildLifecycle {
    Legacy { pid: Pid },
    Pidfd { pid: Pid, pidfd: OwnedFd },
}

impl ChildLifecycle {
    fn pid(&self) -> Pid {
        match self {
            Self::Legacy { pid } | Self::Pidfd { pid, .. } => *pid,
        }
    }

    fn wait(&self) -> Result<WaitStatus, Errno> {
        match self {
            Self::Legacy { pid } => wait_for_child(*pid),
            Self::Pidfd { pidfd, .. } => wait_for_pidfd(pidfd.as_fd(), WaitPidFlag::WEXITED),
        }
    }

    fn cleanup(self) -> Result<(), Error> {
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

    fn cleanup_after_failure(self, primary: Error) -> Error {
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
    fn new(pidfd: OwnedFd) -> Self {
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
struct PidfdCleanupFailure {
    cleanup: io::Error,
    pidfd: OwnedFd,
}

fn send_pidfd_signal(pidfd: BorrowedFd<'_>, signal: Signal) -> Result<(), Errno> {
    let mut interrupted = 0;
    loop {
        // SAFETY: pidfd is a live borrowed descriptor, a null siginfo requests
        // ordinary process-directed signal semantics, and flags must be zero.
        let result = unsafe {
            syscall(
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

fn wait_for_pidfd(pidfd: BorrowedFd<'_>, flags: WaitPidFlag) -> Result<WaitStatus, Errno> {
    let mut interrupted = 0;
    loop {
        match waitid(WaitId::PIDFd(pidfd), flags) {
            Err(Errno::EINTR) if interrupted < MAX_CONTROL_EINTR_RETRIES => interrupted += 1,
            result => return result,
        }
    }
}

fn cleanup_pidfd_child(pidfd: OwnedFd) -> Result<(), PidfdCleanupFailure> {
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

fn wait_for_pidfd_reap(pidfd: BorrowedFd<'_>, timeout: Duration) -> io::Result<WaitStatus> {
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
    let mut output = String::new();
    let mut current: Option<&dyn std::error::Error> = Some(&error);
    for depth in 0..MAX_ERROR_SOURCE_DEPTH {
        let Some(source) = current else {
            break;
        };
        if depth != 0 && !push_bounded_error_text(&mut output, ": ") {
            break;
        }
        let rendered = {
            let mut writer = BoundedErrorWriter { output: &mut output };
            std::fmt::write(&mut writer, format_args!("{source}"))
        };
        if rendered.is_err() {
            break;
        }
        current = source.source();
    }
    if current.is_some() {
        let _ = push_bounded_error_text(&mut output, " [truncated]");
    }
    output
}

struct BoundedErrorWriter<'a> {
    output: &'a mut String,
}

impl std::fmt::Write for BoundedErrorWriter<'_> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if push_bounded_error_text(self.output, text) {
            Ok(())
        } else {
            Err(std::fmt::Error)
        }
    }
}

fn push_bounded_error_text(output: &mut String, text: &str) -> bool {
    let remaining = MAX_CHILD_ERROR_BYTES.saturating_sub(output.len());
    if text.len() <= remaining {
        output.push_str(text);
        return true;
    }
    let mut end = remaining.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    output.push_str(&text[..end]);
    false
}

/// Per-run clone stack with a protected low-address guard page. Linux clone
/// starts at the high end of the supplied slice and grows downward, so stack
/// exhaustion faults instead of corrupting an adjacent allocator object.
struct CloneStack {
    mapping: NonNull<nix::libc::c_void>,
    mapping_len: usize,
    page_size: usize,
}

impl CloneStack {
    fn new() -> io::Result<Self> {
        // SAFETY: sysconf has no pointer arguments.
        let page_size = unsafe { nix::libc::sysconf(nix::libc::_SC_PAGESIZE) };
        let page_size = usize::try_from(page_size)
            .ok()
            .filter(|size| size.is_power_of_two())
            .ok_or_else(|| io::Error::other("kernel returned an invalid page size"))?;
        let mapping_len = CLONE_STACK_BYTES
            .checked_add(page_size)
            .ok_or_else(|| io::Error::other("clone stack mapping length overflow"))?;
        // SAFETY: anonymous mapping uses no input pointer or file descriptor.
        let mapping = unsafe {
            nix::libc::mmap(
                std::ptr::null_mut(),
                mapping_len,
                nix::libc::PROT_NONE,
                nix::libc::MAP_PRIVATE | nix::libc::MAP_ANONYMOUS | nix::libc::MAP_STACK,
                -1,
                0,
            )
        };
        if mapping == nix::libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let Some(mapping) = NonNull::new(mapping) else {
            // A null mapping is legal only on hosts that permit mapping page
            // zero, but NonNull is part of this type's safety invariant.
            // SAFETY: mmap returned this exact live mapping and length.
            unsafe {
                nix::libc::munmap(mapping, mapping_len);
            }
            return Err(io::Error::other("clone stack mapping unexpectedly starts at null"));
        };
        // SAFETY: the mapping is page aligned and the selected range excludes
        // exactly the first guard page.
        let usable = unsafe { mapping.as_ptr().cast::<u8>().add(page_size).cast() };
        if unsafe { nix::libc::mprotect(usable, CLONE_STACK_BYTES, nix::libc::PROT_READ | nix::libc::PROT_WRITE) } == -1
        {
            let source = io::Error::last_os_error();
            // SAFETY: mapping and length came from the successful mmap above.
            unsafe {
                nix::libc::munmap(mapping.as_ptr(), mapping_len);
            }
            return Err(source);
        }
        Ok(Self {
            mapping,
            mapping_len,
            page_size,
        })
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: the usable range was made read-write, is wholly owned by this
        // value, and remains mapped until Drop after the child is reaped.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.mapping.as_ptr().cast::<u8>().add(self.page_size),
                CLONE_STACK_BYTES,
            )
        }
    }

    #[cfg(test)]
    fn guard_address(&self) -> usize {
        self.mapping.as_ptr() as usize
    }

    #[cfg(test)]
    fn usable_address(&self) -> usize {
        self.guard_address() + self.page_size
    }
}

impl Drop for CloneStack {
    fn drop(&mut self) {
        // SAFETY: this value owns the complete live mapping.
        unsafe {
            nix::libc::munmap(self.mapping.as_ptr(), self.mapping_len);
        }
    }
}

/// Parent-owned bidirectional synchronization socket. `SOCK_SEQPACKET` keeps
/// the release byte and one bounded diagnostic as distinct atomic messages;
/// `MSG_NOSIGNAL` ensures child death can never terminate the supervisor.
struct SyncSocket {
    supervisor: Option<RawFd>,
    child: Option<RawFd>,
}

impl SyncSocket {
    fn new() -> Result<Self, Error> {
        let mut endpoints = [-1_i32; 2];
        // SAFETY: endpoints is a writable two-element output array and all
        // socket domain/type/protocol values are valid Linux constants.
        if unsafe {
            nix::libc::socketpair(
                nix::libc::AF_UNIX,
                nix::libc::SOCK_SEQPACKET | nix::libc::SOCK_CLOEXEC,
                0,
                endpoints.as_mut_ptr(),
            )
        } == -1
        {
            return Err(Error::Nix { source: Errno::last() });
        }
        let supervisor = match rehome_sync_fd(endpoints[0]) {
            Ok(fd) => fd,
            Err(source) => {
                let _ = close(endpoints[1]);
                return Err(Error::Nix { source });
            }
        };
        let child = match rehome_sync_fd(endpoints[1]) {
            Ok(fd) => fd,
            Err(source) => {
                let _ = close(supervisor);
                return Err(Error::Nix { source });
            }
        };
        Ok(Self {
            supervisor: Some(supervisor),
            child: Some(child),
        })
    }

    fn raw(&self) -> (RawFd, RawFd) {
        (self.supervisor_fd(), self.child_fd())
    }

    fn supervisor_fd(&self) -> RawFd {
        self.supervisor.unwrap_or(-1)
    }

    fn child_fd(&self) -> RawFd {
        self.child.unwrap_or(-1)
    }

    fn close_child_endpoint(&mut self) -> Result<(), nix::Error> {
        let Some(fd) = self.child.take() else {
            return Err(Errno::EBADF);
        };
        close_sync_endpoint(fd)
    }
}

fn close_sync_endpoint(fd: RawFd) -> Result<(), Errno> {
    match close(fd) {
        Ok(()) | Err(Errno::EINTR) => Ok(()),
        Err(source) => Err(source),
    }
}

fn send_packet_no_signal(fd: RawFd, bytes: &[u8]) -> Result<usize, Errno> {
    let mut interrupted = 0;
    loop {
        // SAFETY: bytes remains readable for its declared length and send does
        // not retain the pointer. MSG_NOSIGNAL converts peer closure to EPIPE;
        // MSG_DONTWAIT prevents a compromised child-side producer from ever
        // turning the supervisor's control path into an unbounded wait.
        let sent = unsafe {
            nix::libc::send(
                fd,
                bytes.as_ptr().cast(),
                bytes.len(),
                nix::libc::MSG_NOSIGNAL | nix::libc::MSG_DONTWAIT,
            )
        };
        if sent >= 0 {
            return usize::try_from(sent).map_err(|_| Errno::EOVERFLOW);
        }
        let source = Errno::last();
        if source == Errno::EINTR && interrupted < MAX_CONTROL_EINTR_RETRIES {
            interrupted += 1;
            continue;
        }
        return Err(source);
    }
}

fn rehome_sync_fd(fd: RawFd) -> Result<RawFd, Errno> {
    if fd >= 3 {
        return Ok(fd);
    }
    // SAFETY: the source descriptor is live and success returns a new
    // close-on-exec descriptor numbered at least three.
    let duplicated = unsafe { nix::libc::fcntl(fd, nix::libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated == -1 {
        let source = Errno::last();
        let _ = close(fd);
        return Err(source);
    }
    if let Err(source) = close(fd) {
        let _ = close(duplicated);
        return Err(source);
    }
    Ok(duplicated)
}

fn set_fd_nonblocking(fd: RawFd) -> Result<(), Errno> {
    // SAFETY: fd is live for both fcntl calls.
    let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
    if flags == -1 {
        return Err(Errno::last());
    }
    // SAFETY: F_SETFL updates only status flags on the same live descriptor.
    if unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) } == -1 {
        return Err(Errno::last());
    }
    Ok(())
}

impl Drop for SyncSocket {
    fn drop(&mut self) {
        if let Some(fd) = self.supervisor.take() {
            let _ = close(fd);
        }
        if let Some(fd) = self.child.take() {
            let _ = close(fd);
        }
    }
}

struct Bind {
    source: BindSource,
    target: PathBuf,
    read_only: bool,
}

enum BindSource {
    /// Legacy pathname bind. Deliberately rejected by anchored execution.
    Path(PathBuf),
    /// Normalized path resolved beneath the authenticated root descriptor.
    RootRelative(PathBuf),
    /// Descriptor selected by the supervising runtime before activation.
    Pinned { descriptor: OwnedFd, label: PathBuf },
}

fn container_error_to_invalid_input(error: ContainerError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format_error(error))
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
    #[snafu(display("open anchored mount source {}", path.display()))]
    OpenAnchoredMountSource { path: PathBuf, source: io::Error },
    #[snafu(display(
        "anchored container rejects pathname bind source {}; use a descriptor-pinned or root-relative bind",
        path.display()
    ))]
    UnpinnedAnchoredMountSource { path: PathBuf },
    #[snafu(display("anchored bind source {} cannot be used by a pathname container", path.display()))]
    AnchoredBindOnPathContainer { path: PathBuf },
    #[snafu(display("unsupported anchored mount source {} with mode {mode:o}", path.display()))]
    UnsupportedAnchoredMountSource { path: PathBuf, mode: nix::libc::mode_t },
    #[snafu(display(
        "minimal device source {} has Linux device identity ({actual_major},{actual_minor}); expected ({expected_major},{expected_minor})",
        path.display()
    ))]
    UnexpectedMinimalDeviceIdentity {
        path: PathBuf,
        expected_major: u64,
        expected_minor: u64,
        actual_major: u64,
        actual_minor: u64,
    },
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
    use std::fmt;
    use std::io::{self, Read as _};
    use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
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
        AnchoredMountTargetKind, Bind, BindSource, BlockedSignalMask, CLONE_STACK_BYTES, CapabilityData,
        ChildLifecycle, ChildPidfdQuarantine, CloneStack, Container, ContainerError, DevPolicy,
        Error as ContainerRunError, LoopbackPolicy, MAX_CHILD_ERROR_BYTES, MAX_LINUX_CAPABILITY_NUMBER,
        MINIMAL_DEV_IDENTITIES, MINIMAL_DEV_NODES, Message, PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, PR_CAPBSET_READ,
        PreparedAnchoredMount, ProcPolicy, PseudoFilesystemPolicy, PseudoMountDecision, RootFilesystemPolicy,
        RootMountDecision, SignalOverride, SyncSocket, SysPolicy, TmpPolicy, capability_is_set, checked_prctl_value,
        cleanup_pidfd_child, close_sync_endpoint, contain_raw_clone_child_panic, descriptor_stat, duplicate_cloexec,
        format_error, namespace_flags, normalized_anchored_mount_target, open_anchored_mount_target,
        open_anchored_resolver_target, pin_anchored_bind_sources, prctl, prepare_bind_target, pseudo_mount_decisions,
        read_capabilities, read_child_error, reopen_pinned_readonly, require_atomic_cgroup_bind_policy,
        require_atomic_cgroup_policy, resolver_stat_stable, root_mount_decisions, sealed_resolver_file,
        send_packet_no_signal, send_pidfd_signal, set_mount_access, standard_descriptor_is_unsafe,
        supported_capability_numbers, validate_anchored_mount_topology, validate_minimal_device_source,
        validate_payload_credentials, validate_resolver_target, wait_for_pidfd, wait_for_pidfd_reap,
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

    #[test]
    fn anchored_constructor_owns_a_cloexec_opath_directory_duplicate() {
        let root = tempfile::tempdir().unwrap();
        let anchor = open_path_directory(root.path());
        let caller_descriptor = anchor.as_raw_fd();
        let container = Container::new_anchored("diagnostic-root", &anchor).unwrap();
        let retained = container.root_anchor.as_ref().unwrap().as_raw_fd();

        assert_ne!(retained, caller_descriptor);
        assert_eq!(container.root, Path::new("diagnostic-root"));
        let status = fcntl(retained, FcntlArg::F_GETFL).unwrap();
        assert_eq!(status & nix::libc::O_PATH, nix::libc::O_PATH);
        let descriptor = FdFlag::from_bits_truncate(fcntl(retained, FcntlArg::F_GETFD).unwrap());
        assert!(descriptor.contains(FdFlag::FD_CLOEXEC));

        drop(anchor);
        assert!(fcntl(retained, FcntlArg::F_GETFD).is_ok());
    }

    #[test]
    fn anchored_constructor_rejects_every_non_opath_or_non_directory_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let ordinary_directory = std::fs::File::open(root.path()).unwrap();
        let error = Container::new_anchored(root.path(), &ordinary_directory).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            "anchored container root descriptor must be opened with O_PATH"
        );

        let regular_path = root.path().join("regular");
        fs::write(&regular_path, b"not a directory").unwrap();
        let ordinary_file = open_path_file(&regular_path);
        let error = Container::new_anchored(root.path(), &ordinary_file).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            "anchored container root descriptor must reference a directory"
        );

        struct InvalidDescriptor;
        impl std::os::fd::AsRawFd for InvalidDescriptor {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                -1
            }
        }
        let error = Container::new_anchored(root.path(), &InvalidDescriptor).err().unwrap();
        assert_eq!(error.raw_os_error(), Some(nix::libc::EBADF));
    }

    #[test]
    fn anchored_bind_source_is_pinned_before_clone_and_survives_path_substitution() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let pinned_name = temporary.path().join("pinned-source");
        fs::write(&source, b"authenticated source").unwrap();
        let expected = fs::metadata(&source).unwrap();

        let root_anchor = open_path_directory(temporary.path());
        let pinned = pin_anchored_bind_sources(
            root_anchor.as_raw_fd(),
            &[Bind {
                source: BindSource::RootRelative(PathBuf::from("source")),
                target: PathBuf::from("/payload/source"),
                read_only: true,
            }],
        )
        .unwrap();

        // This is the adversarial checkpoint: after the supervising process
        // has pinned the source but before the child clones its mount, replace
        // the complete pathname with a different object.
        fs::rename(&source, &pinned_name).unwrap();
        fs::write(&source, b"replacement source").unwrap();

        let retained = descriptor_stat(pinned[0].source.as_raw_fd()).unwrap();
        assert_eq!(retained.st_dev as u64, expected.dev());
        assert_eq!(retained.st_ino as u64, expected.ino());
        let mut retained_reader = reopen_pinned_readonly(pinned[0].source.as_raw_fd()).unwrap();
        let mut retained_bytes = Vec::new();
        retained_reader.read_to_end(&mut retained_bytes).unwrap();
        assert_eq!(retained_bytes, b"authenticated source");
        assert_eq!(fs::read(&source).unwrap(), b"replacement source");
        assert_eq!(pinned[0].target, Path::new("payload/source"));
    }

    #[test]
    fn anchored_mount_targets_must_preexist_and_reject_symlink_traversal() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("target"), b"outside witness").unwrap();
        let anchor = open_path_directory(&root);

        let missing = open_anchored_mount_target(
            anchor.as_raw_fd(),
            Path::new("missing"),
            AnchoredMountTargetKind::RegularFile,
        )
        .unwrap_err();
        assert!(matches!(
            missing,
            ContainerError::OpenAnchoredMountTarget { source, .. }
                if source.raw_os_error() == Some(nix::libc::ENOENT)
        ));
        assert!(
            !root.join("missing").exists(),
            "target resolution must never create mountpoints"
        );

        std::os::unix::fs::symlink(&outside, root.join("redirect")).unwrap();
        let redirected = open_anchored_mount_target(
            anchor.as_raw_fd(),
            Path::new("redirect/target"),
            AnchoredMountTargetKind::RegularFile,
        )
        .unwrap_err();
        assert!(matches!(redirected, ContainerError::OpenAnchoredMountTarget { .. }));
        assert_eq!(fs::read(outside.join("target")).unwrap(), b"outside witness");

        let host_root = open_path_directory(Path::new("/"));
        let nested_mount = open_anchored_mount_target(
            host_root.as_raw_fd(),
            Path::new("proc/self"),
            AnchoredMountTargetKind::Directory,
        )
        .unwrap_err();
        assert!(matches!(
            nested_mount,
            ContainerError::OpenAnchoredMountTarget { source, .. }
                if source.raw_os_error() == Some(nix::libc::EXDEV)
        ));
    }

    #[test]
    fn anchored_mount_target_normalization_rejects_escape_and_root_aliases() {
        for invalid in [
            "",
            "/",
            ".",
            "relative",
            "../escape",
            "/safe/../escape",
            "/safe/./target",
        ] {
            assert!(
                normalized_anchored_mount_target(Path::new(invalid)).is_err(),
                "accepted {invalid:?}"
            );
        }
        assert_eq!(
            normalized_anchored_mount_target(Path::new("/safe/target")).unwrap(),
            Path::new("safe/target")
        );

        let mut maximal_components = std::iter::repeat_n("a".repeat(255), 15).collect::<Vec<_>>();
        maximal_components.push("b".repeat(254));
        let maximal = format!("/{}", maximal_components.join("/"));
        assert_eq!(maximal.len(), 4095);
        assert!(normalized_anchored_mount_target(Path::new(&maximal)).is_ok());
        assert!(normalized_anchored_mount_target(Path::new(&format!("{maximal}x"))).is_err());
    }

    #[test]
    fn anchored_mount_topology_rejects_duplicate_and_nested_targets() {
        let source = tempfile::tempdir().unwrap();
        let mounts = |first: &str, second: &str| {
            vec![
                PreparedAnchoredMount {
                    source_mount: open_path_directory(source.path()),
                    target: PathBuf::from(first),
                    target_kind: AnchoredMountTargetKind::Directory,
                },
                PreparedAnchoredMount {
                    source_mount: open_path_directory(source.path()),
                    target: PathBuf::from(second),
                    target_kind: AnchoredMountTargetKind::Directory,
                },
            ]
        };

        for (first, second) in [("work", "work"), ("work", "work/cache"), ("work/cache", "work")] {
            assert!(matches!(
                validate_anchored_mount_topology(&mounts(first, second)),
                Err(ContainerError::OverlappingAnchoredMountTargets { .. })
            ));
        }
        validate_anchored_mount_topology(&mounts("work", "cache")).unwrap();
    }

    #[test]
    fn anchored_execution_rejects_pathname_and_special_file_bind_sources_before_clone() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let anchor = open_path_directory(root.path());
        let path_error = pin_anchored_bind_sources(
            anchor.as_raw_fd(),
            &[Bind {
                source: BindSource::Path(source.path().to_owned()),
                target: PathBuf::from("work"),
                read_only: false,
            }],
        )
        .err()
        .unwrap();
        assert!(matches!(path_error, ContainerError::UnpinnedAnchoredMountSource { .. }));

        let fifo_path = source.path().join("fifo");
        mkfifo(&fifo_path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let fifo = open_path_file(&fifo_path);
        let error = Container::new_anchored(root.path(), &anchor)
            .unwrap()
            .bind_rw_pinned(&fifo, &fifo_path, "/work")
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("unsupported anchored mount source"));
    }

    #[test]
    fn anchored_bind_apis_require_absolute_source_and_guest_paths() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let anchor = open_path_directory(root.path());
        let pinned = open_path_directory(source.path());

        for result in [
            Container::new_anchored(root.path(), &anchor)
                .unwrap()
                .bind_rw_from_root("install", "/install"),
            Container::new_anchored(root.path(), &anchor)
                .unwrap()
                .bind_rw_from_root("/install", "install"),
            Container::new_anchored(root.path(), &anchor)
                .unwrap()
                .bind_rw_pinned(&pinned, source.path(), "work"),
        ] {
            let error = result.err().expect("relative anchored path must fail");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("invalid anchored mount target"));
        }
    }

    #[test]
    fn sealed_resolver_file_has_exact_metadata_seals_and_cleanup() {
        let file = sealed_resolver_file(b"nameserver 192.0.2.1\n").unwrap();
        let fd = file.as_raw_fd();
        let stat = descriptor_stat(fd).unwrap();
        assert_eq!(stat.st_mode & nix::libc::S_IFMT, nix::libc::S_IFREG);
        assert_eq!(stat.st_mode & 0o777, 0o644);
        assert_eq!(stat.st_size, b"nameserver 192.0.2.1\n".len() as i64);
        // SAFETY: fd is a live memfd and F_GET_SEALS takes no third argument.
        let seals = unsafe { nix::libc::fcntl(fd, nix::libc::F_GET_SEALS) };
        let required =
            nix::libc::F_SEAL_WRITE | nix::libc::F_SEAL_GROW | nix::libc::F_SEAL_SHRINK | nix::libc::F_SEAL_SEAL;
        assert_eq!(seals & required, required);
        let mutation = b"mutation";
        // SAFETY: mutation is live for the write and fd denotes the sealed
        // memfd. The syscall must reject the write without reading elsewhere.
        assert_eq!(
            unsafe { nix::libc::write(fd, mutation.as_ptr().cast(), mutation.len()) },
            -1
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(nix::libc::EPERM));
        drop(file);
        assert_eq!(fcntl(fd, FcntlArg::F_GETFD), Err(Errno::EBADF));
    }

    #[test]
    fn resolver_stability_witness_detects_content_metadata_change() {
        let temporary = tempfile::NamedTempFile::new().unwrap();
        fs::write(temporary.path(), b"first").unwrap();
        let file = fs::File::open(temporary.path()).unwrap();
        let before = descriptor_stat(file.as_raw_fd()).unwrap();
        let same = descriptor_stat(file.as_raw_fd()).unwrap();
        assert!(resolver_stat_stable(&before, &same));
        fs::write(temporary.path(), b"different-size").unwrap();
        let after = descriptor_stat(file.as_raw_fd()).unwrap();
        assert!(!resolver_stat_stable(&before, &after));
    }

    #[test]
    fn error_transport_format_is_bounded_even_for_cyclic_and_huge_sources() {
        #[derive(Debug)]
        struct CyclicHugeError;
        impl fmt::Display for CyclicHugeError {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                for _ in 0..(MAX_CHILD_ERROR_BYTES * 4) {
                    formatter.write_str("x")?;
                }
                Ok(())
            }
        }
        impl std::error::Error for CyclicHugeError {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(self)
            }
        }

        let rendered = format_error(CyclicHugeError);
        assert_eq!(rendered.len(), MAX_CHILD_ERROR_BYTES);
        assert!(rendered.bytes().all(|byte| byte == b'x'));
    }

    #[test]
    fn raw_clone_child_panic_is_contained_and_reported() {
        let mut sync = SyncSocket::new().unwrap();
        let error_writer = sync.child.take().unwrap();
        let exit_code = contain_raw_clone_child_panic(error_writer, || -> i32 {
            panic!("panic must not unwind through the raw clone boundary")
        });
        assert_eq!(exit_code, 1);
        let message = read_child_error(sync.supervisor_fd()).unwrap();
        assert_eq!(
            message,
            "raw fork-like clone child panicked; payload setup was aborted before returning through the cloned parent stack"
        );
    }

    #[test]
    fn child_error_read_does_not_wait_for_a_leaked_descendant_socket() {
        let mut sync = SyncSocket::new().unwrap();
        let child = sync.child.take().unwrap();
        let leaked_child = duplicate_cloexec(child).unwrap();
        assert_eq!(send_packet_no_signal(child, b"bounded child error").unwrap(), 19);
        close_sync_endpoint(child).unwrap();

        let result = read_child_error(sync.supervisor_fd()).unwrap();
        assert_eq!(result, "bounded child error");
        drop(leaked_child);
    }

    #[test]
    fn anchored_resolver_target_uses_the_descriptor_not_the_replaced_label() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let pinned = temporary.path().join("pinned");
        fs::create_dir_all(root.join("etc")).unwrap();
        fs::write(root.join("etc/resolv.conf"), b"authenticated placeholder").unwrap();
        let anchor = open_path_directory(&root);
        fs::rename(&root, &pinned).unwrap();
        fs::create_dir_all(root.join("etc")).unwrap();
        fs::write(root.join("etc/resolv.conf"), b"replacement").unwrap();

        let target = open_anchored_resolver_target(anchor.as_raw_fd()).unwrap();
        let target_stat = descriptor_stat(target.as_raw_fd()).unwrap();
        let expected = fs::metadata(pinned.join("etc/resolv.conf")).unwrap();

        assert_eq!(target_stat.st_dev as u64, expected.dev());
        assert_eq!(target_stat.st_ino as u64, expected.ino());
        assert_eq!(
            fs::read(pinned.join("etc/resolv.conf")).unwrap(),
            b"authenticated placeholder"
        );
        assert_eq!(fs::read(root.join("etc/resolv.conf")).unwrap(), b"replacement");
    }

    #[test]
    fn anchored_resolver_rejects_fifo_and_device_targets_without_opening_data() {
        let fifo_root = tempfile::tempdir().unwrap();
        fs::create_dir(fifo_root.path().join("etc")).unwrap();
        mkfifo(&fifo_root.path().join("etc/resolv.conf"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let fifo_anchor = open_path_directory(fifo_root.path());
        assert!(matches!(
            open_anchored_resolver_target(fifo_anchor.as_raw_fd()),
            Err(ContainerError::UnsafeResolverTarget { mode, .. }) if mode == nix::libc::S_IFIFO
        ));

        let device = open_path_file(Path::new("/dev/null"));
        assert!(matches!(
            validate_resolver_target(device.as_raw_fd(), Path::new("etc/resolv.conf")),
            Err(ContainerError::UnsafeResolverTarget { mode, .. }) if mode == nix::libc::S_IFCHR
        ));

        let hardlink_root = tempfile::tempdir().unwrap();
        fs::create_dir(hardlink_root.path().join("etc")).unwrap();
        let target = hardlink_root.path().join("etc/resolv.conf");
        let alias = hardlink_root.path().join("resolver-alias");
        fs::write(&target, b"do not mutate").unwrap();
        fs::hard_link(&target, &alias).unwrap();
        let hardlink_anchor = open_path_directory(hardlink_root.path());
        let hardlink_descriptor = open_anchored_resolver_target(hardlink_anchor.as_raw_fd()).unwrap();
        let hardlink_stat = descriptor_stat(hardlink_descriptor.as_raw_fd()).unwrap();
        assert_eq!(hardlink_stat.st_nlink, 2);
        assert_eq!(fs::read(&target).unwrap(), b"do not mutate");
        assert_eq!(fs::read(&alias).unwrap(), b"do not mutate");
    }

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
    fn atomic_cgroup_execution_never_exposes_writable_host_sysfs() {
        let root = tempfile::tempdir().unwrap();
        let root_anchor = open_path_directory(root.path());
        let mut container = Container::new_anchored(root.path(), &root_anchor).unwrap();
        assert!(matches!(
            require_atomic_cgroup_policy(&container),
            Err(ContainerRunError::UnsafeCgroupSysPolicy)
        ));

        container.pseudo_filesystems.sys = SysPolicy::HostReadOnly;
        require_atomic_cgroup_policy(&container).unwrap();
        container.pseudo_filesystems.sys = SysPolicy::None;
        require_atomic_cgroup_policy(&container).unwrap();

        assert!(matches!(
            require_atomic_cgroup_policy(&Container::new(root.path())),
            Err(ContainerRunError::AtomicCgroupRequiresAnchoredRoot)
        ));
    }

    #[test]
    fn atomic_cgroup_execution_rejects_direct_cgroup_filesystem_authority() {
        let cgroup_anchor = open_path_directory(Path::new("/sys/fs/cgroup"));
        let mut cgroup_root = Container::new_anchored("/sys/fs/cgroup", &cgroup_anchor).unwrap();
        cgroup_root.pseudo_filesystems.sys = SysPolicy::None;
        assert!(matches!(
            require_atomic_cgroup_policy(&cgroup_root),
            Err(ContainerRunError::UnsafeCgroupRootFilesystem { .. })
        ));

        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("work")).unwrap();
        let root_anchor = open_path_directory(root.path());
        let writable = Container::new_anchored(root.path(), &root_anchor)
            .unwrap()
            .bind_rw_pinned(&cgroup_anchor, "/sys/fs/cgroup", "/work")
            .unwrap();
        let writable_sources =
            pin_anchored_bind_sources(writable.root_anchor.as_ref().unwrap().as_raw_fd(), &writable.binds).unwrap();
        assert!(matches!(
            require_atomic_cgroup_bind_policy(&writable_sources),
            Err(ContainerRunError::UnsafeCgroupBindSource { .. })
        ));

        let read_only = Container::new_anchored(root.path(), &root_anchor)
            .unwrap()
            .bind_ro_pinned(&cgroup_anchor, "/sys/fs/cgroup", "/work")
            .unwrap();
        let read_only_sources =
            pin_anchored_bind_sources(read_only.root_anchor.as_ref().unwrap().as_raw_fd(), &read_only.binds).unwrap();
        require_atomic_cgroup_bind_policy(&read_only_sources).unwrap();
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
    fn empty_payload_capability_state_removes_every_live_capability() {
        let capabilities = [
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
        for capability in 0..=MAX_LINUX_CAPABILITY_NUMBER {
            assert!(capability_is_set(&capabilities, capability));
        }

        let empty = [CapabilityData::default(); 2];
        for capability in 0..=MAX_LINUX_CAPABILITY_NUMBER {
            assert!(!capability_is_set(&empty, capability));
        }
    }

    #[test]
    fn standard_descriptors_reject_pathname_capabilities() {
        assert!(standard_descriptor_is_unsafe(nix::libc::S_IFDIR, nix::libc::O_RDONLY));
        assert!(standard_descriptor_is_unsafe(nix::libc::S_IFREG, nix::libc::O_PATH));
        assert!(!standard_descriptor_is_unsafe(nix::libc::S_IFREG, nix::libc::O_RDONLY));
        assert!(!standard_descriptor_is_unsafe(nix::libc::S_IFIFO, nix::libc::O_WRONLY));
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
                require_payload_security_boundary()?;

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

    #[test]
    fn anchored_root_path_substitution_cannot_redirect_payload() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let pinned = temporary.path().join("pinned-root");
        let replacement = temporary.path().join("replacement-root");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("etc")).unwrap();
        fs::write(root.join("etc/resolv.conf"), b"verified placeholder").unwrap();
        fs::write(root.join("identity"), b"descriptor-root").unwrap();

        let anchor = open_path_directory(&root);
        let caller_anchor_descriptor = anchor.as_raw_fd();
        let sentinel = open_path_file(Path::new("/dev/null"));
        let sentinel_descriptor = sentinel.as_raw_fd();
        let container = Container::new_anchored(&root, &anchor)
            .unwrap()
            .pseudo_filesystems(PseudoFilesystemPolicy {
                proc: ProcPolicy::None,
                tmp: TmpPolicy::Disabled,
                sys: SysPolicy::None,
                dev: DevPolicy::None,
            })
            .loopback(LoopbackPolicy::KernelDefault)
            .networking(true);
        let child_anchor_descriptor = container.root_anchor.as_ref().unwrap().as_raw_fd();

        // Substitute the complete label after construction. The O_PATH
        // duplicate retained by Container still denotes the renamed tree. A
        // symlink is deliberately stronger than an ordinary directory swap:
        // descriptor-only attachment must not resolve the label at all.
        fs::rename(&root, &pinned).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(replacement.join("identity"), b"replacement-root").unwrap();
        std::os::unix::fs::symlink(&replacement, &root).unwrap();

        let result = container.run::<io::Error>(|| {
            for (name, fd) in [
                ("container root duplicate", child_anchor_descriptor),
                ("caller root anchor", caller_anchor_descriptor),
                ("unrelated sentinel", sentinel_descriptor),
            ] {
                if fcntl(fd, FcntlArg::F_GETFD) != Err(Errno::EBADF) {
                    return Err(io::Error::other(format!("{name} descriptor {fd} leaked into payload")));
                }
            }
            let identity = fs::read("/identity")?;
            if identity != b"descriptor-root" {
                return Err(io::Error::other(format!(
                    "payload entered substituted root: {}",
                    String::from_utf8_lossy(&identity)
                )));
            }
            let resolver = fs::metadata("/etc/resolv.conf")?;
            if !resolver.is_file() || resolver.permissions().mode() & 0o777 != 0o644 {
                return Err(io::Error::other(format!(
                    "resolver mount has nondeterministic type/mode {:o}",
                    resolver.permissions().mode()
                )));
            }
            require_errno(
                fs::write("/etc/resolv.conf", b"payload mutation"),
                Errno::EROFS,
                "mutate sealed resolver mount",
            )?;
            fs::write("/payload-witness", b"anchored")
        });

        match result {
            Ok(()) => {
                assert!(fcntl(caller_anchor_descriptor, FcntlArg::F_GETFD).is_ok());
                assert!(fcntl(sentinel_descriptor, FcntlArg::F_GETFD).is_ok());
                assert_eq!(fs::read(pinned.join("payload-witness")).unwrap(), b"anchored");
                assert!(!root.join("payload-witness").exists());
                assert_eq!(fs::read(root.join("identity")).unwrap(), b"replacement-root");
                assert_eq!(
                    fs::read(pinned.join("etc/resolv.conf")).unwrap(),
                    b"verified placeholder",
                    "resolver publication must not mutate the authenticated backing tree"
                );
                assert!(
                    !pinned.join("old_root").exists(),
                    "anchored pivot must not create put_old inside the authenticated backing tree"
                );
            }
            Err(error) => {
                let classification = classify_anchored_activation_unavailable(&error, &root);
                if let Some(classification) = classification
                    && std::env::var_os("CONTAINER_REQUIRE_ANCHORED_ACTIVATION").as_deref()
                        != Some(std::ffi::OsStr::new("1"))
                {
                    eprintln!(
                        "SKIP anchored-root substitution test: required host capability unavailable: {classification}: {error}"
                    );
                    return;
                }
                panic!("anchored-root substitution test failed: {error}");
            }
        }
    }

    #[test]
    fn anchored_root_relative_install_is_exact_writable_exception_after_label_substitution() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let authenticated = temporary.path().join("authenticated-root");
        let external_work = temporary.path().join("external-work");
        fs::create_dir_all(root.join("install")).unwrap();
        fs::create_dir(root.join("work")).unwrap();
        fs::create_dir(root.join("locked")).unwrap();
        fs::write(root.join("locked/input"), b"immutable").unwrap();
        fs::create_dir(&external_work).unwrap();

        let anchor = open_path_directory(&root);
        let work = open_path_directory(&external_work);
        let container = Container::new_anchored(&root, &anchor)
            .unwrap()
            .bind_rw_from_root("/install", "/install")
            .unwrap()
            .bind_rw_pinned(&work, &external_work, "/work")
            .unwrap()
            .root_filesystem(RootFilesystemPolicy::ReadOnly)
            .pseudo_filesystems(PseudoFilesystemPolicy {
                proc: ProcPolicy::None,
                tmp: TmpPolicy::Disabled,
                sys: SysPolicy::None,
                dev: DevPolicy::None,
            })
            .loopback(LoopbackPolicy::KernelDefault);

        fs::rename(&root, &authenticated).unwrap();
        fs::create_dir_all(root.join("install")).unwrap();
        fs::write(root.join("install/replacement"), b"must stay hidden").unwrap();

        let result = container.run::<io::Error>(|| {
            fs::write("/install/result", b"authenticated install")?;
            fs::write("/work/result", b"external work")?;
            require_errno(
                fs::write("/locked/mutation", b"rejected"),
                Errno::EROFS,
                "mutate undeclared anchored root path",
            )?;
            require_payload_security_boundary()
        });

        match result {
            Ok(()) => {
                assert_eq!(
                    fs::read(authenticated.join("install/result")).unwrap(),
                    b"authenticated install"
                );
                assert!(!root.join("install/result").exists());
                assert_eq!(fs::read(root.join("install/replacement")).unwrap(), b"must stay hidden");
                assert_eq!(fs::read(external_work.join("result")).unwrap(), b"external work");
                assert!(!authenticated.join("locked/mutation").exists());
            }
            Err(error) => {
                let classification = classify_anchored_activation_unavailable(&error, &root);
                if let Some(classification) = classification
                    && std::env::var_os("CONTAINER_REQUIRE_ANCHORED_ACTIVATION").as_deref()
                        != Some(std::ffi::OsStr::new("1"))
                {
                    eprintln!(
                        "SKIP anchored install-bind test: required host capability unavailable: {classification}: {error}"
                    );
                    return;
                }
                panic!("anchored install-bind test failed: {error}");
            }
        }
    }

    #[test]
    fn anchored_payload_error_transport_is_bounded_and_completes() {
        let root = tempfile::tempdir().unwrap();
        let anchor = open_path_directory(root.path());
        let container = Container::new_anchored(root.path(), &anchor)
            .unwrap()
            .pseudo_filesystems(PseudoFilesystemPolicy {
                proc: ProcPolicy::None,
                tmp: TmpPolicy::Disabled,
                sys: SysPolicy::None,
                dev: DevPolicy::None,
            })
            .loopback(LoopbackPolicy::KernelDefault);
        let result = container.run::<io::Error>(|| Err(io::Error::other("x".repeat(1024 * 1024))));

        match result {
            Err(ContainerRunError::Failure { message }) if message.starts_with("run: ") => {
                assert_eq!(message.len(), MAX_CHILD_ERROR_BYTES);
            }
            Err(error) => {
                let classification = classify_anchored_activation_unavailable(&error, root.path());
                if let Some(classification) = classification
                    && std::env::var_os("CONTAINER_REQUIRE_ANCHORED_ACTIVATION").as_deref()
                        != Some(std::ffi::OsStr::new("1"))
                {
                    eprintln!(
                        "SKIP anchored bounded-error test: required host capability unavailable: {classification}: {error}"
                    );
                    return;
                }
                panic!("anchored bounded-error test failed: {error}");
            }
            Ok(()) => panic!("anchored payload unexpectedly accepted an error"),
        }
    }

    #[test]
    fn anchored_root_clone_excludes_undeclared_nested_mounts() {
        const PROC_SUPER_MAGIC: nix::libc::c_long = 0x0000_9fa0;

        let anchor = open_path_directory(Path::new("/"));
        let label = PathBuf::from("/diagnostic-only/host-root");
        let container = Container::new_anchored(&label, &anchor)
            .unwrap()
            .pseudo_filesystems(PseudoFilesystemPolicy {
                proc: ProcPolicy::None,
                tmp: TmpPolicy::Disabled,
                sys: SysPolicy::None,
                dev: DevPolicy::None,
            })
            .loopback(LoopbackPolicy::KernelDefault);
        drop(anchor);

        let result = container.run::<io::Error>(|| {
            // SAFETY: the path is static and NUL terminated; statfs points to
            // a fully initialized output object for the duration of the call.
            let mut stat: nix::libc::statfs = unsafe { std::mem::zeroed() };
            if unsafe { nix::libc::statfs(c"/proc".as_ptr(), &mut stat) } == -1 {
                return Err(io::Error::last_os_error());
            }
            if stat.f_type == PROC_SUPER_MAGIC {
                return Err(io::Error::other(format!(
                    "undeclared nested /proc mount was imported: filesystem magic={:#x}",
                    stat.f_type
                )));
            }
            match fs::metadata("/proc/self/stat") {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
                Ok(_) => return Err(io::Error::other("undeclared nested /proc contents were imported")),
            }
            Ok(())
        });

        match result {
            Ok(()) => {}
            Err(error) => {
                let classification = classify_anchored_activation_unavailable(&error, &label);
                if let Some(classification) = classification
                    && std::env::var_os("CONTAINER_REQUIRE_ANCHORED_ACTIVATION").as_deref()
                        != Some(std::ffi::OsStr::new("1"))
                {
                    eprintln!(
                        "SKIP anchored root nested-mount exclusion test: required host capability unavailable: {classification}: {error}"
                    );
                    return;
                }
                panic!("anchored root nested-mount exclusion test failed: {error}");
            }
        }
    }

    #[test]
    fn anchored_directory_bind_excludes_undeclared_nested_mounts() {
        const PROC_SUPER_MAGIC: nix::libc::c_long = 0x0000_9fa0;

        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("import")).unwrap();
        let root_anchor = open_path_directory(root.path());
        let host_root = open_path_directory(Path::new("/"));
        let container = Container::new_anchored(root.path(), &root_anchor)
            .unwrap()
            .bind_ro_pinned(&host_root, "/", "/import")
            .unwrap()
            .pseudo_filesystems(PseudoFilesystemPolicy {
                proc: ProcPolicy::None,
                tmp: TmpPolicy::Disabled,
                sys: SysPolicy::None,
                dev: DevPolicy::None,
            })
            .loopback(LoopbackPolicy::KernelDefault);

        let result = container.run::<io::Error>(|| {
            // SAFETY: the path is static and NUL terminated; statfs points to
            // a fully initialized output object for the duration of the call.
            let mut stat: nix::libc::statfs = unsafe { std::mem::zeroed() };
            if unsafe { nix::libc::statfs(c"/import/proc".as_ptr(), &mut stat) } == -1 {
                return Err(io::Error::last_os_error());
            }
            if stat.f_type == PROC_SUPER_MAGIC {
                return Err(io::Error::other(format!(
                    "directory bind imported nested /proc mount: filesystem magic={:#x}",
                    stat.f_type
                )));
            }
            match fs::metadata("/import/proc/self/stat") {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
                Ok(_) => Err(io::Error::other(
                    "directory bind imported undeclared nested /proc contents",
                )),
            }
        });

        match result {
            Ok(()) => {}
            Err(error) => {
                let classification = classify_anchored_activation_unavailable(&error, root.path());
                if let Some(classification) = classification
                    && std::env::var_os("CONTAINER_REQUIRE_ANCHORED_ACTIVATION").as_deref()
                        != Some(std::ffi::OsStr::new("1"))
                {
                    eprintln!(
                        "SKIP anchored bind nested-mount exclusion test: required host capability unavailable: {classification}: {error}"
                    );
                    return;
                }
                panic!("anchored bind nested-mount exclusion test failed: {error}");
            }
        }
    }

    fn classify_anchored_activation_unavailable(error: &ContainerRunError, label: &Path) -> Option<&'static str> {
        if host_denied_user_namespace_setup(error) {
            return Some("user-namespace setup denied");
        }
        let ContainerRunError::Failure { message } = error else {
            return None;
        };
        let permission_denied = "EPERM: Operation not permitted";
        let syscall_missing = "ENOSYS: Function not implemented";
        if message == &format!("mount /: {permission_denied}") {
            return Some("private mount namespace denied");
        }
        if message.starts_with("clone sealed resolver mount through anchored root descriptor:")
            && (message.contains("Operation not permitted") || message.contains("Function not implemented"))
        {
            return Some("detached resolver mounts unavailable");
        }
        if message.starts_with("make sealed resolver mount read-only through anchored root descriptor:")
            && (message.contains("Operation not permitted") || message.contains("Function not implemented"))
        {
            return Some("resolver mount attributes unavailable");
        }
        if message.starts_with("attach sealed resolver mount through anchored root descriptor:")
            && (message.contains("Operation not permitted") || message.contains("Function not implemented"))
        {
            return Some("resolver mount attachment unavailable");
        }
        if message
            == &format!(
                "clone descriptor-backed root mount for anchored root {}: {permission_denied}",
                label.display()
            )
        {
            return Some("open_tree denied");
        }
        if message
            == &format!(
                "clone descriptor-backed root mount for anchored root {}: {syscall_missing}",
                label.display()
            )
        {
            return Some("open_tree unavailable");
        }
        if message
            == &format!(
                "attach descriptor-backed root mount for anchored root {}: {permission_denied}",
                label.display()
            )
        {
            return Some("move_mount denied");
        }
        if message
            == &format!(
                "attach descriptor-backed root mount for anchored root {}: {syscall_missing}",
                label.display()
            )
        {
            return Some("move_mount unavailable");
        }
        None
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
                checked_prctl_value(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, capability, 0, 0))
                    .map_err(errno_to_io)?
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

    #[test]
    fn namespace_capability_denial_is_not_inferred_from_generic_nix_failures() {
        for source in [Errno::EPERM, Errno::EACCES, Errno::ENOSYS] {
            assert!(host_denied_user_namespace_setup(&ContainerRunError::CloneNamespaces {
                source,
            }));
        }
        assert!(!host_denied_user_namespace_setup(&ContainerRunError::CloneNamespaces {
            source: Errno::EAGAIN,
        }));
        assert!(!host_denied_user_namespace_setup(&ContainerRunError::Nix {
            source: Errno::EPERM,
        }));
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
        assert_eq!(MINIMAL_DEV_IDENTITIES, [("null", 1, 3), ("zero", 1, 5), ("full", 1, 7)]);
    }

    #[test]
    fn minimal_dev_accepts_only_exact_linux_character_device_identities() {
        for &(name, major, minor) in MINIMAL_DEV_IDENTITIES {
            let path = Path::new("/dev").join(name);
            let device = open_path_file(&path);
            validate_minimal_device_source(device.as_raw_fd(), &path, major, minor).unwrap();
        }

        let regular = tempfile::NamedTempFile::new().unwrap();
        let regular = open_path_file(regular.path());
        assert!(matches!(
            validate_minimal_device_source(regular.as_raw_fd(), Path::new("regular"), 1, 3),
            Err(ContainerError::UnsupportedAnchoredMountSource { mode, .. }) if mode == nix::libc::S_IFREG
        ));

        for (source, label, expected_major, expected_minor, actual_major, actual_minor) in [
            ("/dev/zero", "/dev/null", 1, 3, 1, 5),
            ("/dev/full", "/dev/zero", 1, 5, 1, 7),
            ("/dev/null", "/dev/full", 1, 7, 1, 3),
        ] {
            let wrong_device = open_path_file(Path::new(source));
            assert!(matches!(
                validate_minimal_device_source(
                    wrong_device.as_raw_fd(),
                    Path::new(label),
                    expected_major,
                    expected_minor,
                ),
                Err(ContainerError::UnexpectedMinimalDeviceIdentity {
                    expected_major: error_expected_major,
                    expected_minor: error_expected_minor,
                    actual_major: error_actual_major,
                    actual_minor: error_actual_minor,
                    ..
                }) if (error_expected_major, error_expected_minor, error_actual_major, error_actual_minor)
                    == (expected_major, expected_minor, actual_major, actual_minor)
            ));
        }
    }

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
    fn clone_stack_has_a_non_accessible_guard_and_read_write_usable_mapping() {
        fn permissions(address: usize) -> Option<String> {
            let maps = fs::read_to_string("/proc/self/maps").ok()?;
            maps.lines().find_map(|line| {
                let mut fields = line.split_whitespace();
                let mut range = fields.next()?.split('-');
                let start = usize::from_str_radix(range.next()?, 16).ok()?;
                let end = usize::from_str_radix(range.next()?, 16).ok()?;
                let permissions = fields.next()?;
                (start <= address && address < end).then(|| permissions.to_owned())
            })
        }

        let mut stack = CloneStack::new().unwrap();
        let guard = stack.guard_address();
        let usable = stack.usable_address();
        assert_eq!(permissions(guard).as_deref(), Some("---p"));
        assert_eq!(permissions(usable).as_deref(), Some("rw-p"));
        let slice = stack.as_mut_slice();
        assert_eq!(slice.len(), CLONE_STACK_BYTES);
        assert_eq!(slice.as_ptr() as usize, usable);
        slice[0] = 1;
        slice[CLONE_STACK_BYTES - 1] = 2;
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

    #[test]
    fn special_file_bind_gets_a_file_mountpoint() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("device.fifo");
        mkfifo(&source, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let target = temporary.path().join("mountpoints/device");

        assert!(fs::metadata(&source).unwrap().file_type().is_fifo());
        prepare_bind_target(&source, &target).unwrap();

        let target_metadata = fs::metadata(target).unwrap();
        assert!(target_metadata.is_file());
        assert_eq!(target_metadata.len(), 0);
    }
}
