/// Authenticated authority to retry removal of one setup-time cgroup leaf.
///
/// This value is returned only when automatic setup rollback itself failed.
/// It owns the delegated-root lock and the unpredictable leaf name. Dropping
/// it performs no syscall: a supervisor must explicitly retry or quarantine
/// the delegation rather than receiving an unreported cleanup attempt.
#[derive(Debug)]
pub struct CgroupRecovery {
    authority: DelegationAuthority,
    name: CString,
    label: PathBuf,
    identity_witness: Option<DescriptorIdentity>,
    active: bool,
}

impl CgroupRecovery {
    fn new(
        authority: DelegationAuthority,
        name: CString,
        label: PathBuf,
        identity_witness: Option<DescriptorIdentity>,
    ) -> Self {
        Self {
            authority,
            name,
            label,
            identity_witness,
            active: true,
        }
    }

    pub fn label(&self) -> &Path {
        &self.label
    }

    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Reopen, authenticate, and remove the exact empty leaf.
    ///
    /// A missing initial witness is possible only for a failure immediately
    /// after the exclusive `mkdirat`. The delegation contract excludes any
    /// uncooperative same-UID actor from reaching that unpredictable name.
    pub fn retry_remove(&mut self) -> Result<()> {
        if !self.active {
            // Removal may already have succeeded while the asynchronous
            // topology verification failed. Retrying must revalidate the
            // supervisor-only baseline rather than silently treating that
            // earlier error as final success.
            return self.authority.probe_cleanup_baseline();
        }
        let identity_witness = match self.identity_witness {
            Some(identity_witness) => identity_witness,
            None => {
                let pinned = open_control_path(
                    &self.authority.directory,
                    &self.name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
                .map_err(|source| descriptor_error("pin provisional cgroup leaf for recovery", &self.label, source))?;
                require_directory(&pinned, &self.label)?;
                let identity_witness = descriptor_identity(&pinned, &self.label)?;
                self.identity_witness = Some(identity_witness);
                identity_witness
            }
        };
        remove_named_authenticated(
            &self.authority.directory,
            &self.name,
            &self.label,
            identity_witness,
            "retry authenticated cgroup leaf cleanup",
        )?;
        self.active = false;
        self.authority.probe_cleanup_baseline()
    }
}

/// Owned lifecycle guard for one configured per-derivation cgroup leaf.
#[derive(Debug)]
pub struct CgroupLeaf {
    authority: Option<DelegationAuthority>,
    directory: OwnedFd,
    name: CString,
    label: PathBuf,
    identity: String,
    identity_witness: DescriptorIdentity,
    active: bool,
    drop_cleanup_enabled: bool,
}

impl CgroupLeaf {
    fn authority(&self) -> Result<&DelegationAuthority> {
        self.authority.as_ref().ok_or(CgroupError::RemovalAuthorityUnavailable)
    }

    fn into_recovery(mut self) -> Result<CgroupRecovery> {
        self.drop_cleanup_enabled = false;
        self.active = false;
        let authority = self.authority.take().ok_or(CgroupError::RemovalAuthorityUnavailable)?;
        Ok(CgroupRecovery::new(
            authority,
            self.name.clone(),
            self.label.clone(),
            Some(self.identity_witness),
        ))
    }

    fn probe_ready_topology(&self) -> Result<()> {
        self.authority()?.probe_ready(self)
    }

    fn probe_cleanup_baseline(&self) -> Result<()> {
        self.authority()?.probe_cleanup_baseline()
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn label(&self) -> &Path {
        &self.label
    }

    /// Borrow the pinned cgroup directory for crate-owned atomic placement.
    ///
    /// This is intentionally not public API. Numeric writes to `cgroup.procs`
    /// cannot authenticate a process because Linux may recycle a PID between
    /// observation and write. The future `Container` integration must pass
    /// this descriptor to `clone3(CLONE_INTO_CGROUP | CLONE_PIDFD)` instead and
    /// close all inherited cgroup capabilities in the child before payload code
    /// can run.
    #[allow(dead_code)]
    pub(crate) fn placement(&self) -> Result<CgroupPlacement<'_>> {
        self.probe_ready_topology()?;
        let authority = self.authority()?;
        Ok(CgroupPlacement {
            root: authority.directory.as_fd(),
            target: self.directory.as_fd(),
        })
    }

