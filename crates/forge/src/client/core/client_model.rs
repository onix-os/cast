/// A Client is a connection to the underlying package management systems
pub struct Client {
    /// Combined set of data sources for current state and potential packages
    registry: Registry,
    /// All installed packages across all states
    install_db: db::meta::Database,
    /// All States
    state_db: db::state::Database,
    /// All layouts for all packages
    layout_db: db::layout::Database,
    /// Runtime configuration for Cast's package manager
    config: Option<config::Manager>,
    /// All of our configured repositories, to seed the [`crate::registry::Registry`]
    repositories: repository::Manager,
    /// Operational scope (real systems, ephemeral, etc)
    scope: Scope,
    /// Root and namespace locks that we operate on. This field is deliberately
    /// last so every SQLite connection and repository handle closes before the
    /// retained mutable namespace and its global lock are released.
    installation: Installation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatefulTransitionCheckpoint {
    AfterTransactionTriggers,
    BeforeUsrExchange,
    AfterUsrExchange,
    AfterSystemTriggersStarted,
    AfterSystemTriggers,
    BeforePreviousStateArchive,
    AfterPreviousStateArchive,
    BeforeCandidateBootSynchronization,
    AfterCandidateBootSynchronizationStarted,
    BeforeRecoveryPreviousStateRestore,
    BeforeRecoveryUsrExchange,
    BeforeRecoveryCandidatePreservation,
    BeforeRecoveryCandidateInvalidation,
    BeforeRecoveryBootSynchronization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatefulCandidateOrigin {
    Fresh,
    Archived,
    ActiveReblit,
}

/// One ephemeral filesystem candidate plus the process-local writer lease
/// held from destructive materialization through metadata and trigger work.
struct EphemeralCandidate {
    tree: vfs::Tree<PendingFile>,
    root: PathBuf,
    target: RetainedExternalMaterializationTarget,
    candidate_usr: candidate_metadata::RetainedEphemeralUsr,
    active_state: active_state_snapshot::ActiveStateLease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviousUsrLocation {
    Staging,
    Archived(state::Id),
}

#[derive(Debug, Default)]
struct StatefulRecoveryFailures {
    previous_archive_cleanup: Option<Box<Error>>,
    restore_previous: Option<Box<Error>>,
    reverse_exchange: Option<Box<Error>>,
    preserve_candidate: Option<Box<Error>>,
    invalidate_candidate: Option<Box<Error>>,
    repair_boot: Option<Box<Error>>,
}

impl StatefulRecoveryFailures {
    fn is_empty(&self) -> bool {
        self.previous_archive_cleanup.is_none()
            && self.restore_previous.is_none()
            && self.reverse_exchange.is_none()
            && self.preserve_candidate.is_none()
            && self.invalidate_candidate.is_none()
            && self.repair_boot.is_none()
    }
}

/// One executable path that must be supplied by one exact package in a
/// materialized frozen closure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrozenExecutableBinding {
    pub package: package::Id,
    pub path: PathBuf,
}

/// The exact directory inode published by one frozen-root materialization.
///
/// The descriptor is opened while the root still has its private staging name
/// and is retained across the atomic publication rename.  Consequently this
/// value is provenance for the materialized inode, not merely for the pathname
/// at which it was published.  It is deliberately non-cloneable and must be
/// consumed by [`Client::require_materialized_frozen_executables`] to issue an
/// activation guard.
#[derive(Debug)]
#[must_use = "the materialized-root token must be retained through root preparation and verification"]
pub struct MaterializedFrozenRoot {
    root_path: PathBuf,
    root: fs::File,
    identity: FrozenRootIdentity,
}

impl MaterializedFrozenRoot {
    /// The destination at which the retained inode was published.
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// Revalidate that the public destination still names the retained inode.
    pub fn revalidate(&self) -> Result<(), Error> {
        require_materialized_frozen_root(&self.root_path, &self.root, self.identity)
    }

    /// Revalidate the public name, then borrow the exact staged-root
    /// descriptor for immediate descriptor-relative preparation.
    pub fn revalidated_anchor(&self) -> Result<BorrowedFd<'_>, Error> {
        self.revalidate()?;
        Ok(self.root.as_fd())
    }

    fn into_guard_root(self) -> Result<(PathBuf, fs::File, FrozenExecutableWitness), Error> {
        self.revalidate()?;
        let root_witness = frozen_root_anchor_witness(&self.root, &self.root_path)?;
        Ok((self.root_path, self.root, root_witness))
    }
}

/// Timings plus the non-cloneable inode proof produced by frozen
/// materialization.
#[must_use = "frozen materialization returns an inode token required for activation"]
pub struct FrozenMaterialization {
    pub timing: install::Timing,
    pub root: MaterializedFrozenRoot,
}

impl FrozenMaterialization {
    pub fn into_parts(self) -> (install::Timing, MaterializedFrozenRoot) {
        (self.timing, self.root)
    }
}

/// A retained proof for revalidating that one exact frozen root and every
/// executable required from it still name the inodes verified by
/// [`Client::require_frozen_executables`].
///
/// The guard is deliberately not cloneable. It retains the root, executable,
/// interpreter, symlink, and root-ABI descriptors until container activation.
/// Call [`Self::revalidated_anchor`] immediately before constructing the
/// container; the returned descriptor is borrowed from this guard and cannot
/// outlive it through the safe API.
#[derive(Debug)]
#[must_use = "dropping the frozen-root guard discards the executable proof"]
pub struct FrozenRootGuard {
    root_path: PathBuf,
    root: fs::File,
    root_witness: FrozenExecutableWitness,
    executables: Vec<PinnedFrozenExecutable>,
    root_aliases: BTreeMap<PathBuf, PinnedFrozenRootAlias>,
}

impl FrozenRootGuard {
    /// The authenticated pathname whose current name is checked during every
    /// revalidation. Activation itself must use [`Self::revalidated_anchor`],
    /// not reopen this path.
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// Revalidate the complete retained proof under a fresh finite deadline and
    /// borrow the exact root descriptor for immediate container activation.
    pub fn revalidated_anchor(&self) -> Result<BorrowedFd<'_>, Error> {
        self.revalidate()?;
        Ok(self.root.as_fd())
    }

