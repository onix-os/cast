//! Terminal unlink and journal-directory-sync fault classification.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            DurableUsrRollbackActiveReblitFinalizationRecord, UsrRollbackActiveReblitFinalizationError,
            finalize_usr_rollback_active_reblit,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_delete_canonical_unlink_fault, arm_next_delete_directory_sync_fault,
        assert_delete_canonical_unlink_fault_consumed, assert_delete_directory_sync_fault_consumed,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, assert_canonical_absent, assert_no_candidate_effects, build_active,
        capture_finalization_ready, enter_clean_candidate, persist_rollback_complete, reset_candidate_effect_observers,
    },
};

#[derive(Clone, Copy)]
struct DeleteFault {
    arm: fn(),
    assert_consumed: fn(),
    durable: DurableUsrRollbackActiveReblitFinalizationRecord,
}

const DELETE_FAULTS: [DeleteFault; 2] = [
    DeleteFault {
        arm: arm_next_delete_canonical_unlink_fault,
        assert_consumed: assert_delete_canonical_unlink_fault_consumed,
        durable: DurableUsrRollbackActiveReblitFinalizationRecord::RollbackComplete,
    },
    DeleteFault {
        arm: arm_next_delete_directory_sync_fault,
        assert_consumed: assert_delete_directory_sync_fault_consumed,
        durable: DurableUsrRollbackActiveReblitFinalizationRecord::Absent,
    },
];

#[test]
fn startup_active_reblit_finalization_classifies_both_terminal_delete_faults_and_converges() {
    for fault in DELETE_FAULTS {
        let fixture = build_active(
            Epoch::Current,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateOrigin::AlreadySatisfied,
        );
        let terminal = persist_rollback_complete(&fixture, CandidateOrigin::Applied);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
        let database_before = fixture.fixture.database_snapshot();
        let namespace_before = fixture.fixture.namespace_snapshot();
        reset_candidate_effect_observers();
        (fault.arm)();

        let error = finalize_usr_rollback_active_reblit(journal, authority).unwrap_err();

        (fault.assert_consumed)();
        assert!(
            matches!(
                error,
                UsrRollbackActiveReblitFinalizationError::Delete { durable, .. }
                    if durable == fault.durable
            ),
            "expected durable {:?}, got {error:?}",
            fault.durable
        );
        match fault.durable {
            DurableUsrRollbackActiveReblitFinalizationRecord::RollbackComplete => {
                assert_eq!(fixture.fixture.canonical_record(), terminal);
            }
            DurableUsrRollbackActiveReblitFinalizationRecord::Absent => {
                assert_canonical_absent(&fixture.fixture.installation.root);
            }
        }
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_no_candidate_effects();
        drop(reservation);

        let clean = enter_clean_candidate(&fixture);

        assert_canonical_absent(&fixture.fixture.installation.root);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
        assert_no_candidate_effects();
        drop(clean);
    }
}