    /// Revalidate atomic placement before releasing the clone child.
    ///
    /// At this point the child is still blocked on the setup pipe, so its
    /// unique TGID must be the leaf's complete membership. This closes the
    /// gap between a successful `CLONE_INTO_CGROUP` return and untrusted setup
    /// by rejecting a missing, duplicated-foreign, or pre-populated target.
    pub(crate) fn require_sole_member(&self, expected_tgid: u32) -> Result<()> {
        self.authority()?.probe_activated(self, expected_tgid)
    }

    /// Read and strictly parse the leaf's current core event state.
    pub fn events(&self) -> Result<CgroupEvents> {
        read_events(&self.directory, &self.label)
    }

    /// Ask the kernel to SIGKILL every process in this cgroup subtree.
    pub fn kill(&self) -> Result<()> {
        write_control(&self.directory, c"cgroup.kill", b"1", &self.label)
    }

    /// Boundedly wait until `cgroup.events` reports `populated 0`.
    pub fn wait_until_empty(&self, policy: DrainPolicy) -> Result<()> {
        let started = Instant::now();
        loop {
            if !self.events()?.populated() {
                return Ok(());
            }
            let elapsed = started.elapsed();
            if elapsed >= policy.timeout {
                return Err(CgroupError::DrainTimeout {
                    path: self.label.clone(),
                    timeout: policy.timeout,
                });
            }
            thread::sleep(policy.poll_interval.min(policy.timeout.saturating_sub(elapsed)));
        }
    }

    /// Kill, drain, and remove this exact leaf, returning cleanup failures.
    pub fn kill_and_remove(&mut self, policy: DrainPolicy) -> Result<()> {
        // This explicit operation is authoritative. If it fails, Drop must not
        // silently retry with a different timeout, but the caller retains this
        // authenticated capability for an explicit retry or quarantine.
        self.drop_cleanup_enabled = false;
        self.cleanup(policy)
    }

    /// Remove a configured leaf when no clone child was created.
    ///
    /// This path is used for parent-side preparation or `clone3` failures. It
    /// must not issue `cgroup.kill`: population at this stage is an invariant
    /// violation rather than a process tree that the caller knowingly owns.
    pub(crate) fn remove_unstarted(&mut self) -> Result<()> {
        self.drop_cleanup_enabled = false;
        if !self.active {
            return self.probe_cleanup_baseline();
        }
        self.require_empty_for_configuration()?;
        self.remove_authenticated()?;
        self.probe_cleanup_baseline()
    }

    fn configure(&self, limits: CgroupLimits) -> Result<()> {
        self.require_empty_for_configuration()?;
        write_control(
            &self.directory,
            c"pids.max",
            limits.pids_max.to_string().as_bytes(),
            &self.label,
        )?;
        write_control(
            &self.directory,
            c"memory.max",
            limits.memory_max.to_string().as_bytes(),
            &self.label,
        )?;
        write_control(
            &self.directory,
            c"memory.swap.max",
            limits.memory_swap_max.to_string().as_bytes(),
            &self.label,
        )?;
        write_control(&self.directory, c"memory.oom.group", b"1", &self.label)?;
        // A derivation is a terminal resource domain. Prevent payload-created
        // cgroup subtrees from consuming unbounded kernel metadata or keeping
        // authenticated leaf removal busy after every process has exited.
        write_control(&self.directory, c"cgroup.max.depth", b"0", &self.label)?;
        write_control(&self.directory, c"cgroup.max.descendants", b"0", &self.label)?;
        // Upstream Linux 5.14 exposes cpu.max.burst together with cpu.max.
        // Accept exact absence for custom or selectively backported kernels:
        // absence preserves the kernel's no-burst behavior, while every
        // present control is still authenticated, written, and read back.
        let cpu_max_burst_present = write_control_if_present(&self.directory, c"cpu.max.burst", b"0", &self.label)?;
        let cpu_max = format!("{} {}", limits.cpu_quota_micros, limits.cpu_period_micros);
        write_control(&self.directory, c"cpu.max", cpu_max.as_bytes(), &self.label)?;

        self.require_empty_for_configuration()?;
        self.verify_configured_controls(limits, cpu_max_burst_present)?;
        self.require_activation_controls()
    }

    fn require_empty_for_configuration(&self) -> Result<()> {
        let events = self.events()?;
        if events.populated() {
            Err(CgroupError::LeafPopulatedDuringConfiguration {
                path: self.label.join("cgroup.events"),
            })
        } else if events.frozen() {
            Err(CgroupError::LeafFrozenDuringConfiguration {
                path: self.label.join("cgroup.events"),
            })
        } else {
            Ok(())
        }
    }

