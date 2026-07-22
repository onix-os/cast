use thiserror::Error;

use crate::{
    Installation,
    boot_publication::BootPublicationReceiptFingerprint,
    db, installation, transition_identity, transition_journal,
};

use super::{
    MutableSystemCapabilities,
    active_state_snapshot::{ActiveStateLease, ActiveStateReservation},
    startup_reconciliation,
};

mod active_reblit_boot_sync_complete;
mod active_reblit_boot_sync_started;
mod active_reblit_commit_cleanup;
mod active_reblit_commit_cleanup_complete;
mod active_reblit_complete_finalization;
mod default_system_intent;
#[cfg(test)]
mod root_links_terminal_process_harness;
mod usr_rollback_activate_archived;
mod usr_rollback_active_reblit;
mod usr_rollback_new_state;

pub(in crate::client) use usr_rollback_activate_archived::{
    UsrRollbackActivateArchivedCompleteRouteSeal, UsrRollbackActivateArchivedFinalizationSeal,
};
pub(in crate::client) use usr_rollback_active_reblit::{
    UsrRollbackActiveReblitBootRepairCompleteSeal, UsrRollbackActiveReblitBootRepairRequiredSeal,
    UsrRollbackActiveReblitBootRepairUnverifiedSeal, UsrRollbackActiveReblitCompleteRouteSeal,
    UsrRollbackActiveReblitFinalizationSeal,
};
pub(in crate::client) use usr_rollback_new_state::{
    UsrRollbackCompleteRouteSeal, UsrRollbackFinalizationSeal, UsrRollbackFreshDbInvalidationRouteSeal,
    UsrRollbackFreshDbInvalidationSeal,
};

/// Unforgeable safe-code token limiting candidate-preservation authority
/// capture to the writer-first startup gate and its phase-specific children.
pub(in crate::client) struct UsrRollbackCandidatePreserveSeal {
    _private: (),
}

impl UsrRollbackCandidatePreserveSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

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

/// Unforgeable safe-code token limiting exact forward ActiveReblit
/// `BootSyncComplete` adoption to this writer-first startup gate.
pub(in crate::client) struct ActiveReblitBootSyncCompleteSeal {
    _private: (),
}

impl ActiveReblitBootSyncCompleteSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable proof that restart cleanup belongs to the exact receipt which
/// is already the committed boot-publication head at `BootSyncStarted`.
///
/// Only the writer-first startup child can construct this value. Read-only
/// target adapters may inspect the inert owner fingerprint but cannot mint
/// cleanup admission themselves.
pub(in crate::client) struct ActiveReblitBootSyncStartedCleanupSeal {
    promoted_receipt: BootPublicationReceiptFingerprint,
}

