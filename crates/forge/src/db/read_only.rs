//! Raw SQLite boundary for existing, non-mutating database snapshots.

use std::{
    ffi::{CStr, c_char, c_int, c_void},
    marker::PhantomData,
    ptr::{self, NonNull},
    slice,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use libsqlite3_sys as sqlite;
use thiserror::Error;

use crate::{
    Installation,
    installation::{DatabaseKind, ReadOnlyDatabaseFile},
};

mod admission;
use admission::{configure_memory_temp_store, expected_migrations, require_deserialized_read_only};

#[cfg(test)]
mod tests;

const BUSY_TIMEOUT_MILLISECONDS: c_int = 1_000;
const MAX_DATABASE_IMAGE_BYTES: usize = 256 * 1024 * 1024;
const MAX_SQLITE_VALUE_BYTES: c_int = 16 * 1024 * 1024;
const MAX_SQLITE_COLUMNS: c_int = 128;
const MAX_SQL_BYTES: c_int = 64 * 1024;
const PROGRESS_OPCODE_INTERVAL: c_int = 1_000;
const QUERY_PROGRESS_CALLBACK_BUDGET: usize = 50_000;
const QUERY_DEADLINE: Duration = Duration::from_secs(2);
const MAX_MIGRATION_ROWS: usize = 8;

#[derive(Clone, Debug)]
pub(crate) struct ReadOnlyConnection {
    raw: Arc<Mutex<RawConnection>>,
    anchor: ReadOnlyDatabaseFile,
}

#[derive(Debug)]
struct RawConnection {
    handle: NonNull<sqlite::sqlite3>,
    _image: Box<[u8]>,
    progress: Box<ProgressState>,
    poisoned: bool,
    compile_temp_store: c_int,
    runtime_temp_store: c_int,
}

#[derive(Debug)]
struct ProgressState {
    remaining_callbacks: AtomicUsize,
    deadline: Mutex<Option<Instant>>,
    interrupted: AtomicU8,
}

#[derive(Clone, Copy)]
struct QueryLimits {
    callback_budget: usize,
    deadline: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum InterruptReason {
    None = 0,
    OpcodeBudget = 1,
    Deadline = 2,
}

// The handle is opened FULLMUTEX and every use is additionally serialized by
// ReadOnlyConnection::raw, including destruction.
unsafe impl Send for RawConnection {}

impl ReadOnlyConnection {
    pub(crate) fn open(installation: &Installation, kind: DatabaseKind) -> Result<Self, ReadOnlyError> {
        let anchor = installation.open_read_only_database(kind)?;
        let image = installation.read_read_only_database_image(&anchor, MAX_DATABASE_IMAGE_BYTES)?;
        let mut handle = ptr::null_mut();
        let flags = sqlite::SQLITE_OPEN_READWRITE
            | sqlite::SQLITE_OPEN_CREATE
            | sqlite::SQLITE_OPEN_MEMORY
            | sqlite::SQLITE_OPEN_FULLMUTEX;
        let status = unsafe { sqlite::sqlite3_open_v2(c":memory:".as_ptr(), &mut handle, flags, ptr::null()) };
        if status != sqlite::SQLITE_OK {
            let message = connection_message(handle, status);
            unsafe {
                let _ = sqlite::sqlite3_close(handle);
            }
            return Err(ReadOnlyError::Open { status, message });
        }
        let handle = NonNull::new(handle).ok_or_else(|| ReadOnlyError::Open {
            status: sqlite::SQLITE_CANTOPEN,
            message: "SQLite returned a null in-memory connection".to_owned(),
        })?;
        let mut raw = RawConnection {
            handle,
            _image: image,
            progress: Box::new(ProgressState::new()),
            poisoned: false,
            compile_temp_store: 0,
            runtime_temp_store: 0,
        };

        let image_length = i64::try_from(raw._image.len()).map_err(|_| ReadOnlyError::ImageLength {
            actual: raw._image.len(),
        })?;
        let deserialize = unsafe {
            sqlite::sqlite3_deserialize(
                raw.handle.as_ptr(),
                c"main".as_ptr(),
                raw._image.as_mut_ptr(),
                image_length,
                image_length,
                sqlite::SQLITE_DESERIALIZE_READONLY as u32,
            )
        };
        if deserialize != sqlite::SQLITE_OK {
            let source = ReadOnlyError::Deserialize {
                status: deserialize,
                message: connection_message(raw.handle.as_ptr(), deserialize),
            };
            return if source.sqlite_status_is_corruption() {
                Err(ReadOnlyError::CorruptImage {
                    database: expected_migrations(kind).0,
                    source: Box::new(source),
                })
            } else {
                Err(source)
            };
        }

        require_deserialized_read_only(&raw, kind)?;
        let (compile_temp_store, runtime_temp_store) = configure_memory_temp_store(&raw)?;
        raw.compile_temp_store = compile_temp_store;
        raw.runtime_temp_store = runtime_temp_store;
        require_ok(
            raw.handle.as_ptr(),
            unsafe { sqlite::sqlite3_extended_result_codes(raw.handle.as_ptr(), 1) },
            "enable extended SQLite result codes",
        )?;
        require_ok(
            raw.handle.as_ptr(),
            unsafe { sqlite::sqlite3_busy_timeout(raw.handle.as_ptr(), BUSY_TIMEOUT_MILLISECONDS) },
            "configure bounded SQLite busy timeout",
        )?;
        unsafe {
            sqlite::sqlite3_limit(raw.handle.as_ptr(), sqlite::SQLITE_LIMIT_LENGTH, MAX_SQLITE_VALUE_BYTES);
            sqlite::sqlite3_limit(raw.handle.as_ptr(), sqlite::SQLITE_LIMIT_COLUMN, MAX_SQLITE_COLUMNS);
            sqlite::sqlite3_limit(raw.handle.as_ptr(), sqlite::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES);
        }
        require_ok(
            raw.handle.as_ptr(),
            unsafe { sqlite::sqlite3_set_authorizer(raw.handle.as_ptr(), Some(read_only_authorizer), ptr::null_mut()) },
            "install read-only SQLite authorizer",
        )?;

        let connection = Self {
            raw: Arc::new(Mutex::new(raw)),
            anchor,
        };
        connection.verify_migration_set(kind)?;
        installation.revalidate_read_only_database(&connection.anchor)?;
        Ok(connection)
    }

    pub(crate) fn snapshot<T>(
        &self,
        operation: impl FnOnce(&ReadOnlyRow) -> Result<T, ReadOnlyError>,
    ) -> Result<T, ReadOnlyError> {
        // The monotonic deadline starts after this connection's cooperative
        // mutex is acquired. The finite opcode callback bounds SQLite work;
        // it is not a scheduler or hostile-thread preemption guarantee.
        self.snapshot_with_limits(
            QueryLimits {
                callback_budget: QUERY_PROGRESS_CALLBACK_BUDGET,
                deadline: QUERY_DEADLINE,
            },
            operation,
        )
    }

    fn snapshot_with_limits<T>(
        &self,
        limits: QueryLimits,
        operation: impl FnOnce(&ReadOnlyRow) -> Result<T, ReadOnlyError>,
    ) -> Result<T, ReadOnlyError> {
        let mut raw = self.raw.lock().map_err(|_| ReadOnlyError::LockPoisoned)?;
        if raw.poisoned {
            return Err(ReadOnlyError::ConnectionPoisoned);
        }
        raw.arm_progress(limits);
        let row = ReadOnlyRow {
            database: raw.handle.as_ptr(),
        };
        if let Err(source) = row.execute(c"BEGIN") {
            let source = raw.progress.decorate(source);
            raw.clear_progress();
            return transaction_failure(&mut raw, &row, source);
        }
        if autocommit(&raw) {
            raw.clear_progress();
            return transaction_failure(
                &mut raw,
                &row,
                ReadOnlyError::TransactionState {
                    context: "BEGIN completed without opening a transaction",
                },
            );
        }

        match operation(&row) {
            Ok(value) => match row.execute(c"COMMIT") {
                Ok(()) if autocommit(&raw) => {
                    raw.clear_progress();
                    Ok(value)
                }
                Ok(()) => {
                    raw.clear_progress();
                    transaction_failure(
                        &mut raw,
                        &row,
                        ReadOnlyError::TransactionState {
                            context: "COMMIT completed but the transaction remains active",
                        },
                    )
                }
                Err(source) => {
                    let source = raw.progress.decorate(source);
                    raw.clear_progress();
                    transaction_failure(&mut raw, &row, source)
                }
            },
            Err(source) => {
                let source = raw.progress.decorate(source);
                raw.clear_progress();
                transaction_failure(&mut raw, &row, source)
            }
        }
    }

    pub(crate) fn anchor(&self) -> &ReadOnlyDatabaseFile {
        &self.anchor
    }

    #[cfg(test)]
    pub(crate) fn attempt_test_write(&self) -> Result<(), ReadOnlyError> {
        self.snapshot(|row| row.execute(c"CREATE TABLE forbidden_read_only_write(value INTEGER)"))
    }

    #[cfg(test)]
    pub(crate) fn attempt_test_function(&self) -> Result<(), ReadOnlyError> {
        self.snapshot(|row| {
            let _ = row.prepare(c"SELECT random()")?;
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn attempt_test_opcode_exhaustion(&self) -> Result<(), ReadOnlyError> {
        self.attempt_test_interruption(QueryLimits {
            callback_budget: 1,
            deadline: Duration::from_secs(30),
        })
    }

    #[cfg(test)]
    pub(crate) fn attempt_test_deadline_exhaustion(&self) -> Result<(), ReadOnlyError> {
        self.attempt_test_interruption(QueryLimits {
            callback_budget: usize::MAX,
            deadline: Duration::ZERO,
        })
    }

    #[cfg(test)]
    pub(crate) fn test_temp_store_modes(&self) -> Result<(c_int, c_int), ReadOnlyError> {
        let raw = self.raw.lock().map_err(|_| ReadOnlyError::LockPoisoned)?;
        Ok((raw.compile_temp_store, raw.runtime_temp_store))
    }

    #[cfg(test)]
    pub(crate) fn attempt_test_ordered_scan(&self) -> Result<(), ReadOnlyError> {
        self.snapshot(|row| {
            let mut statement = row.prepare(c"SELECT a.name FROM sqlite_master AS a, sqlite_master AS b, sqlite_master AS c, sqlite_master AS d, sqlite_master AS e, sqlite_master AS f, sqlite_master AS g ORDER BY a.name, b.name, c.name, d.name, e.name, f.name, g.name")?;
            while statement.step()? == Step::Row {}
            Ok(())
        })
    }

    #[cfg(test)]
    fn attempt_test_interruption(&self, limits: QueryLimits) -> Result<(), ReadOnlyError> {
        self.snapshot_with_limits(limits, |row| {
            let mut statement = row.prepare(c"SELECT a.name FROM sqlite_master AS a, sqlite_master AS b, sqlite_master AS c, sqlite_master AS d, sqlite_master AS e, sqlite_master AS f, sqlite_master AS g, sqlite_master AS h")?;
            while statement.step()? == Step::Row {}
            Ok(())
        })
    }

    fn verify_migration_set(&self, kind: DatabaseKind) -> Result<(), ReadOnlyError> {
        // This admits the exact bounded Diesel migration-version ledger. It
        // deliberately does not claim byte-for-byte DDL/schema equivalence;
        // row decoders still fail closed on structural or type corruption.
        let (database, expected) = expected_migrations(kind);
        let actual = self
            .snapshot(|row| {
                let mut statement = row.prepare(c"SELECT version FROM __diesel_schema_migrations LIMIT 9")?;
                let mut versions = Vec::new();
                while statement.step()? == Step::Row {
                    if versions.len() == MAX_MIGRATION_ROWS {
                        return Err(ReadOnlyError::Limit {
                            resource: "migration-version rows",
                            limit: MAX_MIGRATION_ROWS,
                        });
                    }
                    versions.push(statement.text(0, 32)?);
                }
                versions.sort();
                Ok(versions)
            })
            .map_err(|source| {
                if source.sqlite_status_is_corruption() {
                    ReadOnlyError::CorruptImage {
                        database,
                        source: Box::new(source),
                    }
                } else {
                    ReadOnlyError::MigrationSetValidation {
                        database,
                        source: Box::new(source),
                    }
                }
            })?;
        if actual.iter().map(String::as_str).ne(expected.iter().copied()) {
            return Err(ReadOnlyError::MigrationSetMismatch {
                database,
                expected,
                actual,
            });
        }
        Ok(())
    }
}

impl RawConnection {
    fn arm_progress(&mut self, limits: QueryLimits) {
        self.progress.arm(limits);
        let context = (&*self.progress as *const ProgressState).cast_mut().cast::<c_void>();
        unsafe {
            sqlite::sqlite3_progress_handler(
                self.handle.as_ptr(),
                PROGRESS_OPCODE_INTERVAL,
                Some(query_progress),
                context,
            );
        }
    }

    fn clear_progress(&mut self) {
        unsafe {
            sqlite::sqlite3_progress_handler(self.handle.as_ptr(), 0, None, ptr::null_mut());
        }
        self.progress.clear();
    }
}

impl ProgressState {
    fn new() -> Self {
        Self {
            remaining_callbacks: AtomicUsize::new(0),
            deadline: Mutex::new(None),
            interrupted: AtomicU8::new(InterruptReason::None as u8),
        }
    }

    fn arm(&self, limits: QueryLimits) {
        self.remaining_callbacks
            .store(limits.callback_budget, Ordering::Release);
        self.interrupted.store(InterruptReason::None as u8, Ordering::Release);
        let now = Instant::now();
        let deadline = now.checked_add(limits.deadline).unwrap_or(now);
        *self.deadline.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(deadline);
    }

    fn clear(&self) {
        self.remaining_callbacks.store(0, Ordering::Release);
        *self.deadline.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    fn reason(&self) -> InterruptReason {
        match self.interrupted.load(Ordering::Acquire) {
            value if value == InterruptReason::OpcodeBudget as u8 => InterruptReason::OpcodeBudget,
            value if value == InterruptReason::Deadline as u8 => InterruptReason::Deadline,
            _ => InterruptReason::None,
        }
    }

    fn interrupt(&self, reason: InterruptReason) -> c_int {
        let _ = self.interrupted.compare_exchange(
            InterruptReason::None as u8,
            reason as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        1
    }

    fn decorate(&self, source: ReadOnlyError) -> ReadOnlyError {
        if source.sqlite_status_is_interrupt() {
            match self.reason() {
                InterruptReason::OpcodeBudget => ReadOnlyError::QueryInterrupted {
                    reason: "finite SQLite opcode budget exhausted",
                },
                InterruptReason::Deadline => ReadOnlyError::QueryInterrupted {
                    reason: "monotonic SQLite query deadline elapsed",
                },
                InterruptReason::None => source,
            }
        } else {
            source
        }
    }
}

fn autocommit(raw: &RawConnection) -> bool {
    unsafe { sqlite::sqlite3_get_autocommit(raw.handle.as_ptr()) != 0 }
}

fn transaction_failure<T>(
    raw: &mut RawConnection,
    row: &ReadOnlyRow,
    source: ReadOnlyError,
) -> Result<T, ReadOnlyError> {
    if autocommit(raw) {
        return Err(source);
    }
    match row.execute(c"ROLLBACK") {
        Ok(()) if autocommit(raw) => Err(source),
        Ok(()) => {
            raw.poisoned = true;
            Err(ReadOnlyError::TransactionPoisoned {
                source: Box::new(source),
                cleanup: None,
            })
        }
        Err(cleanup) if autocommit(raw) => Err(ReadOnlyError::Rollback {
            source: Box::new(source),
            rollback: Box::new(cleanup),
        }),
        Err(cleanup) => {
            raw.poisoned = true;
            Err(ReadOnlyError::TransactionPoisoned {
                source: Box::new(source),
                cleanup: Some(Box::new(cleanup)),
            })
        }
    }
}

unsafe extern "C" fn query_progress(context: *mut c_void) -> c_int {
    if context.is_null() {
        return 1;
    }
    let progress = unsafe { &*context.cast::<ProgressState>() };
    let deadline = *progress
        .deadline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
        return progress.interrupt(InterruptReason::Deadline);
    }
    match progress
        .remaining_callbacks
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
            remaining.checked_sub(1)
        }) {
        Ok(remaining) if remaining > 1 => 0,
        _ => progress.interrupt(InterruptReason::OpcodeBudget),
    }
}

impl Drop for RawConnection {
    fn drop(&mut self) {
        unsafe {
            sqlite::sqlite3_progress_handler(self.handle.as_ptr(), 0, None, ptr::null_mut());
            let _ = sqlite::sqlite3_close(self.handle.as_ptr());
        }
    }
}

pub(crate) struct ReadOnlyRow {
    database: *mut sqlite::sqlite3,
}

impl ReadOnlyRow {
    pub(crate) fn prepare<'a>(&'a self, sql: &CStr) -> Result<ReadOnlyStatement<'a>, ReadOnlyError> {
        let mut statement = ptr::null_mut();
        let status = unsafe {
            sqlite::sqlite3_prepare_v3(
                self.database,
                sql.as_ptr(),
                -1,
                sqlite::SQLITE_PREPARE_PERSISTENT as u32,
                &mut statement,
                ptr::null_mut(),
            )
        };
        require_ok(self.database, status, "prepare read-only SQLite query")?;
        let statement = NonNull::new(statement).ok_or(ReadOnlyError::NullStatement)?;
        if unsafe { sqlite::sqlite3_stmt_readonly(statement.as_ptr()) } != 1 {
            unsafe {
                let _ = sqlite::sqlite3_finalize(statement.as_ptr());
            }
            return Err(ReadOnlyError::WritableStatement);
        }
        Ok(ReadOnlyStatement {
            database: self.database,
            statement,
            _session: PhantomData,
        })
    }

    fn execute(&self, sql: &'static CStr) -> Result<(), ReadOnlyError> {
        let mut statement = self.prepare(sql)?;
        match statement.step()? {
            Step::Done => Ok(()),
            Step::Row => Err(ReadOnlyError::UnexpectedRow),
        }
    }
}

pub(crate) struct ReadOnlyStatement<'a> {
    database: *mut sqlite::sqlite3,
    statement: NonNull<sqlite::sqlite3_stmt>,
    _session: PhantomData<&'a ReadOnlyRow>,
}

impl ReadOnlyStatement<'_> {
    pub(crate) fn bind_i64(&mut self, parameter: c_int, value: i64) -> Result<(), ReadOnlyError> {
        require_ok(
            self.database,
            unsafe { sqlite::sqlite3_bind_int64(self.statement.as_ptr(), parameter, value) },
            "bind read-only SQLite integer",
        )
    }

    pub(crate) fn bind_text(&mut self, parameter: c_int, value: &str) -> Result<(), ReadOnlyError> {
        let length = c_int::try_from(value.len()).map_err(|_| ReadOnlyError::TextLimit {
            limit: c_int::MAX as usize,
            actual: value.len(),
        })?;
        require_ok(
            self.database,
            unsafe {
                sqlite::sqlite3_bind_text(
                    self.statement.as_ptr(),
                    parameter,
                    value.as_ptr().cast(),
                    length,
                    sqlite::SQLITE_TRANSIENT(),
                )
            },
            "bind read-only SQLite text",
        )
    }

    pub(crate) fn step(&mut self) -> Result<Step, ReadOnlyError> {
        match unsafe { sqlite::sqlite3_step(self.statement.as_ptr()) } {
            sqlite::SQLITE_ROW => Ok(Step::Row),
            sqlite::SQLITE_DONE => Ok(Step::Done),
            status => Err(sqlite_error(self.database, status, "step read-only SQLite query")),
        }
    }

    pub(crate) fn i64(&self, column: c_int) -> Result<i64, ReadOnlyError> {
        self.require_non_null(column)?;
        if unsafe { sqlite::sqlite3_column_type(self.statement.as_ptr(), column) } != sqlite::SQLITE_INTEGER {
            return Err(ReadOnlyError::ColumnType {
                column,
                expected: "integer",
            });
        }
        Ok(unsafe { sqlite::sqlite3_column_int64(self.statement.as_ptr(), column) })
    }

    pub(crate) fn nullable_i64(&self, column: c_int) -> Result<Option<i64>, ReadOnlyError> {
        if unsafe { sqlite::sqlite3_column_type(self.statement.as_ptr(), column) } == sqlite::SQLITE_NULL {
            Ok(None)
        } else {
            self.i64(column).map(Some)
        }
    }

    pub(crate) fn bool(&self, column: c_int) -> Result<bool, ReadOnlyError> {
        match self.i64(column)? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(ReadOnlyError::Boolean { column, value }),
        }
    }

    pub(crate) fn text(&self, column: c_int, max_bytes: usize) -> Result<String, ReadOnlyError> {
        self.nullable_text(column, max_bytes)?
            .ok_or(ReadOnlyError::UnexpectedNull { column })
    }

    pub(crate) fn nullable_text(&self, column: c_int, max_bytes: usize) -> Result<Option<String>, ReadOnlyError> {
        if unsafe { sqlite::sqlite3_column_type(self.statement.as_ptr(), column) } == sqlite::SQLITE_NULL {
            return Ok(None);
        }
        if unsafe { sqlite::sqlite3_column_type(self.statement.as_ptr(), column) } != sqlite::SQLITE_TEXT {
            return Err(ReadOnlyError::ColumnType {
                column,
                expected: "text",
            });
        }
        let length = unsafe { sqlite::sqlite3_column_bytes(self.statement.as_ptr(), column) };
        let length = usize::try_from(length).map_err(|_| ReadOnlyError::ColumnLength { column })?;
        if length > max_bytes {
            return Err(ReadOnlyError::TextLimit {
                limit: max_bytes,
                actual: length,
            });
        }
        let text = unsafe { sqlite::sqlite3_column_text(self.statement.as_ptr(), column) };
        if text.is_null() {
            return Err(sqlite_error(
                self.database,
                sqlite::SQLITE_NOMEM,
                "read SQLite text column",
            ));
        }
        let bytes = if length == 0 {
            &[][..]
        } else {
            unsafe { slice::from_raw_parts(text, length) }
        };
        let value = std::str::from_utf8(bytes).map_err(|source| ReadOnlyError::Utf8 { column, source })?;
        Ok(Some(value.to_owned()))
    }

    fn require_non_null(&self, column: c_int) -> Result<(), ReadOnlyError> {
        if unsafe { sqlite::sqlite3_column_type(self.statement.as_ptr(), column) } == sqlite::SQLITE_NULL {
            Err(ReadOnlyError::UnexpectedNull { column })
        } else {
            Ok(())
        }
    }
}

