// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use astr::AStr;
use diesel::prelude::*;
use diesel::{Connection as _, ConnectionError, SqliteConnection};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use libsqlite3_sys as sqlite;
use std::{
    any::Any,
    borrow::Cow,
    collections::BTreeSet,
    ffi::{CStr, CString, c_void},
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    ptr::{self, NonNull},
    slice,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use crate::package;

pub use super::Error;
use super::{Connection, MAX_VARIABLE_NUMBER};

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("src/db/layout/migrations");
const PACKAGE_ID_INDEX: &str = "layout_package_id_idx";
const QUERY_PROGRESS_VM_OPS: i32 = 100;
const MAX_BOUNDED_QUERY_VM_OPS: u64 = 250_000_000;
const RAW_CONNECTION_BUSY_TIMEOUT_MS: i32 = 1_000;
const BOUNDED_QUERY_PACKAGE_CHUNK: usize = MAX_VARIABLE_NUMBER - 1;
const MAX_BOUNDED_QUERY_PACKAGES: usize = 4_096;
const MAX_BOUNDED_QUERY_PACKAGE_ID_BYTES: usize = 1024 * 1024;

static NEXT_MEMORY_DATABASE: AtomicU64 = AtomicU64::new(0);

#[allow(dead_code)] // completed substrate; consumed by the next read-only-client slice
mod read_only;
mod schema;

#[allow(unused_imports)] // deliberate internal surface for the next read-only-client slice
pub(crate) use read_only::{ReadOnlyDatabase, ReadOnlyLayoutError};

#[derive(Debug, Clone)]
pub struct Database {
    conn: Connection,
    bounded_conn: Arc<Mutex<RawLayoutConnection>>,
}

/// Hard bounds applied before a layout query allocates its result vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryBounds {
    pub max_rows: usize,
    pub max_string_bytes: usize,
}

/// The bounded outcome is separate from database failures so callers can map
/// resource-policy failures into their own public error type.
#[derive(Debug)]
#[must_use]
pub enum BoundedQueryOutcome {
    Complete(Vec<(package::Id, StonePayloadLayoutRecord)>),
    PackageLimit { limit: usize, actual: usize },
    PackageIdByteLimit { limit: usize, actual: usize },
    RowLimit { limit: usize, actual: usize },
    StringByteLimit { limit: usize, actual: usize },
    Cancelled,
}

impl Database {
    pub fn new(url: &str) -> Result<Self, Error> {
        // A second, narrowly scoped SQLite handle is required because Diesel
        // does not expose the native handle needed by sqlite3_progress_handler.
        // Give `:memory:` a private shared-cache URI so both handles address
        // the same ephemeral database without sharing it with another
        // Database instance.
        let effective_url = effective_database_url(url);
        let mut conn = SqliteConnection::establish(&effective_url)?;

        conn.run_pending_migrations(MIGRATIONS).map_err(Error::Migration)?;
        let bounded_conn = RawLayoutConnection::open(&effective_url)?;

        Ok(Database {
            conn: Connection::new(conn),
            bounded_conn: Arc::new(Mutex::new(bounded_conn)),
        })
    }

