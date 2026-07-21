//! Terminal unlink and journal-directory-sync fault classification.

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            UsrRollbackActivateArchivedFinalizationError, finalize_usr_rollback_activate_archived,
        },
    },
    transition_journal::{
        RollbackActionOutcome, arm_next_delete_canonical_unlink_fault, arm_next_delete_directory_sync_fault,
        assert_delete_canonical_unlink_fault_consumed, assert_delete_directory_sync_fault_consumed,
        TransitionJournalRecordDeleteError, TransitionJournalRecordDeleteState,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, RouteFixture, assert_canonical_absent, candidate_move_count,
        capture_finalization_ready, enter_clean_route, persist_rollback_complete, reset_candidate_observers,
    },
};

#[derive(Clone, Copy)]
struct DeleteFault {
    arm: fn(),
    assert_consumed: fn(),
    state: TransitionJournalRecordDeleteState,
}

const DELETE_FAULTS: [DeleteFault; 2] = [
    DeleteFault {
        arm: arm_next_delete_canonical_unlink_fault,
        assert_consumed: assert_delete_canonical_unlink_fault_consumed,
        state: TransitionJournalRecordDeleteState::ExactSource,
    },
    DeleteFault {
        arm: arm_next_delete_directory_sync_fault,
        assert_consumed: assert_delete_directory_sync_fault_consumed,
        state: TransitionJournalRecordDeleteState::Absent,
    },
];

#[test]
fn startup_activate_archived_finalization_classifies_both_delete_faults_and_converges() {
    for fault in DELETE_FAULTS {
        let fixture = RouteFixture::new(
            Epoch::Current,
            CandidateSource::RootLinksComplete,
            RollbackActionOutcome::Applied,
            CandidateOutcome::Applied,
        );
        let terminal = persist_rollback_complete(&fixture);
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = capture_finalization_ready(&fixture, &journal, &reservation, &terminal);
        let database_before = fixture.database_snapshot();
        let namespace_before = fixture.namespace_snapshot();
        reset_candidate_observers();
        (fault.arm)();

        let error = finalize_usr_rollback_activate_archived(journal, authority).unwrap_err();

        (fault.assert_consumed)();
        assert!(
            matches!(
                error,
                UsrRollbackActivateArchivedFinalizationError::Delete(
                    TransitionJournalRecordDeleteError::Storage { state, .. }
                ) if state == fault.state
            ),
            "expected bound-delete state {:?}, got {error:?}",
            fault.state
        );
        match fault.state {
            TransitionJournalRecordDeleteState::ExactSource => {
                assert_eq!(fixture.canonical_record(), terminal);
            }
            TransitionJournalRecordDeleteState::Absent => {
                assert_canonical_absent(&fixture.fixture.fixture.installation.root);
            }
        }
        assert_eq!(fixture.database_snapshot(), database_before);
        assert_eq!(fixture.namespace_snapshot(), namespace_before);
        assert_eq!(candidate_move_count(), 0);
        drop(reservation);

        let clean = enter_clean_route(&fixture);

        assert_canonical_absent(&fixture.fixture.fixture.installation.root);
        assert_eq!(fixture.database_snapshot(), database_before);
        assert_eq!(fixture.namespace_snapshot(), namespace_before);
        assert_eq!(candidate_move_count(), 0);
        drop(clean);
    }
}
