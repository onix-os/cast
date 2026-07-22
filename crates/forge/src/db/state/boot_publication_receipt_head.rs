//! Durable state-database correlation for boot-publication receipts.
//!
//! This singleton records which complete receipt is currently committed and,
//! while publication is in flight, the exact transition and receipt expected
//! to replace it. Its mutation surface is limited to conditional staging and
//! pending-to-committed head updates; canonical-body admission and promotion
//! policy remain in the receipt-state layer. It grants no repair, deletion,
//! journal, filesystem, or publication authority.

use diesel::{
    SqliteConnection,
    dsl::sql,
    prelude::*,
    sql_types::{BigInt, Nullable, Text},
};

use super::{Database, Error, schema::boot_publication_receipt_head};
use crate::{
    boot_publication::{
        BootPublicationReceiptFingerprint, BootPublicationReceiptFingerprintError, BootPublicationReceiptPair,
    },
    state::{TransitionId, TransitionIdError},
};

const RECEIPT_HEAD_SINGLETON: i32 = 1;
const RECEIPT_HEAD_SCAN_LIMIT: i64 = 2;

/// Exact durable state of the boot-publication receipt head.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BootPublicationReceiptHead {
    committed: Option<BootPublicationReceiptFingerprint>,
    pending: Option<PendingBootPublicationReceipt>,
}

impl BootPublicationReceiptHead {
    pub(crate) fn committed(&self) -> Option<BootPublicationReceiptFingerprint> {
        self.committed
    }

    pub(crate) fn pending(&self) -> Option<&PendingBootPublicationReceipt> {
        self.pending.as_ref()
    }

    /// Return the compact pair stored in the journal while publication is in
    /// flight. No pending publication means there is no pair to correlate.
    pub(crate) fn receipt_pair(&self) -> Option<BootPublicationReceiptPair> {
        self.pending.as_ref().map(|pending| BootPublicationReceiptPair {
            committed: self.committed,
            pending: pending.fingerprint,
        })
    }

    /// Return the compact pair only when it belongs to the exact transition
    /// being authenticated by the caller.
    pub(crate) fn receipt_pair_for(&self, transition_id: &TransitionId) -> Option<BootPublicationReceiptPair> {
        self.pending
            .as_ref()
            .filter(|pending| pending.transition_id == *transition_id)
            .map(|pending| BootPublicationReceiptPair {
                committed: self.committed,
                pending: pending.fingerprint,
            })
    }
}

/// Exact transition correlation for one staged boot-publication receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingBootPublicationReceipt {
    transition_id: TransitionId,
    fingerprint: BootPublicationReceiptFingerprint,
}

impl PendingBootPublicationReceipt {
    pub(crate) fn transition_id(&self) -> &TransitionId {
        &self.transition_id
    }

    pub(crate) fn fingerprint(&self) -> BootPublicationReceiptFingerprint {
        self.fingerprint
    }
}

/// Whether staging wrote the singleton or proved an exact prior write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootPublicationReceiptStageOutcome {
    Staged,
    AlreadyStaged,
}

impl Database {
    /// Inspect exactly one receipt-head row using a two-row bounded audit.
    ///
    /// Missing, duplicate, malformed, partially-null, and dynamically mistyped
    /// storage all fail closed. This method never synthesizes a legacy head.
    pub(crate) fn boot_publication_receipt_head(
        &self,
    ) -> Result<BootPublicationReceiptHead, BootPublicationReceiptHeadError> {
        self.conn.exec(|conn| conn.transaction(load_receipt_head))
    }

