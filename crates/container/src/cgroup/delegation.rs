#[derive(Debug)]
struct SupervisorAuthority {
    identity_witness: DescriptorIdentity,
    opener_tgid: u32,
}

#[derive(Debug)]
enum DelegationTopology {
    Systemd(SupervisorAuthority),
    #[cfg(test)]
    Simulated,
}

/// The single descriptor-pinned authority for one delegated systemd unit.
///
/// Moving this value between the root, provisional rollback, configured leaf,
/// and recovery types preserves both the advisory lock and the invariant that
/// there is never a second delegated-root descriptor hidden in the lifecycle.
#[derive(Debug)]
struct DelegationAuthority {
    directory: OwnedFd,
    label: PathBuf,
    topology: DelegationTopology,
}

impl DelegationAuthority {
    /// Authenticate the complete supervisor-only topology without requiring
    /// the delegated controllers to have been enabled yet.
    ///
    /// A systemd `Delegate=` + `DelegateSubgroup=` unit may leave every
    /// controller disabled in the delegated root. This probe is therefore
    /// used only during initial acquisition, before Cast performs its one
    /// idempotent mutation. It must remain otherwise identical to steady-state
    /// baseline probe so controller activation can never precede topology
    /// authentication.
    fn probe_pre_enable_baseline(&self) -> Result<BTreeSet<String>> {
        match &self.topology {
            DelegationTopology::Systemd(supervisor) => {
                let enabled = probe_root_authority_pre_enable(&self.directory, &self.label)?;
                require_descendant_topology(&self.directory, &self.label, 1, false)?;
                probe_supervisor(&self.directory, &self.label, supervisor)?;
                require_descendant_topology(&self.directory, &self.label, 1, false)?;
                Ok(enabled)
            }
            #[cfg(test)]
            DelegationTopology::Simulated => Ok(BTreeSet::new()),
        }
    }

    fn probe_baseline(&self) -> Result<()> {
        match &self.topology {
            DelegationTopology::Systemd(supervisor) => {
                probe_root_authority(&self.directory, &self.label)?;
                require_descendant_topology(&self.directory, &self.label, 1, false)?;
                probe_supervisor(&self.directory, &self.label, supervisor)?;
                require_descendant_topology(&self.directory, &self.label, 1, false)
            }
            #[cfg(test)]
            DelegationTopology::Simulated => Ok(()),
        }
    }

    fn probe_ready(&self, leaf: &CgroupLeaf) -> Result<()> {
        match &self.topology {
            DelegationTopology::Systemd(supervisor) => {
                probe_root_authority(&self.directory, &self.label)?;
                require_descendant_topology(&self.directory, &self.label, 2, false)?;
                probe_supervisor(&self.directory, &self.label, supervisor)?;
                probe_leaf(&self.directory, leaf)?;
                require_descendant_topology(&self.directory, &self.label, 2, false)
            }
            #[cfg(test)]
            DelegationTopology::Simulated => probe_leaf_witness(&self.directory, leaf),
        }
    }

    fn probe_activated(&self, leaf: &CgroupLeaf, expected_tgid: u32) -> Result<()> {
        match &self.topology {
            DelegationTopology::Systemd(supervisor) => {
                probe_root_authority(&self.directory, &self.label)?;
                require_descendant_topology(&self.directory, &self.label, 2, false)?;
                probe_supervisor(&self.directory, &self.label, supervisor)?;
                probe_activated_leaf(&self.directory, leaf, expected_tgid)?;
                require_descendant_topology(&self.directory, &self.label, 2, false)
            }
            #[cfg(test)]
            DelegationTopology::Simulated => {
                probe_leaf_witness(&self.directory, leaf)?;
                require_exact_leaf_membership(
                    &read_pid_list(&leaf.directory, c"cgroup.procs", &leaf.label)?,
                    expected_tgid,
                    &leaf.label.join("cgroup.procs"),
                )
            }
        }
    }

    /// A removed cgroup may retain dying controller state temporarily. Verify
    /// the visible tree is back to the supervisor-only baseline without
    /// converting normal asynchronous CSS release into a false cleanup error.
    fn probe_cleanup_baseline(&self) -> Result<()> {
        match &self.topology {
            DelegationTopology::Systemd(supervisor) => {
                probe_root_authority(&self.directory, &self.label)?;
                require_descendant_topology(&self.directory, &self.label, 1, true)?;
                probe_supervisor(&self.directory, &self.label, supervisor)?;
                require_descendant_topology(&self.directory, &self.label, 1, true)
            }
            #[cfg(test)]
            DelegationTopology::Simulated => Ok(()),
        }
    }
}