    /// Retrieve all entries for a given package by ID
    pub fn query<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<Vec<(package::Id, StonePayloadLayoutRecord)>, Error> {
        self.conn.exec(|conn| {
            let packages = packages.into_iter().map(package::Id::as_str).collect::<Vec<_>>();

            let mut output = vec![];

            for chunk in packages.chunks(MAX_VARIABLE_NUMBER) {
                output.extend(
                    model::layout::table
                        .select(model::Layout::as_select())
                        .filter(model::layout::package_id.eq_any(chunk))
                        .load_iter(conn)?
                        .map(map_layout)
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }

            Ok(output)
        })
    }

    /// Retrieve selected package layouts without an unbounded scan or result.
    ///
    /// Both passes run in one SQLite read snapshot and force the
    /// `layout(package_id)` index. The preflight streams only per-row byte
    /// counts through an N+1 row limit. The materializing pass independently
    /// reaccounts every decoded row before adding it to the output vector.
    ///
    /// A native SQLite progress handler invokes `checkpoint` every fixed
    /// number of virtual-machine operations and interrupts the statement when
    /// it fails. An independent operation ceiling keeps the query finite even
    /// if a caller supplies a checkpoint that never expires.
    pub fn query_bounded(
        &self,
        packages: &[package::Id],
        bounds: QueryBounds,
        checkpoint: impl FnMut() -> bool,
    ) -> Result<BoundedQueryOutcome, Error> {
        self.query_bounded_impl(packages, bounds, MAX_BOUNDED_QUERY_VM_OPS, checkpoint, |_| {})
    }

    fn query_bounded_impl<F, H>(
        &self,
        packages: &[package::Id],
        bounds: QueryBounds,
        max_vm_ops: u64,
        checkpoint: F,
        between_passes: H,
    ) -> Result<BoundedQueryOutcome, Error>
    where
        F: FnMut() -> bool,
        H: FnOnce(*mut sqlite::sqlite3),
    {
        let packages = match canonical_bounded_packages(packages)? {
            CanonicalPackages::Complete(packages) => packages,
            CanonicalPackages::Limit(outcome) => return Ok(outcome),
        };
        // Reuse the Diesel connection mutex as an operation gate so a clone
        // cannot write through the primary handle while the raw query handle
        // owns its read snapshot.
        self.conn.exec(|_| {
            let mut raw = self.bounded_conn.lock().expect("layout raw connection mutex");
            raw.query_bounded(&packages, bounds, max_vm_ops, checkpoint, between_passes)
        })
    }

    pub fn all(&self) -> Result<Vec<(package::Id, StonePayloadLayoutRecord)>, Error> {
        self.conn.exec(|conn| {
            model::layout::table
                .select(model::Layout::as_select())
                .load_iter(conn)?
                .map(map_layout)
                .collect()
        })
    }

    pub fn package_ids(&self) -> Result<BTreeSet<package::Id>, Error> {
        self.conn.exec(|conn| {
            Ok(model::layout::table
                .select(model::layout::package_id)
                .distinct()
                .load_iter::<AStr, _>(conn)?
                .map(|result| result.map(package::Id::from))
                .collect::<Result<_, _>>()?)
        })
    }

    pub fn file_hashes(&self) -> Result<BTreeSet<String>, Error> {
        self.conn.exec(|conn| {
            let hashes = model::layout::table
                .select(model::layout::entry_value1.assume_not_null())
                .distinct()
                .filter(model::layout::entry_type.eq("regular"))
                .load::<String>(conn)?;

            Ok(hashes
                .into_iter()
                .filter_map(|hash| hash.parse::<u128>().ok().map(|hash| format!("{hash:02x}")))
                .collect())
        })
    }

    pub fn add(&self, package: &package::Id, layout: &StonePayloadLayoutRecord) -> Result<(), Error> {
        self.batch_add(vec![(package, layout)])
    }

    pub fn batch_add<'a>(
        &self,
        layouts: impl IntoIterator<Item = (&'a package::Id, &'a StonePayloadLayoutRecord)>,
    ) -> Result<(), Error> {
        self.conn.exclusive_tx(|tx| {
            let mut ids = vec![];

            let values = layouts
                .into_iter()
                .map(|(package_id, layout)| {
                    ids.push(package_id.as_str());

                    let (entry_type, entry_value1, entry_value2) = encode_entry(&layout.file);

                    model::NewLayout {
                        package_id: package_id.to_string(),
                        uid: layout.uid as i32,
                        gid: layout.gid as i32,
                        mode: layout.mode as i32,
                        tag: layout.tag as i32,
                        entry_type,
                        entry_value1,
                        entry_value2,
                    }
                })
                .collect::<Vec<_>>();

            ids.sort();
            ids.dedup();
            batch_remove_impl(&ids, tx)?;

            for chunk in values.chunks(MAX_VARIABLE_NUMBER / 8) {
                diesel::insert_into(model::layout::table).values(chunk).execute(tx)?;
            }

            Ok(())
        })
    }

    pub fn remove(&self, package: &package::Id) -> Result<(), Error> {
        self.batch_remove(Some(package))
    }

    pub fn batch_remove<'a>(&self, packages: impl IntoIterator<Item = &'a package::Id>) -> Result<(), Error> {
        self.conn.exclusive_tx(|tx| {
            let packages = packages.into_iter().map(package::Id::as_str).collect::<Vec<_>>();

            batch_remove_impl(&packages, tx)?;

            Ok(())
        })
    }
}

enum CanonicalPackages<'a> {
    Complete(Vec<&'a package::Id>),
    Limit(BoundedQueryOutcome),
}

fn canonical_bounded_packages(packages: &[package::Id]) -> Result<CanonicalPackages<'_>, Error> {
    if packages.len() > MAX_BOUNDED_QUERY_PACKAGES {
        return Ok(CanonicalPackages::Limit(BoundedQueryOutcome::PackageLimit {
            limit: MAX_BOUNDED_QUERY_PACKAGES,
            actual: packages.len(),
        }));
    }

    let mut total_bytes = 0usize;
    let mut seen = BTreeSet::new();
    let mut unique = Vec::new();
    unique.try_reserve_exact(packages.len()).map_err(|source| {
        Error::Diesel(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UnableToSendCommand,
            Box::new(format!("reserve bounded layout package selection: {source}")),
        ))
    })?;
    for package in packages {
        total_bytes = total_bytes.checked_add(package.as_str().len()).unwrap_or(usize::MAX);
        if total_bytes > MAX_BOUNDED_QUERY_PACKAGE_ID_BYTES {
            return Ok(CanonicalPackages::Limit(BoundedQueryOutcome::PackageIdByteLimit {
                limit: MAX_BOUNDED_QUERY_PACKAGE_ID_BYTES,
                actual: total_bytes,
            }));
        }
        if seen.insert(package.as_str()) {
            unique.push(package);
        }
    }
    Ok(CanonicalPackages::Complete(unique))
}

