use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use config::declaration::DeclarationEvaluatorSet;
use declarative_config::DeclarationEvaluator as _;
use fs_err as fs;
use futures_util::{StreamExt, stream};
use url::Url;
#[cfg(test)]
use xxhash_rust::xxh3::xxh3_64;

use tui::{MultiProgress, ProgressBar, ProgressStyle, Styled};

use crate::{
    Installation, Package,
    db::meta,
    environment, package,
    repository::{self, Repository},
    runtime,
    system_model::LoadedSystemModel,
};

mod error;
mod index_storage;
mod snapshot;
mod source_validation;

pub use error::{Error, PackageUriError, Removal, UnsupportedRepoFormat};
pub(crate) use snapshot::{StableSnapshotView, VerifiedSnapshot, verified_active_snapshot, verify_active_snapshot};

use index_storage::{
    cache_dir, directory_owner, index_identity, inspect_file, open_meta_db, publish_index_candidate, read_index_bytes,
    require_regular_owned,
};
use snapshot::{
    RepositoryMutationLock, RepositorySnapshotReadLock, lock_verified_snapshot_cache, repository_is_initialized,
    verify_mutation_boundary,
};
use source_validation::{FetchedIndex, decode_repository_index, fetch_index, validate_repository_source};

#[cfg(test)]
use index_storage::{
    IndexIdentity, descendant_resolution, enforce_index_generation_budget, immutable_index_name, open_cache_directory,
    open_indexes_directory, openat2_file, proc_fd_path,
};
#[cfg(test)]
use source_validation::{
    normalize_repository_package_uri, repository_index_decode_limits, validate_repository_transport,
};

const IMMUTABLE_INDEX_DIRECTORY: &str = "indexes";
const IMMUTABLE_INDEX_EXTENSION: &str = "stone";
const INDEX_CANDIDATE_NAME: &str = "candidate.stone";
const REPOSITORY_MUTATION_LOCK_NAME: &str = ".index-refresh.lock";
const INDEX_IDENTITY_BUFFER_SIZE: usize = 64 * 1024;
const MAX_INDEX_GENERATIONS: usize = 32;
const MAX_INDEX_GENERATION_BYTES: u64 = 512 * 1024 * 1024;

#[cfg(test)]
pub(crate) fn immutable_index_path(state: &repository::Cached, sha256: &str) -> std::path::PathBuf {
    index_storage::immutable_index_path(state, sha256)
}

#[derive(Debug)]
pub enum Source {
    ConfigManager(config::Manager),
    SystemModel {
        identifier: String,
        system_model: LoadedSystemModel,
    },
    Explicit {
        identifier: String,
        repos: repository::Map,
    },
}

impl Source {
    fn identifier(&self) -> &str {
        match self {
            Source::ConfigManager(_) => environment::NAME,
            Source::SystemModel { identifier, .. } => identifier,
            Source::Explicit { identifier, .. } => identifier,
        }
    }
}

/// Manage a bunch of repositories
pub struct Manager {
    source: Arc<Source>,
    installation: Installation,
    repositories: BTreeMap<repository::Id, repository::Cached>,
}

impl Manager {
    pub fn is_config_source(&self) -> bool {
        matches!(*self.source, Source::ConfigManager { .. })
    }

    /// Create a [`Manager`] for the supplied [`Installation`] using repositories loaded
    /// via the supplied [`config::Manager`]
    pub fn with_config_manager(config: config::Manager, installation: Installation) -> Result<Self, Error> {
        Self::new(Source::ConfigManager(config), installation)
    }

    /// Create a [`Manager`] for the supplied [`Installation`] using repositories
    /// defined in the provided [`SystemModel`]
    ///
    /// [`Manager`] can't be used to `add` new repos in this mode
    pub fn with_system_model(
        identifier: impl ToString,
        system_model: LoadedSystemModel,
        installation: Installation,
    ) -> Result<Self, Error> {
        Self::new(
            Source::SystemModel {
                identifier: identifier.to_string(),
                system_model,
            },
            installation,
        )
    }

    /// Create a [`Manager`] for the supplied [`Installation`] using the provided configurations
    ///
    /// [`Manager`] can't be used to `add` new repos in this mode
    pub fn with_explicit(
        identifier: impl ToString,
        repos: repository::Map,
        installation: Installation,
    ) -> Result<Self, Error> {
        Self::new(
            Source::Explicit {
                identifier: identifier.to_string(),
                repos,
            },
            installation,
        )
    }

