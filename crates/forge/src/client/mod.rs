// SPDX-FileCopyrightText: 2023 AerynOS Developers
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
    ffi::{CString, OsString},
    fmt,
    io::{self, Read},
    mem::size_of,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, PermissionsExt, symlink},
        },
    },
    path::{Path, PathBuf},
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
    libc::{AT_FDCWD, RENAME_EXCHANGE, SYS_renameat2, syscall},
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

/// One executable path that must be supplied by one exact package in a
/// materialized frozen closure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrozenExecutableBinding {
    pub package: package::Id,
    pub path: PathBuf,
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

        let blit_root = blit_root.into();
        if blit_root.canonicalize()? == installation.root.canonicalize()? {
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
    ) -> Result<install::Timing, Error> {
        install::materialize_frozen_root(self, packages, source_date_epoch)
            .map_err(|error| Error::Install(Box::new(error)))
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
    pub fn require_frozen_executables(
        &self,
        packages: &[package::Id],
        bindings: &[FrozenExecutableBinding],
    ) -> Result<(), Error> {
        let root = self.frozen_root()?;
        let packages = self.canonical_frozen_package_ids(packages)?;
        require_frozen_executables(self, root, &packages, bindings, |_, _| {})
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
    /// The current state gets archived.\
    /// Returns the old state that was archived.
    pub fn activate_state(&self, id: state::Id, skip_triggers: bool, skip_boot: bool) -> Result<state::Id, Error> {
        self.require_non_frozen()?;
        // Fetch the new state
        let new = self.state_db.get(id).map_err(|_| Error::StateDoesntExist(id))?;

        // Get old (current) state
        let Some(old) = self.installation.active_state else {
            return Err(Error::NoActiveState);
        };

        if new.id == old {
            return Err(Error::StateAlreadyActive(id));
        }

        let staging_dir = self.installation.staging_dir();

        // Ensure staging dir exists
        if !staging_dir.exists() {
            fs::create_dir(&staging_dir)?;
        }

        // Move new (archived) state to staging
        fs::rename(self.installation.root_path(new.id.to_string()), &staging_dir)?;

        // Promote staging
        self.promote_staging()?;

        // Archive old state
        self.archive_state(old)?;

        // Build VFS from new state selections
        // to build triggers from
        let fstree = self.vfs(new.selections.iter().map(|selection| &selection.package))?;

        if !skip_triggers {
            // Run system triggers
            Self::apply_triggers(TriggerScope::System(&self.installation, &self.scope), &fstree)?;
        }

        if !skip_boot {
            boot::synchronize(self, &new)?;
        }

        Ok(old)
    }

    /// Create a new recorded state from the provided packages
    /// provided packages and write that state ID to the installation
    /// Then blit the filesystem, promote it, finally archiving the active ID
    ///
    /// Returns `None` if the client is ephemeral
    pub fn new_state(&self, selections: &[Selection], summary: impl ToString) -> Result<Option<State>, Error> {
        self.require_non_frozen()?;
        let _guard = signal::ignore([Signal::SIGINT])?;
        let _fd = signal::inhibit(
            vec!["shutdown", "sleep", "idle", "handle-lid-switch"],
            "cast".into(),
            "Applying new state".into(),
            "block".into(),
        );

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
        self.require_non_frozen()?;
        record_state_id(&self.installation.staging_dir(), state.id)?;
        record_os_release(&self.installation.staging_dir())?;
        record_system_snapshot(&self.installation.staging_dir(), system_snapshot)?;

        create_root_links(&self.installation.isolation_dir())?;

        // The container running triggers expects /etc to exist
        let root_etc = self.installation.root.join("etc");
        fs::create_dir_all(root_etc)?;

        let isolation_etc = self.installation.isolation_dir().join("etc");
        fs::create_dir_all(isolation_etc)?;

        // Apply transaction triggers
        Self::apply_triggers(TriggerScope::Transaction(&self.installation, &self.scope), &fstree)?;

        // Staging is only used with [`Scope::Stateful`]
        self.promote_staging()?;

        // Now we got it staged, we need working rootfs
        create_root_links(&self.installation.root)?;

        if let Some(id) = old_state {
            self.archive_state(id)?;
        }

        // At this point we're allowed to run system triggers
        Self::apply_triggers(TriggerScope::System(&self.installation, &self.scope), &fstree)?;

        boot::synchronize(self, state)?;

        Ok(())
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

                // Add layouts
                layout_db.batch_add(cached.iter().flat_map(|(p, u)| {
                    u.payloads
                        .iter()
                        .flat_map(StoneDecodedPayload::layout)
                        .flat_map(|p| p.body.as_slice())
                        .map(|layout| (&p.id, layout))
                }))?;

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
    fn blit_frozen_root(&self, packages: &[package::Id], source_date_epoch: i64) -> Result<(), Error> {
        let blit_target = self.frozen_root()?.to_owned();
        let packages = self.canonical_frozen_package_ids(packages)?;
        let layouts = self.layout_db.query(packages.iter())?;
        let fstree = frozen_vfs(&packages, layouts)?;

        blit_root_with_materialization(
            &self.installation,
            &fstree,
            &blit_target,
            AssetMaterialization::IndependentCopy,
            BlitExecution::Sequential,
        )?;
        create_frozen_root_links(&blit_target)?;
        normalize_frozen_tree(&blit_target, FileTime::from_unix_time(source_date_epoch, 0))?;

        Ok(())
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

        boot::synchronize(self, &state).map_err(Error::Boot)
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
const MAX_FROZEN_EXECUTABLE_BINDINGS: usize = 4_096;
const MAX_FROZEN_EXECUTABLE_BYTES: u64 = 512 * MIB;
const MAX_TOTAL_FROZEN_EXECUTABLE_BYTES: u64 = 2 * GIB;
const MAX_FROZEN_EXECUTABLE_SYMLINKS: usize = 32;
const MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES: usize = 4_096;
const FROZEN_EXECUTABLE_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(120);

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
    path: PathBuf,
    target: String,
    mode: u32,
}

#[derive(Debug, Clone)]
enum FrozenExecutableLayout {
    Regular { digest: u128, mode: u32 },
    Symlink { target: String, mode: u32 },
    Other,
}

#[derive(Debug)]
struct PinnedFrozenSymlink {
    file: fs::File,
    witness: FrozenExecutableWitness,
    expected: ExpectedFrozenSymlink,
}

fn require_frozen_executables<F>(
    client: &Client,
    root: &Path,
    packages: &[package::Id],
    bindings: &[FrozenExecutableBinding],
    mut checkpoint: F,
) -> Result<(), Error>
where
    F: FnMut(&FrozenExecutableBinding, FrozenExecutableCheckpoint),
{
    require_frozen_executable_binding_count(bindings.len())?;

    let package_set = packages.iter().collect::<BTreeSet<_>>();
    let mut bindings = bindings.to_vec();
    bindings.sort();
    bindings.dedup();

    let mut requested = BTreeMap::<package::Id, BTreeSet<PathBuf>>::new();
    for binding in &bindings {
        if !package_set.contains(&binding.package) {
            return Err(Error::FrozenExecutableProviderOutsideClosure {
                package: binding.package.clone(),
                path: binding.path.clone(),
            });
        }
        let path = binding
            .path
            .to_str()
            .ok_or_else(|| Error::InvalidFrozenExecutablePath {
                package: binding.package.clone(),
                path: binding.path.clone(),
            })?;
        if !is_normalized_frozen_path(path) {
            return Err(Error::InvalidFrozenExecutablePath {
                package: binding.package.clone(),
                path: binding.path.clone(),
            });
        }
        requested
            .entry(binding.package.clone())
            .or_default()
            .insert(binding.path.clone());
    }

    if bindings.is_empty() {
        return Ok(());
    }

    let provider_ids = requested.keys().cloned().collect::<Vec<_>>();
    let layouts = client.layout_db.query(provider_ids.iter())?;
    let mut provider_layouts = BTreeMap::<package::Id, BTreeMap<PathBuf, FrozenExecutableLayout>>::new();
    for (package, layout) in layouts {
        let path = PathBuf::from(
            PendingFile {
                id: package.clone(),
                layout: layout.clone(),
            }
            .path()
            .as_str(),
        );
        if !path.to_str().is_some_and(is_normalized_frozen_path) {
            return Err(Error::InvalidFrozenLayoutPath {
                package,
                path: path.to_string_lossy().into_owned(),
            });
        }
        let entry = match layout.file {
            StonePayloadLayoutFile::Regular(digest, _) => FrozenExecutableLayout::Regular {
                digest,
                mode: layout.mode,
            },
            StonePayloadLayoutFile::Symlink(target, _) => FrozenExecutableLayout::Symlink {
                target: target.to_string(),
                mode: layout.mode,
            },
            StonePayloadLayoutFile::Directory(_)
            | StonePayloadLayoutFile::CharacterDevice(_)
            | StonePayloadLayoutFile::BlockDevice(_)
            | StonePayloadLayoutFile::Fifo(_)
            | StonePayloadLayoutFile::Socket(_)
            | StonePayloadLayoutFile::Unknown(..) => FrozenExecutableLayout::Other,
        };
        if provider_layouts
            .entry(package.clone())
            .or_default()
            .insert(path.clone(), entry)
            .is_some()
        {
            return Err(Error::DuplicateFrozenExecutableLayout { package, path });
        }
    }

    let mut expected = BTreeMap::<(package::Id, PathBuf), ExpectedFrozenExecutable>::new();
    for binding in &bindings {
        let layouts = provider_layouts.get(&binding.package);
        let executable = resolve_frozen_executable_layout(binding, layouts)?;
        expected.insert((binding.package.clone(), binding.path.clone()), executable);
    }

    let root = open_frozen_root_anchor(root)?;
    let deadline = Instant::now() + FROZEN_EXECUTABLE_VERIFICATION_TIMEOUT;
    let mut total_bytes = 0u64;
    for binding in &bindings {
        let key = (binding.package.clone(), binding.path.clone());
        let expected = expected
            .get(&key)
            .cloned()
            .ok_or_else(|| Error::MissingFrozenExecutableLayout {
                package: binding.package.clone(),
                path: binding.path.clone(),
            })?;
        require_frozen_executable_deadline(deadline)?;
        let pinned_symlinks = expected
            .symlinks
            .iter()
            .map(|symlink| pin_frozen_symlink(&root, binding, symlink))
            .collect::<Result<Vec<_>, Error>>()?;
        let mut file = open_frozen_executable(&root, binding, &expected.resolved_path)?;
        let before = frozen_executable_witness(&file, binding)?;
        require_frozen_executable_metadata(binding, &expected, before)?;
        account_frozen_executable_bytes(binding, before.length, &mut total_bytes)?;

        checkpoint(binding, FrozenExecutableCheckpoint::AfterOpen);
        let digest = digest_frozen_executable(&mut file, before.length, deadline, binding)?;
        checkpoint(binding, FrozenExecutableCheckpoint::AfterDigest);
        let after = frozen_executable_witness(&file, binding)?;
        if after != before {
            return Err(Error::FrozenExecutableChanged {
                package: binding.package.clone(),
                path: binding.path.clone(),
            });
        }
        if digest != expected.digest {
            return Err(Error::FrozenExecutableDigestMismatch {
                package: binding.package.clone(),
                path: binding.path.clone(),
                expected: expected.digest,
                actual: digest,
            });
        }

        checkpoint(binding, FrozenExecutableCheckpoint::BeforeReopen);
        let reopened = open_frozen_executable(&root, binding, &expected.resolved_path)?;
        let named = frozen_executable_witness(&reopened, binding)?;
        if named != before {
            return Err(Error::FrozenExecutablePathReplaced {
                package: binding.package.clone(),
                path: binding.path.clone(),
            });
        }
        for symlink in &pinned_symlinks {
            require_pinned_frozen_symlink(&root, binding, symlink)?;
        }
    }
    Ok(())
}

fn resolve_frozen_executable_layout(
    binding: &FrozenExecutableBinding,
    layouts: Option<&BTreeMap<PathBuf, FrozenExecutableLayout>>,
) -> Result<ExpectedFrozenExecutable, Error> {
    let mut current = binding.path.clone();
    let mut visited = BTreeSet::new();
    let mut symlinks = Vec::new();
    loop {
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
                    path: current,
                    target: target.clone(),
                    mode: *mode,
                });
                current = next;
            }
            FrozenExecutableLayout::Other => {
                return Err(Error::FrozenExecutableLayoutNotRegular {
                    package: binding.package.clone(),
                    path: current,
                });
            }
        }
    }
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
        || target.len() > MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES
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

