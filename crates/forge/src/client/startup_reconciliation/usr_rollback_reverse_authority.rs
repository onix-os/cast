//! Sealed admission and semantic reconciliation for one `/usr` reverse effect.
//!
//! Read-only POST/PRE evidence becomes disjoint opaque effect leases. A private
//! child can consume either lease only with startup-recovery seals: POST may
//! make one exchange attempt and PRE makes none, then both paths converge only
//! after ordered parent durability and a final exact PRE proof. This module
//! deliberately stops before journal advance, database mutation, cleanup,
//! triggers, or production dispatch, and never exposes a namespace snapshot or
//! raw retained descriptor.

mod effect_reconciliation;

use crate::{
    Installation, db,
    transition_journal::{
        AbortDisposition, BootRollback, ForwardPhase, Operation, Phase, RollbackAction, TransitionJournalBinding,
        TransitionJournalStore, TransitionRecord,
    },
};

use super::super::{
    active_state_snapshot::ActiveStateReservation, startup_gate::UsrRollbackReverseSeal,
    startup_recovery::UsrRollbackReverseEffectSeal,
};
use super::{
    DatabaseEvidence, InspectionError, UsrExchangeLayout, UsrRollbackReverseNamespaceEffectEvidence,
    UsrRollbackReverseNamespaceError, UsrRollbackReverseNamespaceInspection, UsrRollbackReverseNamespaceProof,
    database_ownership_evidence_compatible, inspect_database, metadata_provenance_evidence_compatible,
};

pub(in crate::client) use effect_reconciliation::{
    UsrRollbackReverseAlreadySatisfiedEffectAuthority, UsrRollbackReverseAppliedEffectAuthority,
    UsrRollbackReverseApplyReconciliation, UsrRollbackReverseDurableEffectAuthority,
};

/// Exact result of read-only reverse-effect admission.
#[allow(dead_code)] // intentionally unwired until the consuming reverse effect lands
pub(in crate::client) enum UsrRollbackReverseAdmission<'reservation> {
    NotApplicable,
    Deferred,
    Apply(UsrRollbackReverseApplyAuthority<'reservation>),
    Finish(UsrRollbackReverseFinishAuthority<'reservation>),
}

/// Common evidence retained privately behind the disjoint POST/PRE typestates.
#[allow(dead_code)] // intentionally unwired until the consuming reverse effect lands
pub(in crate::client) struct UsrRollbackReverseAuthority<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackReverseNamespaceProof,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Exact `ReverseExchangeIntent + POST` evidence. A future executor may
/// consume this type to make one reverse exchange attempt.
#[allow(dead_code)] // intentionally unwired until the consuming reverse effect lands
pub(in crate::client) struct UsrRollbackReverseApplyAuthority<'reservation> {
    evidence: UsrRollbackReverseAuthority<'reservation>,
}

/// Exact `ReverseExchangeIntent + PRE` evidence. A future executor may consume
/// this type only to finish exchange-parent durability and journal completion.
#[allow(dead_code)] // intentionally unwired until the consuming reverse effect lands
pub(in crate::client) struct UsrRollbackReverseFinishAuthority<'reservation> {
    evidence: UsrRollbackReverseAuthority<'reservation>,
}

/// Common evidence privately retained by the disjoint effect leases. No
/// field or generic accessor is exposed outside this module.
#[allow(dead_code)] // consumed by the later rollback-reverse executor
struct UsrRollbackReverseEffectLease<'reservation> {
    installation: Installation,
    state_db: db::state::Database,
    record: TransitionRecord,
    database: DatabaseEvidence,
    namespace: UsrRollbackReverseNamespaceEffectEvidence,
    journal_binding: TransitionJournalBinding,
    _active_state_reservation: &'reservation ActiveStateReservation,
}

/// Consumed, exact `ReverseExchangeIntent + POST` effect typestate.
#[allow(dead_code)] // consumed by the later rollback-reverse executor
pub(in crate::client) struct UsrRollbackReverseApplyEffectLease<'reservation> {
    lease: UsrRollbackReverseEffectLease<'reservation>,
}

/// Consumed, exact `ReverseExchangeIntent + PRE` effect typestate.
#[allow(dead_code)] // consumed by the later rollback-reverse executor
pub(in crate::client) struct UsrRollbackReverseFinishEffectLease<'reservation> {
    lease: UsrRollbackReverseEffectLease<'reservation>,
}

