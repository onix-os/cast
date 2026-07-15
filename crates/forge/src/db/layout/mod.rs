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

#[derive(Debug)]
struct RawLayoutConnection {
    handle: NonNull<sqlite::sqlite3>,
}

// The handle is opened with SQLITE_OPEN_FULLMUTEX and every access is guarded
// by Database::bounded_conn. It is never used concurrently or without that
// mutex, including during destruction.
unsafe impl Send for RawLayoutConnection {}

impl RawLayoutConnection {
    fn open(url: &str) -> Result<Self, Error> {
        let filename = if url.starts_with("sqlite://") {
            url.replacen("sqlite://", "file:", 1)
        } else {
            url.to_owned()
        };
        let filename = CString::new(filename).map_err(ConnectionError::InvalidCString)?;
        let mut handle = ptr::null_mut();
        let flags = sqlite::SQLITE_OPEN_READWRITE
            | sqlite::SQLITE_OPEN_CREATE
            | sqlite::SQLITE_OPEN_URI
            | sqlite::SQLITE_OPEN_FULLMUTEX;
        let status = unsafe { sqlite::sqlite3_open_v2(filename.as_ptr(), &mut handle, flags, ptr::null()) };
        if status != sqlite::SQLITE_OK {
            let message = raw_connection_message(handle, status);
            unsafe {
                sqlite::sqlite3_close(handle);
            }
            return Err(ConnectionError::BadConnection(message).into());
        }
        let handle = NonNull::new(handle).ok_or_else(|| {
            ConnectionError::BadConnection("SQLite returned a null layout query connection".to_owned())
        })?;
        let status = unsafe { sqlite::sqlite3_busy_timeout(handle.as_ptr(), RAW_CONNECTION_BUSY_TIMEOUT_MS) };
        if status != sqlite::SQLITE_OK {
            let error = RawSqliteError::from_connection(handle.as_ptr(), status, "configure bounded busy timeout");
            unsafe {
                sqlite::sqlite3_close(handle.as_ptr());
            }
            return Err(error.into());
        }
        Ok(Self { handle })
    }

    fn query_bounded<F, H>(
        &mut self,
        packages: &[&package::Id],
        bounds: QueryBounds,
        max_vm_ops: u64,
        checkpoint: F,
        between_passes: H,
    ) -> Result<BoundedQueryOutcome, Error>
    where
        F: FnMut() -> bool,
        H: FnOnce(*mut sqlite::sqlite3),
    {
        let mut progress = QueryProgress::new(checkpoint, max_vm_ops);
        if !progress.checkpoint_now() {
            progress.resume_panic_if_any();
            return Ok(BoundedQueryOutcome::Cancelled);
        }

        let mut transaction = RawReadTransaction::begin(self.handle.as_ptr())?;
        let run = {
            let _handler = ProgressHandler::install(self.handle.as_ptr(), &mut progress);
            self.query_two_pass(packages, bounds, &mut progress, between_passes)
        };

        if progress.panic.is_some() {
            transaction.rollback()?;
            progress.resume_panic_if_any();
            unreachable!("resuming a captured checkpoint panic")
        }
        if progress.cancelled || matches!(run, Ok(QueryRun::Cancelled)) {
            transaction.rollback()?;
            return Ok(BoundedQueryOutcome::Cancelled);
        }

        match run {
            Ok(QueryRun::Outcome(outcome @ BoundedQueryOutcome::Complete(_))) => {
                transaction.commit()?;
                Ok(outcome)
            }
            Ok(QueryRun::Outcome(outcome)) => {
                transaction.rollback()?;
                Ok(outcome)
            }
            Ok(QueryRun::Cancelled) => unreachable!("cancelled result handled above"),
            Err(source) => {
                transaction.rollback()?;
                Err(source.into())
            }
        }
    }

