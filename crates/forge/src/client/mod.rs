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
        if self.installation.is_frozen_cache() {
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
    #[error("system and ephemeral clients require an installation opened with Installation::open")]
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
mod tests {
    use std::{
        collections::BTreeSet,
        fs::Permissions,
        os::unix::{
            ffi::OsStringExt as _,
            fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink},
            net::UnixListener,
        },
        process::{Command, Stdio},
    };

    use gluon_config::Source;

    use super::*;
    use crate::test_support::prepare_private_installation_root;

    mod ephemeral_candidate_metadata;
    mod external_materialization;
    mod fixed_staging_transition;
    mod root_abi_preflight;
    mod state_prune;
    mod stateful_candidate_metadata;

    fn test_installation(root: &Path) -> Installation {
        prepare_private_installation_root(root);
        Installation::open(root, None).unwrap()
    }

    fn frozen_test_installation(root: &Path) -> Installation {
        prepare_private_installation_root(root);
        Installation::open_frozen(root, None).unwrap()
    }

    fn test_elf(interpreter: Option<&str>, program_count: usize) -> Vec<u8> {
        assert!(program_count >= 1);
        assert!(interpreter.is_none() || program_count >= 2);
        let class = if usize::BITS == 64 { 2 } else { 1 };
        let little_endian = cfg!(target_endian = "little");
        let header_size = if class == 2 { 64 } else { 52 };
        let program_header_size = if class == 2 { 56 } else { 32 };
        let interpreter_bytes = interpreter.map_or(0, |path| path.len() + 1);
        let interpreter_offset = header_size + program_header_size * program_count;
        let length = interpreter_offset + interpreter_bytes;
        let mut elf = vec![0u8; length];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[4] = class;
        elf[5] = if little_endian { 1 } else { 2 };
        elf[6] = 1;
        test_elf_write_u16(&mut elf, 16, 2, little_endian);
        test_elf_write_u16(
            &mut elf,
            18,
            native_frozen_elf_machine().expect("tests require a supported Linux ELF architecture"),
            little_endian,
        );
        test_elf_write_u32(&mut elf, 20, 1, little_endian);
        if class == 2 {
            test_elf_write_u64(&mut elf, 24, 0x0040_0000, little_endian);
            test_elf_write_u64(&mut elf, 32, header_size as u64, little_endian);
            test_elf_write_u16(&mut elf, 52, header_size as u16, little_endian);
            test_elf_write_u16(&mut elf, 54, program_header_size as u16, little_endian);
            test_elf_write_u16(&mut elf, 56, program_count as u16, little_endian);

            let load = header_size;
            test_elf_write_u32(&mut elf, load, 1, little_endian);
            test_elf_write_u32(&mut elf, load + 4, 5, little_endian);
            test_elf_write_u64(&mut elf, load + 8, 0, little_endian);
            test_elf_write_u64(&mut elf, load + 16, 0x0040_0000, little_endian);
            test_elf_write_u64(&mut elf, load + 32, length as u64, little_endian);
            test_elf_write_u64(&mut elf, load + 40, length as u64, little_endian);
            test_elf_write_u64(&mut elf, load + 48, 1, little_endian);
            if let Some(interpreter) = interpreter {
                let header = load + program_header_size;
                test_elf_write_u32(&mut elf, header, 3, little_endian);
                test_elf_write_u32(&mut elf, header + 4, 4, little_endian);
                test_elf_write_u64(&mut elf, header + 8, interpreter_offset as u64, little_endian);
                test_elf_write_u64(&mut elf, header + 32, interpreter_bytes as u64, little_endian);
                test_elf_write_u64(&mut elf, header + 40, interpreter_bytes as u64, little_endian);
                test_elf_write_u64(&mut elf, header + 48, 1, little_endian);
                elf[interpreter_offset..interpreter_offset + interpreter.len()].copy_from_slice(interpreter.as_bytes());
            }
        } else {
            test_elf_write_u32(&mut elf, 24, 0x0040_0000, little_endian);
            test_elf_write_u32(&mut elf, 28, header_size as u32, little_endian);
            test_elf_write_u16(&mut elf, 40, header_size as u16, little_endian);
            test_elf_write_u16(&mut elf, 42, program_header_size as u16, little_endian);
            test_elf_write_u16(&mut elf, 44, program_count as u16, little_endian);

            let load = header_size;
            test_elf_write_u32(&mut elf, load, 1, little_endian);
            test_elf_write_u32(&mut elf, load + 4, 0, little_endian);
            test_elf_write_u32(&mut elf, load + 8, 0x0040_0000, little_endian);
            test_elf_write_u32(&mut elf, load + 16, length as u32, little_endian);
            test_elf_write_u32(&mut elf, load + 20, length as u32, little_endian);
            test_elf_write_u32(&mut elf, load + 24, 5, little_endian);
            test_elf_write_u32(&mut elf, load + 28, 1, little_endian);
            if let Some(interpreter) = interpreter {
                let header = load + program_header_size;
                test_elf_write_u32(&mut elf, header, 3, little_endian);
                test_elf_write_u32(&mut elf, header + 4, interpreter_offset as u32, little_endian);
                test_elf_write_u32(&mut elf, header + 16, interpreter_bytes as u32, little_endian);
                test_elf_write_u32(&mut elf, header + 20, interpreter_bytes as u32, little_endian);
                test_elf_write_u32(&mut elf, header + 24, 4, little_endian);
                test_elf_write_u32(&mut elf, header + 28, 1, little_endian);
                elf[interpreter_offset..interpreter_offset + interpreter.len()].copy_from_slice(interpreter.as_bytes());
            }
        }
        elf
    }

    fn test_elf_write_u16(output: &mut [u8], offset: usize, value: u16, little_endian: bool) {
        let bytes = if little_endian {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        output[offset..offset + bytes.len()].copy_from_slice(&bytes);
    }

    fn test_elf_write_u32(output: &mut [u8], offset: usize, value: u32, little_endian: bool) {
        let bytes = if little_endian {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        output[offset..offset + bytes.len()].copy_from_slice(&bytes);
    }

    fn test_elf_write_u64(output: &mut [u8], offset: usize, value: u64, little_endian: bool) {
        let bytes = if little_endian {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        output[offset..offset + bytes.len()].copy_from_slice(&bytes);
    }

    #[allow(clippy::result_large_err)]
    fn inspect_test_executable(
        bytes: &[u8],
        binding: &FrozenExecutableBinding,
    ) -> Result<Option<FrozenExecutableInterpreter>, Error> {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("executable");
        fs::write(&path, bytes).unwrap();
        let file = fs::File::open(path).unwrap();
        let probe_length = bytes.len().min(MAX_FROZEN_SHEBANG_LINE_BYTES + 1);
        inspect_frozen_executable_format(
            &file,
            bytes.len() as u64,
            &bytes[..probe_length],
            Instant::now() + Duration::from_secs(10),
            binding,
        )
    }

    fn stateful_test_client(root: &Path) -> Client {
        let installation = test_installation(root);
        Client::builder("state-snapshot-test", installation)
            .repositories(repository::Map::default())
            .build()
            .unwrap()
    }

    fn root_abi_inode(path: &Path) -> (u64, u64) {
        let metadata = fs::symlink_metadata(path).unwrap();
        (metadata.dev(), metadata.ino())
    }

    fn assert_root_abi_absent(path: &Path) {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("failed to inspect expected-absent root ABI path {path:?}: {error}"),
            Ok(metadata) => panic!(
                "expected root ABI path to be absent, found mode {:#o} at {path:?}",
                metadata.mode()
            ),
        }
    }

    fn assert_root_abi_links(root: &Path) {
        for (source, target) in ROOT_ABI_LINKS {
            assert_eq!(
                fs::read_link(root.join(target)).unwrap().as_os_str().as_bytes(),
                source.as_bytes()
            );
            assert_root_abi_absent(&root.join(format!("{target}.next")));
        }
    }

    #[test]
    fn root_abi_entry_open_distinguishes_absence_and_pins_symlink_itself() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let directory = open_root_abi_directory(root).unwrap();
        assert!(open_root_abi_entry(&directory, root, "bin").unwrap().is_none());

        symlink("usr/bin", root.join("bin")).unwrap();
        let entry = open_root_abi_entry(&directory, root, "bin").unwrap().unwrap();
        assert!(entry.metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            read_root_abi_symlink(&entry, &root.join("bin")).unwrap().as_bytes(),
            b"usr/bin"
        );
    }

    #[test]
    fn root_abi_links_create_only_absent_names_and_canonical_noop_is_inode_stable_and_synced() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();

        create_root_links(root).unwrap();
        assert_root_abi_links(root);
        let identities = ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target)));

        let mut syncs = 0;
        create_root_links_with(
            root,
            |_| {},
            |directory| {
                syncs += 1;
                directory.sync_all()
            },
        )
        .unwrap();
        assert_eq!(syncs, 1, "an idempotent no-op must still fsync the root directory");
        assert_root_abi_links(root);
        assert_eq!(
            identities,
            ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target))),
            "canonical dangling links must be accepted without replacement"
        );
    }

    #[test]
    fn root_abi_links_reject_wrong_dangling_and_non_utf8_targets_for_every_final_name() {
        let targets = [
            OsString::from("usr/wrong-live"),
            OsString::from("usr/wrong-dangling"),
            OsString::from_vec(b"usr/wrong-\xff".to_vec()),
        ];
        for (source, target) in ROOT_ABI_LINKS {
            for actual in &targets {
                let temporary = tempfile::tempdir().unwrap();
                let root = temporary.path();
                fs::create_dir_all(root.join("usr")).unwrap();
                fs::write(root.join("usr/wrong-live"), b"live").unwrap();
                symlink(actual, root.join(target)).unwrap();
                let identity = root_abi_inode(&root.join(target));

                let error = create_root_links(root).unwrap_err();
                assert!(matches!(
                    error,
                    Error::RootAbiLinkTargetConflict {
                        path,
                        expected,
                        actual: found,
                    } if path == root.join(target)
                        && expected == source
                        && found.as_bytes() == actual.as_bytes()
                ));
                assert_eq!(root_abi_inode(&root.join(target)), identity);
                assert_eq!(
                    fs::read_link(root.join(target)).unwrap().as_os_str().as_bytes(),
                    actual.as_bytes()
                );
                for (_, other) in ROOT_ABI_LINKS {
                    if other != target {
                        assert_root_abi_absent(&root.join(other));
                    }
                }
            }
        }
    }

    fn assert_root_abi_type_conflict(actual_type: &'static str, setup: impl FnOnce(&Path)) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let path = root.join("bin");
        setup(&path);
        let identity = root_abi_inode(&path);

        let error = create_root_links(root).unwrap_err();
        assert!(matches!(
            error,
            Error::RootAbiLinkTypeConflict {
                path: found,
                target,
                actual_type: found_type,
            } if found == path && target == "usr/bin" && found_type == actual_type
        ));
        assert_eq!(root_abi_inode(&path), identity);
        assert_root_abi_absent(&root.join("sbin"));
    }

    #[test]
    fn root_abi_links_reject_regular_directory_fifo_and_socket_without_mutation() {
        assert_root_abi_type_conflict("regular file", |path| fs::write(path, b"foreign").unwrap());
        assert_root_abi_type_conflict("directory", |path| {
            fs::create_dir(path).unwrap();
            fs::write(path.join("marker"), b"foreign").unwrap();
        });
        assert_root_abi_type_conflict("fifo", |path| {
            nix::unistd::mkfifo(path, Mode::from_bits_truncate(0o600)).unwrap();
        });

        // Some test sandboxes prohibit AF_UNIX creation. Regular files,
        // directories, and FIFOs above always exercise the non-symlink path;
        // exercise its socket classification whenever the host permits the
        // fixture rather than treating a capability denial as success.
        let socket_root = tempfile::tempdir().unwrap();
        let socket = socket_root.path().join("bin");
        match UnixListener::bind(&socket) {
            Ok(listener) => {
                drop(listener);
                let identity = root_abi_inode(&socket);
                let error = create_root_links(socket_root.path()).unwrap_err();
                assert!(matches!(
                    error,
                    Error::RootAbiLinkTypeConflict {
                        path,
                        target,
                        actual_type: "socket",
                    } if path == socket && target == "usr/bin"
                ));
                assert_eq!(root_abi_inode(&socket), identity);
                assert_root_abi_absent(&socket_root.path().join("sbin"));
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
            Err(error) => panic!("create root ABI socket conflict fixture: {error}"),
        }
    }

    #[test]
    fn root_abi_links_reject_every_legacy_next_name_without_cleanup_or_partial_creation() {
        for (_, target) in ROOT_ABI_LINKS {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path();
            let next = root.join(format!("{target}.next"));
            fs::write(&next, b"foreign stage").unwrap();
            let identity = root_abi_inode(&next);

            let error = create_root_links(root).unwrap_err();
            assert!(matches!(
                error,
                Error::RootAbiStagingConflict {
                    path,
                    actual_type: "regular file",
                    symlink_target: None,
                } if path == next
            ));
            assert_eq!(root_abi_inode(&next), identity);
            assert_eq!(fs::read(&next).unwrap(), b"foreign stage");
            for (_, final_name) in ROOT_ABI_LINKS {
                assert_root_abi_absent(&root.join(final_name));
            }
        }

        for actual in [OsString::from("usr/bin"), OsString::from_vec(b"usr/\xff".to_vec())] {
            let temporary = tempfile::tempdir().unwrap();
            let next = temporary.path().join("bin.next");
            symlink(&actual, &next).unwrap();
            let identity = root_abi_inode(&next);
            let error = create_root_links(temporary.path()).unwrap_err();
            assert!(matches!(
                error,
                Error::RootAbiStagingConflict {
                    path,
                    actual_type: "symlink",
                    symlink_target: Some(found),
                } if path == next && found.as_bytes() == actual.as_bytes()
            ));
            assert_eq!(root_abi_inode(&next), identity);
        }
    }

    #[test]
    fn root_abi_links_authenticate_absent_name_races_without_overwriting() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let raced = root.join("sbin");
        create_root_links_with(
            root,
            |checkpoint| {
                if checkpoint == RootAbiLinkCheckpoint::PreflightComplete {
                    fs::write(&raced, b"raced foreign entry").unwrap();
                }
            },
            |directory| directory.sync_all(),
        )
        .unwrap_err();
        assert_eq!(fs::read(&raced).unwrap(), b"raced foreign entry");
        assert_root_abi_absent(&root.join("bin"));

        let exact = tempfile::tempdir().unwrap();
        create_root_links_with(
            exact.path(),
            |checkpoint| {
                if checkpoint == RootAbiLinkCheckpoint::PreflightComplete {
                    symlink("usr/sbin", exact.path().join("sbin")).unwrap();
                }
            },
            |directory| directory.sync_all(),
        )
        .unwrap();
        assert_root_abi_links(exact.path());
    }

    #[test]
    fn root_abi_links_leave_raced_next_and_exact_partial_links_retry_safe() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let raced = root.join("sbin.next");
        let error = create_root_links_with(
            root,
            |checkpoint| {
                if checkpoint == RootAbiLinkCheckpoint::PreflightComplete {
                    fs::write(&raced, b"raced stage").unwrap();
                }
            },
            |directory| directory.sync_all(),
        )
        .unwrap_err();
        assert!(matches!(error, Error::RootAbiStagingConflict { path, .. } if path == raced));
        assert_eq!(fs::read(&raced).unwrap(), b"raced stage");
        assert_root_abi_links_except_next(root, "sbin.next");
        let identities = ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target)));

        fs::remove_file(&raced).unwrap();
        create_root_links(root).unwrap();
        assert_root_abi_links(root);
        assert_eq!(
            identities,
            ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target)))
        );
    }

    fn assert_root_abi_links_except_next(root: &Path, allowed_next: &str) {
        for (source, target) in ROOT_ABI_LINKS {
            assert_eq!(
                fs::read_link(root.join(target)).unwrap().as_os_str().as_bytes(),
                source.as_bytes()
            );
            let next = format!("{target}.next");
            if next != allowed_next {
                assert_root_abi_absent(&root.join(next));
            }
        }
    }

    #[test]
    fn root_abi_links_sync_failure_is_retryable_without_replacing_exact_links() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let error = create_root_links_with(
            root,
            |_| {},
            |_| Err(io::Error::other("injected root directory sync failure")),
        )
        .unwrap_err();
        assert!(matches!(error, Error::SyncRootAbiDirectory { .. }));
        assert_root_abi_links(root);
        let identities = ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target)));

        create_root_links(root).unwrap();
        assert_eq!(
            identities,
            ROOT_ABI_LINKS.map(|(_, target)| root_abi_inode(&root.join(target)))
        );
    }

    #[test]
    fn root_abi_links_revalidate_post_sync_name_races_without_repairing_them() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let bin = root.join("bin");
        let error = create_root_links_with(
            root,
            |checkpoint| {
                if checkpoint == RootAbiLinkCheckpoint::AfterSync {
                    fs::remove_file(&bin).unwrap();
                    symlink("usr/wrong-after-sync", &bin).unwrap();
                }
            },
            |directory| directory.sync_all(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::RootAbiLinkTargetConflict {
                path,
                expected,
                actual,
            } if path == bin && expected == "usr/bin" && actual.as_bytes() == b"usr/wrong-after-sync"
        ));
        assert_eq!(fs::read_link(&bin).unwrap(), Path::new("usr/wrong-after-sync"));
    }

    #[test]
    fn root_abi_links_reject_exact_target_aba_across_sync_and_preserve_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let bin = root.join("bin");
        let mut replacement = None;
        let error = create_root_links_with(
            root,
            |checkpoint| {
                if checkpoint == RootAbiLinkCheckpoint::AfterSync {
                    fs::remove_file(&bin).unwrap();
                    symlink("usr/bin", &bin).unwrap();
                    replacement = Some(root_abi_inode(&bin));
                }
            },
            |directory| directory.sync_all(),
        )
        .unwrap_err();
        assert!(matches!(error, Error::RootAbiLinkReplaced(path) if path == bin));
        assert_eq!(root_abi_inode(&bin), replacement.unwrap());
        assert_eq!(fs::read_link(&bin).unwrap(), Path::new("usr/bin"));
    }

    #[test]
    fn root_abi_links_detect_public_root_replacement_and_never_touch_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let detached = temporary.path().join("detached");
        fs::create_dir(&root).unwrap();

        let error = create_root_links_with(
            &root,
            |checkpoint| {
                if checkpoint == RootAbiLinkCheckpoint::RootOpened {
                    fs::rename(&root, &detached).unwrap();
                    fs::create_dir(&root).unwrap();
                    fs::write(root.join("replacement-marker"), b"replacement").unwrap();
                }
            },
            |directory| directory.sync_all(),
        )
        .unwrap_err();
        assert!(matches!(error, Error::RootAbiDirectoryReplaced(path) if path == root));
        assert_eq!(fs::read(root.join("replacement-marker")).unwrap(), b"replacement");
        assert_root_abi_absent(&root.join("bin"));
        assert_root_abi_links(&detached);
    }

    #[test]
    fn root_abi_links_reject_terminal_and_intermediate_root_symlinks_before_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let real = temporary.path().join("real");
        fs::create_dir(&real).unwrap();
        let alias = temporary.path().join("alias");
        symlink(&real, &alias).unwrap();
        assert!(matches!(
            create_root_links(&alias),
            Err(Error::OpenRootAbiDirectory { root, .. }) if root == alias
        ));
        assert_root_abi_absent(&real.join("bin"));

        let child = real.join("child");
        fs::create_dir(&child).unwrap();
        let through_alias = alias.join("child");
        assert!(matches!(
            create_root_links(&through_alias),
            Err(Error::OpenRootAbiDirectory { root, .. }) if root == through_alias
        ));
        assert_root_abi_absent(&child.join("bin"));
    }

    #[derive(Debug, Clone, Copy)]
    enum TestStoneLayoutKind {
        Regular,
        Symlink,
        Directory,
        CharacterDevice,
        BlockDevice,
        Fifo,
        Socket,
        Unknown,
    }

    const ALL_STONE_LAYOUT_KINDS: [TestStoneLayoutKind; 8] = [
        TestStoneLayoutKind::Regular,
        TestStoneLayoutKind::Symlink,
        TestStoneLayoutKind::Directory,
        TestStoneLayoutKind::CharacterDevice,
        TestStoneLayoutKind::BlockDevice,
        TestStoneLayoutKind::Fifo,
        TestStoneLayoutKind::Socket,
        TestStoneLayoutKind::Unknown,
    ];

    fn test_stone_layout(kind: TestStoneLayoutKind, target: impl Into<AStr>) -> StonePayloadLayoutRecord {
        let target = target.into();
        let file = match kind {
            TestStoneLayoutKind::Regular => StonePayloadLayoutFile::Regular(42, target),
            TestStoneLayoutKind::Symlink => StonePayloadLayoutFile::Symlink("tool".into(), target),
            TestStoneLayoutKind::Directory => StonePayloadLayoutFile::Directory(target),
            TestStoneLayoutKind::CharacterDevice => StonePayloadLayoutFile::CharacterDevice(target),
            TestStoneLayoutKind::BlockDevice => StonePayloadLayoutFile::BlockDevice(target),
            TestStoneLayoutKind::Fifo => StonePayloadLayoutFile::Fifo(target),
            TestStoneLayoutKind::Socket => StonePayloadLayoutFile::Socket(target),
            TestStoneLayoutKind::Unknown => StonePayloadLayoutFile::Unknown("opaque".into(), target),
        };
        StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: 0,
            tag: 0,
            file,
        }
    }

    #[test]
    fn stone_layout_ingestion_confines_every_inode_variant_to_canonical_usr_relative_targets() {
        let package = package::Id::from("layout-path-policy");
        for (index, kind) in ALL_STONE_LAYOUT_KINDS.into_iter().enumerate() {
            let target = format!("share/layout-kind-{index}");
            let valid = test_stone_layout(kind, target.clone());
            require_usr_relative_stone_layout(&package, &valid).unwrap();
            assert_eq!(
                PendingFile {
                    id: package.clone(),
                    layout: valid
                }
                .path()
                .to_string(),
                format!("/usr/{target}")
            );

            let absolute = format!("/usr/{target}");
            let invalid = test_stone_layout(kind, absolute.clone());
            assert!(matches!(
                require_usr_relative_stone_layout(&package, &invalid),
                Err(Error::InvalidStoneLayoutTarget {
                    package: rejected_package,
                    target: rejected_target,
                    reason: "the target is absolute",
                }) if rejected_package == package && rejected_target == absolute
            ));
            assert!(matches!(
                vfs(vec![(package.clone(), invalid)]),
                Err(Error::InvalidStoneLayoutTarget {
                    package: rejected_package,
                    target: rejected_target,
                    reason: "the target is absolute",
                }) if rejected_package == package && rejected_target == absolute
            ));

            for reserved in [
                ".cast-state-id.tmp",
                ".cast-tree-id",
                ".cast-tree-id.tmp",
                ".stateID/forged-child",
                "lib/os-release",
                "lib/os-release/forged-child",
                "lib/system-model.glu",
                "lib/system-model.glu/forged-child",
            ] {
                let invalid = test_stone_layout(kind, reserved);
                assert!(matches!(
                    require_usr_relative_stone_layout(&package, &invalid),
                    Err(Error::InvalidStoneLayoutTarget {
                        package: rejected_package,
                        target,
                        reason: "the target is reserved for Cast system metadata",
                    }) if rejected_package == package && target == reserved
                ));
                assert!(matches!(
                    vfs(vec![(package.clone(), invalid)]),
                    Err(Error::InvalidStoneLayoutTarget {
                        package: rejected_package,
                        target,
                        reason: "the target is reserved for Cast system metadata",
                    }) if rejected_package == package && target == reserved
                ));
            }
        }
    }

    #[test]
    fn stone_layout_ingestion_rejects_every_noncanonical_or_escaping_target() {
        let package = package::Id::from("invalid-layout-path");
        let cases = [
            ("", "the target is empty"),
            ("/", "the target is absolute"),
            ("/usr", "the target is absolute"),
            ("/usr/bin/tool", "the target is absolute"),
            ("/etc/passwd", "the target is absolute"),
            (".", "the target contains a dot component"),
            ("..", "the target contains a dot component"),
            ("./bin/tool", "the target contains a dot component"),
            ("bin/./tool", "the target contains a dot component"),
            ("bin/../tool", "the target contains a dot component"),
            ("bin//tool", "the target contains a repeated separator"),
            ("bin/tool/", "the target has a trailing separator"),
            ("bin/\0tool", "the target contains an ASCII control byte"),
            ("bin/\ntool", "the target contains an ASCII control byte"),
            ("bin/\u{7f}tool", "the target contains an ASCII control byte"),
        ];

        for (target, expected_reason) in cases {
            let layout = test_stone_layout(TestStoneLayoutKind::Regular, target);
            assert!(
                matches!(
                    require_usr_relative_stone_layout(&package, &layout),
                    Err(Error::InvalidStoneLayoutTarget {
                        package: rejected_package,
                        target: rejected_target,
                        reason,
                    }) if rejected_package == package && rejected_target == target && reason == expected_reason
                ),
                "accepted invalid Stone layout target {target:?}"
            );
        }

        let oversized_absolute = format!("/{}", "工".repeat(MAX_STONE_LAYOUT_TARGET_DIAGNOSTIC_BYTES));
        let layout = test_stone_layout(TestStoneLayoutKind::Regular, oversized_absolute);
        let Error::InvalidStoneLayoutTarget {
            target,
            reason: "the target is absolute",
            ..
        } = require_usr_relative_stone_layout(&package, &layout).unwrap_err()
        else {
            panic!("oversized absolute target returned the wrong error");
        };
        assert!(target.ends_with('…'));
        assert!(target.len() <= MAX_STONE_LAYOUT_TARGET_DIAGNOSTIC_BYTES + '…'.len_utf8());
    }

    #[test]
    fn stone_layout_ingestion_accepts_utf8_and_exact_linux_path_boundaries() {
        // Layout targets are AStr values, so non-UTF-8 bytes cannot enter this
        // validator. Non-ASCII UTF-8 remains part of the admitted domain.
        for target in [
            "bin/tool",
            ".hidden",
            ".cast-state-id.tmp-old",
            ".cast-tree-id-old",
            ".cast-tree-id.tmp-old",
            ".stateID.old/child",
            "lib/os-info.json",
            "lib/os-release.local",
            "lib/system-model.glu.old",
            "share/Grüße/工具",
            "usr/bin/nested",
        ] {
            require_usr_relative_stone_target(target).unwrap();
        }

        let exact_component = "a".repeat(MAX_STONE_LAYOUT_COMPONENT_BYTES);
        require_usr_relative_stone_target(&exact_component).unwrap();
        assert_eq!(
            require_usr_relative_stone_target(&format!("{exact_component}a")),
            Err("a target component exceeds Linux NAME_MAX")
        );

        let exact_depth = std::iter::repeat_n("a", MAX_FROZEN_LAYOUT_PATH_COMPONENTS - 1)
            .collect::<Vec<_>>()
            .join("/");
        require_usr_relative_stone_target(&exact_depth).unwrap();
        let excessive_depth = format!("{exact_depth}/a");
        assert_eq!(
            require_usr_relative_stone_target(&excessive_depth),
            Err("the materialized path is too deep")
        );

        let exact_path = std::iter::repeat_n("a".repeat(MAX_STONE_LAYOUT_COMPONENT_BYTES), 15)
            .chain(std::iter::once("a".repeat(250)))
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!("/usr/".len() + exact_path.len(), MAX_FROZEN_EXECUTABLE_PATH_BYTES);
        require_usr_relative_stone_target(&exact_path).unwrap();
        assert_eq!(
            require_usr_relative_stone_target(&format!("{exact_path}a")),
            Err("the materialized path exceeds Linux PATH_MAX")
        );
    }

    #[test]
    fn invalid_stone_layout_batch_cannot_replace_or_insert_database_rows() {
        let layout_db = db::layout::Database::new(":memory:").unwrap();
        let retained_package = package::Id::from("retained-layout");
        let rejected_package = package::Id::from("rejected-layout");
        let retained = test_stone_layout(TestStoneLayoutKind::Regular, "bin/original");
        layout_db.add(&retained_package, &retained).unwrap();

        let replacement = test_stone_layout(TestStoneLayoutKind::Regular, "bin/replacement");
        let invalid = test_stone_layout(TestStoneLayoutKind::Directory, "/etc");
        assert!(matches!(
            ingest_stone_layouts(
                &layout_db,
                [(&retained_package, &replacement), (&rejected_package, &invalid)].into_iter(),
            ),
            Err(Error::InvalidStoneLayoutTarget {
                package,
                target,
                reason: "the target is absolute",
            }) if package == rejected_package && target == "/etc"
        ));

        assert_eq!(
            layout_db.query([&retained_package]).unwrap(),
            vec![(retained_package, retained)]
        );
        assert!(layout_db.query([&rejected_package]).unwrap().is_empty());
    }

    #[test]
    fn reserved_stone_layout_batch_cannot_replace_or_insert_database_rows() {
        let layout_db = db::layout::Database::new(":memory:").unwrap();
        let retained_package = package::Id::from("retained-layout");
        let rejected_package = package::Id::from("reserved-layout");
        let retained = test_stone_layout(TestStoneLayoutKind::Regular, "bin/original");
        layout_db.add(&retained_package, &retained).unwrap();

        let replacement = test_stone_layout(TestStoneLayoutKind::Regular, "bin/replacement");
        let reserved = test_stone_layout(TestStoneLayoutKind::Directory, ".stateID/forged-child");
        assert!(matches!(
            ingest_stone_layouts(
                &layout_db,
                [(&retained_package, &replacement), (&rejected_package, &reserved)].into_iter(),
            ),
            Err(Error::InvalidStoneLayoutTarget {
                package,
                target,
                reason: "the target is reserved for Cast system metadata",
            }) if package == rejected_package && target == ".stateID/forged-child"
        ));

        assert_eq!(
            layout_db.query([&retained_package]).unwrap(),
            vec![(retained_package, retained)]
        );
        assert!(layout_db.query([&rejected_package]).unwrap().is_empty());
    }

    fn state_metadata_mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn install_state_metadata_test_default_acl(path: &Path) -> io::Result<()> {
        const ACL: [u8; 28] = [
            0x02, 0x00, 0x00, 0x00, // version
            0x01, 0x00, 0x07, 0x00, 0xff, 0xff, 0xff, 0xff, // user object
            0x04, 0x00, 0x05, 0x00, 0xff, 0xff, 0xff, 0xff, // group object
            0x20, 0x00, 0x05, 0x00, 0xff, 0xff, 0xff, 0xff, // other
        ];
        let directory = std::fs::File::open(path)?;
        // SAFETY: the descriptor, static name, and complete canonical ACL
        // encoding remain live for the syscall.
        let result = unsafe {
            nix::libc::fsetxattr(
                directory.as_raw_fd(),
                c"system.posix_acl_default".as_ptr(),
                ACL.as_ptr().cast(),
                ACL.len(),
                0,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[test]
    fn state_metadata_rejects_inheritable_default_acls() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), Permissions::from_mode(0o700)).unwrap();
        match install_state_metadata_test_default_acl(temporary.path()) {
            Ok(()) => {}
            Err(source) if source.raw_os_error() == Some(nix::libc::EOPNOTSUPP) => return,
            Err(source) => panic!("install state metadata test default ACL: {source}"),
        }

        let inherited_root = temporary.path().join("inherited-root");
        assert!(record_state_id(&inherited_root, state::Id::from(40)).is_err());
        assert!(inherited_root.is_dir());
        assert!(!inherited_root.join("usr").exists());

        let isolated = tempfile::tempdir().unwrap();
        fs::set_permissions(isolated.path(), Permissions::from_mode(0o700)).unwrap();
        let root = isolated.path().join("root");
        let usr = root.join("usr");
        fs::create_dir_all(&usr).unwrap();
        fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&usr, Permissions::from_mode(0o750)).unwrap();
        install_state_metadata_test_default_acl(&usr).unwrap();
        assert!(record_state_id(&root, state::Id::from(41)).is_err());
        assert!(!usr.join(".stateID").exists());
    }

    #[test]
    fn state_metadata_creation_has_exact_modes_under_hostile_umasks() {
        const CHILD: &str = "CAST_STATE_METADATA_UMASK_TEST_CHILD";
        const ROOT: &str = "CAST_STATE_METADATA_UMASK_TEST_ROOT";
        const TEST: &str = "client::tests::state_metadata_creation_has_exact_modes_under_hostile_umasks";

        if let Some(mask) = std::env::var_os(CHILD) {
            let mask = u32::from_str_radix(mask.to_str().unwrap(), 8).unwrap();
            // umask is process-global. This child runs one exact test and
            // exits immediately after exercising the selected mask.
            // SAFETY: no other test runs in this single-test child process.
            unsafe { nix::libc::umask(mask) };
            let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
            record_state_id(&root, state::Id::from(42)).unwrap();
            assert_eq!(state_metadata_mode(&root), STATE_TREE_DIRECTORY_MODE);
            assert_eq!(state_metadata_mode(&root.join("usr")), STATE_TREE_DIRECTORY_MODE);
            assert_eq!(state_metadata_mode(&root.join("usr/.stateID")), STATE_ID_MODE);
            assert_eq!(fs::read_to_string(root.join("usr/.stateID")).unwrap(), "42");
            return;
        }

        for mask in ["0002", "0777"] {
            let temporary = tempfile::tempdir().unwrap();
            fs::set_permissions(temporary.path(), Permissions::from_mode(0o700)).unwrap();
            let root = temporary.path().join("state-root");
            let output = Command::new(std::env::current_exe().unwrap())
                .arg(TEST)
                .arg("--exact")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env(CHILD, mask)
                .env(ROOT, &root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "state metadata umask {mask} child failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn state_metadata_recovers_restrictive_directory_creation_residue() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), Permissions::from_mode(0o700)).unwrap();

        let root_residue = temporary.path().join("root-residue");
        fs::create_dir(&root_residue).unwrap();
        fs::set_permissions(&root_residue, Permissions::from_mode(0o000)).unwrap();
        record_state_id(&root_residue, state::Id::from(43)).unwrap();
        assert_eq!(state_metadata_mode(&root_residue), STATE_TREE_DIRECTORY_MODE);
        assert_eq!(
            state_metadata_mode(&root_residue.join("usr")),
            STATE_TREE_DIRECTORY_MODE
        );

        let root = temporary.path().join("root");
        let usr_residue = root.join("usr");
        fs::create_dir_all(&usr_residue).unwrap();
        fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&usr_residue, Permissions::from_mode(0o400)).unwrap();
        record_state_id(&root, state::Id::from(44)).unwrap();
        assert_eq!(state_metadata_mode(&root), 0o700);
        assert_eq!(state_metadata_mode(&usr_residue), STATE_TREE_DIRECTORY_MODE);
        assert_eq!(fs::read_to_string(usr_residue.join(STATE_ID_NAME)).unwrap(), "44");
    }

    #[test]
    fn state_metadata_recovers_private_atomic_temporary_residue() {
        for (bytes, mode) in [
            (b"".as_slice(), 0o000),
            (b"4".as_slice(), 0o400),
            (b"wrong".as_slice(), 0o600),
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("root");
            let usr = root.join("usr");
            fs::create_dir_all(&usr).unwrap();
            fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&usr, Permissions::from_mode(0o750)).unwrap();
            let residue = usr.join(STATE_ID_TEMPORARY_NAME);
            fs::write(&residue, bytes).unwrap();
            fs::set_permissions(&residue, Permissions::from_mode(mode)).unwrap();

            record_state_id(&root, state::Id::from(45)).unwrap();

            assert_eq!(fs::read_to_string(usr.join(STATE_ID_NAME)).unwrap(), "45");
            assert_eq!(state_metadata_mode(&usr.join(STATE_ID_NAME)), STATE_ID_MODE);
            assert!(!residue.exists());
        }
    }

    #[test]
    fn unsafe_state_metadata_temporary_is_rejected_unchanged() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let usr = root.join("usr");
        fs::create_dir_all(&usr).unwrap();
        fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&usr, Permissions::from_mode(0o750)).unwrap();
        let target = temporary.path().join("external");
        fs::write(&target, b"external evidence").unwrap();
        let residue = usr.join(STATE_ID_TEMPORARY_NAME);
        symlink(&target, &residue).unwrap();

        assert!(record_state_id(&root, state::Id::from(46)).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"external evidence");
        assert!(fs::symlink_metadata(&residue).unwrap().file_type().is_symlink());
        assert!(!usr.join(STATE_ID_NAME).exists());

        fs::remove_file(&residue).unwrap();
        fs::write(&residue, b"linked evidence").unwrap();
        fs::set_permissions(&residue, Permissions::from_mode(STATE_ID_TEMPORARY_MODE)).unwrap();
        let second = usr.join("state-id-temporary-second-link");
        fs::hard_link(&residue, &second).unwrap();
        assert!(record_state_id(&root, state::Id::from(47)).is_err());
        assert_eq!(fs::read(&residue).unwrap(), b"linked evidence");
        assert_eq!(fs::metadata(&residue).unwrap().nlink(), 2);
        assert!(!usr.join(STATE_ID_NAME).exists());
    }

    #[test]
    fn state_metadata_rejects_symlink_and_non_directory_components_unchanged() {
        let temporary = tempfile::tempdir().unwrap();

        let real_root = temporary.path().join("real-root");
        fs::create_dir(&real_root).unwrap();
        fs::set_permissions(&real_root, Permissions::from_mode(0o700)).unwrap();
        let root_alias = temporary.path().join("root-alias");
        symlink(&real_root, &root_alias).unwrap();
        assert!(record_state_id(&root_alias, state::Id::from(1)).is_err());
        assert!(!real_root.join("usr").exists());

        let file_root = temporary.path().join("file-root");
        fs::write(&file_root, b"root evidence").unwrap();
        assert!(record_state_id(&file_root, state::Id::from(2)).is_err());
        assert_eq!(fs::read(&file_root).unwrap(), b"root evidence");

        let root = temporary.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
        let redirected_usr = temporary.path().join("redirected-usr");
        fs::create_dir(&redirected_usr).unwrap();
        let usr_alias = root.join("usr");
        symlink(&redirected_usr, &usr_alias).unwrap();
        assert!(record_state_id(&root, state::Id::from(3)).is_err());
        assert!(!redirected_usr.join(".stateID").exists());
        assert!(fs::symlink_metadata(&usr_alias).unwrap().file_type().is_symlink());

        fs::remove_file(&usr_alias).unwrap();
        fs::write(&usr_alias, b"usr evidence").unwrap();
        assert!(record_state_id(&root, state::Id::from(4)).is_err());
        assert_eq!(fs::read(&usr_alias).unwrap(), b"usr evidence");
    }

    #[test]
    fn state_metadata_rejects_non_regular_or_linked_markers_unchanged() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let usr = root.join("usr");
        fs::create_dir_all(&usr).unwrap();
        fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&usr, Permissions::from_mode(0o750)).unwrap();

        let external = temporary.path().join("external");
        fs::write(&external, b"external evidence").unwrap();
        let marker = usr.join(".stateID");
        symlink(&external, &marker).unwrap();
        assert!(record_state_id(&root, state::Id::from(5)).is_err());
        assert_eq!(fs::read(&external).unwrap(), b"external evidence");
        assert!(fs::symlink_metadata(&marker).unwrap().file_type().is_symlink());

        fs::remove_file(&marker).unwrap();
        fs::create_dir(&marker).unwrap();
        assert!(record_state_id(&root, state::Id::from(6)).is_err());
        assert!(marker.is_dir());

        fs::remove_dir(&marker).unwrap();
        fs::write(&marker, b"linked evidence").unwrap();
        fs::set_permissions(&marker, Permissions::from_mode(STATE_ID_MODE)).unwrap();
        let second_link = usr.join("state-id-second-link");
        fs::hard_link(&marker, &second_link).unwrap();
        assert!(record_state_id(&root, state::Id::from(7)).is_err());
        assert_eq!(fs::read(&marker).unwrap(), b"linked evidence");
        assert_eq!(fs::read(&second_link).unwrap(), b"linked evidence");

        fs::remove_file(&marker).unwrap();
        fs::remove_file(&second_link).unwrap();
        fs::write(&marker, b"").unwrap();
        fs::set_permissions(&marker, Permissions::from_mode(0o000)).unwrap();
        record_state_id(&root, state::Id::from(8)).unwrap();
        assert_eq!(state_metadata_mode(&marker), STATE_ID_MODE);
        assert_eq!(fs::read_to_string(&marker).unwrap(), "8");
    }

    #[test]
    fn state_metadata_preserves_safe_existing_directory_modes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let usr = root.join("usr");
        fs::create_dir_all(&usr).unwrap();
        fs::set_permissions(&root, Permissions::from_mode(0o711)).unwrap();
        fs::set_permissions(&usr, Permissions::from_mode(0o750)).unwrap();

        record_state_id(&root, state::Id::from(9)).unwrap();
        let first_inode = fs::metadata(usr.join(STATE_ID_NAME)).unwrap().ino();
        record_state_id(&root, state::Id::from(10)).unwrap();

        assert_eq!(state_metadata_mode(&root), 0o711);
        assert_eq!(state_metadata_mode(&usr), 0o750);
        assert_eq!(state_metadata_mode(&usr.join(".stateID")), STATE_ID_MODE);
        assert_eq!(fs::read_to_string(usr.join(".stateID")).unwrap(), "10");
        assert_ne!(fs::metadata(usr.join(STATE_ID_NAME)).unwrap().ino(), first_inode);
        assert!(!usr.join(STATE_ID_TEMPORARY_NAME).exists());
    }

    fn generated_system_snapshot(package: &str) -> SystemModel {
        system_model::create(
            repository::Map::default(),
            BTreeSet::from([Provider::package_name(package)]),
        )
    }

    #[test]
    fn state_creation_records_and_exports_the_generated_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let intent_path = system_model::intent_path(temporary.path());
        fs::create_dir_all(intent_path.parent().unwrap()).unwrap();
        fs::set_permissions(temporary.path().join("etc"), Permissions::from_mode(0o755)).unwrap();
        fs::write(
            &intent_path,
            r#"// Authored intent must remain unchanged.
let cast = import! cast.system.v1
cast.system
"#,
        )
        .unwrap();
        let authored = fs::read_to_string(&intent_path).unwrap();

        let client = stateful_test_client(temporary.path());
        fs::create_dir_all(client.installation.assets_path("v2")).unwrap();
        let authored_fingerprint = client
            .installation
            .system_model
            .as_ref()
            .unwrap()
            .fingerprint()
            .sha256
            .clone();

        let created = client.new_state(&[], "Gluon state creation").unwrap().unwrap();
        let snapshot_path = system_model::snapshot_path(temporary.path());
        let recorded = fs::read_to_string(&snapshot_path).unwrap();
        assert!(recorded.starts_with(system_model::spec::GENERATED_GLUON_MARKER));
        assert!(recorded.contains(&format!("// Authored source fingerprint: {authored_fingerprint}")));
        assert_eq!(fs::read_to_string(&intent_path).unwrap(), authored);

        drop(client);
        let reopened = stateful_test_client(temporary.path());
        assert_eq!(reopened.installation.active_state, Some(created.id));
        let exported = reopened.export_state(created.id).unwrap();

        assert_eq!(exported.encoded(), recorded);
        assert_eq!(exported.source_fingerprint(), Some(authored_fingerprint.as_str()));
        assert_eq!(fs::read_to_string(intent_path).unwrap(), authored);
    }

    fn assert_generated_snapshot(path: &Path, expected: &str, package: &str) {
        let encoded = fs::read_to_string(path).unwrap();
        let evaluated =
            system_model::gluon::evaluate_generated_snapshot(&Source::new("system-model.glu", encoded.clone()))
                .unwrap();

        assert_eq!(encoded, expected);
        assert_eq!(evaluated.encoded(), encoded);
        assert!(evaluated.packages.contains(&Provider::package_name(package)));
    }

    #[test]
    fn ephemeral_import_evaluates_intent_and_records_only_a_generated_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        prepare_private_installation_root(temporary.path());
        let installation_root = temporary.path().join("installation");
        let blit_root = temporary.path().join("ephemeral-root");
        let intent_path = temporary.path().join("import.glu");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&blit_root).unwrap();
        prepare_private_installation_root(&blit_root);

        let authored = r#"// This authored source must never be copied into state.
let cast = import! cast.system.v1
{
    packages = ["alpha"],
    .. cast.system
}
"#;
        fs::write(&intent_path, authored).unwrap();

        let installation = test_installation(&installation_root);
        let client = Client::builder("ephemeral-import-test", installation)
            .system_intent_path(&intent_path)
            .ephemeral(&blit_root)
            .build()
            .unwrap();
        let imported = client.installation.system_model.as_ref().unwrap();

        assert!(client.is_ephemeral());
        assert_eq!(imported.authored_source(), authored);
        assert!(imported.packages.contains(&Provider::package_name("alpha")));

        let imported_fingerprint = imported.fingerprint().sha256.clone();
        record_system_snapshot(&blit_root, SystemModel::try_from(imported.clone()).unwrap()).unwrap();
        let snapshot_path = system_model::snapshot_path(&blit_root);
        let snapshot = fs::read_to_string(&snapshot_path).unwrap();
        let evaluated =
            system_model::gluon::evaluate_generated_snapshot(&Source::new("system-model.glu", snapshot.clone()))
                .unwrap();
        let loaded_snapshot = system_model::load(&snapshot_path).unwrap().unwrap();
        let round_trip = SystemModel::try_from(loaded_snapshot).unwrap();

        assert!(snapshot.starts_with(system_model::spec::GENERATED_GLUON_MARKER));
        assert!(snapshot.contains(&format!("// Authored source fingerprint: {imported_fingerprint}")));
        assert!(!snapshot.contains("This authored source must never be copied into state"));
        assert!(evaluated.packages.contains(&Provider::package_name("alpha")));
        assert_eq!(round_trip.encoded(), snapshot);
        assert_eq!(fs::read_to_string(intent_path).unwrap(), authored);
    }

    #[test]
    fn ephemeral_blit_isolates_cached_asset_bytes_and_mode() {
        let temporary = tempfile::tempdir().unwrap();
        prepare_private_installation_root(temporary.path());
        let installation_root = temporary.path().join("installation");
        let blit_root = temporary.path().join("ephemeral-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&blit_root).unwrap();
        prepare_private_installation_root(&blit_root);

        let installation = test_installation(&installation_root);
        let client = Client::builder("ephemeral-asset-isolation-test", installation)
            .repositories(repository::Map::default())
            .ephemeral(&blit_root)
            .build()
            .unwrap();

        let asset_id = xxhash_rust::xxh3::xxh3_128(b"persistent cached bytes");
        let asset_path = cache::asset_path(&client.installation, &format!("{asset_id:02x}"));
        fs::create_dir_all(asset_path.parent().unwrap()).unwrap();
        fs::write(&asset_path, b"persistent cached bytes").unwrap();
        fs::set_permissions(&asset_path, Permissions::from_mode(0o640)).unwrap();

        let package = package::Id::from("ephemeral-asset-isolation-package");
        client
            .layout_db
            .add(
                &package,
                &StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFREG | 0o755,
                    tag: 0,
                    file: StonePayloadLayoutFile::Regular(asset_id, "bin/cached-tool".into()),
                },
            )
            .unwrap();

        client.blit_root([&package]).unwrap();

        let materialized_path = blit_root.join("usr/bin/cached-tool");
        let cached_metadata = fs::metadata(&asset_path).unwrap();
        let materialized_metadata = fs::metadata(&materialized_path).unwrap();
        assert_ne!(
            (cached_metadata.dev(), cached_metadata.ino()),
            (materialized_metadata.dev(), materialized_metadata.ino())
        );
        assert_eq!(cached_metadata.permissions().mode() & 0o7777, 0o640);
        assert_eq!(materialized_metadata.permissions().mode() & 0o7777, 0o755);

        fs::write(&materialized_path, b"build-mutated bytes").unwrap();
        fs::set_permissions(&materialized_path, Permissions::from_mode(0o600)).unwrap();

        assert_eq!(fs::read(&asset_path).unwrap(), b"persistent cached bytes");
        assert_eq!(fs::metadata(&asset_path).unwrap().permissions().mode() & 0o7777, 0o640);
    }

    struct AssetCopyFixture {
        _temporary: tempfile::TempDir,
        installation: Installation,
        pool: AssetPool,
        source: PathBuf,
        source_path: PathBuf,
        output: PathBuf,
        output_parent: fs::File,
        digest: u128,
    }

    fn asset_copy_fixture(bytes: &[u8]) -> AssetCopyFixture {
        let temporary = tempfile::tempdir().unwrap();
        let installation = test_installation(temporary.path());
        let digest = xxhash_rust::xxh3::xxh3_128(bytes);
        let source_path = cache::asset_path(&installation, &format!("{digest:02x}"));
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, bytes).unwrap();
        fs::set_permissions(&source_path, Permissions::from_mode(0o640)).unwrap();
        let source = source_path
            .strip_prefix(installation.assets_path("v2"))
            .unwrap()
            .to_owned();
        let pool = AssetPool::open(&installation).unwrap();
        let output_directory = temporary.path().join("output");
        fs::create_dir(&output_directory).unwrap();
        let output_parent = fs::File::open(&output_directory).unwrap();
        let output = output_directory.join("copied");
        AssetCopyFixture {
            _temporary: temporary,
            installation,
            pool,
            source,
            source_path,
            output,
            output_parent,
            digest,
        }
    }

    fn copy_fixture_asset(fixture: &AssetCopyFixture) -> Result<(), Error> {
        copy_asset(
            &fixture.pool,
            &fixture.source,
            fixture.digest,
            fixture.output_parent.as_raw_fd(),
            "copied",
            nix::libc::S_IFREG | 0o755,
            None,
            Some(Instant::now() + Duration::from_secs(10)),
        )
    }

    #[test]
    fn frozen_copy_manifest_counts_output_inodes_and_enforces_exact_byte_limit() {
        let first = xxhash_rust::xxh3::xxh3_128(b"first frozen asset");
        let second = xxhash_rust::xxh3::xxh3_128(b"second frozen asset");
        let manifest =
            FrozenCopyManifest::from_digests_with_limit([EMPTY_FILE_DIGEST, first, first, second], 8, |digest| {
                match digest {
                    digest if digest == first => Ok(3),
                    digest if digest == second => Ok(2),
                    _ => unreachable!(),
                }
            })
            .unwrap();
        assert_eq!(
            manifest.total_bytes, 8,
            "duplicate digest must be charged per output inode"
        );
        assert_eq!(manifest.lengths.len(), 2, "empty files consume no cache-manifest entry");

        assert!(matches!(
            FrozenCopyManifest::from_digests_with_limit([first, first, second], 7, |digest| {
                if digest == first { Ok(3) } else { Ok(2) }
            }),
            Err(Error::FrozenMaterializationTotalByteLimit { limit: 7, actual: 8 })
        ));

        let mut total = 7;
        account_frozen_blit_bytes(&mut total, 1, 8).unwrap();
        assert_eq!(total, 8);
        assert!(matches!(
            account_frozen_blit_bytes(&mut total, 1, 8),
            Err(Error::FrozenMaterializationTotalByteLimit { limit: 8, actual: 9 })
        ));
        assert_eq!(total, 8, "a rejected N+1 byte must not mutate accounting");

        let mut overflow = u64::MAX;
        assert!(matches!(
            account_frozen_blit_bytes(&mut overflow, 1, u64::MAX),
            Err(Error::FrozenMaterializationTotalByteLimit {
                limit: u64::MAX,
                actual: u64::MAX
            })
        ));
        assert_eq!(overflow, u64::MAX);
    }

    #[test]
    fn frozen_capability_retry_timeout_remains_a_materialization_timeout() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("file");
        fs::write(&path, b"deadline proof").unwrap();
        let file = fs::File::open(&path).unwrap();
        let error = open_frozen_normalization_readonly(
            file.file(),
            Path::new("/file"),
            Instant::now() - Duration::from_millis(1),
        )
        .unwrap_err();
        assert!(matches!(error, Error::FrozenMaterializationTimeout { .. }));
    }

    #[test]
    fn independent_copy_rejects_length_changed_after_byte_preflight_before_creation() {
        let original = b"preflight length";
        let fixture = asset_copy_fixture(original);
        let manifest = FrozenCopyManifest::from_digests_with_limit([fixture.digest], original.len() as u64, |_| {
            Ok(original.len() as u64)
        })
        .unwrap();
        fs::write(&fixture.source_path, b"longer bytes after preflight").unwrap();

        let result = copy_asset(
            &fixture.pool,
            &fixture.source,
            fixture.digest,
            fixture.output_parent.as_raw_fd(),
            "copied",
            nix::libc::S_IFREG | 0o755,
            Some(&manifest),
            Some(Instant::now() + Duration::from_secs(10)),
        );
        assert!(matches!(
            result,
            Err(Error::FrozenMaterializationAssetLengthChanged { .. })
        ));
        assert!(!fixture.output.exists());
    }

    #[test]
    fn independent_copy_rejects_replaced_asset_pool_and_removes_partial_target() {
        let fixture = asset_copy_fixture(b"authenticated cache bytes");
        let asset_pool = fixture.installation.assets_path("v2");
        let detached = fixture.installation.assets_path("v2-detached");
        fs::rename(&asset_pool, &detached).unwrap();
        fs::create_dir(&asset_pool).unwrap();

        assert!(copy_fixture_asset(&fixture).is_err());
        assert!(!fixture.output.exists());
    }

    #[test]
    fn independent_copy_rejects_symlinked_asset_component() {
        let fixture = asset_copy_fixture(b"component traversal bytes");
        let first = fixture.source.components().next().unwrap().as_os_str();
        let component = fixture.installation.assets_path("v2").join(first);
        let detached = fixture.installation.assets_path("v2").join("detached-component");
        fs::rename(&component, &detached).unwrap();
        symlink(&detached, &component).unwrap();

        assert!(copy_fixture_asset(&fixture).is_err());
        assert!(!fixture.output.exists());
    }

    #[test]
    fn independent_copy_rejects_fifo_without_blocking() {
        let fixture = asset_copy_fixture(b"fifo placeholder");
        fs::remove_file(&fixture.source_path).unwrap();
        nix::unistd::mkfifo(&fixture.source_path, Mode::from_bits_truncate(0o600)).unwrap();

        let started = Instant::now();
        assert!(copy_fixture_asset(&fixture).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!fixture.output.exists());
    }

    #[test]
    fn independent_copy_rejects_final_symlink_and_directory() {
        let fixture = asset_copy_fixture(b"non-regular placeholder");
        fs::remove_file(&fixture.source_path).unwrap();
        symlink("missing", &fixture.source_path).unwrap();
        assert!(copy_fixture_asset(&fixture).is_err());
        assert!(!fixture.output.exists());

        fs::remove_file(&fixture.source_path).unwrap();
        fs::create_dir(&fixture.source_path).unwrap();
        assert!(copy_fixture_asset(&fixture).is_err());
        assert!(!fixture.output.exists());
    }

    #[test]
    fn independent_copy_rejects_digest_mismatch_and_removes_target() {
        let fixture = asset_copy_fixture(b"digest-bound bytes");
        let result = copy_asset(
            &fixture.pool,
            &fixture.source,
            fixture.digest ^ 1,
            fixture.output_parent.as_raw_fd(),
            "copied",
            nix::libc::S_IFREG | 0o755,
            None,
            Some(Instant::now() + Duration::from_secs(10)),
        );

        assert!(result.is_err());
        assert!(!fixture.output.exists());
    }

    #[test]
    fn independent_copy_rejects_source_replacement_after_open() {
        let fixture = asset_copy_fixture(b"pinned original bytes");
        let detached = fixture.source_path.with_extension("detached");
        let mut replaced = false;
        let result = copy_asset_with_checkpoint(
            &fixture.pool,
            &fixture.source,
            fixture.digest,
            fixture.output_parent.as_raw_fd(),
            "copied",
            nix::libc::S_IFREG | 0o755,
            None,
            Some(Instant::now() + Duration::from_secs(10)),
            |checkpoint| {
                if checkpoint == AssetCopyCheckpoint::SourceOpened && !replaced {
                    fs::rename(&fixture.source_path, &detached).unwrap();
                    fs::write(&fixture.source_path, b"hostile replacement").unwrap();
                    replaced = true;
                }
            },
        );

        assert!(result.is_err());
        assert!(!fixture.output.exists(), "copy failure was {result:#?}");
    }

    #[test]
    fn independent_copy_rejects_source_mutation_after_streaming() {
        let original = b"original stable bytes";
        let fixture = asset_copy_fixture(original);
        let mut mutated = false;
        let result = copy_asset_with_checkpoint(
            &fixture.pool,
            &fixture.source,
            fixture.digest,
            fixture.output_parent.as_raw_fd(),
            "copied",
            nix::libc::S_IFREG | 0o755,
            None,
            Some(Instant::now() + Duration::from_secs(10)),
            |checkpoint| {
                if checkpoint == AssetCopyCheckpoint::BytesCopied && !mutated {
                    fs::write(&fixture.source_path, b"mutated hostile bytes").unwrap();
                    mutated = true;
                }
            },
        );

        assert!(result.is_err());
        assert!(!fixture.output.exists(), "copy failure was {result:#?}");
    }

    #[test]
    fn exact_copy_accepts_n_and_rejects_n_minus_or_plus_one() {
        let temporary = tempfile::tempdir().unwrap();
        let bytes = vec![0x5a; ASSET_COPY_BUFFER_BYTES * 2];
        let digest = xxhash_rust::xxh3::xxh3_128(&bytes);
        let source_path = temporary.path().join("source");
        fs::write(&source_path, &bytes).unwrap();

        let source = fs::File::open(&source_path).unwrap();
        let target = fs::File::create(temporary.path().join("exact")).unwrap();
        copy_fd_exact(
            source.as_raw_fd(),
            target.as_raw_fd(),
            bytes.len() as u64,
            digest,
            Some(Instant::now() + Duration::from_secs(10)),
        )
        .unwrap();
        drop(target);
        assert_eq!(fs::read(temporary.path().join("exact")).unwrap(), bytes);

        let source = fs::File::open(&source_path).unwrap();
        let target = fs::File::create(temporary.path().join("short-bound")).unwrap();
        assert!(
            copy_fd_exact(
                source.as_raw_fd(),
                target.as_raw_fd(),
                bytes.len() as u64 - 1,
                digest,
                Some(Instant::now() + Duration::from_secs(10)),
            )
            .is_err()
        );

        let source = fs::File::open(&source_path).unwrap();
        let target = fs::File::create(temporary.path().join("long-bound")).unwrap();
        assert!(
            copy_fd_exact(
                source.as_raw_fd(),
                target.as_raw_fd(),
                bytes.len() as u64 + 1,
                digest,
                Some(Instant::now() + Duration::from_secs(10)),
            )
            .is_err()
        );
    }

    #[test]
    fn independent_copy_never_unlinks_preexisting_hardlink_target() {
        let fixture = asset_copy_fixture(b"exclusive destination bytes");
        let sentinel = fixture.output.with_extension("sentinel");
        fs::write(&sentinel, b"sentinel").unwrap();
        fs::hard_link(&sentinel, &fixture.output).unwrap();
        let before = fs::metadata(&sentinel).unwrap();

        assert!(copy_fixture_asset(&fixture).is_err());
        assert_eq!(fs::read(&fixture.output).unwrap(), b"sentinel");
        let after = fs::metadata(&fixture.output).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(after.nlink(), 2);
    }

    #[test]
    fn frozen_executable_limits_accept_the_boundary_and_reject_the_next_value() {
        assert!(require_frozen_executable_package_count(MAX_FROZEN_EXECUTABLE_PACKAGES).is_ok());
        assert!(matches!(
            require_frozen_executable_package_count(MAX_FROZEN_EXECUTABLE_PACKAGES + 1),
            Err(Error::FrozenExecutablePackageLimit { limit, actual })
                if limit == MAX_FROZEN_EXECUTABLE_PACKAGES
                    && actual == MAX_FROZEN_EXECUTABLE_PACKAGES + 1
        ));
        assert!(require_frozen_executable_binding_count(MAX_FROZEN_EXECUTABLE_BINDINGS).is_ok());
        assert!(matches!(
            require_frozen_executable_binding_count(MAX_FROZEN_EXECUTABLE_BINDINGS + 1),
            Err(Error::FrozenExecutableBindingLimit { limit, actual })
                if limit == MAX_FROZEN_EXECUTABLE_BINDINGS
                    && actual == MAX_FROZEN_EXECUTABLE_BINDINGS + 1
        ));

        let binding = FrozenExecutableBinding {
            package: package::Id::from("limit-provider"),
            path: PathBuf::from("/usr/bin/limit-tool"),
        };
        let package = binding.package.clone();

        let path_prefix = "/usr/bin/";
        let accepted_path = format!(
            "{path_prefix}{}",
            "a".repeat(MAX_FROZEN_EXECUTABLE_PATH_BYTES - path_prefix.len())
        );
        let accepted_binding = FrozenExecutableBinding {
            package: package.clone(),
            path: PathBuf::from(&accepted_path),
        };
        assert_eq!(accepted_path.len(), MAX_FROZEN_EXECUTABLE_PATH_BYTES);
        require_frozen_executable_path(&accepted_binding).unwrap();
        let rejected_binding = FrozenExecutableBinding {
            package: package.clone(),
            path: PathBuf::from(format!("{accepted_path}a")),
        };
        assert!(matches!(
            require_frozen_executable_path(&rejected_binding),
            Err(Error::FrozenExecutablePathByteLimit { limit, actual })
                if limit == MAX_FROZEN_EXECUTABLE_PATH_BYTES
                    && actual == MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1
        ));
        assert!(frozen_executable_symlink_target_length_is_admitted(
            MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES
        ));
        assert!(!frozen_executable_symlink_target_length_is_admitted(
            MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1
        ));

        let mut closure_bytes = MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES - package.as_str().len();
        account_frozen_closure_id_bytes(&package, &mut closure_bytes).unwrap();
        assert_eq!(closure_bytes, MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES);
        assert!(matches!(
            account_frozen_closure_id_bytes(&package, &mut closure_bytes),
            Err(Error::FrozenExecutableClosureIdByteLimit { limit, actual })
                if limit == MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES
                    && actual == MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES + package.as_str().len()
        ));

        let mut binding_bytes = MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES - 1;
        account_frozen_binding_bytes(&binding, 1, &mut binding_bytes).unwrap();
        assert_eq!(binding_bytes, MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES);
        assert!(matches!(
            account_frozen_binding_bytes(&binding, 1, &mut binding_bytes),
            Err(Error::FrozenExecutableBindingByteLimit { limit, actual, .. })
                if limit == MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES
                    && actual == MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES + 1
        ));

        assert!(require_frozen_executable_layout_count(MAX_FROZEN_EXECUTABLE_LAYOUTS).is_ok());
        assert!(matches!(
            require_frozen_executable_layout_count(MAX_FROZEN_EXECUTABLE_LAYOUTS + 1),
            Err(Error::FrozenExecutableLayoutLimit { limit, actual })
                if limit == MAX_FROZEN_EXECUTABLE_LAYOUTS
                    && actual == MAX_FROZEN_EXECUTABLE_LAYOUTS + 1
        ));
        let mut layout_bytes = MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES - 1;
        account_frozen_layout_bytes(&package, &binding.path, 1, &mut layout_bytes).unwrap();
        assert_eq!(layout_bytes, MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES);
        assert!(matches!(
            account_frozen_layout_bytes(&package, &binding.path, 1, &mut layout_bytes),
            Err(Error::FrozenExecutableLayoutByteLimit { limit, actual, .. })
                if limit == MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES
                    && actual == MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES + 1
        ));

        assert!(require_frozen_executable_directory_count(MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS).is_ok());
        assert!(matches!(
            require_frozen_executable_directory_count(MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS + 1),
            Err(Error::FrozenExecutableDirectoryLimit { limit, actual })
                if limit == MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS
                    && actual == MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS + 1
        ));
        let mut directory_bytes = MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES - 1;
        account_frozen_executable_directory_bytes(1, &mut directory_bytes).unwrap();
        assert_eq!(directory_bytes, MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES);
        assert!(matches!(
            account_frozen_executable_directory_bytes(1, &mut directory_bytes),
            Err(Error::FrozenExecutableDirectoryByteLimit { limit, actual })
                if limit == MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES
                    && actual == MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES + 1
        ));

        let mut total = MAX_TOTAL_FROZEN_EXECUTABLE_BYTES - MAX_FROZEN_EXECUTABLE_BYTES;
        account_frozen_executable_bytes(&binding, MAX_FROZEN_EXECUTABLE_BYTES, &mut total).unwrap();
        assert_eq!(total, MAX_TOTAL_FROZEN_EXECUTABLE_BYTES);

        let mut empty = 0;
        assert!(matches!(
            account_frozen_executable_bytes(&binding, MAX_FROZEN_EXECUTABLE_BYTES + 1, &mut empty),
            Err(Error::FrozenExecutableByteLimit { limit, actual, .. })
                if limit == MAX_FROZEN_EXECUTABLE_BYTES
                    && actual == MAX_FROZEN_EXECUTABLE_BYTES + 1
        ));

        let mut full = MAX_TOTAL_FROZEN_EXECUTABLE_BYTES;
        assert!(matches!(
            account_frozen_executable_bytes(&binding, 1, &mut full),
            Err(Error::FrozenExecutableTotalByteLimit { limit, actual })
                if limit == MAX_TOTAL_FROZEN_EXECUTABLE_BYTES
                    && actual == MAX_TOTAL_FROZEN_EXECUTABLE_BYTES + 1
        ));
        assert_eq!(full, MAX_TOTAL_FROZEN_EXECUTABLE_BYTES);

        assert!(require_frozen_executable_deadline(Instant::now() + Duration::from_secs(1)).is_ok());
        assert!(matches!(
            require_frozen_executable_deadline(Instant::now() - Duration::from_secs(1)),
            Err(Error::FrozenExecutableVerificationTimeout { .. })
        ));
        assert!(require_frozen_materialization_deadline(Instant::now() + Duration::from_secs(1)).is_ok());
        assert!(matches!(
            require_frozen_materialization_deadline(Instant::now() - Duration::from_secs(1)),
            Err(Error::FrozenMaterializationTimeout { .. })
        ));
    }

    #[test]
    fn frozen_binding_paths_preflight_raw_bounds_before_provider_lookup_or_copy() {
        let package = package::Id::from("inside-provider");
        let outside = package::Id::from("outside-provider");

        let accepted = FrozenExecutableBinding {
            package: package.clone(),
            path: PathBuf::from(format!(
                "/{}",
                std::iter::once("usr")
                    .chain(std::iter::repeat_n("a", MAX_FROZEN_LAYOUT_PATH_COMPONENTS - 1))
                    .join("/")
            )),
        };
        assert_eq!(
            accepted.path.components().count(),
            MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1
        );
        require_frozen_executable_path(&accepted).unwrap();

        let too_deep = FrozenExecutableBinding {
            package: package.clone(),
            path: PathBuf::from(format!("{}/a", accepted.path.display())),
        };
        assert!(matches!(
            require_frozen_executable_path(&too_deep),
            Err(Error::FrozenExecutablePathDepthLimit { limit, actual })
                if limit == MAX_FROZEN_LAYOUT_PATH_COMPONENTS
                    && actual == MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1
        ));

        let invalid_utf8 = FrozenExecutableBinding {
            package: package.clone(),
            path: PathBuf::from(OsString::from_vec(b"/usr/bin/\xff".to_vec())),
        };
        assert!(matches!(
            require_frozen_executable_path(&invalid_utf8),
            Err(Error::FrozenExecutablePathEncoding { bytes }) if bytes == b"/usr/bin/\xff".len()
        ));

        let mut oversized_non_utf8 = vec![b'a'; MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1];
        oversized_non_utf8[0] = 0xff;
        let oversized = FrozenExecutableBinding {
            package: outside,
            path: PathBuf::from(OsString::from_vec(oversized_non_utf8)),
        };
        assert!(matches!(
            require_frozen_executable_path(&oversized),
            Err(Error::FrozenExecutablePathByteLimit { limit, actual })
                if limit == MAX_FROZEN_EXECUTABLE_PATH_BYTES
                    && actual == MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1
        ));

        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let frozen_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&frozen_root).unwrap();
        let client = Client::frozen(
            "frozen-raw-binding-preflight-test",
            frozen_test_installation(&installation_root),
            repository::Map::default(),
            &frozen_root,
        )
        .unwrap();
        assert!(matches!(
            client.require_frozen_executables(std::slice::from_ref(&package), &[oversized]),
            Err(Error::FrozenExecutablePathByteLimit { .. })
        ));
    }

    #[test]
    fn empty_frozen_binding_set_returns_a_live_root_guard_and_detects_substitution() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let frozen_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&frozen_root).unwrap();
        let client = Client::frozen(
            "empty-frozen-root-guard-test",
            frozen_test_installation(&installation_root),
            repository::Map::default(),
            &frozen_root,
        )
        .unwrap();
        let package = package::Id::from("guard-provider");
        let guard = client
            .require_frozen_executables(std::slice::from_ref(&package), &[])
            .unwrap();
        drop(client);

        assert_eq!(guard.root_path(), frozen_root);
        assert!(guard.revalidated_anchor().unwrap().as_raw_fd() >= 0);

        let moved = temporary.path().join("moved-frozen-root");
        fs::rename(&frozen_root, &moved).unwrap();
        fs::create_dir(&frozen_root).unwrap();
        assert!(matches!(
            guard.revalidate(),
            Err(Error::FrozenExecutableRootReplaced(path)) if path == frozen_root
        ));
    }

    #[test]
    fn frozen_shebang_limits_accept_n_and_reject_n_plus_one() {
        let prefix = "/usr/bin/";
        let accepted_path = format!(
            "{prefix}{}",
            "a".repeat(MAX_FROZEN_SHEBANG_INTERPRETER_BYTES - prefix.len())
        );
        assert_eq!(accepted_path.len(), MAX_FROZEN_SHEBANG_INTERPRETER_BYTES);
        let accepted = format!("#!{accepted_path}\n");
        assert_eq!(accepted.len(), MAX_FROZEN_SHEBANG_LINE_BYTES);
        assert_eq!(
            parse_frozen_shebang(accepted.as_bytes()).unwrap(),
            Some(FrozenShebangInterpreter {
                path: PathBuf::from(accepted_path),
                root_alias: None,
            })
        );

        let rejected_path = format!(
            "{prefix}{}",
            "a".repeat(MAX_FROZEN_SHEBANG_INTERPRETER_BYTES + 1 - prefix.len())
        );
        let rejected = format!("#!{rejected_path}\n");
        assert_eq!(rejected.len(), MAX_FROZEN_SHEBANG_LINE_BYTES + 1);
        assert_eq!(
            parse_frozen_shebang(rejected.as_bytes()),
            Err(FrozenShebangParseError::LineTooLong)
        );

        let binding = FrozenExecutableBinding {
            package: package::Id::from("script-provider"),
            path: PathBuf::from("/usr/bin/script"),
        };
        require_frozen_shebang_interpreter_count(&binding, MAX_FROZEN_SHEBANG_INTERPRETERS).unwrap();
        assert!(matches!(
            require_frozen_shebang_interpreter_count(&binding, MAX_FROZEN_SHEBANG_INTERPRETERS + 1),
            Err(Error::FrozenShebangInterpreterLimit { package, path, limit })
                if package == binding.package
                    && path == binding.path
                    && limit == MAX_FROZEN_SHEBANG_INTERPRETERS
        ));
        require_frozen_executable_interpreter_count(&binding, MAX_FROZEN_EXECUTABLE_INTERPRETERS).unwrap();
        assert!(matches!(
            require_frozen_executable_interpreter_count(&binding, MAX_FROZEN_EXECUTABLE_INTERPRETERS + 1),
            Err(Error::FrozenExecutableInterpreterLimit { package, path, limit })
                if package == binding.package
                    && path == binding.path
                    && limit == MAX_FROZEN_EXECUTABLE_INTERPRETERS
        ));

        let mut pinned = MAX_FROZEN_EXECUTABLE_PINNED_FILES - 1;
        reserve_frozen_pinned_files(&binding, &mut pinned, 1).unwrap();
        assert_eq!(pinned, MAX_FROZEN_EXECUTABLE_PINNED_FILES);
        assert!(matches!(
            reserve_frozen_pinned_files(&binding, &mut pinned, 1),
            Err(Error::FrozenExecutablePinnedFileLimit { package, path, limit, actual })
                if package == binding.package
                    && path == binding.path
                    && limit == MAX_FROZEN_EXECUTABLE_PINNED_FILES
                    && actual == MAX_FROZEN_EXECUTABLE_PINNED_FILES + 1
        ));
        assert_eq!(pinned, MAX_FROZEN_EXECUTABLE_PINNED_FILES);
    }

    #[test]
    fn frozen_shebang_depth_matches_the_linux_execve_boundary() {
        fn install_chain(root: &Path, depth: usize) -> PathBuf {
            let paths = (0..depth)
                .map(|index| root.join(format!("script-{index}")))
                .collect::<Vec<_>>();
            for (index, path) in paths.iter().enumerate() {
                let interpreter = paths
                    .get(index + 1)
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from("/bin/true"));
                fs::write(path, format!("#!{}\n", interpreter.display())).unwrap();
                fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
            }
            paths.into_iter().next().unwrap()
        }

        assert_eq!(MAX_FROZEN_SHEBANG_INTERPRETERS, 5);
        // Put scripts beside the running test binary rather than in /tmp;
        // hardened CI commonly mounts /tmp noexec, while this filesystem is
        // proven executable by the current process.
        let executable_directory = std::env::current_exe().unwrap();
        let executable_directory = executable_directory.parent().unwrap();
        let accepted = tempfile::Builder::new()
            .prefix("forge-shebang-accepted-")
            .tempdir_in(executable_directory)
            .unwrap();
        let accepted_entry = install_chain(accepted.path(), MAX_FROZEN_SHEBANG_INTERPRETERS);
        assert!(Command::new(accepted_entry).status().unwrap().success());

        let rejected = tempfile::Builder::new()
            .prefix("forge-shebang-rejected-")
            .tempdir_in(executable_directory)
            .unwrap();
        let rejected_entry = install_chain(rejected.path(), MAX_FROZEN_SHEBANG_INTERPRETERS + 1);
        let error = Command::new(rejected_entry).status().unwrap_err();
        assert_eq!(error.raw_os_error(), Some(nix::libc::ELOOP));
    }

    #[test]
    fn frozen_shebang_parser_accepts_only_one_absolute_frozen_path() {
        assert_eq!(
            parse_frozen_shebang(b"#!/bin/sh\necho ok\n").unwrap(),
            Some(FrozenShebangInterpreter {
                path: PathBuf::from("/usr/bin/sh"),
                root_alias: Some(ExpectedFrozenRootAlias {
                    path: PathBuf::from("/bin"),
                    target: "usr/bin".to_owned(),
                }),
            })
        );
        assert_eq!(parse_frozen_shebang(b"\x7fELFnot-a-script").unwrap(), None);
        assert_eq!(
            parse_frozen_shebang(b"#!/usr/bin/env\n"),
            Err(FrozenShebangParseError::EnvironmentLookup)
        );
        assert_eq!(
            parse_frozen_shebang(b"#!/usr/bin/env bash\n"),
            Err(FrozenShebangParseError::WhitespaceOrOptions)
        );
        assert_eq!(
            parse_frozen_shebang(b"#!/usr/bin/bash -e\n"),
            Err(FrozenShebangParseError::WhitespaceOrOptions)
        );
        assert_eq!(
            parse_frozen_shebang(b"#!relative/interpreter\n"),
            Err(FrozenShebangParseError::Relative)
        );
        assert_eq!(
            parse_frozen_shebang(b"#!/usr/bin/ba\0sh\n"),
            Err(FrozenShebangParseError::Nul)
        );
        assert_eq!(
            parse_frozen_shebang(b"#!/usr/bin/bash"),
            Err(FrozenShebangParseError::Unterminated)
        );
        assert_eq!(
            parse_frozen_shebang(b"#!/usr/bin/../bin/bash\n"),
            Err(FrozenShebangParseError::NonNormalized)
        );
    }

    #[test]
    fn frozen_executable_format_rejects_shell_fallback_and_unknown_binfmt_inputs() {
        let binding = FrozenExecutableBinding {
            package: package::Id::from("format-provider"),
            path: PathBuf::from("/usr/bin/tool"),
        };
        for bytes in [
            b"echo this must never reach a shell\n".as_slice(),
            b"MZunknown-binfmt-input".as_slice(),
            b"".as_slice(),
        ] {
            assert!(matches!(
                inspect_test_executable(bytes, &binding),
                Err(Error::InvalidFrozenExecutableFormat { package, path, .. })
                    if package == binding.package && path == binding.path
            ));
        }
        assert!(matches!(
            inspect_test_executable(b"#!/usr/bin/sh", &binding),
            Err(Error::InvalidFrozenShebang { package, path, .. })
                if package == binding.package && path == binding.path
        ));
    }

    #[test]
    fn frozen_elf_admission_is_structural_and_binds_pt_interp() {
        let binding = FrozenExecutableBinding {
            package: package::Id::from("elf-provider"),
            path: PathBuf::from("/usr/bin/elf-tool"),
        };
        assert_eq!(inspect_test_executable(&test_elf(None, 1), &binding).unwrap(), None);
        assert_eq!(
            inspect_test_executable(&test_elf(Some("/lib64/ld-frozen.so"), 2), &binding).unwrap(),
            Some(FrozenExecutableInterpreter::Elf(FrozenShebangInterpreter {
                path: PathBuf::from("/usr/lib/ld-frozen.so"),
                root_alias: Some(ExpectedFrozenRootAlias {
                    path: PathBuf::from("/lib64"),
                    target: "usr/lib".to_owned(),
                }),
            }))
        );

        let mut truncated = test_elf(None, 1);
        truncated.truncate(16);
        assert!(matches!(
            inspect_test_executable(&truncated, &binding),
            Err(Error::InvalidFrozenExecutableFormat { .. })
        ));

        let mut wrong_machine = test_elf(None, 1);
        test_elf_write_u16(&mut wrong_machine, 18, 0, cfg!(target_endian = "little"));
        assert!(matches!(
            inspect_test_executable(&wrong_machine, &binding),
            Err(Error::InvalidFrozenExecutableFormat { .. })
        ));

        let mut unterminated_interp = test_elf(Some("/lib64/ld-frozen.so"), 2);
        *unterminated_interp.last_mut().unwrap() = b'x';
        assert!(matches!(
            inspect_test_executable(&unterminated_interp, &binding),
            Err(Error::InvalidFrozenExecutableFormat { .. })
        ));
    }

    #[test]
    fn frozen_elf_rejects_malformed_headers_segments_and_interpreters() {
        let binding = FrozenExecutableBinding {
            package: package::Id::from("malformed-elf-provider"),
            path: PathBuf::from("/usr/bin/malformed-elf"),
        };
        let little_endian = cfg!(target_endian = "little");
        let class64 = usize::BITS == 64;
        let header_size = if class64 { 64 } else { 52 };
        let program_header_size = if class64 { 56 } else { 32 };
        let assert_invalid = |bytes: &[u8]| {
            assert!(matches!(
                inspect_test_executable(bytes, &binding),
                Err(Error::InvalidFrozenExecutableFormat { package, path, .. })
                    if package == binding.package && path == binding.path
            ));
        };

        let mut relative_interp = test_elf(Some("relative/loader"), 2);
        assert_invalid(&relative_interp);
        let interpreter_offset = header_size + 2 * program_header_size;
        relative_interp[interpreter_offset + 1] = 0;
        assert_invalid(&relative_interp);

        let mut relocatable = test_elf(None, 1);
        test_elf_write_u16(&mut relocatable, 16, 1, little_endian);
        assert_invalid(&relocatable);

        let mut wrong_class = test_elf(None, 1);
        wrong_class[4] = if class64 { 1 } else { 2 };
        assert_invalid(&wrong_class);

        let mut no_program_headers = test_elf(None, 1);
        test_elf_write_u16(&mut no_program_headers, if class64 { 56 } else { 44 }, 0, little_endian);
        assert_invalid(&no_program_headers);

        let mut table_past_eof = test_elf(None, 1);
        let table_offset = table_past_eof.len() as u64 + 1;
        if class64 {
            test_elf_write_u64(&mut table_past_eof, 32, table_offset, little_endian);
        } else {
            test_elf_write_u32(&mut table_past_eof, 28, table_offset as u32, little_endian);
        }
        assert_invalid(&table_past_eof);

        let load = header_size;
        let mut non_executable_load = test_elf(None, 1);
        test_elf_write_u32(
            &mut non_executable_load,
            load + if class64 { 4 } else { 24 },
            4,
            little_endian,
        );
        assert_invalid(&non_executable_load);

        let mut segment_past_eof = test_elf(None, 1);
        let oversized = segment_past_eof.len() as u64 + 1;
        if class64 {
            test_elf_write_u64(&mut segment_past_eof, load + 32, oversized, little_endian);
        } else {
            test_elf_write_u32(&mut segment_past_eof, load + 16, oversized as u32, little_endian);
        }
        assert_invalid(&segment_past_eof);

        let mut memory_smaller_than_file = test_elf(None, 1);
        if class64 {
            test_elf_write_u64(&mut memory_smaller_than_file, load + 40, 1, little_endian);
        } else {
            test_elf_write_u32(&mut memory_smaller_than_file, load + 20, 1, little_endian);
        }
        assert_invalid(&memory_smaller_than_file);

        let mut invalid_alignment = test_elf(None, 1);
        if class64 {
            test_elf_write_u64(&mut invalid_alignment, load + 48, 3, little_endian);
        } else {
            test_elf_write_u32(&mut invalid_alignment, load + 28, 3, little_endian);
        }
        assert_invalid(&invalid_alignment);

        let mut duplicate_interp = test_elf(Some("/lib64/ld-frozen.so"), 3);
        let first_interp = header_size + program_header_size;
        let duplicate = first_interp + program_header_size;
        let header = duplicate_interp[first_interp..first_interp + program_header_size].to_vec();
        duplicate_interp[duplicate..duplicate + program_header_size].copy_from_slice(&header);
        assert_invalid(&duplicate_interp);

        let mut one_byte_interp = test_elf(Some("/lib64/ld-frozen.so"), 2);
        let interp_header = header_size + program_header_size;
        if class64 {
            test_elf_write_u64(&mut one_byte_interp, interp_header + 32, 1, little_endian);
        } else {
            test_elf_write_u32(&mut one_byte_interp, interp_header + 16, 1, little_endian);
        }
        assert_invalid(&one_byte_interp);
    }

    #[test]
    fn frozen_elf_admission_parses_the_host_binary_and_confines_its_interp() {
        let binding = FrozenExecutableBinding {
            package: package::Id::from("host-elf-provider"),
            path: PathBuf::from("/usr/bin/host-elf"),
        };
        let mut file = fs::File::open(std::env::current_exe().unwrap()).unwrap();
        let length = file.metadata().unwrap().len();
        let mut probe = vec![0; MAX_FROZEN_SHEBANG_LINE_BYTES + 1];
        let read = file.read(&mut probe).unwrap();
        probe.truncate(read);
        match inspect_frozen_executable_format(
            &file,
            length,
            &probe,
            Instant::now() + Duration::from_secs(10),
            &binding,
        ) {
            Ok(None | Some(FrozenExecutableInterpreter::Elf(_))) => {}
            // Nix-linked test binaries deliberately name a store interpreter,
            // which a /usr-only frozen root must reject after successfully
            // parsing the real ELF and its PT_INTERP segment.
            Err(Error::InvalidFrozenExecutableFormat {
                reason: "ELF PT_INTERP path is not absolute and normalized",
                ..
            }) => {}
            result => panic!("unexpected host ELF admission result: {result:?}"),
        }
    }

    #[test]
    fn frozen_elf_program_header_limit_accepts_n_and_rejects_n_plus_one() {
        let binding = FrozenExecutableBinding {
            package: package::Id::from("elf-limit-provider"),
            path: PathBuf::from("/usr/bin/elf-limit"),
        };
        inspect_test_executable(&test_elf(None, MAX_FROZEN_ELF_PROGRAM_HEADERS), &binding).unwrap();
        assert!(matches!(
            inspect_test_executable(&test_elf(None, MAX_FROZEN_ELF_PROGRAM_HEADERS + 1), &binding),
            Err(Error::FrozenElfProgramHeaderLimit { package, path, limit, actual })
                if package == binding.package
                    && path == binding.path
                    && limit == MAX_FROZEN_ELF_PROGRAM_HEADERS
                    && actual == MAX_FROZEN_ELF_PROGRAM_HEADERS + 1
        ));
    }

    #[test]
    fn frozen_interpreter_layout_requires_one_provider_and_confined_symlinks() {
        let link_provider = package::Id::from("link-provider");
        let regular_provider = package::Id::from("regular-provider");
        let link = PathBuf::from("/usr/bin/interpreter");
        let regular = PathBuf::from("/usr/bin/interpreter-real");
        let mut layouts = BTreeMap::from([
            (
                link_provider.clone(),
                BTreeMap::from([(
                    link.clone(),
                    FrozenExecutableLayout::Symlink {
                        target: "interpreter-real".to_owned(),
                        mode: nix::libc::S_IFLNK | 0o777,
                    },
                )]),
            ),
            (
                regular_provider.clone(),
                BTreeMap::from([(
                    regular.clone(),
                    FrozenExecutableLayout::Regular {
                        digest: 7,
                        mode: nix::libc::S_IFREG | 0o755,
                    },
                )]),
            ),
        ]);
        let mut providers = BTreeMap::from([
            (link.clone(), BTreeSet::from([link_provider.clone()])),
            (regular.clone(), BTreeSet::from([regular_provider.clone()])),
        ]);
        let redirects = BTreeMap::new();
        let deadline = Instant::now() + Duration::from_secs(1);

        let (binding, expected) =
            resolve_frozen_interpreter_layout(&link, &layouts, &providers, &redirects, deadline).unwrap();
        assert_eq!(binding.package, regular_provider);
        assert_eq!(binding.path, link);
        assert_eq!(expected.resolved_path, regular);
        assert_eq!(expected.symlinks.len(), 1);
        assert_eq!(expected.symlinks[0].package, link_provider);

        let missing = PathBuf::from("/usr/bin/missing");
        assert!(matches!(
            resolve_frozen_interpreter_layout(&missing, &layouts, &providers, &redirects, deadline),
            Err(Error::MissingFrozenInterpreterProvider { path }) if path == missing
        ));

        let ambiguous = PathBuf::from("/usr/bin/ambiguous");
        for provider in [&link_provider, &regular_provider] {
            layouts.get_mut(provider).unwrap().insert(
                ambiguous.clone(),
                FrozenExecutableLayout::Regular {
                    digest: 8,
                    mode: nix::libc::S_IFREG | 0o755,
                },
            );
        }
        providers.insert(
            ambiguous.clone(),
            BTreeSet::from([link_provider.clone(), regular_provider.clone()]),
        );
        assert!(matches!(
            resolve_frozen_interpreter_layout(&ambiguous, &layouts, &providers, &redirects, deadline),
            Err(Error::AmbiguousFrozenInterpreterProvider { path, providers })
                if path == ambiguous && providers.len() == 2
        ));

        layouts.get_mut(&link_provider).unwrap().insert(
            link.clone(),
            FrozenExecutableLayout::Symlink {
                target: "../../../etc/passwd".to_owned(),
                mode: nix::libc::S_IFLNK | 0o777,
            },
        );
        assert!(matches!(
            resolve_frozen_interpreter_layout(&link, &layouts, &providers, &redirects, deadline),
            Err(Error::InvalidFrozenExecutableSymlinkTarget { package, path, .. })
                if package == link_provider && path == link
        ));

        let cycle_a = PathBuf::from("/usr/bin/cycle-a");
        let cycle_b = PathBuf::from("/usr/bin/cycle-b");
        layouts.insert(
            link_provider.clone(),
            BTreeMap::from([
                (
                    cycle_a.clone(),
                    FrozenExecutableLayout::Symlink {
                        target: "cycle-b".to_owned(),
                        mode: nix::libc::S_IFLNK | 0o777,
                    },
                ),
                (
                    cycle_b.clone(),
                    FrozenExecutableLayout::Symlink {
                        target: "cycle-a".to_owned(),
                        mode: nix::libc::S_IFLNK | 0o777,
                    },
                ),
            ]),
        );
        providers.insert(cycle_a.clone(), BTreeSet::from([link_provider.clone()]));
        providers.insert(cycle_b, BTreeSet::from([link_provider]));
        assert!(matches!(
            resolve_frozen_interpreter_layout(&cycle_a, &layouts, &providers, &redirects, deadline),
            Err(Error::FrozenInterpreterSymlinkCycle { path }) if path == cycle_a
        ));
    }

    #[test]
    fn frozen_executables_explicitly_reject_materialized_directory_redirects() {
        let package = package::Id::from("redirect-provider");
        let source = PathBuf::from("/usr/lib/redirect");
        let target = PathBuf::from("/usr/lib/real");
        let logical_tool = source.join("tool");
        let prepared = vec![
            PreparedFrozenExecutableLayout {
                package: package.clone(),
                path: source.clone(),
                entry: FrozenExecutableLayout::Symlink {
                    target: target.to_string_lossy().into_owned(),
                    mode: nix::libc::S_IFLNK | 0o777,
                },
                is_directory: false,
            },
            PreparedFrozenExecutableLayout {
                package: package.clone(),
                path: target.clone(),
                entry: FrozenExecutableLayout::Other,
                is_directory: true,
            },
            PreparedFrozenExecutableLayout {
                package: package.clone(),
                path: logical_tool.clone(),
                entry: FrozenExecutableLayout::Regular {
                    digest: 7,
                    mode: nix::libc::S_IFREG | 0o755,
                },
                is_directory: false,
            },
        ];
        let redirects =
            frozen_executable_directory_redirects(&prepared, Instant::now() + Duration::from_secs(1)).unwrap();
        assert_eq!(redirects.get(&source), Some(&target));

        let layouts = BTreeMap::from([(
            logical_tool.clone(),
            FrozenExecutableLayout::Regular {
                digest: 7,
                mode: nix::libc::S_IFREG | 0o755,
            },
        )]);
        let binding = FrozenExecutableBinding {
            package: package.clone(),
            path: logical_tool.clone(),
        };
        let provider_layouts = BTreeMap::from([(package.clone(), layouts)]);
        let path_providers = BTreeMap::from([(logical_tool.clone(), BTreeSet::from([package]))]);
        assert!(matches!(
            resolve_frozen_executable_layout(
                &binding,
                &provider_layouts,
                &path_providers,
                &redirects,
                Instant::now() + Duration::from_secs(1),
            ),
            Err(Error::FrozenExecutableDirectoryRedirect {
                path,
                redirect_source,
                target: actual_target,
            }) if path == logical_tool
                && redirect_source.as_path() == source
                && actual_target.as_path() == target
        ));
    }

    #[test]
    fn frozen_executable_symlink_targets_are_resolved_lexically_beneath_usr() {
        let link = Path::new("/usr/bin/tool");
        assert_eq!(
            resolve_frozen_symlink_target(link, "tool-1"),
            Some(PathBuf::from("/usr/bin/tool-1"))
        );
        assert_eq!(
            resolve_frozen_symlink_target(link, "../libexec/tool-1"),
            Some(PathBuf::from("/usr/libexec/tool-1"))
        );
        assert_eq!(
            resolve_frozen_symlink_target(link, "/usr/libexec/tool-1"),
            Some(PathBuf::from("/usr/libexec/tool-1"))
        );
        assert_eq!(resolve_frozen_symlink_target(link, "../../etc/passwd"), None);
        assert_eq!(resolve_frozen_symlink_target(link, "/etc/passwd"), None);
        assert_eq!(resolve_frozen_symlink_target(link, "tool-1/"), None);
        assert_eq!(resolve_frozen_symlink_target(link, "tool//1"), None);
    }

    #[test]
    fn frozen_executable_symlink_handoff_requires_one_closure_provider() {
        let entry_provider = package::Id::from("entry-provider");
        let target_provider = package::Id::from("target-provider");
        let duplicate_provider = package::Id::from("duplicate-provider");
        let entry = PathBuf::from("/usr/bin/tool");
        let target = PathBuf::from("/usr/bin/tool-real");
        let binding = FrozenExecutableBinding {
            package: entry_provider.clone(),
            path: entry.clone(),
        };
        let provider_layouts = BTreeMap::from([
            (
                entry_provider.clone(),
                BTreeMap::from([(
                    entry.clone(),
                    FrozenExecutableLayout::Symlink {
                        target: "tool-real".to_owned(),
                        mode: nix::libc::S_IFLNK | 0o777,
                    },
                )]),
            ),
            (
                target_provider.clone(),
                BTreeMap::from([(
                    target.clone(),
                    FrozenExecutableLayout::Regular {
                        digest: 7,
                        mode: nix::libc::S_IFREG | 0o755,
                    },
                )]),
            ),
        ]);
        let mut path_providers = BTreeMap::from([
            (entry.clone(), BTreeSet::from([entry_provider.clone()])),
            (target.clone(), BTreeSet::from([target_provider.clone()])),
        ]);
        let redirects = BTreeMap::new();
        let deadline = Instant::now() + Duration::from_secs(1);

        let expected =
            resolve_frozen_executable_layout(&binding, &provider_layouts, &path_providers, &redirects, deadline)
                .unwrap();
        assert_eq!(expected.resolved_path, target);
        assert_eq!(expected.symlinks.len(), 1);
        assert_eq!(expected.symlinks[0].package, entry_provider);

        path_providers.remove(&target);
        assert!(matches!(
            resolve_frozen_executable_layout(
                &binding,
                &provider_layouts,
                &path_providers,
                &redirects,
                deadline,
            ),
            Err(Error::MissingFrozenExecutableSymlinkTarget { package, binding: path, target: missing })
                if package == binding.package && path == binding.path && missing == target
        ));

        path_providers.insert(
            target.clone(),
            BTreeSet::from([target_provider.clone(), duplicate_provider.clone()]),
        );
        assert!(matches!(
            resolve_frozen_executable_layout(
                &binding,
                &provider_layouts,
                &path_providers,
                &redirects,
                deadline,
            ),
            Err(Error::AmbiguousFrozenExecutableSymlinkTarget {
                package,
                binding: path,
                target: ambiguous,
                providers,
            }) if package == binding.package
                && path == binding.path
                && ambiguous == target
                && providers == vec![duplicate_provider, target_provider]
        ));
    }

    #[test]
    fn frozen_executable_symlink_chain_accepts_n_and_rejects_n_plus_one() {
        let binding = FrozenExecutableBinding {
            package: package::Id::from("symlink-chain-provider"),
            path: PathBuf::from("/usr/bin/link-0"),
        };
        let mut layouts = BTreeMap::new();
        for index in 0..MAX_FROZEN_EXECUTABLE_SYMLINKS {
            layouts.insert(
                PathBuf::from(format!("/usr/bin/link-{index}")),
                FrozenExecutableLayout::Symlink {
                    target: format!("link-{}", index + 1),
                    mode: nix::libc::S_IFLNK | 0o777,
                },
            );
        }
        let final_path = PathBuf::from(format!("/usr/bin/link-{MAX_FROZEN_EXECUTABLE_SYMLINKS}"));
        layouts.insert(
            final_path.clone(),
            FrozenExecutableLayout::Regular {
                digest: 7,
                mode: nix::libc::S_IFREG | 0o755,
            },
        );
        let redirects = BTreeMap::new();
        let deadline = Instant::now() + Duration::from_secs(1);
        let provider_layouts = BTreeMap::from([(binding.package.clone(), layouts.clone())]);
        let path_providers = layouts
            .keys()
            .cloned()
            .map(|path| (path, BTreeSet::from([binding.package.clone()])))
            .collect();
        let expected =
            resolve_frozen_executable_layout(&binding, &provider_layouts, &path_providers, &redirects, deadline)
                .unwrap();
        assert_eq!(expected.symlinks.len(), MAX_FROZEN_EXECUTABLE_SYMLINKS);
        assert_eq!(expected.resolved_path, final_path);

        layouts.insert(
            final_path,
            FrozenExecutableLayout::Symlink {
                target: format!("link-{}", MAX_FROZEN_EXECUTABLE_SYMLINKS + 1),
                mode: nix::libc::S_IFLNK | 0o777,
            },
        );
        layouts.insert(
            PathBuf::from(format!("/usr/bin/link-{}", MAX_FROZEN_EXECUTABLE_SYMLINKS + 1)),
            FrozenExecutableLayout::Regular {
                digest: 7,
                mode: nix::libc::S_IFREG | 0o755,
            },
        );
        let provider_layouts = BTreeMap::from([(binding.package.clone(), layouts.clone())]);
        let path_providers = layouts
            .keys()
            .cloned()
            .map(|path| (path, BTreeSet::from([binding.package.clone()])))
            .collect();
        assert!(matches!(
            resolve_frozen_executable_layout(
                &binding,
                &provider_layouts,
                &path_providers,
                &redirects,
                deadline,
            ),
            Err(Error::FrozenExecutableSymlinkLimit { package, path, limit })
                if package == binding.package
                    && path == binding.path
                    && limit == MAX_FROZEN_EXECUTABLE_SYMLINKS
        ));
    }

    #[test]
    fn frozen_script_interpreters_are_closure_owned_confined_and_race_checked() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let frozen_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&frozen_root).unwrap();

        let installation = frozen_test_installation(&installation_root);
        let client = Client::frozen(
            "frozen-shebang-test",
            installation,
            repository::Map::default(),
            &frozen_root,
        )
        .unwrap();
        let script_package = package::Id::from("script-package");
        let interpreter_package = package::Id::from("interpreter-package");
        let loader_package = package::Id::from("loader-package");
        let packages = [
            script_package.clone(),
            interpreter_package.clone(),
            loader_package.clone(),
        ];
        let script_bytes = b"#!/bin/interpreter\nexit 0\n";
        let native_bytes = test_elf(Some("/lib64/ld-frozen.so"), 2);
        let loader_bytes = test_elf(None, 1);
        let script_digest = xxhash_rust::xxh3::xxh3_128(script_bytes);
        let native_digest = xxhash_rust::xxh3::xxh3_128(&native_bytes);
        let loader_digest = xxhash_rust::xxh3::xxh3_128(&loader_bytes);
        let directory = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFDIR | 0o755,
            tag: 0,
            file: StonePayloadLayoutFile::Directory("bin".into()),
        };
        let lib_directory = StonePayloadLayoutRecord {
            file: StonePayloadLayoutFile::Directory("lib".into()),
            ..directory.clone()
        };
        let script_layout = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o755,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(script_digest, "bin/script".into()),
        };
        let native_layout = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o755,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(native_digest, "bin/interpreter".into()),
        };
        let loader_layout = StonePayloadLayoutRecord {
            file: StonePayloadLayoutFile::Regular(loader_digest, "lib/ld-frozen.so".into()),
            ..native_layout.clone()
        };
        client
            .layout_db
            .batch_add([
                (&script_package, &directory),
                // Frozen materialization collapses byte-identical duplicate
                // directory rows; executable verification must do the same.
                (&script_package, &directory),
                (&script_package, &script_layout),
                (&interpreter_package, &directory),
                (&interpreter_package, &native_layout),
                (&loader_package, &lib_directory),
                (&loader_package, &loader_layout),
            ])
            .unwrap();
        for (digest, bytes) in [
            (script_digest, script_bytes.as_slice()),
            (native_digest, native_bytes.as_slice()),
            (loader_digest, loader_bytes.as_slice()),
        ] {
            let path = cache::asset_path(&client.installation, &format!("{digest:02x}"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, 1_700_000_123).unwrap();

        let binding = FrozenExecutableBinding {
            package: script_package.clone(),
            path: PathBuf::from("/usr/bin/script"),
        };
        let _guard = client
            .require_frozen_executables(&packages, std::slice::from_ref(&binding))
            .unwrap();

        let bin_alias = frozen_root.join("bin");
        fs::remove_file(&bin_alias).unwrap();
        symlink("usr/sbin", &bin_alias).unwrap();
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&binding)),
            Err(Error::FrozenInterpreterRootAliasTarget { path, expected, actual })
                if path == Path::new("/bin")
                    && expected == "usr/bin"
                    && actual == "usr/sbin"
        ));
        fs::remove_file(&bin_alias).unwrap();
        symlink("usr/bin", &bin_alias).unwrap();

        let lib64_alias = frozen_root.join("lib64");
        fs::remove_file(&lib64_alias).unwrap();
        symlink("usr/lib32", &lib64_alias).unwrap();
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&binding)),
            Err(Error::FrozenInterpreterRootAliasTarget { path, expected, actual })
                if path == Path::new("/lib64")
                    && expected == "usr/lib"
                    && actual == "usr/lib32"
        ));
        fs::remove_file(&lib64_alias).unwrap();
        symlink("usr/lib", &lib64_alias).unwrap();

        let interpreted_loader_bytes = b"#!/usr/bin/interpreter\n";
        let interpreted_loader_digest = xxhash_rust::xxh3::xxh3_128(interpreted_loader_bytes);
        let interpreted_loader_layout = StonePayloadLayoutRecord {
            file: StonePayloadLayoutFile::Regular(interpreted_loader_digest, "lib/ld-frozen.so".into()),
            ..loader_layout.clone()
        };
        client
            .layout_db
            .batch_add([
                (&loader_package, &lib_directory),
                (&loader_package, &interpreted_loader_layout),
            ])
            .unwrap();
        let interpreted_loader_asset =
            cache::asset_path(&client.installation, &format!("{interpreted_loader_digest:02x}"));
        fs::create_dir_all(interpreted_loader_asset.parent().unwrap()).unwrap();
        fs::write(interpreted_loader_asset, interpreted_loader_bytes).unwrap();
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, 1_700_000_123).unwrap();
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&binding)),
            Err(Error::FrozenElfInterpreterIsInterpreted { package, path })
                if package == loader_package && path == Path::new("/usr/lib/ld-frozen.so")
        ));
        client
            .layout_db
            .batch_add([(&loader_package, &lib_directory), (&loader_package, &loader_layout)])
            .unwrap();
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, 1_700_000_123).unwrap();

        // A file which happens to exist in the root is not an interpreter
        // provider when its package is absent from the exact frozen closure.
        assert!(matches!(
            client.require_frozen_executables(std::slice::from_ref(&script_package), std::slice::from_ref(&binding)),
            Err(Error::MissingFrozenInterpreterProvider { path })
                if path == Path::new("/usr/bin/interpreter")
        ));

        let interpreter_path = frozen_root.join("usr/bin/interpreter");
        fs::remove_file(&interpreter_path).unwrap();
        symlink("interpreter-escape", &interpreter_path).unwrap();
        fs::write(frozen_root.join("usr/bin/interpreter-escape"), &native_bytes).unwrap();
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&binding)),
            Err(Error::OpenFrozenExecutable { package, path, .. })
                if package == interpreter_package && path == Path::new("/usr/bin/interpreter")
        ));
        fs::remove_file(frozen_root.join("usr/bin/interpreter-escape")).unwrap();
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, 1_700_000_123).unwrap();

        let moved_root = temporary.path().join("moved-frozen-root");
        let mut root_raced = false;
        let error = require_frozen_executables(
            &client,
            test_materialized_frozen_root(&frozen_root).unwrap(),
            &packages,
            std::slice::from_ref(&binding),
            |checked, checkpoint| {
                if checked == &binding && checkpoint == FrozenExecutableCheckpoint::AfterOpen && !root_raced {
                    fs::rename(&frozen_root, &moved_root).unwrap();
                    fs::create_dir(&frozen_root).unwrap();
                    root_raced = true;
                }
            },
        )
        .unwrap_err();
        assert!(root_raced);
        assert!(matches!(
            error,
            Error::FrozenExecutableRootReplaced(path) if path == frozen_root
        ));
        fs::remove_dir(&frozen_root).unwrap();
        fs::rename(&moved_root, &frozen_root).unwrap();

        let script_path = frozen_root.join("usr/bin/script");
        let mut raced = false;
        let error = require_frozen_executables(
            &client,
            test_materialized_frozen_root(&frozen_root).unwrap(),
            &packages,
            std::slice::from_ref(&binding),
            |checked, checkpoint| {
                if checked.package == interpreter_package
                    && checked.path == Path::new("/usr/bin/interpreter")
                    && checkpoint == FrozenExecutableCheckpoint::AfterOpen
                    && !raced
                {
                    // The script itself was already accepted. Mutating it
                    // while its interpreter is inspected must be caught by
                    // the retained-graph revalidation before return.
                    fs::write(&script_path, b"#!/bin/interpreter\nexit 1\n").unwrap();
                    raced = true;
                }
            },
        )
        .unwrap_err();
        assert!(raced);
        assert!(matches!(
            error,
            Error::FrozenExecutablePathReplaced { package, path }
                if package == script_package && path == Path::new("/usr/bin/script")
        ));

        let cycle_bytes = b"#!/usr/bin/script\n";
        let cycle_digest = xxhash_rust::xxh3::xxh3_128(cycle_bytes);
        let cycle_layout = StonePayloadLayoutRecord {
            file: StonePayloadLayoutFile::Regular(cycle_digest, "bin/interpreter".into()),
            ..native_layout
        };
        client
            .layout_db
            .batch_add([
                (&interpreter_package, &directory),
                (&interpreter_package, &cycle_layout),
            ])
            .unwrap();
        let cycle_asset = cache::asset_path(&client.installation, &format!("{cycle_digest:02x}"));
        fs::create_dir_all(cycle_asset.parent().unwrap()).unwrap();
        fs::write(cycle_asset, cycle_bytes).unwrap();
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, 1_700_000_123).unwrap();
        assert!(matches!(
            client.require_frozen_executables(&packages, &[binding]),
            Err(Error::FrozenExecutableInterpreterCycle { package, path })
                if package == script_package && path == Path::new("/usr/bin/script")
        ));
    }

    #[test]
    fn frozen_script_chain_accepts_n_and_rejects_n_plus_one_end_to_end() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let frozen_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir_all(frozen_root.join("usr/bin")).unwrap();

        let installation = frozen_test_installation(&installation_root);
        let client = Client::frozen(
            "frozen-shebang-depth-test",
            installation,
            repository::Map::default(),
            &frozen_root,
        )
        .unwrap();
        let package = package::Id::from("script-chain-package");
        let binding = FrozenExecutableBinding {
            package: package.clone(),
            path: PathBuf::from("/usr/bin/chain-0"),
        };

        let install_chain = |interpreter_count: usize| {
            let mut layouts = vec![StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFDIR | 0o755,
                tag: 0,
                file: StonePayloadLayoutFile::Directory("bin".into()),
            }];
            for index in 0..=interpreter_count {
                let bytes = if index == interpreter_count {
                    test_elf(None, 1)
                } else {
                    format!("#!/usr/bin/chain-{}\n", index + 1).into_bytes()
                };
                let digest = xxhash_rust::xxh3::xxh3_128(&bytes);
                layouts.push(StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFREG | 0o755,
                    tag: 0,
                    file: StonePayloadLayoutFile::Regular(digest, format!("bin/chain-{index}").into()),
                });
                let path = frozen_root.join(format!("usr/bin/chain-{index}"));
                fs::write(&path, bytes).unwrap();
                fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
            }
            client
                .layout_db
                .batch_add(layouts.iter().map(|layout| (&package, layout)))
                .unwrap();
        };

        install_chain(MAX_FROZEN_SHEBANG_INTERPRETERS);
        let _guard = client
            .require_frozen_executables(std::slice::from_ref(&package), std::slice::from_ref(&binding))
            .unwrap();

        install_chain(MAX_FROZEN_SHEBANG_INTERPRETERS + 1);
        assert!(matches!(
            client.require_frozen_executables(
                std::slice::from_ref(&package),
                std::slice::from_ref(&binding),
            ),
            Err(Error::FrozenShebangInterpreterLimit { package, path, limit })
                if package == binding.package
                    && path == binding.path
                    && limit == MAX_FROZEN_SHEBANG_INTERPRETERS
        ));
    }

    fn frozen_normalization_test_root(path: &Path) -> fs::File {
        openat2_frozen(
            AT_FDCWD,
            path,
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            (nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
        )
        .unwrap()
    }

    fn frozen_normalization_test_tree(
        entries: impl IntoIterator<Item = (PathBuf, FrozenExpectedEntry)>,
        limits: FrozenNormalizationLimits,
    ) -> Result<FrozenExpectedTree, Error> {
        let mut expected = BTreeMap::from([(
            PathBuf::from("/"),
            FrozenExpectedEntry {
                kind: FrozenExpectedKind::Directory,
                mode: 0o755,
            },
        )]);
        for (path, entry) in entries {
            assert!(expected.insert(path, entry).is_none());
        }
        FrozenExpectedTree::from_entries(expected, limits)
    }

    fn frozen_expected_directory(mode: u32) -> FrozenExpectedEntry {
        FrozenExpectedEntry {
            kind: FrozenExpectedKind::Directory,
            mode,
        }
    }

    fn frozen_expected_regular(mode: u32) -> FrozenExpectedEntry {
        FrozenExpectedEntry {
            kind: FrozenExpectedKind::Regular { digest: 0 },
            mode,
        }
    }

    fn frozen_expected_regular_bytes(mode: u32, bytes: &[u8]) -> FrozenExpectedEntry {
        FrozenExpectedEntry {
            kind: FrozenExpectedKind::Regular {
                digest: xxhash_rust::xxh3::xxh3_128(bytes),
            },
            mode,
        }
    }

    fn frozen_expected_symlink(mode: u32, target: &[u8]) -> FrozenExpectedEntry {
        FrozenExpectedEntry {
            kind: FrozenExpectedKind::Symlink {
                target: target.to_vec(),
            },
            mode,
        }
    }

    fn install_test_posix_acl(path: &Path, name: &CStr) -> bool {
        const ACL_UNDEFINED_ID: u32 = u32::MAX;
        // One named-user entry makes the ACL non-minimal so the kernel cannot
        // collapse it into ordinary mode bits.
        // SAFETY: geteuid takes no arguments and cannot fail.
        let named_user = unsafe { nix::libc::geteuid() };
        let entries = [
            (0x01_u16, 0o7_u16, ACL_UNDEFINED_ID),
            (0x02, 0o4, named_user),
            (0x04, 0o5, ACL_UNDEFINED_ID),
            (0x10, 0o5, ACL_UNDEFINED_ID),
            (0x20, 0o5, ACL_UNDEFINED_ID),
        ];
        let mut value = Vec::with_capacity(4 + entries.len() * 8);
        value.extend_from_slice(&2_u32.to_le_bytes());
        for (tag, permissions, id) in entries {
            value.extend_from_slice(&tag.to_le_bytes());
            value.extend_from_slice(&permissions.to_le_bytes());
            value.extend_from_slice(&id.to_le_bytes());
        }
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: both C strings and the complete xattr value remain live for
        // the call. The fixtures are private same-owner regular directories.
        if unsafe { nix::libc::setxattr(path.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0) } == 0 {
            return true;
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(nix::libc::EOPNOTSUPP) | Some(nix::libc::EPERM)
        ) {
            if std::env::var_os("CAST_REQUIRE_POSIX_ACL_TESTS").is_some() {
                panic!(
                    "required POSIX ACL fixture is unavailable for {}: {error}",
                    path.to_string_lossy()
                );
            }
            eprintln!("skipping POSIX ACL assertion for {}: {error}", path.to_string_lossy());
            false
        } else {
            panic!("install test POSIX ACL: {error}");
        }
    }

    #[test]
    fn frozen_normalization_handles_mode_zero_entries_and_never_follows_symlinks() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root_path).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(root_path.join("locked")).unwrap();
        fs::write(root_path.join("locked/file"), b"mode zero").unwrap();
        fs::set_permissions(root_path.join("locked/file"), Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(root_path.join("locked"), Permissions::from_mode(0o000)).unwrap();
        fs::write(&outside, b"external sentinel").unwrap();
        filetime::set_file_times(
            &outside,
            FileTime::from_unix_time(444, 0),
            FileTime::from_unix_time(444, 0),
        )
        .unwrap();
        symlink(&outside, root_path.join("link")).unwrap();
        let target = outside.as_os_str().as_bytes();
        let expected = frozen_normalization_test_tree(
            [
                (PathBuf::from("/link"), frozen_expected_symlink(0o777, target)),
                (PathBuf::from("/locked"), frozen_expected_directory(0o000)),
                (
                    PathBuf::from("/locked/file"),
                    frozen_expected_regular_bytes(0o000, b"mode zero"),
                ),
            ],
            FrozenNormalizationLimits { inodes: 4, depth: 2 },
        )
        .unwrap();
        let root = frozen_normalization_test_root(&root_path);
        let timestamp = FileTime::from_unix_time(123, 456);

        normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            timestamp,
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 4, depth: 2 },
            |_, _| {},
        )
        .unwrap();

        for path in [root_path.clone(), root_path.join("locked"), root_path.join("link")] {
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert_eq!((metadata.atime(), metadata.atime_nsec()), (123, 456), "{path:?}");
            assert_eq!((metadata.mtime(), metadata.mtime_nsec()), (123, 456), "{path:?}");
        }
        assert_eq!(
            fs::symlink_metadata(root_path.join("locked")).unwrap().mode() & 0o7777,
            0
        );
        fs::set_permissions(root_path.join("locked"), Permissions::from_mode(0o700)).unwrap();
        let file_metadata = fs::symlink_metadata(root_path.join("locked/file")).unwrap();
        assert_eq!(file_metadata.mode() & 0o7777, 0);
        assert_eq!((file_metadata.atime(), file_metadata.atime_nsec()), (123, 456));
        assert_eq!((file_metadata.mtime(), file_metadata.mtime_nsec()), (123, 456));
        let outside_metadata = fs::symlink_metadata(&outside).unwrap();
        assert_eq!((outside_metadata.atime(), outside_metadata.mtime()), (444, 444));
        assert_eq!(fs::read(&outside).unwrap(), b"external sentinel");
        fs::set_permissions(root_path.join("locked/file"), Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn frozen_normalization_rejects_unplanned_missing_and_extra_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        fs::create_dir(&root_path).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        let unexpected = root_path.join("unexpected");
        fs::write(&unexpected, b"must not be normalized").unwrap();
        filetime::set_file_times(
            &unexpected,
            FileTime::from_unix_time(333, 0),
            FileTime::from_unix_time(333, 0),
        )
        .unwrap();
        let expected = frozen_normalization_test_tree([], FrozenNormalizationLimits { inodes: 2, depth: 1 }).unwrap();
        let root = frozen_normalization_test_root(&root_path);

        assert!(matches!(
            normalize_frozen_tree_with(
                &root,
                &root_path,
                &expected,
                FileTime::from_unix_time(123, 0),
                Instant::now() + Duration::from_secs(10),
                FrozenNormalizationLimits { inodes: 2, depth: 1 },
                |_, _| {},
            ),
            Err(Error::FrozenNormalizationInventoryMismatch {
                reason: "the filesystem contains an undeclared entry",
                ..
            })
        ));
        assert_eq!(fs::symlink_metadata(&unexpected).unwrap().mtime(), 333);

        fs::remove_file(&unexpected).unwrap();
        let expected = frozen_normalization_test_tree(
            [(PathBuf::from("/missing"), frozen_expected_regular(0o600))],
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
        )
        .unwrap();
        assert!(matches!(
            normalize_frozen_tree_with(
                &root,
                &root_path,
                &expected,
                FileTime::from_unix_time(123, 0),
                Instant::now() + Duration::from_secs(10),
                FrozenNormalizationLimits { inodes: 2, depth: 1 },
                |_, _| {},
            ),
            Err(Error::FrozenNormalizationInventoryMismatch {
                reason: "the filesystem is missing a declared entry",
                ..
            })
        ));
    }

    #[test]
    fn frozen_normalization_directory_to_symlink_race_cannot_escape_root() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root_path).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(root_path.join("child")).unwrap();
        fs::set_permissions(root_path.join("child"), Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&outside).unwrap();
        let sentinel = outside.join("sentinel");
        fs::write(&sentinel, b"outside").unwrap();
        fs::set_permissions(&outside, Permissions::from_mode(0o500)).unwrap();
        filetime::set_file_times(
            &sentinel,
            FileTime::from_unix_time(777, 0),
            FileTime::from_unix_time(777, 0),
        )
        .unwrap();
        let outside_before = fs::symlink_metadata(&outside).unwrap();
        let expected = frozen_normalization_test_tree(
            [(PathBuf::from("/child"), frozen_expected_directory(0o700))],
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
        )
        .unwrap();
        let root = frozen_normalization_test_root(&root_path);
        let displaced = root_path.join("displaced");
        let mut raced = false;

        let error = normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
            |checkpoint, path| {
                if checkpoint == FrozenNormalizationCheckpoint::EntryPinned && path == Path::new("/child") && !raced {
                    fs::rename(root_path.join("child"), &displaced).unwrap();
                    symlink(&outside, root_path.join("child")).unwrap();
                    raced = true;
                }
            },
        )
        .unwrap_err();
        assert!(raced);
        assert!(matches!(
            error,
            Error::FrozenNormalizationEntryChanged(_) | Error::OpenFrozenNormalizationEntry { .. }
        ));
        let outside_after = fs::symlink_metadata(&outside).unwrap();
        assert_eq!(outside_after.mode(), outside_before.mode());
        assert_eq!(
            (outside_after.atime(), outside_after.mtime()),
            (outside_before.atime(), outside_before.mtime())
        );
        assert_eq!(fs::symlink_metadata(&sentinel).unwrap().mtime(), 777);
        assert_eq!(fs::read(&sentinel).unwrap(), b"outside");
        fs::set_permissions(&outside, Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn frozen_normalization_hardlink_substitution_is_rejected_before_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root_path).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        fs::write(root_path.join("file"), b"declared").unwrap();
        fs::set_permissions(root_path.join("file"), Permissions::from_mode(0o600)).unwrap();
        fs::write(&outside, b"external sentinel").unwrap();
        fs::set_permissions(&outside, Permissions::from_mode(0o640)).unwrap();
        filetime::set_file_times(
            &outside,
            FileTime::from_unix_time(888, 0),
            FileTime::from_unix_time(888, 0),
        )
        .unwrap();
        let expected = frozen_normalization_test_tree(
            [(PathBuf::from("/file"), frozen_expected_regular(0o600))],
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
        )
        .unwrap();
        let root = frozen_normalization_test_root(&root_path);
        let displaced = root_path.join("displaced");
        let mut raced = false;

        let error = normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
            |checkpoint, path| {
                if checkpoint == FrozenNormalizationCheckpoint::EntryPinned && path == Path::new("/file") && !raced {
                    fs::rename(root_path.join("file"), &displaced).unwrap();
                    fs::hard_link(&outside, root_path.join("file")).unwrap();
                    raced = true;
                }
            },
        )
        .unwrap_err();
        assert!(raced);
        assert!(matches!(error, Error::FrozenNormalizationEntryChanged(_)));
        let outside_metadata = fs::symlink_metadata(&outside).unwrap();
        assert_eq!(outside_metadata.mode() & 0o7777, 0o640);
        assert_eq!((outside_metadata.atime(), outside_metadata.mtime()), (888, 888));
        assert_eq!(fs::read(&outside).unwrap(), b"external sentinel");
    }

    #[test]
    fn frozen_normalization_rejects_stage_root_name_substitution() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        let displaced = temporary.path().join("displaced");
        fs::create_dir(&root_path).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        let expected = frozen_normalization_test_tree([], FrozenNormalizationLimits { inodes: 1, depth: 0 }).unwrap();
        let root = frozen_normalization_test_root(&root_path);
        let mut raced = false;

        let error = normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 1, depth: 0 },
            |checkpoint, path| {
                if checkpoint == FrozenNormalizationCheckpoint::BeforeRootRevalidation && path == Path::new("/") {
                    fs::rename(&root_path, &displaced).unwrap();
                    fs::create_dir(&root_path).unwrap();
                    fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
                    fs::write(root_path.join("replacement"), b"must not publish").unwrap();
                    raced = true;
                }
            },
        )
        .unwrap_err();
        assert!(raced);
        assert!(match error {
            Error::FrozenNormalizationRootChanged(path) => path == root_path,
            Error::FrozenNormalizationEntryChanged(path) => path == Path::new("/"),
            _ => false,
        });
        assert_eq!(fs::read(root_path.join("replacement")).unwrap(), b"must not publish");
        assert_eq!(fs::symlink_metadata(&displaced).unwrap().mtime(), 123);
    }

    #[test]
    fn frozen_normalization_limits_accept_n_and_reject_n_plus_one() {
        let entries = || {
            [
                (PathBuf::from("/a"), frozen_expected_directory(0o755)),
                (PathBuf::from("/a/b"), frozen_expected_regular(0o600)),
            ]
        };
        assert!(frozen_normalization_test_tree(entries(), FrozenNormalizationLimits { inodes: 3, depth: 2 }).is_ok());
        assert!(matches!(
            frozen_normalization_test_tree(entries(), FrozenNormalizationLimits { inodes: 2, depth: 2 }),
            Err(Error::FrozenNormalizationInodeLimit { limit: 2, actual: 3 })
        ));
        assert!(matches!(
            frozen_normalization_test_tree(entries(), FrozenNormalizationLimits { inodes: 3, depth: 1 }),
            Err(Error::FrozenNormalizationDepthLimit { limit: 1, actual: 2 })
        ));
    }

    #[test]
    fn frozen_normalization_runtime_walk_enforces_the_inode_limit() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        fs::create_dir_all(root_path.join("nested")).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(root_path.join("nested"), Permissions::from_mode(0o755)).unwrap();
        fs::write(root_path.join("nested/file"), b"bounded").unwrap();
        fs::set_permissions(root_path.join("nested/file"), Permissions::from_mode(0o600)).unwrap();
        let expected = frozen_normalization_test_tree(
            [
                (PathBuf::from("/nested"), frozen_expected_directory(0o755)),
                (
                    PathBuf::from("/nested/file"),
                    frozen_expected_regular_bytes(0o600, b"bounded"),
                ),
            ],
            FrozenNormalizationLimits { inodes: 3, depth: 2 },
        )
        .unwrap();
        let root = frozen_normalization_test_root(&root_path);

        assert!(matches!(
            normalize_frozen_tree_with(
                &root,
                &root_path,
                &expected,
                FileTime::from_unix_time(123, 0),
                Instant::now() + Duration::from_secs(10),
                FrozenNormalizationLimits { inodes: 2, depth: 2 },
                |_, _| {},
            ),
            Err(Error::FrozenNormalizationInodeLimit { limit: 2, actual: 3 })
        ));
    }

    #[test]
    fn frozen_normalization_rejects_regular_content_outside_the_declaration() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        fs::create_dir(&root_path).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        let file = root_path.join("file");
        fs::write(&file, b"tampered").unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();
        filetime::set_file_times(
            &file,
            FileTime::from_unix_time(999, 0),
            FileTime::from_unix_time(999, 0),
        )
        .unwrap();
        let expected = frozen_normalization_test_tree(
            [(
                PathBuf::from("/file"),
                frozen_expected_regular_bytes(0o600, b"declared"),
            )],
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
        )
        .unwrap();
        let root = frozen_normalization_test_root(&root_path);

        assert!(matches!(
            normalize_frozen_tree_with(
                &root,
                &root_path,
                &expected,
                FileTime::from_unix_time(123, 0),
                Instant::now() + Duration::from_secs(10),
                FrozenNormalizationLimits { inodes: 2, depth: 1 },
                |_, _| {},
            ),
            Err(Error::FrozenNormalizationInventoryMismatch {
                reason: "the regular file content digest differs from its declaration",
                ..
            })
        ));
        assert_eq!(fs::symlink_metadata(&file).unwrap().mode() & 0o7777, 0o600);
        assert_eq!(fs::read(&file).unwrap(), b"tampered");
    }

    #[test]
    fn frozen_normalization_detects_same_inode_mutation_before_final_revalidation() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        fs::create_dir(&root_path).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        let file = root_path.join("file");
        fs::write(&file, b"original").unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();
        let expected = frozen_normalization_test_tree(
            [(
                PathBuf::from("/file"),
                frozen_expected_regular_bytes(0o600, b"original"),
            )],
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
        )
        .unwrap();
        let root = frozen_normalization_test_root(&root_path);
        let mut raced = false;

        let error = normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
            |checkpoint, path| {
                if checkpoint == FrozenNormalizationCheckpoint::AfterRegularDigest
                    && path == Path::new("/file")
                    && !raced
                {
                    fs::write(&file, b"mutated!").unwrap();
                    raced = true;
                }
            },
        )
        .unwrap_err();
        assert!(raced);
        assert!(matches!(error, Error::FrozenNormalizationEntryChanged(path) if path == Path::new("/file")));
        assert_eq!(fs::read(&file).unwrap(), b"mutated!");
    }

    #[test]
    fn frozen_normalization_final_pass_detects_deep_content_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        fs::create_dir_all(root_path.join("nested")).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(root_path.join("nested"), Permissions::from_mode(0o755)).unwrap();
        let file = root_path.join("nested/file");
        fs::write(&file, b"original").unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();
        let expected = frozen_normalization_test_tree(
            [
                (PathBuf::from("/nested"), frozen_expected_directory(0o755)),
                (
                    PathBuf::from("/nested/file"),
                    frozen_expected_regular_bytes(0o600, b"original"),
                ),
            ],
            FrozenNormalizationLimits { inodes: 3, depth: 2 },
        )
        .unwrap();
        let root = frozen_normalization_test_root(&root_path);
        let mut raced = false;

        let error = normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 3, depth: 2 },
            |checkpoint, path| {
                if checkpoint == FrozenNormalizationCheckpoint::BeforeFinalTreeConfirmation && path == Path::new("/") {
                    fs::write(&file, b"mutated!").unwrap();
                    raced = true;
                }
            },
        )
        .unwrap_err();
        assert!(raced);
        assert!(matches!(
            error,
            Error::FrozenNormalizationInventoryMismatch {
                reason: "the regular file content digest differs from its declaration",
                ..
            }
        ));
    }

    #[test]
    fn frozen_normalization_root_inventory_detects_post_digest_child_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        fs::create_dir(&root_path).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        let file = root_path.join("file");
        fs::write(&file, b"original").unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();
        let expected = frozen_normalization_test_tree(
            [(
                PathBuf::from("/file"),
                frozen_expected_regular_bytes(0o600, b"original"),
            )],
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
        )
        .unwrap();
        let root = frozen_normalization_test_root(&root_path);
        let mut raced = false;

        let error = normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
            |checkpoint, path| {
                if checkpoint == FrozenNormalizationCheckpoint::BeforeRootRevalidation && path == Path::new("/") {
                    fs::write(&file, b"mutated!").unwrap();
                    raced = true;
                }
            },
        )
        .unwrap_err();
        assert!(raced);
        assert!(matches!(error, Error::FrozenNormalizationEntryChanged(path) if path == Path::new("/file")));
    }

    #[test]
    fn frozen_normalization_detects_entry_added_after_final_inventory() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        fs::create_dir(&root_path).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        let expected = frozen_normalization_test_tree([], FrozenNormalizationLimits { inodes: 1, depth: 0 }).unwrap();
        let root = frozen_normalization_test_root(&root_path);
        let mut raced = false;

        let error = normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 1, depth: 0 },
            |checkpoint, path| {
                if checkpoint == FrozenNormalizationCheckpoint::AfterDirectoryFinalInventory && path == Path::new("/") {
                    fs::write(root_path.join("late"), b"must not publish").unwrap();
                    raced = true;
                }
            },
        )
        .unwrap_err();
        assert!(raced);
        assert!(matches!(error, Error::FrozenNormalizationEntryChanged(path) if path == Path::new("/")));
        assert_eq!(fs::read(root_path.join("late")).unwrap(), b"must not publish");
    }

    #[test]
    fn frozen_normalization_orders_non_utf8_names_as_raw_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        let root_path = temporary.path().join("root");
        fs::create_dir(&root_path).unwrap();
        fs::set_permissions(&root_path, Permissions::from_mode(0o755)).unwrap();
        let name = OsString::from_vec(vec![b'n', 0xff]);
        let file = root_path.join(&name);
        fs::write(&file, b"raw name").unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();
        let expected_path = Path::new("/").join(&name);
        let expected = frozen_normalization_test_tree(
            [(expected_path, frozen_expected_regular_bytes(0o600, b"raw name"))],
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
        )
        .unwrap();
        let root = frozen_normalization_test_root(&root_path);

        normalize_frozen_tree_with(
            &root,
            &root_path,
            &expected,
            FileTime::from_unix_time(123, 0),
            Instant::now() + Duration::from_secs(10),
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
            |_, _| {},
        )
        .unwrap();

        assert_eq!(fs::symlink_metadata(&file).unwrap().mtime(), 123);
    }

    #[test]
    fn frozen_normalization_rejects_cross_mount_entries_before_mutation() {
        let root = fs::File::open("/").unwrap();
        for name in [c"proc", c"sys", c"dev"] {
            match open_frozen_normalization_entry(
                &root,
                name,
                Path::new("/").join(OsStr::from_bytes(name.to_bytes())).as_path(),
                FrozenNormalizationOpen::Anchor,
                Instant::now() + Duration::from_secs(10),
            ) {
                Err(Error::OpenFrozenNormalizationEntry { source, .. })
                    if source.raw_os_error() == Some(nix::libc::EXDEV) =>
                {
                    return;
                }
                Ok(_) => {}
                Err(error) => panic!("unexpected cross-mount probe failure: {error}"),
            }
        }
        panic!("expected /proc, /sys, or /dev to reside on another mount");
    }

    #[test]
    fn frozen_normalization_rejects_access_acl_after_active_mode_change() {
        let access = tempfile::tempdir().unwrap();
        let access_root = access.path().join("root");
        fs::create_dir(&access_root).unwrap();
        fs::set_permissions(&access_root, Permissions::from_mode(0o755)).unwrap();
        let file = access_root.join("file");
        fs::write(&file, b"acl protected").unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o640)).unwrap();
        if !install_test_posix_acl(&file, c"system.posix_acl_access") {
            return;
        }
        // Preserve the non-minimal ACL while forcing phase one to add owner
        // read permission through the retained descriptor.
        fs::set_permissions(&file, Permissions::from_mode(0o000)).unwrap();
        let file_mode = fs::symlink_metadata(&file).unwrap().mode() & 0o7777;
        assert_eq!(file_mode, 0);
        let expected = frozen_normalization_test_tree(
            [(
                PathBuf::from("/file"),
                frozen_expected_regular_bytes(file_mode, b"acl protected"),
            )],
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
        )
        .unwrap();
        let root = frozen_normalization_test_root(&access_root);
        assert!(matches!(
            normalize_frozen_tree_with(
                &root,
                &access_root,
                &expected,
                FileTime::from_unix_time(123, 0),
                Instant::now() + Duration::from_secs(10),
                FrozenNormalizationLimits { inodes: 2, depth: 1 },
                |_, _| {},
            ),
            Err(Error::FrozenNormalizationAcl { path, .. }) if path == Path::new("/file")
        ));
    }

    #[test]
    fn frozen_normalization_rejects_default_acl_after_active_mode_change() {
        let default = tempfile::tempdir().unwrap();
        let default_root = default.path().join("root");
        let directory = default_root.join("directory");
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&default_root, Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&directory, Permissions::from_mode(0o755)).unwrap();
        if !install_test_posix_acl(&directory, c"system.posix_acl_default") {
            return;
        }
        // Force phase one to add traversal permission; a default ACL must
        // remain visible and rejected after that descriptor mutation.
        fs::set_permissions(&directory, Permissions::from_mode(0o000)).unwrap();
        let directory_mode = fs::symlink_metadata(&directory).unwrap().mode() & 0o7777;
        assert_eq!(directory_mode, 0);
        let expected = frozen_normalization_test_tree(
            [(PathBuf::from("/directory"), frozen_expected_directory(directory_mode))],
            FrozenNormalizationLimits { inodes: 2, depth: 1 },
        )
        .unwrap();
        let root = frozen_normalization_test_root(&default_root);
        assert!(matches!(
            normalize_frozen_tree_with(
                &root,
                &default_root,
                &expected,
                FileTime::from_unix_time(123, 0),
                Instant::now() + Duration::from_secs(10),
                FrozenNormalizationLimits { inodes: 2, depth: 1 },
                |_, _| {},
            ),
            Err(Error::FrozenNormalizationAcl { path, .. }) if path == Path::new("/directory")
        ));
    }

    #[test]
    fn frozen_root_normalizes_enforceable_metadata_in_canonical_order() {
        const CHILD: &str = "CAST_FROZEN_ROOT_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            run_frozen_root_materialization_test();
            return;
        }

        // umask is process-global. Run the hostile-umask proof in a dedicated
        // test process so unrelated parallel tests cannot observe it.
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("frozen_root_normalizes_enforceable_metadata_in_canonical_order")
            .arg("--test-threads=1")
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn run_frozen_root_materialization_test() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let blit_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&blit_root).unwrap();

        let installation = frozen_test_installation(&installation_root);
        let client = Client::frozen("frozen-root-test", installation, repository::Map::default(), &blit_root).unwrap();
        let isolation_marker = client.installation.isolation_dir().join("must-remain");
        fs::create_dir_all(isolation_marker.parent().unwrap()).unwrap();
        fs::write(&isolation_marker, b"isolation root is out of scope").unwrap();

        let first = package::Id::from("a-frozen-package");
        let second = package::Id::from("z-frozen-package");
        let omitted = package::Id::from("zz-omitted-package");
        let asset_bytes = test_elf(None, 1);
        let mut adversarial_asset_bytes = asset_bytes.clone();
        let last = adversarial_asset_bytes.last_mut().unwrap();
        *last ^= 1;
        let asset_id = xxhash_rust::xxh3::xxh3_128(&asset_bytes);
        let empty_id = 0x99aa_06d3_0147_98d8_6001_c324_468d_497f_u128;
        let layouts = [
            (
                &first,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFDIR | 0o751,
                    tag: 0,
                    file: StonePayloadLayoutFile::Directory("bin".into()),
                },
            ),
            (
                &first,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFREG | 0o755,
                    tag: 0,
                    file: StonePayloadLayoutFile::Regular(asset_id, "bin/tool".into()),
                },
            ),
            (
                &first,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFLNK | 0o777,
                    tag: 0,
                    file: StonePayloadLayoutFile::Symlink("tool".into(), "bin/tool-link".into()),
                },
            ),
            (
                &first,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFLNK | 0o777,
                    tag: 0,
                    file: StonePayloadLayoutFile::Symlink("other-tool".into(), "bin/cross-tool".into()),
                },
            ),
            (
                &first,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFLNK | 0o777,
                    tag: 0,
                    file: StonePayloadLayoutFile::Symlink("cycle-b".into(), "bin/cycle-a".into()),
                },
            ),
            (
                &first,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFLNK | 0o777,
                    tag: 0,
                    file: StonePayloadLayoutFile::Symlink("cycle-a".into(), "bin/cycle-b".into()),
                },
            ),
            (
                &first,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFREG | 0o640,
                    tag: 0,
                    file: StonePayloadLayoutFile::Regular(empty_id, "share/empty".into()),
                },
            ),
            (
                &first,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFDIR | 0o555,
                    tag: 0,
                    file: StonePayloadLayoutFile::Directory("share/restricted".into()),
                },
            ),
            (
                &first,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFREG | 0o644,
                    tag: 0,
                    file: StonePayloadLayoutFile::Regular(asset_id, "share/restricted/tool".into()),
                },
            ),
            // Identical directory records may be shared by packages.
            (
                &second,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFDIR | 0o751,
                    tag: 0,
                    file: StonePayloadLayoutFile::Directory("bin".into()),
                },
            ),
            (
                &second,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFREG | 0o755,
                    tag: 0,
                    file: StonePayloadLayoutFile::Regular(asset_id, "bin/other-tool".into()),
                },
            ),
            (
                &omitted,
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFREG | 0o600,
                    tag: 0,
                    file: StonePayloadLayoutFile::Regular(asset_id, "share/omitted".into()),
                },
            ),
        ];
        client
            .layout_db
            .batch_add(layouts.iter().map(|(package, layout)| (*package, layout)))
            .unwrap();

        let asset_path = cache::asset_path(&client.installation, &format!("{asset_id:02x}"));
        fs::create_dir_all(asset_path.parent().unwrap()).unwrap();
        fs::write(&asset_path, &asset_bytes).unwrap();
        fs::set_permissions(&asset_path, Permissions::from_mode(0o640)).unwrap();
        let asset_metadata = fs::metadata(&asset_path).unwrap();

        nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
        const EPOCH: i64 = 1_700_000_123;
        client.discard_frozen_root().unwrap();
        let _materialized = client
            .blit_frozen_root(&[second.clone(), first.clone()], EPOCH)
            .unwrap();

        let tool = blit_root.join("usr/bin/tool");
        let empty = blit_root.join("usr/share/empty");
        let tool_link = blit_root.join("usr/bin/tool-link");
        let timestamped = [
            blit_root.clone(),
            blit_root.join("usr"),
            blit_root.join("usr/bin"),
            blit_root.join("usr/share"),
            blit_root.join("usr/share/restricted"),
            blit_root.join("usr/share/restricted/tool"),
            tool.clone(),
            empty.clone(),
            tool_link.clone(),
            blit_root.join("bin"),
            blit_root.join("sbin"),
            blit_root.join("lib"),
            blit_root.join("lib64"),
            blit_root.join("lib32"),
        ];
        for path in &timestamped {
            let metadata = fs::symlink_metadata(path).unwrap();
            assert_eq!(
                FileTime::from_last_access_time(&metadata).unix_seconds(),
                EPOCH,
                "{path:?}"
            );
            assert_eq!(
                FileTime::from_last_modification_time(&metadata).unix_seconds(),
                EPOCH,
                "{path:?}"
            );
        }
        // This manifest intentionally covers only metadata the materializer
        // can enforce: path, inode type, mode, bytes/link target, atime and
        // mtime. Kernel-assigned inode/dev/ctime/btime are outside the claim.
        let first_manifest = frozen_enforceable_manifest(&blit_root);

        assert_eq!(fs::metadata(&blit_root).unwrap().permissions().mode() & 0o7777, 0o755);
        assert_eq!(
            fs::metadata(blit_root.join("usr/bin")).unwrap().permissions().mode() & 0o7777,
            0o751
        );
        assert_eq!(fs::metadata(&tool).unwrap().permissions().mode() & 0o7777, 0o755);
        assert_eq!(fs::metadata(&empty).unwrap().permissions().mode() & 0o7777, 0o640);
        assert_eq!(
            fs::metadata(blit_root.join("usr/share/restricted"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o555
        );
        assert_eq!(
            fs::read(blit_root.join("usr/share/restricted/tool")).unwrap(),
            asset_bytes
        );
        assert_eq!(fs::read(&tool).unwrap(), asset_bytes);
        assert_eq!(fs::metadata(&tool).unwrap().len(), asset_bytes.len() as u64);
        assert_ne!(
            (asset_metadata.dev(), asset_metadata.ino()),
            (fs::metadata(&tool).unwrap().dev(), fs::metadata(&tool).unwrap().ino())
        );
        assert_eq!(fs::read_link(&tool_link).unwrap(), PathBuf::from("tool"));
        for (source, target) in ROOT_ABI_LINKS {
            assert_eq!(fs::read_link(blit_root.join(target)).unwrap(), PathBuf::from(source));
        }

        assert!(!blit_root.join("usr/share/omitted").exists());
        assert!(!blit_root.join("usr/.stateID").exists());
        assert!(!blit_root.join("usr/lib/os-release").exists());
        assert!(!system_model::snapshot_path(&blit_root).exists());
        assert!(!blit_root.join("etc").exists());
        assert_eq!(fs::read(&isolation_marker).unwrap(), b"isolation root is out of scope");
        assert_eq!(fs::read(&asset_path).unwrap(), asset_bytes);
        assert_eq!(fs::metadata(&asset_path).unwrap().permissions().mode() & 0o7777, 0o640);

        let packages = [second.clone(), first.clone()];
        let tool_binding = FrozenExecutableBinding {
            package: first.clone(),
            path: PathBuf::from("/usr/bin/tool"),
        };
        let tool_guard = client
            .require_frozen_executables(&packages, std::slice::from_ref(&tool_binding))
            .unwrap();
        let retained_tool = blit_root.join("usr/bin/tool-before-substitution");
        fs::rename(&tool, &retained_tool).unwrap();
        fs::write(&tool, &asset_bytes).unwrap();
        fs::set_permissions(&tool, Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            tool_guard.revalidate(),
            Err(Error::FrozenExecutablePathReplaced { package, path })
                if package == first && path == Path::new("/usr/bin/tool")
        ));
        fs::remove_file(&tool).unwrap();
        fs::rename(&retained_tool, &tool).unwrap();
        drop(tool_guard);

        let outside = FrozenExecutableBinding {
            package: omitted.clone(),
            path: PathBuf::from("/usr/share/omitted"),
        };
        assert!(matches!(
            client.require_frozen_executables(&packages, &[outside]),
            Err(Error::FrozenExecutableProviderOutsideClosure { package, path })
                if package == omitted && path == Path::new("/usr/share/omitted")
        ));

        let wrong_provider = FrozenExecutableBinding {
            package: second.clone(),
            path: PathBuf::from("/usr/bin/tool"),
        };
        assert!(matches!(
            client.require_frozen_executables(&packages, &[wrong_provider]),
            Err(Error::MissingFrozenExecutableLayout { package, path })
                if package == second && path == Path::new("/usr/bin/tool")
        ));

        let cross_provider_symlink = FrozenExecutableBinding {
            package: first.clone(),
            path: PathBuf::from("/usr/bin/cross-tool"),
        };
        let cross_provider_guard = client
            .require_frozen_executables(&packages, std::slice::from_ref(&cross_provider_symlink))
            .unwrap();
        cross_provider_guard.revalidate().unwrap();
        drop(cross_provider_guard);

        let cyclic_symlink = FrozenExecutableBinding {
            package: first.clone(),
            path: PathBuf::from("/usr/bin/cycle-a"),
        };
        assert!(matches!(
            client.require_frozen_executables(&packages, &[cyclic_symlink]),
            Err(Error::FrozenExecutableSymlinkCycle { package, path })
                if package == first && path == Path::new("/usr/bin/cycle-a")
        ));

        let symlink_binding = FrozenExecutableBinding {
            package: first.clone(),
            path: PathBuf::from("/usr/bin/tool-link"),
        };
        let _guard = client
            .require_frozen_executables(&packages, std::slice::from_ref(&symlink_binding))
            .unwrap();
        fs::remove_file(&tool_link).unwrap();
        symlink("../share/empty", &tool_link).unwrap();
        assert!(matches!(
            client.require_frozen_executables(&packages, &[symlink_binding]),
            Err(Error::FrozenExecutableSymlinkTargetMismatch { package, path, expected, actual })
                if package == first
                    && path == Path::new("/usr/bin/tool-link")
                    && expected == "tool"
                    && actual == OsString::from("../share/empty")
        ));
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

        let non_executable = FrozenExecutableBinding {
            package: first.clone(),
            path: PathBuf::from("/usr/share/empty"),
        };
        assert!(matches!(
            client.require_frozen_executables(&packages, &[non_executable]),
            Err(Error::FrozenExecutableLayoutNotExecutable { package, path, mode })
                if package == first
                    && path == Path::new("/usr/share/empty")
                    && mode == nix::libc::S_IFREG | 0o640
        ));

        let invalid_path = FrozenExecutableBinding {
            package: first.clone(),
            path: PathBuf::from("/usr/bin/../bin/tool"),
        };
        assert!(matches!(
            client.require_frozen_executables(&packages, &[invalid_path]),
            Err(Error::InvalidFrozenExecutablePath { package, path })
                if package == first && path == Path::new("/usr/bin/../bin/tool")
        ));

        fs::set_permissions(&tool, Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
            Err(Error::FrozenExecutableModeMismatch { package, path, expected, actual })
                if package == first
                    && path == Path::new("/usr/bin/tool")
                    && expected == nix::libc::S_IFREG | 0o755
                    && actual == nix::libc::S_IFREG | 0o700
        ));
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

        let hardlink = blit_root.join("usr/bin/tool-hardlink");
        fs::hard_link(&tool, &hardlink).unwrap();
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
            Err(Error::FrozenExecutableNotIndependentRegular { package, path, links: 2, .. })
                if package == first && path == Path::new("/usr/bin/tool")
        ));
        fs::remove_file(&hardlink).unwrap();
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

        let oversized = fs::OpenOptions::new().write(true).open(&tool).unwrap();
        oversized.set_len(MAX_FROZEN_EXECUTABLE_BYTES + 1).unwrap();
        drop(oversized);
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
            Err(Error::FrozenExecutableByteLimit { package, path, limit, actual })
                if package == first
                    && path == Path::new("/usr/bin/tool")
                    && limit == MAX_FROZEN_EXECUTABLE_BYTES
                    && actual == MAX_FROZEN_EXECUTABLE_BYTES + 1
        ));
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

        fs::write(&tool, &adversarial_asset_bytes).unwrap();
        assert_eq!(fs::metadata(&tool).unwrap().len(), asset_bytes.len() as u64);
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
            Err(Error::FrozenExecutableDigestMismatch { package, path, .. })
                if package == first && path == Path::new("/usr/bin/tool")
        ));
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

        let runtime_symlink = blit_root.join("usr/bin/tool-runtime-link");
        fs::remove_file(&tool).unwrap();
        symlink("tool-runtime-link", &tool).unwrap();
        fs::write(&runtime_symlink, &asset_bytes).unwrap();
        fs::set_permissions(&runtime_symlink, Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
            Err(Error::OpenFrozenExecutable { package, path, .. })
                if package == first && path == Path::new("/usr/bin/tool")
        ));
        fs::remove_file(&runtime_symlink).unwrap();
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

        let mut changed_after_digest = false;
        let error = require_frozen_executables(
            &client,
            test_materialized_frozen_root(&blit_root).unwrap(),
            &packages,
            std::slice::from_ref(&tool_binding),
            |binding, checkpoint| {
                if checkpoint == FrozenExecutableCheckpoint::AfterDigest && !changed_after_digest {
                    assert_eq!(binding, &tool_binding);
                    fs::write(&tool, &adversarial_asset_bytes).unwrap();
                    fs::set_permissions(&tool, Permissions::from_mode(0o700)).unwrap();
                    changed_after_digest = true;
                }
            },
        )
        .unwrap_err();
        assert!(changed_after_digest);
        assert!(matches!(
            error,
            Error::FrozenExecutableChanged { package, path }
                if package == first && path == Path::new("/usr/bin/tool")
        ));
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

        let replacement = blit_root.join("usr/bin/tool-replacement");
        fs::write(&replacement, &asset_bytes).unwrap();
        fs::set_permissions(&replacement, Permissions::from_mode(0o755)).unwrap();
        let mut replaced_before_reopen = false;
        let error = require_frozen_executables(
            &client,
            test_materialized_frozen_root(&blit_root).unwrap(),
            &packages,
            std::slice::from_ref(&tool_binding),
            |binding, checkpoint| {
                if checkpoint == FrozenExecutableCheckpoint::BeforeReopen && !replaced_before_reopen {
                    assert_eq!(binding, &tool_binding);
                    fs::rename(&replacement, &tool).unwrap();
                    replaced_before_reopen = true;
                }
            },
        )
        .unwrap_err();
        assert!(replaced_before_reopen);
        assert!(matches!(
            error,
            Error::FrozenExecutablePathReplaced { package, path }
                if package == first && path == Path::new("/usr/bin/tool")
        ));
        client.discard_frozen_root().unwrap();
        let _materialized = client.blit_frozen_root(&packages, EPOCH).unwrap();

        // A second materialization reverses caller and database order, changes
        // the process umask, and still reproduces all enforceable metadata.
        fs::write(&tool, b"mutated build root").unwrap();
        fs::set_permissions(&tool, Permissions::from_mode(0o600)).unwrap();
        client
            .layout_db
            .batch_add(layouts.iter().rev().map(|(package, layout)| (*package, layout)))
            .unwrap();
        nix::sys::stat::umask(Mode::from_bits_truncate(0o022));
        client.discard_frozen_root().unwrap();
        let _materialized = client
            .blit_frozen_root(&[first.clone(), second.clone()], EPOCH)
            .unwrap();
        assert_eq!(frozen_enforceable_manifest(&blit_root), first_manifest);
        assert_eq!(fs::read(&tool).unwrap(), asset_bytes);
        assert_eq!(fs::metadata(&tool).unwrap().permissions().mode() & 0o7777, 0o755);
        assert_eq!(
            FileTime::from_last_modification_time(&fs::metadata(&tool).unwrap()).unix_seconds(),
            EPOCH
        );
        make_tree_removable(&blit_root).unwrap();
    }

    fn frozen_enforceable_manifest(root: &Path) -> Vec<(String, &'static str, u32, i64, i64, Vec<u8>)> {
        fn visit(root: &Path, path: &Path, manifest: &mut Vec<(String, &'static str, u32, i64, i64, Vec<u8>)>) {
            let metadata = fs::symlink_metadata(path).unwrap();
            let (kind, content) = if metadata.file_type().is_symlink() {
                (
                    "symlink",
                    fs::read_link(path).unwrap().to_string_lossy().into_owned().into_bytes(),
                )
            } else if metadata.is_dir() {
                ("directory", Vec::new())
            } else {
                ("regular", fs::read(path).unwrap())
            };
            let relative = path.strip_prefix(root).unwrap();
            manifest.push((
                if relative.as_os_str().is_empty() {
                    ".".to_owned()
                } else {
                    relative.to_string_lossy().into_owned()
                },
                kind,
                metadata.mode() & 0o7777,
                metadata.atime(),
                metadata.mtime(),
                content,
            ));

            if metadata.is_dir() {
                let mut children = fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect::<Vec<_>>();
                children.sort();
                for child in children {
                    visit(root, &child, manifest);
                }
            }
        }

        let mut manifest = Vec::new();
        visit(root, root, &mut manifest);
        manifest
    }

    #[test]
    fn frozen_root_rejects_non_directory_collisions_before_touching_destination() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let blit_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&blit_root).unwrap();
        let marker = blit_root.join("untouched");
        fs::write(&marker, b"original root").unwrap();

        let installation = frozen_test_installation(&installation_root);
        let client = Client::frozen(
            "frozen-collision-test",
            installation,
            repository::Map::default(),
            &blit_root,
        )
        .unwrap();
        let first = package::Id::from("a-collision");
        let second = package::Id::from("z-collision");
        let first_layout = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, "bin/conflict".into()),
        };
        let second_layout = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o755,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(2, "bin/conflict".into()),
        };
        client
            .layout_db
            .batch_add([(&second, &second_layout), (&first, &first_layout)])
            .unwrap();

        let error = client
            .blit_frozen_root(&[second.clone(), first.clone()], 1_700_000_000)
            .unwrap_err();
        assert!(matches!(
            error,
            Error::FrozenPathCollision { path, first: found_first, second: found_second }
                if path == "/usr/bin/conflict" && found_first == first && found_second == second
        ));
        assert_eq!(fs::read(marker).unwrap(), b"original root");
    }

    fn frozen_publication_destination(parent_path: &Path, name: &str) -> FrozenRootDestination {
        let parent = open_frozen_destination_parent(parent_path).unwrap();
        FrozenRootDestination {
            root_path: parent_path.join(name),
            parent_path: parent_path.to_owned(),
            name: CString::new(name).unwrap(),
            parent_identity: frozen_root_identity(&parent, parent_path).unwrap(),
            parent,
        }
    }

    fn frozen_publication_fixture(
        parent_path: &Path,
    ) -> (
        FrozenRootDestination,
        FrozenPrivateDirectory,
        fs::File,
        fs::File,
        Instant,
    ) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let destination = frozen_publication_destination(parent_path, "published");
        let stage = create_frozen_private_directory(&destination, b".publication-test-", deadline).unwrap();
        mkdirat(stage.file.as_raw_fd(), "root", Mode::from_bits_truncate(0o755)).unwrap();
        fs::write(stage.path.join("root/candidate"), b"retained candidate").unwrap();
        let root_anchor = openat2_frozen_until(
            stage.file.as_raw_fd(),
            Path::new("root"),
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            (nix::libc::RESOLVE_BENEATH
                | nix::libc::RESOLVE_NO_SYMLINKS
                | nix::libc::RESOLVE_NO_MAGICLINKS
                | nix::libc::RESOLVE_NO_XDEV) as u64,
            deadline,
        )
        .unwrap();
        let root = openat2_frozen_until(
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
        .unwrap();
        assert_eq!(
            frozen_root_identity(&root_anchor, &stage.path.join("root")).unwrap(),
            frozen_root_identity(&root, &stage.path.join("root")).unwrap()
        );
        (destination, stage, root, root_anchor, deadline)
    }

    fn frozen_discard_fixture(parent_path: &Path) -> (FrozenRootDestination, fs::File, FrozenRootIdentity, Instant) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let destination = frozen_publication_destination(parent_path, "published");
        fs::create_dir(&destination.root_path).unwrap();
        fs::write(destination.root_path.join("candidate"), b"retained candidate").unwrap();
        let pinned =
            open_frozen_named_entry_until(&destination.parent, &destination.name, &destination.root_path, deadline)
                .unwrap()
                .unwrap();
        let identity = frozen_root_identity(&pinned, &destination.root_path).unwrap();
        (destination, pinned, identity, deadline)
    }

    fn frozen_discard_quarantine_names(parent: &Path) -> Vec<OsString> {
        let mut names = fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.as_bytes().starts_with(b".forge-frozen-discard-"))
            .collect::<Vec<_>>();
        names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        names
    }

    #[test]
    fn frozen_blit_returns_an_opath_guard_accepted_by_anchored_container() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let frozen_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&frozen_root).unwrap();
        let client = Client::frozen(
            "frozen-activation-anchor-test",
            frozen_test_installation(&installation_root),
            repository::Map::default(),
            &frozen_root,
        )
        .unwrap();
        let package = package::Id::from("directory-only-activation-provider");
        client
            .layout_db
            .add(
                &package,
                &StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFDIR | 0o755,
                    tag: 0,
                    file: StonePayloadLayoutFile::Directory("share".into()),
                },
            )
            .unwrap();
        client.discard_frozen_root().unwrap();

        let materialized = client
            .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000)
            .unwrap();
        let guard = client
            .require_materialized_frozen_executables(materialized, std::slice::from_ref(&package), &[])
            .unwrap();
        let retained_identity = frozen_root_identity(&guard.root, guard.root_path()).unwrap();
        let anchor = guard.revalidated_anchor().unwrap();
        // SAFETY: the guard retains the borrowed descriptor for this call.
        let status_flags = unsafe { nix::libc::fcntl(anchor.as_raw_fd(), nix::libc::F_GETFL) };
        assert_ne!(status_flags, -1);
        assert_eq!(
            status_flags & (nix::libc::O_PATH | nix::libc::O_DIRECTORY),
            nix::libc::O_PATH | nix::libc::O_DIRECTORY
        );
        // SAFETY: the guard retains the borrowed descriptor for this call.
        let descriptor_flags = unsafe { nix::libc::fcntl(anchor.as_raw_fd(), nix::libc::F_GETFD) };
        assert_ne!(descriptor_flags, -1);
        assert_ne!(descriptor_flags & nix::libc::FD_CLOEXEC, 0);
        let _container = container::Container::new_anchored(guard.root_path(), &anchor).unwrap();

        let displaced = temporary.path().join("displaced-frozen-root");
        fs::rename(&frozen_root, &displaced).unwrap();
        fs::create_dir(&frozen_root).unwrap();
        assert_eq!(
            frozen_root_identity(&guard.root, &displaced).unwrap(),
            retained_identity,
            "the guard must retain the pre-publication inode rather than reopen the public path"
        );
        assert_ne!(
            frozen_root_identity(&open_frozen_root_anchor(&frozen_root).unwrap(), &frozen_root).unwrap(),
            retained_identity
        );
        assert!(matches!(
            guard.revalidate(),
            Err(Error::FrozenExecutableRootReplaced(path)) if path == frozen_root
        ));
    }

    #[test]
    fn frozen_publication_rejects_a_readable_activation_descriptor_before_rename() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
        drop(root_anchor);

        let error = publish_frozen_root(&stage, &destination, &root, root.try_clone().unwrap(), deadline).unwrap_err();
        assert!(matches!(
            error,
            Error::FrozenPublicationNamespaceMismatch {
                reason: "the retained activation anchor is not the exact close-on-exec staged O_PATH directory",
                ..
            }
        ));
        assert!(!destination.root_path.exists());
        assert_eq!(
            fs::read(stage.path.join("root/candidate")).unwrap(),
            b"retained candidate"
        );
    }

    #[test]
    fn frozen_publication_rejects_a_foreign_opath_activation_anchor_before_rename() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
        drop(root_anchor);
        let foreign = temporary.path().join("foreign-anchor");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("untouched"), b"foreign inode").unwrap();
        let foreign_anchor = open_frozen_root_anchor(&foreign).unwrap();

        let error = publish_frozen_root(&stage, &destination, &root, foreign_anchor, deadline).unwrap_err();
        assert!(matches!(
            error,
            Error::FrozenPublicationNamespaceMismatch {
                reason: "the retained activation anchor is not the exact close-on-exec staged O_PATH directory",
                ..
            }
        ));
        assert!(!destination.root_path.exists());
        assert_eq!(
            fs::read(stage.path.join("root/candidate")).unwrap(),
            b"retained candidate"
        );
        assert_eq!(fs::read(foreign.join("untouched")).unwrap(), b"foreign inode");
    }

    #[test]
    fn frozen_publication_rejects_an_inheritable_opath_activation_anchor_before_rename() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
        // SAFETY: root_anchor owns a live descriptor for both fcntl calls.
        let descriptor_flags = unsafe { nix::libc::fcntl(root_anchor.as_raw_fd(), nix::libc::F_GETFD) };
        assert_ne!(descriptor_flags, -1);
        // SAFETY: F_SETFD updates only descriptor-local inheritance flags.
        assert_ne!(
            unsafe {
                nix::libc::fcntl(
                    root_anchor.as_raw_fd(),
                    nix::libc::F_SETFD,
                    descriptor_flags & !nix::libc::FD_CLOEXEC,
                )
            },
            -1
        );

        let error = publish_frozen_root(&stage, &destination, &root, root_anchor, deadline).unwrap_err();
        assert!(matches!(
            error,
            Error::FrozenPublicationNamespaceMismatch {
                reason: "the retained activation anchor is not the exact close-on-exec staged O_PATH directory",
                ..
            }
        ));
        assert!(!destination.root_path.exists());
        assert_eq!(
            fs::read(stage.path.join("root/candidate")).unwrap(),
            b"retained candidate"
        );
    }

    #[test]
    fn frozen_root_publication_never_replaces_an_existing_destination() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let frozen_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&frozen_root).unwrap();
        let marker = frozen_root.join("untouched");
        fs::write(&marker, b"original root").unwrap();
        let client = Client::frozen(
            "frozen-existing-destination-test",
            frozen_test_installation(&installation_root),
            repository::Map::default(),
            &frozen_root,
        )
        .unwrap();
        let package = package::Id::from("valid-directory-package");
        client
            .layout_db
            .add(
                &package,
                &StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFDIR | 0o755,
                    tag: 0,
                    file: StonePayloadLayoutFile::Directory("share".into()),
                },
            )
            .unwrap();

        assert!(matches!(
            client.blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000),
            Err(Error::FrozenRootDestinationExists(path)) if path == frozen_root
        ));
        assert_eq!(fs::read(marker).unwrap(), b"original root");

        // Exercise the publication syscall itself, not only the early
        // destination preflight: a destination appearing in that interval is
        // never exchanged or overwritten.
        let deadline = Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT;
        let raced_destination = temporary.path().join("raced-destination");
        let raced_parent = open_frozen_destination_parent(temporary.path()).unwrap();
        let raced_destination_authority = FrozenRootDestination {
            root_path: raced_destination.clone(),
            parent_path: temporary.path().to_owned(),
            name: CString::new("raced-destination").unwrap(),
            parent_identity: frozen_root_identity(&raced_parent, temporary.path()).unwrap(),
            parent: raced_parent,
        };
        let _lock = lock_frozen_destination_until(&raced_destination_authority, deadline).unwrap();
        let raced_stage =
            create_frozen_private_directory(&raced_destination_authority, b".publication-test-", deadline).unwrap();
        mkdirat(raced_stage.file.as_raw_fd(), "root", Mode::from_bits_truncate(0o755)).unwrap();
        fs::write(raced_stage.path.join("root/candidate"), b"candidate").unwrap();
        fs::create_dir(&raced_destination).unwrap();
        fs::write(raced_destination.join("winner"), b"winner").unwrap();
        let raced_activation_anchor = openat2_frozen_until(
            raced_stage.file.as_raw_fd(),
            Path::new("root"),
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            (nix::libc::RESOLVE_BENEATH
                | nix::libc::RESOLVE_NO_SYMLINKS
                | nix::libc::RESOLVE_NO_MAGICLINKS
                | nix::libc::RESOLVE_NO_XDEV) as u64,
            deadline,
        )
        .unwrap();
        let raced_anchor = openat2_frozen_until(
            raced_stage.file.as_raw_fd(),
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
        .unwrap();
        assert!(matches!(
            publish_frozen_root(
                &raced_stage,
                &raced_destination_authority,
                &raced_anchor,
                raced_activation_anchor,
                deadline,
            ),
            Err(Error::FrozenRootDestinationExists(path)) if path == raced_destination
        ));
        assert_eq!(fs::read(raced_stage.path.join("root/candidate")).unwrap(), b"candidate");
        assert_eq!(fs::read(raced_destination.join("winner")).unwrap(), b"winner");
    }

    #[test]
    fn frozen_publication_adopts_an_applied_rename_even_when_the_syscall_reports_error() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();

        let materialized = publish_frozen_root_with(
            &stage,
            &destination,
            &root,
            root_anchor,
            deadline,
            |source_directory, source_name, destination_directory, destination_name| {
                renameat2_noreplace_until(
                    source_directory.file(),
                    source_name,
                    destination_directory.file(),
                    destination_name,
                    deadline,
                )?;
                Err(io::Error::from_raw_os_error(nix::libc::EIO))
            },
        )
        .unwrap();

        materialized.revalidate().unwrap();
        assert_eq!(
            fs::read(destination.root_path.join("candidate")).unwrap(),
            b"retained candidate"
        );
        assert!(!stage.path.join("root").exists());
        remove_empty_frozen_private_directory(&stage, &destination, deadline).unwrap();
    }

    #[test]
    fn frozen_publication_reconciles_an_applied_rename_after_the_work_deadline_expires() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, stage, root, root_anchor, fixture_deadline) = frozen_publication_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, fixture_deadline).unwrap();
        let work_deadline = Instant::now() + Duration::from_secs(1);

        let materialized = publish_frozen_root_with(
            &stage,
            &destination,
            &root,
            root_anchor,
            work_deadline,
            |source_directory, source_name, destination_directory, destination_name| {
                renameat2_noreplace_until(
                    source_directory.file(),
                    source_name,
                    destination_directory.file(),
                    destination_name,
                    work_deadline,
                )?;
                std::thread::sleep(
                    work_deadline
                        .saturating_duration_since(Instant::now())
                        .saturating_add(Duration::from_millis(1)),
                );
                Err(io::Error::from_raw_os_error(nix::libc::EIO))
            },
        )
        .unwrap();

        materialized.revalidate().unwrap();
        assert_eq!(
            fs::read(destination.root_path.join("candidate")).unwrap(),
            b"retained candidate"
        );
        remove_empty_frozen_private_directory(&stage, &destination, frozen_namespace_recovery_deadline()).unwrap();
    }

    #[test]
    fn frozen_private_directory_setup_failures_remove_the_exact_provisional_wrapper() {
        for rejected in [
            FrozenPrivateDirectoryCheckpoint::Retained,
            FrozenPrivateDirectoryCheckpoint::ModeNormalized,
            FrozenPrivateDirectoryCheckpoint::ReadableOpened,
            FrozenPrivateDirectoryCheckpoint::AclsChecked,
            FrozenPrivateDirectoryCheckpoint::InventoryVerified,
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let destination = frozen_publication_destination(temporary.path(), "published");
            let deadline = Instant::now() + Duration::from_secs(10);
            let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
            let reached = std::cell::Cell::new(false);

            let error = create_frozen_private_directory_with(
                &destination,
                b".setup-failure-test-",
                deadline,
                |checkpoint, _| {
                    if checkpoint == rejected {
                        reached.set(true);
                        Err(io::Error::other(format!("injected failure at {checkpoint:?}")).into())
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            assert!(reached.get(), "injection did not reach {rejected:?}: {error}");
            assert!(
                fs::read_dir(temporary.path()).unwrap().all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .as_bytes()
                    .starts_with(b".setup-failure-test-")),
                "{rejected:?} left a provisional wrapper: {error}"
            );
        }
    }

    #[test]
    fn frozen_private_directory_normalizes_setgid_inherited_from_its_parent() {
        let temporary = tempfile::tempdir().unwrap();
        let namespace = temporary.path().join("namespace");
        fs::create_dir(&namespace).unwrap();
        fs::set_permissions(&namespace, Permissions::from_mode(0o2770)).unwrap();
        assert_ne!(fs::symlink_metadata(&namespace).unwrap().mode() & nix::libc::S_ISGID, 0);
        let destination = frozen_publication_destination(&namespace, "published");
        let deadline = Instant::now() + Duration::from_secs(10);
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();

        let directory = create_frozen_private_directory(&destination, b".setgid-test-", deadline).unwrap();
        assert_eq!(directory.file.metadata().unwrap().mode() & 0o7777, 0o700);
        remove_empty_frozen_private_directory(&directory, &destination, deadline).unwrap();
    }

    #[test]
    fn frozen_publication_error_before_rename_preserves_the_retained_stage_for_bounded_cleanup() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();

        let error = publish_frozen_root_with(&stage, &destination, &root, root_anchor, deadline, |_, _, _, _| {
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        })
        .unwrap_err();
        assert!(matches!(error, Error::PublishFrozenRoot { .. }));
        assert_eq!(
            fs::read(stage.path.join("root/candidate")).unwrap(),
            b"retained candidate"
        );
        assert!(!destination.root_path.exists());

        discard_retained_frozen_stage(&stage, &destination, &root, deadline).unwrap();
        remove_empty_frozen_private_directory(&stage, &destination, deadline).unwrap();
        assert!(!stage.path.exists());
    }

    #[test]
    fn frozen_publication_reconciles_a_racing_destination_without_replacing_it() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
        let winner = destination.root_path.clone();

        let error = publish_frozen_root_with(
            &stage,
            &destination,
            &root,
            root_anchor,
            deadline,
            |source_directory, source_name, destination_directory, destination_name| {
                fs::create_dir(&winner)?;
                fs::write(winner.join("winner"), b"foreign winner")?;
                renameat2_noreplace_until(
                    source_directory.file(),
                    source_name,
                    destination_directory.file(),
                    destination_name,
                    deadline,
                )
            },
        )
        .unwrap_err();
        assert!(matches!(error, Error::FrozenRootDestinationExists(path) if path == winner));
        assert_eq!(fs::read(winner.join("winner")).unwrap(), b"foreign winner");
        assert_eq!(
            fs::read(stage.path.join("root/candidate")).unwrap(),
            b"retained candidate"
        );
        discard_retained_frozen_stage(&stage, &destination, &root, deadline).unwrap();
        remove_empty_frozen_private_directory(&stage, &destination, deadline).unwrap();
        assert_eq!(fs::read(winner.join("winner")).unwrap(), b"foreign winner");
    }

    #[test]
    fn frozen_publication_detects_destination_substitution_and_never_deletes_the_foreign_tree() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
        let public = destination.root_path.clone();
        let displaced = temporary.path().join("displaced-retained-root");

        let error = publish_frozen_root_with(
            &stage,
            &destination,
            &root,
            root_anchor,
            deadline,
            |source_directory, source_name, destination_directory, destination_name| {
                renameat2_noreplace_until(
                    source_directory.file(),
                    source_name,
                    destination_directory.file(),
                    destination_name,
                    deadline,
                )?;
                fs::rename(&public, &displaced)?;
                fs::create_dir(&public)?;
                fs::write(public.join("foreign"), b"must survive")?;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(matches!(error, Error::FrozenPublicationNamespaceMismatch { .. }));
        assert_eq!(fs::read(public.join("foreign")).unwrap(), b"must survive");
        assert_eq!(fs::read(displaced.join("candidate")).unwrap(), b"retained candidate");
        assert!(discard_retained_frozen_stage(&stage, &destination, &root, deadline).is_err());
        assert_eq!(fs::read(public.join("foreign")).unwrap(), b"must survive");
        remove_empty_frozen_private_directory(&stage, &destination, deadline).unwrap();
    }

    #[test]
    fn frozen_publication_rejects_a_foreign_stage_name_without_publishing_or_deleting_it() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, stage, root, root_anchor, deadline) = frozen_publication_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
        let displaced = temporary.path().join("displaced-intended-root");
        fs::rename(stage.path.join("root"), &displaced).unwrap();
        fs::create_dir(stage.path.join("root")).unwrap();
        fs::write(stage.path.join("root/foreign"), b"must survive").unwrap();

        let error = publish_frozen_root(&stage, &destination, &root, root_anchor, deadline).unwrap_err();
        assert!(matches!(error, Error::FrozenPublicationNamespaceMismatch { .. }));
        assert!(!destination.root_path.exists());
        assert_eq!(fs::read(stage.path.join("root/foreign")).unwrap(), b"must survive");
        assert_eq!(fs::read(displaced.join("candidate")).unwrap(), b"retained candidate");
        assert!(discard_retained_frozen_stage(&stage, &destination, &root, deadline).is_err());
        assert_eq!(fs::read(stage.path.join("root/foreign")).unwrap(), b"must survive");
    }

    #[test]
    fn frozen_destination_lock_serializes_cooperating_publishers_with_a_finite_wait() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, _stage, _root, _root_anchor, deadline) = frozen_publication_fixture(temporary.path());
        let _first = lock_frozen_destination_until(&destination, deadline).unwrap();
        let second_parent = open_frozen_destination_parent(temporary.path()).unwrap();
        let second = FrozenRootDestination {
            root_path: destination.root_path.clone(),
            parent_path: destination.parent_path.clone(),
            name: destination.name.clone(),
            parent_identity: frozen_root_identity(&second_parent, temporary.path()).unwrap(),
            parent: second_parent,
        };

        let error = lock_frozen_destination_until(&second, Instant::now() + Duration::from_millis(20)).unwrap_err();
        assert!(matches!(error, Error::FrozenMaterializationTimeout { .. }));
    }

    #[test]
    fn frozen_discard_widens_unreadable_roots_for_detach_and_private_cleanup() {
        for mode in [0o000, 0o300] {
            let temporary = tempfile::tempdir().unwrap();
            let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
            fs::set_permissions(&destination.root_path, Permissions::from_mode(mode)).unwrap();

            discard_frozen_root_destination_until(&destination, deadline).unwrap();

            assert!(!destination.root_path.exists(), "root mode was {mode:04o}");
            assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
        }
    }

    #[test]
    fn frozen_discard_restores_mode_when_post_chmod_identity_inspection_fails() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
        fs::set_permissions(&destination.root_path, Permissions::from_mode(0o000)).unwrap();
        let expected = frozen_root_identity(&pinned, &destination.root_path).unwrap();

        let error = prepare_frozen_discard_root_mode_with(&pinned, &destination, expected, deadline, |_, path| {
            Err(Error::StatFrozenExecutableRoot {
                path: path.to_owned(),
                source: io::Error::from_raw_os_error(nix::libc::EIO),
            })
        })
        .unwrap_err();

        assert!(matches!(error, Error::StatFrozenExecutableRoot { .. }));
        assert_eq!(frozen_root_identity(&pinned, &destination.root_path).unwrap(), expected);
        assert_eq!(
            fs::symlink_metadata(&destination.root_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0
        );
        fs::set_permissions(&destination.root_path, Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn frozen_discard_is_idempotent_when_the_public_root_is_absent() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = frozen_publication_destination(temporary.path(), "published");
        let deadline = Instant::now() + Duration::from_secs(30);

        discard_frozen_root_destination_until(&destination, deadline).unwrap();
        discard_frozen_root_destination_until(&destination, deadline).unwrap();

        assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
    }

    #[test]
    fn frozen_discard_unlinks_symlinks_without_touching_external_targets() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
        let outside = temporary.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("must-survive"), b"external").unwrap();
        symlink(&outside, destination.root_path.join("escape")).unwrap();

        discard_frozen_root_destination_until(&destination, deadline).unwrap();

        assert_eq!(fs::read(outside.join("must-survive")).unwrap(), b"external");
        assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
    }

    #[test]
    fn frozen_discard_depth_limit_accepts_n_and_preserves_n_plus_one_privately() {
        for depth in [MAX_FROZEN_LAYOUT_PATH_COMPONENTS, MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1] {
            let temporary = tempfile::tempdir().unwrap();
            let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
            let mut nested = destination.root_path.clone();
            for _ in 0..depth {
                nested.push("d");
                fs::create_dir(&nested).unwrap();
            }

            let result = discard_frozen_root_destination_until(&destination, deadline);
            if depth == MAX_FROZEN_LAYOUT_PATH_COMPONENTS {
                result.unwrap();
                assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
            } else {
                assert!(matches!(
                    result,
                    Err(Error::FrozenDiscardDepthLimit { limit, actual })
                        if limit == MAX_FROZEN_LAYOUT_PATH_COMPONENTS
                            && actual == MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1
                ));
                assert!(!destination.root_path.exists());
                assert_eq!(frozen_discard_quarantine_names(temporary.path()).len(), 1);
            }
        }
    }

    #[test]
    fn frozen_discard_entry_limit_rejects_n_plus_one_before_deletion() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
        let root = fs::File::open(&destination.root_path).unwrap();
        let mut entries = MAX_FROZEN_NORMALIZED_INODES;

        let error = discard_frozen_directory(&root, &destination.root_path, 0, &mut entries, deadline).unwrap_err();

        assert!(matches!(
            error,
            Error::FrozenDiscardEntryLimit { limit, actual }
                if limit == MAX_FROZEN_NORMALIZED_INODES && actual == MAX_FROZEN_NORMALIZED_INODES + 1
        ));
        assert_eq!(
            fs::read(destination.root_path.join("candidate")).unwrap(),
            b"retained candidate"
        );
    }

    #[test]
    fn frozen_discard_rejects_non_directory_roots_without_creating_quarantine() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = frozen_publication_destination(temporary.path(), "published");
        let deadline = Instant::now() + Duration::from_secs(30);
        fs::write(&destination.root_path, b"must survive").unwrap();

        let error = discard_frozen_root_destination_until(&destination, deadline).unwrap_err();
        assert!(matches!(error, Error::UnsafeFrozenRootDiscard { .. }));
        assert_eq!(fs::read(&destination.root_path).unwrap(), b"must survive");
        assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());

        fs::remove_file(&destination.root_path).unwrap();
        let target = temporary.path().join("symlink-target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("must-survive"), b"foreign").unwrap();
        symlink(&target, &destination.root_path).unwrap();

        assert!(discard_frozen_root_destination_until(&destination, deadline).is_err());
        assert_eq!(fs::read(target.join("must-survive")).unwrap(), b"foreign");
        assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
    }

    #[test]
    fn frozen_discard_rename_failure_removes_only_its_exact_empty_quarantine() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
        fs::set_permissions(&destination.root_path, Permissions::from_mode(0o000)).unwrap();

        let error = discard_frozen_root_destination_with(&destination, deadline, |_, _, _, _| {
            Err(io::Error::from_raw_os_error(nix::libc::EIO))
        })
        .unwrap_err();

        assert!(matches!(error, Error::DetachFrozenRoot { .. }));
        assert_eq!(
            fs::symlink_metadata(&destination.root_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0
        );
        fs::set_permissions(&destination.root_path, Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            fs::read(destination.root_path.join("candidate")).unwrap(),
            b"retained candidate"
        );
        assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
    }

    #[test]
    fn frozen_discard_adopts_an_applied_detach_even_when_the_syscall_reports_error() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, pinned, expected, deadline) = frozen_discard_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
        let quarantine = create_frozen_private_directory(&destination, b".discard-applied-test-", deadline).unwrap();

        detach_frozen_root_with(
            &destination,
            &quarantine,
            &pinned,
            expected,
            deadline,
            |source_directory, source_name, destination_directory, destination_name| {
                renameat2_noreplace_until(
                    source_directory.file(),
                    source_name,
                    destination_directory.file(),
                    destination_name,
                    deadline,
                )?;
                Err(io::Error::from_raw_os_error(nix::libc::EIO))
            },
        )
        .unwrap();

        assert!(!destination.root_path.exists());
        assert_eq!(
            fs::read(quarantine.path.join("root/candidate")).unwrap(),
            b"retained candidate"
        );
        let cleanup_deadline = frozen_namespace_recovery_deadline();
        discard_retained_frozen_stage(&quarantine, &destination, &pinned, cleanup_deadline).unwrap();
        remove_empty_frozen_private_directory(&quarantine, &destination, cleanup_deadline).unwrap();
    }

    #[test]
    fn frozen_discard_completes_after_an_applied_detach_reports_error() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());

        discard_frozen_root_destination_with(
            &destination,
            deadline,
            |source_directory, source_name, destination_directory, destination_name| {
                renameat2_noreplace_until(
                    source_directory.file(),
                    source_name,
                    destination_directory.file(),
                    destination_name,
                    deadline,
                )?;
                Err(io::Error::from_raw_os_error(nix::libc::EIO))
            },
        )
        .unwrap();

        assert!(!destination.root_path.exists());
        assert!(frozen_discard_quarantine_names(temporary.path()).is_empty());
    }

    #[test]
    fn frozen_discard_reconciles_an_applied_detach_after_the_work_deadline_expires() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, pinned, expected, _) = frozen_discard_fixture(temporary.path());
        let setup_deadline = Instant::now() + Duration::from_secs(30);
        let _lock = lock_frozen_destination_until(&destination, setup_deadline).unwrap();
        let quarantine =
            create_frozen_private_directory(&destination, b".discard-expired-test-", setup_deadline).unwrap();
        let work_deadline = Instant::now() + Duration::from_millis(50);

        detach_frozen_root_with(
            &destination,
            &quarantine,
            &pinned,
            expected,
            work_deadline,
            |source_directory, source_name, destination_directory, destination_name| {
                renameat2_noreplace_until(
                    source_directory.file(),
                    source_name,
                    destination_directory.file(),
                    destination_name,
                    work_deadline,
                )?;
                while Instant::now() <= work_deadline {
                    std::thread::yield_now();
                }
                Ok(())
            },
        )
        .unwrap();

        assert!(!destination.root_path.exists());
        assert_eq!(
            fs::read(quarantine.path.join("root/candidate")).unwrap(),
            b"retained candidate"
        );
        let cleanup_deadline = frozen_namespace_recovery_deadline();
        discard_retained_frozen_stage(&quarantine, &destination, &pinned, cleanup_deadline).unwrap();
        remove_empty_frozen_private_directory(&quarantine, &destination, cleanup_deadline).unwrap();
    }

    #[test]
    fn frozen_discard_unlink_reconciles_applied_errors_and_bounded_interrupts() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = open_frozen_destination_parent(temporary.path()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);

        for (name, report_applied_error) in [(c"applied", true), (c"interrupted", false)] {
            let path = temporary.path().join(OsStr::from_bytes(name.to_bytes()));
            fs::write(&path, b"discard me").unwrap();
            let anchor = open_frozen_named_entry_until(&directory, name, &path, deadline)
                .unwrap()
                .unwrap();
            let expected = frozen_root_identity(&anchor, &path).unwrap();
            let mut calls = 0usize;

            unlink_frozen_discard_entry_with(&directory, name, &path, expected, deadline, |directory, name| {
                calls += 1;
                if report_applied_error {
                    unlinkat(Some(directory.as_raw_fd()), name, UnlinkatFlags::NoRemoveDir)?;
                    Err(Errno::EIO)
                } else if calls == 1 {
                    Err(Errno::EINTR)
                } else {
                    unlinkat(Some(directory.as_raw_fd()), name, UnlinkatFlags::NoRemoveDir)
                }
            })
            .unwrap();

            assert!(!path.exists());
            assert_eq!(calls, if report_applied_error { 1 } else { 2 });
        }

        let bounded = temporary.path().join("bounded-interrupts");
        fs::write(&bounded, b"must survive").unwrap();
        let anchor = open_frozen_named_entry_until(&directory, c"bounded-interrupts", &bounded, deadline)
            .unwrap()
            .unwrap();
        let expected = frozen_root_identity(&anchor, &bounded).unwrap();
        let mut calls = 0usize;
        let error = unlink_frozen_discard_entry_with(
            &directory,
            c"bounded-interrupts",
            &bounded,
            expected,
            deadline,
            |_, _| {
                calls += 1;
                Err(Errno::EINTR)
            },
        )
        .unwrap_err();
        assert!(matches!(error, Error::RemoveFrozenDiscardEntry { .. }));
        assert_eq!(calls, MAX_FROZEN_DESTINATION_LOCK_INTERRUPTS + 1);
        assert_eq!(fs::read(&bounded).unwrap(), b"must survive");
    }

    #[test]
    fn frozen_discard_unlink_never_retries_against_a_foreign_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = open_frozen_destination_parent(temporary.path()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let path = temporary.path().join("candidate");
        let displaced = temporary.path().join("displaced-candidate");
        fs::write(&path, b"retained").unwrap();
        let anchor = open_frozen_named_entry_until(&directory, c"candidate", &path, deadline)
            .unwrap()
            .unwrap();
        let expected = frozen_root_identity(&anchor, &path).unwrap();

        let error = unlink_frozen_discard_entry_with(&directory, c"candidate", &path, expected, deadline, |_, _| {
            fs::rename(&path, &displaced).unwrap();
            fs::write(&path, b"foreign").unwrap();
            Err(Errno::EIO)
        })
        .unwrap_err();

        assert!(matches!(error, Error::FrozenDiscardEntryChanged));
        assert_eq!(fs::read(&path).unwrap(), b"foreign");
        assert_eq!(fs::read(&displaced).unwrap(), b"retained");
    }

    #[test]
    fn frozen_discard_preserves_a_racing_quarantine_collision_and_the_public_root() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, pinned, expected, deadline) = frozen_discard_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
        let quarantine = create_frozen_private_directory(&destination, b".discard-collision-test-", deadline).unwrap();
        let collision = quarantine.path.join("root");

        let error = detach_frozen_root_with(
            &destination,
            &quarantine,
            &pinned,
            expected,
            deadline,
            |source_directory, source_name, destination_directory, destination_name| {
                fs::create_dir(&collision)?;
                fs::write(collision.join("foreign"), b"must survive")?;
                renameat2_noreplace_until(
                    source_directory.file(),
                    source_name,
                    destination_directory.file(),
                    destination_name,
                    deadline,
                )
            },
        )
        .unwrap_err();
        assert!(matches!(error, Error::FrozenDiscardNamespaceMismatch { .. }));
        assert_eq!(
            fs::read(destination.root_path.join("candidate")).unwrap(),
            b"retained candidate"
        );
        assert_eq!(fs::read(collision.join("foreign")).unwrap(), b"must survive");
    }

    #[test]
    fn frozen_discard_detects_source_substitution_without_deleting_the_foreign_tree() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, pinned, expected, deadline) = frozen_discard_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();
        let quarantine = create_frozen_private_directory(&destination, b".discard-source-test-", deadline).unwrap();
        let displaced = temporary.path().join("displaced-retained-root");
        let public = destination.root_path.clone();

        let error = detach_frozen_root_with(
            &destination,
            &quarantine,
            &pinned,
            expected,
            deadline,
            |source_directory, source_name, destination_directory, destination_name| {
                fs::rename(&public, &displaced)?;
                fs::create_dir(&public)?;
                fs::write(public.join("foreign"), b"must survive")?;
                renameat2_noreplace_until(
                    source_directory.file(),
                    source_name,
                    destination_directory.file(),
                    destination_name,
                    deadline,
                )
            },
        )
        .unwrap_err();
        assert!(matches!(error, Error::FrozenDiscardNamespaceMismatch { .. }));
        assert_eq!(fs::read(displaced.join("candidate")).unwrap(), b"retained candidate");
        assert_eq!(fs::read(quarantine.path.join("root/foreign")).unwrap(), b"must survive");
        assert!(!public.exists());
    }

    #[test]
    fn frozen_discard_preserves_a_replaced_quarantine_wrapper_and_the_detached_root() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
        let displaced_wrapper = temporary.path().join("displaced-discard-wrapper");

        let error = discard_frozen_root_destination_with(
            &destination,
            deadline,
            |source_directory, source_name, destination_directory, destination_name| {
                let names = frozen_discard_quarantine_names(temporary.path());
                assert_eq!(names.len(), 1);
                let public_wrapper = temporary.path().join(&names[0]);
                fs::rename(&public_wrapper, &displaced_wrapper)?;
                fs::create_dir(&public_wrapper)?;
                fs::write(public_wrapper.join("foreign"), b"must survive")?;
                renameat2_noreplace_until(
                    source_directory.file(),
                    source_name,
                    destination_directory.file(),
                    destination_name,
                    deadline,
                )
            },
        )
        .unwrap_err();

        assert!(matches!(error, Error::CleanupFrozenDiscardQuarantine { .. }));
        assert!(!destination.root_path.exists());
        assert_eq!(
            fs::read(displaced_wrapper.join("root/candidate")).unwrap(),
            b"retained candidate"
        );
        let names = frozen_discard_quarantine_names(temporary.path());
        assert_eq!(names.len(), 1);
        assert_eq!(
            fs::read(temporary.path().join(&names[0]).join("foreign")).unwrap(),
            b"must survive"
        );
    }

    #[test]
    fn frozen_discard_uses_the_same_finite_parent_lock_as_publication() {
        let temporary = tempfile::tempdir().unwrap();
        let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(temporary.path());
        let _lock = lock_frozen_destination_until(&destination, deadline).unwrap();

        let error = discard_frozen_root_destination_until(&destination, Instant::now() + Duration::from_millis(20))
            .unwrap_err();
        assert!(matches!(error, Error::FrozenMaterializationTimeout { .. }));
        assert_eq!(
            fs::read(destination.root_path.join("candidate")).unwrap(),
            b"retained candidate"
        );
    }

    #[test]
    fn frozen_discard_rejects_destination_parent_replacement_without_touching_either_tree() {
        let temporary = tempfile::tempdir().unwrap();
        let namespace = temporary.path().join("namespace");
        fs::create_dir(&namespace).unwrap();
        let (destination, _pinned, _expected, deadline) = frozen_discard_fixture(&namespace);
        let displaced_namespace = temporary.path().join("displaced-namespace");
        fs::rename(&namespace, &displaced_namespace).unwrap();
        fs::create_dir(&namespace).unwrap();
        fs::create_dir(namespace.join("published")).unwrap();
        fs::write(namespace.join("published/foreign"), b"must survive").unwrap();

        assert!(discard_frozen_root_destination_until(&destination, deadline).is_err());
        assert_eq!(
            fs::read(displaced_namespace.join("published/candidate")).unwrap(),
            b"retained candidate"
        );
        assert_eq!(fs::read(namespace.join("published/foreign")).unwrap(), b"must survive");
    }

    #[test]
    fn failed_frozen_root_blit_never_publishes_or_leaves_a_reusable_stage() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let frozen_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        let client = Client::frozen(
            "frozen-partial-stage-test",
            frozen_test_installation(&installation_root),
            repository::Map::default(),
            &frozen_root,
        )
        .unwrap();
        let package = package::Id::from("missing-asset-package");
        client
            .layout_db
            .batch_add([
                (
                    &package,
                    &StonePayloadLayoutRecord {
                        uid: 0,
                        gid: 0,
                        mode: nix::libc::S_IFDIR | 0o755,
                        tag: 0,
                        file: StonePayloadLayoutFile::Directory("bin".into()),
                    },
                ),
                (
                    &package,
                    &StonePayloadLayoutRecord {
                        uid: 0,
                        gid: 0,
                        mode: nix::libc::S_IFREG | 0o755,
                        tag: 0,
                        file: StonePayloadLayoutFile::Regular(42, "bin/missing".into()),
                    },
                ),
            ])
            .unwrap();

        assert!(
            client
                .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000)
                .is_err()
        );
        assert!(!frozen_root.exists());
        let stage_count = fs::read_dir(temporary.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().as_bytes().starts_with(b".forge-frozen-stage-"))
            .count();
        assert_eq!(stage_count, 0);
    }

    #[test]
    fn frozen_root_normalizes_and_discards_a_mode_zero_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let frozen_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        let client = Client::frozen(
            "frozen-mode-zero-directory-test",
            frozen_test_installation(&installation_root),
            repository::Map::default(),
            &frozen_root,
        )
        .unwrap();
        fs::create_dir_all(client.installation.assets_path("v2")).unwrap();
        let package = package::Id::from("mode-zero-directory-package");
        client
            .layout_db
            .add(
                &package,
                &StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFDIR,
                    tag: 0,
                    file: StonePayloadLayoutFile::Directory("locked".into()),
                },
            )
            .unwrap();

        let _materialized = client
            .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000)
            .unwrap();
        assert_eq!(
            fs::symlink_metadata(frozen_root.join("usr/locked"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0
        );
        client.discard_frozen_root().unwrap();
        assert!(!frozen_root.exists());
    }

    #[test]
    fn frozen_root_rejects_unenforceable_ownership_before_touching_destination() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let blit_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&blit_root).unwrap();
        let marker = blit_root.join("untouched");
        fs::write(&marker, b"original root").unwrap();
        let installation = frozen_test_installation(&installation_root);
        let client = Client::frozen(
            "frozen-ownership-test",
            installation,
            repository::Map::default(),
            &blit_root,
        )
        .unwrap();
        let package = package::Id::from("owned-by-another-user");
        client
            .layout_db
            .add(
                &package,
                &StonePayloadLayoutRecord {
                    uid: 1000,
                    gid: 1000,
                    mode: nix::libc::S_IFREG | 0o644,
                    tag: 0,
                    file: StonePayloadLayoutFile::Regular(1, "share/owned".into()),
                },
            )
            .unwrap();

        let error = client
            .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000)
            .unwrap_err();
        assert!(matches!(
            error,
            Error::UnsupportedFrozenOwnership {
                package: found,
                path,
                uid: 1000,
                gid: 1000,
            } if found == package && path == "/usr/share/owned"
        ));
        assert_eq!(fs::read(marker).unwrap(), b"original root");
    }

    fn assert_frozen_layout_rejected_before_touching_destination(
        layout: StonePayloadLayoutRecord,
        assert_error: impl FnOnce(Error),
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let blit_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&blit_root).unwrap();
        let marker = blit_root.join("untouched");
        fs::write(&marker, b"original root").unwrap();
        let installation = frozen_test_installation(&installation_root);
        let client = Client::frozen(
            "frozen-invalid-layout-test",
            installation,
            repository::Map::default(),
            &blit_root,
        )
        .unwrap();
        let package = package::Id::from("invalid-layout");
        client.layout_db.add(&package, &layout).unwrap();

        assert_error(
            client
                .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_000)
                .unwrap_err(),
        );
        assert_eq!(fs::read(marker).unwrap(), b"original root");
    }

    #[test]
    fn frozen_consumers_reject_absolute_raw_stone_targets_without_a_compatibility_spelling() {
        let layout = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o755,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, "/usr/bin/tool".into()),
        };

        assert_frozen_layout_rejected_before_touching_destination(layout.clone(), |error| {
            assert!(matches!(
                error,
                Error::InvalidStoneLayoutTarget {
                    package,
                    target,
                    reason: "the target is absolute",
                } if package == package::Id::from("invalid-layout")
                    && target == "/usr/bin/tool"
            ));
        });

        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let frozen_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&frozen_root).unwrap();
        let client = Client::frozen(
            "frozen-invalid-executable-layout-test",
            frozen_test_installation(&installation_root),
            repository::Map::default(),
            &frozen_root,
        )
        .unwrap();
        let package = package::Id::from("absolute-executable-layout");
        client.layout_db.add(&package, &layout).unwrap();
        let binding = FrozenExecutableBinding {
            package: package.clone(),
            path: PathBuf::from("/usr/bin/tool"),
        };

        assert!(matches!(
            client.require_frozen_executables(
                std::slice::from_ref(&package),
                std::slice::from_ref(&binding),
            ),
            Err(Error::InvalidStoneLayoutTarget {
                package: rejected_package,
                target,
                reason: "the target is absolute",
            }) if rejected_package == package && target == "/usr/bin/tool"
        ));
    }

    #[test]
    fn direct_database_frozen_consumer_rejects_reserved_targets_before_destination_mutation() {
        for target in [
            ".cast-state-id.tmp",
            ".cast-tree-id",
            ".cast-tree-id.tmp",
            ".stateID/forged-child",
            "lib/os-release",
            "lib/os-release/forged-child",
            "lib/system-model.glu",
            "lib/system-model.glu/forged-child",
        ] {
            let layout = StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o755,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(1, target.into()),
            };

            assert_frozen_layout_rejected_before_touching_destination(layout, |error| {
                assert!(matches!(
                    error,
                    Error::InvalidStoneLayoutTarget {
                        package,
                        target: rejected_target,
                        reason: "the target is reserved for Cast system metadata",
                    } if package == package::Id::from("invalid-layout")
                        && rejected_target == target
                ));
            });
        }
    }

    #[test]
    fn frozen_root_rejects_inconsistent_or_unenforceable_modes_before_touching_destination() {
        let cases = [
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFDIR | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(1, "share/type-mismatch".into()),
            },
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o777 | (1 << 31),
                tag: 0,
                file: StonePayloadLayoutFile::Regular(1, "share/unsupported-mode-bit".into()),
            },
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFLNK | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Symlink("target".into(), "share/symlink-mode".into()),
            },
        ];

        for layout in cases {
            assert_frozen_layout_rejected_before_touching_destination(layout, |error| {
                assert!(matches!(error, Error::InvalidFrozenLayoutMode { .. }));
            });
        }
    }

    #[test]
    fn frozen_root_rejects_empty_and_nul_symlink_targets_before_touching_destination() {
        for (target, expected_reason) in [("", "the target is empty"), ("bad\0target", "the target contains NUL")] {
            assert_frozen_layout_rejected_before_touching_destination(
                StonePayloadLayoutRecord {
                    uid: 0,
                    gid: 0,
                    mode: nix::libc::S_IFLNK | 0o777,
                    tag: 0,
                    file: StonePayloadLayoutFile::Symlink(target.into(), "share/link".into()),
                },
                |error| {
                    assert!(matches!(
                        error,
                        Error::InvalidFrozenLayoutSymlinkTarget { package, reason }
                            if package == package::Id::from("invalid-layout")
                                && reason == expected_reason
                    ));
                },
            );
        }
    }

    #[test]
    fn frozen_root_rejects_nul_paths_before_touching_destination() {
        assert_frozen_layout_rejected_before_touching_destination(
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(1, "share/nul\0path".into()),
            },
            |error| {
                assert!(matches!(
                    error,
                    Error::InvalidStoneLayoutTarget {
                        package,
                        target,
                        reason: "the target contains an ASCII control byte",
                    } if package == package::Id::from("invalid-layout")
                        && target == "share/nul\0path"
                ));
            },
        );
    }

    fn frozen_path_with_components(components: usize) -> String {
        assert!(components >= 1);
        let mut path = String::from("/usr");
        for _ in 1..components {
            path.push_str("/a");
        }
        path
    }

    #[test]
    fn frozen_layout_path_policy_accepts_exact_limits_and_rejects_n_plus_one() {
        let exact_bytes = format!("/usr/{}", "a".repeat(MAX_FROZEN_EXECUTABLE_PATH_BYTES - "/usr/".len()));
        assert_eq!(exact_bytes.len(), MAX_FROZEN_EXECUTABLE_PATH_BYTES);
        assert!(require_materialized_frozen_path_policy(&exact_bytes).is_ok());
        let oversized = format!("{exact_bytes}a");
        assert!(matches!(
            require_materialized_frozen_path_policy(&oversized),
            Err(FrozenLayoutPathPolicyError::TooLong { actual })
                if actual == MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1
        ));

        let exact_depth = frozen_path_with_components(MAX_FROZEN_LAYOUT_PATH_COMPONENTS);
        assert!(require_materialized_frozen_path_policy(&exact_depth).is_ok());
        let excessive_depth = frozen_path_with_components(MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1);
        assert!(matches!(
            require_materialized_frozen_path_policy(&excessive_depth),
            Err(FrozenLayoutPathPolicyError::TooDeep { actual })
                if actual == MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1
        ));

        let symlink_layout = |target: String| StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFLNK | 0o777,
            tag: 0,
            file: StonePayloadLayoutFile::Symlink(target.into(), "share/link".into()),
        };
        assert!(
            FrozenLayoutEntry::new(
                package::Id::from("exact-target"),
                symlink_layout("a".repeat(MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES)),
                0,
            )
            .is_ok()
        );
        assert!(matches!(
            FrozenLayoutEntry::new(
                package::Id::from("oversized-target"),
                symlink_layout("a".repeat(MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1)),
                0,
            ),
            Err(Error::FrozenLayoutSymlinkTargetTooLong { limit, actual, .. })
                if limit == MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES
                    && actual == MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1
        ));
    }

    #[test]
    fn frozen_root_rejects_oversized_paths_targets_and_depth_before_touching_destination() {
        let oversized_path = "a".repeat(MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1 - "/usr/".len());
        assert_frozen_layout_rejected_before_touching_destination(
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(1, oversized_path.into()),
            },
            |error| {
                assert!(matches!(
                    error,
                    Error::InvalidStoneLayoutTarget {
                        package,
                        reason: "the materialized path exceeds Linux PATH_MAX",
                        ..
                    } if package == package::Id::from("invalid-layout")
                ));
            },
        );

        assert_frozen_layout_rejected_before_touching_destination(
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFLNK | 0o777,
                tag: 0,
                file: StonePayloadLayoutFile::Symlink(
                    "a".repeat(MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1).into(),
                    "share/oversized-target".into(),
                ),
            },
            |error| {
                assert!(matches!(
                    error,
                    Error::FrozenLayoutSymlinkTargetTooLong { limit, actual, .. }
                        if limit == MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES
                            && actual == MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1
                ));
            },
        );

        assert_frozen_layout_rejected_before_touching_destination(
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(
                    1,
                    std::iter::repeat_n("a", MAX_FROZEN_LAYOUT_PATH_COMPONENTS)
                        .collect::<Vec<_>>()
                        .join("/")
                        .into(),
                ),
            },
            |error| {
                assert!(matches!(
                    error,
                    Error::InvalidStoneLayoutTarget {
                        package,
                        reason: "the materialized path is too deep",
                        ..
                    } if package == package::Id::from("invalid-layout")
                ));
            },
        );
    }

    #[test]
    fn frozen_materializer_implicit_directory_limits_accept_n_and_reject_n_plus_one() {
        let package = package::Id::from("implicit-directory-budget");
        let entry = FrozenLayoutEntry::new(
            package,
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(1, "a/b".into()),
            },
            0,
        )
        .unwrap();
        let entries = [entry];
        let exact_bytes = "/usr/a".len() + "/usr".len() + "/".len();

        validate_frozen_tree_collisions_with_limits(&entries, 3, exact_bytes).unwrap();
        assert!(matches!(
            validate_frozen_tree_collisions_with_limits(&entries, 2, usize::MAX),
            Err(Error::FrozenExecutableDirectoryLimit { limit: 2, actual: 3 })
        ));
        assert!(matches!(
            validate_frozen_tree_collisions_with_limits(&entries, 3, exact_bytes - 1),
            Err(Error::FrozenExecutableDirectoryByteLimit { limit, actual })
                if limit == exact_bytes - 1 && actual == exact_bytes
        ));
    }

    #[test]
    fn frozen_root_rejects_conflicting_duplicate_directory_metadata() {
        let first = package::Id::from("a-directory-owner");
        let second = package::Id::from("z-directory-owner");
        let first_layout = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFDIR | 0o755,
            tag: 0,
            file: StonePayloadLayoutFile::Directory("share/collision".into()),
        };
        let second_layout = StonePayloadLayoutRecord {
            mode: nix::libc::S_IFDIR | 0o700,
            ..first_layout.clone()
        };

        let error = frozen_vfs(
            &[first.clone(), second.clone()],
            vec![(second.clone(), second_layout), (first.clone(), first_layout)],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::FrozenPathCollision { path, first: found_first, second: found_second }
                if path == "/usr/share/collision" && found_first == first && found_second == second
        ));
    }

    #[test]
    fn frozen_root_rejects_an_explicit_child_beneath_a_non_directory_parent() {
        let parent_package = package::Id::from("a-file-parent");
        let child_package = package::Id::from("z-file-child");
        let regular = |digest, path: &str| StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(digest, path.into()),
        };

        let error = frozen_vfs(
            &[parent_package.clone(), child_package.clone()],
            vec![
                (parent_package.clone(), regular(1, "share/file")),
                (child_package.clone(), regular(2, "share/file/child")),
            ],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::FrozenPathCollision { path, first, second }
                if path == "/usr/share/file/child"
                    && first == parent_package
                    && second == child_package
        ));
    }

    #[test]
    fn frozen_root_rejects_descendants_beneath_directory_symlink_redirects_outside_usr() {
        let package = package::Id::from("redirect-escape");
        let link = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFLNK | 0o777,
            tag: 0,
            file: StonePayloadLayoutFile::Symlink("/".into(), "escape".into()),
        };
        let child = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, "escape/etc/passwd".into()),
        };

        let error = frozen_vfs(
            std::slice::from_ref(&package),
            vec![(package.clone(), link), (package.clone(), child)],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::FrozenDirectorySymlinkDescendant { package: found, path, redirect }
                if found == package
                    && path.as_ref() == "/usr/escape/etc/passwd"
                    && redirect.as_ref() == "/usr/escape"
        ));
    }

    #[test]
    fn frozen_root_rejects_arbitrary_descendants_beneath_directory_symlinks_before_materializing() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let frozen_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&frozen_root).unwrap();
        let marker = frozen_root.join("untouched");
        fs::write(&marker, b"original root").unwrap();
        let client = Client::frozen(
            "frozen-directory-redirect-test",
            frozen_test_installation(&installation_root),
            repository::Map::default(),
            &frozen_root,
        )
        .unwrap();
        let package = package::Id::from("redirected-data-package");
        let directory = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFDIR | 0o755,
            tag: 0,
            file: StonePayloadLayoutFile::Directory("real".into()),
        };
        let redirect = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFLNK | 0o777,
            tag: 0,
            file: StonePayloadLayoutFile::Symlink("/usr/real".into(), "alias".into()),
        };
        let data = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, "alias/data".into()),
        };
        client
            .layout_db
            .batch_add([(&package, &directory), (&package, &redirect), (&package, &data)])
            .unwrap();

        let error = client
            .blit_frozen_root(std::slice::from_ref(&package), 1_700_000_123)
            .unwrap_err();
        assert!(matches!(
            error,
            Error::FrozenDirectorySymlinkDescendant { package: found, path, redirect }
                if found == package
                    && path.as_ref() == "/usr/alias/data"
                    && redirect.as_ref() == "/usr/alias"
        ));
        assert_eq!(fs::read(marker).unwrap(), b"original root");
        assert!(!frozen_root.join("usr").exists());
    }

    #[test]
    fn frozen_root_rejects_conflicting_directory_metadata_after_redirect() {
        let first = package::Id::from("a-redirected-directory");
        let second = package::Id::from("z-real-directory");
        let directory = |mode: u32, target: &str| StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFDIR | mode,
            tag: 0,
            file: StonePayloadLayoutFile::Directory(target.into()),
        };
        let link = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFLNK | 0o777,
            tag: 0,
            file: StonePayloadLayoutFile::Symlink("/usr/real".into(), "alias".into()),
        };

        let error = frozen_vfs(
            &[first.clone(), second.clone()],
            vec![
                (first.clone(), link),
                (first.clone(), directory(0o700, "alias/shared")),
                (second.clone(), directory(0o755, "real")),
                (second.clone(), directory(0o755, "real/shared")),
            ],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::FrozenDirectorySymlinkDescendant { package: found, path, redirect }
                if found == first
                    && path.as_ref() == "/usr/alias/shared"
                    && redirect.as_ref() == "/usr/alias"
        ));
    }

    #[test]
    fn verify_reblits_and_preserves_the_existing_normalized_snapshot() {
        const CHILD: &str = "CAST_VERIFY_REPAIR_TIMEOUT_CHILD";
        const TEST: &str = "client::tests::verify_reblits_and_preserves_the_existing_normalized_snapshot";

        if std::env::var_os(CHILD).is_none() {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .arg(TEST)
                .arg("--exact")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env(CHILD, "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if child.try_wait().unwrap().is_some() {
                    let output = child.wait_with_output().unwrap();
                    assert!(
                        output.status.success(),
                        "verify repair child failed\nstdout:\n{}\nstderr:\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    return;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    let output = child.wait_with_output().unwrap();
                    panic!(
                        "verify repair exceeded 15 seconds (possible coordinator deadlock)\nstdout:\n{}\nstderr:\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }

        let temporary = tempfile::tempdir().unwrap();
        let mut client = stateful_test_client(temporary.path());
        fs::create_dir_all(client.installation.root.join("etc")).unwrap();
        fs::set_permissions(client.installation.root.join("etc"), Permissions::from_mode(0o755)).unwrap();
        fs::create_dir_all(client.installation.assets_path("v2")).unwrap();

        let package = package::Id::from("verify-package");
        let layout = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFDIR | 0o755,
            tag: 0,
            file: StonePayloadLayoutFile::Directory("share/verify-proof".into()),
        };
        client.layout_db.add(&package, &layout).unwrap();
        let state = client
            .state_db
            .add(&[Selection::explicit(package)], Some("active"), None)
            .unwrap();
        client.installation.active_state = Some(state.id);
        record_state_id(&client.installation.root, state.id).unwrap();

        let original = generated_system_snapshot("active-package");
        let expected = original.encoded().to_owned();
        record_system_snapshot(&client.installation.root, original).unwrap();
        let restored_path = client.installation.root.join("usr/share/verify-proof");
        assert!(!restored_path.exists());

        client.verify(true, false).unwrap();

        assert!(restored_path.is_dir());
        assert_generated_snapshot(
            &system_model::snapshot_path(&client.installation.root),
            &expected,
            "active-package",
        );
    }

    struct StatefulTransitionFixture {
        _temporary: tempfile::TempDir,
        client: Client,
        previous: State,
        candidate: State,
        previous_snapshot: String,
        candidate_snapshot: String,
    }

    fn stateful_transition_fixture(archive_candidate: bool) -> StatefulTransitionFixture {
        let temporary = tempfile::tempdir().unwrap();
        let mut client = stateful_test_client(temporary.path());
        let previous = client.state_db.add(&[], Some("previous"), None).unwrap();
        let candidate = client.state_db.add(&[], Some("candidate"), None).unwrap();
        client.installation.active_state = Some(previous.id);

        let previous_model = generated_system_snapshot("previous-package");
        let previous_snapshot = previous_model.encoded().to_owned();
        record_state_id(&client.installation.root, previous.id).unwrap();
        record_system_snapshot(&client.installation.root, previous_model).unwrap();

        let candidate_model = generated_system_snapshot("candidate-package");
        let candidate_snapshot = candidate_model.encoded().to_owned();
        if archive_candidate {
            let candidate_root = client.installation.root_path(candidate.id.to_string());
            record_state_id(&candidate_root, candidate.id).unwrap();
            record_system_snapshot(&candidate_root, candidate_model).unwrap();
        } else {
            // Production fresh-state activation receives an already
            // materialized staging /usr from blit_root. Candidate metadata is
            // deliberately absent until descriptor-bound decoration runs after
            // marker identity preparation.
            record_state_id(&client.installation.staging_dir(), candidate.id).unwrap();
        }

        StatefulTransitionFixture {
            _temporary: temporary,
            client,
            previous,
            candidate,
            previous_snapshot,
            candidate_snapshot,
        }
    }

    fn injected_state_transition_error(message: &'static str) -> Error {
        Error::Io(io::Error::other(message))
    }

    fn recovery_tree_token(usr: &Path) -> String {
        let store = crate::tree_marker::TreeMarkerStore::open_path(usr).unwrap();
        store.read_for_recovery().unwrap().token().as_str().to_owned()
    }

    fn previous_slot_parking_paths(installation: &Installation, state: state::Id) -> Vec<PathBuf> {
        let prefix = format!(".previous-slot-{state}-");
        let mut paths = fs::read_dir(installation.root_path(""))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn archived_candidate_slot_parking_paths(installation: &Installation, state: state::Id) -> Vec<PathBuf> {
        let prefix = format!(".archived-candidate-slot-{state}-");
        let mut paths = fs::read_dir(installation.root_path(""))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn externally_exchange_directory_names(first: &Path, second: &Path) {
        let parking = first.with_extension("external-exchange");
        fs::rename(first, &parking).unwrap();
        fs::rename(second, first).unwrap();
        fs::rename(parking, second).unwrap();
    }

    #[test]
    fn retained_archived_candidate_move_classifies_and_resumes_exact_layouts() {
        use crate::transition_identity::{
            RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
        };

        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let state_inode = fs::symlink_metadata(&state_root).unwrap().ino();
        let staging_inode = fs::symlink_metadata(&staging_root).unwrap().ino();
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();

        arm_retained_archived_candidate_move_fault(FaultPoint::BeforeExchange);
        let failure = identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::NotApplied);
        assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
        assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);

        arm_retained_archived_candidate_move_fault(FaultPoint::AfterExchange);
        identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), staging_inode);
        assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), state_inode);
        arm_retained_archived_candidate_move_fault(FaultPoint::AfterExchange);
        identity
            .rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
        assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);

        arm_retained_archived_candidate_move_fault(FaultPoint::CandidatePostSync);
        let failure = identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Applied);
        assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), staging_inode);
        assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), state_inode);
        identity
            .finish_applied_archived_candidate_stage(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), state_inode);

        arm_retained_archived_candidate_move_fault(FaultPoint::RootsParentSync);
        let failure = identity
            .rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Applied);
        assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
        assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);
        identity
            .finish_applied_archived_candidate_rearchive(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
        assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);
    }

    #[test]
    fn retained_archived_candidate_move_adopts_only_the_exact_exchanged_wrappers() {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();

        let staged_state = state_root.clone();
        let staged_staging = staging_root.clone();
        crate::transition_identity::arm_before_retained_archived_candidate_exchange(move || {
            externally_exchange_directory_names(&staged_state, &staged_staging);
        });
        identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        assert_eq!(
            fs::read_to_string(staging_root.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );

        let archived_state = state_root.clone();
        let archived_staging = staging_root.clone();
        crate::transition_identity::arm_before_retained_archived_candidate_exchange(move || {
            externally_exchange_directory_names(&archived_state, &archived_staging);
        });
        identity
            .rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        assert_eq!(
            fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert!(!staging_root.join("usr").exists());
    }

    #[test]
    fn displaced_archived_candidate_slot_retirement_preserves_racing_occupants() {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();

        let held_candidate = fixture.client.installation.root_path("held-archived-candidate-usr");
        fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
        let displaced_inode = fs::symlink_metadata(&state_root).unwrap().ino();
        let displaced = fixture.client.installation.root_path("displaced-staging-wrapper");
        let raced_state = state_root.clone();
        let raced_displaced = displaced.clone();
        crate::transition_identity::arm_before_retired_archived_candidate_slot_move(move || {
            fs::rename(&raced_state, &raced_displaced).unwrap();
            fs::create_dir(&raced_state).unwrap();
            fs::set_permissions(&raced_state, Permissions::from_mode(0o700)).unwrap();
            fs::write(raced_state.join("foreign"), b"racing occupant").unwrap();
        });

        identity
            .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();

        assert_eq!(fs::symlink_metadata(&displaced).unwrap().ino(), displaced_inode);
        assert!(held_candidate.join(".stateID").is_file());
        assert!(!state_root.exists());
        let parking = archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id);
        assert_eq!(parking.len(), 1);
        assert_eq!(fs::read(parking[0].join("foreign")).unwrap(), b"racing occupant");
    }

    #[test]
    fn archived_activation_resumes_applied_staging_suffix_before_full_recovery() {
        let fixture = stateful_transition_fixture(true);
        crate::transition_identity::arm_retained_archived_candidate_move_fault(
            crate::transition_identity::RetainedArchivedCandidateMoveFaultPoint::CandidatePostSync,
        );

        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                    Err(injected_state_transition_error("recover after staged suffix resume"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
        assert_recovered_stateful_transition(&fixture);
    }

    #[test]
    fn archived_activation_resumes_applied_rearchive_suffix_during_full_recovery() {
        let fixture = stateful_transition_fixture(true);

        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                    crate::transition_identity::arm_retained_archived_candidate_move_fault(
                        crate::transition_identity::RetainedArchivedCandidateMoveFaultPoint::FinalRevalidation,
                    );
                    Err(injected_state_transition_error("resume rearchive durability suffix"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
        assert_recovered_stateful_transition(&fixture);
    }

    #[test]
    fn archived_activation_keeps_rearchive_preparation_sticky_through_presync_faults() {
        use crate::transition_identity::{
            RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
        };

        for fault in [
            FaultPoint::CandidatePreSync,
            FaultPoint::CandidateWrapperSync,
            FaultPoint::DisplacedWrapperSync,
        ] {
            let fixture = stateful_transition_fixture(true);
            let error = fixture
                .client
                .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                        arm_retained_archived_candidate_move_fault(fault);
                        Err(injected_state_transition_error("retry sticky rearchive preparation"))
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err();

            assert!(
                matches!(error, Error::StatefulTransitionUsrRestored { .. }),
                "recovery did not complete for {fault:?}: {error:#?}"
            );
            assert_recovered_stateful_transition(&fixture);
        }
    }

    #[test]
    fn forged_exact_tree_marker_hardlink_is_not_adopted_in_process() {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        let token = recovery_tree_token(&state_root.join("usr"));
        let forged = state_root.join(format!(".cast-state-slot-{}-{token}", fixture.candidate.id));
        fs::hard_link(state_root.join("usr/.cast-tree-id"), &forged).unwrap();

        let failure = identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::NotApplied);
        assert_eq!(
            fs::symlink_metadata(&forged).unwrap().ino(),
            fs::symlink_metadata(state_root.join("usr/.cast-tree-id"))
                .unwrap()
                .ino()
        );
    }

    #[test]
    fn exact_parked_tree_marker_hardlink_is_reauthorized_after_reopen() {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let held_candidate = fixture.client.installation.root_path("held-candidate-for-slot-reopen");
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
        identity
            .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        drop(identity);

        let marker = held_candidate.join(".cast-tree-id");
        let marker_metadata = fs::symlink_metadata(&marker).unwrap();
        assert_eq!(marker_metadata.nlink(), 2);
        let parked = archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id);
        assert_eq!(parked.len(), 1);
        let slot_link = fs::read_dir(&parked[0])
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".cast-state-slot-")
            })
            .unwrap();
        assert_eq!(fs::symlink_metadata(&slot_link).unwrap().ino(), marker_metadata.ino());

        let reopened = fixture
            .client
            .prepare_stateful_tree_identity(&held_candidate, fixture.candidate.id)
            .unwrap();
        reopened.verify_candidate_for_recovery(&held_candidate).unwrap();
        drop(reopened);

        let marker_name = slot_link.file_name().unwrap().to_owned();
        let token = marker_name.to_string_lossy().rsplit('-').next().unwrap().to_owned();
        let copied_wrapper = fixture
            .client
            .installation
            .root_path(format!(".archived-candidate-slot-{}-{token}-1", fixture.candidate.id));
        fs::create_dir(&copied_wrapper).unwrap();
        fs::set_permissions(&copied_wrapper, Permissions::from_mode(0o700)).unwrap();
        let copied_marker = copied_wrapper.join(&marker_name);
        fs::copy(&marker, &copied_marker).unwrap();
        fs::set_permissions(&copied_marker, Permissions::from_mode(0o444)).unwrap();
        assert_eq!(
            archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).len(),
            2
        );
        fixture
            .client
            .prepare_stateful_tree_identity(&held_candidate, fixture.candidate.id)
            .unwrap_err();
        assert_ne!(
            fs::symlink_metadata(&copied_marker).unwrap().ino(),
            marker_metadata.ino()
        );

        fs::remove_file(&copied_marker).unwrap();
        fs::remove_dir(&copied_wrapper).unwrap();
        let extra_link = fixture.client.installation.root_path("extra-tree-marker-hardlink");
        fs::hard_link(&marker, &extra_link).unwrap();
        fixture
            .client
            .prepare_stateful_tree_identity(&held_candidate, fixture.candidate.id)
            .unwrap_err();
        assert_eq!(fs::symlink_metadata(&marker).unwrap().nlink(), 3);
    }

    #[test]
    fn retained_archived_candidate_move_rejects_substituted_roots_as_ambiguous() {
        let fixture = stateful_transition_fixture(true);
        let roots = fixture.client.installation.root_path("");
        let displaced_roots = fixture.client.installation.root.join("displaced-state-roots");
        let candidate = fixture.candidate.id;
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(
                &fixture.client.installation.root_path(candidate.to_string()).join("usr"),
                candidate,
            )
            .unwrap();
        let raced_roots = roots.clone();
        let raced_displaced = displaced_roots.clone();
        crate::transition_identity::arm_before_retained_archived_candidate_exchange(move || {
            fs::rename(&raced_roots, &raced_displaced).unwrap();
            fs::create_dir(&raced_roots).unwrap();
            fs::set_permissions(&raced_roots, Permissions::from_mode(0o700)).unwrap();
        });

        let failure = identity
            .stage_archived_candidate(&fixture.client.installation, candidate)
            .unwrap_err();

        assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Ambiguous);
        assert!(roots.is_dir());
        assert!(
            displaced_roots
                .join(candidate.to_string())
                .join("usr/.stateID")
                .is_file()
        );
        assert!(displaced_roots.join("staging").is_dir());
    }

    #[test]
    fn retained_archived_candidate_move_rejects_a_substituted_source_wrapper() {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let displaced = fixture
            .client
            .installation
            .root_path("displaced-archived-candidate-wrapper");
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        let raced_state = state_root.clone();
        let raced_displaced = displaced.clone();
        crate::transition_identity::arm_before_retained_archived_candidate_exchange(move || {
            fs::rename(&raced_state, &raced_displaced).unwrap();
            fs::create_dir(&raced_state).unwrap();
            fs::set_permissions(&raced_state, Permissions::from_mode(0o700)).unwrap();
            fs::write(raced_state.join("foreign"), b"replacement wrapper").unwrap();
        });

        let failure = identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();

        assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Ambiguous);
        assert_eq!(fs::read(state_root.join("foreign")).unwrap(), b"replacement wrapper");
        assert_eq!(
            fs::read_to_string(displaced.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert!(!fixture.client.installation.staging_path("usr").exists());
    }

    #[test]
    fn retained_archived_candidate_move_rejects_a_substituted_fixed_staging_wrapper() {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let displaced = fixture.client.installation.root_path("displaced-fixed-staging-wrapper");
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        let raced_staging = staging_root.clone();
        let raced_displaced = displaced.clone();
        crate::transition_identity::arm_before_retained_archived_candidate_exchange(move || {
            fs::rename(&raced_staging, &raced_displaced).unwrap();
            fs::create_dir(&raced_staging).unwrap();
            fs::set_permissions(&raced_staging, Permissions::from_mode(0o700)).unwrap();
            fs::write(raced_staging.join("foreign"), b"replacement staging wrapper").unwrap();
        });

        let failure = identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();

        assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Ambiguous);
        assert_eq!(
            fs::read(staging_root.join("foreign")).unwrap(),
            b"replacement staging wrapper"
        );
        assert!(displaced.is_dir());
        assert_eq!(
            fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
    }

    #[test]
    fn displaced_archived_candidate_restore_faults_are_exactly_classified_and_resumable() {
        use crate::transition_identity::{
            RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
        };

        for (fault, expected) in [
            (
                FaultPoint::BeforeDisplacedSlotRestore,
                Some(RetainedArchivedCandidateMoveOutcome::NotApplied),
            ),
            (FaultPoint::AfterDisplacedSlotRestore, None),
            (
                FaultPoint::RootsAfterDisplacedSlotRestoreSync,
                Some(RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied),
            ),
        ] {
            let fixture = stateful_transition_fixture(true);
            let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
            let staging_root = fixture.client.installation.staging_dir();
            let held_candidate = fixture
                .client
                .installation
                .root_path(format!("held-candidate-for-{fault:?}"));
            let identity = fixture
                .client
                .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
                .unwrap();
            identity
                .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
                .unwrap();
            fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
            arm_retained_archived_candidate_move_fault(FaultPoint::FinalDisplacedSlotRetirementRevalidation);
            identity
                .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
                .unwrap_err();
            assert_eq!(
                archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).len(),
                1
            );
            fs::rename(&held_candidate, staging_root.join("usr")).unwrap();

            arm_retained_archived_candidate_move_fault(fault);
            let first = identity.rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id);
            match expected {
                Some(expected) => assert_eq!(first.unwrap_err().outcome(), expected, "fault {fault:?}"),
                None => first.unwrap(),
            }
            if expected.is_some() {
                identity
                    .rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id)
                    .unwrap();
            }

            assert!(
                archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).is_empty()
            );
            assert_eq!(
                fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
                fixture.candidate.id.to_string()
            );
            assert!(!staging_root.join("usr").exists());
        }
    }

    #[test]
    fn archived_candidate_marker_transfer_faults_resume_without_a_second_wrapper_exchange() {
        use crate::transition_identity::{
            RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
        };

        for fault in [
            FaultPoint::BeforeSlotMarkerTransfer,
            FaultPoint::SlotMarkerParentSync,
            FaultPoint::FinalSlotMarkerRevalidation,
        ] {
            let fixture = stateful_transition_fixture(true);
            let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
            let staging_root = fixture.client.installation.staging_dir();
            let state_inode = fs::symlink_metadata(&state_root).unwrap().ino();
            let staging_inode = fs::symlink_metadata(&staging_root).unwrap().ino();
            let identity = fixture
                .client
                .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
                .unwrap();

            arm_retained_archived_candidate_move_fault(fault);
            let failure = identity
                .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
                .unwrap_err();
            assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Applied);
            assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), staging_inode);
            assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), state_inode);

            identity
                .finish_applied_archived_candidate_stage(&fixture.client.installation, fixture.candidate.id)
                .unwrap();
            assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), staging_inode);
            assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), state_inode);
        }
    }

    #[test]
    fn externally_premoved_slot_marker_fast_path_still_finishes_durability() {
        use crate::transition_identity::{
            RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_before_archived_candidate_slot_marker_location,
            arm_retained_archived_candidate_move_fault,
        };

        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let state_inode = fs::symlink_metadata(&state_root).unwrap().ino();
        let staging_inode = fs::symlink_metadata(&staging_root).unwrap().ino();
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        let token = recovery_tree_token(&state_root.join("usr"));
        identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        let marker_name = format!(".cast-state-slot-{}-{token}", fixture.candidate.id);
        let source = state_root.join(&marker_name);
        let destination = staging_root.join(&marker_name);
        arm_before_archived_candidate_slot_marker_location(move || {
            fs::rename(&source, &destination).unwrap();
        });
        arm_retained_archived_candidate_move_fault(FaultPoint::SlotMarkerParentSync);

        let failure = identity
            .rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();
        assert_eq!(
            failure.outcome(),
            RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied
        );
        fixture
            .client
            .rearchive_archived_candidate(&identity, fixture.candidate.id)
            .unwrap();
        assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
        assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);
    }

    #[test]
    fn archived_candidate_rearchive_marker_preparation_faults_are_resumable() {
        use crate::transition_identity::{
            RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
        };

        for (fault, expected) in [
            (
                FaultPoint::BeforeSlotMarkerTransfer,
                Some(RetainedArchivedCandidateMoveOutcome::NotApplied),
            ),
            (FaultPoint::AfterSlotMarkerTransfer, None),
            (
                FaultPoint::SlotMarkerParentSync,
                Some(RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied),
            ),
            (
                FaultPoint::FinalSlotMarkerRevalidation,
                Some(RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied),
            ),
        ] {
            let fixture = stateful_transition_fixture(true);
            let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
            let staging_root = fixture.client.installation.staging_dir();
            let state_inode = fs::symlink_metadata(&state_root).unwrap().ino();
            let staging_inode = fs::symlink_metadata(&staging_root).unwrap().ino();
            let identity = fixture
                .client
                .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
                .unwrap();
            identity
                .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
                .unwrap();

            arm_retained_archived_candidate_move_fault(fault);
            let first = identity.rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id);
            match expected {
                Some(expected) => assert_eq!(first.unwrap_err().outcome(), expected, "fault {fault:?}"),
                None => first.unwrap(),
            }
            if expected.is_some() {
                fixture
                    .client
                    .rearchive_archived_candidate(&identity, fixture.candidate.id)
                    .unwrap();
            }

            assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
            assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);
            assert_eq!(
                fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
                fixture.candidate.id.to_string()
            );
        }
    }

    #[test]
    fn archived_candidate_parking_scan_skips_every_foreign_occupant_kind() {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let roots = fixture.client.installation.root_path("");
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        let token = recovery_tree_token(&state_root.join("usr"));
        let parking = |index| {
            roots.join(format!(
                ".archived-candidate-slot-{}-{token}-{index}",
                fixture.candidate.id
            ))
        };
        fs::write(parking(0), b"regular occupant").unwrap();
        symlink("/", parking(1)).unwrap();
        nix::unistd::mkfifo(&parking(2), Mode::from_bits_truncate(0o600)).unwrap();
        fs::create_dir(parking(3)).unwrap();
        fs::set_permissions(parking(3), Permissions::from_mode(0o777)).unwrap();

        identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        let held_candidate = fixture.client.installation.root_path("held-candidate-for-parking-scan");
        fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
        identity
            .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
            .unwrap();

        assert_eq!(fs::read(parking(0)).unwrap(), b"regular occupant");
        assert!(fs::symlink_metadata(parking(1)).unwrap().file_type().is_symlink());
        assert!(fs::symlink_metadata(parking(2)).unwrap().file_type().is_fifo());
        assert_eq!(
            fs::symlink_metadata(parking(3)).unwrap().permissions().mode() & 0o7777,
            0o777
        );
        let retained = parking(4);
        assert!(retained.is_dir());
        let retained_entries = fs::read_dir(&retained)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(retained_entries.len(), 1);
        assert!(retained_entries[0].starts_with(".cast-state-slot-"));
        assert!(held_candidate.join(".stateID").is_file());
    }

    #[test]
    fn archived_candidate_restore_preparation_uses_one_bounded_client_retry() {
        use crate::transition_identity::{
            RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
        };

        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let held_candidate = fixture
            .client
            .installation
            .root_path("held-candidate-for-client-restore-retry");
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
        arm_retained_archived_candidate_move_fault(FaultPoint::FinalDisplacedSlotRetirementRevalidation);
        identity
            .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();
        fs::rename(&held_candidate, staging_root.join("usr")).unwrap();

        arm_retained_archived_candidate_move_fault(FaultPoint::RootsAfterDisplacedSlotRestoreSync);
        fixture
            .client
            .rearchive_archived_candidate(&identity, fixture.candidate.id)
            .unwrap();

        assert_eq!(
            fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert!(!staging_root.join("usr").exists());
    }

    #[test]
    fn archived_candidate_marker_preparation_after_restore_uses_one_bounded_client_retry() {
        use crate::transition_identity::{
            RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
        };

        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let held_candidate = fixture
            .client
            .installation
            .root_path("held-candidate-for-client-marker-retry");
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
        arm_retained_archived_candidate_move_fault(FaultPoint::FinalDisplacedSlotRetirementRevalidation);
        identity
            .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();
        fs::rename(&held_candidate, staging_root.join("usr")).unwrap();

        arm_retained_archived_candidate_move_fault(FaultPoint::BeforeSlotMarkerTransfer);
        fixture
            .client
            .rearchive_archived_candidate(&identity, fixture.candidate.id)
            .unwrap();

        assert_eq!(
            fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert!(!staging_root.join("usr").exists());
    }

    #[test]
    fn multiple_structural_reusable_state_slot_links_fail_closed() {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let token = recovery_tree_token(&staged);
        let marker_name = format!(".cast-state-slot-{}-{token}", fixture.previous.id);

        for index in 0..2 {
            let slot = installation.root_path(format!(
                ".archived-candidate-slot-{}-{token}-{index}",
                fixture.previous.id
            ));
            fs::create_dir(&slot).unwrap();
            fs::set_permissions(&slot, Permissions::from_mode(0o700)).unwrap();
            let marker = slot.join(&marker_name);
            fs::hard_link(staged.join(".cast-tree-id"), marker).unwrap();
        }

        let failure = identity
            .archive_previous(installation, fixture.previous.id)
            .unwrap_err();

        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
        assert!(
            format!("{failure:?}").contains("links: 3"),
            "unexpected extra-link failure: {failure:?}"
        );
        assert!(staged.join(".stateID").is_file());
        assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
    }

    #[test]
    fn repeated_archived_activations_reuse_wrapper_slots_beyond_the_scan_bound() {
        let mut fixture = stateful_transition_fixture(true);
        fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, true, true, |_| Ok(()))
            .unwrap();
        fixture.client.installation.active_state = Some(fixture.candidate.id);
        let mut active = fixture.candidate.id;
        let mut next = fixture.previous.id;

        // The bounded parking namespace has 256 names. Crossing it proves
        // successful activation reuses the authenticated wrapper instead of
        // consuming one name per transaction.
        for _ in 0..257 {
            let replaced = fixture
                .client
                .activate_state_with_checkpoint(next, true, true, |_| Ok(()))
                .unwrap();
            assert_eq!(replaced, active);
            fixture.client.installation.active_state = Some(next);
            std::mem::swap(&mut active, &mut next);

            let retained_count =
                archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.previous.id).len()
                    + archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).len();
            assert_eq!(retained_count, 1);
        }

        assert_eq!(
            fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
            active.to_string()
        );
        assert!(
            fixture
                .client
                .installation
                .root_path(next.to_string())
                .join("usr")
                .is_dir()
        );
    }

    #[test]
    fn displaced_archived_candidate_retirement_without_an_attempt_fails_closed() {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();

        let error = identity
            .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();

        assert!(format!("{error:?}").contains("AttemptMissing"));
        assert!(state_root.join("usr/.stateID").is_file());
    }

    #[test]
    fn displaced_archived_candidate_retirement_resumes_without_a_second_move() {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        let held_candidate = fixture
            .client
            .installation
            .root_path("held-candidate-for-retirement-resume");
        fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
        crate::transition_identity::arm_retained_archived_candidate_move_fault(
            crate::transition_identity::RetainedArchivedCandidateMoveFaultPoint::RootsAfterDisplacedSlotRetireSync,
        );

        identity
            .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();
        let parked = archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id);
        assert_eq!(parked.len(), 1);
        let parked_inode = fs::symlink_metadata(&parked[0]).unwrap().ino();
        assert!(!state_root.exists());

        identity
            .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        assert_eq!(fs::symlink_metadata(&parked[0]).unwrap().ino(), parked_inode);
        assert!(!state_root.exists());
        assert!(held_candidate.join(".stateID").is_file());
    }

    #[test]
    fn archived_retirement_suffix_failure_restores_the_slot_during_full_recovery() {
        let fixture = stateful_transition_fixture(true);
        crate::transition_identity::arm_retained_archived_candidate_move_fault(
            crate::transition_identity::RetainedArchivedCandidateMoveFaultPoint::FinalDisplacedSlotRetirementRevalidation,
        );

        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, true, true, |_| Ok(()))
            .unwrap_err();

        assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
        assert_recovered_stateful_transition(&fixture);
        assert!(archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).is_empty());
    }

    #[test]
    fn quarantined_archived_candidate_retries_only_retirement_durability() {
        let fixture = stateful_transition_fixture(true);
        crate::transition_identity::arm_retained_archived_candidate_move_fault(
            crate::transition_identity::RetainedArchivedCandidateMoveFaultPoint::RootsAfterDisplacedSlotRetireSync,
        );

        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, false, true, |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterSystemTriggersStarted {
                    Err(injected_state_transition_error(
                        "quarantine with retirement suffix retry",
                    ))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
        assert_eq!(
            fs::read_dir(fixture.client.installation.state_quarantine_dir())
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).len(),
            1
        );
        assert!(!fixture.client.installation.staging_path("usr").exists());
    }

    fn exchanged_stateful_identity(fixture: &StatefulTransitionFixture) -> StatefulTreeIdentity {
        let staged_usr = fixture.client.installation.staging_path("usr");
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&staged_usr, fixture.candidate.id)
            .unwrap();
        identity.exchange_forward(&fixture.client.installation).unwrap();
        identity
    }

    #[test]
    fn retained_previous_moves_reconcile_before_and_after_rename_faults() {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let slot = installation.root_path(fixture.previous.id.to_string());
        let archived = slot.join("usr");
        let previous_inode = fs::symlink_metadata(&staged).unwrap().ino();

        crate::transition_identity::arm_retained_previous_move_fault(
            crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeRename,
        );
        let failure = identity
            .archive_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
        assert!(!archived.exists());
        assert!(!slot.exists());
        assert_eq!(previous_slot_parking_paths(installation, fixture.previous.id).len(), 1);

        crate::transition_identity::arm_retained_previous_move_fault(
            crate::transition_identity::RetainedPreviousMoveFaultPoint::AfterRename,
        );
        identity.archive_previous(installation, fixture.previous.id).unwrap();
        assert!(!staged.exists());
        assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);
        assert_eq!(
            fs::symlink_metadata(&slot).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        identity.verify_previous_for_recovery(&archived).unwrap();

        crate::transition_identity::arm_retained_previous_move_fault(
            crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeRename,
        );
        let failure = identity
            .restore_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
        assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);
        assert!(!staged.exists());

        crate::transition_identity::arm_retained_previous_move_fault(
            crate::transition_identity::RetainedPreviousMoveFaultPoint::AfterRename,
        );
        identity.restore_previous(installation, fixture.previous.id).unwrap();
        assert!(!archived.exists());
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
        assert!(!slot.exists());
        identity.verify_previous_for_recovery(&staged).unwrap();

        // A compensating restore retires the empty wrapper away from the
        // canonical state name. A fresh attempt therefore succeeds instead
        // of treating its own prior cleanup residue as ambient state.
        identity.archive_previous(installation, fixture.previous.id).unwrap();
        assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);
        identity.restore_previous(installation, fixture.previous.id).unwrap();
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
        assert!(!slot.exists());
        let parked = previous_slot_parking_paths(installation, fixture.previous.id);
        assert_eq!(parked.len(), 3);
        assert!(parked.iter().all(|path| fs::read_dir(path).unwrap().next().is_none()));
    }

    #[test]
    fn retained_previous_archive_applied_faults_resume_only_the_sync_suffix() {
        for point in [
            crate::transition_identity::RetainedPreviousMoveFaultPoint::SourceParentSync,
            crate::transition_identity::RetainedPreviousMoveFaultPoint::DestinationParentSync,
            crate::transition_identity::RetainedPreviousMoveFaultPoint::FinalRevalidation,
        ] {
            let fixture = stateful_transition_fixture(false);
            let identity = exchanged_stateful_identity(&fixture);
            let installation = &fixture.client.installation;
            let staged = installation.staging_path("usr");
            let archived = installation.root_path(fixture.previous.id.to_string()).join("usr");
            let previous_inode = fs::symlink_metadata(&staged).unwrap().ino();

            crate::transition_identity::arm_retained_previous_move_fault(point);
            let failure = identity
                .archive_previous(installation, fixture.previous.id)
                .unwrap_err();
            assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Applied);
            assert!(!staged.exists(), "archive source returned after {point:?}");
            assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);

            identity
                .finish_applied_previous_archive(installation, fixture.previous.id)
                .unwrap();
            assert!(!staged.exists(), "sync-only archive resume renamed after {point:?}");
            assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);
        }
    }

    #[test]
    fn retained_previous_restore_applied_faults_resume_only_the_sync_suffix() {
        for point in [
            crate::transition_identity::RetainedPreviousMoveFaultPoint::SourceParentSync,
            crate::transition_identity::RetainedPreviousMoveFaultPoint::DestinationParentSync,
            crate::transition_identity::RetainedPreviousMoveFaultPoint::FinalRevalidation,
        ] {
            let fixture = stateful_transition_fixture(false);
            let identity = exchanged_stateful_identity(&fixture);
            let installation = &fixture.client.installation;
            let staged = installation.staging_path("usr");
            let archived = installation.root_path(fixture.previous.id.to_string()).join("usr");
            identity.archive_previous(installation, fixture.previous.id).unwrap();
            let previous_inode = fs::symlink_metadata(&archived).unwrap().ino();

            crate::transition_identity::arm_retained_previous_move_fault(point);
            let failure = identity
                .restore_previous(installation, fixture.previous.id)
                .unwrap_err();
            assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Applied);
            assert!(!archived.exists(), "restore source returned after {point:?}");
            assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);

            identity
                .finish_applied_previous_restore(installation, fixture.previous.id)
                .unwrap();
            assert!(!archived.exists(), "sync-only restore resume renamed after {point:?}");
            assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
        }
    }

    #[test]
    fn retained_previous_slot_creation_faults_retire_the_state_name_before_retry() {
        for point in [
            crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeSlotPublish,
            crate::transition_identity::RetainedPreviousMoveFaultPoint::SlotSync,
            crate::transition_identity::RetainedPreviousMoveFaultPoint::RootsParentSync,
        ] {
            let fixture = stateful_transition_fixture(false);
            let identity = exchanged_stateful_identity(&fixture);
            let installation = &fixture.client.installation;
            let staged = installation.staging_path("usr");
            let slot = installation.root_path(fixture.previous.id.to_string());
            let previous_inode = fs::symlink_metadata(&staged).unwrap().ino();

            crate::transition_identity::arm_retained_previous_move_fault(point);
            let failure = identity
                .archive_previous(installation, fixture.previous.id)
                .unwrap_err();
            assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
            assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
            assert!(!slot.exists(), "canonical slot survived {point:?}");

            identity.archive_previous(installation, fixture.previous.id).unwrap();
            identity.restore_previous(installation, fixture.previous.id).unwrap();
            assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
            assert!(!slot.exists(), "retry left canonical slot after {point:?}");
        }
    }

    #[test]
    fn retained_previous_parking_scan_skips_occupied_non_mount_file_types() {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let roots = installation.root_path("");
        let staged = installation.staging_path("usr");
        let token = recovery_tree_token(&staged);
        let parking = |index| roots.join(format!(".previous-slot-{}-{token}-{index}", fixture.previous.id));

        fs::write(parking(0), b"regular occupant").unwrap();
        symlink("/", parking(1)).unwrap();
        fs::create_dir(parking(2)).unwrap();
        fs::set_permissions(parking(2), Permissions::from_mode(0o777)).unwrap();

        identity.archive_previous(installation, fixture.previous.id).unwrap();
        identity.restore_previous(installation, fixture.previous.id).unwrap();

        assert_eq!(fs::read(parking(0)).unwrap(), b"regular occupant");
        assert!(fs::symlink_metadata(parking(1)).unwrap().file_type().is_symlink());
        assert_eq!(
            fs::symlink_metadata(parking(2)).unwrap().permissions().mode() & 0o7777,
            0o777
        );
        assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
        assert!(parking(3).is_dir(), "the first safe free parking name was not used");
        assert!(fs::read_dir(parking(3)).unwrap().next().is_none());
    }

    #[test]
    fn retained_previous_parking_scan_uses_the_final_bounded_candidate() {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let roots = installation.root_path("");
        let staged = installation.staging_path("usr");
        let token = recovery_tree_token(&staged);
        let parking = |index| roots.join(format!(".previous-slot-{}-{token}-{index}", fixture.previous.id));

        for index in 0..255 {
            fs::write(parking(index), b"occupied").unwrap();
        }

        identity.archive_previous(installation, fixture.previous.id).unwrap();
        identity.restore_previous(installation, fixture.previous.id).unwrap();

        assert!(parking(255).is_dir(), "the final bounded parking name was not used");
        assert!(fs::read_dir(parking(255)).unwrap().next().is_none());
        assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
        identity.verify_previous_for_recovery(&staged).unwrap();
    }

    #[test]
    fn retained_previous_parking_exhaustion_preserves_both_namespaces() {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let roots = installation.root_path("");
        let staged = installation.staging_path("usr");
        let previous_inode = fs::symlink_metadata(&staged).unwrap().ino();
        let token = recovery_tree_token(&staged);
        let parking = |index| roots.join(format!(".previous-slot-{}-{token}-{index}", fixture.previous.id));

        for index in 0..256 {
            fs::write(parking(index), b"occupied").unwrap();
        }

        let failure = identity
            .archive_previous(installation, fixture.previous.id)
            .unwrap_err();

        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
        assert!(
            format!("{failure:?}").contains("PreviousArchiveParkingExhausted"),
            "unexpected bounded-scan failure: {failure:?}"
        );
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
        assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
        identity.verify_previous_for_recovery(&staged).unwrap();
        for index in 0..256 {
            assert_eq!(fs::read(parking(index)).unwrap(), b"occupied");
        }
    }

    #[test]
    fn retained_previous_restore_retirement_faults_resume_without_a_second_rename() {
        for point in [
            crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeSlotRetire,
            crate::transition_identity::RetainedPreviousMoveFaultPoint::RootsAfterSlotRetireSync,
            crate::transition_identity::RetainedPreviousMoveFaultPoint::FinalSlotRetirementRevalidation,
        ] {
            let fixture = stateful_transition_fixture(false);
            let identity = exchanged_stateful_identity(&fixture);
            let installation = &fixture.client.installation;
            let staged = installation.staging_path("usr");
            let slot = installation.root_path(fixture.previous.id.to_string());
            let archived = slot.join("usr");
            identity.archive_previous(installation, fixture.previous.id).unwrap();
            let previous_inode = fs::symlink_metadata(&archived).unwrap().ino();

            crate::transition_identity::arm_retained_previous_move_fault(point);
            let failure = identity
                .restore_previous(installation, fixture.previous.id)
                .unwrap_err();
            assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Applied);
            assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);

            identity
                .finish_applied_previous_restore(installation, fixture.previous.id)
                .unwrap();
            assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
            assert!(!slot.exists(), "retirement resume left state name after {point:?}");
        }

        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let slot = installation.root_path(fixture.previous.id.to_string());
        identity.archive_previous(installation, fixture.previous.id).unwrap();
        crate::transition_identity::arm_retained_previous_move_fault(
            crate::transition_identity::RetainedPreviousMoveFaultPoint::AfterSlotRetire,
        );
        identity.restore_previous(installation, fixture.previous.id).unwrap();
        assert!(
            !slot.exists(),
            "applied retirement evidence must supersede its syscall error"
        );
    }

    #[test]
    fn retained_previous_moves_adopt_exact_pre_syscall_archive_and_restore_layouts() {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let slot = installation.root_path(fixture.previous.id.to_string());
        let archived = slot.join("usr");
        let previous_inode = fs::symlink_metadata(&staged).unwrap().ino();

        let hook_staged = staged.clone();
        let hook_archived = archived.clone();
        crate::transition_identity::arm_before_retained_previous_move_rename(move || {
            fs::rename(&hook_staged, &hook_archived).unwrap();
        });
        identity.archive_previous(installation, fixture.previous.id).unwrap();
        assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);

        let hook_staged = staged.clone();
        let hook_archived = archived.clone();
        crate::transition_identity::arm_before_retained_previous_move_rename(move || {
            fs::rename(&hook_archived, &hook_staged).unwrap();
        });
        identity.restore_previous(installation, fixture.previous.id).unwrap();
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
        assert!(!slot.exists());
    }

    #[test]
    fn retained_previous_slot_retirement_preserves_a_racing_replacement() {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let slot = installation.root_path(fixture.previous.id.to_string());
        let archived = slot.join("usr");
        let displaced = installation.root_path("displaced-retained-previous-slot");
        identity.archive_previous(installation, fixture.previous.id).unwrap();
        let previous_inode = fs::symlink_metadata(&archived).unwrap().ino();

        let hook_slot = slot.clone();
        let hook_displaced = displaced.clone();
        crate::transition_identity::arm_before_previous_slot_retirement_rename(move || {
            fs::rename(&hook_slot, &hook_displaced).unwrap();
            fs::create_dir(&hook_slot).unwrap();
            fs::write(hook_slot.join("foreign"), b"must survive").unwrap();
        });
        let failure = identity
            .restore_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Applied);
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
        assert!(displaced.is_dir(), "retained exact slot was destroyed");
        assert!(
            !slot.exists(),
            "racing replacement should have been retired, not deleted"
        );
        let replacements = previous_slot_parking_paths(installation, fixture.previous.id)
            .into_iter()
            .filter(|path| path.join("foreign").exists())
            .collect::<Vec<_>>();
        assert_eq!(replacements.len(), 1);
        assert_eq!(fs::read(replacements[0].join("foreign")).unwrap(), b"must survive");
        assert!(
            identity
                .finish_applied_previous_restore(installation, fixture.previous.id)
                .is_err()
        );
        assert_eq!(fs::read(replacements[0].join("foreign")).unwrap(), b"must survive");
    }

    #[test]
    fn previous_archive_abort_retirement_faults_resume_in_production_recovery() {
        for retirement_point in [
            crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeSlotRetire,
            crate::transition_identity::RetainedPreviousMoveFaultPoint::AfterSlotRetire,
            crate::transition_identity::RetainedPreviousMoveFaultPoint::RootsAfterSlotRetireSync,
            crate::transition_identity::RetainedPreviousMoveFaultPoint::FinalSlotRetirementRevalidation,
        ] {
            let fixture = stateful_transition_fixture(false);
            let mut armed = false;
            let error = fixture
                .client
                .apply_stateful_blit_with_checkpoint(
                    vfs(Vec::new()).unwrap(),
                    &fixture.candidate,
                    Some(fixture.previous.id),
                    generated_system_snapshot("candidate-package"),
                    |checkpoint| {
                        if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                            armed = true;
                            crate::transition_identity::arm_retained_previous_move_faults(&[
                                crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeRename,
                                retirement_point,
                            ]);
                        }
                        Ok(())
                    },
                )
                .unwrap_err();

            assert!(armed, "archive boundary was not reached for {retirement_point:?}");
            assert!(
                matches!(error, Error::StatefulTransitionUsrRestored { .. }),
                "archive-abort retirement did not resume after {retirement_point:?}: {error:#?}"
            );
            assert!(
                !fixture
                    .client
                    .installation
                    .root_path(fixture.previous.id.to_string())
                    .exists(),
                "canonical previous-state slot survived {retirement_point:?}"
            );
            assert_fresh_candidate_quarantined_and_invalidated(&fixture);
        }
    }

    #[test]
    fn applied_previous_archive_and_restore_faults_use_full_client_suffix_routing() {
        let fixture = stateful_transition_fixture(false);
        let mut archive_armed = false;
        let mut restore_armed = false;
        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| match checkpoint {
                    StatefulTransitionCheckpoint::BeforePreviousStateArchive => {
                        archive_armed = true;
                        crate::transition_identity::arm_retained_previous_move_fault(
                            crate::transition_identity::RetainedPreviousMoveFaultPoint::SourceParentSync,
                        );
                        Ok(())
                    }
                    StatefulTransitionCheckpoint::AfterPreviousStateArchive => {
                        restore_armed = true;
                        crate::transition_identity::arm_retained_previous_move_fault(
                            crate::transition_identity::RetainedPreviousMoveFaultPoint::RootsAfterSlotRetireSync,
                        );
                        Err(injected_state_transition_error("force compensating restore"))
                    }
                    _ => Ok(()),
                },
            )
            .unwrap_err();

        assert!(archive_armed && restore_armed);
        assert!(
            matches!(error, Error::StatefulTransitionUsrRestored { .. }),
            "{error:#?}"
        );
        assert!(
            !fixture
                .client
                .installation
                .root_path(fixture.previous.id.to_string())
                .exists()
        );
        assert_fresh_candidate_quarantined_and_invalidated(&fixture);
    }

    #[test]
    fn retained_previous_moves_reject_roots_and_restore_staging_substitution() {
        {
            let fixture = stateful_transition_fixture(false);
            let identity = exchanged_stateful_identity(&fixture);
            let installation = &fixture.client.installation;
            let roots = installation.root_path("");
            let displaced_roots = roots.parent().unwrap().join("displaced-root");
            let hook_roots = roots.clone();
            let hook_displaced = displaced_roots.clone();
            crate::transition_identity::arm_before_retained_previous_move_rename(move || {
                fs::rename(&hook_roots, &hook_displaced).unwrap();
                fs::create_dir(&hook_roots).unwrap();
                fs::set_permissions(&hook_roots, Permissions::from_mode(0o700)).unwrap();
            });

            let failure = identity
                .archive_previous(installation, fixture.previous.id)
                .unwrap_err();
            assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Ambiguous);
            identity
                .verify_previous_for_recovery(&displaced_roots.join("staging/usr"))
                .unwrap();
            assert!(fs::read_dir(&roots).unwrap().next().is_none());
        }

        {
            let fixture = stateful_transition_fixture(false);
            let identity = exchanged_stateful_identity(&fixture);
            let installation = &fixture.client.installation;
            let staging = installation.staging_dir();
            let displaced_staging = installation.root_path("displaced-staging");
            let archived = installation.root_path(fixture.previous.id.to_string()).join("usr");
            identity.archive_previous(installation, fixture.previous.id).unwrap();
            let previous_inode = fs::symlink_metadata(&archived).unwrap().ino();
            let hook_staging = staging.clone();
            let hook_displaced = displaced_staging.clone();
            crate::transition_identity::arm_before_retained_previous_move_rename(move || {
                fs::rename(&hook_staging, &hook_displaced).unwrap();
                fs::create_dir(&hook_staging).unwrap();
                fs::set_permissions(&hook_staging, Permissions::from_mode(0o700)).unwrap();
            });

            let failure = identity
                .restore_previous(installation, fixture.previous.id)
                .unwrap_err();
            assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Ambiguous);
            assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);
            assert!(fs::read_dir(&staging).unwrap().next().is_none());
            assert!(fs::read_dir(&displaced_staging).unwrap().next().is_none());
        }
    }

    #[test]
    fn fresh_identity_can_archive_after_a_complete_compensating_recovery() {
        let fixture = stateful_transition_fixture(false);
        let first_error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterPreviousStateArchive {
                        Err(injected_state_transition_error("force first compensating recovery"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
        assert!(matches!(first_error, Error::StatefulTransitionUsrRestored { .. }));
        assert_fresh_candidate_quarantined_and_invalidated(&fixture);

        let next = fixture.client.state_db.add(&[], Some("next candidate"), None).unwrap();
        record_state_id(&fixture.client.installation.staging_dir(), next.id).unwrap();
        let staged = fixture.client.installation.staging_path("usr");
        let mut active_state =
            active_state_authority::ActiveStateAuthority::acquire(&fixture.client.installation).unwrap();
        let identity = fixture.client.prepare_stateful_tree_identity(&staged, next.id).unwrap();
        let metadata =
            candidate_metadata::decorate_stateful(&identity, &generated_system_snapshot("next-package")).unwrap();
        active_state
            .refresh_after_tree_identity_preparation(&fixture.client.installation)
            .unwrap();
        let live_root_abi = preflight_root_links(&fixture.client.installation.root).unwrap();
        let mut no_fault = |_| Ok(());
        fixture
            .client
            .commit_stateful_staging(
                &vfs(Vec::new()).unwrap(),
                &next,
                Some(&fixture.previous),
                StatefulCandidateOrigin::Fresh,
                true,
                false,
                false,
                &identity,
                Some(&metadata),
                live_root_abi,
                &active_state,
                &mut no_fault,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
            next.id.to_string()
        );
        assert_eq!(
            fs::read_to_string(
                fixture
                    .client
                    .installation
                    .root_path(fixture.previous.id.to_string())
                    .join("usr/.stateID")
            )
            .unwrap(),
            fixture.previous.id.to_string()
        );
    }

    #[test]
    fn retained_previous_archive_never_adopts_an_ambient_empty_state_slot() {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let slot = installation.root_path(fixture.previous.id.to_string());
        for invalid in [state::Id::from(0), state::Id::from(-1)] {
            let failure = identity.archive_previous(installation, invalid).unwrap_err();
            assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
        }
        fs::create_dir(&slot).unwrap();
        fs::set_permissions(&slot, Permissions::from_mode(0o700)).unwrap();
        let ambient_inode = fs::symlink_metadata(&slot).unwrap().ino();

        let failure = identity
            .archive_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
        assert_eq!(fs::symlink_metadata(&slot).unwrap().ino(), ambient_inode);
        assert_eq!(fs::read_dir(&slot).unwrap().count(), 0);
        identity.verify_previous_for_recovery(&staged).unwrap();
    }

    #[test]
    fn retained_previous_archive_rejects_slot_replacement_before_retention() {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let roots = installation.root_path("");
        let slot = installation.root_path(fixture.previous.id.to_string());
        let displaced = installation.root_path("displaced-fresh-previous-slot");
        let hook_roots = roots.clone();
        let hook_displaced = displaced.clone();
        crate::transition_identity::arm_before_previous_archive_slot_reopen(move || {
            let parked = fs::read_dir(&hook_roots)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(".previous-slot-"))
                })
                .expect("private previous-state slot must exist before reopen");
            fs::rename(&parked, &hook_displaced).unwrap();
            fs::create_dir(&parked).unwrap();
            fs::set_permissions(&parked, Permissions::from_mode(0o700)).unwrap();
        });

        let failure = identity
            .archive_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
        assert!(displaced.is_dir());
        assert!(!slot.exists());
        identity.verify_previous_for_recovery(&staged).unwrap();

        // The replaced provisional name is inert. A new bounded parking name
        // can still be prepared and published to the canonical state slot.
        identity.archive_previous(installation, fixture.previous.id).unwrap();
        identity.restore_previous(installation, fixture.previous.id).unwrap();
        assert!(!slot.exists());
    }

    #[test]
    fn retained_previous_archive_rejects_state_slot_parent_substitution_before_rename() {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let slot = installation.root_path(fixture.previous.id.to_string());
        let displaced = installation.root_path("displaced-previous-slot");
        let hook_slot = slot.clone();
        let hook_displaced = displaced.clone();
        crate::transition_identity::arm_before_retained_previous_move_rename(move || {
            fs::rename(&hook_slot, &hook_displaced).unwrap();
            fs::create_dir(&hook_slot).unwrap();
            fs::set_permissions(&hook_slot, Permissions::from_mode(0o700)).unwrap();
        });

        let failure = identity
            .archive_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Ambiguous);
        assert!(displaced.is_dir());
        assert_eq!(fs::read_dir(&slot).unwrap().count(), 0);
        identity.verify_previous_for_recovery(&staged).unwrap();
    }

    #[test]
    fn retained_previous_archive_rejects_same_token_child_substitution_before_rename() {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let displaced = installation.staging_path("displaced-previous-usr");
        let replacement_token = recovery_tree_token(&staged);
        let hook_staged = staged.clone();
        let hook_displaced = displaced.clone();
        crate::transition_identity::arm_before_retained_previous_move_rename(move || {
            fs::rename(&hook_staged, &hook_displaced).unwrap();
            fs::create_dir(&hook_staged).unwrap();
            fs::set_permissions(&hook_staged, Permissions::from_mode(0o755)).unwrap();
            fs::copy(hook_displaced.join(".cast-tree-id"), hook_staged.join(".cast-tree-id")).unwrap();
        });

        let failure = identity
            .archive_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Ambiguous);
        assert_eq!(recovery_tree_token(&staged), replacement_token);
        identity.verify_previous_for_recovery(&displaced).unwrap();
        let slot = installation.root_path(fixture.previous.id.to_string());
        assert!(slot.is_dir());
        assert!(fs::read_dir(slot).unwrap().next().is_none());
    }

    #[test]
    fn retained_exchange_adopts_applied_forward_and_reverse_moves_when_the_syscall_reports_error() {
        let fixture = stateful_transition_fixture(false);
        let live_usr = fixture.client.installation.root.join("usr");
        let staged_usr = fixture.client.installation.staging_path("usr");
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&staged_usr, fixture.candidate.id)
            .unwrap();
        let candidate_token = recovery_tree_token(&staged_usr);
        let previous_token = recovery_tree_token(&live_usr);

        crate::transition_identity::arm_retained_exchange_fault(
            crate::transition_identity::RetainedExchangeFaultPoint::AfterRename,
        );
        identity.exchange_forward(&fixture.client.installation).unwrap();

        identity.verify_forward_exchange(&live_usr, &staged_usr).unwrap();
        assert_eq!(recovery_tree_token(&live_usr), candidate_token);
        assert_eq!(recovery_tree_token(&staged_usr), previous_token);

        crate::transition_identity::arm_retained_exchange_fault(
            crate::transition_identity::RetainedExchangeFaultPoint::AfterRename,
        );
        identity.exchange_reverse(&fixture.client.installation).unwrap();

        identity.verify_restored(&live_usr, &staged_usr).unwrap();
        assert_eq!(recovery_tree_token(&live_usr), previous_token);
        assert_eq!(recovery_tree_token(&staged_usr), candidate_token);
    }

    #[test]
    fn retained_exchange_error_before_rename_preserves_both_exact_names() {
        let fixture = stateful_transition_fixture(false);
        let live_usr = fixture.client.installation.root.join("usr");
        let staged_usr = fixture.client.installation.staging_path("usr");
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&staged_usr, fixture.candidate.id)
            .unwrap();
        let candidate_token = recovery_tree_token(&staged_usr);
        let previous_token = recovery_tree_token(&live_usr);

        crate::transition_identity::arm_retained_exchange_fault(
            crate::transition_identity::RetainedExchangeFaultPoint::BeforeRename,
        );
        let failure = identity.exchange_forward(&fixture.client.installation).unwrap_err();

        assert_eq!(failure.outcome(), RetainedExchangeOutcome::NotApplied);
        identity.verify_pre_exchange(&staged_usr, &live_usr).unwrap();
        assert_eq!(recovery_tree_token(&staged_usr), candidate_token);
        assert_eq!(recovery_tree_token(&live_usr), previous_token);
    }

    #[test]
    fn retained_exchange_parent_replacement_is_rejected_before_the_syscall() {
        let fixture = stateful_transition_fixture(false);
        let installation = &fixture.client.installation;
        let live_usr = installation.root.join("usr");
        let staging = installation.staging_dir();
        let staged_usr = installation.staging_path("usr");
        let displaced = installation.root_path("displaced-retained-exchange-staging");
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&staged_usr, fixture.candidate.id)
            .unwrap();
        let previous_token = recovery_tree_token(&live_usr);
        let candidate_token = recovery_tree_token(&staged_usr);
        let raced_staging = staging.clone();
        let raced_displaced = displaced.clone();

        crate::transition_identity::arm_before_retained_exchange_rename(move || {
            fs::rename(&raced_staging, &raced_displaced).unwrap();
            fs::create_dir(&raced_staging).unwrap();
            fs::create_dir(raced_staging.join("usr")).unwrap();
            fs::write(raced_staging.join("usr/foreign"), b"racing staging tree").unwrap();
        });
        let failure = identity.exchange_forward(installation).unwrap_err();

        assert_eq!(failure.outcome(), RetainedExchangeOutcome::NotApplied);
        assert_eq!(recovery_tree_token(&live_usr), previous_token);
        assert_eq!(recovery_tree_token(&displaced.join("usr")), candidate_token);
        assert_eq!(fs::read(staging.join("usr/foreign")).unwrap(), b"racing staging tree");
    }

    #[test]
    fn retained_exchange_child_substitution_is_rejected_before_the_syscall() {
        let fixture = stateful_transition_fixture(false);
        let installation = &fixture.client.installation;
        let live_usr = installation.root.join("usr");
        let staged_usr = installation.staging_path("usr");
        let displaced = installation.root_path("displaced-retained-exchange-candidate");
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&staged_usr, fixture.candidate.id)
            .unwrap();
        let previous_token = recovery_tree_token(&live_usr);
        let candidate_token = recovery_tree_token(&staged_usr);
        let raced_staged = staged_usr.clone();
        let raced_displaced = displaced.clone();

        crate::transition_identity::arm_before_retained_exchange_rename(move || {
            fs::rename(&raced_staged, &raced_displaced).unwrap();
            fs::create_dir(&raced_staged).unwrap();
            fs::write(raced_staged.join("foreign"), b"substituted candidate").unwrap();
        });
        let failure = identity.exchange_forward(installation).unwrap_err();

        assert_eq!(failure.outcome(), RetainedExchangeOutcome::NotApplied);
        assert_eq!(recovery_tree_token(&live_usr), previous_token);
        assert_eq!(recovery_tree_token(&displaced), candidate_token);
        assert_eq!(fs::read(staged_usr.join("foreign")).unwrap(), b"substituted candidate");
    }

    #[test]
    fn retained_exchange_post_move_faults_run_the_swapped_recovery_path() {
        for point in [
            crate::transition_identity::RetainedExchangeFaultPoint::StagingParentSync,
            crate::transition_identity::RetainedExchangeFaultPoint::InstallationRootSync,
            crate::transition_identity::RetainedExchangeFaultPoint::FinalRevalidation,
        ] {
            let fixture = stateful_transition_fixture(false);
            crate::transition_identity::arm_retained_exchange_fault(point);

            let error = fixture
                .client
                .apply_stateful_blit_with_checkpoint(
                    vfs(Vec::new()).unwrap(),
                    &fixture.candidate,
                    Some(fixture.previous.id),
                    generated_system_snapshot("candidate-package"),
                    |_| Ok(()),
                )
                .unwrap_err();

            assert!(
                matches!(error, Error::StatefulTransitionUsrRestored { .. }),
                "unexpected recovery result after {point:?}: {error:#?}"
            );
            assert_fresh_candidate_quarantined_and_invalidated(&fixture);
        }
    }

    #[test]
    fn retained_reverse_exchange_post_move_faults_finish_without_a_second_exchange() {
        for point in [
            crate::transition_identity::RetainedExchangeFaultPoint::StagingParentSync,
            crate::transition_identity::RetainedExchangeFaultPoint::InstallationRootSync,
            crate::transition_identity::RetainedExchangeFaultPoint::FinalRevalidation,
        ] {
            let fixture = stateful_transition_fixture(false);
            let mut primary_injected = false;

            let error = fixture
                .client
                .apply_stateful_blit_with_checkpoint(
                    vfs(Vec::new()).unwrap(),
                    &fixture.candidate,
                    Some(fixture.previous.id),
                    generated_system_snapshot("candidate-package"),
                    |checkpoint| match checkpoint {
                        StatefulTransitionCheckpoint::AfterUsrExchange if !primary_injected => {
                            primary_injected = true;
                            Err(injected_state_transition_error("force compensating reverse exchange"))
                        }
                        StatefulTransitionCheckpoint::BeforeRecoveryUsrExchange => {
                            crate::transition_identity::arm_retained_exchange_fault(point);
                            Ok(())
                        }
                        _ => Ok(()),
                    },
                )
                .unwrap_err();

            assert!(primary_injected, "forward exchange fault was not reached for {point:?}");
            assert!(
                matches!(error, Error::StatefulTransitionUsrRestored { .. }),
                "reverse durability completion failed after {point:?}: {error:#?}"
            );
            assert_fresh_candidate_quarantined_and_invalidated(&fixture);
        }
    }

    fn assert_recovered_stateful_transition(fixture: &StatefulTransitionFixture) {
        let installation = &fixture.client.installation;
        assert_eq!(
            fs::read_to_string(installation.root.join("usr/.stateID")).unwrap(),
            fixture.previous.id.to_string()
        );
        assert_generated_snapshot(
            &system_model::snapshot_path(&installation.root),
            &fixture.previous_snapshot,
            "previous-package",
        );

        let candidate_root = installation.root_path(fixture.candidate.id.to_string());
        assert_eq!(
            fs::read_to_string(candidate_root.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert_generated_snapshot(
            &system_model::snapshot_path(&candidate_root),
            &fixture.candidate_snapshot,
            "candidate-package",
        );
        assert!(!installation.staging_path("usr").exists());
        assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
    }

    fn assert_fresh_candidate_quarantined_and_invalidated(fixture: &StatefulTransitionFixture) {
        let installation = &fixture.client.installation;
        assert_eq!(
            fs::read_to_string(installation.root.join("usr/.stateID")).unwrap(),
            fixture.previous.id.to_string()
        );
        assert_generated_snapshot(
            &system_model::snapshot_path(&installation.root),
            &fixture.previous_snapshot,
            "previous-package",
        );
        assert!(fixture.client.state_db.get(fixture.candidate.id).is_err());
        assert!(!installation.root_path(fixture.candidate.id.to_string()).exists());
        assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
        assert!(!installation.staging_path("usr").exists());

        let quarantines = fs::read_dir(installation.state_quarantine_dir())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(quarantines.len(), 1);
        let quarantine = &quarantines[0];
        assert!(
            quarantine
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("failed-new-state-{}-", fixture.candidate.id))
        );
        assert_eq!(
            fs::read_to_string(quarantine.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert_generated_snapshot(
            &system_model::snapshot_path(quarantine),
            &fixture.candidate_snapshot,
            "candidate-package",
        );
    }

    #[test]
    fn quarantine_durability_faults_never_invalidate_the_fresh_candidate() {
        use crate::transition_identity::QuarantineFaultPoint;

        for fault in [
            QuarantineFaultPoint::CandidatePreSync,
            QuarantineFaultPoint::SlotSync,
            QuarantineFaultPoint::QuarantineBaseSync,
            QuarantineFaultPoint::Rename,
            QuarantineFaultPoint::MovedCandidateSync,
            QuarantineFaultPoint::SourceParentSync,
            QuarantineFaultPoint::DestinationParentSync,
            QuarantineFaultPoint::FinalRevalidation,
        ] {
            let fixture = stateful_transition_fixture(false);
            let mut candidate_token = None;
            crate::transition_identity::arm_quarantine_faults(fault, 2);

            let error = fixture
                .client
                .apply_stateful_blit_with_checkpoint(
                    vfs(Vec::new()).unwrap(),
                    &fixture.candidate,
                    Some(fixture.previous.id),
                    generated_system_snapshot("candidate-package"),
                    |checkpoint| {
                        if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                            candidate_token = Some(recovery_tree_token(&fixture.client.installation.root.join("usr")));
                            Err(injected_state_transition_error("force failed-candidate quarantine"))
                        } else {
                            Ok(())
                        }
                    },
                )
                .unwrap_err();

            assert!(
                matches!(
                    &error,
                    Error::StatefulTransitionRecoveryFailed {
                        preserve_candidate: Some(_),
                        ..
                    }
                ),
                "fault {fault:?} unexpectedly completed preservation: {error:#?}"
            );
            assert_eq!(
                fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
                fixture.candidate.id,
                "fault {fault:?} deleted the only candidate correlation"
            );
            assert_eq!(
                fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
                fixture.previous.id.to_string()
            );

            let expected = candidate_token.unwrap();
            let mut retained_tokens = Vec::new();
            let staged = fixture.client.installation.staging_path("usr");
            if staged.exists() {
                retained_tokens.push(recovery_tree_token(&staged));
            }
            for entry in fs::read_dir(fixture.client.installation.state_quarantine_dir()).unwrap() {
                let usr = entry.unwrap().path().join("usr");
                if usr.exists() {
                    retained_tokens.push(recovery_tree_token(&usr));
                }
            }
            assert_eq!(
                retained_tokens,
                [expected],
                "fault {fault:?} lost or duplicated the candidate tree"
            );
        }
    }

    #[test]
    fn single_quarantine_durability_fault_is_resumed_before_invalidation() {
        use crate::transition_identity::QuarantineFaultPoint;

        for fault in [
            QuarantineFaultPoint::CandidatePreSync,
            QuarantineFaultPoint::SlotSync,
            QuarantineFaultPoint::QuarantineBaseSync,
            QuarantineFaultPoint::Rename,
            QuarantineFaultPoint::MovedCandidateSync,
            QuarantineFaultPoint::SourceParentSync,
            QuarantineFaultPoint::DestinationParentSync,
            QuarantineFaultPoint::FinalRevalidation,
        ] {
            let fixture = stateful_transition_fixture(false);
            let mut token = None;

            crate::transition_identity::arm_quarantine_fault(fault);
            let error = fixture
                .client
                .apply_stateful_blit_with_checkpoint(
                    vfs(Vec::new()).unwrap(),
                    &fixture.candidate,
                    Some(fixture.previous.id),
                    generated_system_snapshot("candidate-package"),
                    |checkpoint| {
                        if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                            token = Some(recovery_tree_token(&fixture.client.installation.root.join("usr")));
                            Err(injected_state_transition_error("force resumable quarantine fault"))
                        } else {
                            Ok(())
                        }
                    },
                )
                .unwrap_err();

            assert!(
                matches!(error, Error::StatefulTransitionUsrRestored { .. }),
                "single fault {fault:?} did not resume through production recovery: {error:#?}"
            );
            assert!(fixture.client.state_db.get(fixture.candidate.id).is_err());
            assert!(!fixture.client.installation.staging_path("usr").exists());
            let quarantines = fs::read_dir(fixture.client.installation.state_quarantine_dir())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            assert_eq!(quarantines.len(), 1);
            assert_eq!(recovery_tree_token(&quarantines[0].join("usr")), token.unwrap());
        }
    }

    #[test]
    fn quarantine_is_revalidated_after_the_invalidation_checkpoint() {
        let fixture = stateful_transition_fixture(false);
        let mut displaced = None;

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| match checkpoint {
                    StatefulTransitionCheckpoint::AfterUsrExchange => {
                        Err(injected_state_transition_error("force quarantine"))
                    }
                    StatefulTransitionCheckpoint::BeforeRecoveryCandidateInvalidation => {
                        let quarantine = fs::read_dir(fixture.client.installation.state_quarantine_dir())
                            .unwrap()
                            .next()
                            .unwrap()
                            .unwrap()
                            .path();
                        let moved = quarantine.with_extension("displaced");
                        fs::rename(&quarantine, &moved).unwrap();
                        fs::create_dir(&quarantine).unwrap();
                        fs::write(quarantine.join("sentinel"), b"substituted slot").unwrap();
                        displaced = Some((quarantine, moved));
                        Ok(())
                    }
                    _ => Ok(()),
                },
            )
            .unwrap_err();

        assert!(matches!(
            error,
            Error::StatefulTransitionRecoveryFailed {
                invalidate_candidate: Some(_),
                ..
            }
        ));
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id
        );
        let (replacement, moved) = displaced.unwrap();
        assert_eq!(fs::read(replacement.join("sentinel")).unwrap(), b"substituted slot");
        assert_eq!(
            fs::read_to_string(moved.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
    }

    #[test]
    fn deterministic_quarantine_name_collision_preserves_foreign_entry_and_database_row() {
        let fixture = stateful_transition_fixture(false);
        let mut collision = None;

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                        let token = recovery_tree_token(&fixture.client.installation.root.join("usr"));
                        let path = fixture
                            .client
                            .installation
                            .state_quarantine_dir()
                            .join(format!("failed-new-state-{}-{token}", fixture.candidate.id));
                        fs::create_dir(&path).unwrap();
                        fs::write(path.join("sentinel"), b"foreign quarantine occupant").unwrap();
                        collision = Some(path);
                        Err(injected_state_transition_error("quarantine collision"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(matches!(
            &error,
            Error::StatefulTransitionRecoveryFailed {
                preserve_candidate: Some(_),
                ..
            }
        ));
        let collision = collision.unwrap();
        assert_eq!(
            fs::read(collision.join("sentinel")).unwrap(),
            b"foreign quarantine occupant"
        );
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id
        );
        assert_eq!(
            fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
    }

    #[test]
    fn empty_deterministic_quarantine_collision_is_never_adopted() {
        let fixture = stateful_transition_fixture(false);
        let mut collision = None;

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                        let token = recovery_tree_token(&fixture.client.installation.root.join("usr"));
                        let path = fixture
                            .client
                            .installation
                            .state_quarantine_dir()
                            .join(format!("failed-new-state-{}-{token}", fixture.candidate.id));
                        fs::create_dir(&path).unwrap();
                        fs::set_permissions(&path, Permissions::from_mode(0o700)).unwrap();
                        collision = Some(path);
                        Err(injected_state_transition_error("empty quarantine collision"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(matches!(
            &error,
            Error::StatefulTransitionRecoveryFailed {
                preserve_candidate: Some(_),
                ..
            }
        ));
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id
        );
        assert!(fixture.client.installation.staging_path("usr").is_dir());
        assert_eq!(fs::read_dir(collision.unwrap()).unwrap().count(), 0);
    }

    #[test]
    fn quarantine_slot_creation_rejects_replacement_before_retention() {
        let fixture = stateful_transition_fixture(false);
        let quarantine_root = fixture.client.installation.state_quarantine_dir();
        let observed: std::rc::Rc<std::cell::RefCell<Option<(PathBuf, PathBuf)>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let hook_observed = observed.clone();
        crate::transition_identity::arm_before_quarantine_slot_reopen(move || {
            let created = fs::read_dir(&quarantine_root)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .next()
                .expect("quarantine slot must have been created before reopen");
            let displaced = created.with_extension("created");
            fs::rename(&created, &displaced).unwrap();
            fs::create_dir(&created).unwrap();
            fs::set_permissions(&created, Permissions::from_mode(0o700)).unwrap();
            hook_observed.replace(Some((created, displaced)));
        });

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                        Err(injected_state_transition_error("replace fresh quarantine slot"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(matches!(
            &error,
            Error::StatefulTransitionRecoveryFailed {
                preserve_candidate: Some(_),
                ..
            }
        ));
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id
        );
        assert_eq!(
            fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        let (replacement, displaced) = observed.as_ref().borrow().clone().unwrap();
        assert_eq!(fs::read_dir(replacement).unwrap().count(), 0);
        assert_eq!(fs::read_dir(displaced).unwrap().count(), 0);
    }

    #[test]
    fn stateful_tree_tokens_follow_their_logical_trees_through_exchange_and_archive() {
        let fixture = stateful_transition_fixture(true);
        let live_usr = fixture.client.installation.root.join("usr");
        let staged_usr = fixture.client.installation.staging_path("usr");
        let previous_archive = fixture
            .client
            .installation
            .root_path(fixture.previous.id.to_string())
            .join("usr");
        let mut exchanged_tokens = None;

        fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                    let candidate_wrapper = fixture.client.installation.root_path(fixture.candidate.id.to_string());
                    let candidate_token = fs::read_dir(candidate_wrapper)
                        .unwrap()
                        .map(|entry| entry.unwrap().file_name())
                        .find_map(|name| {
                            name.to_string_lossy()
                                .strip_prefix(&format!(".cast-state-slot-{}-", fixture.candidate.id))
                                .map(str::to_owned)
                        })
                        .expect("candidate slot hardlink was present at the exchange boundary");
                    exchanged_tokens = Some((candidate_token, recovery_tree_token(&staged_usr)));
                }
                Ok(())
            })
            .unwrap();

        let (candidate_token, previous_token) = exchanged_tokens.expect("exchange boundary was observed");
        assert_ne!(candidate_token, previous_token);
        let parked = archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id);
        assert_eq!(parked.len(), 1);
        let slot_link = fs::read_dir(&parked[0])
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.file_name().unwrap().to_string_lossy().ends_with(&candidate_token))
            .unwrap();
        assert_eq!(
            fs::symlink_metadata(live_usr.join(".cast-tree-id")).unwrap().ino(),
            fs::symlink_metadata(slot_link).unwrap().ino()
        );
        assert_eq!(recovery_tree_token(&previous_archive), previous_token);
        assert!(!staged_usr.exists());
    }

    #[test]
    fn recovery_never_recreates_a_missing_candidate_tree_marker() {
        let fixture = stateful_transition_fixture(false);
        let candidate_model = generated_system_snapshot("candidate-package");
        let live_usr = fixture.client.installation.root.join("usr");
        let staged_usr = fixture.client.installation.staging_path("usr");

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                candidate_model,
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                        fs::remove_file(live_usr.join(".cast-tree-id")).unwrap();
                        Err(injected_state_transition_error("marker removed after exchange"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(
            matches!(
                &error,
                Error::StatefulTransitionRecoveryFailed {
                    candidate,
                    previous: Some(previous),
                    reverse_exchange: Some(_),
                    ..
                } if *candidate == fixture.candidate.id && *previous == fixture.previous.id
            ),
            "unexpected recovery result: {error:#?}"
        );
        assert!(!live_usr.join(".cast-tree-id").exists());
        assert!(staged_usr.join(".cast-tree-id").is_file());
        assert_eq!(
            fs::read_to_string(live_usr.join(".stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert_eq!(
            fs::read_to_string(staged_usr.join(".stateID")).unwrap(),
            fixture.previous.id.to_string()
        );
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id,
            "an unauthenticated candidate must retain its database row"
        );
    }

    #[test]
    fn unresolved_journal_evidence_blocks_marker_publication_before_activation() {
        let fixture = stateful_transition_fixture(false);
        let journal =
            crate::transition_journal::TransitionJournalStore::open(&fixture.client.installation.root).unwrap();
        drop(journal);
        let canonical = fixture.client.installation.root.join(".cast/journal/state-transition");
        fs::write(&canonical, b"not-a-canonical-transition-record").unwrap();
        fs::set_permissions(&canonical, Permissions::from_mode(0o600)).unwrap();

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |_| Ok(()),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            Error::StatefulTreeIdentityPreparationFailed {
                candidate,
                previous: Some(previous),
                ..
            } if candidate == fixture.candidate.id && previous == fixture.previous.id
        ));
        assert!(!fixture.client.installation.root.join("usr/.cast-tree-id").exists());
        assert!(!fixture.client.installation.staging_path("usr/.cast-tree-id").exists());
        assert_eq!(fs::read(&canonical).unwrap(), b"not-a-canonical-transition-record");
    }

    #[test]
    fn orphan_transition_row_blocks_marker_publication_before_activation() {
        let fixture = stateful_transition_fixture(false);
        let transition = state::TransitionId::generate().unwrap();
        fixture
            .client
            .state_db
            .add_with_transition(&transition, &[], Some("orphan"), None)
            .unwrap();

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |_| Ok(()),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            Error::StatefulTreeIdentityPreparationFailed {
                candidate,
                previous: Some(previous),
                ..
            } if candidate == fixture.candidate.id && previous == fixture.previous.id
        ));
        assert!(!fixture.client.installation.root.join("usr/.cast-tree-id").exists());
        assert!(!fixture.client.installation.staging_path("usr/.cast-tree-id").exists());
        assert!(
            !fixture
                .client
                .installation
                .root
                .join(".cast/journal/state-transition")
                .exists()
        );
    }

    #[test]
    fn first_install_synthesizes_syncs_marks_and_exchanges_an_empty_previous_usr() {
        let temporary = tempfile::tempdir().unwrap();
        let client = stateful_test_client(temporary.path());
        let candidate = client.state_db.add(&[], Some("first state"), None).unwrap();
        record_state_id(&client.installation.staging_dir(), candidate.id).unwrap();
        let candidate_usr = client.installation.staging_path("usr");
        let live_usr = client.installation.root.join("usr");
        assert!(!live_usr.exists());

        let mut active_state = active_state_authority::ActiveStateAuthority::acquire(&client.installation).unwrap();
        let tree_identity = client
            .prepare_stateful_tree_identity(&candidate_usr, candidate.id)
            .unwrap();
        let metadata_proof =
            candidate_metadata::decorate_stateful(&tree_identity, &generated_system_snapshot("first-state-package"))
                .unwrap();
        active_state
            .refresh_after_tree_identity_preparation(&client.installation)
            .unwrap();
        let live_root_abi = preflight_root_links(&client.installation.root).unwrap();
        tree_identity.verify_pre_exchange(&candidate_usr, &live_usr).unwrap();
        let synthesized_token = recovery_tree_token(&live_usr);
        let candidate_token = recovery_tree_token(&candidate_usr);
        assert_ne!(synthesized_token, candidate_token);
        let metadata = fs::symlink_metadata(&live_usr).unwrap();
        assert!(metadata.file_type().is_dir());
        assert_eq!(metadata.uid(), unsafe { nix::libc::geteuid() });
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o755);
        let entries = fs::read_dir(&live_usr)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, [OsString::from(".cast-tree-id")]);

        client
            .commit_stateful_staging(
                &vfs(Vec::new()).unwrap(),
                &candidate,
                None,
                StatefulCandidateOrigin::Fresh,
                false,
                false,
                false,
                &tree_identity,
                Some(&metadata_proof),
                live_root_abi,
                &active_state,
                &mut |_| Ok(()),
            )
            .unwrap();

        assert_eq!(recovery_tree_token(&live_usr), candidate_token);
        assert_eq!(recovery_tree_token(&candidate_usr), synthesized_token);
        assert_eq!(
            fs::read_to_string(live_usr.join(".stateID")).unwrap(),
            candidate.id.to_string()
        );
        assert_eq!(client.state_db.get(candidate.id).unwrap().id, candidate.id);
        assert!(!client.installation.root.join(".cast/journal/state-transition").exists());
    }

    #[test]
    fn failed_first_install_can_retry_the_exact_marker_only_previous_baseline() {
        let temporary = tempfile::tempdir().unwrap();
        let client = stateful_test_client(temporary.path());
        let live_usr = client.installation.root.join("usr");
        let mut previous_token = None;

        for summary in ["first failed attempt", "retry attempt"] {
            let candidate = client.state_db.add(&[], Some(summary), None).unwrap();
            let mut reached_exchange = false;
            let error = client
                .apply_stateful_blit_with_checkpoint(
                    vfs(Vec::new()).unwrap(),
                    &candidate,
                    None,
                    generated_system_snapshot(summary),
                    |checkpoint| {
                        if checkpoint == StatefulTransitionCheckpoint::BeforeUsrExchange {
                            reached_exchange = true;
                            Err(injected_state_transition_error("fail before first-install exchange"))
                        } else {
                            Ok(())
                        }
                    },
                )
                .unwrap_err();

            assert!(
                reached_exchange,
                "retry stopped before the exchange boundary: {error:#?}"
            );
            assert!(matches!(error, Error::StatefulCandidatePreserved { .. }));
            let token = recovery_tree_token(&live_usr);
            if let Some(previous_token) = &previous_token {
                assert_eq!(
                    &token, previous_token,
                    "retry must adopt the exact durable baseline token"
                );
            } else {
                previous_token = Some(token);
            }
            let entries = fs::read_dir(&live_usr)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            assert_eq!(entries, [OsString::from(".cast-tree-id")]);
        }
    }

    #[test]
    fn first_install_marker_retry_rejects_marker_plus_foreign_content_unchanged() {
        let temporary = tempfile::tempdir().unwrap();
        let client = stateful_test_client(temporary.path());
        let first = client.state_db.add(&[], Some("first attempt"), None).unwrap();
        record_state_id(&client.installation.staging_dir(), first.id).unwrap();
        let identity = client
            .prepare_stateful_tree_identity(&client.installation.staging_path("usr"), first.id)
            .unwrap();
        drop(identity);

        let live_usr = client.installation.root.join("usr");
        let token = recovery_tree_token(&live_usr);
        let foreign = live_usr.join("foreign");
        fs::write(&foreign, b"do not remove").unwrap();

        let error = client
            .prepare_stateful_tree_identity(&client.installation.staging_path("usr"), first.id)
            .unwrap_err();
        assert!(matches!(
            error,
            crate::transition_identity::Error::LiveUsrNotEmpty { .. }
        ));
        assert_eq!(recovery_tree_token(&live_usr), token);
        assert_eq!(fs::read(&foreign).unwrap(), b"do not remove");
    }

    #[test]
    fn first_install_rejects_a_hostile_live_usr_symlink_unchanged() {
        let temporary = tempfile::tempdir().unwrap();
        let client = stateful_test_client(temporary.path());
        let candidate = client.state_db.add(&[], Some("first state"), None).unwrap();
        record_state_id(&client.installation.staging_dir(), candidate.id).unwrap();
        let candidate_usr = client.installation.staging_path("usr");
        let foreign = client.installation.root.join("foreign-usr");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("foreign"), b"untouched").unwrap();
        symlink("foreign-usr", client.installation.root.join("usr")).unwrap();

        let error = client
            .prepare_stateful_tree_identity(&candidate_usr, candidate.id)
            .unwrap_err();
        assert!(matches!(error, crate::transition_identity::Error::LiveUsr { .. }));
        assert!(
            fs::symlink_metadata(client.installation.root.join("usr"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(client.installation.root.join("usr")).unwrap(),
            Path::new("foreign-usr")
        );
        assert_eq!(fs::read(foreign.join("foreign")).unwrap(), b"untouched");
        assert!(!candidate_usr.join(".cast-tree-id").exists());
    }

    #[test]
    fn first_install_rejects_a_preexisting_nonempty_unmanaged_usr_unchanged() {
        let temporary = tempfile::tempdir().unwrap();
        let client = stateful_test_client(temporary.path());
        let candidate = client.state_db.add(&[], Some("first state"), None).unwrap();
        record_state_id(&client.installation.staging_dir(), candidate.id).unwrap();
        let candidate_usr = client.installation.staging_path("usr");
        let live_usr = client.installation.root.join("usr");
        fs::create_dir(&live_usr).unwrap();
        fs::set_permissions(&live_usr, Permissions::from_mode(0o755)).unwrap();
        fs::write(live_usr.join("foreign"), b"untouched").unwrap();

        let error = client
            .prepare_stateful_tree_identity(&candidate_usr, candidate.id)
            .unwrap_err();
        assert!(
            matches!(&error, crate::transition_identity::Error::LiveUsrNotEmpty { .. }),
            "unexpected nonempty live /usr result: {error:#?}"
        );
        assert_eq!(fs::read(live_usr.join("foreign")).unwrap(), b"untouched");
        assert!(!live_usr.join(".cast-tree-id").exists());
        assert!(!candidate_usr.join(".cast-tree-id").exists());
    }

    #[test]
    fn first_install_rejects_a_racing_nonempty_usr_occupant_unchanged() {
        let temporary = tempfile::tempdir().unwrap();
        let client = stateful_test_client(temporary.path());
        let candidate = client.state_db.add(&[], Some("first state"), None).unwrap();
        record_state_id(&client.installation.staging_dir(), candidate.id).unwrap();
        let candidate_usr = client.installation.staging_path("usr");
        let live_usr = client.installation.root.join("usr");
        let raced = live_usr.clone();
        crate::transition_identity::arm_before_live_usr_mkdir(move || {
            fs::create_dir(&raced).unwrap();
            fs::write(raced.join("foreign"), b"racing occupant").unwrap();
        });

        let error = client
            .prepare_stateful_tree_identity(&candidate_usr, candidate.id)
            .unwrap_err();
        assert!(matches!(
            error,
            crate::transition_identity::Error::LiveUsrAppeared { .. }
        ));
        assert_eq!(fs::read(live_usr.join("foreign")).unwrap(), b"racing occupant");
        assert!(!live_usr.join(".cast-tree-id").exists());
        assert!(!candidate_usr.join(".cast-tree-id").exists());
    }

    #[test]
    fn duplicate_permanent_tree_tokens_block_exchange_and_retain_both_trees() {
        let fixture = stateful_transition_fixture(false);
        let candidate_usr = fixture.client.installation.staging_path("usr");
        let live_usr = fixture.client.installation.root.join("usr");
        let journal =
            crate::transition_journal::TransitionJournalStore::open(&fixture.client.installation.root).unwrap();
        assert!(journal.load().unwrap().is_none());
        let candidate_store = crate::tree_marker::TreeMarkerStore::open_path(&candidate_usr).unwrap();
        let candidate_marker = candidate_store.adopt_or_create_before_journal().unwrap();
        candidate_marker.revalidate(&candidate_store).unwrap();
        let frame = fs::read(candidate_usr.join(".cast-tree-id")).unwrap();
        fs::write(live_usr.join(".cast-tree-id"), &frame).unwrap();
        fs::set_permissions(live_usr.join(".cast-tree-id"), Permissions::from_mode(0o444)).unwrap();
        drop(candidate_marker);
        drop(candidate_store);
        drop(journal);

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |_| Ok(()),
            )
            .unwrap_err();

        let Error::StatefulTreeIdentityPreparationFailed { source, .. } = error else {
            panic!("expected durable identity preparation failure");
        };
        let Error::StatefulTreeIdentity { source } = *source else {
            panic!("expected tree identity source");
        };
        assert!(matches!(
            source.downcast_ref::<crate::transition_identity::Error>(),
            Some(crate::transition_identity::Error::DuplicateTreeToken { .. })
        ));
        assert_eq!(fs::read(candidate_usr.join(".cast-tree-id")).unwrap(), frame);
        assert_eq!(fs::read(live_usr.join(".cast-tree-id")).unwrap(), frame);
        assert_eq!(
            fs::read_to_string(candidate_usr.join(".stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert_eq!(
            fs::read_to_string(live_usr.join(".stateID")).unwrap(),
            fixture.previous.id.to_string()
        );
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id
        );
    }

    #[test]
    fn recovery_rejects_same_content_marker_name_substitution_without_repair() {
        let fixture = stateful_transition_fixture(false);
        let candidate_model = generated_system_snapshot("candidate-package");
        let live_usr = fixture.client.installation.root.join("usr");
        let marker_path = live_usr.join(".cast-tree-id");
        let mut replacement = None;

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                candidate_model,
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                        let frame = fs::read(&marker_path).unwrap();
                        let original = fs::symlink_metadata(&marker_path).unwrap().ino();
                        fs::remove_file(&marker_path).unwrap();
                        fs::write(&marker_path, &frame).unwrap();
                        fs::set_permissions(&marker_path, Permissions::from_mode(0o444)).unwrap();
                        let substituted = fs::symlink_metadata(&marker_path).unwrap().ino();
                        assert_ne!(original, substituted);
                        replacement = Some((frame, substituted));
                        Err(injected_state_transition_error("same-content marker substitution"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(matches!(
            error,
            Error::StatefulTransitionRecoveryFailed {
                reverse_exchange: Some(_),
                ..
            }
        ));
        let (frame, inode) = replacement.unwrap();
        assert_eq!(fs::read(&marker_path).unwrap(), frame);
        assert_eq!(fs::symlink_metadata(&marker_path).unwrap().ino(), inode);
        assert_eq!(
            fs::read_to_string(live_usr.join(".stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id
        );
    }

    #[test]
    fn recovery_rejects_whole_directory_same_token_substitution_without_exchange() {
        let fixture = stateful_transition_fixture(false);
        let live_usr = fixture.client.installation.root.join("usr");
        let displaced = fixture.client.installation.root.join("displaced-candidate-usr");
        let mut replacement_identity = None;

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                        let marker = fs::read(live_usr.join(".cast-tree-id")).unwrap();
                        let state_id = fs::read(live_usr.join(".stateID")).unwrap();
                        fs::rename(&live_usr, &displaced).unwrap();
                        fs::create_dir(&live_usr).unwrap();
                        fs::set_permissions(&live_usr, Permissions::from_mode(0o755)).unwrap();
                        fs::write(live_usr.join(".cast-tree-id"), marker).unwrap();
                        fs::set_permissions(live_usr.join(".cast-tree-id"), Permissions::from_mode(0o444)).unwrap();
                        fs::write(live_usr.join(".stateID"), state_id).unwrap();
                        replacement_identity = Some(fs::symlink_metadata(&live_usr).unwrap().ino());
                        Err(injected_state_transition_error(
                            "whole-directory same-token substitution",
                        ))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(matches!(
            error,
            Error::StatefulTransitionRecoveryFailed {
                reverse_exchange: Some(_),
                ..
            }
        ));
        assert_eq!(
            fs::symlink_metadata(&live_usr).unwrap().ino(),
            replacement_identity.unwrap(),
            "recovery must not exchange the substituted directory"
        );
        assert_eq!(recovery_tree_token(&live_usr), recovery_tree_token(&displaced));
        assert_eq!(
            fs::read_to_string(displaced.join(".stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert_eq!(
            fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
            fixture.previous.id.to_string()
        );
    }

    #[test]
    fn missing_live_usr_between_identity_check_and_exchange_is_never_recreated() {
        let fixture = stateful_transition_fixture(false);
        let live_usr = fixture.client.installation.root.join("usr");
        let displaced = fixture.client.installation.root.join("displaced-previous-usr");
        let staged_usr = fixture.client.installation.staging_path("usr");
        let mut expected_tokens = None;

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::BeforeUsrExchange {
                        expected_tokens = Some((recovery_tree_token(&staged_usr), recovery_tree_token(&live_usr)));
                        fs::rename(&live_usr, &displaced).unwrap();
                    }
                    Ok(())
                },
            )
            .unwrap_err();

        assert!(matches!(&error, Error::StatefulCandidatePreserved { .. }), "{error:#?}");
        let (candidate_token, previous_token) = expected_tokens.unwrap();
        assert!(
            !live_usr.exists(),
            "promotion must not synthesize an unmarked exchange target"
        );
        assert_eq!(recovery_tree_token(&displaced), previous_token);
        let quarantines = fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(quarantines.len(), 1);
        assert_eq!(recovery_tree_token(&quarantines[0].join("usr")), candidate_token);
    }

    #[test]
    fn archived_live_root_abi_conflict_precedes_staging_triggers_and_usr_exchange() {
        let fixture = stateful_transition_fixture(true);
        let installation = &fixture.client.installation;
        let foreign = installation.root.join("bin");
        fs::write(&foreign, b"foreign live root entry").unwrap();
        let identity = root_abi_inode(&foreign);
        let live_usr = installation.root.join("usr");
        let archived_root = installation.root_path(fixture.candidate.id.to_string());
        let archived_usr = archived_root.join("usr");
        let staging = installation.staging_dir();
        let live_identity = root_abi_inode(&live_usr);
        let archive_identity = root_abi_inode(&archived_root);
        let archived_usr_identity = root_abi_inode(&archived_usr);
        let staging_identity = root_abi_inode(&staging);
        let states = fixture.client.state_db.all().unwrap();
        let mut checkpoints = Vec::new();
        assert!(take_observed_trigger_scopes().is_empty());

        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, false, true, |checkpoint| {
                checkpoints.push(checkpoint);
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(
            error,
            Error::RootAbiLinkTypeConflict { path, .. } if path == foreign
        ));
        assert!(checkpoints.is_empty());
        assert!(take_observed_trigger_scopes().is_empty());
        assert_eq!(root_abi_inode(&foreign), identity);
        assert_eq!(fs::read(&foreign).unwrap(), b"foreign live root entry");
        assert_root_abi_absent(&installation.root.join("sbin"));
        assert_eq!(root_abi_inode(&live_usr), live_identity);
        assert_eq!(root_abi_inode(&archived_root), archive_identity);
        assert_eq!(root_abi_inode(&archived_usr), archived_usr_identity);
        assert_eq!(root_abi_inode(&staging), staging_identity);
        assert!(!installation.staging_path("usr").exists());
        assert!(!live_usr.join(".cast-tree-id").exists());
        assert!(!archived_usr.join(".cast-tree-id").exists());
        assert_eq!(fixture.client.state_db.all().unwrap(), states);
        assert_eq!(
            fs::read_to_string(live_usr.join(".stateID")).unwrap(),
            fixture.previous.id.to_string()
        );
        assert_eq!(
            fs::read_to_string(archived_usr.join(".stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
    }

    #[test]
    fn isolation_root_abi_conflict_fails_before_usr_exchange_and_preserves_foreign_entry() {
        let fixture = stateful_transition_fixture(false);
        let foreign = fixture.client.installation.isolation_dir().join("bin");
        fs::write(&foreign, b"foreign isolation entry").unwrap();
        let identity = root_abi_inode(&foreign);
        let candidate_model = generated_system_snapshot("candidate-package");

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                candidate_model,
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            Error::StatefulCandidatePreserved {
                primary,
                candidate,
                previous: Some(previous),
            } if candidate == fixture.candidate.id
                && previous == fixture.previous.id
                && matches!(
                    primary.as_ref(),
                    Error::RootAbiLinkTypeConflict { path, .. } if path == &foreign
                )
        ));
        assert_eq!(root_abi_inode(&foreign), identity);
        assert_eq!(fs::read(&foreign).unwrap(), b"foreign isolation entry");
        assert_root_abi_absent(&fixture.client.installation.isolation_dir().join("sbin"));
        assert_fresh_candidate_quarantined_and_invalidated(&fixture);
    }

    #[test]
    fn ephemeral_root_and_isolation_root_abi_conflicts_are_both_non_destructive() {
        let root_temporary = tempfile::tempdir().unwrap();
        prepare_private_installation_root(root_temporary.path());
        let installation_root = root_temporary.path().join("installation");
        let blit_root = root_temporary.path().join("ephemeral");
        fs::create_dir(&installation_root).unwrap();
        let installation = test_installation(&installation_root);
        let client = Client::builder("root-abi-ephemeral-root-test", installation)
            .repositories(repository::Map::default())
            .ephemeral(&blit_root)
            .build()
            .unwrap();
        let candidate = client
            .materialize_ephemeral_candidate(std::iter::empty::<&package::Id>())
            .unwrap();

        let foreign = blit_root.join("bin");
        fs::write(&foreign, b"foreign ephemeral entry").unwrap();
        let identity = root_abi_inode(&foreign);
        let error = client
            .apply_ephemeral_candidate(candidate, generated_system_snapshot("ephemeral-package"))
            .unwrap_err();
        assert!(matches!(error, Error::RootAbiLinkTypeConflict { path, .. } if path == foreign));
        assert_eq!(root_abi_inode(&foreign), identity);
        assert_eq!(fs::read(&foreign).unwrap(), b"foreign ephemeral entry");
        assert!(!blit_root.join("usr/lib/os-release").exists());
        assert!(!blit_root.join("usr/lib/system-model.glu").exists());

        let isolation_temporary = tempfile::tempdir().unwrap();
        prepare_private_installation_root(isolation_temporary.path());
        let installation_root = isolation_temporary.path().join("installation");
        let blit_root = isolation_temporary.path().join("ephemeral");
        fs::create_dir(&installation_root).unwrap();
        let installation = test_installation(&installation_root);
        let client = Client::builder("root-abi-ephemeral-isolation-test", installation)
            .repositories(repository::Map::default())
            .ephemeral(&blit_root)
            .build()
            .unwrap();
        let candidate = client
            .materialize_ephemeral_candidate(std::iter::empty::<&package::Id>())
            .unwrap();
        let isolation_foreign = client.installation.isolation_dir().join("bin");
        fs::write(&isolation_foreign, b"foreign isolation entry").unwrap();
        let isolation_identity = root_abi_inode(&isolation_foreign);
        let error = client
            .apply_ephemeral_candidate(candidate, generated_system_snapshot("ephemeral-package"))
            .unwrap_err();
        assert!(matches!(
            error,
            Error::RootAbiLinkTypeConflict { path, .. } if path == isolation_foreign
        ));
        assert_eq!(root_abi_inode(&isolation_foreign), isolation_identity);
        assert_eq!(fs::read(&isolation_foreign).unwrap(), b"foreign isolation entry");
        assert_root_abi_links(&blit_root);
        assert!(!blit_root.join("usr/lib/os-release").exists());
        assert!(!blit_root.join("usr/lib/system-model.glu").exists());
    }

    #[test]
    fn archived_activation_archive_failure_reverses_usr_and_rearchives_the_candidate() {
        let fixture = stateful_transition_fixture(true);
        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                    Err(injected_state_transition_error("previous-state archive"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert!(matches!(
            error,
            Error::StatefulTransitionUsrRestored {
                candidate,
                previous: Some(previous),
                ..
            } if candidate == fixture.candidate.id && previous == fixture.previous.id
        ));
        assert_recovered_stateful_transition(&fixture);
    }

    #[test]
    fn skipped_boot_is_not_synchronized_during_pre_boot_recovery() {
        let fixture = stateful_transition_fixture(true);
        let mut attempted_boot_repair = false;
        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| match checkpoint {
                StatefulTransitionCheckpoint::AfterUsrExchange => {
                    Err(injected_state_transition_error("pre-boot activation failure"))
                }
                StatefulTransitionCheckpoint::BeforeRecoveryBootSynchronization => {
                    attempted_boot_repair = true;
                    Ok(())
                }
                _ => Ok(()),
            })
            .unwrap_err();

        assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
        assert!(!attempted_boot_repair);
        assert_recovered_stateful_transition(&fixture);
    }

    #[test]
    fn candidate_boot_sees_the_archived_previous_state_and_failure_restores_it() {
        let fixture = stateful_transition_fixture(true);
        let previous_archive = fixture
            .client
            .installation
            .root_path(fixture.previous.id.to_string())
            .join("usr");
        let staged = fixture.client.installation.staging_path("usr");
        let mut observed_boot_boundary = false;

        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, true, false, |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::BeforeCandidateBootSynchronization {
                    observed_boot_boundary = true;
                    assert_eq!(
                        fs::read_to_string(previous_archive.join(".stateID")).unwrap(),
                        fixture.previous.id.to_string()
                    );
                    assert!(!staged.exists());
                    Err(injected_state_transition_error("candidate boot synchronization"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert!(observed_boot_boundary);
        assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
        assert_recovered_stateful_transition(&fixture);
    }

    #[test]
    fn new_stateful_post_swap_failure_quarantines_and_invalidates_candidate() {
        let fixture = stateful_transition_fixture(false);
        let candidate_model = generated_system_snapshot("candidate-package");
        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                candidate_model,
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterPreviousStateArchive {
                        Err(injected_state_transition_error("after previous-state archive"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(matches!(
            error,
            Error::StatefulTransitionUsrRestored {
                candidate,
                previous: Some(previous),
                ..
            } if candidate == fixture.candidate.id && previous == fixture.previous.id
        ));
        assert_fresh_candidate_quarantined_and_invalidated(&fixture);
    }

    #[test]
    fn previous_archive_never_replaces_a_racing_empty_destination() {
        let fixture = stateful_transition_fixture(false);
        let destination = fixture
            .client
            .installation
            .root_path(fixture.previous.id.to_string())
            .join("usr");
        let mut occupant_inode = None;

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                        fs::create_dir_all(&destination).unwrap();
                        occupant_inode = Some(fs::symlink_metadata(&destination).unwrap().ino());
                    }
                    Ok(())
                },
            )
            .unwrap_err();

        assert!(
            matches!(&error, Error::StatefulTransitionUsrRestored { .. }),
            "{error:#?}"
        );
        assert_eq!(
            fs::symlink_metadata(&destination).unwrap().ino(),
            occupant_inode.unwrap()
        );
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_eq!(
            fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
            fixture.previous.id.to_string()
        );
    }

    #[test]
    fn previous_restore_never_replaces_a_racing_empty_staging_destination() {
        let fixture = stateful_transition_fixture(false);
        let staged = fixture.client.installation.staging_path("usr");
        let archived = fixture
            .client
            .installation
            .root_path(fixture.previous.id.to_string())
            .join("usr");
        let mut occupant_inode = None;

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterPreviousStateArchive {
                        fs::create_dir(&staged).unwrap();
                        occupant_inode = Some(fs::symlink_metadata(&staged).unwrap().ino());
                        Err(injected_state_transition_error("force previous restore"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(matches!(
            error,
            Error::StatefulTransitionRecoveryFailed {
                restore_previous: Some(_),
                ..
            }
        ));
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), occupant_inode.unwrap());
        assert_eq!(fs::read_dir(&staged).unwrap().count(), 0);
        assert_eq!(
            fs::read_to_string(archived.join(".stateID")).unwrap(),
            fixture.previous.id.to_string()
        );
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id
        );
    }

    #[test]
    fn incomplete_fresh_reverse_retains_live_candidate_record_and_reopens() {
        let fixture = stateful_transition_fixture(false);
        let root = fixture._temporary.path().to_owned();
        let candidate_model = generated_system_snapshot("candidate-package");
        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                candidate_model,
                |checkpoint| match checkpoint {
                    StatefulTransitionCheckpoint::AfterUsrExchange => {
                        Err(injected_state_transition_error("fresh transition failure"))
                    }
                    StatefulTransitionCheckpoint::BeforeRecoveryUsrExchange => {
                        Err(injected_state_transition_error("reverse exchange failure"))
                    }
                    _ => Ok(()),
                },
            )
            .unwrap_err();

        let Error::StatefulTransitionRecoveryFailed {
            reverse_exchange: Some(_),
            invalidate_candidate,
            ..
        } = error
        else {
            panic!("expected incomplete reverse recovery");
        };
        assert!(invalidate_candidate.is_none());
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id
        );
        assert_eq!(
            fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert_eq!(
            fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
            fixture.previous.id.to_string()
        );

        let candidate = fixture.candidate.id;
        drop(fixture.client);
        let reopened = stateful_test_client(&root);
        assert_eq!(reopened.installation.active_state, Some(candidate));
        assert_eq!(reopened.get_active_state().unwrap().unwrap().id, candidate);
    }

    #[test]
    fn incomplete_previous_restore_retains_live_fresh_candidate_record_and_reopens() {
        let fixture = stateful_transition_fixture(false);
        let root = fixture._temporary.path().to_owned();
        let candidate_model = generated_system_snapshot("candidate-package");
        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                candidate_model,
                |checkpoint| match checkpoint {
                    StatefulTransitionCheckpoint::AfterPreviousStateArchive => {
                        Err(injected_state_transition_error("fresh transition failure"))
                    }
                    StatefulTransitionCheckpoint::BeforeRecoveryPreviousStateRestore => {
                        Err(injected_state_transition_error("previous-state restore failure"))
                    }
                    _ => Ok(()),
                },
            )
            .unwrap_err();

        let Error::StatefulTransitionRecoveryFailed {
            restore_previous: Some(_),
            invalidate_candidate,
            ..
        } = error
        else {
            panic!("expected incomplete previous-state restore");
        };
        assert!(invalidate_candidate.is_none());
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id
        );
        assert_eq!(
            fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert_eq!(
            fs::read_to_string(
                fixture
                    .client
                    .installation
                    .root_path(fixture.previous.id.to_string())
                    .join("usr/.stateID")
            )
            .unwrap(),
            fixture.previous.id.to_string()
        );

        let candidate = fixture.candidate.id;
        drop(fixture.client);
        let reopened = stateful_test_client(&root);
        assert_eq!(reopened.installation.active_state, Some(candidate));
        assert_eq!(reopened.get_active_state().unwrap().unwrap().id, candidate);
    }

    #[test]
    fn new_stateful_pre_swap_failure_quarantines_and_invalidates_candidate() {
        let fixture = stateful_transition_fixture(false);
        let candidate_model = generated_system_snapshot("candidate-package");
        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                candidate_model,
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterTransactionTriggers {
                        Err(injected_state_transition_error("pre-swap preparation"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(matches!(
            error,
            Error::StatefulCandidatePreserved {
                candidate,
                previous: Some(previous),
                ..
            } if candidate == fixture.candidate.id && previous == fixture.previous.id
        ));
        assert_fresh_candidate_quarantined_and_invalidated(&fixture);
    }

    #[test]
    fn incomplete_archived_system_trigger_phase_quarantines_the_mutated_candidate() {
        let fixture = stateful_transition_fixture(true);
        let root = fixture._temporary.path().to_owned();
        let marker = Path::new("usr/partial-system-trigger");
        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, false, true, |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterSystemTriggersStarted {
                    fs::write(fixture.client.installation.root.join(marker), b"partial mutation").unwrap();
                    Err(injected_state_transition_error("incomplete system trigger phase"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert!(matches!(
            error,
            Error::StatefulTransitionUsrRestored {
                candidate,
                previous: Some(previous),
                ..
            } if candidate == fixture.candidate.id && previous == fixture.previous.id
        ));
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id
        );
        assert_eq!(
            fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
            fixture.previous.id.to_string()
        );
        assert!(
            !fixture
                .client
                .installation
                .root_path(fixture.candidate.id.to_string())
                .exists()
        );
        assert!(!fixture.client.installation.staging_path("usr").exists());

        let quarantines = fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(quarantines.len(), 1);
        assert!(
            quarantines[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("failed-archived-state-{}-", fixture.candidate.id))
        );
        assert_eq!(fs::read(quarantines[0].join(marker)).unwrap(), b"partial mutation");

        let previous = fixture.previous.id;
        drop(fixture.client);
        let reopened = stateful_test_client(&root);
        assert_eq!(reopened.installation.active_state, Some(previous));
        assert_eq!(reopened.get_active_state().unwrap().unwrap().id, previous);
    }

    #[test]
    fn completed_archived_system_trigger_phase_can_rearchive_after_later_failure() {
        let fixture = stateful_transition_fixture(true);
        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, false, false, |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::BeforeCandidateBootSynchronization {
                    Err(injected_state_transition_error("post-trigger boot preparation"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
        assert_recovered_stateful_transition(&fixture);
        assert_eq!(
            fs::read_dir(fixture.client.installation.state_quarantine_dir())
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn two_failed_active_state_reblits_use_unique_non_state_quarantines() {
        let temporary = tempfile::tempdir().unwrap();
        let mut client = stateful_test_client(temporary.path());
        let state = client.state_db.add(&[], Some("active"), None).unwrap();
        client.installation.active_state = Some(state.id);

        let restored_model = generated_system_snapshot("restored-active-package");
        let restored_snapshot = restored_model.encoded().to_owned();
        record_state_id(&client.installation.root, state.id).unwrap();
        record_system_snapshot(&client.installation.root, restored_model).unwrap();

        let mut failed_snapshots = BTreeSet::new();
        for package in ["first-failed-reblit-package", "second-failed-reblit-package"] {
            let failed_model = generated_system_snapshot(package);
            failed_snapshots.insert(failed_model.encoded().to_owned());
            let error = client
                .apply_stateful_blit_with_checkpoint(
                    vfs(Vec::new()).unwrap(),
                    &state,
                    None,
                    failed_model,
                    |checkpoint| match checkpoint {
                        StatefulTransitionCheckpoint::AfterTransactionTriggers => {
                            fs::write(client.installation.staging_path("wrapper-sentinel"), package)?;
                            Ok(())
                        }
                        StatefulTransitionCheckpoint::AfterUsrExchange => {
                            Err(injected_state_transition_error("active-state reblit"))
                        }
                        _ => Ok(()),
                    },
                )
                .unwrap_err();

            assert!(
                matches!(
                    &error,
                    Error::StatefulTransitionUsrRestored {
                        candidate,
                        previous: Some(previous),
                        ..
                    } if *candidate == state.id && *previous == state.id
                ),
                "unexpected active reblit recovery result: {error:#?}"
            );
            assert_eq!(
                fs::read_to_string(client.installation.root.join("usr/.stateID")).unwrap(),
                state.id.to_string()
            );
            assert_generated_snapshot(
                &system_model::snapshot_path(&client.installation.root),
                &restored_snapshot,
                "restored-active-package",
            );
            assert!(!client.installation.root_path(state.id.to_string()).join("usr").exists());
            assert_eq!(fs::read_dir(client.installation.staging_dir()).unwrap().count(), 0);
        }

        let quarantine_dir = client.installation.state_quarantine_dir();
        assert!(!quarantine_dir.starts_with(client.installation.root_path("")));
        let quarantines = fs::read_dir(&quarantine_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(quarantines.len(), 2);
        assert_eq!(quarantines.iter().collect::<BTreeSet<_>>().len(), 2);

        let mut preserved_snapshots = BTreeSet::new();
        let mut preserved_tokens = BTreeSet::new();
        let mut preserved_sentinels = BTreeSet::new();
        for quarantine in quarantines {
            assert!(
                quarantine
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("replaced-active-reblit-wrapper-{}-", state.id))
            );
            assert_eq!(
                fs::read_to_string(quarantine.join("usr/.stateID")).unwrap(),
                state.id.to_string()
            );
            let token = recovery_tree_token(&quarantine.join("usr"));
            preserved_tokens.insert(token);
            preserved_sentinels.insert(fs::read_to_string(quarantine.join("wrapper-sentinel")).unwrap());
            preserved_snapshots.insert(fs::read_to_string(system_model::snapshot_path(&quarantine)).unwrap());
        }
        assert_eq!(preserved_tokens.len(), 2);
        assert_eq!(
            preserved_sentinels,
            BTreeSet::from([
                "first-failed-reblit-package".to_owned(),
                "second-failed-reblit-package".to_owned(),
            ])
        );
        assert_eq!(preserved_snapshots, failed_snapshots);
    }

    #[test]
    fn recovery_reports_candidate_preservation_and_boot_repair_failures_without_losing_either_usr() {
        let fixture = stateful_transition_fixture(true);
        let mut attempted_boot_repair = false;
        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, true, false, |checkpoint| match checkpoint {
                StatefulTransitionCheckpoint::AfterCandidateBootSynchronizationStarted => Err(
                    injected_state_transition_error("candidate boot synchronization failure"),
                ),
                StatefulTransitionCheckpoint::BeforeRecoveryCandidatePreservation => {
                    Err(injected_state_transition_error("candidate preservation failure"))
                }
                StatefulTransitionCheckpoint::BeforeRecoveryBootSynchronization => {
                    attempted_boot_repair = true;
                    Err(injected_state_transition_error("restored-state boot repair failure"))
                }
                _ => Ok(()),
            })
            .unwrap_err();

        let Error::StatefulTransitionRecoveryFailed {
            candidate,
            previous: Some(previous),
            restore_previous,
            reverse_exchange,
            preserve_candidate,
            repair_boot,
            ..
        } = error
        else {
            panic!("expected structured state recovery failure");
        };
        assert_eq!(candidate, fixture.candidate.id);
        assert_eq!(previous, fixture.previous.id);
        assert!(restore_previous.is_none());
        assert!(reverse_exchange.is_none());
        assert!(preserve_candidate.is_some());
        assert!(repair_boot.is_some());
        assert!(attempted_boot_repair);

        assert_eq!(
            fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
            fixture.previous.id.to_string()
        );
        assert_generated_snapshot(
            &system_model::snapshot_path(&fixture.client.installation.root),
            &fixture.previous_snapshot,
            "previous-package",
        );
        assert_eq!(
            fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert_generated_snapshot(
            &system_model::snapshot_path(&fixture.client.installation.staging_dir()),
            &fixture.candidate_snapshot,
            "candidate-package",
        );
        assert!(
            !fixture
                .client
                .installation
                .root_path(fixture.candidate.id.to_string())
                .join("usr")
                .exists()
        );
    }

    #[test]
    fn apparent_boot_repair_success_remains_structurally_unverified() {
        let fixture = stateful_transition_fixture(true);
        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, true, false, |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterCandidateBootSynchronizationStarted {
                    Err(injected_state_transition_error(
                        "candidate boot synchronization failure",
                    ))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        let Error::StatefulTransitionRecoveryFailed {
            candidate,
            previous: Some(previous),
            repair_boot: Some(repair_boot),
            ..
        } = error
        else {
            panic!("expected unverified boot repair failure");
        };
        assert_eq!(candidate, fixture.candidate.id);
        assert_eq!(previous, fixture.previous.id);
        assert!(matches!(
            *repair_boot,
            Error::StatefulBootRepairUnverified {
                candidate,
                previous: Some(previous),
            } if candidate == fixture.candidate.id && previous == fixture.previous.id
        ));
        assert_recovered_stateful_transition(&fixture);
    }

    #[test]
    fn archived_state_activation_carries_each_generated_snapshot_with_its_usr_tree() {
        let temporary = tempfile::tempdir().unwrap();
        let mut client = stateful_test_client(temporary.path());
        let old = client.state_db.add(&[], Some("old"), None).unwrap();
        let new = client.state_db.add(&[], Some("new"), None).unwrap();
        client.installation.active_state = Some(old.id);

        let old_snapshot = generated_system_snapshot("old-package");
        let old_encoded = old_snapshot.encoded().to_owned();
        record_state_id(&client.installation.root, old.id).unwrap();
        record_system_snapshot(&client.installation.root, old_snapshot).unwrap();

        let archived_new_root = client.installation.root_path(new.id.to_string());
        let new_snapshot = generated_system_snapshot("new-package");
        let new_encoded = new_snapshot.encoded().to_owned();
        record_state_id(&archived_new_root, new.id).unwrap();
        record_system_snapshot(&archived_new_root, new_snapshot).unwrap();

        let archived = client.activate_state(new.id, true, true).unwrap();

        assert_eq!(archived, old.id);
        assert_generated_snapshot(
            &system_model::snapshot_path(&client.installation.root),
            &new_encoded,
            "new-package",
        );
        assert_generated_snapshot(
            &system_model::snapshot_path(&client.installation.root_path(old.id.to_string())),
            &old_encoded,
            "old-package",
        );
        assert_eq!(
            fs::read_to_string(client.installation.root.join("usr/.stateID")).unwrap(),
            new.id.to_string()
        );
        assert_eq!(
            fs::read_to_string(client.installation.root_path(old.id.to_string()).join("usr/.stateID")).unwrap(),
            old.id.to_string()
        );
    }
}