fn pin_frozen_symlink(
    root: &fs::File,
    binding: &FrozenExecutableBinding,
    expected: &ExpectedFrozenSymlink,
) -> Result<PinnedFrozenSymlink, Error> {
    let file = open_frozen_symlink(root, binding, &expected.path)?;
    let witness = frozen_symlink_witness(&file, binding, &expected.path)?;
    if witness.mode != expected.mode || witness.mode & nix::libc::S_IFMT != nix::libc::S_IFLNK || witness.links != 1 {
        return Err(Error::FrozenExecutableSymlinkMetadataMismatch {
            package: binding.package.clone(),
            path: expected.path.clone(),
            expected: expected.mode,
            actual: witness.mode,
            links: witness.links,
        });
    }
    let actual = read_frozen_symlink(&file, binding, &expected.path)?;
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

fn require_pinned_frozen_symlink(
    root: &fs::File,
    binding: &FrozenExecutableBinding,
    pinned: &PinnedFrozenSymlink,
) -> Result<(), Error> {
    let descriptor = frozen_symlink_witness(&pinned.file, binding, &pinned.expected.path)?;
    let reopened = open_frozen_symlink(root, binding, &pinned.expected.path)?;
    let named = frozen_symlink_witness(&reopened, binding, &pinned.expected.path)?;
    if descriptor != pinned.witness || named != pinned.witness {
        return Err(Error::FrozenExecutableSymlinkChanged {
            package: binding.package.clone(),
            path: pinned.expected.path.clone(),
        });
    }
    let descriptor_target = read_frozen_symlink(&pinned.file, binding, &pinned.expected.path)?;
    let named_target = read_frozen_symlink(&reopened, binding, &pinned.expected.path)?;
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
) -> Result<u128, Error> {
    let mut hasher = StoneDigestWriterHasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut actual = 0u64;
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
    }
    if actual != expected_length {
        return Err(Error::FrozenExecutableLengthChanged {
            package: binding.package.clone(),
            path: binding.path.clone(),
            expected: expected_length,
            actual,
        });
    }
    Ok(hasher.digest128())
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
fn create_frozen_root_links(root: &Path) -> io::Result<()> {
    for (source, target) in ROOT_ABI_LINKS {
        symlink(source, root.join(target))?;
    }
    Ok(())
}

/// Normalize every materialized inode after the complete frozen tree and its
/// root ABI links exist. Directories are updated after their children so
/// traversal cannot leave ambient access or modification timestamps behind.
fn normalize_frozen_tree(path: &Path, timestamp: FileTime) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        set_symlink_file_times(path, timestamp, timestamp)?;
        return Ok(());
    }
    if metadata.is_dir() {
        let mut children = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        children.sort();
        for child in children {
            normalize_frozen_tree(&child, timestamp)?;
        }
    }
    set_file_times(path, timestamp, timestamp)
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
        tbuild.push(PendingFile { id: id.clone(), layout });
    }

    tbuild.bake();

    Ok(tbuild.tree()?)
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
        let path = PendingFile {
            id: package.clone(),
            layout: layout.clone(),
        }
        .path()
        .to_string();
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
fn frozen_vfs(
    packages: &[package::Id],
    layouts: Vec<(package::Id, StonePayloadLayoutRecord)>,
) -> Result<vfs::Tree<PendingFile>, Error> {
    let package_order = packages
        .iter()
        .enumerate()
        .map(|(order, package)| (package.clone(), order))
        .collect::<BTreeMap<_, _>>();
    let mut entries = layouts
        .into_iter()
        .map(|(package, layout)| {
            let order = package_order
                .get(&package)
                .copied()
                .ok_or_else(|| Error::UnexpectedFrozenLayoutPackage(package.clone()))?;
            FrozenLayoutEntry::new(package, layout, order)
        })
        .collect::<Result<Vec<_>, Error>>()?;

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

    let mut selected: Vec<FrozenLayoutEntry> = Vec::with_capacity(entries.len());
    for entry in entries {
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

    validate_frozen_tree_collisions(&selected)?;

    let mut builder = TreeBuilder::new();
    for entry in &selected {
        builder.push(entry.pending());
    }
    builder.bake();
    Ok(builder.tree()?)
}

fn is_normalized_frozen_path(path: &str) -> bool {
    path.starts_with("/usr/")
        && !path.as_bytes().contains(&0)
        && !path.ends_with('/')
        && !path.contains("//")
        && !path.split('/').any(|component| component == "." || component == "..")
}

fn validate_frozen_tree_collisions(entries: &[FrozenLayoutEntry]) -> Result<(), Error> {
    let explicit = entries
        .iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut directories = BTreeSet::new();
    for entry in entries {
        let mut parent = Path::new(&entry.path).parent();
        while let Some(path) = parent {
            directories.insert(path.to_string_lossy().into_owned());
            parent = path.parent();
        }
    }
    for entry in entries {
        if entry.is_directory() {
            directories.insert(entry.path.clone());
        } else {
            directories.remove(&entry.path);
        }
    }

    let redirects = entries
        .iter()
        .filter_map(|entry| {
            let StonePayloadLayoutFile::Symlink(target, _) = &entry.layout.file else {
                return None;
            };
            let target = if target.starts_with('/') {
                target.to_string()
            } else {
                let parent = Path::new(&entry.path)
                    .parent()
                    .expect("validated frozen path has a parent")
                    .to_string_lossy();
                vfs::path::join(parent.as_ref(), target.as_str()).to_string()
            };
            directories.contains(&target).then_some((entry.path.clone(), target))
        })
        .collect::<BTreeMap<_, _>>();

    let mut effective = BTreeMap::<String, &FrozenLayoutEntry>::new();
    for entry in entries {
        let path = redirected_frozen_path(&entry.path, &redirects)?;
        if !is_normalized_frozen_path(&path) {
            return Err(Error::FrozenRedirectOutsideUsr {
                package: entry.package.clone(),
                path,
            });
        }
        if let Some(previous) = effective.get(&path) {
            if !identical_directory_metadata(previous, entry) {
                return Err(frozen_collision(&path, previous, entry));
            }
        } else {
            effective.insert(path, entry);
        }
    }

    for (path, entry) in &effective {
        let mut parent = Path::new(path).parent();
        while let Some(parent_path) = parent {
            if let Some(ancestor) = effective.get(parent_path.to_string_lossy().as_ref())
                && !ancestor.is_directory()
            {
                return Err(frozen_collision(path, ancestor, entry));
            }
            parent = parent_path.parent();
        }
    }

    // A non-directory explicit parent which redirects to a real directory is
    // valid; every other original child-under-file relation is not.
    for entry in entries {
        let mut parent = Path::new(&entry.path).parent();
        while let Some(parent_path) = parent {
            let parent_name = parent_path.to_string_lossy();
            if let Some(ancestor) = explicit.get(parent_name.as_ref())
                && !ancestor.is_directory()
                && !redirects.contains_key(parent_name.as_ref())
            {
                return Err(frozen_collision(&entry.path, ancestor, entry));
            }
            parent = parent_path.parent();
        }
    }

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

fn redirected_frozen_path(path: &str, redirects: &BTreeMap<String, String>) -> Result<String, Error> {
    let mut path = path.to_owned();
    let mut visited = BTreeSet::new();
    loop {
        let Some((source, target)) = redirects
            .iter()
            .filter(|(source, _)| path.starts_with(&format!("{source}/")))
            .max_by_key(|(source, _)| source.len())
        else {
            return Ok(path);
        };
        if !visited.insert(path.clone()) {
            return Err(Error::FrozenSymlinkRedirectCycle { path });
        }
        path = format!("{}{}", target.trim_end_matches('/'), &path[source.len()..]);
    }
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
    if materialization == AssetMaterialization::IndependentCopy {
        make_tree_removable(blit_target)?;
    }
    fs::remove_dir_all(blit_target)?;

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

    let cache_dir = installation.assets_path("v2");
    let cache_fd = open_owned(
        &cache_dir,
        OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_RDONLY,
        Mode::empty(),
    )?;

    let blit = || -> Result<BlitStats, Error> {
        let mut stats = BlitStats::default();
        if let Some(root) = tree.structured() {
            mkdir(blit_target, Mode::from_bits_truncate(0o755))?;
            let root_dir = open_owned(
                blit_target,
                OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_RDONLY,
                Mode::empty(),
            )?;
            fchmod(root_dir.as_raw_fd(), Mode::from_bits_truncate(0o755))?;

            if let Element::Directory(_, _, children) = root {
                stats = stats.merge(blit_children(
                    root_dir.as_raw_fd(),
                    cache_fd.as_raw_fd(),
                    children,
                    &progress,
                    materialization,
                    execution,
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

    progress.finish_and_clear();

    let elapsed = now.elapsed();
    let num_entries = stats.num_entries();

    println!(
        "\n{} entries blitted in {} {}",
        num_entries.to_string().bold(),
        format!("{:.2}s", elapsed.as_secs_f32()).bold(),
        format!("({:.1}k / s)", num_entries as f32 / elapsed.as_secs_f32() / 1_000.0).dim()
    );

    Ok(())
}

/// Recursively write a directory, or a single flat inode, to the staging tree.
/// Care is taken to retain the directory file descriptor to avoid costly path
/// resolution at runtime.
fn blit_element(
    parent: RawFd,
    cache: RawFd,
    element: Element<'_, PendingFile>,
    progress: &ProgressBar,
    materialization: AssetMaterialization,
    execution: BlitExecution,
) -> Result<BlitStats, Error> {
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
            blit_element_item(parent, cache, name, item, &mut stats, materialization)?;

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
            blit_element_item(parent, cache, name, item, &mut stats, materialization)?;

            Ok(stats)
        }
    }
}

fn blit_children(
    parent: RawFd,
    cache: RawFd,
    children: Vec<Element<'_, PendingFile>>,
    progress: &ProgressBar,
    materialization: AssetMaterialization,
    execution: BlitExecution,
) -> Result<BlitStats, Error> {
    match execution {
        BlitExecution::Parallel => {
            let current_span = tracing::Span::current();
            children
                .into_par_iter()
                .map(|child| {
                    let _guard = current_span.enter();
                    blit_element(parent, cache, child, progress, materialization, execution)
                })
                .try_reduce(BlitStats::default, |left, right| Ok(left.merge(right)))
        }
        BlitExecution::Sequential => children.into_iter().try_fold(BlitStats::default(), |stats, child| {
            blit_element(parent, cache, child, progress, materialization, execution)
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
    cache: RawFd,
    subpath: &str,
    item: &PendingFile,
    stats: &mut BlitStats,
    materialization: AssetMaterialization,
) -> Result<(), Error> {
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
                0x99aa_06d3_0147_98d8_6001_c324_468d_497f => {
                    let _file = openat_owned(
                        parent,
                        subpath,
                        OFlag::O_CLOEXEC | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_WRONLY,
                        Mode::from_bits_truncate(0o600),
                    )?;
                }
                // Regular file
                _ => match materialization {
                    AssetMaterialization::HardLink => {
                        linkat(
                            Some(cache),
                            fp.to_str().unwrap(),
                            Some(parent),
                            subpath,
                            nix::unistd::LinkatFlags::NoSymlinkFollow,
                        )?;
                    }
                    AssetMaterialization::IndependentCopy => {
                        copy_asset(cache, &fp, parent, subpath)?;
                    }
                },
            }

            // Creation modes are filtered through the process umask. Apply
            // the package's complete mode after materialization instead.
            fchmodat(
                Some(parent),
                subpath,
                Mode::from_bits_truncate(item.layout.mode),
                nix::sys::stat::FchmodatFlags::NoFollowSymlink,
            )?;

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

/// Copy one cached asset into a fresh inode under `parent`.
///
/// Ephemeral package roots are writable by build steps, so hardlinking them to
/// the persistent content store would let a write or chmod corrupt the cached
/// asset. Keep the descriptor-relative traversal used by the blitter while
/// giving the destination independent bytes and metadata.
fn copy_asset(cache: RawFd, source: &Path, parent: RawFd, target: &str) -> Result<(), Errno> {
    let source_fd = openat_owned(
        cache,
        source,
        OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_RDONLY,
        Mode::empty(),
    )?;
    let target_fd = openat_owned(
        parent,
        target,
        OFlag::O_CLOEXEC | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_WRONLY,
        Mode::from_bits_truncate(0o600),
    )?;

    if let Err(error) = copy_fd(source_fd.as_raw_fd(), target_fd.as_raw_fd()) {
        let _ = unlinkat(Some(parent), target, UnlinkatFlags::NoRemoveDir);
        return Err(error);
    }

    Ok(())
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

fn copy_fd(source: RawFd, target: RawFd) -> Result<(), Errno> {
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let read_count = match read(source, &mut buffer) {
            Ok(count) => count,
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error),
        };
        if read_count == 0 {
            return Ok(());
        }

        let mut written = 0;
        while written < read_count {
            match write(target, &buffer[written..read_count]) {
                Ok(0) => return Err(Errno::EIO),
                Ok(count) => written += count,
                Err(Errno::EINTR) => {}
                Err(error) => return Err(error),
            }
        }
    }
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
    #[error("No metadata found for package {0:?}")]
    MissingMetadata(package::Id),
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
    #[error("frozen-root directory-symlink redirect cycle at {path:?}")]
    FrozenSymlinkRedirectCycle { path: String },
    #[error("package {package} redirects a frozen-root path outside /usr: {path:?}")]
    FrozenRedirectOutsideUsr { package: package::Id, path: String },
    #[error("frozen executable binding count exceeds {limit} (got {actual})")]
    FrozenExecutableBindingLimit { limit: usize, actual: usize },
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
    #[error("frozen executable root path is invalid: {0:?}")]
    InvalidFrozenExecutableRoot(PathBuf),
    #[error("open frozen executable root {path:?}")]
    OpenFrozenExecutableRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
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
    #[error("ignore signals during blit")]
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

    fn stateful_test_client(root: &Path) -> Client {
        let installation = Installation::open(root, None).unwrap();
        Client::builder("state-snapshot-test", installation)
            .repositories(repository::Map::default())
            .build()
            .unwrap()
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

    #[test]
    fn frozen_executable_limits_accept_the_boundary_and_reject_the_next_value() {
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
        let expected = resolve_frozen_executable_layout(&binding, Some(&layouts)).unwrap();
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
            resolve_frozen_executable_layout(&binding, Some(&layouts)),
            Err(Error::FrozenExecutableSymlinkLimit { package, path, limit })
                if package == binding.package
                    && path == binding.path
                    && limit == MAX_FROZEN_EXECUTABLE_SYMLINKS
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
        let asset_id = xxhash_rust::xxh3::xxh3_128(b"persistent cached bytes");
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
        fs::write(&asset_path, b"persistent cached bytes").unwrap();
        fs::set_permissions(&asset_path, Permissions::from_mode(0o640)).unwrap();
        let asset_metadata = fs::metadata(&asset_path).unwrap();

        nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
        const EPOCH: i64 = 1_700_000_123;
        client
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
            b"persistent cached bytes"
        );
        assert_eq!(fs::read(&tool).unwrap(), b"persistent cached bytes");
        assert_eq!(
            fs::metadata(&tool).unwrap().len(),
            b"persistent cached bytes".len() as u64
        );
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
        assert_eq!(fs::read(&asset_path).unwrap(), b"persistent cached bytes");
        assert_eq!(fs::metadata(&asset_path).unwrap().permissions().mode() & 0o7777, 0o640);

        let packages = [second.clone(), first.clone()];
        let tool_binding = FrozenExecutableBinding {
            package: first.clone(),
            path: PathBuf::from("/usr/bin/tool"),
        };
        client
            .require_frozen_executables(&packages, std::slice::from_ref(&tool_binding))
            .unwrap();

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
        client
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
        client.blit_frozen_root(&packages, EPOCH).unwrap();

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
        client.blit_frozen_root(&packages, EPOCH).unwrap();

        let hardlink = blit_root.join("usr/bin/tool-hardlink");
        fs::hard_link(&tool, &hardlink).unwrap();
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
            Err(Error::FrozenExecutableNotIndependentRegular { package, path, links: 2, .. })
                if package == first && path == Path::new("/usr/bin/tool")
        ));
        fs::remove_file(&hardlink).unwrap();
        client.blit_frozen_root(&packages, EPOCH).unwrap();

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
        client.blit_frozen_root(&packages, EPOCH).unwrap();

        fs::write(&tool, b"adversarial cached data").unwrap();
        assert_eq!(
            fs::metadata(&tool).unwrap().len(),
            b"persistent cached bytes".len() as u64
        );
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
            Err(Error::FrozenExecutableDigestMismatch { package, path, .. })
                if package == first && path == Path::new("/usr/bin/tool")
        ));
        client.blit_frozen_root(&packages, EPOCH).unwrap();

        let runtime_symlink = blit_root.join("usr/bin/tool-runtime-link");
        fs::remove_file(&tool).unwrap();
        symlink("tool-runtime-link", &tool).unwrap();
        fs::write(&runtime_symlink, b"persistent cached bytes").unwrap();
        fs::set_permissions(&runtime_symlink, Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            client.require_frozen_executables(&packages, std::slice::from_ref(&tool_binding)),
            Err(Error::OpenFrozenExecutable { package, path, .. })
                if package == first && path == Path::new("/usr/bin/tool")
        ));
        fs::remove_file(&runtime_symlink).unwrap();
        client.blit_frozen_root(&packages, EPOCH).unwrap();

        let mut changed_after_digest = false;
        let error = require_frozen_executables(
            &client,
            &blit_root,
            &packages,
            std::slice::from_ref(&tool_binding),
            |binding, checkpoint| {
                if checkpoint == FrozenExecutableCheckpoint::AfterDigest && !changed_after_digest {
                    assert_eq!(binding, &tool_binding);
                    fs::write(&tool, b"adversarial cached data").unwrap();
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
        client.blit_frozen_root(&packages, EPOCH).unwrap();

        let replacement = blit_root.join("usr/bin/tool-replacement");
        fs::write(&replacement, b"persistent cached bytes").unwrap();
        fs::set_permissions(&replacement, Permissions::from_mode(0o755)).unwrap();
        let mut replaced_before_reopen = false;
        let error = require_frozen_executables(
            &client,
            &blit_root,
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
        client.blit_frozen_root(&packages, EPOCH).unwrap();

        // A second materialization reverses caller and database order, changes
        // the process umask, and still reproduces all enforceable metadata.
        fs::write(&tool, b"mutated build root").unwrap();
        fs::set_permissions(&tool, Permissions::from_mode(0o600)).unwrap();
        client
            .layout_db
            .batch_add(layouts.iter().rev().map(|(package, layout)| (*package, layout)))
            .unwrap();
        nix::sys::stat::umask(Mode::from_bits_truncate(0o022));
        client
            .blit_frozen_root(&[first.clone(), second.clone()], EPOCH)
            .unwrap();
        assert_eq!(frozen_enforceable_manifest(&blit_root), first_manifest);
        assert_eq!(fs::read(&tool).unwrap(), b"persistent cached bytes");
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
    fn frozen_root_rejects_nul_paths_before_touching_destination() {
        assert_frozen_layout_rejected_before_touching_destination(
            StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(1, "share/nul\0path".into()),
            },
            |error| assert!(matches!(error, Error::InvalidFrozenLayoutPath { .. })),
        );
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
    fn frozen_root_rejects_directory_symlink_redirects_outside_usr() {
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
            Error::FrozenRedirectOutsideUsr { package: found, path }
                if found == package && path == "/etc/passwd"
        ));
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
            Error::FrozenPathCollision { path, first: found_first, second: found_second }
                if path == "/usr/real/shared"
                    && found_first == first
                    && found_second == second
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
