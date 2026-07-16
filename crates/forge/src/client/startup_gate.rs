use thiserror::Error;

use crate::{Installation, db, installation, transition_identity, transition_journal};

use super::{
    active_state_snapshot::{ActiveStateLease, ActiveStateReservation},
    startup_reconciliation,
};

mod default_system_intent;

/// Exclusive proof that no interrupted system transition predates client
/// construction.
///
/// Keeping the journal store alive serializes repository and registry
/// construction with transition writers. The builder must drop this proof
/// before returning the client because stateful operations acquire their own
/// retained journal store.
pub(super) struct CleanSystemStartup {
    _authority: startup_reconciliation::StartupRecoveryAuthority,
}

/// Unforgeable safe-code token limiting aggregate replacement-mutation
/// provider construction to this writer-first startup gate.
pub(in crate::client) struct ActiveReblitReplacementMutationSeal {
    _private: (),
}

impl ActiveReblitReplacementMutationSeal {
    fn new() -> Self {
        Self { _private: () }
    }
}

/// Unforgeable safe-code token limiting rollback-decision authority capture
/// to this writer-first startup gate.
pub(in crate::client) struct UsrRollbackDecisionSeal {
    _private: (),
}

impl UsrRollbackDecisionSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable safe-code token limiting rollback-resume routing authority
/// capture to this writer-first startup gate.
pub(in crate::client) struct UsrRollbackResumeRouteSeal {
    _private: (),
}

impl UsrRollbackResumeRouteSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

impl CleanSystemStartup {
    pub(super) fn enter(
        installation: &Installation,
        state_db: &db::state::Database,
        active_state_reservation: &ActiveStateReservation,
    ) -> Result<Self, Error> {
        installation.revalidate_mutable_namespace()?;
        let cast = installation.retained_mutable_cast_directory()?;
        after_mutable_namespace_preflight();
        let journal = transition_journal::TransitionJournalStore::open_in_retained_cast(cast, &installation.root);
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let journal = journal?;

        let record = journal.load();
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let record = record?;
        let in_flight = state_db.audit_in_flight_transition();
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let in_flight = in_flight?;
        if let Some(record) = record {
            {
                let mutation_seal = ActiveReblitReplacementMutationSeal::new();
                let mut mutation_authority =
                    startup_reconciliation::ActiveReblitReplacementMutationAuthorityProvider::new(
                        &mutation_seal,
                        installation,
                        &journal,
                        state_db,
                        active_state_reservation,
                        &record,
                        in_flight.clone(),
                    );
                transition_identity::recover_active_reblit_replacement_residue(&mut mutation_authority)?;
            }

            let decision_seal = UsrRollbackDecisionSeal::new();
            let decision = startup_reconciliation::UsrRollbackDecisionAuthority::capture(
                &decision_seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
                in_flight.clone(),
            )?;
            let authority = match decision {
                startup_reconciliation::UsrRollbackDecisionAdmission::Ready(authority) => Some(authority),
                startup_reconciliation::UsrRollbackDecisionAdmission::ParentDurabilityRequired(authority) => Some(
                    super::startup_recovery::normalize_usr_exchange_parent_durability(&journal, authority)?,
                ),
                startup_reconciliation::UsrRollbackDecisionAdmission::NotApplicable
                | startup_reconciliation::UsrRollbackDecisionAdmission::Deferred(_) => None,
            };
            if let Some(authority) = authority {
                let (journal, record) =
                    super::startup_recovery::persist_usr_rollback_decision_and_reopen(journal, authority)?;
                let in_flight = state_db.audit_in_flight_transition()?;
                let pending = startup_reconciliation::PendingSystemTransition::inspect(
                    installation,
                    state_db,
                    journal,
                    record,
                    in_flight,
                )
                .map_err(map_reconciliation_error)?;
                return Err(Error::RecoveryPending(pending));
            }

            let route_seal = UsrRollbackResumeRouteSeal::new();
            let route = startup_reconciliation::UsrRollbackResumeRouteAuthority::capture(
                &route_seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
                in_flight.clone(),
            )?;
            if let startup_reconciliation::UsrRollbackResumeRouteAdmission::Ready(authority) = route {
                let (journal, record) =
                    super::startup_recovery::persist_usr_rollback_resume_route_and_reopen(journal, authority)?;
                let in_flight = state_db.audit_in_flight_transition()?;
                let pending = startup_reconciliation::PendingSystemTransition::inspect(
                    installation,
                    state_db,
                    journal,
                    record,
                    in_flight,
                )
                .map_err(map_reconciliation_error)?;
                return Err(Error::RecoveryPending(pending));
            }
            let pending = startup_reconciliation::PendingSystemTransition::inspect(
                installation,
                state_db,
                journal,
                record,
                in_flight,
            )
            .map_err(map_reconciliation_error)?;
            return Err(Error::RecoveryPending(pending));
        }
        if let Some(orphan) = in_flight {
            return Err(Error::OrphanTransitionRow {
                state: i32::from(orphan.state_id),
                transition: orphan.transition_id.as_str().to_owned(),
            });
        }

        let authority = startup_reconciliation::StartupRecoveryAuthority::new(installation, journal, state_db);
        let residue = transition_identity::audit_archived_state_prune_residue(installation, authority.journal());
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        residue?;
        Ok(Self { _authority: authority })
    }

