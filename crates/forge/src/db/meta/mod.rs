use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use astr::AStr;
use diesel::prelude::*;
use diesel::{Connection as _, SqliteConnection};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use url::Url;

use crate::db::Connection;
use crate::package::{self, Meta};
use crate::{Dependency, Provider};

pub use super::Error;
use super::MAX_VARIABLE_NUMBER;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("src/db/meta/migrations");
const PACKAGE_INSERT_CHUNK_SIZE: usize = 128;
const ACTIVE_SNAPSHOT_SINGLETON: i32 = 1;
const MAX_SNAPSHOT_INDEX_URI_BYTES: usize = 8 * 1024;
const MAX_SNAPSHOT_BYTE_SIZE: u64 = 16 * 1024 * 1024;

#[allow(dead_code)] // completed substrate; consumed by the next read-only-client slice
mod read_only;
mod schema;

#[allow(unused_imports)] // deliberate internal surface for the next read-only-client slice
pub(crate) use read_only::{ReadOnlyDatabase, ReadOnlyMetaError};

#[derive(Debug)]
pub enum Filter<'a> {
    Provider(Provider),
    Dependency(Dependency),
    Name(package::Name),
    Keyword(&'a str),
}

#[derive(Debug, Clone)]
pub struct Database {
    conn: Connection,
    // Keeps a descriptor used by `/proc/self/fd/<n>/db` SQLite paths alive for
    // the complete connection lifetime. Ordinary databases leave this empty.
    _repository_directory_anchor: Option<Arc<fs_err::File>>,
    _mutable_system_directory_anchor: Option<Arc<std::fs::File>>,
}

/// The exact repository index whose package rows are active in this database.
///
/// Fields are private so every value crosses the same URI, digest, and size
/// validation boundary before it can participate in a replacement transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    index_uri: Url,
    sha256: String,
    byte_size: u64,
}

impl Snapshot {
    pub(crate) fn new(index_uri: Url, sha256: String, byte_size: u64) -> Result<Self, Error> {
        let snapshot = Self {
            index_uri,
            sha256,
            byte_size,
        };
        validate_snapshot(&snapshot)?;
        Ok(snapshot)
    }

    pub(crate) fn index_uri(&self) -> &Url {
        &self.index_uri
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(crate) fn byte_size(&self) -> u64 {
        self.byte_size
    }
}

impl Database {
    pub fn new(url: &str) -> Result<Self, Error> {
        Self::new_with_anchors(url, None, None)
    }

    pub(crate) fn new_anchored(url: &str, directory_anchor: Arc<fs_err::File>) -> Result<Self, Error> {
        Self::new_with_anchors(url, Some(directory_anchor), None)
    }

    pub(crate) fn new_mutable_system_anchored(url: &str, directory_anchor: Arc<std::fs::File>) -> Result<Self, Error> {
        Self::new_with_anchors(url, None, Some(directory_anchor))
    }

    fn new_with_anchors(
        url: &str,
        repository_directory_anchor: Option<Arc<fs_err::File>>,
        mutable_system_directory_anchor: Option<Arc<std::fs::File>>,
    ) -> Result<Self, Error> {
        let mut conn = SqliteConnection::establish(url)?;

        conn.run_pending_migrations(MIGRATIONS).map_err(Error::Migration)?;

        Ok(Database {
            conn: Connection::new(conn),
            _repository_directory_anchor: repository_directory_anchor,
            _mutable_system_directory_anchor: mutable_system_directory_anchor,
        })
    }

    pub fn wipe(&self) -> Result<(), Error> {
        self.conn.exclusive_tx(|tx| {
            clear_active_snapshot_impl(tx)?;
            clear_packages_impl(tx)
        })
    }

    pub fn get(&self, package: &package::Id) -> Result<Meta, Error> {
        self.conn.exec(|conn| get_impl(package, conn))
    }