impl<'reservation> UsrRollbackReverseAuthority<'reservation> {
    /// Capture is sealed and read-only. Production cannot construct the seal
    /// until the future startup dispatcher is intentionally wired.
    #[allow(dead_code)] // intentionally unwired until the consuming reverse effect lands
    pub(in crate::client) fn capture(
        _startup_gate_seal: &UsrRollbackReverseSeal,
        installation: &Installation,
        journal: &TransitionJournalStore,
        state_db: &db::state::Database,
        active_state_reservation: &'reservation ActiveStateReservation,
        record: &TransitionRecord,
        initial_in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<UsrRollbackReverseAdmission<'reservation>, UsrRollbackReverseAuthorityError> {
        if record.phase != Phase::ReverseExchangeIntent {
            return Ok(UsrRollbackReverseAdmission::NotApplicable);
        }

        let journal_binding = journal.binding();
        installation.revalidate_mutable_namespace()?;
        let namespace_inspection = match UsrRollbackReverseNamespaceInspection::begin(installation, journal, record) {
            Ok(inspection) => inspection,
            Err(_) => return Ok(UsrRollbackReverseAdmission::Deferred),
        };
        let database = inspect_database(record, state_db, initial_in_flight)?;
        if !database_is_compatible(record, &database) || !reverse_plan_is_exact(record) {
            return Ok(UsrRollbackReverseAdmission::Deferred);
        }

        run_between_initial_database_captures();
        let in_flight_after = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
        let database_after = inspect_database(record, state_db, in_flight_after)?;
        if !database_is_compatible(record, &database_after) || database != database_after {
            return Ok(UsrRollbackReverseAdmission::Deferred);
        }
        let namespace = match namespace_inspection.finish(installation, journal, record) {
            Ok(namespace) => namespace,
            Err(_) => return Ok(UsrRollbackReverseAdmission::Deferred),
        };

        let retained_state_db = state_db.clone();
        debug_assert!(retained_state_db.same_instance(state_db));
        installation.revalidate_mutable_namespace()?;
        let layout = namespace.layout();
        let authority = Self {
            installation: installation.clone(),
            state_db: retained_state_db,
            record: record.clone(),
            database,
            namespace,
            journal_binding,
            _active_state_reservation: active_state_reservation,
        };
        Ok(match layout {
            UsrExchangeLayout::Post => {
                UsrRollbackReverseAdmission::Apply(UsrRollbackReverseApplyAuthority { evidence: authority })
            }
            UsrExchangeLayout::Pre => {
                UsrRollbackReverseAdmission::Finish(UsrRollbackReverseFinishAuthority { evidence: authority })
            }
        })
    }

    fn revalidate(
        &self,
        journal: &TransitionJournalStore,
        expected_layout: UsrExchangeLayout,
    ) -> Result<(), UsrRollbackReverseAuthorityError> {
        // Per-open binding must be the first observation. No namespace or
        // database evidence may be consulted for a mixed journal store.
        if !journal.has_binding(&self.journal_binding) {
            return Err(UsrRollbackReverseAuthorityErrorKind::JournalBindingMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        let database_before = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_before)?;
        self.namespace.revalidate(&self.installation, journal, &self.record)?;
        let database_after = inspect_current_database(&self.record, &self.state_db)?;
        require_exact_database(&self.database, database_after)?;
        if !reverse_plan_is_exact(&self.record) || self.namespace.layout() != expected_layout {
            return Err(UsrRollbackReverseAuthorityErrorKind::ReverseEvidenceMismatch.into());
        }
        self.installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    fn into_effect_lease(
        self,
        journal: &TransitionJournalStore,
        expected_layout: UsrExchangeLayout,
    ) -> Result<UsrRollbackReverseEffectLease<'reservation>, UsrRollbackReverseAuthorityError> {
        // This call starts with the per-open binding check. No owned field is
        // moved and no other evidence is observed before it succeeds.
        self.revalidate(journal, expected_layout)?;
        let Self {
            installation,
            state_db,
            record,
            database,
            namespace,
            journal_binding,
            _active_state_reservation,
        } = self;
        Ok(UsrRollbackReverseEffectLease {
            installation,
            state_db,
            record,
            database,
            namespace: namespace.into_effect_evidence(expected_layout)?,
            journal_binding,
            _active_state_reservation,
        })
    }
}

impl<'reservation> UsrRollbackReverseApplyAuthority<'reservation> {
    /// Revalidate only the exact POST typestate; this remains read-only.
    #[allow(dead_code)] // intentionally unwired until the consuming reverse effect lands
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackReverseAuthorityError> {
        self.evidence.revalidate(journal, UsrExchangeLayout::Post)
    }

    /// Consume POST admission into its sealed effect typestate. Possessing an
    /// authority alone is insufficient: only mutable startup recovery can
    /// construct the required seal in production.
    #[allow(dead_code)] // consumed by the later rollback-reverse executor
    pub(in crate::client) fn into_effect_lease(
        self,
        _effect_seal: &UsrRollbackReverseEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackReverseApplyEffectLease<'reservation>, UsrRollbackReverseAuthorityError> {
        let lease = self.evidence.into_effect_lease(journal, UsrExchangeLayout::Post)?;
        Ok(UsrRollbackReverseApplyEffectLease { lease })
    }
}

impl<'reservation> UsrRollbackReverseFinishAuthority<'reservation> {
    /// Revalidate only the exact PRE typestate; this remains read-only.
    #[allow(dead_code)] // intentionally unwired until the consuming reverse effect lands
    pub(in crate::client) fn revalidate(
        &self,
        journal: &TransitionJournalStore,
    ) -> Result<(), UsrRollbackReverseAuthorityError> {
        self.evidence.revalidate(journal, UsrExchangeLayout::Pre)
    }

