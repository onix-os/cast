//! Immutable canonical boot-publication receipt storage.
//!
//! The state database stores inert receipt bodies and correlates exactly one
//! pending body through the receipt-head singleton. Loading validates every
//! body referenced by that head. Staging inserts the immutable body and moves
//! the empty pending slot in one exclusive SQLite transaction. Promotion may
//! move only one exact pending head to committed while retaining every body.
//! This module exposes no journal, filesystem, publication, replacement,
//! deletion, cleanup, or garbage-collection authority.

use diesel::{
    SqliteConnection,
    dsl::sql,
    prelude::*,
    sql_types::{BigInt, Text},
};

use super::{
    Database, Error,
    boot_publication_receipt_head::{
        BootPublicationReceiptHead, BootPublicationReceiptHeadError,
        BootPublicationReceiptStageOutcome, load_receipt_head, stage_pending_row,
    },
    schema::boot_publication_receipts,
};
use crate::{
    boot_publication::{
        BootPublicationReceiptCodecError, BootPublicationReceiptFingerprint,
        BootPublicationReceiptFingerprintError, BootPublicationReceiptPair,
        CanonicalBootPublicationReceipt, MAX_CANONICAL_BOOT_PUBLICATION_RECEIPT_BODY_BYTES,
        decode_boot_publication_receipt,
    },
    state::{TransitionId, TransitionIdError},
};

const RECEIPT_LOOKUP_LIMIT: i64 = 2;

#[allow(dead_code)] // DB-only substrate; consumed by the aggregate coordination slice
#[path = "boot_publication_receipts/promotion.rs"]
mod promotion;
pub(crate) use promotion::{
    BootPublicationReceiptPromotionDurableState,
    BootPublicationReceiptPromotionError,
    BootPublicationReceiptPromotionOutcome,
};

/// One strictly decoded state of the compact head and its referenced bodies.
///
/// These values are authenticated only as self-consistent database data. They
/// carry no live destination observation or mutation/deletion authority.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct BootPublicationReceiptState {
    head: BootPublicationReceiptHead,
    committed: Option<CanonicalBootPublicationReceipt>,
    pending: Option<CanonicalBootPublicationReceipt>,
}

impl BootPublicationReceiptState {
    pub(crate) const fn head(&self) -> &BootPublicationReceiptHead {
        &self.head
    }

    #[allow(dead_code)] // consumed by receipt promotion before coordinator wiring
    pub(crate) const fn committed(&self) -> Option<&CanonicalBootPublicationReceipt> {
        self.committed.as_ref()
    }

    pub(crate) const fn pending(&self) -> Option<&CanonicalBootPublicationReceipt> {
        self.pending.as_ref()
    }

    /// Return the compact pair only for the exact pending transition.
    pub(crate) fn receipt_pair_for(&self, transition_id: &TransitionId) -> Option<BootPublicationReceiptPair> {
        self.head.receipt_pair_for(transition_id)
    }
}

impl Database {
    /// Load the receipt head and every body it references in one SQLite read
    /// transaction. Shape checks run before any canonical body is allocated.
    pub(crate) fn boot_publication_receipt_state(
        &self,
    ) -> Result<BootPublicationReceiptState, BootPublicationReceiptStateError> {
        self.conn.exec(|conn| conn.transaction(load_receipt_state))
    }

    /// Atomically insert one immutable canonical body and stage it as pending.
    ///
    /// The body owns its transition and committed-predecessor correlation.
    /// Exact retry is distinguished without mutation. Any conflicting head,
    /// body, transition, predecessor, or corrupt durable value fails closed.
    pub(crate) fn stage_boot_publication_receipt(
        &self,
        receipt: &CanonicalBootPublicationReceipt,
    ) -> Result<BootPublicationReceiptStageOutcome, BootPublicationReceiptStateError> {
        self.conn.exclusive_tx(|tx| stage_receipt(tx, receipt))
    }
}