impl Drop for ReadOnlyStatement<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = sqlite::sqlite3_finalize(self.statement.as_ptr());
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    Row,
    Done,
}

unsafe extern "C" fn read_only_authorizer(
    _context: *mut c_void,
    action: c_int,
    _first: *const c_char,
    _second: *const c_char,
    _database: *const c_char,
    _trigger: *const c_char,
) -> c_int {
    match action {
        sqlite::SQLITE_SELECT | sqlite::SQLITE_READ | sqlite::SQLITE_TRANSACTION => sqlite::SQLITE_OK,
        _ => sqlite::SQLITE_DENY,
    }
}

fn require_ok(database: *mut sqlite::sqlite3, status: c_int, operation: &'static str) -> Result<(), ReadOnlyError> {
    if status == sqlite::SQLITE_OK {
        Ok(())
    } else {
        Err(sqlite_error(database, status, operation))
    }
}

fn sqlite_error(database: *mut sqlite::sqlite3, status: c_int, operation: &'static str) -> ReadOnlyError {
    ReadOnlyError::Sqlite {
        operation,
        status,
        message: connection_message(database, status),
    }
}

fn connection_message(database: *mut sqlite::sqlite3, status: c_int) -> String {
    let message = if database.is_null() {
        unsafe { sqlite::sqlite3_errstr(status) }
    } else {
        unsafe { sqlite::sqlite3_errmsg(database) }
    };
    if message.is_null() {
        format!("SQLite status {status}")
    } else {
        unsafe { CStr::from_ptr(message) }.to_string_lossy().into_owned()
    }
}