    /// Stage one exact pending pair under its transition correlation.
    ///
    /// The durable committed fingerprint must equal the caller's expected
    /// committed fingerprint. An empty pending slot is written once; an exact
    /// transition/fingerprint retry is reported without mutation. Every other
    /// existing pending value is a conflict, including reuse of the same
    /// transition ID with different receipt bytes.
    #[cfg(test)]
    pub(crate) fn stage_boot_publication_receipt_pair(
        &self,
        transition_id: &TransitionId,
        pair: &BootPublicationReceiptPair,
    ) -> Result<BootPublicationReceiptStageOutcome, BootPublicationReceiptHeadError> {
        self.conn.exclusive_tx(|tx| {
            let head = load_receipt_head(tx)?;
            if head.committed != pair.committed {
                return Err(BootPublicationReceiptHeadError::CommittedMismatch {
                    expected: pair.committed,
                    actual: head.committed,
                });
            }

            if let Some(existing) = head.pending {
                if existing.transition_id == *transition_id && existing.fingerprint == pair.pending {
                    return Ok(BootPublicationReceiptStageOutcome::AlreadyStaged);
                }
                return Err(BootPublicationReceiptHeadError::PendingConflict {
                    existing_transition_id: existing.transition_id,
                    existing_fingerprint: existing.fingerprint,
                    requested_transition_id: transition_id.clone(),
                    requested_fingerprint: pair.pending,
                });
            }

            let changed = stage_pending_row(tx, transition_id, pair)?;
            if changed != 1 {
                return Err(BootPublicationReceiptHeadError::StageRowMismatch { changed });
            }

            let staged = load_receipt_head(tx)?;
            let exact_pending = staged
                .pending()
                .is_some_and(|pending| pending.transition_id() == transition_id);
            if staged.receipt_pair() != Some(*pair) || !exact_pending {
                return Err(BootPublicationReceiptHeadError::StageRevalidationMismatch);
            }
            Ok(BootPublicationReceiptStageOutcome::Staged)
        })
    }

    /// Replace the singleton with typed test evidence without bypassing its
    /// database constraints.
    #[cfg(test)]
    pub(crate) fn replace_boot_publication_receipt_head_for_test(
        &self,
        committed: Option<BootPublicationReceiptFingerprint>,
        pending: Option<(&TransitionId, BootPublicationReceiptFingerprint)>,
    ) -> Result<(), Error> {
        self.conn.exclusive_tx(|tx| {
            let pending_transition_id = pending.map(|(transition, _)| transition.as_str());
            let pending_receipt = pending.map(|(_, fingerprint)| fingerprint);
            let changed = diesel::update(
                boot_publication_receipt_head::table
                    .filter(boot_publication_receipt_head::singleton.eq(RECEIPT_HEAD_SINGLETON)),
            )
            .set((
                boot_publication_receipt_head::committed_receipt_sha256
                    .eq(committed.as_ref().map(|fingerprint| fingerprint.as_bytes().as_slice())),
                boot_publication_receipt_head::pending_transition_id.eq(pending_transition_id),
                boot_publication_receipt_head::pending_receipt_sha256
                    .eq(pending_receipt.as_ref().map(|fingerprint| fingerprint.as_bytes().as_slice())),
            ))
            .execute(tx)?;
            require_single_test_update(changed)
        })
    }

    #[cfg(test)]
    pub(crate) fn clear_boot_publication_receipt_head_for_test(&self) -> Result<(), Error> {
        self.replace_boot_publication_receipt_head_for_test(None, None)
    }

    /// Replace payload columns while bypassing CHECK constraints so recovery
    /// tests can prove that independently corrupted durable evidence fails
    /// closed at both database seams.
    #[cfg(test)]
    pub(crate) fn replace_boot_publication_receipt_head_raw_for_test(
        &self,
        raw: &BootPublicationReceiptHeadRawForTest,
    ) -> Result<(), Error> {
        self.conn.exec(|conn| {
            diesel::sql_query("PRAGMA ignore_check_constraints = ON").execute(conn)?;
            let update = diesel::update(
                boot_publication_receipt_head::table
                    .filter(boot_publication_receipt_head::singleton.eq(RECEIPT_HEAD_SINGLETON)),
            )
            .set((
                boot_publication_receipt_head::committed_receipt_sha256
                    .eq(raw.committed_receipt_sha256.as_deref()),
                boot_publication_receipt_head::pending_transition_id.eq(raw.pending_transition_id.as_deref()),
                boot_publication_receipt_head::pending_receipt_sha256.eq(raw.pending_receipt_sha256.as_deref()),
            ))
            .execute(conn);
            let restore = diesel::sql_query("PRAGMA ignore_check_constraints = OFF").execute(conn);
            let changed = update?;
            restore?;
            require_single_test_update(changed)
        })
    }

    #[cfg(test)]
    pub(crate) fn delete_boot_publication_receipt_head_for_test(&self) -> Result<(), Error> {
        self.conn.exclusive_tx(|tx| {
            let changed = diesel::delete(
                boot_publication_receipt_head::table
                    .filter(boot_publication_receipt_head::singleton.eq(RECEIPT_HEAD_SINGLETON)),
            )
            .execute(tx)?;
            require_single_test_update(changed)
        })
    }
}

