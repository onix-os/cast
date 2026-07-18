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
         WHERE package_id IN ({placeholders}) ORDER BY package_id, id LIMIT ?"
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
