//! Sealed startup authority for live-root ABI normalization and durability.

use crate::{
    Installation, db,
    transition_journal::{
        Phase, StorageError, TransitionJournalBinding, TransitionJournalRecordBinding, TransitionJournalStore,
        TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrExchangedRootAbiNormalizationSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrExchangedRootAbiNamespaceAdmission,
    UsrExchangedRootAbiNamespaceError, UsrExchangedRootAbiNamespaceInspection,
    UsrExchangedRootAbiNamespaceProof, database_ownership_evidence_compatible, inspect_database,
    metadata_provenance_evidence_compatible,
};

pub(in crate::client) enum UsrExchangedRootAbiNormalizationAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Normalize(UsrExchangedRootAbiNormalizationAuthority<'reservation>),
    Synchronize(UsrExchangedRootAbiDurabilityAuthority<'reservation>),
}

/// Exact database and namespace evidence for filling an incomplete canonical
/// subset. It exposes neither journal advancement nor any unrelated namespace
/// effect.
pub(in crate::client) struct UsrExchangedRootAbiNormalizationAuthority<'reservation> {
    retained: RetainedUsrExchangedRootAbiAuthority<'reservation>,
}

/// Exact database and namespace evidence for synchronizing a complete set
/// through its retained root directory before rollback-decision admission.
pub(in crate::client) struct UsrExchangedRootAbiDurabilityAuthority<'reservation> {
    retained: RetainedUsrExchangedRootAbiAuthority<'reservation>,
}

struct RetainedUsrExchangedRootAbiAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: Option<UsrExchangedRootAbiNamespaceProof>,
    journal_binding: TransitionJournalBinding,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

impl<'reservation> UsrExchangedRootAbiNormalizationAuthority<'reservation> {
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrExchangedRootAbiNormalizationSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<UsrExchangedRootAbiNormalizationAdmission<'reservation>, UsrExchangedRootAbiNormalizationAuthorityError>
    {
        if record.phase != Phase::UsrExchanged || record.rollback.is_some() {
            return Ok(UsrExchangedRootAbiNormalizationAdmission::NotApplicable);
        }

        let journal_binding = journal.binding();
        require_journal_binding(journal, &journal_binding)?;
        let journal_record_binding =
            journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;

        // The initial audit was obtained while this same journal guard was
        // retained. Close a DB -> namespace -> DB sandwich before granting any
        // effect authority.
        let database = inspect_database(record, state_db, initial_in_flight)?;
        if !database_is_compatible(record, &database) {
            return Ok(UsrExchangedRootAbiNormalizationAdmission::Deferred);
        }
        let inspection = match UsrExchangedRootAbiNamespaceInspection::begin(installation, journal, record) {
            Ok(inspection) => inspection,
            Err(_) => return Ok(UsrExchangedRootAbiNormalizationAdmission::Deferred),
        };
        let namespace = match inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrExchangedRootAbiNormalizationAdmission::Deferred),
        };
        let database_after = inspect_current_database(record, state_db)?;
        if database != database_after {
            return Ok(UsrExchangedRootAbiNormalizationAdmission::Deferred);
        }
        installation.revalidate_mutable_namespace()?;
        require_journal_binding(journal, &journal_binding)?;
        require_journal_record_binding(installation, journal, &journal_record_binding, record)?;

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        let (namespace, complete) = match namespace {
            UsrExchangedRootAbiNamespaceAdmission::Complete(namespace) => (namespace, true),
            UsrExchangedRootAbiNamespaceAdmission::Incomplete(namespace) => (namespace, false),
        };
        let retained = RetainedUsrExchangedRootAbiAuthority {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database,
            namespace: Some(namespace),
            journal_binding,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        };
        Ok(if complete {
            UsrExchangedRootAbiNormalizationAdmission::Synchronize(UsrExchangedRootAbiDurabilityAuthority {
                retained,
            })
        } else {
            UsrExchangedRootAbiNormalizationAdmission::Normalize(UsrExchangedRootAbiNormalizationAuthority {
                retained,
            })
        })
    }

    pub(in crate::client) fn normalize(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrExchangedRootAbiNormalizationAuthorityError> {
        let mut retained = self.retained;
        retained.revalidate(journal)?;
        let namespace = retained.namespace.take().expect("normalization authority owns one namespace proof");
        let applied = namespace.normalize(&retained.installation, journal, &retained.record, || {
            retained
                .revalidate_database_and_binding(journal)
                .map_err(|_| UsrExchangedRootAbiNamespaceError::FinalAuthorityRejected)
        })?;
        retained.revalidate_database_and_binding(journal)?;
        // The publisher-returned link capability is intentionally held across
        // the complete DB/journal close and checked immediately before success.
        applied.revalidate_final(&retained.installation, journal, &retained.record)?;
        Ok(())
    }
}