fn effective_database_url(url: &str) -> Cow<'_, str> {
    if url == ":memory:" {
        let sequence = NEXT_MEMORY_DATABASE.fetch_add(1, Ordering::Relaxed);
        Cow::Owned(format!(
            "file:forge-layout-{}-{sequence}?mode=memory&cache=shared",
            std::process::id()
        ))
    } else {
        Cow::Borrowed(url)
    }
}

include!("raw_query.rs");

fn batch_remove_impl(packages: &[&str], tx: &mut SqliteConnection) -> Result<(), Error> {
    for chunk in packages.chunks(MAX_VARIABLE_NUMBER) {
        diesel::delete(model::layout::table.filter(model::layout::package_id.eq_any(chunk))).execute(tx)?;
    }
    Ok(())
}

fn map_layout(result: QueryResult<model::Layout>) -> Result<(package::Id, StonePayloadLayoutRecord), Error> {
    let row = result?;

    let entry = decode_entry(row.entry_type, row.entry_value1, row.entry_value2).ok_or(Error::LayoutEntryDecode)?;

    let layout = StonePayloadLayoutRecord {
        uid: row.uid as u32,
        gid: row.gid as u32,
        mode: row.mode as u32,
        tag: row.tag as u32,
        file: entry,
    };

    Ok((row.package_id, layout))
}

fn layout_string_bytes(row: &model::Layout) -> usize {
    row.package_id
        .as_str()
        .len()
        .saturating_add(row.entry_type.len())
        .saturating_add(row.entry_value1.as_ref().map_or(0, |value| value.len()))
        .saturating_add(row.entry_value2.as_ref().map_or(0, |value| value.len()))
}

fn decode_entry(
    entry_type: String,
    entry_value1: Option<AStr>,
    entry_value2: Option<AStr>,
) -> Option<StonePayloadLayoutFile> {
    match entry_type.as_str() {
        "regular" => {
            let hash = entry_value1?.parse::<u128>().ok()?;
            let name = entry_value2?;

            Some(StonePayloadLayoutFile::Regular(hash, name))
        }
        "symlink" => Some(StonePayloadLayoutFile::Symlink(entry_value1?, entry_value2?)),
        "directory" => Some(StonePayloadLayoutFile::Directory(entry_value1?)),
        "character-device" => Some(StonePayloadLayoutFile::CharacterDevice(entry_value1?)),
        "block-device" => Some(StonePayloadLayoutFile::BlockDevice(entry_value1?)),
        "fifo" => Some(StonePayloadLayoutFile::Fifo(entry_value1?)),
        "socket" => Some(StonePayloadLayoutFile::Socket(entry_value1?)),
        "unknown" => Some(StonePayloadLayoutFile::Unknown(entry_value1?, entry_value2?)),
        _ => None,
    }
}

fn encode_entry(entry: &StonePayloadLayoutFile) -> (&'static str, Option<Cow<'_, str>>, Option<&str>) {
    match entry {
        StonePayloadLayoutFile::Regular(hash, name) => ("regular", Some(hash.to_string().into()), Some(name)),
        StonePayloadLayoutFile::Symlink(a, b) => ("symlink", Some(a.into()), Some(b)),
        StonePayloadLayoutFile::Directory(name) => ("directory", Some(name.into()), None),
        StonePayloadLayoutFile::CharacterDevice(name) => ("character-device", Some(name.into()), None),
        StonePayloadLayoutFile::BlockDevice(name) => ("block-device", Some(name.into()), None),
        StonePayloadLayoutFile::Fifo(name) => ("fifo", Some(name.into()), None),
        StonePayloadLayoutFile::Socket(name) => ("socket", Some(name.into()), None),
        StonePayloadLayoutFile::Unknown(a, b) => ("unknown", Some(a.into()), Some(b)),
    }
}

mod model {
    use std::borrow::Cow;

    use astr::AStr;
    use diesel::{Selectable, associations::Identifiable, deserialize::Queryable, prelude::Insertable};

    use crate::package;

    pub use super::schema::layout;

    #[derive(Queryable, Selectable, Identifiable)]
    #[diesel(table_name = layout)]
    pub struct Layout {
        pub id: i32,
        #[diesel(deserialize_as = AStr)]
        pub package_id: package::Id,
        pub uid: i32,
        pub gid: i32,
        pub mode: i32,
        pub tag: i32,
        pub entry_type: String,
        pub entry_value1: Option<AStr>,
        pub entry_value2: Option<AStr>,
    }

    #[derive(Insertable)]
    #[diesel(table_name = layout)]
    pub struct NewLayout<'a> {
        pub package_id: String,
        pub uid: i32,
        pub gid: i32,
        pub mode: i32,
        pub tag: i32,
        pub entry_type: &'a str,
        pub entry_value1: Option<Cow<'a, str>>,
        pub entry_value2: Option<&'a str>,
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod test;
