//! One-time admission checks for a deserialized read-only SQLite image.

use std::{
    ffi::{CStr, CString, c_int},
    ptr::{self, NonNull},
};

use libsqlite3_sys as sqlite;

use crate::installation::DatabaseKind;

use super::{RawConnection, ReadOnlyError, connection_message};

/// `sqlite3_db_readonly()` reports VFS access and returns zero for a
/// deserialized main database even when SQLITE_DESERIALIZE_READONLY is active.
/// Prove the deserialize flag by reading `main.user_version`, then attempting
/// to store the other bounded value before the authorizer is installed. This
/// cannot collide with an attacker-chosen schema name. A bundled SQLite which
/// accepts it is rejected and the private image/handle are discarded; the
/// retained source file is never writable or attached to this connection.
pub(super) fn require_deserialized_read_only(raw: &RawConnection, kind: DatabaseKind) -> Result<(), ReadOnlyError> {
    let reported = unsafe { sqlite::sqlite3_db_readonly(raw.handle.as_ptr(), c"main".as_ptr()) };
    let current = read_probe_user_version(raw, reported, kind)?;
    let proposed = i64::from(current == 0);
    let sql = CString::new(format!("PRAGMA main.user_version = {proposed}"))
        .expect("bounded user-version probe contains no NUL");
    let mut statement = ptr::null_mut();
    let prepare = unsafe {
        sqlite::sqlite3_prepare_v3(
            raw.handle.as_ptr(),
            sql.as_ptr(),
            -1,
            0,
            &mut statement,
            ptr::null_mut(),
        )
    };
    if prepare != sqlite::SQLITE_OK {
        let source = ReadOnlyError::ReadOnlyProbe {
            reported,
            status: prepare,
            message: connection_message(raw.handle.as_ptr(), prepare),
        };
        return match prepare & 0xff {
            sqlite::SQLITE_READONLY => Ok(()),
            sqlite::SQLITE_CORRUPT | sqlite::SQLITE_NOTADB => Err(ReadOnlyError::CorruptImage {
                database: expected_migrations(kind).0,
                source: Box::new(source),
            }),
            _ => Err(source),
        };
    }

    let statement = NonNull::new(statement).ok_or(ReadOnlyError::NullStatement)?;
    let status = unsafe { sqlite::sqlite3_step(statement.as_ptr()) };
    let message = connection_message(raw.handle.as_ptr(), status);
    unsafe {
        let _ = sqlite::sqlite3_finalize(statement.as_ptr());
    }
    if unsafe { sqlite::sqlite3_get_autocommit(raw.handle.as_ptr()) } == 0 {
        return Err(ReadOnlyError::TransactionState {
            context: "deserialized read-only write probe left a transaction active",
        });
    }
    match status & 0xff {
        sqlite::SQLITE_READONLY => Ok(()),
        sqlite::SQLITE_CORRUPT | sqlite::SQLITE_NOTADB => Err(ReadOnlyError::CorruptImage {
            database: expected_migrations(kind).0,
            source: Box::new(ReadOnlyError::ReadOnlyProbe {
                reported,
                status,
                message,
            }),
        }),
        sqlite::SQLITE_DONE => Err(ReadOnlyError::WritableHandle { observed: reported }),
        _ => Err(ReadOnlyError::ReadOnlyProbe {
            reported,
            status,
            message,
        }),
    }
}

fn read_probe_user_version(raw: &RawConnection, reported: c_int, kind: DatabaseKind) -> Result<i64, ReadOnlyError> {
    let mut statement = ptr::null_mut();
    let prepare = unsafe {
        sqlite::sqlite3_prepare_v3(
            raw.handle.as_ptr(),
            c"PRAGMA main.user_version".as_ptr(),
            -1,
            0,
            &mut statement,
            ptr::null_mut(),
        )
    };
    if prepare != sqlite::SQLITE_OK {
        return Err(read_only_probe_error(raw, kind, reported, prepare));
    }
    let statement = NonNull::new(statement).ok_or(ReadOnlyError::NullStatement)?;
    let first = unsafe { sqlite::sqlite3_step(statement.as_ptr()) };
    if first != sqlite::SQLITE_ROW {
        let source = read_only_probe_error(raw, kind, reported, first);
        unsafe {
            let _ = sqlite::sqlite3_finalize(statement.as_ptr());
        }
        return Err(source);
    }
    if unsafe { sqlite::sqlite3_column_type(statement.as_ptr(), 0) } != sqlite::SQLITE_INTEGER {
        unsafe {
            let _ = sqlite::sqlite3_finalize(statement.as_ptr());
        }
        return Err(ReadOnlyError::Policy {
            context: "SQLite user-version probe returned a non-integer",
        });
    }
    let value = unsafe { sqlite::sqlite3_column_int64(statement.as_ptr(), 0) };
    let second = unsafe { sqlite::sqlite3_step(statement.as_ptr()) };
    unsafe {
        let _ = sqlite::sqlite3_finalize(statement.as_ptr());
    }
    if second != sqlite::SQLITE_DONE {
        return Err(read_only_probe_error(raw, kind, reported, second));
    }
    Ok(value)
}

