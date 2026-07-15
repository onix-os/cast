use std::io;
use std::os::fd::{AsRawFd, RawFd};

use nix::errno::Errno;
use nix::libc::SIGCHLD;
use nix::sched::{CloneFlags, clone};
use nix::sys::signal::Signal;
use nix::sys::wait::WaitStatus;
use nix::unistd::{close, read};
use snafu::ResultExt;

use super::cgroup;
#[cfg(not(test))]
use super::clone3;
use super::clone3::{Clone3Outcome, clone3_into_cgroup};
use super::idmap::{idmap, validated_caller_identity};
use super::mounts::{PinnedAnchoredBindSource, pin_anchored_bind_sources};
use super::payload::enter;
#[cfg(test)]
use super::process_runtime::LEGACY_TEST_ACTIVATION_LOCK;
use super::process_runtime::{
    BlockedSignalMask, ChildLifecycle, CloneStack, SignalOverride, SyncSocket, abort_child, close_sync_endpoint,
    format_error, send_packet_no_signal, set_fd_nonblocking,
};
use super::{
    CloseInheritedCgroupDescriptorSnafu, Container, ContainerError, Error, InvalidInheritedCgroupDescriptorsSnafu,
    MAX_CHILD_ERROR_BYTES, MAX_CONTROL_EINTR_RETRIES, Message, NixSnafu, RetainedInheritedCgroupDescriptorSnafu,
    SysPolicy,
};

impl Container {
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
                let caller_identity = match validated_caller_identity() {
                    Ok(identity) => identity,
                    Err(source) => {
                        if let Err(restore) = signal_mask.restore() {
                            return Err(Error::Failure {
                                message: format!(
                                    "validate caller credentials before clone3: {source}; additionally failed to restore the supervisor signal mask: {restore}"
                                ),
                            });
                        }
                        return Err(Error::Idmap { source });
                    }
                };
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
                Ok::<_, Error>((outcome, inherited, signal_mask, caller_identity))
            })();
            match result {
                Ok((Clone3Outcome::Parent { pid, pidfd }, _, mut signal_mask, caller_identity)) => {
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
                        Ok((child, caller_identity))
                    }
                }
                Ok((Clone3Outcome::Child, inherited, mut signal_mask, _caller_identity)) => {
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

                let mut signal_mask = signal_mask;
                let caller_identity = match validated_caller_identity() {
                    Ok(identity) => identity,
                    Err(source) => {
                        if let Err(restore) = signal_mask.restore() {
                            return Err(Error::Failure {
                                message: format!(
                                    "validate caller credentials before legacy clone: {source}; additionally failed to restore the supervisor signal mask: {restore}"
                                ),
                            });
                        }
                        return Err(Error::Idmap { source });
                    }
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
                    (Ok(pid), Ok(())) => Ok((ChildLifecycle::Legacy { pid }, caller_identity)),
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

        let (child, caller_identity) = match spawn {
            Ok(spawned) => spawned,
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
            if let Err(source) = idmap(pid, &caller_identity) {
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

pub(super) fn require_atomic_cgroup_policy(container: &Container) -> Result<(), Error> {
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

pub(super) fn require_atomic_cgroup_bind_policy(bind_sources: &[PinnedAnchoredBindSource]) -> Result<(), Error> {
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

pub(super) fn descriptor_filesystem_magic(fd: RawFd) -> Result<nix::libc::c_long, Errno> {
    // SAFETY: stat is a live writable output object and fd remains live for
    // the complete fstatfs call.
    let mut stat: nix::libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { nix::libc::fstatfs(fd, &mut stat) } == -1 {
        return Err(Errno::last());
    }
    Ok(stat.f_type)
}

pub(super) fn is_cgroup_filesystem(filesystem: nix::libc::c_long) -> bool {
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

pub(super) fn contain_raw_clone_child_panic(error_writer: RawFd, child: impl FnOnce() -> i32) -> i32 {
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

pub(super) fn read_child_error(fd: RawFd) -> Result<String, Errno> {
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

pub(super) fn namespace_flags(networking: bool) -> CloneFlags {
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