    fn new(source: Source, installation: Installation) -> Result<Self, Error> {
        // Repository queries need a writable, caller-owned cache today: the
        // manager may create/migrate SQLite state and every operation-wide
        // snapshot view relies on an owner-authenticated lock file. Do not
        // silently weaken ownership/mode checks for an unprivileged reader of
        // a root-owned installation. A future read-only backend must open
        // SQLite mode=ro and authenticate a separately defined trusted owner.
        if installation.read_only() {
            return Err(Error::ReadOnlyRepositoryCacheUnsupported(installation.root.clone()));
        }
        let configs = match &source {
            Source::ConfigManager(config) =>
            // Load all configs, default if none exist
            {
                let evaluators = DeclarationEvaluatorSet::new(
                    repository::RepositoryEvaluator::registered(),
                )
                .expect("the repository languages register distinct extensions");
                config
                    .load_declarations(&evaluators)?
                    .into_iter()
                    .map(|loaded| (Some(loaded.path), loaded.value))
                    .collect()
            }
            Source::SystemModel { system_model, .. } => vec![(None, system_model.repositories.clone())],
            Source::Explicit { repos, .. } => vec![(None, repos.clone())],
        };

        // Open all repo meta dbs and collect into hash map
        let repositories = configs
            .into_iter()
            .flat_map(|(config_path, repo_map)| {
                repo_map
                    .into_iter()
                    .map(|(id, repository)| {
                        validate_repository_source(&repository)?;
                        let (db, cache_dir) = open_meta_db(source.identifier(), &id, &repository, &installation)?;

                        Ok((
                            id.clone(),
                            repository::Cached::new(id, repository, db, config_path.clone(), cache_dir),
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Result<_, Error>>()?;

        Ok(Self {
            source: Arc::new(source),
            installation,
            repositories,
        })
    }

    /// Add a [`Repository`]
    pub fn add_repository(&mut self, id: repository::Id, repository: Repository) -> Result<(), Error> {
        let Source::ConfigManager(config) = &*self.source else {
            return Err(Error::ExplicitUnsupported);
        };
        validate_repository_source(&repository)?;

        // Save repo as new config file
        // We save it as a map for easy merging across
        // multiple configuration files
        let map = repository::Map::with([(id.clone(), repository.clone())]);
        let codec = repository::RepositoryCodec::default();
        let active_language = codec.language_spec().clone();
        let evaluators = DeclarationEvaluatorSet::new([codec])
            .expect("one validated repository adapter has no extension collision");
        let config_path = config
            .save_declaration(
                &id,
                &map,
                &evaluators,
                &active_language,
            )?;

        let (db, cache_dir) = open_meta_db(self.source.identifier(), &id, &repository, &self.installation)?;

        self.repositories.insert(
            id.clone(),
            repository::Cached::new(id, repository, db, Some(config_path), cache_dir),
        );

        Ok(())
    }

    /// Refresh a [`Repository`] by Id
    pub async fn refresh(&self, id: &repository::Id) -> Result<(), Error> {
        let Some(repo) = self.repositories.get(id).cloned() else {
            return Err(Error::UnknownRepo(id.clone()));
        };

        if repo.repository.active {
            // The filesystem lock is acquired before fetching, not merely
            // before publication. A slow generation A therefore cannot fetch,
            // let generation B commit, and then roll the repository back to A.
            // `flock` also coordinates separate Managers/processes which do
            // not share an in-memory mutex.
            let lock_repo = repo.clone();
            let mutation = runtime::unblock(move || RepositoryMutationLock::acquire(&lock_repo)).await?;
            let candidate = fetch_index(&self.source, &repo, &mutation.cache_directory).await?;
            runtime::unblock(move || activate_index_candidate(&repo, candidate, mutation)).await?;
        }

        Ok(())
    }

    /// Refresh all [`Repository`]'s by fetching it's latest index
    /// file and updating it's associated meta database
    pub async fn refresh_all(&self) -> Result<(), Error> {
        let mpb = MultiProgress::new();

        // Fetch index files asynchronously and then
        // update to DB
        stream::iter(self.repositories.iter().filter(|(_, r)| r.repository.active))
            .map(|(id, _)| async {
                let pb = mpb.add(
                    ProgressBar::new_spinner()
                        .with_style(
                            ProgressStyle::with_template(" {spinner} {wide_msg}")
                                .unwrap()
                                .tick_chars("--=≡■≡=--"),
                        )
                        .with_message(format!("{} {}", "Refreshing".blue(), *id)),
                );
                pb.enable_steady_tick(Duration::from_millis(150));

                self.refresh(id).await?;

                pb.suspend(|| println!("{} {}", "Refreshed".green(), *id));

                Ok(())
            })
            .buffer_unordered(environment::MAX_NETWORK_CONCURRENCY)
            .fold(Ok(()), |acc, result| async {
                match (acc, result) {
                    (Ok(_), Ok(_)) => Ok(()),
                    (Err(err), Ok(_)) => Err(err),
                    (Err(Error::UnsupportedRepos(a)), Err(Error::UnsupportedRepos(b))) => {
                        Err(Error::UnsupportedRepos(a.into_iter().chain(b).collect()))
                    }
                    (Err(Error::OutdatedRepos(_, a)), Err(Error::OutdatedRepos(_, b))) => Err(Error::OutdatedRepos(
                        self.source.clone(),
                        a.into_iter().chain(b).collect(),
                    )),
                    (_, Err(err)) => Err(err),
                }
            })
            .await
    }

    /// Ensures all repositories are initialized - index file downloaded and meta db
    /// populated.
    ///
    /// This is useful to call when initializing the Cast client in case users added configs
    /// manually outside the CLI
    pub async fn ensure_all_initialized(&mut self) -> Result<usize, Error> {
        let uninitialized = self
            .repositories
            .iter()
            .filter(|(_, r)| r.repository.active)
            .filter_map(|(id, state)| match repository_is_initialized(state) {
                Ok(true) => None,
                Ok(false) => Some(Ok(id)),
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>, Error>>()?;

        if uninitialized.is_empty() {
            return Ok(0);
        }

        let mpb = MultiProgress::new();

        // Fetch index files asynchronously and then
        // update to DB
        stream::iter(&uninitialized)
            .map(|id| async {
                let pb = mpb.add(
                    ProgressBar::new_spinner()
                        .with_style(
                            ProgressStyle::with_template(" {spinner} {wide_msg}")
                                .unwrap()
                                .tick_chars("--=≡■≡=--"),
                        )
                        .with_message(format!("{} {}", "Refreshing".blue(), *id)),
                );
                pb.enable_steady_tick(Duration::from_millis(150));

                self.refresh(id).await?;

                pb.suspend(|| println!("{} {}", "Refreshed".green(), *id));

                Ok(()) as Result<_, Error>
            })
            .buffer_unordered(environment::MAX_NETWORK_CONCURRENCY)
            .fold(Ok(()), |acc, result| async {
                match (acc, result) {
                    (Ok(_), Ok(_)) => Ok(()),
                    (Err(err), Ok(_)) => Err(err),
                    (Err(Error::UnsupportedRepos(a)), Err(Error::UnsupportedRepos(b))) => {
                        Err(Error::UnsupportedRepos(a.into_iter().chain(b).collect()))
                    }
                    (Err(Error::OutdatedRepos(_, a)), Err(Error::OutdatedRepos(_, b))) => Err(Error::OutdatedRepos(
                        self.source.clone(),
                        a.into_iter().chain(b).collect(),
                    )),
                    (_, Err(err)) => Err(err),
                }
            })
            .await?;

        Ok(uninitialized.len())
    }

    /// Returns the active repositories held by this manager
    pub(crate) fn active(&self) -> impl Iterator<Item = repository::Cached> + '_ {
        self.repositories.values().filter(|c| c.repository.active).cloned()
    }

    /// Resolve an exact package only from explicitly configured, active
    /// repositories. Repository priority and ID provide a stable selection
    /// order when the same content identity appears in multiple indexes.
    pub(crate) fn resolve_exact_package(
        &self,
        package: &package::Id,
    ) -> Result<Option<(repository::Id, Package)>, Error> {
        let mut repositories = self.active().collect::<Vec<_>>();
        repositories.sort_by(|left, right| {
            let left_priority = u64::from(left.repository.priority);
            let right_priority = u64::from(right.repository.priority);
            right_priority.cmp(&left_priority).then_with(|| left.id.cmp(&right.id))
        });

        for repository in repositories {
            let (snapshot, meta) = repository.db.get_with_active_snapshot(package)?;
            let snapshot = verify_active_snapshot(&repository, snapshot)?;
            let Some(mut meta) = meta else {
                continue;
            };
            meta.uri = meta
                .uri
                .and_then(|stored| Url::parse(&stored).or_else(|_| snapshot.index_uri().join(&stored)).ok())
                .map(|uri| uri.to_string());

            return Ok(Some((
                repository.id,
                Package {
                    id: package.clone(),
                    meta,
                    flags: package::Flags::new().with_available(),
                },
            )));
        }

        Ok(None)
    }

    /// Return the active repository which supplied an exact package ID, using
    /// the same priority order as registry resolution.
    pub(crate) fn repository_for_package(&self, package: &package::Id) -> Result<Option<repository::Id>, Error> {
        let mut repositories = self.active().collect::<Vec<_>>();
        repositories.sort_by(|left, right| {
            let left_priority = u64::from(left.repository.priority);
            let right_priority = u64::from(right.repository.priority);
            right_priority.cmp(&left_priority).then_with(|| left.id.cmp(&right.id))
        });
        for repository in repositories {
            let (snapshot, package_ids) = repository.db.package_ids_with_active_snapshot()?;
            verify_active_snapshot(&repository, snapshot)?;
            if package_ids.contains(package) {
                return Ok(Some(repository.id));
            }
        }
        Ok(None)
    }

    /// Return the verified content identity recorded for every active repository.
    pub fn index_snapshots(&self) -> Result<Vec<repository::IndexSnapshot>, Error> {
        let mut snapshots = self
            .active()
            .map(|repository| {
                let snapshot = verified_active_snapshot(&repository)?;
                Ok(repository::IndexSnapshot {
                    id: repository.id,
                    index_uri: snapshot.index_uri().clone(),
                    sha256: snapshot.sha256().to_owned(),
                    byte_size: snapshot.byte_size(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        snapshots.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(snapshots)
    }

    /// Hold a cross-process shared lock for every active repository and return
    /// the exact verified snapshots protected by those locks. Writers take the
    /// corresponding locks exclusively before fetching, so a multi-query
    /// resolver cannot observe A, then B, or an A -> B -> A cycle while this
    /// view is alive.
    pub(crate) fn stable_snapshot_view(&self) -> Result<StableSnapshotView, Error> {
        let mut locks = Vec::new();
        for repository in self.active() {
            locks.push(RepositorySnapshotReadLock::acquire(&repository)?);
        }
        let snapshots = self.index_snapshots()?;
        Ok(StableSnapshotView {
            snapshots,
            _locks: locks,
        })
    }

    /// Verify every active DB-selected generation before an operation is
    /// allowed to consult the error-erasing registry plugin interface. A
    /// missing legacy snapshot or corrupt high-priority repository is an error,
    /// never an invitation to fall through to a lower-priority source.
    pub(crate) fn preflight_active_snapshots(&self) -> Result<(), Error> {
        for repository in self.active() {
            verified_active_snapshot(&repository)?;
        }
        Ok(())
    }

    /// Return package identities from active repositories only after reading
    /// each set paired with its owning SQLite snapshot and verifying that exact
    /// immutable generation. This is the only repository package-ID source
    /// suitable for cache pruning.
    pub(crate) fn active_package_ids(&self) -> Result<BTreeSet<package::Id>, Error> {
        // Keep every repository generation stable until the union is complete.
        // Otherwise prune could read IDs from A, race a refresh to B, and then
        // delete assets needed by the now-active B generation.
        let _stable_view = self.stable_snapshot_view()?;
        let mut packages = BTreeSet::new();
        for repository in self.active() {
            let (snapshot, repository_packages) = repository.db.package_ids_with_active_snapshot()?;
            verify_active_snapshot(&repository, snapshot)?;
            packages.extend(repository_packages);
        }
        Ok(packages)
    }

    /// Remove a repository, deleting any related config & cached data
    pub fn remove(&mut self, id: impl Into<repository::Id>) -> Result<Removal, Error> {
        // Only allow removal for system repo manager
        let Source::ConfigManager(config) = &*self.source else {
            return Err(Error::ExplicitUnsupported);
        };

        let id = id.into();
        let Some(repo) = self.repositories.get(&id).cloned() else {
            return Ok(Removal::NotFound);
        };

        // Removal is a repository write. Wait for every operation-wide shared
        // snapshot view, then revalidate the same cache/lock boundary used by
        // refresh before any visible name is removed. Installation's global
        // mutable lock prevents another cooperative Manager from recreating
        // this namespace while its config is being deleted.
        let mutation = RepositoryMutationLock::acquire(&repo)?;
        verify_mutation_boundary(&repo, &mutation)?;
        let cache_dir = cache_dir(self.source.identifier(), &repo.id, &repo.repository, &self.installation);

        // Remove cache
        if cache_dir.exists() {
            fs::remove_dir_all(&cache_dir).map_err(Error::RemoveDir)?;
        }

        // Delete config, only succeeds for configs that live in their
        // own config file w/ matching repo name
        let adapter = repository::RepositoryCodec::default();
        let active_language = adapter.language_spec().clone();
        let evaluators = DeclarationEvaluatorSet::new([adapter])
            .expect("one validated repository adapter has no extension collision");
        if config
            .delete_declaration(&repo.id, &evaluators, &active_language)
            .is_err()
        {
            return Ok(Removal::ConfigDeleted(false));
        }
        self.repositories.remove(&id);

        Ok(Removal::ConfigDeleted(true))
    }

    /// List all of the known repositories
    pub fn list(&self) -> impl ExactSizeIterator<Item = (&repository::Id, &Repository)> {
        self.repositories.iter().map(|(id, state)| (id, &state.repository))
    }

    /// Sets the repo as active or not
    async fn set_active(&mut self, id: &repository::Id, active: bool) -> Result<(), Error> {
        // Only allow disable for system repo manager
        let Source::ConfigManager(config) = &*self.source else {
            return Err(Error::ExplicitUnsupported);
        };

        let Some(cached) = self.repositories.get_mut(id) else {
            return Err(Error::UnknownRepo(id.clone()));
        };

        if active != cached.repository.active {
            cached.repository.active = active;

            let map = repository::Map::with([(id.clone(), cached.repository.clone())]);
            let codec = repository::RepositoryCodec::default();
            let active_language = codec.language_spec().clone();
            let evaluators = DeclarationEvaluatorSet::new([codec])
                .expect("one validated repository adapter has no extension collision");
            config
                .save_declaration(
                    id,
                    &map,
                    &evaluators,
                    &active_language,
                )?;
        }

        Ok(())
    }

    /// Enable the repo
    pub async fn enable(&mut self, id: &repository::Id) -> Result<(), Error> {
        self.set_active(id, true).await
    }

    /// Disable the repo
    pub async fn disable(&mut self, id: &repository::Id) -> Result<(), Error> {
        self.set_active(id, false).await
    }
}

/// Validate one retained/buffered candidate, durably publish its exact inode,
/// then make package rows and the immutable identity visible in one DB commit.
fn activate_index_candidate(
    state: &repository::Cached,
    candidate: FetchedIndex,
    mutation: RepositoryMutationLock,
) -> Result<(), Error> {
    let cache_owner = directory_owner(&mutation.cache_directory, &state.cache_dir)?;
    let candidate_witness = inspect_file(&candidate.file, &candidate.path)?;
    require_regular_owned(&candidate.path, candidate_witness, cache_owner, None)?;
    let (bytes, after_read) = read_index_bytes(&candidate.file, &candidate.path)?;
    if after_read != candidate_witness {
        return Err(Error::IndexChanged(candidate.path.clone()));
    }
    let identity = index_identity(&bytes);
    let packages = decode_repository_index(&bytes, &state.repository.source, &candidate.index_uri)?;
    // Publication consumes a bounded immutable-generation slot. Prove every
    // package value is representable by SQLite before that irreversible step;
    // the database repeats this validation as a defense-in-depth boundary.
    meta::validate_package_batch(&packages)?;
    let snapshot = meta::Snapshot::new(candidate.index_uri.clone(), identity.sha256.clone(), identity.byte_size)?;

    let published = publish_index_candidate(state, &mutation, &candidate, &bytes, &identity)?;
    // Every inode and directory durability barrier above precedes this sole
    // authority transition. A DB failure leaves only an unreachable orphan and
    // intentionally performs no generation deletion.
    verify_mutation_boundary(state, &mutation)?;
    state.db.replace_all_with_snapshot(packages, snapshot.clone())?;

    // This cache is non-authoritative. Recovering a poisoned mutex and
    // replacing its contents cannot invalidate the already-committed DB
    // transition, and no fallible work is permitted after that transition.
    let mut cache = lock_verified_snapshot_cache(state);
    *cache = Some(VerifiedSnapshot {
        snapshot,
        file: published.file,
        witness: published.witness,
    });
    Ok(())
}

#[cfg(test)]
mod tests;