/// Authenticated, linear capability for a delegated cgroup v2 activation root.
///
/// The root itself is an internal domain with no direct processes. systemd
/// places this Cast process in the fixed `cast-supervisor` child through
/// `DelegateSubgroup=cast-supervisor`; the only other permitted child is the
/// one derivation leaf created by consuming this value.
pub struct DelegatedCgroupRoot {
    authority: DelegationAuthority,
}

impl DelegatedCgroupRoot {
    /// Open and exclusively lock one supervisor-only delegated subtree.
    ///
    /// `mount_point` is opened without following any symlink component. The
    /// expected cgroup mount transition is allowed for that first open; every
    /// subsequent lookup below its descriptor additionally rejects mount
    /// crossings with `RESOLVE_NO_XDEV`.
    ///
    /// The delegated directory must be owned by the effective UID and must not
    /// be group/other writable. A non-blocking advisory lock rejects a second
    /// cooperating supervisor. Linux offers no mandatory directory lock, so
    /// the caller must also ensure that no uncooperative same-UID process or
    /// container payload can reach this subtree for the guard's lifetime.
    pub fn open(mount_point: impl AsRef<Path>, delegated_relative: impl AsRef<Path>) -> Result<Self> {
        let mount_point = normalized_absolute(mount_point.as_ref())?;
        let delegated_relative = normalized_relative(delegated_relative.as_ref())?;
        let mount_name = path_cstring(&mount_point)
            .map_err(|source| descriptor_error("encode cgroup mount path", &mount_point, source))?;
        let mount = openat2(
            libc::AT_FDCWD,
            &mount_name,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
        )
        .map_err(|source| descriptor_error("open cgroup v2 mount", &mount_point, source))?;
        require_cgroup2(&mount, &mount_point)?;

        let relative_name = path_cstring(&delegated_relative)
            .map_err(|source| descriptor_error("encode delegated cgroup path", &delegated_relative, source))?;
        let label = mount_point.join(&delegated_relative);
        let directory = openat2(
            mount.as_raw_fd(),
            &relative_name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            ANCHORED_RESOLUTION,
        )
        .map_err(|source| descriptor_error("open delegated cgroup", &label, source))?;
        require_directory(&directory, &label)?;
        require_cgroup2(&directory, &label)?;

        // A correct systemd `Delegate=` + `DelegateSubgroup=` unit may start
        // with an empty `cgroup.subtree_control`. Authenticate the root and
        // exact supervisor topology before Cast enables anything in it.
        probe_root_authority_pre_enable(&directory, &label)?;
        require_descendant_topology(&directory, &label, 1, false)?;
        let supervisor = capture_supervisor(&directory, &label)?;
        require_descendant_topology(&directory, &label, 1, false)?;
        let authority = DelegationAuthority {
            directory,
            label,
            topology: DelegationTopology::Systemd(supervisor),
        };

        // Repeat the complete pre-mutation authentication through the stored
        // identity witness. Only the exact missing required controllers are
        // then enabled through the pinned root descriptor. A subsequent
        // steady-state probe verifies both effective controls and topology.
        let enabled = authority.probe_pre_enable_baseline()?;
        enable_required_controllers(&authority.directory, &authority.label, &enabled)?;
        let root = Self { authority };
        root.probe()?;
        Ok(root)
    }

    /// Diagnostic pathname retained only for errors and logs.
    pub fn label(&self) -> &Path {
        &self.authority.label
    }

    /// Revalidate the stable delegation contract without mutating it.
    pub fn probe(&self) -> Result<()> {
        self.authority.probe_baseline()
    }

    /// Consume this delegation to create its one per-derivation leaf.
    pub fn create_leaf(self, identity: &str, limits: CgroupLimits) -> Result<CgroupLeaf> {
        self.probe()?;
        configure_created_leaf(self.create_unconfigured_leaf(identity)?, limits)
    }

    fn create_unconfigured_leaf(self, identity: &str) -> Result<CgroupLeaf> {
        self.create_unconfigured_leaf_with(identity, &mut |_| Ok(()))
    }