    /// Read one package and the snapshot owning its rows from the same SQLite
    /// read transaction. `None` means the exact package is absent, not that a
    /// different snapshot may be substituted by the caller.
    pub(crate) fn get_with_active_snapshot(
        &self,
        package: &package::Id,
    ) -> Result<(Option<Snapshot>, Option<Meta>), Error> {
        self.conn.exec(|conn| {
            conn.transaction(|tx| {
                let snapshot = active_snapshot_impl(tx)?;
                let package = match get_impl(package, tx) {
                    Ok(package) => Some(package),
                    Err(Error::RowNotFound) => None,
                    Err(error) => return Err(error),
                };
                Ok((snapshot, package))
            })
        })
    }

    pub fn provider_packages(&self, provider: &Provider) -> Result<Vec<package::Id>, Error> {
        self.conn.exec(|conn| provider_packages_impl(provider, conn))
    }

    pub(crate) fn provider_packages_with_active_snapshot(
        &self,
        provider: &Provider,
    ) -> Result<(Option<Snapshot>, Vec<package::Id>), Error> {
        self.conn
            .exec(|conn| conn.transaction(|tx| Ok((active_snapshot_impl(tx)?, provider_packages_impl(provider, tx)?))))
    }

    pub fn query(&self, filter: Option<Filter<'_>>) -> Result<Vec<(package::Id, Meta)>, Error> {
        self.conn.exec(|conn| query_impl(filter, conn))
    }

    pub(crate) fn query_with_active_snapshot(
        &self,
        filter: Option<Filter<'_>>,
    ) -> Result<(Option<Snapshot>, Vec<(package::Id, Meta)>), Error> {
        self.conn
            .exec(|conn| conn.transaction(|tx| Ok((active_snapshot_impl(tx)?, query_impl(filter, tx)?))))
    }

    pub fn package_ids(&self) -> Result<BTreeSet<package::Id>, Error> {
        self.conn.exec(package_ids_impl)
    }

    pub(crate) fn package_ids_with_active_snapshot(&self) -> Result<(Option<Snapshot>, BTreeSet<package::Id>), Error> {
        self.conn
            .exec(|conn| conn.transaction(|tx| Ok((active_snapshot_impl(tx)?, package_ids_impl(tx)?))))
    }

    pub fn file_hashes(&self) -> Result<BTreeSet<String>, Error> {
        self.conn.exec(|conn| {
            Ok(model::meta::table
                .select(model::meta::hash.assume_not_null())
                .filter(model::meta::hash.is_not_null())
                .distinct()
                .load_iter::<String, _>(conn)?
                .collect::<Result<_, _>>()?)
        })
    }

    pub fn add(&self, id: package::Id, meta: Meta) -> Result<(), Error> {
        self.batch_add(vec![(id, meta)])
    }

    pub fn batch_add(&self, packages: Vec<(package::Id, Meta)>) -> Result<(), Error> {
        validate_package_batch(&packages)?;
        self.conn.exclusive_tx(|tx| {
            let ids = packages.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>();
            clear_active_snapshot_impl(tx)?;
            batch_remove_impl(&ids, tx)?;
            insert_packages_impl(&packages, tx)
        })
    }

    /// Replace the complete repository metadata set as one atomic operation.
    ///
    /// The complete candidate is validated before the transaction deletes any
    /// existing rows. Any later SQLite error rolls the delete and every partial
    /// insert back together.
    pub fn replace_all(&self, packages: Vec<(package::Id, Meta)>) -> Result<(), Error> {
        validate_package_batch(&packages)?;
        self.conn.exclusive_tx(|tx| {
            clear_active_snapshot_impl(tx)?;
            clear_packages_impl(tx)?;
            insert_packages_impl(&packages, tx)
        })
    }

    /// Atomically replace the complete package set and its accepted index
    /// identity. No snapshot row is visible unless every package chunk and
    /// relation insert committed successfully in the same transaction.
    pub(crate) fn replace_all_with_snapshot(
        &self,
        packages: Vec<(package::Id, Meta)>,
        snapshot: Snapshot,
    ) -> Result<(), Error> {
        validate_package_batch(&packages)?;
        let snapshot_byte_size = validate_snapshot(&snapshot)?;

        self.conn.exclusive_tx(|tx| {
            clear_active_snapshot_impl(tx)?;
            clear_packages_impl(tx)?;
            insert_packages_impl(&packages, tx)?;
            insert_active_snapshot_impl(&snapshot, snapshot_byte_size, tx)
        })
    }