    fn query_two_pass<F, H>(
        &mut self,
        packages: &[&package::Id],
        bounds: QueryBounds,
        progress: &mut QueryProgress<F>,
        between_passes: H,
    ) -> Result<QueryRun, RawSqliteError>
    where
        F: FnMut() -> bool,
        H: FnOnce(*mut sqlite::sqlite3),
    {
        let mut preflight_rows = 0usize;
        let mut preflight_string_bytes = 0usize;
        for chunk in packages.chunks(BOUNDED_QUERY_PACKAGE_CHUNK) {
            if !progress.checkpoint_now() {
                return Ok(QueryRun::Cancelled);
            }
            let remaining_rows = bounds.max_rows.saturating_sub(preflight_rows);
            let limit = remaining_rows.saturating_add(1);
            let sql = selected_layout_sql(chunk.len(), SelectedPass::Preflight);
            let mut statement = RawStatement::prepare(self.handle.as_ptr(), &sql)?;
            statement.bind_packages_and_limit(chunk, limit)?;
            loop {
                match statement.step()? {
                    RawStep::Done => break,
                    RawStep::Row => {
                        if !progress.checkpoint_now() {
                            return Ok(QueryRun::Cancelled);
                        }
                        let string_bytes = statement.required_i64(0)?;
                        let string_bytes = usize::try_from(string_bytes).unwrap_or(usize::MAX);
                        preflight_rows = preflight_rows.checked_add(1).unwrap_or(usize::MAX);
                        if preflight_rows > bounds.max_rows {
                            return Ok(QueryRun::Outcome(BoundedQueryOutcome::RowLimit {
                                limit: bounds.max_rows,
                                actual: preflight_rows,
                            }));
                        }
                        preflight_string_bytes = preflight_string_bytes.checked_add(string_bytes).unwrap_or(usize::MAX);
                        if preflight_string_bytes > bounds.max_string_bytes {
                            return Ok(QueryRun::Outcome(BoundedQueryOutcome::StringByteLimit {
                                limit: bounds.max_string_bytes,
                                actual: preflight_string_bytes,
                            }));
                        }
                    }
                }
            }
        }

        if !progress.checkpoint_now() {
            return Ok(QueryRun::Cancelled);
        }
        between_passes(self.handle.as_ptr());
        if !progress.checkpoint_now() {
            return Ok(QueryRun::Cancelled);
        }

        let mut output = Vec::new();
        output.try_reserve_exact(preflight_rows).map_err(|source| {
            RawSqliteError::policy(format!("reserve {preflight_rows} bounded layout rows: {source}"))
        })?;
        let mut streamed_rows = 0usize;
        let mut streamed_string_bytes = 0usize;
        for chunk in packages.chunks(BOUNDED_QUERY_PACKAGE_CHUNK) {
            if !progress.checkpoint_now() {
                return Ok(QueryRun::Cancelled);
            }
            let remaining_rows = bounds.max_rows.saturating_sub(streamed_rows);
            let limit = remaining_rows.saturating_add(1);
            let sql = selected_layout_sql(chunk.len(), SelectedPass::Materialize);
            let mut statement = RawStatement::prepare(self.handle.as_ptr(), &sql)?;
            statement.bind_packages_and_limit(chunk, limit)?;
            loop {
                match statement.step()? {
                    RawStep::Done => break,
                    RawStep::Row => {
                        if !progress.checkpoint_now() {
                            return Ok(QueryRun::Cancelled);
                        }
                        let row = statement.layout()?;
                        streamed_rows = streamed_rows.checked_add(1).unwrap_or(usize::MAX);
                        if streamed_rows > bounds.max_rows {
                            return Ok(QueryRun::Outcome(BoundedQueryOutcome::RowLimit {
                                limit: bounds.max_rows,
                                actual: streamed_rows,
                            }));
                        }
                        streamed_string_bytes = streamed_string_bytes
                            .checked_add(layout_string_bytes(&row))
                            .unwrap_or(usize::MAX);
                        if streamed_string_bytes > bounds.max_string_bytes {
                            return Ok(QueryRun::Outcome(BoundedQueryOutcome::StringByteLimit {
                                limit: bounds.max_string_bytes,
                                actual: streamed_string_bytes,
                            }));
                        }
                        output.push(map_layout(Ok(row)).map_err(RawSqliteError::layout)?);
                    }
                }
            }
        }
        if !progress.checkpoint_now() {
            return Ok(QueryRun::Cancelled);
        }

        Ok(QueryRun::Outcome(BoundedQueryOutcome::Complete(output)))
    }
}

impl Drop for RawLayoutConnection {
    fn drop(&mut self) {
        unsafe {
            let _ = sqlite::sqlite3_close(self.handle.as_ptr());
        }
    }
}

#[derive(Debug)]
enum QueryRun {
    Outcome(BoundedQueryOutcome),
    Cancelled,
}