impl ActiveReblitBootSyncStartedCleanupSeal {
    fn new(promoted_receipt: BootPublicationReceiptFingerprint) -> Self {
        Self { promoted_receipt }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test(
        promoted_receipt: BootPublicationReceiptFingerprint,
    ) -> Self {
        Self::new(promoted_receipt)
    }

    pub(in crate::client) const fn promoted_receipt(
        &self,
    ) -> BootPublicationReceiptFingerprint {
        self.promoted_receipt
    }
}

/// Unforgeable safe-code token limiting exact forward ActiveReblit
/// `CommitCleanupComplete` installed-receipt authentication and record advance
/// to this writer-first gate.
pub(in crate::client) struct ActiveReblitCommitCleanupCompleteSeal {
    _private: (),
}

impl ActiveReblitCommitCleanupCompleteSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable safe-code token limiting exact forward ActiveReblit
/// `Complete` terminal finalization to this writer-first startup gate.
pub(in crate::client) struct ActiveReblitCompleteFinalizationSeal {
    _private: (),
}

impl ActiveReblitCompleteFinalizationSeal {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(in crate::client) fn new_for_test() -> Self {
        Self::new()
    }
}

/// Unforgeable safe-code token limiting rollback-decision authority capture
/// to this writer-first startup gate.
pub(in crate::client) struct UsrRollbackDecisionSeal {
    _private: (),
}

/// Unforgeable safe-code token limiting forward-`UsrExchanged` root ABI
/// normalization authority to the writer-first startup gate.
pub(in crate::client) struct UsrExchangedRootAbiNormalizationSeal {
    _private: (),
}

impl UsrExchangedRootAbiNormalizationSeal {
    fn new() -> Self {
        Self { _private: () }
    }
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

/// Unforgeable safe-code token limiting rollback-reverse authority capture to
/// this writer-first startup gate.
pub(in crate::client) struct UsrRollbackReverseSeal {
    _private: (),
}

impl UsrRollbackReverseSeal {
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
        system: &MutableSystemCapabilities,
        active_state_reservation: &ActiveStateReservation,
    ) -> Result<Self, Error> {
        let installation = system.installation();
        let state_db = system.state_db();
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
            // Receipt promotion is the irreversible boundary inside
            // BootSyncStarted. An exactly pending or legacy receipt remains
            // rollback-eligible, while an exactly promoted receipt stays at
            // this forward checkpoint until cleanup recovery can resume it.
            let (journal, record) = match active_reblit_boot_sync_started::dispatch(
                installation,
                state_db,
                active_state_reservation,
                journal,
                record,
            )? {
                active_reblit_boot_sync_started::Dispatch::Unhandled { journal, record } => {
                    (journal, record)
                }
                active_reblit_boot_sync_started::Dispatch::Handled { journal, record } => {
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
            };

            // Adopt forward boot completion before any replacement cleanup,
            // root-ABI normalization, or rollback admission. Both deferred
            // evidence and the one durable successor end this startup entry,
            // so `BootSyncComplete` can never fall through and a newly written
            // `CommitDecided` record can never be redispatched here.
            let (journal, record) = match active_reblit_boot_sync_complete::dispatch(
                installation,
                state_db,
                active_state_reservation,
                journal,
                record,
            )? {
                active_reblit_boot_sync_complete::Dispatch::Unhandled { journal, record } => {
                    (journal, record)
                }
                active_reblit_boot_sync_complete::Dispatch::Handled { journal, record } => {
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
            };

            // Handle at most the exact CommitDecided cleanup checkpoint before
            // replacement mutation, root-ABI normalization, or rollback. A
            // handled source or freshly persisted successor ends this startup
            // entry and is never redispatched here.
            let (journal, record) = match active_reblit_commit_cleanup::dispatch(
                installation,
                state_db,
                active_state_reservation,
                journal,
                record,
            )? {
                active_reblit_commit_cleanup::Dispatch::Unhandled { journal, record } => {
                    (journal, record)
                }
                active_reblit_commit_cleanup::Dispatch::Handled { journal, record } => {
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
            };

            // Authenticate the installed receipt only after cleanup is durably
            // complete, then advance once to Complete without changing that
            // receipt. A deferred source or newly persisted successor ends
            // this entry and can never fall through to rollback admission.
            let (journal, record) = match active_reblit_commit_cleanup_complete::dispatch(
                installation,
                state_db,
                active_state_reservation,
                journal,
                record,
            )? {
                active_reblit_commit_cleanup_complete::Dispatch::Unhandled {
                    journal,
                    record,
                } => (journal, record),
                active_reblit_commit_cleanup_complete::Dispatch::Handled { journal, record } => {
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
            };

            // Exact forward Complete owns this entry before replacement or
            // rollback logic. Incompatible evidence remains pending; exact
            // evidence consumes one bound deletion and hands the same locked
            // store directly to shared clean admission.
            let (journal, record) = match active_reblit_complete_finalization::dispatch(
                installation,
                state_db,
                active_state_reservation,
                journal,
                record,
            )? {
                active_reblit_complete_finalization::Dispatch::Unhandled {
                    journal,
                    record,
                } => (journal, record),
                active_reblit_complete_finalization::Dispatch::Handled { journal, record } => {
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
                active_reblit_complete_finalization::Dispatch::Finalized { journal } => {
                    return Self::admit_clean_after_terminal_finalization(
                        installation,
                        state_db,
                        journal,
                    );
                }
            };

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

            // Normalize the live root ABI before rollback-decision admission.
            // Incomplete canonical subsets receive exactly one publication
            // attempt and end this startup entry. Complete sets still cross a
            // retained root-directory sync, after which decision evidence is
            // captured from scratch below. Deferred normalization must never
            // fall through to decision capture.
            let root_abi_seal = UsrExchangedRootAbiNormalizationSeal::new();
            let root_abi = startup_reconciliation::UsrExchangedRootAbiNormalizationAuthority::capture(
                &root_abi_seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
                in_flight.clone(),
            )?;
            let decision_in_flight = match root_abi {
                startup_reconciliation::UsrExchangedRootAbiNormalizationAdmission::NotApplicable => {
                    in_flight.clone()
                }
                startup_reconciliation::UsrExchangedRootAbiNormalizationAdmission::Deferred => {
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
                startup_reconciliation::UsrExchangedRootAbiNormalizationAdmission::Normalize(authority) => {
                    super::startup_recovery::normalize_usr_exchanged_root_abi(&journal, authority)?;
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
                startup_reconciliation::UsrExchangedRootAbiNormalizationAdmission::Synchronize(authority) => {
                    super::startup_recovery::synchronize_usr_exchanged_root_abi(&journal, authority)?;
                    state_db.audit_in_flight_transition()?
                }
            };

            let decision_seal = UsrRollbackDecisionSeal::new();
            let decision = startup_reconciliation::UsrRollbackDecisionAuthority::capture(
                &decision_seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
                decision_in_flight,
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

            let reverse_seal = UsrRollbackReverseSeal::new();
            let reverse = startup_reconciliation::UsrRollbackReverseAuthority::capture(
                &reverse_seal,
                installation,
                &journal,
                state_db,
                active_state_reservation,
                &record,
                in_flight.clone(),
            )?;
            let ready = match reverse {
                startup_reconciliation::UsrRollbackReverseAdmission::Apply(authority) => {
                    Some(super::startup_recovery::UsrRollbackReverseReady::Apply(authority))
                }
                startup_reconciliation::UsrRollbackReverseAdmission::Finish(authority) => {
                    Some(super::startup_recovery::UsrRollbackReverseReady::Finish(authority))
                }
                startup_reconciliation::UsrRollbackReverseAdmission::NotApplicable
                | startup_reconciliation::UsrRollbackReverseAdmission::Deferred => None,
            };
            if let Some(ready) = ready {
                let (journal, record) =
                    super::startup_recovery::dispatch_usr_rollback_reverse_and_reopen(journal, ready)?;
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

            let (journal, record) = match usr_rollback_activate_archived::dispatch(
                installation,
                state_db,
                active_state_reservation,
                journal,
                record,
                in_flight.clone(),
            )? {
                usr_rollback_activate_archived::Dispatch::Unhandled { journal, record } => (journal, record),
                usr_rollback_activate_archived::Dispatch::Handled { journal, record } => {
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
                usr_rollback_activate_archived::Dispatch::Finalized { journal } => {
                    return Self::admit_clean_after_terminal_finalization(installation, state_db, journal);
                }
            };

            let (journal, record) = match usr_rollback_active_reblit::dispatch(
                installation,
                state_db,
                active_state_reservation,
                journal,
                record,
                in_flight.clone(),
            )? {
                usr_rollback_active_reblit::Dispatch::Unhandled { journal, record } => (journal, record),
                usr_rollback_active_reblit::Dispatch::Handled { journal, record } => {
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
                usr_rollback_active_reblit::Dispatch::Finalized { journal } => {
                    return Self::admit_clean_after_terminal_finalization(installation, state_db, journal);
                }
            };

            let (journal, record) = match usr_rollback_new_state::dispatch(
                installation,
                state_db,
                active_state_reservation,
                journal,
                record,
                in_flight.clone(),
            )? {
                usr_rollback_new_state::Dispatch::Unhandled { journal, record } => (journal, record),
                usr_rollback_new_state::Dispatch::Handled { journal, record } => {
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
                usr_rollback_new_state::Dispatch::Finalized { journal } => {
                    return Self::admit_clean_after_terminal_finalization(installation, state_db, journal);
                }
            };
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
        Self::admit_clean(installation, state_db, journal, in_flight)
    }

    /// Re-establish clean-startup evidence after any operation-specific
    /// terminal finalizer consumed its record. The finalizer's same locked
    /// journal store is retained through both mutable-namespace captures and
    /// the database audit, then handed to the shared clean residue gate.
    fn admit_clean_after_terminal_finalization(
        installation: &Installation,
        state_db: &db::state::Database,
        journal: transition_journal::TransitionJournalStore,
    ) -> Result<Self, Error> {
        after_usr_rollback_finalization_before_clean_audit();
        installation.revalidate_mutable_namespace()?;
        let in_flight = state_db.audit_in_flight_transition();
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let in_flight = in_flight?;
        return Self::admit_clean(installation, state_db, journal, in_flight);
    }

    /// Enter the shared clean-startup residue audit with the caller's exact
    /// continuously locked journal store. Terminal finalization and initially
    /// absent startup both converge here without reopening the journal. A
    /// final public-aware read through that same store proves that no record
    /// appeared while the database and residue audits ran.
    fn admit_clean(
        installation: &Installation,
        state_db: &db::state::Database,
        journal: transition_journal::TransitionJournalStore,
        in_flight: Option<db::state::InFlightTransition>,
    ) -> Result<Self, Error> {
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

        installation.revalidate_mutable_namespace()?;
        let cast = installation.retained_mutable_cast_directory()?;
        let canonical = authority.journal().load_revalidated_retained_cast(cast);
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let canonical = canonical?;
        if let Some(record) = canonical {
            return Err(Error::CanonicalTransitionAppearedDuringCleanAdmission {
                transition: record.transition_id.as_str().to_owned(),
                operation: record.operation,
                phase: record.phase,
            });
        }
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
    static AFTER_USR_ROLLBACK_FINALIZATION_BEFORE_CLEAN_AUDIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_after_mutable_namespace_preflight(hook: impl FnOnce() + 'static) {
    AFTER_MUTABLE_NAMESPACE_PREFLIGHT.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(super) fn arm_after_usr_rollback_finalization_before_clean_audit(hook: impl FnOnce() + 'static) {
    AFTER_USR_ROLLBACK_FINALIZATION_BEFORE_CLEAN_AUDIT.with(|slot| {
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

#[cfg(test)]
fn after_usr_rollback_finalization_before_clean_audit() {
    AFTER_USR_ROLLBACK_FINALIZATION_BEFORE_CLEAN_AUDIT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_mutable_namespace_preflight() {}

#[cfg(not(test))]
fn after_usr_rollback_finalization_before_clean_audit() {}

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
    #[error("canonical transition {transition} appeared as {operation:?} {phase:?} while proving clean startup")]
    CanonicalTransitionAppearedDuringCleanAdmission {
        transition: String,
        operation: transition_journal::Operation,
        phase: transition_journal::Phase,
    },
    #[error("audit interrupted archived-state prune evidence")]
    ArchivedStatePruneResidue(#[from] transition_identity::ArchivedStatePruneResidueError),
    #[error("recover CandidatePrepared active-reblit replacement residue")]
    ActiveReblitReplacementRecovery(#[from] transition_identity::ActiveReblitReplacementRecoveryError),
    #[error("guard the ActiveReblit BootSyncStarted receipt-promotion boundary")]
    ActiveReblitBootSyncStartedDispatch(#[from] active_reblit_boot_sync_started::Error),
    #[error("dispatch the exact forward startup ActiveReblit BootSyncComplete checkpoint")]
    ActiveReblitBootSyncCompleteDispatch(#[from] active_reblit_boot_sync_complete::Error),
    #[error("dispatch the exact forward startup ActiveReblit CommitDecided cleanup checkpoint")]
    ActiveReblitCommitCleanupDispatch(#[from] active_reblit_commit_cleanup::Error),
    #[error("dispatch the exact forward startup ActiveReblit CommitCleanupComplete checkpoint")]
    ActiveReblitCommitCleanupCompleteDispatch(
        #[from] active_reblit_commit_cleanup_complete::Error,
    ),
    #[error("dispatch the exact forward startup ActiveReblit Complete finalizer")]
    ActiveReblitCompleteFinalizationDispatch(
        #[from] active_reblit_complete_finalization::Error,
    ),
    #[error("capture exact startup /usr rollback-decision authority")]
    UsrRollbackDecisionAuthority(#[from] startup_reconciliation::UsrRollbackDecisionAuthorityError),
    #[error("capture exact startup UsrExchanged root ABI normalization authority")]
    UsrExchangedRootAbiNormalizationAuthority(
        #[from] startup_reconciliation::UsrExchangedRootAbiNormalizationAuthorityError,
    ),
    #[error("execute exact startup UsrExchanged root ABI normalization")]
    UsrExchangedRootAbiNormalizationExecution(
        #[from] super::startup_recovery::UsrExchangedRootAbiNormalizationExecutionError,
    ),
    #[error("normalize exact startup /usr exchange-parent durability")]
    UsrExchangeParentDurability(#[from] super::startup_recovery::UsrExchangeParentDurabilityError),
    #[error("persist and reconcile the exact startup /usr rollback decision")]
    UsrRollbackDecisionPersistence(#[from] super::startup_recovery::UsrRollbackDecisionPersistenceError),
    #[error("capture exact startup /usr rollback-resume routing authority")]
    UsrRollbackResumeRouteAuthority(#[from] startup_reconciliation::UsrRollbackResumeRouteAuthorityError),
    #[error("persist and reconcile the exact startup /usr rollback-resume route")]
    UsrRollbackResumeRoutePersistence(#[from] super::startup_recovery::UsrRollbackResumeRoutePersistenceError),
    #[error("capture exact startup /usr rollback-reverse authority")]
    UsrRollbackReverseAuthority(#[from] startup_reconciliation::UsrRollbackReverseAuthorityError),
    #[error("execute and persist one exact startup /usr rollback-reverse phase")]
    UsrRollbackReverseDispatch(#[from] super::startup_recovery::UsrRollbackReverseDispatchError),
    #[error("dispatch the exact startup ActiveReblit candidate-preservation checkpoint")]
    UsrRollbackActiveReblitDispatch(#[from] usr_rollback_active_reblit::Error),
    #[error("dispatch the exact startup ActivateArchived candidate-preservation checkpoint")]
    UsrRollbackActivateArchivedDispatch(#[from] usr_rollback_activate_archived::Error),
    #[error("dispatch one exact phase of the startup NewState rollback suffix")]
    UsrRollbackNewStateDispatch(#[from] usr_rollback_new_state::Error),
    #[error("revalidate retained mutable installation namespace during startup")]
    Installation(#[from] installation::Error),
    #[error("load canonical authored system intent after the system startup gate")]
    DefaultSystemIntent(#[source] default_system_intent::Error),
}

#[cfg(test)]
pub(super) use default_system_intent::arm_after_default_directory_retained;