    pub(crate) fn active_snapshot(&self) -> Result<Option<Snapshot>, Error> {
        self.conn.exec(active_snapshot_impl)
    }

    pub fn remove(&self, package: &package::Id) -> Result<(), Error> {
        self.batch_remove(Some(package))
    }

    pub fn batch_remove<'a>(&self, packages: impl IntoIterator<Item = &'a package::Id>) -> Result<(), Error> {
        self.conn.exclusive_tx(|tx| {
            let packages = packages.into_iter().map(package::Id::as_str).collect::<Vec<_>>();
            clear_active_snapshot_impl(tx)?;
            batch_remove_impl(&packages, tx)?;
            Ok(())
        })
    }
}

fn get_impl(package: &package::Id, conn: &mut SqliteConnection) -> Result<Meta, Error> {
    let meta = model::meta::table
        .select(model::Meta::as_select())
        .find(package.to_string())
        .first::<model::Meta>(conn)
        .optional()?
        .ok_or(Error::RowNotFound)?;
    let licenses = model::License::belonging_to(&meta)
        .select(model::meta_licenses::license)
        .load::<String>(conn)?;
    let dependencies = model::Dependency::belonging_to(&meta)
        .select(model::Dependency::as_select())
        .load_iter(conn)?
        .map(|dependency| Ok(dependency?.dependency))
        .collect::<Result<_, Error>>()?;
    let providers = model::Provider::belonging_to(&meta)
        .select(model::Provider::as_select())
        .load_iter(conn)?
        .map(|provider| Ok(provider?.provider))
        .collect::<Result<_, Error>>()?;
    let conflicts = model::Conflict::belonging_to(&meta)
        .select(model::Conflict::as_select())
        .load_iter(conn)?
        .map(|conflict| Ok(conflict?.conflict))
        .collect::<Result<_, Error>>()?;

    Ok(Meta {
        name: meta.name,
        version_identifier: meta.version_identifier,
        source_release: meta.source_release as u64,
        build_release: meta.build_release as u64,
        architecture: meta.architecture,
        summary: meta.summary,
        description: meta.description,
        source_id: meta.source_id,
        homepage: meta.homepage,
        licenses,
        dependencies,
        providers,
        conflicts,
        uri: meta.uri,
        hash: meta.hash,
        download_size: meta.download_size.map(|size| size as u64),
    })
}

fn provider_packages_impl(provider: &Provider, conn: &mut SqliteConnection) -> Result<Vec<package::Id>, Error> {
    model::meta_providers::table
        .select(model::meta_providers::package)
        .distinct()
        .filter(model::meta_providers::provider.eq(provider.to_string()))
        .load_iter::<AStr, _>(conn)?
        .map(|result| {
            let id = result?;
            Ok(id.into())
        })
        .collect()
}