#[derive(Debug, Clone, Copy)]
enum SelectedPass {
    Preflight,
    Materialize,
}

fn selected_layout_sql(package_count: usize, pass: SelectedPass) -> String {
    let columns = match pass {
        SelectedPass::Preflight => {
            "LENGTH(CAST(package_id AS BLOB)) + \
             LENGTH(CAST(entry_type AS BLOB)) + \
             COALESCE(LENGTH(CAST(entry_value1 AS BLOB)), 0) + \
             COALESCE(LENGTH(CAST(entry_value2 AS BLOB)), 0)"
        }
        SelectedPass::Materialize => "id, package_id, uid, gid, mode, tag, entry_type, entry_value1, entry_value2",
    };
    let placeholders = std::iter::repeat_n("?", package_count).collect::<Vec<_>>().join(",");
    format!(
        "SELECT {columns} FROM layout INDEXED BY {PACKAGE_ID_INDEX} \
         WHERE package_id IN ({placeholders}) LIMIT ?"
    )
}

struct QueryProgress<F> {
    checkpoint: F,
    max_vm_ops: u64,
    observed_vm_ops: u64,
    cancelled: bool,
    panic: Option<Box<dyn Any + Send>>,
}

impl<F> QueryProgress<F>
where
    F: FnMut() -> bool,
{
    fn new(checkpoint: F, max_vm_ops: u64) -> Self {
        Self {
            checkpoint,
            max_vm_ops,
            observed_vm_ops: 0,
            cancelled: false,
            panic: None,
        }
    }

    fn checkpoint_now(&mut self) -> bool {
        if self.cancelled {
            return false;
        }
        match catch_unwind(AssertUnwindSafe(&mut self.checkpoint)) {
            Ok(true) => true,
            Ok(false) => {
                self.cancelled = true;
                false
            }
            Err(payload) => {
                self.cancelled = true;
                self.panic = Some(payload);
                false
            }
        }
    }

    fn progress(&mut self) -> i32 {
        self.observed_vm_ops = self.observed_vm_ops.saturating_add(QUERY_PROGRESS_VM_OPS as u64);
        if self.observed_vm_ops >= self.max_vm_ops {
            self.cancelled = true;
            return 1;
        }
        i32::from(!self.checkpoint_now())
    }

    fn resume_panic_if_any(&mut self) {
        if let Some(payload) = self.panic.take() {
            resume_unwind(payload);
        }
    }
}

struct ProgressHandler<F> {
    database: *mut sqlite::sqlite3,
    _state: PhantomData<*mut QueryProgress<F>>,
}

impl<F> ProgressHandler<F>
where
    F: FnMut() -> bool,
{
    fn install(database: *mut sqlite::sqlite3, state: &mut QueryProgress<F>) -> Self {
        unsafe {
            sqlite::sqlite3_progress_handler(
                database,
                QUERY_PROGRESS_VM_OPS,
                Some(query_progress_callback::<F>),
                ptr::from_mut(state).cast::<c_void>(),
            );
        }
        Self {
            database,
            _state: PhantomData,
        }
    }
}

impl<F> Drop for ProgressHandler<F> {
    fn drop(&mut self) {
        // Unregister before the stack-owned QueryProgress can be inspected or
        // dropped. SQLite invokes this callback synchronously on the thread
        // currently stepping this exact connection.
        unsafe {
            sqlite::sqlite3_progress_handler(self.database, 0, None, ptr::null_mut());
        }
    }
}

unsafe extern "C" fn query_progress_callback<F>(context: *mut c_void) -> i32
where
    F: FnMut() -> bool,
{
    let state = unsafe { &mut *context.cast::<QueryProgress<F>>() };
    state.progress()
}

struct RawReadTransaction {
    database: *mut sqlite::sqlite3,
    finished: bool,
}

impl RawReadTransaction {
    fn begin(database: *mut sqlite::sqlite3) -> Result<Self, RawSqliteError> {
        raw_execute(database, "BEGIN DEFERRED TRANSACTION", "begin bounded layout snapshot")?;
        Ok(Self {
            database,
            finished: false,
        })
    }

    fn commit(&mut self) -> Result<(), RawSqliteError> {
        self.finish("COMMIT", "commit bounded layout snapshot")
    }

    fn rollback(&mut self) -> Result<(), RawSqliteError> {
        self.finish("ROLLBACK", "roll back bounded layout snapshot")
    }