    /// Revalidate the root name and every retained executable, interpreter,
    /// symlink, and root-ABI alias under a fresh finite deadline.
    pub fn revalidate(&self) -> Result<(), Error> {
        let deadline = Instant::now() + FROZEN_EXECUTABLE_VERIFICATION_TIMEOUT;
        self.revalidate_until(deadline)
    }

    fn revalidate_until(&self, deadline: Instant) -> Result<(), Error> {
        require_frozen_executable_deadline(deadline)?;
        require_pinned_frozen_root_anchor(&self.root_path, &self.root, self.root_witness)?;
        for executable in &self.executables {
            require_frozen_executable_deadline(deadline)?;
            require_pinned_frozen_executable(&self.root, executable)?;
        }
        for alias in self.root_aliases.values() {
            require_frozen_executable_deadline(deadline)?;
            require_pinned_frozen_root_alias(&self.root, alias)?;
        }
        require_frozen_executable_deadline(deadline)?;
        require_pinned_frozen_root_anchor(&self.root_path, &self.root, self.root_witness)
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_REGISTRY_SNAPSHOT_ACQUISITION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_before_registry_snapshot_acquisition(hook: impl FnOnce() + 'static) {
    BEFORE_REGISTRY_SNAPSHOT_ACQUISITION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_registry_snapshot_acquisition() {
    BEFORE_REGISTRY_SNAPSHOT_ACQUISITION.with(|slot| {
        let hook = slot.borrow_mut().take();
        if let Some(hook) = hook {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_registry_snapshot_acquisition() {}