    /// Evaluate the canonical system intent only after strict active-state
    /// discovery, while this retained journal guard and the active lease's
    /// cooperating-writer coordinator are both still alive.
    pub(super) fn load_default_system_intent(
        &self,
        installation: &Installation,
        _active_state: &ActiveStateLease,
    ) -> Result<Option<crate::system_model::LoadedSystemModel>, Error> {
        default_system_intent::load(installation).map_err(Error::DefaultSystemIntent)
    }
}

fn map_reconciliation_error(source: startup_reconciliation::InspectionError) -> Error {
    match source {
        startup_reconciliation::InspectionError::Database(source) => Error::TransitionEvidence(source),
        startup_reconciliation::InspectionError::MetadataProvenance(source) => Error::MetadataProvenance(source),
        startup_reconciliation::InspectionError::Installation(source) => Error::Installation(source),
    }
}

#[cfg(test)]
std::thread_local! {
    static AFTER_MUTABLE_NAMESPACE_PREFLIGHT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_after_mutable_namespace_preflight(hook: impl FnOnce() + 'static) {
    AFTER_MUTABLE_NAMESPACE_PREFLIGHT.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_mutable_namespace_preflight() {
    AFTER_MUTABLE_NAMESPACE_PREFLIGHT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_mutable_namespace_preflight() {}

#[derive(Debug, Error)]
pub(super) enum Error {
    #[error("inspect retained state-transition journal")]
    Journal(#[from] transition_journal::StorageError),
    #[error(transparent)]
    RecoveryPending(#[from] startup_reconciliation::PendingSystemTransition),
    #[error("audit in-flight state-transition rows")]
    TransitionEvidence(#[from] db::state::TransitionEvidenceError),
    #[error("inspect immutable generated-metadata provenance during startup reconciliation")]
    MetadataProvenance(#[from] db::state::MetadataProvenanceError),
    #[error("state {state} retains orphan transition {transition} while the canonical journal is absent")]
    OrphanTransitionRow { state: i32, transition: String },
    #[error("audit interrupted archived-state prune evidence")]
    ArchivedStatePruneResidue(#[from] transition_identity::ArchivedStatePruneResidueError),
    #[error("recover CandidatePrepared active-reblit replacement residue")]
    ActiveReblitReplacementRecovery(#[from] transition_identity::ActiveReblitReplacementRecoveryError),
    #[error("capture exact startup /usr rollback-decision authority")]
    UsrRollbackDecisionAuthority(#[from] startup_reconciliation::UsrRollbackDecisionAuthorityError),
    #[error("normalize exact startup /usr exchange-parent durability")]
    UsrExchangeParentDurability(#[from] super::startup_recovery::UsrExchangeParentDurabilityError),
    #[error("persist and reconcile the exact startup /usr rollback decision")]
    UsrRollbackDecisionPersistence(#[from] super::startup_recovery::UsrRollbackDecisionPersistenceError),
    #[error("capture exact startup /usr rollback-resume routing authority")]
    UsrRollbackResumeRouteAuthority(#[from] startup_reconciliation::UsrRollbackResumeRouteAuthorityError),
    #[error("persist and reconcile the exact startup /usr rollback-resume route")]
    UsrRollbackResumeRoutePersistence(#[from] super::startup_recovery::UsrRollbackResumeRoutePersistenceError),
    #[error("revalidate retained mutable installation namespace during startup")]
    Installation(#[from] installation::Error),
    #[error("load canonical authored system intent after the system startup gate")]
    DefaultSystemIntent(#[source] default_system_intent::Error),
}

#[cfg(test)]
pub(super) use default_system_intent::arm_after_default_directory_retained;