pub(super) fn load_receipt_head(
    connection: &mut SqliteConnection,
) -> Result<BootPublicationReceiptHead, BootPublicationReceiptHeadError> {
    let mut shapes = boot_publication_receipt_head::table
        .select((
            boot_publication_receipt_head::singleton,
            sql::<Text>("typeof(committed_receipt_sha256)"),
            sql::<Nullable<BigInt>>("length(committed_receipt_sha256)"),
            sql::<Text>("typeof(pending_transition_id)"),
            sql::<Nullable<BigInt>>("length(CAST(pending_transition_id AS BLOB))"),
            sql::<Text>("typeof(pending_receipt_sha256)"),
            sql::<Nullable<BigInt>>("length(pending_receipt_sha256)"),
        ))
        .order(boot_publication_receipt_head::singleton.asc())
        .limit(RECEIPT_HEAD_SCAN_LIMIT)
        .load::<StoredBootPublicationReceiptHeadShape>(connection)
        .map_err(Error::from)?;

    let shape = match shapes.len() {
        0 => return Err(BootPublicationReceiptHeadError::MissingSingleton),
        1 => shapes.pop().expect("one bounded receipt-head shape"),
        _ => return Err(BootPublicationReceiptHeadError::MultipleRows),
    };
    validate_receipt_head_shape(&shape)?;

    let stored = boot_publication_receipt_head::table
        .filter(boot_publication_receipt_head::singleton.eq(RECEIPT_HEAD_SINGLETON))
        .select(StoredBootPublicationReceiptHead::as_select())
        .first::<StoredBootPublicationReceiptHead>(connection)
        .map_err(Error::from)?;
    decode_receipt_head(stored)
}

fn validate_receipt_head_shape(
    shape: &StoredBootPublicationReceiptHeadShape,
) -> Result<(), BootPublicationReceiptHeadError> {
    if shape.singleton != RECEIPT_HEAD_SINGLETON {
        return Err(BootPublicationReceiptHeadError::InvalidSingleton {
            actual: shape.singleton,
        });
    }
    validate_nullable_storage(
        "committed_receipt_sha256",
        "blob",
        shape.committed_storage.as_str(),
        shape.committed_bytes,
        32,
    )?;
    let pending_transition_present = validate_nullable_storage(
        "pending_transition_id",
        "text",
        shape.pending_transition_storage.as_str(),
        shape.pending_transition_bytes,
        i64::try_from(TransitionId::TEXT_LENGTH).expect("transition-ID length fits i64"),
    )?;
    let pending_receipt_present = validate_nullable_storage(
        "pending_receipt_sha256",
        "blob",
        shape.pending_receipt_storage.as_str(),
        shape.pending_receipt_bytes,
        32,
    )?;
    if pending_transition_present != pending_receipt_present {
        return Err(BootPublicationReceiptHeadError::IncompletePendingPair {
            transition_present: pending_transition_present,
            receipt_present: pending_receipt_present,
        });
    }
    Ok(())
}

fn validate_nullable_storage(
    field: &'static str,
    present_storage: &'static str,
    actual_storage: &str,
    actual_bytes: Option<i64>,
    expected_bytes: i64,
) -> Result<bool, BootPublicationReceiptHeadError> {
    match (actual_storage, actual_bytes) {
        ("null", None) => Ok(false),
        (storage, Some(actual)) if storage == present_storage && actual == expected_bytes => Ok(true),
        (storage, _) if storage != present_storage => Err(BootPublicationReceiptHeadError::InvalidStorageType {
            field,
            expected: present_storage,
            actual: storage.to_owned(),
        }),
        (_, actual) => Err(BootPublicationReceiptHeadError::InvalidStoredFieldLength {
            field,
            expected: expected_bytes,
            actual,
        }),
    }
}

