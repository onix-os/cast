// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! The core client implementation for the moss package manager
//!
//! A [`Client`] needs to be constructed to handle the initialisation of various
//! databases, plugins and data sources to centralise package query and management
//! operations

use std::{
    borrow::Borrow,
    fmt, io,
    os::{fd::RawFd, unix::fs::symlink},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use astr::AStr;
use fs_err as fs;
use futures_util::{StreamExt, TryStreamExt, stream};
use itertools::Itertools;
use nix::{
    errno::Errno,
    fcntl::{self, OFlag},
    libc::{AT_FDCWD, RENAME_EXCHANGE, SYS_renameat2, syscall},
    sys::stat::{Mode, fchmodat, mkdirat},
    unistd::{UnlinkatFlags, close, linkat, mkdir, read, symlinkat, unlinkat, write},
};
use postblit::TriggerScope;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use stone::{StoneDecodedPayload, StonePayloadLayoutFile, StonePayloadLayoutRecord};
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
    /// This is useful for installing a root to a container (i.e. Boulder) while
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

        let config = config::Manager::system(&self.installation.root, "moss");
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
            config,
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
    /// Runtime configuration for the moss package manager
    config: config::Manager,
    /// All of our configured repositories, to seed the [`crate::registry::Registry`]
    repositories: repository::Manager,
    /// Operational scope (real systems, ephemeral, etc)
    scope: Scope,
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

    /// Returns `true` if this is an ephemeral client
    pub fn is_ephemeral(&self) -> bool {
        matches!(self.scope, Scope::Ephemeral { .. })
    }

    /// Perform package installation
    pub fn install(&mut self, packages: &[&str], yes: bool, simulate: bool) -> Result<install::Timing, Error> {
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
        install::install_exact(self, packages, yes, simulate).map_err(|error| Error::Install(Box::new(error)))
    }

    /// Perform package removals
    pub fn remove(&mut self, packages: &[&str], yes: bool, simulate: bool) -> Result<remove::Timing, Error> {
        remove(self, packages, yes, simulate).map_err(|error| Error::Remove(Box::new(error)))
    }

    /// Perform package fetches
    pub fn fetch(&mut self, packages: &[&str], output_dir: &Path, verbose: bool) -> Result<fetch::Timing, Error> {
        fetch(self, packages, output_dir, verbose).map_err(|error| Error::Fetch(Box::new(error)))
    }

    /// Perform a sync
    pub fn sync(&mut self, yes: bool, simulate: bool) -> Result<sync::Timing, Error> {
        sync(self, yes, simulate).map_err(|error| Error::Sync(Box::new(error)))
    }

    /// Transition to an ephemeral client that doesn't record state changes
    /// and blits to a different root.
    ///
    /// This is useful for installing a root to a container (i.e. Boulder) while
    /// using a shared cache.
    ///
    /// Returns an error if `blit_root` is the same as the installation root,
    /// since the system client should always be stateful.
    pub fn ephemeral(self, blit_root: impl Into<PathBuf>) -> Result<Self, Error> {
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
        let num_initialized = self.repositories.ensure_all_initialized().await?;
        self.registry = build_registry(&self.installation, &self.repositories, &self.install_db, &self.state_db)?;
        Ok(num_initialized)
    }

    /// Reload all configured repositories and refreshes their index file, then update
    /// registry with all active repositories.
    pub async fn refresh_repositories(&mut self) -> Result<(), Error> {
        // Reload manager if config sourced to pickup config changes
        // then refresh indexes
        if self.repositories.is_config_source() {
            self.repositories =
                repository::Manager::with_config_manager(self.config.clone(), self.installation.clone())?;
        };
        self.repositories.refresh_all().await?;

        // Rebuild registry
        self.registry = build_registry(&self.installation, &self.repositories, &self.install_db, &self.state_db)?;

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
        self.registry
            .by_id(package)
            .next()
            .ok_or(Error::MissingMetadata(package.clone()))
    }

    /// Resolves the provided id's with the underlying registry, returning
    /// the first [`Package`] for each id.
    ///
    /// Packages are sorted by name and deduped before returning.
    pub fn resolve_packages<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<Vec<Package>, Error> {
        let mut metadata = packages
            .into_iter()
            .map(|id| self.registry.by_id(id).next().ok_or(Error::MissingMetadata(id.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        metadata.sort_by_key(|p| p.meta.name.to_string());
        metadata.dedup_by_key(|p| p.meta.name.to_string());
        Ok(metadata)
    }

    /// Content identities for all active repository indexes participating in
    /// available-package resolution.
    pub fn repository_index_snapshots(&self) -> Result<Vec<repository::IndexSnapshot>, Error> {
        self.repositories.index_snapshots().map_err(Error::Repository)
    }

    /// Returns all unique packages which provide the supplied [`Provider`]
    pub fn lookup_packages_by_provider(&self, provider: &Provider, flags: package::Flags) -> Vec<Package> {
        self.registry
            .by_provider(provider, flags)
            .unique_by(|p| p.id.clone())
            .collect()
    }

    /// Return a sorted iterator of packages matching the given flags
    pub fn list_packages(&self, flags: package::Flags) -> impl Iterator<Item = Package> + '_ {
        self.registry.list(flags)
    }

    /// Returns all packages with names containing the provided keyword
    /// and match the given flags
    pub fn search_packages<'a>(
        &'a self,
        keyword: &'a str,
        flags: package::Flags,
    ) -> impl Iterator<Item = Package> + 'a {
        self.registry.by_keyword(keyword, flags)
    }

    /// Activates the provided state and runs system triggers once applied.
    ///
    /// The current state gets archived.\
    /// Returns the old state that was archived.
    pub fn activate_state(&self, id: state::Id, skip_triggers: bool, skip_boot: bool) -> Result<state::Id, Error> {
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
        let _guard = signal::ignore([Signal::SIGINT])?;
        let _fd = signal::inhibit(
            vec!["shutdown", "sleep", "idle", "handle-lid-switch"],
            "moss".into(),
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
    pub async fn cache_packages<T>(&self, packages: &[T]) -> Result<(), Error>
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

    /// Blit the packages to a filesystem root
    ///
    /// This functionality is core to all moss filesystem transactions, forming the entire
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
        };

        let fstree = self.vfs(packages)?;

        let materialization = match &self.scope {
            Scope::Stateful => AssetMaterialization::HardLink,
            Scope::Ephemeral { .. } => AssetMaterialization::IndependentCopy,
        };
        blit_root_with_materialization(&self.installation, &fstree, &blit_target, materialization)?;

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
        let Some(state_id) = self.installation.active_state else {
            return Err(Error::NoActiveState);
        };

        let state = self.state_db.get(state_id)?;

        boot::synchronize(self, &state).map_err(Error::Boot)
    }

    /// List all states for this moss [`Installation`]
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

    /// Return the active [`State`] for this moss [`Installation`]
    pub fn get_active_state(&self) -> Result<Option<State>, Error> {
        match self.installation.active_state {
            Some(id) => self.get_state(id).map(Some),
            None => Ok(None),
        }
    }

    /// List all layout entries cached by this moss [`Installation`], which
    /// includes packages installed across all states
    pub fn list_layouts(&self) -> Result<Vec<(package::Id, StonePayloadLayoutRecord)>, Error> {
        self.layout_db.all().map_err(Error::Db)
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn mocked(installation: Installation, registry: Registry) -> Result<Client, Error> {
        let config = config::Manager::system(&installation.root, "moss");
        let install_db = db::meta::Database::new(":memory:")?;
        let state_db = db::state::Database::new(":memory:")?;
        let layout_db = db::layout::Database::new(":memory:")?;

        let repositories = repository::Manager::with_config_manager(config.clone(), installation.clone())?;

        Ok(Client {
            config,
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

/// Add root symlinks & os-release file
fn create_root_links(root: &Path) -> io::Result<()> {
    let links = vec![
        ("usr/sbin", "sbin"),
        ("usr/bin", "bin"),
        ("usr/lib", "lib"),
        ("usr/lib", "lib64"),
        ("usr/lib32", "lib32"),
    ];

    'linker: for (source, target) in links.into_iter() {
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

/// Blit the packages to a filesystem root
///
/// This functionality is core to all moss filesystem transactions, forming the entire
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
    blit_root_with_materialization(installation, tree, blit_target, AssetMaterialization::HardLink)
}

fn blit_root_with_materialization(
    installation: &Installation,
    tree: &vfs::Tree<PendingFile>,
    blit_target: &Path,
    materialization: AssetMaterialization,
) -> Result<(), Error> {
    // undirt.
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
    let mut stats = BlitStats::default();

    progress.set_length(tree.len());
    progress.set_position(0_u64);

    let cache_dir = installation.assets_path("v2");
    let cache_fd = fcntl::open(&cache_dir, OFlag::O_DIRECTORY | OFlag::O_RDONLY, Mode::empty())?;

    // We need to ensure this runtime is dropped so it doesn't linger
    // since this is in the boulder call path & boulder can't have
    // multithreading when CLONE into a user namespace / "container"
    let rayon_runtime = rayon::ThreadPoolBuilder::new().build().expect("rayon runtime");

    rayon_runtime.install(|| -> Result<(), Error> {
        if let Some(root) = tree.structured() {
            mkdir(blit_target, Mode::from_bits_truncate(0o755))?;
            let root_dir = fcntl::open(blit_target, OFlag::O_DIRECTORY | OFlag::O_RDONLY, Mode::empty())?;

            if let Element::Directory(_, _, children) = root {
                let current_span = tracing::Span::current();
                stats = stats.merge(
                    children
                        .into_par_iter()
                        .map(|child| {
                            let _guard = current_span.enter();
                            blit_element(root_dir, cache_fd, child, &progress, materialization)
                        })
                        .try_reduce(BlitStats::default, |a, b| Ok(a.merge(b)))?,
                );
            }

            close(root_dir)?;
        }

        Ok(())
    })?;

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
            let newdir = fcntl::openat(parent, name, OFlag::O_RDONLY | OFlag::O_DIRECTORY, Mode::empty())?;

            let current_span = tracing::Span::current();
            stats = stats.merge(
                children
                    .into_par_iter()
                    .map(|child| {
                        let _guard = current_span.enter();
                        blit_element(newdir, cache, child, progress, materialization)
                    })
                    .try_reduce(BlitStats::default, |a, b| Ok(a.merge(b)))?,
            );

            close(newdir)?;

            Ok(stats)
        }
        Element::Child(name, item) => {
            blit_element_item(parent, cache, name, item, &mut stats, materialization)?;

            Ok(stats)
        }
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
                    let fd = fcntl::openat(
                        parent,
                        subpath,
                        OFlag::O_CREAT | OFlag::O_WRONLY | OFlag::O_TRUNC,
                        Mode::from_bits_truncate(item.layout.mode),
                    )?;
                    close(fd)?;
                }
                // Regular file
                _ => {
                    match materialization {
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
                    }

                    // Fix permissions
                    fchmodat(
                        Some(parent),
                        subpath,
                        Mode::from_bits_truncate(item.layout.mode),
                        nix::sys::stat::FchmodatFlags::NoFollowSymlink,
                    )?;
                }
            }

            stats.num_files += 1;
        }
        StonePayloadLayoutFile::Symlink(source, _) => {
            symlinkat(source.as_str(), Some(parent), subpath)?;
            stats.num_symlinks += 1;
        }
        StonePayloadLayoutFile::Directory(_) => {
            mkdirat(parent, subpath, Mode::from_bits_truncate(item.layout.mode))?;
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
    let source_fd = fcntl::openat(
        cache,
        source,
        OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_RDONLY,
        Mode::empty(),
    )?;
    let target_fd = match fcntl::openat(
        parent,
        target,
        OFlag::O_CLOEXEC | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_WRONLY,
        Mode::from_bits_truncate(0o600),
    ) {
        Ok(fd) => fd,
        Err(error) => {
            let _ = close(source_fd);
            return Err(error);
        }
    };

    let copy_result = copy_fd(source_fd, target_fd);
    let source_close_result = close(source_fd);
    let target_close_result = close(target_fd);

    if let Err(error) = copy_result {
        let _ = unlinkat(Some(parent), target, UnlinkatFlags::NoRemoveDir);
        return Err(error);
    }
    source_close_result?;
    target_close_result?;

    Ok(())
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
}

impl Scope {
    fn is_ephemeral(&self) -> bool {
        matches!(self, Self::Ephemeral { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssetMaterialization {
    HardLink,
    IndependentCopy,
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
    #[error("installation")]
    Installation(#[from] installation::Error),
    #[error("fetch package {1}")]
    CacheFetch(#[source] cache::FetchError, package::Name),
    #[error("unpack package {1}, file {2}")]
    CacheUnpack(#[source] cache::UnpackError, package::Name, PathBuf),
    #[error("repository manager")]
    Repository(#[from] repository::manager::Error),
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
let moss = import! moss.system.v1
moss.system
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
let moss = import! moss.system.v1
{
    packages = ["alpha"],
    .. moss.system
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

        let asset_id = 0x1234_5678_90ab_cdef_1234_5678_90ab_cdef_u128;
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