fn load_receipt_state(
    connection: &mut SqliteConnection,
) -> Result<BootPublicationReceiptState, BootPublicationReceiptStateError> {
    let head = load_receipt_head(connection)?;
    let committed = head
        .committed()
        .map(|fingerprint| load_required_receipt(connection, ReceiptReference::Committed, fingerprint))
        .transpose()?;
    let pending = head
        .pending()
        .map(|pending| {
            load_required_receipt(
                connection,
                ReceiptReference::Pending,
                pending.fingerprint(),
            )
        })
        .transpose()?;

    if let (Some(pending_head), Some(pending_body)) = (head.pending(), pending.as_ref()) {
        if pending_body.body().transition_id() != pending_head.transition_id() {
            return Err(BootPublicationReceiptStateError::PendingTransitionMismatch {
                head: pending_head.transition_id().clone(),
                body: pending_body.body().transition_id().clone(),
            });
        }
        if pending_body.body().committed_predecessor() != head.committed() {
            return Err(BootPublicationReceiptStateError::PendingPredecessorMismatch {
                head: head.committed(),
                body: pending_body.body().committed_predecessor(),
            });
        }
    }

    Ok(BootPublicationReceiptState {
        head,
        committed,
        pending,
    })
}

fn stage_receipt(
    connection: &mut SqliteConnection,
    receipt: &CanonicalBootPublicationReceipt,
) -> Result<BootPublicationReceiptStageOutcome, BootPublicationReceiptStateError> {
    let before = load_receipt_state(connection)?;
    let transition_id = receipt.body().transition_id();
    let pair = BootPublicationReceiptPair {
        committed: receipt.body().committed_predecessor(),
        pending: receipt.fingerprint(),
    };

    if before.head().committed() != pair.committed {
        return Err(BootPublicationReceiptStateError::CommittedMismatch {
            expected: pair.committed,
            actual: before.head().committed(),
        });
    }

    if let Some(existing) = before.head().pending() {
        if existing.transition_id() == transition_id && existing.fingerprint() == pair.pending {
            let existing_body = before
                .pending()
                .expect("a strict pending head retains its canonical body");
            if existing_body.canonical_body() != receipt.canonical_body() {
                return Err(BootPublicationReceiptStateError::ImmutableBodyConflict {
                    fingerprint: pair.pending,
                });
            }
            return Ok(BootPublicationReceiptStageOutcome::AlreadyStaged);
        }
        return Err(BootPublicationReceiptStateError::PendingConflict {
            existing_transition_id: existing.transition_id().clone(),
            existing_fingerprint: existing.fingerprint(),
            requested_transition_id: transition_id.clone(),
            requested_fingerprint: pair.pending,
        });
    }

    if let Some(existing_fingerprint) = load_transition_owner(connection, transition_id)? {
        return Err(BootPublicationReceiptStateError::OrphanTransitionConflict {
            transition_id: transition_id.clone(),
            existing_fingerprint,
            requested_fingerprint: pair.pending,
        });
    }
    match load_receipt(connection, pair.pending)? {
        Some(_) => {
            return Err(BootPublicationReceiptStateError::ImmutableBodyConflict {
                fingerprint: pair.pending,
            });
        }
        None => insert_receipt(connection, receipt)?,
    }

    let persisted = load_required_receipt(connection, ReceiptReference::Pending, pair.pending)?;
    if persisted.canonical_body() != receipt.canonical_body() {
        return Err(BootPublicationReceiptStateError::ImmutableBodyConflict {
            fingerprint: pair.pending,
        });
    }

    let changed = stage_pending_row(connection, transition_id, &pair)?;
    if changed != 1 {
        return Err(BootPublicationReceiptStateError::StageRowMismatch { changed });
    }

    let after = load_receipt_state(connection)?;
    let exact_body = after
        .pending()
        .is_some_and(|stored| stored.canonical_body() == receipt.canonical_body());
    if after.receipt_pair_for(transition_id) != Some(pair) || !exact_body {
        return Err(BootPublicationReceiptStateError::StageRevalidationMismatch);
    }
    Ok(BootPublicationReceiptStageOutcome::Staged)
}

fn insert_receipt(
    connection: &mut SqliteConnection,
    receipt: &CanonicalBootPublicationReceipt,
) -> Result<(), BootPublicationReceiptStateError> {
    let inserted = diesel::insert_into(boot_publication_receipts::table)
        .values(NewBootPublicationReceipt {
            receipt_sha256: receipt.fingerprint().as_bytes(),
            transition_id: receipt.body().transition_id().as_str(),
            canonical_body: receipt.canonical_body(),
        })
        .execute(connection)
        .map_err(Error::from)?;
    if inserted == 1 {
        Ok(())
    } else {
        Err(BootPublicationReceiptStateError::InsertRowMismatch { changed: inserted })
    }
}