fn decode_receipt_head(
    stored: StoredBootPublicationReceiptHead,
) -> Result<BootPublicationReceiptHead, BootPublicationReceiptHeadError> {
    if stored.singleton != RECEIPT_HEAD_SINGLETON {
        return Err(BootPublicationReceiptHeadError::InvalidSingleton {
            actual: stored.singleton,
        });
    }

    let committed = stored
        .committed_receipt_sha256
        .map(|bytes| decode_fingerprint("committed_receipt_sha256", bytes))
        .transpose()?;
    let pending = match (stored.pending_transition_id, stored.pending_receipt_sha256) {
        (None, None) => None,
        (Some(transition_id), Some(receipt)) => Some(PendingBootPublicationReceipt {
            transition_id: TransitionId::parse(transition_id)
                .map_err(|source| BootPublicationReceiptHeadError::InvalidPendingTransitionId { source })?,
            fingerprint: decode_fingerprint("pending_receipt_sha256", receipt)?,
        }),
        (transition_id, receipt) => {
            return Err(BootPublicationReceiptHeadError::IncompletePendingPair {
                transition_present: transition_id.is_some(),
                receipt_present: receipt.is_some(),
            });
        }
    };

    Ok(BootPublicationReceiptHead { committed, pending })
}

fn decode_fingerprint(
    field: &'static str,
    bytes: Vec<u8>,
) -> Result<BootPublicationReceiptFingerprint, BootPublicationReceiptHeadError> {
    BootPublicationReceiptFingerprint::from_slice(&bytes)
        .map_err(|source| BootPublicationReceiptHeadError::InvalidStoredFingerprint { field, source })
}

pub(super) fn stage_pending_row(
    connection: &mut SqliteConnection,
    transition_id: &TransitionId,
    pair: &BootPublicationReceiptPair,
) -> Result<usize, Error> {
    let base = boot_publication_receipt_head::table
        .filter(boot_publication_receipt_head::singleton.eq(RECEIPT_HEAD_SINGLETON))
        .filter(boot_publication_receipt_head::pending_transition_id.is_null())
        .filter(boot_publication_receipt_head::pending_receipt_sha256.is_null());
    let values = (
        boot_publication_receipt_head::pending_transition_id.eq(Some(transition_id.as_str())),
        boot_publication_receipt_head::pending_receipt_sha256.eq(Some(pair.pending.as_bytes().as_slice())),
    );
    match pair.committed {
        Some(committed) => diesel::update(
            base.filter(
                boot_publication_receipt_head::committed_receipt_sha256.eq(committed.as_bytes().as_slice()),
            ),
        )
        .set(values)
        .execute(connection)
        .map_err(Error::from),
        None => diesel::update(base.filter(boot_publication_receipt_head::committed_receipt_sha256.is_null()))
            .set(values)
            .execute(connection)
            .map_err(Error::from),
    }
}

/// Conditionally promote one exact pending pair and clear its transition slot.
///
/// Canonical body admission belongs to the receipt-state layer. This narrow
/// helper changes only the singleton head and succeeds only while every scalar
/// in the caller's already-admitted preimage is still exact.
#[allow(dead_code)] // DB-only substrate; consumed by the aggregate coordination slice
pub(super) fn promote_pending_row(
    connection: &mut SqliteConnection,
    transition_id: &TransitionId,
    pair: &BootPublicationReceiptPair,
) -> Result<usize, Error> {
    let base = boot_publication_receipt_head::table
        .filter(boot_publication_receipt_head::singleton.eq(RECEIPT_HEAD_SINGLETON))
        .filter(
            boot_publication_receipt_head::pending_transition_id
                .eq(Some(transition_id.as_str())),
        )
        .filter(
            boot_publication_receipt_head::pending_receipt_sha256
                .eq(Some(pair.pending.as_bytes().as_slice())),
        );
    let promoted = (
        boot_publication_receipt_head::committed_receipt_sha256
            .eq(Some(pair.pending.as_bytes().as_slice())),
        boot_publication_receipt_head::pending_transition_id.eq(None::<&str>),
        boot_publication_receipt_head::pending_receipt_sha256
            .eq(None::<&[u8]>),
    );
    match pair.committed {
        Some(committed) => diesel::update(
            base.filter(
                boot_publication_receipt_head::committed_receipt_sha256
                    .eq(committed.as_bytes().as_slice()),
            ),
        )
        .set(promoted)
        .execute(connection)
        .map_err(Error::from),
        None => diesel::update(
            base.filter(
                boot_publication_receipt_head::committed_receipt_sha256.is_null(),
            ),
        )
        .set(promoted)
        .execute(connection)
        .map_err(Error::from),
    }
}

