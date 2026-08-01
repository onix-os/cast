//! Sealed admission and reconciliation for one previous-restore effect.
//!
//! `PreviousRestoreIntent` is the first phase of a rollback that has to undo a
//! completed predecessor archive. Read-only `Archived`/`Restored` evidence
//! becomes disjoint opaque effect leases: `Archived` may make one compensating
//! move, `Restored` makes none. Both converge on the same exact
//! `PreviousRestoredToStaging` persistence boundary. Nothing here touches the
//! `/usr` exchange, the candidate, the database, or any trigger.

mod effect_reconciliation;

use crate::{
    Installation, db,
    transition_journal::{
        BootRollback, Phase, RollbackAction, StorageError, TransitionJournalRecordBinding, TransitionJournalStore,
        TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackPreviousRestoreSeal,
    startup_recovery::UsrRollbackPreviousRestoreEffectSeal,
};
use super::{
    DatabaseEvidence, InspectionError, PreviousRestoreLayout, UsrRollbackPreviousRestoreNamespaceEffectEvidence,
    UsrRollbackPreviousRestoreNamespaceError, UsrRollbackPreviousRestoreNamespaceInspection,
    UsrRollbackPreviousRestoreNamespaceProof, database_ownership_evidence_compatible, inspect_database,
    metadata_provenance_evidence_compatible,
};

pub(in crate::client) use effect_reconciliation::{
    UsrRollbackPreviousRestoreApplyReconciliation, UsrRollbackPreviousRestoreDurableEffectAuthority,
    UsrRollbackPreviousRestoreRecordAdvanceError,
};

/// Exact result of read-only previous-restore admission.
pub(in crate::client) enum UsrRollbackPreviousRestoreAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Apply(UsrRollbackPreviousRestoreApplyAuthority<'reservation>),
    Finish(UsrRollbackPreviousRestoreFinishAuthority<'reservation>),
}

/// Common evidence retained privately behind the disjoint typestates.
pub(in crate::client) struct UsrRollbackPreviousRestoreAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackPreviousRestoreNamespaceProof,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Exact `PreviousRestoreIntent + Archived` evidence: the compensating move
/// still has to run.
pub(in crate::client) struct UsrRollbackPreviousRestoreApplyAuthority<'reservation> {
    evidence: UsrRollbackPreviousRestoreAuthority<'reservation>,
}

/// Exact `PreviousRestoreIntent + Restored` evidence: a previous attempt
/// already moved the predecessor, so only journal completion remains.
pub(in crate::client) struct UsrRollbackPreviousRestoreFinishAuthority<'reservation> {
    evidence: UsrRollbackPreviousRestoreAuthority<'reservation>,
}

/// Common evidence privately retained by the disjoint effect leases.
struct UsrRollbackPreviousRestoreEffectLease<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackPreviousRestoreNamespaceEffectEvidence,
    journal_record_binding: TransitionJournalRecordBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Consumed, exact `Archived` effect typestate.
pub(in crate::client) struct UsrRollbackPreviousRestoreApplyEffectLease<'reservation> {
    lease: UsrRollbackPreviousRestoreEffectLease<'reservation>,
}

/// Consumed, exact `Restored` effect typestate.
pub(in crate::client) struct UsrRollbackPreviousRestoreFinishEffectLease<'reservation> {
    lease: UsrRollbackPreviousRestoreEffectLease<'reservation>,
}

impl<'reservation> UsrRollbackPreviousRestoreAuthority<'reservation> {
    /// Capture is sealed and read-only. Only the production startup dispatcher
    /// constructs the seal which admits this phase.
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackPreviousRestoreSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<UsrRollbackPreviousRestoreAdmission<'reservation>, UsrRollbackPreviousRestoreAuthorityError> {
        if record.phase != Phase::PreviousRestoreIntent {
            return Ok(UsrRollbackPreviousRestoreAdmission::NotApplicable);
        }

        installation.revalidate_mutable_namespace()?;
        let journal_record_binding = journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;
        installation.revalidate_mutable_namespace()?;
        let namespace_inspection =
            match UsrRollbackPreviousRestoreNamespaceInspection::begin(installation, journal, record) {
                Ok(inspection) => inspection,
                Err(_) => return Ok(UsrRollbackPreviousRestoreAdmission::Deferred),
            };
        let database = inspect_database(record, state_db, initial_in_flight)?;
        if !database_is_compatible(record, &database) || !previous_restore_plan_is_exact(record) {
            return Ok(UsrRollbackPreviousRestoreAdmission::Deferred);
        }

