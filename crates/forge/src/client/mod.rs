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
    ffi::{CStr, CString, OsString},
    fmt,
    io::{self, Read},
    mem::{MaybeUninit, size_of},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, PermissionsExt, symlink},
        },
    },
    path::{Component as PathComponent, Path, PathBuf},
    ptr::NonNull,
    time::{Duration, Instant},
};

use astr::AStr;
use filetime::{FileTime, set_file_times, set_symlink_file_times};
use fs_err as fs;
use futures_util::{StreamExt, TryStreamExt, stream};
use itertools::Itertools;
use nix::{
    errno::Errno,
    fcntl::{self, OFlag},
    libc::{AT_FDCWD, RENAME_EXCHANGE, RENAME_NOREPLACE, SYS_renameat2, syscall},
    sys::stat::{Mode, fchmod, fchmodat, mkdirat},
    unistd::{UnlinkatFlags, linkat, mkdir, read, symlinkat, unlinkat, write},
};
use postblit::TriggerScope;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use stone::{StoneDecodedPayload, StoneDigestWriterHasher, StonePayloadLayoutFile, StonePayloadLayoutRecord};
use thiserror::Error;
use tracing::{info, info_span, trace};
use tui::{MultiProgress, ProgressBar, ProgressStyle, Styled};
use vfs::tree::{BlitFile, Element, builder::TreeBuilder};

use self::install::install;
use self::prune::{prune_cache, prune_states};
use self::remove::remove;
use self::sync::sync;
use self::verify::verify;
use crate::{
    Installation, Package, Provider, Registry, Signal, State, SystemModel,
    client::fetch::fetch,
    db, environment, installation, package,
    registry::plugin::{self, Plugin},
    repository, runtime, signal,
    state::{self, Selection},
    system_model::{self, LoadedSystemModel},
};

pub use self::extract::extract;
pub use self::index::index;
pub use self::resolve::{AvailableClosure, Error as ResolveError, ResolvedPackage, ResolvedRequest};
pub use self::self_upgrade::self_upgrade;

