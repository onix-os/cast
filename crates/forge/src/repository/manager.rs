// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeMap, BTreeSet, HashSet, TryReserveError};
use std::ffi::{CStr, CString, OsStr};
use std::io::{self, Cursor};
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::Duration;

use fs_err as fs;
use futures_util::{StreamExt, stream};
use gluon_config::Evaluator;
use sha2::{Digest, Sha256};
use stone::{
    StoneDecodeLimits, StoneDecodedPayload, StoneHeader, StoneHeaderV1FileType, StonePayloadKind, StonePayloadMetaTag,
    StoneReadError,
};
use thiserror::Error;
use url::Url;
#[cfg(test)]
use xxhash_rust::xxh3::xxh3_64;

use tui::{MultiProgress, ProgressBar, ProgressStyle, Styled};

use crate::{
    Installation, Package,
    db::meta,
    environment, package,
    repository::{self, Format, OutdatedRepoIndexUri, Repository, format},
    runtime,
    system_model::LoadedSystemModel,
};

const IMMUTABLE_INDEX_DIRECTORY: &str = "indexes";
const IMMUTABLE_INDEX_EXTENSION: &str = "stone";
const INDEX_CANDIDATE_NAME: &str = "candidate.stone";
const REPOSITORY_MUTATION_LOCK_NAME: &str = ".index-refresh.lock";
const INDEX_IDENTITY_BUFFER_SIZE: usize = 64 * 1024;
const MAX_INDEX_GENERATIONS: usize = 32;
const MAX_INDEX_GENERATION_BYTES: u64 = 512 * 1024 * 1024;

fn repository_index_decode_limits() -> StoneDecodeLimits {
    StoneDecodeLimits {
        max_payloads: 8_192,
        max_records_per_payload: 512,
        max_record_bytes: 64 * 1024,
        max_stored_payload_bytes: 64 * 1024,
        max_plain_payload_bytes: 256 * 1024,
        max_total_records: 262_144,
        max_total_record_bytes: 16 * 1024 * 1024,
        max_total_stored_bytes: 8 * 1024 * 1024,
        max_total_plain_bytes: 16 * 1024 * 1024,
        max_zstd_window_log: 20,
    }
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
                config
                    .load_gluon(&Evaluator::default(), &repository::RepositoryCodec)
                    .map_err(|error| Error::LoadConfig(Box::new(error)))?
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
        let config_path = config
            .save_gluon(&id, &map, &repository::RepositoryCodec)
            .map_err(|error| Error::SaveConfig(Box::new(error)))?;

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
        if config.delete_gluon::<repository::Map>(&repo.id).is_err() {
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
            config
                .save_gluon(id, &map, &repository::RepositoryCodec)
                .map_err(|error| Error::SaveConfig(Box::new(error)))?;
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

/// Directory for the repo cached data (db & stone index), hashed by identifier & repo URI
fn cache_dir(identifier: &str, id: &repository::Id, repo: &Repository, installation: &Installation) -> PathBuf {
    // Repository identity is part of the namespace. Two authored IDs may use
    // the same source intentionally, but must never share DB state, mutation
    // locks, immutable generations, or removal lifetime.
    let mut hasher = Sha256::new();
    for component in ["repository-cache-v1", identifier, id.as_ref()] {
        hasher.update(component.len().to_be_bytes());
        hasher.update(component.as_bytes());
    }
    match &repo.source {
        repository::Source::DirectIndex(uri) => {
            hasher.update(b"direct");
            hasher.update(uri.as_str().len().to_be_bytes());
            hasher.update(uri.as_str().as_bytes());
        }
        repository::Source::RootIndex(repository::RootIndexSource {
            base_uri,
            channel,
            version,
            arch,
        }) => {
            hasher.update(b"root");
            for component in [base_uri.as_str(), channel.as_ref(), &version.to_string(), arch.as_str()] {
                hasher.update(component.len().to_be_bytes());
                hasher.update(component.as_bytes());
            }
        }
    }
    installation.repo_path(hex::encode(hasher.finalize()))
}

/// Open the meta db file, ensuring it's
/// directory exists
fn open_meta_db(
    identifier: &str,
    id: &repository::Id,
    repo: &Repository,
    installation: &Installation,
) -> Result<(meta::Database, PathBuf), Error> {
    let dir = cache_dir(identifier, id, repo, installation);

    let created = match fs::create_dir(&dir) {
        Ok(()) => true,
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => false,
        Err(source) => return Err(Error::CreateDir(source)),
    };
    let directory = open_cache_path(&dir)?;
    if created {
        directory
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|source| Error::PrepareCacheDirectory {
                path: dir.clone(),
                source,
            })?;
        sync_directory_file(&directory, &dir)?;
        let parent = dir.parent().ok_or_else(|| Error::InvalidIndexPath(dir.clone()))?;
        let parent_directory = open_directory_path(parent).map_err(|source| Error::OpenCacheDirectory {
            path: parent.to_owned(),
            source,
        })?;
        sync_directory_file(&parent_directory, parent)?;
    }
    let owner = directory_owner(&directory, &dir)?;
    let db_path = dir.join("db");
    let (db_file, db_created) = open_or_create_repository_db(&directory, &db_path)?;
    let mut db_witness = inspect_file(&db_file, &db_path)?;
    require_regular_owned(&db_path, db_witness, owner, None)?;
    if db_witness.mode & 0o022 != 0 {
        return Err(Error::IndexMetadataPolicy {
            path: db_path,
            reason: "repository metadata database is writable by group or other users",
        });
    }
    if db_created {
        db_file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| Error::PrepareRepositoryDatabase {
                path: dir.join("db"),
                source,
            })?;
        db_file.sync_all().map_err(|source| Error::SyncIndexFile {
            path: dir.join("db"),
            source,
        })?;
        sync_directory_file(&directory, &dir)?;
        db_witness = inspect_file(&db_file, &dir.join("db"))?;
    }
    let db_identity = (db_witness.device, db_witness.inode);
    let directory = Arc::new(directory);
    let anchored_db_path = proc_fd_path(&directory).join("db");
    let db = meta::Database::new_anchored(anchored_db_path.to_str().unwrap_or_default(), directory.clone())?;
    let current = witness_at(
        &directory,
        CStr::from_bytes_with_nul(b"db\0").expect("static C string"),
        &dir.join("db"),
    )?
    .ok_or_else(|| Error::IndexPathChanged(dir.join("db")))?;
    if (current.device, current.inode) != db_identity {
        return Err(Error::IndexPathChanged(dir.join("db")));
    }

    Ok((db, dir))
}