    fn finish(&mut self, sql: &str, operation: &'static str) -> Result<(), RawSqliteError> {
        if self.finished || unsafe { sqlite::sqlite3_get_autocommit(self.database) } != 0 {
            self.finished = true;
            return Ok(());
        }
        raw_execute(self.database, sql, operation)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for RawReadTransaction {
    fn drop(&mut self) {
        if !self.finished && unsafe { sqlite::sqlite3_get_autocommit(self.database) } == 0 {
            let _ = raw_execute(self.database, "ROLLBACK", "drop unfinished bounded layout snapshot");
        }
    }
}

#[derive(Debug)]
enum RawSqliteError {
    Sqlite {
        code: i32,
        operation: &'static str,
        message: String,
    },
    Layout(Error),
    Policy(String),
}

impl RawSqliteError {
    fn from_connection(database: *mut sqlite::sqlite3, code: i32, operation: &'static str) -> Self {
        Self::Sqlite {
            code,
            operation,
            message: raw_connection_message(database, code),
        }
    }

    fn layout(source: Error) -> Self {
        Self::Layout(source)
    }

    fn policy(message: impl Into<String>) -> Self {
        Self::Policy(message.into())
    }
}

impl From<RawSqliteError> for Error {
    fn from(source: RawSqliteError) -> Self {
        match source {
            RawSqliteError::Layout(source) => source,
            RawSqliteError::Sqlite {
                code,
                operation,
                message,
            } => Error::Diesel(diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::Unknown,
                Box::new(format!("{operation}: SQLite error {code}: {message}")),
            )),
            RawSqliteError::Policy(message) => Error::Diesel(diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UnableToSendCommand,
                Box::new(message),
            )),
        }
    }
}

fn raw_connection_message(database: *mut sqlite::sqlite3, fallback_code: i32) -> String {
    if database.is_null() {
        return format!("SQLite error {fallback_code}");
    }
    let message = unsafe { sqlite::sqlite3_errmsg(database) };
    if message.is_null() {
        format!("SQLite error {fallback_code}")
    } else {
        unsafe { CStr::from_ptr(message) }.to_string_lossy().into_owned()
    }
}

fn raw_execute(database: *mut sqlite::sqlite3, sql: &str, operation: &'static str) -> Result<(), RawSqliteError> {
    let sql = CString::new(sql).map_err(|source| RawSqliteError::policy(format!("invalid SQL: {source}")))?;
    let status = unsafe { sqlite::sqlite3_exec(database, sql.as_ptr(), None, ptr::null_mut(), ptr::null_mut()) };
    if status == sqlite::SQLITE_OK {
        Ok(())
    } else {
        Err(RawSqliteError::from_connection(database, status, operation))
    }
}

struct RawStatement {
    database: *mut sqlite::sqlite3,
    statement: NonNull<sqlite::sqlite3_stmt>,
}

impl RawStatement {
    fn prepare(database: *mut sqlite::sqlite3, sql: &str) -> Result<Self, RawSqliteError> {
        let sql = CString::new(sql).map_err(|source| RawSqliteError::policy(format!("invalid SQL: {source}")))?;
        let mut statement = ptr::null_mut();
        let status = unsafe { sqlite::sqlite3_prepare_v2(database, sql.as_ptr(), -1, &mut statement, ptr::null_mut()) };
        if status != sqlite::SQLITE_OK {
            return Err(RawSqliteError::from_connection(
                database,
                status,
                "prepare bounded layout query",
            ));
        }
        let statement = NonNull::new(statement)
            .ok_or_else(|| RawSqliteError::policy("SQLite prepared a null bounded layout statement"))?;
        Ok(Self { database, statement })
    }

    fn bind_packages_and_limit(&mut self, packages: &[&package::Id], limit: usize) -> Result<(), RawSqliteError> {
        for (offset, package) in packages.iter().enumerate() {
            let bytes = package.as_str().as_bytes();
            let length = i32::try_from(bytes.len()).map_err(|_| {
                RawSqliteError::policy(format!(
                    "layout package identifier exceeds SQLite's {}-byte bind limit",
                    i32::MAX
                ))
            })?;
            let parameter = i32::try_from(offset + 1)
                .map_err(|_| RawSqliteError::policy("too many bounded layout package parameters"))?;
            let status = unsafe {
                sqlite::sqlite3_bind_text(
                    self.statement.as_ptr(),
                    parameter,
                    bytes.as_ptr().cast(),
                    length,
                    sqlite::SQLITE_TRANSIENT(),
                )
            };
            self.require_ok(status, "bind bounded layout package identifier")?;
        }
        let parameter = i32::try_from(packages.len() + 1)
            .map_err(|_| RawSqliteError::policy("too many bounded layout parameters"))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let status = unsafe { sqlite::sqlite3_bind_int64(self.statement.as_ptr(), parameter, limit) };
        self.require_ok(status, "bind bounded layout row limit")
    }