mod boot;
mod cache;
mod fetch;
mod install;
mod postblit;
mod remove;
mod resolve;
mod self_upgrade;
mod sync;
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
        if let Some(path) = self.system_intent_path {
            self.installation.system_model =
                Some(system_model::load(&path)?.ok_or(Error::ImportSystemIntentDoesntExist(path.to_owned()))?);
        }

        let config = config::Manager::system(&self.installation.root, "cast");
        let install_db = db::meta::Database::new(self.installation.db_path("install").to_str().unwrap_or_default())?;
        let state_db = db::state::Database::new(self.installation.db_path("state").to_str().unwrap_or_default())?;
        let layout_db = db::layout::Database::new(self.installation.db_path("layout").to_str().unwrap_or_default())?;

        let repositories = if let Some(repos) = self.repositories {
            repository::Manager::with_explicit(&self.client_name, repos, self.installation.clone())?
        } else if let Some(system_model) = &self.installation.system_model {
            repository::Manager::with_system_model(&self.client_name, system_model.clone(), self.installation.clone())?
        } else {
            repository::Manager::with_config_manager(config.clone(), self.installation.clone())?
        };

        let registry = build_registry(&self.installation, &repositories, &install_db, &state_db)?;

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
        Ok(client)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviousUsrLocation {
    Staging,
    Archived(state::Id),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchivedStatePublication {
    Exchanged,
    Published,
}

#[derive(Debug, Default)]
struct StatefulRecoveryFailures {
    restore_previous: Option<Box<Error>>,
    reverse_exchange: Option<Box<Error>>,
    preserve_candidate: Option<Box<Error>>,
    invalidate_candidate: Option<Box<Error>>,
    repair_boot: Option<Box<Error>>,
}

impl StatefulRecoveryFailures {
    fn is_empty(&self) -> bool {
        self.restore_previous.is_none()
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

impl Client {
    /// Construct a new ClientBuilder for the given [`Installation`]
    pub fn builder(client_name: impl ToString, installation: Installation) -> ClientBuilder {
        ClientBuilder {
            client_name: client_name.to_string(),
            installation,
            repositories: None,
            system_intent_path: None,
            blit_root: None,
        }
    }

    /// Construct a new Client for the given [`Installation`]
    pub fn new(client_name: impl ToString, installation: Installation) -> Result<Client, Error> {
        Self::builder(client_name.to_string(), installation).build()
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
        let blit_root = parent.join(root_name);
        if blit_root == installation.root.canonicalize()? {
            return Err(Error::EphemeralInstallationRoot);
        }

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
            scope: Scope::Frozen { blit_root },
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

    fn preflight_repository_integrity(&self) -> Result<(), Error> {
        self.repositories
            .preflight_active_snapshots()
            .map_err(Error::Repository)
    }

    /// Perform package installation
    pub fn install(&mut self, packages: &[&str], yes: bool, simulate: bool) -> Result<install::Timing, Error> {
        self.require_non_frozen()?;
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
        discard_frozen_root_path(self.frozen_root()?)
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
        self.preflight_repository_integrity()?;
        sync(self, yes, simulate).map_err(|error| Error::Sync(Box::new(error)))
    }

    /// Transition to an ephemeral client that doesn't record state changes
    /// and blits to a different root.
    ///
    /// This is useful for installing a root to a container (for example, Mason) while
    /// using a shared cache.
    ///
    /// Returns an error if `blit_root` is the same as the installation root,
    /// since the system client should always be stateful.
    pub fn ephemeral(self, blit_root: impl Into<PathBuf>) -> Result<Self, Error> {
        self.require_non_frozen()?;
        let blit_root = blit_root.into();

        if blit_root.canonicalize()? == self.installation.root.canonicalize()? {
            return Err(Error::EphemeralInstallationRoot);
        }

        Ok(Self {
            scope: Scope::Ephemeral { blit_root },
            ..self
        })
    }

    /// Ensures all repositories have been initialized by ensuring their stone indexes
    /// are downloaded and added to the meta db
    pub async fn ensure_repos_initialized(&mut self) -> Result<usize, Error> {
        self.require_non_frozen()?;
        let num_initialized = self.repositories.ensure_all_initialized().await?;
        self.rebuild_registry()?;
        Ok(num_initialized)
    }

    /// Reload all configured repositories and refreshes their index file, then update
    /// registry with all active repositories.
    pub async fn refresh_repositories(&mut self) -> Result<(), Error> {
        self.require_non_frozen()?;
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
        self.registry = match &self.scope {
            Scope::Frozen { .. } => build_repository_registry(&self.repositories),
            Scope::Stateful | Scope::Ephemeral { .. } => {
                build_registry(&self.installation, &self.repositories, &self.install_db, &self.state_db)?
            }
        };
        Ok(())
    }

    pub fn verify(&self, yes: bool, verbose: bool) -> Result<(), Error> {
        if self.scope.is_ephemeral() {
            return Err(Error::EphemeralProhibitedOperation);
        }
        verify(self, yes, verbose)?;
        Ok(())
    }

    /// Prune states with the provided [`prune::Strategy`].
    ///
    /// This allows automatic removal of unused states (and their associated assets)
    /// from the disk, acting as a garbage collection facility.
    pub fn prune_states(&self, strategy: prune::Strategy<'_>, yes: bool) -> Result<(), Error> {
        if self.scope.is_ephemeral() {
            return Err(Error::EphemeralProhibitedOperation);
        }

        prune_states(self, strategy, yes)?;

        Ok(())
    }

    /// Prune all cached data that isn't related to any states or active repositories.
    ///
    /// This will remove all downloaded stones & unpacked asset data for packages not
    /// in that set.
    pub fn prune_cache(&self) -> Result<usize, Error> {
        if self.scope.is_ephemeral() {
            return Err(Error::EphemeralProhibitedOperation);
        }

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
        self.preflight_repository_integrity()?;
        let resolved = self.registry.by_id(package)?.into_iter().next();
        self.preflight_repository_integrity()?;
        resolved.ok_or(Error::MissingMetadata(package.clone()))
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
        self.preflight_repository_integrity()?;
        let mut metadata = packages
            .into_iter()
            .map(|id| {
                self.registry
                    .by_id(id)?
                    .into_iter()
                    .next()
                    .ok_or(Error::MissingMetadata(id.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        metadata.sort_by_key(|p| p.meta.name.to_string());
        metadata.dedup_by_key(|p| p.meta.name.to_string());
        self.preflight_repository_integrity()?;
        Ok(metadata)
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
        self.preflight_repository_integrity()?;
        let packages = self
            .registry
            .by_provider(provider, flags)?
            .into_iter()
            .unique_by(|p| p.id.clone())
            .collect();
        self.preflight_repository_integrity()?;
        Ok(packages)
    }

    /// Return sorted packages matching the given flags. Repository integrity
    /// failures are first-class and cannot be flattened into an empty list.
    pub fn list_packages(&self, flags: package::Flags) -> Result<Vec<Package>, Error> {
        self.preflight_repository_integrity()?;
        let packages = self.registry.list(flags)?;
        self.preflight_repository_integrity()?;
        Ok(packages)
    }

    /// Returns all packages with names containing the provided keyword
    /// and match the given flags
    pub fn search_packages(&self, keyword: &str, flags: package::Flags) -> Result<Vec<Package>, Error> {
        self.preflight_repository_integrity()?;
        let packages = self.registry.by_keyword(keyword, flags)?;
        self.preflight_repository_integrity()?;
        Ok(packages)
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
        self.require_non_frozen()?;
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
        // Fetch the new state
        let new = self.state_db.get(id).map_err(|_| Error::StateDoesntExist(id))?;

        // Get old (current) state
        let Some(old_id) = self.installation.active_state else {
            return Err(Error::NoActiveState);
        };

        if new.id == old_id {
            return Err(Error::StateAlreadyActive(id));
        }
        let old = self.state_db.get(old_id)?;

        // Resolve the trigger view before moving either filesystem tree. A
        // database or VFS failure must leave the archived candidate untouched.
        let fstree = self.vfs(new.selections.iter().map(|selection| &selection.package))?;

        let staging_dir = self.installation.staging_dir();

        // Ensure staging dir exists
        if !staging_dir.exists() {
            fs::create_dir(&staging_dir)?;
        }

        // Move new (archived) state to staging
        fs::rename(self.installation.root_path(new.id.to_string()), &staging_dir)?;

        self.commit_stateful_staging(
            &fstree,
            &new,
            Some(&old),
            StatefulCandidateOrigin::Archived,
            true,
            !skip_triggers,
            !skip_boot,
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

        let old_state = self.installation.active_state;

        let fstree = self.blit_root(selections.iter().map(|s| &s.package))?;

        let result = match &self.scope {
            Scope::Stateful => {
                // Add to db
                let state = self.state_db.add(selections, Some(&summary.to_string()), None)?;

                self.apply_stateful_blit(fstree, &state, old_state, system_snapshot)?;

                Ok(Some(state))
            }
            Scope::Ephemeral { blit_root } => {
                self.apply_ephemeral_blit(fstree, blit_root, system_snapshot)?;

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
        fstree: vfs::Tree<PendingFile>,
        state: &State,
        old_state: Option<state::Id>,
        system_snapshot: SystemModel,
    ) -> Result<(), Error> {
        self.apply_stateful_blit_with_checkpoint(fstree, state, old_state, system_snapshot, |_| Ok(()))
    }

    fn apply_stateful_blit_with_checkpoint<F>(
        &self,
        fstree: vfs::Tree<PendingFile>,
        state: &State,
        old_state: Option<state::Id>,
        system_snapshot: SystemModel,
        mut checkpoint: F,
    ) -> Result<(), Error>
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        self.require_non_frozen()?;
        let archive_previous = old_state.is_some();
        let (previous, candidate_origin) = match old_state {
            Some(id) => match self.state_db.get(id) {
                Ok(previous) => (Some(previous), StatefulCandidateOrigin::Fresh),
                Err(error) => {
                    return Err(self.preserve_unswapped_candidate(
                        state.id,
                        Some(id),
                        StatefulCandidateOrigin::Fresh,
                        Error::Db(error),
                        &mut checkpoint,
                    ));
                }
            },
            // Active-state verification reblits the same state and deliberately
            // does not archive the replaced corrupt tree on success. It still
            // needs the state value for boot repair if recovery reverses the
            // exchange.
            None if self.installation.active_state == Some(state.id) => {
                (Some(state.clone()), StatefulCandidateOrigin::ActiveReblit)
            }
            None => (None, StatefulCandidateOrigin::Fresh),
        };

        let prepare = (|| {
            record_state_id(&self.installation.staging_dir(), state.id)?;
            record_os_release(&self.installation.staging_dir())?;
            record_system_snapshot(&self.installation.staging_dir(), system_snapshot)?;

            create_root_links(&self.installation.isolation_dir())?;

            // The container running triggers expects /etc to exist.
            let root_etc = self.installation.root.join("etc");
            fs::create_dir_all(root_etc)?;

            let isolation_etc = self.installation.isolation_dir().join("etc");
            fs::create_dir_all(isolation_etc)?;

            // Transaction triggers run before `/usr` is exchanged. Their
            // arbitrary external side effects cannot be undone, but the
            // candidate tree can still be preserved outside the active root.
            Self::apply_triggers(TriggerScope::Transaction(&self.installation, &self.scope), &fstree)?;
            checkpoint(StatefulTransitionCheckpoint::AfterTransactionTriggers)?;
            Ok(())
        })();

        if let Err(primary) = prepare {
            return Err(self.preserve_unswapped_candidate(
                state.id,
                previous.as_ref().map(|state| state.id),
                candidate_origin,
                primary,
                &mut checkpoint,
            ));
        }

        self.commit_stateful_staging(
            &fstree,
            state,
            previous.as_ref(),
            candidate_origin,
            archive_previous,
            true,
            true,
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
        checkpoint: &mut F,
    ) -> Result<(), Error>
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        if let Err(primary) = self.promote_staging() {
            return Err(self.preserve_unswapped_candidate(
                candidate.id,
                previous.map(|state| state.id),
                candidate_origin,
                primary,
                checkpoint,
            ));
        }

        let mut previous_location = PreviousUsrLocation::Staging;
        let mut system_triggers_incomplete = false;
        let mut candidate_boot_synchronization_started = false;
        let primary = (|| {
            checkpoint(StatefulTransitionCheckpoint::AfterUsrExchange)?;

            // Root ABI links refer into `/usr`, so the same links remain valid
            // after either the forward or reverse exchange.
            create_root_links(&self.installation.root)?;

            if run_system_triggers {
                system_triggers_incomplete = true;
                checkpoint(StatefulTransitionCheckpoint::AfterSystemTriggersStarted)?;
                Self::apply_triggers(TriggerScope::System(&self.installation, &self.scope), fstree)?;
                system_triggers_incomplete = false;
            }
            checkpoint(StatefulTransitionCheckpoint::AfterSystemTriggers)?;

            if archive_previous && let Some(previous) = previous {
                checkpoint(StatefulTransitionCheckpoint::BeforePreviousStateArchive)?;
                self.archive_state(previous.id)?;
                previous_location = PreviousUsrLocation::Archived(previous.id);
                checkpoint(StatefulTransitionCheckpoint::AfterPreviousStateArchive)?;
            }

            if run_boot_synchronization {
                checkpoint(StatefulTransitionCheckpoint::BeforeCandidateBootSynchronization)?;
                candidate_boot_synchronization_started = true;
                checkpoint(StatefulTransitionCheckpoint::AfterCandidateBootSynchronizationStarted)?;
                boot::synchronize(self, candidate, previous)?;
            }

            Ok(())
        })();

        match primary {
            Ok(()) => Ok(()),
            Err(primary) => Err(self.recover_swapped_candidate(
                candidate.id,
                previous,
                candidate_origin,
                previous_location,
                system_triggers_incomplete,
                candidate_boot_synchronization_started,
                primary,
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
        checkpoint: &mut F,
    ) -> Error
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        let mut failures = StatefulRecoveryFailures::default();
        self.recover_failed_candidate(candidate, candidate_origin, false, checkpoint, &mut failures);

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
        system_triggers_incomplete: bool,
        candidate_boot_synchronization_started: bool,
        primary: Error,
        checkpoint: &mut F,
    ) -> Error
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        let previous_id = previous.map(|state| state.id);
        let mut failures = StatefulRecoveryFailures::default();

        if let PreviousUsrLocation::Archived(previous) = previous_location {
            let restored = checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryPreviousStateRestore)
                .and_then(|()| self.restore_archived_state_to_staging(previous));
            if let Err(error) = restored {
                failures.restore_previous = Some(Box::new(error));
                return self.stateful_recovery_error(candidate, previous_id, primary, failures);
            }
        }

        let reversed = checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryUsrExchange)
            .and_then(|()| self.exchange_staging_and_live_usr());
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
        checkpoint: &mut F,
        failures: &mut StatefulRecoveryFailures,
    ) where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        if let Err(error) = checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryCandidatePreservation)
            .and_then(|()| self.preserve_failed_candidate(candidate, candidate_origin, quarantine_archived_candidate))
        {
            failures.preserve_candidate = Some(Box::new(error));
        }

        self.invalidate_fresh_candidate(candidate, candidate_origin, checkpoint, failures);
    }

    fn invalidate_fresh_candidate<F>(
        &self,
        candidate: state::Id,
        candidate_origin: StatefulCandidateOrigin,
        checkpoint: &mut F,
        failures: &mut StatefulRecoveryFailures,
    ) where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        if candidate_origin == StatefulCandidateOrigin::Fresh
            && let Err(error) = checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryCandidateInvalidation)
                .and_then(|()| self.state_db.remove(&candidate).map_err(Error::Db))
        {
            failures.invalidate_candidate = Some(Box::new(error));
        }
    }

    fn exchange_staging_and_live_usr(&self) -> Result<(), Error> {
        let staged = self.installation.staging_path("usr");
        let live = self.installation.root.join("usr");
        Self::atomic_swap(&staged, &live).map_err(Error::Blit)
    }

    fn restore_archived_state_to_staging(&self, state: state::Id) -> Result<(), Error> {
        let archived = self.installation.root_path(state.to_string()).join("usr");
        let staged = self.installation.staging_path("usr");
        fs::rename(archived, staged)?;
        Ok(())
    }

    fn preserve_failed_candidate(
        &self,
        candidate: state::Id,
        candidate_origin: StatefulCandidateOrigin,
        quarantine_archived_candidate: bool,
    ) -> Result<(), Error> {
        if candidate_origin == StatefulCandidateOrigin::Archived && !quarantine_archived_candidate {
            return self.archive_state(candidate);
        }

        // Fresh candidates may be only partially prepared, an active reblit
        // would duplicate the restored live state identity, and an archived
        // candidate whose system-trigger phase did not complete may have been
        // partially mutated. None is safe in the ordinary bootable/prunable
        // state-root namespace.
        let kind = match candidate_origin {
            StatefulCandidateOrigin::Fresh => "new-state",
            StatefulCandidateOrigin::ActiveReblit => "active-reblit",
            StatefulCandidateOrigin::Archived => "archived-state",
        };
        let prefix = format!("failed-{kind}-{candidate}-");
        let quarantine = tempfile::Builder::new()
            .prefix(&prefix)
            .tempdir_in(self.installation.state_quarantine_dir())?;
        rename_noreplace(&self.installation.staging_path("usr"), &quarantine.path().join("usr"))?;
        // The quarantine owns the preserved candidate from this point on;
        // prevent TempDir from deleting it when the recovery helper exits.
        let _quarantine = quarantine.keep();
        Ok(())
    }

    /// Atomically publish a repaired non-active state without first deleting
    /// the only archived tree. Existing archives are exchanged with staging;
    /// missing archives are installed with no-replace publication.
    fn publish_rebuilt_archived_state(&self, state: state::Id) -> Result<ArchivedStatePublication, Error> {
        let staged = self.installation.staging_path("usr");
        let archived = self.installation.root_path(state.to_string()).join("usr");

        match fs::symlink_metadata(&archived) {
            Ok(_) => {
                Self::atomic_swap(&staged, &archived)?;
                Ok(ArchivedStatePublication::Exchanged)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(parent) = archived.parent() {
                    fs::create_dir_all(parent)?;
                }
                rename_noreplace(&staged, &archived)?;
                Ok(ArchivedStatePublication::Published)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn apply_ephemeral_blit(
        &self,
        fstree: vfs::Tree<PendingFile>,
        blit_root: &Path,
        system_snapshot: SystemModel,
    ) -> Result<(), Error> {
        self.require_non_frozen()?;
        record_os_release(blit_root)?;
        record_system_snapshot(blit_root, system_snapshot)?;

        create_root_links(blit_root)?;
        create_root_links(&self.installation.isolation_dir())?;

        // The container running triggers expects /etc to exist
        let etc = blit_root.join("etc");
        fs::create_dir_all(etc)?;

        // ephemeral tx triggers
        Self::apply_triggers(TriggerScope::Transaction(&self.installation, &self.scope), &fstree)?;
        // ephemeral system triggers
        Self::apply_triggers(TriggerScope::System(&self.installation, &self.scope), &fstree)?;

        Ok(())
    }

    /// "Activate" the staging tree
    /// In practice, this means we perform an atomic swap of the `/usr` directory on the
    /// host filesystem with the `/usr` tree within the transaction tree.
    ///
    /// This is performed using `renameat2` and results in instantly available, atomically updated
    /// `/usr`. In combination with the mandated "`/usr`` merge" and statelessness approach of
    /// Serpent OS, provides a unique atomic upgrade strategy.
    fn promote_staging(&self) -> Result<(), Error> {
        if self.scope.is_ephemeral() {
            return Err(Error::EphemeralProhibitedOperation);
        }

        let usr_target = self.installation.root.join("usr");
        let usr_source = self.installation.staging_path("usr");

        // Create the target tree
        if !usr_target.try_exists()? {
            fs::create_dir_all(&usr_target)?;
        }

        // Now swap staging with live
        Self::atomic_swap(&usr_source, &usr_target)?;

        Ok(())
    }

    /// syscall based wrapper for renameat2 so we can support musl libc which
    /// unfortunately does not expose the API.
    /// largely modelled on existing renameat2 API in nix crae
    fn atomic_swap<A: ?Sized + nix::NixPath, B: ?Sized + nix::NixPath>(old_path: &A, new_path: &B) -> nix::Result<()> {
        let result = old_path.with_nix_path(|old| {
            new_path.with_nix_path(|new| unsafe {
                syscall(
                    SYS_renameat2,
                    AT_FDCWD,
                    old.as_ptr(),
                    AT_FDCWD,
                    new.as_ptr(),
                    RENAME_EXCHANGE,
                )
            })
        })?? as i32;
        Errno::result(result).map(drop)
    }

    /// Archive old states (currently not "activated") into their respective tree
    fn archive_state(&self, id: state::Id) -> Result<(), Error> {
        if self.scope.is_ephemeral() {
            return Err(Error::EphemeralProhibitedOperation);
        }

        // After promotion, the old active /usr is now in staging/usr
        let usr_target = self.installation.root_path(id.to_string()).join("usr");
        let usr_source = self.installation.staging_path("usr");
        if let Some(parent) = usr_target.parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent)?;
        }
        // hot swap the staging/usr into the root/$id/usr
        fs::rename(usr_source, &usr_target)?;
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

        let unpacking_in_progress = cache::UnpackingInProgress::default();

        // Download and unpack each package
        let cached = stream::iter(packages)
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

                let is_cached = download.was_cached;

                // Move rest of blocking code to threadpool

                let multi_progress = multi_progress.clone();
                let total_progress = total_progress.clone();
                let unpacking_in_progress = unpacking_in_progress.clone();
                let package = (*package).clone();
                let current_span = tracing::Span::current();

                runtime::unblock(move || {
                    let _guard = current_span.enter();
                    let package_name = &package.meta.name;
                    let download_path = download.path().to_owned();

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

                    Ok((package, unpacked)) as Result<(Package, cache::UnpackedAsset), Error>
                })
                .await
            })
            // Use max network concurrency since we download files here
            .buffer_unordered(environment::MAX_NETWORK_CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;

        // Add layouts & packages to DBs
        runtime::unblock({
            let layout_db = self.layout_db.clone();
            let install_db = self.install_db.clone();
            move || {
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
            Scope::Frozen { blit_root } => Ok(blit_root),
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
        let blit_target = self.frozen_root()?.to_owned();
        let deadline = Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT;
        require_frozen_materialization_deadline(deadline)?;
        let packages = self.canonical_frozen_package_ids(packages)?;
        let layouts = bounded_frozen_layouts(self, &packages, deadline, FrozenLayoutQueryOperation::Materialization)?;
        let fstree = frozen_vfs_until(&packages, layouts, deadline)?;

        match fs::symlink_metadata(&blit_target) {
            Ok(_) => return Err(Error::FrozenRootDestinationExists(blit_target)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let parent = blit_target
            .parent()
            .ok_or_else(|| Error::InvalidFrozenRootDestination(blit_target.clone()))?;
        let stage = tempfile::Builder::new()
            .prefix(".forge-frozen-stage-")
            .tempdir_in(parent)?;
        // From this point onward generic TempDir recursion is disabled. Every
        // failure cleans the bounded generated tree through the same anchored
        // discard boundary used for completed roots.
        let stage_wrapper = stage.keep();
        // The random sibling wrapper remains 0700 for the entire build. The
        // publishable root can therefore carry its final 0755 mode without
        // exposing partial contents before the atomic rename.
        let stage_path = stage_wrapper.join("root");
        let result = (|| -> Result<MaterializedFrozenRoot, Error> {
            mkdir(&stage_path, Mode::from_bits_truncate(0o755))?;
            let root = open_owned(
                &stage_path,
                OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_RDONLY | OFlag::O_NOFOLLOW,
                Mode::empty(),
            )?;
            fchmod(root.as_raw_fd(), Mode::from_bits_truncate(0o755))?;

            blit_tree_into_open_root(
                &self.installation,
                &fstree,
                root.as_raw_fd(),
                AssetMaterialization::IndependentCopy,
                BlitExecution::Sequential,
                Some(deadline),
            )?;
            create_frozen_root_links(root.as_raw_fd(), deadline)?;
            normalize_frozen_tree(&stage_path, FileTime::from_unix_time(source_date_epoch, 0), deadline)?;
            require_frozen_materialization_deadline(deadline)?;
            // Open the provenance anchor while the completed root still has
            // its unguessable private staging name, retain it across rename,
            // and authenticate the public destination against that same
            // inode before returning it to the caller.
            let staged_root = open_frozen_root_anchor(&stage_path)?;
            publish_frozen_root(&stage_path, &blit_target, staged_root)
        })();

        match result {
            Ok(materialized_root) => {
                // `stage_path` moved out atomically; the private wrapper is
                // now empty and can be removed without traversal.
                if let Err(error) = fs::remove_dir(&stage_wrapper) {
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
                let cleanup = discard_frozen_root_path(&stage_path)
                    .and_then(|()| fs::remove_dir(&stage_wrapper).map_err(Error::from));
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
    /// file descriptors. Stateful roots hardlink files from the assets store to provide
    /// deduplication, while ephemeral roots receive independent copies so build-time writes
    /// cannot mutate the persistent asset store.
    ///
    /// This provides a very quick means to generate a filesystem snapshot on-demand,
    /// which can then be activated via [`Self::promote_staging`]
    pub fn blit_root<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<vfs::Tree<PendingFile>, Error> {
        let blit_target = match &self.scope {
            Scope::Stateful => self.installation.staging_dir(),
            Scope::Ephemeral { blit_root } => blit_root.to_owned(),
            Scope::Frozen { .. } => return Err(Error::FrozenClientProhibitedOperation),
        };

        let fstree = self.vfs(packages)?;

        let materialization = match &self.scope {
            Scope::Stateful => AssetMaterialization::HardLink,
            Scope::Ephemeral { .. } => AssetMaterialization::IndependentCopy,
            Scope::Frozen { .. } => unreachable!("frozen scope returned above"),
        };
        blit_root_with_materialization(
            &self.installation,
            &fstree,
            &blit_target,
            materialization,
            BlitExecution::Parallel,
        )?;

        Ok(fstree)
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
        let state = self.state_db.get(state)?;
        let is_active = self.installation.active_state == Some(state.id);

        let path = if is_active {
            system_model::snapshot_path(&self.installation.root)
        } else {
            system_model::snapshot_path(&self.installation.root_path(state.id.to_string()))
        };

        self.load_or_create_system_snapshot(path, &state)
    }

    /// Print boot status to stdout
    pub fn print_boot_status(&self) -> Result<(), Error> {
        boot::print_status(&self.installation).map_err(Error::Boot)
    }

    /// Synchronize boot for the active state
    pub fn synchronize_boot(&self) -> Result<(), Error> {
        self.require_non_frozen()?;
        let Some(state_id) = self.installation.active_state else {
            return Err(Error::NoActiveState);
        };

        let state = self.state_db.get(state_id)?;

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
        match self.installation.active_state {
            Some(id) => self.get_state(id).map(Some),
            None => Ok(None),
        }
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
const MAX_FROZEN_NORMALIZED_INODES: usize = MAX_FROZEN_EXECUTABLE_LAYOUTS + MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS + 6;

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
        let layouts = provider_layouts.get(&binding.package);
        let executable = resolve_frozen_executable_layout(binding, layouts, &directory_redirects, deadline)?;
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
    layouts: Option<&BTreeMap<PathBuf, FrozenExecutableLayout>>,
    directory_redirects: &BTreeMap<PathBuf, PathBuf>,
    deadline: Instant,
) -> Result<ExpectedFrozenExecutable, Error> {
    let mut current = binding.path.clone();
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
        let Some(layout) = layouts.and_then(|layouts| layouts.get(&current)) else {
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
                        package: binding.package.clone(),
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
                        package: binding.package.clone(),
                        path: current,
                    });
                }
                let next = resolve_frozen_symlink_target(&current, target).ok_or_else(|| {
                    Error::InvalidFrozenExecutableSymlinkTarget {
                        package: binding.package.clone(),
                        path: current.clone(),
                        target: target.clone(),
                    }
                })?;
                symlinks.push(ExpectedFrozenSymlink {
                    package: binding.package.clone(),
                    path: current,
                    target: target.clone(),
                    mode: *mode,
                });
                current = next;
            }
            FrozenExecutableLayout::Directory { .. } | FrozenExecutableLayout::Other => {
                return Err(Error::FrozenExecutableLayoutNotRegular {
                    package: binding.package.clone(),
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
    openat2_frozen(
        system_root.as_raw_fd(),
        relative,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
        (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
    )
    .map_err(|source| Error::OpenFrozenExecutableRoot {
        path: root.to_owned(),
        source,
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
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    let display_path = path.to_owned();
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let how = OpenHow {
        flags: flags as u64,
        mode: 0,
        resolve,
    };
    // SAFETY: `path` and `how` remain live for the syscall. A successful
    // openat2 returns a fresh descriptor owned by the resulting File.
    let descriptor = unsafe {
        syscall(
            nix::libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how as *const OpenHow,
            size_of::<OpenHow>(),
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned one fresh owned descriptor.
    let file = unsafe { std::fs::File::from_raw_fd(descriptor) };
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

/// Add root symlinks & os-release file
fn create_root_links(root: &Path) -> io::Result<()> {
    'linker: for (source, target) in ROOT_ABI_LINKS {
        let final_target = root.join(target);
        let staging_target = root.join(format!("{target}.next"));

        if staging_target.exists() {
            fs::remove_file(&staging_target)?;
        }

        if final_target.exists() && final_target.is_symlink() && final_target.read_link()?.to_string_lossy() == source {
            continue 'linker;
        }
        symlink(source, &staging_target)?;
        fs::rename(staging_target, final_target)?;
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

fn publish_frozen_root(
    stage: &Path,
    destination: &Path,
    staged_root: fs::File,
) -> Result<MaterializedFrozenRoot, Error> {
    let staged_identity = frozen_root_identity(&staged_root, stage)?;
    match rename_noreplace(stage, destination) {
        Ok(()) => {
            let published_identity = frozen_root_identity(&staged_root, destination)?;
            // rename(2) may update directory ctime, but cannot legitimately
            // change inode identity, type, ownership, or mode.
            if published_identity != staged_identity {
                return Err(Error::FrozenRootChangedDuringPublication {
                    stage: stage.to_owned(),
                    destination: destination.to_owned(),
                });
            }
            let materialized = MaterializedFrozenRoot {
                root_path: destination.to_owned(),
                root: staged_root,
                identity: published_identity,
            };
            materialized.revalidate()?;
            Ok(materialized)
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            Err(Error::FrozenRootDestinationExists(destination.to_owned()))
        }
        Err(source) => Err(Error::PublishFrozenRoot {
            stage: stage.to_owned(),
            destination: destination.to_owned(),
            source,
        }),
    }
}

fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    let source_name = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let destination_name = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL"))?;
    // SAFETY: both C strings remain live for the syscall. RENAME_NOREPLACE
    // atomically publishes the fully normalized sibling without ever
    // overwriting a root created by another process.
    let result = unsafe {
        syscall(
            SYS_renameat2,
            AT_FDCWD,
            source_name.as_ptr(),
            AT_FDCWD,
            destination_name.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
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

fn discard_frozen_root_path(root_path: &Path) -> Result<(), Error> {
    let deadline = Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT;
    require_frozen_materialization_deadline(deadline)?;
    match fs::symlink_metadata(root_path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }

    let pinned = open_frozen_root_anchor(root_path)?;
    let expected = frozen_root_anchor_witness(&pinned, root_path)?;
    let parent = root_path
        .parent()
        .ok_or_else(|| Error::InvalidFrozenRootDestination(root_path.to_owned()))?;
    let quarantine = tempfile::Builder::new()
        .prefix(".forge-frozen-discard-")
        .tempdir_in(parent)?;
    let detached = quarantine.path().join("root");
    rename_noreplace(root_path, &detached).map_err(|source| Error::DetachFrozenRoot {
        root: root_path.to_owned(),
        quarantine: detached.clone(),
        source,
    })?;

    // The quarantine now owns a real tree. Disable TempDir's generic recursive
    // destructor before any fallible check so every cleanup attempt remains on
    // this bounded descriptor-rooted path.
    let quarantine_path = quarantine.keep();
    let detached = quarantine_path.join("root");
    let moved = open_frozen_root_anchor(&detached)?;
    let actual = frozen_root_anchor_witness(&moved, &detached)?;
    // rename(2) legitimately advances directory ctime. Identity here is the
    // descriptor-pinned inode and its directory type; the still-open `pinned`
    // descriptor prevents inode reuse while the name is detached.
    if actual.device != expected.device
        || actual.inode != expected.inode
        || (actual.mode & nix::libc::S_IFMT) != (expected.mode & nix::libc::S_IFMT)
    {
        return Err(Error::FrozenRootChangedDuringDiscard {
            root: root_path.to_owned(),
            quarantine: detached,
        });
    }

    let mut permissions = moved.metadata()?.permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    fs::set_permissions(&detached, permissions)?;
    let directory = open_owned(
        &detached,
        OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?;
    let mut entries = 1usize;
    discard_frozen_directory(directory.as_raw_fd(), 0, &mut entries, deadline)?;
    drop(directory);
    drop(moved);
    drop(pinned);
    require_frozen_materialization_deadline(deadline)?;
    fs::remove_dir(&detached)?;
    fs::remove_dir(&quarantine_path)?;
    Ok(())
}

fn discard_frozen_directory(
    directory: RawFd,
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

    let names = frozen_discard_entry_names(directory, entries, deadline)?;
    for name in names {
        require_frozen_materialization_deadline(deadline)?;
        let mut metadata = MaybeUninit::<nix::libc::stat>::uninit();
        // SAFETY: directory and name are live, metadata points to writable
        // storage, and AT_SYMLINK_NOFOLLOW prevents target traversal.
        let status = unsafe {
            nix::libc::fstatat(
                directory,
                name.as_ptr(),
                metadata.as_mut_ptr(),
                nix::libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status != 0 {
            return Err(Error::InspectFrozenDiscardEntry {
                source: io::Error::last_os_error(),
            });
        }
        // SAFETY: successful fstatat initialized the complete stat value.
        let metadata = unsafe { metadata.assume_init() };
        if metadata.st_mode & nix::libc::S_IFMT == nix::libc::S_IFDIR {
            let child_name = Path::new(std::ffi::OsStr::from_bytes(name.as_bytes()));
            // Prove this name is an ordinary directory on the same filesystem
            // before chmod touches it. In particular, a hostile mount point
            // fails RESOLVE_NO_XDEV without changing the mounted root's mode.
            let anchor = openat2_frozen(
                directory,
                child_name,
                nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                nix::libc::RESOLVE_BENEATH
                    | nix::libc::RESOLVE_NO_SYMLINKS
                    | nix::libc::RESOLVE_NO_MAGICLINKS
                    | nix::libc::RESOLVE_NO_XDEV,
            )
            .map_err(|source| Error::OpenFrozenDiscardDirectory { source })?;
            fchmodat(
                Some(directory),
                name.as_c_str(),
                Mode::from_bits_truncate(metadata.st_mode | 0o700),
                nix::sys::stat::FchmodatFlags::NoFollowSymlink,
            )?;
            let child = openat2_frozen(
                directory,
                child_name,
                nix::libc::O_RDONLY
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK,
                nix::libc::RESOLVE_BENEATH
                    | nix::libc::RESOLVE_NO_SYMLINKS
                    | nix::libc::RESOLVE_NO_MAGICLINKS
                    | nix::libc::RESOLVE_NO_XDEV,
            )
            .map_err(|source| Error::OpenFrozenDiscardDirectory { source })?;
            let anchored_metadata = anchor.metadata()?;
            let child_metadata = child.metadata()?;
            if (anchored_metadata.dev(), anchored_metadata.ino()) != (child_metadata.dev(), child_metadata.ino()) {
                return Err(Error::FrozenDiscardEntryChanged);
            }
            discard_frozen_directory(child.as_raw_fd(), depth + 1, entries, deadline)?;
            drop(child);
            drop(anchor);
            unlinkat(Some(directory), name.as_c_str(), UnlinkatFlags::RemoveDir)?;
        } else {
            unlinkat(Some(directory), name.as_c_str(), UnlinkatFlags::NoRemoveDir)?;
        }
    }
    Ok(())
}

fn frozen_discard_entry_names(directory: RawFd, entries: &mut usize, deadline: Instant) -> Result<Vec<CString>, Error> {
    require_frozen_materialization_deadline(deadline)?;
    let cursor = openat_owned(
        directory,
        ".",
        OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?;
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

/// Normalize every materialized inode after the complete frozen tree and its
/// root ABI links exist. Directories are updated after their children so
/// traversal cannot leave ambient access or modification timestamps behind.
fn normalize_frozen_tree(path: &Path, timestamp: FileTime, deadline: Instant) -> Result<(), Error> {
    let mut inodes = 0usize;
    normalize_frozen_tree_inner(path, timestamp, deadline, 0, &mut inodes)
}

fn normalize_frozen_tree_inner(
    path: &Path,
    timestamp: FileTime,
    deadline: Instant,
    depth: usize,
    inodes: &mut usize,
) -> Result<(), Error> {
    require_frozen_materialization_deadline(deadline)?;
    if depth > MAX_FROZEN_LAYOUT_PATH_COMPONENTS {
        return Err(Error::FrozenNormalizationDepthLimit {
            limit: MAX_FROZEN_LAYOUT_PATH_COMPONENTS,
            actual: depth,
        });
    }
    let actual = inodes.saturating_add(1);
    if actual > MAX_FROZEN_NORMALIZED_INODES {
        return Err(Error::FrozenNormalizationInodeLimit {
            limit: MAX_FROZEN_NORMALIZED_INODES,
            actual,
        });
    }
    *inodes = actual;

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        set_symlink_file_times(path, timestamp, timestamp)?;
        return Ok(());
    }
    if metadata.is_dir() {
        // Keep the private stage traversable even when the declared final mode
        // is 000. Restore the exact authored mode after children and times are
        // normalized; a failure before restoration intentionally leaves owner
        // access available to TempDir's private cleanup.
        let final_permissions = metadata.permissions();
        let mut traversal_permissions = final_permissions.clone();
        traversal_permissions.set_mode(traversal_permissions.mode() | 0o700);
        fs::set_permissions(path, traversal_permissions)?;
        let mut children = Vec::new();
        for entry in fs::read_dir(path)? {
            require_frozen_materialization_deadline(deadline)?;
            let entry = entry?;
            let discovered = inodes.saturating_add(children.len()).saturating_add(1);
            if discovered > MAX_FROZEN_NORMALIZED_INODES {
                return Err(Error::FrozenNormalizationInodeLimit {
                    limit: MAX_FROZEN_NORMALIZED_INODES,
                    actual: discovered,
                });
            }
            children.push(entry.path());
        }
        require_frozen_materialization_deadline(deadline)?;
        children.sort();
        require_frozen_materialization_deadline(deadline)?;
        for child in children {
            normalize_frozen_tree_inner(&child, timestamp, deadline, depth + 1, inodes)?;
        }
        require_frozen_materialization_deadline(deadline)?;
        set_file_times(path, timestamp, timestamp)?;
        fs::set_permissions(path, final_permissions)?;
        return Ok(());
    }
    require_frozen_materialization_deadline(deadline)?;
    set_file_times(path, timestamp, timestamp)?;
    Ok(())
}

/// Restore owner traversal and mutation permissions before replacing a tree.
///
/// Frozen package metadata may legitimately make a directory read-only. The
/// next materialization still has to be able to remove children within that
/// directory. Symlinks are never followed.
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

/// Blit the packages to a filesystem root
///
/// This functionality is core to all Cast filesystem transactions, forming the entire
/// staging logic. For all the [`crate::package::Id`] present in the staging state,
/// query their stored [`StonePayloadLayoutBody`] and cache into a [`vfs::Tree`].
///
/// The new `/usr` filesystem is written in optimal order to a staging tree by making
/// use of the "at" family of functions (`mkdirat`, `linkat`, etc) with relative directory
/// file descriptors, linking files from the assets store to provide deduplication.
///
/// This provides a very quick means to generate a hardlinked "snapshot" on-demand,
/// which can then be activated via [`Self::promote_staging`]
pub fn blit_root(installation: &Installation, tree: &vfs::Tree<PendingFile>, blit_target: &Path) -> Result<(), Error> {
    blit_root_with_materialization(
        installation,
        tree,
        blit_target,
        AssetMaterialization::HardLink,
        BlitExecution::Parallel,
    )
}

fn blit_root_with_materialization(
    installation: &Installation,
    tree: &vfs::Tree<PendingFile>,
    blit_target: &Path,
    materialization: AssetMaterialization,
    execution: BlitExecution,
) -> Result<(), Error> {
    // undirt.
    match fs::symlink_metadata(blit_target) {
        Ok(_) => {
            if materialization == AssetMaterialization::IndependentCopy {
                make_tree_removable(blit_target)?;
            }
            fs::remove_dir_all(blit_target)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    // Preserve the historical stateful/ephemeral empty-tree behavior: the
    // previous destination is removed and no replacement root is created.
    if tree.len() == 0 {
        return Ok(());
    }

    mkdir(blit_target, Mode::from_bits_truncate(0o755))?;
    let root_dir = open_owned(
        blit_target,
        OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_RDONLY,
        Mode::empty(),
    )?;
    fchmod(root_dir.as_raw_fd(), Mode::from_bits_truncate(0o755))?;
    blit_tree_into_open_root(
        installation,
        tree,
        root_dir.as_raw_fd(),
        materialization,
        execution,
        None,
    )
}

fn blit_tree_into_open_root(
    installation: &Installation,
    tree: &vfs::Tree<PendingFile>,
    root_fd: RawFd,
    materialization: AssetMaterialization,
    execution: BlitExecution,
    deadline: Option<Instant>,
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
                stats = stats.merge(blit_children(
                    root_fd,
                    cache.as_ref(),
                    children,
                    &progress,
                    materialization,
                    execution,
                    deadline,
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
    deadline: Option<Instant>,
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
            // Construct within the parent
            blit_element_item(parent, cache, name, item, &mut stats, materialization, deadline)?;

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
                deadline,
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
            blit_element_item(parent, cache, name, item, &mut stats, materialization, deadline)?;

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
    deadline: Option<Instant>,
) -> Result<BlitStats, Error> {
    require_blit_deadline(deadline)?;
    match execution {
        BlitExecution::Parallel => {
            let current_span = tracing::Span::current();
            children
                .into_par_iter()
                .map(|child| {
                    let _guard = current_span.enter();
                    blit_element(parent, cache, child, progress, materialization, execution, deadline)
                })
                .try_reduce(BlitStats::default, |left, right| Ok(left.merge(right)))
        }
        BlitExecution::Sequential => children.into_iter().try_fold(BlitStats::default(), |stats, child| {
            blit_element(parent, cache, child, progress, materialization, execution, deadline)
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
    deadline: Option<Instant>,
) -> Result<(), Error> {
    require_blit_deadline(deadline)?;
    match &item.layout.file {
        StonePayloadLayoutFile::Regular(id, _) => {
            let hash = format!("{id:02x}");
            let directory = if hash.len() >= 10 {
                PathBuf::from(&hash[..2]).join(&hash[2..4]).join(&hash[4..6])
            } else {
                "".into()
            };

            // Link relative from cache to target
            let fp = directory.join(hash);

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
                            copy_asset(cache, &fp, *id, parent, subpath, item.layout.mode, deadline)?;
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
/// Ephemeral package roots are writable by build steps, so hardlinking them to
/// the persistent content store would let a write or chmod corrupt the cached
/// asset. Keep the descriptor-relative traversal used by the blitter while
/// giving the destination independent bytes and metadata.
fn copy_asset(
    pool: &AssetPool,
    source: &Path,
    expected_digest: u128,
    parent: RawFd,
    target: &str,
    mode: u32,
    deadline: Option<Instant>,
) -> Result<(), Error> {
    copy_asset_with_checkpoint(pool, source, expected_digest, parent, target, mode, deadline, |_| {})
}

fn copy_asset_with_checkpoint<F>(
    pool: &AssetPool,
    source: &Path,
    expected_digest: u128,
    parent: RawFd,
    target: &str,
    mode: u32,
    deadline: Option<Instant>,
    mut checkpoint: F,
) -> Result<(), Error>
where
    F: FnMut(AssetCopyCheckpoint),
{
    require_blit_deadline(deadline)?;
    require_single_component(Path::new(target))?;
    let asset = pool.open_asset(source)?;
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

fn open_owned(path: &Path, flags: OFlag, mode: Mode) -> Result<OwnedFd, Errno> {
    fcntl::open(path, flags, mode).map(raw_fd_into_owned)
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

fn record_state_id(root: &Path, state: state::Id) -> Result<(), Error> {
    let usr = root.join("usr");
    fs::create_dir_all(&usr)?;
    let state_path = usr.join(".stateID");
    fs::write(state_path, state.to_string())?;
    Ok(())
}

/// Record the operating system release info
/// Requires `os-info.json` to be present in the root, otherwise
/// we'll somewhat spitefully generate a generic os-release.
fn record_os_release(root: &Path) -> Result<(), Error> {
    let os_info_path = root.join("usr").join("lib").join("os-info.json");
    let os_release_data = match os_info::load_os_info_from_path(os_info_path) {
        Ok(ref info) => {
            let os_rel: os_info::OsRelease = info.into();
            os_rel.to_string()
        }
        Err(_) => {
            // Fallback to a generic os-release to break the system
            // TLDR: Implement your OS properly.
            format!(
                r#"NAME="Unbranded OS"
                VERSION="{version}"
                ID="unbranded-os"
                VERSION_CODENAME={version}
                VERSION_ID="{version}"
                PRETTY_NAME="Unbranded OS {version} - I forgot to add os-info.json"
                HOME_URL="https://github.com/AerynOS/os-info"
                BUG_REPORT_URL="https://.com""#,
                version = "no-os-info.json"
            )
        }
    };

    // It's possible this doesn't exist if
    // we remove all packages (=
    let dir = root.join("usr").join("lib");
    if !dir.exists() {
        fs::create_dir(&dir)?;
    }

    fs::write(dir.join("os-release"), os_release_data)?;

    Ok(())
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

fn record_system_snapshot(root: &Path, system_snapshot: SystemModel) -> Result<(), Error> {
    let path = system_model::snapshot_path(root);
    let dir = path.parent().expect("system snapshot path has a parent");
    fs::create_dir_all(dir)?;
    fs::write(path, system_snapshot.encoded())?;

    Ok(())
}

#[derive(Clone, Debug)]
enum Scope {
    Stateful,
    Ephemeral { blit_root: PathBuf },
    Frozen { blit_root: PathBuf },
}

impl Scope {
    fn is_ephemeral(&self) -> bool {
        matches!(self, Self::Ephemeral { .. } | Self::Frozen { .. })
    }
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
    installation: &Installation,
    repositories: &repository::Manager,
    installdb: &db::meta::Database,
    statedb: &db::state::Database,
) -> Result<Registry, Error> {
    let state = match installation.active_state {
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
    #[error("state {0} already active")]
    StateAlreadyActive(state::Id),
    #[error("state {0} doesn't exist")]
    StateDoesntExist(state::Id),
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
        "state transition for candidate {candidate} failed with primary error {primary}; recovery for previous state {previous:?} was incomplete (restore_previous={restore_previous:?}, reverse_exchange={reverse_exchange:?}, preserve_candidate={preserve_candidate:?}, invalidate_candidate={invalidate_candidate:?}, repair_boot={repair_boot:?})"
    )]
    StatefulTransitionRecoveryFailed {
        candidate: state::Id,
        previous: Option<state::Id>,
        #[source]
        primary: Box<Error>,
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
    #[error("Ephemeral client not allowed on installation root")]
    EphemeralInstallationRoot,
    #[error("Operation not allowed with ephemeral client")]
    EphemeralProhibitedOperation,
    #[error("frozen-root materialization requires a dedicated frozen client")]
    FrozenRootRequiresFrozenClient,
    #[error("frozen clients require an installation opened with Installation::open_frozen")]
    FrozenInstallationRequired,
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
        "frozen executable provider {package} binding {binding:?} resolves to {target:?}, which is absent from that same package"
    )]
    MissingFrozenExecutableSymlinkTarget {
        package: package::Id,
        binding: PathBuf,
        target: PathBuf,
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
    #[error("publish frozen root {stage:?} to {destination:?}")]
    PublishFrozenRoot {
        stage: PathBuf,
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen root inode changed while publishing {stage:?} to {destination:?}")]
    FrozenRootChangedDuringPublication { stage: PathBuf, destination: PathBuf },
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
    #[error("frozen root {root:?} changed while detaching it into {quarantine:?}")]
    FrozenRootChangedDuringDiscard { root: PathBuf, quarantine: PathBuf },
    #[error("frozen-root discard exceeds {limit} entries (got {actual})")]
    FrozenDiscardEntryLimit { limit: usize, actual: usize },
    #[error("frozen-root discard exceeds {limit} path components (got {actual})")]
    FrozenDiscardDepthLimit { limit: usize, actual: usize },
    #[error("frozen-root discard directory changed while it was pinned")]
    FrozenDiscardEntryChanged,
    #[error("inspect frozen-root discard entry")]
    InspectFrozenDiscardEntry {
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

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs::Permissions,
        os::unix::fs::{MetadataExt, PermissionsExt},
        process::Command,
    };

    use gluon_config::Source;

    use super::*;

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
        let installation = Installation::open(root, None).unwrap();
        Client::builder("state-snapshot-test", installation)
            .repositories(repository::Map::default())
            .build()
            .unwrap()
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
        for target in ["bin/tool", ".hidden", "share/Grüße/工具", "usr/bin/nested"] {
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
        let installation_root = temporary.path().join("installation");
        let blit_root = temporary.path().join("ephemeral-root");
        let intent_path = temporary.path().join("import.glu");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&blit_root).unwrap();

        let authored = r#"// This authored source must never be copied into state.
let cast = import! cast.system.v1
{
    packages = ["alpha"],
    .. cast.system
}
"#;
        fs::write(&intent_path, authored).unwrap();

        let installation = Installation::open(&installation_root, None).unwrap();
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
        let installation_root = temporary.path().join("installation");
        let blit_root = temporary.path().join("ephemeral-root");
        fs::create_dir(&installation_root).unwrap();
        fs::create_dir(&blit_root).unwrap();

        let installation = Installation::open(&installation_root, None).unwrap();
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
        let installation = Installation::open(temporary.path(), None).unwrap();
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
            Some(Instant::now() + Duration::from_secs(10)),
        )
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
            Installation::open_frozen(&installation_root, None).unwrap(),
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
            Installation::open_frozen(&installation_root, None).unwrap(),
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
            package,
            path: logical_tool.clone(),
        };
        assert!(matches!(
            resolve_frozen_executable_layout(
                &binding,
                Some(&layouts),
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
        let expected = resolve_frozen_executable_layout(&binding, Some(&layouts), &redirects, deadline).unwrap();
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
        assert!(matches!(
            resolve_frozen_executable_layout(&binding, Some(&layouts), &redirects, deadline),
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

        let installation = Installation::open_frozen(&installation_root, None).unwrap();
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

        let installation = Installation::open_frozen(&installation_root, None).unwrap();
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

        let installation = Installation::open_frozen(&installation_root, None).unwrap();
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
        assert!(matches!(
            client.require_frozen_executables(&packages, &[cross_provider_symlink]),
            Err(Error::MissingFrozenExecutableSymlinkTarget { package, binding, target })
                if package == first
                    && binding == Path::new("/usr/bin/cross-tool")
                    && target == Path::new("/usr/bin/other-tool")
        ));

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

        let installation = Installation::open_frozen(&installation_root, None).unwrap();
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
            Installation::open_frozen(&installation_root, None).unwrap(),
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
        let raced_stage = temporary.path().join("raced-stage");
        let raced_destination = temporary.path().join("raced-destination");
        fs::create_dir(&raced_stage).unwrap();
        fs::write(raced_stage.join("candidate"), b"candidate").unwrap();
        fs::create_dir(&raced_destination).unwrap();
        fs::write(raced_destination.join("winner"), b"winner").unwrap();
        let raced_anchor = open_frozen_root_anchor(&raced_stage).unwrap();
        assert!(matches!(
            publish_frozen_root(&raced_stage, &raced_destination, raced_anchor),
            Err(Error::FrozenRootDestinationExists(path)) if path == raced_destination
        ));
        assert_eq!(fs::read(raced_stage.join("candidate")).unwrap(), b"candidate");
        assert_eq!(fs::read(raced_destination.join("winner")).unwrap(), b"winner");
    }

    #[test]
    fn failed_frozen_root_blit_never_publishes_or_leaves_a_reusable_stage() {
        let temporary = tempfile::tempdir().unwrap();
        let installation_root = temporary.path().join("installation");
        let frozen_root = temporary.path().join("frozen-root");
        fs::create_dir(&installation_root).unwrap();
        let client = Client::frozen(
            "frozen-partial-stage-test",
            Installation::open_frozen(&installation_root, None).unwrap(),
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
            Installation::open_frozen(&installation_root, None).unwrap(),
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
        let installation = Installation::open_frozen(&installation_root, None).unwrap();
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
        let installation = Installation::open_frozen(&installation_root, None).unwrap();
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
            Installation::open_frozen(&installation_root, None).unwrap(),
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
            Installation::open_frozen(&installation_root, None).unwrap(),
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
        let temporary = tempfile::tempdir().unwrap();
        let mut client = stateful_test_client(temporary.path());
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

    #[test]
    fn rebuilt_non_active_state_atomically_exchanges_with_existing_archive() {
        let temporary = tempfile::tempdir().unwrap();
        let client = stateful_test_client(temporary.path());
        let state = client.state_db.add(&[], Some("archived"), None).unwrap();

        let archived_root = client.installation.root_path(state.id.to_string());
        let old = generated_system_snapshot("old-archived-package");
        let old_encoded = old.encoded().to_owned();
        record_state_id(&archived_root, state.id).unwrap();
        record_system_snapshot(&archived_root, old).unwrap();

        let repaired = generated_system_snapshot("repaired-archived-package");
        let repaired_encoded = repaired.encoded().to_owned();
        record_state_id(&client.installation.staging_dir(), state.id).unwrap();
        record_system_snapshot(&client.installation.staging_dir(), repaired).unwrap();

        let publication = client.publish_rebuilt_archived_state(state.id).unwrap();
        assert_eq!(publication, ArchivedStatePublication::Exchanged);
        assert_generated_snapshot(
            &system_model::snapshot_path(&archived_root),
            &repaired_encoded,
            "repaired-archived-package",
        );
        assert_generated_snapshot(
            &system_model::snapshot_path(&client.installation.staging_dir()),
            &old_encoded,
            "old-archived-package",
        );
    }

    #[test]
    fn rebuilt_missing_non_active_state_uses_noreplace_publication() {
        let temporary = tempfile::tempdir().unwrap();
        let client = stateful_test_client(temporary.path());
        let state = client.state_db.add(&[], Some("missing archive"), None).unwrap();
        let repaired = generated_system_snapshot("repaired-missing-package");
        let repaired_encoded = repaired.encoded().to_owned();
        record_state_id(&client.installation.staging_dir(), state.id).unwrap();
        record_system_snapshot(&client.installation.staging_dir(), repaired).unwrap();

        let publication = client.publish_rebuilt_archived_state(state.id).unwrap();
        assert_eq!(publication, ArchivedStatePublication::Published);
        assert_generated_snapshot(
            &system_model::snapshot_path(&client.installation.root_path(state.id.to_string())),
            &repaired_encoded,
            "repaired-missing-package",
        );
        assert!(!client.installation.staging_path("usr").exists());
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
        assert!(
            !installation
                .root_path(fixture.previous.id.to_string())
                .join("usr")
                .exists()
        );
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
                    |checkpoint| {
                        if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                            Err(injected_state_transition_error("active-state reblit"))
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
                } if candidate == state.id && previous == state.id
            ));
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
            assert!(!client.installation.staging_path("usr").exists());
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
        for quarantine in quarantines {
            assert!(
                quarantine
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("failed-active-reblit-{}-", state.id))
            );
            assert_eq!(
                fs::read_to_string(quarantine.join("usr/.stateID")).unwrap(),
                state.id.to_string()
            );
            preserved_snapshots.insert(fs::read_to_string(system_model::snapshot_path(&quarantine)).unwrap());
        }
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