    fn verify_configured_controls(&self, limits: CgroupLimits, cpu_max_burst_present: bool) -> Result<()> {
        verify_control(&self.directory, c"pids.max", &limits.pids_max.to_string(), &self.label)?;
        verify_control(
            &self.directory,
            c"memory.max",
            &limits.memory_max.to_string(),
            &self.label,
        )?;
        verify_control(
            &self.directory,
            c"memory.swap.max",
            &limits.memory_swap_max.to_string(),
            &self.label,
        )?;
        verify_control(&self.directory, c"memory.oom.group", "1", &self.label)?;
        verify_control(&self.directory, c"cgroup.max.depth", "0", &self.label)?;
        verify_control(&self.directory, c"cgroup.max.descendants", "0", &self.label)?;
        if cpu_max_burst_present {
            verify_control(&self.directory, c"cpu.max.burst", "0", &self.label)?;
        }
        verify_control(
            &self.directory,
            c"cpu.max",
            &format!("{} {}", limits.cpu_quota_micros, limits.cpu_period_micros),
            &self.label,
        )
    }

    fn require_activation_controls(&self) -> Result<()> {
        // Atomic CLONE_INTO_CGROUP placement still requires migration access
        // to this leaf, and every post-activation error path depends on the
        // race-safe subtree kill primitive. Prove both capabilities while the
        // leaf is empty, before lending its placement descriptor.
        drop(open_owned_writable_control(
            &self.directory,
            c"cgroup.procs",
            &self.label,
        )?);
        drop(open_owned_writable_control(
            &self.directory,
            c"cgroup.threads",
            &self.label,
        )?);
        drop(open_owned_writable_control(
            &self.directory,
            c"cgroup.kill",
            &self.label,
        )?);
        self.require_empty_for_configuration()
    }

    fn cleanup(&mut self, policy: DrainPolicy) -> Result<()> {
        if !self.active {
            return self.probe_cleanup_baseline();
        }

        let mut failure = self.kill().err();
        let drained = self.wait_until_empty(policy);
        if let Err(error) = drained {
            append_failure(&mut failure, error);
        } else {
            match self.remove_authenticated() {
                Err(error) => append_failure(&mut failure, error),
                Ok(()) => {
                    if let Err(error) = self.probe_cleanup_baseline() {
                        append_failure(&mut failure, error);
                    }
                }
            }
        }

        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Reopen and authenticate the witnessed leaf, then directly remove it.
    ///
    /// cgroup v2 rejects rename, so `unlinkat(AT_REMOVEDIR)` must address the
    /// original name. Linux has no conditional-rmdir syscall: the advisory
    /// delegated-root lock and caller's exclusive-ownership guarantee are what
    /// make the final precheck/remove sequence valid.
    fn remove_authenticated(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }

        self.probe_ready_topology()?;
        remove_named_authenticated(
            &self.authority()?.directory,
            &self.name,
            &self.label,
            self.identity_witness,
            "remove authenticated empty cgroup leaf",
        )?;
        self.active = false;
        Ok(())
    }
}

impl Drop for CgroupLeaf {
    fn drop(&mut self) {
        if self.active && self.drop_cleanup_enabled {
            // Drop never blocks for the default drain timeout. It issues the
            // kill, observes events once, and removes only an already-empty
            // authenticated leaf. A populated leaf is deliberately left for a
            // supervisor-owned reaper rather than hidden latency in Drop.
            let _ = self.kill();
            if matches!(self.events(), Ok(events) if !events.populated()) {
                let _ = self.remove_authenticated();
            }
        }
    }
}

/// Crate-private capability intended for `clone3(CLONE_INTO_CGROUP)`.
#[allow(dead_code)]
pub(crate) struct CgroupPlacement<'a> {
    root: BorrowedFd<'a>,
    target: BorrowedFd<'a>,
}