        run_between_initial_database_captures();
        let in_flight_after = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
        let database_after = inspect_database(record, state_db, in_flight_after)?;
        if !database_is_compatible(record, &database_after) || database != database_after {
            return Ok(UsrRollbackPreviousRestoreAdmission::Deferred);
        }
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackPreviousRestoreAdmission::Deferred),
        };

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        require_journal_record_binding(installation, journal, &journal_record_binding, record)?;
        installation.revalidate_mutable_namespace()?;
        let layout = namespace.layout();
        let authority = Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation: active_state_reservation,
        };
        Ok(match layout {
            PreviousRestoreLayout::Archived => {
                UsrRollbackPreviousRestoreAdmission::Apply(UsrRollbackPreviousRestoreApplyAuthority {
                    evidence: authority,
                })
            }
            PreviousRestoreLayout::Restored => {
                UsrRollbackPreviousRestoreAdmission::Finish(UsrRollbackPreviousRestoreFinishAuthority {
                    evidence: authority,
                })
            }
        })
    }

    fn revalidate(
        &self,
        journal: &TransitionJournalStore,
        expected_layout: PreviousRestoreLayout,
    ) -> Result<(), UsrRollbackPreviousRestoreAuthorityError> {
        // Exact public record identity is the first observation. Equal bytes
        // at a replacement inode cannot authorize an effect.
        require_journal_record_binding(&self.installation, journal, &self.journal_record_binding, &self.record)?;
        self.installation.revalidate_mutable_namespace()?;
        let database_before = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_before)?;
        self.namespace.revalidate(&self.installation, journal, &self.record)?;
        let database_after = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_after)?;
        if !previous_restore_plan_is_exact(&self.record) || self.namespace.layout() != expected_layout {
            return Err(UsrRollbackPreviousRestoreAuthorityErrorKind::RestoreEvidenceMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        require_journal_record_binding(&self.installation, journal, &self.journal_record_binding, &self.record)?;
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    fn into_effect_lease(
        self,
        journal: &TransitionJournalStore,
        expected_layout: PreviousRestoreLayout,
    ) -> Result<UsrRollbackPreviousRestoreEffectLease<'reservation>, UsrRollbackPreviousRestoreAuthorityError> {
        // This call starts with the per-open binding check. No owned field is
        // moved and no other evidence is observed before it succeeds.
        self.revalidate(journal, expected_layout)?;
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        } = self;
        let namespace = namespace.into_effect_evidence(expected_layout)?;
        installation.revalidate_mutable_namespace()?;
        require_journal_record_binding(&installation, journal, &journal_record_binding, &record)?;
        installation.revalidate_mutable_namespace()?;
        Ok(UsrRollbackPreviousRestoreEffectLease {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_record_binding,
            _active_state_reservation,
        })
    }
}

