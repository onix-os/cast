// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! The core client implementation for Cast's package manager
//!
//! A [`Client`] needs to be constructed to handle the initialisation of various
//! databases, plugins and data sources to centralise package query and management
//! operations

use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString, OsStr, OsString},
    fmt,
    io::{self, Read},
    mem::MaybeUninit,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::MetadataExt,
        },
    },
    path::{Component as PathComponent, Path, PathBuf},
    ptr::NonNull,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::os::unix::fs::PermissionsExt as _;

use astr::AStr;
use filetime::FileTime;
use fs_err as fs;
use futures_util::{StreamExt, TryStreamExt, stream};
use itertools::Itertools;
use nix::{
    errno::Errno,
    fcntl::{self, OFlag},
    libc::{AT_FDCWD, RENAME_NOREPLACE, SYS_renameat2, syscall},
    sys::stat::{Mode, fchmod, fchmodat, mkdirat},
    unistd::{UnlinkatFlags, linkat, read, symlinkat, unlinkat, write},
};
use postblit::TriggerScope;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use stone::{StoneDecodedPayload, StoneDigestWriterHasher, StonePayloadLayoutFile, StonePayloadLayoutRecord};
use thiserror::Error;
use tracing::{info, info_span, trace};
use tui::{MultiProgress, ProgressBar, ProgressStyle, Styled};
use vfs::tree::{BlitFile, Element, builder::TreeBuilder};

use self::external_materialization::{ExternalMaterializationAdmission, RetainedExternalMaterializationTarget};
use self::install::install;
use self::prune::{prune_cache, prune_states};
use self::remove::remove;
use self::sync::sync;
use self::verify::verify;
use crate::{
    Installation, Package, Provider, Registry, Signal, State, SystemModel,
    client::fetch::fetch,
    db, environment, installation,
    linux_fs::{
        chmod_path_descriptor, chmod_path_descriptor_until, open_path_descriptor_readonly_until, openat2_file,
        openat2_file_until, renameat2_noreplace_until, require_no_access_acl_until, require_no_default_acl,
        require_no_default_acl_until, set_path_descriptor_times_until, sync_filesystem_until,
    },
    package,
    registry::plugin::{self, Plugin},
    repository, runtime, signal,
    state::{self, Selection},
    system_model::{self, LoadedSystemModel},
    transition_identity::{
        ArchivedCandidateError, FailedCandidateKind, QuarantinedCandidate, RetainedArchivedCandidateMoveFailure,
        RetainedArchivedCandidateMoveOutcome, RetainedExchangeFailure, RetainedExchangeOutcome,
        RetainedPreviousMoveFailure, RetainedPreviousMoveOutcome, RetainedStagingWrapperRotationFailure,
        RetainedStagingWrapperRotationOutcome, StatefulTreeIdentity,
    },
};

pub use self::extract::extract;
pub use self::index::index;
pub use self::read_only::{ReadOnlyClient, ReadOnlyClientError};
pub use self::resolve::{AvailableClosure, Error as ResolveError, ResolvedPackage, ResolvedRequest};
pub use self::self_upgrade::self_upgrade;

#[cfg(test)]
mod active_reblit_tests;
mod active_state_authority;
#[cfg(test)]
mod active_state_authority_tests;
mod active_state_snapshot;
#[cfg(test)]
mod active_state_snapshot_tests;
mod archived_repair;
mod archived_repair_materialization;
#[cfg(test)]
mod archived_repair_tests;
mod boot;
mod cache;
mod candidate_metadata;
mod external_materialization;
mod fetch;
mod fixed_staging;
mod install;
mod postblit;
mod read_only;
mod remove;
mod resolve;
mod self_upgrade;
mod startup_gate;
#[cfg(test)]
mod startup_gate_tests;
mod sync;
mod transaction_root;
mod verify;

pub mod extract;
pub mod index;
pub mod prune;

/// A builder for [`Client`]
pub struct ClientBuilder {
    client_name: String,
    installation: Installation,
    repositories: Option<repository::Map>,
    system_intent_path: Option<PathBuf>,
    system_intent_notice: Option<bool>,
    blit_root: Option<PathBuf>,
}

impl ClientBuilder {
    /// Set the repositories
    pub fn repositories(mut self, repositories: repository::Map) -> ClientBuilder {
        self.repositories = Some(repositories);
        self
    }

    /// Import user-authored Gluon system intent from the provided path.
    pub fn system_intent_path(mut self, path: impl Into<PathBuf>) -> ClientBuilder {
        self.system_intent_path = Some(path.into());
        self
    }

    /// Emit the interactive declarative-intent notice only after a complete,
    /// successful client build. Library callers remain silent by default.
    pub(crate) fn system_intent_notice(mut self, verbose: bool) -> ClientBuilder {
        self.system_intent_notice = Some(verbose);
        self
    }

    /// Set the client to an ephemeral client that doesn't record state changes
    /// and blits to a different root.
    ///
    /// This is useful for installing a root to a container (for example, Mason) while
    /// using a shared cache.
    ///
    /// Returns an error on construction if `blit_root` is the same as the installation
    /// root, since the system client should always be stateful.
    pub fn ephemeral(mut self, blit_root: impl Into<PathBuf>) -> ClientBuilder {
        self.blit_root = Some(blit_root.into());
        self
    }

    /// Build the [`Client`]
    pub fn build(mut self) -> Result<Client, Error> {
        // A system or ephemeral Client owns mutable databases, the startup
        // coordinator, and transition journals. Reject every non-mutable
        // installation mode before acquiring any of that authority. In
        // particular, an explicit read-only snapshot must never become a
        // mutable client merely because its underlying root is writable.
        if !self.installation.is_mutable_system() {
            return Err(Error::SystemInstallationRequired);
        }

        // Preserve the lock order used by every transition: cooperating-writer
        // coordinator first, retained journal lock second. Strict live-state
        // discovery is deliberately deferred until the databases, journal,
        // and orphan evidence have been inspected, but taking the coordinator
        // only after the journal would introduce an ABBA deadlock with an
        // in-flight transition.
        let active_state_reservation = active_state_snapshot::ActiveStateReservation::acquire()?;
        let install_db = db::meta::Database::new(self.installation.db_path("install").to_str().unwrap_or_default())?;
        let state_db = db::state::Database::new(self.installation.db_path("state").to_str().unwrap_or_default())?;
        let layout_db = db::layout::Database::new(self.installation.db_path("layout").to_str().unwrap_or_default())?;

        let startup_gate =
            startup_gate::CleanSystemStartup::enter(&self.installation, &state_db).map_err(|source| {
                Error::SystemStartupGate {
                    source: Box::new(source),
                }
            })?;
        let active_state = active_state_reservation.discover_after_startup_gate(&self.installation, &startup_gate)?;

        active_state.revalidate(&self.installation)?;
        if let Some(path) = self.system_intent_path {
            self.installation.system_model =
                Some(system_model::load(&path)?.ok_or(Error::ImportSystemIntentDoesntExist(path.to_owned()))?);
        } else {
            self.installation.system_model = startup_gate
                .load_default_system_intent(&self.installation, &active_state)
                .map_err(|source| Error::SystemStartupGate {
                    source: Box::new(source),
                })?;
        }
        active_state.revalidate(&self.installation)?;

        let config = config::Manager::system(&self.installation.root, "cast");
        let repositories = if let Some(repos) = self.repositories {
            repository::Manager::with_explicit(&self.client_name, repos, self.installation.clone())?
        } else if let Some(system_model) = &self.installation.system_model {
            repository::Manager::with_system_model(&self.client_name, system_model.clone(), self.installation.clone())?
        } else {
            repository::Manager::with_config_manager(config.clone(), self.installation.clone())?
        };

        let registry = build_registry(active_state.active(), &repositories, &install_db, &state_db)?;
        active_state.revalidate(&self.installation)?;
        drop(startup_gate);
        drop(active_state);

        let mut client = Client {
            config: Some(config),
            installation: self.installation,
            repositories,
            registry,
            install_db,
            state_db,
            layout_db,
            scope: Scope::Stateful,
        };

        if let Some(blit_root) = self.blit_root {
            client = client.ephemeral(blit_root)?;
        }
        if let Some(verbose) = self.system_intent_notice {
            print_system_intent_notice(&client, verbose);
        }
        Ok(client)
    }
}

fn print_system_intent_notice(client: &Client, verbose: bool) {
    if let Some(notice) = render_system_intent_notice(client, verbose) {
        emit_system_intent_notice(notice);
    }
}

fn render_system_intent_notice(client: &Client, verbose: bool) -> Option<String> {
    let Some(system_model) = client.system_intent() else {
        return None;
    };
    if system_model.disable_warning && !verbose {
        return None;
    }
    let path = system_model.path();
    let first_line = format!(
        "{}: authored Gluon system intent at {path:?} is active.",
        "INFO".green()
    );
    if system_model.disable_warning {
        return Some(first_line);
    }

    Some(format!(
        "{first_line}
Hence:
- This system intent is the source of truth and defines all
  repositories & installed packages.
- Any changes made via `cast` commands will be temporary
  until the authored intent is updated.
- The system state can be reverted to match the declared intent
  by doing a `cast sync`.
- Each state stores a generated `/usr/lib/system-model.glu` snapshot;
  it is not the authored source and should not be edited.
- To disable declarative system intent, remove or rename {path:?}.",
    ))
}

#[cfg(not(test))]
fn emit_system_intent_notice(notice: String) {
    eprintln!("{notice}");
}

#[cfg(test)]
std::thread_local! {
    static SYSTEM_INTENT_NOTICE_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce(String)>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_system_intent_notice_capture(capture: impl FnOnce(String) + 'static) {
    SYSTEM_INTENT_NOTICE_CAPTURE.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(capture)).is_none());
    });
}

#[cfg(test)]
fn disarm_system_intent_notice_capture() -> bool {
    SYSTEM_INTENT_NOTICE_CAPTURE.with(|slot| slot.borrow_mut().take().is_some())
}

#[cfg(test)]
fn emit_system_intent_notice(notice: String) {
    let capture = SYSTEM_INTENT_NOTICE_CAPTURE.with(|slot| slot.borrow_mut().take());
    if let Some(capture) = capture {
        capture(notice);
    } else {
        eprintln!("{notice}");
    }
}

/// A Client is a connection to the underlying package management systems
pub struct Client {
    /// Root that we operate on
    installation: Installation,
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

impl Client {
    /// Construct a new ClientBuilder for the given [`Installation`]
    pub fn builder(client_name: impl ToString, installation: Installation) -> ClientBuilder {
        ClientBuilder {
            client_name: client_name.to_string(),
            installation,
            repositories: None,
            system_intent_path: None,
            system_intent_notice: None,
            blit_root: None,
        }
    }

    /// Construct a CLI client whose declarative-intent notice is emitted only
    /// after the startup gate, strict discovery, intent evaluation,
    /// repositories, and registry all succeed.
    pub(crate) fn for_cli(
        client_name: impl ToString,
        installation: Installation,
        verbose: bool,
    ) -> Result<Client, Error> {
        Self::builder(client_name, installation)
            .system_intent_notice(verbose)
            .build()
    }

    pub(crate) fn cli_builder(client_name: impl ToString, installation: Installation, verbose: bool) -> ClientBuilder {
        Self::builder(client_name, installation).system_intent_notice(verbose)
    }

    /// Construct a new Client for the given [`Installation`]
    pub fn new(client_name: impl ToString, installation: Installation) -> Result<Client, Error> {
        Self::builder(client_name.to_string(), installation).build()
    }

    pub(crate) fn system_intent(&self) -> Option<&LoadedSystemModel> {
        self.installation.system_model.as_ref()
    }

    pub(crate) fn into_repository_manager(self) -> repository::Manager {
        self.repositories
    }

    /// Construct a cache-only client for one frozen root materialization.
    ///
    /// The installation must have been opened with [`Installation::open_frozen`].
    /// Only the supplied active repositories are registered: authored system
    /// intent, active-state metadata, local cobble packages, and Cast config
    /// files cannot participate in exact package selection.
    pub fn frozen(
        client_name: impl ToString,
        installation: Installation,
        repositories: repository::Map,
        blit_root: impl Into<PathBuf>,
    ) -> Result<Client, Error> {
        if !installation.is_frozen_cache() {
            return Err(Error::FrozenInstallationRequired);
        }

        let requested_root = blit_root.into();
        let root_name = requested_root
            .file_name()
            .filter(|name| !name.as_bytes().contains(&0))
            .ok_or_else(|| Error::InvalidFrozenRootDestination(requested_root.clone()))?;
        let requested_parent = requested_root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = requested_parent.canonicalize()?;
        if !parent.is_dir() {
            return Err(Error::InvalidFrozenRootDestination(requested_root));
        }
        let destination_name = CString::new(root_name.as_bytes())
            .map_err(|_| Error::InvalidFrozenRootDestination(requested_root.clone()))?;
        let blit_root = require_disjoint_materialization_target(&installation, &parent.join(root_name))?;
        let destination_parent = open_frozen_destination_parent(&parent)?;
        let destination_parent_identity = frozen_root_identity(&destination_parent, &parent)?;

        let install_db = db::meta::Database::new(installation.db_path("install").to_str().unwrap_or_default())?;
        let state_db = db::state::Database::new(":memory:")?;
        let layout_db = db::layout::Database::new(installation.db_path("layout").to_str().unwrap_or_default())?;
        let repositories = repository::Manager::with_explicit(client_name, repositories, installation.clone())?;
        let registry = build_repository_registry(&repositories);

        Ok(Client {
            installation,
            registry,
            install_db,
            state_db,
            layout_db,
            config: None,
            repositories,
            scope: Scope::Frozen {
                destination: FrozenRootDestination {
                    root_path: blit_root,
                    parent_path: parent,
                    name: destination_name,
                    parent: destination_parent,
                    parent_identity: destination_parent_identity,
                },
            },
        })
    }

    /// Returns `true` if this is an ephemeral client
    pub fn is_ephemeral(&self) -> bool {
        self.scope.is_ephemeral()
    }

    fn require_non_frozen(&self) -> Result<(), Error> {
        if matches!(self.scope, Scope::Frozen { .. }) {
            Err(Error::FrozenClientProhibitedOperation)
        } else {
            Ok(())
        }
    }

    fn require_stateful_scope(&self) -> Result<(), Error> {
        match self.scope {
            Scope::Stateful => Ok(()),
            Scope::Ephemeral { .. } => Err(Error::EphemeralProhibitedOperation),
            Scope::Frozen { .. } => Err(Error::FrozenClientProhibitedOperation),
        }
    }

    fn preflight_active_state_snapshot(&self) -> Result<(), Error> {
        if matches!(&self.scope, Scope::Stateful | Scope::Ephemeral { .. }) {
            drop(active_state_snapshot::ActiveStateLease::acquire(&self.installation)?);
        }
        Ok(())
    }

    fn active_state_for_planning(&self) -> Result<Option<state::Id>, Error> {
        match &self.scope {
            Scope::Stateful | Scope::Ephemeral { .. } => {
                let active_state = active_state_snapshot::ActiveStateLease::acquire(&self.installation)?;
                active_state.revalidate(&self.installation)?;
                Ok(active_state.active())
            }
            Scope::Frozen { .. } => Err(Error::FrozenClientProhibitedOperation),
        }
    }

    fn with_registry_snapshot<T, E>(&self, read: impl FnOnce(&Registry) -> Result<T, E>) -> Result<T, E>
    where
        E: From<Error>,
    {
        let active_state = match &self.scope {
            Scope::Frozen { .. } => None,
            Scope::Stateful | Scope::Ephemeral { .. } => {
                before_registry_snapshot_acquisition();
                Some(active_state_snapshot::ActiveStateLease::acquire(&self.installation)?)
            }
        };
        self.preflight_repository_integrity()?;
        let result = read(&self.registry);
        let repository_revalidation = self.preflight_repository_integrity();
        if let Some(active_state) = active_state.as_ref() {
            active_state.revalidate(&self.installation)?;
        }
        repository_revalidation?;
        result
    }

    fn preflight_repository_integrity(&self) -> Result<(), Error> {
        self.repositories
            .preflight_active_snapshots()
            .map_err(Error::Repository)
    }

    /// Perform package installation
    pub fn install(&mut self, packages: &[&str], yes: bool, simulate: bool) -> Result<install::Timing, Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        self.preflight_repository_integrity()?;
        install(self, packages, yes, simulate).map_err(|error| Error::Install(Box::new(error)))
    }

    /// Install an exact, pre-resolved package closure.
    ///
    /// Unlike [`Self::install`], this does not resolve provider requests or
    /// traverse dependency metadata. The supplied package IDs are the complete
    /// closure selected by an external planner.
    pub fn install_exact(
        &mut self,
        packages: &[package::Id],
        yes: bool,
        simulate: bool,
    ) -> Result<install::Timing, Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        self.preflight_repository_integrity()?;
        install::install_exact(self, packages, yes, simulate).map_err(|error| Error::Install(Box::new(error)))
    }

    /// Materialize an exact package closure into a dedicated frozen root.
    ///
    /// Package IDs are treated as a canonical set and sorted by ID. No
    /// provider lookup, dependency traversal, state creation, system-model
    /// generation, triggers, boot synchronization, or isolation-root writes
    /// occur on this path. The resulting root uses independent file inodes and
    /// normalizes content, inode type, mode, atime, and mtime. Kernel-assigned
    /// inode/dev/ctime/btime values are deliberately outside this contract.
    pub fn materialize_frozen_root(
        &mut self,
        packages: &[package::Id],
        source_date_epoch: i64,
    ) -> Result<FrozenMaterialization, Error> {
        install::materialize_frozen_root(self, packages, source_date_epoch)
            .map_err(|error| Error::Install(Box::new(error)))
    }

    /// Explicitly discard the previously published root owned by this frozen
    /// client so a later absent-only materialization can publish a new one.
    ///
    /// The named root is first atomically detached into a private sibling.
    /// Cleanup then remains descriptor-rooted, never follows symlinks or mount
    /// points, restores owner access only on directories being discarded, and
    /// enforces finite entry, depth, and wall-time bounds.
    pub fn discard_frozen_root(&self) -> Result<(), Error> {
        discard_frozen_root_destination(self.frozen_destination()?)
    }

    /// Prove that every frozen executable binding is supplied by its exact
    /// resolved package and names the unchanged regular executable now present
    /// in the materialized root.
    ///
    /// This is deliberately separate from provider resolution. Callers first
    /// materialize the already-locked closure, then invoke this method before
    /// entering the build container. A metadata-only package which advertises
    /// `binary(foo)` but has no `/usr/bin/foo` layout entry therefore fails
    /// closed instead of borrowing an ambient executable from another package.
    /// Only the explicitly supported structural host-ELF subset and scripts
    /// with one strict absolute shebang path are admitted. Every shebang or ELF
    /// `PT_INTERP` target is recursively matched to one closure layout
    /// provider, pinned, fully verified, and rechecked before return. The
    /// returned guard must remain live through activation; callers borrow its
    /// revalidated root anchor immediately before constructing the container.
    pub fn require_materialized_frozen_executables(
        &self,
        materialized_root: MaterializedFrozenRoot,
        packages: &[package::Id],
        bindings: &[FrozenExecutableBinding],
    ) -> Result<FrozenRootGuard, Error> {
        if materialized_root.root_path() != self.frozen_root()? {
            return Err(Error::ForeignMaterializedFrozenRoot {
                expected: self.frozen_root()?.to_owned(),
                found: materialized_root.root_path().to_owned(),
            });
        }
        let packages = self.canonical_frozen_package_ids(packages)?;
        require_frozen_executables(self, materialized_root, &packages, bindings, |_, _| {})
    }

    /// Unit-test adapter for verifier cases which construct their filesystem
    /// fixture directly rather than through the production materializer.
    #[cfg(test)]
    fn require_frozen_executables(
        &self,
        packages: &[package::Id],
        bindings: &[FrozenExecutableBinding],
    ) -> Result<FrozenRootGuard, Error> {
        let root = test_materialized_frozen_root(self.frozen_root()?)?;
        self.require_materialized_frozen_executables(root, packages, bindings)
    }

    /// Perform package removals
    pub fn remove(&mut self, packages: &[&str], yes: bool, simulate: bool) -> Result<remove::Timing, Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        remove(self, packages, yes, simulate).map_err(|error| Error::Remove(Box::new(error)))
    }

    /// Perform package fetches
    pub fn fetch(&mut self, packages: &[&str], output_dir: &Path, verbose: bool) -> Result<fetch::Timing, Error> {
        self.require_non_frozen()?;
        self.preflight_repository_integrity()?;
        fetch(self, packages, output_dir, verbose).map_err(|error| Error::Fetch(Box::new(error)))
    }

    /// Perform a sync
    pub fn sync(&mut self, yes: bool, simulate: bool) -> Result<sync::Timing, Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        self.preflight_repository_integrity()?;
        sync(self, yes, simulate).map_err(|error| Error::Sync(Box::new(error)))
    }

    /// Transition to an ephemeral client that doesn't record state changes
    /// and blits to a different root.
    ///
    /// This is useful for installing a root to a container (for example, Mason) while
    /// using a shared cache.
    ///
    /// Returns an error if the canonical destination is equal to, beneath, or
    /// an ancestor of the installation root. Destructive ephemeral
    /// materialization must be namespace-disjoint from all persistent state.
    pub fn ephemeral(self, blit_root: impl Into<PathBuf>) -> Result<Self, Error> {
        self.require_non_frozen()?;
        let destination = ExternalMaterializationAdmission::admit(&self.installation, &blit_root.into())?;

        Ok(Self {
            scope: Scope::Ephemeral { destination },
            ..self
        })
    }

    /// Ensures all repositories have been initialized by ensuring their stone indexes
    /// are downloaded and added to the meta db
    pub async fn ensure_repos_initialized(&mut self) -> Result<usize, Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        let num_initialized = self.repositories.ensure_all_initialized().await?;
        self.rebuild_registry()?;
        Ok(num_initialized)
    }

    /// Reload all configured repositories and refreshes their index file, then update
    /// registry with all active repositories.
    pub async fn refresh_repositories(&mut self) -> Result<(), Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        // Reload manager if config sourced to pickup config changes
        // then refresh indexes
        if self.repositories.is_config_source() {
            let config = self
                .config
                .clone()
                .expect("config-sourced clients always retain their config manager");
            self.repositories = repository::Manager::with_config_manager(config, self.installation.clone())?;
        };
        self.repositories.refresh_all().await?;

        // Rebuild registry
        self.rebuild_registry()?;

        Ok(())
    }

    fn rebuild_registry(&mut self) -> Result<(), Error> {
        let registry = match &self.scope {
            Scope::Frozen { .. } => build_repository_registry(&self.repositories),
            Scope::Stateful | Scope::Ephemeral { .. } => {
                let active_state = active_state_snapshot::ActiveStateLease::acquire(&self.installation)?;
                let registry = build_registry(
                    active_state.active(),
                    &self.repositories,
                    &self.install_db,
                    &self.state_db,
                )?;
                active_state.revalidate(&self.installation)?;
                registry
            }
        };
        self.registry = registry;
        Ok(())
    }

    pub fn verify(&self, yes: bool, verbose: bool) -> Result<(), Error> {
        self.require_stateful_scope()?;
        verify(self, yes, verbose)?;
        Ok(())
    }

    /// Prune states with the provided [`prune::Strategy`].
    ///
    /// This allows automatic removal of unused states (and their associated assets)
    /// from the disk, acting as a garbage collection facility.
    pub fn prune_states(&self, strategy: prune::Strategy<'_>, yes: bool) -> Result<(), Error> {
        self.require_stateful_scope()?;
        let active_state = active_state_snapshot::ActiveStateLease::acquire(&self.installation)?;

        prune_states(self, strategy, yes, &active_state)?;

        Ok(())
    }

    /// Prune all cached data that isn't related to any states or active repositories.
    ///
    /// This will remove all downloaded stones & unpacked asset data for packages not
    /// in that set.
    pub fn prune_cache(&self) -> Result<usize, Error> {
        self.require_stateful_scope()?;
        let _stateful_coordinator = fixed_staging::lock_coordinator()?;

        prune_cache(
            &self.state_db,
            &self.install_db,
            &self.layout_db,
            &self.installation,
            &self.repositories,
        )
        .map_err(Error::Prune)
    }

    /// Resolves the provided id with the underlying registry, returning the first matching [`Package`]
    pub fn resolve_package(&self, package: &package::Id) -> Result<Package, Error> {
        self.with_registry_snapshot(|registry| {
            registry
                .by_id(package)?
                .into_iter()
                .next()
                .ok_or(Error::MissingMetadata(package.clone()))
        })
    }

    fn resolve_frozen_repository_package(&self, package: &package::Id) -> Result<Package, Error> {
        self.frozen_root()?;
        self.repositories
            .resolve_exact_package(package)?
            .map(|(_, package)| package)
            .ok_or_else(|| Error::MissingMetadata(package.clone()))
    }

    /// Resolves the provided id's with the underlying registry, returning
    /// the first [`Package`] for each id.
    ///
    /// Packages are sorted by name and deduped before returning.
    pub fn resolve_packages<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<Vec<Package>, Error> {
        self.with_registry_snapshot(|registry| {
            let mut metadata = packages
                .into_iter()
                .map(|id| {
                    registry
                        .by_id(id)?
                        .into_iter()
                        .next()
                        .ok_or(Error::MissingMetadata(id.clone()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            metadata.sort_by_key(|p| p.meta.name.to_string());
            metadata.dedup_by_key(|p| p.meta.name.to_string());
            Ok(metadata)
        })
    }

    /// Content identities for all active repository indexes participating in
    /// available-package resolution.
    pub fn repository_index_snapshots(&self) -> Result<Vec<repository::IndexSnapshot>, Error> {
        self.repositories.index_snapshots().map_err(Error::Repository)
    }

    /// Returns all unique packages which provide the supplied [`Provider`]
    pub fn lookup_packages_by_provider(
        &self,
        provider: &Provider,
        flags: package::Flags,
    ) -> Result<Vec<Package>, Error> {
        self.with_registry_snapshot(|registry| {
            Ok(registry
                .by_provider(provider, flags)?
                .into_iter()
                .unique_by(|p| p.id.clone())
                .collect())
        })
    }

    /// Return sorted packages matching the given flags. Repository integrity
    /// failures are first-class and cannot be flattened into an empty list.
    pub fn list_packages(&self, flags: package::Flags) -> Result<Vec<Package>, Error> {
        self.with_registry_snapshot(|registry| registry.list(flags).map_err(Error::from))
    }

    /// Returns all packages with names containing the provided keyword
    /// and match the given flags
    pub fn search_packages(&self, keyword: &str, flags: package::Flags) -> Result<Vec<Package>, Error> {
        self.with_registry_snapshot(|registry| registry.by_keyword(keyword, flags).map_err(Error::from))
    }

    /// Activates the provided state and runs system triggers once applied.
    ///
    /// The current state gets archived only after system triggers complete.
    /// If a later archive or boot synchronization step fails, Cast restores
    /// the previous `/usr`, preserves the failed candidate, and attempts to
    /// repair boot metadata for the restored state. Once candidate boot
    /// synchronization has begun, recovery remains explicitly unverified even
    /// when that compensating synchronization appears to succeed. Arbitrary
    /// side effects already performed by a system trigger are outside that
    /// filesystem recovery.
    ///
    /// Returns the old state that was archived.
    pub fn activate_state(&self, id: state::Id, skip_triggers: bool, skip_boot: bool) -> Result<state::Id, Error> {
        self.require_stateful_scope()?;
        let _guard = signal::ignore([Signal::SIGINT])?;
        let _inhibitor = signal::inhibit(
            vec!["shutdown", "sleep", "idle", "handle-lid-switch"],
            "cast".into(),
            "Activating state".into(),
            "block".into(),
        )?;

        self.activate_state_with_checkpoint(id, skip_triggers, skip_boot, |_| Ok(()))
    }

    fn activate_state_with_checkpoint<F>(
        &self,
        id: state::Id,
        skip_triggers: bool,
        skip_boot: bool,
        mut checkpoint: F,
    ) -> Result<state::Id, Error>
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        self.require_stateful_scope()?;
        let _local_etc = transaction_root::prepare_local_etc(&self.installation)?;
        let mut active_state = active_state_authority::ActiveStateAuthority::acquire(&self.installation)?;
        // Fetch the new state
        let new = self.state_db.get(id).map_err(|_| Error::StateDoesntExist(id))?;

        // Get old (current) state
        let Some(old_id) = active_state.active() else {
            return Err(Error::NoActiveState);
        };

        if new.id == old_id {
            return Err(Error::StateAlreadyActive(id));
        }
        let old = self.state_db.get(old_id)?;

        // Resolve the trigger view before moving either filesystem tree. A
        // database or VFS failure must leave the archived candidate untouched.
        let fstree = self.vfs(new.selections.iter().map(|selection| &selection.package))?;

        // Root ABI conflicts are immutable preflight failures, not reasons to
        // move the archived candidate or exchange the live /usr first. Retain
        // the read-only proof so the same names can be revalidated at the
        // exchange boundary without reopening mutable path authority.
        let live_root_abi = preflight_root_links(&self.installation.root)?;

        let archived_usr = self.installation.root_path(new.id.to_string()).join("usr");
        active_state.revalidate(&self.installation)?;
        let tree_identity = self
            .prepare_stateful_tree_identity(&archived_usr, new.id)
            .map_err(|source| Error::StatefulTreeIdentityPreparationFailed {
                candidate: new.id,
                previous: Some(old.id),
                location: archived_usr.clone(),
                source: Box::new(source.into()),
            })?;
        active_state.refresh_after_tree_identity_preparation(&self.installation)?;
        live_root_abi.revalidate()?;

        // Exchange the exact archived-state wrapper with the fixed staging
        // wrapper. Both inodes remain retained so a racing path replacement
        // is classified instead of overwritten or adopted.
        match tree_identity.stage_archived_candidate(&self.installation, new.id) {
            Ok(()) => {}
            Err(failure) if failure.outcome() == RetainedArchivedCandidateMoveOutcome::Applied => {
                // The exchange already happened. Resume only its idempotent
                // durability suffix; repeating the exchange would undo it.
                if let Err(primary) = tree_identity
                    .finish_applied_archived_candidate_stage(&self.installation, new.id)
                    .map_err(Error::from)
                {
                    return Err(self.preserve_unswapped_candidate(
                        new.id,
                        Some(old.id),
                        StatefulCandidateOrigin::Archived,
                        primary,
                        &tree_identity,
                        &mut checkpoint,
                    ));
                }
            }
            Err(failure) => return Err(failure.into()),
        }
        if let Err(primary) = tree_identity.verify_pre_exchange(
            &self.installation.staging_path("usr"),
            &self.installation.root.join("usr"),
        ) {
            return Err(self.preserve_unswapped_candidate(
                new.id,
                Some(old.id),
                StatefulCandidateOrigin::Archived,
                primary.into(),
                &tree_identity,
                &mut checkpoint,
            ));
        }

        self.commit_stateful_staging(
            &fstree,
            &new,
            Some(&old),
            StatefulCandidateOrigin::Archived,
            true,
            !skip_triggers,
            !skip_boot,
            &tree_identity,
            None,
            live_root_abi,
            &active_state,
            &mut checkpoint,
        )?;

        Ok(old_id)
    }

    /// Create a new recorded state from the provided packages
    /// provided packages and write that state ID to the installation
    /// Then blit the filesystem, promote it, finally archiving the active ID
    ///
    /// Returns `None` if the client is ephemeral
    pub fn new_state(&self, selections: &[Selection], summary: impl ToString) -> Result<Option<State>, Error> {
        self.require_non_frozen()?;
        let _guard = signal::ignore([Signal::SIGINT])?;
        let _inhibitor = signal::inhibit(
            vec!["shutdown", "sleep", "idle", "handle-lid-switch"],
            "cast".into(),
            "Applying new state".into(),
            "block".into(),
        )?;

        let explicit_packages =
            self.resolve_packages(selections.iter().filter_map(|s| s.explicit.then_some(&s.package)))?;
        let system_snapshot = generate_system_snapshot(
            self.installation.system_model.clone(),
            &self.repositories,
            &explicit_packages,
        )?;

        let timer = Instant::now();

        let state_span = info_span!(
            "progress",
            phase = summary.to_string().to_lowercase(),
            event_type = "progress"
        );
        let _state_guard = state_span.enter();
        info!(
            total_items = selections.len(),
            progress = 0.0,
            event_type = "progress_start",
        );

        let result = match &self.scope {
            Scope::Stateful => {
                // The non-cloneable candidate retains the authenticated
                // staging wrapper and the sole cooperating-writer lease from
                // its first possible mutation through row allocation and
                // durable tree-identity preparation.
                let candidate = self.materialize_stateful_candidate(selections.iter().map(|s| &s.package))?;
                let old_state = candidate.active_state.active();

                // Add to db
                candidate.active_state.revalidate(&self.installation)?;
                let state = self.state_db.add(selections, Some(&summary.to_string()), None)?;

                self.apply_stateful_candidate(candidate, &state, old_state, system_snapshot)?;

                Ok(Some(state))
            }
            Scope::Ephemeral { destination } => {
                let candidate = self.materialize_ephemeral_candidate(selections.iter().map(|s| &s.package))?;
                debug_assert_eq!(candidate.root, destination.path());
                self.apply_ephemeral_candidate(candidate, system_snapshot)?;

                Ok(None)
            }
            Scope::Frozen { .. } => unreachable!("frozen scope rejected before state creation"),
        };

        info!(
            duration_ms = timer.elapsed().as_millis(),
            items_processed = selections.len(),
            progress = 1.0,
            event_type = "progress_completed",
        );

        result
    }

    /// Apply all triggers with the given scope, wrapping with a progressbar.
    fn apply_triggers(scope: TriggerScope<'_>, fstree: &vfs::Tree<PendingFile>) -> Result<(), postblit::Error> {
        #[cfg(test)]
        observe_trigger_scope(&scope);
        let triggers = postblit::triggers(scope, fstree)?;

        let progress = ProgressBar::new(triggers.len() as u64).with_style(
            ProgressStyle::with_template("\n|{bar:20.green/blue}| {pos}/{len} {msg}")
                .unwrap()
                .progress_chars("■≡=- "),
        );

        let phase_name = match &scope {
            TriggerScope::Transaction(..) => {
                progress.set_message("Running transaction-scope triggers");
                "transaction-scope-triggers"
            }
            TriggerScope::RetainedTransaction {
                kind: postblit::RetainedTransactionKind::Stateful,
                ..
            } => {
                progress.set_message("Running transaction-scope triggers");
                "transaction-scope-triggers"
            }
            TriggerScope::RetainedTransaction {
                kind: postblit::RetainedTransactionKind::ArchivedRepair,
                ..
            } => {
                progress.set_message("Running retained transaction-scope triggers");
                "retained-transaction-scope-triggers"
            }
            TriggerScope::RetainedEphemeral {
                phase: postblit::RetainedEphemeralPhase::Transaction,
                ..
            } => {
                progress.set_message("Running retained ephemeral transaction-scope triggers");
                "retained-ephemeral-transaction-scope-triggers"
            }
            TriggerScope::RetainedEphemeral {
                phase: postblit::RetainedEphemeralPhase::System,
                ..
            } => {
                progress.set_message("Running retained ephemeral system-scope triggers");
                "retained-ephemeral-system-scope-triggers"
            }
            TriggerScope::System(..) => {
                progress.set_message("Running system-scope triggers");
                "system-scope-triggers"
            }
        };

        let timer = Instant::now();

        info!(
            phase = phase_name,
            total_items = triggers.len(),
            progress = 0.0,
            event_type = "progress_start",
        );

        for (i, trigger) in progress.wrap_iter(triggers.iter()).enumerate() {
            trigger.execute()?;

            info!(
                progress = (i + 1) as f32 / triggers.len() as f32,
                current = i + 1,
                total = triggers.len(),
                event_type = "progress_update",
                "Executing `{}`",
                trigger.handler()
            );
        }

        info!(
            phase = phase_name,
            duration_ms = timer.elapsed().as_millis(),
            items_processed = triggers.len(),
            progress = 1.0,
            event_type = "progress_completed",
        );

        progress.finish_and_clear();

        Ok(())
    }

    pub fn apply_stateful_blit(
        &self,
        _fstree: vfs::Tree<PendingFile>,
        _state: &State,
        _old_state: Option<state::Id>,
        _system_snapshot: SystemModel,
    ) -> Result<(), Error> {
        Err(Error::FixedStagingCapabilityRequired {
            operation: "apply a stateful blit",
        })
    }

    fn apply_stateful_candidate(
        &self,
        candidate: fixed_staging::StatefulCandidate,
        state: &State,
        old_state: Option<state::Id>,
        system_snapshot: SystemModel,
    ) -> Result<(), Error> {
        let fixed_staging::StatefulCandidate {
            tree,
            staging,
            candidate_usr,
            local_etc,
            mut active_state,
        } = candidate;
        self.apply_stateful_blit_with_capability(
            tree,
            Some((&staging, &candidate_usr)),
            local_etc,
            state,
            old_state,
            &mut active_state,
            system_snapshot,
            |_| Ok(()),
        )
    }

    #[cfg(test)]
    fn apply_stateful_blit_with_checkpoint<F>(
        &self,
        fstree: vfs::Tree<PendingFile>,
        state: &State,
        old_state: Option<state::Id>,
        system_snapshot: SystemModel,
        checkpoint: F,
    ) -> Result<(), Error>
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        let local_etc = transaction_root::prepare_local_etc(&self.installation)?;
        let mut active_state = active_state_authority::ActiveStateAuthority::acquire(&self.installation)?;
        self.apply_stateful_blit_with_capability(
            fstree,
            None,
            local_etc,
            state,
            old_state,
            &mut active_state,
            system_snapshot,
            checkpoint,
        )
    }

    fn apply_stateful_blit_with_capability<F>(
        &self,
        fstree: vfs::Tree<PendingFile>,
        retained_staging: Option<(&fixed_staging::RetainedFixedStaging, &std::fs::File)>,
        local_etc: transaction_root::RetainedLocalEtc,
        state: &State,
        old_state: Option<state::Id>,
        active_state: &mut active_state_authority::ActiveStateAuthority,
        system_snapshot: SystemModel,
        mut checkpoint: F,
    ) -> Result<(), Error>
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        self.require_non_frozen()?;
        // Complete preflight before candidate identity preparation or any
        // trigger. A static conflict therefore leaves the staged candidate and
        // its database row available for inspection or an exact retry.
        let live_root_abi = preflight_root_links(&self.installation.root)?;
        let archive_previous = old_state.is_some();
        let candidate_usr = self.installation.staging_path("usr");
        // Empty package selections deliberately materialize no filesystem
        // root. Record the state identity first so the hardened metadata path
        // creates and authenticates the candidate `/usr` before the tree
        // marker guard attempts to pin it. This also covers remove-last-package
        // and empty active-state verification reblits.
        revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;
        let retained_usr = match retained_staging {
            Some((staging, candidate_usr)) => {
                record_state_id_retained(staging, candidate_usr, state.id)?;
                Some(candidate_usr)
            }
            None => {
                record_state_id(&self.installation.staging_dir(), state.id)?;
                None
            }
        };
        revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;
        active_state.revalidate(&self.installation)?;
        let captured_active_state = active_state.active();
        let prepared_identity = match retained_usr.as_ref() {
            Some(candidate) => self.prepare_stateful_tree_identity_retained(&candidate_usr, candidate, state.id),
            None => self.prepare_stateful_tree_identity(&candidate_usr, state.id),
        };
        let tree_identity = prepared_identity.map_err(|source| Error::StatefulTreeIdentityPreparationFailed {
            candidate: state.id,
            previous: old_state,
            location: candidate_usr,
            source: Box::new(source.into()),
        })?;
        active_state.refresh_after_tree_identity_preparation(&self.installation)?;
        revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;
        let (previous, candidate_origin) = match old_state {
            Some(id) => match self.state_db.get(id) {
                Ok(previous) => (Some(previous), StatefulCandidateOrigin::Fresh),
                Err(error) => {
                    return Err(self.preserve_unswapped_candidate(
                        state.id,
                        Some(id),
                        StatefulCandidateOrigin::Fresh,
                        Error::Db(error),
                        &tree_identity,
                        &mut checkpoint,
                    ));
                }
            },
            // Active-state verification reblits the same state and deliberately
            // does not archive the replaced corrupt tree on success. It still
            // needs the state value for boot repair if recovery reverses the
            // exchange.
            None if captured_active_state == Some(state.id) => {
                (Some(state.clone()), StatefulCandidateOrigin::ActiveReblit)
            }
            None => (None, StatefulCandidateOrigin::Fresh),
        };

        if candidate_origin == StatefulCandidateOrigin::ActiveReblit
            && let Err(primary) = tree_identity
                .prepare_active_reblit_staging_rotation(&self.installation, &self.state_db, state)
                .map_err(Error::from)
        {
            return Err(self.preserve_unswapped_candidate(
                state.id,
                previous.as_ref().map(|state| state.id),
                candidate_origin,
                primary,
                &tree_identity,
                &mut checkpoint,
            ));
        }

        let prepare = (|| {
            #[cfg(test)]
            before_stateful_candidate_metadata();
            revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;
            active_state.revalidate(&self.installation)?;
            tree_identity.verify_candidate_for_activation(&self.installation.staging_path("usr"))?;
            let metadata = candidate_metadata::decorate_stateful(&tree_identity, &system_snapshot)?;
            metadata.revalidate()?;
            tree_identity.verify_candidate_for_activation(&self.installation.staging_path("usr"))?;
            revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;

            let isolation_root = create_root_links(&self.installation.isolation_dir())?;
            #[cfg(test)]
            after_stateful_isolation_root_retention();

            // The container running triggers receives this exact retained
            // local /etc inode rather than resolving its mutable pathname.
            local_etc.revalidate(&self.installation)?;

            // Transaction triggers run before `/usr` is exchanged. Their
            // arbitrary external side effects cannot be undone, but the
            // candidate tree can still be preserved outside the active root.
            active_state.revalidate(&self.installation)?;
            tree_identity.verify_pre_exchange(
                &self.installation.staging_path("usr"),
                &self.installation.root.join("usr"),
            )?;
            live_root_abi.revalidate()?;
            match retained_usr {
                Some(candidate_usr) => Self::apply_triggers(
                    TriggerScope::RetainedTransaction {
                        kind: postblit::RetainedTransactionKind::Stateful,
                        installation: &self.installation,
                        isolation_root: &isolation_root,
                        local_etc: &local_etc,
                        candidate_usr,
                        candidate_usr_path: &self.installation.staging_path("usr"),
                    },
                    &fstree,
                )?,
                // Only unit-test adapters construct a stateful candidate
                // without the production retained fixed-staging capability.
                None => Self::apply_triggers(TriggerScope::Transaction(&self.installation), &fstree)?,
            }
            tree_identity.verify_pre_exchange(
                &self.installation.staging_path("usr"),
                &self.installation.root.join("usr"),
            )?;
            tree_identity.verify_candidate_for_activation(&self.installation.staging_path("usr"))?;
            metadata.revalidate()?;
            revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;
            active_state.revalidate(&self.installation)?;
            if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                tree_identity.verify_active_reblit_candidate_snapshot(
                    &self.installation,
                    &self.state_db,
                    state,
                    false,
                )?;
            }
            checkpoint(StatefulTransitionCheckpoint::AfterTransactionTriggers)?;
            Ok::<_, Error>(metadata)
        })();

        let metadata = match prepare {
            Ok(metadata) => metadata,
            Err(primary) => {
                return Err(self.preserve_unswapped_candidate(
                    state.id,
                    previous.as_ref().map(|state| state.id),
                    candidate_origin,
                    primary,
                    &tree_identity,
                    &mut checkpoint,
                ));
            }
        };

        self.commit_stateful_staging(
            &fstree,
            state,
            previous.as_ref(),
            candidate_origin,
            archive_previous,
            true,
            true,
            &tree_identity,
            Some(&metadata),
            live_root_abi,
            active_state,
            &mut checkpoint,
        )
    }

    /// Commit a completely prepared staging `/usr` and keep the prior tree
    /// recoverable until system triggers have succeeded.
    ///
    /// The prior state is archived before candidate boot synchronization so
    /// `boot::synchronize` can still enumerate it as an immediate rollback
    /// entry. A failure after that archive first moves it back to staging,
    /// reverses the same atomic exchange, preserves the failed candidate, and
    /// attempts to repair boot metadata for the restored state. A candidate
    /// boot failure remains a structured incomplete recovery because the boot
    /// backend cannot prove that partial candidate metadata was removed. This
    /// does not claim to reverse arbitrary side effects performed by a trigger.
    fn commit_stateful_staging<F>(
        &self,
        fstree: &vfs::Tree<PendingFile>,
        candidate: &State,
        previous: Option<&State>,
        candidate_origin: StatefulCandidateOrigin,
        archive_previous: bool,
        run_system_triggers: bool,
        run_boot_synchronization: bool,
        tree_identity: &StatefulTreeIdentity,
        metadata: Option<&candidate_metadata::CandidateMetadataProof<'_>>,
        live_root_abi: RootAbiPreflight,
        active_state: &active_state_authority::ActiveStateAuthority,
        checkpoint: &mut F,
    ) -> Result<(), Error>
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        // Preserve the production guard that historically lived in
        // `promote_staging`: an ephemeral client must never reach the
        // stateful `/usr` exchange, even if a future caller bypasses the
        // ordinary public entry-point checks.
        if self.scope.is_ephemeral() {
            return Err(Error::EphemeralProhibitedOperation);
        }

        if candidate_origin != StatefulCandidateOrigin::Archived && metadata.is_none() {
            return Err(self.preserve_unswapped_candidate(
                candidate.id,
                previous.map(|state| state.id),
                candidate_origin,
                Error::StatefulCandidateMetadataProofRequired {
                    candidate: candidate.id,
                },
                tree_identity,
                checkpoint,
            ));
        }

        if let Err(primary) = tree_identity
            .verify_pre_exchange(
                &self.installation.staging_path("usr"),
                &self.installation.root.join("usr"),
            )
            .map_err(Error::from)
            .and_then(|()| {
                tree_identity
                    .verify_candidate_for_activation(&self.installation.staging_path("usr"))
                    .map_err(Error::from)
            })
            .and_then(|()| metadata.map_or(Ok(()), |proof| proof.revalidate()))
            .and_then(|()| active_state.revalidate(&self.installation))
            .and_then(|()| live_root_abi.revalidate())
            .and_then(|()| {
                if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                    tree_identity
                        .verify_active_reblit_candidate_snapshot(&self.installation, &self.state_db, candidate, false)
                        .map_err(Error::from)
                } else {
                    Ok(())
                }
            })
            .and_then(|()| checkpoint(StatefulTransitionCheckpoint::BeforeUsrExchange))
        {
            return Err(self.preserve_unswapped_candidate(
                candidate.id,
                previous.map(|state| state.id),
                candidate_origin,
                primary,
                tree_identity,
                checkpoint,
            ));
        }

        let promotion = tree_identity.exchange_forward_validated(&self.installation, &|| {
            tree_identity.verify_candidate_for_activation(&self.installation.staging_path("usr"))?;
            if let Some(metadata) = metadata {
                metadata
                    .revalidate()
                    .map_err(|source| crate::transition_identity::Error::RetainedExchange {
                        operation: "revalidate retained candidate metadata immediately before forward exchange",
                        path: metadata.diagnostic_path().to_owned(),
                        source: io::Error::other(source),
                    })?;
            }
            active_state.revalidate(&self.installation).map_err(|source| {
                crate::transition_identity::Error::RetainedExchange {
                    operation: "revalidate active-state authority immediately before forward exchange",
                    path: self.installation.root.join("usr/.stateID"),
                    source: io::Error::other(source),
                }
            })?;
            live_root_abi
                .revalidate()
                .map_err(|source| crate::transition_identity::Error::RetainedExchange {
                    operation: "revalidate retained root ABI immediately before forward exchange",
                    path: live_root_abi.path().to_owned(),
                    source: io::Error::other(source),
                })?;
            if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                tree_identity.verify_active_reblit_candidate_snapshot(
                    &self.installation,
                    &self.state_db,
                    candidate,
                    false,
                )?;
            }
            Ok(())
        });
        if let Err(failure) = promotion {
            let outcome = failure.outcome();
            let primary = Error::from(failure);
            return match outcome {
                RetainedExchangeOutcome::NotApplied => Err(self.preserve_unswapped_candidate(
                    candidate.id,
                    previous.map(|state| state.id),
                    candidate_origin,
                    primary,
                    tree_identity,
                    checkpoint,
                )),
                RetainedExchangeOutcome::Applied => Err(self.recover_swapped_candidate(
                    candidate.id,
                    previous,
                    candidate_origin,
                    PreviousUsrLocation::Staging,
                    None,
                    false,
                    false,
                    primary,
                    tree_identity,
                    checkpoint,
                )),
                // Neither authenticated layout survived reconciliation. Do
                // not guess which tree to move. The candidate row is left in
                // place, but durable mutation fencing remains the pending
                // journal-coordinator work documented in Phase 11.
                RetainedExchangeOutcome::Ambiguous => Err(primary),
            };
        }

        let mut previous_location = PreviousUsrLocation::Staging;
        let mut previous_archive_cleanup_pending = None;
        let mut system_triggers_incomplete = false;
        let mut candidate_boot_synchronization_started = false;
        let primary = (|| {
            tree_identity.verify_forward_exchange(
                &self.installation.root.join("usr"),
                &self.installation.staging_path("usr"),
            )?;
            tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
            if let Some(metadata) = metadata {
                metadata.revalidate()?;
            }
            let live_root_abi = live_root_abi.publish()?;
            live_root_abi.revalidate()?;
            if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                tree_identity.verify_active_reblit_candidate_snapshot(
                    &self.installation,
                    &self.state_db,
                    candidate,
                    true,
                )?;
            }
            checkpoint(StatefulTransitionCheckpoint::AfterUsrExchange)?;

            if run_system_triggers {
                system_triggers_incomplete = true;
                checkpoint(StatefulTransitionCheckpoint::AfterSystemTriggersStarted)?;
                Self::apply_triggers(TriggerScope::System(&self.installation), fstree)?;
                tree_identity.verify_forward_exchange(
                    &self.installation.root.join("usr"),
                    &self.installation.staging_path("usr"),
                )?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                    tree_identity.verify_active_reblit_candidate_snapshot(
                        &self.installation,
                        &self.state_db,
                        candidate,
                        true,
                    )?;
                }
                live_root_abi.revalidate()?;
                system_triggers_incomplete = false;
            }
            checkpoint(StatefulTransitionCheckpoint::AfterSystemTriggers)?;
            tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
            if let Some(metadata) = metadata {
                metadata.revalidate()?;
            }
            live_root_abi.revalidate()?;

            if archive_previous && let Some(previous) = previous {
                checkpoint(StatefulTransitionCheckpoint::BeforePreviousStateArchive)?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                tree_identity.verify_previous_for_recovery(&self.installation.staging_path("usr"))?;
                match tree_identity.archive_previous(&self.installation, previous.id) {
                    Ok(()) => {}
                    Err(failure) if failure.outcome() == RetainedPreviousMoveOutcome::Applied => {
                        // Recovery must look in the archive even when the
                        // idempotent durability suffix still reports failure.
                        previous_location = PreviousUsrLocation::Archived(previous.id);
                        tree_identity.finish_applied_previous_archive(&self.installation, previous.id)?;
                    }
                    Err(failure) if failure.outcome() == RetainedPreviousMoveOutcome::NotApplied => {
                        // `archive_previous` already made one exact-slot
                        // retirement attempt. Recovery performs one bounded
                        // suffix retry before reversing /usr, so a transient
                        // post-retirement durability failure cannot poison the
                        // next transaction's canonical state name.
                        previous_archive_cleanup_pending = Some(previous.id);
                        return Err(failure.into());
                    }
                    Err(failure) => return Err(failure.into()),
                }
                previous_location = PreviousUsrLocation::Archived(previous.id);
                tree_identity.verify_forward_exchange(
                    &self.installation.root.join("usr"),
                    &self.installation.root_path(previous.id.to_string()).join("usr"),
                )?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                checkpoint(StatefulTransitionCheckpoint::AfterPreviousStateArchive)?;
            }

            if run_boot_synchronization {
                checkpoint(StatefulTransitionCheckpoint::BeforeCandidateBootSynchronization)?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                    tree_identity.verify_active_reblit_candidate_snapshot(
                        &self.installation,
                        &self.state_db,
                        candidate,
                        true,
                    )?;
                }
                candidate_boot_synchronization_started = true;
                checkpoint(StatefulTransitionCheckpoint::AfterCandidateBootSynchronizationStarted)?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                    tree_identity.verify_active_reblit_candidate_snapshot(
                        &self.installation,
                        &self.state_db,
                        candidate,
                        true,
                    )?;
                }
                boot::synchronize(self, candidate, previous)?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                    tree_identity.verify_active_reblit_candidate_snapshot(
                        &self.installation,
                        &self.state_db,
                        candidate,
                        true,
                    )?;
                }
            }

            if candidate_origin == StatefulCandidateOrigin::Archived {
                tree_identity.retire_displaced_archived_candidate_slot(&self.installation, candidate.id)?;
            }

            tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
            if let Some(metadata) = metadata {
                metadata.revalidate()?;
            }

            Ok(())
        })();

        match primary {
            Ok(()) if candidate_origin == StatefulCandidateOrigin::ActiveReblit => {
                match tree_identity.rotate_active_reblit_staging(&self.installation, &self.state_db, candidate) {
                    Ok(()) => Ok(()),
                    Err(failure) if failure.outcome() == RetainedStagingWrapperRotationOutcome::NotApplied => Err(self
                        .recover_swapped_candidate(
                            candidate.id,
                            previous,
                            candidate_origin,
                            PreviousUsrLocation::Staging,
                            None,
                            false,
                            candidate_boot_synchronization_started,
                            Error::from(failure),
                            tree_identity,
                            checkpoint,
                        )),
                    Err(failure) => {
                        let outcome = match failure.outcome() {
                            RetainedStagingWrapperRotationOutcome::Applied => "applied",
                            RetainedStagingWrapperRotationOutcome::Ambiguous => "ambiguous",
                            RetainedStagingWrapperRotationOutcome::NotApplied => "not-applied",
                        };
                        Err(Error::ActiveReblitCommittedCleanupIncomplete {
                            state: candidate.id,
                            outcome,
                            source: Box::new(failure),
                        })
                    }
                }
            }
            Ok(()) => Ok(()),
            Err(primary) => Err(self.recover_swapped_candidate(
                candidate.id,
                previous,
                candidate_origin,
                previous_location,
                previous_archive_cleanup_pending,
                system_triggers_incomplete,
                candidate_boot_synchronization_started,
                primary,
                tree_identity,
                checkpoint,
            )),
        }
    }

    fn preserve_unswapped_candidate<F>(
        &self,
        candidate: state::Id,
        previous: Option<state::Id>,
        candidate_origin: StatefulCandidateOrigin,
        primary: Error,
        tree_identity: &StatefulTreeIdentity,
        checkpoint: &mut F,
    ) -> Error
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        let mut failures = StatefulRecoveryFailures::default();
        self.recover_failed_candidate(
            candidate,
            candidate_origin,
            false,
            tree_identity,
            checkpoint,
            &mut failures,
        );

        if failures.is_empty() {
            Error::StatefulCandidatePreserved {
                candidate,
                previous,
                primary: Box::new(primary),
            }
        } else {
            Error::StatefulTransitionRecoveryFailed {
                candidate,
                previous,
                primary: Box::new(primary),
                previous_archive_cleanup: None,
                restore_previous: None,
                reverse_exchange: None,
                preserve_candidate: failures.preserve_candidate,
                invalidate_candidate: failures.invalidate_candidate,
                repair_boot: None,
            }
        }
    }

    fn recover_swapped_candidate<F>(
        &self,
        candidate: state::Id,
        previous: Option<&State>,
        candidate_origin: StatefulCandidateOrigin,
        previous_location: PreviousUsrLocation,
        previous_archive_cleanup: Option<state::Id>,
        system_triggers_incomplete: bool,
        candidate_boot_synchronization_started: bool,
        primary: Error,
        tree_identity: &StatefulTreeIdentity,
        checkpoint: &mut F,
    ) -> Error
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        let previous_id = previous.map(|state| state.id);
        let mut failures = StatefulRecoveryFailures::default();

        if let Some(previous) = previous_archive_cleanup
            && let Err(error) = tree_identity.finish_not_applied_previous_archive(&self.installation, previous)
        {
            failures.previous_archive_cleanup = Some(Box::new(error.into()));
        }

        if let PreviousUsrLocation::Archived(previous) = previous_location {
            let restored = tree_identity
                .verify_candidate_for_recovery(&self.installation.root.join("usr"))
                .map_err(Error::from)
                .and_then(|()| {
                    tree_identity
                        .verify_previous_for_recovery(&self.installation.root_path(previous.to_string()).join("usr"))
                        .map_err(Error::from)
                })
                .and_then(|()| checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryPreviousStateRestore))
                .and_then(|()| self.restore_previous_to_staging(tree_identity, previous))
                .and_then(|()| {
                    tree_identity
                        .verify_previous_for_recovery(&self.installation.staging_path("usr"))
                        .map_err(Error::from)
                });
            if let Err(error) = restored {
                failures.restore_previous = Some(Box::new(error));
                return self.stateful_recovery_error(candidate, previous_id, primary, failures);
            }
        }

        let reversed = tree_identity
            .verify_forward_exchange(
                &self.installation.root.join("usr"),
                &self.installation.staging_path("usr"),
            )
            .map_err(Error::from)
            .and_then(|()| checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryUsrExchange))
            .and_then(|()| self.exchange_staging_and_live_usr(tree_identity))
            .and_then(|()| {
                tree_identity
                    .verify_restored(
                        &self.installation.root.join("usr"),
                        &self.installation.staging_path("usr"),
                    )
                    .map_err(Error::from)
            });
        if let Err(error) = reversed {
            failures.reverse_exchange = Some(Box::new(error));
            return self.stateful_recovery_error(candidate, previous_id, primary, failures);
        }

        // Once the reverse exchange succeeds, the failed candidate is safely
        // back in staging. Candidate preservation and restored-state boot repair
        // are independent recovery steps, so attempt both and retain both
        // errors if necessary.
        self.recover_failed_candidate(
            candidate,
            candidate_origin,
            system_triggers_incomplete,
            tree_identity,
            checkpoint,
            &mut failures,
        );

        if candidate_boot_synchronization_started {
            let repair: Result<(), Error> = checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryBootSynchronization)
                .and_then(|()| {
                    let Some(previous) = previous else {
                        return Err(Error::StatefulBootRepairUnverified {
                            candidate,
                            previous: None,
                        });
                    };

                    match boot::synchronize(self, previous, None) {
                        Ok(()) => Err(Error::StatefulBootRepairUnverified {
                            candidate,
                            previous: Some(previous.id),
                        }),
                        Err(error) => Err(Error::Boot(error)),
                    }
                });
            if let Err(error) = repair {
                failures.repair_boot = Some(Box::new(error));
            }
        }

        self.stateful_recovery_error(candidate, previous_id, primary, failures)
    }

    fn stateful_recovery_error(
        &self,
        candidate: state::Id,
        previous: Option<state::Id>,
        primary: Error,
        failures: StatefulRecoveryFailures,
    ) -> Error {
        if failures.is_empty() {
            Error::StatefulTransitionUsrRestored {
                candidate,
                previous,
                primary: Box::new(primary),
            }
        } else {
            Error::StatefulTransitionRecoveryFailed {
                candidate,
                previous,
                primary: Box::new(primary),
                previous_archive_cleanup: failures.previous_archive_cleanup,
                restore_previous: failures.restore_previous,
                reverse_exchange: failures.reverse_exchange,
                preserve_candidate: failures.preserve_candidate,
                invalidate_candidate: failures.invalidate_candidate,
                repair_boot: failures.repair_boot,
            }
        }
    }

    fn recover_failed_candidate<F>(
        &self,
        candidate: state::Id,
        candidate_origin: StatefulCandidateOrigin,
        quarantine_archived_candidate: bool,
        tree_identity: &StatefulTreeIdentity,
        checkpoint: &mut F,
        failures: &mut StatefulRecoveryFailures,
    ) where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        let preflight = tree_identity
            .verify_candidate_for_recovery(&self.installation.staging_path("usr"))
            .map_err(Error::from)
            .and_then(|()| checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryCandidatePreservation));
        if let Err(error) = preflight {
            failures.preserve_candidate = Some(Box::new(error));
            return;
        }

        let preservation = match self.preserve_failed_candidate(
            candidate,
            candidate_origin,
            quarantine_archived_candidate,
            tree_identity,
        ) {
            Ok(preservation) => preservation,
            Err(first)
                if candidate_origin == StatefulCandidateOrigin::Fresh
                    || (candidate_origin == StatefulCandidateOrigin::Archived && quarantine_archived_candidate) =>
            {
                match self.preserve_failed_candidate(
                    candidate,
                    candidate_origin,
                    quarantine_archived_candidate,
                    tree_identity,
                ) {
                    Ok(preservation) => preservation,
                    Err(retry) => {
                        failures.preserve_candidate = Some(Box::new(Error::StatefulCandidatePreservationRetryFailed {
                            first: Box::new(first),
                            retry: Box::new(retry),
                        }));
                        // Never delete the only database correlation for a
                        // candidate whose retained quarantine publication did
                        // not survive one bounded in-process retry.
                        return;
                    }
                }
            }
            Err(error) => {
                failures.preserve_candidate = Some(Box::new(error));
                // Never delete the only database correlation for a candidate
                // which has not first been durably preserved.
                return;
            }
        };

        self.invalidate_fresh_candidate(
            candidate,
            candidate_origin,
            preservation.as_ref(),
            tree_identity,
            checkpoint,
            failures,
        );
    }

    fn invalidate_fresh_candidate<F>(
        &self,
        candidate: state::Id,
        candidate_origin: StatefulCandidateOrigin,
        preservation: Option<&QuarantinedCandidate>,
        tree_identity: &StatefulTreeIdentity,
        checkpoint: &mut F,
        failures: &mut StatefulRecoveryFailures,
    ) where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        if candidate_origin == StatefulCandidateOrigin::Fresh
            && let Err(error) = checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryCandidateInvalidation)
                .and_then(|()| {
                    let preservation = preservation.ok_or_else(|| {
                        Error::Io(io::Error::other(
                            "fresh candidate has no retained quarantine proof before invalidation",
                        ))
                    })?;
                    tree_identity
                        .revalidate_quarantined_candidate(&self.installation, preservation)
                        .map_err(Error::from)
                })
                .and_then(|()| self.state_db.remove(&candidate).map_err(Error::Db))
        {
            failures.invalidate_candidate = Some(Box::new(error));
        }
    }

    fn exchange_staging_and_live_usr(&self, tree_identity: &StatefulTreeIdentity) -> Result<(), Error> {
        match tree_identity.exchange_reverse(&self.installation) {
            Ok(()) => Ok(()),
            Err(failure) if failure.outcome() == RetainedExchangeOutcome::Applied => {
                // The exact previous and candidate trees are already restored.
                // Retry only the idempotent fsync/revalidation suffix; a
                // second RENAME_EXCHANGE would undo the recovery.
                tree_identity
                    .finish_applied_reverse(&self.installation)
                    .map_err(Error::from)
            }
            Err(failure) => Err(Error::from(failure)),
        }
    }

    fn restore_previous_to_staging(&self, tree_identity: &StatefulTreeIdentity, state: state::Id) -> Result<(), Error> {
        match tree_identity.restore_previous(&self.installation, state) {
            Ok(()) => Ok(()),
            Err(failure) if failure.outcome() == RetainedPreviousMoveOutcome::Applied => tree_identity
                .finish_applied_previous_restore(&self.installation, state)
                .map_err(Error::from),
            Err(failure) => Err(failure.into()),
        }
    }

    fn rearchive_archived_candidate(
        &self,
        tree_identity: &StatefulTreeIdentity,
        state: state::Id,
    ) -> Result<(), Error> {
        let mut preparation_retried = false;
        loop {
            match tree_identity.rearchive_archived_candidate(&self.installation, state) {
                Ok(()) => return Ok(()),
                Err(failure) if failure.outcome() == RetainedArchivedCandidateMoveOutcome::Applied => {
                    return tree_identity
                        .finish_applied_archived_candidate_rearchive(&self.installation, state)
                        .map_err(Error::from);
                }
                Err(failure)
                    if failure.outcome() == RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied
                        && !preparation_retried =>
                {
                    // The exact prerequisite rename already applied. Retry
                    // once so its retained durability suffix can finish, but
                    // never spin on a persistent sync/revalidation failure.
                    preparation_retried = true;
                }
                Err(failure) => return Err(failure.into()),
            }
        }
    }

    fn preserve_failed_candidate(
        &self,
        candidate: state::Id,
        candidate_origin: StatefulCandidateOrigin,
        quarantine_archived_candidate: bool,
        tree_identity: &StatefulTreeIdentity,
    ) -> Result<Option<QuarantinedCandidate>, Error> {
        if candidate_origin == StatefulCandidateOrigin::ActiveReblit
            && tree_identity
                .has_active_reblit_staging_rotation()
                .map_err(Error::from)?
        {
            tree_identity
                .preserve_failed_active_reblit_wrapper(&self.installation, candidate)
                .map_err(Error::from)?;
            return Ok(None);
        }
        if candidate_origin == StatefulCandidateOrigin::Archived && !quarantine_archived_candidate {
            self.rearchive_archived_candidate(tree_identity, candidate)?;
            tree_identity
                .verify_candidate_for_recovery(&self.installation.root_path(candidate.to_string()).join("usr"))?;
            return Ok(None);
        }

        // Fresh candidates may be only partially prepared, an active reblit
        // would duplicate the restored live state identity, and an archived
        // candidate whose system-trigger phase did not complete may have been
        // partially mutated. None is safe in the ordinary bootable/prunable
        // state-root namespace.
        let kind = match candidate_origin {
            StatefulCandidateOrigin::Fresh => FailedCandidateKind::NewState,
            StatefulCandidateOrigin::ActiveReblit => FailedCandidateKind::ActiveReblit,
            StatefulCandidateOrigin::Archived => FailedCandidateKind::ArchivedState,
        };
        let preserved = tree_identity.quarantine_candidate(&self.installation, candidate, kind)?;
        if candidate_origin == StatefulCandidateOrigin::Archived {
            tree_identity.retire_displaced_archived_candidate_slot(&self.installation, candidate)?;
        }
        Ok(Some(preserved))
    }

    /// Acquire the canonical journal lock, reject unresolved journal/database
    /// evidence, and establish permanent marker identities for the staged
    /// candidate and live previous tree. The returned guard retains all three
    /// capabilities through activation and compensating recovery.
    fn prepare_stateful_tree_identity(
        &self,
        candidate_usr: &Path,
        candidate_state: state::Id,
    ) -> Result<StatefulTreeIdentity, crate::transition_identity::Error> {
        StatefulTreeIdentity::prepare(&self.installation, &self.state_db, candidate_usr, candidate_state)
    }

    fn prepare_stateful_tree_identity_retained(
        &self,
        candidate_usr_path: &Path,
        candidate_usr: &std::fs::File,
        candidate_state: state::Id,
    ) -> Result<StatefulTreeIdentity, crate::transition_identity::Error> {
        StatefulTreeIdentity::prepare_retained_candidate(
            &self.installation,
            &self.state_db,
            candidate_usr_path,
            candidate_usr,
            candidate_state,
        )
    }

    fn apply_ephemeral_candidate(
        &self,
        candidate: EphemeralCandidate,
        system_snapshot: SystemModel,
    ) -> Result<(), Error> {
        let EphemeralCandidate {
            tree,
            root,
            mut target,
            candidate_usr,
            active_state,
        } = candidate;
        active_state.revalidate(&self.installation)?;
        self.require_configured_ephemeral_target(&root)?;
        target.revalidate_candidate_usr(&self.installation, &candidate_usr)?;
        let result =
            self.apply_ephemeral_blit_under_guard(tree, &mut target, &candidate_usr, &active_state, system_snapshot);
        let revalidation = target.revalidate_candidate_usr(&self.installation, &candidate_usr);
        let active_revalidation = active_state.revalidate(&self.installation);
        match (result, revalidation, active_revalidation) {
            (Ok(()), Ok(()), Ok(())) => Ok(()),
            (Err(primary), _, _) => Err(primary),
            (Ok(()), Err(revalidation), _) => Err(revalidation),
            (Ok(()), Ok(()), Err(revalidation)) => Err(revalidation),
        }
    }

    fn require_configured_ephemeral_target(&self, requested: &Path) -> Result<PathBuf, Error> {
        let configured = match &self.scope {
            Scope::Ephemeral { destination } => destination.path().to_owned(),
            Scope::Stateful => return Err(Error::EphemeralProhibitedOperation),
            Scope::Frozen { .. } => return Err(Error::FrozenClientProhibitedOperation),
        };
        let requested = require_disjoint_materialization_target(&self.installation, requested)?;
        if requested != configured {
            return Err(Error::EphemeralDestinationMismatch { configured, requested });
        }
        Ok(requested)
    }

    fn apply_ephemeral_blit_under_guard(
        &self,
        fstree: vfs::Tree<PendingFile>,
        target: &mut RetainedExternalMaterializationTarget,
        candidate_usr: &candidate_metadata::RetainedEphemeralUsr,
        active_state: &active_state_snapshot::ActiveStateLease,
        system_snapshot: SystemModel,
    ) -> Result<(), Error> {
        target.revalidate_candidate_usr(&self.installation, candidate_usr)?;
        let root_abi = target.create_root_abi(&self.installation, candidate_usr)?;
        target.revalidate_candidate_usr(&self.installation, candidate_usr)?;
        let isolation_root_abi = create_root_links(&self.installation.isolation_dir())?;
        target.revalidate_candidate_usr(&self.installation, candidate_usr)?;
        let trigger_view = target.prepare_trigger_view(&self.installation, candidate_usr)?;
        trigger_view.revalidate(&self.installation)?;

        let metadata = candidate_metadata::decorate_ephemeral(candidate_usr, &system_snapshot)?;
        let revalidate = || -> Result<(), Error> {
            trigger_view.revalidate(&self.installation)?;
            metadata.revalidate()?;
            trigger_view.revalidate(&self.installation)?;
            root_abi.revalidate()?;
            isolation_root_abi.revalidate()?;
            active_state.revalidate(&self.installation)
        };
        revalidate()?;

        // ephemeral tx triggers
        before_ephemeral_transaction_triggers();
        let transaction = Self::apply_triggers(
            TriggerScope::RetainedEphemeral {
                phase: postblit::RetainedEphemeralPhase::Transaction,
                installation: &self.installation,
                isolation_root: &isolation_root_abi,
                view: trigger_view,
            },
            &fstree,
        );
        after_ephemeral_transaction_triggers();
        let transaction_revalidation = revalidate();
        match (transaction, transaction_revalidation) {
            (Ok(()), Ok(())) => {}
            (Err(primary), _) => return Err(primary.into()),
            (Ok(()), Err(revalidation)) => return Err(revalidation),
        }
        // ephemeral system triggers
        before_ephemeral_system_triggers();
        let system = Self::apply_triggers(
            TriggerScope::RetainedEphemeral {
                phase: postblit::RetainedEphemeralPhase::System,
                installation: &self.installation,
                isolation_root: &isolation_root_abi,
                view: trigger_view,
            },
            &fstree,
        );
        after_ephemeral_system_triggers();
        let system_revalidation = revalidate();
        match (system, system_revalidation) {
            (Ok(()), Ok(())) => {}
            (Err(primary), _) => return Err(primary.into()),
            (Ok(()), Err(revalidation)) => return Err(revalidation),
        }

        Ok(())
    }

    /// Download & unpack the provided packages. Packages already cached will be validated & skipped.
    pub(crate) async fn cache_packages<T>(&self, packages: &[T]) -> Result<(), Error>
    where
        T: Borrow<Package>,
    {
        // Setup progress bar
        let multi_progress = MultiProgress::new();

        // Add bar to track total package counts
        let total_progress = multi_progress.add(
            ProgressBar::new(packages.len() as u64).with_style(
                ProgressStyle::with_template("\n|{bar:20.cyan/blue}| {pos}/{len}")
                    .unwrap()
                    .progress_chars("■≡=- "),
            ),
        );
        total_progress.tick();

        // Network downloads remain concurrent and never hold the synchronous
        // materialization-writer mutex.
        let downloads = stream::iter(packages)
            .map(|package| async {
                let package: &Package = package.borrow();

                // Setup the progress bar and set as downloading
                let progress_bar = multi_progress.insert_before(
                    &total_progress,
                    ProgressBar::new(package.meta.download_size.unwrap_or_default())
                        .with_message(format!(
                            "{} {}",
                            "Downloading".blue(),
                            package.meta.name.as_str().bold(),
                        ))
                        .with_style(
                            ProgressStyle::with_template(
                                " {spinner} |{percent:>3}%| {wide_msg} {binary_bytes_per_sec:>.dim} ",
                            )
                            .unwrap()
                            .tick_chars("--=≡■≡=--"),
                        ),
                );
                progress_bar.enable_steady_tick(Duration::from_millis(150));

                // Download and update progress
                let download = cache::fetch(&package.meta, &self.installation, |progress| {
                    progress_bar.inc(progress.delta);
                    info!(
                        progress = progress.completed as f32 / progress.total as f32,
                        current = progress.completed as usize,
                        total = progress.total as usize,
                        event_type = "progress_update",
                        "Downloading {}",
                        package.meta.name
                    );
                })
                .await
                .map_err(|err| Error::CacheFetch(err, package.meta.name.clone()))?;

                let package = (*package).clone();
                let current_span = tracing::Span::current();
                Ok::<_, Error>((package, download, progress_bar, current_span))
            })
            .buffer_unordered(environment::MAX_NETWORK_CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;

        // Publish every asset and then the complete layout/install DB batch
        // under one synchronous lease. Pruning and candidate readers can see
        // either the old store or the complete new publication, never orphaned
        // asset names in a gap before their metadata becomes reachable.
        runtime::unblock({
            let layout_db = self.layout_db.clone();
            let install_db = self.install_db.clone();
            let multi_progress = multi_progress.clone();
            let total_progress = total_progress.clone();
            move || {
                let _writer_coordinator = fixed_staging::lock_coordinator()?;
                let unpacking_in_progress = cache::UnpackingInProgress::default();
                let mut cached = Vec::with_capacity(downloads.len());

                for (package, download, progress_bar, current_span) in downloads {
                    let _span_guard = current_span.enter();
                    let package_name = &package.meta.name;
                    let download_path = download.path().to_owned();
                    let is_cached = download.was_cached;

                    // Set progress to unpacking
                    progress_bar.set_message(format!("{} {}", "Unpacking".yellow(), package_name.to_string().bold()));
                    progress_bar.set_length(1000);
                    progress_bar.set_position(0);

                    // Unpack and update progress
                    let unpacked = download
                        .unpack(unpacking_in_progress.clone(), {
                            let progress_bar = progress_bar.clone();
                            let package_name = package_name.clone();

                            move |progress| {
                                progress_bar.set_position((progress.pct() * 1000.0) as u64);
                                info!(
                                    progress = progress.completed as f32 / progress.total as f32,
                                    current = progress.completed as usize,
                                    total = progress.total as usize,
                                    event_type = "progress_update",
                                    "Unpacking {package_name}",
                                );
                            }
                        })
                        .map_err(|err| Error::CacheUnpack(err, package_name.clone(), download_path))?;

                    // Remove this progress bar
                    progress_bar.finish();
                    multi_progress.remove(&progress_bar);

                    let cached_tag = is_cached
                        .then_some(format!("{}", " (cached)".dim()))
                        .unwrap_or_default();

                    // Write installed line
                    multi_progress.suspend(|| {
                        println!(
                            "{} {}{cached_tag}",
                            "Installed".green(),
                            package_name.to_string().bold()
                        );
                    });

                    // Inc total progress by 1
                    total_progress.inc(1);

                    info!(
                        progress = total_progress.position() as f32 / total_progress.length().unwrap_or(1) as f32,
                        current = total_progress.position() as usize,
                        total = total_progress.length().unwrap_or(0) as usize,
                        event_type = "progress_update",
                        "Cached {}",
                        package_name
                    );

                    cached.push((package, unpacked));
                }

                total_progress.set_position(0);
                total_progress.set_length(2);
                total_progress.set_message("Storing DB layouts");
                total_progress.tick();

                // Validate the complete decoded batch before opening the
                // layout transaction. Stone targets are canonically relative
                // to `/usr`; accepting an absolute target would bypass the
                // sole prefix supplied by `PendingFile::path` and could place
                // a package outside the stateful tree. Invalid packages must
                // not leave even partial layout rows behind.
                ingest_stone_layouts(
                    &layout_db,
                    cached.iter().flat_map(|(p, u)| {
                        u.payloads
                            .iter()
                            .flat_map(StoneDecodedPayload::layout)
                            .flat_map(|p| p.body.as_slice())
                            .map(|layout| (&p.id, layout))
                    }),
                )?;

                total_progress.inc(1);
                total_progress.set_message("Storing DB packages");

                // Add packages
                install_db.batch_add(cached.into_iter().map(|(p, _)| (p.id, p.meta)).collect())?;

                total_progress.inc(1);

                Ok::<_, Error>(())
            }
        })
        .await?;

        // Remove progress
        multi_progress.clear()?;

        Ok(())
    }

    /// Build a [`vfs::Tree`] for the specified package IDs
    ///
    /// Returns a newly built vfs Tree to plan the filesystem operations for blitting
    /// and conflict detection.
    pub fn vfs<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<vfs::Tree<PendingFile>, Error> {
        vfs(self.layout_db.query(packages)?)
    }

    fn canonical_frozen_package_ids(&self, packages: &[package::Id]) -> Result<Vec<package::Id>, Error> {
        // Bound borrowed inputs before cloning or sorting them. This helper is
        // shared by public frozen materialization and executable verification,
        // so neither entry point may allocate an attacker-sized closure first.
        require_frozen_executable_package_count(packages.len())?;
        let mut closure_id_bytes = 0usize;
        for package in packages {
            account_frozen_closure_id_bytes(package, &mut closure_id_bytes)?;
        }
        let mut canonical = packages.to_vec();
        canonical.sort();
        if canonical.is_empty() {
            return Err(Error::EmptyFrozenPackageClosure);
        }
        for duplicate in canonical.windows(2) {
            if duplicate[0] == duplicate[1] {
                return Err(Error::DuplicateFrozenPackage(duplicate[0].clone()));
            }
        }
        Ok(canonical)
    }

    fn frozen_root(&self) -> Result<&Path, Error> {
        match &self.scope {
            Scope::Frozen { destination } => Ok(&destination.root_path),
            Scope::Stateful | Scope::Ephemeral { .. } => Err(Error::FrozenRootRequiresFrozenClient),
        }
    }

    fn frozen_destination(&self) -> Result<&FrozenRootDestination, Error> {
        match &self.scope {
            Scope::Frozen { destination } => Ok(destination),
            Scope::Stateful | Scope::Ephemeral { .. } => Err(Error::FrozenRootRequiresFrozenClient),
        }
    }

    /// Materialize already-cached package layouts through the frozen-root
    /// filesystem path. This deliberately bypasses every stateful and legacy
    /// ephemeral post-blit operation.
    fn blit_frozen_root(
        &self,
        packages: &[package::Id],
        source_date_epoch: i64,
    ) -> Result<MaterializedFrozenRoot, Error> {
        let destination = self.frozen_destination()?;
        let blit_target = destination.root_path.clone();
        let deadline = Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT - FROZEN_NAMESPACE_RECOVERY_TIMEOUT;
        require_frozen_materialization_deadline(deadline)?;
        let _destination_lock = lock_frozen_destination_until(destination, deadline)?;
        let packages = self.canonical_frozen_package_ids(packages)?;
        let layouts = bounded_frozen_layouts(self, &packages, deadline, FrozenLayoutQueryOperation::Materialization)?;
        let fstree = frozen_vfs_until(&packages, layouts, deadline)?;
        let expected_tree = FrozenExpectedTree::from_vfs(&fstree, deadline)?;

        if frozen_named_identity_until(&destination.parent, &destination.name, &blit_target, deadline)?.is_some() {
            return Err(Error::FrozenRootDestinationExists(blit_target));
        }
        // Authenticate and account every independent regular-file copy before
        // creating staging state. Duplicate digests are charged once per
        // output inode, while canonical empty files consume zero bytes.
        let copy_manifest = FrozenCopyManifest::from_tree(&self.installation, &fstree, deadline)?;
        let stage = create_frozen_private_directory(destination, b".forge-frozen-stage-", deadline)?;
        // The random sibling wrapper remains 0700 for the entire build. The
        // publishable root can therefore carry its final 0755 mode without
        // exposing partial contents before the atomic rename.
        let stage_wrapper = stage.path.clone();
        let stage_path = stage_wrapper.join("root");
        let stage_wrapper_file = &stage.file;
        let mut retained_root = None;
        let result = (|| -> Result<MaterializedFrozenRoot, Error> {
            mkdirat(stage_wrapper_file.as_raw_fd(), "root", Mode::from_bits_truncate(0o755))?;
            let resolution = (nix::libc::RESOLVE_BENEATH
                | nix::libc::RESOLVE_NO_SYMLINKS
                | nix::libc::RESOLVE_NO_MAGICLINKS
                | nix::libc::RESOLVE_NO_XDEV) as u64;
            let root_anchor = openat2_frozen_until(
                stage_wrapper_file.as_raw_fd(),
                Path::new("root"),
                nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                resolution,
                deadline,
            )
            .map_err(|source| frozen_materialization_io_error(deadline, source, Error::from))?;
            let wrapper_metadata = stage_wrapper_file.metadata()?;
            let root_metadata = root_anchor.metadata()?;
            // SAFETY: geteuid takes no arguments and cannot fail.
            let effective_owner = unsafe { nix::libc::geteuid() };
            if !root_metadata.is_dir()
                || root_metadata.uid() != effective_owner
                || root_metadata.dev() != wrapper_metadata.dev()
                || root_metadata.mode() & 0o7777 & !0o755 != 0
            {
                return Err(Error::FrozenNormalizationInventoryMismatch {
                    path: stage_path.clone(),
                    reason: "the descriptor-relative stage root is not a same-owner same-filesystem creation residue",
                });
            }
            let root_residue = FrozenNormalizationWitness::from_metadata(&root_metadata);
            chmod_path_descriptor_until(root_anchor.file(), 0o755, deadline).map_err(|source| {
                frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenEntryMode {
                    path: stage_path.clone(),
                    source,
                })
            })?;
            let root = openat2_frozen_until(
                stage_wrapper_file.as_raw_fd(),
                Path::new("root"),
                nix::libc::O_RDONLY
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK
                    | nix::libc::O_NOATIME,
                resolution,
                deadline,
            )
            .map_err(|source| frozen_materialization_io_error(deadline, source, Error::from))?;
            retained_root = Some(root);
            let root = retained_root
                .as_ref()
                .expect("retained frozen root was assigned immediately above");
            let normalized_root = root_residue.with_permissions(0o755);
            require_frozen_normalization_witness(Path::new("/"), &root_anchor, normalized_root)?;
            require_frozen_normalization_witness(Path::new("/"), &root, normalized_root)?;
            let empty = frozen_normalization_inventory(&root, Path::new("/"), 0, None, deadline)?;
            debug_assert!(empty.is_empty());

            blit_tree_into_open_root(
                &self.installation,
                &fstree,
                root.as_raw_fd(),
                AssetMaterialization::IndependentCopy,
                BlitExecution::Sequential,
                Some(&copy_manifest),
                Some(deadline),
                None,
            )?;
            create_frozen_root_links(root.as_raw_fd(), deadline)?;
            normalize_frozen_tree(
                &root,
                &stage_path,
                &expected_tree,
                FileTime::from_unix_time(source_date_epoch, 0),
                deadline,
            )?;
            require_frozen_materialization_deadline(deadline)?;
            // Keep the descriptor opened before blitting as provenance across
            // normalization and publication. A replacement staging name can
            // never become the returned root token.
            publish_frozen_root(&stage, destination, root, root_anchor, deadline)
        })();

        let cleanup_deadline = frozen_namespace_recovery_deadline();
        match result {
            Ok(materialized_root) => {
                // `stage_path` moved out atomically; the private wrapper is
                // now empty and can be removed without traversal.
                if let Err(error) = remove_empty_frozen_private_directory(&stage, destination, cleanup_deadline) {
                    // Publication is already complete and cannot be reported
                    // as a failed materialization. The random 0700 wrapper is
                    // never a reusable root even if its empty-dir cleanup is
                    // interrupted.
                    trace!(path = ?stage_wrapper, %error, "failed to remove empty frozen stage wrapper");
                }
                materialized_root.revalidate()?;
                Ok(materialized_root)
            }
            Err(primary) => {
                let cleanup = match retained_root.as_ref() {
                    Some(root) => discard_retained_frozen_stage(&stage, destination, root, cleanup_deadline),
                    None => require_frozen_private_directory_entries(&stage, &[], cleanup_deadline),
                }
                .and_then(|()| remove_empty_frozen_private_directory(&stage, destination, cleanup_deadline));
                match cleanup {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(Error::CleanupFrozenStage {
                        stage: stage_wrapper,
                        primary: Box::new(primary),
                        cleanup: Box::new(cleanup),
                    }),
                }
            }
        }
    }

    /// Blit the packages to a filesystem root
    ///
    /// This functionality is core to all Cast filesystem transactions, forming the entire
    /// staging logic. For all the [`crate::package::Id`] present in the staging state,
    /// query their stored [`StonePayloadLayoutBody`] and cache into a [`vfs::Tree`].
    ///
    /// The new `/usr` filesystem is written in optimal order to a staging tree by making
    /// use of the "at" family of functions (`mkdirat`, `linkat`, etc) with relative directory
    /// file descriptors. Every writable root receives independent, digest-
    /// verified copies so trigger or build-time writes and per-path mode changes
    /// cannot mutate the persistent content-addressed store.
    ///
    /// This provides a very quick means to generate a filesystem snapshot on-demand,
    /// which can then be activated via [`Self::promote_staging`]
    pub fn blit_root<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<vfs::Tree<PendingFile>, Error> {
        let candidate = self.materialize_ephemeral_candidate(packages)?;
        let EphemeralCandidate {
            tree,
            root: _root,
            target: _target,
            candidate_usr: _candidate_usr,
            active_state: _active_state,
        } = candidate;
        Ok(tree)
    }

    fn materialize_ephemeral_candidate<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<EphemeralCandidate, Error> {
        let destination = match &self.scope {
            Scope::Stateful => {
                return Err(Error::FixedStagingCapabilityRequired {
                    operation: "materialize a stateful client root",
                });
            }
            Scope::Ephemeral { destination } => destination,
            Scope::Frozen { .. } => return Err(Error::FrozenClientProhibitedOperation),
        };
        let active_state = active_state_snapshot::ActiveStateLease::acquire(&self.installation)?;

        let tree = self.vfs(packages)?;
        active_state.revalidate(&self.installation)?;
        let mut target = RetainedExternalMaterializationTarget::prepare_from(&self.installation, destination)?;
        let candidate_usr = target.materialize(
            &self.installation,
            &tree,
            AssetMaterialization::IndependentCopy,
            BlitExecution::Parallel,
        )?;
        active_state.revalidate(&self.installation)?;
        let root = target.path().to_owned();

        Ok(EphemeralCandidate {
            tree,
            root,
            target,
            candidate_usr,
            active_state,
        })
    }

    fn load_or_create_system_snapshot(&self, path: PathBuf, state: &State) -> Result<SystemModel, Error> {
        match system_model::load(&path).map_err(Error::LoadSystemModel)? {
            Some(system_model) => SystemModel::try_from(system_model)
                .map_err(system_model::UpdateError::from)
                .map_err(Error::UpdateSystemModel),
            None => {
                let active_repos = self
                    .repositories
                    .active()
                    .map(|repo| (repo.id, repo.repository))
                    .collect::<repository::Map>();

                let packages = self
                    .resolve_packages(state.selections.iter().filter_map(|s| s.explicit.then_some(&s.package)))?
                    .into_iter()
                    .map(|package| Provider::package_name(package.meta.name.as_str()))
                    .collect();

                Ok(system_model::create(active_repos, packages))
            }
        }
    }

    /// Export the provided state as a [`SystemModel`]
    pub fn export_state(&self, state: state::Id) -> Result<SystemModel, Error> {
        let active_state = match &self.scope {
            Scope::Frozen { .. } => None,
            Scope::Stateful | Scope::Ephemeral { .. } => {
                Some(active_state_snapshot::ActiveStateLease::acquire(&self.installation)?)
            }
        };
        let state = self.state_db.get(state)?;
        if let Some(active_state) = active_state.as_ref() {
            active_state.revalidate(&self.installation)?;
        }
        let is_active = active_state.as_ref().and_then(|lease| lease.active()) == Some(state.id);

        let path = if is_active {
            system_model::snapshot_path(&self.installation.root)
        } else {
            system_model::snapshot_path(&self.installation.root_path(state.id.to_string()))
        };

        let active_snapshot = active_state
            .map(|active_state| active_state.suspend(&self.installation))
            .transpose()?;
        let snapshot = self.load_or_create_system_snapshot(path, &state)?;
        if let Some(active_snapshot) = active_snapshot {
            drop(active_snapshot.resume(&self.installation)?);
        }
        Ok(snapshot)
    }

    /// Print boot status to stdout
    pub fn print_boot_status(&self) -> Result<(), Error> {
        boot::print_status(&self.installation).map_err(Error::Boot)
    }

    /// Synchronize boot for the active state
    pub fn synchronize_boot(&self) -> Result<(), Error> {
        self.require_stateful_scope()?;
        let active_state = active_state_snapshot::ActiveStateLease::acquire(&self.installation)?;
        let Some(state_id) = active_state.active() else {
            return Err(Error::NoActiveState);
        };

        let state = self.state_db.get(state_id)?;
        active_state.revalidate(&self.installation)?;

        boot::synchronize(self, &state, None).map_err(Error::Boot)
    }

    /// List all states for this Cast [`Installation`]
    pub fn list_states(&self) -> Result<Vec<State>, Error> {
        self.state_db
            .list_ids()?
            .into_iter()
            .map(|(id, _)| self.state_db.get(id).map_err(Error::Db))
            .collect()
    }

    /// Return a [`State`] for the provided state id
    pub fn get_state(&self, id: state::Id) -> Result<State, Error> {
        self.state_db.get(id).map_err(Error::Db)
    }

    /// Return the active [`State`] for this Cast [`Installation`]
    pub fn get_active_state(&self) -> Result<Option<State>, Error> {
        let active_state = match &self.scope {
            Scope::Frozen { .. } => return Ok(None),
            Scope::Stateful | Scope::Ephemeral { .. } => {
                active_state_snapshot::ActiveStateLease::acquire(&self.installation)?
            }
        };
        let state = match active_state.active() {
            Some(id) => self.get_state(id).map(Some),
            None => Ok(None),
        }?;
        active_state.revalidate(&self.installation)?;
        Ok(state)
    }

    /// List all layout entries cached by this Cast [`Installation`], which
    /// includes packages installed across all states
    pub fn list_layouts(&self) -> Result<Vec<(package::Id, StonePayloadLayoutRecord)>, Error> {
        self.layout_db.all().map_err(Error::Db)
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn mocked(installation: Installation, registry: Registry) -> Result<Client, Error> {
        let config = config::Manager::system(&installation.root, "cast");
        let install_db = db::meta::Database::new(":memory:")?;
        let state_db = db::state::Database::new(":memory:")?;
        let layout_db = db::layout::Database::new(":memory:")?;

        let repositories = repository::Manager::with_config_manager(config.clone(), installation.clone())?;

        Ok(Client {
            config: Some(config),
            installation,
            repositories,
            registry,
            install_db,
            state_db,
            layout_db,
            scope: Scope::Stateful,
        })
    }
}

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const EMPTY_FILE_DIGEST: u128 = 0x99aa_06d3_0147_98d8_6001_c324_468d_497f;
const MAX_FROZEN_EXECUTABLE_PACKAGES: usize = 4_096;
const MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES: usize = MIB as usize;
const MAX_FROZEN_EXECUTABLE_BINDINGS: usize = 4_096;
// Linux PATH_MAX includes the terminating NUL; frozen paths and link targets
// are stored without it.
const MAX_FROZEN_EXECUTABLE_PATH_BYTES: usize = nix::libc::PATH_MAX as usize - 1;
// Tree::structured_children recursively descends path components. Keep frozen
// layouts well below the stack depth that PATH_MAX alone could permit.
const MAX_FROZEN_LAYOUT_PATH_COMPONENTS: usize = 128;
const MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES: usize = 16 * MIB as usize;
const MAX_FROZEN_EXECUTABLE_LAYOUTS: usize = 262_144;
const MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES: usize = 64 * MIB as usize;
const MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS: usize = 262_144;
const MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES: usize = 64 * MIB as usize;
const MAX_FROZEN_EXECUTABLE_BYTES: u64 = 512 * MIB;
const MAX_TOTAL_FROZEN_EXECUTABLE_BYTES: u64 = 2 * GIB;
const MAX_FROZEN_EXECUTABLE_SYMLINKS: usize = 32;
const MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES: usize = nix::libc::PATH_MAX as usize - 1;
// Linux inspects at most 256 bytes of a script header. Requiring the newline
// inside that same finite window avoids kernel-version-dependent truncation
// and leaves at most 253 bytes for `#!` plus one absolute interpreter path.
const MAX_FROZEN_SHEBANG_LINE_BYTES: usize = 256;
const MAX_FROZEN_SHEBANG_INTERPRETER_BYTES: usize = MAX_FROZEN_SHEBANG_LINE_BYTES - 3;
// Linux's binary-parameter recursion ceiling admits five nested scripts and
// rejects the sixth with ELOOP. Keep the script-specific counter identical to
// the kernel; ELF PT_INTERP edges have their own finite graph ceiling below.
const MAX_FROZEN_SHEBANG_INTERPRETERS: usize = 5;
const MAX_FROZEN_EXECUTABLE_INTERPRETERS: usize = 32;
const MAX_FROZEN_ELF_PROGRAM_HEADERS: usize = 1_024;
const MAX_FROZEN_ELF_INTERPRETER_BYTES: usize = MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1;
// Descriptors are retained until the complete graph is revalidated. Keep a
// conservative ceiling below the common 1024-descriptor process limit so the
// verifier fails deliberately rather than through ambient EMFILE pressure.
const MAX_FROZEN_EXECUTABLE_PINNED_FILES: usize = 512;
const FROZEN_EXECUTABLE_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(120);
const FROZEN_MATERIALIZATION_TIMEOUT: Duration = Duration::from_secs(600);
const FROZEN_NAMESPACE_RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const FROZEN_DESTINATION_LOCK_RETRY: Duration = Duration::from_millis(10);
const MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS: usize = 1_024;
const MAX_FROZEN_PRIVATE_DIRECTORY_ATTEMPTS: usize = 128;
const FROZEN_PRIVATE_DIRECTORY_RANDOM_BYTES: usize = 16;
// Independent frozen-root copies densify every regular output inode. Match the
// existing Mason archive-staging ceiling: one cached asset remains bounded at
// 8 GiB and the complete copied userspace at 32 GiB of logical file bytes.
const MAX_TOTAL_FROZEN_BLIT_BYTES: u64 = 32 * GIB;
const MAX_FROZEN_NORMALIZED_INODES: usize = MAX_FROZEN_EXECUTABLE_LAYOUTS + MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS + 6;

#[derive(Debug, Default)]
struct FrozenCopyManifest {
    lengths: BTreeMap<u128, u64>,
    total_bytes: u64,
}

impl FrozenCopyManifest {
    fn from_tree(installation: &Installation, tree: &vfs::Tree<PendingFile>, deadline: Instant) -> Result<Self, Error> {
        Self::from_tree_with_limit(installation, tree, deadline, MAX_TOTAL_FROZEN_BLIT_BYTES)
    }

    fn from_tree_with_limit(
        installation: &Installation,
        tree: &vfs::Tree<PendingFile>,
        deadline: Instant,
        limit: u64,
    ) -> Result<Self, Error> {
        let mut digests = tree.iter().filter_map(|item| match item.layout.file {
            StonePayloadLayoutFile::Regular(digest, _) if digest != EMPTY_FILE_DIGEST => Some(digest),
            _ => None,
        });
        let Some(first) = digests.next() else {
            return Ok(Self::default());
        };
        let pool = AssetPool::open(installation)?;
        let manifest = Self::from_digests_with_limit(std::iter::once(first).chain(digests), limit, |digest| {
            require_frozen_materialization_deadline(deadline)?;
            let asset = pool.open_asset(&frozen_asset_path(digest))?;
            Ok(asset.witness.length)
        })?;
        require_frozen_materialization_deadline(deadline)?;
        pool.revalidate()?;
        Ok(manifest)
    }

    fn from_digests_with_limit(
        digests: impl IntoIterator<Item = u128>,
        limit: u64,
        mut length: impl FnMut(u128) -> Result<u64, Error>,
    ) -> Result<Self, Error> {
        let mut manifest = Self::default();
        for digest in digests {
            if digest == EMPTY_FILE_DIGEST {
                continue;
            }
            let actual = length(digest)?;
            if let Some(expected) = manifest.lengths.get(&digest) {
                if *expected != actual {
                    return Err(Error::FrozenMaterializationAssetLengthChanged {
                        digest,
                        expected: *expected,
                        actual,
                    });
                }
            } else {
                manifest.lengths.insert(digest, actual);
            }
            account_frozen_blit_bytes(&mut manifest.total_bytes, actual, limit)?;
        }
        Ok(manifest)
    }

    fn require_length(&self, digest: u128, actual: u64) -> Result<(), Error> {
        match self.lengths.get(&digest) {
            Some(expected) if *expected == actual => Ok(()),
            Some(expected) => Err(Error::FrozenMaterializationAssetLengthChanged {
                digest,
                expected: *expected,
                actual,
            }),
            None => Err(Error::FrozenMaterializationAssetMissingFromManifest { digest }),
        }
    }
}

fn account_frozen_blit_bytes(total: &mut u64, additional: u64, limit: u64) -> Result<(), Error> {
    let actual = total
        .checked_add(additional)
        .ok_or(Error::FrozenMaterializationTotalByteLimit {
            limit,
            actual: u64::MAX,
        })?;
    if actual > limit {
        return Err(Error::FrozenMaterializationTotalByteLimit { limit, actual });
    }
    *total = actual;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct FrozenNormalizationLimits {
    inodes: usize,
    depth: usize,
}

impl FrozenNormalizationLimits {
    const PRODUCTION: Self = Self {
        inodes: MAX_FROZEN_NORMALIZED_INODES,
        depth: MAX_FROZEN_LAYOUT_PATH_COMPONENTS,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FrozenExpectedKind {
    Directory,
    Regular { digest: u128 },
    Symlink { target: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenExpectedEntry {
    kind: FrozenExpectedKind,
    mode: u32,
}

#[derive(Debug)]
struct FrozenExpectedTree {
    entries: BTreeMap<PathBuf, FrozenExpectedEntry>,
    children: BTreeMap<PathBuf, BTreeMap<OsString, PathBuf>>,
}

impl FrozenExpectedTree {
    fn from_vfs(tree: &vfs::Tree<PendingFile>, deadline: Instant) -> Result<Self, Error> {
        let mut entries = BTreeMap::new();
        Self::insert_entry(
            &mut entries,
            PathBuf::from("/"),
            FrozenExpectedEntry {
                kind: FrozenExpectedKind::Directory,
                mode: 0o755,
            },
        )?;

        for item in tree.iter() {
            require_frozen_materialization_deadline(deadline)?;
            let path = PathBuf::from(item.path().as_str());
            let kind = match &item.layout.file {
                StonePayloadLayoutFile::Directory(_) => FrozenExpectedKind::Directory,
                StonePayloadLayoutFile::Regular(digest, _) => FrozenExpectedKind::Regular { digest: *digest },
                StonePayloadLayoutFile::Symlink(target, _) => FrozenExpectedKind::Symlink {
                    target: target.as_bytes().to_vec(),
                },
                StonePayloadLayoutFile::CharacterDevice(_)
                | StonePayloadLayoutFile::BlockDevice(_)
                | StonePayloadLayoutFile::Fifo(_)
                | StonePayloadLayoutFile::Socket(_)
                | StonePayloadLayoutFile::Unknown(..) => {
                    return Err(Error::InvalidFrozenNormalizationDeclaration {
                        path,
                        reason: "the declarative tree contains an unsupported inode type",
                    });
                }
            };
            Self::insert_entry(
                &mut entries,
                path,
                FrozenExpectedEntry {
                    kind,
                    mode: item.layout.mode & 0o7777,
                },
            )?;
        }

        for (target, name) in ROOT_ABI_LINKS {
            require_frozen_materialization_deadline(deadline)?;
            Self::insert_entry(
                &mut entries,
                Path::new("/").join(name),
                FrozenExpectedEntry {
                    kind: FrozenExpectedKind::Symlink {
                        target: target.as_bytes().to_vec(),
                    },
                    mode: 0o777,
                },
            )?;
        }
        require_frozen_materialization_deadline(deadline)?;
        Self::from_entries(entries, FrozenNormalizationLimits::PRODUCTION)
    }

    fn insert_entry(
        entries: &mut BTreeMap<PathBuf, FrozenExpectedEntry>,
        path: PathBuf,
        entry: FrozenExpectedEntry,
    ) -> Result<(), Error> {
        match entries.entry(path.clone()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(entry);
                Ok(())
            }
            std::collections::btree_map::Entry::Occupied(slot) if slot.get() == &entry => Ok(()),
            std::collections::btree_map::Entry::Occupied(_) => Err(Error::InvalidFrozenNormalizationDeclaration {
                path,
                reason: "two declarations disagree about one path",
            }),
        }
    }

    fn from_entries(
        entries: BTreeMap<PathBuf, FrozenExpectedEntry>,
        limits: FrozenNormalizationLimits,
    ) -> Result<Self, Error> {
        let actual = entries.len();
        if actual > limits.inodes {
            return Err(Error::FrozenNormalizationInodeLimit {
                limit: limits.inodes,
                actual,
            });
        }
        let Some(root) = entries.get(Path::new("/")) else {
            return Err(Error::InvalidFrozenNormalizationDeclaration {
                path: PathBuf::from("/"),
                reason: "the declarative tree has no root",
            });
        };
        if root
            != &(FrozenExpectedEntry {
                kind: FrozenExpectedKind::Directory,
                mode: 0o755,
            })
        {
            return Err(Error::InvalidFrozenNormalizationDeclaration {
                path: PathBuf::from("/"),
                reason: "the declarative root is not a mode-0755 directory",
            });
        }

        let mut children: BTreeMap<PathBuf, BTreeMap<OsString, PathBuf>> = BTreeMap::new();
        for path in entries.keys() {
            let depth =
                frozen_normalization_path_depth(path).ok_or_else(|| Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative path is not normalized and absolute",
                })?;
            if depth > limits.depth {
                return Err(Error::FrozenNormalizationDepthLimit {
                    limit: limits.depth,
                    actual: depth,
                });
            }
            if path == Path::new("/") {
                continue;
            }
            let parent = path
                .parent()
                .ok_or_else(|| Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative path has no parent",
                })?;
            let Some(parent_entry) = entries.get(parent) else {
                return Err(Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative path has no declared parent",
                });
            };
            if !matches!(parent_entry.kind, FrozenExpectedKind::Directory) {
                return Err(Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative path has a non-directory parent",
                });
            }
            let name = path
                .file_name()
                .ok_or_else(|| Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative path has no final component",
                })?
                .to_owned();
            if children
                .entry(parent.to_owned())
                .or_default()
                .insert(name, path.clone())
                .is_some()
            {
                return Err(Error::InvalidFrozenNormalizationDeclaration {
                    path: path.clone(),
                    reason: "the declarative directory contains a duplicate name",
                });
            }
        }
        Ok(Self { entries, children })
    }

    fn entry(&self, path: &Path) -> Result<&FrozenExpectedEntry, Error> {
        self.entries
            .get(path)
            .ok_or_else(|| Error::InvalidFrozenNormalizationDeclaration {
                path: path.to_owned(),
                reason: "the normalizer requested an undeclared path",
            })
    }

    fn children(&self, path: &Path) -> impl Iterator<Item = (&OsString, &PathBuf)> {
        self.children.get(path).into_iter().flat_map(BTreeMap::iter)
    }
}

fn frozen_normalization_path_depth(path: &Path) -> Option<usize> {
    if !path.is_absolute() {
        return None;
    }
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            PathComponent::RootDir => {}
            PathComponent::Normal(_) => depth = depth.saturating_add(1),
            PathComponent::CurDir | PathComponent::ParentDir | PathComponent::Prefix(_) => return None,
        }
    }
    Some(depth)
}

fn frozen_normalization_declared_children<'a>(
    expected: &'a FrozenExpectedTree,
    path: &Path,
) -> Result<Vec<(&'a OsString, &'a PathBuf)>, Error> {
    let count = expected.children.get(path).map_or(0, BTreeMap::len);
    let mut children = Vec::new();
    children
        .try_reserve_exact(count)
        .map_err(|source| Error::ReserveFrozenNormalizationInventory {
            path: path.to_owned(),
            source,
        })?;
    children.extend(expected.children(path));
    Ok(children)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenNormalizationCheckpoint {
    EntryPinned,
    DirectoryTraversalModeApplied,
    DirectoryEnumerated,
    BeforeFinalTreeConfirmation,
    AfterRegularDigest,
    BeforeDirectoryFinalInventory,
    AfterDirectoryFinalInventory,
    BeforeEntryRevalidation,
    BeforeRootRevalidation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenNormalizationOpen {
    Anchor,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenNormalizationWitness {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    length: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenNormalizationFinalWitness {
    stable: FrozenNormalizationWitness,
    accessed_seconds: i64,
    accessed_nanoseconds: i64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FrozenNormalizationFinalWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            stable: FrozenNormalizationWitness::from_metadata(metadata),
            accessed_seconds: metadata.atime(),
            accessed_nanoseconds: metadata.atime_nsec(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

impl FrozenNormalizationWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
            links: metadata.nlink(),
            length: metadata.len(),
        }
    }

    fn with_permissions(self, mode: u32) -> Self {
        Self {
            mode: (self.mode & nix::libc::S_IFMT) | mode,
            ..self
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenNormalizationInventoryEntry {
    name: CString,
    witness: FrozenNormalizationWitness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenExecutableCheckpoint {
    AfterOpen,
    AfterDigest,
    BeforeReopen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenExecutableWitness {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

/// Stable root-inode properties which remain invariant while callers add
/// build-visible descendants.  Directory mtime/ctime/link-count deliberately
/// do not participate: creating source and mount-target directories changes
/// those values without changing the root inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenRootIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
}

impl FrozenRootIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
        }
    }
}

impl FrozenExecutableWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone)]
struct ExpectedFrozenExecutable {
    digest: u128,
    mode: u32,
    resolved_path: PathBuf,
    symlinks: Vec<ExpectedFrozenSymlink>,
}

#[derive(Debug, Clone)]
struct ExpectedFrozenSymlink {
    package: package::Id,
    path: PathBuf,
    target: String,
    mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FrozenExecutableLayout {
    Regular { digest: u128, mode: u32 },
    Symlink { target: String, mode: u32 },
    Directory { uid: u32, gid: u32, mode: u32, tag: u32 },
    Other,
}

impl FrozenExecutableLayout {
    fn is_identical_directory(&self, other: &Self) -> bool {
        matches!((self, other), (Self::Directory { .. }, Self::Directory { .. })) && self == other
    }
}

#[derive(Debug)]
struct PinnedFrozenSymlink {
    file: fs::File,
    witness: FrozenExecutableWitness,
    expected: ExpectedFrozenSymlink,
}

#[derive(Debug)]
struct PinnedFrozenExecutable {
    file: fs::File,
    witness: FrozenExecutableWitness,
    binding: FrozenExecutableBinding,
    expected: ExpectedFrozenExecutable,
    symlinks: Vec<PinnedFrozenSymlink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenShebangInterpreter {
    path: PathBuf,
    root_alias: Option<ExpectedFrozenRootAlias>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpectedFrozenRootAlias {
    path: PathBuf,
    target: String,
}

#[derive(Debug)]
struct PinnedFrozenRootAlias {
    file: fs::File,
    witness: FrozenExecutableWitness,
    expected: ExpectedFrozenRootAlias,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenShebangParseError {
    LineTooLong,
    Unterminated,
    EmptyInterpreter,
    InterpreterTooLong,
    Nul,
    WhitespaceOrOptions,
    NonUtf8,
    Relative,
    NonNormalized,
    EnvironmentLookup,
}

impl FrozenShebangParseError {
    fn reason(self) -> &'static str {
        match self {
            Self::LineTooLong => "the shebang line exceeds the 256-byte kernel inspection window",
            Self::Unterminated => "the shebang line is not newline-terminated",
            Self::EmptyInterpreter => "the shebang does not name an interpreter",
            Self::InterpreterTooLong => "the interpreter path exceeds the shebang path limit",
            Self::Nul => "the interpreter path contains NUL",
            Self::WhitespaceOrOptions => "whitespace and interpreter options are not supported",
            Self::NonUtf8 => "the interpreter path is not UTF-8",
            Self::Relative => "the interpreter path is not absolute",
            Self::NonNormalized => "the interpreter path is not lexically normalized",
            Self::EnvironmentLookup => "environment-based interpreter lookup is forbidden",
        }
    }
}

#[derive(Debug)]
struct FrozenExecutableDigest {
    digest: u128,
    shebang_probe: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FrozenExecutableInterpreter {
    Shebang(FrozenShebangInterpreter),
    Elf(FrozenShebangInterpreter),
}

impl FrozenExecutableInterpreter {
    fn binding(&self) -> &FrozenShebangInterpreter {
        match self {
            Self::Shebang(binding) | Self::Elf(binding) => binding,
        }
    }

    fn is_shebang(&self) -> bool {
        matches!(self, Self::Shebang(_))
    }
}

#[derive(Debug)]
struct PreparedFrozenExecutableLayout {
    package: package::Id,
    path: PathBuf,
    entry: FrozenExecutableLayout,
    is_directory: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenLayoutQueryOperation {
    Materialization,
    ExecutableVerification,
}

impl FrozenLayoutQueryOperation {
    fn timeout(self) -> Error {
        match self {
            Self::Materialization => Error::FrozenMaterializationTimeout {
                seconds: FROZEN_MATERIALIZATION_TIMEOUT.as_secs(),
            },
            Self::ExecutableVerification => Error::FrozenExecutableVerificationTimeout {
                seconds: FROZEN_EXECUTABLE_VERIFICATION_TIMEOUT.as_secs(),
            },
        }
    }
}

fn bounded_frozen_layouts(
    client: &Client,
    packages: &[package::Id],
    deadline: Instant,
    operation: FrozenLayoutQueryOperation,
) -> Result<Vec<(package::Id, StonePayloadLayoutRecord)>, Error> {
    let outcome = client.layout_db.query_bounded(
        packages,
        db::layout::QueryBounds {
            max_rows: MAX_FROZEN_EXECUTABLE_LAYOUTS,
            max_string_bytes: MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES,
        },
        || Instant::now() <= deadline,
    )?;
    match outcome {
        db::layout::BoundedQueryOutcome::Complete(layouts) => Ok(layouts),
        db::layout::BoundedQueryOutcome::PackageLimit { limit, actual } => {
            Err(Error::FrozenExecutablePackageLimit { limit, actual })
        }
        db::layout::BoundedQueryOutcome::PackageIdByteLimit { limit, actual } => {
            Err(Error::FrozenExecutableClosureIdByteLimit { limit, actual })
        }
        db::layout::BoundedQueryOutcome::RowLimit { limit, actual } => {
            Err(Error::FrozenExecutableLayoutLimit { limit, actual })
        }
        db::layout::BoundedQueryOutcome::StringByteLimit { limit, actual } => {
            Err(Error::FrozenLayoutStorageByteLimit { limit, actual })
        }
        db::layout::BoundedQueryOutcome::Cancelled => Err(operation.timeout()),
    }
}

fn require_frozen_executables<F>(
    client: &Client,
    materialized_root: MaterializedFrozenRoot,
    packages: &[package::Id],
    bindings: &[FrozenExecutableBinding],
    mut checkpoint: F,
) -> Result<FrozenRootGuard, Error>
where
    F: FnMut(&FrozenExecutableBinding, FrozenExecutableCheckpoint),
{
    // The clock and all input bounds begin before any closure set or database
    // layout is discovered. An oversized request must not first allocate or
    // query an unbounded representation and only then be rejected.
    let deadline = Instant::now() + FROZEN_EXECUTABLE_VERIFICATION_TIMEOUT;
    require_frozen_executable_deadline(deadline)?;
    require_frozen_executable_package_count(packages.len())?;
    require_frozen_executable_binding_count(bindings.len())?;

    let mut closure_id_bytes = 0usize;
    let mut package_set = BTreeSet::new();
    for package in packages {
        require_frozen_executable_deadline(deadline)?;
        account_frozen_closure_id_bytes(package, &mut closure_id_bytes)?;
        if !package_set.insert(package) {
            return Err(Error::DuplicateFrozenPackage(package.clone()));
        }
    }

    let mut binding_bytes = 0usize;
    for binding in bindings {
        require_frozen_executable_deadline(deadline)?;
        // Inspect the borrowed raw path before provider lookup or any path
        // clone. Oversized and non-UTF-8 attacker input therefore produces a
        // bounded diagnostic rather than being copied into an error value.
        let path = require_frozen_executable_path(binding)?;
        account_frozen_binding_bytes(binding, path.len(), &mut binding_bytes)?;
        if !package_set.contains(&binding.package) {
            return Err(Error::FrozenExecutableProviderOutsideClosure {
                package: binding.package.clone(),
                path: binding.path.clone(),
            });
        }
    }

    let mut bindings = bindings.to_vec();
    bindings.sort();
    bindings.dedup();

    // Consume the exact descriptor opened before publication.  No successful
    // verification path is allowed to reopen the configured destination and
    // treat that newly found inode as the materialized root.
    let (root_path, root, root_witness) = materialized_root.into_guard_root()?;
    if bindings.is_empty() {
        let guard = FrozenRootGuard {
            root_path,
            root,
            root_witness,
            executables: Vec::new(),
            root_aliases: BTreeMap::new(),
        };
        guard.revalidate_until(deadline)?;
        return Ok(guard);
    }

    // Interpreter ownership is discovered from the complete frozen closure,
    // never from the host or from a provider lookup performed after planning.
    let layouts = bounded_frozen_layouts(
        client,
        packages,
        deadline,
        FrozenLayoutQueryOperation::ExecutableVerification,
    )?;
    require_frozen_executable_deadline(deadline)?;
    require_frozen_executable_layout_count(layouts.len())?;

    let mut prepared_layouts = Vec::with_capacity(layouts.len());
    let mut layout_bytes = 0usize;
    for (package, layout) in layouts {
        require_frozen_executable_deadline(deadline)?;
        if !package_set.contains(&package) {
            return Err(Error::UnexpectedFrozenLayoutPackage(package));
        }
        // Direct database fixtures can bypass normal Stone ingestion. Apply
        // the same canonical raw-target contract again before executable
        // verification materializes or accounts any layout path.
        let raw_path = require_usr_relative_stone_layout(&package, &layout)?;
        let path = PathBuf::from(materialized_frozen_layout_path(raw_path));
        let Some(path_str) = path.to_str() else {
            return Err(Error::InvalidFrozenLayoutPath {
                package,
                path: path.to_string_lossy().into_owned(),
            });
        };
        if path_str.len() > MAX_FROZEN_EXECUTABLE_PATH_BYTES || !is_normalized_frozen_path(path_str) {
            return Err(Error::InvalidFrozenLayoutPath {
                package,
                path: path_str.to_owned(),
            });
        }
        let auxiliary_bytes = frozen_executable_layout_auxiliary_bytes(&layout.file);
        account_frozen_layout_bytes(
            &package,
            &path,
            package
                .as_str()
                .len()
                .saturating_add(path_str.len())
                .saturating_add(auxiliary_bytes),
            &mut layout_bytes,
        )?;
        let is_directory = matches!(layout.file, StonePayloadLayoutFile::Directory(_));
        let entry = match layout.file {
            StonePayloadLayoutFile::Regular(digest, _) => FrozenExecutableLayout::Regular {
                digest,
                mode: layout.mode,
            },
            StonePayloadLayoutFile::Symlink(target, _) => {
                require_frozen_layout_symlink_target(&package, &target)?;
                FrozenExecutableLayout::Symlink {
                    target: target.to_string(),
                    mode: layout.mode,
                }
            }
            StonePayloadLayoutFile::Directory(_) => FrozenExecutableLayout::Directory {
                uid: layout.uid,
                gid: layout.gid,
                mode: layout.mode,
                tag: layout.tag,
            },
            StonePayloadLayoutFile::CharacterDevice(_)
            | StonePayloadLayoutFile::BlockDevice(_)
            | StonePayloadLayoutFile::Fifo(_)
            | StonePayloadLayoutFile::Socket(_)
            | StonePayloadLayoutFile::Unknown(..) => FrozenExecutableLayout::Other,
        };
        prepared_layouts.push(PreparedFrozenExecutableLayout {
            package,
            path,
            entry,
            is_directory,
        });
    }

    let directory_redirects = frozen_executable_directory_redirects(&prepared_layouts, deadline)?;
    let mut provider_layouts = BTreeMap::<package::Id, BTreeMap<PathBuf, FrozenExecutableLayout>>::new();
    let mut path_providers = BTreeMap::<PathBuf, BTreeSet<package::Id>>::new();
    for PreparedFrozenExecutableLayout {
        package,
        path,
        entry,
        is_directory: _,
    } in prepared_layouts
    {
        require_frozen_executable_deadline(deadline)?;
        let layouts = provider_layouts.entry(package.clone()).or_default();
        if let Some(previous) = layouts.get(&path) {
            if previous.is_identical_directory(&entry) {
                continue;
            }
            return Err(Error::DuplicateFrozenExecutableLayout { package, path });
        }
        layouts.insert(path.clone(), entry);
        path_providers.entry(path).or_default().insert(package);
    }

    let mut expected = BTreeMap::<(package::Id, PathBuf), ExpectedFrozenExecutable>::new();
    for binding in &bindings {
        require_frozen_executable_deadline(deadline)?;
        let executable = resolve_frozen_executable_layout(
            binding,
            &provider_layouts,
            &path_providers,
            &directory_redirects,
            deadline,
        )?;
        expected.insert((binding.package.clone(), binding.path.clone()), executable);
    }

    let mut total_bytes = 0u64;
    let mut pinned_file_count = 0usize;
    let mut verified = BTreeMap::<FrozenExecutableBinding, Option<FrozenExecutableInterpreter>>::new();
    let mut pinned = Vec::<PinnedFrozenExecutable>::new();
    let mut pinned_root_aliases = BTreeMap::<PathBuf, PinnedFrozenRootAlias>::new();

    for declared in &bindings {
        let key = (declared.package.clone(), declared.path.clone());
        let mut binding = declared.clone();
        let mut executable = expected
            .get(&key)
            .cloned()
            .ok_or_else(|| Error::MissingFrozenExecutableLayout {
                package: declared.package.clone(),
                path: declared.path.clone(),
            })?;
        let mut chain = BTreeSet::<FrozenExecutableBinding>::new();
        let mut shebang_interpreter_count = 0usize;
        let mut interpreter_count = 0usize;
        let mut require_terminal_elf = false;

        loop {
            if !chain.insert(binding.clone()) {
                return Err(Error::FrozenExecutableInterpreterCycle {
                    package: binding.package,
                    path: binding.path,
                });
            }

            let interpreter = if let Some(interpreter) = verified.get(&binding) {
                interpreter.clone()
            } else {
                reserve_frozen_pinned_files(&binding, &mut pinned_file_count, executable.symlinks.len() + 1)?;
                let (interpreter, retained) =
                    verify_frozen_executable(&root, &binding, executable, deadline, &mut total_bytes, &mut checkpoint)?;
                verified.insert(binding.clone(), interpreter.clone());
                pinned.push(retained);
                interpreter
            };

            if require_terminal_elf && interpreter.is_some() {
                return Err(Error::FrozenElfInterpreterIsInterpreted {
                    package: binding.package,
                    path: binding.path,
                });
            }

            let Some(interpreter_kind) = interpreter else {
                break;
            };
            let interpreter = interpreter_kind.binding();
            if let Some(alias) = interpreter.root_alias.clone()
                && !pinned_root_aliases.contains_key(&alias.path)
            {
                require_frozen_executable_deadline(deadline)?;
                reserve_frozen_pinned_files(declared, &mut pinned_file_count, 1)?;
                let pinned = pin_frozen_root_alias(&root, &alias)?;
                pinned_root_aliases.insert(alias.path.clone(), pinned);
            }
            interpreter_count = interpreter_count.saturating_add(1);
            require_frozen_executable_interpreter_count(declared, interpreter_count)?;
            if interpreter_kind.is_shebang() {
                shebang_interpreter_count = shebang_interpreter_count.saturating_add(1);
                require_frozen_shebang_interpreter_count(declared, shebang_interpreter_count)?;
            }
            require_terminal_elf = matches!(interpreter_kind, FrozenExecutableInterpreter::Elf(_));
            (binding, executable) = resolve_frozen_interpreter_layout(
                &interpreter.path,
                &provider_layouts,
                &path_providers,
                &directory_redirects,
                deadline,
            )?;
        }
    }

    // Keep every inspected inode pinned in the returned proof. Revalidate the
    // complete descriptor/name graph before handing it to the caller so a
    // writer racing a later interpreter cannot invalidate an earlier proof.
    let guard = FrozenRootGuard {
        root_path,
        root,
        root_witness,
        executables: pinned,
        root_aliases: pinned_root_aliases,
    };
    guard.revalidate_until(deadline)?;
    Ok(guard)
}

fn resolve_frozen_executable_layout(
    binding: &FrozenExecutableBinding,
    provider_layouts: &BTreeMap<package::Id, BTreeMap<PathBuf, FrozenExecutableLayout>>,
    path_providers: &BTreeMap<PathBuf, BTreeSet<package::Id>>,
    directory_redirects: &BTreeMap<PathBuf, PathBuf>,
    deadline: Instant,
) -> Result<ExpectedFrozenExecutable, Error> {
    let mut current = binding.path.clone();
    // Capability resolution pins the owner of the declared entry point.  A
    // symlink may intentionally hand off to another package in the exact
    // frozen closure (for example `cp -> gnu-cp`), but every later hop must
    // have one unambiguous owner.  We never resolve through the host or choose
    // a provider by iteration order.
    let mut provider = binding.package.clone();
    let mut visited = BTreeSet::new();
    let mut symlinks = Vec::new();
    loop {
        reject_frozen_executable_directory_redirect(&current, directory_redirects, deadline)?;
        require_frozen_executable_deadline(deadline)?;
        if !visited.insert(current.clone()) {
            return Err(Error::FrozenExecutableSymlinkCycle {
                package: binding.package.clone(),
                path: current,
            });
        }
        let Some(layout) = provider_layouts
            .get(&provider)
            .and_then(|layouts| layouts.get(&current))
        else {
            if symlinks.is_empty() {
                return Err(Error::MissingFrozenExecutableLayout {
                    package: binding.package.clone(),
                    path: binding.path.clone(),
                });
            }
            return Err(Error::MissingFrozenExecutableSymlinkTarget {
                package: binding.package.clone(),
                binding: binding.path.clone(),
                target: current,
            });
        };
        match layout {
            FrozenExecutableLayout::Regular { digest, mode } => {
                if mode & nix::libc::S_IFMT != nix::libc::S_IFREG || mode & 0o111 == 0 {
                    return Err(Error::FrozenExecutableLayoutNotExecutable {
                        package: provider,
                        path: current,
                        mode: *mode,
                    });
                }
                return Ok(ExpectedFrozenExecutable {
                    digest: *digest,
                    mode: *mode,
                    resolved_path: current,
                    symlinks,
                });
            }
            FrozenExecutableLayout::Symlink { target, mode } => {
                if symlinks.len() == MAX_FROZEN_EXECUTABLE_SYMLINKS {
                    return Err(Error::FrozenExecutableSymlinkLimit {
                        package: binding.package.clone(),
                        path: binding.path.clone(),
                        limit: MAX_FROZEN_EXECUTABLE_SYMLINKS,
                    });
                }
                if mode & nix::libc::S_IFMT != nix::libc::S_IFLNK || mode & 0o7777 != 0o777 {
                    return Err(Error::FrozenExecutableLayoutNotRegular {
                        package: provider,
                        path: current,
                    });
                }
                let next = resolve_frozen_symlink_target(&current, target).ok_or_else(|| {
                    Error::InvalidFrozenExecutableSymlinkTarget {
                        package: provider.clone(),
                        path: current.clone(),
                        target: target.clone(),
                    }
                })?;
                symlinks.push(ExpectedFrozenSymlink {
                    package: provider,
                    path: current,
                    target: target.clone(),
                    mode: *mode,
                });
                current = next;
                let providers =
                    path_providers
                        .get(&current)
                        .ok_or_else(|| Error::MissingFrozenExecutableSymlinkTarget {
                            package: binding.package.clone(),
                            binding: binding.path.clone(),
                            target: current.clone(),
                        })?;
                if providers.is_empty() {
                    return Err(Error::MissingFrozenExecutableSymlinkTarget {
                        package: binding.package.clone(),
                        binding: binding.path.clone(),
                        target: current,
                    });
                }
                if providers.len() > 1 {
                    return Err(Error::AmbiguousFrozenExecutableSymlinkTarget {
                        package: binding.package.clone(),
                        binding: binding.path.clone(),
                        target: current,
                        providers: providers.iter().cloned().collect(),
                    });
                }
                provider =
                    providers
                        .iter()
                        .next()
                        .cloned()
                        .ok_or_else(|| Error::MissingFrozenExecutableSymlinkTarget {
                            package: binding.package.clone(),
                            binding: binding.path.clone(),
                            target: current.clone(),
                        })?;
            }
            FrozenExecutableLayout::Directory { .. } | FrozenExecutableLayout::Other => {
                return Err(Error::FrozenExecutableLayoutNotRegular {
                    package: provider,
                    path: current,
                });
            }
        }
    }
}

fn resolve_frozen_interpreter_layout(
    interpreter: &Path,
    provider_layouts: &BTreeMap<package::Id, BTreeMap<PathBuf, FrozenExecutableLayout>>,
    path_providers: &BTreeMap<PathBuf, BTreeSet<package::Id>>,
    directory_redirects: &BTreeMap<PathBuf, PathBuf>,
    deadline: Instant,
) -> Result<(FrozenExecutableBinding, ExpectedFrozenExecutable), Error> {
    let mut current = interpreter.to_owned();
    let mut visited = BTreeSet::new();
    let mut symlinks = Vec::new();
    let mut initial_provider = None;

    loop {
        reject_frozen_executable_directory_redirect(&current, directory_redirects, deadline)?;
        require_frozen_executable_deadline(deadline)?;
        if !visited.insert(current.clone()) {
            return Err(Error::FrozenInterpreterSymlinkCycle { path: current });
        }
        let providers = path_providers
            .get(&current)
            .ok_or_else(|| Error::MissingFrozenInterpreterProvider { path: current.clone() })?;
        if providers.is_empty() {
            return Err(Error::MissingFrozenInterpreterProvider { path: current });
        }
        if providers.len() > 1 {
            return Err(Error::AmbiguousFrozenInterpreterProvider {
                path: current,
                providers: providers.iter().cloned().collect(),
            });
        }
        let provider = providers
            .iter()
            .next()
            .cloned()
            .ok_or_else(|| Error::MissingFrozenInterpreterProvider { path: current.clone() })?;
        initial_provider.get_or_insert_with(|| provider.clone());
        let layout = provider_layouts
            .get(&provider)
            .and_then(|layouts| layouts.get(&current))
            .ok_or_else(|| Error::MissingFrozenInterpreterProvider { path: current.clone() })?;

        match layout {
            FrozenExecutableLayout::Regular { digest, mode } => {
                if mode & nix::libc::S_IFMT != nix::libc::S_IFREG || mode & 0o111 == 0 {
                    return Err(Error::FrozenExecutableLayoutNotExecutable {
                        package: provider,
                        path: current,
                        mode: *mode,
                    });
                }
                let binding = FrozenExecutableBinding {
                    package: provider.clone(),
                    path: interpreter.to_owned(),
                };
                return Ok((
                    binding,
                    ExpectedFrozenExecutable {
                        digest: *digest,
                        mode: *mode,
                        resolved_path: current,
                        symlinks,
                    },
                ));
            }
            FrozenExecutableLayout::Symlink { target, mode } => {
                if symlinks.len() == MAX_FROZEN_EXECUTABLE_SYMLINKS {
                    return Err(Error::FrozenExecutableSymlinkLimit {
                        package: initial_provider.clone().unwrap_or_else(|| provider.clone()),
                        path: interpreter.to_owned(),
                        limit: MAX_FROZEN_EXECUTABLE_SYMLINKS,
                    });
                }
                if mode & nix::libc::S_IFMT != nix::libc::S_IFLNK || mode & 0o7777 != 0o777 {
                    return Err(Error::FrozenExecutableLayoutNotRegular {
                        package: provider,
                        path: current,
                    });
                }
                let next = resolve_frozen_symlink_target(&current, target).ok_or_else(|| {
                    Error::InvalidFrozenExecutableSymlinkTarget {
                        package: provider.clone(),
                        path: current.clone(),
                        target: target.clone(),
                    }
                })?;
                symlinks.push(ExpectedFrozenSymlink {
                    package: provider,
                    path: current,
                    target: target.clone(),
                    mode: *mode,
                });
                current = next;
            }
            FrozenExecutableLayout::Directory { .. } | FrozenExecutableLayout::Other => {
                return Err(Error::FrozenExecutableLayoutNotRegular {
                    package: provider,
                    path: current,
                });
            }
        }
    }
}

fn frozen_executable_directory_redirects(
    layouts: &[PreparedFrozenExecutableLayout],
    deadline: Instant,
) -> Result<BTreeMap<PathBuf, PathBuf>, Error> {
    let mut directories = BTreeSet::new();
    let mut directory_bytes = 0usize;
    for layout in layouts {
        require_frozen_executable_deadline(deadline)?;
        let mut parent = layout.path.parent();
        while let Some(path) = parent {
            require_frozen_executable_deadline(deadline)?;
            insert_frozen_executable_directory(path, &mut directories, &mut directory_bytes)?;
            parent = path.parent();
        }
    }
    for layout in layouts {
        require_frozen_executable_deadline(deadline)?;
        if layout.is_directory {
            insert_frozen_executable_directory(&layout.path, &mut directories, &mut directory_bytes)?;
        } else if directories.remove(&layout.path) {
            directory_bytes = directory_bytes.saturating_sub(layout.path.as_os_str().len());
        }
    }

    let mut redirects = BTreeMap::new();
    for layout in layouts {
        require_frozen_executable_deadline(deadline)?;
        let FrozenExecutableLayout::Symlink { target, .. } = &layout.entry else {
            continue;
        };
        let Some(target) = resolve_frozen_symlink_target(&layout.path, target) else {
            continue;
        };
        if directories.contains(&target) {
            redirects.insert(layout.path.clone(), target);
        }
    }
    Ok(redirects)
}

fn insert_frozen_executable_directory(
    path: &Path,
    directories: &mut BTreeSet<PathBuf>,
    total_bytes: &mut usize,
) -> Result<(), Error> {
    if directories.contains(path) {
        return Ok(());
    }
    require_frozen_executable_directory_count(directories.len().saturating_add(1))?;
    account_frozen_executable_directory_bytes(path.as_os_str().len(), total_bytes)?;
    directories.insert(path.to_owned());
    Ok(())
}

fn reject_frozen_executable_directory_redirect(
    path: &Path,
    redirects: &BTreeMap<PathBuf, PathBuf>,
    deadline: Instant,
) -> Result<(), Error> {
    let mut ancestor = path.parent();
    while let Some(source) = ancestor {
        require_frozen_executable_deadline(deadline)?;
        if let Some(target) = redirects.get(source) {
            return Err(Error::FrozenExecutableDirectoryRedirect {
                path: path.to_owned(),
                redirect_source: Box::new(source.to_owned()),
                target: Box::new(target.clone()),
            });
        }
        ancestor = source.parent();
    }
    Ok(())
}

fn parse_frozen_shebang(probe: &[u8]) -> Result<Option<FrozenShebangInterpreter>, FrozenShebangParseError> {
    if !probe.starts_with(b"#!") {
        return Ok(None);
    }
    let newline = probe.iter().position(|byte| *byte == b'\n').ok_or({
        if probe.len() > MAX_FROZEN_SHEBANG_LINE_BYTES {
            FrozenShebangParseError::LineTooLong
        } else {
            FrozenShebangParseError::Unterminated
        }
    })?;
    if newline + 1 > MAX_FROZEN_SHEBANG_LINE_BYTES {
        return Err(FrozenShebangParseError::LineTooLong);
    }
    let interpreter = &probe[2..newline];
    if interpreter.is_empty() {
        return Err(FrozenShebangParseError::EmptyInterpreter);
    }
    if interpreter.len() > MAX_FROZEN_SHEBANG_INTERPRETER_BYTES {
        return Err(FrozenShebangParseError::InterpreterTooLong);
    }
    if interpreter.contains(&0) {
        return Err(FrozenShebangParseError::Nul);
    }
    if interpreter.iter().any(|byte| byte.is_ascii_whitespace()) {
        return Err(FrozenShebangParseError::WhitespaceOrOptions);
    }
    if interpreter.first() != Some(&b'/') {
        return Err(FrozenShebangParseError::Relative);
    }
    let interpreter = std::str::from_utf8(interpreter).map_err(|_| FrozenShebangParseError::NonUtf8)?;
    let interpreter = normalize_frozen_interpreter_path(interpreter).ok_or(FrozenShebangParseError::NonNormalized)?;
    if interpreter.path == Path::new("/usr/bin/env") {
        return Err(FrozenShebangParseError::EnvironmentLookup);
    }
    Ok(Some(interpreter))
}

fn normalize_frozen_interpreter_path(path: &str) -> Option<FrozenShebangInterpreter> {
    if !path.starts_with('/')
        || path.as_bytes().contains(&0)
        || path.ends_with('/')
        || path.contains("//")
        || path.split('/').any(|component| component == "." || component == "..")
    {
        return None;
    }

    let mut canonical = path.to_owned();
    let mut root_alias = None;
    for (source, target) in ROOT_ABI_LINKS {
        let alias = format!("/{target}");
        if path == alias || path.strip_prefix(&alias).is_some_and(|suffix| suffix.starts_with('/')) {
            canonical = format!("/{source}{}", &path[alias.len()..]);
            root_alias = Some(ExpectedFrozenRootAlias {
                path: PathBuf::from(alias),
                target: source.to_owned(),
            });
            break;
        }
    }
    is_normalized_frozen_path(&canonical).then(|| FrozenShebangInterpreter {
        path: PathBuf::from(canonical),
        root_alias,
    })
}

fn inspect_frozen_executable_format(
    file: &fs::File,
    length: u64,
    probe: &[u8],
    deadline: Instant,
    binding: &FrozenExecutableBinding,
) -> Result<Option<FrozenExecutableInterpreter>, Error> {
    require_frozen_executable_deadline(deadline)?;
    if probe.starts_with(b"#!") {
        return parse_frozen_shebang(probe)
            .map(|interpreter| interpreter.map(FrozenExecutableInterpreter::Shebang))
            .map_err(|source| Error::InvalidFrozenShebang {
                package: binding.package.clone(),
                path: binding.path.clone(),
                reason: source.reason(),
            });
    }
    if !probe.starts_with(b"\x7fELF") {
        return Err(invalid_frozen_executable_format(
            binding,
            "unsupported executable magic; only strict ELF and shebang scripts are admitted",
        ));
    }
    inspect_frozen_elf(file, length, probe, deadline, binding)
}

fn inspect_frozen_elf(
    file: &fs::File,
    length: u64,
    probe: &[u8],
    deadline: Instant,
    binding: &FrozenExecutableBinding,
) -> Result<Option<FrozenExecutableInterpreter>, Error> {
    const ELFCLASS32: u8 = 1;
    const ELFCLASS64: u8 = 2;
    const ELFDATA2LSB: u8 = 1;
    const ELFDATA2MSB: u8 = 2;
    const ET_EXEC: u16 = 2;
    const ET_DYN: u16 = 3;
    const PT_LOAD: u32 = 1;
    const PT_INTERP: u32 = 3;
    const PF_X: u32 = 1;

    require_frozen_executable_deadline(deadline)?;
    if probe.len() < 16 || probe.get(6) != Some(&1) {
        return Err(invalid_frozen_executable_format(
            binding,
            "invalid ELF identification header",
        ));
    }
    let class = probe[4];
    let data = probe[5];
    let expected_class = if usize::BITS == 64 { ELFCLASS64 } else { ELFCLASS32 };
    let expected_data = if cfg!(target_endian = "little") {
        ELFDATA2LSB
    } else {
        ELFDATA2MSB
    };
    if class != expected_class || data != expected_data {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF class or byte order does not match the build host",
        ));
    }
    let little_endian = data == ELFDATA2LSB;
    let (header_size, program_header_size) = match class {
        ELFCLASS32 => (52usize, 32usize),
        ELFCLASS64 => (64usize, 56usize),
        _ => {
            return Err(invalid_frozen_executable_format(binding, "unsupported ELF class"));
        }
    };
    if length < header_size as u64 || probe.len() < header_size {
        return Err(invalid_frozen_executable_format(binding, "truncated ELF header"));
    }

    let elf_type = frozen_elf_u16(probe, 16, little_endian);
    let machine = frozen_elf_u16(probe, 18, little_endian);
    let version = frozen_elf_u32(probe, 20, little_endian);
    if !matches!(elf_type, Some(ET_EXEC) | Some(ET_DYN)) || version != Some(1) {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF is not an executable or position-independent executable",
        ));
    }
    let Some(machine) = machine else {
        return Err(invalid_frozen_executable_format(binding, "truncated ELF machine field"));
    };
    if Some(machine) != native_frozen_elf_machine() {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF machine does not match the build host",
        ));
    }

    let (entry, program_offset, encoded_header_size, encoded_program_header_size, program_count) =
        if class == ELFCLASS64 {
            (
                frozen_elf_u64(probe, 24, little_endian),
                frozen_elf_u64(probe, 32, little_endian),
                frozen_elf_u16(probe, 52, little_endian),
                frozen_elf_u16(probe, 54, little_endian),
                frozen_elf_u16(probe, 56, little_endian),
            )
        } else {
            (
                frozen_elf_u32(probe, 24, little_endian).map(u64::from),
                frozen_elf_u32(probe, 28, little_endian).map(u64::from),
                frozen_elf_u16(probe, 40, little_endian),
                frozen_elf_u16(probe, 42, little_endian),
                frozen_elf_u16(probe, 44, little_endian),
            )
        };
    let (
        Some(entry),
        Some(program_offset),
        Some(encoded_header_size),
        Some(encoded_program_header_size),
        Some(program_count),
    ) = (
        entry,
        program_offset,
        encoded_header_size,
        encoded_program_header_size,
        program_count,
    )
    else {
        return Err(invalid_frozen_executable_format(binding, "truncated ELF header fields"));
    };
    let program_count = usize::from(program_count);
    if program_count > MAX_FROZEN_ELF_PROGRAM_HEADERS {
        return Err(Error::FrozenElfProgramHeaderLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_FROZEN_ELF_PROGRAM_HEADERS,
            actual: program_count,
        });
    }
    if program_count == 0
        || usize::from(encoded_header_size) != header_size
        || usize::from(encoded_program_header_size) != program_header_size
        || program_offset < header_size as u64
    {
        return Err(invalid_frozen_executable_format(
            binding,
            "invalid ELF program-header geometry",
        ));
    }
    let program_bytes = program_count
        .checked_mul(program_header_size)
        .ok_or_else(|| invalid_frozen_executable_format(binding, "ELF program-header table overflows"))?;
    let program_end = program_offset
        .checked_add(program_bytes as u64)
        .ok_or_else(|| invalid_frozen_executable_format(binding, "ELF program-header table overflows"))?;
    if program_end > length {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF program-header table extends beyond the file",
        ));
    }
    let mut program_headers = vec![0u8; program_bytes];
    read_frozen_executable_at(file, program_offset, &mut program_headers, deadline, binding)?;

    let mut load_segments = 0usize;
    let mut executable_entry = false;
    let mut interpreter_segment = None;
    for header in program_headers.chunks_exact(program_header_size) {
        require_frozen_executable_deadline(deadline)?;
        let segment_type = frozen_elf_u32(header, 0, little_endian)
            .ok_or_else(|| invalid_frozen_executable_format(binding, "truncated ELF program header"))?;
        let (flags, offset, virtual_address, file_size, memory_size, alignment) = if class == ELFCLASS64 {
            (
                frozen_elf_u32(header, 4, little_endian),
                frozen_elf_u64(header, 8, little_endian),
                frozen_elf_u64(header, 16, little_endian),
                frozen_elf_u64(header, 32, little_endian),
                frozen_elf_u64(header, 40, little_endian),
                frozen_elf_u64(header, 48, little_endian),
            )
        } else {
            (
                frozen_elf_u32(header, 24, little_endian),
                frozen_elf_u32(header, 4, little_endian).map(u64::from),
                frozen_elf_u32(header, 8, little_endian).map(u64::from),
                frozen_elf_u32(header, 16, little_endian).map(u64::from),
                frozen_elf_u32(header, 20, little_endian).map(u64::from),
                frozen_elf_u32(header, 28, little_endian).map(u64::from),
            )
        };
        let (Some(flags), Some(offset), Some(virtual_address), Some(file_size), Some(memory_size), Some(alignment)) =
            (flags, offset, virtual_address, file_size, memory_size, alignment)
        else {
            return Err(invalid_frozen_executable_format(
                binding,
                "truncated ELF program header",
            ));
        };
        let segment_end = offset
            .checked_add(file_size)
            .ok_or_else(|| invalid_frozen_executable_format(binding, "ELF segment range overflows"))?;
        if segment_end > length || (alignment > 1 && !alignment.is_power_of_two()) {
            return Err(invalid_frozen_executable_format(
                binding,
                "invalid ELF segment bounds or alignment",
            ));
        }
        if segment_type == PT_LOAD {
            if alignment > 1 && offset % alignment != virtual_address % alignment {
                return Err(invalid_frozen_executable_format(binding, "misaligned ELF load mapping"));
            }
            if memory_size < file_size {
                return Err(invalid_frozen_executable_format(
                    binding,
                    "ELF load segment is smaller in memory than in the file",
                ));
            }
            load_segments = load_segments.saturating_add(1);
            let memory_end = virtual_address
                .checked_add(memory_size)
                .ok_or_else(|| invalid_frozen_executable_format(binding, "ELF memory range overflows"))?;
            if flags & PF_X != 0 && entry >= virtual_address && entry < memory_end {
                executable_entry = true;
            }
        }
        if segment_type == PT_INTERP {
            if interpreter_segment.replace((offset, file_size)).is_some() {
                return Err(invalid_frozen_executable_format(
                    binding,
                    "ELF has multiple PT_INTERP segments",
                ));
            }
        }
    }
    if load_segments == 0 || !executable_entry {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF has no executable load segment containing its entry point",
        ));
    }

    let Some((interpreter_offset, interpreter_size)) = interpreter_segment else {
        return Ok(None);
    };
    let interpreter_size = usize::try_from(interpreter_size)
        .map_err(|_| invalid_frozen_executable_format(binding, "ELF PT_INTERP size does not fit in memory"))?;
    if !(2..=MAX_FROZEN_ELF_INTERPRETER_BYTES).contains(&interpreter_size) {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF PT_INTERP path has an invalid length",
        ));
    }
    let mut interpreter = vec![0u8; interpreter_size];
    read_frozen_executable_at(file, interpreter_offset, &mut interpreter, deadline, binding)?;
    if interpreter.last() != Some(&0) || interpreter[..interpreter.len() - 1].contains(&0) {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF PT_INTERP is not one NUL-terminated path",
        ));
    }
    interpreter.pop();
    let interpreter = std::str::from_utf8(&interpreter)
        .ok()
        .and_then(normalize_frozen_interpreter_path)
        .ok_or_else(|| {
            invalid_frozen_executable_format(binding, "ELF PT_INTERP path is not absolute and normalized")
        })?;
    if interpreter.path == Path::new("/usr/bin/env") {
        return Err(invalid_frozen_executable_format(
            binding,
            "ELF PT_INTERP environment lookup is forbidden",
        ));
    }
    Ok(Some(FrozenExecutableInterpreter::Elf(interpreter)))
}

fn invalid_frozen_executable_format(binding: &FrozenExecutableBinding, reason: &'static str) -> Error {
    Error::InvalidFrozenExecutableFormat {
        package: binding.package.clone(),
        path: binding.path.clone(),
        reason,
    }
}

fn native_frozen_elf_machine() -> Option<u16> {
    match std::env::consts::ARCH {
        "x86" => Some(3),
        "mips" | "mips64" => Some(8),
        "powerpc" => Some(20),
        "powerpc64" => Some(21),
        "s390x" => Some(22),
        "arm" => Some(40),
        "x86_64" => Some(62),
        "aarch64" => Some(183),
        "riscv32" | "riscv64" => Some(243),
        _ => None,
    }
}

fn frozen_elf_u16(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u16> {
    let bytes: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(if little_endian {
        u16::from_le_bytes(bytes)
    } else {
        u16::from_be_bytes(bytes)
    })
}

fn frozen_elf_u32(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u32> {
    let bytes: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(if little_endian {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    })
}

fn frozen_elf_u64(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u64> {
    let bytes: [u8; 8] = bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(if little_endian {
        u64::from_le_bytes(bytes)
    } else {
        u64::from_be_bytes(bytes)
    })
}

fn read_frozen_executable_at(
    file: &fs::File,
    offset: u64,
    output: &mut [u8],
    deadline: Instant,
    binding: &FrozenExecutableBinding,
) -> Result<(), Error> {
    let mut read = 0usize;
    while read < output.len() {
        require_frozen_executable_deadline(deadline)?;
        let position = offset
            .checked_add(read as u64)
            .and_then(|position| i64::try_from(position).ok())
            .ok_or_else(|| invalid_frozen_executable_format(binding, "ELF read offset overflows"))?;
        // SAFETY: `file` remains live, `output[read..]` is writable, and the
        // checked offset is representable as off_t on supported Linux hosts.
        let result = unsafe {
            nix::libc::pread(
                file.as_raw_fd(),
                output[read..].as_mut_ptr().cast(),
                output.len() - read,
                position,
            )
        };
        if result < 0 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(Error::ReadFrozenExecutable {
                package: binding.package.clone(),
                path: binding.path.clone(),
                source,
            });
        }
        let result = usize::try_from(result).map_err(|_| Error::ReadFrozenExecutable {
            package: binding.package.clone(),
            path: binding.path.clone(),
            source: io::Error::other("pread returned a negative byte count"),
        })?;
        if result == 0 {
            return Err(invalid_frozen_executable_format(
                binding,
                "ELF changed or ended during inspection",
            ));
        }
        read += result;
    }
    Ok(())
}

fn verify_frozen_executable<F>(
    root: &fs::File,
    binding: &FrozenExecutableBinding,
    expected: ExpectedFrozenExecutable,
    deadline: Instant,
    total_bytes: &mut u64,
    checkpoint: &mut F,
) -> Result<(Option<FrozenExecutableInterpreter>, PinnedFrozenExecutable), Error>
where
    F: FnMut(&FrozenExecutableBinding, FrozenExecutableCheckpoint),
{
    require_frozen_executable_deadline(deadline)?;
    let pinned_symlinks = expected
        .symlinks
        .iter()
        .map(|symlink| pin_frozen_symlink(root, symlink))
        .collect::<Result<Vec<_>, Error>>()?;
    let mut file = open_frozen_executable(root, binding, &expected.resolved_path)?;
    let before = frozen_executable_witness(&file, binding)?;
    require_frozen_executable_metadata(binding, &expected, before)?;
    account_frozen_executable_bytes(binding, before.length, total_bytes)?;

    checkpoint(binding, FrozenExecutableCheckpoint::AfterOpen);
    let inspected = digest_frozen_executable(&mut file, before.length, deadline, binding)?;
    checkpoint(binding, FrozenExecutableCheckpoint::AfterDigest);
    let after = frozen_executable_witness(&file, binding)?;
    if after != before {
        return Err(Error::FrozenExecutableChanged {
            package: binding.package.clone(),
            path: binding.path.clone(),
        });
    }
    if inspected.digest != expected.digest {
        return Err(Error::FrozenExecutableDigestMismatch {
            package: binding.package.clone(),
            path: binding.path.clone(),
            expected: expected.digest,
            actual: inspected.digest,
        });
    }
    let interpreter =
        inspect_frozen_executable_format(&file, before.length, &inspected.shebang_probe, deadline, binding)?;

    checkpoint(binding, FrozenExecutableCheckpoint::BeforeReopen);
    let reopened = open_frozen_executable(root, binding, &expected.resolved_path)?;
    let named = frozen_executable_witness(&reopened, binding)?;
    if named != before {
        return Err(Error::FrozenExecutablePathReplaced {
            package: binding.package.clone(),
            path: binding.path.clone(),
        });
    }
    for symlink in &pinned_symlinks {
        require_pinned_frozen_symlink(root, symlink)?;
    }

    Ok((
        interpreter,
        PinnedFrozenExecutable {
            file,
            witness: before,
            binding: binding.clone(),
            expected,
            symlinks: pinned_symlinks,
        },
    ))
}

fn require_pinned_frozen_executable(root: &fs::File, pinned: &PinnedFrozenExecutable) -> Result<(), Error> {
    let descriptor = frozen_executable_witness(&pinned.file, &pinned.binding)?;
    let reopened = open_frozen_executable(root, &pinned.binding, &pinned.expected.resolved_path)?;
    let named = frozen_executable_witness(&reopened, &pinned.binding)?;
    if descriptor != pinned.witness || named != pinned.witness {
        return Err(Error::FrozenExecutablePathReplaced {
            package: pinned.binding.package.clone(),
            path: pinned.binding.path.clone(),
        });
    }
    for symlink in &pinned.symlinks {
        require_pinned_frozen_symlink(root, symlink)?;
    }
    Ok(())
}

fn pin_frozen_root_alias(root: &fs::File, expected: &ExpectedFrozenRootAlias) -> Result<PinnedFrozenRootAlias, Error> {
    let file = open_frozen_root_alias(root, &expected.path)?;
    let witness = frozen_root_alias_witness(&file, &expected.path)?;
    if witness.mode & nix::libc::S_IFMT != nix::libc::S_IFLNK || witness.mode & 0o7777 != 0o777 || witness.links != 1 {
        return Err(Error::FrozenInterpreterRootAliasMetadata {
            path: expected.path.clone(),
            mode: witness.mode,
            links: witness.links,
        });
    }
    let target = read_frozen_root_alias(&file, &expected.path)?;
    if target.as_os_str().as_bytes() != expected.target.as_bytes() {
        return Err(Error::FrozenInterpreterRootAliasTarget {
            path: expected.path.clone(),
            expected: expected.target.clone(),
            actual: target,
        });
    }
    Ok(PinnedFrozenRootAlias {
        file,
        witness,
        expected: expected.clone(),
    })
}

fn require_pinned_frozen_root_alias(root: &fs::File, pinned: &PinnedFrozenRootAlias) -> Result<(), Error> {
    let descriptor = frozen_root_alias_witness(&pinned.file, &pinned.expected.path)?;
    let reopened = open_frozen_root_alias(root, &pinned.expected.path)?;
    let named = frozen_root_alias_witness(&reopened, &pinned.expected.path)?;
    let descriptor_target = read_frozen_root_alias(&pinned.file, &pinned.expected.path)?;
    let named_target = read_frozen_root_alias(&reopened, &pinned.expected.path)?;
    if descriptor != pinned.witness
        || named != pinned.witness
        || descriptor_target.as_os_str().as_bytes() != pinned.expected.target.as_bytes()
        || named_target.as_os_str().as_bytes() != pinned.expected.target.as_bytes()
    {
        return Err(Error::FrozenInterpreterRootAliasChanged {
            path: pinned.expected.path.clone(),
        });
    }
    Ok(())
}

fn open_frozen_root_alias(root: &fs::File, path: &Path) -> Result<fs::File, Error> {
    let relative = path
        .strip_prefix(Path::new("/"))
        .map_err(|_| Error::InvalidFrozenInterpreterRootAlias { path: path.to_owned() })?;
    openat2_frozen(
        root.as_raw_fd(),
        relative,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV,
    )
    .map_err(|source| Error::OpenFrozenInterpreterRootAlias {
        path: path.to_owned(),
        source,
    })
}

fn frozen_root_alias_witness(file: &fs::File, path: &Path) -> Result<FrozenExecutableWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenExecutableWitness::from_metadata(&metadata))
        .map_err(|source| Error::StatFrozenInterpreterRootAlias {
            path: path.to_owned(),
            source,
        })
}

fn read_frozen_root_alias(file: &fs::File, path: &Path) -> Result<OsString, Error> {
    let mut target = [0_u8; MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1];
    // SAFETY: the O_PATH descriptor pins the exact symlink and the output
    // buffer is writable for its complete length.
    let read =
        unsafe { nix::libc::readlinkat(file.as_raw_fd(), c"".as_ptr(), target.as_mut_ptr().cast(), target.len()) };
    if read < 0 {
        return Err(Error::ReadFrozenInterpreterRootAlias {
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| Error::ReadFrozenInterpreterRootAlias {
        path: path.to_owned(),
        source: io::Error::other("readlinkat returned a negative size"),
    })?;
    if read > MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES {
        return Err(Error::FrozenInterpreterRootAliasTargetTooLong {
            path: path.to_owned(),
            limit: MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES,
            actual: read,
        });
    }
    Ok(OsString::from_vec(target[..read].to_vec()))
}

fn require_frozen_executable_binding_count(actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_EXECUTABLE_BINDINGS {
        Err(Error::FrozenExecutableBindingLimit {
            limit: MAX_FROZEN_EXECUTABLE_BINDINGS,
            actual,
        })
    } else {
        Ok(())
    }
}

fn require_frozen_executable_path(binding: &FrozenExecutableBinding) -> Result<&str, Error> {
    let raw = binding.path.as_os_str().as_bytes();
    if raw.len() > MAX_FROZEN_EXECUTABLE_PATH_BYTES {
        return Err(Error::FrozenExecutablePathByteLimit {
            limit: MAX_FROZEN_EXECUTABLE_PATH_BYTES,
            actual: raw.len(),
        });
    }

    let components = raw
        .split(|byte| *byte == b'/')
        .filter(|component| !component.is_empty())
        .count();
    if components > MAX_FROZEN_LAYOUT_PATH_COMPONENTS {
        return Err(Error::FrozenExecutablePathDepthLimit {
            limit: MAX_FROZEN_LAYOUT_PATH_COMPONENTS,
            actual: components,
        });
    }

    let path = std::str::from_utf8(raw).map_err(|_| Error::FrozenExecutablePathEncoding { bytes: raw.len() })?;
    if require_materialized_frozen_path_policy(path).is_err() || !is_normalized_frozen_path(path) {
        // The path is known to be bounded before it is copied into this
        // diagnostic. Oversized or non-UTF-8 inputs never reach this branch.
        return Err(Error::InvalidFrozenExecutablePath {
            package: binding.package.clone(),
            path: binding.path.clone(),
        });
    }
    Ok(path)
}

fn require_frozen_executable_package_count(actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_EXECUTABLE_PACKAGES {
        Err(Error::FrozenExecutablePackageLimit {
            limit: MAX_FROZEN_EXECUTABLE_PACKAGES,
            actual,
        })
    } else {
        Ok(())
    }
}

fn account_frozen_closure_id_bytes(package: &package::Id, total: &mut usize) -> Result<(), Error> {
    let actual = total.checked_add(package.as_str().len()).unwrap_or(usize::MAX);
    if actual > MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES {
        return Err(Error::FrozenExecutableClosureIdByteLimit {
            limit: MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES,
            actual,
        });
    }
    *total = actual;
    Ok(())
}

fn account_frozen_binding_bytes(
    binding: &FrozenExecutableBinding,
    additional: usize,
    total: &mut usize,
) -> Result<(), Error> {
    let actual = total.checked_add(additional).unwrap_or(usize::MAX);
    if actual > MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES {
        return Err(Error::FrozenExecutableBindingByteLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES,
            actual,
        });
    }
    *total = actual;
    Ok(())
}

fn require_frozen_executable_layout_count(actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_EXECUTABLE_LAYOUTS {
        Err(Error::FrozenExecutableLayoutLimit {
            limit: MAX_FROZEN_EXECUTABLE_LAYOUTS,
            actual,
        })
    } else {
        Ok(())
    }
}

fn account_frozen_layout_bytes(
    package: &package::Id,
    path: &Path,
    additional: usize,
    total: &mut usize,
) -> Result<(), Error> {
    let actual = total.checked_add(additional).unwrap_or(usize::MAX);
    if actual > MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES {
        return Err(Error::FrozenExecutableLayoutByteLimit {
            package: package.clone(),
            path: path.to_owned(),
            limit: MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES,
            actual,
        });
    }
    *total = actual;
    Ok(())
}

fn require_frozen_executable_directory_count(actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS {
        Err(Error::FrozenExecutableDirectoryLimit {
            limit: MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS,
            actual,
        })
    } else {
        Ok(())
    }
}

fn account_frozen_executable_directory_bytes(additional: usize, total: &mut usize) -> Result<(), Error> {
    let actual = total.checked_add(additional).unwrap_or(usize::MAX);
    if actual > MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES {
        return Err(Error::FrozenExecutableDirectoryByteLimit {
            limit: MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES,
            actual,
        });
    }
    *total = actual;
    Ok(())
}

fn frozen_executable_layout_auxiliary_bytes(file: &StonePayloadLayoutFile) -> usize {
    match file {
        StonePayloadLayoutFile::Symlink(target, _) => target.len(),
        StonePayloadLayoutFile::Unknown(source, _) => source.len(),
        StonePayloadLayoutFile::Regular(..)
        | StonePayloadLayoutFile::Directory(_)
        | StonePayloadLayoutFile::CharacterDevice(_)
        | StonePayloadLayoutFile::BlockDevice(_)
        | StonePayloadLayoutFile::Fifo(_)
        | StonePayloadLayoutFile::Socket(_) => 0,
    }
}

fn require_frozen_shebang_interpreter_count(binding: &FrozenExecutableBinding, actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_SHEBANG_INTERPRETERS {
        Err(Error::FrozenShebangInterpreterLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_FROZEN_SHEBANG_INTERPRETERS,
        })
    } else {
        Ok(())
    }
}

fn require_frozen_executable_interpreter_count(binding: &FrozenExecutableBinding, actual: usize) -> Result<(), Error> {
    if actual > MAX_FROZEN_EXECUTABLE_INTERPRETERS {
        Err(Error::FrozenExecutableInterpreterLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_FROZEN_EXECUTABLE_INTERPRETERS,
        })
    } else {
        Ok(())
    }
}

fn reserve_frozen_pinned_files(
    binding: &FrozenExecutableBinding,
    current: &mut usize,
    additional: usize,
) -> Result<(), Error> {
    let actual = current.saturating_add(additional);
    if actual > MAX_FROZEN_EXECUTABLE_PINNED_FILES {
        return Err(Error::FrozenExecutablePinnedFileLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_FROZEN_EXECUTABLE_PINNED_FILES,
            actual,
        });
    }
    *current = actual;
    Ok(())
}

fn account_frozen_executable_bytes(
    binding: &FrozenExecutableBinding,
    length: u64,
    total: &mut u64,
) -> Result<(), Error> {
    if length > MAX_FROZEN_EXECUTABLE_BYTES {
        return Err(Error::FrozenExecutableByteLimit {
            package: binding.package.clone(),
            path: binding.path.clone(),
            limit: MAX_FROZEN_EXECUTABLE_BYTES,
            actual: length,
        });
    }
    let next = total.checked_add(length).ok_or(Error::FrozenExecutableTotalByteLimit {
        limit: MAX_TOTAL_FROZEN_EXECUTABLE_BYTES,
        actual: u64::MAX,
    })?;
    if next > MAX_TOTAL_FROZEN_EXECUTABLE_BYTES {
        return Err(Error::FrozenExecutableTotalByteLimit {
            limit: MAX_TOTAL_FROZEN_EXECUTABLE_BYTES,
            actual: next,
        });
    }
    *total = next;
    Ok(())
}

fn resolve_frozen_symlink_target(link: &Path, target: &str) -> Option<PathBuf> {
    if target.is_empty()
        || !frozen_executable_symlink_target_length_is_admitted(target.len())
        || target.as_bytes().contains(&0)
        || target.ends_with('/')
        || target.contains("//")
    {
        return None;
    }

    let target_path = Path::new(target);
    let mut components = Vec::<OsString>::new();
    if !target_path.is_absolute() {
        for component in link.parent()?.components() {
            match component {
                std::path::Component::RootDir => {}
                std::path::Component::Normal(component) => components.push(component.to_owned()),
                std::path::Component::CurDir | std::path::Component::ParentDir | std::path::Component::Prefix(_) => {
                    return None;
                }
            }
        }
    }
    for component in target_path.components() {
        match component {
            std::path::Component::RootDir => {
                if !target_path.is_absolute() {
                    return None;
                }
                components.clear();
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                components.pop()?;
            }
            std::path::Component::Normal(component) => components.push(component.to_owned()),
            std::path::Component::Prefix(_) => return None,
        }
    }

    let mut resolved = PathBuf::from("/");
    resolved.extend(components);
    if resolved.to_str().is_some_and(is_normalized_frozen_path) {
        Some(resolved)
    } else {
        None
    }
}

fn frozen_executable_symlink_target_length_is_admitted(actual: usize) -> bool {
    actual <= MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES
}

fn pin_frozen_symlink(root: &fs::File, expected: &ExpectedFrozenSymlink) -> Result<PinnedFrozenSymlink, Error> {
    let binding = FrozenExecutableBinding {
        package: expected.package.clone(),
        path: expected.path.clone(),
    };
    let file = open_frozen_symlink(root, &binding, &expected.path)?;
    let witness = frozen_symlink_witness(&file, &binding, &expected.path)?;
    if witness.mode != expected.mode || witness.mode & nix::libc::S_IFMT != nix::libc::S_IFLNK || witness.links != 1 {
        return Err(Error::FrozenExecutableSymlinkMetadataMismatch {
            package: binding.package.clone(),
            path: expected.path.clone(),
            expected: expected.mode,
            actual: witness.mode,
            links: witness.links,
        });
    }
    let actual = read_frozen_symlink(&file, &binding, &expected.path)?;
    if actual.as_os_str().as_bytes() != expected.target.as_bytes() {
        return Err(Error::FrozenExecutableSymlinkTargetMismatch {
            package: binding.package.clone(),
            path: expected.path.clone(),
            expected: expected.target.clone(),
            actual,
        });
    }
    Ok(PinnedFrozenSymlink {
        file,
        witness,
        expected: expected.clone(),
    })
}

fn require_pinned_frozen_symlink(root: &fs::File, pinned: &PinnedFrozenSymlink) -> Result<(), Error> {
    let binding = FrozenExecutableBinding {
        package: pinned.expected.package.clone(),
        path: pinned.expected.path.clone(),
    };
    let descriptor = frozen_symlink_witness(&pinned.file, &binding, &pinned.expected.path)?;
    let reopened = open_frozen_symlink(root, &binding, &pinned.expected.path)?;
    let named = frozen_symlink_witness(&reopened, &binding, &pinned.expected.path)?;
    if descriptor != pinned.witness || named != pinned.witness {
        return Err(Error::FrozenExecutableSymlinkChanged {
            package: binding.package.clone(),
            path: pinned.expected.path.clone(),
        });
    }
    let descriptor_target = read_frozen_symlink(&pinned.file, &binding, &pinned.expected.path)?;
    let named_target = read_frozen_symlink(&reopened, &binding, &pinned.expected.path)?;
    if descriptor_target.as_os_str().as_bytes() != pinned.expected.target.as_bytes()
        || named_target.as_os_str().as_bytes() != pinned.expected.target.as_bytes()
    {
        return Err(Error::FrozenExecutableSymlinkChanged {
            package: binding.package.clone(),
            path: pinned.expected.path.clone(),
        });
    }
    Ok(())
}

fn require_frozen_executable_metadata(
    binding: &FrozenExecutableBinding,
    expected: &ExpectedFrozenExecutable,
    witness: FrozenExecutableWitness,
) -> Result<(), Error> {
    if witness.mode & nix::libc::S_IFMT != nix::libc::S_IFREG || witness.links != 1 {
        return Err(Error::FrozenExecutableNotIndependentRegular {
            package: binding.package.clone(),
            path: binding.path.clone(),
            mode: witness.mode,
            links: witness.links,
        });
    }
    if witness.mode != expected.mode || witness.mode & 0o111 == 0 {
        return Err(Error::FrozenExecutableModeMismatch {
            package: binding.package.clone(),
            path: binding.path.clone(),
            expected: expected.mode,
            actual: witness.mode,
        });
    }
    Ok(())
}

fn open_frozen_root_anchor(root: &Path) -> Result<fs::File, Error> {
    open_frozen_root_anchor_with_deadline(root, None)
}

/// Retain the exact directory selected as the frozen publication namespace.
///
/// `Client::frozen` canonicalizes the authored parent once, then opens it from
/// the filesystem root without following symlinks.  Every later staging,
/// publication, and discard operation is relative to this descriptor; the
/// pathname is retained only so we can fail closed if the public namespace is
/// renamed or replaced while the client is alive.
fn open_frozen_destination_parent(parent: &Path) -> Result<fs::File, Error> {
    let relative = parent
        .strip_prefix(Path::new("/"))
        .ok()
        .filter(|relative| {
            relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        })
        .ok_or_else(|| Error::InvalidFrozenRootDestination(parent.to_owned()))?;
    let system_root = fs::File::open("/").map_err(|source| Error::OpenFrozenRootDestinationParent {
        path: parent.to_owned(),
        source,
    })?;
    let relative = if relative.as_os_str().is_empty() {
        Path::new(".")
    } else {
        relative
    };
    let resolution =
        (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64;
    let pinned = openat2_frozen(
        system_root.as_raw_fd(),
        relative,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        resolution,
    )
    .map_err(|source| Error::OpenFrozenRootDestinationParent {
        path: parent.to_owned(),
        source,
    })?;
    let readable = openat2_frozen(
        system_root.as_raw_fd(),
        relative,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        resolution,
    )
    .map_err(|source| Error::OpenFrozenRootDestinationParent {
        path: parent.to_owned(),
        source,
    })?;
    if frozen_root_identity(&pinned, parent)? != frozen_root_identity(&readable, parent)? {
        return Err(Error::FrozenRootDestinationParentChanged(parent.to_owned()));
    }
    Ok(readable)
}

fn require_frozen_destination_parent(destination: &FrozenRootDestination) -> Result<(), Error> {
    let retained = frozen_root_identity(&destination.parent, &destination.parent_path)?;
    let named = open_frozen_destination_parent(&destination.parent_path)?;
    if retained != destination.parent_identity
        || frozen_root_identity(&named, &destination.parent_path)? != destination.parent_identity
    {
        return Err(Error::FrozenRootDestinationParentChanged(
            destination.parent_path.clone(),
        ));
    }
    Ok(())
}

fn lock_frozen_destination_until(
    destination: &FrozenRootDestination,
    deadline: Instant,
) -> Result<FrozenDestinationLock, Error> {
    require_frozen_materialization_deadline(deadline)?;
    require_frozen_destination_parent(destination)?;
    let directory = openat2_frozen_until(
        destination.parent.as_raw_fd(),
        Path::new("."),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::LockFrozenRootDestinationParent {
            path: destination.parent_path.clone(),
            source,
        })
    })?;
    if frozen_root_identity(&directory, &destination.parent_path)? != destination.parent_identity {
        return Err(Error::FrozenRootDestinationParentChanged(
            destination.parent_path.clone(),
        ));
    }

    let mut interruptions = 0usize;
    loop {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: directory remains open for the guard's lifetime. LOCK_NB
        // keeps the materialization deadline observable while another
        // cooperating Forge process owns this namespace.
        if unsafe { nix::libc::flock(directory.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) } == 0 {
            break;
        }
        let source = io::Error::last_os_error();
        match source.raw_os_error() {
            Some(code) if code == nix::libc::EWOULDBLOCK || code == nix::libc::EAGAIN => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    require_frozen_materialization_deadline(deadline)?;
                }
                std::thread::sleep(remaining.min(FROZEN_DESTINATION_LOCK_RETRY));
            }
            Some(nix::libc::EINTR) if interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS => {
                interruptions += 1;
            }
            _ => {
                return Err(Error::LockFrozenRootDestinationParent {
                    path: destination.parent_path.clone(),
                    source,
                });
            }
        }
    }
    require_frozen_destination_parent(destination)?;
    Ok(FrozenDestinationLock { _directory: directory })
}

fn open_frozen_root_anchor_until(root: &Path, deadline: Instant) -> Result<fs::File, Error> {
    open_frozen_root_anchor_with_deadline(root, Some(deadline))
}

fn open_frozen_root_anchor_with_deadline(root: &Path, deadline: Option<Instant>) -> Result<fs::File, Error> {
    let relative = root
        .strip_prefix(Path::new("/"))
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .filter(|relative| {
            relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        })
        .ok_or_else(|| Error::InvalidFrozenExecutableRoot(root.to_owned()))?;
    let system_root = fs::File::open("/").map_err(|source| Error::OpenFrozenExecutableRoot {
        path: root.to_owned(),
        source,
    })?;
    let opened = match deadline {
        Some(deadline) => openat2_frozen_until(
            system_root.as_raw_fd(),
            relative,
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
            (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
            deadline,
        ),
        None => openat2_frozen(
            system_root.as_raw_fd(),
            relative,
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
            (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
        ),
    };
    opened.map_err(|source| match deadline {
        Some(deadline) => frozen_materialization_io_error(deadline, source, |source| Error::OpenFrozenExecutableRoot {
            path: root.to_owned(),
            source,
        }),
        None => Error::OpenFrozenExecutableRoot {
            path: root.to_owned(),
            source,
        },
    })
}

fn frozen_root_anchor_witness(file: &fs::File, path: &Path) -> Result<FrozenExecutableWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenExecutableWitness::from_metadata(&metadata))
        .map_err(|source| Error::StatFrozenExecutableRoot {
            path: path.to_owned(),
            source,
        })
}

fn frozen_root_identity(file: &fs::File, path: &Path) -> Result<FrozenRootIdentity, Error> {
    file.metadata()
        .map(|metadata| FrozenRootIdentity::from_metadata(&metadata))
        .map_err(|source| Error::StatFrozenExecutableRoot {
            path: path.to_owned(),
            source,
        })
}

fn require_materialized_frozen_root(path: &Path, pinned: &fs::File, expected: FrozenRootIdentity) -> Result<(), Error> {
    let descriptor = frozen_root_identity(pinned, path)?;
    let Ok(reopened) = open_frozen_root_anchor(path) else {
        return Err(Error::MaterializedFrozenRootReplaced(path.to_owned()));
    };
    let named = frozen_root_identity(&reopened, path)?;
    if descriptor != expected || named != expected {
        return Err(Error::MaterializedFrozenRootReplaced(path.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
fn test_materialized_frozen_root(path: &Path) -> Result<MaterializedFrozenRoot, Error> {
    let root = open_frozen_root_anchor(path)?;
    let identity = frozen_root_identity(&root, path)?;
    Ok(MaterializedFrozenRoot {
        root_path: path.to_owned(),
        root,
        identity,
    })
}

fn require_pinned_frozen_root_anchor(
    path: &Path,
    pinned: &fs::File,
    expected: FrozenExecutableWitness,
) -> Result<(), Error> {
    let descriptor = frozen_root_anchor_witness(pinned, path)?;
    let Ok(reopened) = open_frozen_root_anchor(path) else {
        return Err(Error::FrozenExecutableRootReplaced(path.to_owned()));
    };
    let named = frozen_root_anchor_witness(&reopened, path)?;
    if descriptor != expected || named != expected {
        return Err(Error::FrozenExecutableRootReplaced(path.to_owned()));
    }
    Ok(())
}

fn open_frozen_executable(
    root: &fs::File,
    binding: &FrozenExecutableBinding,
    resolved_path: &Path,
) -> Result<fs::File, Error> {
    let relative = resolved_path
        .strip_prefix(Path::new("/"))
        .map_err(|_| Error::InvalidFrozenExecutablePath {
            package: binding.package.clone(),
            path: binding.path.clone(),
        })?;
    openat2_frozen(
        root.as_raw_fd(),
        relative,
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
    )
    .map_err(|source| Error::OpenFrozenExecutable {
        package: binding.package.clone(),
        path: binding.path.clone(),
        source,
    })
}

fn open_frozen_symlink(root: &fs::File, binding: &FrozenExecutableBinding, path: &Path) -> Result<fs::File, Error> {
    let relative = path
        .strip_prefix(Path::new("/"))
        .map_err(|_| Error::InvalidFrozenExecutablePath {
            package: binding.package.clone(),
            path: path.to_owned(),
        })?;
    openat2_frozen(
        root.as_raw_fd(),
        relative,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
    )
    .map_err(|source| Error::OpenFrozenExecutableSymlink {
        package: binding.package.clone(),
        path: path.to_owned(),
        source,
    })
}

fn openat2_frozen(dirfd: RawFd, path: &Path, flags: i32, resolve: u64) -> io::Result<fs::File> {
    let display_path = path.to_owned();
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let file = openat2_file(dirfd, &path, flags, 0, resolve)?;
    Ok(fs::File::from_parts(file, display_path))
}

fn openat2_frozen_until(
    dirfd: RawFd,
    path: &Path,
    flags: i32,
    resolve: u64,
    deadline: Instant,
) -> io::Result<fs::File> {
    let display_path = path.to_owned();
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let file = openat2_file_until(dirfd, &path, flags, 0, resolve, deadline)?;
    Ok(fs::File::from_parts(file, display_path))
}

fn frozen_executable_witness(
    file: &fs::File,
    binding: &FrozenExecutableBinding,
) -> Result<FrozenExecutableWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenExecutableWitness::from_metadata(&metadata))
        .map_err(|source| Error::StatFrozenExecutable {
            package: binding.package.clone(),
            path: binding.path.clone(),
            source,
        })
}

fn frozen_symlink_witness(
    file: &fs::File,
    binding: &FrozenExecutableBinding,
    path: &Path,
) -> Result<FrozenExecutableWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenExecutableWitness::from_metadata(&metadata))
        .map_err(|source| Error::StatFrozenExecutableSymlink {
            package: binding.package.clone(),
            path: path.to_owned(),
            source,
        })
}

fn read_frozen_symlink(file: &fs::File, binding: &FrozenExecutableBinding, path: &Path) -> Result<OsString, Error> {
    let mut target = [0_u8; MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1];
    // Linux reads the exact symlink pinned by an O_PATH|O_NOFOLLOW descriptor
    // when readlinkat receives an empty relative path.
    // SAFETY: `file` is live and `target` is writable for its complete length.
    let read =
        unsafe { nix::libc::readlinkat(file.as_raw_fd(), c"".as_ptr(), target.as_mut_ptr().cast(), target.len()) };
    if read < 0 {
        return Err(Error::ReadFrozenExecutableSymlink {
            package: binding.package.clone(),
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| Error::ReadFrozenExecutableSymlink {
        package: binding.package.clone(),
        path: path.to_owned(),
        source: io::Error::other("readlinkat returned a negative size"),
    })?;
    if read > MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES {
        return Err(Error::FrozenExecutableSymlinkTargetTooLong {
            package: binding.package.clone(),
            path: path.to_owned(),
            limit: MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES,
            actual: read,
        });
    }
    Ok(OsString::from_vec(target[..read].to_vec()))
}

fn digest_frozen_executable(
    file: &mut fs::File,
    expected_length: u64,
    deadline: Instant,
    binding: &FrozenExecutableBinding,
) -> Result<FrozenExecutableDigest, Error> {
    let mut hasher = StoneDigestWriterHasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut actual = 0u64;
    let mut shebang_probe = Vec::with_capacity(MAX_FROZEN_SHEBANG_LINE_BYTES + 1);
    loop {
        require_frozen_executable_deadline(deadline)?;
        let read = file.read(&mut buffer).map_err(|source| Error::ReadFrozenExecutable {
            package: binding.package.clone(),
            path: binding.path.clone(),
            source,
        })?;
        if read == 0 {
            break;
        }
        actual = actual
            .checked_add(read as u64)
            .ok_or_else(|| Error::FrozenExecutableLengthChanged {
                package: binding.package.clone(),
                path: binding.path.clone(),
                expected: expected_length,
                actual: u64::MAX,
            })?;
        if actual > expected_length {
            return Err(Error::FrozenExecutableLengthChanged {
                package: binding.package.clone(),
                path: binding.path.clone(),
                expected: expected_length,
                actual,
            });
        }
        hasher.update(&buffer[..read]);
        let remaining = (MAX_FROZEN_SHEBANG_LINE_BYTES + 1).saturating_sub(shebang_probe.len());
        shebang_probe.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    if actual != expected_length {
        return Err(Error::FrozenExecutableLengthChanged {
            package: binding.package.clone(),
            path: binding.path.clone(),
            expected: expected_length,
            actual,
        });
    }
    Ok(FrozenExecutableDigest {
        digest: hasher.digest128(),
        shebang_probe,
    })
}

fn require_frozen_executable_deadline(deadline: Instant) -> Result<(), Error> {
    if Instant::now() > deadline {
        Err(Error::FrozenExecutableVerificationTimeout {
            seconds: FROZEN_EXECUTABLE_VERIFICATION_TIMEOUT.as_secs(),
        })
    } else {
        Ok(())
    }
}

fn require_frozen_materialization_deadline(deadline: Instant) -> Result<(), Error> {
    if Instant::now() > deadline {
        Err(Error::FrozenMaterializationTimeout {
            seconds: FROZEN_MATERIALIZATION_TIMEOUT.as_secs(),
        })
    } else {
        Ok(())
    }
}

fn frozen_namespace_recovery_deadline() -> Instant {
    Instant::now() + FROZEN_NAMESPACE_RECOVERY_TIMEOUT
}

fn frozen_materialization_io_error(
    deadline: Instant,
    source: io::Error,
    map: impl FnOnce(io::Error) -> Error,
) -> Error {
    match require_frozen_materialization_deadline(deadline) {
        Err(timeout) => timeout,
        Ok(()) => map(source),
    }
}

fn require_blit_deadline(deadline: Option<Instant>) -> Result<(), Error> {
    if let Some(deadline) = deadline {
        require_frozen_materialization_deadline(deadline)?;
    }
    Ok(())
}

const ROOT_ABI_LINKS: [(&str, &str); 5] = [
    ("usr/sbin", "sbin"),
    ("usr/bin", "bin"),
    ("usr/lib", "lib"),
    ("usr/lib", "lib64"),
    ("usr/lib32", "lib32"),
];

/// Establish the stable merged-/usr root ABI links without replacing anything.
fn create_root_links(root: &Path) -> Result<RetainedRootAbi, Error> {
    create_root_links_with(root, |_| {}, |directory| directory.sync_all())
}

/// Establish the merged-/usr ABI through an already-retained root descriptor.
/// The path is diagnostic and is used only to prove that the retained inode is
/// still publicly named; no write authority is reacquired through it.
pub(super) fn create_root_links_retained(root: &Path, retained: &std::fs::File) -> Result<RetainedRootAbi, Error> {
    RootAbiPreflight::open_retained(root, retained)?.publish()
}

/// Inspect every stable and legacy staging name without mutating the root.
///
/// Stateful activation performs this half before candidate identity preparation
/// so a static foreign occupant leaves the already-materialized candidate and
/// its allocated database row unchanged.
/// Publication is deliberately separate: it runs only after the canonical
/// journal guard has proved a clean baseline, using this same retained root
/// descriptor rather than reopening the public pathname.
fn preflight_root_links(root: &Path) -> Result<RootAbiPreflight, Error> {
    RootAbiPreflight::open_with(root, &mut |_| {})
}

#[derive(Debug)]
struct RootAbiPreflight {
    root: PathBuf,
    directory: fs::File,
    identity: FrozenRootIdentity,
    links: Vec<(&'static str, &'static str, Option<PinnedRootAbiLink>)>,
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_STATEFUL_CANDIDATE_METADATA: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_STATEFUL_ROOT_ABI_PUBLICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_STATEFUL_ISOLATION_ROOT_RETENTION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_before_stateful_candidate_metadata(hook: impl FnOnce() + 'static) {
    BEFORE_STATEFUL_CANDIDATE_METADATA.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_stateful_candidate_metadata() {
    BEFORE_STATEFUL_CANDIDATE_METADATA.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
fn arm_before_stateful_root_abi_publication(hook: impl FnOnce() + 'static) {
    BEFORE_STATEFUL_ROOT_ABI_PUBLICATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_stateful_root_abi_publication() {
    BEFORE_STATEFUL_ROOT_ABI_PUBLICATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
fn arm_after_stateful_isolation_root_retention(hook: impl FnOnce() + 'static) {
    AFTER_STATEFUL_ISOLATION_ROOT_RETENTION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_stateful_isolation_root_retention() {
    AFTER_STATEFUL_ISOLATION_ROOT_RETENTION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

impl RootAbiPreflight {
    fn path(&self) -> &Path {
        &self.root
    }

    fn revalidate(&self) -> Result<(), Error> {
        require_root_abi_directory(&self.root, &self.directory, self.identity)?;
        for (source, target, pinned) in &self.links {
            require_root_abi_staging_absent(&self.directory, &self.root, target)?;
            match pinned {
                Some(pinned) => {
                    require_pinned_root_abi_link(&self.directory, &self.root, source, target, pinned)?;
                }
                None => {
                    if open_root_abi_entry(&self.directory, &self.root, target)?.is_some() {
                        return Err(Error::RootAbiLinkAppeared(self.root.join(target)));
                    }
                }
            }
        }
        require_root_abi_directory(&self.root, &self.directory, self.identity)
    }
}

/// Exact merged-/usr scratch-root capability established while the ABI links
/// are provisioned. Transaction containers must consume this retained inode;
/// reopening the public path would admit a replacement root after validation.
#[derive(Debug)]
pub(super) struct RetainedRootAbi {
    root: PathBuf,
    directory: fs::File,
    anchor: std::fs::File,
    identity: FrozenRootIdentity,
    links: Vec<(&'static str, &'static str, PinnedRootAbiLink)>,
}

impl RetainedRootAbi {
    pub(super) fn path(&self) -> &Path {
        &self.root
    }

    pub(super) fn directory(&self) -> &std::fs::File {
        &self.anchor
    }

    pub(super) fn revalidate(&self) -> Result<(), Error> {
        require_root_abi_directory(&self.root, &self.directory, self.identity)?;
        let anchor = self
            .anchor
            .metadata()
            .map(|metadata| FrozenRootIdentity::from_metadata(&metadata))
            .map_err(|source| Error::StatRootAbiDirectory {
                root: self.root.clone(),
                source,
            })?;
        if anchor != self.identity {
            return Err(Error::RootAbiDirectoryReplaced(self.root.clone()));
        }
        for (source, target, link) in &self.links {
            require_root_abi_staging_absent(&self.directory, &self.root, target)?;
            require_pinned_root_abi_link(&self.directory, &self.root, source, target, link)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootAbiLinkCheckpoint {
    RootOpened,
    PreflightComplete,
    AfterSync,
}

fn create_root_links_with<C, S>(root: &Path, mut checkpoint: C, mut sync: S) -> Result<RetainedRootAbi, Error>
where
    C: FnMut(RootAbiLinkCheckpoint),
    S: FnMut(&fs::File) -> io::Result<()>,
{
    RootAbiPreflight::open_with(root, &mut checkpoint)?.publish_with(&mut checkpoint, &mut sync)
}

impl RootAbiPreflight {
    fn open_with<C>(root: &Path, checkpoint: &mut C) -> Result<Self, Error>
    where
        C: FnMut(RootAbiLinkCheckpoint),
    {
        let root = absolute_root_abi_path(root)?;
        let directory = open_root_abi_directory(&root).map_err(|source| Error::OpenRootAbiDirectory {
            root: root.clone(),
            source,
        })?;
        Self::open_directory_with(root, directory, checkpoint)
    }

    fn open_retained(root: &Path, retained: &std::fs::File) -> Result<Self, Error> {
        let root = absolute_root_abi_path(root)?;
        let retained = retained.try_clone().map_err(|source| Error::OpenRootAbiDirectory {
            root: root.clone(),
            source,
        })?;
        let directory = fs::File::from_parts(retained, root.clone());
        Self::open_directory_with(root, directory, &mut |_| {})
    }

    fn open_directory_with<C>(root: PathBuf, directory: fs::File, checkpoint: &mut C) -> Result<Self, Error>
    where
        C: FnMut(RootAbiLinkCheckpoint),
    {
        let identity = root_abi_directory_identity(&directory).map_err(|source| Error::StatRootAbiDirectory {
            root: root.clone(),
            source,
        })?;
        checkpoint(RootAbiLinkCheckpoint::RootOpened);

        let mut links = Vec::with_capacity(ROOT_ABI_LINKS.len());
        for (source, target) in ROOT_ABI_LINKS {
            require_root_abi_staging_absent(&directory, &root, target)?;
            links.push((
                source,
                target,
                pin_root_abi_link(&directory, &root, source, target, true)?,
            ));
        }
        checkpoint(RootAbiLinkCheckpoint::PreflightComplete);

        Ok(Self {
            root,
            directory,
            identity,
            links,
        })
    }

    fn publish(self) -> Result<RetainedRootAbi, Error> {
        #[cfg(test)]
        before_stateful_root_abi_publication();
        self.publish_with(&mut |_| {}, &mut |directory| directory.sync_all())
    }

    fn publish_with<C, S>(mut self, checkpoint: &mut C, sync: &mut S) -> Result<RetainedRootAbi, Error>
    where
        C: FnMut(RootAbiLinkCheckpoint),
        S: FnMut(&fs::File) -> io::Result<()>,
    {
        for (source, target, pinned) in &mut self.links {
            let source = *source;
            let target = *target;
            if pinned.is_some() {
                continue;
            }
            match symlinkat(source, Some(self.directory.as_raw_fd()), target) {
                Ok(()) => {}
                Err(Errno::EEXIST) => {
                    // A concurrent creator is authenticated by the common pin
                    // below. Never replace or remove what won the race.
                }
                Err(error) => {
                    return Err(Error::CreateRootAbiLink {
                        path: self.root.join(target),
                        target: source.to_owned(),
                        source: io::Error::from_raw_os_error(error as i32),
                    });
                }
            }
            *pinned = pin_root_abi_link(&self.directory, &self.root, source, target, false)?;
        }

        // Always sync, including an idempotent no-op retry after a prior sync
        // failure, so every successful return is a durability boundary.
        sync(&self.directory).map_err(|source| Error::SyncRootAbiDirectory {
            root: self.root.clone(),
            source,
        })?;
        checkpoint(RootAbiLinkCheckpoint::AfterSync);

        // Revalidate the complete namespace after publication and sync. This
        // also detects `.next` or final-name races without cleaning them up.
        for (source, target, pinned) in &self.links {
            let source = *source;
            let target = *target;
            require_root_abi_staging_absent(&self.directory, &self.root, target)?;
            require_pinned_root_abi_link(
                &self.directory,
                &self.root,
                source,
                target,
                pinned.as_ref().expect("every root ABI link was pinned before sync"),
            )?;
        }
        require_root_abi_directory(&self.root, &self.directory, self.identity)?;
        let anchor = openat2_file(
            self.directory.as_raw_fd(),
            c".",
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            crate::linux_fs::controlled_resolution(),
        )
        .map_err(|source| Error::OpenRootAbiDirectory {
            root: self.root.clone(),
            source,
        })?;
        if anchor
            .metadata()
            .map(|metadata| FrozenRootIdentity::from_metadata(&metadata))
            .map_err(|source| Error::StatRootAbiDirectory {
                root: self.root.clone(),
                source,
            })?
            != self.identity
        {
            return Err(Error::RootAbiDirectoryReplaced(self.root));
        }
        Ok(RetainedRootAbi {
            root: self.root,
            directory: self.directory,
            anchor,
            identity: self.identity,
            links: self
                .links
                .into_iter()
                .map(|(source, target, link)| {
                    (
                        source,
                        target,
                        link.expect("every root ABI link was pinned before retention"),
                    )
                })
                .collect(),
        })
    }
}

fn absolute_root_abi_path(root: &Path) -> Result<PathBuf, Error> {
    if root.is_absolute() {
        Ok(root.to_owned())
    } else {
        std::env::current_dir()
            .map_err(|source| Error::OpenRootAbiDirectory {
                root: root.to_owned(),
                source,
            })
            .map(|current| current.join(root))
    }
}

fn open_root_abi_directory(root: &Path) -> io::Result<fs::File> {
    openat2_frozen(
        AT_FDCWD,
        root,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
    )
}

fn open_root_abi_entry(directory: &fs::File, root: &Path, name: &str) -> Result<Option<fs::File>, Error> {
    match openat2_frozen(
        directory.as_raw_fd(),
        Path::new(name),
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
    ) {
        Ok(entry) => Ok(Some(entry)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::InspectRootAbiEntry {
            path: root.join(name),
            source,
        }),
    }
}

fn require_root_abi_staging_absent(directory: &fs::File, root: &Path, target: &str) -> Result<(), Error> {
    let name = format!("{target}.next");
    let Some(entry) = open_root_abi_entry(directory, root, &name)? else {
        return Ok(());
    };
    let metadata = entry.metadata().map_err(|source| Error::InspectRootAbiEntry {
        path: root.join(&name),
        source,
    })?;
    let symlink_target = metadata
        .file_type()
        .is_symlink()
        .then(|| read_root_abi_symlink(&entry, &root.join(&name)))
        .transpose()?;
    Err(Error::RootAbiStagingConflict {
        path: root.join(name),
        actual_type: root_abi_entry_type(metadata.mode()),
        symlink_target,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RootAbiLinkWitness {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    owner: u32,
    group: u32,
    length: u64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl RootAbiLinkWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            owner: metadata.uid(),
            group: metadata.gid(),
            length: metadata.len(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
struct PinnedRootAbiLink {
    entry: fs::File,
    witness: RootAbiLinkWitness,
}

/// Return a retained exact link, or `None` only for an allowed absence.
fn pin_root_abi_link(
    directory: &fs::File,
    root: &Path,
    source: &'static str,
    target: &'static str,
    allow_missing: bool,
) -> Result<Option<PinnedRootAbiLink>, Error> {
    let path = root.join(target);
    let Some(entry) = open_root_abi_entry(directory, root, target)? else {
        return if allow_missing {
            Ok(None)
        } else {
            Err(Error::RootAbiLinkMissing {
                path,
                target: source.to_owned(),
            })
        };
    };
    let metadata = entry.metadata().map_err(|source| Error::InspectRootAbiEntry {
        path: path.clone(),
        source,
    })?;
    if !metadata.file_type().is_symlink() {
        return Err(Error::RootAbiLinkTypeConflict {
            path,
            target: source.to_owned(),
            actual_type: root_abi_entry_type(metadata.mode()),
        });
    }
    let actual = read_root_abi_symlink(&entry, &path)?;
    if actual.as_bytes() != source.as_bytes() {
        return Err(Error::RootAbiLinkTargetConflict {
            path,
            expected: source.to_owned(),
            actual,
        });
    }
    Ok(Some(PinnedRootAbiLink {
        witness: RootAbiLinkWitness::from_metadata(&metadata),
        entry,
    }))
}

fn require_pinned_root_abi_link(
    directory: &fs::File,
    root: &Path,
    source: &'static str,
    target: &'static str,
    expected: &PinnedRootAbiLink,
) -> Result<(), Error> {
    let path = root.join(target);
    let retained = expected
        .entry
        .metadata()
        .map(|metadata| RootAbiLinkWitness::from_metadata(&metadata))
        .map_err(|source| Error::InspectRootAbiEntry {
            path: path.clone(),
            source,
        })?;
    let named =
        pin_root_abi_link(directory, root, source, target, false)?.expect("a required root ABI link cannot be absent");
    if retained != expected.witness || named.witness != expected.witness {
        return Err(Error::RootAbiLinkReplaced(path));
    }
    Ok(())
}

fn read_root_abi_symlink(entry: &fs::File, path: &Path) -> Result<OsString, Error> {
    let mut target = vec![0_u8; nix::libc::PATH_MAX as usize + 1];
    // O_PATH|O_NOFOLLOW pins the symlink inode; an empty readlinkat path reads
    // that exact inode rather than resolving its public name a second time.
    // SAFETY: `entry` is live and `target` is writable for its full length.
    let read = unsafe {
        nix::libc::readlinkat(
            entry.as_raw_fd(),
            c"".as_ptr(),
            target.as_mut_ptr().cast(),
            target.len(),
        )
    };
    if read < 0 {
        return Err(Error::ReadRootAbiLink {
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    let read = usize::try_from(read).map_err(|_| Error::ReadRootAbiLink {
        path: path.to_owned(),
        source: io::Error::other("readlinkat returned a negative size"),
    })?;
    if read == target.len() {
        return Err(Error::RootAbiLinkTargetTooLong {
            path: path.to_owned(),
            limit: target.len() - 1,
        });
    }
    target.truncate(read);
    Ok(OsString::from_vec(target))
}

fn root_abi_entry_type(mode: u32) -> &'static str {
    match mode & nix::libc::S_IFMT {
        nix::libc::S_IFREG => "regular file",
        nix::libc::S_IFDIR => "directory",
        nix::libc::S_IFLNK => "symlink",
        nix::libc::S_IFIFO => "fifo",
        nix::libc::S_IFSOCK => "socket",
        nix::libc::S_IFCHR => "character device",
        nix::libc::S_IFBLK => "block device",
        _ => "unknown inode",
    }
}

fn root_abi_directory_identity(directory: &fs::File) -> io::Result<FrozenRootIdentity> {
    directory
        .metadata()
        .map(|metadata| FrozenRootIdentity::from_metadata(&metadata))
}

fn require_root_abi_directory(root: &Path, directory: &fs::File, expected: FrozenRootIdentity) -> Result<(), Error> {
    let retained = root_abi_directory_identity(directory).map_err(|source| Error::StatRootAbiDirectory {
        root: root.to_owned(),
        source,
    })?;
    let Ok(named) = open_root_abi_directory(root) else {
        return Err(Error::RootAbiDirectoryReplaced(root.to_owned()));
    };
    let named = root_abi_directory_identity(&named).map_err(|source| Error::StatRootAbiDirectory {
        root: root.to_owned(),
        source,
    })?;
    if retained != expected || named != expected {
        return Err(Error::RootAbiDirectoryReplaced(root.to_owned()));
    }
    Ok(())
}

/// Create only the stable root ABI links required by a frozen build root.
///
/// The root has just been recreated, so any pre-existing entry is an invariant
/// violation rather than something to merge or replace.
fn create_frozen_root_links(root: RawFd, deadline: Instant) -> Result<(), Error> {
    for (source, target) in ROOT_ABI_LINKS {
        require_frozen_materialization_deadline(deadline)?;
        symlinkat(source, Some(root), target)?;
    }
    Ok(())
}

#[derive(Debug)]
struct FrozenPrivateDirectory {
    name: CString,
    path: PathBuf,
    file: fs::File,
    identity: FrozenRootIdentity,
}

fn open_frozen_named_entry_until(
    parent: &fs::File,
    name: &CStr,
    path: &Path,
    deadline: Instant,
) -> Result<Option<fs::File>, Error> {
    let relative = Path::new(OsStr::from_bytes(name.to_bytes()));
    match openat2_frozen_until(
        parent.as_raw_fd(),
        relative,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    ) {
        Ok(file) => Ok(Some(file)),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
        Err(source) => Err(frozen_materialization_io_error(deadline, source, |source| {
            Error::InspectFrozenPublicationName {
                path: path.to_owned(),
                source,
            }
        })),
    }
}

fn frozen_named_identity_until(
    parent: &fs::File,
    name: &CStr,
    path: &Path,
    deadline: Instant,
) -> Result<Option<FrozenRootIdentity>, Error> {
    open_frozen_named_entry_until(parent, name, path, deadline)?
        .map(|file| frozen_root_identity(&file, path))
        .transpose()
}

fn random_frozen_private_name(prefix: &[u8], deadline: Instant) -> Result<CString, Error> {
    let mut random = [0_u8; FROZEN_PRIVATE_DIRECTORY_RANDOM_BYTES];
    let mut filled = 0usize;
    let mut interruptions = 0usize;
    while filled < random.len() {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: the remaining slice is writable for the supplied length.
        // GRND_NONBLOCK avoids an unbounded entropy wait inside a supposedly
        // finite materialization operation.
        let result = unsafe {
            syscall(
                nix::libc::SYS_getrandom,
                random[filled..].as_mut_ptr(),
                random.len() - filled,
                nix::libc::GRND_NONBLOCK,
            )
        };
        if result == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted && interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS {
                interruptions += 1;
                continue;
            }
            return Err(source.into());
        }
        let read = usize::try_from(result).map_err(|_| io::Error::other("getrandom returned an invalid length"))?;
        if read == 0 || read > random.len() - filled {
            return Err(
                io::Error::new(io::ErrorKind::UnexpectedEof, "getrandom returned an invalid short read").into(),
            );
        }
        filled += read;
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = Vec::with_capacity(prefix.len() + random.len() * 2);
    encoded.extend_from_slice(prefix);
    for byte in random {
        encoded.push(HEX[usize::from(byte >> 4)]);
        encoded.push(HEX[usize::from(byte & 0x0f)]);
    }
    CString::new(encoded).map_err(|_| io::Error::other("generated frozen private name contains NUL").into())
}

fn create_frozen_private_directory(
    destination: &FrozenRootDestination,
    prefix: &[u8],
    deadline: Instant,
) -> Result<FrozenPrivateDirectory, Error> {
    create_frozen_private_directory_with(destination, prefix, deadline, |_, _| Ok(()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrozenPrivateDirectoryCheckpoint {
    Retained,
    ModeNormalized,
    ReadableOpened,
    AclsChecked,
    InventoryVerified,
}

#[derive(Debug)]
struct ProvisionalFrozenPrivateDirectory {
    name: CString,
    path: PathBuf,
    pinned: fs::File,
    device: u64,
    inode: u64,
}

fn create_frozen_private_directory_with(
    destination: &FrozenRootDestination,
    prefix: &[u8],
    deadline: Instant,
    mut checkpoint: impl FnMut(FrozenPrivateDirectoryCheckpoint, &Path) -> Result<(), Error>,
) -> Result<FrozenPrivateDirectory, Error> {
    'attempts: for _ in 0..MAX_FROZEN_PRIVATE_DIRECTORY_ATTEMPTS {
        require_frozen_materialization_deadline(deadline)?;
        let name = random_frozen_private_name(prefix, deadline)?;
        let path = destination.parent_path.join(OsStr::from_bytes(name.to_bytes()));
        let mut interruptions = 0usize;
        loop {
            require_frozen_materialization_deadline(deadline)?;
            // SAFETY: the retained parent and generated single-component name
            // remain live. mkdirat never follows or replaces the final name.
            if unsafe { nix::libc::mkdirat(destination.parent.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
                break;
            }
            let source = io::Error::last_os_error();
            match source.kind() {
                io::ErrorKind::Interrupted if interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS => {
                    interruptions += 1;
                }
                io::ErrorKind::AlreadyExists => continue 'attempts,
                _ => {
                    return Err(Error::CreateFrozenPrivateDirectory { path, source });
                }
            }
        }

        // Once mkdirat has changed the namespace, retaining or cleaning that
        // exact residue is recovery work. It gets a fresh finite budget even
        // when ordinary materialization time expired immediately after mkdir.
        let provisional = match retain_provisional_frozen_private_directory(
            destination,
            &name,
            &path,
            frozen_namespace_recovery_deadline(),
        ) {
            Ok(Some(provisional)) => provisional,
            Ok(None) => continue 'attempts,
            Err(primary) => {
                let cleanup = retain_provisional_frozen_private_directory(
                    destination,
                    &name,
                    &path,
                    frozen_namespace_recovery_deadline(),
                );
                return match cleanup {
                    Ok(None) => Err(primary),
                    Ok(Some(provisional)) => {
                        match cleanup_provisional_frozen_private_directory(
                            destination,
                            &provisional,
                            frozen_namespace_recovery_deadline(),
                        ) {
                            Ok(()) => Err(primary),
                            Err(cleanup) => Err(Error::CleanupFrozenPrivateDirectory {
                                path: provisional.path,
                                primary: Box::new(primary),
                                cleanup: Box::new(cleanup),
                            }),
                        }
                    }
                    Err(cleanup) => Err(Error::CleanupFrozenPrivateDirectory {
                        path: destination.parent_path.clone(),
                        primary: Box::new(primary),
                        cleanup: Box::new(cleanup),
                    }),
                };
            }
        };

        let result = finish_frozen_private_directory(destination, provisional, deadline, &mut checkpoint);
        return match result {
            Ok(directory) => Ok(directory),
            Err((primary, provisional)) => {
                match cleanup_provisional_frozen_private_directory(
                    destination,
                    &provisional,
                    frozen_namespace_recovery_deadline(),
                ) {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(Error::CleanupFrozenPrivateDirectory {
                        path: provisional.path,
                        primary: Box::new(primary),
                        cleanup: Box::new(cleanup),
                    }),
                }
            }
        };
    }
    Err(Error::CreateFrozenPrivateDirectory {
        path: destination.parent_path.clone(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "failed to reserve a unique private directory after {MAX_FROZEN_PRIVATE_DIRECTORY_ATTEMPTS} attempts"
            ),
        ),
    })
}

fn retain_provisional_frozen_private_directory(
    destination: &FrozenRootDestination,
    name: &CStr,
    path: &Path,
    deadline: Instant,
) -> Result<Option<ProvisionalFrozenPrivateDirectory>, Error> {
    require_frozen_materialization_deadline(deadline)?;
    let resolution = (nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_XDEV) as u64;
    let relative = Path::new(OsStr::from_bytes(name.to_bytes()));
    let pinned = match openat2_frozen_until(
        destination.parent.as_raw_fd(),
        relative,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        resolution,
        deadline,
    ) {
        Ok(pinned) => pinned,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::OpenFrozenPrivateDirectory {
                path: path.to_owned(),
                source,
            });
        }
    };
    let parent_metadata = destination.parent.metadata()?;
    let metadata = pinned.metadata()?;
    let mode = metadata.mode() & 0o7777;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    // A setgid parent may cause the fresh child to inherit S_ISGID. It is a
    // valid creation residue and is cleared through the retained descriptor
    // before the wrapper becomes usable. No other extra permission is
    // accepted.
    if !metadata.is_dir()
        || metadata.uid() != effective_owner
        || metadata.dev() != parent_metadata.dev()
        || mode & !(0o700 | nix::libc::S_ISGID) != 0
    {
        return Err(Error::FrozenPrivateDirectoryChanged { path: path.to_owned() });
    }
    Ok(Some(ProvisionalFrozenPrivateDirectory {
        name: name.to_owned(),
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        pinned,
    }))
}

fn finish_frozen_private_directory(
    destination: &FrozenRootDestination,
    provisional: ProvisionalFrozenPrivateDirectory,
    deadline: Instant,
    checkpoint: &mut impl FnMut(FrozenPrivateDirectoryCheckpoint, &Path) -> Result<(), Error>,
) -> Result<FrozenPrivateDirectory, (Error, ProvisionalFrozenPrivateDirectory)> {
    let result = (|| -> Result<FrozenPrivateDirectory, Error> {
        require_frozen_materialization_deadline(deadline)?;
        checkpoint(FrozenPrivateDirectoryCheckpoint::Retained, &provisional.path)?;
        let resolution = (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64;
        let relative = Path::new(OsStr::from_bytes(provisional.name.to_bytes()));
        chmod_path_descriptor_until(provisional.pinned.file(), 0o700, deadline).map_err(|source| {
            frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
                path: provisional.path.clone(),
                source,
            })
        })?;
        checkpoint(FrozenPrivateDirectoryCheckpoint::ModeNormalized, &provisional.path)?;
        let readable = openat2_frozen_until(
            destination.parent.as_raw_fd(),
            relative,
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            resolution,
            deadline,
        )
        .map_err(|source| Error::OpenFrozenPrivateDirectory {
            path: provisional.path.clone(),
            source,
        })?;
        let identity = frozen_root_identity(&provisional.pinned, &provisional.path)?;
        if identity.device != provisional.device
            || identity.inode != provisional.inode
            || identity != frozen_root_identity(&readable, &provisional.path)?
            || identity.mode & 0o7777 != 0o700
        {
            return Err(Error::FrozenPrivateDirectoryChanged {
                path: provisional.path.clone(),
            });
        }
        checkpoint(FrozenPrivateDirectoryCheckpoint::ReadableOpened, &provisional.path)?;
        require_no_access_acl_until(readable.file(), &provisional.path, deadline).map_err(|source| {
            frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
                path: provisional.path.clone(),
                source,
            })
        })?;
        require_no_default_acl_until(readable.file(), &provisional.path, deadline).map_err(|source| {
            frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
                path: provisional.path.clone(),
                source,
            })
        })?;
        checkpoint(FrozenPrivateDirectoryCheckpoint::AclsChecked, &provisional.path)?;
        let mut entries = 0usize;
        if !frozen_discard_entry_names(readable.as_raw_fd(), &mut entries, deadline)?.is_empty() {
            return Err(Error::FrozenPrivateDirectoryChanged {
                path: provisional.path.clone(),
            });
        }
        checkpoint(FrozenPrivateDirectoryCheckpoint::InventoryVerified, &provisional.path)?;
        Ok(FrozenPrivateDirectory {
            name: provisional.name.clone(),
            path: provisional.path.clone(),
            file: readable,
            identity,
        })
    })();
    result.map_err(|error| (error, provisional))
}

fn cleanup_provisional_frozen_private_directory(
    destination: &FrozenRootDestination,
    provisional: &ProvisionalFrozenPrivateDirectory,
    deadline: Instant,
) -> Result<(), Error> {
    require_frozen_materialization_deadline(deadline)?;
    let Some(named) =
        open_frozen_named_entry_until(&destination.parent, &provisional.name, &provisional.path, deadline)?
    else {
        return Ok(());
    };
    let metadata = named.metadata()?;
    if metadata.dev() != provisional.device || metadata.ino() != provisional.inode || !metadata.is_dir() {
        return Err(Error::FrozenPrivateDirectoryChanged {
            path: provisional.path.clone(),
        });
    }
    let readable = openat2_frozen_until(
        destination.parent.as_raw_fd(),
        Path::new(OsStr::from_bytes(provisional.name.to_bytes())),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .map_err(|source| Error::OpenFrozenPrivateDirectory {
        path: provisional.path.clone(),
        source,
    })?;
    let readable_metadata = readable.metadata()?;
    if (readable_metadata.dev(), readable_metadata.ino()) != (provisional.device, provisional.inode) {
        return Err(Error::FrozenPrivateDirectoryChanged {
            path: provisional.path.clone(),
        });
    }
    let mut entries = 0usize;
    if !frozen_discard_entry_names(readable.as_raw_fd(), &mut entries, deadline)?.is_empty() {
        return Err(Error::FrozenPrivateDirectoryChanged {
            path: provisional.path.clone(),
        });
    }
    unlinkat(
        Some(destination.parent.as_raw_fd()),
        provisional.name.as_c_str(),
        UnlinkatFlags::RemoveDir,
    )?;
    if frozen_named_identity_until(&destination.parent, &provisional.name, &provisional.path, deadline)?.is_some() {
        return Err(Error::FrozenPrivateDirectoryChanged {
            path: provisional.path.clone(),
        });
    }
    sync_frozen_publication_file(
        &destination.parent,
        &destination.parent_path,
        "sync provisional frozen-root cleanup",
        deadline,
    )
}

fn publish_frozen_root(
    stage: &FrozenPrivateDirectory,
    destination: &FrozenRootDestination,
    staged_root: &fs::File,
    staged_root_anchor: fs::File,
    deadline: Instant,
) -> Result<MaterializedFrozenRoot, Error> {
    publish_frozen_root_with(
        stage,
        destination,
        staged_root,
        staged_root_anchor,
        deadline,
        |source_directory, source_name, destination_directory, destination_name| {
            renameat2_noreplace_until(
                source_directory.file(),
                source_name,
                destination_directory.file(),
                destination_name,
                deadline,
            )
        },
    )
}

fn publish_frozen_root_with(
    stage: &FrozenPrivateDirectory,
    destination: &FrozenRootDestination,
    staged_root: &fs::File,
    staged_root_anchor: fs::File,
    deadline: Instant,
    rename: impl FnOnce(&fs::File, &CStr, &fs::File, &CStr) -> io::Result<()>,
) -> Result<MaterializedFrozenRoot, Error> {
    require_frozen_materialization_deadline(deadline)?;
    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(stage, destination, deadline)?;
    require_frozen_private_directory_entries(stage, &[b"root"], deadline)?;

    let staged_identity = frozen_root_identity(staged_root, &stage.path.join("root"))?;
    let anchor_identity = frozen_root_identity(&staged_root_anchor, &stage.path.join("root"))?;
    // Activation must retain the capability opened from the private staging
    // namespace, never reopen the replaceable public destination after the
    // rename. Keep that invariant explicit here because Container's anchored
    // API deliberately rejects ordinary readable directory descriptors.
    // SAFETY: the retained file owns a live descriptor for the fcntl call.
    let anchor_flags = unsafe { nix::libc::fcntl(staged_root_anchor.as_raw_fd(), nix::libc::F_GETFL) };
    if anchor_flags == -1 {
        return Err(Error::OpenFrozenExecutableRoot {
            path: stage.path.join("root"),
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: the retained file owns a live descriptor for the fcntl call.
    let anchor_descriptor_flags = unsafe { nix::libc::fcntl(staged_root_anchor.as_raw_fd(), nix::libc::F_GETFD) };
    if anchor_descriptor_flags == -1 {
        return Err(Error::OpenFrozenExecutableRoot {
            path: stage.path.join("root"),
            source: io::Error::last_os_error(),
        });
    }
    if anchor_identity != staged_identity
        || anchor_flags & (nix::libc::O_PATH | nix::libc::O_DIRECTORY) != nix::libc::O_PATH | nix::libc::O_DIRECTORY
        || anchor_descriptor_flags & nix::libc::FD_CLOEXEC == 0
    {
        return Err(Error::FrozenPublicationNamespaceMismatch {
            stage: stage.path.clone(),
            destination: destination.root_path.clone(),
            reason: "the retained activation anchor is not the exact close-on-exec staged O_PATH directory",
        });
    }
    if staged_identity.device != destination.parent_identity.device || staged_identity.device != stage.identity.device {
        return Err(Error::FrozenPublicationNamespaceMismatch {
            stage: stage.path.clone(),
            destination: destination.root_path.clone(),
            reason: "the retained root and publication parents are not on one filesystem",
        });
    }
    match frozen_publication_name_state(
        &stage.file,
        c"root",
        &stage.path.join("root"),
        staged_identity,
        deadline,
    )? {
        FrozenPublicationNameState::Expected => {}
        FrozenPublicationNameState::Absent | FrozenPublicationNameState::Foreign => {
            return Err(Error::FrozenPublicationNamespaceMismatch {
                stage: stage.path.clone(),
                destination: destination.root_path.clone(),
                reason: "the private stage name does not identify the retained root",
            });
        }
    }
    if !matches!(
        frozen_publication_name_state(
            &destination.parent,
            &destination.name,
            &destination.root_path,
            staged_identity,
            deadline,
        )?,
        FrozenPublicationNameState::Absent
    ) {
        return Err(Error::FrozenRootDestinationExists(destination.root_path.clone()));
    }

    sync_frozen_publication_file(staged_root, &stage.path.join("root"), "sync staged root", deadline)?;
    sync_filesystem_until(staged_root.file(), deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::SyncFrozenPublication {
            path: stage.path.join("root"),
            operation: "sync staged filesystem before publication",
            source,
        })
    })?;
    sync_frozen_publication_file(&stage.file, &stage.path, "sync stage wrapper", deadline)?;
    sync_frozen_publication_file(
        &destination.parent,
        &destination.parent_path,
        "sync destination parent before publication",
        deadline,
    )?;

    // Repeat the complete namespace proof at the last userspace boundary.
    // The advisory parent lock serializes cooperating Forge writers; Linux has
    // no rename primitive which can additionally compare the source inode.
    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(stage, destination, deadline)?;
    require_frozen_private_directory_entries(stage, &[b"root"], deadline)?;
    if !matches!(
        frozen_publication_name_state(
            &stage.file,
            c"root",
            &stage.path.join("root"),
            staged_identity,
            deadline,
        )?,
        FrozenPublicationNameState::Expected
    ) || !matches!(
        frozen_publication_name_state(
            &destination.parent,
            &destination.name,
            &destination.root_path,
            staged_identity,
            deadline,
        )?,
        FrozenPublicationNameState::Absent
    ) {
        return Err(Error::FrozenPublicationNamespaceMismatch {
            stage: stage.path.clone(),
            destination: destination.root_path.clone(),
            reason: "the source or destination name changed before the publication syscall",
        });
    }

    let rename_error = rename(&stage.file, c"root", &destination.parent, &destination.name).err();
    // Namespace reconciliation is mandatory after every attempted rename,
    // even when the ordinary work budget expired during the syscall. The
    // caller reserves a separate tail of the same total materialization
    // budget exclusively for reconciliation and bounded cleanup.
    let recovery_deadline = frozen_namespace_recovery_deadline();
    let source_state = frozen_publication_name_state(
        &stage.file,
        c"root",
        &stage.path.join("root"),
        staged_identity,
        recovery_deadline,
    )?;
    let destination_state = frozen_publication_name_state(
        &destination.parent,
        &destination.name,
        &destination.root_path,
        staged_identity,
        recovery_deadline,
    )?;

    if source_state == FrozenPublicationNameState::Absent && destination_state == FrozenPublicationNameState::Expected {
        // Some filesystems may report an error after the namespace operation
        // has already taken effect. Exact two-name reconciliation is stronger
        // evidence than the syscall return value, so adopt the applied move.
        sync_frozen_publication_file(
            staged_root,
            &destination.root_path,
            "sync published root",
            recovery_deadline,
        )?;
        sync_frozen_publication_file(
            &stage.file,
            &stage.path,
            "sync emptied stage wrapper",
            recovery_deadline,
        )?;
        sync_frozen_publication_file(
            &destination.parent,
            &destination.parent_path,
            "sync destination parent after publication",
            recovery_deadline,
        )?;
        sync_filesystem_until(destination.parent.file(), recovery_deadline).map_err(|source| {
            frozen_materialization_io_error(recovery_deadline, source, |source| Error::SyncFrozenPublication {
                path: destination.parent_path.clone(),
                operation: "sync published frozen-root namespace",
                source,
            })
        })?;
        require_frozen_destination_parent(destination)?;
        require_frozen_private_directory_named(stage, destination, recovery_deadline)?;
        require_frozen_private_directory_entries(stage, &[], recovery_deadline)?;
        if frozen_publication_name_state(
            &stage.file,
            c"root",
            &stage.path.join("root"),
            staged_identity,
            recovery_deadline,
        )? != FrozenPublicationNameState::Absent
            || frozen_publication_name_state(
                &destination.parent,
                &destination.name,
                &destination.root_path,
                staged_identity,
                recovery_deadline,
            )? != FrozenPublicationNameState::Expected
            || frozen_root_identity(staged_root, &destination.root_path)? != staged_identity
            || frozen_root_identity(&staged_root_anchor, &destination.root_path)? != staged_identity
        {
            return Err(Error::FrozenPublicationNamespaceMismatch {
                stage: stage.path.clone(),
                destination: destination.root_path.clone(),
                reason: "the published root changed during the durability barrier",
            });
        }
        let materialized = MaterializedFrozenRoot {
            root_path: destination.root_path.clone(),
            root: staged_root_anchor,
            identity: staged_identity,
        };
        materialized.revalidate()?;
        return Ok(materialized);
    }

    match (source_state, destination_state, rename_error) {
        (FrozenPublicationNameState::Expected, FrozenPublicationNameState::Absent, Some(source)) => {
            Err(Error::PublishFrozenRoot {
                stage: stage.path.join("root"),
                destination: destination.root_path.clone(),
                source,
            })
        }
        (FrozenPublicationNameState::Expected, FrozenPublicationNameState::Foreign, Some(source))
            if source.kind() == io::ErrorKind::AlreadyExists =>
        {
            Err(Error::FrozenRootDestinationExists(destination.root_path.clone()))
        }
        _ => Err(Error::FrozenPublicationNamespaceMismatch {
            stage: stage.path.clone(),
            destination: destination.root_path.clone(),
            reason: "publication did not leave the retained root at exactly one authoritative name",
        }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrozenPublicationNameState {
    Absent,
    Expected,
    Foreign,
}

fn frozen_publication_name_state(
    parent: &fs::File,
    name: &CStr,
    path: &Path,
    expected: FrozenRootIdentity,
    deadline: Instant,
) -> Result<FrozenPublicationNameState, Error> {
    Ok(match frozen_named_identity_until(parent, name, path, deadline)? {
        None => FrozenPublicationNameState::Absent,
        Some(actual) if actual == expected => FrozenPublicationNameState::Expected,
        Some(_) => FrozenPublicationNameState::Foreign,
    })
}

fn require_frozen_private_directory_named(
    directory: &FrozenPrivateDirectory,
    destination: &FrozenRootDestination,
    deadline: Instant,
) -> Result<(), Error> {
    if frozen_root_identity(&directory.file, &directory.path)? != directory.identity
        || frozen_named_identity_until(&destination.parent, &directory.name, &directory.path, deadline)?
            != Some(directory.identity)
    {
        return Err(Error::FrozenPrivateDirectoryChanged {
            path: directory.path.clone(),
        });
    }
    Ok(())
}

fn require_frozen_private_directory_entries(
    directory: &FrozenPrivateDirectory,
    expected: &[&[u8]],
    deadline: Instant,
) -> Result<(), Error> {
    let mut entries = 0usize;
    let mut actual = frozen_discard_entry_names(directory.file.as_raw_fd(), &mut entries, deadline)?
        .into_iter()
        .map(|name| name.into_bytes())
        .collect::<Vec<_>>();
    let mut expected = expected.iter().map(|name| name.to_vec()).collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    if actual == expected {
        Ok(())
    } else {
        Err(Error::FrozenPrivateDirectoryChanged {
            path: directory.path.clone(),
        })
    }
}

fn sync_frozen_publication_file(
    file: &fs::File,
    path: &Path,
    operation: &'static str,
    deadline: Instant,
) -> Result<(), Error> {
    let mut interruptions = 0usize;
    loop {
        require_frozen_materialization_deadline(deadline)?;
        match file.sync_all() {
            Ok(()) => break,
            Err(source)
                if source.kind() == io::ErrorKind::Interrupted
                    && interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS =>
            {
                interruptions += 1;
            }
            Err(source) => {
                return Err(frozen_materialization_io_error(deadline, source, |source| {
                    Error::SyncFrozenPublication {
                        path: path.to_owned(),
                        operation,
                        source,
                    }
                }));
            }
        }
    }
    require_frozen_materialization_deadline(deadline)
}

fn discard_retained_frozen_stage(
    stage: &FrozenPrivateDirectory,
    destination: &FrozenRootDestination,
    staged_root: &fs::File,
    deadline: Instant,
) -> Result<(), Error> {
    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(stage, destination, deadline)?;
    require_frozen_private_directory_entries(stage, &[b"root"], deadline)?;
    let expected = frozen_root_identity(staged_root, &stage.path.join("root"))?;
    if frozen_publication_name_state(&stage.file, c"root", &stage.path.join("root"), expected, deadline)?
        != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenRetainedStageChanged {
            stage: stage.path.clone(),
        });
    }

    let mode = staged_root.metadata()?.mode() & 0o7777;
    chmod_path_descriptor_until(staged_root.file(), mode | 0o700, deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
            path: stage.path.join("root"),
            source,
        })
    })?;
    let readable = openat2_frozen_until(
        stage.file.as_raw_fd(),
        Path::new("root"),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .map_err(|source| Error::OpenFrozenPrivateDirectory {
        path: stage.path.join("root"),
        source,
    })?;
    let expected_after_chmod = frozen_root_identity(staged_root, &stage.path.join("root"))?;
    if frozen_root_identity(&readable, &stage.path.join("root"))? != expected_after_chmod {
        return Err(Error::FrozenRetainedStageChanged {
            stage: stage.path.clone(),
        });
    }
    let mut entries = 1usize;
    discard_frozen_directory(&readable, &stage.path.join("root"), 0, &mut entries, deadline)?;
    drop(readable);
    if frozen_publication_name_state(
        &stage.file,
        c"root",
        &stage.path.join("root"),
        expected_after_chmod,
        deadline,
    )? != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenRetainedStageChanged {
            stage: stage.path.clone(),
        });
    }
    unlink_frozen_discard_entry_until(
        &stage.file,
        c"root",
        &stage.path.join("root"),
        expected_after_chmod,
        UnlinkatFlags::RemoveDir,
        deadline,
    )?;
    require_frozen_private_directory_entries(stage, &[], deadline)?;
    sync_frozen_publication_file(&stage.file, &stage.path, "sync discarded private stage", deadline)
}

fn remove_empty_frozen_private_directory(
    directory: &FrozenPrivateDirectory,
    destination: &FrozenRootDestination,
    deadline: Instant,
) -> Result<(), Error> {
    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(directory, destination, deadline)?;
    require_frozen_private_directory_entries(directory, &[], deadline)?;
    let mut interruptions = 0usize;
    loop {
        require_frozen_materialization_deadline(deadline)?;
        let result = unlinkat(
            Some(destination.parent.as_raw_fd()),
            directory.name.as_c_str(),
            UnlinkatFlags::RemoveDir,
        );
        let recovery_deadline = frozen_namespace_recovery_deadline();
        let named =
            frozen_named_identity_until(&destination.parent, &directory.name, &directory.path, recovery_deadline)?;
        match (result, named) {
            (_, None) => {
                sync_frozen_publication_file(
                    &destination.parent,
                    &destination.parent_path,
                    "sync removed private frozen-root directory",
                    recovery_deadline,
                )?;
                return Ok(());
            }
            (Err(Errno::EINTR), Some(identity))
                if identity == directory.identity && interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS =>
            {
                interruptions += 1;
            }
            (Err(source), Some(identity)) if identity == directory.identity => {
                return Err(frozen_materialization_io_error(
                    deadline,
                    io::Error::from_raw_os_error(source as i32),
                    Error::Io,
                ));
            }
            _ => {
                return Err(Error::FrozenPrivateDirectoryChanged {
                    path: directory.path.clone(),
                });
            }
        }
    }
}

struct FrozenDiscardDirectoryStream(NonNull<nix::libc::DIR>);

impl Drop for FrozenDiscardDirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the stream returned by fdopendir.
        unsafe {
            nix::libc::closedir(self.0.as_ptr());
        }
    }
}

fn discard_frozen_root_destination(destination: &FrozenRootDestination) -> Result<(), Error> {
    let deadline = Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT - FROZEN_NAMESPACE_RECOVERY_TIMEOUT;
    discard_frozen_root_destination_until(destination, deadline)
}

fn discard_frozen_root_destination_until(destination: &FrozenRootDestination, deadline: Instant) -> Result<(), Error> {
    discard_frozen_root_destination_with(
        destination,
        deadline,
        |source_directory, source_name, destination_directory, destination_name| {
            renameat2_noreplace_until(
                source_directory.file(),
                source_name,
                destination_directory.file(),
                destination_name,
                deadline,
            )
        },
    )
}

fn discard_frozen_root_destination_with(
    destination: &FrozenRootDestination,
    deadline: Instant,
    rename: impl FnOnce(&fs::File, &CStr, &fs::File, &CStr) -> io::Result<()>,
) -> Result<(), Error> {
    require_frozen_materialization_deadline(deadline)?;
    let _lock = lock_frozen_destination_until(destination, deadline)?;
    let Some(pinned) =
        open_frozen_named_entry_until(&destination.parent, &destination.name, &destination.root_path, deadline)?
    else {
        return Ok(());
    };
    let expected = frozen_root_identity(&pinned, &destination.root_path)?;
    let metadata = pinned.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if metadata.mode() & nix::libc::S_IFMT != nix::libc::S_IFDIR
        || metadata.uid() != effective_owner
        || metadata.dev() != destination.parent_identity.device
    {
        return Err(Error::UnsafeFrozenRootDiscard {
            root: destination.root_path.clone(),
            owner: metadata.uid(),
            mode: metadata.mode(),
        });
    }
    let quarantine = create_frozen_private_directory(destination, b".forge-frozen-discard-", deadline)?;
    let detached = quarantine.path.join("root");
    let detached_identity = match detach_frozen_root_with(destination, &quarantine, &pinned, expected, deadline, rename)
    {
        Ok(identity) => identity,
        Err(primary) => {
            let cleanup_deadline = frozen_namespace_recovery_deadline();
            let cleanup = require_frozen_private_directory_entries(&quarantine, &[], cleanup_deadline)
                .and_then(|()| remove_empty_frozen_private_directory(&quarantine, destination, cleanup_deadline));
            return match cleanup {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(Error::CleanupFrozenDiscardQuarantine {
                    quarantine: quarantine.path,
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
    };

    // The root is now durably absent from its public name and exact at the
    // retained private slot. Destructive traversal gets its own finite budget;
    // any failure preserves the non-reusable quarantine instead of exposing a
    // partially deleted public root.
    let cleanup_deadline = Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT;
    let moved = openat2_frozen_until(
        quarantine.file.as_raw_fd(),
        Path::new("root"),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        cleanup_deadline,
    )
    .map_err(|source| Error::OpenFrozenDiscardDirectory { source })?;
    if frozen_root_identity(&moved, &detached)? != detached_identity {
        return Err(Error::FrozenRootChangedDuringDiscard {
            root: destination.root_path.clone(),
            quarantine: detached,
        });
    }
    let mut entries = 1usize;
    discard_frozen_directory(&moved, &detached, 0, &mut entries, cleanup_deadline)?;
    if frozen_publication_name_state(
        &quarantine.file,
        c"root",
        &detached,
        detached_identity,
        cleanup_deadline,
    )? != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenRootChangedDuringDiscard {
            root: destination.root_path.clone(),
            quarantine: detached,
        });
    }
    require_frozen_private_directory_entries(&quarantine, &[b"root"], cleanup_deadline)?;
    unlink_frozen_discard_entry_until(
        &quarantine.file,
        c"root",
        &detached,
        detached_identity,
        UnlinkatFlags::RemoveDir,
        cleanup_deadline,
    )?;
    require_frozen_private_directory_entries(&quarantine, &[], cleanup_deadline)?;
    sync_frozen_publication_file(
        &quarantine.file,
        &quarantine.path,
        "sync emptied frozen discard quarantine",
        cleanup_deadline,
    )?;
    remove_empty_frozen_private_directory(&quarantine, destination, cleanup_deadline)
}

fn detach_frozen_root_with(
    destination: &FrozenRootDestination,
    quarantine: &FrozenPrivateDirectory,
    pinned: &fs::File,
    expected: FrozenRootIdentity,
    deadline: Instant,
    rename: impl FnOnce(&fs::File, &CStr, &fs::File, &CStr) -> io::Result<()>,
) -> Result<FrozenRootIdentity, Error> {
    require_frozen_materialization_deadline(deadline)?;
    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(quarantine, destination, deadline)?;
    require_frozen_private_directory_entries(quarantine, &[], deadline)?;
    if frozen_root_identity(pinned, &destination.root_path)? != expected
        || frozen_publication_name_state(
            &destination.parent,
            &destination.name,
            &destination.root_path,
            expected,
            deadline,
        )? != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenRootChangedDuringDiscard {
            root: destination.root_path.clone(),
            quarantine: quarantine.path.join("root"),
        });
    }

    // The root may legitimately be mode 000. syncfs on the retained parent
    // flushes the filesystem without requiring a readable root descriptor.
    // The exact mode is widened only immediately before rename below and is
    // restored through the retained descriptor on every failed detach.
    sync_filesystem_until(destination.parent.file(), deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::SyncFrozenPublication {
            path: destination.root_path.clone(),
            operation: "sync frozen-root filesystem before detach",
            source,
        })
    })?;
    sync_frozen_publication_file(
        &destination.parent,
        &destination.parent_path,
        "sync frozen-root parent before detach",
        deadline,
    )?;
    sync_frozen_publication_file(
        &quarantine.file,
        &quarantine.path,
        "sync empty frozen-root quarantine",
        deadline,
    )?;

    require_frozen_destination_parent(destination)?;
    require_frozen_private_directory_named(quarantine, destination, deadline)?;
    require_frozen_private_directory_entries(quarantine, &[], deadline)?;
    if frozen_publication_name_state(
        &destination.parent,
        &destination.name,
        &destination.root_path,
        expected,
        deadline,
    )? != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenRootChangedDuringDiscard {
            root: destination.root_path.clone(),
            quarantine: quarantine.path.join("root"),
        });
    }

    // Linux requires write/search permission on a directory whose `..` entry
    // changes during a cross-parent rename. Restore owner access through the
    // retained descriptor immediately before the mutation, then restore the
    // exact original mode on every failed detach. Successful detaches keep the
    // widened mode only inside the private wrapper for bounded deletion.
    let detached_expected = prepare_frozen_discard_root_mode(pinned, destination, expected, deadline)?;

    let detach = (|| {
        require_frozen_materialization_deadline(deadline)?;
        if frozen_publication_name_state(
            &destination.parent,
            &destination.name,
            &destination.root_path,
            detached_expected,
            deadline,
        )? != FrozenPublicationNameState::Expected
        {
            return Err(Error::FrozenRootChangedDuringDiscard {
                root: destination.root_path.clone(),
                quarantine: quarantine.path.join("root"),
            });
        }

        let rename_error = rename(&destination.parent, &destination.name, &quarantine.file, c"root").err();
        let recovery_deadline = frozen_namespace_recovery_deadline();
        let source_state = frozen_publication_name_state(
            &destination.parent,
            &destination.name,
            &destination.root_path,
            detached_expected,
            recovery_deadline,
        )?;
        let quarantine_state = frozen_publication_name_state(
            &quarantine.file,
            c"root",
            &quarantine.path.join("root"),
            detached_expected,
            recovery_deadline,
        )?;
        if source_state == FrozenPublicationNameState::Absent
            && quarantine_state == FrozenPublicationNameState::Expected
        {
            sync_frozen_publication_file(
                &destination.parent,
                &destination.parent_path,
                "sync public parent after frozen-root detach",
                recovery_deadline,
            )?;
            sync_frozen_publication_file(
                &quarantine.file,
                &quarantine.path,
                "sync quarantine after frozen-root detach",
                recovery_deadline,
            )?;
            sync_filesystem_until(quarantine.file.file(), recovery_deadline).map_err(|source| {
                frozen_materialization_io_error(recovery_deadline, source, |source| Error::SyncFrozenPublication {
                    path: quarantine.path.clone(),
                    operation: "sync detached frozen-root namespace",
                    source,
                })
            })?;
            require_frozen_destination_parent(destination)?;
            require_frozen_private_directory_named(quarantine, destination, recovery_deadline)?;
            require_frozen_private_directory_entries(quarantine, &[b"root"], recovery_deadline)?;
            if frozen_publication_name_state(
                &destination.parent,
                &destination.name,
                &destination.root_path,
                detached_expected,
                recovery_deadline,
            )? != FrozenPublicationNameState::Absent
                || frozen_publication_name_state(
                    &quarantine.file,
                    c"root",
                    &quarantine.path.join("root"),
                    detached_expected,
                    recovery_deadline,
                )? != FrozenPublicationNameState::Expected
                || frozen_root_identity(pinned, &quarantine.path.join("root"))? != detached_expected
            {
                return Err(Error::FrozenDiscardNamespaceMismatch {
                    root: destination.root_path.clone(),
                    quarantine: quarantine.path.join("root"),
                });
            }
            return Ok(detached_expected);
        }

        match (source_state, quarantine_state, rename_error) {
            (FrozenPublicationNameState::Expected, FrozenPublicationNameState::Absent, Some(source)) => {
                Err(Error::DetachFrozenRoot {
                    root: destination.root_path.clone(),
                    quarantine: quarantine.path.join("root"),
                    source,
                })
            }
            _ => Err(Error::FrozenDiscardNamespaceMismatch {
                root: destination.root_path.clone(),
                quarantine: quarantine.path.join("root"),
            }),
        }
    })();

    match detach {
        Ok(identity) => Ok(identity),
        Err(primary) => Err(restore_frozen_discard_root_mode(pinned, destination, expected, primary)),
    }
}

fn prepare_frozen_discard_root_mode(
    pinned: &fs::File,
    destination: &FrozenRootDestination,
    expected: FrozenRootIdentity,
    deadline: Instant,
) -> Result<FrozenRootIdentity, Error> {
    prepare_frozen_discard_root_mode_with(pinned, destination, expected, deadline, frozen_root_identity)
}

fn prepare_frozen_discard_root_mode_with(
    pinned: &fs::File,
    destination: &FrozenRootDestination,
    expected: FrozenRootIdentity,
    deadline: Instant,
    inspect: impl FnOnce(&fs::File, &Path) -> Result<FrozenRootIdentity, Error>,
) -> Result<FrozenRootIdentity, Error> {
    let discard_permissions = expected.mode & 0o7777 | 0o700;
    let mut detached_expected = expected;
    detached_expected.mode = expected.mode & !0o7777 | discard_permissions;
    let normalize = chmod_path_descriptor_until(pinned.file(), discard_permissions, deadline);
    let normalized = match inspect(pinned, &destination.root_path) {
        Ok(normalized) => normalized,
        Err(primary) => {
            return Err(restore_frozen_discard_root_mode(pinned, destination, expected, primary));
        }
    };
    if normalized == detached_expected {
        return Ok(detached_expected);
    }
    let primary = match normalize {
        Err(source) => {
            frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
                path: destination.root_path.clone(),
                source,
            })
        }
        Ok(()) => Error::FrozenRootChangedDuringDiscard {
            root: destination.root_path.clone(),
            quarantine: destination.parent_path.clone(),
        },
    };
    Err(restore_frozen_discard_root_mode(pinned, destination, expected, primary))
}

fn restore_frozen_discard_root_mode(
    pinned: &fs::File,
    destination: &FrozenRootDestination,
    expected: FrozenRootIdentity,
    primary: Error,
) -> Error {
    if frozen_root_identity(pinned, &destination.root_path).ok() == Some(expected) {
        return primary;
    }
    let recovery_deadline = frozen_namespace_recovery_deadline();
    let restore = chmod_path_descriptor_until(pinned.file(), expected.mode & 0o7777, recovery_deadline)
        .map_err(|source| Error::NormalizeFrozenPrivateDirectory {
            path: destination.root_path.clone(),
            source,
        })
        .and_then(|()| {
            if frozen_root_identity(pinned, &destination.root_path)? == expected {
                Ok(())
            } else {
                Err(Error::FrozenRootChangedDuringDiscard {
                    root: destination.root_path.clone(),
                    quarantine: destination.parent_path.clone(),
                })
            }
        })
        .and_then(|()| {
            sync_filesystem_until(destination.parent.file(), recovery_deadline).map_err(|source| {
                frozen_materialization_io_error(recovery_deadline, source, |source| Error::SyncFrozenPublication {
                    path: destination.root_path.clone(),
                    operation: "sync restored frozen-root discard mode",
                    source,
                })
            })
        })
        .and_then(|()| {
            if frozen_root_identity(pinned, &destination.root_path)? == expected {
                Ok(())
            } else {
                Err(Error::FrozenRootChangedDuringDiscard {
                    root: destination.root_path.clone(),
                    quarantine: destination.parent_path.clone(),
                })
            }
        });
    match restore {
        Ok(()) => primary,
        Err(restore) => Error::RestoreFrozenDiscardRootMode {
            root: destination.root_path.clone(),
            primary: Box::new(primary),
            restore: Box::new(restore),
        },
    }
}

fn unlink_frozen_discard_entry_until(
    directory: &fs::File,
    name: &CStr,
    path: &Path,
    expected: FrozenRootIdentity,
    flags: UnlinkatFlags,
    deadline: Instant,
) -> Result<(), Error> {
    unlink_frozen_discard_entry_with(directory, name, path, expected, deadline, |directory, name| {
        unlinkat(Some(directory.as_raw_fd()), name, flags)
    })
}

fn unlink_frozen_discard_entry_with(
    directory: &fs::File,
    name: &CStr,
    path: &Path,
    expected: FrozenRootIdentity,
    deadline: Instant,
    mut remove: impl FnMut(&fs::File, &CStr) -> Result<(), Errno>,
) -> Result<(), Error> {
    if frozen_publication_name_state(directory, name, path, expected, deadline)? != FrozenPublicationNameState::Expected
    {
        return Err(Error::FrozenDiscardEntryChanged);
    }

    let mut interruptions = 0usize;
    loop {
        require_frozen_materialization_deadline(deadline)?;
        let result = remove(directory, name);

        // unlinkat can be interrupted after its observable namespace effect.
        // Always classify the retained parent/name before deciding whether to
        // retry. The still-open anchor held by the caller prevents the exact
        // inode number from being recycled during this reconciliation.
        let recovery_deadline = frozen_namespace_recovery_deadline();
        match frozen_publication_name_state(directory, name, path, expected, recovery_deadline)? {
            FrozenPublicationNameState::Absent => return Ok(()),
            FrozenPublicationNameState::Foreign => return Err(Error::FrozenDiscardEntryChanged),
            FrozenPublicationNameState::Expected => match result {
                Err(Errno::EINTR) if interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS => {
                    interruptions += 1;
                }
                Err(source) => {
                    return Err(frozen_materialization_io_error(
                        deadline,
                        io::Error::from_raw_os_error(source as i32),
                        |source| Error::RemoveFrozenDiscardEntry {
                            path: path.to_owned(),
                            source,
                        },
                    ));
                }
                Ok(()) => return Err(Error::FrozenDiscardEntryChanged),
            },
        }
    }
}

fn discard_frozen_directory(
    directory: &fs::File,
    directory_path: &Path,
    depth: usize,
    entries: &mut usize,
    deadline: Instant,
) -> Result<(), Error> {
    require_frozen_materialization_deadline(deadline)?;
    if depth > MAX_FROZEN_LAYOUT_PATH_COMPONENTS {
        return Err(Error::FrozenDiscardDepthLimit {
            limit: MAX_FROZEN_LAYOUT_PATH_COMPONENTS,
            actual: depth,
        });
    }

    let names = frozen_discard_entry_names(directory.as_raw_fd(), entries, deadline)?;
    for name in names {
        require_frozen_materialization_deadline(deadline)?;
        let child_name = Path::new(OsStr::from_bytes(name.as_bytes()));
        let child_path = directory_path.join(child_name);
        let resolution = nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV;
        let anchor = openat2_frozen_until(
            directory.as_raw_fd(),
            child_name,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            resolution,
            deadline,
        )
        .map_err(|source| Error::OpenFrozenDiscardEntry {
            path: child_path.clone(),
            source,
        })?;
        let anchored_before = anchor.metadata()?;
        if anchored_before.mode() & nix::libc::S_IFMT == nix::libc::S_IFDIR {
            // Prove this name is an ordinary directory on the same filesystem
            // before chmod touches it. In particular, a hostile mount point
            // fails RESOLVE_NO_XDEV without changing the mounted root's mode.
            chmod_path_descriptor_until(anchor.file(), anchored_before.mode() & 0o7777 | 0o700, deadline).map_err(
                |source| {
                    frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenPrivateDirectory {
                        path: child_path.clone(),
                        source,
                    })
                },
            )?;
            let expected = frozen_root_identity(&anchor, &child_path)?;
            let child = openat2_frozen_until(
                directory.as_raw_fd(),
                child_name,
                nix::libc::O_RDONLY
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK,
                resolution,
                deadline,
            )
            .map_err(|source| Error::OpenFrozenDiscardEntry {
                path: child_path.clone(),
                source,
            })?;
            if frozen_root_identity(&child, &child_path)? != expected {
                return Err(Error::FrozenDiscardEntryChanged);
            }
            discard_frozen_directory(&child, &child_path, depth + 1, entries, deadline)?;
            drop(child);
            if frozen_root_identity(&anchor, &child_path)? != expected {
                return Err(Error::FrozenDiscardEntryChanged);
            }
            // Linux has no inode-conditional unlink. The private 0700 wrapper
            // and cooperating-writer lock are therefore the final-component
            // race boundary; post-syscall reconciliation still refuses to
            // retry against a foreign replacement.
            unlink_frozen_discard_entry_until(
                directory,
                name.as_c_str(),
                &child_path,
                expected,
                UnlinkatFlags::RemoveDir,
                deadline,
            )?;
        } else {
            let expected = frozen_root_identity(&anchor, &child_path)?;
            unlink_frozen_discard_entry_until(
                directory,
                name.as_c_str(),
                &child_path,
                expected,
                UnlinkatFlags::NoRemoveDir,
                deadline,
            )?;
        }
    }
    Ok(())
}

fn frozen_discard_entry_names(directory: RawFd, entries: &mut usize, deadline: Instant) -> Result<Vec<CString>, Error> {
    require_frozen_materialization_deadline(deadline)?;
    let cursor = openat2_frozen_until(
        directory,
        Path::new("."),
        nix::libc::O_CLOEXEC
            | nix::libc::O_DIRECTORY
            | nix::libc::O_RDONLY
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .map_err(|source| Error::OpenFrozenDiscardDirectory { source })?;
    let descriptor = cursor.into_raw_fd();
    // SAFETY: fdopendir consumes this fresh owned descriptor on success. On
    // failure it remains ours and is closed explicitly below.
    let stream = unsafe { nix::libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume descriptor.
        unsafe {
            nix::libc::close(descriptor);
        }
        return Err(Error::ReadFrozenDiscardDirectory { source });
    };
    let stream = FrozenDiscardDirectoryStream(stream);
    let mut names = Vec::new();
    let mut interruptions = 0usize;
    loop {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: errno is thread-local and readdir uses null for both EOF and
        // failure, so clear it immediately before the call.
        unsafe {
            *nix::libc::__errno_location() = 0;
        }
        // SAFETY: stream is live and exclusively used by this loop.
        let entry = unsafe { nix::libc::readdir(stream.0.as_ptr()) };
        if entry.is_null() {
            // SAFETY: errno was cleared immediately before readdir.
            let errno = unsafe { *nix::libc::__errno_location() };
            if errno == 0 {
                break;
            }
            if errno == nix::libc::EINTR && interruptions < MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS {
                interruptions += 1;
                continue;
            }
            return Err(Error::ReadFrozenDiscardDirectory {
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: readdir returned a NUL-terminated name valid until the next
        // call; copy it before advancing the stream.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let actual = entries.saturating_add(1);
        if actual > MAX_FROZEN_NORMALIZED_INODES {
            return Err(Error::FrozenDiscardEntryLimit {
                limit: MAX_FROZEN_NORMALIZED_INODES,
                actual,
            });
        }
        *entries = actual;
        names.push(CString::new(bytes).expect("directory entry names contain no interior NUL"));
    }
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    require_frozen_materialization_deadline(deadline)?;
    Ok(names)
}

/// Prove and normalize the complete declarative frozen tree through retained
/// descriptors. A preparation walk makes mode-zero directories and regular
/// files temporarily owner-accessible; a second full walk authenticates
/// content and seals leaves and directories bottom-up. The filesystem must
/// match `expected` exactly: no missing, extra, type-changed, mode-changed,
/// hardlinked, POSIX access/default-ACL-bearing, or cross-mount entry is
/// eligible for publication.
///
/// This is a proof over Forge's private, quiescent staging tree, not a kernel
/// filesystem freeze. An uncooperative process with the same effective UID can
/// still mutate an ordinary inode after its last check; publication therefore
/// must not claim adversarial same-UID snapshot atomicity.
fn chmod_frozen_normalization_entry(
    file: &std::fs::File,
    path: &Path,
    mode: u32,
    deadline: Instant,
) -> Result<(), Error> {
    chmod_path_descriptor_until(file, mode, deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenEntryMode {
            path: path.to_owned(),
            source,
        })
    })
}

fn open_frozen_normalization_readonly(
    file: &std::fs::File,
    path: &Path,
    deadline: Instant,
) -> Result<std::fs::File, Error> {
    open_path_descriptor_readonly_until(file, deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::OpenFrozenNormalizationEntry {
            path: path.to_owned(),
            source,
        })
    })
}

fn require_frozen_normalization_access_acl(file: &std::fs::File, path: &Path, deadline: Instant) -> Result<(), Error> {
    require_no_access_acl_until(file, path, deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::FrozenNormalizationAcl {
            path: path.to_owned(),
            source,
        })
    })
}

fn require_frozen_normalization_default_acl(file: &std::fs::File, path: &Path, deadline: Instant) -> Result<(), Error> {
    require_no_default_acl_until(file, path, deadline).map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::FrozenNormalizationAcl {
            path: path.to_owned(),
            source,
        })
    })
}

fn normalize_frozen_tree(
    root: &fs::File,
    display_path: &Path,
    expected: &FrozenExpectedTree,
    timestamp: FileTime,
    deadline: Instant,
) -> Result<(), Error> {
    normalize_frozen_tree_with(
        root,
        display_path,
        expected,
        timestamp,
        deadline,
        FrozenNormalizationLimits::PRODUCTION,
        |_, _| {},
    )
}

fn normalize_frozen_tree_with<F>(
    root: &fs::File,
    display_path: &Path,
    expected: &FrozenExpectedTree,
    timestamp: FileTime,
    deadline: Instant,
    limits: FrozenNormalizationLimits,
    mut checkpoint: F,
) -> Result<(), Error>
where
    F: FnMut(FrozenNormalizationCheckpoint, &Path),
{
    require_frozen_materialization_deadline(deadline)?;
    if limits.inodes == 0 {
        return Err(Error::FrozenNormalizationInodeLimit { limit: 0, actual: 1 });
    }
    let expected_path = Path::new("/");
    let declaration = expected.entry(expected_path)?;
    let witness = frozen_normalization_witness(root, expected_path)?;
    let root_device = witness.device;
    require_frozen_normalization_declaration(expected_path, witness, declaration, root_device)?;
    require_named_frozen_normalization_root(display_path, root, witness, deadline)?;

    let mut inodes = 1usize;
    normalize_frozen_directory(
        root,
        root,
        expected_path,
        expected,
        declaration,
        witness,
        deadline,
        limits,
        root_device,
        &mut inodes,
        &mut checkpoint,
    )?;
    if inodes != expected.entries.len() {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: expected_path.to_owned(),
            reason: "the runtime walk did not account for the complete declarative tree",
        });
    }
    checkpoint(
        FrozenNormalizationCheckpoint::BeforeFinalTreeConfirmation,
        expected_path,
    );
    let final_witness = seal_frozen_directory(
        root,
        root,
        expected_path,
        expected,
        declaration,
        timestamp,
        deadline,
        root_device,
        &mut checkpoint,
    )?;
    require_named_frozen_normalization_root_final(display_path, root, final_witness, deadline)?;
    require_frozen_normalization_access_acl(root.file(), expected_path, deadline)?;
    require_frozen_normalization_default_acl(root.file(), expected_path, deadline)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn normalize_frozen_directory<F>(
    anchor: &fs::File,
    directory: &fs::File,
    expected_path: &Path,
    expected: &FrozenExpectedTree,
    declaration: &FrozenExpectedEntry,
    original: FrozenNormalizationWitness,
    deadline: Instant,
    limits: FrozenNormalizationLimits,
    root_device: u64,
    inodes: &mut usize,
    checkpoint: &mut F,
) -> Result<(), Error>
where
    F: FnMut(FrozenNormalizationCheckpoint, &Path),
{
    require_frozen_materialization_deadline(deadline)?;
    let FrozenExpectedKind::Directory = declaration.kind else {
        return Err(Error::InvalidFrozenNormalizationDeclaration {
            path: expected_path.to_owned(),
            reason: "a non-directory declaration reached directory traversal",
        });
    };
    let traversal_mode = declaration.mode | 0o700;
    if traversal_mode != declaration.mode {
        chmod_frozen_normalization_entry(anchor.file(), expected_path, traversal_mode, deadline)?;
    }
    checkpoint(
        FrozenNormalizationCheckpoint::DirectoryTraversalModeApplied,
        expected_path,
    );
    require_frozen_normalization_witness(expected_path, directory, original.with_permissions(traversal_mode))?;
    require_frozen_normalization_access_acl(directory.file(), expected_path, deadline)?;
    require_frozen_normalization_default_acl(directory.file(), expected_path, deadline)?;

    let declared_children = frozen_normalization_declared_children(expected, expected_path)?;
    let inventory = frozen_normalization_inventory(
        directory,
        expected_path,
        declared_children.len(),
        Some((inodes, limits.inodes)),
        deadline,
    )?;
    require_frozen_normalization_inventory(expected_path, &inventory, &declared_children, expected, root_device)?;
    checkpoint(FrozenNormalizationCheckpoint::DirectoryEnumerated, expected_path);

    for (entry, (_, child_path)) in inventory.iter().zip(declared_children.iter()) {
        normalize_frozen_entry(
            directory,
            entry,
            child_path,
            expected,
            deadline,
            limits,
            root_device,
            inodes,
            checkpoint,
        )?;
    }

    require_frozen_materialization_deadline(deadline)?;
    let confirmed = frozen_normalization_inventory(directory, expected_path, inventory.len(), None, deadline)?;
    require_frozen_normalization_active_inventory(
        expected_path,
        &inventory,
        &confirmed,
        &declared_children,
        expected,
        root_device,
    )?;
    require_frozen_normalization_witness(expected_path, anchor, original.with_permissions(traversal_mode))?;
    require_frozen_normalization_access_acl(directory.file(), expected_path, deadline)?;
    require_frozen_normalization_default_acl(directory.file(), expected_path, deadline)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn normalize_frozen_entry<F>(
    parent: &fs::File,
    inventory: &FrozenNormalizationInventoryEntry,
    expected_path: &Path,
    expected: &FrozenExpectedTree,
    deadline: Instant,
    limits: FrozenNormalizationLimits,
    root_device: u64,
    inodes: &mut usize,
    checkpoint: &mut F,
) -> Result<(), Error>
where
    F: FnMut(FrozenNormalizationCheckpoint, &Path),
{
    require_frozen_materialization_deadline(deadline)?;
    let depth =
        frozen_normalization_path_depth(expected_path).ok_or_else(|| Error::InvalidFrozenNormalizationDeclaration {
            path: expected_path.to_owned(),
            reason: "the declarative path is not normalized and absolute",
        })?;
    if depth > limits.depth {
        return Err(Error::FrozenNormalizationDepthLimit {
            limit: limits.depth,
            actual: depth,
        });
    }
    let declaration = expected.entry(expected_path)?;
    let pinned = open_frozen_normalization_entry(
        parent,
        &inventory.name,
        expected_path,
        FrozenNormalizationOpen::Anchor,
        deadline,
    )?;
    require_frozen_normalization_witness(expected_path, &pinned, inventory.witness)?;
    require_frozen_normalization_declaration(expected_path, inventory.witness, declaration, root_device)?;
    checkpoint(FrozenNormalizationCheckpoint::EntryPinned, expected_path);
    require_named_frozen_normalization_entry(parent, &inventory.name, expected_path, inventory.witness, deadline)?;

    let active_witness = match &declaration.kind {
        FrozenExpectedKind::Directory => {
            let traversal_mode = declaration.mode | 0o700;
            if traversal_mode != declaration.mode {
                chmod_frozen_normalization_entry(pinned.file(), expected_path, traversal_mode, deadline)?;
            }
            checkpoint(
                FrozenNormalizationCheckpoint::DirectoryTraversalModeApplied,
                expected_path,
            );
            let traversal_witness = inventory.witness.with_permissions(traversal_mode);
            require_named_frozen_normalization_entry(
                parent,
                &inventory.name,
                expected_path,
                traversal_witness,
                deadline,
            )?;
            let directory = open_frozen_normalization_entry(
                parent,
                &inventory.name,
                expected_path,
                FrozenNormalizationOpen::Directory,
                deadline,
            )?;
            require_frozen_normalization_witness(expected_path, &directory, traversal_witness)?;
            normalize_frozen_directory(
                &pinned,
                &directory,
                expected_path,
                expected,
                declaration,
                inventory.witness,
                deadline,
                limits,
                root_device,
                inodes,
                checkpoint,
            )?;
            traversal_witness
        }
        FrozenExpectedKind::Regular { .. } => {
            let readable_mode = declaration.mode | 0o400;
            if readable_mode != declaration.mode {
                chmod_frozen_normalization_entry(pinned.file(), expected_path, readable_mode, deadline)?;
            }
            let readable_witness = inventory.witness.with_permissions(readable_mode);
            require_named_frozen_normalization_entry(
                parent,
                &inventory.name,
                expected_path,
                readable_witness,
                deadline,
            )?;
            let readable = match open_frozen_normalization_readonly(pinned.file(), expected_path, deadline) {
                Ok(readable) => fs::File::from_parts(readable, expected_path.to_owned()),
                Err(primary) => {
                    if readable_mode != declaration.mode {
                        chmod_frozen_normalization_entry(pinned.file(), expected_path, declaration.mode, deadline)?;
                    }
                    return Err(primary);
                }
            };
            require_frozen_normalization_witness(expected_path, &readable, readable_witness)?;
            if let Err(primary) = require_frozen_normalization_access_acl(readable.file(), expected_path, deadline) {
                if readable_mode != declaration.mode {
                    chmod_frozen_normalization_entry(pinned.file(), expected_path, declaration.mode, deadline)?;
                }
                return Err(primary);
            }
            readable_witness
        }
        FrozenExpectedKind::Symlink { target } => {
            let actual = read_frozen_normalization_symlink(&pinned, expected_path, deadline)?;
            if &actual != target {
                return Err(Error::FrozenNormalizationSymlinkTargetMismatch {
                    path: expected_path.to_owned(),
                    expected: OsString::from_vec(target.clone()),
                    actual: OsString::from_vec(actual),
                });
            }
            inventory.witness
        }
    };

    checkpoint(FrozenNormalizationCheckpoint::BeforeEntryRevalidation, expected_path);
    require_named_frozen_normalization_entry(parent, &inventory.name, expected_path, active_witness, deadline)
}

#[allow(clippy::too_many_arguments)]
fn seal_frozen_directory<F>(
    anchor: &fs::File,
    directory: &fs::File,
    expected_path: &Path,
    expected: &FrozenExpectedTree,
    declaration: &FrozenExpectedEntry,
    timestamp: FileTime,
    deadline: Instant,
    root_device: u64,
    checkpoint: &mut F,
) -> Result<FrozenNormalizationFinalWitness, Error>
where
    F: FnMut(FrozenNormalizationCheckpoint, &Path),
{
    require_frozen_materialization_deadline(deadline)?;
    let FrozenExpectedKind::Directory = declaration.kind else {
        return Err(Error::InvalidFrozenNormalizationDeclaration {
            path: expected_path.to_owned(),
            reason: "a non-directory declaration reached final directory sealing",
        });
    };
    let active = frozen_normalization_witness(anchor, expected_path)?;
    require_frozen_normalization_active_declaration(expected_path, active, declaration, root_device)?;
    require_frozen_normalization_witness(expected_path, directory, active)?;
    require_frozen_normalization_access_acl(directory.file(), expected_path, deadline)?;
    require_frozen_normalization_default_acl(directory.file(), expected_path, deadline)?;

    let declared_children = frozen_normalization_declared_children(expected, expected_path)?;
    let inventory = frozen_normalization_inventory(directory, expected_path, declared_children.len(), None, deadline)?;
    require_frozen_normalization_active_declarations(
        expected_path,
        &inventory,
        &declared_children,
        expected,
        root_device,
    )?;

    let mut sealed = Vec::new();
    sealed
        .try_reserve_exact(inventory.len())
        .map_err(|source| Error::ReserveFrozenNormalizationInventory {
            path: expected_path.to_owned(),
            source,
        })?;
    for (entry, (_, child_path)) in inventory.iter().zip(declared_children.iter()) {
        let witness = seal_frozen_entry(
            directory,
            entry,
            child_path,
            expected,
            timestamp,
            deadline,
            root_device,
            checkpoint,
        )?;
        sealed.push((entry.name.clone(), (*child_path).clone(), witness));
    }

    // Normalize the directory itself before its last child inventory. The
    // O_NOATIME inventory below must leave this full witness untouched, so a
    // concurrent add/remove cannot be hidden by a later Forge utimens call.
    set_frozen_normalization_times(anchor, expected_path, timestamp, deadline)?;
    require_frozen_normalization_times(expected_path, anchor, timestamp)?;
    let active_final_witness = frozen_normalization_final_witness(anchor, expected_path)?;
    checkpoint(
        FrozenNormalizationCheckpoint::BeforeDirectoryFinalInventory,
        expected_path,
    );
    if expected_path == Path::new("/") {
        checkpoint(FrozenNormalizationCheckpoint::BeforeRootRevalidation, expected_path);
    }
    let confirmed = frozen_normalization_final_inventory(directory, expected_path, sealed.len(), deadline)?;
    require_frozen_normalization_final_inventory(expected_path, &confirmed, &sealed)?;
    checkpoint(
        FrozenNormalizationCheckpoint::AfterDirectoryFinalInventory,
        expected_path,
    );
    if frozen_normalization_final_witness(anchor, expected_path)? != active_final_witness {
        return Err(Error::FrozenNormalizationEntryChanged(expected_path.to_owned()));
    }

    let active_mode = declaration.mode | 0o700;
    if active_mode != declaration.mode {
        chmod_frozen_normalization_entry(anchor.file(), expected_path, declaration.mode, deadline)?;
    }
    let final_stable = frozen_normalization_witness(anchor, expected_path)?;
    require_frozen_normalization_declaration(expected_path, final_stable, declaration, root_device)?;
    require_frozen_normalization_times(expected_path, anchor, timestamp)?;
    require_frozen_normalization_access_acl(directory.file(), expected_path, deadline)?;
    require_frozen_normalization_default_acl(directory.file(), expected_path, deadline)?;
    frozen_normalization_final_witness(anchor, expected_path)
}

#[allow(clippy::too_many_arguments)]
fn seal_frozen_entry<F>(
    parent: &fs::File,
    inventory: &FrozenNormalizationInventoryEntry,
    expected_path: &Path,
    expected: &FrozenExpectedTree,
    timestamp: FileTime,
    deadline: Instant,
    root_device: u64,
    checkpoint: &mut F,
) -> Result<FrozenNormalizationFinalWitness, Error>
where
    F: FnMut(FrozenNormalizationCheckpoint, &Path),
{
    require_frozen_materialization_deadline(deadline)?;
    let declaration = expected.entry(expected_path)?;
    require_frozen_normalization_active_declaration(expected_path, inventory.witness, declaration, root_device)?;
    let pinned = open_frozen_normalization_entry(
        parent,
        &inventory.name,
        expected_path,
        FrozenNormalizationOpen::Anchor,
        deadline,
    )?;
    require_frozen_normalization_witness(expected_path, &pinned, inventory.witness)?;
    require_named_frozen_normalization_entry(parent, &inventory.name, expected_path, inventory.witness, deadline)?;

    let mut final_acl_check: Option<fs::File> = None;
    let final_witness = match &declaration.kind {
        FrozenExpectedKind::Directory => {
            let directory = open_frozen_normalization_entry(
                parent,
                &inventory.name,
                expected_path,
                FrozenNormalizationOpen::Directory,
                deadline,
            )?;
            require_frozen_normalization_witness(expected_path, &directory, inventory.witness)?;
            seal_frozen_directory(
                &pinned,
                &directory,
                expected_path,
                expected,
                declaration,
                timestamp,
                deadline,
                root_device,
                checkpoint,
            )?
        }
        FrozenExpectedKind::Regular { digest } => {
            let readable = open_frozen_normalization_readonly(pinned.file(), expected_path, deadline)?;
            let readable = fs::File::from_parts(readable, expected_path.to_owned());
            require_frozen_normalization_witness(expected_path, &readable, inventory.witness)?;
            require_frozen_normalization_access_acl(readable.file(), expected_path, deadline)?;

            let active_mode = declaration.mode | 0o400;
            if active_mode != declaration.mode {
                chmod_frozen_normalization_entry(pinned.file(), expected_path, declaration.mode, deadline)?;
            }
            set_frozen_normalization_times(&pinned, expected_path, timestamp, deadline)?;
            let final_stable = frozen_normalization_witness(&pinned, expected_path)?;
            require_frozen_normalization_declaration(expected_path, final_stable, declaration, root_device)?;
            require_frozen_normalization_times(expected_path, &pinned, timestamp)?;
            let final_witness = frozen_normalization_final_witness(&pinned, expected_path)?;
            require_frozen_normalization_regular_digest(&readable, expected_path, *digest, final_witness, deadline)?;
            checkpoint(FrozenNormalizationCheckpoint::AfterRegularDigest, expected_path);
            final_acl_check = Some(readable);
            final_witness
        }
        FrozenExpectedKind::Symlink { target } => {
            let actual = read_frozen_normalization_symlink(&pinned, expected_path, deadline)?;
            if &actual != target {
                return Err(Error::FrozenNormalizationSymlinkTargetMismatch {
                    path: expected_path.to_owned(),
                    expected: OsString::from_vec(target.clone()),
                    actual: OsString::from_vec(actual),
                });
            }
            set_frozen_normalization_times(&pinned, expected_path, timestamp, deadline)?;
            let final_stable = frozen_normalization_witness(&pinned, expected_path)?;
            require_frozen_normalization_declaration(expected_path, final_stable, declaration, root_device)?;
            require_frozen_normalization_times(expected_path, &pinned, timestamp)?;
            frozen_normalization_final_witness(&pinned, expected_path)?
        }
    };

    checkpoint(FrozenNormalizationCheckpoint::BeforeEntryRevalidation, expected_path);
    require_named_frozen_normalization_entry_final(
        parent,
        &inventory.name,
        expected_path,
        &pinned,
        final_witness,
        deadline,
    )?;
    if let Some(file) = final_acl_check {
        require_frozen_normalization_access_acl(file.file(), expected_path, deadline)?;
    }
    Ok(final_witness)
}

fn frozen_normalization_inventory(
    directory: &fs::File,
    expected_path: &Path,
    expected_entries: usize,
    mut accounting: Option<(&mut usize, usize)>,
    deadline: Instant,
) -> Result<Vec<FrozenNormalizationInventoryEntry>, Error> {
    require_frozen_materialization_deadline(deadline)?;
    let cursor = openat_owned(
        directory.as_raw_fd(),
        ".",
        OFlag::O_CLOEXEC
            | OFlag::O_DIRECTORY
            | OFlag::O_RDONLY
            | OFlag::O_NOFOLLOW
            | OFlag::O_NONBLOCK
            | OFlag::O_NOATIME,
        Mode::empty(),
    )?;
    let descriptor = cursor.into_raw_fd();
    // SAFETY: fdopendir consumes this fresh descriptor on success. On failure
    // it remains ours and is closed explicitly below.
    let stream = unsafe { nix::libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume descriptor.
        unsafe {
            nix::libc::close(descriptor);
        }
        return Err(Error::ReadFrozenNormalizationDirectory {
            path: expected_path.to_owned(),
            source,
        });
    };
    let stream = FrozenDiscardDirectoryStream(stream);
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(expected_entries.saturating_add(1))
        .map_err(|source| Error::ReserveFrozenNormalizationInventory {
            path: expected_path.to_owned(),
            source,
        })?;
    loop {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: errno is thread-local and readdir uses null for both EOF and
        // failure, so clear it immediately before the call.
        unsafe {
            *nix::libc::__errno_location() = 0;
        }
        // SAFETY: stream is live and exclusively used by this loop.
        let entry = unsafe { nix::libc::readdir(stream.0.as_ptr()) };
        if entry.is_null() {
            // SAFETY: errno was cleared immediately before readdir.
            let errno = unsafe { *nix::libc::__errno_location() };
            if errno == 0 {
                break;
            }
            return Err(Error::ReadFrozenNormalizationDirectory {
                path: expected_path.to_owned(),
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: readdir returned a NUL-terminated name valid until the next
        // call; copy it before advancing the stream.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let name = CString::new(bytes).expect("directory entry names contain no interior NUL");
        if entries.len() >= expected_entries {
            return Err(Error::FrozenNormalizationInventoryMismatch {
                path: expected_path.join(OsStr::from_bytes(name.as_bytes())),
                reason: "the filesystem contains an undeclared entry",
            });
        }
        if let Some((inodes, limit)) = accounting.as_mut() {
            let actual = inodes.saturating_add(1);
            if actual > *limit {
                return Err(Error::FrozenNormalizationInodeLimit { limit: *limit, actual });
            }
            **inodes = actual;
        }
        let witness = fstatat_frozen_normalization_entry(directory.as_raw_fd(), &name, expected_path, deadline)?;
        entries.push(FrozenNormalizationInventoryEntry { name, witness });
    }
    entries.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    require_frozen_materialization_deadline(deadline)?;
    Ok(entries)
}

fn fstatat_frozen_normalization_entry(
    directory: RawFd,
    name: &CStr,
    parent: &Path,
    deadline: Instant,
) -> Result<FrozenNormalizationWitness, Error> {
    let mut metadata = MaybeUninit::<nix::libc::stat>::uninit();
    loop {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: directory and name are live, metadata points to writable
        // storage, and AT_SYMLINK_NOFOLLOW prevents target traversal.
        if unsafe {
            nix::libc::fstatat(
                directory,
                name.as_ptr(),
                metadata.as_mut_ptr(),
                nix::libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            break;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(Error::InspectFrozenNormalizationEntry {
                path: parent.join(OsStr::from_bytes(name.to_bytes())),
                source,
            });
        }
    }
    // SAFETY: successful fstatat initialized the complete stat value.
    let metadata = unsafe { metadata.assume_init() };
    Ok(FrozenNormalizationWitness {
        device: metadata.st_dev,
        inode: metadata.st_ino,
        mode: metadata.st_mode,
        owner: metadata.st_uid,
        group: metadata.st_gid,
        links: metadata.st_nlink,
        length: u64::try_from(metadata.st_size).unwrap_or(0),
    })
}

fn require_frozen_normalization_inventory(
    parent: &Path,
    actual: &[FrozenNormalizationInventoryEntry],
    expected_children: &[(&OsString, &PathBuf)],
    expected: &FrozenExpectedTree,
    root_device: u64,
) -> Result<(), Error> {
    if actual.len() != expected_children.len() {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: parent.to_owned(),
            reason: "the filesystem is missing a declared entry",
        });
    }
    for (actual, (expected_name, expected_path)) in actual.iter().zip(expected_children) {
        if actual.name.as_bytes() != expected_name.as_bytes() {
            return Err(Error::FrozenNormalizationInventoryMismatch {
                path: parent.join(OsStr::from_bytes(actual.name.as_bytes())),
                reason: "the filesystem entry name is not declared",
            });
        }
        require_frozen_normalization_declaration(
            expected_path,
            actual.witness,
            expected.entry(expected_path)?,
            root_device,
        )?;
    }
    Ok(())
}

fn require_frozen_normalization_active_inventory(
    parent: &Path,
    original: &[FrozenNormalizationInventoryEntry],
    actual: &[FrozenNormalizationInventoryEntry],
    expected_children: &[(&OsString, &PathBuf)],
    expected: &FrozenExpectedTree,
    root_device: u64,
) -> Result<(), Error> {
    if original.len() != expected_children.len() || actual.len() != expected_children.len() {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: parent.to_owned(),
            reason: "the active filesystem inventory differs from its declaration",
        });
    }
    for ((original, actual), (expected_name, expected_path)) in original.iter().zip(actual).zip(expected_children) {
        if original.name.as_bytes() != expected_name.as_bytes() || actual.name != original.name {
            return Err(Error::FrozenNormalizationInventoryMismatch {
                path: parent.join(OsStr::from_bytes(actual.name.as_bytes())),
                reason: "an active filesystem entry name differs from its declaration",
            });
        }
        let declaration = expected.entry(expected_path)?;
        let expected_witness = original
            .witness
            .with_permissions(frozen_normalization_active_mode(declaration));
        if actual.witness != expected_witness {
            return Err(Error::FrozenNormalizationEntryChanged((*expected_path).clone()));
        }
        require_frozen_normalization_active_declaration(expected_path, actual.witness, declaration, root_device)?;
    }
    Ok(())
}

fn require_frozen_normalization_active_declarations(
    parent: &Path,
    actual: &[FrozenNormalizationInventoryEntry],
    expected_children: &[(&OsString, &PathBuf)],
    expected: &FrozenExpectedTree,
    root_device: u64,
) -> Result<(), Error> {
    if actual.len() != expected_children.len() {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: parent.to_owned(),
            reason: "the final pass found a missing declared entry",
        });
    }
    for (actual, (expected_name, expected_path)) in actual.iter().zip(expected_children) {
        if actual.name.as_bytes() != expected_name.as_bytes() {
            return Err(Error::FrozenNormalizationInventoryMismatch {
                path: parent.join(OsStr::from_bytes(actual.name.as_bytes())),
                reason: "the final pass found an undeclared filesystem name",
            });
        }
        require_frozen_normalization_active_declaration(
            expected_path,
            actual.witness,
            expected.entry(expected_path)?,
            root_device,
        )?;
    }
    Ok(())
}

fn frozen_normalization_active_mode(expected: &FrozenExpectedEntry) -> u32 {
    match expected.kind {
        FrozenExpectedKind::Directory => expected.mode | 0o700,
        FrozenExpectedKind::Regular { .. } => expected.mode | 0o400,
        FrozenExpectedKind::Symlink { .. } => expected.mode,
    }
}

fn require_frozen_normalization_active_declaration(
    path: &Path,
    witness: FrozenNormalizationWitness,
    expected: &FrozenExpectedEntry,
    root_device: u64,
) -> Result<(), Error> {
    let mut active = expected.clone();
    active.mode = frozen_normalization_active_mode(expected);
    require_frozen_normalization_declaration(path, witness, &active, root_device)
}

fn frozen_normalization_final_inventory(
    directory: &fs::File,
    expected_path: &Path,
    expected_entries: usize,
    deadline: Instant,
) -> Result<Vec<(CString, FrozenNormalizationFinalWitness)>, Error> {
    let inventory = frozen_normalization_inventory(directory, expected_path, expected_entries, None, deadline)?;
    let mut final_inventory = Vec::new();
    final_inventory.try_reserve_exact(inventory.len()).map_err(|source| {
        Error::ReserveFrozenNormalizationInventory {
            path: expected_path.to_owned(),
            source,
        }
    })?;
    for entry in inventory {
        require_frozen_materialization_deadline(deadline)?;
        let child_path = expected_path.join(OsStr::from_bytes(entry.name.as_bytes()));
        let pinned = open_frozen_normalization_entry(
            directory,
            &entry.name,
            &child_path,
            FrozenNormalizationOpen::Anchor,
            deadline,
        )?;
        require_frozen_normalization_witness(&child_path, &pinned, entry.witness)?;
        final_inventory.push((entry.name, frozen_normalization_final_witness(&pinned, &child_path)?));
    }
    Ok(final_inventory)
}

fn require_frozen_normalization_final_inventory(
    parent: &Path,
    actual: &[(CString, FrozenNormalizationFinalWitness)],
    expected: &[(CString, PathBuf, FrozenNormalizationFinalWitness)],
) -> Result<(), Error> {
    if actual.len() != expected.len() {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: parent.to_owned(),
            reason: "the sealed filesystem inventory changed before parent sealing",
        });
    }
    for ((actual_name, actual_witness), (expected_name, expected_path, expected_witness)) in actual.iter().zip(expected)
    {
        if actual_name != expected_name {
            return Err(Error::FrozenNormalizationInventoryMismatch {
                path: parent.join(OsStr::from_bytes(actual_name.as_bytes())),
                reason: "a sealed filesystem name changed before parent sealing",
            });
        }
        if actual_witness != expected_witness {
            return Err(Error::FrozenNormalizationEntryChanged(expected_path.clone()));
        }
    }
    Ok(())
}

fn open_frozen_normalization_entry(
    parent: &fs::File,
    name: &CStr,
    expected_path: &Path,
    open: FrozenNormalizationOpen,
    deadline: Instant,
) -> Result<fs::File, Error> {
    let mut flags = nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW;
    flags |= match open {
        FrozenNormalizationOpen::Anchor => nix::libc::O_PATH,
        FrozenNormalizationOpen::Directory => nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_NONBLOCK,
    };
    require_frozen_materialization_deadline(deadline)?;
    openat2_frozen_until(
        parent.as_raw_fd(),
        Path::new(OsStr::from_bytes(name.to_bytes())),
        flags,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::OpenFrozenNormalizationEntry {
            path: expected_path.to_owned(),
            source,
        })
    })
}

fn require_named_frozen_normalization_entry(
    parent: &fs::File,
    name: &CStr,
    expected_path: &Path,
    expected: FrozenNormalizationWitness,
    deadline: Instant,
) -> Result<(), Error> {
    let named =
        open_frozen_normalization_entry(parent, name, expected_path, FrozenNormalizationOpen::Anchor, deadline)?;
    require_frozen_normalization_witness(expected_path, &named, expected)
}

fn require_named_frozen_normalization_entry_final(
    parent: &fs::File,
    name: &CStr,
    expected_path: &Path,
    pinned: &fs::File,
    expected: FrozenNormalizationFinalWitness,
    deadline: Instant,
) -> Result<(), Error> {
    let named =
        open_frozen_normalization_entry(parent, name, expected_path, FrozenNormalizationOpen::Anchor, deadline)?;
    let retained = frozen_normalization_final_witness(pinned, expected_path)?;
    let named = frozen_normalization_final_witness(&named, expected_path)?;
    if retained == expected && named == expected {
        Ok(())
    } else {
        Err(Error::FrozenNormalizationEntryChanged(expected_path.to_owned()))
    }
}

fn require_named_frozen_normalization_root(
    path: &Path,
    root: &fs::File,
    expected: FrozenNormalizationWitness,
    deadline: Instant,
) -> Result<(), Error> {
    let retained = frozen_normalization_witness(root, Path::new("/"))?;
    let named = match open_frozen_root_anchor_until(path, deadline) {
        Ok(named) => named,
        Err(error @ Error::FrozenMaterializationTimeout { .. }) => return Err(error),
        Err(_) => return Err(Error::FrozenNormalizationRootChanged(path.to_owned())),
    };
    let named = frozen_normalization_witness(&named, Path::new("/"))?;
    if retained != expected || named != expected {
        return Err(Error::FrozenNormalizationRootChanged(path.to_owned()));
    }
    Ok(())
}

fn require_named_frozen_normalization_root_final(
    path: &Path,
    root: &fs::File,
    expected: FrozenNormalizationFinalWitness,
    deadline: Instant,
) -> Result<(), Error> {
    let retained = frozen_normalization_final_witness(root, Path::new("/"))?;
    let named = match open_frozen_root_anchor_until(path, deadline) {
        Ok(named) => named,
        Err(error @ Error::FrozenMaterializationTimeout { .. }) => return Err(error),
        Err(_) => return Err(Error::FrozenNormalizationRootChanged(path.to_owned())),
    };
    let named = frozen_normalization_final_witness(&named, Path::new("/"))?;
    if retained == expected && named == expected {
        Ok(())
    } else {
        Err(Error::FrozenNormalizationRootChanged(path.to_owned()))
    }
}

fn frozen_normalization_witness(file: &fs::File, path: &Path) -> Result<FrozenNormalizationWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenNormalizationWitness::from_metadata(&metadata))
        .map_err(|source| Error::InspectFrozenNormalizationEntry {
            path: path.to_owned(),
            source,
        })
}

fn frozen_normalization_final_witness(file: &fs::File, path: &Path) -> Result<FrozenNormalizationFinalWitness, Error> {
    file.metadata()
        .map(|metadata| FrozenNormalizationFinalWitness::from_metadata(&metadata))
        .map_err(|source| Error::InspectFrozenNormalizationEntry {
            path: path.to_owned(),
            source,
        })
}

fn require_frozen_normalization_witness(
    path: &Path,
    file: &fs::File,
    expected: FrozenNormalizationWitness,
) -> Result<(), Error> {
    if frozen_normalization_witness(file, path)? == expected {
        Ok(())
    } else {
        Err(Error::FrozenNormalizationEntryChanged(path.to_owned()))
    }
}

fn require_frozen_normalization_declaration(
    path: &Path,
    witness: FrozenNormalizationWitness,
    expected: &FrozenExpectedEntry,
    root_device: u64,
) -> Result<(), Error> {
    // SAFETY: geteuid takes no arguments and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if witness.owner != effective_owner {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the materialized inode is not owned by the effective user",
        });
    }
    if witness.device != root_device {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the materialized inode resides on another filesystem",
        });
    }
    let actual_kind = witness.mode & nix::libc::S_IFMT;
    let expected_kind = match expected.kind {
        FrozenExpectedKind::Directory => nix::libc::S_IFDIR,
        FrozenExpectedKind::Regular { .. } => nix::libc::S_IFREG,
        FrozenExpectedKind::Symlink { .. } => nix::libc::S_IFLNK,
    };
    if actual_kind != expected_kind {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the filesystem inode type differs from its declaration",
        });
    }
    if witness.mode & 0o7777 != expected.mode {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the filesystem mode differs from its declaration",
        });
    }
    if matches!(
        expected.kind,
        FrozenExpectedKind::Regular { .. } | FrozenExpectedKind::Symlink { .. }
    ) && witness.links != 1
    {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "a declarative regular file or symlink must have exactly one name",
        });
    }
    Ok(())
}

fn set_frozen_normalization_times(
    file: &fs::File,
    path: &Path,
    timestamp: FileTime,
    deadline: Instant,
) -> Result<(), Error> {
    set_path_descriptor_times_until(
        file.file(),
        timestamp.unix_seconds(),
        i64::from(timestamp.nanoseconds()),
        deadline,
    )
    .map_err(|source| {
        frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenEntryTime {
            path: path.to_owned(),
            source,
        })
    })
}

fn require_frozen_normalization_regular_digest(
    file: &fs::File,
    path: &Path,
    expected_digest: u128,
    expected_witness: FrozenNormalizationFinalWitness,
    deadline: Instant,
) -> Result<(), Error> {
    if expected_witness.stable.length > MAX_BLIT_ASSET_BYTES {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the regular file exceeds the bounded asset size",
        });
    }
    let before = file
        .metadata()
        .map_err(|source| Error::InspectFrozenNormalizationEntry {
            path: path.to_owned(),
            source,
        })?;
    if FrozenNormalizationFinalWitness::from_metadata(&before) != expected_witness {
        return Err(Error::FrozenNormalizationEntryChanged(path.to_owned()));
    }
    let mut hasher = StoneDigestWriterHasher::new();
    let mut remaining = expected_witness.stable.length;
    let mut buffer = [0_u8; ASSET_COPY_BUFFER_BYTES];
    while remaining != 0 {
        require_frozen_materialization_deadline(deadline)?;
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            Error::FrozenNormalizationInventoryMismatch {
                path: path.to_owned(),
                reason: "the regular file length is not representable",
            }
        })?;
        let offset = nix::libc::off_t::try_from(expected_witness.stable.length - remaining).map_err(|_| {
            Error::FrozenNormalizationInventoryMismatch {
                path: path.to_owned(),
                reason: "the regular file offset is not representable",
            }
        })?;
        let count = loop {
            require_frozen_materialization_deadline(deadline)?;
            // SAFETY: the retained readable descriptor and writable buffer
            // remain live, and the explicit bounded offset is representable.
            let count = unsafe { nix::libc::pread(file.as_raw_fd(), buffer.as_mut_ptr().cast(), requested, offset) };
            if count >= 0 {
                break usize::try_from(count).map_err(|_| Error::FrozenNormalizationInventoryMismatch {
                    path: path.to_owned(),
                    reason: "pread returned an invalid byte count",
                })?;
            }
            let source = Errno::last();
            if source != Errno::EINTR {
                return Err(Error::Blit(source));
            }
        };
        match count {
            0 => {
                return Err(Error::FrozenNormalizationInventoryMismatch {
                    path: path.to_owned(),
                    reason: "the regular file ended before its pinned length",
                });
            }
            _ => {}
        }
        hasher.update(&buffer[..count]);
        remaining = remaining.saturating_sub(count as u64);
    }
    let trailing_offset = nix::libc::off_t::try_from(expected_witness.stable.length).map_err(|_| {
        Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the trailing regular file offset is not representable",
        }
    })?;
    let trailing = loop {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: the retained readable descriptor and one-byte writable
        // buffer remain live for the bounded explicit offset.
        let count = unsafe { nix::libc::pread(file.as_raw_fd(), buffer.as_mut_ptr().cast(), 1, trailing_offset) };
        if count >= 0 {
            break usize::try_from(count).map_err(|_| Error::FrozenNormalizationInventoryMismatch {
                path: path.to_owned(),
                reason: "trailing pread returned an invalid byte count",
            })?;
        }
        let source = Errno::last();
        if source != Errno::EINTR {
            return Err(Error::Blit(source));
        }
    };
    if trailing != 0 {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the regular file grew beyond its pinned length",
        });
    }
    if hasher.digest128() != expected_digest {
        return Err(Error::FrozenNormalizationInventoryMismatch {
            path: path.to_owned(),
            reason: "the regular file content digest differs from its declaration",
        });
    }
    let after = file
        .metadata()
        .map_err(|source| Error::InspectFrozenNormalizationEntry {
            path: path.to_owned(),
            source,
        })?;
    if FrozenNormalizationFinalWitness::from_metadata(&after) != expected_witness {
        return Err(Error::FrozenNormalizationEntryChanged(path.to_owned()));
    }
    Ok(())
}

fn require_frozen_normalization_times(file_path: &Path, file: &fs::File, timestamp: FileTime) -> Result<(), Error> {
    let metadata = file
        .metadata()
        .map_err(|source| Error::InspectFrozenNormalizationEntry {
            path: file_path.to_owned(),
            source,
        })?;
    if metadata.atime() == timestamp.unix_seconds()
        && metadata.atime_nsec() == i64::from(timestamp.nanoseconds())
        && metadata.mtime() == timestamp.unix_seconds()
        && metadata.mtime_nsec() == i64::from(timestamp.nanoseconds())
    {
        Ok(())
    } else {
        Err(Error::FrozenNormalizationEntryChanged(file_path.to_owned()))
    }
}

fn read_frozen_normalization_symlink(file: &fs::File, path: &Path, deadline: Instant) -> Result<Vec<u8>, Error> {
    let mut target = vec![0_u8; MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1];
    loop {
        require_frozen_materialization_deadline(deadline)?;
        // SAFETY: the retained O_PATH descriptor and empty path name the exact
        // symlink inode, and target is writable for its complete capacity.
        let read =
            unsafe { nix::libc::readlinkat(file.as_raw_fd(), c"".as_ptr(), target.as_mut_ptr().cast(), target.len()) };
        if read >= 0 {
            let read = usize::try_from(read).map_err(|_| Error::ReadFrozenNormalizationSymlink {
                path: path.to_owned(),
                source: io::Error::other("readlinkat returned an invalid length"),
            })?;
            if read == target.len() {
                return Err(Error::FrozenNormalizationInventoryMismatch {
                    path: path.to_owned(),
                    reason: "the symlink target exceeds the declarative byte limit",
                });
            }
            target.truncate(read);
            return Ok(target);
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(Error::ReadFrozenNormalizationSymlink {
                path: path.to_owned(),
                source,
            });
        }
    }
}

/// Restore owner traversal and mutation permissions before replacing a tree.
///
/// Frozen package metadata may legitimately make a directory read-only. The
/// next materialization still has to be able to remove children within that
/// directory. Symlinks are never followed.
#[cfg(test)]
fn make_tree_removable(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok(());
    }

    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    fs::set_permissions(path, permissions)?;

    let mut children = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    children.sort();
    for child in children {
        make_tree_removable(&child)?;
    }
    Ok(())
}

/// Build a [`vfs::Tree`] for the specified layouts
///
/// Returns a newly built vfs Tree to plan the filesystem operations for blitting
/// and conflict detection.
pub fn vfs(layouts: Vec<(package::Id, StonePayloadLayoutRecord)>) -> Result<vfs::Tree<PendingFile>, Error> {
    let mut tbuild = TreeBuilder::new();

    for (id, layout) in layouts {
        // Revalidate database rows and direct Stone extraction input at the
        // final stateful/ephemeral VFS boundary. Normal cache ingestion already
        // enforces this contract atomically, but corrupted or test-populated
        // databases must not recover an escape path here.
        require_usr_relative_stone_layout(&id, &layout)?;
        tbuild.push(PendingFile { id: id.clone(), layout });
    }

    tbuild.bake();

    Ok(tbuild.tree()?)
}

const MAX_STONE_LAYOUT_COMPONENT_BYTES: usize = nix::libc::NAME_MAX as usize;
const MAX_STONE_LAYOUT_TARGET_DIAGNOSTIC_BYTES: usize = 256;

/// Proof that a raw Stone target has the one admitted representation.
///
/// Keeping materialization behind this type prevents a future caller from
/// accidentally restoring the old absolute-path compatibility spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UsrRelativeStoneTarget<'a>(&'a str);

/// Store decoded Stone layouts only after proving that every raw target uses
/// the one canonical package namespace.
///
/// Stone omits the leading `/usr/`; [`PendingFile::path`] adds it exactly once.
/// Requiring a non-empty normalized relative target here therefore makes every
/// persisted row strictly `/usr`-only and prevents alternate spellings of the
/// same materialized path. The iterator is cloned so the complete batch is
/// preflighted before `batch_add` can remove or insert any database rows.
fn ingest_stone_layouts<'a, I>(layout_db: &db::layout::Database, layouts: I) -> Result<(), Error>
where
    I: Iterator<Item = (&'a package::Id, &'a StonePayloadLayoutRecord)> + Clone,
{
    for (package, layout) in layouts.clone() {
        require_usr_relative_stone_layout(package, layout)?;
    }
    layout_db.batch_add(layouts)?;
    Ok(())
}

fn require_usr_relative_stone_layout<'a>(
    package: &package::Id,
    layout: &'a StonePayloadLayoutRecord,
) -> Result<UsrRelativeStoneTarget<'a>, Error> {
    let target = layout.file.target();
    require_usr_relative_stone_target(target).map_err(|reason| Error::InvalidStoneLayoutTarget {
        package: package.clone(),
        target: stone_layout_target_diagnostic(target),
        reason,
    })
}

fn stone_layout_target_diagnostic(target: &str) -> String {
    if target.len() <= MAX_STONE_LAYOUT_TARGET_DIAGNOSTIC_BYTES {
        return target.to_owned();
    }

    let mut end = MAX_STONE_LAYOUT_TARGET_DIAGNOSTIC_BYTES;
    while !target.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &target[..end])
}

fn require_usr_relative_stone_target(target: &str) -> Result<UsrRelativeStoneTarget<'_>, &'static str> {
    if target.is_empty() {
        return Err("the target is empty");
    }
    if target.starts_with('/') {
        return Err("the target is absolute");
    }
    if target.bytes().any(|byte| byte.is_ascii_control()) {
        return Err("the target contains an ASCII control byte");
    }
    if target.ends_with('/') {
        return Err("the target has a trailing separator");
    }
    if target.contains("//") {
        return Err("the target contains a repeated separator");
    }

    let materialized_bytes = "/usr/"
        .len()
        .checked_add(target.len())
        .ok_or("the materialized path length overflows")?;
    if materialized_bytes > MAX_FROZEN_EXECUTABLE_PATH_BYTES {
        return Err("the materialized path exceeds Linux PATH_MAX");
    }

    let mut components = 1usize; // the materialized `/usr` component
    for component in target.split('/') {
        if component == "." || component == ".." {
            return Err("the target contains a dot component");
        }
        if component.len() > MAX_STONE_LAYOUT_COMPONENT_BYTES {
            return Err("a target component exceeds Linux NAME_MAX");
        }
        components = components
            .checked_add(1)
            .ok_or("the materialized component count overflows")?;
        if components > MAX_FROZEN_LAYOUT_PATH_COMPONENTS {
            return Err("the materialized path is too deep");
        }
    }
    if package::is_reserved_usr_layout_target(target) {
        return Err("the target is reserved for Cast system metadata");
    }
    Ok(UsrRelativeStoneTarget(target))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenLayoutPathPolicyError {
    TooLong { actual: usize },
    TooDeep { actual: usize },
}

fn require_materialized_frozen_path_policy(path: &str) -> Result<(), FrozenLayoutPathPolicyError> {
    if path.len() > MAX_FROZEN_EXECUTABLE_PATH_BYTES {
        return Err(FrozenLayoutPathPolicyError::TooLong { actual: path.len() });
    }

    let materialized_components = path.split('/').filter(|component| !component.is_empty()).count();
    if materialized_components > MAX_FROZEN_LAYOUT_PATH_COMPONENTS {
        return Err(FrozenLayoutPathPolicyError::TooDeep {
            actual: materialized_components,
        });
    }
    Ok(())
}

fn require_frozen_layout_symlink_target(package: &package::Id, target: &str) -> Result<(), Error> {
    if target.len() > MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES {
        return Err(Error::FrozenLayoutSymlinkTargetTooLong {
            package: package.clone(),
            limit: MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES,
            actual: target.len(),
        });
    }
    if target.is_empty() {
        return Err(Error::InvalidFrozenLayoutSymlinkTarget {
            package: package.clone(),
            reason: "the target is empty",
        });
    }
    if target.as_bytes().contains(&0) {
        return Err(Error::InvalidFrozenLayoutSymlinkTarget {
            package: package.clone(),
            reason: "the target contains NUL",
        });
    }
    Ok(())
}

fn materialized_frozen_layout_path(raw_path: UsrRelativeStoneTarget<'_>) -> String {
    format!("/usr/{}", raw_path.0)
}

#[derive(Debug)]
struct FrozenLayoutEntry {
    package: package::Id,
    layout: StonePayloadLayoutRecord,
    path: String,
    package_order: usize,
    kind_order: u8,
    source_order: String,
}

impl FrozenLayoutEntry {
    fn new(package: package::Id, layout: StonePayloadLayoutRecord, package_order: usize) -> Result<Self, Error> {
        // Keep frozen materialization fail-closed even for callers which
        // populate the layout database without going through Stone ingestion.
        let raw_path = require_usr_relative_stone_layout(&package, &layout)?;
        if let StonePayloadLayoutFile::Symlink(target, _) = &layout.file {
            require_frozen_layout_symlink_target(&package, target)?;
        }

        let path = materialized_frozen_layout_path(raw_path);
        if !is_normalized_frozen_path(&path) {
            return Err(Error::InvalidFrozenLayoutPath { package, path });
        }
        if layout.uid != 0 || layout.gid != 0 {
            return Err(Error::UnsupportedFrozenOwnership {
                package,
                path,
                uid: layout.uid,
                gid: layout.gid,
            });
        }

        let expected_file_type = match &layout.file {
            StonePayloadLayoutFile::Regular(..) => nix::libc::S_IFREG,
            StonePayloadLayoutFile::Directory(_) => nix::libc::S_IFDIR,
            StonePayloadLayoutFile::Symlink(..) => nix::libc::S_IFLNK,
            StonePayloadLayoutFile::CharacterDevice(_)
            | StonePayloadLayoutFile::BlockDevice(_)
            | StonePayloadLayoutFile::Fifo(_)
            | StonePayloadLayoutFile::Socket(_)
            | StonePayloadLayoutFile::Unknown(..) => {
                return Err(Error::UnsupportedFrozenLayout { package, path });
            }
        };
        let actual_file_type = layout.mode & nix::libc::S_IFMT;
        let unsupported_mode_bits = layout.mode & !(nix::libc::S_IFMT | 0o7777);
        let symlink_mode_is_enforceable = expected_file_type != nix::libc::S_IFLNK || layout.mode & 0o7777 == 0o777;
        if actual_file_type != expected_file_type || unsupported_mode_bits != 0 || !symlink_mode_is_enforceable {
            return Err(Error::InvalidFrozenLayoutMode {
                package,
                path,
                mode: layout.mode,
            });
        }

        let (kind_order, source_order) = match &layout.file {
            StonePayloadLayoutFile::Directory(_) => (0, String::new()),
            StonePayloadLayoutFile::Regular(source, _) => (1, format!("{source:032x}")),
            StonePayloadLayoutFile::Symlink(source, _) => (2, source.to_string()),
            StonePayloadLayoutFile::CharacterDevice(_)
            | StonePayloadLayoutFile::BlockDevice(_)
            | StonePayloadLayoutFile::Fifo(_)
            | StonePayloadLayoutFile::Socket(_)
            | StonePayloadLayoutFile::Unknown(..) => unreachable!("unsupported inode returned above"),
        };

        Ok(Self {
            package,
            layout,
            path,
            package_order,
            kind_order,
            source_order,
        })
    }

    fn is_directory(&self) -> bool {
        matches!(self.layout.file, StonePayloadLayoutFile::Directory(_))
    }

    fn pending(&self) -> PendingFile {
        PendingFile {
            id: self.package.clone(),
            layout: self.layout.clone(),
        }
    }
}

/// Build a deterministic tree for a frozen package closure.
///
/// SQLite row order and concurrent cache completion are deliberately ignored.
/// Package IDs establish canonical precedence, entries are sorted by their
/// complete materialization data, byte-identical directory records collapse,
/// and every metadata disagreement or non-directory collision is rejected
/// before the destination root is touched.
#[cfg(test)]
fn frozen_vfs(
    packages: &[package::Id],
    layouts: Vec<(package::Id, StonePayloadLayoutRecord)>,
) -> Result<vfs::Tree<PendingFile>, Error> {
    frozen_vfs_until(packages, layouts, Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT)
}

fn frozen_vfs_until(
    packages: &[package::Id],
    layouts: Vec<(package::Id, StonePayloadLayoutRecord)>,
    deadline: Instant,
) -> Result<vfs::Tree<PendingFile>, Error> {
    require_frozen_materialization_deadline(deadline)?;
    let mut package_order = BTreeMap::new();
    for (order, package) in packages.iter().enumerate() {
        require_frozen_materialization_deadline(deadline)?;
        package_order.insert(package.clone(), order);
    }
    let mut entries = Vec::with_capacity(layouts.len());
    for (package, layout) in layouts {
        require_frozen_materialization_deadline(deadline)?;
        let order = package_order
            .get(&package)
            .copied()
            .ok_or_else(|| Error::UnexpectedFrozenLayoutPackage(package.clone()))?;
        entries.push(FrozenLayoutEntry::new(package, layout, order)?);
    }

    require_frozen_materialization_deadline(deadline)?;
    entries.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.package_order.cmp(&right.package_order))
            .then_with(|| left.kind_order.cmp(&right.kind_order))
            .then_with(|| left.source_order.cmp(&right.source_order))
            .then_with(|| left.layout.uid.cmp(&right.layout.uid))
            .then_with(|| left.layout.gid.cmp(&right.layout.gid))
            .then_with(|| left.layout.mode.cmp(&right.layout.mode))
            .then_with(|| left.layout.tag.cmp(&right.layout.tag))
    });
    require_frozen_materialization_deadline(deadline)?;

    let mut selected: Vec<FrozenLayoutEntry> = Vec::with_capacity(entries.len());
    for entry in entries {
        require_frozen_materialization_deadline(deadline)?;
        if let Some(previous) = selected.last()
            && previous.path == entry.path
        {
            if identical_directory_metadata(previous, &entry) {
                continue;
            }
            return Err(frozen_collision(&entry.path, previous, &entry));
        }
        selected.push(entry);
    }

    validate_frozen_tree_collisions_until(&selected, deadline)?;

    let mut builder = TreeBuilder::new();
    for entry in &selected {
        require_frozen_materialization_deadline(deadline)?;
        builder.push(entry.pending());
    }
    require_frozen_materialization_deadline(deadline)?;
    builder.bake();
    require_frozen_materialization_deadline(deadline)?;
    let tree = builder.tree()?;
    require_frozen_materialization_deadline(deadline)?;
    Ok(tree)
}

fn is_normalized_frozen_path(path: &str) -> bool {
    path.starts_with("/usr/")
        && !path.as_bytes().contains(&0)
        && !path.ends_with('/')
        && !path.contains("//")
        && !path.split('/').any(|component| component == "." || component == "..")
}

fn validate_frozen_tree_collisions_until(entries: &[FrozenLayoutEntry], deadline: Instant) -> Result<(), Error> {
    validate_frozen_tree_collisions_with_limits_until(
        entries,
        MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS,
        MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES,
        Some(deadline),
    )
}

#[cfg(test)]
fn validate_frozen_tree_collisions_with_limits(
    entries: &[FrozenLayoutEntry],
    max_directory_paths: usize,
    max_directory_bytes: usize,
) -> Result<(), Error> {
    validate_frozen_tree_collisions_with_limits_until(entries, max_directory_paths, max_directory_bytes, None)
}

fn validate_frozen_tree_collisions_with_limits_until(
    entries: &[FrozenLayoutEntry],
    max_directory_paths: usize,
    max_directory_bytes: usize,
    deadline: Option<Instant>,
) -> Result<(), Error> {
    require_blit_deadline(deadline)?;
    let explicit = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut directories = BTreeSet::new();
    let mut directory_bytes = 0usize;
    for entry in entries {
        require_blit_deadline(deadline)?;
        let mut parent = Path::new(&entry.path).parent();
        while let Some(parent_path) = parent {
            require_blit_deadline(deadline)?;
            let path = parent_path
                .to_str()
                .expect("validated frozen layout paths remain valid UTF-8");
            insert_frozen_materialized_directory(
                path,
                &mut directories,
                &mut directory_bytes,
                max_directory_paths,
                max_directory_bytes,
            )?;
            parent = parent_path.parent();
        }
    }
    for entry in entries {
        require_blit_deadline(deadline)?;
        if entry.is_directory() {
            insert_frozen_materialized_directory(
                &entry.path,
                &mut directories,
                &mut directory_bytes,
                max_directory_paths,
                max_directory_bytes,
            )?;
        } else if directories.remove(&entry.path) {
            directory_bytes = directory_bytes.saturating_sub(entry.path.len());
        }
    }

    let mut redirects = BTreeMap::new();
    for entry in entries {
        require_blit_deadline(deadline)?;
        if let StonePayloadLayoutFile::Symlink(target, _) = &entry.layout.file {
            let target = if target.starts_with('/') {
                target.to_string()
            } else {
                let parent = Path::new(&entry.path)
                    .parent()
                    .expect("validated frozen path has a parent")
                    .to_string_lossy();
                vfs::path::join(parent.as_ref(), target.as_str()).to_string()
            };
            if directories.contains(&target) {
                redirects.insert(entry.path.clone(), target);
            }
        }
    }

    // TreeBuilder's historical redirect cloning can place an explicit child
    // somewhere other than either its declared or validated effective path.
    // Frozen roots therefore reject every explicit descendant beneath a
    // directory symlink; packages must declare the canonical directory path.
    for entry in entries {
        require_blit_deadline(deadline)?;
        let mut ancestor = Path::new(&entry.path).parent();
        while let Some(ancestor_path) = ancestor {
            require_blit_deadline(deadline)?;
            let redirect = ancestor_path.to_string_lossy();
            if redirects.contains_key(redirect.as_ref()) {
                return Err(Error::FrozenDirectorySymlinkDescendant {
                    package: entry.package.clone(),
                    path: entry.path.clone().into_boxed_str(),
                    redirect: redirect.into_owned().into_boxed_str(),
                });
            }
            ancestor = ancestor_path.parent();
        }
    }

    // Redirect descendants have already failed closed, so every surviving
    // entry retains its declared path. Non-directory parents therefore need
    // only ancestor-key lookups; no redirect cross-product is necessary.
    for entry in entries {
        require_blit_deadline(deadline)?;
        let mut parent = Path::new(&entry.path).parent();
        while let Some(parent_path) = parent {
            require_blit_deadline(deadline)?;
            let parent_name = parent_path
                .to_str()
                .expect("validated frozen layout paths remain valid UTF-8");
            if let Some(ancestor) = explicit.get(parent_name)
                && !ancestor.is_directory()
            {
                return Err(frozen_collision(&entry.path, ancestor, entry));
            }
            parent = parent_path.parent();
        }
    }

    require_blit_deadline(deadline)?;
    Ok(())
}

fn insert_frozen_materialized_directory(
    path: &str,
    directories: &mut BTreeSet<String>,
    total_bytes: &mut usize,
    max_paths: usize,
    max_bytes: usize,
) -> Result<(), Error> {
    if directories.contains(path) {
        return Ok(());
    }
    let actual_paths = directories.len().saturating_add(1);
    if actual_paths > max_paths {
        return Err(Error::FrozenExecutableDirectoryLimit {
            limit: max_paths,
            actual: actual_paths,
        });
    }
    let actual_bytes = total_bytes.checked_add(path.len()).unwrap_or(usize::MAX);
    if actual_bytes > max_bytes {
        return Err(Error::FrozenExecutableDirectoryByteLimit {
            limit: max_bytes,
            actual: actual_bytes,
        });
    }
    directories.insert(path.to_owned());
    *total_bytes = actual_bytes;
    Ok(())
}

fn identical_directory_metadata(first: &FrozenLayoutEntry, second: &FrozenLayoutEntry) -> bool {
    first.is_directory()
        && second.is_directory()
        && first.layout.uid == second.layout.uid
        && first.layout.gid == second.layout.gid
        && first.layout.mode == second.layout.mode
        && first.layout.tag == second.layout.tag
}

fn frozen_collision(path: &str, first: &FrozenLayoutEntry, second: &FrozenLayoutEntry) -> Error {
    Error::FrozenPathCollision {
        path: path.to_owned(),
        first: first.package.clone(),
        second: second.package.clone(),
    }
}

/// Resolve an existing destination, or its existing parent plus final name,
/// and prove that materialization cannot reach any installation-root
/// namespace. This rejects canonical pathname/symlink aliases as well as
/// lexical descendants; retained capabilities provide the later write bound.
pub(crate) fn require_disjoint_materialization_target(
    installation: &Installation,
    requested: &Path,
) -> Result<PathBuf, Error> {
    installation.revalidate_root_directory()?;
    let installation_root = installation.root.canonicalize()?;
    let target = match requested.canonicalize() {
        Ok(target) => target,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            let name = requested.file_name().ok_or(Error::EphemeralInstallationRoot)?;
            let parent = requested
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .canonicalize()?;
            parent.join(name)
        }
        Err(source) => return Err(source.into()),
    };
    installation.revalidate_root_directory()?;

    if target.starts_with(&installation_root)
        || installation_root.starts_with(&target)
        || target.ancestors().any(has_cast_control_topology)
    {
        Err(Error::EphemeralInstallationRoot)
    } else {
        installation.revalidate_root_directory()?;
        Ok(target)
    }
}

pub(super) fn has_cast_control_topology(path: &Path) -> bool {
    [
        path.join(".cast"),
        path.join(".cast/root"),
        path.join(".cast/root/staging"),
    ]
    .into_iter()
    .all(|component| {
        fs::symlink_metadata(component)
            .is_ok_and(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
    })
}

/// Blit the packages to a filesystem root
///
/// This functionality is core to all Cast filesystem transactions, forming the entire
/// staging logic. For all the [`crate::package::Id`] present in the staging state,
/// query their stored [`StonePayloadLayoutBody`] and cache into a [`vfs::Tree`].
///
/// The new `/usr` filesystem is written in optimal order to a staging tree by making
/// use of the "at" family of functions (`mkdirat`, `openat`, etc) with relative
/// directory file descriptors. Writable outputs receive digest-verified private
/// inodes rather than aliases into the content-addressed store.
///
/// This produces a digest-verified private candidate which can then be
/// published atomically via [`Self::promote_staging`] without aliasing writable
/// state to the content-addressed store.
#[cfg(test)]
pub(crate) fn blit_root(
    installation: &Installation,
    tree: &vfs::Tree<PendingFile>,
    blit_target: &Path,
) -> Result<(), Error> {
    let _staging_coordinator = fixed_staging::lock_coordinator()?;
    blit_root_with_materialization(
        installation,
        tree,
        blit_target,
        AssetMaterialization::IndependentCopy,
        BlitExecution::Parallel,
    )
}

fn blit_root_from_admission(
    installation: &Installation,
    tree: &vfs::Tree<PendingFile>,
    admission: &ExternalMaterializationAdmission,
) -> Result<(), Error> {
    let _writer_coordinator = fixed_staging::lock_coordinator()?;
    let mut target = RetainedExternalMaterializationTarget::prepare_from(installation, admission)?;
    target
        .materialize(
            installation,
            tree,
            AssetMaterialization::IndependentCopy,
            BlitExecution::Parallel,
        )
        .map(drop)
}

#[cfg(test)]
fn blit_root_with_materialization(
    installation: &Installation,
    tree: &vfs::Tree<PendingFile>,
    blit_target: &Path,
    materialization: AssetMaterialization,
    execution: BlitExecution,
) -> Result<(), Error> {
    let mut target = RetainedExternalMaterializationTarget::prepare(installation, blit_target)?;
    target
        .materialize(installation, tree, materialization, execution)
        .map(drop)
}

fn blit_tree_into_open_root(
    installation: &Installation,
    tree: &vfs::Tree<PendingFile>,
    root_fd: RawFd,
    materialization: AssetMaterialization,
    execution: BlitExecution,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
    retained_top_level_usr: Option<&std::fs::File>,
) -> Result<(), Error> {
    require_blit_deadline(deadline)?;

    let progress = ProgressBar::new(1).with_style(
        ProgressStyle::with_template("\n|{bar:20.red/blue}| {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("■≡=- "),
    );
    progress.set_message("Blitting filesystem");
    progress.enable_steady_tick(Duration::from_millis(150));
    progress.tick();

    let now = Instant::now();

    progress.set_length(tree.len());
    progress.set_position(0_u64);

    // Metadata-only closures and packages containing only directories,
    // symlinks, or canonical empty files have no asset-store dependency.
    // Opening assets/v2 unconditionally made those valid closures fail with
    // ENOENT after cache unpacking correctly produced no asset directory.
    let mut requires_asset_cache = false;
    for item in tree.iter() {
        require_blit_deadline(deadline)?;
        if matches!(
            &item.layout.file,
            StonePayloadLayoutFile::Regular(digest, _) if *digest != EMPTY_FILE_DIGEST
        ) {
            requires_asset_cache = true;
            break;
        }
    }
    let cache = if requires_asset_cache {
        Some(AssetPool::open(installation)?)
    } else {
        None
    };

    let blit = || -> Result<BlitStats, Error> {
        let mut stats = BlitStats::default();
        require_blit_deadline(deadline)?;
        if let Some(root) = tree.structured() {
            if let Element::Directory(_, _, children) = root {
                if tree.len() != 0
                    && retained_top_level_usr.is_some()
                    && (children.len() != 1
                        || !matches!(children.first(), Some(Element::Directory(name, _, _)) if *name == "usr"))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "stateful candidate must contain exactly one top-level usr directory",
                    )
                    .into());
                }
                stats = stats.merge(blit_children(
                    root_fd,
                    cache.as_ref(),
                    children,
                    &progress,
                    materialization,
                    execution,
                    copy_manifest,
                    deadline,
                    retained_top_level_usr,
                )?);
            }
        }

        Ok(stats)
    };

    let stats = match execution {
        BlitExecution::Parallel => {
            // Stateful transactions retain the established parallel blit
            // path. The pool is dropped before Mason enters a namespace.
            let rayon_runtime = rayon::ThreadPoolBuilder::new().build().expect("rayon runtime");
            rayon_runtime.install(blit)?
        }
        // Frozen roots use canonical tree order without host-sized scheduling.
        BlitExecution::Sequential => blit()?,
    };
    require_blit_deadline(deadline)?;

    progress.finish_and_clear();

    let elapsed = now.elapsed();
    let num_entries = stats.num_entries();

    println!(
        "\n{} entries blitted in {} {}",
        num_entries.to_string().bold(),
        format!("{:.2}s", elapsed.as_secs_f32()).bold(),
        format!("({:.1}k / s)", num_entries as f32 / elapsed.as_secs_f32() / 1_000.0).dim()
    );

    require_blit_deadline(deadline)?;
    Ok(())
}

/// Recursively write a directory, or a single flat inode, to the staging tree.
/// Care is taken to retain the directory file descriptor to avoid costly path
/// resolution at runtime.
fn blit_element(
    parent: RawFd,
    cache: Option<&AssetPool>,
    element: Element<'_, PendingFile>,
    progress: &ProgressBar,
    materialization: AssetMaterialization,
    execution: BlitExecution,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
    retained_usr: Option<&std::fs::File>,
) -> Result<BlitStats, Error> {
    require_blit_deadline(deadline)?;
    let mut stats = BlitStats::default();

    progress.inc(1);

    let (_, item) = match &element {
        Element::Directory(_, item, _) => ("directory", item),
        Element::Child(_, item) => ("file", item),
    };

    trace!(
        progress = progress.position() as f32 / progress.length().unwrap_or(1) as f32,
        current = progress.position() as usize,
        total = progress.length().unwrap_or(0) as usize,
        event_type = "progress_update",
        "Blitting {}",
        item.path()
    );

    match element {
        Element::Directory(name, item, children) => {
            if name == "usr"
                && let Some(retained_usr) = retained_usr
            {
                let active_mode = match materialization {
                    AssetMaterialization::HardLink => item.layout.mode,
                    AssetMaterialization::IndependentCopy => item.layout.mode | 0o700,
                };
                fchmod(retained_usr.as_raw_fd(), Mode::from_bits_truncate(active_mode))?;
                stats.num_dirs += 1;
                stats = stats.merge(blit_children(
                    retained_usr.as_raw_fd(),
                    cache,
                    children,
                    progress,
                    materialization,
                    execution,
                    copy_manifest,
                    deadline,
                    None,
                )?);
                if materialization == AssetMaterialization::IndependentCopy {
                    fchmod(retained_usr.as_raw_fd(), Mode::from_bits_truncate(item.layout.mode))?;
                }
                return Ok(stats);
            }

            // Construct within the parent
            blit_element_item(
                parent,
                cache,
                name,
                item,
                &mut stats,
                materialization,
                copy_manifest,
                deadline,
            )?;

            // open the new dir
            let newdir = openat_owned(
                parent,
                name,
                OFlag::O_CLOEXEC | OFlag::O_RDONLY | OFlag::O_DIRECTORY,
                Mode::empty(),
            )?;

            stats = stats.merge(blit_children(
                newdir.as_raw_fd(),
                cache,
                children,
                progress,
                materialization,
                execution,
                copy_manifest,
                deadline,
                None,
            )?);

            // Frozen directories are created owner-accessible so restrictive
            // final modes cannot prevent their own children from being
            // materialized. Apply the declared mode only after the complete
            // subtree exists. Stateful blits retain their established mode
            // timing.
            if materialization == AssetMaterialization::IndependentCopy {
                fchmod(newdir.as_raw_fd(), Mode::from_bits_truncate(item.layout.mode))?;
            }

            Ok(stats)
        }
        Element::Child(name, item) => {
            if name == "usr" && retained_usr.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "candidate top-level usr entry is not a directory",
                )
                .into());
            }
            blit_element_item(
                parent,
                cache,
                name,
                item,
                &mut stats,
                materialization,
                copy_manifest,
                deadline,
            )?;

            Ok(stats)
        }
    }
}

fn blit_children(
    parent: RawFd,
    cache: Option<&AssetPool>,
    children: Vec<Element<'_, PendingFile>>,
    progress: &ProgressBar,
    materialization: AssetMaterialization,
    execution: BlitExecution,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
    retained_usr: Option<&std::fs::File>,
) -> Result<BlitStats, Error> {
    require_blit_deadline(deadline)?;
    match execution {
        BlitExecution::Parallel => {
            let current_span = tracing::Span::current();
            children
                .into_par_iter()
                .map(|child| {
                    let _guard = current_span.enter();
                    blit_element(
                        parent,
                        cache,
                        child,
                        progress,
                        materialization,
                        execution,
                        copy_manifest,
                        deadline,
                        retained_usr,
                    )
                })
                .try_reduce(BlitStats::default, |left, right| Ok(left.merge(right)))
        }
        BlitExecution::Sequential => children.into_iter().try_fold(BlitStats::default(), |stats, child| {
            blit_element(
                parent,
                cache,
                child,
                progress,
                materialization,
                execution,
                copy_manifest,
                deadline,
                retained_usr,
            )
            .map(|child_stats| stats.merge(child_stats))
        }),
    }
}

/// Write a single inode into the staging tree.
///
/// # Arguments
///
/// * `parent`  - raw file descriptor for parent directory in which the inode is being record to
/// * `cache`   - raw file descriptor for the system asset pool tree
/// * `subpath` - the base name of the new inode
/// * `item`    - New inode being recorded
fn blit_element_item(
    parent: RawFd,
    cache: Option<&AssetPool>,
    subpath: &str,
    item: &PendingFile,
    stats: &mut BlitStats,
    materialization: AssetMaterialization,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
) -> Result<(), Error> {
    require_blit_deadline(deadline)?;
    match &item.layout.file {
        StonePayloadLayoutFile::Regular(id, _) => {
            // Link relative from cache to target.
            let fp = frozen_asset_path(*id);

            match *id {
                // Mystery empty-file hash. Do not allow dupes!
                // https://github.com/serpent-os/tools/issues/372
                EMPTY_FILE_DIGEST => {
                    let _file = openat_owned(
                        parent,
                        subpath,
                        OFlag::O_CLOEXEC | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_WRONLY,
                        Mode::from_bits_truncate(0o600),
                    )?;
                }
                // Regular file
                _ => {
                    let cache = cache.ok_or_else(|| {
                        io::Error::other("non-empty regular blit entry has no authenticated asset-cache descriptor")
                    })?;
                    match materialization {
                        AssetMaterialization::HardLink => {
                            link_asset(cache, &fp, parent, subpath)?;
                        }
                        AssetMaterialization::IndependentCopy => {
                            copy_asset(
                                cache,
                                &fp,
                                *id,
                                parent,
                                subpath,
                                item.layout.mode,
                                copy_manifest,
                                deadline,
                            )?;
                        }
                    }
                }
            }

            // Creation modes are filtered through the process umask. Apply
            // the package's complete mode after materialization instead. An
            // independent copy applies it through the still-pinned output
            // descriptor inside `copy_asset`; do not reopen that trust
            // boundary by chmodding a pathname here.
            if materialization == AssetMaterialization::HardLink || *id == EMPTY_FILE_DIGEST {
                fchmodat(
                    Some(parent),
                    subpath,
                    Mode::from_bits_truncate(item.layout.mode),
                    nix::sys::stat::FchmodatFlags::NoFollowSymlink,
                )?;
            }

            stats.num_files += 1;
        }
        StonePayloadLayoutFile::Symlink(source, _) => {
            symlinkat(source.as_str(), Some(parent), subpath)?;
            stats.num_symlinks += 1;
        }
        StonePayloadLayoutFile::Directory(_) => {
            let mode = match materialization {
                AssetMaterialization::HardLink => item.layout.mode,
                AssetMaterialization::IndependentCopy => item.layout.mode | 0o700,
            };
            mkdirat(parent, subpath, Mode::from_bits_truncate(mode))?;
            fchmodat(
                Some(parent),
                subpath,
                Mode::from_bits_truncate(mode),
                nix::sys::stat::FchmodatFlags::NoFollowSymlink,
            )?;
            stats.num_dirs += 1;
        }

        // Unimplemented
        StonePayloadLayoutFile::CharacterDevice(_)
        | StonePayloadLayoutFile::BlockDevice(_)
        | StonePayloadLayoutFile::Fifo(_)
        | StonePayloadLayoutFile::Socket(_)
        | StonePayloadLayoutFile::Unknown(..) => {}
    };

    Ok(())
}

fn frozen_asset_path(digest: u128) -> PathBuf {
    let hash = format!("{digest:02x}");
    let directory = if hash.len() >= 10 {
        PathBuf::from(&hash[..2]).join(&hash[2..4]).join(&hash[4..6])
    } else {
        PathBuf::new()
    };
    directory.join(hash)
}

const MAX_BLIT_ASSET_BYTES: u64 = crate::request::DEFAULT_DOWNLOAD_LIMITS.max_bytes;
const ASSET_COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AssetDirectoryIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
}

impl AssetDirectoryIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> io::Result<Self> {
        if metadata.mode() & nix::libc::S_IFMT != nix::libc::S_IFDIR {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "asset-cache anchor is not a directory",
            ));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AssetFileWitness {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl AssetFileWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

/// Retained descriptor chain from the installation root to `assets/v2`.
///
/// Acquisition may cross host mount points before reaching the configured
/// installation root. Every cache component below that explicit trust anchor
/// is opened with `RESOLVE_BENEATH | NO_SYMLINKS | NO_MAGICLINKS | NO_XDEV`.
/// Both named anchors are then re-opened and compared around each operation so
/// replacing either public pathname fails closed instead of silently changing
/// the source tree.
struct AssetPool {
    installation_path: PathBuf,
    installation_root: fs::File,
    installation_identity: AssetDirectoryIdentity,
    relative_path: PathBuf,
    root: fs::File,
    identity: AssetDirectoryIdentity,
}

impl AssetPool {
    fn open(installation: &Installation) -> Result<Self, Error> {
        let installation_path = lexical_absolute_path(&installation.root)?;
        let assets_path = lexical_absolute_path(&installation.assets_path("v2"))?;
        let relative_path = assets_path
            .strip_prefix(&installation_path)
            .ok()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "asset pool is outside installation root"))?
            .to_owned();
        require_beneath_path(&relative_path)?;

        let installation_root = open_absolute_directory(&installation_path)?;
        let installation_identity = asset_directory_identity(&installation_root)?;
        let root = openat2_frozen(
            installation_root.as_raw_fd(),
            &relative_path,
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            asset_resolve_flags(),
        )?;
        let identity = asset_directory_identity(&root)?;
        let pool = Self {
            installation_path,
            installation_root,
            installation_identity,
            relative_path,
            root,
            identity,
        };
        pool.revalidate()?;
        Ok(pool)
    }

    fn revalidate(&self) -> Result<(), Error> {
        if asset_directory_identity(&self.installation_root)? != self.installation_identity
            || asset_directory_identity(&self.root)? != self.identity
        {
            return Err(asset_copy_error("retained asset-cache anchor changed"));
        }

        let named_installation = open_absolute_directory(&self.installation_path)?;
        if asset_directory_identity(&named_installation)? != self.installation_identity {
            return Err(asset_copy_error(
                "installation root was replaced while using asset cache",
            ));
        }
        let named_root = openat2_frozen(
            named_installation.as_raw_fd(),
            &self.relative_path,
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            asset_resolve_flags(),
        )?;
        if asset_directory_identity(&named_root)? != self.identity {
            return Err(asset_copy_error("asset pool was replaced while materializing a root"));
        }
        Ok(())
    }

    fn open_asset(&self, path: &Path) -> Result<OpenedAsset, Error> {
        self.revalidate()?;
        require_beneath_path(path)?;
        let parent_path = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| asset_copy_error("asset path has no descriptor-rooted parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| asset_copy_error("asset path has no final component"))?
            .to_owned();
        require_single_component(Path::new(&name))?;
        let parent = openat2_frozen(
            self.root.as_raw_fd(),
            parent_path,
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            asset_resolve_flags(),
        )?;
        // Probe through O_PATH first so a hostile FIFO or device is rejected
        // without invoking its open handler. Only an exact regular inode is
        // then opened for bounded nonblocking reads.
        let probe = openat2_frozen(
            parent.as_raw_fd(),
            Path::new(&name),
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            asset_resolve_flags(),
        )?;
        let witness = asset_source_witness(&probe)?;
        let file = openat2_frozen(
            parent.as_raw_fd(),
            Path::new(&name),
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            asset_resolve_flags(),
        )?;
        if asset_source_witness(&file)? != witness {
            return Err(asset_copy_error(
                "cached asset was replaced between type probe and open",
            ));
        }
        self.revalidate()?;
        Ok(OpenedAsset {
            path: path.to_owned(),
            parent,
            name,
            file,
            witness,
        })
    }
}

struct OpenedAsset {
    path: PathBuf,
    parent: fs::File,
    name: OsString,
    file: fs::File,
    witness: AssetFileWitness,
}

fn asset_resolve_flags() -> u64 {
    (nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_XDEV) as u64
}

fn lexical_absolute_path(path: &Path) -> io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            PathComponent::RootDir => {}
            PathComponent::Normal(component) => normalized.push(component),
            PathComponent::CurDir => {}
            PathComponent::ParentDir | PathComponent::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "asset-cache path contains a parent or platform prefix component",
                ));
            }
        }
    }
    Ok(normalized)
}

fn open_absolute_directory(path: &Path) -> Result<fs::File, Error> {
    let relative = path
        .strip_prefix(Path::new("/"))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "asset-cache anchor is not absolute"))?;
    let system_root = fs::File::open("/")?;
    if relative.as_os_str().is_empty() {
        return Ok(system_root);
    }
    require_beneath_path(relative)?;
    openat2_frozen(
        system_root.as_raw_fd(),
        relative,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
    )
    .map_err(Error::from)
}

fn require_beneath_path(path: &Path) -> Result<(), Error> {
    if path.is_absolute()
        || path.as_os_str().is_empty()
        || !path
            .components()
            .all(|component| matches!(component, PathComponent::Normal(_)))
    {
        return Err(asset_copy_error(
            "asset-cache path is not a non-empty normalized relative path",
        ));
    }
    Ok(())
}

fn require_single_component(path: &Path) -> Result<(), Error> {
    require_beneath_path(path)?;
    if path.components().count() != 1 {
        return Err(asset_copy_error("asset-cache leaf is not one path component"));
    }
    Ok(())
}

fn asset_directory_identity(file: &fs::File) -> Result<AssetDirectoryIdentity, Error> {
    AssetDirectoryIdentity::from_metadata(&file.metadata()?).map_err(Error::from)
}

fn asset_source_witness(file: &fs::File) -> Result<AssetFileWitness, Error> {
    let witness = AssetFileWitness::from_metadata(&file.metadata()?);
    if witness.mode & nix::libc::S_IFMT != nix::libc::S_IFREG {
        return Err(asset_copy_error("cached asset is not a regular file"));
    }
    if witness.links == 0 {
        return Err(asset_copy_error("cached asset has no filesystem links"));
    }
    if witness.length > MAX_BLIT_ASSET_BYTES {
        return Err(asset_copy_error(format!(
            "cached asset is {} bytes, exceeding the {}-byte copy limit",
            witness.length, MAX_BLIT_ASSET_BYTES
        )));
    }
    Ok(witness)
}

fn open_named_asset(asset: &OpenedAsset) -> Result<fs::File, Error> {
    openat2_frozen(
        asset.parent.as_raw_fd(),
        Path::new(&asset.name),
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        asset_resolve_flags(),
    )
    .map_err(Error::from)
}

fn require_asset_unchanged(pool: &AssetPool, asset: &OpenedAsset) -> Result<(), Error> {
    let descriptor = asset_source_witness(&asset.file)?;
    let reopened = open_named_asset(asset)?;
    let named = asset_source_witness(&reopened)?;
    let full_reopened = pool.open_asset(&asset.path)?;
    let final_descriptor = asset_source_witness(&asset.file)?;
    pool.revalidate()?;
    if descriptor != asset.witness
        || named != asset.witness
        || full_reopened.witness != asset.witness
        || final_descriptor != asset.witness
    {
        return Err(asset_copy_error(
            "cached asset changed or was replaced while being materialized",
        ));
    }
    Ok(())
}

fn asset_copy_error(message: impl Into<String>) -> Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into()).into()
}

fn cleanup_failed_materialization(
    parent: RawFd,
    target: &str,
    created: &fs::File,
    expected_links_after: u64,
    primary: Error,
) -> Error {
    let created_identity = match created.metadata() {
        Ok(metadata) => AssetFileWitness::from_metadata(&metadata),
        Err(cleanup) => {
            return asset_copy_error(format!(
                "asset materialization failed: {primary}; stat during cleanup also failed: {cleanup}"
            ));
        }
    };
    let named = openat2_frozen(
        parent,
        Path::new(target),
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_XDEV) as u64,
    );
    match named {
        Ok(named) => {
            let named_identity = match named.metadata() {
                Ok(metadata) => AssetFileWitness::from_metadata(&metadata),
                Err(cleanup) => {
                    return asset_copy_error(format!(
                        "asset materialization failed: {primary}; stat named cleanup target also failed: {cleanup}"
                    ));
                }
            };
            if (named_identity.device, named_identity.inode) != (created_identity.device, created_identity.inode) {
                return asset_copy_error(format!(
                    "asset materialization failed: {primary}; refusing to unlink a replacement cleanup target"
                ));
            }
            if let Err(cleanup) = unlinkat(Some(parent), target, UnlinkatFlags::NoRemoveDir) {
                return asset_copy_error(format!(
                    "asset materialization failed: {primary}; unlink cleanup also failed: {cleanup}"
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(cleanup) => {
            return asset_copy_error(format!(
                "asset materialization failed: {primary}; reopen during cleanup also failed: {cleanup}"
            ));
        }
    }

    match created.metadata() {
        Ok(metadata) if metadata.nlink() == expected_links_after => primary,
        Ok(metadata) => asset_copy_error(format!(
            "asset materialization failed: {primary}; cleanup left {} links, expected {expected_links_after}",
            metadata.nlink()
        )),
        Err(cleanup) => asset_copy_error(format!(
            "asset materialization failed: {primary}; final cleanup stat also failed: {cleanup}"
        )),
    }
}

fn link_asset(pool: &AssetPool, source: &Path, parent: RawFd, target: &str) -> Result<(), Error> {
    require_single_component(Path::new(target))?;
    let asset = pool.open_asset(source)?;
    linkat(
        Some(asset.parent.as_raw_fd()),
        Path::new(&asset.name),
        Some(parent),
        Path::new(target),
        nix::unistd::LinkatFlags::NoSymlinkFollow,
    )?;

    let result = (|| -> Result<(), Error> {
        let target_file = openat2_frozen(
            parent,
            Path::new(target),
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            asset_resolve_flags(),
        )?;
        let target_witness = asset_source_witness(&target_file)?;
        let source_after = asset_source_witness(&asset.file)?;
        if (target_witness.device, target_witness.inode) != (asset.witness.device, asset.witness.inode)
            || source_after.links != asset.witness.links.saturating_add(1)
            || source_after.length != asset.witness.length
            || source_after.mode != asset.witness.mode
        {
            return Err(asset_copy_error(
                "hardlinked asset changed or target names a different inode",
            ));
        }
        let reopened = open_named_asset(&asset)?;
        let named = asset_source_witness(&reopened)?;
        let full_reopened = pool.open_asset(&asset.path)?;
        if (named.device, named.inode) != (asset.witness.device, asset.witness.inode)
            || (full_reopened.witness.device, full_reopened.witness.inode)
                != (asset.witness.device, asset.witness.inode)
            || full_reopened.witness.links != source_after.links
        {
            return Err(asset_copy_error(
                "hardlinked cached asset was replaced during publication",
            ));
        }
        pool.revalidate()
    })();
    if let Err(error) = result {
        return Err(cleanup_failed_materialization(
            parent,
            target,
            &asset.file,
            asset.witness.links,
            error,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetCopyCheckpoint {
    SourceOpened,
    BytesCopied,
}

/// Copy one cached asset into a fresh inode under `parent`.
///
/// Writable package roots are modified by transaction triggers or build steps,
/// so aliasing them to the persistent content store would let a write or chmod
/// corrupt cached assets. Keep descriptor-relative traversal while giving each
/// destination independent digest-verified bytes and metadata.
fn copy_asset(
    pool: &AssetPool,
    source: &Path,
    expected_digest: u128,
    parent: RawFd,
    target: &str,
    mode: u32,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
) -> Result<(), Error> {
    copy_asset_with_checkpoint(
        pool,
        source,
        expected_digest,
        parent,
        target,
        mode,
        copy_manifest,
        deadline,
        |_| {},
    )
}

fn copy_asset_with_checkpoint<F>(
    pool: &AssetPool,
    source: &Path,
    expected_digest: u128,
    parent: RawFd,
    target: &str,
    mode: u32,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
    mut checkpoint: F,
) -> Result<(), Error>
where
    F: FnMut(AssetCopyCheckpoint),
{
    require_blit_deadline(deadline)?;
    require_single_component(Path::new(target))?;
    let asset = pool.open_asset(source)?;
    if let Some(copy_manifest) = copy_manifest {
        copy_manifest.require_length(expected_digest, asset.witness.length)?;
    }
    checkpoint(AssetCopyCheckpoint::SourceOpened);
    pool.revalidate()?;
    let target_fd = openat2_frozen(
        parent,
        Path::new(target),
        nix::libc::O_CLOEXEC
            | nix::libc::O_CREAT
            | nix::libc::O_EXCL
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_WRONLY,
        asset_resolve_flags(),
    )?;

    let result = (|| -> Result<(), Error> {
        fchmod(target_fd.as_raw_fd(), Mode::from_bits_truncate(0o600))?;
        require_private_copy_target(&target_fd, 0, 0o600)?;
        copy_fd_exact(
            asset.file.as_raw_fd(),
            target_fd.as_raw_fd(),
            asset.witness.length,
            expected_digest,
            deadline,
        )?;
        checkpoint(AssetCopyCheckpoint::BytesCopied);
        require_asset_unchanged(pool, &asset)?;
        fchmod(target_fd.as_raw_fd(), Mode::from_bits_truncate(mode))?;
        target_fd.sync_data()?;
        let expected_target = require_copy_target(&target_fd, asset.witness.length, mode)?;
        let named_target = openat2_frozen(
            parent,
            Path::new(target),
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            asset_resolve_flags(),
        )?;
        let named_witness = require_copy_target(&named_target, asset.witness.length, mode)?;
        let final_target = require_copy_target(&target_fd, asset.witness.length, mode)?;
        if named_witness != expected_target || final_target != expected_target {
            return Err(asset_copy_error(
                "copied asset target changed or was replaced before publication",
            ));
        }
        Ok(())
    })();
    if let Err(error) = result {
        return Err(cleanup_failed_materialization(parent, target, &target_fd, 0, error));
    }

    Ok(())
}

fn require_private_copy_target(file: &fs::File, expected_length: u64, expected_mode: u32) -> Result<(), Error> {
    let witness = require_copy_target(file, expected_length, expected_mode)?;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if witness.owner != effective_owner {
        return Err(asset_copy_error(
            "fresh asset-copy target is not owned by the effective user",
        ));
    }
    Ok(())
}

fn require_copy_target(file: &fs::File, expected_length: u64, expected_mode: u32) -> Result<AssetFileWitness, Error> {
    let witness = AssetFileWitness::from_metadata(&file.metadata()?);
    let expected_permissions = expected_mode & 0o7777;
    if witness.mode & nix::libc::S_IFMT != nix::libc::S_IFREG
        || witness.links != 1
        || witness.length != expected_length
        || witness.mode & 0o7777 != expected_permissions
    {
        return Err(asset_copy_error(format!(
            "asset-copy target metadata mismatch: mode {:#o}, links {}, length {}; expected permissions {:#o}, one link, length {}",
            witness.mode, witness.links, witness.length, expected_permissions, expected_length
        )));
    }
    Ok(witness)
}

fn openat_owned<P: ?Sized + nix::NixPath>(parent: RawFd, path: &P, flags: OFlag, mode: Mode) -> Result<OwnedFd, Errno> {
    fcntl::openat(parent, path, flags, mode).map(raw_fd_into_owned)
}

fn raw_fd_into_owned(fd: RawFd) -> OwnedFd {
    // SAFETY: every successful nix open/openat call returns one newly owned
    // descriptor. Ownership is transferred exactly once to OwnedFd here.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

fn copy_fd_exact(
    source: RawFd,
    target: RawFd,
    expected_length: u64,
    expected_digest: u128,
    deadline: Option<Instant>,
) -> Result<(), Error> {
    if expected_length > MAX_BLIT_ASSET_BYTES {
        return Err(asset_copy_error(format!(
            "asset-copy length {expected_length} exceeds {MAX_BLIT_ASSET_BYTES} bytes"
        )));
    }
    let mut buffer = [0_u8; ASSET_COPY_BUFFER_BYTES];
    let mut remaining = expected_length;
    let mut hasher = StoneDigestWriterHasher::new();

    while remaining != 0 {
        require_blit_deadline(deadline)?;
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| asset_copy_error("asset-copy chunk length is not representable"))?;
        let read_count = match read(source, &mut buffer[..requested]) {
            Ok(count) => count,
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
        };
        if read_count == 0 {
            return Err(asset_copy_error(format!(
                "cached asset ended early with {remaining} bytes still required"
            )));
        }
        hasher.update(&buffer[..read_count]);
        remaining = remaining
            .checked_sub(read_count as u64)
            .ok_or_else(|| asset_copy_error("asset-copy byte count underflow"))?;

        let mut written = 0;
        while written < read_count {
            require_blit_deadline(deadline)?;
            match write(target, &buffer[written..read_count]) {
                Ok(0) => return Err(Errno::EIO.into()),
                Ok(count) => written += count,
                Err(Errno::EINTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    require_blit_deadline(deadline)?;
    let trailing = loop {
        match read(source, &mut buffer[..1]) {
            Ok(count) => break count,
            Err(Errno::EINTR) => require_blit_deadline(deadline)?,
            Err(error) => return Err(error.into()),
        }
    };
    if trailing != 0 {
        return Err(asset_copy_error(format!(
            "cached asset exceeds its pinned {expected_length}-byte length"
        )));
    }

    let actual_digest = hasher.digest128();
    if actual_digest != expected_digest {
        return Err(asset_copy_error(format!(
            "cached asset digest mismatch: expected {expected_digest:032x}, got {actual_digest:032x}"
        )));
    }
    Ok(())
}

const STATE_TREE_DIRECTORY_MODE: u32 = 0o755;
const STATE_ID_MODE: u32 = 0o644;
const STATE_ID_TEMPORARY_MODE: u32 = 0o600;
const STATE_ID_NAME: &str = ".stateID";
const STATE_ID_TEMPORARY_NAME: &str = ".cast-state-id.tmp";
const STATE_ID_C_NAME: &CStr = c".stateID";
const STATE_ID_TEMPORARY_C_NAME: &CStr = c".cast-state-id.tmp";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StateMetadataDirectoryWitness {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
}

impl StateMetadataDirectoryWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode() & 0o7777,
            owner: metadata.uid(),
        }
    }
}

#[derive(Debug)]
struct StateMetadataDirectory {
    path: PathBuf,
    file: std::fs::File,
    witness: StateMetadataDirectoryWitness,
}

fn record_state_id(root: &Path, state: state::Id) -> Result<(), Error> {
    let root_path = state_metadata_absolute_path(root)?;
    let root = open_or_create_state_metadata_root(&root_path)?;
    let usr_path = root_path.join("usr");
    let usr =
        open_or_create_state_metadata_directory(&root.file, OsStr::new("usr"), &usr_path, STATE_TREE_DIRECTORY_MODE)?;

    write_state_id(&usr, state.to_string().as_bytes())?;
    usr.file.sync_all()?;
    root.file.sync_all()?;
    require_state_metadata_directory(&root.file, &usr)?;
    require_named_state_metadata_root(&root_path, &root)?;
    Ok(())
}

/// Write `.stateID` beneath the exact wrapper retained before candidate
/// materialization. No pathname is reopened as write authority; the returned
/// `/usr` descriptor is the same inode subsequently handed to tree-identity
/// preparation.
fn record_state_id_retained(
    root: &fixed_staging::RetainedFixedStaging,
    candidate_usr: &std::fs::File,
    state: state::Id,
) -> Result<(), Error> {
    let root_file = root.directory();
    let root_path = root.path();
    let root_witness = state_metadata_directory_witness(root_file, root_path)?;
    require_no_default_acl(root_file, root_path)?;

    let usr_path = root_path.join("usr");
    let usr_file = candidate_usr.try_clone()?;
    let usr = StateMetadataDirectory {
        path: usr_path.clone(),
        witness: state_metadata_directory_witness(&usr_file, &usr_path)?,
        file: usr_file,
    };
    require_state_metadata_directory(root_file, &usr)?;

    fixed_staging::before_retained_state_metadata();
    write_state_id(&usr, state.to_string().as_bytes())?;
    usr.file.sync_all()?;
    root_file.sync_all()?;
    require_state_metadata_directory(root_file, &usr)?;
    if state_metadata_directory_witness(root_file, root_path)? != root_witness {
        return Err(io::Error::other(format!(
            "retained state metadata root changed while writing {}",
            usr_path.display()
        ))
        .into());
    }
    require_no_default_acl(root_file, root_path)?;
    require_state_metadata_directory(root_file, &usr)?;
    Ok(())
}

fn revalidate_fixed_staging(
    retained: Option<&fixed_staging::RetainedFixedStaging>,
    installation: &Installation,
) -> Result<(), Error> {
    retained
        .map(|retained| retained.revalidate(installation))
        .transpose()
        .map(|_| ())
        .map_err(|source| Error::StatefulCandidateMaterialization {
            source: Box::new(source),
        })
}

fn state_metadata_absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "state metadata root path is empty",
        ));
    }
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            PathComponent::RootDir | PathComponent::CurDir => {}
            PathComponent::Normal(component) => normalized.push(component),
            PathComponent::ParentDir | PathComponent::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "state metadata root contains a parent or platform prefix component",
                ));
            }
        }
    }
    Ok(normalized)
}

fn open_or_create_state_metadata_root(path: &Path) -> Result<StateMetadataDirectory, Error> {
    match open_absolute_state_metadata_path(path) {
        Ok(pinned) => {
            let expected =
                normalize_recoverable_state_metadata_directory(&pinned, path, STATE_TREE_DIRECTORY_MODE, false)?;
            let file = open_absolute_state_metadata_directory(path)?;
            let actual = state_metadata_directory_witness(&file, path)?;
            require_no_default_acl(&file, path)?;
            if actual != expected {
                return Err(io::Error::other(format!(
                    "state metadata root was replaced while reopening {}",
                    path.display()
                ))
                .into());
            }
            Ok(StateMetadataDirectory {
                path: path.to_owned(),
                file,
                witness: expected,
            })
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound && path != Path::new("/") => {
            let parent_path = path.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "state metadata root has no parent directory",
                )
            })?;
            let name = path.file_name().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "state metadata root has no final component",
                )
            })?;
            let parent = open_absolute_state_metadata_directory(parent_path)?;
            let parent_witness = state_metadata_directory_witness(&parent, parent_path)?;
            let root = open_or_create_state_metadata_directory(&parent, name, path, STATE_TREE_DIRECTORY_MODE)?;
            let named_parent = open_absolute_state_metadata_directory(parent_path)?;
            if state_metadata_directory_witness(&named_parent, parent_path)? != parent_witness {
                return Err(io::Error::other(format!(
                    "state metadata root parent was replaced while creating {}",
                    path.display()
                ))
                .into());
            }
            Ok(root)
        }
        Err(source) => Err(source.into()),
    }
}

fn open_or_create_state_metadata_directory(
    parent: &std::fs::File,
    name: &OsStr,
    path: &Path,
    creation_mode: u32,
) -> Result<StateMetadataDirectory, Error> {
    let name_c = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "state metadata component contains NUL"))?;
    let created = mkdirat_state_metadata(parent.as_raw_fd(), &name_c, creation_mode)?;
    let pinned = open_state_metadata_at(
        parent.as_raw_fd(),
        Path::new(name),
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
    )?;

    let expected = normalize_recoverable_state_metadata_directory(&pinned, path, creation_mode, created)?;

    let file = open_state_metadata_at(
        parent.as_raw_fd(),
        Path::new(name),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
    )?;
    let actual = state_metadata_directory_witness(&file, path)?;
    require_no_default_acl(&file, path)?;
    if actual != expected {
        return Err(io::Error::other(format!(
            "state metadata directory was replaced while opening: {}",
            path.display()
        ))
        .into());
    }

    let directory = StateMetadataDirectory {
        path: path.to_owned(),
        file,
        witness: expected,
    };
    directory.file.sync_all()?;
    parent.sync_all()?;
    require_state_metadata_directory(parent, &directory)?;
    Ok(directory)
}

fn normalize_recoverable_state_metadata_directory(
    file: &std::fs::File,
    path: &Path,
    requested_mode: u32,
    created: bool,
) -> io::Result<StateMetadataDirectoryWitness> {
    if !created && let Ok(witness) = state_metadata_directory_witness(file, path) {
        return Ok(witness);
    }

    require_fresh_state_metadata_directory(file, path, requested_mode)?;
    chmod_path_descriptor(file, requested_mode)?;
    let witness = state_metadata_directory_witness(file, path)?;
    if witness.mode != requested_mode {
        return Err(io::Error::other(format!(
            "recovered state metadata directory has mode {:04o}, expected {requested_mode:04o}: {}",
            witness.mode,
            path.display()
        )));
    }
    Ok(witness)
}

fn open_absolute_state_metadata_path(path: &Path) -> io::Result<std::fs::File> {
    open_state_metadata_at(
        AT_FDCWD,
        path,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
    )
}

fn open_absolute_state_metadata_directory(path: &Path) -> io::Result<std::fs::File> {
    open_state_metadata_at(
        AT_FDCWD,
        path,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
    )
}

fn open_state_metadata_at(parent: RawFd, path: &Path, flags: i32) -> io::Result<std::fs::File> {
    let resolve = if path.is_absolute() {
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64
    } else {
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64
    };
    openat2_frozen(parent, path, flags, resolve).map(|file| file.into_parts().0)
}

fn mkdirat_state_metadata(parent: RawFd, name: &CStr, mode: u32) -> io::Result<bool> {
    loop {
        // SAFETY: parent is a live directory descriptor and name is one
        // retained NUL-terminated component. mkdirat never follows that final
        // component.
        if unsafe { nix::libc::mkdirat(parent, name.as_ptr(), mode) } == 0 {
            return Ok(true);
        }
        let source = io::Error::last_os_error();
        match source.kind() {
            io::ErrorKind::Interrupted => {}
            io::ErrorKind::AlreadyExists => return Ok(false),
            _ => return Err(source),
        }
    }
}

fn require_fresh_state_metadata_directory(file: &std::fs::File, path: &Path, requested_mode: u32) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    // A successful mkdir in this call may expose only an owner-owned subset
    // of the requested bits. Anything else could be a replacement and must
    // not be chmod-laundered through the retained descriptor.
    if metadata.file_type().is_dir() && metadata.uid() == effective_user_id() && mode & !requested_mode == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "fresh state metadata directory is not recoverable mkdir residue: {} (uid={}, mode={mode:04o})",
                path.display(),
                metadata.uid()
            ),
        ))
    }
}

fn state_metadata_directory_witness(file: &std::fs::File, path: &Path) -> io::Result<StateMetadataDirectoryWitness> {
    let metadata = file.metadata()?;
    let witness = StateMetadataDirectoryWitness::from_metadata(&metadata);
    if metadata.file_type().is_dir()
        && witness.owner == effective_user_id()
        && witness.mode & 0o7000 == 0
        && witness.mode & 0o022 == 0
        && witness.mode & 0o700 == 0o700
    {
        Ok(witness)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "state metadata component is not one safe owner-controlled directory: {} (uid={}, mode={:04o})",
                path.display(),
                witness.owner,
                witness.mode
            ),
        ))
    }
}

fn require_state_metadata_directory(parent: &std::fs::File, expected: &StateMetadataDirectory) -> Result<(), Error> {
    if state_metadata_directory_witness(&expected.file, &expected.path)? != expected.witness {
        return Err(io::Error::other(format!(
            "retained state metadata directory changed: {}",
            expected.path.display()
        ))
        .into());
    }
    require_no_default_acl(&expected.file, &expected.path)?;
    let name = expected
        .path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "state metadata directory has no name"))?;
    let named = open_state_metadata_at(
        parent.as_raw_fd(),
        Path::new(name),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
    )?;
    if state_metadata_directory_witness(&named, &expected.path)? != expected.witness {
        return Err(io::Error::other(format!(
            "named state metadata directory changed: {}",
            expected.path.display()
        ))
        .into());
    }
    require_no_default_acl(&named, &expected.path)?;
    Ok(())
}

fn require_named_state_metadata_root(path: &Path, expected: &StateMetadataDirectory) -> Result<(), Error> {
    if state_metadata_directory_witness(&expected.file, path)? != expected.witness {
        return Err(io::Error::other(format!("retained state metadata root changed: {}", path.display())).into());
    }
    require_no_default_acl(&expected.file, path)?;
    let named = open_absolute_state_metadata_directory(path)?;
    if state_metadata_directory_witness(&named, path)? != expected.witness {
        return Err(io::Error::other(format!("named state metadata root changed: {}", path.display())).into());
    }
    require_no_default_acl(&named, path)?;
    Ok(())
}

fn write_state_id(usr: &StateMetadataDirectory, contents: &[u8]) -> Result<(), Error> {
    let marker_path = usr.path.join(STATE_ID_NAME);
    let temporary_path = usr.path.join(STATE_ID_TEMPORARY_NAME);
    let previous = open_existing_state_id(usr, &marker_path)?;
    let (temporary, temporary_identity) = prepare_state_id_temporary(usr, &temporary_path)?;

    truncate_state_id(&temporary)?;
    fchmod(temporary.as_raw_fd(), Mode::from_bits_truncate(STATE_ID_TEMPORARY_MODE))?;
    write_state_id_bytes(&temporary, contents)?;
    temporary.sync_all()?;
    fchmod(temporary.as_raw_fd(), Mode::from_bits_truncate(STATE_ID_MODE))?;
    temporary.sync_all()?;
    require_complete_state_id(&temporary, &temporary_path, temporary_identity, contents.len() as u64)?;

    require_expected_state_id_name(usr, previous, &marker_path)?;
    rename_state_id_temporary(usr.file.as_raw_fd(), previous.is_some())?;
    usr.file.sync_all()?;

    let mut named = open_state_metadata_at(
        usr.file.as_raw_fd(),
        Path::new(STATE_ID_NAME),
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
    )?;
    require_complete_state_id(&named, &marker_path, temporary_identity, contents.len() as u64)?;
    let bound = contents.len().saturating_add(1);
    let mut actual = Vec::with_capacity(bound);
    (&mut named).take(bound as u64).read_to_end(&mut actual)?;
    if actual != contents {
        return Err(io::Error::other(format!("state ID marker content mismatch at {}", marker_path.display())).into());
    }
    require_complete_state_id(&named, &marker_path, temporary_identity, contents.len() as u64)?;
    match open_state_metadata_at(
        usr.file.as_raw_fd(),
        Path::new(STATE_ID_TEMPORARY_NAME),
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
    ) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(io::Error::other("state ID temporary still has a public name after publication").into()),
        Err(source) => Err(source.into()),
    }
}

fn open_existing_state_id(usr: &StateMetadataDirectory, marker_path: &Path) -> Result<Option<(u64, u64)>, Error> {
    let probe = match open_state_metadata_at(
        usr.file.as_raw_fd(),
        Path::new(STATE_ID_NAME),
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
    ) {
        Ok(probe) => probe,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source.into()),
    };
    let identity = require_state_id_inode(&probe, marker_path)?;
    if probe.metadata()?.mode() & 0o7777 != STATE_ID_MODE {
        // Recover only a same-owner, single-link regular file whose mode is a
        // subset of the canonical marker mode. Atomic publication itself can
        // never expose this state, but older in-place writers could.
        chmod_path_descriptor(&probe, STATE_ID_MODE)?;
        require_state_id_inode(&probe, marker_path)?;
    }
    Ok(Some(identity))
}

fn prepare_state_id_temporary(
    usr: &StateMetadataDirectory,
    temporary_path: &Path,
) -> Result<(std::fs::File, (u64, u64)), Error> {
    loop {
        match open_state_metadata_at(
            usr.file.as_raw_fd(),
            Path::new(STATE_ID_TEMPORARY_NAME),
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        ) {
            Ok(probe) => {
                require_state_id_temporary_inode(&probe, temporary_path)?;
                if probe.metadata()?.mode() & 0o7777 != STATE_ID_TEMPORARY_MODE {
                    chmod_path_descriptor(&probe, STATE_ID_TEMPORARY_MODE)?;
                }
                let identity = require_state_id_temporary_inode(&probe, temporary_path)?;
                let file = open_state_metadata_at(
                    usr.file.as_raw_fd(),
                    Path::new(STATE_ID_TEMPORARY_NAME),
                    nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
                )?;
                if require_state_id_temporary_inode(&file, temporary_path)? != identity {
                    return Err(io::Error::other("state ID temporary was replaced before opening for write").into());
                }
                return Ok((file, identity));
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                match open_state_metadata_at(
                    usr.file.as_raw_fd(),
                    Path::new(STATE_ID_TEMPORARY_NAME),
                    nix::libc::O_RDWR
                        | nix::libc::O_CLOEXEC
                        | nix::libc::O_CREAT
                        | nix::libc::O_EXCL
                        | nix::libc::O_NOFOLLOW
                        | nix::libc::O_NONBLOCK,
                ) {
                    Ok(file) => {
                        require_state_id_temporary_inode(&file, temporary_path)?;
                        fchmod(file.as_raw_fd(), Mode::from_bits_truncate(STATE_ID_TEMPORARY_MODE))?;
                        let identity = require_state_id_temporary_inode(&file, temporary_path)?;
                        return Ok((file, identity));
                    }
                    Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(source) => return Err(source.into()),
                }
            }
            Err(source) => return Err(source.into()),
        }
    }
}

fn require_expected_state_id_name(
    usr: &StateMetadataDirectory,
    expected: Option<(u64, u64)>,
    marker_path: &Path,
) -> Result<(), Error> {
    match (
        expected,
        open_state_metadata_at(
            usr.file.as_raw_fd(),
            Path::new(STATE_ID_NAME),
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        ),
    ) {
        (None, Err(source)) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        (None, Ok(_)) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "state ID marker appeared before exclusive publication",
        )
        .into()),
        (None, Err(source)) => Err(source.into()),
        (Some(expected), Ok(file)) => {
            if require_state_id_inode(&file, marker_path)? == expected {
                Ok(())
            } else {
                Err(io::Error::other("state ID marker was replaced before atomic publication").into())
            }
        }
        (Some(_), Err(source)) => Err(source.into()),
    }
}

fn rename_state_id_temporary(directory: RawFd, replace: bool) -> io::Result<()> {
    let flags = if replace { 0 } else { RENAME_NOREPLACE };
    loop {
        // SAFETY: the retained directory and both static NUL-terminated names
        // remain live. Same-directory rename atomically replaces an existing
        // validated marker or exclusively publishes the first one.
        let result = unsafe {
            syscall(
                SYS_renameat2,
                directory,
                STATE_ID_TEMPORARY_C_NAME.as_ptr(),
                directory,
                STATE_ID_C_NAME.as_ptr(),
                flags,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn require_state_id_temporary_inode(file: &std::fs::File, path: &Path) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    let recoverable_mode = mode & !STATE_ID_TEMPORARY_MODE == 0 || mode == STATE_ID_MODE;
    if metadata.file_type().is_file()
        && metadata.nlink() == 1
        && metadata.uid() == effective_user_id()
        && recoverable_mode
    {
        Ok((metadata.dev(), metadata.ino()))
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "state ID temporary is not one recoverable owner-controlled regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ))
    }
}

fn require_state_id_inode(file: &std::fs::File, path: &Path) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if metadata.file_type().is_file()
        && metadata.nlink() == 1
        && metadata.uid() == effective_user_id()
        && mode & !STATE_ID_MODE == 0
    {
        Ok((metadata.dev(), metadata.ino()))
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "state ID marker is not one safe independent owner-controlled regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ))
    }
}

fn require_complete_state_id(
    file: &std::fs::File,
    path: &Path,
    expected_identity: (u64, u64),
    expected_length: u64,
) -> io::Result<()> {
    let metadata = file.metadata()?;
    if (metadata.dev(), metadata.ino()) == expected_identity
        && metadata.file_type().is_file()
        && metadata.nlink() == 1
        && metadata.uid() == effective_user_id()
        && metadata.mode() & 0o7777 == STATE_ID_MODE
        && metadata.len() == expected_length
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("state ID marker metadata changed at {}", path.display()),
        ))
    }
}

fn truncate_state_id(file: &std::fs::File) -> io::Result<()> {
    loop {
        // SAFETY: file is a retained writable regular-file descriptor.
        if unsafe { nix::libc::ftruncate(file.as_raw_fd(), 0) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn write_state_id_bytes(file: &std::fs::File, contents: &[u8]) -> io::Result<()> {
    let mut written = 0;
    while written != contents.len() {
        match write(file.as_raw_fd(), &contents[written..]) {
            Ok(0) => return Err(io::Error::from_raw_os_error(nix::libc::EIO)),
            Ok(count) => written += count,
            Err(Errno::EINTR) => {}
            Err(source) => return Err(source.into()),
        }
    }
    Ok(())
}

fn effective_user_id() -> u32 {
    // SAFETY: geteuid has no arguments and cannot fail.
    unsafe { nix::libc::geteuid() }
}

fn generate_system_snapshot(
    current: Option<LoadedSystemModel>,
    repositories: &repository::Manager,
    packages: &[Package],
) -> Result<SystemModel, Error> {
    let active_repos = repositories
        .active()
        .map(|repo| (repo.id, repo.repository))
        .collect::<repository::Map>();

    match current {
        // Update existing w/ incoming packages
        Some(existing) => SystemModel::try_from(existing)
            .map_err(system_model::UpdateError::from)?
            .sync_packages(packages)
            .map_err(Error::UpdateSystemModel),

        // Generate a fresh normalized state snapshot.
        None => {
            let packages = packages
                .iter()
                .map(|package| Provider::package_name(package.meta.name.as_str()))
                .collect();

            Ok(system_model::create(active_repos, packages))
        }
    }
}

#[cfg(test)]
fn record_system_snapshot(root: &Path, system_snapshot: SystemModel) -> Result<(), Error> {
    let path = system_model::snapshot_path(root);
    let dir = path.parent().expect("system snapshot path has a parent");
    fs::create_dir_all(dir)?;
    fs::write(path, system_snapshot.encoded())?;

    Ok(())
}

#[derive(Debug)]
enum Scope {
    Stateful,
    Ephemeral {
        destination: ExternalMaterializationAdmission,
    },
    Frozen {
        destination: FrozenRootDestination,
    },
}

#[derive(Debug)]
struct FrozenRootDestination {
    root_path: PathBuf,
    parent_path: PathBuf,
    name: CString,
    parent: fs::File,
    parent_identity: FrozenRootIdentity,
}

/// Exclusive cooperating-writer guard for one frozen publication namespace.
///
/// Linux rename and unlink syscalls cannot make their source operation
/// conditional on a previously observed inode. Forge clients therefore hold
/// this advisory directory lock across every preflight, namespace mutation,
/// reconciliation, and durability barrier. The separately opened descriptor
/// is intentionally owned by the guard: closing it releases the lock even
/// while the client's retained parent capability remains alive.
#[derive(Debug)]
struct FrozenDestinationLock {
    _directory: fs::File,
}

impl Scope {
    fn is_ephemeral(&self) -> bool {
        matches!(self, Self::Ephemeral { .. } | Self::Frozen { .. })
    }
}

#[cfg(test)]
std::thread_local! {
    static OBSERVED_TRIGGER_SCOPES: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_EPHEMERAL_TRANSACTION_TRIGGERS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_EPHEMERAL_SYSTEM_TRIGGERS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_EPHEMERAL_TRANSACTION_TRIGGERS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_EPHEMERAL_SYSTEM_TRIGGERS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_before_ephemeral_transaction_triggers(hook: impl FnOnce() + 'static) {
    BEFORE_EPHEMERAL_TRANSACTION_TRIGGERS.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn arm_before_ephemeral_system_triggers(hook: impl FnOnce() + 'static) {
    BEFORE_EPHEMERAL_SYSTEM_TRIGGERS.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_ephemeral_transaction_triggers() {
    BEFORE_EPHEMERAL_TRANSACTION_TRIGGERS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_ephemeral_transaction_triggers() {}

#[cfg(test)]
fn before_ephemeral_system_triggers() {
    BEFORE_EPHEMERAL_SYSTEM_TRIGGERS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_ephemeral_system_triggers() {}

#[cfg(test)]
fn arm_after_ephemeral_transaction_triggers(hook: impl FnOnce() + 'static) {
    AFTER_EPHEMERAL_TRANSACTION_TRIGGERS.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn arm_after_ephemeral_system_triggers(hook: impl FnOnce() + 'static) {
    AFTER_EPHEMERAL_SYSTEM_TRIGGERS.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_ephemeral_transaction_triggers() {
    AFTER_EPHEMERAL_TRANSACTION_TRIGGERS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_ephemeral_transaction_triggers() {}

#[cfg(test)]
fn after_ephemeral_system_triggers() {
    AFTER_EPHEMERAL_SYSTEM_TRIGGERS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_ephemeral_system_triggers() {}

#[cfg(test)]
fn observe_trigger_scope(scope: &TriggerScope<'_>) {
    let name = match scope {
        TriggerScope::Transaction(..) => "transaction",
        TriggerScope::RetainedTransaction {
            kind: postblit::RetainedTransactionKind::Stateful,
            ..
        } => "transaction",
        TriggerScope::RetainedTransaction {
            kind: postblit::RetainedTransactionKind::ArchivedRepair,
            ..
        } => "retained-transaction",
        TriggerScope::RetainedEphemeral {
            phase: postblit::RetainedEphemeralPhase::Transaction,
            ..
        } => "transaction",
        TriggerScope::RetainedEphemeral {
            phase: postblit::RetainedEphemeralPhase::System,
            ..
        } => "system",
        TriggerScope::System(..) => "system",
    };
    OBSERVED_TRIGGER_SCOPES.with(|observed| observed.borrow_mut().push(name));
}

#[cfg(test)]
fn take_observed_trigger_scopes() -> Vec<&'static str> {
    OBSERVED_TRIGGER_SCOPES.with(|observed| std::mem::take(&mut *observed.borrow_mut()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssetMaterialization {
    HardLink,
    IndependentCopy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlitExecution {
    Parallel,
    Sequential,
}

/// A pending file for blitting
#[derive(Debug, Clone)]
pub struct PendingFile {
    /// The origin package for this file/inode
    pub id: package::Id,

    /// Corresponding layout entry, describing the inode
    pub layout: StonePayloadLayoutRecord,
}

impl BlitFile for PendingFile {
    /// Match internal kind to minimalist vfs kind
    fn kind(&self) -> vfs::tree::Kind {
        match &self.layout.file {
            StonePayloadLayoutFile::Symlink(source, _) => vfs::tree::Kind::Symlink(source.clone()),
            StonePayloadLayoutFile::Directory(_) => vfs::tree::Kind::Directory,
            _ => vfs::tree::Kind::Regular,
        }
    }

    /// Return ID for conflict
    fn id(&self) -> AStr {
        self.id.clone().into()
    }

    /// Resolve the target path, including the missing `/usr` prefix
    fn path(&self) -> AStr {
        let result = match &self.layout.file {
            StonePayloadLayoutFile::Regular(_, target) => target.clone(),
            StonePayloadLayoutFile::Symlink(_, target) => target.clone(),
            StonePayloadLayoutFile::Directory(target) => target.clone(),
            StonePayloadLayoutFile::CharacterDevice(target) => target.clone(),
            StonePayloadLayoutFile::BlockDevice(target) => target.clone(),
            StonePayloadLayoutFile::Fifo(target) => target.clone(),
            StonePayloadLayoutFile::Socket(target) => target.clone(),
            StonePayloadLayoutFile::Unknown(.., target) => target.clone(),
        };

        vfs::path::join("/usr", &result)
    }

    /// Clone the node to a reparented path, for symlink resolution
    fn cloned_to(&self, path: AStr) -> Self {
        let mut new = self.clone();
        new.layout.file = match &self.layout.file {
            StonePayloadLayoutFile::Regular(source, _) => StonePayloadLayoutFile::Regular(*source, path),
            StonePayloadLayoutFile::Symlink(source, _) => StonePayloadLayoutFile::Symlink(source.clone(), path),
            StonePayloadLayoutFile::Directory(_) => StonePayloadLayoutFile::Directory(path),
            StonePayloadLayoutFile::CharacterDevice(_) => StonePayloadLayoutFile::CharacterDevice(path),
            StonePayloadLayoutFile::BlockDevice(_) => StonePayloadLayoutFile::BlockDevice(path),
            StonePayloadLayoutFile::Fifo(_) => StonePayloadLayoutFile::Fifo(path),
            StonePayloadLayoutFile::Socket(_) => StonePayloadLayoutFile::Socket(path),
            StonePayloadLayoutFile::Unknown(source, _) => StonePayloadLayoutFile::Unknown(source.clone(), path),
        };
        new
    }
}

impl From<AStr> for PendingFile {
    fn from(value: AStr) -> Self {
        PendingFile {
            id: Default::default(),
            layout: StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: 0o755,
                tag: 0,
                file: StonePayloadLayoutFile::Directory(value),
            },
        }
    }
}

impl fmt::Display for PendingFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.path().fmt(f)
    }
}

/// Build a [`crate::registry::Registry`] during client initialisation
///
/// # Arguments
///
/// * `installation` - Describe our installation target tree
/// * `repositories` - Configured repositories to laoad [`crate::registry::Plugin::Repository`]
/// * `installdb`    - Installation database opened in the installation tree
/// * `statedb`      - State database opened in the installation tree
fn build_registry(
    active_state: Option<state::Id>,
    repositories: &repository::Manager,
    installdb: &db::meta::Database,
    statedb: &db::state::Database,
) -> Result<Registry, Error> {
    let state = match active_state {
        Some(id) => Some(statedb.get(id)?),
        None => None,
    };

    let mut registry = Registry::default();

    registry.add_plugin(Plugin::Cobble(plugin::Cobble::default()));
    registry.add_plugin(Plugin::Active(plugin::Active::new(state, installdb.clone())));

    for repo in repositories.active() {
        registry.add_plugin(Plugin::Repository(plugin::Repository::new(repo)));
    }

    Ok(registry)
}

fn build_repository_registry(repositories: &repository::Manager) -> Registry {
    let mut registry = Registry::default();
    for repository in repositories.active() {
        registry.add_plugin(Plugin::Repository(plugin::Repository::new(repository)));
    }
    registry
}

#[derive(Debug, Clone, Copy, Default)]
struct BlitStats {
    num_files: u64,
    num_symlinks: u64,
    num_dirs: u64,
}

impl BlitStats {
    fn merge(self, other: Self) -> Self {
        Self {
            num_files: self.num_files + other.num_files,
            num_symlinks: self.num_symlinks + other.num_symlinks,
            num_dirs: self.num_dirs + other.num_dirs,
        }
    }

    fn num_entries(&self) -> u64 {
        self.num_files + self.num_symlinks + self.num_dirs
    }
}

/// Client-relevant error mapping type
#[derive(Debug, Error)]
pub enum Error {
    #[error("root must have an active state")]
    NoActiveState,
    #[error("{operation} at {path:?} while proving the live active-state selection")]
    LiveActiveStateProof {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("active-state snapshot changed since installation discovery: expected {expected:?}, found {actual:?}")]
    ActiveStateSnapshotChanged {
        expected: Option<state::Id>,
        actual: Option<state::Id>,
    },
    #[error("state {0} already active")]
    StateAlreadyActive(state::Id),
    #[error("state {0} doesn't exist")]
    StateDoesntExist(state::Id),
    #[error("open merged-/usr root ABI directory {root:?}")]
    OpenRootAbiDirectory {
        root: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stat merged-/usr root ABI directory {root:?}")]
    StatRootAbiDirectory {
        root: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("merged-/usr root ABI directory was replaced while linking: {0:?}")]
    RootAbiDirectoryReplaced(PathBuf),
    #[error("inspect merged-/usr root ABI entry {path:?}")]
    InspectRootAbiEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read merged-/usr root ABI symlink {path:?}")]
    ReadRootAbiLink {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("merged-/usr root ABI symlink {path:?} exceeds {limit} target bytes")]
    RootAbiLinkTargetTooLong { path: PathBuf, limit: usize },
    #[error(
        "legacy merged-/usr staging entry {path:?} must be absent, found {actual_type} with target {symlink_target:?}"
    )]
    RootAbiStagingConflict {
        path: PathBuf,
        actual_type: &'static str,
        symlink_target: Option<OsString>,
    },
    #[error("merged-/usr root ABI link {path:?} must target {target:?}, found {actual_type}")]
    RootAbiLinkTypeConflict {
        path: PathBuf,
        target: String,
        actual_type: &'static str,
    },
    #[error("merged-/usr root ABI link {path:?} must target {expected:?}, found {actual:?}")]
    RootAbiLinkTargetConflict {
        path: PathBuf,
        expected: String,
        actual: OsString,
    },
    #[error("merged-/usr root ABI link {path:?} targeting {target:?} is missing after publication")]
    RootAbiLinkMissing { path: PathBuf, target: String },
    #[error("merged-/usr root ABI link appeared after absence was retained: {0:?}")]
    RootAbiLinkAppeared(PathBuf),
    #[error("merged-/usr root ABI link was replaced across its durability boundary: {0:?}")]
    RootAbiLinkReplaced(PathBuf),
    #[error("create absent merged-/usr root ABI link {path:?} targeting {target:?}")]
    CreateRootAbiLink {
        path: PathBuf,
        target: String,
        #[source]
        source: io::Error,
    },
    #[error("sync merged-/usr root ABI directory {root:?}")]
    SyncRootAbiDirectory {
        root: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "state transition for candidate {candidate} failed before /usr was exchanged; the candidate was preserved outside the active root and arbitrary trigger side effects may remain: {primary}"
    )]
    StatefulCandidatePreserved {
        candidate: state::Id,
        previous: Option<state::Id>,
        #[source]
        primary: Box<Error>,
    },
    #[error(
        "state transition for candidate {candidate} failed; /usr was restored to {previous:?}, the failed candidate was preserved outside the active root, and arbitrary trigger side effects may remain: {primary}"
    )]
    StatefulTransitionUsrRestored {
        candidate: state::Id,
        previous: Option<state::Id>,
        #[source]
        primary: Box<Error>,
    },
    #[error(
        "boot synchronization for failed candidate {candidate} had already started; synchronization for restored state {previous:?} returned without a verifiable proof that candidate boot metadata was removed"
    )]
    StatefulBootRepairUnverified {
        candidate: state::Id,
        previous: Option<state::Id>,
    },
    #[error(
        "failed-candidate preservation failed and its one bounded in-process retry also failed (first={first}; retry={retry})"
    )]
    StatefulCandidatePreservationRetryFailed { first: Box<Error>, retry: Box<Error> },
    #[error(
        "state transition for candidate {candidate} failed with primary error {primary}; recovery for previous state {previous:?} was incomplete (previous_archive_cleanup={previous_archive_cleanup:?}, restore_previous={restore_previous:?}, reverse_exchange={reverse_exchange:?}, preserve_candidate={preserve_candidate:?}, invalidate_candidate={invalidate_candidate:?}, repair_boot={repair_boot:?})"
    )]
    StatefulTransitionRecoveryFailed {
        candidate: state::Id,
        previous: Option<state::Id>,
        #[source]
        primary: Box<Error>,
        previous_archive_cleanup: Option<Box<Error>>,
        restore_previous: Option<Box<Error>>,
        reverse_exchange: Option<Box<Error>>,
        preserve_candidate: Option<Box<Error>>,
        invalidate_candidate: Option<Box<Error>>,
        repair_boot: Option<Box<Error>>,
    },
    #[error("No metadata found for package {0:?}")]
    MissingMetadata(package::Id),
    #[error("package {package} has invalid /usr-relative Stone layout target {target:?}: {reason}")]
    InvalidStoneLayoutTarget {
        package: package::Id,
        target: String,
        reason: &'static str,
    },
    #[error("ephemeral/public materialization destination overlaps the installation root")]
    EphemeralInstallationRoot,
    #[error("ephemeral postblit requested {requested:?}, but this client is bound to {configured:?}")]
    EphemeralDestinationMismatch { configured: PathBuf, requested: PathBuf },
    #[error(
        "initial materialization target {path:?} must be an exact empty owner-controlled ACL-free directory (uid={owner}, mode={mode:04o})"
    )]
    UnsafeInitialMaterializationTarget { path: PathBuf, owner: u32, mode: u32 },
    #[error("retained initial materialization target changed at {path:?}")]
    InitialMaterializationTargetChanged { path: PathBuf },
    #[error(
        "initial materialization parent {path:?} must be an owner-controlled ACL-free directory (uid={owner}, mode={mode:04o})"
    )]
    UnsafeInitialMaterializationParent { path: PathBuf, owner: u32, mode: u32 },
    #[error("Operation not allowed with ephemeral client")]
    EphemeralProhibitedOperation,
    #[error("frozen-root materialization requires a dedicated frozen client")]
    FrozenRootRequiresFrozenClient,
    #[error("frozen clients require an installation opened with Installation::open_frozen")]
    FrozenInstallationRequired,
    #[error("system and ephemeral clients require Installation::open on a writable system root")]
    SystemInstallationRequired,
    #[error("operation is not available on a dedicated frozen client")]
    FrozenClientProhibitedOperation,
    #[error("duplicate package ID in frozen closure: {0}")]
    DuplicateFrozenPackage(package::Id),
    #[error("frozen package closure must not be empty")]
    EmptyFrozenPackageClosure,
    #[error("layout query returned package outside the frozen closure: {0}")]
    UnexpectedFrozenLayoutPackage(package::Id),
    #[error("package {package} has a frozen-root layout path exceeding {limit} bytes (got {actual})")]
    FrozenLayoutPathTooLong {
        package: package::Id,
        limit: usize,
        actual: usize,
    },
    #[error("package {package} has a frozen-root layout path exceeding {limit} components (got {actual})")]
    FrozenLayoutPathTooDeep {
        package: package::Id,
        limit: usize,
        actual: usize,
    },
    #[error("package {package} has a frozen-root symlink target exceeding {limit} bytes (got {actual})")]
    FrozenLayoutSymlinkTargetTooLong {
        package: package::Id,
        limit: usize,
        actual: usize,
    },
    #[error("package {package} has an invalid frozen-root layout path: {path:?}")]
    InvalidFrozenLayoutPath { package: package::Id, path: String },
    #[error("package {package} has an invalid or unenforceable frozen-root mode {mode:#o} at {path:?}")]
    InvalidFrozenLayoutMode {
        package: package::Id,
        path: String,
        mode: u32,
    },
    #[error("package {package} has an unsupported frozen-root inode at {path:?}")]
    UnsupportedFrozenLayout { package: package::Id, path: String },
    #[error("package {package} requests unsupported frozen-root ownership {uid}:{gid} at {path:?}")]
    UnsupportedFrozenOwnership {
        package: package::Id,
        path: String,
        uid: u32,
        gid: u32,
    },
    #[error("frozen-root path collision at {path:?}: packages {first} and {second}")]
    FrozenPathCollision {
        path: String,
        first: package::Id,
        second: package::Id,
    },
    #[error(
        "package {package} declares frozen-root path {path:?} beneath directory symlink {redirect:?}; explicit descendants under directory symlinks are forbidden"
    )]
    FrozenDirectorySymlinkDescendant {
        package: package::Id,
        path: Box<str>,
        redirect: Box<str>,
    },
    #[error("frozen executable closure package count exceeds {limit} (got {actual})")]
    FrozenExecutablePackageLimit { limit: usize, actual: usize },
    #[error("frozen executable closure package IDs exceed {limit} aggregate bytes (got {actual})")]
    FrozenExecutableClosureIdByteLimit { limit: usize, actual: usize },
    #[error("frozen executable binding count exceeds {limit} (got {actual})")]
    FrozenExecutableBindingLimit { limit: usize, actual: usize },
    #[error("frozen executable path exceeds {limit} bytes (got {actual})")]
    FrozenExecutablePathByteLimit { limit: usize, actual: usize },
    #[error("frozen executable path exceeds {limit} components (got {actual})")]
    FrozenExecutablePathDepthLimit { limit: usize, actual: usize },
    #[error("frozen executable path is not UTF-8 ({bytes} bytes)")]
    FrozenExecutablePathEncoding { bytes: usize },
    #[error(
        "frozen executable bindings exceed {limit} aggregate path bytes at provider {package} path {path:?} (got {actual})"
    )]
    FrozenExecutableBindingByteLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error("frozen closure layout count exceeds {limit} (got {actual})")]
    FrozenExecutableLayoutLimit { limit: usize, actual: usize },
    #[error("frozen closure stored layout strings exceed {limit} aggregate bytes (got {actual})")]
    FrozenLayoutStorageByteLimit { limit: usize, actual: usize },
    #[error(
        "frozen executable closure layouts exceed {limit} aggregate bytes at provider {package} path {path:?} (got {actual})"
    )]
    FrozenExecutableLayoutByteLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error("frozen directory discovery exceeds {limit} paths (got {actual})")]
    FrozenExecutableDirectoryLimit { limit: usize, actual: usize },
    #[error("frozen directory discovery exceeds {limit} aggregate path bytes (got {actual})")]
    FrozenExecutableDirectoryByteLimit { limit: usize, actual: usize },
    #[error(
        "frozen executable graph from provider {package} at {path:?} exceeds the retained-file limit {limit} (got {actual})"
    )]
    FrozenExecutablePinnedFileLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error("frozen executable provider {package} at {path:?} is outside the materialized closure")]
    FrozenExecutableProviderOutsideClosure { package: package::Id, path: PathBuf },
    #[error("frozen executable provider {package} has invalid path {path:?}")]
    InvalidFrozenExecutablePath { package: package::Id, path: PathBuf },
    #[error("frozen executable provider {package} has duplicate layout entries at {path:?}")]
    DuplicateFrozenExecutableLayout { package: package::Id, path: PathBuf },
    #[error("frozen executable provider {package} has no regular layout entry at {path:?}")]
    MissingFrozenExecutableLayout { package: package::Id, path: PathBuf },
    #[error(
        "frozen executable provider {package} binding {binding:?} resolves to {target:?}, which has no provider in the exact frozen closure"
    )]
    MissingFrozenExecutableSymlinkTarget {
        package: package::Id,
        binding: PathBuf,
        target: PathBuf,
    },
    #[error(
        "frozen executable provider {package} binding {binding:?} resolves to ambiguous target {target:?} from providers {providers:?}"
    )]
    AmbiguousFrozenExecutableSymlinkTarget {
        package: package::Id,
        binding: PathBuf,
        target: PathBuf,
        providers: Vec<package::Id>,
    },
    #[error("frozen executable provider {package} names a non-regular layout entry at {path:?}")]
    FrozenExecutableLayoutNotRegular { package: package::Id, path: PathBuf },
    #[error("frozen executable provider {package} has non-executable layout mode {mode:#o} at {path:?}")]
    FrozenExecutableLayoutNotExecutable {
        package: package::Id,
        path: PathBuf,
        mode: u32,
    },
    #[error("frozen executable provider {package} has invalid symlink target {target:?} at {path:?}")]
    InvalidFrozenExecutableSymlinkTarget {
        package: package::Id,
        path: PathBuf,
        target: String,
    },
    #[error("frozen executable provider {package} has a symlink cycle at {path:?}")]
    FrozenExecutableSymlinkCycle { package: package::Id, path: PathBuf },
    #[error("frozen executable provider {package} binding {path:?} exceeds the symlink-chain limit {limit}")]
    FrozenExecutableSymlinkLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
    },
    #[error(
        "frozen executable path {path:?} traverses materialized directory-symlink redirect {redirect_source:?} -> {target:?}; executable redirects are forbidden"
    )]
    FrozenExecutableDirectoryRedirect {
        path: PathBuf,
        redirect_source: Box<PathBuf>,
        target: Box<PathBuf>,
    },
    #[error("frozen executable from provider {package} at {path:?} has an invalid format: {reason}")]
    InvalidFrozenExecutableFormat {
        package: package::Id,
        path: PathBuf,
        reason: &'static str,
    },
    #[error("frozen ELF from provider {package} at {path:?} exceeds {limit} program headers (got {actual})")]
    FrozenElfProgramHeaderLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error(
        "frozen ELF PT_INTERP target from provider {package} at {path:?} is itself interpreted; Linux requires a terminal ELF loader"
    )]
    FrozenElfInterpreterIsInterpreted { package: package::Id, path: PathBuf },
    #[error("frozen executable script from provider {package} at {path:?} has an invalid shebang: {reason}")]
    InvalidFrozenShebang {
        package: package::Id,
        path: PathBuf,
        reason: &'static str,
    },
    #[error("frozen script interpreter at {path:?} is not supplied by the frozen package closure")]
    MissingFrozenInterpreterProvider { path: PathBuf },
    #[error("frozen script interpreter at {path:?} has multiple providers: {providers:?}")]
    AmbiguousFrozenInterpreterProvider { path: PathBuf, providers: Vec<package::Id> },
    #[error("frozen script interpreter layout has a symlink cycle at {path:?}")]
    FrozenInterpreterSymlinkCycle { path: PathBuf },
    #[error("frozen executable interpreter chain cycles through provider {package} at {path:?}")]
    FrozenExecutableInterpreterCycle { package: package::Id, path: PathBuf },
    #[error("frozen script from provider {package} at {path:?} exceeds the interpreter-chain limit {limit}")]
    FrozenShebangInterpreterLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
    },
    #[error("frozen executable from provider {package} at {path:?} exceeds the interpreter-graph limit {limit}")]
    FrozenExecutableInterpreterLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
    },
    #[error("invalid frozen interpreter root ABI alias path {path:?}")]
    InvalidFrozenInterpreterRootAlias { path: PathBuf },
    #[error("open frozen interpreter root ABI alias {path:?}")]
    OpenFrozenInterpreterRootAlias {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stat frozen interpreter root ABI alias {path:?}")]
    StatFrozenInterpreterRootAlias {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read frozen interpreter root ABI alias {path:?}")]
    ReadFrozenInterpreterRootAlias {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen interpreter root ABI alias {path:?} has mode {mode:#o} and {links} links")]
    FrozenInterpreterRootAliasMetadata { path: PathBuf, mode: u32, links: u64 },
    #[error("frozen interpreter root ABI alias {path:?} points to {actual:?}; expected {expected:?}")]
    FrozenInterpreterRootAliasTarget {
        path: PathBuf,
        expected: String,
        actual: OsString,
    },
    #[error(
        "frozen interpreter root ABI alias {path:?} exceeds the target limit {limit} bytes (got at least {actual})"
    )]
    FrozenInterpreterRootAliasTargetTooLong { path: PathBuf, limit: usize, actual: usize },
    #[error("frozen interpreter root ABI alias changed during verification at {path:?}")]
    FrozenInterpreterRootAliasChanged { path: PathBuf },
    #[error("frozen executable root path is invalid: {0:?}")]
    InvalidFrozenExecutableRoot(PathBuf),
    #[error("open frozen executable root {path:?}")]
    OpenFrozenExecutableRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stat frozen executable root {path:?}")]
    StatFrozenExecutableRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen executable root path was replaced during verification: {0:?}")]
    FrozenExecutableRootReplaced(PathBuf),
    #[error("materialized frozen root destination was replaced after publication: {0:?}")]
    MaterializedFrozenRootReplaced(PathBuf),
    #[error("materialized frozen root belongs to {found:?}, expected this client destination {expected:?}")]
    ForeignMaterializedFrozenRoot { expected: PathBuf, found: PathBuf },
    #[error("open frozen executable from provider {package} at {path:?}")]
    OpenFrozenExecutable {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stat frozen executable from provider {package} at {path:?}")]
    StatFrozenExecutable {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open frozen executable symlink from provider {package} at {path:?}")]
    OpenFrozenExecutableSymlink {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stat frozen executable symlink from provider {package} at {path:?}")]
    StatFrozenExecutableSymlink {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read frozen executable symlink from provider {package} at {path:?}")]
    ReadFrozenExecutableSymlink {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "frozen executable symlink from provider {package} at {path:?} has mode {actual:#o} and {links} links; expected mode {expected:#o} and one link"
    )]
    FrozenExecutableSymlinkMetadataMismatch {
        package: package::Id,
        path: PathBuf,
        expected: u32,
        actual: u32,
        links: u64,
    },
    #[error(
        "frozen executable symlink from provider {package} at {path:?} points to {actual:?}; expected {expected:?}"
    )]
    FrozenExecutableSymlinkTargetMismatch {
        package: package::Id,
        path: PathBuf,
        expected: String,
        actual: OsString,
    },
    #[error("frozen executable symlink from provider {package} changed during verification at {path:?}")]
    FrozenExecutableSymlinkChanged { package: package::Id, path: PathBuf },
    #[error(
        "frozen executable symlink from provider {package} at {path:?} exceeds the target limit {limit} bytes (got at least {actual})"
    )]
    FrozenExecutableSymlinkTargetTooLong {
        package: package::Id,
        path: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error("read frozen executable from provider {package} at {path:?}")]
    ReadFrozenExecutable {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "frozen executable from provider {package} at {path:?} is not an independent regular file (mode {mode:#o}, links {links})"
    )]
    FrozenExecutableNotIndependentRegular {
        package: package::Id,
        path: PathBuf,
        mode: u32,
        links: u64,
    },
    #[error("frozen executable from provider {package} at {path:?} has mode {actual:#o}; expected {expected:#o}")]
    FrozenExecutableModeMismatch {
        package: package::Id,
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    #[error("frozen executable from provider {package} at {path:?} exceeds {limit} bytes (got {actual})")]
    FrozenExecutableByteLimit {
        package: package::Id,
        path: PathBuf,
        limit: u64,
        actual: u64,
    },
    #[error("total frozen executable bytes exceed {limit} (got {actual})")]
    FrozenExecutableTotalByteLimit { limit: u64, actual: u64 },
    #[error(
        "frozen executable from provider {package} at {path:?} changed length while hashing: expected {expected}, got {actual}"
    )]
    FrozenExecutableLengthChanged {
        package: package::Id,
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("frozen executable from provider {package} changed while hashing at {path:?}")]
    FrozenExecutableChanged { package: package::Id, path: PathBuf },
    #[error("frozen executable from provider {package} at {path:?} has digest {actual:032x}; expected {expected:032x}")]
    FrozenExecutableDigestMismatch {
        package: package::Id,
        path: PathBuf,
        expected: u128,
        actual: u128,
    },
    #[error("frozen executable path from provider {package} was replaced during verification: {path:?}")]
    FrozenExecutablePathReplaced { package: package::Id, path: PathBuf },
    #[error("frozen executable verification exceeded {seconds} seconds")]
    FrozenExecutableVerificationTimeout { seconds: u64 },
    #[error("frozen-root materialization exceeded {seconds} seconds")]
    FrozenMaterializationTimeout { seconds: u64 },
    #[error("frozen-root independent-copy bytes exceed {limit} (got {actual})")]
    FrozenMaterializationTotalByteLimit { limit: u64, actual: u64 },
    #[error(
        "frozen-root cached asset {digest:032x} changed length between byte preflight and copy: expected {expected}, got {actual}"
    )]
    FrozenMaterializationAssetLengthChanged { digest: u128, expected: u64, actual: u64 },
    #[error("frozen-root cached asset {digest:032x} was not admitted by the byte preflight")]
    FrozenMaterializationAssetMissingFromManifest { digest: u128 },
    #[error("frozen-root destination is invalid: {0:?}")]
    InvalidFrozenRootDestination(PathBuf),
    #[error("frozen-root destination already exists: {0:?}")]
    FrozenRootDestinationExists(PathBuf),
    #[error("package {package} has an invalid frozen-root symlink target: {reason}")]
    InvalidFrozenLayoutSymlinkTarget { package: package::Id, reason: &'static str },
    #[error("frozen-root normalization exceeds {limit} inodes (got {actual})")]
    FrozenNormalizationInodeLimit { limit: usize, actual: usize },
    #[error("frozen-root normalization exceeds {limit} path components (got {actual})")]
    FrozenNormalizationDepthLimit { limit: usize, actual: usize },
    #[error("invalid declarative frozen-root entry {path:?}: {reason}")]
    InvalidFrozenNormalizationDeclaration { path: PathBuf, reason: &'static str },
    #[error("frozen-root filesystem does not match its declaration at {path:?}: {reason}")]
    FrozenNormalizationInventoryMismatch { path: PathBuf, reason: &'static str },
    #[error("frozen-root entry changed while being normalized: {0:?}")]
    FrozenNormalizationEntryChanged(PathBuf),
    #[error("frozen-root staging name changed while its original descriptor was retained: {0:?}")]
    FrozenNormalizationRootChanged(PathBuf),
    #[error("open frozen-root normalization entry {path:?}")]
    OpenFrozenNormalizationEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("inspect frozen-root normalization entry {path:?}")]
    InspectFrozenNormalizationEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read frozen-root normalization directory {path:?}")]
    ReadFrozenNormalizationDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("reserve bounded frozen-root normalization inventory for {path:?}")]
    ReserveFrozenNormalizationInventory {
        path: PathBuf,
        #[source]
        source: std::collections::TryReserveError,
    },
    #[error("frozen-root entry carries a non-canonical ACL at {path:?}")]
    FrozenNormalizationAcl {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("normalize frozen-root mode through retained entry {path:?}")]
    NormalizeFrozenEntryMode {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("normalize frozen-root timestamp through retained entry {path:?}")]
    NormalizeFrozenEntryTime {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read retained frozen-root symlink {path:?}")]
    ReadFrozenNormalizationSymlink {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen-root symlink {path:?} must target {expected:?}, found {actual:?}")]
    FrozenNormalizationSymlinkTargetMismatch {
        path: PathBuf,
        expected: OsString,
        actual: OsString,
    },
    #[error("publish frozen root {stage:?} to {destination:?}")]
    PublishFrozenRoot {
        stage: PathBuf,
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open frozen-root destination parent {path:?}")]
    OpenFrozenRootDestinationParent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen-root destination parent changed while retained: {0:?}")]
    FrozenRootDestinationParentChanged(PathBuf),
    #[error("lock frozen-root destination parent {path:?}")]
    LockFrozenRootDestinationParent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("create private frozen-root directory {path:?}")]
    CreateFrozenPrivateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open private frozen-root directory {path:?}")]
    OpenFrozenPrivateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("normalize private frozen-root directory {path:?}")]
    NormalizeFrozenPrivateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("private frozen-root directory changed while retained: {path:?}")]
    FrozenPrivateDirectoryChanged { path: PathBuf },
    #[error(
        "private frozen-root setup failed at {path:?}: {primary}; bounded provisional cleanup also failed: {cleanup}"
    )]
    CleanupFrozenPrivateDirectory {
        path: PathBuf,
        primary: Box<Error>,
        cleanup: Box<Error>,
    },
    #[error("retained frozen-root stage changed before bounded cleanup: {stage:?}")]
    FrozenRetainedStageChanged { stage: PathBuf },
    #[error("inspect frozen-root publication name {path:?}")]
    InspectFrozenPublicationName {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen root inode changed while publishing {stage:?} to {destination:?}")]
    FrozenRootChangedDuringPublication { stage: PathBuf, destination: PathBuf },
    #[error("frozen-root publication namespace changed from {stage:?} to {destination:?}: {reason}")]
    FrozenPublicationNamespaceMismatch {
        stage: PathBuf,
        destination: PathBuf,
        reason: &'static str,
    },
    #[error("{operation} at {path:?}")]
    SyncFrozenPublication {
        path: PathBuf,
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error(
        "frozen-root materialization failed at stage {stage:?}: {primary}; bounded stage cleanup also failed: {cleanup}"
    )]
    CleanupFrozenStage {
        stage: PathBuf,
        primary: Box<Error>,
        cleanup: Box<Error>,
    },
    #[error("detach frozen root {root:?} into private quarantine {quarantine:?}")]
    DetachFrozenRoot {
        root: PathBuf,
        quarantine: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("unsafe frozen root at discard boundary {root:?}: uid={owner}, mode={mode:#o}")]
    UnsafeFrozenRootDiscard { root: PathBuf, owner: u32, mode: u32 },
    #[error("frozen-root discard namespace changed from {root:?} to {quarantine:?}")]
    FrozenDiscardNamespaceMismatch { root: PathBuf, quarantine: PathBuf },
    #[error(
        "frozen-root discard failed for {root:?}: {primary}; restoring its exact public mode also failed: {restore}"
    )]
    RestoreFrozenDiscardRootMode {
        root: PathBuf,
        primary: Box<Error>,
        restore: Box<Error>,
    },
    #[error(
        "frozen-root detach failed for {quarantine:?}: {primary}; exact empty-quarantine cleanup also failed: {cleanup}"
    )]
    CleanupFrozenDiscardQuarantine {
        quarantine: PathBuf,
        primary: Box<Error>,
        cleanup: Box<Error>,
    },
    #[error("frozen root {root:?} changed while detaching it into {quarantine:?}")]
    FrozenRootChangedDuringDiscard { root: PathBuf, quarantine: PathBuf },
    #[error("frozen-root discard exceeds {limit} entries (got {actual})")]
    FrozenDiscardEntryLimit { limit: usize, actual: usize },
    #[error("frozen-root discard exceeds {limit} path components (got {actual})")]
    FrozenDiscardDepthLimit { limit: usize, actual: usize },
    #[error("frozen-root discard directory changed while it was pinned")]
    FrozenDiscardEntryChanged,
    #[error("open frozen-root discard entry {path:?}")]
    OpenFrozenDiscardEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("remove frozen-root discard entry {path:?}")]
    RemoveFrozenDiscardEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open frozen-root discard directory")]
    OpenFrozenDiscardDirectory {
        #[source]
        source: io::Error,
    },
    #[error("read frozen-root discard directory")]
    ReadFrozenDiscardDirectory {
        #[source]
        source: io::Error,
    },
    #[error("installation")]
    Installation(#[from] installation::Error),
    #[error("fetch package {1}")]
    CacheFetch(#[source] cache::FetchError, package::Name),
    #[error("unpack package {1}, file {2}")]
    CacheUnpack(#[source] cache::UnpackError, package::Name, PathBuf),
    #[error("repository manager")]
    Repository(#[from] repository::manager::Error),
    #[error("package registry query")]
    Registry(#[from] crate::registry::Error),
    #[error("db")]
    Db(#[from] db::Error),
    #[error("prune")]
    Prune(#[from] prune::Error),
    #[error("io")]
    Io(#[from] io::Error),
    #[error("filesystem")]
    Filesystem(#[from] vfs::tree::Error),
    #[error("blit")]
    Blit(#[from] Errno),
    #[error("postblit")]
    PostBlit(#[from] postblit::Error),
    #[error("boot")]
    Boot(#[from] boot::Error),
    #[error("establish clean system-client startup baseline")]
    SystemStartupGate {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("prepare or authenticate durable state-transition tree identities")]
    StatefulTreeIdentity {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("repair an inactive archived state")]
    ArchivedStateRepair {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("decorate a stateful candidate through retained metadata capabilities")]
    StatefulCandidateMetadata {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("prepare or decorate an ephemeral candidate through retained metadata capabilities")]
    EphemeralCandidateMetadata {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("prepare or revalidate retained ephemeral trigger authority")]
    EphemeralTriggerAuthority {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("stateful candidate {candidate} reached activation without its retained metadata proof")]
    StatefulCandidateMetadataProofRequired { candidate: state::Id },
    #[error("materialize an inactive archived state")]
    ArchivedRepairMaterialization {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("materialize a stateful candidate through retained fixed staging")]
    StatefulCandidateMaterialization {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("{operation} cannot target fixed staging without its retained capability")]
    FixedStagingCapabilityRequired { operation: &'static str },
    #[error("state {state} database record changed between the verify scan and its retained repair")]
    VerifyStateChanged { state: state::Id },
    #[error("fixed-staging cooperating-writer coordinator is poisoned")]
    FixedStagingCoordinatorPoisoned,
    #[error(
        "active-state reblit for state {state} committed, but whole-wrapper cleanup ended with {outcome}; do not reverse through fixed staging"
    )]
    ActiveReblitCommittedCleanupIncomplete {
        state: state::Id,
        outcome: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error(
        "durable tree-identity preparation for candidate {candidate} failed before activation; candidate tree remains at {location:?}, previous state is {previous:?}, and the candidate database row was not invalidated"
    )]
    StatefulTreeIdentityPreparationFailed {
        candidate: state::Id,
        previous: Option<state::Id>,
        location: PathBuf,
        #[source]
        source: Box<Error>,
    },
    /// Had issues processing user-provided string input
    #[error("string processing")]
    Dialog(#[from] tui::dialoguer::Error),
    /// The operation was explicitly cancelled at the user's request
    #[error("cancelled")]
    Cancelled,
    #[error("protect state mutation from interruption")]
    BlitSignalIgnore(#[from] signal::Error),
    #[error("load Gluon system intent or generated state snapshot")]
    LoadSystemModel(#[from] system_model::LoadError),
    #[error("update system model")]
    UpdateSystemModel(#[from] system_model::UpdateError),
    #[error("install")]
    Install(#[source] Box<install::Error>),
    #[error("remove")]
    Remove(#[source] Box<remove::Error>),
    #[error("fetch")]
    Fetch(#[source] Box<fetch::Error>),
    #[error("sync")]
    Sync(#[source] Box<sync::Error>),
    #[error("Gluon system intent doesn't exist at {0:?}")]
    ImportSystemIntentDoesntExist(PathBuf),
}

impl From<crate::transition_identity::Error> for Error {
    fn from(source: crate::transition_identity::Error) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}

impl From<RetainedExchangeFailure> for Error {
    fn from(source: RetainedExchangeFailure) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}

impl From<RetainedPreviousMoveFailure> for Error {
    fn from(source: RetainedPreviousMoveFailure) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}

impl From<ArchivedCandidateError> for Error {
    fn from(source: ArchivedCandidateError) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}

impl From<RetainedArchivedCandidateMoveFailure> for Error {
    fn from(source: RetainedArchivedCandidateMoveFailure) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}

impl From<RetainedStagingWrapperRotationFailure> for Error {
    fn from(source: RetainedStagingWrapperRotationFailure) -> Self {
        Self::StatefulTreeIdentity {
            source: Box::new(source),
        }
    }
}

#[cfg(test)]
mod tests;