fn load_required_receipt(
    connection: &mut SqliteConnection,
    reference: ReceiptReference,
    fingerprint: BootPublicationReceiptFingerprint,
) -> Result<CanonicalBootPublicationReceipt, BootPublicationReceiptStateError> {
    load_receipt(connection, fingerprint)?.ok_or(BootPublicationReceiptStateError::DanglingReference {
        reference,
        fingerprint,
    })
}

fn load_receipt(
    connection: &mut SqliteConnection,
    expected_fingerprint: BootPublicationReceiptFingerprint,
) -> Result<Option<CanonicalBootPublicationReceipt>, BootPublicationReceiptStateError> {
    let key = expected_fingerprint.as_bytes().as_slice();
    let mut shapes = boot_publication_receipts::table
        .filter(boot_publication_receipts::receipt_sha256.eq(key))
        .select((
            sql::<Text>("typeof(receipt_sha256)"),
            sql::<BigInt>("length(receipt_sha256)"),
            sql::<Text>("typeof(transition_id)"),
            sql::<BigInt>("length(CAST(transition_id AS BLOB))"),
            sql::<Text>("typeof(canonical_body)"),
            sql::<BigInt>("length(canonical_body)"),
        ))
        .limit(RECEIPT_LOOKUP_LIMIT)
        .load::<StoredBootPublicationReceiptShape>(connection)
        .map_err(Error::from)?;

    let shape = match shapes.len() {
        0 => return Ok(None),
        1 => shapes.pop().expect("one bounded receipt shape"),
        _ => {
            return Err(BootPublicationReceiptStateError::MultipleBodies {
                fingerprint: expected_fingerprint,
            });
        }
    };
    validate_receipt_shape(&shape)?;

    let stored = boot_publication_receipts::table
        .filter(boot_publication_receipts::receipt_sha256.eq(key))
        .select(StoredBootPublicationReceipt::as_select())
        .first::<StoredBootPublicationReceipt>(connection)
        .map_err(Error::from)?;
    decode_stored_receipt(stored, expected_fingerprint).map(Some)
}

fn load_transition_owner(
    connection: &mut SqliteConnection,
    transition_id: &TransitionId,
) -> Result<Option<BootPublicationReceiptFingerprint>, BootPublicationReceiptStateError> {
    let mut shapes = boot_publication_receipts::table
        .filter(boot_publication_receipts::transition_id.eq(transition_id.as_str()))
        .select((
            sql::<Text>("typeof(receipt_sha256)"),
            sql::<BigInt>("length(receipt_sha256)"),
            sql::<Text>("typeof(transition_id)"),
            sql::<BigInt>("length(CAST(transition_id AS BLOB))"),
            sql::<Text>("typeof(canonical_body)"),
            sql::<BigInt>("length(canonical_body)"),
        ))
        .limit(RECEIPT_LOOKUP_LIMIT)
        .load::<StoredBootPublicationReceiptShape>(connection)
        .map_err(Error::from)?;
    let shape = match shapes.len() {
        0 => return Ok(None),
        1 => shapes.pop().expect("one bounded transition receipt shape"),
        _ => {
            return Err(BootPublicationReceiptStateError::MultipleTransitionBodies {
                transition_id: transition_id.clone(),
            });
        }
    };
    validate_receipt_shape(&shape)?;
    let stored = boot_publication_receipts::table
        .filter(boot_publication_receipts::transition_id.eq(transition_id.as_str()))
        .select(boot_publication_receipts::receipt_sha256)
        .first::<Vec<u8>>(connection)
        .map_err(Error::from)?;
    BootPublicationReceiptFingerprint::from_slice(&stored)
        .map(Some)
        .map_err(BootPublicationReceiptStateError::InvalidStoredFingerprint)
}

