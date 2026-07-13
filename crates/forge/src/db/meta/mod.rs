// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeMap, BTreeSet};

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
// The repository manager consumes these in the next integration checkpoint.
#[allow(dead_code)]
const ACTIVE_SNAPSHOT_SINGLETON: i32 = 1;
#[allow(dead_code)]
const MAX_SNAPSHOT_INDEX_URI_BYTES: usize = 8 * 1024;
#[allow(dead_code)]
const MAX_SNAPSHOT_BYTE_SIZE: u64 = 16 * 1024 * 1024;

mod schema;

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
}

/// The exact repository index whose package rows are active in this database.
///
/// Fields are private so every value crosses the same URI, digest, and size
/// validation boundary before it can participate in a replacement transaction.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    index_uri: Url,
    sha256: String,
    byte_size: u64,
}

#[allow(dead_code)]
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
        let mut conn = SqliteConnection::establish(url)?;

        conn.run_pending_migrations(MIGRATIONS).map_err(Error::Migration)?;

        Ok(Database {
            conn: Connection::new(conn),
        })
    }

    pub fn wipe(&self) -> Result<(), Error> {
        self.conn.exclusive_tx(|tx| {
            clear_active_snapshot_impl(tx)?;
            clear_packages_impl(tx)
        })
    }

    pub fn get(&self, package: &package::Id) -> Result<Meta, Error> {
        self.conn.exec(|conn| {
            let meta = model::meta::table
                .select(model::Meta::as_select())
                .find(package.to_string())
                .first::<model::Meta>(conn)?;
            let licenses = model::License::belonging_to(&meta)
                .select(model::meta_licenses::license)
                .load::<String>(conn)?;
            let dependencies = model::Dependency::belonging_to(&meta)
                .select(model::Dependency::as_select())
                .load_iter(conn)?
                .map(|d| Ok(d?.dependency))
                .collect::<Result<_, Error>>()?;
            let providers = model::Provider::belonging_to(&meta)
                .select(model::Provider::as_select())
                .load_iter(conn)?
                .map(|p| Ok(p?.provider))
                .collect::<Result<_, Error>>()?;
            let conflicts = model::Conflict::belonging_to(&meta)
                .select(model::Conflict::as_select())
                .load_iter(conn)?
                .map(|p| Ok(p?.conflict))
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
        })
    }

    pub fn provider_packages(&self, provider: &Provider) -> Result<Vec<package::Id>, Error> {
        self.conn.exec(|conn| {
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
        })
    }

    pub fn query(&self, filter: Option<Filter<'_>>) -> Result<Vec<(package::Id, Meta)>, Error> {
        self.conn.exec(|conn| {
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
                // Add licenses
                model::License::belonging_to(chunk)
                    .load_iter::<model::License, _>(conn)?
                    .try_for_each::<_, Result<_, Error>>(|result| {
                        let row = result?;
                        if let Some(meta) = entries.get_mut(row.package.as_str()) {
                            meta.licenses.push(row.license);
                        }
                        Ok(())
                    })?;

                // Add dependencies
                model::Dependency::belonging_to(chunk)
                    .load_iter::<model::Dependency, _>(conn)?
                    .try_for_each::<_, Result<_, Error>>(|result| {
                        let row = result?;
                        if let Some(meta) = entries.get_mut(row.package.as_str()) {
                            meta.dependencies.insert(row.dependency);
                        }
                        Ok(())
                    })?;

                // Add providers
                model::Provider::belonging_to(chunk)
                    .load_iter::<model::Provider, _>(conn)?
                    .try_for_each::<_, Result<_, Error>>(|result| {
                        let row = result?;
                        if let Some(meta) = entries.get_mut(row.package.as_str()) {
                            meta.providers.insert(row.provider);
                        }
                        Ok(())
                    })?;

                // Add conflicts
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
        })
    }

    pub fn package_ids(&self) -> Result<BTreeSet<package::Id>, Error> {
        self.conn.exec(|conn| {
            Ok(model::meta::table
                .select(model::meta::package)
                .distinct()
                .load_iter::<AStr, _>(conn)?
                .map(|result| result.map(package::Id::from))
                .collect::<Result<_, _>>()?)
        })
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
    #[allow(dead_code)]
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

    #[allow(dead_code)]
    pub(crate) fn active_snapshot(&self) -> Result<Option<Snapshot>, Error> {
        self.conn.exec(|conn| {
            model::active_repository_snapshot::table
                .select(model::ActiveRepositorySnapshot::as_select())
                .find(ACTIVE_SNAPSHOT_SINGLETON)
                .first::<model::ActiveRepositorySnapshot>(conn)
                .optional()?
                .map(decode_active_snapshot)
                .transpose()
        })
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

#[allow(dead_code)]
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
        "http" | "https" if snapshot.index_uri().host_str().is_some() => {}
        "file" if snapshot.index_uri().query().is_none() && snapshot.index_uri().to_file_path().is_ok() => {}
        "file" => {
            return Err(Error::SnapshotIndexUriPolicy {
                reason: "file URI must be an absolute local path without a query",
            });
        }
        _ => {
            return Err(Error::SnapshotIndexUriPolicy {
                reason: "only absolute HTTP(S) and local file URIs are supported",
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

fn validate_package_batch(packages: &[(package::Id, Meta)]) -> Result<(), Error> {
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
mod test {
    use stone::StoneDecodedPayload;

    use crate::dependency::Kind;

    use super::*;

    fn fixture_meta() -> Meta {
        let bytes = include_bytes!("../../../../../tests/fixtures/bash-completion-2.11-1-1-x86_64.stone");
        let mut stone = stone::read_bytes(bytes).unwrap();
        let payloads = stone.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let payload = payloads.iter().find_map(StoneDecodedPayload::meta).unwrap();
        Meta::from_stone_payload(&payload.body).unwrap()
    }

    fn fixture_snapshot(hash: char) -> Snapshot {
        Snapshot::new(
            format!("https://cdn.example.test/main/history/{hash}/x86_64/stone.index")
                .parse()
                .unwrap(),
            hash.to_string().repeat(64),
            2_432_187,
        )
        .unwrap()
    }

    fn snapshot_uri_with_length(length: usize) -> Url {
        const PREFIX: &str = "https://example.test/";
        assert!(length >= PREFIX.len());
        let uri = format!("{PREFIX}{}", "a".repeat(length - PREFIX.len()));
        assert_eq!(uri.len(), length);
        uri.parse().unwrap()
    }

    #[test]
    fn create_insert_select() {
        let db = Database::new(":memory:").unwrap();

        let bash_completion = include_bytes!("../../../../../tests/fixtures/bash-completion-2.11-1-1-x86_64.stone");

        let mut stone = stone::read_bytes(bash_completion).unwrap();

        let payloads = stone.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let meta_payload = payloads.iter().find_map(StoneDecodedPayload::meta).unwrap();
        let meta = Meta::from_stone_payload(&meta_payload.body).unwrap();

        let id = package::Id::from("test");

        db.add(id.clone(), meta.clone()).unwrap();

        assert_eq!(&meta.name, &"bash-completion".to_owned().into());

        // Now retrieve by provider.
        let lookup = Filter::Provider(Provider {
            kind: Kind::PackageName,
            name: "bash-completion".to_owned(),
        });
        let fetched = db.query(Some(lookup)).unwrap();
        assert_eq!(fetched.len(), 1);

        db.remove(&id).unwrap();

        let result = db.get(&id);

        assert!(result.is_err());

        // Test wipe
        db.add(id.clone(), meta).unwrap();
        db.wipe().unwrap();
        let result = db.get(&id);
        assert!(result.is_err());
    }

    #[test]
    fn test_conflict_is_recognized() {
        let db = Database::new(":memory:").unwrap();

        // See `tests/fixtures/conflicts/italian-pizza.glu` for the recipe file that produced this stone.
        // It should be obvious that this package conflicts with `name(pineapple)`.
        let italian_pizza = include_bytes!("../../../../../tests/fixtures/conflicts/italian-pizza-1-1-1-x86_64.stone");
        let pineapple_provider = Provider {
            kind: Kind::PackageName,
            name: "pineapple".to_owned(),
        };

        let mut stone = stone::read_bytes(italian_pizza).unwrap();

        let payloads = stone.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let meta_payload = payloads.iter().find_map(StoneDecodedPayload::meta).unwrap();
        let meta = Meta::from_stone_payload(&meta_payload.body).unwrap();
        db.add(package::Id::from(meta.id()), meta.clone()).unwrap();

        // Ensure we're parsing the correct package!
        assert_eq!(&meta.name, &"italian-pizza".to_owned().into());
        // Ensure that the conflict info already exists in the binary package.
        assert_eq!(
            meta.conflicts.iter().collect::<Vec<&Provider>>(),
            vec![&pineapple_provider]
        );

        // Now retrieve by provider.
        let lookup = Filter::Provider(Provider {
            kind: Kind::PackageName,
            name: "italian-pizza".to_owned(),
        });
        let fetched = db.query(Some(lookup)).unwrap();
        assert_eq!(fetched.len(), 1);

        let (_, retrieved_pkg) = fetched.first().unwrap();
        let retrieved_conflicts: Vec<&Provider> = retrieved_pkg.conflicts.iter().collect();
        // Ensure that the conflicts field is inserted into and can be queried from our database
        // correctly.
        assert_eq!(retrieved_conflicts, vec![&pineapple_provider]);
    }

    #[test]
    fn replace_all_commits_complete_metadata_and_relations() {
        let db = Database::new(":memory:").unwrap();
        let old = package::Id::from("old");
        db.add(old.clone(), fixture_meta()).unwrap();

        let first = package::Id::from("first");
        let second = package::Id::from("second");
        db.replace_all(vec![(first.clone(), fixture_meta()), (second.clone(), fixture_meta())])
            .unwrap();

        assert!(!db.package_ids().unwrap().contains(&old));
        assert!(!db.get(&first).unwrap().providers.is_empty());
        assert!(!db.get(&second).unwrap().licenses.is_empty());
        assert_eq!(db.package_ids().unwrap(), BTreeSet::from([first, second]));
    }

    #[test]
    fn active_snapshot_validates_uri_digest_and_exact_size_boundaries() {
        let exact = Snapshot::new(
            snapshot_uri_with_length(MAX_SNAPSHOT_INDEX_URI_BYTES),
            "a".repeat(64),
            MAX_SNAPSHOT_BYTE_SIZE,
        )
        .unwrap();
        assert_eq!(exact.index_uri().as_str().len(), MAX_SNAPSHOT_INDEX_URI_BYTES);
        assert_eq!(exact.byte_size(), MAX_SNAPSHOT_BYTE_SIZE);

        assert!(matches!(
            Snapshot::new(
                snapshot_uri_with_length(MAX_SNAPSHOT_INDEX_URI_BYTES + 1),
                "a".repeat(64),
                0,
            ),
            Err(Error::SnapshotIndexUriTooLong {
                limit: MAX_SNAPSHOT_INDEX_URI_BYTES,
                actual,
            }) if actual == MAX_SNAPSHOT_INDEX_URI_BYTES + 1
        ));
        assert!(matches!(
            Snapshot::new("ftp://example.test/stone.index".parse().unwrap(), "a".repeat(64), 0,),
            Err(Error::SnapshotIndexUriPolicy { .. })
        ));
        assert!(matches!(
            Snapshot::new("https://example.test/stone.index".parse().unwrap(), "A".repeat(64), 0,),
            Err(Error::InvalidSnapshotSha256)
        ));
        assert!(matches!(
            Snapshot::new("https://example.test/stone.index".parse().unwrap(), "a".repeat(63), 0,),
            Err(Error::InvalidSnapshotSha256)
        ));
        assert!(matches!(
            Snapshot::new(
                "https://example.test/stone.index".parse().unwrap(),
                "a".repeat(64),
                MAX_SNAPSHOT_BYTE_SIZE + 1,
            ),
            Err(Error::SnapshotByteSizeOutOfRange {
                limit: MAX_SNAPSHOT_BYTE_SIZE,
                actual,
            }) if actual == MAX_SNAPSHOT_BYTE_SIZE + 1
        ));
    }

    #[test]
    fn active_snapshot_migration_enforces_singleton_hash_uri_and_size_bounds() {
        use diesel::sql_types::{BigInt, Integer, Text};

        let db = Database::new(":memory:").unwrap();
        let insert = |singleton: i32, index_uri: &str, sha256: &str, byte_size: i64| {
            db.conn.exec(|conn| {
                diesel::sql_query(
                    "INSERT INTO active_repository_snapshot \
                     (singleton, index_uri, sha256, byte_size) VALUES (?, ?, ?, ?)",
                )
                .bind::<Integer, _>(singleton)
                .bind::<Text, _>(index_uri)
                .bind::<Text, _>(sha256)
                .bind::<BigInt, _>(byte_size)
                .execute(conn)
            })
        };

        assert!(insert(2, "https://example.test/stone.index", &"a".repeat(64), 0).is_err());
        assert!(insert(1, "https://example.test/stone.index", &"A".repeat(64), 0).is_err());
        assert!(
            insert(
                1,
                snapshot_uri_with_length(MAX_SNAPSHOT_INDEX_URI_BYTES + 1).as_str(),
                &"a".repeat(64),
                0,
            )
            .is_err()
        );
        assert!(
            insert(
                1,
                "https://example.test/stone.index",
                &"a".repeat(64),
                i64::try_from(MAX_SNAPSHOT_BYTE_SIZE + 1).unwrap(),
            )
            .is_err()
        );
        assert!(insert(1, "https://example.test/stone.index", &"a".repeat(64), -1).is_err());
        assert_eq!(db.active_snapshot().unwrap(), None);
    }

    #[test]
    fn replace_all_with_snapshot_round_trips_complete_active_state() {
        let db = Database::new(":memory:").unwrap();
        let first = package::Id::from("first");
        let second = package::Id::from("second");
        let snapshot = fixture_snapshot('a');

        db.replace_all_with_snapshot(
            vec![(first.clone(), fixture_meta()), (second.clone(), fixture_meta())],
            snapshot.clone(),
        )
        .unwrap();

        assert_eq!(db.package_ids().unwrap(), BTreeSet::from([first, second]));
        assert_eq!(db.active_snapshot().unwrap(), Some(snapshot));
    }

    #[test]
    fn legacy_package_mutation_invalidates_the_active_snapshot() {
        let db = Database::new(":memory:").unwrap();
        db.replace_all_with_snapshot(vec![(package::Id::from("old"), fixture_meta())], fixture_snapshot('a'))
            .unwrap();

        db.replace_all(vec![(package::Id::from("new"), fixture_meta())])
            .unwrap();

        assert_eq!(db.active_snapshot().unwrap(), None);
    }

    #[test]
    fn replace_all_validates_complete_batch_before_deleting_existing_metadata() {
        let db = Database::new(":memory:").unwrap();
        let sentinel = package::Id::from("sentinel");
        db.add(sentinel.clone(), fixture_meta()).unwrap();

        let duplicate = package::Id::from("duplicate");
        let error = db
            .replace_all(vec![(duplicate.clone(), fixture_meta()), (duplicate, fixture_meta())])
            .unwrap_err();
        assert!(matches!(error, Error::DuplicatePackageId));
        assert!(db.get(&sentinel).is_ok());

        let mut overflowing = fixture_meta();
        overflowing.source_release = u64::MAX;
        let error = db
            .replace_all(vec![(package::Id::from("overflowing"), overflowing)])
            .unwrap_err();
        assert!(matches!(
            error,
            Error::MetaIntegerOutOfRange {
                field: "source_release",
                ..
            }
        ));
        assert!(db.get(&sentinel).is_ok());
    }

    #[test]
    fn replace_all_rolls_back_delete_and_partial_insert_on_sqlite_failure() {
        let db = Database::new(":memory:").unwrap();
        let sentinel = package::Id::from("sentinel");
        let sentinel_meta = fixture_meta();
        db.add(sentinel.clone(), sentinel_meta.clone()).unwrap();

        db.conn.exec(|conn| {
            diesel::sql_query(
                "CREATE TRIGGER reject_broken_package \
                 BEFORE INSERT ON meta \
                 WHEN NEW.package = 'broken' \
                 BEGIN SELECT RAISE(ABORT, 'injected replacement failure'); END",
            )
            .execute(conn)
            .unwrap();
        });

        let replacement_meta = fixture_meta();
        let mut candidates = (0..PACKAGE_INSERT_CHUNK_SIZE)
            .map(|index| {
                (
                    package::Id::from(format!("candidate-{index:03}")),
                    replacement_meta.clone(),
                )
            })
            .collect::<Vec<_>>();
        candidates.push((package::Id::from("broken"), replacement_meta));

        let error = db.replace_all(candidates).unwrap_err();
        assert!(matches!(error, Error::Diesel(_)));

        assert_eq!(db.get(&sentinel).unwrap(), sentinel_meta);
        assert_eq!(db.package_ids().unwrap(), BTreeSet::from([sentinel]));
    }

    #[test]
    fn snapshot_replacement_failure_in_package_chunk_129_preserves_complete_old_state() {
        let db = Database::new(":memory:").unwrap();
        let sentinel = package::Id::from("sentinel");
        let sentinel_meta = fixture_meta();
        let old_snapshot = fixture_snapshot('a');
        db.replace_all_with_snapshot(vec![(sentinel.clone(), sentinel_meta.clone())], old_snapshot.clone())
            .unwrap();

        db.conn.exec(|conn| {
            diesel::sql_query(
                "CREATE TRIGGER reject_broken_snapshot_package \
                 BEFORE INSERT ON meta \
                 WHEN NEW.package = 'broken' \
                 BEGIN SELECT RAISE(ABORT, 'injected snapshot replacement failure'); END",
            )
            .execute(conn)
            .unwrap();
        });

        let replacement_meta = fixture_meta();
        let mut candidates = (0..PACKAGE_INSERT_CHUNK_SIZE)
            .map(|index| {
                (
                    package::Id::from(format!("candidate-{index:03}")),
                    replacement_meta.clone(),
                )
            })
            .collect::<Vec<_>>();
        candidates.push((package::Id::from("broken"), replacement_meta));

        let error = db
            .replace_all_with_snapshot(candidates, fixture_snapshot('b'))
            .unwrap_err();
        assert!(matches!(error, Error::Diesel(_)));

        assert_eq!(db.get(&sentinel).unwrap(), sentinel_meta);
        assert_eq!(db.package_ids().unwrap(), BTreeSet::from([sentinel]));
        assert_eq!(db.active_snapshot().unwrap(), Some(old_snapshot));
    }
}