fn read_only_probe_error(raw: &RawConnection, kind: DatabaseKind, reported: c_int, status: c_int) -> ReadOnlyError {
    let source = ReadOnlyError::ReadOnlyProbe {
        reported,
        status,
        message: connection_message(raw.handle.as_ptr(), status),
    };
    if matches!(status & 0xff, sqlite::SQLITE_CORRUPT | sqlite::SQLITE_NOTADB) {
        ReadOnlyError::CorruptImage {
            database: expected_migrations(kind).0,
            source: Box::new(source),
        }
    } else {
        source
    }
}

pub(super) fn configure_memory_temp_store(raw: &RawConnection) -> Result<(c_int, c_int), ReadOnlyError> {
    let compile_mode = compiled_temp_store_mode()?;
    execute_temp_store_pragma(raw, c"PRAGMA temp_store=MEMORY")?;
    let runtime_mode = query_temp_store_mode(raw)?;
    if runtime_mode != 2 {
        return Err(ReadOnlyError::TempStoreMode {
            compile_mode,
            runtime_mode,
        });
    }
    Ok((compile_mode, runtime_mode))
}

fn compiled_temp_store_mode() -> Result<c_int, ReadOnlyError> {
    let mut selected = None;
    for mode in 0..=3 {
        let option = CString::new(format!("TEMP_STORE={mode}")).expect("fixed compile option contains no NUL");
        if unsafe { sqlite::sqlite3_compileoption_used(option.as_ptr()) } != 0 && selected.replace(mode).is_some() {
            return Err(ReadOnlyError::TempStoreCompileMode);
        }
    }
    // SQLite's documented default when no explicit compile option is emitted.
    Ok(selected.unwrap_or(1))
}

fn execute_temp_store_pragma(raw: &RawConnection, sql: &CStr) -> Result<(), ReadOnlyError> {
    let mut statement = ptr::null_mut();
    let prepare = unsafe {
        sqlite::sqlite3_prepare_v3(
            raw.handle.as_ptr(),
            sql.as_ptr(),
            -1,
            0,
            &mut statement,
            ptr::null_mut(),
        )
    };
    if prepare != sqlite::SQLITE_OK {
        return Err(temp_store_error(raw, prepare));
    }
    let statement = NonNull::new(statement).ok_or(ReadOnlyError::NullStatement)?;
    let status = unsafe { sqlite::sqlite3_step(statement.as_ptr()) };
    unsafe {
        let _ = sqlite::sqlite3_finalize(statement.as_ptr());
    }
    if status == sqlite::SQLITE_DONE {
        Ok(())
    } else {
        Err(temp_store_error(raw, status))
    }
}

fn query_temp_store_mode(raw: &RawConnection) -> Result<c_int, ReadOnlyError> {
    let mut statement = ptr::null_mut();
    let prepare = unsafe {
        sqlite::sqlite3_prepare_v3(
            raw.handle.as_ptr(),
            c"PRAGMA temp_store".as_ptr(),
            -1,
            0,
            &mut statement,
            ptr::null_mut(),
        )
    };
    if prepare != sqlite::SQLITE_OK {
        return Err(temp_store_error(raw, prepare));
    }
    let statement = NonNull::new(statement).ok_or(ReadOnlyError::NullStatement)?;
    let first = unsafe { sqlite::sqlite3_step(statement.as_ptr()) };
    if first != sqlite::SQLITE_ROW
        || unsafe { sqlite::sqlite3_column_type(statement.as_ptr(), 0) } != sqlite::SQLITE_INTEGER
    {
        unsafe {
            let _ = sqlite::sqlite3_finalize(statement.as_ptr());
        }
        return Err(temp_store_error(raw, first));
    }
    let mode = unsafe { sqlite::sqlite3_column_int(statement.as_ptr(), 0) };
    let second = unsafe { sqlite::sqlite3_step(statement.as_ptr()) };
    unsafe {
        let _ = sqlite::sqlite3_finalize(statement.as_ptr());
    }
    if second != sqlite::SQLITE_DONE {
        return Err(temp_store_error(raw, second));
    }
    Ok(mode)
}

fn temp_store_error(raw: &RawConnection, status: c_int) -> ReadOnlyError {
    ReadOnlyError::TempStore {
        status,
        message: connection_message(raw.handle.as_ptr(), status),
    }
}

pub(super) fn expected_migrations(kind: DatabaseKind) -> (&'static str, &'static [&'static str]) {
    match kind {
        DatabaseKind::Install => ("install", &["20240303165811", "20260714000000"]),
        DatabaseKind::State => (
            "state",
            &["20240304201550", "20260714000000", "20260716000000", "20260718000000"],
        ),
        DatabaseKind::Layout => ("layout", &["20240304192634", "20260714120000"]),
    }
}
