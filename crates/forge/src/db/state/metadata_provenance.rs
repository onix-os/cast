use diesel::prelude::*;
use diesel::sqlite::{Sqlite, SqliteConnection};
use sha2::{Digest as _, Sha256};

use super::{
    Database, Error, TransitionEvidenceError, TransitionOwnership, model, parse_transition_evidence,
    schema::state_metadata_provenance,
};
use crate::state::{Id, TransitionId};

const SHA256_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MetadataDigest([u8; SHA256_BYTES]);

/// Immutable byte identity for the two generated files owned by one state.
///
/// Fields remain private so callers can compare authenticated output bytes or
/// pass the complete pair back to the state database, but cannot accidentally
/// exchange the two labeled digests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MetadataProvenance {
    os_release_sha256: MetadataDigest,
    system_model_sha256: MetadataDigest,
}

impl MetadataProvenance {
    pub(crate) fn from_outputs(os_release: &[u8], system_model: &[u8]) -> Self {
        Self {
            os_release_sha256: MetadataDigest::hash(os_release),
            system_model_sha256: MetadataDigest::hash(system_model),
        }
    }

    pub(crate) fn matches_os_release(&self, bytes: &[u8]) -> bool {
        self.os_release_sha256 == MetadataDigest::hash(bytes)
    }

    pub(crate) fn matches_system_model(&self, bytes: &[u8]) -> bool {
        self.system_model_sha256 == MetadataDigest::hash(bytes)
    }

    /// Require both labeled output buffers to match this independently loaded
    /// immutable pair.
    pub(crate) fn require_outputs(
        &self,
        state: Id,
        os_release: &[u8],
        system_model: &[u8],
    ) -> Result<(), MetadataProvenanceError> {
        if self.matches_os_release(os_release) && self.matches_system_model(system_model) {
            Ok(())
        } else {
            Err(MetadataProvenanceError::Mismatch {
                state_id: i32::from(state),
            })
        }
    }
}

impl MetadataDigest {
    fn hash(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    fn decode(state: Id, field: &'static str, stored: Vec<u8>) -> Result<Self, MetadataProvenanceError> {
        let actual = stored.len();
        let digest = stored
            .try_into()
            .map_err(|_| MetadataProvenanceError::InvalidStoredDigestLength {
                state_id: i32::from(state),
                field,
                actual,
            })?;
        Ok(Self(digest))
    }

    fn as_bytes(&self) -> &[u8; SHA256_BYTES] {
        &self.0
    }
}

impl Database {
    /// Read the immutable generated-metadata identity for one state.
    ///
    /// `None` deliberately covers a legacy state which predates provenance;
    /// this API never hashes or backfills bytes from an archived candidate.
    pub(crate) fn metadata_provenance(&self, state: Id) -> Result<Option<MetadataProvenance>, MetadataProvenanceError> {
        self.conn.exec(|conn| metadata_provenance_impl(conn, state))
    }

    /// Require a provenance row without inventing a legacy fallback.
    pub(crate) fn required_metadata_provenance(
        &self,
        state: Id,
    ) -> Result<MetadataProvenance, MetadataProvenanceError> {
        self.metadata_provenance(state)?
            .ok_or(MetadataProvenanceError::Missing {
                state_id: i32::from(state),
            })
    }

    /// Re-read and compare the complete immutable pair for an evidence
    /// sandwich around filesystem proof construction.
    pub(crate) fn require_exact_metadata_provenance(
        &self,
        state: Id,
        expected: &MetadataProvenance,
    ) -> Result<(), MetadataProvenanceError> {
        let actual = self.required_metadata_provenance(state)?;
        if actual == *expected {
            Ok(())
        } else {
            Err(MetadataProvenanceError::Mismatch {
                state_id: i32::from(state),
            })
        }
    }