fn validate_receipt_shape(
    shape: &StoredBootPublicationReceiptShape,
) -> Result<(), BootPublicationReceiptStateError> {
    validate_storage_type("receipt_sha256", "blob", &shape.fingerprint_storage)?;
    validate_exact_length("receipt_sha256", 32, shape.fingerprint_bytes)?;
    validate_storage_type("transition_id", "text", &shape.transition_storage)?;
    validate_exact_length(
        "transition_id",
        i64::try_from(TransitionId::TEXT_LENGTH).expect("transition-ID length fits i64"),
        shape.transition_bytes,
    )?;
    validate_storage_type("canonical_body", "blob", &shape.body_storage)?;
    let max = i64::try_from(MAX_CANONICAL_BOOT_PUBLICATION_RECEIPT_BODY_BYTES)
        .expect("receipt body limit fits i64");
    if !(1..=max).contains(&shape.body_bytes) {
        return Err(BootPublicationReceiptStateError::InvalidStoredBodyLength {
            actual: shape.body_bytes,
            limit: max,
        });
    }
    Ok(())
}

fn validate_storage_type(
    field: &'static str,
    expected: &'static str,
    actual: &str,
) -> Result<(), BootPublicationReceiptStateError> {
    if actual == expected {
        Ok(())
    } else {
        Err(BootPublicationReceiptStateError::InvalidStorageType {
            field,
            expected,
            actual: actual.to_owned(),
        })
    }
}

fn validate_exact_length(
    field: &'static str,
    expected: i64,
    actual: i64,
) -> Result<(), BootPublicationReceiptStateError> {
    if actual == expected {
        Ok(())
    } else {
        Err(BootPublicationReceiptStateError::InvalidStoredFieldLength {
            field,
            expected,
            actual,
        })
    }
}

fn decode_stored_receipt(
    stored: StoredBootPublicationReceipt,
    expected_fingerprint: BootPublicationReceiptFingerprint,
) -> Result<CanonicalBootPublicationReceipt, BootPublicationReceiptStateError> {
    let stored_fingerprint = BootPublicationReceiptFingerprint::from_slice(&stored.receipt_sha256)
        .map_err(BootPublicationReceiptStateError::InvalidStoredFingerprint)?;
    if stored_fingerprint != expected_fingerprint {
        return Err(BootPublicationReceiptStateError::LookupFingerprintMismatch {
            expected: expected_fingerprint,
            actual: stored_fingerprint,
        });
    }
    let stored_transition = TransitionId::parse(stored.transition_id)
        .map_err(BootPublicationReceiptStateError::InvalidStoredTransitionId)?;
    let receipt = decode_boot_publication_receipt(&stored.canonical_body)?;
    if receipt.fingerprint() != stored_fingerprint {
        return Err(BootPublicationReceiptStateError::BodyFingerprintMismatch {
            stored: stored_fingerprint,
            body: receipt.fingerprint(),
        });
    }
    if receipt.body().transition_id() != &stored_transition {
        return Err(BootPublicationReceiptStateError::BodyTransitionMismatch {
            stored: stored_transition,
            body: receipt.body().transition_id().clone(),
        });
    }
    Ok(receipt)
}

#[derive(Insertable)]
#[diesel(table_name = boot_publication_receipts)]
struct NewBootPublicationReceipt<'receipt> {
    receipt_sha256: &'receipt [u8],
    transition_id: &'receipt str,
    canonical_body: &'receipt [u8],
}