fn query_impl(filter: Option<Filter<'_>>, conn: &mut SqliteConnection) -> Result<Vec<(package::Id, Meta)>, Error> {
    let map_row = |result| {
        let meta: model::Meta = result?;

        Ok((
            package::Id::from(AStr::from(meta.package)),
            Meta {
                name: meta.name,
                version_identifier: meta.version_identifier,
                source_release: meta.source_release as u64,
                build_release: meta.build_release as u64,
                architecture: meta.architecture,
                summary: meta.summary,
                description: meta.description,
                source_id: meta.source_id,
                homepage: meta.homepage,
                licenses: Default::default(),
                dependencies: Default::default(),
                providers: Default::default(),
                conflicts: Default::default(),
                uri: meta.uri,
                hash: meta.hash,
                download_size: meta.download_size.map(|size| size as u64),
            },
        ))
    };

    let mut entries: BTreeMap<package::Id, Meta> = match &filter {
        Some(Filter::Provider(provider)) => model::meta::table
            .select(model::Meta::as_select())
            .inner_join(model::meta_providers::table)
            .filter(model::meta_providers::provider.eq(provider.to_string()))
            .load_iter::<model::Meta, _>(conn)?,
        Some(Filter::Dependency(dependency)) => model::meta::table
            .select(model::Meta::as_select())
            .inner_join(model::meta_dependencies::table)
            .filter(model::meta_dependencies::dependency.eq(dependency.to_string()))
            .load_iter::<model::Meta, _>(conn)?,
        Some(Filter::Name(name)) => model::meta::table
            .select(model::Meta::as_select())
            .filter(model::meta::name.eq(name.to_string()))
            .load_iter::<model::Meta, _>(conn)?,
        Some(Filter::Keyword(keyword)) => {
            let pattern = format!("%{keyword}%");
            model::meta::table
                .select(model::Meta::as_select())
                .filter(
                    model::meta::name
                        .like(pattern.clone())
                        .or(model::meta::summary.like(pattern)),
                )
                .load_iter::<model::Meta, _>(conn)?
        }
        None => model::meta::table
            .select(model::Meta::as_select())
            .load_iter::<model::Meta, _>(conn)?,
    }
    .map(map_row)
    .collect::<Result<_, Error>>()?;

    let package_ids = entries
        .keys()
        .map(|id| model::PackageId { id: id.to_string() })
        .collect::<Vec<_>>();

    for chunk in package_ids.chunks(MAX_VARIABLE_NUMBER) {
        model::License::belonging_to(chunk)
            .load_iter::<model::License, _>(conn)?
            .try_for_each::<_, Result<_, Error>>(|result| {
                let row = result?;
                if let Some(meta) = entries.get_mut(row.package.as_str()) {
                    meta.licenses.push(row.license);
                }
                Ok(())
            })?;

        model::Dependency::belonging_to(chunk)
            .load_iter::<model::Dependency, _>(conn)?
            .try_for_each::<_, Result<_, Error>>(|result| {
                let row = result?;
                if let Some(meta) = entries.get_mut(row.package.as_str()) {
                    meta.dependencies.insert(row.dependency);
                }
                Ok(())
            })?;

        model::Provider::belonging_to(chunk)
            .load_iter::<model::Provider, _>(conn)?
            .try_for_each::<_, Result<_, Error>>(|result| {
                let row = result?;
                if let Some(meta) = entries.get_mut(row.package.as_str()) {
                    meta.providers.insert(row.provider);
                }
                Ok(())
            })?;

        model::Conflict::belonging_to(chunk)
            .load_iter::<model::Conflict, _>(conn)?
            .try_for_each::<_, Result<_, Error>>(|result| {
                let row = result?;
                if let Some(meta) = entries.get_mut(row.package.as_str()) {
                    meta.conflicts.insert(row.conflict);
                }
                Ok(())
            })?;
    }

    Ok(entries.into_iter().collect())
}

fn package_ids_impl(conn: &mut SqliteConnection) -> Result<BTreeSet<package::Id>, Error> {
    Ok(model::meta::table
        .select(model::meta::package)
        .distinct()
        .load_iter::<AStr, _>(conn)?
        .map(|result| result.map(package::Id::from))
        .collect::<Result<_, _>>()?)
}

fn active_snapshot_impl(conn: &mut SqliteConnection) -> Result<Option<Snapshot>, Error> {
    model::active_repository_snapshot::table
        .select(model::ActiveRepositorySnapshot::as_select())
        .find(ACTIVE_SNAPSHOT_SINGLETON)
        .first::<model::ActiveRepositorySnapshot>(conn)
        .optional()?
        .map(decode_active_snapshot)
        .transpose()
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<i64, Error> {
    let uri_bytes = snapshot.index_uri().as_str().len();
    if uri_bytes > MAX_SNAPSHOT_INDEX_URI_BYTES {
        return Err(Error::SnapshotIndexUriTooLong {
            limit: MAX_SNAPSHOT_INDEX_URI_BYTES,
            actual: uri_bytes,
        });
    }
    if !snapshot.index_uri().username().is_empty() || snapshot.index_uri().password().is_some() {
        return Err(Error::SnapshotIndexUriPolicy {
            reason: "embedded credentials are not allowed",
        });
    }
    if snapshot.index_uri().fragment().is_some() {
        return Err(Error::SnapshotIndexUriPolicy {
            reason: "fragments are not allowed",
        });
    }
    match snapshot.index_uri().scheme() {
        "https" if snapshot.index_uri().host_str().is_some() => {}
        "file" if snapshot.index_uri().query().is_none() && snapshot.index_uri().to_file_path().is_ok() => {}
        "file" => {
            return Err(Error::SnapshotIndexUriPolicy {
                reason: "file URI must be an absolute local path without a query",
            });
        }
        _ => {
            return Err(Error::SnapshotIndexUriPolicy {
                reason: "only absolute HTTPS and local file URIs are supported",
            });
        }
    }

    if snapshot.sha256().len() != 64
        || !snapshot
            .sha256()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::InvalidSnapshotSha256);
    }

    checked_snapshot_byte_size(snapshot)
}