/// Conditionally retire one exact committed fingerprint while preserving the
/// singleton and every immutable receipt body.
pub(super) fn retire_committed_row(
    connection: &mut SqliteConnection,
    fingerprint: BootPublicationReceiptFingerprint,
) -> Result<usize, Error> {
    diesel::update(
        boot_publication_receipt_head::table
            .filter(boot_publication_receipt_head::singleton.eq(RECEIPT_HEAD_SINGLETON))
            .filter(
                boot_publication_receipt_head::committed_receipt_sha256
                    .eq(fingerprint.as_bytes().as_slice()),
            )
            .filter(boot_publication_receipt_head::pending_transition_id.is_null())
            .filter(boot_publication_receipt_head::pending_receipt_sha256.is_null()),
    )
    .set((
        boot_publication_receipt_head::committed_receipt_sha256.eq(None::<&[u8]>),
        boot_publication_receipt_head::pending_transition_id.eq(None::<&str>),
        boot_publication_receipt_head::pending_receipt_sha256.eq(None::<&[u8]>),
    ))
    .execute(connection)
    .map_err(Error::from)
}

#[derive(Queryable)]
struct StoredBootPublicationReceiptHeadShape {
    singleton: i32,
    committed_storage: String,
    committed_bytes: Option<i64>,
    pending_transition_storage: String,
    pending_transition_bytes: Option<i64>,
    pending_receipt_storage: String,
    pending_receipt_bytes: Option<i64>,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = boot_publication_receipt_head)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct StoredBootPublicationReceiptHead {
    singleton: i32,
    committed_receipt_sha256: Option<Vec<u8>>,
    pending_transition_id: Option<String>,
    pending_receipt_sha256: Option<Vec<u8>>,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BootPublicationReceiptHeadRawForTest {
    pub(crate) committed_receipt_sha256: Option<Vec<u8>>,
    pub(crate) pending_transition_id: Option<String>,
    pub(crate) pending_receipt_sha256: Option<Vec<u8>>,
}

#[cfg(test)]
fn require_single_test_update(changed: usize) -> Result<(), Error> {
    if changed == 1 {
        Ok(())
    } else {
        Err(Error::RowNotFound)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BootPublicationReceiptHeadError {
    #[error("state database has no boot-publication receipt-head singleton")]
    MissingSingleton,
    #[error("state database has multiple boot-publication receipt-head rows")]
    MultipleRows,
    #[error("boot-publication receipt head has singleton key {actual} instead of 1")]
    InvalidSingleton { actual: i32 },
    #[error("boot-publication receipt head has only one member of its pending pair")]
    IncompletePendingPair {
        transition_present: bool,
        receipt_present: bool,
    },
    #[error("boot-publication receipt head contains a noncanonical pending transition ID")]
    InvalidPendingTransitionId {
        #[source]
        source: TransitionIdError,
    },
    #[error("boot-publication receipt head contains an invalid {field}")]
    InvalidStoredFingerprint {
        field: &'static str,
        #[source]
        source: BootPublicationReceiptFingerprintError,
    },
    #[error("boot-publication receipt head stores {field} as {actual} instead of {expected} or null")]
    InvalidStorageType {
        field: &'static str,
        expected: &'static str,
        actual: String,
    },
    #[error("boot-publication receipt head stores {field} with {actual:?} bytes instead of {expected} or null")]
    InvalidStoredFieldLength {
        field: &'static str,
        expected: i64,
        actual: Option<i64>,
    },
    #[error("boot-publication committed receipt differs from the staged pair expectation")]
    CommittedMismatch {
        expected: Option<BootPublicationReceiptFingerprint>,
        actual: Option<BootPublicationReceiptFingerprint>,
    },
    #[error("boot-publication receipt head already has a different pending transition or receipt")]
    PendingConflict {
        existing_transition_id: TransitionId,
        existing_fingerprint: BootPublicationReceiptFingerprint,
        requested_transition_id: TransitionId,
        requested_fingerprint: BootPublicationReceiptFingerprint,
    },
    #[error("boot-publication receipt-head stage changed {changed} rows instead of exactly one")]
    StageRowMismatch { changed: usize },
    #[error("boot-publication receipt-head stage did not revalidate to the exact requested pair")]
    StageRevalidationMismatch,
    #[error(transparent)]
    Database(#[from] Error),
}

impl From<diesel::result::Error> for BootPublicationReceiptHeadError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database(Error::from(source))
    }
}

#[cfg(test)]
mod tests;