#[derive(Queryable)]
struct StoredBootPublicationReceiptShape {
    fingerprint_storage: String,
    fingerprint_bytes: i64,
    transition_storage: String,
    transition_bytes: i64,
    body_storage: String,
    body_bytes: i64,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = boot_publication_receipts)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct StoredBootPublicationReceipt {
    receipt_sha256: Vec<u8>,
    transition_id: String,
    canonical_body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReceiptReference {
    Committed,
    #[allow(dead_code)] // consumed by receipt promotion before coordinator wiring
    CommittedPredecessor,
    Pending,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BootPublicationReceiptStateError {
    #[error("inspect boot-publication receipt head")]
    Head(#[from] BootPublicationReceiptHeadError),
    #[error("boot-publication receipt head has a dangling {reference:?} reference {fingerprint:?}")]
    DanglingReference {
        reference: ReceiptReference,
        fingerprint: BootPublicationReceiptFingerprint,
    },
    #[error("multiple boot-publication bodies have fingerprint {fingerprint:?}")]
    MultipleBodies {
        fingerprint: BootPublicationReceiptFingerprint,
    },
    #[error("multiple boot-publication bodies claim transition {transition_id}")]
    MultipleTransitionBodies { transition_id: TransitionId },
    #[error("boot-publication receipt stores {field} as {actual} instead of {expected}")]
    InvalidStorageType {
        field: &'static str,
        expected: &'static str,
        actual: String,
    },
    #[error("boot-publication receipt stores {field} with {actual} bytes instead of {expected}")]
    InvalidStoredFieldLength {
        field: &'static str,
        expected: i64,
        actual: i64,
    },
    #[error("boot-publication receipt stores a canonical body with {actual} bytes outside 1..={limit}")]
    InvalidStoredBodyLength { actual: i64, limit: i64 },
    #[error("boot-publication receipt contains an invalid stored fingerprint")]
    InvalidStoredFingerprint(#[source] BootPublicationReceiptFingerprintError),
    #[error("boot-publication receipt contains a noncanonical stored transition ID")]
    InvalidStoredTransitionId(#[source] TransitionIdError),
    #[error("boot-publication receipt lookup returned fingerprint {actual:?} instead of {expected:?}")]
    LookupFingerprintMismatch {
        expected: BootPublicationReceiptFingerprint,
        actual: BootPublicationReceiptFingerprint,
    },
    #[error("decode strict canonical boot-publication receipt body")]
    Codec(#[from] BootPublicationReceiptCodecError),
    #[error("boot-publication receipt body fingerprint {body:?} differs from stored key {stored:?}")]
    BodyFingerprintMismatch {
        stored: BootPublicationReceiptFingerprint,
        body: BootPublicationReceiptFingerprint,
    },
    #[error("boot-publication receipt body transition {body} differs from stored transition {stored}")]
    BodyTransitionMismatch {
        stored: TransitionId,
        body: TransitionId,
    },
    #[error("pending boot-publication receipt body transition {body} differs from head transition {head}")]
    PendingTransitionMismatch { head: TransitionId, body: TransitionId },
    #[error("pending boot-publication receipt predecessor {body:?} differs from head committed value {head:?}")]
    PendingPredecessorMismatch {
        head: Option<BootPublicationReceiptFingerprint>,
        body: Option<BootPublicationReceiptFingerprint>,
    },
    #[error("boot-publication committed receipt differs from the body predecessor")]
    CommittedMismatch {
        expected: Option<BootPublicationReceiptFingerprint>,
        actual: Option<BootPublicationReceiptFingerprint>,
    },
    #[error("boot-publication receipt head already has a different pending transition or body")]
    PendingConflict {
        existing_transition_id: TransitionId,
        existing_fingerprint: BootPublicationReceiptFingerprint,
        requested_transition_id: TransitionId,
        requested_fingerprint: BootPublicationReceiptFingerprint,
    },
    #[error("immutable boot-publication body conflicts at fingerprint {fingerprint:?}")]
    ImmutableBodyConflict {
        fingerprint: BootPublicationReceiptFingerprint,
    },
    #[error(
        "unreferenced boot-publication body for transition {transition_id} has fingerprint {existing_fingerprint:?}, conflicting with requested {requested_fingerprint:?}"
    )]
    OrphanTransitionConflict {
        transition_id: TransitionId,
        existing_fingerprint: BootPublicationReceiptFingerprint,
        requested_fingerprint: BootPublicationReceiptFingerprint,
    },
    #[error("boot-publication receipt insert changed {changed} rows instead of exactly one")]
    InsertRowMismatch { changed: usize },
    #[error("boot-publication receipt-head stage changed {changed} rows instead of exactly one")]
    StageRowMismatch { changed: usize },
    #[error("boot-publication receipt stage did not revalidate to the exact body and head")]
    StageRevalidationMismatch,
    #[error(transparent)]
    Database(#[from] Error),
}

impl From<diesel::result::Error> for BootPublicationReceiptStateError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database(Error::from(source))
    }
}

#[cfg(test)]
#[path = "boot_publication_receipts/tests.rs"]
mod tests;