#[derive(Debug, Error)]
pub enum ReadOnlyError {
    #[error(transparent)]
    Installation(#[from] crate::installation::Error),
    #[error("open private in-memory SQLite database failed ({status}): {message}")]
    Open { status: c_int, message: String },
    #[error("read-only SQLite image length does not fit i64: {actual} bytes")]
    ImageLength { actual: usize },
    #[error("deserialize stable database image as read-only SQLite failed ({status}): {message}")]
    Deserialize { status: c_int, message: String },
    #[error("`{database}` database image is corrupt or is not SQLite")]
    CorruptImage {
        database: &'static str,
        #[source]
        source: Box<ReadOnlyError>,
    },
    #[error("deserialized SQLite main image accepted a write probe (sqlite3_db_readonly returned {observed})")]
    WritableHandle { observed: c_int },
    #[error(
        "prove deserialized SQLite image is read-only failed (sqlite3_db_readonly={reported}, status={status}): {message}"
    )]
    ReadOnlyProbe {
        reported: c_int,
        status: c_int,
        message: String,
    },
    #[error("bundled SQLite reports conflicting SQLITE_TEMP_STORE compile modes")]
    TempStoreCompileMode,
    #[error("configure or inspect SQLite TEMP_STORE failed ({status}): {message}")]
    TempStore { status: c_int, message: String },
    #[error("SQLite TEMP_STORE is not memory-only (compile mode {compile_mode}, runtime PRAGMA mode {runtime_mode})")]
    TempStoreMode { compile_mode: c_int, runtime_mode: c_int },
    #[error("read-only SQLite connection mutex is poisoned")]
    LockPoisoned,
    #[error("read-only SQLite connection was permanently poisoned by ambiguous transaction state")]
    ConnectionPoisoned,
    #[error("SQLite returned a null prepared statement")]
    NullStatement,
    #[error("SQLite prepared a non-read-only statement inside the read-only boundary")]
    WritableStatement,
    #[error("read-only SQLite statement unexpectedly returned a row")]
    UnexpectedRow,
    #[error("SQLite column {column} is null")]
    UnexpectedNull { column: c_int },
    #[error("SQLite column {column} is not the expected {expected}")]
    ColumnType { column: c_int, expected: &'static str },
    #[error("SQLite column {column} has a negative byte length")]
    ColumnLength { column: c_int },
    #[error("SQLite text exceeds the {limit}-byte bound (got {actual})")]
    TextLimit { limit: usize, actual: usize },
    #[error("SQLite text column {column} is not UTF-8")]
    Utf8 {
        column: c_int,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("SQLite boolean column {column} has noncanonical value {value}")]
    Boolean { column: c_int, value: i64 },
    #[error("read-only SQLite policy rejected data: {context}")]
    Policy { context: &'static str },
    #[error("read-only SQLite {resource} exceeds the {limit}-item bound")]
    Limit { resource: &'static str, limit: usize },
    #[error("read-only SQLite query interrupted: {reason}")]
    QueryInterrupted { reason: &'static str },
    #[error("validate exact `{database}` Diesel migration set")]
    MigrationSetValidation {
        database: &'static str,
        #[source]
        source: Box<ReadOnlyError>,
    },
    #[error("`{database}` Diesel migration set mismatch: expected {expected:?}, got {actual:?}")]
    MigrationSetMismatch {
        database: &'static str,
        expected: &'static [&'static str],
        actual: Vec<String>,
    },
    #[error("read-only SQLite transaction-state invariant failed: {context}")]
    TransactionState { context: &'static str },
    #[error("{operation} failed ({status}): {message}")]
    Sqlite {
        operation: &'static str,
        status: c_int,
        message: String,
    },
    #[error("read-only query failed and its read-transaction rollback also failed")]
    Rollback {
        source: Box<ReadOnlyError>,
        rollback: Box<ReadOnlyError>,
    },
    #[error("read-only SQLite transaction cleanup failed; the connection is permanently poisoned")]
    TransactionPoisoned {
        source: Box<ReadOnlyError>,
        cleanup: Option<Box<ReadOnlyError>>,
    },
}

impl ReadOnlyError {
    fn sqlite_status_is_interrupt(&self) -> bool {
        matches!(
            self,
            Self::Sqlite { status, .. } if status & 0xff == sqlite::SQLITE_INTERRUPT
        )
    }

    fn sqlite_status_is_corruption(&self) -> bool {
        let status = match self {
            Self::Deserialize { status, .. } | Self::ReadOnlyProbe { status, .. } | Self::Sqlite { status, .. } => {
                *status & 0xff
            }
            _ => return false,
        };
        matches!(status, sqlite::SQLITE_CORRUPT | sqlite::SQLITE_NOTADB)
    }
}