fn open_or_create_repository_db(directory: &fs::File, path: &Path) -> Result<(fs::File, bool), Error> {
    let flags =
        nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CREAT;
    match openat2_file(
        directory.as_raw_fd(),
        b"db",
        flags | nix::libc::O_EXCL,
        0o600,
        descendant_resolution(),
        path,
    ) {
        Ok(file) => Ok((file, true)),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => openat2_file(
            directory.as_raw_fd(),
            b"db",
            flags & !nix::libc::O_CREAT,
            0,
            descendant_resolution(),
            path,
        )
        .map(|file| (file, false))
        .map_err(|source| Error::OpenRepositoryDatabase {
            path: path.to_owned(),
            source,
        }),
        Err(source) => Err(Error::OpenRepositoryDatabase {
            path: path.to_owned(),
            source,
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileWitness {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            length: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    fn from_stat(stat: &nix::libc::stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            links: stat.st_nlink,
            uid: stat.st_uid,
            gid: stat.st_gid,
            length: stat.st_size.try_into().unwrap_or(u64::MAX),
            modified_seconds: stat.st_mtime,
            modified_nanoseconds: stat.st_mtime_nsec,
            changed_seconds: stat.st_ctime,
            changed_nanoseconds: stat.st_ctime_nsec,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexIdentity {
    sha256: String,
    byte_size: u64,
}

/// Cached proof for one DB-selected generation. It never selects a generation:
/// callers must first read the complete [`meta::Snapshot`] from SQLite and the
/// key must match it. Retaining the descriptor also keeps an inode alive if GC
/// unlinks an older generation while a bounded operation is finishing.
#[derive(Debug)]
pub(crate) struct VerifiedSnapshot {
    snapshot: meta::Snapshot,
    file: Arc<fs::File>,
    witness: FileWitness,
}

struct RepositoryMutationLock {
    cache_directory: fs::File,
    cache_identity: DirectoryIdentity,
    lock_file: fs::File,
    lock_witness: FileWitness,
}

pub(crate) struct StableSnapshotView {
    snapshots: Vec<repository::IndexSnapshot>,
    // Drop only after every query using this view and its snapshot copy has
    // completed. Each file owns one process-wide advisory shared lock.
    _locks: Vec<RepositorySnapshotReadLock>,
}

impl StableSnapshotView {
    pub(crate) fn snapshots(&self) -> &[repository::IndexSnapshot] {
        &self.snapshots
    }
}

struct RepositorySnapshotReadLock {
    _cache_directory: fs::File,
    _lock_file: fs::File,
}

impl RepositorySnapshotReadLock {
    fn acquire(state: &repository::Cached) -> Result<Self, Error> {
        let cache_directory = open_cache_directory(state)?;
        let owner = directory_owner(&cache_directory, &state.cache_dir)?;
        let lock_path = state.cache_dir.join(REPOSITORY_MUTATION_LOCK_NAME);
        let lock_file = openat2_file(
            cache_directory.as_raw_fd(),
            REPOSITORY_MUTATION_LOCK_NAME.as_bytes(),
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            descendant_resolution(),
            &lock_path,
        )
        .map_err(|source| Error::OpenRepositoryMutationLock {
            path: lock_path.clone(),
            source,
        })?;
        let witness = inspect_file(&lock_file, &lock_path)?;
        require_regular_owned(&lock_path, witness, owner, Some(0o600))?;

        loop {
            // SAFETY: `lock_file` is live. LOCK_SH blocks behind the writer's
            // LOCK_EX and is held by this descriptor until the view is dropped.
            if unsafe { nix::libc::flock(lock_file.as_raw_fd(), nix::libc::LOCK_SH) } == 0 {
                break;
            }
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::Interrupted {
                return Err(Error::LockRepositorySnapshot {
                    path: lock_path,
                    source,
                });
            }
        }

        require_name_witness(
            &cache_directory,
            CStr::from_bytes_with_nul(b".index-refresh.lock\0").expect("static C string"),
            witness,
            &lock_path,
        )?;
        if inspect_file(&lock_file, &lock_path)? != witness {
            return Err(Error::IndexPathChanged(lock_path));
        }
        Ok(Self {
            _cache_directory: cache_directory,
            _lock_file: lock_file,
        })
    }
}

impl RepositoryMutationLock {
    fn acquire(state: &repository::Cached) -> Result<Self, Error> {
        let cache_directory = open_cache_directory(state)?;
        let owner = directory_owner(&cache_directory, &state.cache_dir)?;
        let cache_identity = directory_identity(&cache_directory, &state.cache_dir)?;
        let lock_path = state.cache_dir.join(REPOSITORY_MUTATION_LOCK_NAME);
        let lock_file = openat2_file(
            cache_directory.as_raw_fd(),
            REPOSITORY_MUTATION_LOCK_NAME.as_bytes(),
            nix::libc::O_RDWR | nix::libc::O_CREAT | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0o600,
            descendant_resolution(),
            &lock_path,
        )
        .map_err(|source| Error::OpenRepositoryMutationLock {
            path: lock_path.clone(),
            source,
        })?;
        let witness = inspect_file(&lock_file, &lock_path)?;
        require_regular_owned(&lock_path, witness, owner, Some(0o600))?;
        lock_file.sync_all().map_err(|source| Error::SyncIndexFile {
            path: lock_path.clone(),
            source,
        })?;
        sync_directory_file(&cache_directory, &state.cache_dir)?;

        loop {
            // SAFETY: `lock_file` is a live descriptor. LOCK_EX coordinates
            // independent opens in this and other processes.
            if unsafe { nix::libc::flock(lock_file.as_raw_fd(), nix::libc::LOCK_EX) } == 0 {
                break;
            }
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::Interrupted {
                return Err(Error::LockRepositoryMutation {
                    path: lock_path,
                    source,
                });
            }
        }

        require_name_witness(
            &cache_directory,
            CStr::from_bytes_with_nul(b".index-refresh.lock\0").expect("static C string"),
            witness,
            &lock_path,
        )?;
        Ok(Self {
            cache_directory,
            cache_identity,
            lock_file,
            lock_witness: witness,
        })
    }
}

struct FetchedIndex {
    // Field order matters: TempDir cleanup uses the retained cache descriptor's
    // `/proc/self/fd` path, so it must be dropped before that descriptor.
    _directory: tempfile::TempDir,
    _cache_directory: fs::File,
    directory: fs::File,
    file: fs::File,
    path: PathBuf,
    index_uri: Url,
}

/// Download one candidate below an already authenticated cache descriptor.
/// The downloader receives an intentional `/proc/self/fd/<n>` capability path;
/// every trusted read and the eventual rename remain descriptor-relative.
async fn fetch_index(
    source: &Arc<Source>,
    state: &repository::Cached,
    cache_directory: &fs::File,
) -> Result<FetchedIndex, Error> {
    let index_uri = match &state.repository.source {
        repository::Source::DirectIndex(uri) => match identify_legacy_index_uri(uri) {
            None => uri.clone(),
            Some(legacy_index) => {
                let root_source = legacy_index.compatible_root_index_source();
                if let Ok(root_index) = root_source.fetch_root_index().await
                    && let Some((_, history)) = root_index.resolve_version_to_history(&root_source.version)
                    && history.format != Format::Legacy
                {
                    return Err(Error::OutdatedRepos(
                        source.clone(),
                        vec![OutdatedRepoIndexUri {
                            repository: state.clone(),
                            legacy_index_uri: uri.clone(),
                            compatible_root_index_source: root_source,
                        }],
                    ));
                }
                uri.clone()
            }
        },
        repository::Source::RootIndex(source) => resolve_index_from_root(state, source).await?,
    };

    let cache_clone = cache_directory
        .try_clone()
        .map_err(|source| Error::OpenCacheDirectory {
            path: state.cache_dir.clone(),
            source,
        })?;
    let capability = proc_fd_path(&cache_clone);
    let directory = tempfile::Builder::new()
        .prefix(".index-candidate-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&capability)
        .map_err(|source| Error::CreateIndexCandidate {
            parent: state.cache_dir.clone(),
            source,
        })?;
    let name = directory
        .path()
        .file_name()
        .ok_or_else(|| Error::InvalidIndexPath(directory.path().to_owned()))?;
    let directory_display = state.cache_dir.join(name);
    let directory_file = openat2_file(
        cache_directory.as_raw_fd(),
        name.as_bytes(),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        &directory_display,
    )
    .map_err(|source| Error::OpenIndexCandidateDirectory {
        path: directory_display.clone(),
        source,
    })?;
    let cache_owner = directory_owner(cache_directory, &state.cache_dir)?;
    require_directory_owned(&directory_file, &directory_display, cache_owner, Some(0o700))?;

    let path = proc_fd_path(&directory_file).join(INDEX_CANDIDATE_NAME);
    repository::fetch_index(index_uri.clone(), &path).await?;
    let display_path = directory_display.join(INDEX_CANDIDATE_NAME);
    let file = openat2_file(
        directory_file.as_raw_fd(),
        INDEX_CANDIDATE_NAME.as_bytes(),
        nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        &display_path,
    )
    .map_err(|source| Error::OpenIndex {
        path: display_path.clone(),
        source,
    })?;
    let witness = inspect_file(&file, &display_path)?;
    require_regular_owned(&display_path, witness, cache_owner, None)?;
    require_name_witness(
        &directory_file,
        CStr::from_bytes_with_nul(b"candidate.stone\0").expect("static C string"),
        witness,
        &display_path,
    )?;

    Ok(FetchedIndex {
        _directory: directory,
        _cache_directory: cache_clone,
        directory: directory_file,
        file,
        path: display_path,
        index_uri,
    })
}

#[derive(Debug, Clone, Copy)]
struct DirectoryOwner {
    device: u64,
    uid: u32,
    gid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
}

fn directory_identity(directory: &fs::File, path: &Path) -> Result<DirectoryIdentity, Error> {
    let metadata = directory.metadata().map_err(|source| Error::InspectIndex {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_dir()
        || metadata.mode() & 0o022 != 0
        || metadata.uid() != nix::unistd::Uid::effective().as_raw()
    {
        return Err(Error::IndexDirectoryPolicy {
            path: path.to_owned(),
            reason: "repository cache must be a directory not writable by group or other users",
        });
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        mode: metadata.mode() & 0o7777,
    })
}

fn directory_owner(directory: &fs::File, path: &Path) -> Result<DirectoryOwner, Error> {
    let metadata = directory.metadata().map_err(|source| Error::InspectIndex {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_dir() {
        return Err(Error::IndexDirectoryPolicy {
            path: path.to_owned(),
            reason: "not a directory",
        });
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(Error::IndexDirectoryPolicy {
            path: path.to_owned(),
            reason: "repository cache directory is writable by group or other users",
        });
    }
    // Never adopt an attacker-precreated cache namespace, including when Cast
    // is privileged. Group may legitimately be inherited from a setgid parent,
    // but ownership must be the effective caller.
    if metadata.uid() != nix::unistd::Uid::effective().as_raw() {
        return Err(Error::IndexDirectoryPolicy {
            path: path.to_owned(),
            reason: "repository cache directory is not owned by the effective user",
        });
    }
    Ok(DirectoryOwner {
        device: metadata.dev(),
        uid: metadata.uid(),
        gid: metadata.gid(),
    })
}

fn require_directory_owned(
    directory: &fs::File,
    path: &Path,
    owner: DirectoryOwner,
    exact_mode: Option<u32>,
) -> Result<(), Error> {
    let metadata = directory.metadata().map_err(|source| Error::InspectIndex {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_dir()
        || metadata.dev() != owner.device
        || metadata.uid() != owner.uid
        || metadata.gid() != owner.gid
        || exact_mode.is_some_and(|mode| metadata.mode() & 0o7777 != mode)
    {
        return Err(Error::IndexDirectoryPolicy {
            path: path.to_owned(),
            reason: "directory type, filesystem, ownership, or mode does not match the repository cache",
        });
    }
    Ok(())
}

fn inspect_file(file: &fs::File, path: &Path) -> Result<FileWitness, Error> {
    file.metadata()
        .map(|metadata| FileWitness::from_metadata(&metadata))
        .map_err(|source| Error::InspectIndex {
            path: path.to_owned(),
            source,
        })
}

fn require_regular_owned(
    path: &Path,
    witness: FileWitness,
    owner: DirectoryOwner,
    exact_mode: Option<u32>,
) -> Result<(), Error> {
    if witness.mode & nix::libc::S_IFMT != nix::libc::S_IFREG {
        return Err(Error::IndexNotRegular(path.to_owned()));
    }
    if witness.device != owner.device {
        return Err(Error::IndexMetadataPolicy {
            path: path.to_owned(),
            reason: "file is not on the repository cache filesystem",
        });
    }
    if witness.links != 1 {
        return Err(Error::IndexMetadataPolicy {
            path: path.to_owned(),
            reason: "file must have exactly one hard link",
        });
    }
    if witness.uid != owner.uid || witness.gid != owner.gid {
        return Err(Error::IndexMetadataPolicy {
            path: path.to_owned(),
            reason: "file ownership does not match the repository cache",
        });
    }
    if exact_mode.is_some_and(|mode| witness.mode & 0o7777 != mode) {
        return Err(Error::IndexMetadataPolicy {
            path: path.to_owned(),
            reason: "file mode does not match the required immutable mode",
        });
    }
    Ok(())
}

/// Read one retained inode into a bounded buffer. The complete metadata
/// witness must be unchanged across the read; decoding later consumes only
/// this buffer, never a replaceable path or a second file.
fn read_index_bytes(file: &fs::File, path: &Path) -> Result<(Vec<u8>, FileWitness), Error> {
    let before = inspect_file(file, path)?;
    let limit = repository::REPOSITORY_INDEX_DOWNLOAD_LIMITS.max_bytes;
    if before.length > limit {
        return Err(Error::IndexTooLarge {
            path: path.to_owned(),
            limit,
        });
    }
    let initial = usize::try_from(before.length).map_err(|_| Error::IndexTooLarge {
        path: path.to_owned(),
        limit,
    })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(initial).map_err(Error::ReserveIndexBytes)?;
    let mut buffer = [0_u8; INDEX_IDENTITY_BUFFER_SIZE];
    let mut offset = 0_u64;
    loop {
        let remaining = limit.saturating_sub(offset);
        let requested = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let read = loop {
            // SAFETY: buffer and descriptor remain live; offset is bounded by
            // the download limit and therefore representable by off_t here.
            let result = unsafe {
                nix::libc::pread(
                    file.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    requested,
                    offset as nix::libc::off_t,
                )
            };
            if result >= 0 {
                break result as usize;
            }
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::Interrupted {
                return Err(Error::ReadIndex {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        if read == 0 {
            break;
        }
        if read as u64 > remaining {
            return Err(Error::IndexTooLarge {
                path: path.to_owned(),
                limit,
            });
        }
        bytes.try_reserve(read).map_err(Error::ReserveIndexBytes)?;
        bytes.extend_from_slice(&buffer[..read]);
        offset += read as u64;
    }
    let after = inspect_file(file, path)?;
    if before != after || after.length != offset {
        return Err(Error::IndexChanged(path.to_owned()));
    }
    Ok((bytes, after))
}

fn index_identity(bytes: &[u8]) -> IndexIdentity {
    IndexIdentity {
        sha256: hex::encode(Sha256::digest(bytes)),
        byte_size: bytes.len() as u64,
    }
}

fn verify_identity(path: &Path, bytes: &[u8], expected: &IndexIdentity) -> Result<(), Error> {
    let actual = index_identity(bytes);
    if actual.byte_size != expected.byte_size {
        return Err(Error::IndexSizeMismatch {
            path: path.to_owned(),
            expected: expected.byte_size,
            actual: actual.byte_size,
        });
    }
    if actual.sha256 != expected.sha256 {
        return Err(Error::IndexHashMismatch {
            path: path.to_owned(),
            expected: expected.sha256.clone(),
            actual: actual.sha256,
        });
    }
    Ok(())
}

pub(crate) fn immutable_index_path(state: &repository::Cached, sha256: &str) -> PathBuf {
    state
        .cache_dir
        .join(IMMUTABLE_INDEX_DIRECTORY)
        .join(format!("{sha256}.{IMMUTABLE_INDEX_EXTENSION}"))
}

fn immutable_index_name(sha256: &str) -> Result<CString, Error> {
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::InvalidImmutableIndexName(sha256.to_owned()));
    }
    CString::new(format!("{sha256}.{IMMUTABLE_INDEX_EXTENSION}"))
        .map_err(|_| Error::InvalidImmutableIndexName(sha256.to_owned()))
}

fn sync_directory_file(directory: &fs::File, path: &Path) -> Result<(), Error> {
    directory.sync_all().map_err(|source| Error::SyncIndexDirectory {
        path: path.to_owned(),
        source,
    })
}

fn open_cache_directory(state: &repository::Cached) -> Result<fs::File, Error> {
    open_cache_path(&state.cache_dir)
}

fn open_cache_path(path: &Path) -> Result<fs::File, Error> {
    open_directory_path(path).map_err(|source| Error::OpenCacheDirectory {
        path: path.to_owned(),
        source,
    })
}

fn open_directory_path(path: &Path) -> io::Result<fs::File> {
    openat2_file(
        nix::libc::AT_FDCWD,
        path.as_os_str().as_bytes(),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS,
        path,
    )
}

fn open_indexes_directory(
    state: &repository::Cached,
    cache_directory: &fs::File,
    create: bool,
) -> Result<fs::File, Error> {
    let path = state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY);
    if create {
        let name = CStr::from_bytes_with_nul(b"indexes\0").expect("static C string");
        // SAFETY: cache descriptor and static single-component name are live.
        if unsafe { nix::libc::mkdirat(cache_directory.as_raw_fd(), name.as_ptr(), 0o700) } == -1 {
            let source = io::Error::last_os_error();
            if source.kind() != io::ErrorKind::AlreadyExists {
                return Err(Error::CreateIndexDirectory { path, source });
            }
        }
    }
    let directory = openat2_file(
        cache_directory.as_raw_fd(),
        IMMUTABLE_INDEX_DIRECTORY.as_bytes(),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        &path,
    )
    .map_err(|source| Error::OpenIndexDirectory {
        path: path.clone(),
        source,
    })?;
    let owner = directory_owner(cache_directory, &state.cache_dir)?;
    require_directory_owned(&directory, &path, owner, Some(0o700))?;
    Ok(directory)
}

struct DirectoryStream(NonNull<nix::libc::DIR>);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the stream returned by fdopendir.
        unsafe {
            nix::libc::closedir(self.0.as_ptr());
        }
    }
}

fn immutable_index_entry_names(directory: &fs::File, path: &Path) -> Result<Vec<CString>, Error> {
    // Open a fresh directory description rather than dup'ing `directory`:
    // dup would share its enumeration offset with earlier calls.
    let cursor = openat2_file(
        directory.as_raw_fd(),
        b".",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        path,
    )
    .map_err(|source| Error::ReadIndexDirectory {
        path: path.to_owned(),
        source,
    })?;
    let descriptor = cursor.into_raw_fd();
    // SAFETY: fdopendir consumes this fresh owned directory descriptor on
    // success; on failure it remains ours and is closed below.
    let stream = unsafe { nix::libc::fdopendir(descriptor) };
    let Some(stream) = NonNull::new(stream) else {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and therefore did not consume descriptor.
        unsafe {
            nix::libc::close(descriptor);
        }
        return Err(Error::ReadIndexDirectory {
            path: path.to_owned(),
            source,
        });
    };
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        // SAFETY: errno is thread-local on Linux and readdir uses null for
        // both end-of-directory and failure.
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
            return Err(Error::ReadIndexDirectory {
                path: path.to_owned(),
                source: io::Error::from_raw_os_error(errno),
            });
        }
        // SAFETY: readdir returned a NUL-terminated name valid until the next
        // operation on this stream; copy it before advancing.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        if names.len() > MAX_INDEX_GENERATIONS {
            return Err(Error::IndexGenerationLimit {
                limit: MAX_INDEX_GENERATIONS,
            });
        }
        names.push(CString::new(bytes).expect("readdir names contain no interior NUL"));
    }
    Ok(names)
}

/// Refuse publication before it can grow the immutable generation directory
/// beyond a fixed count or byte budget. Existing content hashes remain usable
/// at the limit, so an idempotent refresh cannot be turned into a failure. We
/// deliberately do not delete old generations here: cross-process readers may
/// still hold them as the authority for an in-flight resolution.
fn enforce_index_generation_budget(
    state: &repository::Cached,
    indexes_directory: &fs::File,
    owner: DirectoryOwner,
    target_name: &CStr,
    identity: &IndexIdentity,
) -> Result<(), Error> {
    let target_path = immutable_index_path(state, &identity.sha256);
    if witness_at(indexes_directory, target_name, &target_path)?.is_some() {
        return Ok(());
    }

    let indexes_path = state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY);
    let names = immutable_index_entry_names(indexes_directory, &indexes_path)?;
    if names.len() >= MAX_INDEX_GENERATIONS {
        return Err(Error::IndexGenerationLimit {
            limit: MAX_INDEX_GENERATIONS,
        });
    }

    let mut bytes = 0_u64;
    for name in names {
        let raw_name = name.to_bytes();
        let Some(hash) = raw_name.strip_suffix(b".stone") else {
            return Err(Error::InvalidIndexDirectoryEntry(
                indexes_path.join(OsStr::from_bytes(raw_name)),
            ));
        };
        if hash.len() != 64
            || !hash
                .iter()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(Error::InvalidIndexDirectoryEntry(
                indexes_path.join(OsStr::from_bytes(raw_name)),
            ));
        }
        let path = indexes_path.join(OsStr::from_bytes(raw_name));
        let file = openat2_file(
            indexes_directory.as_raw_fd(),
            raw_name,
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            0,
            descendant_resolution(),
            &path,
        )
        .map_err(|source| Error::OpenIndex {
            path: path.clone(),
            source,
        })?;
        let witness = inspect_file(&file, &path)?;
        require_regular_owned(&path, witness, owner, Some(0o444))?;
        bytes = bytes
            .checked_add(witness.length)
            .ok_or(Error::IndexGenerationByteLimit {
                limit: MAX_INDEX_GENERATION_BYTES,
            })?;
    }
    if !matches!(
        bytes.checked_add(identity.byte_size),
        Some(total) if total <= MAX_INDEX_GENERATION_BYTES
    ) {
        return Err(Error::IndexGenerationByteLimit {
            limit: MAX_INDEX_GENERATION_BYTES,
        });
    }
    Ok(())
}

struct PublishedIndex {
    file: Arc<fs::File>,
    witness: FileWitness,
}

/// Seal and atomically rename the exact retained candidate inode. Hard links
/// are deliberately forbidden: the active object has nlink=1 before, during,
/// and after the DB commit. An EEXIST race converges only after byte-for-byte
/// and metadata verification of the independently retained final descriptor.
fn publish_index_candidate(
    state: &repository::Cached,
    mutation: &RepositoryMutationLock,
    candidate: &FetchedIndex,
    bytes: &[u8],
    identity: &IndexIdentity,
) -> Result<PublishedIndex, Error> {
    verify_mutation_boundary(state, mutation)?;
    let cache_owner = directory_owner(&mutation.cache_directory, &state.cache_dir)?;
    require_directory_owned(
        &candidate.directory,
        candidate
            .path
            .parent()
            .ok_or_else(|| Error::InvalidIndexPath(candidate.path.clone()))?,
        cache_owner,
        Some(0o700),
    )?;
    let before = inspect_file(&candidate.file, &candidate.path)?;
    require_regular_owned(&candidate.path, before, cache_owner, None)?;
    let (confirmed, read_witness) = read_index_bytes(&candidate.file, &candidate.path)?;
    if confirmed != bytes || read_witness != before {
        return Err(Error::IndexChanged(candidate.path.clone()));
    }

    candidate
        .file
        .set_permissions(std::fs::Permissions::from_mode(0o444))
        .map_err(|source| Error::PrepareIndexCandidate {
            path: candidate.path.clone(),
            source,
        })?;
    candidate.file.sync_all().map_err(|source| Error::SyncIndexFile {
        path: candidate.path.clone(),
        source,
    })?;
    let sealed = inspect_file(&candidate.file, &candidate.path)?;
    require_regular_owned(&candidate.path, sealed, cache_owner, Some(0o444))?;
    let (sealed_bytes, sealed_after_read) = read_index_bytes(&candidate.file, &candidate.path)?;
    if sealed_bytes != bytes || sealed_after_read != sealed {
        return Err(Error::IndexChanged(candidate.path.clone()));
    }
    require_name_witness(
        &candidate.directory,
        CStr::from_bytes_with_nul(b"candidate.stone\0").expect("static C string"),
        sealed,
        &candidate.path,
    )?;

    let indexes_directory = open_indexes_directory(state, &mutation.cache_directory, true)?;
    let target_path = immutable_index_path(state, &identity.sha256);
    let target_name = immutable_index_name(&identity.sha256)?;
    enforce_index_generation_budget(state, &indexes_directory, cache_owner, &target_name, identity)?;
    let source_name = CStr::from_bytes_with_nul(b"candidate.stone\0").expect("static C string");
    // SAFETY: both retained directory descriptors and names remain live.
    // RENAME_NOREPLACE either moves the authenticated inode or changes nothing.
    let renamed = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            candidate.directory.as_raw_fd(),
            source_name.as_ptr(),
            indexes_directory.as_raw_fd(),
            target_name.as_ptr(),
            nix::libc::RENAME_NOREPLACE,
        )
    };
    let created = if renamed == 0 {
        true
    } else {
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::AlreadyExists {
            false
        } else {
            return Err(Error::PublishIndex {
                source_path: candidate.path.clone(),
                target: target_path,
                source,
            });
        }
    };

    if !created {
        require_name_witness(&candidate.directory, source_name, sealed, &candidate.path)?;
    }
    let final_file = openat2_file(
        indexes_directory.as_raw_fd(),
        target_name.to_bytes(),
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        &target_path,
    )
    .map_err(|source| Error::OpenIndex {
        path: target_path.clone(),
        source,
    })?;
    let final_before = inspect_file(&final_file, &target_path)?;
    require_regular_owned(&target_path, final_before, cache_owner, Some(0o444))?;
    if created && (final_before.device != sealed.device || final_before.inode != sealed.inode) {
        return Err(Error::IndexPathChanged(target_path));
    }
    let (final_bytes, final_after) = read_index_bytes(&final_file, &target_path)?;
    require_regular_owned(&target_path, final_after, cache_owner, Some(0o444))?;
    if final_bytes != bytes || final_after != final_before {
        return Err(Error::IndexChanged(target_path));
    }
    verify_identity(&target_path, &final_bytes, identity)?;
    final_file.sync_all().map_err(|source| Error::SyncIndexFile {
        path: target_path.clone(),
        source,
    })?;
    sync_directory_file(
        &candidate.directory,
        candidate
            .path
            .parent()
            .ok_or_else(|| Error::InvalidIndexPath(candidate.path.clone()))?,
    )?;
    sync_directory_file(&indexes_directory, &state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY))?;
    sync_directory_file(&mutation.cache_directory, &state.cache_dir)?;
    require_name_witness(&indexes_directory, &target_name, final_after, &target_path)?;

    Ok(PublishedIndex {
        file: Arc::new(final_file),
        witness: final_after,
    })
}

fn verify_mutation_boundary(state: &repository::Cached, mutation: &RepositoryMutationLock) -> Result<(), Error> {
    let reopened_cache = open_cache_directory(state)?;
    if directory_identity(&reopened_cache, &state.cache_dir)? != mutation.cache_identity {
        return Err(Error::IndexPathChanged(state.cache_dir.clone()));
    }
    let lock_path = state.cache_dir.join(REPOSITORY_MUTATION_LOCK_NAME);
    require_name_witness(
        &mutation.cache_directory,
        CStr::from_bytes_with_nul(b".index-refresh.lock\0").expect("static C string"),
        mutation.lock_witness,
        &lock_path,
    )?;
    if inspect_file(&mutation.lock_file, &lock_path)? != mutation.lock_witness {
        return Err(Error::IndexPathChanged(lock_path));
    }
    Ok(())
}

fn decode_repository_index(
    bytes: &[u8],
    source: &repository::Source,
    index_uri: &Url,
) -> Result<Vec<(package::Id, package::Meta)>, Error> {
    let mut cursor = Cursor::new(bytes);
    let mut reader = stone::read_with_limits(&mut cursor, repository_index_decode_limits())?;
    let file_type = match reader.header {
        StoneHeader::V1(header) => header.file_type,
    };
    if file_type != StoneHeaderV1FileType::Repository {
        return Err(Error::UnexpectedIndexFileType(file_type));
    }

    let package_count = usize::from(reader.header.num_payloads());
    let mut packages = Vec::new();
    packages
        .try_reserve_exact(package_count)
        .map_err(Error::ReserveIndexPackages)?;
    let mut package_ids = HashSet::new();
    package_ids
        .try_reserve(package_count)
        .map_err(Error::ReserveIndexPackageIds)?;

    for (index, payload) in reader.payloads()?.enumerate() {
        let payload = payload?;
        let StoneDecodedPayload::Meta(payload) = payload else {
            return Err(Error::UnexpectedIndexPayload {
                index,
                kind: payload.header().kind,
            });
        };
        let mut meta = package::Meta::from_repository_index_payload(&payload.body)
            .map_err(|source| Error::InvalidRepositoryMeta { index, source })?;
        let hash = meta.hash.clone().ok_or(Error::RepositoryMetaInvariant { index })?;
        let id = package::Id::from(hash);
        if !package_ids.insert(id.clone()) {
            return Err(Error::DuplicateIndexPackage { index });
        }
        let raw_uri = meta.uri.as_deref().ok_or(Error::RepositoryMetaInvariant { index })?;
        let uri = normalize_repository_package_uri(source, index_uri, raw_uri)
            .map_err(|source| Error::InvalidRepositoryPackageUri { index, source })?;
        meta.uri = Some(uri.into());
        packages.push((id, meta));
    }

    Ok(packages)
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

pub(crate) fn verify_active_snapshot(
    state: &repository::Cached,
    snapshot: Option<meta::Snapshot>,
) -> Result<meta::Snapshot, Error> {
    let snapshot = snapshot.ok_or_else(|| Error::MissingActiveSnapshot(state.id.clone()))?;
    let target_path = immutable_index_path(state, snapshot.sha256());
    let target_name = immutable_index_name(snapshot.sha256())?;
    let expected = IndexIdentity {
        sha256: snapshot.sha256().to_owned(),
        byte_size: snapshot.byte_size(),
    };
    let mut cache = lock_verified_snapshot_cache(state);
    if let Some(verified) = cache.as_ref()
        && verified.snapshot == snapshot
    {
        let held = inspect_file(&verified.file, &target_path)?;
        if held != verified.witness {
            return Err(Error::IndexChanged(target_path));
        }
        let cache_directory = open_cache_directory(state)?;
        let indexes_directory = open_indexes_directory(state, &cache_directory, false)?;
        let current = openat2_file(
            indexes_directory.as_raw_fd(),
            target_name.to_bytes(),
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            0,
            descendant_resolution(),
            &target_path,
        )
        .map_err(|source| Error::OpenIndex {
            path: target_path.clone(),
            source,
        })?;
        let current_witness = inspect_file(&current, &target_path)?;
        if current_witness != verified.witness {
            return Err(Error::IndexPathChanged(target_path));
        }
        return Ok(snapshot);
    }

    let cache_directory = open_cache_directory(state)?;
    let owner = directory_owner(&cache_directory, &state.cache_dir)?;
    let indexes_directory = open_indexes_directory(state, &cache_directory, false)?;
    let file = openat2_file(
        indexes_directory.as_raw_fd(),
        target_name.to_bytes(),
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        descendant_resolution(),
        &target_path,
    )
    .map_err(|source| Error::OpenIndex {
        path: target_path.clone(),
        source,
    })?;
    let before = inspect_file(&file, &target_path)?;
    require_regular_owned(&target_path, before, owner, Some(0o444))?;
    let (bytes, after) = read_index_bytes(&file, &target_path)?;
    require_regular_owned(&target_path, after, owner, Some(0o444))?;
    if before != after {
        return Err(Error::IndexChanged(target_path));
    }
    verify_identity(&target_path, &bytes, &expected)?;
    require_name_witness(&indexes_directory, &target_name, after, &target_path)?;
    let file = Arc::new(file);
    *cache = Some(VerifiedSnapshot {
        snapshot: snapshot.clone(),
        file,
        witness: after,
    });
    Ok(snapshot)
}

pub(crate) fn verified_active_snapshot(state: &repository::Cached) -> Result<meta::Snapshot, Error> {
    verify_active_snapshot(state, state.db.active_snapshot()?)
}

fn repository_is_initialized(state: &repository::Cached) -> Result<bool, Error> {
    let Some(snapshot) = state.db.active_snapshot()? else {
        return Ok(false);
    };
    Ok(verify_active_snapshot(state, Some(snapshot)).is_ok())
}

fn lock_verified_snapshot_cache(state: &repository::Cached) -> std::sync::MutexGuard<'_, Option<VerifiedSnapshot>> {
    match state.verified_snapshot.lock() {
        Ok(cache) => cache,
        Err(poisoned) => {
            let mut cache = poisoned.into_inner();
            *cache = None;
            state.verified_snapshot.clear_poison();
            cache
        }
    }
}

fn require_name_witness(directory: &fs::File, name: &CStr, expected: FileWitness, path: &Path) -> Result<(), Error> {
    match witness_at(directory, name, path)? {
        Some(actual) if actual == expected => Ok(()),
        _ => Err(Error::IndexPathChanged(path.to_owned())),
    }
}

fn witness_at(directory: &fs::File, name: &CStr, path: &Path) -> Result<Option<FileWitness>, Error> {
    let mut stat = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
    // SAFETY: directory/name are live and stat points to writable storage.
    if unsafe {
        nix::libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            nix::libc::AT_SYMLINK_NOFOLLOW,
        )
    } == -1
    {
        let source = io::Error::last_os_error();
        return if source.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(Error::InspectIndex {
                path: path.to_owned(),
                source,
            })
        };
    }
    // SAFETY: successful fstatat initialized stat.
    let stat = unsafe { stat.assume_init() };
    Ok(Some(FileWitness::from_stat(&stat)))
}

fn descendant_resolution() -> u64 {
    nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV
}

fn openat2_file(
    dirfd: RawFd,
    path: &[u8],
    flags: i32,
    mode: u32,
    resolve: u64,
    display_path: &Path,
) -> io::Result<fs::File> {
    let path = CString::new(path).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: zero is valid for every open_how field.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: arguments remain live and a successful call returns a fresh fd.
    let descriptor = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    let raw = RawFd::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned a fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
    Ok(fs::File::from_parts(descriptor.into(), display_path))
}

fn proc_fd_path(file: &fs::File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

fn validate_repository_source(repository: &Repository) -> Result<(), Error> {
    match &repository.source {
        repository::Source::DirectIndex(uri) => validate_repository_transport(uri),
        repository::Source::RootIndex(source) => {
            validate_repository_transport(&source.base_uri)?;
            let arch = source.arch.as_bytes();
            if arch.is_empty()
                || arch.len() > 64
                || matches!(arch, b"." | b"..")
                || !arch
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            {
                return Err(Error::InvalidRootArchitecture(source.arch.clone()));
            }
            Ok(())
        }
    }
}

fn validate_repository_transport(uri: &Url) -> Result<(), Error> {
    if !uri.username().is_empty() || uri.password().is_some() || uri.fragment().is_some() {
        return Err(Error::RepositoryTransportPolicy {
            reason: "credentials and fragments are forbidden",
        });
    }
    match uri.scheme() {
        "https" if uri.host_str().is_some() => Ok(()),
        "file" if uri.host_str().is_none() && uri.query().is_none() && uri.to_file_path().is_ok() => Ok(()),
        "http" => Err(Error::RepositoryTransportPolicy {
            reason: "plaintext HTTP repositories are forbidden; use HTTPS or a local file repository",
        }),
        _ => Err(Error::RepositoryTransportPolicy {
            reason: "repository transport must be HTTPS or a local file URL without authority/query",
        }),
    }
}

fn normalize_repository_package_uri(
    source: &repository::Source,
    index_uri: &Url,
    raw_uri: &str,
) -> Result<Url, PackageUriError> {
    if raw_uri.is_empty() {
        return Err(PackageUriError::Empty);
    }
    if raw_uri.as_bytes().contains(&b'\\') || contains_encoded_path_control(raw_uri) {
        return Err(PackageUriError::EncodedTraversal);
    }
    if Url::parse(raw_uri).is_ok() {
        return Err(PackageUriError::AbsoluteReference);
    }

    let resolved = index_uri.join(raw_uri).map_err(PackageUriError::Resolve)?;
    if !resolved.username().is_empty() || resolved.password().is_some() {
        return Err(PackageUriError::Credentials);
    }
    if resolved.fragment().is_some() {
        return Err(PackageUriError::Fragment);
    }
    if resolved.query().is_some() {
        return Err(PackageUriError::Query);
    }
    if resolved.scheme() != index_uri.scheme()
        || resolved.host_str() != index_uri.host_str()
        || resolved.port_or_known_default() != index_uri.port_or_known_default()
    {
        return Err(PackageUriError::CrossOrigin);
    }

    let capability = match source {
        repository::Source::RootIndex(root) => Some(root_package_capability(root)),
        repository::Source::DirectIndex(_) if index_uri.scheme() == "file" => {
            Some(index_uri.join("./").map_err(PackageUriError::Capability)?)
        }
        repository::Source::DirectIndex(_) => None,
    };
    if let Some(capability) = capability {
        if !url_within_capability(index_uri, &capability) || !url_within_capability(&resolved, &capability) {
            return Err(PackageUriError::CapabilityEscape);
        }
    }
    if resolved.scheme() == "file" {
        if resolved.host_str().is_some() {
            return Err(PackageUriError::NonLocalFileAuthority);
        }
        let path = resolved.to_file_path().map_err(|_| PackageUriError::InvalidFilePath)?;
        let base = index_uri
            .join("./")
            .map_err(PackageUriError::Capability)?
            .to_file_path()
            .map_err(|_| PackageUriError::InvalidFilePath)?;
        let required_base = match source {
            repository::Source::RootIndex(root) => root_package_capability(root)
                .to_file_path()
                .map_err(|_| PackageUriError::InvalidFilePath)?,
            repository::Source::DirectIndex(_) => base,
        };
        if !path.starts_with(&required_base) {
            return Err(PackageUriError::CapabilityEscape);
        }
    }

    Ok(resolved)
}

/// Build the package capability with the same directory-style base semantics
/// as [`repository::RootIndexSource::uri`] and `history_index_uri`. `Url::join`
/// would treat a base without a trailing slash as a file and drop its final
/// path component.
fn root_package_capability(root: &repository::RootIndexSource) -> Url {
    let mut capability = root.base_uri.clone();
    let mut path = capability.path().to_owned();
    if !path.ends_with('/') {
        path.push('/');
    }
    path.push_str(root.channel.as_ref());
    path.push('/');
    capability.set_path(&path);
    capability
}

fn contains_encoded_path_control(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.windows(3).any(|window| {
        if window[0] != b'%' {
            return false;
        }
        let Some(high) = (window[1] as char).to_digit(16) else {
            return false;
        };
        let Some(low) = (window[2] as char).to_digit(16) else {
            return false;
        };
        matches!(((high << 4) | low) as u8, b'.' | b'/' | b'\\' | 0)
    })
}

fn url_within_capability(url: &Url, capability: &Url) -> bool {
    url.scheme() == capability.scheme()
        && url.host_str() == capability.host_str()
        && url.port_or_known_default() == capability.port_or_known_default()
        && url.path().starts_with(capability.path())
}

async fn resolve_index_from_root(
    state: &repository::Cached,
    source: &repository::RootIndexSource,
) -> Result<Url, Error> {
    let index_uri = match source.resolve_history_index_uri().await? {
        repository::ResolvedHistoryIndexUri::Supported(uri) => uri,
        repository::ResolvedHistoryIndexUri::Unsupported {
            format,
            version,
            root_index_uri,
            upgrade_via_index_uri,
        } => {
            return Err(Error::UnsupportedRepos(vec![UnsupportedRepoFormat {
                repository: state.clone(),
                root_index_uri,
                upgrade_via_index_uri,
                version,
                format,
            }]));
        }
    };

    Ok(index_uri)
}

#[derive(Debug, Error)]
pub enum PackageUriError {
    #[error("package URI is empty")]
    Empty,
    #[error("package URI must be a relative reference")]
    AbsoluteReference,
    #[error("package URI cannot be resolved against the repository index")]
    Resolve(#[source] url::ParseError),
    #[error("resolved package URI contains credentials")]
    Credentials,
    #[error("resolved package URI contains a fragment")]
    Fragment,
    #[error("resolved package URI contains a query")]
    Query,
    #[error("resolved package URI changes repository origin")]
    CrossOrigin,
    #[error("package URI contains percent-encoded path traversal or separator bytes")]
    EncodedTraversal,
    #[error("derive package URI capability root")]
    Capability(#[source] url::ParseError),
    #[error("package URI escapes its configured repository capability root")]
    CapabilityEscape,
    #[error("file package URI has a non-local authority")]
    NonLocalFileAuthority,
    #[error("file package URI is not an absolute local path")]
    InvalidFilePath,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Can't modify repos when using explicit configs or authored Gluon system intent")]
    ExplicitUnsupported,
    #[error("invalid root-index architecture {0:?}")]
    InvalidRootArchitecture(String),
    #[error("repository URI violates transport policy: {reason}")]
    RepositoryTransportPolicy { reason: &'static str },
    #[error(
        "repository metadata queries for read-only installation {0:?} are unsupported; use an owner-authorized writable cache"
    )]
    ReadOnlyRepositoryCacheUnsupported(PathBuf),
    #[error("Missing metadata field: {0:?}")]
    MissingMetaField(StonePayloadMetaTag),
    #[error("create directory")]
    CreateDir(#[source] io::Error),
    #[error("remove directory")]
    RemoveDir(#[source] io::Error),
    #[error("prepare owned repository cache directory {path:?}")]
    PrepareCacheDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open descriptor-rooted repository metadata database {path:?}")]
    OpenRepositoryDatabase {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("prepare descriptor-rooted repository metadata database {path:?}")]
    PrepareRepositoryDatabase {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("fetch index file")]
    FetchIndex(#[from] repository::FetchError),
    #[error("create private repository index candidate directory in {parent:?}")]
    CreateIndexCandidate {
        parent: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open authenticated repository cache directory {path:?}")]
    OpenCacheDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open private repository index candidate directory {path:?}")]
    OpenIndexCandidateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("create immutable repository index directory {path:?}")]
    CreateIndexDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open immutable repository index directory {path:?}")]
    OpenIndexDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open repository mutation lock {path:?}")]
    OpenRepositoryMutationLock {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("lock repository mutation boundary {path:?}")]
    LockRepositoryMutation {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("lock stable repository snapshot view {path:?}")]
    LockRepositorySnapshot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid repository index path {0:?}")]
    InvalidIndexPath(PathBuf),
    #[error("invalid immutable repository index name for SHA-256 {0:?}")]
    InvalidImmutableIndexName(String),
    #[error("unexpected entry in immutable repository index directory: {0:?}")]
    InvalidIndexDirectoryEntry(PathBuf),
    #[error("open repository index {path:?}")]
    OpenIndex {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("inspect repository index {path:?}")]
    InspectIndex {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository index is not a regular file: {0:?}")]
    IndexNotRegular(PathBuf),
    #[error("repository index directory {path:?} violates policy: {reason}")]
    IndexDirectoryPolicy { path: PathBuf, reason: &'static str },
    #[error("repository index {path:?} violates metadata policy: {reason}")]
    IndexMetadataPolicy { path: PathBuf, reason: &'static str },
    #[error("repository index changed while retained: {0:?}")]
    IndexChanged(PathBuf),
    #[error("repository index name no longer identifies the retained inode: {0:?}")]
    IndexPathChanged(PathBuf),
    #[error("read repository index {path:?}")]
    ReadIndex {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository index {path:?} exceeds the {limit}-byte limit")]
    IndexTooLarge { path: PathBuf, limit: u64 },
    #[error("repository index {path:?} size mismatch: expected {expected}, got {actual}")]
    IndexSizeMismatch { path: PathBuf, expected: u64, actual: u64 },
    #[error("repository index {path:?} SHA-256 mismatch: expected {expected}, got {actual}")]
    IndexHashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("reserve bounded repository index bytes")]
    ReserveIndexBytes(#[source] TryReserveError),
    #[error("prepare repository index candidate {path:?} for immutable publication")]
    PrepareIndexCandidate {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync repository index file {path:?}")]
    SyncIndexFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("publish repository index candidate {source_path:?} as {target:?}")]
    PublishIndex {
        source_path: PathBuf,
        target: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("prepare published immutable repository index {path:?}")]
    PrepareImmutableIndex {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync immutable repository index directory {path:?}")]
    SyncIndexDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read immutable repository index directory {path:?}")]
    ReadIndexDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("immutable repository index directory cannot exceed {limit} generations")]
    IndexGenerationLimit { limit: usize },
    #[error("immutable repository index directory cannot exceed {limit} bytes")]
    IndexGenerationByteLimit { limit: u64 },
    #[error("remove inactive immutable repository index generation {path:?}")]
    RemoveIndexGeneration {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository snapshot verification cache is poisoned for `{0}`")]
    VerificationCachePoisoned(repository::Id),
    #[error("read index file")]
    ReadStone(#[from] StoneReadError),
    #[error("repository index has file type {0:?}; expected Repository")]
    UnexpectedIndexFileType(StoneHeaderV1FileType),
    #[error("repository index payload {index} has kind {kind:?}; expected Meta")]
    UnexpectedIndexPayload { index: usize, kind: StonePayloadKind },
    #[error("reserve bounded repository package metadata")]
    ReserveIndexPackages(#[source] TryReserveError),
    #[error("reserve bounded repository package identities")]
    ReserveIndexPackageIds(#[source] TryReserveError),
    #[error("repository index metadata entry {index} is invalid")]
    InvalidRepositoryMeta {
        index: usize,
        #[source]
        source: package::RepositoryMetaError,
    },
    #[error("repository index metadata entry {index} violated a validated-field invariant")]
    RepositoryMetaInvariant { index: usize },
    #[error("repository index contains a duplicate package identity at entry {index}")]
    DuplicateIndexPackage { index: usize },
    #[error("repository index package URI at entry {index} is invalid")]
    InvalidRepositoryPackageUri {
        index: usize,
        #[source]
        source: PackageUriError,
    },
    #[error("meta db")]
    Database(#[from] meta::Error),
    #[error("save config")]
    SaveConfig(#[source] Box<config::SaveGluonError>),
    #[error("load config")]
    LoadConfig(#[source] Box<config::LoadGluonError>),
    #[error("unknown repo")]
    UnknownRepo(repository::Id),
    #[error("resolve history index uri from root index")]
    ResolveHistoryIndexUri(#[from] repository::ResolveHistoryIndexUriError),
    #[error("root index doesn't have version identifier {0}")]
    MissingRootIndexVersion(format::ScopedIdentifier),
    #[error("repository `{0}` has no active index snapshot; initialize or refresh it before resolution")]
    MissingActiveSnapshot(repository::Id),
    #[error("one or more repositories has an unsupported format")]
    UnsupportedRepos(Vec<UnsupportedRepoFormat>),
    #[error("one or more repositories with a legacy URI need to be upgraded to the new configuration format")]
    OutdatedRepos(Arc<Source>, Vec<OutdatedRepoIndexUri>),
}

impl From<package::MissingMetaFieldError> for Error {
    fn from(error: package::MissingMetaFieldError) -> Self {
        Self::MissingMetaField(error.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Removal {
    NotFound,
    ConfigDeleted(bool),
}

#[derive(Debug, Clone)]
pub struct UnsupportedRepoFormat {
    pub repository: repository::Cached,
    pub root_index_uri: Url,
    pub upgrade_via_index_uri: Option<Url>,
    pub version: format::ScopedIdentifier,
    pub format: Format,
}

struct LegacyIndexUri {
    base_uri: Url,
    stream: LegacyIndexUriStream,
}

enum LegacyIndexUriStream {
    Volatile,
    Unstable,
}

impl LegacyIndexUri {
    fn compatible_root_index_source(self) -> repository::RootIndexSource {
        repository::RootIndexSource {
            version: self.stream.version(),
            base_uri: self.base_uri,
            channel: repository::DEFAULT_CHANNEL.try_into().expect("valid identifier"),
            arch: repository::DEFAULT_ARCH.to_owned(),
        }
    }
}

impl LegacyIndexUriStream {
    fn version(&self) -> format::ScopedIdentifier {
        format::ScopedIdentifier::Stream(
            format::Identifier::new(match self {
                LegacyIndexUriStream::Volatile => "volatile",
                LegacyIndexUriStream::Unstable => "unstable",
            })
            .expect("valid ident"),
        )
    }
}

fn identify_legacy_index_uri(uri: &Url) -> Option<LegacyIndexUri> {
    const DOMAINS: &[&str] = &[
        "dev.serpentos.com",
        "packages.aerynos.com",
        "cdn.aerynos.dev",
        "packages.aerynos.dev",
        "build.aerynos.dev",
        "infratest.aerynos.dev",
    ];
    const VOLATILE_PATHS: &[&str] = &["/volatile/x86_64/stone.index", "/stream/volatile/x86_64/stone.index"];
    const UNSTABLE_PATHS: &[&str] = &["/unstable/x86_64/stone.index", "/stream/unstable/x86_64/stone.index"];

    let mut stream = None;

    if uri.domain().is_some_and(|domain| DOMAINS.contains(&domain)) {
        if VOLATILE_PATHS.contains(&uri.path()) {
            stream = Some(LegacyIndexUriStream::Volatile);
        }

        if UNSTABLE_PATHS.contains(&uri.path()) {
            stream = Some(LegacyIndexUriStream::Unstable);
        }
    }

    if let Some(stream) = stream {
        let mut base_uri = uri.clone();
        base_uri.set_path("");

        Some(LegacyIndexUri { base_uri, stream })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use stone::{
        StoneHeaderV1, StonePayloadCompression, StonePayloadHeader, StonePayloadLayoutRecord,
        StonePayloadMetaDependency, StonePayloadMetaPrimitive, StonePayloadMetaRecord, StoneWriter,
        read_bytes_with_limits,
    };

    use super::*;

    fn string(tag: StonePayloadMetaTag, value: impl Into<String>) -> StonePayloadMetaRecord {
        StonePayloadMetaRecord {
            tag,
            primitive: StonePayloadMetaPrimitive::String(value.into()),
        }
    }

    fn integer(tag: StonePayloadMetaTag, value: u64) -> StonePayloadMetaRecord {
        StonePayloadMetaRecord {
            tag,
            primitive: StonePayloadMetaPrimitive::Uint64(value),
        }
    }

    fn valid_meta(hash: char, uri: &str) -> Vec<StonePayloadMetaRecord> {
        vec![
            string(StonePayloadMetaTag::Name, format!("package-{hash}")),
            string(StonePayloadMetaTag::Architecture, "x86_64"),
            string(StonePayloadMetaTag::Version, "1.0"),
            string(StonePayloadMetaTag::Summary, "A package"),
            string(StonePayloadMetaTag::Description, "A package for repository tests"),
            string(StonePayloadMetaTag::Homepage, "https://example.test"),
            string(StonePayloadMetaTag::SourceID, format!("package-{hash}")),
            integer(StonePayloadMetaTag::Release, 1),
            integer(StonePayloadMetaTag::BuildRelease, 1),
            string(StonePayloadMetaTag::PackageURI, uri),
            string(StonePayloadMetaTag::PackageHash, hash.to_string().repeat(64)),
            integer(StonePayloadMetaTag::PackageSize, 4_096),
            string(StonePayloadMetaTag::License, "MPL-2.0"),
            StonePayloadMetaRecord {
                tag: StonePayloadMetaTag::Depends,
                primitive: StonePayloadMetaPrimitive::Dependency(
                    StonePayloadMetaDependency::PackageName,
                    "runtime".to_owned(),
                ),
            },
        ]
    }

    fn meta_index(file_type: StoneHeaderV1FileType, payloads: &[Vec<StonePayloadMetaRecord>]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut writer = StoneWriter::new(&mut bytes, file_type).unwrap();
        for payload in payloads {
            writer.add_payload(payload.as_slice()).unwrap();
        }
        writer.finalize().unwrap();
        bytes
    }

    fn layout_index() -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut writer = StoneWriter::new(&mut bytes, StoneHeaderV1FileType::Repository).unwrap();
        let layout = Vec::<StonePayloadLayoutRecord>::new();
        writer.add_payload(layout.as_slice()).unwrap();
        writer.finalize().unwrap();
        bytes
    }

    fn test_index_uri() -> Url {
        "https://cdn.example.test/main/history/1783706384/x86_64/stone.index"
            .parse()
            .unwrap()
    }

    fn cached(db: meta::Database) -> (tempfile::TempDir, repository::Cached) {
        let index_uri = test_index_uri();
        let cache = tempfile::tempdir().unwrap();
        fs::set_permissions(cache.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = repository::Cached::new(
            repository::Id::new("test"),
            Repository {
                description: "test".to_owned(),
                source: repository::Source::DirectIndex(index_uri),
                priority: repository::Priority::new(0),
                active: true,
            },
            db,
            None,
            cache.path().to_owned(),
        );
        (cache, state)
    }

    fn sentinel_meta() -> package::Meta {
        let mut meta = package::Meta::from_repository_index_payload(&valid_meta(
            'f',
            "../../../pool/v0/f/foo/foo-1.0-1-1-x86_64.stone",
        ))
        .unwrap();
        meta.uri = Some("https://cdn.example.test/main/pool/v0/f/foo/foo-1.0-1-1-x86_64.stone".to_owned());
        meta
    }

    fn write_index(bytes: &[u8]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), bytes).unwrap();
        file
    }

    fn fetched_test_index(
        state: &repository::Cached,
        source: &Path,
        index_uri: Url,
        mutation: &RepositoryMutationLock,
    ) -> FetchedIndex {
        let cache_clone = mutation.cache_directory.try_clone().unwrap();
        let directory = tempfile::Builder::new()
            .prefix(".index-candidate-test-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(proc_fd_path(&cache_clone))
            .unwrap();
        let name = directory.path().file_name().unwrap();
        let directory_display = state.cache_dir.join(name);
        let directory_file = openat2_file(
            mutation.cache_directory.as_raw_fd(),
            name.as_bytes(),
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            0,
            descendant_resolution(),
            &directory_display,
        )
        .unwrap();
        let path = directory_display.join(INDEX_CANDIDATE_NAME);
        fs::write(
            proc_fd_path(&directory_file).join(INDEX_CANDIDATE_NAME),
            fs::read(source).unwrap(),
        )
        .unwrap();
        let file = openat2_file(
            directory_file.as_raw_fd(),
            INDEX_CANDIDATE_NAME.as_bytes(),
            nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            0,
            descendant_resolution(),
            &path,
        )
        .unwrap();
        FetchedIndex {
            _directory: directory,
            _cache_directory: cache_clone,
            directory: directory_file,
            file,
            path,
            index_uri,
        }
    }

    fn activate_test_index(state: &repository::Cached, source: &Path, index_uri: &Url) -> Result<(), Error> {
        let mutation = RepositoryMutationLock::acquire(state)?;
        let candidate = fetched_test_index(state, source, index_uri.clone(), &mutation);
        activate_index_candidate(state, candidate, mutation)
    }

    fn test_installation() -> (tempfile::TempDir, Installation) {
        let root = crate::test_support::private_installation_tempdir();
        let installation = Installation::open(root.path(), None).unwrap();
        (root, installation)
    }

    fn direct_repository(index_uri: Url) -> Repository {
        Repository {
            description: "test".to_owned(),
            source: repository::Source::DirectIndex(index_uri),
            priority: repository::Priority::new(0),
            active: true,
        }
    }

    fn explicit_manager(
        identifier: &str,
        repository: Repository,
        installation: Installation,
    ) -> (repository::Id, Manager) {
        let id = repository::Id::new("test");
        let manager = Manager::with_explicit(
            identifier,
            repository::Map::with([(id.clone(), repository)]),
            installation,
        )
        .unwrap();
        (id, manager)
    }

    fn empty_payload_archive(payloads: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        StoneHeader::V1(StoneHeaderV1 {
            num_payloads: payloads,
            file_type: StoneHeaderV1FileType::Repository,
        })
        .encode(&mut bytes)
        .unwrap();
        let header = StonePayloadHeader {
            stored_size: 0,
            plain_size: 0,
            checksum: xxh3_64(&[]).to_be_bytes(),
            num_records: 0,
            version: 1,
            kind: StonePayloadKind::Unknown,
            compression: StonePayloadCompression::None,
        };
        for _ in 0..payloads {
            header.encode(&mut bytes).unwrap();
        }
        bytes
    }

    #[test]
    fn observed_pinned_aerynos_index_shape_fits_repository_limits() {
        // Audited by fully decoding the official immutable snapshot at
        // https://cdn.aerynos.dev/main/history/1783706384/x86_64/stone.index
        // with SHA-256 0f986f19f4e88f74ed5ae3452fbd9cc34ab53915f391555952d68ac90b202efc.
        let limits = repository_index_decode_limits();
        let download_limits = repository::REPOSITORY_INDEX_DOWNLOAD_LIMITS;
        assert_eq!(download_limits.max_bytes, 16 * 1024 * 1024);
        assert_eq!(download_limits.total_timeout, Duration::from_secs(120));
        assert_eq!(limits.max_payloads, 8_192);
        assert!(5_463 <= limits.max_payloads);
        assert!(334 <= limits.max_records_per_payload);
        assert!(18_812 <= limits.max_plain_payload_bytes);
        assert!(18_812 <= limits.max_record_bytes);
        assert!(2_715 <= limits.max_stored_payload_bytes);
        assert!(114_447 <= limits.max_total_records);
        assert!(4_113_121 <= limits.max_total_plain_bytes);
        assert!(4_113_121 <= limits.max_total_record_bytes);
        assert!(2_257_339 <= limits.max_total_stored_bytes);
        assert!(2_432_187 <= download_limits.max_bytes);
    }

    #[test]
    fn repository_payload_limit_accepts_n_and_rejects_n_plus_one() {
        let limits = repository_index_decode_limits();
        let accepted = empty_payload_archive(8_192);
        let mut reader = read_bytes_with_limits(&accepted, limits).unwrap();
        let decoded = reader
            .payloads()
            .unwrap()
            .try_fold(0, |count, payload| payload.map(|_| count + 1))
            .unwrap();
        assert_eq!(decoded, 8_192);

        let rejected = empty_payload_archive(8_193);
        assert!(matches!(
            read_bytes_with_limits(&rejected, limits),
            Err(StoneReadError::LimitExceeded {
                resource: "payload count",
                limit: 8_192,
                actual: 8_193,
            })
        ));
    }

    #[test]
    fn read_only_installation_rejects_repository_cache_without_weakening_ownership() {
        let (_root, mut installation) = test_installation();
        installation.mutability = crate::installation::Mutability::ReadOnly;
        let index_uri = test_index_uri();
        assert!(matches!(
            Manager::with_explicit(
                "read-only",
                repository::Map::with([(repository::Id::new("test"), direct_repository(index_uri))]),
                installation,
            ),
            Err(Error::ReadOnlyRepositoryCacheUnsupported(_))
        ));
    }

    #[test]
    fn repository_transport_errors_never_render_credentials() {
        let uri = Url::parse("https://user:secret@example.test/stone.index").unwrap();
        let error = validate_repository_transport(&uri).unwrap_err().to_string();
        assert!(!error.contains("user"));
        assert!(!error.contains("secret"));
        assert!(!error.contains(uri.as_str()));
    }

    #[test]
    fn repository_package_uri_accepts_official_parent_paths_and_rejects_origin_changes() {
        let index = test_index_uri();
        let source = repository::Source::DirectIndex(index.clone());
        let resolved = normalize_repository_package_uri(
            &source,
            &index,
            "../../../pool/v0/e/example/example-1.0-1-1-x86_64.stone",
        )
        .unwrap();
        assert_eq!(
            resolved.as_str(),
            "https://cdn.example.test/main/pool/v0/e/example/example-1.0-1-1-x86_64.stone"
        );

        assert!(matches!(
            normalize_repository_package_uri(&source, &index, "https://cdn.example.test/package.stone"),
            Err(PackageUriError::AbsoluteReference)
        ));
        assert!(matches!(
            normalize_repository_package_uri(&source, &index, "//other.example.test/package.stone"),
            Err(PackageUriError::CrossOrigin)
        ));
        assert!(matches!(
            normalize_repository_package_uri(&source, &index, "../../../pool/package.stone#fragment"),
            Err(PackageUriError::Fragment)
        ));
    }

    #[test]
    fn root_package_capability_preserves_trailing_and_non_trailing_base_paths() {
        let history = format::Identifier::new("1783706384").unwrap();
        for base in [
            "https://cdn.example.test/repositories",
            "https://cdn.example.test/repositories/",
        ] {
            let root = repository::RootIndexSource {
                base_uri: base.parse().unwrap(),
                channel: "main".try_into().unwrap(),
                version: "stream/unstable".try_into().unwrap(),
                arch: "x86_64".to_owned(),
            };
            let index = root.history_index_uri(&history);
            let source = repository::Source::RootIndex(root);
            let package = normalize_repository_package_uri(&source, &index, "../../../pool/package.stone").unwrap();
            assert_eq!(
                package.as_str(),
                "https://cdn.example.test/repositories/main/pool/package.stone"
            );
        }

        let temp = tempfile::tempdir().unwrap();
        let repository_path = temp.path().join("repositories");
        for trailing_slash in [false, true] {
            let mut base_uri = Url::from_file_path(&repository_path).unwrap();
            if trailing_slash {
                let mut path = base_uri.path().to_owned();
                path.push('/');
                base_uri.set_path(&path);
            }
            let root = repository::RootIndexSource {
                base_uri,
                channel: "main".try_into().unwrap(),
                version: "stream/unstable".try_into().unwrap(),
                arch: "x86_64".to_owned(),
            };
            let index = root.history_index_uri(&history);
            let source = repository::Source::RootIndex(root);
            let package = normalize_repository_package_uri(&source, &index, "../../../pool/package.stone").unwrap();
            assert_eq!(
                package.to_file_path().unwrap(),
                repository_path.join("main/pool/package.stone")
            );
        }
    }

    #[test]
    fn update_rejects_wrong_container_or_payload_without_changing_database() {
        let db = meta::Database::new(":memory:").unwrap();
        let sentinel = package::Id::from("sentinel");
        db.add(sentinel.clone(), sentinel_meta()).unwrap();
        let (_cache, state) = cached(db);

        let wrong_container = write_index(&meta_index(
            StoneHeaderV1FileType::Binary,
            &[valid_meta('a', "../../../pool/v0/a/a/a-1.0-1-1-x86_64.stone")],
        ));
        let error = activate_test_index(&state, wrong_container.path(), &test_index_uri()).unwrap_err();
        assert!(
            matches!(error, Error::UnexpectedIndexFileType(StoneHeaderV1FileType::Binary)),
            "{error:?}"
        );
        assert!(state.db.get(&sentinel).is_ok());

        let wrong_payload = write_index(&layout_index());
        assert!(matches!(
            activate_test_index(&state, wrong_payload.path(), &test_index_uri()),
            Err(Error::UnexpectedIndexPayload {
                index: 0,
                kind: StonePayloadKind::Layout,
            })
        ));
        assert!(state.db.get(&sentinel).is_ok());
    }

    #[test]
    fn invalid_late_entry_and_duplicate_identity_preserve_existing_database() {
        let db = meta::Database::new(":memory:").unwrap();
        let sentinel = package::Id::from("sentinel");
        db.add(sentinel.clone(), sentinel_meta()).unwrap();
        let (_cache, state) = cached(db);

        let first = valid_meta('a', "../../../pool/v0/a/a/a-1.0-1-1-x86_64.stone");
        let mut invalid = valid_meta('b', "../../../pool/v0/b/b/b-1.0-1-1-x86_64.stone");
        invalid.retain(|record| record.tag != StonePayloadMetaTag::PackageSize);
        let invalid_index = write_index(&meta_index(
            StoneHeaderV1FileType::Repository,
            &[first.clone(), invalid],
        ));
        assert!(matches!(
            activate_test_index(&state, invalid_index.path(), &test_index_uri()),
            Err(Error::InvalidRepositoryMeta { index: 1, .. })
        ));
        assert!(state.db.get(&sentinel).is_ok());
        assert!(state.db.get(&package::Id::from("a".repeat(64))).is_err());

        let duplicate_index = write_index(&meta_index(StoneHeaderV1FileType::Repository, &[first.clone(), first]));
        assert!(matches!(
            activate_test_index(&state, duplicate_index.path(), &test_index_uri()),
            Err(Error::DuplicateIndexPackage { index: 1 })
        ));
        assert!(state.db.get(&sentinel).is_ok());
    }

    #[test]
    fn valid_repository_index_replaces_database_and_normalizes_package_uri() {
        let db = meta::Database::new(":memory:").unwrap();
        let sentinel = package::Id::from("sentinel");
        db.add(sentinel.clone(), sentinel_meta()).unwrap();
        let (_cache, state) = cached(db);
        let hash = "a".repeat(64);
        let index = write_index(&meta_index(
            StoneHeaderV1FileType::Repository,
            &[valid_meta('a', "../../../pool/v0/a/a/a-1.0-1-1-x86_64.stone")],
        ));

        activate_test_index(&state, index.path(), &test_index_uri()).unwrap();

        assert!(state.db.get(&sentinel).is_err());
        let stored = state.db.get(&package::Id::from(hash)).unwrap();
        assert_eq!(
            stored.uri.as_deref(),
            Some("https://cdn.example.test/main/pool/v0/a/a/a-1.0-1-1-x86_64.stone")
        );
    }

    #[test]
    fn unrepresentable_metadata_never_consumes_generation_budget() {
        let (_cache, state) = cached(meta::Database::new(":memory:").unwrap());
        for attempt in 0..=MAX_INDEX_GENERATIONS {
            let mut invalid = valid_meta('a', "package-a.stone");
            for record in &mut invalid {
                if record.tag == StonePayloadMetaTag::Release {
                    record.primitive = StonePayloadMetaPrimitive::Uint64(i32::MAX as u64 + 1);
                }
                if record.tag == StonePayloadMetaTag::Summary {
                    record.primitive = StonePayloadMetaPrimitive::String(format!("attempt-{attempt}"));
                }
            }
            let index = write_index(&meta_index(StoneHeaderV1FileType::Repository, &[invalid]));
            let error = activate_test_index(&state, index.path(), &test_index_uri()).unwrap_err();
            assert!(
                matches!(
                    error,
                    Error::InvalidRepositoryMeta {
                        source: package::RepositoryMetaError::IntegerOutOfRange {
                            tag: StonePayloadMetaTag::Release,
                            ..
                        },
                        ..
                    } | Error::Database(meta::Error::MetaIntegerOutOfRange {
                        field: "source_release",
                        ..
                    })
                ),
                "{error:?}"
            );
        }
        assert!(state.db.active_snapshot().unwrap().is_none());
        let indexes = state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY);
        assert!(!indexes.exists() || fs::read_dir(indexes).unwrap().next().is_none());
    }

    #[test]
    fn direct_file_refresh_publishes_and_uses_one_verified_immutable_snapshot() {
        let source = tempfile::tempdir().unwrap();
        let source_path = source.path().join("stone.index");
        let bytes = meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('a', "package-a.stone")]);
        fs::write(&source_path, &bytes).unwrap();
        let index_uri = Url::from_file_path(&source_path).unwrap();
        let (_root, installation) = test_installation();
        let (id, manager) = explicit_manager("direct-file", direct_repository(index_uri.clone()), installation);

        runtime::block_on(manager.refresh(&id)).unwrap();

        let state = manager.repositories.get(&id).unwrap();
        let snapshot = verified_active_snapshot(state).unwrap();
        let expected = index_identity(&bytes);
        assert_eq!(snapshot.index_uri(), &index_uri);
        assert_eq!(snapshot.sha256(), expected.sha256);
        assert_eq!(snapshot.byte_size(), expected.byte_size);

        let immutable = immutable_index_path(state, snapshot.sha256());
        assert_eq!(fs::read(&immutable).unwrap(), bytes);
        assert_eq!(fs::metadata(&immutable).unwrap().permissions().mode() & 0o222, 0);
        assert!(!state.cache_dir.join("stone.index").exists());
        assert!(!state.cache_dir.join("index-uri").exists());

        let exported = manager.index_snapshots().unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].index_uri, index_uri);
        assert_eq!(exported[0].sha256, snapshot.sha256());
        assert_eq!(exported[0].byte_size, snapshot.byte_size());

        let package = manager
            .resolve_exact_package(&package::Id::from("a".repeat(64)))
            .unwrap()
            .unwrap()
            .1;
        assert_eq!(
            package.meta.uri,
            Some(
                Url::from_file_path(source.path().join("package-a.stone"))
                    .unwrap()
                    .to_string()
            )
        );
    }

    #[test]
    fn downloads_use_distinct_private_candidates_without_creating_active_state() {
        let source = tempfile::tempdir().unwrap();
        let source_path = source.path().join("candidate.index");
        fs::write(&source_path, b"candidate bytes").unwrap();
        let (_root, installation) = test_installation();
        let (id, manager) = explicit_manager(
            "private-candidates",
            direct_repository(Url::from_file_path(&source_path).unwrap()),
            installation,
        );
        let state = manager.repositories.get(&id).unwrap();

        let mutation = RepositoryMutationLock::acquire(state).unwrap();
        let first = runtime::block_on(fetch_index(&manager.source, state, &mutation.cache_directory)).unwrap();
        let second = runtime::block_on(fetch_index(&manager.source, state, &mutation.cache_directory)).unwrap();
        assert_ne!(first.path, second.path);
        for candidate in [&first, &second] {
            assert_eq!(
                fs::metadata(candidate._directory.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(fs::read(&candidate.path).unwrap(), b"candidate bytes");
        }
        assert_eq!(state.db.active_snapshot().unwrap(), None);
        assert!(!state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY).exists());
    }

    #[test]
    fn root_file_source_ignores_legacy_sidecars_and_initializes_from_db_snapshot() {
        let source = tempfile::tempdir().unwrap();
        let history_dir = source.path().join("main/history/1/x86_64");
        fs::create_dir_all(&history_dir).unwrap();
        let bytes = meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('b', "package-b.stone")]);
        fs::write(history_dir.join("stone.index"), &bytes).unwrap();
        fs::create_dir_all(source.path().join("main")).unwrap();
        fs::write(
            source.path().join("main").join(repository::ROOT_INDEX_WIRE_FILENAME),
            r#"{
  "formats": { "v0": {} },
  "streams": { "unstable": { "format": "v0", "history": "1" } },
  "tags": {},
  "history": { "1": { "format": "v0" } }
}"#,
        )
        .unwrap();

        let repository = Repository {
            description: "root test".to_owned(),
            source: repository::Source::RootIndex(repository::RootIndexSource {
                base_uri: Url::from_directory_path(source.path()).unwrap(),
                channel: "main".try_into().unwrap(),
                version: "stream/unstable".parse().unwrap(),
                arch: "x86_64".to_owned(),
            }),
            priority: repository::Priority::new(0),
            active: true,
        };
        let identifier = "root-file";
        let (_root, installation) = test_installation();
        let legacy_cache = cache_dir(identifier, &repository::Id::new("test"), &repository, &installation);
        fs::create_dir_all(&legacy_cache).unwrap();
        fs::set_permissions(&legacy_cache, std::fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(legacy_cache.join("stone.index"), b"legacy mutable cache").unwrap();
        fs::write(legacy_cache.join("index-uri"), b"not even a URL").unwrap();

        let (id, mut manager) = explicit_manager(identifier, repository, installation);
        assert!(matches!(
            manager.index_snapshots(),
            Err(Error::MissingActiveSnapshot(missing)) if missing == id
        ));

        assert_eq!(runtime::block_on(manager.ensure_all_initialized()).unwrap(), 1);
        assert_eq!(
            fs::read(legacy_cache.join("stone.index")).unwrap(),
            b"legacy mutable cache"
        );
        assert_eq!(fs::read(legacy_cache.join("index-uri")).unwrap(), b"not even a URL");

        let state = manager.repositories.get(&id).unwrap();
        let snapshot = verified_active_snapshot(state).unwrap();
        assert_eq!(
            snapshot.index_uri(),
            &Url::from_file_path(history_dir.join("stone.index")).unwrap()
        );
        assert!(state.db.get(&package::Id::from("b".repeat(64))).is_ok());
    }

    #[test]
    fn refresh_failure_missing_and_corrupt_active_files_fail_closed_without_losing_snapshot() {
        let source = tempfile::tempdir().unwrap();
        let source_path = source.path().join("stone.index");
        let valid = meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('c', "package-c.stone")]);
        fs::write(&source_path, &valid).unwrap();
        let (_root, installation) = test_installation();
        let (id, mut manager) = explicit_manager(
            "failure-preservation",
            direct_repository(Url::from_file_path(&source_path).unwrap()),
            installation,
        );
        runtime::block_on(manager.refresh(&id)).unwrap();

        let state = manager.repositories.get(&id).unwrap().clone();
        let old_snapshot = state.db.active_snapshot().unwrap().unwrap();
        let immutable = immutable_index_path(&state, old_snapshot.sha256());
        let old_bytes = fs::read(&immutable).unwrap();

        fs::write(&source_path, layout_index()).unwrap();
        assert!(matches!(
            runtime::block_on(manager.refresh(&id)),
            Err(Error::UnexpectedIndexPayload { .. })
        ));
        assert_eq!(state.db.active_snapshot().unwrap(), Some(old_snapshot.clone()));
        assert_eq!(fs::read(&immutable).unwrap(), old_bytes);
        assert!(state.db.get(&package::Id::from("c".repeat(64))).is_ok());

        fs::remove_file(&immutable).unwrap();
        assert!(manager.index_snapshots().is_err());
        assert!(
            manager
                .resolve_exact_package(&package::Id::from("c".repeat(64)))
                .is_err()
        );
        fs::write(&source_path, &valid).unwrap();
        assert_eq!(runtime::block_on(manager.ensure_all_initialized()).unwrap(), 1);
        assert_eq!(verified_active_snapshot(&state).unwrap(), old_snapshot);
        let registry_repository = crate::registry::plugin::Repository::new(state.clone());
        assert!(
            registry_repository
                .package(&package::Id::from("c".repeat(64)))
                .unwrap()
                .is_some()
        );

        fs::set_permissions(&immutable, std::fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&immutable, b"corrupt immutable index").unwrap();
        let corrupt = fs::read(&immutable).unwrap();
        let error = manager.index_snapshots().unwrap_err();
        assert!(
            matches!(
                error,
                Error::IndexSizeMismatch { .. } | Error::IndexMetadataPolicy { .. } | Error::IndexChanged(_)
            ),
            "{error:?}"
        );
        assert!(
            manager
                .resolve_exact_package(&package::Id::from("c".repeat(64)))
                .is_err()
        );
        assert!(registry_repository.package(&package::Id::from("c".repeat(64))).is_err());
        assert!(runtime::block_on(manager.ensure_all_initialized()).is_err());
        assert_eq!(fs::read(&immutable).unwrap(), corrupt);
        assert_eq!(state.db.active_snapshot().unwrap(), Some(old_snapshot));
    }

    #[test]
    fn concurrent_refreshes_converge_on_one_no_replace_content_address() {
        let source = tempfile::tempdir().unwrap();
        let source_path = source.path().join("stone.index");
        fs::write(
            &source_path,
            meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('d', "package-d.stone")]),
        )
        .unwrap();
        let (_root, installation) = test_installation();
        let (id, manager) = explicit_manager(
            "concurrent-refresh",
            direct_repository(Url::from_file_path(&source_path).unwrap()),
            installation,
        );

        runtime::block_on(async { futures_util::future::try_join(manager.refresh(&id), manager.refresh(&id)).await })
            .unwrap();

        let state = manager.repositories.get(&id).unwrap();
        verified_active_snapshot(state).unwrap();
        assert_eq!(
            fs::read_dir(state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY))
                .unwrap()
                .count(),
            1
        );
        assert!(
            fs::read_dir(&state.cache_dir)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(".index-candidate-"))
        );
    }

    #[test]
    fn stable_snapshot_view_blocks_refresh_across_multiple_queries() {
        let source = tempfile::tempdir().unwrap();
        let source_path = source.path().join("stone.index");
        let first = meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('a', "package-a.stone")]);
        fs::write(&source_path, &first).unwrap();
        let (_root, installation) = test_installation();
        let (id, manager) = explicit_manager(
            "stable-view",
            direct_repository(Url::from_file_path(&source_path).unwrap()),
            installation,
        );
        runtime::block_on(manager.refresh(&id)).unwrap();
        let manager = Arc::new(manager);
        let stable = manager.stable_snapshot_view().unwrap();
        assert_eq!(stable.snapshots()[0].sha256, index_identity(&first).sha256);

        let second = meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('b', "package-b.stone")]);
        fs::write(&source_path, &second).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let writer = manager.clone();
        let writer_id = id.clone();
        let thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx.send(runtime::block_on(writer.refresh(&writer_id))).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());

        assert!(
            manager
                .resolve_exact_package(&package::Id::from("a".repeat(64)))
                .unwrap()
                .is_some()
        );
        assert!(
            manager
                .resolve_exact_package(&package::Id::from("b".repeat(64)))
                .unwrap()
                .is_none()
        );
        assert_eq!(stable.snapshots()[0].sha256, index_identity(&first).sha256);

        drop(stable);
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        thread.join().unwrap();
        assert!(
            manager
                .resolve_exact_package(&package::Id::from("b".repeat(64)))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn stable_snapshot_view_blocks_repository_removal() {
        let source = tempfile::tempdir().unwrap();
        let source_path = source.path().join("stone.index");
        fs::write(
            &source_path,
            meta_index(StoneHeaderV1FileType::Repository, &[valid_meta('a', "package-a.stone")]),
        )
        .unwrap();
        let config_directory = tempfile::tempdir().unwrap();
        fs::set_permissions(config_directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = config::Manager::custom(config_directory.path());
        let (_root, installation) = test_installation();
        let id = repository::Id::new("removable");
        let repository = direct_repository(Url::from_file_path(&source_path).unwrap());
        let mut writer = Manager::with_config_manager(config.clone(), installation.clone()).unwrap();
        writer.add_repository(id.clone(), repository).unwrap();
        runtime::block_on(writer.refresh(&id)).unwrap();
        let reader = Manager::with_config_manager(config, installation).unwrap();
        let cache_path = reader.repositories.get(&id).unwrap().cache_dir.clone();
        let stable = reader.stable_snapshot_view().unwrap();

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let remove_id = id.clone();
        let thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx.send(writer.remove(remove_id)).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(cache_path.exists());
        assert!(
            reader
                .resolve_exact_package(&package::Id::from("a".repeat(64)))
                .unwrap()
                .is_some()
        );

        drop(stable);
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap(),
            Removal::ConfigDeleted(true)
        ));
        thread.join().unwrap();
        assert!(!cache_path.exists());
    }

    #[test]
    fn immutable_generation_budget_accepts_n_and_rejects_n_plus_one() {
        let (_cache, state) = cached(meta::Database::new(":memory:").unwrap());
        let cache_directory = open_cache_directory(&state).unwrap();
        let owner = directory_owner(&cache_directory, &state.cache_dir).unwrap();
        let indexes = open_indexes_directory(&state, &cache_directory, true).unwrap();
        let indexes_path = state.cache_dir.join(IMMUTABLE_INDEX_DIRECTORY);

        for generation in 0..(MAX_INDEX_GENERATIONS - 1) {
            let path = indexes_path.join(format!("{generation:064x}.stone"));
            fs::write(&path, [generation as u8]).unwrap();
            fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        }
        let candidate = IndexIdentity {
            sha256: "f".repeat(64),
            byte_size: 1,
        };
        let candidate_name = immutable_index_name(&candidate.sha256).unwrap();
        enforce_index_generation_budget(&state, &indexes, owner, &candidate_name, &candidate).unwrap();

        let final_existing_path = indexes_path.join(format!("{:064x}.stone", MAX_INDEX_GENERATIONS - 1));
        fs::write(&final_existing_path, [0_u8]).unwrap();
        fs::set_permissions(&final_existing_path, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(matches!(
            enforce_index_generation_budget(&state, &indexes, owner, &candidate_name, &candidate),
            Err(Error::IndexGenerationLimit {
                limit: MAX_INDEX_GENERATIONS
            })
        ));

        let existing = IndexIdentity {
            sha256: format!("{:064x}", MAX_INDEX_GENERATIONS - 1),
            byte_size: 1,
        };
        let existing_name = immutable_index_name(&existing.sha256).unwrap();
        enforce_index_generation_budget(&state, &indexes, owner, &existing_name, &existing).unwrap();
    }

    #[test]
    fn bounded_index_identity_accepts_n_and_rejects_n_plus_one() {
        let temporary = tempfile::tempdir().unwrap();
        let limit = repository::REPOSITORY_INDEX_DOWNLOAD_LIMITS.max_bytes;
        let exact = temporary.path().join("exact");
        fs::File::create(&exact).unwrap().set_len(limit).unwrap();
        let exact_file = fs::File::open(&exact).unwrap();
        assert_eq!(read_index_bytes(&exact_file, &exact).unwrap().0.len() as u64, limit);

        let too_large = temporary.path().join("too-large");
        fs::File::create(&too_large).unwrap().set_len(limit + 1).unwrap();
        assert!(matches!(
            read_index_bytes(&fs::File::open(&too_large).unwrap(), &too_large),
            Err(Error::IndexTooLarge { limit: actual, .. }) if actual == limit
        ));
    }
}