fn checked_snapshot_byte_size(snapshot: &Snapshot) -> Result<i64, Error> {
    if snapshot.byte_size() > MAX_SNAPSHOT_BYTE_SIZE {
        return Err(Error::SnapshotByteSizeOutOfRange {
            limit: MAX_SNAPSHOT_BYTE_SIZE,
            actual: snapshot.byte_size(),
        });
    }
    i64::try_from(snapshot.byte_size()).map_err(|_| Error::SnapshotByteSizeOutOfRange {
        limit: MAX_SNAPSHOT_BYTE_SIZE,
        actual: snapshot.byte_size(),
    })
}

fn decode_active_snapshot(stored: model::ActiveRepositorySnapshot) -> Result<Snapshot, Error> {
    if stored.singleton != ACTIVE_SNAPSHOT_SINGLETON {
        return Err(Error::InvalidSnapshotSingleton(stored.singleton));
    }
    let index_uri = stored.index_uri.parse().map_err(Error::ParseSnapshotIndexUri)?;
    let byte_size = u64::try_from(stored.byte_size).map_err(|_| Error::NegativeSnapshotByteSize(stored.byte_size))?;
    Snapshot::new(index_uri, stored.sha256, byte_size)
}

fn clear_active_snapshot_impl(tx: &mut SqliteConnection) -> Result<(), Error> {
    diesel::delete(model::active_repository_snapshot::table).execute(tx)?;
    Ok(())
}

fn insert_active_snapshot_impl(snapshot: &Snapshot, byte_size: i64, tx: &mut SqliteConnection) -> Result<(), Error> {
    diesel::insert_into(model::active_repository_snapshot::table)
        .values(model::NewActiveRepositorySnapshot {
            singleton: ACTIVE_SNAPSHOT_SINGLETON,
            index_uri: snapshot.index_uri().as_str(),
            sha256: snapshot.sha256(),
            byte_size,
        })
        .execute(tx)?;
    Ok(())
}

fn clear_packages_impl(tx: &mut SqliteConnection) -> Result<(), Error> {
    // Be explicit rather than depending on a connection-local foreign-key
    // pragma for cascading deletes.
    diesel::delete(model::meta_conflicts::table).execute(tx)?;
    diesel::delete(model::meta_providers::table).execute(tx)?;
    diesel::delete(model::meta_dependencies::table).execute(tx)?;
    diesel::delete(model::meta_licenses::table).execute(tx)?;
    diesel::delete(model::meta::table).execute(tx)?;
    Ok(())
}

pub(crate) fn validate_package_batch(packages: &[(package::Id, Meta)]) -> Result<(), Error> {
    let mut ids = Vec::new();
    ids.try_reserve_exact(packages.len())
        .map_err(Error::ReservePackageIds)?;
    ids.extend(packages.iter().map(|(id, _)| id.as_str()));
    ids.sort_unstable();
    if ids.windows(2).any(|ids| ids[0] == ids[1]) {
        return Err(Error::DuplicatePackageId);
    }

    for (_, meta) in packages {
        checked_meta_numbers(meta)?;
    }
    Ok(())
}

