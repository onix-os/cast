// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeMap, HashSet, TryReserveError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fs_err::{self as fs, File};
use futures_util::{StreamExt, stream};
use gluon_config::Evaluator;
use sha2::{Digest, Sha256};
use stone::{
    StoneDecodeLimits, StoneDecodedPayload, StoneHeader, StoneHeaderV1FileType, StonePayloadKind, StonePayloadMetaTag,
    StoneReadError,
};
use thiserror::Error;
use url::Url;
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
                        let db = open_meta_db(source.identifier(), &repository, &installation)?;

                        let index_uri = match &repository.source {
                            repository::Source::DirectIndex(uri) => Some(uri.clone()),
                            repository::Source::RootIndex(_) => {
                                load_cached_index_uri(source.identifier(), &repository, &installation)?
                            }
                        };

                        Ok((
                            id.clone(),
                            repository::Cached::new(id, repository, db, config_path.clone(), index_uri),
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

        // Save repo as new config file
        // We save it as a map for easy merging across
        // multiple configuration files
        let map = repository::Map::with([(id.clone(), repository.clone())]);
        let config_path = config
            .save_gluon(&id, &map, &repository::RepositoryCodec)
            .map_err(|error| Error::SaveConfig(Box::new(error)))?;

        let db = open_meta_db(self.source.identifier(), &repository, &self.installation)?;

        let index_uri = match &repository.source {
            repository::Source::DirectIndex(uri) => Some(uri.clone()),
            repository::Source::RootIndex(_) => {
                load_cached_index_uri(self.source.identifier(), &repository, &self.installation)?
            }
        };

        self.repositories.insert(
            id.clone(),
            repository::Cached::new(id, repository, db, Some(config_path), index_uri),
        );

        Ok(())
    }

    /// Refresh a [`Repository`] by Id
    pub async fn refresh(&self, id: &repository::Id) -> Result<(), Error> {
        let Some(repo) = self.repositories.get(id).cloned() else {
            return Err(Error::UnknownRepo(id.clone()));
        };

        if repo.repository.active {
            let (file, index_uri) = fetch_index(&self.source, &repo, &self.installation).await?;
            runtime::unblock(move || update_meta_db(&repo, &file, &index_uri)).await?;
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
            .filter_map(|(id, state)| {
                let index_file =
                    cache_dir(self.source.identifier(), &state.repository, &self.installation).join("stone.index");

                if !index_file.exists() { Some(id) } else { None }
            })
            .collect::<Vec<_>>();

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
            let mut meta = match repository.db.get(package) {
                Ok(meta) => meta,
                Err(meta::Error::RowNotFound) => continue,
                Err(error) => return Err(error.into()),
            };
            let index_uri = repository
                .index_uri()
                .ok_or_else(|| Error::MissingIndexUri(repository.id.clone()))?;
            meta.uri = meta
                .uri
                .and_then(|stored| Url::parse(&stored).or_else(|_| index_uri.join(&stored)).ok())
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
            if repository.db.package_ids()?.contains(package) {
                return Ok(Some(repository.id));
            }
        }
        Ok(None)
    }

    /// Hash the exact cached `stone.index` bytes for every active repository.
    pub fn index_snapshots(&self) -> Result<Vec<repository::IndexSnapshot>, Error> {
        let mut snapshots = self
            .active()
            .map(|repository| {
                let path =
                    cache_dir(self.source.identifier(), &repository.repository, &self.installation).join("stone.index");
                let bytes = fs::read(&path).map_err(|source| Error::ReadIndexSnapshot {
                    path: path.clone(),
                    source,
                })?;
                let index_uri = repository
                    .index_uri()
                    .ok_or_else(|| Error::MissingIndexUri(repository.id.clone()))?;
                Ok(repository::IndexSnapshot {
                    id: repository.id,
                    index_uri,
                    sha256: format!("{:x}", Sha256::digest(bytes)),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        snapshots.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(snapshots)
    }

    /// Remove a repository, deleting any related config & cached data
    pub fn remove(&mut self, id: impl Into<repository::Id>) -> Result<Removal, Error> {
        // Only allow removal for system repo manager
        let Source::ConfigManager(config) = &*self.source else {
            return Err(Error::ExplicitUnsupported);
        };

        // Remove from memory
        let Some(repo) = self.repositories.remove(&id.into()) else {
            return Ok(Removal::NotFound);
        };

        let cache_dir = cache_dir(self.source.identifier(), &repo.repository, &self.installation);

        // Remove cache
        if cache_dir.exists() {
            fs::remove_dir_all(&cache_dir).map_err(Error::RemoveDir)?;
        }

        // Delete config, only succeeds for configs that live in their
        // own config file w/ matching repo name
        if config.delete_gluon::<repository::Map>(&repo.id).is_err() {
            return Ok(Removal::ConfigDeleted(false));
        }

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
fn cache_dir(identifier: &str, repo: &Repository, installation: &Installation) -> PathBuf {
    let hash = match &repo.source {
        repository::Source::DirectIndex(uri) => {
            format!("{:02x}", xxh3_64(format!("{identifier}-{uri}").as_bytes()))
        }
        repository::Source::RootIndex(repository::RootIndexSource {
            base_uri,
            channel,
            version,
            arch,
        }) => format!(
            "{:02x}",
            xxh3_64(format!("{identifier}-{base_uri}-{channel}-{version}-{arch}").as_bytes())
        ),
    };

    installation.repo_path(hash)
}

/// Open the meta db file, ensuring it's
/// directory exists
fn open_meta_db(identifier: &str, repo: &Repository, installation: &Installation) -> Result<meta::Database, Error> {
    let dir = cache_dir(identifier, repo, installation);

    fs::create_dir_all(&dir).map_err(Error::CreateDir)?;

    let db = meta::Database::new(dir.join("db").to_str().unwrap_or_default())?;

    Ok(db)
}

fn load_cached_index_uri(
    identifier: &str,
    repo: &Repository,
    installation: &Installation,
) -> Result<Option<Url>, Error> {
    let dir = cache_dir(identifier, repo, installation);

    let path = dir.join("index-uri");

    if !path.exists() {
        return Ok(None);
    }

    let content = fs_err::read_to_string(path).map_err(Error::ReadCachedIndexUri)?;

    content.parse().map_err(Error::ParseCachedIndexUri).map(Some)
}

/// Fetches a stone index file from the repository URL
/// and saves it to the repo installation path
async fn fetch_index(
    source: &Arc<Source>,
    state: &repository::Cached,
    installation: &Installation,
) -> Result<(PathBuf, Url), Error> {
    let out_dir = cache_dir(source.identifier(), &state.repository, installation);

    fs_err::tokio::create_dir_all(&out_dir)
        .await
        .map_err(Error::CreateDir)?;

    let index_uri = match &state.repository.source {
        repository::Source::DirectIndex(uri) => match identify_legacy_index_uri(uri) {
            None => uri.clone(),
            Some(legacy_index) => {
                // The compatible root index source for this legacy index uri
                let root_source = legacy_index.compatible_root_index_source();

                // If this root index exists & the related stream is now on v0+, we
                // need to return an error informing the user to upgrade their repo
                // configuration to the new format
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
        repository::Source::RootIndex(source) => resolve_index_from_root(state, &out_dir, source).await?,
    };

    let out_path = out_dir.join("stone.index");

    // Fetch index & write to `out_path`
    repository::fetch_index(index_uri.clone(), &out_path).await?;

    Ok((out_path, index_uri))
}

/// Updates a stones metadata into the meta db
fn update_meta_db(state: &repository::Cached, index_path: &Path, index_uri: &Url) -> Result<(), Error> {
    let mut file = File::open(index_path).map_err(Error::OpenIndex)?;
    let mut reader = stone::read_with_limits(&mut file, repository_index_decode_limits())?;
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
        let uri = normalize_repository_package_uri(index_uri, raw_uri)
            .map_err(|source| Error::InvalidRepositoryPackageUri { index, source })?;
        meta.uri = Some(uri.into());
        packages.push((id, meta));
    }

    state.db.replace_all(packages)?;

    Ok(())
}

fn normalize_repository_package_uri(index_uri: &Url, raw_uri: &str) -> Result<Url, PackageUriError> {
    if raw_uri.is_empty() {
        return Err(PackageUriError::Empty);
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
    if resolved.scheme() != index_uri.scheme()
        || resolved.host_str() != index_uri.host_str()
        || resolved.port_or_known_default() != index_uri.port_or_known_default()
    {
        return Err(PackageUriError::CrossOrigin);
    }

    Ok(resolved)
}

async fn resolve_index_from_root(
    state: &repository::Cached,
    out_dir: &Path,
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

    fs_err::tokio::write(out_dir.join("index-uri"), index_uri.as_str().as_bytes())
        .await
        .map_err(Error::WriteCachedIndexUri)?;

    state.set_index_uri(index_uri.clone());

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
    #[error("resolved package URI changes repository origin")]
    CrossOrigin,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Can't modify repos when using explicit configs or authored Gluon system intent")]
    ExplicitUnsupported,
    #[error("Missing metadata field: {0:?}")]
    MissingMetaField(StonePayloadMetaTag),
    #[error("create directory")]
    CreateDir(#[source] io::Error),
    #[error("remove directory")]
    RemoveDir(#[source] io::Error),
    #[error("fetch index file")]
    FetchIndex(#[from] repository::FetchError),
    #[error("open index file")]
    OpenIndex(#[source] io::Error),
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
    #[error("read cached index uri")]
    ReadCachedIndexUri(#[source] io::Error),
    #[error("write cached index uri")]
    WriteCachedIndexUri(#[source] io::Error),
    #[error("parse cached index uri")]
    ParseCachedIndexUri(#[source] url::ParseError),
    #[error("read repository index snapshot {path:?}")]
    ReadIndexSnapshot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository `{0}` has no resolved index URI; initialize or refresh it before resolution")]
    MissingIndexUri(repository::Id),
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

    fn cached(db: meta::Database) -> repository::Cached {
        let index_uri = test_index_uri();
        repository::Cached::new(
            repository::Id::new("test"),
            Repository {
                description: "test".to_owned(),
                source: repository::Source::DirectIndex(index_uri.clone()),
                priority: repository::Priority::new(0),
                active: true,
            },
            db,
            None,
            Some(index_uri),
        )
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
    fn repository_package_uri_accepts_official_parent_paths_and_rejects_origin_changes() {
        let index = test_index_uri();
        let resolved =
            normalize_repository_package_uri(&index, "../../../pool/v0/e/example/example-1.0-1-1-x86_64.stone")
                .unwrap();
        assert_eq!(
            resolved.as_str(),
            "https://cdn.example.test/main/pool/v0/e/example/example-1.0-1-1-x86_64.stone"
        );

        assert!(matches!(
            normalize_repository_package_uri(&index, "https://cdn.example.test/package.stone"),
            Err(PackageUriError::AbsoluteReference)
        ));
        assert!(matches!(
            normalize_repository_package_uri(&index, "//other.example.test/package.stone"),
            Err(PackageUriError::CrossOrigin)
        ));
        assert!(matches!(
            normalize_repository_package_uri(&index, "../../../pool/package.stone#fragment"),
            Err(PackageUriError::Fragment)
        ));
    }

    #[test]
    fn update_rejects_wrong_container_or_payload_without_changing_database() {
        let db = meta::Database::new(":memory:").unwrap();
        let sentinel = package::Id::from("sentinel");
        db.add(sentinel.clone(), sentinel_meta()).unwrap();
        let state = cached(db);

        let wrong_container = write_index(&meta_index(
            StoneHeaderV1FileType::Binary,
            &[valid_meta('a', "../../../pool/v0/a/a/a-1.0-1-1-x86_64.stone")],
        ));
        assert!(matches!(
            update_meta_db(&state, wrong_container.path(), &test_index_uri()),
            Err(Error::UnexpectedIndexFileType(StoneHeaderV1FileType::Binary))
        ));
        assert!(state.db.get(&sentinel).is_ok());

        let wrong_payload = write_index(&layout_index());
        assert!(matches!(
            update_meta_db(&state, wrong_payload.path(), &test_index_uri()),
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
        let state = cached(db);

        let first = valid_meta('a', "../../../pool/v0/a/a/a-1.0-1-1-x86_64.stone");
        let mut invalid = valid_meta('b', "../../../pool/v0/b/b/b-1.0-1-1-x86_64.stone");
        invalid.retain(|record| record.tag != StonePayloadMetaTag::PackageSize);
        let invalid_index = write_index(&meta_index(
            StoneHeaderV1FileType::Repository,
            &[first.clone(), invalid],
        ));
        assert!(matches!(
            update_meta_db(&state, invalid_index.path(), &test_index_uri()),
            Err(Error::InvalidRepositoryMeta { index: 1, .. })
        ));
        assert!(state.db.get(&sentinel).is_ok());
        assert!(state.db.get(&package::Id::from("a".repeat(64))).is_err());

        let duplicate_index = write_index(&meta_index(StoneHeaderV1FileType::Repository, &[first.clone(), first]));
        assert!(matches!(
            update_meta_db(&state, duplicate_index.path(), &test_index_uri()),
            Err(Error::DuplicateIndexPackage { index: 1 })
        ));
        assert!(state.db.get(&sentinel).is_ok());
    }

    #[test]
    fn valid_repository_index_replaces_database_and_normalizes_package_uri() {
        let db = meta::Database::new(":memory:").unwrap();
        let sentinel = package::Id::from("sentinel");
        db.add(sentinel.clone(), sentinel_meta()).unwrap();
        let state = cached(db);
        let hash = "a".repeat(64);
        let index = write_index(&meta_index(
            StoneHeaderV1FileType::Repository,
            &[valid_meta('a', "../../../pool/v0/a/a/a-1.0-1-1-x86_64.stone")],
        ));

        update_meta_db(&state, index.path(), &test_index_uri()).unwrap();

        assert!(state.db.get(&sentinel).is_err());
        let stored = state.db.get(&package::Id::from(hash)).unwrap();
        assert_eq!(
            stored.uri.as_deref(),
            Some("https://cdn.example.test/main/pool/v0/a/a/a-1.0-1-1-x86_64.stone")
        );
    }
}