impl<'reservation> UsrExchangedRootAbiDurabilityAuthority<'reservation> {
    pub(in crate::client) fn synchronize(
        self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrExchangedRootAbiNormalizationAuthorityError> {
        let mut retained = self.retained;
        retained.revalidate(journal)?;
        let namespace = retained.namespace.take().expect("durability authority owns one namespace proof");
        let durable = namespace.synchronize_complete(&retained.installation, journal, &retained.record, || {
            retained
                .revalidate_database_and_binding(journal)
                .map_err(|_| UsrExchangedRootAbiNamespaceError::FinalAuthorityRejected)
        })?;
        retained.revalidate_database_and_binding(journal)?;
        durable.revalidate_final(&retained.installation, journal, &retained.record)?;
        Ok(())
    }
}

impl RetainedUsrExchangedRootAbiAuthority<'_> {
    fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrExchangedRootAbiNormalizationAuthorityError> {
        require_journal_binding(journal, &self.journal_binding)?;
        require_journal_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        let database_before = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_before)?;
        self.namespace
            .as_ref()
            .expect("unconsumed root ABI authority owns one namespace proof")
            .revalidate_source(&self.installation, journal, &self.record)?;
        let database_after = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_after)?;
        self.installation.revalidate_mutable_namespace()?;
        require_journal_binding(journal, &self.journal_binding)?;
        require_journal_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )
    }

    fn revalidate_database_and_binding(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrExchangedRootAbiNormalizationAuthorityError> {
        require_journal_binding(journal, &self.journal_binding)?;
        require_journal_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )?;
        self.installation.revalidate_mutable_namespace()?;
        let database_before = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_before)?;
        let database_after = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_after)?;
        self.installation.revalidate_mutable_namespace()?;
        require_journal_binding(journal, &self.journal_binding)?;
        require_journal_record_binding(
            &self.installation,
            journal,
            &self.journal_record_binding,
            &self.record,
        )
    }
}

fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseEvidence, UsrExchangedRootAbiNormalizationAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let evidence = inspect_database(record, state_db, in_flight)?;
    if database_is_compatible(record, &evidence) {
        Ok(evidence)
    } else {
        Err(UsrExchangedRootAbiNormalizationAuthorityErrorKind::DatabaseIncompatible {
            evidence: Box::new(evidence),
        }
        .into())
    }
}

fn database_is_compatible(record: &TransitionRecord, evidence: &DatabaseEvidence) -> bool {
    database_ownership_evidence_compatible(record, evidence)
        && metadata_provenance_evidence_compatible(record, evidence)
}

fn require_exact_database(
    expected: &DatabaseEvidence,
    actual: DatabaseEvidence,
) -> Result<(), UsrExchangedRootAbiNormalizationAuthorityError> {
    if *expected == actual {
        Ok(())
    } else {
        Err(UsrExchangedRootAbiNormalizationAuthorityErrorKind::DatabaseChanged {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual),
        }
        .into())
    }
}

fn require_journal_binding(
    journal: &TransitionJournalStore,
    expected: &TransitionJournalBinding,
) -> Result<(), UsrExchangedRootAbiNormalizationAuthorityError> {
    if journal.has_binding(expected) {
        Ok(())
    } else {
        Err(UsrExchangedRootAbiNormalizationAuthorityErrorKind::JournalBindingMismatch.into())
    }
}

fn require_journal_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    expected: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrExchangedRootAbiNormalizationAuthorityError> {
    if journal.has_record_binding(
        installation.retained_mutable_cast_directory()?,
        expected,
        record,
    )? {
        Ok(())
    } else {
        Err(UsrExchangedRootAbiNormalizationAuthorityErrorKind::JournalRecordBindingMismatch.into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrExchangedRootAbiNormalizationAuthorityError(
    #[from] UsrExchangedRootAbiNormalizationAuthorityErrorKind,
);

impl From<InspectionError> for UsrExchangedRootAbiNormalizationAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrExchangedRootAbiNormalizationAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrExchangedRootAbiNamespaceError> for UsrExchangedRootAbiNormalizationAuthorityError {
    fn from(source: UsrExchangedRootAbiNamespaceError) -> Self {
        UsrExchangedRootAbiNormalizationAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrExchangedRootAbiNormalizationAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrExchangedRootAbiNormalizationAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrExchangedRootAbiNormalizationAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrExchangedRootAbiNormalizationAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrExchangedRootAbiNormalizationAuthorityErrorKind {
    #[error("root ABI normalization authority was paired with a different open journal store")]
    JournalBindingMismatch,
    #[error("the canonical transition journal inode changed after root ABI authority capture")]
    JournalRecordBindingMismatch,
    #[error("inspect the exact canonical transition journal inode")]
    Journal(#[source] StorageError),
    #[error("inspect exact UsrExchanged database evidence")]
    Inspection(#[source] InspectionError),
    #[error("revalidate exact UsrExchanged root ABI namespace evidence")]
    Namespace(#[source] UsrExchangedRootAbiNamespaceError),
    #[error("revalidate retained mutable installation namespace around root ABI normalization")]
    Installation(#[source] crate::installation::Error),
    #[error("UsrExchanged database evidence is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("UsrExchanged database evidence changed from {expected:?} to {actual:?}")]
    DatabaseChanged {
        expected: Box<DatabaseEvidence>,
        actual: Box<DatabaseEvidence>,
    },
}