impl CgroupPlacement<'_> {
    /// The delegated root is retained only for authenticated cleanup. The
    /// clone child must close its copied descriptor before trusted setup runs.
    #[allow(dead_code)]
    pub(crate) fn root(&self) -> BorrowedFd<'_> {
        self.root
    }

    /// Directory descriptor passed to `clone3(CLONE_INTO_CGROUP)`.
    #[allow(dead_code)]
    pub(crate) fn target(&self) -> BorrowedFd<'_> {
        self.target
    }

    /// Both cgroup capabilities copied by clone, in deterministic close order.
    #[allow(dead_code)]
    pub(crate) fn inherited_raw_fds(&self) -> [RawFd; 2] {
        [self.root.as_raw_fd(), self.target.as_raw_fd()]
    }
}

impl AsFd for CgroupPlacement<'_> {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.target
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DescriptorIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreationStage {
    Mkdir,
    Pinned,
    Witnessed,
    AuthorityTransferred,
}

struct ProvisionalLeafRollback {
    authority: DelegationAuthority,
    name: CString,
    label: PathBuf,
    identity_witness: Option<DescriptorIdentity>,
}

impl ProvisionalLeafRollback {
    fn new(authority: DelegationAuthority, name: CString, label: PathBuf) -> Self {
        Self {
            authority,
            name,
            label,
            identity_witness: None,
        }
    }

    fn authenticate(&mut self, identity_witness: DescriptorIdentity) {
        self.identity_witness = Some(identity_witness);
    }

    fn disarm(self) -> (DelegationAuthority, CString, PathBuf) {
        (self.authority, self.name, self.label)
    }

    fn rollback_after(mut self, failure: CgroupError) -> CgroupError {
        match self.rollback() {
            Ok(()) => match self.authority.probe_cleanup_baseline() {
                Ok(()) => failure,
                Err(cleanup) => CgroupError::CleanupAfterFailure {
                    failure: Box::new(failure),
                    cleanup: Box::new(cleanup),
                },
            },
            Err(cleanup) => CgroupError::CleanupRecovery {
                failure: Box::new(failure),
                cleanup: Box::new(cleanup),
                recovery: Box::new(CgroupRecovery::new(
                    self.authority,
                    self.name,
                    self.label,
                    self.identity_witness,
                )),
            },
        }
    }

    fn rollback(&mut self) -> Result<()> {
        let identity_witness = match self.identity_witness {
            Some(identity_witness) => identity_witness,
            None => {
                // No fallible operation occurs between mkdir and arming this
                // guard. Under the locked-root ownership contract, pinning the
                // unpredictable name now witnesses the directory just created.
                let pinned = open_control_path(
                    &self.authority.directory,
                    &self.name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
                .map_err(|source| descriptor_error("pin provisional cgroup leaf for rollback", &self.label, source))?;
                require_directory(&pinned, &self.label)?;
                let identity_witness = descriptor_identity(&pinned, &self.label)?;
                self.identity_witness = Some(identity_witness);
                identity_witness
            }
        };
        remove_named_authenticated(
            &self.authority.directory,
            &self.name,
            &self.label,
            identity_witness,
            "roll back authenticated provisional cgroup leaf",
        )
    }
}

fn creation_checkpoint(
    checkpoint: &mut dyn FnMut(CreationStage) -> io::Result<()>,
    stage: CreationStage,
    label: &Path,
) -> Result<()> {
    checkpoint(stage).map_err(|source| descriptor_error("cgroup leaf creation checkpoint", label, source))
}

fn remove_named_authenticated(
    root: &OwnedFd,
    name: &CStr,
    label: &Path,
    expected: DescriptorIdentity,
    operation: &'static str,
) -> Result<()> {
    let pinned = open_control_path(
        root,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    )
    .map_err(|source| descriptor_error("reopen named cgroup leaf for removal", label, source))?;
    require_directory(&pinned, label)?;
    let found = descriptor_identity(&pinned, label)?;
    if found != expected {
        return Err(CgroupError::LeafReplaced {
            path: label.to_owned(),
            expected_device: expected.device,
            expected_inode: expected.inode,
            found_device: found.device,
            found_inode: found.inode,
        });
    }

    // SAFETY: root and the authenticated single-component name remain live.
    // The exclusive-root contract prevents a legitimate mutation between this
    // witness check and unlinkat; Linux has no atomic conditional-rmdir API.
    if unsafe { libc::unlinkat(root.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } == -1 {
        Err(descriptor_error(operation, label, io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn append_failure(failure: &mut Option<CgroupError>, next: CgroupError) {
    *failure = Some(match failure.take() {
        Some(previous) => CgroupError::CleanupAfterFailure {
            failure: Box::new(previous),
            cleanup: Box::new(next),
        },
        None => next,
    });
}