impl<'reservation> UsrRollbackPreviousRestoreApplyAuthority<'reservation> {
    /// Consume `Archived` admission into its sealed effect typestate. Only
    /// mutable startup recovery can construct the required seal in production.
    pub(in crate::client) fn into_effect_lease(
        self,
        _effect_seal: &UsrRollbackPreviousRestoreEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackPreviousRestoreApplyEffectLease<'reservation>, UsrRollbackPreviousRestoreAuthorityError>
    {
        let lease = self
            .evidence
            .into_effect_lease(journal, PreviousRestoreLayout::Archived)?;
        Ok(UsrRollbackPreviousRestoreApplyEffectLease { lease })
    }
}

impl<'reservation> UsrRollbackPreviousRestoreFinishAuthority<'reservation> {
    /// Consume `Restored` admission into its sealed completion typestate.
    pub(in crate::client) fn into_effect_lease(
        self,
        _effect_seal: &UsrRollbackPreviousRestoreEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackPreviousRestoreFinishEffectLease<'reservation>, UsrRollbackPreviousRestoreAuthorityError>
    {
        let lease = self
            .evidence
            .into_effect_lease(journal, PreviousRestoreLayout::Restored)?;
        Ok(UsrRollbackPreviousRestoreFinishEffectLease { lease })
    }
}

/// The exact plan a `PreviousRestoreIntent` record must carry.
///
/// Every field is derived, for the reasons the rest of the rollback tail
/// records: the archive is possible only when this record's own options say it
/// happened, the disposition comes from the journal's own rule, and the
/// external-effects evidence is a function of the source phase.
fn previous_restore_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    record.phase == Phase::PreviousRestoreIntent
        && crate::transition_journal::rollback_evidence_is_on_chain(record)
        // Routing reaches this phase only while the restore is outstanding: it
        // is the first action in the chain, so nothing before it can be
        // pending and it cannot already be resolved.
        && record.previous_restore_rollback_is_possible(rollback.source)
        && rollback.previous_archive == RollbackAction::Pending
        // The exchange is still in place; reversing it is the next action.
        && rollback.usr_exchange == RollbackAction::Pending
        && rollback.candidate.action == RollbackAction::Pending
        && rollback.candidate.disposition == record.candidate_disposition_for(rollback.source)
        && if record.fresh_db_rollback_is_possible(rollback.source) {
            rollback.fresh_db == RollbackAction::Pending
        } else {
            rollback.fresh_db == RollbackAction::NotRequired
        }
        && rollback.boot
            == if crate::transition_journal::boot_rollback_is_possible(rollback.source) {
                BootRollback::PendingUnverifiable
            } else {
                BootRollback::NotRequired
            }
        && rollback.external_effects_may_remain == record.expected_external_effects_may_remain(rollback.source)
        // The parking name the archive recorded is what makes it reversible;
        // `validate_previous_archive_slot` guarantees it is present here, and
        // demanding it again keeps the effect from inferring one.
        && record.previous_archive_slot.is_some()
        && record.previous.id.is_some()
        && record.candidate.id.is_some()
}

#[cfg(test)]
pub(in crate::client) fn usr_rollback_previous_restore_plan_is_exact_for_test(record: &TransitionRecord) -> bool {
    previous_restore_plan_is_exact(record)
}

fn require_journal_record_binding(
    installation: &Installation,
    journal: &TransitionJournalStore,
    binding: &TransitionJournalRecordBinding,
    record: &TransitionRecord,
) -> Result<(), UsrRollbackPreviousRestoreAuthorityError> {
    if !journal.has_record_store_binding(binding) {
        return Err(UsrRollbackPreviousRestoreAuthorityErrorKind::JournalRecordBindingMismatch.into());
    }
    let cast = installation.retained_mutable_cast_directory()?;
    if journal.has_record_binding(cast, binding, record)? {
        Ok(())
    } else {
        Err(UsrRollbackPreviousRestoreAuthorityErrorKind::JournalRecordBindingMismatch.into())
    }
}

fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseEvidence, UsrRollbackPreviousRestoreAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let evidence = inspect_database(record, state_db, in_flight)?;
    if database_is_compatible(record, &evidence) {
        Ok(evidence)
    } else {
        Err(UsrRollbackPreviousRestoreAuthorityErrorKind::DatabaseIncompatible {
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
) -> Result<(), UsrRollbackPreviousRestoreAuthorityError> {
    if *expected == actual {
        Ok(())
    } else {
        Err(UsrRollbackPreviousRestoreAuthorityErrorKind::DatabaseChanged {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual),
        }
        .into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackPreviousRestoreAuthorityError(
    #[from] UsrRollbackPreviousRestoreAuthorityErrorKind,
);

impl From<InspectionError> for UsrRollbackPreviousRestoreAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackPreviousRestoreAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrRollbackPreviousRestoreNamespaceError> for UsrRollbackPreviousRestoreAuthorityError {
    fn from(source: UsrRollbackPreviousRestoreNamespaceError) -> Self {
        UsrRollbackPreviousRestoreAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackPreviousRestoreAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackPreviousRestoreAuthorityErrorKind::Installation(source).into()
    }
}

impl From<StorageError> for UsrRollbackPreviousRestoreAuthorityError {
    fn from(source: StorageError) -> Self {
        UsrRollbackPreviousRestoreAuthorityErrorKind::Journal(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackPreviousRestoreAuthorityErrorKind {
    #[error("startup previous-restore authority lost its exact canonical journal record binding")]
    JournalRecordBindingMismatch,
    #[error("exact startup previous-restore evidence no longer selects its retained typestate")]
    RestoreEvidenceMismatch,
    #[error("inspect exact previous-restore database evidence")]
    Inspection(#[source] InspectionError),
    #[error("revalidate the independent previous-restore namespace proof")]
    Namespace(#[source] UsrRollbackPreviousRestoreNamespaceError),
    #[error("revalidate retained mutable installation namespace around previous-restore authority")]
    Installation(#[source] crate::installation::Error),
    #[error("capture or revalidate the exact previous-restore journal record binding")]
    Journal(#[source] StorageError),
    #[error("perform the exact compensating previous-tree restore")]
    Restore(#[source] Box<crate::transition_identity::Error>),
    #[error("previous-restore database evidence is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("previous-restore database evidence changed from {expected:?} to {actual:?}")]
    DatabaseChanged {
        expected: Box<DatabaseEvidence>,
        actual: Box<DatabaseEvidence>,
    },
}

#[cfg(test)]
std::thread_local! {
    static BETWEEN_INITIAL_DATABASE_CAPTURES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::client) fn arm_between_usr_rollback_previous_restore_database_captures(hook: impl FnOnce() + 'static) {
    BETWEEN_INITIAL_DATABASE_CAPTURES.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn run_between_initial_database_captures() {
    BETWEEN_INITIAL_DATABASE_CAPTURES.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_between_initial_database_captures() {}