    /// Consume PRE admission into its sealed durability-finish typestate.
    #[allow(dead_code)] // consumed by the later rollback-reverse executor
    pub(in crate::client) fn into_effect_lease(
        self,
        _effect_seal: &UsrRollbackReverseEffectSeal,
        journal: &TransitionJournalStore,
    ) -> Result<UsrRollbackReverseFinishEffectLease<'reservation>, UsrRollbackReverseAuthorityError> {
        let lease = self.evidence.into_effect_lease(journal, UsrExchangeLayout::Pre)?;
        Ok(UsrRollbackReverseFinishEffectLease { lease })
    }
}

fn reverse_plan_is_exact(record: &TransitionRecord) -> bool {
    let Some(rollback) = record.rollback.as_ref() else {
        return false;
    };
    if record.phase != Phase::ReverseExchangeIntent
        || !matches!(
            rollback.source,
            ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged
        )
        || rollback.previous_archive != RollbackAction::NotRequired
        || rollback.usr_exchange != RollbackAction::Pending
        || rollback.candidate.action != RollbackAction::Pending
        || rollback.boot != BootRollback::NotRequired
    {
        return false;
    }
    let fresh_is_exact = match record.operation {
        Operation::NewState => rollback.fresh_db == RollbackAction::Pending,
        Operation::ActivateArchived | Operation::ActiveReblit => rollback.fresh_db == RollbackAction::NotRequired,
    };
    let candidate_disposition_is_exact = match record.operation {
        Operation::ActivateArchived => rollback.candidate.disposition == AbortDisposition::Rearchive,
        Operation::NewState | Operation::ActiveReblit => rollback.candidate.disposition == AbortDisposition::Quarantine,
    };
    fresh_is_exact
        && candidate_disposition_is_exact
        && rollback.external_effects_may_remain == (record.operation != Operation::ActivateArchived)
}

fn inspect_current_database(
    record: &TransitionRecord,
    state_db: &db::state::Database,
) -> Result<DatabaseEvidence, UsrRollbackReverseAuthorityError> {
    let in_flight = state_db.audit_in_flight_transition().map_err(InspectionError::from)?;
    let evidence = inspect_database(record, state_db, in_flight)?;
    if database_is_compatible(record, &evidence) {
        Ok(evidence)
    } else {
        Err(UsrRollbackReverseAuthorityErrorKind::DatabaseIncompatible {
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
) -> Result<(), UsrRollbackReverseAuthorityError> {
    if *expected == actual {
        Ok(())
    } else {
        Err(UsrRollbackReverseAuthorityErrorKind::DatabaseChanged {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual),
        }
        .into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::client) struct UsrRollbackReverseAuthorityError(#[from] UsrRollbackReverseAuthorityErrorKind);

impl From<InspectionError> for UsrRollbackReverseAuthorityError {
    fn from(source: InspectionError) -> Self {
        UsrRollbackReverseAuthorityErrorKind::Inspection(source).into()
    }
}

impl From<UsrRollbackReverseNamespaceError> for UsrRollbackReverseAuthorityError {
    fn from(source: UsrRollbackReverseNamespaceError) -> Self {
        UsrRollbackReverseAuthorityErrorKind::Namespace(source).into()
    }
}

impl From<crate::installation::Error> for UsrRollbackReverseAuthorityError {
    fn from(source: crate::installation::Error) -> Self {
        UsrRollbackReverseAuthorityErrorKind::Installation(source).into()
    }
}

#[derive(Debug, thiserror::Error)]
enum UsrRollbackReverseAuthorityErrorKind {
    #[error("startup rollback-reverse authority was paired with a different open journal store")]
    JournalBindingMismatch,
    #[error("read the exact rollback-reverse journal during effect reconciliation")]
    JournalReadDuringEffect(#[source] crate::transition_journal::StorageError),
    #[error("the exact rollback-reverse journal changed during effect reconciliation")]
    JournalChangedDuringEffect,
    #[error("exact startup rollback-reverse evidence no longer selects its retained typestate")]
    ReverseEvidenceMismatch,
    #[error("inspect exact rollback-reverse database evidence")]
    Inspection(#[source] InspectionError),
    #[error("revalidate the independent rollback-reverse namespace proof")]
    Namespace(#[source] UsrRollbackReverseNamespaceError),
    #[error("revalidate retained mutable installation namespace around rollback-reverse authority")]
    Installation(#[source] crate::installation::Error),
    #[error("rollback-reverse database evidence is incompatible: {evidence:?}")]
    DatabaseIncompatible { evidence: Box<DatabaseEvidence> },
    #[error("rollback-reverse database evidence changed from {expected:?} to {actual:?}")]
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
pub(in crate::client) fn arm_between_usr_rollback_reverse_database_captures(hook: impl FnOnce() + 'static) {
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

#[cfg(test)]
#[allow(dead_code)] // shared fixture contains wider startup-recovery helpers
#[path = "../startup_recovery/test_support.rs"]
mod test_fixture;
#[cfg(test)]
#[path = "usr_rollback_reverse_authority/tests/support.rs"]
mod test_support;
#[cfg(test)]
mod tests;