    fn create_unconfigured_leaf_with(
        self,
        identity: &str,
        checkpoint: &mut dyn FnMut(CreationStage) -> io::Result<()>,
    ) -> Result<CgroupLeaf> {
        validate_leaf_identity(identity)?;
        let (name, label) = self.create_unique_leaf_directory(identity)?;
        let Self { authority } = self;

        // The sole locked root authority moves into rollback immediately after
        // mkdir. No descriptor allocation or duplication is needed here.
        let mut rollback = ProvisionalLeafRollback::new(authority, name, label);
        let setup = (|| {
            creation_checkpoint(checkpoint, CreationStage::Mkdir, &rollback.label)?;
            let directory = open_control_path(
                &rollback.authority.directory,
                &rollback.name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
            .map_err(|source| descriptor_error("pin newly created cgroup leaf", &rollback.label, source))?;
            creation_checkpoint(checkpoint, CreationStage::Pinned, &rollback.label)?;
            require_directory(&directory, &rollback.label)?;
            let identity_witness = descriptor_identity(&directory, &rollback.label)?;
            rollback.authenticate(identity_witness);
            creation_checkpoint(checkpoint, CreationStage::Witnessed, &rollback.label)?;
            creation_checkpoint(checkpoint, CreationStage::AuthorityTransferred, &rollback.label)?;
            Ok((directory, identity_witness))
        })();

        match setup {
            Ok((directory, identity_witness)) => {
                let (authority, name, label) = rollback.disarm();
                Ok(CgroupLeaf {
                    authority: Some(authority),
                    directory,
                    name,
                    label,
                    identity: identity.to_owned(),
                    identity_witness,
                    active: true,
                    drop_cleanup_enabled: true,
                })
            }
            Err(failure) => Err(rollback.rollback_after(failure)),
        }
    }

    fn create_unique_leaf_directory(&self, identity: &str) -> Result<(CString, PathBuf)> {
        let mut last_collision = None;
        for _ in 0..LEAF_CREATE_ATTEMPTS {
            let name = random_leaf_name(identity)
                .map_err(|source| descriptor_error("generate unpredictable cgroup leaf name", self.label(), source))?;
            let label = self.label().join(os_str(&name));

            // SAFETY: directory and name remain live and mode is valid.
            if unsafe { libc::mkdirat(self.authority.directory.as_raw_fd(), name.as_ptr(), 0o700) } == -1 {
                let source = io::Error::last_os_error();
                if source.kind() == io::ErrorKind::AlreadyExists {
                    last_collision = Some(source);
                    continue;
                }
                return Err(descriptor_error(
                    "create cgroup leaf without replacement",
                    &label,
                    source,
                ));
            }
            return Ok((name, label));
        }

        Err(descriptor_error(
            "create unique unpredictable cgroup leaf",
            self.label(),
            last_collision.unwrap_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "leaf-name collision")),
        ))
    }

    #[cfg(test)]
    fn simulated(directory: &File, label: PathBuf) -> Self {
        Self {
            authority: DelegationAuthority {
                directory: duplicate_cloexec(directory).unwrap(),
                label,
                topology: DelegationTopology::Simulated,
            },
        }
    }
}

fn configure_created_leaf(mut leaf: CgroupLeaf, limits: CgroupLimits) -> Result<CgroupLeaf> {
    if let Err(failure) = leaf.configure(limits).and_then(|()| leaf.probe_ready_topology()) {
        leaf.drop_cleanup_enabled = false;
        return match leaf.remove_authenticated() {
            Ok(()) => match leaf.probe_cleanup_baseline() {
                Ok(()) => Err(failure),
                Err(cleanup) => Err(CgroupError::CleanupAfterFailure {
                    failure: Box::new(failure),
                    cleanup: Box::new(cleanup),
                }),
            },
            Err(cleanup) => match leaf.into_recovery() {
                Ok(recovery) => Err(CgroupError::CleanupRecovery {
                    failure: Box::new(failure),
                    cleanup: Box::new(cleanup),
                    recovery: Box::new(recovery),
                }),
                Err(authority) => Err(CgroupError::CleanupAfterFailure {
                    failure: Box::new(CgroupError::CleanupAfterFailure {
                        failure: Box::new(failure),
                        cleanup: Box::new(cleanup),
                    }),
                    cleanup: Box::new(authority),
                }),
            },
        };
    }
    Ok(leaf)
}