    fn step(&mut self) -> Result<RawStep, RawSqliteError> {
        match unsafe { sqlite::sqlite3_step(self.statement.as_ptr()) } {
            sqlite::SQLITE_ROW => Ok(RawStep::Row),
            sqlite::SQLITE_DONE => Ok(RawStep::Done),
            status => Err(RawSqliteError::from_connection(
                self.database,
                status,
                "step bounded layout query",
            )),
        }
    }

    fn required_i64(&self, column: i32) -> Result<i64, RawSqliteError> {
        if unsafe { sqlite::sqlite3_column_type(self.statement.as_ptr(), column) } == sqlite::SQLITE_NULL {
            return Err(RawSqliteError::policy(format!(
                "bounded layout column {column} was unexpectedly null"
            )));
        }
        Ok(unsafe { sqlite::sqlite3_column_int64(self.statement.as_ptr(), column) })
    }

    fn layout(&self) -> Result<model::Layout, RawSqliteError> {
        Ok(model::Layout {
            id: unsafe { sqlite::sqlite3_column_int(self.statement.as_ptr(), 0) },
            package_id: package::Id::from(self.required_text(1)?),
            uid: unsafe { sqlite::sqlite3_column_int(self.statement.as_ptr(), 2) },
            gid: unsafe { sqlite::sqlite3_column_int(self.statement.as_ptr(), 3) },
            mode: unsafe { sqlite::sqlite3_column_int(self.statement.as_ptr(), 4) },
            tag: unsafe { sqlite::sqlite3_column_int(self.statement.as_ptr(), 5) },
            entry_type: self.required_text(6)?,
            entry_value1: self.nullable_text(7)?.map(AStr::from),
            entry_value2: self.nullable_text(8)?.map(AStr::from),
        })
    }

    fn required_text(&self, column: i32) -> Result<String, RawSqliteError> {
        self.nullable_text(column)?
            .ok_or_else(|| RawSqliteError::policy(format!("bounded layout text column {column} was unexpectedly null")))
    }

    fn nullable_text(&self, column: i32) -> Result<Option<String>, RawSqliteError> {
        if unsafe { sqlite::sqlite3_column_type(self.statement.as_ptr(), column) } == sqlite::SQLITE_NULL {
            return Ok(None);
        }
        let length = unsafe { sqlite::sqlite3_column_bytes(self.statement.as_ptr(), column) };
        let length = usize::try_from(length)
            .map_err(|_| RawSqliteError::policy(format!("bounded layout column {column} has a negative length")))?;
        let text = unsafe { sqlite::sqlite3_column_text(self.statement.as_ptr(), column) };
        if text.is_null() && length != 0 {
            return Err(RawSqliteError::from_connection(
                self.database,
                sqlite::SQLITE_NOMEM,
                "read bounded layout text",
            ));
        }
        let bytes = if length == 0 {
            &[][..]
        } else {
            unsafe { slice::from_raw_parts(text, length) }
        };
        let mut owned = Vec::new();
        owned.try_reserve_exact(length).map_err(|source| {
            RawSqliteError::policy(format!("reserve {length} bounded layout text bytes: {source}"))
        })?;
        owned.extend_from_slice(bytes);
        String::from_utf8(owned)
            .map(Some)
            .map_err(|source| RawSqliteError::policy(format!("bounded layout text is not UTF-8: {source}")))
    }

    fn require_ok(&self, status: i32, operation: &'static str) -> Result<(), RawSqliteError> {
        if status == sqlite::SQLITE_OK {
            Ok(())
        } else {
            Err(RawSqliteError::from_connection(self.database, status, operation))
        }
    }
}

