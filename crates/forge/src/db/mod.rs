use std::{
    collections::TryReserveError,
    fmt,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use diesel::SqliteConnection;
use thiserror::Error;

pub mod layout;
pub mod meta;
#[allow(dead_code)] // completed substrate; consumed by the next read-only-client slice
mod read_only;
pub mod state;

#[allow(unused_imports)] // deliberate internal surface for the next read-only-client slice
pub(crate) use read_only::{ReadOnlyConnection, ReadOnlyError, ReadOnlyRow, Step as ReadOnlyStep};

/// Max number of variables (binds) for a prepared statement
///
/// https://www.sqlite.org/limits.html#max_variable_number
const MAX_VARIABLE_NUMBER: usize = 32766;

#[derive(Clone)]
struct Connection(Arc<Mutex<SqliteConnection>>);

impl Connection {
    fn new(connection: SqliteConnection) -> Self {
        Self(Arc::new(Mutex::new(connection)))
    }

    fn exec<T>(&self, f: impl FnOnce(&mut SqliteConnection) -> T) -> T {
        let mut _guard = self.0.lock().expect("mutex guard");
        f(&mut _guard)
    }

    fn exclusive_tx<T, E>(&self, f: impl FnOnce(&mut SqliteConnection) -> Result<T, E>) -> Result<T, E>
    where
        E: From<diesel::result::Error>,
    {
        let mut _guard = self.0.lock().expect("mutex guard");
        _guard.exclusive_transaction(|tx| f(tx))
    }
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection").finish()
    }
}

pub struct Timestamp(pub DateTime<Utc>);

impl TryFrom<i64> for Timestamp {
    type Error = Error;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Ok(Self(
            DateTime::<Utc>::from_timestamp(value, 0).ok_or(Error::InvalidTimestamp(value))?,
        ))
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Row not found")]
    RowNotFound,
    #[error("failed to decode layout entry")]
    LayoutEntryDecode,
    #[error("invalid timestamp: {0}")]
    InvalidTimestamp(i64),
    #[error("duplicate package identifier in metadata batch")]
    DuplicatePackageId,
    #[error("reserve package identities for metadata batch validation")]
    ReservePackageIds(#[source] TryReserveError),
    #[error("metadata field `{field}` value {value} is outside the SQLite storage range")]
    MetaIntegerOutOfRange { field: &'static str, value: u64 },
    #[error("active repository snapshot index URI exceeds {limit} bytes (got {actual})")]
    SnapshotIndexUriTooLong { limit: usize, actual: usize },
    #[error("active repository snapshot index URI is invalid: {reason}")]
    SnapshotIndexUriPolicy { reason: &'static str },
    #[error("parse active repository snapshot index URI")]
    ParseSnapshotIndexUri(#[source] url::ParseError),
    #[error("active repository snapshot SHA-256 must be exactly 64 lowercase ASCII hexadecimal characters")]
    InvalidSnapshotSha256,
    #[error("active repository snapshot byte size exceeds {limit} bytes (got {actual})")]
    SnapshotByteSizeOutOfRange { limit: u64, actual: u64 },
    #[error("stored active repository snapshot byte size is negative: {0}")]
    NegativeSnapshotByteSize(i64),
    #[error("stored active repository snapshot has invalid singleton key {0}")]
    InvalidSnapshotSingleton(i32),
    #[error("diesel")]
    Diesel(#[from] diesel::result::Error),
    #[error("diesel connection")]
    Connection(#[from] diesel::ConnectionError),
    #[error("diesel migration")]
    Migration(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}
