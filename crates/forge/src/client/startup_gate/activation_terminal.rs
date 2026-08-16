//! One-entry startup dispatch of the activation terminal tail.
//!
//! A NewState or ActivateArchived transition that crashes after its commit
//! decision leaves a live journal record with no route: the ActiveReblit arms
//! beside this one are built around a staging-wrapper exchange these operations
//! never perform, so they decline it and startup reports `RecoveryPending`
//! forever.
//!
//! This drives the same terminal route the forward path uses, which resumes
//! from whichever step the record actually reached and ends with the record
//! deleted.

use thiserror::Error;

use crate::{
    Installation, db,
    transition_journal::{Operation, Phase, TransitionJournalStore, TransitionRecord},
};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_recovery::{ActivationCommitCleanupPersistenceError, finish_activation_after_commit},
};

/// Whether this entry finished the transition.
pub(super) enum Dispatch {
    Unhandled {
        journal: TransitionJournalStore,
        record: TransitionRecord,
    },
    /// The terminal record was deleted; there is deliberately no record left
    /// that could fall through into another dispatch in this startup entry.
    Finalized { journal: TransitionJournalStore },
}

/// Whether this record belongs to the activation terminal route.
///
/// ActiveReblit is excluded deliberately: it keeps its own tail, whose evidence
/// is built around the staging-wrapper exchange it performs. A rollback record
/// is excluded because this route only finishes committed forward transitions.
fn handles(record: &TransitionRecord) -> bool {
    matches!(record.operation, Operation::NewState | Operation::ActivateArchived)
        && matches!(
            record.phase,
            Phase::CommitDecided | Phase::CommitCleanupComplete | Phase::Complete
        )
        && record.rollback.is_none()
}

pub(super) fn dispatch(
    installation: &Installation,
    state_db: &db::state::Database,
    active_state_reservation: &ActiveStateReservation,
    journal: TransitionJournalStore,
    record: TransitionRecord,
) -> Result<Dispatch, Error> {
    if !handles(&record) {
        return Ok(Dispatch::Unhandled { journal, record });
    }

    let journal = finish_activation_after_commit(journal, state_db, installation, record, active_state_reservation)?;
    Ok(Dispatch::Finalized { journal })
}

#[derive(Debug, Error)]
pub(in crate::client) enum Error {
    #[error("finish the committed activation transition from its recovered phase")]
    Terminal(#[from] ActivationCommitCleanupPersistenceError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        state::TransitionId,
        transition_journal::{
            BootId, MountNamespaceIdentity, Previous, PreviousOrigin, QuarantineName, RollbackPlan, RuntimeEpoch,
            RuntimeTreeIdentity, TreeToken,
        },
    };

    fn committed(operation: Operation, phase: Phase) -> TransitionRecord {
        let mut record = TransitionRecord::preparing(
            TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            RuntimeEpoch {
                boot_id: BootId::parse("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
                mount_namespace: MountNamespaceIdentity { st_dev: 30, inode: 31 },
            },
            operation,
            // A fresh state is allocated by NewState alone; the other two adopt
            // an existing candidate and are refused without one.
            match operation {
                Operation::NewState => None,
                Operation::ActiveReblit => Some(41),
                Operation::ActivateArchived => Some(42),
            },
            TreeToken::parse("a".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
            RuntimeTreeIdentity {
                st_dev: 10,
                inode: 10,
                mount_id: 12,
            },
            Previous {
                id: Some(41),
                tree_token: TreeToken::parse("b".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
                usr_runtime_identity: RuntimeTreeIdentity {
                    st_dev: 10,
                    inode: 20,
                    mount_id: 12,
                },
                // A reblit adopts its own corrupt copy and archives nothing.
                origin: if operation == Operation::ActiveReblit {
                    PreviousOrigin::ActiveReblitCorrupt
                } else {
                    PreviousOrigin::ActiveState
                },
            },
            operation != Operation::ActiveReblit,
            true,
            QuarantineName::parse("activation-terminal-test").unwrap(),
        )
        .unwrap();
        record.phase = phase;
        record.candidate.id = Some(42);
        record
    }

    /// The route's whole contract. A crash anywhere in the committed tail must
    /// be claimed for the two operations that reclaim nothing, and never for
    /// ActiveReblit, which owns a tail built on evidence these lack.
    #[test]
    fn the_terminal_route_claims_only_committed_activation_records() {
        for phase in [Phase::CommitDecided, Phase::CommitCleanupComplete, Phase::Complete] {
            for operation in [Operation::NewState, Operation::ActivateArchived] {
                assert!(
                    handles(&committed(operation, phase)),
                    "{operation:?} at {phase:?} has no other route"
                );
            }
            assert!(
                !handles(&committed(Operation::ActiveReblit, phase)),
                "ActiveReblit at {phase:?} keeps its own tail"
            );
        }
    }

    /// Everything before the commit decision still belongs to the phase-specific
    /// arms, and a rollback record never belongs to a forward finisher.
    #[test]
    fn the_terminal_route_declines_earlier_phases_and_rollback_records() {
        for phase in [
            Phase::BootSyncStarted,
            Phase::BootSyncComplete,
            Phase::UsrExchanged,
            Phase::RollbackComplete,
        ] {
            assert!(
                !handles(&committed(Operation::NewState, phase)),
                "{phase:?} is not part of the committed tail"
            );
        }

        let mut rolling_back = committed(Operation::NewState, Phase::CommitDecided);
        rolling_back.rollback = Some(RollbackPlan {
            source: crate::transition_journal::ForwardPhase::CommitDecided,
            previous_archive: crate::transition_journal::RollbackAction::NotRequired,
            usr_exchange: crate::transition_journal::RollbackAction::NotRequired,
            candidate: crate::transition_journal::CandidateRollback {
                action: crate::transition_journal::RollbackAction::Pending,
                disposition: crate::transition_journal::AbortDisposition::Quarantine,
            },
            fresh_db: crate::transition_journal::RollbackAction::NotRequired,
            boot: crate::transition_journal::BootRollback::NotRequired,
            external_effects_may_remain: false,
        });
        assert!(!handles(&rolling_back), "a rollback record is not a forward finish");
    }
}