impl Drop for RawStatement {
    fn drop(&mut self) {
        unsafe {
            sqlite::sqlite3_finalize(self.statement.as_ptr());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawStep {
    Row,
    Done,
}

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
mod test {
    use diesel::{
        QueryableByName,
        connection::SimpleConnection,
        sql_query,
        sql_types::{BigInt, Text},
    };
    use stone::StoneDecodedPayload;

    use super::*;

    fn regular(path: impl Into<AStr>) -> StonePayloadLayoutRecord {
        StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, path.into()),
        }
    }

    fn layout_bytes(package: &package::Id, path: &str) -> usize {
        package.as_str().len() + "regular".len() + "1".len() + path.len()
    }

    #[derive(Debug, QueryableByName)]
    struct Count {
        #[diesel(sql_type = BigInt)]
        count: i64,
    }

    fn package_index_count(connection: &mut SqliteConnection) -> i64 {
        sql_query(format!(
            "SELECT COUNT(*) AS count FROM sqlite_schema WHERE type = 'index' AND name = '{PACKAGE_ID_INDEX}'"
        ))
        .get_result::<Count>(connection)
        .unwrap()
        .count
    }

    #[test]
    fn create_insert_select() {
        let database = Database::new(":memory:").unwrap();

        let bash_completion = include_bytes!("../../../../../tests/fixtures/bash-completion-2.11-1-1-x86_64.stone");

        let mut stone = stone::read_bytes(bash_completion).unwrap();

        let payloads = stone.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let layouts = payloads
            .iter()
            .filter_map(StoneDecodedPayload::layout)
            .flat_map(|p| &p.body)
            .map(|layout| (package::Id::from("test"), layout))
            .collect::<Vec<_>>();

        let count = layouts.len();

        database.batch_add(layouts.iter().map(|(p, l)| (p, *l))).unwrap();

        let all = database.all().unwrap();

        assert_eq!(count, all.len());
    }

    #[test]
    fn bounded_query_admits_n_rows_and_rejects_n_plus_one_before_allocation() {
        let database = Database::new(":memory:").unwrap();
        let package = package::Id::from("bounded-rows");
        let layouts = [
            "share/one",
            "share/two",
            "share/three",
            "share/four",
            "share/five",
            "share/six",
        ]
        .map(|path| StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, path.into()),
        });
        database
            .batch_add(layouts.iter().map(|layout| (&package, layout)))
            .unwrap();

        let complete = database
            .query_bounded(
                slice::from_ref(&package),
                QueryBounds {
                    max_rows: layouts.len(),
                    max_string_bytes: usize::MAX,
                },
                || true,
            )
            .unwrap();
        assert!(matches!(complete, BoundedQueryOutcome::Complete(rows) if rows.len() == layouts.len()));