    /// Insert provenance only while the fresh state row still carries the
    /// exact transition which authorized candidate preparation.
    ///
    /// Existing rows are immutable, including an already-equal row. Crash
    /// reconciliation must read durable evidence rather than treating this
    /// method as an upsert or an idempotent adoption surface. An ordinary
    /// SQLite commit error has uncertain durable outcome; callers must fail
    /// stop and reopen rather than infer absence from the returned error.
    pub(crate) fn insert_fresh_metadata_provenance_if_transition_matches(
        &self,
        state: Id,
        transition: &TransitionId,
        provenance: &MetadataProvenance,
    ) -> Result<(), MetadataProvenanceError> {
        self.conn.exclusive_tx(|tx| {
            let ownership = transition_ownership_impl(tx, state, transition)?;
            if ownership != TransitionOwnership::Matching {
                return Err(MetadataProvenanceError::FreshTransitionMismatch {
                    state_id: i32::from(state),
                    ownership,
                });
            }
            if metadata_provenance_impl(tx, state)?.is_some() {
                return Err(MetadataProvenanceError::AlreadyExists {
                    state_id: i32::from(state),
                });
            }
            insert_metadata_provenance_row(tx, state, provenance)?;
            metadata_provenance_fault(MetadataProvenanceFaultPoint::BeforeCommit)
        })?;
        metadata_provenance_fault(MetadataProvenanceFaultPoint::AfterCommit)
    }