fn checked_meta_numbers(meta: &Meta) -> Result<(i32, i32, Option<i64>), Error> {
    let source_release = i32::try_from(meta.source_release).map_err(|_| Error::MetaIntegerOutOfRange {
        field: "source_release",
        value: meta.source_release,
    })?;
    let build_release = i32::try_from(meta.build_release).map_err(|_| Error::MetaIntegerOutOfRange {
        field: "build_release",
        value: meta.build_release,
    })?;
    let download_size = meta
        .download_size
        .map(|value| {
            i64::try_from(value).map_err(|_| Error::MetaIntegerOutOfRange {
                field: "download_size",
                value,
            })
        })
        .transpose()?;
    Ok((source_release, build_release, download_size))
}

fn insert_packages_impl(packages: &[(package::Id, Meta)], tx: &mut SqliteConnection) -> Result<(), Error> {
    for packages in packages.chunks(PACKAGE_INSERT_CHUNK_SIZE) {
        let entries = packages
            .iter()
            .map(|(package, meta)| {
                let (source_release, build_release, download_size) = checked_meta_numbers(meta)?;
                Ok(model::NewMeta {
                    package: package.as_str(),
                    name: meta.name.as_str(),
                    version_identifier: &meta.version_identifier,
                    source_release,
                    build_release,
                    architecture: &meta.architecture,
                    summary: &meta.summary,
                    description: &meta.description,
                    source_id: &meta.source_id,
                    homepage: &meta.homepage,
                    uri: meta.uri.as_deref(),
                    hash: meta.hash.as_deref(),
                    download_size,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let licenses = packages
            .iter()
            .flat_map(|(package, meta)| {
                meta.licenses.iter().map(|license| {
                    (
                        model::meta_licenses::package.eq(package.as_str()),
                        model::meta_licenses::license.eq(license),
                    )
                })
            })
            .collect::<Vec<_>>();
        let dependencies = packages
            .iter()
            .flat_map(|(package, meta)| {
                meta.dependencies.iter().map(|dependency| {
                    (
                        model::meta_dependencies::package.eq(package.as_str()),
                        model::meta_dependencies::dependency.eq(dependency.to_string()),
                    )
                })
            })
            .collect::<Vec<_>>();
        let providers = packages
            .iter()
            .flat_map(|(package, meta)| {
                meta.providers.iter().map(|provider| {
                    (
                        model::meta_providers::package.eq(package.as_str()),
                        model::meta_providers::provider.eq(provider.to_string()),
                    )
                })
            })
            .collect::<Vec<_>>();
        let conflicts = packages
            .iter()
            .flat_map(|(package, meta)| {
                meta.conflicts.iter().map(|conflict| {
                    (
                        model::meta_conflicts::package.eq(package.as_str()),
                        model::meta_conflicts::conflict.eq(conflict.to_string()),
                    )
                })
            })
            .collect::<Vec<_>>();

        for chunk in entries.chunks(MAX_VARIABLE_NUMBER / 13) {
            diesel::insert_into(model::meta::table).values(chunk).execute(tx)?;
        }
        for chunk in licenses.chunks(MAX_VARIABLE_NUMBER / 2) {
            diesel::insert_or_ignore_into(model::meta_licenses::table)
                .values(chunk)
                .execute(tx)?;
        }
        for chunk in dependencies.chunks(MAX_VARIABLE_NUMBER / 2) {
            diesel::insert_or_ignore_into(model::meta_dependencies::table)
                .values(chunk)
                .execute(tx)?;
        }
        for chunk in providers.chunks(MAX_VARIABLE_NUMBER / 2) {
            diesel::insert_or_ignore_into(model::meta_providers::table)
                .values(chunk)
                .execute(tx)?;
        }
        for chunk in conflicts.chunks(MAX_VARIABLE_NUMBER / 2) {
            diesel::insert_or_ignore_into(model::meta_conflicts::table)
                .values(chunk)
                .execute(tx)?;
        }
    }

    Ok(())
}

fn batch_remove_impl(packages: &[&str], tx: &mut SqliteConnection) -> Result<(), Error> {
    for chunk in packages.chunks(MAX_VARIABLE_NUMBER) {
        diesel::delete(model::meta::table.filter(model::meta::package.eq_any(chunk))).execute(tx)?;
    }
    Ok(())
}

mod model {
    use diesel::{
        Selectable,
        associations::{Associations, Identifiable},
        deserialize::Queryable,
        prelude::Insertable,
    };

    pub use crate::db::meta::schema::{
        active_repository_snapshot, meta, meta_conflicts, meta_dependencies, meta_licenses, meta_providers,
    };
    use crate::package;

    #[derive(Queryable, Selectable, Identifiable)]
    #[diesel(table_name = active_repository_snapshot)]
    #[diesel(primary_key(singleton))]
    pub struct ActiveRepositorySnapshot {
        pub singleton: i32,
        pub index_uri: String,
        pub sha256: String,
        pub byte_size: i64,
    }

    #[derive(Insertable)]
    #[diesel(table_name = active_repository_snapshot)]
    pub struct NewActiveRepositorySnapshot<'a> {
        pub singleton: i32,
        pub index_uri: &'a str,
        pub sha256: &'a str,
        pub byte_size: i64,
    }

    #[derive(Queryable, Selectable, Identifiable)]
    #[diesel(table_name = meta)]
    #[diesel(primary_key(package))]
    pub struct Meta {
        pub package: String,
        #[diesel(deserialize_as = String)]
        pub name: package::Name,
        pub version_identifier: String,
        pub source_release: i32,
        pub build_release: i32,
        pub architecture: String,
        pub summary: String,
        pub description: String,
        pub source_id: String,
        pub homepage: String,
        pub uri: Option<String>,
        pub hash: Option<String>,
        pub download_size: Option<i64>,
    }

    #[derive(Queryable, Selectable, Identifiable)]
    #[diesel(table_name = meta)]
    #[diesel(primary_key(package))]
    pub struct PackageId {
        #[diesel(column_name = "package")]
        pub id: String,
    }

    #[derive(Queryable, Selectable, Identifiable, Associations)]
    #[diesel(table_name = meta_licenses)]
    #[diesel(primary_key(package, license))]
    #[diesel(belongs_to(Meta, foreign_key = package))]
    #[diesel(belongs_to(PackageId, foreign_key = package))]
    pub struct License {
        pub package: String,
        pub license: String,
    }

    #[derive(Queryable, Selectable, Identifiable, Associations)]
    #[diesel(table_name = meta_dependencies)]
    #[diesel(primary_key(package, dependency))]
    #[diesel(belongs_to(Meta, foreign_key = package))]
    #[diesel(belongs_to(PackageId, foreign_key = package))]
    pub struct Dependency {
        pub package: String,
        #[diesel(deserialize_as = String)]
        pub dependency: crate::Dependency,
    }

    #[derive(Queryable, Selectable, Identifiable, Associations)]
    #[diesel(table_name = meta_providers)]
    #[diesel(primary_key(package, provider))]
    #[diesel(belongs_to(Meta, foreign_key = package))]
    #[diesel(belongs_to(PackageId, foreign_key = package))]
    pub struct Provider {
        pub package: String,
        #[diesel(deserialize_as = String)]
        pub provider: crate::Provider,
    }

    #[derive(Queryable, Selectable, Identifiable, Associations)]
    #[diesel(table_name = meta_conflicts)]
    #[diesel(primary_key(package, conflict))]
    #[diesel(belongs_to(Meta, foreign_key = package))]
    #[diesel(belongs_to(PackageId, foreign_key = package))]
    pub struct Conflict {
        pub package: String,
        #[diesel(deserialize_as = String)]
        pub conflict: crate::Provider,
    }

    #[derive(Insertable)]
    #[diesel(table_name = meta)]
    pub struct NewMeta<'a> {
        pub package: &'a str,
        pub name: &'a str,
        pub version_identifier: &'a str,
        pub source_release: i32,
        pub build_release: i32,
        pub architecture: &'a str,
        pub summary: &'a str,
        pub description: &'a str,
        pub source_id: &'a str,
        pub homepage: &'a str,
        pub uri: Option<&'a str>,
        pub hash: Option<&'a str>,
        pub download_size: Option<i64>,
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod test;