        let rejected = database
            .query_bounded(
                slice::from_ref(&package),
                QueryBounds {
                    max_rows: 2,
                    max_string_bytes: usize::MAX,
                },
                || true,
            )
            .unwrap();
        assert!(matches!(
            rejected,
            BoundedQueryOutcome::RowLimit { limit, actual }
                if limit == 2 && actual == 3
        ));
    }

    #[test]
    fn bounded_query_counts_utf8_storage_bytes_at_the_exact_boundary() {
        let database = Database::new(":memory:").unwrap();
        let package = package::Id::from("bounded-bytes");
        let path = "share/café";
        let layout = StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFREG | 0o644,
            tag: 0,
            file: StonePayloadLayoutFile::Regular(1, path.into()),
        };
        database.add(&package, &layout).unwrap();
        let exact = package.as_str().len() + "regular".len() + "1".len() + path.len();

        let complete = database
            .query_bounded(
                slice::from_ref(&package),
                QueryBounds {
                    max_rows: 1,
                    max_string_bytes: exact,
                },
                || true,
            )
            .unwrap();
        assert!(matches!(complete, BoundedQueryOutcome::Complete(rows) if rows.len() == 1));

        let rejected = database
            .query_bounded(
                slice::from_ref(&package),
                QueryBounds {
                    max_rows: 1,
                    max_string_bytes: exact - 1,
                },
                || true,
            )
            .unwrap();
        assert!(matches!(
            rejected,
            BoundedQueryOutcome::StringByteLimit { limit, actual }
                if limit == exact - 1 && actual == exact
        ));
    }

    #[test]
    fn package_index_migration_upgrades_downgrades_and_upgrades_cleanly() {
        let database = Database::new(":memory:").unwrap();
        database.conn.exec(|connection| {
            assert_eq!(package_index_count(connection), 1);
            let reverted = connection.revert_last_migration(MIGRATIONS).unwrap();
            assert_eq!(reverted.to_string(), "20260714120000");
            assert_eq!(package_index_count(connection), 0);
            let applied = connection.run_pending_migrations(MIGRATIONS).unwrap();
            assert_eq!(
                applied.iter().map(ToString::to_string).collect::<Vec<_>>(),
                ["20260714120000"]
            );
            assert_eq!(package_index_count(connection), 1);
        });
    }

    #[derive(Debug, QueryableByName)]
    struct QueryPlanDetail {
        #[diesel(sql_type = Text)]
        detail: String,
    }

    #[test]
    fn both_selected_package_query_plans_use_package_index() {
        let database = Database::new(":memory:").unwrap();
        for pass in [SelectedPass::Preflight, SelectedPass::Materialize] {
            let sql = format!("EXPLAIN QUERY PLAN {}", selected_layout_sql(1, pass));
            let details = database.conn.exec(|connection| {
                sql_query(sql)
                    .bind::<Text, _>("selected")
                    .bind::<BigInt, _>(2_i64)
                    .load::<QueryPlanDetail>(connection)
                    .unwrap()
            });
            assert!(
                details.iter().any(|row| {
                    row.detail.contains(&format!("USING INDEX {PACKAGE_ID_INDEX}"))
                        || row.detail.contains(&format!("USING COVERING INDEX {PACKAGE_ID_INDEX}"))
                }),
                "{pass:?} query plan did not use {PACKAGE_ID_INDEX}: {details:?}"
            );
        }
    }

    #[test]
    fn bounded_query_package_inputs_accept_n_reject_n_plus_one_and_deduplicate() {
        let database = Database::new(":memory:").unwrap();
        let selected = package::Id::from("selected-once");
        database.add(&selected, &regular("share/once")).unwrap();
        let duplicates = [selected.clone(), selected.clone(), selected.clone()];
        let deduplicated = database
            .query_bounded(
                &duplicates,
                QueryBounds {
                    max_rows: 1,
                    max_string_bytes: usize::MAX,
                },
                || true,
            )
            .unwrap();
        assert!(matches!(deduplicated, BoundedQueryOutcome::Complete(rows) if rows.len() == 1));

        let exact = (0..MAX_BOUNDED_QUERY_PACKAGES)
            .map(|index| package::Id::from(format!("absent-{index}")))
            .collect::<Vec<_>>();
        assert!(matches!(
            database
                .query_bounded(
                    &exact,
                    QueryBounds {
                        max_rows: 1,
                        max_string_bytes: usize::MAX,
                    },
                    || true,
                )
                .unwrap(),
            BoundedQueryOutcome::Complete(rows) if rows.is_empty()
        ));

        let mut over = exact;
        over.push(package::Id::from("absent-over"));
        assert!(matches!(
            database
                .query_bounded(
                    &over,
                    QueryBounds {
                        max_rows: 1,
                        max_string_bytes: usize::MAX,
                    },
                    || true,
                )
                .unwrap(),
            BoundedQueryOutcome::PackageLimit { limit, actual }
                if limit == MAX_BOUNDED_QUERY_PACKAGES && actual == limit + 1
        ));
    }

    #[test]
    fn bounded_query_package_id_bytes_accept_n_and_reject_n_plus_one() {
        let database = Database::new(":memory:").unwrap();
        let exact = package::Id::from("x".repeat(MAX_BOUNDED_QUERY_PACKAGE_ID_BYTES));
        assert!(matches!(
            database
                .query_bounded(
                    slice::from_ref(&exact),
                    QueryBounds {
                        max_rows: 0,
                        max_string_bytes: 0,
                    },
                    || true,
                )
                .unwrap(),
            BoundedQueryOutcome::Complete(rows) if rows.is_empty()
        ));

        let over = package::Id::from("x".repeat(MAX_BOUNDED_QUERY_PACKAGE_ID_BYTES + 1));
        assert!(matches!(
            database
                .query_bounded(
                    slice::from_ref(&over),
                    QueryBounds {
                        max_rows: 0,
                        max_string_bytes: 0,
                    },
                    || true,
                )
                .unwrap(),
            BoundedQueryOutcome::PackageIdByteLimit { limit, actual }
                if limit == MAX_BOUNDED_QUERY_PACKAGE_ID_BYTES && actual == limit + 1
        ));
    }

    #[test]
    fn sqlite_progress_policy_interrupts_selected_package_work() {
        let database = Database::new(":memory:").unwrap();
        let selected = package::Id::from("progress-selected");
        let layouts = (0..4_096)
            .map(|index| regular(format!("share/progress/{index}")))
            .collect::<Vec<_>>();
        database
            .batch_add(layouts.iter().map(|layout| (&selected, layout)))
            .unwrap();

        let outcome = database
            .query_bounded_impl(
                slice::from_ref(&selected),
                QueryBounds {
                    max_rows: layouts.len(),
                    max_string_bytes: usize::MAX,
                },
                QUERY_PROGRESS_VM_OPS as u64,
                || true,
                |_| {},
            )
            .unwrap();
        assert!(matches!(outcome, BoundedQueryOutcome::Cancelled));
    }

    #[test]
    fn absent_selected_package_does_not_scan_many_unrelated_rows() {
        let database = Database::new(":memory:").unwrap();
        let unrelated = package::Id::from("unrelated");
        let layouts = (0..8_192)
            .map(|index| regular(format!("share/unrelated/{index}")))
            .collect::<Vec<_>>();
        database
            .batch_add(layouts.iter().map(|layout| (&unrelated, layout)))
            .unwrap();
        let absent = package::Id::from("absent");

        let outcome = database
            .query_bounded_impl(
                slice::from_ref(&absent),
                QueryBounds {
                    max_rows: 1,
                    max_string_bytes: 1,
                },
                2_000,
                || true,
                |_| {},
            )
            .unwrap();
        assert!(matches!(outcome, BoundedQueryOutcome::Complete(rows) if rows.is_empty()));
    }

    #[test]
    fn two_pass_query_keeps_one_snapshot_across_external_commit() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("layout.sqlite");
        let url = path.to_str().unwrap();
        let database = Database::new(url).unwrap();
        database
            .conn
            .exec(|connection| connection.batch_execute("PRAGMA journal_mode = WAL").unwrap());
        let writer = Database::new(url).unwrap();
        let selected = package::Id::from("snapshot-selected");
        let old_path = "share/old";
        let new_path = "share/new";
        database.add(&selected, &regular(old_path)).unwrap();
        let exact = layout_bytes(&selected, old_path);

        let outcome = database
            .query_bounded_impl(
                slice::from_ref(&selected),
                QueryBounds {
                    max_rows: 1,
                    max_string_bytes: exact,
                },
                MAX_BOUNDED_QUERY_VM_OPS,
                || true,
                |_| writer.add(&selected, &regular(new_path)).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            outcome,
            BoundedQueryOutcome::Complete(rows)
                if matches!(&rows[0].1.file, StonePayloadLayoutFile::Regular(_, path) if path.as_str() == old_path)
        ));
        let current = database.query(slice::from_ref(&selected)).unwrap();
        assert!(matches!(
            &current[0].1.file,
            StonePayloadLayoutFile::Regular(_, path) if path.as_str() == new_path
        ));
    }

    #[test]
    fn materialization_reaccounts_changed_rows_and_rolls_back_test_mutation() {
        let database = Database::new(":memory:").unwrap();
        let selected = package::Id::from("accounting-selected");
        let old_path = "a";
        let changed_path = "a-much-longer-path";
        database.add(&selected, &regular(old_path)).unwrap();
        let exact = layout_bytes(&selected, old_path);

        let outcome = database
            .query_bounded_impl(
                slice::from_ref(&selected),
                QueryBounds {
                    max_rows: 1,
                    max_string_bytes: exact,
                },
                MAX_BOUNDED_QUERY_VM_OPS,
                || true,
                |connection| {
                    raw_execute(
                        connection,
                        &format!("UPDATE layout SET entry_value2 = '{changed_path}'"),
                        "inject accounting test mutation",
                    )
                    .unwrap();
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            BoundedQueryOutcome::StringByteLimit { limit, actual }
                if limit == exact && actual == layout_bytes(&selected, changed_path)
        ));
        let current = database.query(slice::from_ref(&selected)).unwrap();
        assert!(matches!(
            &current[0].1.file,
            StonePayloadLayoutFile::Regular(_, path) if path.as_str() == old_path
        ));
    }
}