    #[cfg(test)]
    pub(crate) fn delete_metadata_provenance_for_test(&self, state: Id) -> Result<(), Error> {
        self.conn.exclusive_tx(|tx| {
            diesel::delete(state_metadata_provenance::table.find(i32::from(state))).execute(tx)?;
            Ok(())
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetadataProvenanceFaultPoint {
    BeforeCommit,
    AfterCommit,
}

/// Exact outcome of the two deterministic test-only insertion boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) enum MetadataProvenancePersistenceOutcome {
    DefinitelyNotApplied,
    AppliedButReportedError,
}

#[cfg(test)]
std::thread_local! {
    static METADATA_PROVENANCE_FAULT: std::cell::Cell<Option<MetadataProvenanceFaultPoint>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_metadata_provenance_fault(point: MetadataProvenanceFaultPoint) {
    METADATA_PROVENANCE_FAULT.with(|fault| {
        assert!(
            fault.replace(Some(point)).is_none(),
            "a metadata-provenance fault is already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn assert_metadata_provenance_fault_consumed() {
    METADATA_PROVENANCE_FAULT.with(|fault| {
        assert!(fault.get().is_none(), "armed metadata-provenance fault was not reached");
    });
}

#[cfg(test)]
fn metadata_provenance_fault(point: MetadataProvenanceFaultPoint) -> Result<(), MetadataProvenanceError> {
    let injected = METADATA_PROVENANCE_FAULT.with(|fault| fault.get() == Some(point));
    if injected {
        METADATA_PROVENANCE_FAULT.with(|fault| fault.set(None));
        let outcome = match point {
            MetadataProvenanceFaultPoint::BeforeCommit => MetadataProvenancePersistenceOutcome::DefinitelyNotApplied,
            MetadataProvenanceFaultPoint::AfterCommit => MetadataProvenancePersistenceOutcome::AppliedButReportedError,
        };
        Err(MetadataProvenanceError::FaultInjected { point, outcome })
    } else {
        Ok(())
    }
}

#[cfg(not(test))]
fn metadata_provenance_fault(_point: MetadataProvenanceFaultPoint) -> Result<(), MetadataProvenanceError> {
    Ok(())
}

fn transition_ownership_impl(
    tx: &mut SqliteConnection,
    state: Id,
    transition: &TransitionId,
) -> Result<TransitionOwnership, MetadataProvenanceError> {
    let stored = model::state::table
        .find(i32::from(state))
        .select(model::state::transition_id)
        .first::<Option<String>>(tx)
        .optional()
        .map_err(Error::from)?;
    match stored {
        None => Ok(TransitionOwnership::Missing),
        Some(None) => Ok(TransitionOwnership::Cleared),
        Some(Some(raw)) => {
            let stored = parse_transition_evidence(state, raw)?;
            Ok(if stored == *transition {
                TransitionOwnership::Matching
            } else {
                TransitionOwnership::Foreign
            })
        }
    }
}

fn metadata_provenance_impl(
    conn: &mut SqliteConnection,
    state: Id,
) -> Result<Option<MetadataProvenance>, MetadataProvenanceError> {
    state_metadata_provenance::table
        .find(i32::from(state))
        .select(StoredMetadataProvenance::as_select())
        .first::<StoredMetadataProvenance>(conn)
        .optional()
        .map_err(Error::from)?
        .map(MetadataProvenance::try_from)
        .transpose()
}

pub(super) fn insert_metadata_provenance_row(
    tx: &mut SqliteConnection,
    state: Id,
    provenance: &MetadataProvenance,
) -> Result<(), MetadataProvenanceError> {
    diesel::insert_into(state_metadata_provenance::table)
        .values(NewMetadataProvenance {
            state_id: i32::from(state),
            os_release_sha256: provenance.os_release_sha256.as_bytes(),
            system_model_sha256: provenance.system_model_sha256.as_bytes(),
        })
        .execute(tx)
        .map_err(Error::from)?;
    Ok(())
}

pub(super) fn delete_metadata_provenance(tx: &mut SqliteConnection, states: &[i32]) -> Result<(), Error> {
    diesel::delete(state_metadata_provenance::table.filter(state_metadata_provenance::state_id.eq_any(states)))
        .execute(tx)?;
    Ok(())
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = state_metadata_provenance)]
#[diesel(check_for_backend(Sqlite))]
struct StoredMetadataProvenance {
    state_id: i32,
    os_release_sha256: Vec<u8>,
    system_model_sha256: Vec<u8>,
}

impl TryFrom<StoredMetadataProvenance> for MetadataProvenance {
    type Error = MetadataProvenanceError;

    fn try_from(stored: StoredMetadataProvenance) -> Result<Self, Self::Error> {
        let state = Id::from(stored.state_id);
        Ok(Self {
            os_release_sha256: MetadataDigest::decode(state, "os_release_sha256", stored.os_release_sha256)?,
            system_model_sha256: MetadataDigest::decode(state, "system_model_sha256", stored.system_model_sha256)?,
        })
    }
}

#[derive(Insertable)]
#[diesel(table_name = state_metadata_provenance)]
struct NewMetadataProvenance<'a> {
    state_id: i32,
    os_release_sha256: &'a [u8],
    system_model_sha256: &'a [u8],
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MetadataProvenanceError {
    #[error("state {state_id} has no durable generated-metadata provenance")]
    Missing { state_id: i32 },
    #[error("state {state_id} generated-metadata provenance differs from the retained expectation")]
    Mismatch { state_id: i32 },
    #[error("state {state_id} already has immutable generated-metadata provenance")]
    AlreadyExists { state_id: i32 },
    #[error("fresh state {state_id} has {ownership:?} transition ownership instead of Matching")]
    FreshTransitionMismatch {
        state_id: i32,
        ownership: TransitionOwnership,
    },
    #[error("state {state_id} stores a {field} digest with {actual} bytes instead of 32")]
    InvalidStoredDigestLength {
        state_id: i32,
        field: &'static str,
        actual: usize,
    },
    #[cfg(test)]
    #[error("injected generated-metadata provenance fault at {point:?} with {outcome:?}")]
    FaultInjected {
        point: MetadataProvenanceFaultPoint,
        outcome: MetadataProvenancePersistenceOutcome,
    },
    #[error(transparent)]
    TransitionEvidence(#[from] TransitionEvidenceError),
    #[error(transparent)]
    Database(#[from] Error),
}

impl From<diesel::result::Error> for MetadataProvenanceError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database(Error::from(source))
    }
}

#[cfg(test)]
mod tests;
